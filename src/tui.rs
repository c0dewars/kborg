use crate::{
    Result,
    cli::{Options, runtime},
    gateway::{Connection, Gateway, Kind, Prepared, contexts, summary},
    policy::{Action, Approval},
    safety,
    themes::{self, Theme},
};
use crossterm::{
    cursor,
    event::{
        self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind, KeyModifiers,
        MouseEventKind,
    },
    execute, queue,
    style::{self, Color},
    terminal::{
        self, ClearType, DisableLineWrap, EnableLineWrap, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use serde_json::Value;
use std::{
    io::{self, IsTerminal, Write},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};

struct Terminal;
impl Terminal {
    fn enter(mouse: bool) -> Result<Self> {
        if std::env::var("TERM").is_ok_and(|term| term == "dumb") {
            return Err(
                "TERM=dumb cannot run the interactive UI; use CLI JSON/YAML commands".into(),
            );
        }
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err("interactive mode needs a terminal; use list/get --json or --help".into());
        }
        terminal::enable_raw_mode().map_err(|_| "cannot enable raw mode")?;
        let guard = Self;
        if execute!(
            io::stdout(),
            EnterAlternateScreen,
            DisableLineWrap,
            cursor::Hide
        )
        .is_err()
        {
            let _ = terminal::disable_raw_mode();
            return Err("cannot enter alternate screen".into());
        }
        if mouse {
            execute!(io::stdout(), EnableMouseCapture)
                .map_err(|_| "cannot enable mouse capture")?;
        }
        std::panic::set_hook(Box::new(|_| {
            let _ = terminal::disable_raw_mode();
            let _ = execute!(
                io::stdout(),
                LeaveAlternateScreen,
                EnableLineWrap,
                DisableMouseCapture,
                cursor::Show,
                style::ResetColor
            );
            eprintln!("KBorg encountered an internal error; no object contents recorded.");
        }));
        Ok(guard)
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            EnableLineWrap,
            DisableMouseCapture,
            cursor::Show,
            style::ResetColor
        );
    }
}

enum Task {
    Connect(Connection),
    List(String, String),
    Watch(String, String),
    Metrics(crate::metrics::MetricsScope, String),
    Get(String, String, String),
    Yaml(String, String, String),
    Overview(Connection, Vec<String>),
    Containers(String, String, bool, bool),
    Logs(String, String, Option<String>, bool),
    Follow(String, String, Option<String>, u64),
    Discover,
    Prepare(String, String, String, Option<String>, Action),
    Execute(Box<Prepared>, Approval),
}
enum Reply {
    Connected(String, String, String, Vec<Kind>),
    Items(Value),
    Detail(String),
    Containers(String, String, Vec<String>, bool, bool),
    Following,
    Catalog(Vec<Kind>),
    Prepared(Box<Prepared>),
    Notice(String),
    Failure(String),
    Watching,
    Metrics(
        crate::metrics::MetricsScope,
        String,
        Result<crate::metrics::MetricsSnapshot>,
    ),
}
#[derive(Default)]
struct WatchSlot {
    epoch: std::sync::atomic::AtomicU64,
    snapshot: std::sync::Mutex<Option<(u64, crate::watch::WatchSnapshot)>>,
    abort: std::sync::Mutex<Option<tokio::task::AbortHandle>>,
}
impl WatchSlot {
    fn invalidate(&self, epoch: u64) {
        self.epoch.store(epoch, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut abort) = self.abort.lock()
            && let Some(handle) = abort.take()
        {
            handle.abort();
        }
        self.clear();
    }
    fn clear(&self) {
        if let Ok(mut slot) = self.snapshot.lock() {
            *slot = None;
        }
    }
    fn publish(&self, epoch: u64, snapshot: crate::watch::WatchSnapshot) {
        if let Ok(mut slot) = self.snapshot.lock()
            && self.epoch.load(std::sync::atomic::Ordering::SeqCst) == epoch
        {
            *slot = Some((epoch, snapshot));
        }
    }
    fn take(&self) -> Option<(u64, crate::watch::WatchSnapshot)> {
        self.snapshot.lock().ok().and_then(|mut slot| slot.take())
    }
}
type WatchMailbox = std::sync::Arc<WatchSlot>;

#[derive(Default)]
struct LogState {
    text: String,
    dirty: bool,
}
#[derive(Default)]
struct LogSlot {
    generation: std::sync::atomic::AtomicU64,
    state: std::sync::Mutex<LogState>,
    abort: std::sync::Mutex<Option<tokio::task::AbortHandle>>,
}
impl LogSlot {
    fn cancel(&self) -> u64 {
        let generation = self
            .generation
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            + 1;
        if let Ok(mut abort) = self.abort.lock()
            && let Some(handle) = abort.take()
        {
            handle.abort();
        }
        if let Ok(mut state) = self.state.lock() {
            *state = LogState::default();
        }
        generation
    }
    fn append(&self, generation: u64, line: &str) -> bool {
        if let Ok(mut state) = self.state.lock() {
            if self.generation.load(std::sync::atomic::Ordering::SeqCst) != generation {
                return false;
            }
            // A single bounded mailbox, never one queued message per log line.
            let line = safety::safe_text(line, 4096);
            if state.text.len() + line.len() < crate::logs::MAX_LOG_BYTES {
                state.text.push_str(&line);
                state.text.push('\n');
                state.dirty = true;
                return true;
            }
            if state.text.len() <= crate::logs::MAX_LOG_BYTES {
                state
                    .text
                    .push_str("[Stopped: display reached 256 KiB; start a new session]\n");
                state.dirty = true;
            }
        }
        false
    }
    fn take(&self) -> Option<String> {
        let mut state = self.state.lock().ok()?;
        if !state.dirty {
            return None;
        }
        state.dirty = false;
        Some(state.text.clone())
    }
}

struct Worker {
    tx: tokio::sync::mpsc::Sender<(u64, Task)>,
    rx: Receiver<(u64, Reply)>,
    watch: WatchMailbox,
    logs: std::sync::Arc<LogSlot>,
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.logs.cancel();
        self.watch.invalidate(u64::MAX);
    }
}
fn worker() -> Worker {
    let (tx, mut commands) = tokio::sync::mpsc::channel::<(u64, Task)>(4);
    let (results, rx) = mpsc::sync_channel(4);
    let watch: WatchMailbox = std::sync::Arc::new(WatchSlot::default());
    let watch_results = watch.clone();
    let logs = std::sync::Arc::new(LogSlot::default());
    let log_results = logs.clone();
    std::thread::spawn(move || {
        let rt = match runtime() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = results.send((0, Reply::Failure(e)));
                return;
            }
        };
        rt.block_on(async move {
            let mut gateway: Option<Gateway> = None;
            let mut watch_job: Option<tokio::task::JoinHandle<()>> = None;
            while let Some((epoch, task)) = commands.recv().await {
                if matches!(&task, Task::Connect(_) | Task::Watch(..) | Task::List(..)) {
                    if let Some(job) = watch_job.take() { job.abort(); }
                    watch_results.clear();
                }
                let reply = match task {
                    Task::Connect(options) => {
                        gateway = None;
                        match tokio::time::timeout(Duration::from_secs(15), Gateway::connect(options)).await {
                            Ok(Ok(g)) => {
                                let reply = Reply::Connected(g.context.clone(), g.server.clone(), g.namespace.clone(), g.kinds.clone());
                                gateway = Some(g);
                                reply
                            }
                            Ok(Err(e)) => Reply::Failure(e),
                            Err(_) => Reply::Failure("connection timeout; select a context or retry".into()),
                        }
                    }
                    Task::Watch(k, n) => {
                        match gateway.as_ref().ok_or("not connected; select a context or retry".to_string()).and_then(|g| g.watch(&k, &n)) {
                            Err(e) => Reply::Failure(e),
                            Ok(mut feed) => {
                                let mailbox = watch_results.clone();
                                watch_job = Some(tokio::spawn(async move {
                                    loop {
                                        let snapshot = feed.next().await;
                                        let terminal = snapshot.terminal;
                                        mailbox.publish(epoch, snapshot);
                                        if terminal { break; }
                                    }
                                }));
                                if let Some(job) = &watch_job && let Ok(mut handle) = watch_results.abort.lock() {
                                    if watch_results.epoch.load(std::sync::atomic::Ordering::SeqCst) == epoch { *handle = Some(job.abort_handle()); }
                                    else { job.abort(); }
                                }
                                Reply::Watching
                            }
                        }
                    }
                    Task::Metrics(scope, n) => {
                        let result = if let Some(g) = gateway.as_ref() {
                            match tokio::time::timeout(Duration::from_secs(20), crate::metrics::collect(g, scope, &n)).await {
                                Ok(result) => result,
                                Err(_) => Err("Metrics request timed out; resource browsing remains available.".into()),
                            }
                        } else { Err("Not connected; select a context or retry.".into()) };
                        Reply::Metrics(scope, n, result)
                    }
                    task => {
                        if let Some(g) = gateway.as_mut() {
                            let future = async {
                                Ok(match task {
                                    Task::List(k,n) => Reply::Items(g.list(&k,&n).await?),
                                    Task::Get(k,n,name) => Reply::Detail(serde_json::to_string_pretty(&g.get(&k,&n,&name).await?).map_err(|_| "serialization failed")?),
                                    Task::Yaml(k,n,name) => Reply::Detail(crate::inspect::yaml(&g.get(&k,&n,&name).await?)?),
                                    Task::Overview(options,names) => Reply::Detail(crate::overview::text(&crate::overview::collect(&options,&names).await?)),
                                    Task::Containers(n,name,previous,follow) => {
                                        let names = g.containers(&n,&name).await?;
                                        Reply::Containers(n,name,names,previous,follow)
                                    },
                                    Task::Logs(n,name,container,previous) => Reply::Detail(g.logs(&n,&name,container,previous).await?),
                                    Task::Follow(n,name,container,generation) => {
                                        if log_results.generation.load(std::sync::atomic::Ordering::SeqCst) != generation {return Ok(Reply::Following);}
                                        let mut feed = g.follow_logs(&n,&name,container,false).await?;
                                        let mailbox = log_results.clone();
                                        let job = tokio::spawn(async move {
                                            loop {
                                                match feed.next().await {
                                                    Ok(Some(line)) => {if !mailbox.append(generation,&line) {break;}},
                                                    Ok(None) => {mailbox.append(generation,"[Log stream ended]"); break;},
                                                    Err(error) => {mailbox.append(generation,&format!("[Stopped: {error}]")); break;}
                                                }
                                            }
                                        });
                                        if let Ok(mut handle) = log_results.abort.lock() {
                                            if log_results.generation.load(std::sync::atomic::Ordering::SeqCst) == generation {
                                                *handle = Some(job.abort_handle());
                                            } else {job.abort();}
                                        } else {job.abort();}
                                        Reply::Following
                                    },
                                    Task::Discover => Reply::Catalog(g.discover().await?),
                                    Task::Prepare(k,n,name,uid,a) => {
                                        let prepared = g.prepare(&k,&n,&name,a).await?;
                                        if uid.as_deref().is_some_and(|u| u != prepared.target.uid) { return Err("selected object was replaced; reselect it before approval".into()); }
                                        Reply::Prepared(Box::new(prepared))
                                    },
                                    Task::Execute(p,a) => Reply::Notice(g.execute(*p,a).await?),
                                    Task::Connect(_) | Task::Watch(..) | Task::Metrics(..) => return Err("invalid worker task".into()),
                                })
                            };
                            match tokio::time::timeout(Duration::from_secs(25), future).await {
                                Ok(Ok(reply)) => reply,
                                Ok(Err(e)) => Reply::Failure(e),
                                Err(_) => Reply::Failure("request timed out; data STALE, mutation outcome potentially UNKNOWN".into()),
                            }
                        } else { Reply::Failure("not connected; select a context or retry".into()) }
                    }
                };
                if results.send((epoch, reply)).is_err() { break; }
            }
            if let Some(job) = watch_job { job.abort(); }
        });
    });
    Worker {
        tx,
        rx,
        watch,
        logs,
    }
}

enum Modal {
    None,
    Input(&'static str, String),
    Filter(String, String, Option<String>, usize),
    Detail(String, usize),
    Metrics(String, usize),
    Follow(String, usize),
    Containers(String, String, Vec<String>, usize, bool, bool),
    Image(
        String,
        String,
        String,
        Option<String>,
        Option<String>,
        String,
    ),
    Picker(&'static str, Vec<String>, usize),
    Themes(Vec<Theme>, usize, Theme),
    NewTheme(String, Theme, Theme),
    Confirm(Box<Prepared>, String),
    Scale(String, String, String, Option<String>, String),
    Help(usize),
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
enum Sort {
    #[default]
    Name,
    Namespace,
    Status,
    Restarts,
    Age,
}
impl Sort {
    fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Namespace => "namespace",
            Self::Status => "status",
            Self::Restarts => "restarts",
            Self::Age => "age",
        }
    }
    fn next(self) -> Self {
        match self {
            Self::Name => Self::Namespace,
            Self::Namespace => Self::Status,
            Self::Status => Self::Restarts,
            Self::Restarts => Self::Age,
            Self::Age => Self::Name,
        }
    }
}
struct App {
    options: Options,
    theme: Theme,
    context: String,
    server: String,
    namespace: String,
    kinds: Vec<Kind>,
    kind_index: usize,
    items: Vec<Value>,
    selected: usize,
    filter: String,
    sort: Sort,
    left_focus: bool,
    status: String,
    stale: bool,
    truncated: bool,
    modal: Modal,
    epoch: u64,
    busy: bool,
    command_epoch: Option<u64>,
    connecting_epoch: Option<u64>,
    watching: bool,
    watch_blocked: bool,
    sticky_status: bool,
    namespace_override: bool,
    write_inflight: bool,
    connected: bool,
    refresh: bool,
    last_refresh: Instant,
    metrics_history: crate::metrics::MetricsHistory,
    last_metrics: Instant,
    worker: Worker,
}
impl App {
    fn kind(&self) -> String {
        self.kinds
            .get(self.kind_index)
            .map(Kind::key)
            .unwrap_or_else(|| "pods".into())
    }
    fn visible(&self) -> Vec<&Value> {
        let filter = self.filter.to_ascii_lowercase();
        let mut items: Vec<_> = self
            .items
            .iter()
            .filter(|v| {
                v["metadata"]["name"]
                    .as_str()
                    .unwrap_or("")
                    .to_ascii_lowercase()
                    .contains(&filter)
                    || v["metadata"]["namespace"]
                        .as_str()
                        .unwrap_or("")
                        .to_ascii_lowercase()
                        .contains(&filter)
            })
            .collect();
        items.sort_by(|a, b| {
            let ordering = match self.sort {
                Sort::Name => name(a).cmp(name(b)),
                Sort::Namespace => namespace(a).cmp(namespace(b)),
                Sort::Status => summary(a).cmp(&summary(b)),
                Sort::Restarts => restarts(b).cmp(&restarts(a)),
                Sort::Age => timestamp(a)
                    .unwrap_or(i64::MAX)
                    .cmp(&timestamp(b).unwrap_or(i64::MAX)),
            };
            ordering
                .then_with(|| namespace(a).cmp(namespace(b)))
                .then_with(|| name(a).cmp(name(b)))
        });
        items
    }
    fn set_sort(&mut self, sort: Sort) {
        let uid = self.selected_uid();
        self.sort = sort;
        self.selected = uid
            .and_then(|uid| {
                self.visible()
                    .iter()
                    .position(|v| v["metadata"]["uid"].as_str() == Some(uid.as_str()))
            })
            .unwrap_or(0);
        self.status = format!(
            "Sorted by {}{}.",
            sort.label(),
            if sort == Sort::Restarts {
                " (highest first)"
            } else {
                ""
            }
        );
    }
    fn command(&mut self, text: &str) -> Result<bool> {
        let words: Vec<_> = text
            .trim()
            .trim_start_matches(':')
            .split_whitespace()
            .collect();
        match words.as_slice() {
            [] => {}
            ["q" | "quit"] => return Ok(true),
            ["help"] => self.modal = Modal::Help(0),
            ["ctx" | "contexts"] => return self.key(KeyCode::Char('c')),
            ["ns" | "namespaces", namespace] => {
                if *namespace == "*" || safety::valid_id(namespace) {
                    self.namespace = (*namespace).into();
                    self.namespace_override = true;
                    self.changed();
                    self.status = format!("Namespace: {namespace}");
                } else {
                    self.status = "Invalid namespace.".into();
                }
            }
            ["all"] => {
                self.namespace = "*".into();
                self.namespace_override = true;
                self.changed();
                self.status = "Browsing all namespaces.".into();
            }
            ["sort", key] => {
                let sort = match *key {
                    "name" => Some(Sort::Name),
                    "namespace" | "ns" => Some(Sort::Namespace),
                    "status" => Some(Sort::Status),
                    "restarts" => Some(Sort::Restarts),
                    "age" => Some(Sort::Age),
                    _ => None,
                };
                if let Some(sort) = sort {
                    self.set_sort(sort);
                } else {
                    self.status = "Sort: name | namespace | status | restarts | age".into();
                }
            }
            [resource] => {
                let resource = resource_alias(resource);
                let matches: Vec<_> = self
                    .kinds
                    .iter()
                    .enumerate()
                    .filter(|(_, kind)| {
                        kind.key() == resource
                            || kind.plural == resource
                            || kind.kind.eq_ignore_ascii_case(resource)
                    })
                    .map(|(index, _)| index)
                    .collect();
                if matches.len() == 1 {
                    self.kind_index = matches[0];
                    self.left_focus = false;
                    self.changed();
                    self.status = format!("Browsing {}.", self.kind());
                } else if matches.len() > 1 {
                    self.status = "Ambiguous resource; use its full plural.group name.".into();
                } else {
                    self.status =
                        "Unknown resource; use :pods, :deploy, :svc, :nodes or R discovery.".into();
                }
            }
            _ => {
                self.status =
                    "Commands: :RESOURCE | :ns NAME | :all | :ctx | :sort FIELD | :help".into()
            }
        }
        Ok(false)
    }
    fn selected_target(&self) -> Option<(String, String, String)> {
        self.visible().get(self.selected).map(|v| {
            (
                self.kind(),
                v["metadata"]["namespace"]
                    .as_str()
                    .unwrap_or(&self.namespace)
                    .into(),
                v["metadata"]["name"].as_str().unwrap_or("").into(),
            )
        })
    }
    fn selected_uid(&self) -> Option<String> {
        self.visible()
            .get(self.selected)
            .and_then(|v| v["metadata"]["uid"].as_str())
            .map(str::to_owned)
    }
    fn replace_items(&mut self, items: Vec<Value>) {
        let identity = self.visible().get(self.selected).map(|v| {
            (
                v["metadata"]["namespace"].clone(),
                v["metadata"]["name"].clone(),
                v["metadata"]["uid"].clone(),
            )
        });
        self.items = items;
        self.selected = identity
            .and_then(|(ns, name, uid)| {
                self.visible().iter().position(|v| {
                    v["metadata"]["namespace"] == ns
                        && v["metadata"]["name"] == name
                        && v["metadata"]["uid"] == uid
                })
            })
            .unwrap_or_else(|| self.selected.min(self.visible().len().saturating_sub(1)));
    }
    fn send(&mut self, task: Task) {
        if self.busy && !matches!(&task, Task::Connect(_)) {
            self.status = "Busy; wait for the current request.".into();
            return;
        }
        let writes = matches!(&task, Task::Execute(..));
        match self.worker.tx.try_send((self.epoch, task)) {
            Ok(()) => {
                self.busy = true;
                self.command_epoch = Some(self.epoch);
                self.write_inflight = writes;
            }
            Err(_) => self.status = "Request queue is full/closed.".into(),
        }
    }
    fn changed(&mut self) {
        self.epoch += 1;
        self.items.clear();
        self.selected = 0;
        self.stale = true;
        self.refresh = true;
        self.watching = false;
        self.watch_blocked = false;
        self.sticky_status = false;
        self.worker.watch.invalidate(self.epoch);
        self.worker.logs.cancel();
        self.metrics_history.clear();
    }
    fn connect(&mut self) {
        self.namespace_override = false;
        self.changed();
        self.connected = false;
        self.server = "connecting...".into();
        self.connecting_epoch = Some(self.epoch);
        self.send(Task::Connect(self.options.connection.clone()));
    }
    fn receive(&mut self) -> bool {
        let mut changed = false;
        while let Ok((epoch, reply)) = self.worker.rx.try_recv() {
            changed = true;
            let command_result = self.command_epoch == Some(epoch);
            let write_result = command_result && self.write_inflight;
            if command_result {
                self.busy = false;
                self.write_inflight = false;
                self.command_epoch = None;
            }
            let connection_result =
                matches!(&reply, Reply::Connected(..)) && self.connecting_epoch == Some(epoch);
            if matches!(&reply, Reply::Connected(..)) && !connection_result {
                continue;
            }
            if epoch != self.epoch && !connection_result && !write_result {
                continue;
            }
            match reply {
                Reply::Connected(c, s, n, k) => {
                    let selected_kind = self.kind();
                    self.connecting_epoch = None;
                    self.context = c;
                    self.server = s;
                    if !self.namespace_override {
                        self.namespace = n;
                    }
                    self.kinds = k;
                    let retained_kind = self
                        .kinds
                        .iter()
                        .position(|kind| kind.key() == selected_kind);
                    self.kind_index = retained_kind.unwrap_or(0);
                    self.connected = true;
                    self.refresh = retained_kind.is_some();
                    self.watch_blocked = retained_kind.is_none();
                    self.status = if retained_kind.is_some() {
                        "Connected. READ ONLY unless explicitly enabled."
                    } else {
                        "Selected resource needs catalog refresh; press R before resuming."
                    }
                    .into();
                }
                Reply::Items(value) => {
                    self.replace_items(value["items"].as_array().cloned().unwrap_or_default());
                    self.truncated = value["truncated"] == true;
                    self.selected = self.selected.min(self.visible().len().saturating_sub(1));
                    self.stale = false;
                    self.status = format!(
                        "{} objects | cached filter | refresh every 5s",
                        self.items.len()
                    );
                }
                Reply::Detail(text) => {
                    self.modal = Modal::Detail(text.replace('\t', " | "), 0);
                }
                Reply::Containers(n, name, names, previous, follow) => {
                    if names.is_empty() {
                        self.status = "No containers found in the selected pod.".into();
                    } else {
                        self.modal = Modal::Containers(n, name, names, 0, previous, follow);
                    }
                }
                Reply::Following => {}
                Reply::Catalog(k) => {
                    self.kinds = k;
                    self.kind_index = 0;
                    self.changed();
                    self.status = "API discovery complete (Secrets excluded).".into();
                }
                Reply::Prepared(p) => self.modal = Modal::Confirm(p, String::new()),
                Reply::Notice(text) => {
                    self.sticky_status = true;
                    self.modal = Modal::None;
                    self.status = text;
                    self.refresh = true;
                }
                Reply::Failure(error) => {
                    if matches!(self.modal, Modal::Follow(..)) {
                        self.worker.logs.cancel();
                        self.modal = Modal::Detail(format!("LOGS unavailable\n{error}"), 0);
                    }
                    if self.connecting_epoch == Some(epoch) {
                        self.connecting_epoch = None;
                    }
                    if write_result {
                        self.sticky_status = true;
                    }
                    self.stale = true;
                    if error.contains("API 401") || error.contains("API 403") {
                        self.items.clear();
                        self.modal = Modal::None;
                    }
                    self.status = error;
                }
                Reply::Watching => {
                    self.watching = true;
                    self.status = "WATCH: synchronizing bounded snapshot...".into();
                }
                Reply::Metrics(scope, namespace, result) => {
                    let now = Instant::now();
                    let text = match result {
                        Ok(snapshot) => {
                            self.metrics_history.record(&self.context, &snapshot, now);
                            format!("{}\n{}", self.metrics_history.text(now), snapshot.text())
                        }
                        Err(error) => {
                            self.metrics_history.record_error(
                                &self.context,
                                scope,
                                &namespace,
                                now,
                            );
                            format!(
                                "METRICS unavailable\n{error}\nResource browsing remains available.\n{}",
                                self.metrics_history.text(now)
                            )
                        }
                    };
                    if let Modal::Metrics(_, offset) = &self.modal {
                        self.modal = Modal::Metrics(text, *offset);
                    }
                }
            }
        }
        let snapshot = self.worker.watch.take();
        if let Some((epoch, snapshot)) = snapshot
            && epoch == self.epoch
            && self.connected
        {
            changed = true;
            self.replace_items(snapshot.objects);
            self.selected = self.selected.min(self.visible().len().saturating_sub(1));
            self.stale = snapshot.stale;
            self.truncated = snapshot.truncated;
            self.watching = !snapshot.terminal;
            self.watch_blocked = snapshot.terminal;
            if self.watch_blocked {
                self.refresh = false;
            }
            if !self.write_inflight && !self.sticky_status {
                self.status = snapshot.message;
            }
        }
        if let Some(text) = self.worker.logs.take()
            && let Modal::Follow(_, offset) = &self.modal
        {
            self.modal = Modal::Follow(text, *offset);
            changed = true;
        }
        changed
    }
    fn theme_picker(&mut self) -> Result<()> {
        let all = themes::all()?;
        let index = all.iter().position(|t| t.id == self.theme.id).unwrap_or(0);
        self.modal = Modal::Themes(all, index, self.theme.clone());
        Ok(())
    }
    fn request_metrics(&mut self) {
        if self.busy {
            self.status = "Busy; wait for the current request.".into();
            return;
        }
        if !matches!(self.modal, Modal::Metrics(..)) {
            self.modal = Modal::Metrics("Loading Metrics API...".into(), 0);
        }
        self.last_metrics = Instant::now();
        self.send(Task::Metrics(
            if self.kind() == "nodes" {
                crate::metrics::MetricsScope::Nodes
            } else {
                crate::metrics::MetricsScope::Pods
            },
            self.namespace.clone(),
        ));
    }
    fn action_menu(&self) -> Vec<String> {
        let kind = self.kind();
        let has_selection = !self.visible().is_empty();
        [
            "Inspect JSON",
            "Inspect YAML",
            "Logs snapshot",
            "Previous logs",
            "Follow logs (bounded)",
            "Metrics",
            "Context overview",
            "Scale deployment",
            "Change deployment image",
            "Restart deployment",
            "Delete selected resource",
        ]
        .iter()
        .enumerate()
        .map(|(index, title)| {
            let reason = if index == 5 || index == 6 {
                None
            } else if !has_selection {
                Some("select an object")
            } else if (2..=4).contains(&index) && !self.options.connection.logs {
                Some("requires --allow-logs")
            } else if (2..=4).contains(&index) && kind != "pods" {
                Some("Pods only")
            } else if index >= 7 && !self.options.connection.writable {
                Some("READ ONLY; requires --write")
            } else if (7..=9).contains(&index) && kind != "deployments.apps" {
                Some("Deployments only")
            } else if index == 10
                && !matches!(
                    kind.as_str(),
                    "pods" | "deployments.apps" | "services" | "jobs.batch"
                )
            {
                Some("unsupported resource")
            } else {
                None
            };
            reason
                .map(|reason| format!("[OFF] {title}: {reason}"))
                .unwrap_or_else(|| format!("      {title}"))
        })
        .collect()
    }
    fn prepare(&mut self, action: Action) {
        if !self.options.connection.writable {
            self.status = "READ ONLY: enable --write before requesting a mutation.".into();
            return;
        }
        let allowed = match &action {
            Action::Scale(_) | Action::Restart | Action::SetImage { .. } => {
                self.kind() == "deployments.apps"
            }
            Action::Delete => matches!(
                self.kind().as_str(),
                "pods" | "deployments.apps" | "services" | "jobs.batch"
            ),
        };
        if !allowed {
            self.status = "This operation is disabled for the selected resource.".into();
            return;
        }
        if let Some((k, n, name)) = self.selected_target() {
            self.send(Task::Prepare(k, n, name, self.selected_uid(), action));
        }
    }
    fn key(&mut self, code: KeyCode) -> Result<bool> {
        if self.write_inflight {
            self.status =
                "Mutation in progress; wait for the result before navigating or quitting.".into();
            return Ok(false);
        }
        self.sticky_status = false;
        let modal = std::mem::replace(&mut self.modal, Modal::None);
        match modal {
            Modal::Filter(original, mut text, original_uid, original_index) => match code {
                KeyCode::Esc => {
                    self.filter = original;
                    self.selected = original_uid
                        .and_then(|uid| {
                            self.visible().iter().position(|object| {
                                object["metadata"]["uid"].as_str() == Some(uid.as_str())
                            })
                        })
                        .unwrap_or_else(|| {
                            original_index.min(self.visible().len().saturating_sub(1))
                        });
                }
                KeyCode::Enter => {}
                KeyCode::Backspace => {
                    text.pop();
                    self.filter = text.clone();
                    self.selected = 0;
                    self.modal = Modal::Filter(original, text, original_uid, original_index);
                }
                KeyCode::Char(c) if !c.is_control() && text.len() < 100 => {
                    text.push(c);
                    self.filter = text.clone();
                    self.selected = 0;
                    self.modal = Modal::Filter(original, text, original_uid, original_index);
                }
                _ => self.modal = Modal::Filter(original, text, original_uid, original_index),
            },
            Modal::Follow(text, mut offset) => match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.worker.logs.cancel();
                }
                _ => {
                    offset = scroll_offset(&text, offset, code);
                    self.modal = Modal::Follow(text, offset);
                }
            },
            Modal::Containers(n, name, names, mut index, previous, follow) => match code {
                KeyCode::Esc => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    index = (index + 1).min(names.len().saturating_sub(1));
                    self.modal = Modal::Containers(n, name, names, index, previous, follow);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal =
                        Modal::Containers(n, name, names, index.saturating_sub(1), previous, follow)
                }
                KeyCode::Enter => {
                    if let Some(container) = names.get(index) {
                        if follow {
                            let generation = self.worker.logs.cancel();
                            self.modal = Modal::Follow("Connecting to logs...".into(), 0);
                            self.send(Task::Follow(n, name, Some(container.clone()), generation));
                        } else {
                            self.send(Task::Logs(n, name, Some(container.clone()), previous));
                        }
                    }
                }
                _ => self.modal = Modal::Containers(n, name, names, index, previous, follow),
            },
            Modal::Image(k, n, name, uid, container, mut text) => match code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    if let Some(container) = container {
                        self.send(Task::Prepare(
                            k,
                            n,
                            name,
                            uid,
                            Action::SetImage {
                                container,
                                image: text,
                            },
                        ));
                    } else if safety::valid_id(&text) {
                        self.modal = Modal::Image(k, n, name, uid, Some(text), String::new());
                    } else {
                        self.status = "Enter a valid deployment container name.".into();
                        self.modal = Modal::Image(k, n, name, uid, None, text);
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.modal = Modal::Image(k, n, name, uid, container, text);
                }
                KeyCode::Char(c) if c.is_ascii_graphic() && text.len() < 512 => {
                    text.push(c);
                    self.modal = Modal::Image(k, n, name, uid, container, text);
                }
                _ => self.modal = Modal::Image(k, n, name, uid, container, text),
            },
            Modal::Input(label, mut text) => match code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    if label == "Command" {
                        return self.command(&text);
                    } else if label == "Filter" {
                        self.filter = text;
                        self.selected = 0;
                    } else if label == "Overview contexts (comma separated, max 4)" {
                        let names: Vec<String> =
                            text.split(',').map(|name| name.trim().to_owned()).collect();
                        if names.len() > 4 {
                            self.status="TUI overview allows up to 4 explicit contexts; CLI supports up to 100.".into();
                        } else if let Err(error) = crate::overview::validate(&names) {
                            self.status = error;
                        } else {
                            self.send(Task::Overview(self.options.connection.clone(), names));
                        }
                    } else if label == "Namespace" {
                        if text == "*" || safety::valid_id(&text) {
                            self.namespace = text;
                            self.namespace_override = true;
                            self.changed();
                        } else {
                            self.status = "Invalid namespace.".into();
                        }
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.modal = Modal::Input(label, text);
                }
                KeyCode::Char(c) => {
                    if !c.is_control() && text.len() < 100 {
                        text.push(c);
                    }
                    self.modal = Modal::Input(label, text);
                }
                _ => self.modal = Modal::Input(label, text),
            },
            Modal::Detail(text, mut offset) => match code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                _ => {
                    offset = scroll_offset(&text, offset, code);
                    self.modal = Modal::Detail(text, offset);
                }
            },
            Modal::Metrics(text, mut offset) => match code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Char('m') | KeyCode::Char('r') => {
                    self.modal = Modal::Metrics(text, offset);
                    self.request_metrics();
                }
                _ => {
                    offset = scroll_offset(&text, offset, code);
                    self.modal = Modal::Metrics(text, offset);
                }
            },
            Modal::Picker(label, list, mut index) => match code {
                KeyCode::Esc => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    index = (index + 1).min(list.len().saturating_sub(1));
                    self.modal = Modal::Picker(label, list, index);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal = Modal::Picker(label, list, index.saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(value) = list.get(index) {
                        if label == "Actions" {
                            let keys = [
                                KeyCode::Enter,
                                KeyCode::Char('y'),
                                KeyCode::Char('l'),
                                KeyCode::Char('L'),
                                KeyCode::Char('f'),
                                KeyCode::Char('m'),
                                KeyCode::Char('o'),
                                KeyCode::Char('s'),
                                KeyCode::Char('e'),
                                KeyCode::Char('x'),
                                KeyCode::Delete,
                            ];
                            if let Some(key) = keys.get(index) {
                                return self.key(*key);
                            }
                        } else if label == "Context" {
                            if !self.busy {
                                self.options.connection.context = Some(value.clone());
                                self.connect();
                            } else {
                                self.status =
                                    "Wait for the current request before changing context.".into();
                            }
                        } else {
                            self.namespace = value.clone();
                            self.namespace_override = true;
                            self.changed();
                        }
                    }
                }
                _ => self.modal = Modal::Picker(label, list, index),
            },
            Modal::Themes(list, mut index, old) => match code {
                KeyCode::Esc => self.theme = old,
                KeyCode::Down | KeyCode::Char('j') => {
                    index = (index + 1).min(list.len().saturating_sub(1));
                    if let Some(t) = list.get(index) {
                        self.theme = t.clone();
                    }
                    self.modal = Modal::Themes(list, index, old);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    index = index.saturating_sub(1);
                    if let Some(t) = list.get(index) {
                        self.theme = t.clone();
                    }
                    self.modal = Modal::Themes(list, index, old);
                }
                KeyCode::Enter => {
                    themes::use_theme(&self.theme.id)?;
                    self.status = format!("Theme saved: {}", self.theme.name);
                }
                KeyCode::Char('N') => {
                    self.modal = Modal::NewTheme(String::new(), self.theme.clone(), old);
                }
                _ => self.modal = Modal::Themes(list, index, old),
            },
            Modal::NewTheme(mut id, source, old) => match code {
                KeyCode::Esc => self.theme = old,
                KeyCode::Enter => {
                    let result = themes::create_from_theme(&id, &source).and_then(|path| {
                        themes::use_theme(&id)?;
                        Ok(path)
                    });
                    match result {
                        Ok(_) => {
                            self.theme = themes::find(&id)?;
                            self.status = format!("Custom theme created and selected: {id}");
                        }
                        Err(error) => {
                            self.status = error;
                            self.modal = Modal::NewTheme(id, source, old);
                        }
                    }
                }
                KeyCode::Backspace => {
                    id.pop();
                    self.modal = Modal::NewTheme(id, source, old);
                }
                KeyCode::Char(c) if c.is_ascii_alphanumeric() || c == '-' => {
                    if id.len() < 48 {
                        id.push(c);
                    }
                    self.modal = Modal::NewTheme(id, source, old);
                }
                _ => self.modal = Modal::NewTheme(id, source, old),
            },
            Modal::Confirm(p, mut text) => match code {
                KeyCode::Esc => self.status = "Operation canceled.".into(),
                KeyCode::Enter => {
                    let (width, height) = terminal::size().map_err(|_| "terminal size failed")?;
                    if !confirmation_fits(&p.summary, &text, width, height) {
                        self.status="Enlarge terminal to inspect the complete confirmation before approving.".into();
                        self.modal = Modal::Confirm(p, text);
                        return Ok(false);
                    }
                    match Approval::confirm(p.target.clone(), p.action.clone(), &text) {
                        Ok(a) => self.send(Task::Execute(p, a)),
                        Err(e) => {
                            self.status = e;
                            self.modal = Modal::Confirm(p, text);
                        }
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.modal = Modal::Confirm(p, text);
                }
                KeyCode::Char(c) => {
                    if c.is_ascii_graphic() && text.len() < 320 {
                        text.push(c);
                    }
                    self.modal = Modal::Confirm(p, text);
                }
                _ => self.modal = Modal::Confirm(p, text),
            },
            Modal::Scale(k, n, name, uid, mut text) => match code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    if let Ok(replicas) = text.parse() {
                        self.send(Task::Prepare(k, n, name, uid, Action::Scale(replicas)));
                    } else {
                        self.status = "Enter replicas 0..20".into();
                    }
                }
                KeyCode::Char(c) if c.is_ascii_digit() && text.len() < 2 => {
                    text.push(c);
                    self.modal = Modal::Scale(k, n, name, uid, text);
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.modal = Modal::Scale(k, n, name, uid, text);
                }
                _ => self.modal = Modal::Scale(k, n, name, uid, text),
            },
            Modal::Help(offset) => match code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    self.modal = Modal::Help((offset + 1).min(100))
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal = Modal::Help(offset.saturating_sub(1))
                }
                KeyCode::PageDown => self.modal = Modal::Help((offset + 15).min(100)),
                KeyCode::PageUp => self.modal = Modal::Help(offset.saturating_sub(15)),
                _ => self.modal = Modal::Help(offset),
            },
            Modal::None => match code {
                KeyCode::Char('q') => return Ok(true),
                KeyCode::Esc => {
                    self.filter.clear();
                    self.selected = 0;
                }
                KeyCode::Tab => self.left_focus = !self.left_focus,
                KeyCode::BackTab => self.left_focus = !self.left_focus,
                KeyCode::PageDown | KeyCode::PageUp | KeyCode::Home | KeyCode::End => {
                    let page = terminal::size()
                        .map(|(w, h)| Layout::new(w, h).table_rows())
                        .unwrap_or(15)
                        .max(1);
                    let (index, len) = if self.left_focus {
                        (&mut self.kind_index, self.kinds.len())
                    } else {
                        let len = self.visible().len();
                        (&mut self.selected, len)
                    };
                    *index = match code {
                        KeyCode::PageDown => index.saturating_add(page).min(len.saturating_sub(1)),
                        KeyCode::PageUp => index.saturating_sub(page),
                        KeyCode::Home => 0,
                        _ => len.saturating_sub(1),
                    };
                    if self.left_focus {
                        self.changed();
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.left_focus {
                        let index = (self.kind_index + 1).min(self.kinds.len().saturating_sub(1));
                        if index != self.kind_index {
                            self.kind_index = index;
                            self.changed();
                        }
                    } else {
                        self.selected =
                            (self.selected + 1).min(self.visible().len().saturating_sub(1));
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.left_focus {
                        let index = self.kind_index.saturating_sub(1);
                        if index != self.kind_index {
                            self.kind_index = index;
                            self.changed();
                        }
                    } else {
                        self.selected = self.selected.saturating_sub(1);
                    }
                }
                KeyCode::Enter => {
                    if self.left_focus {
                        self.left_focus = false;
                    } else if let Some((k, n, name)) = self.selected_target() {
                        self.send(Task::Get(k, n, name));
                    }
                }
                KeyCode::Char('/') => {
                    self.modal = Modal::Filter(
                        self.filter.clone(),
                        self.filter.clone(),
                        self.selected_uid(),
                        self.selected,
                    )
                }
                KeyCode::Char(':') => self.modal = Modal::Input("Command", String::new()),
                KeyCode::Char('S') => self.set_sort(self.sort.next()),
                KeyCode::Char('n') => {
                    self.modal = Modal::Input("Namespace", self.namespace.clone())
                }
                KeyCode::Char('c') => {
                    let list = contexts(&self.options.connection)?;
                    let index = list.iter().position(|c| c == &self.context).unwrap_or(0);
                    self.modal = Modal::Picker("Context", list, index);
                }
                KeyCode::Char('t') => self.theme_picker()?,
                KeyCode::Char('?') => self.modal = Modal::Help(0),
                KeyCode::Char('r') => {
                    if self.watch_blocked {
                        self.options.connection.context = Some(self.context.clone());
                        self.connect();
                        self.namespace_override = true;
                    } else if self.connected {
                        self.refresh = true;
                    } else if !self.busy {
                        self.connect();
                    }
                }
                KeyCode::Char('R') => self.send(Task::Discover),
                KeyCode::Char('m') => self.request_metrics(),
                KeyCode::Char('w') => {
                    if self.options.connection.demo {
                        self.status =
                            "DEMO uses synthetic polling; live watches require a cluster.".into();
                    } else {
                        self.options.poll = !self.options.poll;
                        self.changed();
                    }
                }
                KeyCode::Char('a') => {
                    self.modal = Modal::Picker("Actions", self.action_menu(), 0);
                }
                KeyCode::Char('y') => {
                    if let Some((k, n, name)) = self.selected_target() {
                        self.send(Task::Yaml(k, n, name));
                    }
                }
                KeyCode::Char('o') => {
                    self.modal = Modal::Input(
                        "Overview contexts (comma separated, max 4)",
                        self.context.clone(),
                    );
                }
                KeyCode::Char('e') => {
                    if !self.options.connection.writable {
                        self.status =
                            "READ ONLY: enable --write before requesting a mutation.".into();
                    } else if self.kind() != "deployments.apps" {
                        self.status = "Image edits apply to deployments.".into();
                    } else if let Some((k, n, name)) = self.selected_target() {
                        self.modal =
                            Modal::Image(k, n, name, self.selected_uid(), None, String::new());
                    }
                }
                KeyCode::Char('l') | KeyCode::Char('L') | KeyCode::Char('f') => {
                    if !self.options.connection.logs {
                        self.status =
                            "Logs may contain sensitive data; opt in with --allow-logs.".into();
                        return Ok(false);
                    }
                    if let Some((k, n, name)) = self.selected_target() {
                        if k == "pods" {
                            self.send(Task::Containers(
                                n,
                                name,
                                code == KeyCode::Char('L'),
                                code == KeyCode::Char('f'),
                            ));
                        } else {
                            self.status = "Logs apply to Pods; choose a pod.".into();
                        }
                    }
                }
                KeyCode::Char('s') => {
                    if !self.options.connection.writable {
                        self.status =
                            "READ ONLY: enable --write before requesting a mutation.".into();
                    } else if self.kind() != "deployments.apps" {
                        self.status = "Scale applies to deployments; select a deployment.".into();
                    } else if let Some((k, n, name)) = self.selected_target() {
                        self.modal = Modal::Scale(k, n, name, self.selected_uid(), String::new());
                    }
                }
                KeyCode::Char('x') => self.prepare(Action::Restart),
                KeyCode::Delete => self.prepare(Action::Delete),
                _ => {}
            },
        }
        Ok(false)
    }
}

fn resource_alias(resource: &str) -> &str {
    match resource {
        "po" | "pod" => "pods",
        "deploy" | "deployment" => "deployments.apps",
        "svc" | "service" => "services",
        "ev" | "event" => "events",
        "sts" => "statefulsets.apps",
        "ds" => "daemonsets.apps",
        "rs" => "replicasets.apps",
        "job" => "jobs.batch",
        "cj" => "cronjobs.batch",
        "ing" => "ingresses.networking.k8s.io",
        "cm" => "configmaps",
        "pvc" => "persistentvolumeclaims",
        "no" | "node" => "nodes",
        "ns" => "namespaces",
        "sc" => "storageclasses.storage.k8s.io",
        _ => resource,
    }
}
fn name(object: &Value) -> &str {
    object["metadata"]["name"].as_str().unwrap_or("-")
}
fn namespace(object: &Value) -> &str {
    object["metadata"]["namespace"].as_str().unwrap_or("-")
}
fn restarts(object: &Value) -> u64 {
    [
        "initContainerStatuses",
        "containerStatuses",
        "ephemeralContainerStatuses",
    ]
    .iter()
    .filter_map(|field| object["status"][field].as_array())
    .flatten()
    .filter_map(|status| status["restartCount"].as_u64())
    .fold(0, u64::saturating_add)
}
fn readiness(object: &Value) -> String {
    match object["kind"].as_str().unwrap_or("") {
        "Pod" => {
            let ready = object["status"]["containerStatuses"]
                .as_array()
                .map(|items| items.iter().filter(|item| item["ready"] == true).count())
                .unwrap_or(0);
            let total = object["spec"]["containers"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            format!("{ready}/{total}")
        }
        "Deployment" | "StatefulSet" | "ReplicaSet" => format!(
            "{}/{}",
            object["status"]["readyReplicas"].as_u64().unwrap_or(0),
            object["spec"]["replicas"].as_u64().unwrap_or(1)
        ),
        "DaemonSet" => format!(
            "{}/{}",
            object["status"]["numberReady"].as_u64().unwrap_or(0),
            object["status"]["desiredNumberScheduled"]
                .as_u64()
                .unwrap_or(0)
        ),
        "Job" => format!(
            "{}/{}",
            object["status"]["succeeded"].as_u64().unwrap_or(0),
            object["spec"]["completions"].as_u64().unwrap_or(1)
        ),
        "Node" => object["status"]["conditions"]
            .as_array()
            .and_then(|conditions| {
                conditions
                    .iter()
                    .find(|condition| condition["type"] == "Ready")
                    .and_then(|condition| condition["status"].as_str())
            })
            .unwrap_or("Unknown")
            .into(),
        _ => "-".into(),
    }
}
fn timestamp(object: &Value) -> Option<i64> {
    object["metadata"]["creationTimestamp"]
        .as_str()?
        .parse::<k8s_openapi::jiff::Timestamp>()
        .ok()
        .map(|time| time.as_second())
}
fn age(object: &Value, now: i64) -> String {
    let Some(created) = timestamp(object) else {
        return "-".into();
    };
    if created > now {
        return "future".into();
    }
    let seconds = now.saturating_sub(created);
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}
#[derive(Clone, Copy, Debug)]
struct Rect {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}
#[derive(Clone, Copy, Debug)]
struct Layout {
    sidebar: Option<Rect>,
    table: Rect,
}
impl Layout {
    fn new(width: u16, height: u16) -> Self {
        let sidebar_width = if width >= 88 { 26 } else { 0 };
        Self {
            sidebar: (sidebar_width > 0).then_some(Rect {
                x: 0,
                y: 3,
                width: sidebar_width,
                height: height.saturating_sub(6),
            }),
            table: Rect {
                x: sidebar_width,
                y: 3,
                width: width.saturating_sub(sidebar_width),
                height: height.saturating_sub(6),
            },
        }
    }
    fn table_rows(self) -> usize {
        usize::from(self.table.height.saturating_sub(3))
    }
    fn kind_rows(self) -> usize {
        usize::from(self.table.height.saturating_sub(2))
    }
}
#[derive(Clone, Copy)]
enum Column {
    Namespace,
    Name,
    Ready,
    Status,
    Restarts,
    Age,
}
impl Column {
    fn title(self) -> &'static str {
        match self {
            Self::Namespace => "NAMESPACE",
            Self::Name => "NAME",
            Self::Ready => "READY",
            Self::Status => "STATUS",
            Self::Restarts => "RSTS",
            Self::Age => "AGE",
        }
    }
    fn value(self, object: &Value, now: i64) -> String {
        match self {
            Self::Namespace => namespace(object).into(),
            Self::Name => name(object).into(),
            Self::Ready => readiness(object),
            Self::Status => summary(object),
            Self::Restarts => {
                if object["kind"] == "Pod" {
                    restarts(object).to_string()
                } else {
                    "-".into()
                }
            }
            Self::Age => age(object, now),
        }
    }
}
fn columns(width: usize, namespaced: bool) -> Vec<(Column, usize)> {
    // Name and health remain usable even when optional columns no longer fit.
    let mut columns = Vec::new();
    if namespaced && width >= 54 {
        columns.push((Column::Namespace, if width >= 94 { 16 } else { 12 }));
    }
    columns.push((Column::Name, 1));
    if width >= 46 {
        columns.push((Column::Ready, 7));
    }
    columns.push((
        Column::Status,
        if width >= 64 {
            20
        } else if width >= 46 {
            16
        } else {
            12
        },
    ));
    if width >= 70 {
        columns.push((Column::Restarts, 5));
    }
    if width >= 46 {
        columns.push((Column::Age, 6));
    }
    let occupied: usize =
        columns.iter().map(|(_, width)| width).sum::<usize>() + columns.len().saturating_sub(1);
    if let Some((_, name_width)) = columns
        .iter_mut()
        .find(|(column, _)| matches!(column, Column::Name))
    {
        *name_width += width.saturating_sub(occupied);
    }
    columns
}
fn table_line(columns: &[(Column, usize)], object: Option<&Value>, now: i64) -> String {
    columns
        .iter()
        .map(|(column, width)| {
            let text = object
                .map(|object| column.value(object, now))
                .unwrap_or_else(|| column.title().into());
            format!("{:<width$}", display(&text, *width), width = *width)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn display(text: &str, width: usize) -> String {
    let mut text: String = safety::safe_text(text, width.saturating_add(1))
        .chars()
        .map(|c| if c.is_ascii() { c } else { '?' })
        .collect();
    if text.len() > width {
        text.truncate(width);
        if width > 0 {
            text.pop();
            text.push('~');
        }
    }
    text
}
fn identity_line(context: &str, namespace: &str, server: &str, width: usize) -> String {
    let server_width = if width >= 110 { 32 } else { 0 };
    let available = width.saturating_sub(10 + server_width);
    let namespace_width = (available / 3).clamp(6, 24).min(available);
    let context_width = available.saturating_sub(namespace_width);
    let mut line = format!(
        "CTX {} | NS {}",
        display(context, context_width),
        display(namespace, namespace_width)
    );
    if server_width > 0 {
        line.push_str(&format!(" | API {}", display(server, server_width - 7)));
    }
    line
}
fn row(
    out: &mut impl Write,
    y: u16,
    width: u16,
    text: &str,
    fg: Color,
    bg: Color,
) -> io::Result<()> {
    queue!(
        out,
        cursor::MoveTo(0, y),
        themes::ForegroundColor(fg),
        themes::BackgroundColor(bg),
        style::Print(format!(
            "{:<width$}",
            display(text, width as usize),
            width = width as usize
        ))
    )
}
fn cell(
    out: &mut impl Write,
    x: u16,
    y: u16,
    width: u16,
    text: &str,
    fg: Color,
    bg: Color,
) -> io::Result<()> {
    queue!(
        out,
        cursor::MoveTo(x, y),
        themes::ForegroundColor(fg),
        themes::BackgroundColor(bg),
        style::Print(format!(
            "{:<width$}",
            display(text, usize::from(width)),
            width = usize::from(width)
        ))
    )
}
fn panel(
    out: &mut impl Write,
    rect: Rect,
    title: &str,
    focused: bool,
    fg: Color,
    bg: Color,
) -> io::Result<()> {
    if rect.width < 2 || rect.height < 2 {
        return Ok(());
    }
    let width = usize::from(rect.width - 2);
    let title = display(
        &format!(" {} {title} ", if focused { ">" } else { " " }),
        width,
    );
    let top = format!("+{title}{}+", "-".repeat(width.saturating_sub(title.len())));
    cell(out, rect.x, rect.y, rect.width, &top, fg, bg)?;
    for y in rect.y + 1..rect.y + rect.height - 1 {
        cell(
            out,
            rect.x,
            y,
            rect.width,
            &format!("|{}|", " ".repeat(width)),
            fg,
            bg,
        )?;
    }
    cell(
        out,
        rect.x,
        rect.y + rect.height - 1,
        rect.width,
        &format!("+{}+", "-".repeat(width)),
        fg,
        bg,
    )
}
fn draw(app: &App) -> Result<()> {
    let (w, h) = terminal::size().map_err(|_| "cannot read terminal size")?;
    render(app, w, h, &mut io::stdout().lock())
}
fn render(app: &App, w: u16, h: u16, out: &mut impl Write) -> Result<()> {
    let t = &app.theme;
    let color = |role: &str| {
        if app.options.color && !t.terminal {
            t.get(role).color()
        } else {
            Color::Reset
        }
    };
    let bg = color("background");
    let fg = color("text");
    queue!(
        out,
        themes::BackgroundColor(bg),
        terminal::Clear(ClearType::All)
    )
    .map_err(|_| "draw failed")?;
    if w < 28 || h < 10 {
        row(out, 0, w, "KBorg: needs 28x10; q quits", fg, bg).map_err(|_| "draw failed")?;
        if h > 1 {
            row(out, 1, w, &app.status, color("warning"), bg).map_err(|_| "draw failed")?;
        }
        out.flush().map_err(|_| "draw failed")?;
        return Ok(());
    }
    let mode = if app.options.connection.demo {
        "DEMO SAMPLE DATA"
    } else if app.options.connection.writable {
        "PRODUCTION WRITE"
    } else {
        "PRODUCTION READ ONLY"
    };
    let header = format!(
        "KBorg | {mode}{}",
        if w >= 80 {
            format!(" | {}", app.theme.name)
        } else {
            String::new()
        }
    );
    row(
        out,
        0,
        w,
        &header,
        color("status_text"),
        color("status_background"),
    )
    .map_err(|_| "draw failed")?;
    row(
        out,
        1,
        w,
        &identity_line(&app.context, &app.namespace, &app.server, usize::from(w)),
        color("accent"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    let freshness = if app.busy {
        "LOADING"
    } else if app.stale {
        "STALE"
    } else if app.options.connection.demo {
        "SAMPLE"
    } else if app.options.poll {
        "POLL"
    } else if app.watching {
        "LIVE"
    } else {
        "CACHED"
    };
    let visible = app.visible();
    row(
        out,
        2,
        w,
        &format!(
            "{freshness}{} | {} | {}/{} objects | sort:{}{}",
            if app.truncated { " TRUNCATED" } else { "" },
            app.kind(),
            visible.len(),
            app.items.len(),
            app.sort.label(),
            if app.filter.is_empty() {
                String::new()
            } else {
                format!(" | /{}", app.filter)
            }
        ),
        if app.stale || app.truncated {
            color("warning")
        } else {
            color("muted")
        },
        bg,
    )
    .map_err(|_| "draw failed")?;
    let layout = Layout::new(w, h);
    let height = layout.table_rows();
    let sidebar = layout
        .sidebar
        .or_else(|| app.left_focus.then_some(layout.table));
    if let Some(rect) = sidebar {
        panel(
            out,
            rect,
            "RESOURCES [Tab]",
            app.left_focus,
            if app.left_focus {
                color("accent")
            } else {
                color("muted")
            },
            bg,
        )
        .map_err(|_| "draw failed")?;
        let rows = layout.kind_rows();
        let start = app.kind_index.saturating_sub(rows.saturating_sub(1));
        for (r, kind) in app.kinds.iter().skip(start).take(rows).enumerate() {
            let selected = start + r == app.kind_index;
            cell(
                out,
                rect.x + 1,
                rect.y + 1 + r as u16,
                rect.width - 2,
                &format!("{} {}", if selected { ">" } else { " " }, kind.key()),
                if selected {
                    color("selection_text")
                } else {
                    fg
                },
                if selected {
                    color("selection_background")
                } else {
                    bg
                },
            )
            .map_err(|_| "draw failed")?;
        }
    }
    if layout.sidebar.is_some() || !app.left_focus {
        let rect = layout.table;
        let title = format!(
            "{} [{}/{}]{}",
            app.kind(),
            if visible.is_empty() {
                0
            } else {
                app.selected + 1
            },
            visible.len(),
            if app.filter.is_empty() {
                ""
            } else {
                " FILTERED"
            }
        );
        panel(
            out,
            rect,
            &title,
            !app.left_focus,
            if app.left_focus {
                color("muted")
            } else {
                color("accent")
            },
            bg,
        )
        .map_err(|_| "draw failed")?;
        let namespaced = app
            .kinds
            .get(app.kind_index)
            .is_some_and(|kind| kind.namespaced);
        let cols = columns(usize::from(rect.width - 4), namespaced);
        let now = k8s_openapi::jiff::Timestamp::now().as_second();
        cell(
            out,
            rect.x + 1,
            rect.y + 1,
            rect.width - 2,
            &format!("  {}", table_line(&cols, None, now)),
            color("muted"),
            bg,
        )
        .map_err(|_| "draw failed")?;
        let start = app.selected.saturating_sub(height.saturating_sub(1));
        for (r, object) in visible.iter().skip(start).take(height).enumerate() {
            let selected = !app.left_focus && start + r == app.selected;
            let status = summary(object);
            let unhealthy = [
                "Failed",
                "Error",
                "Crash",
                "BackOff",
                "NotReady",
                "Terminating",
                "Pending",
            ]
            .iter()
            .any(|needle| status.contains(needle));
            cell(
                out,
                rect.x + 1,
                rect.y + 2 + r as u16,
                rect.width - 2,
                &format!(
                    "{} {}",
                    if start + r == app.selected {
                        if app.left_focus { "*" } else { ">" }
                    } else {
                        " "
                    },
                    table_line(&cols, Some(object), now)
                ),
                if selected {
                    color("selection_text")
                } else if unhealthy {
                    color("warning")
                } else {
                    fg
                },
                if selected {
                    color("selection_background")
                } else {
                    bg
                },
            )
            .map_err(|_| "draw failed")?;
        }
        if visible.is_empty() && height > 0 {
            let empty = if !app.filter.is_empty() {
                "No matches. Esc clears the filter."
            } else if app.busy {
                "Loading resources..."
            } else if !app.connected {
                "Not connected. r retries; c selects context."
            } else if app.watch_blocked {
                "Access unavailable. r retries; c selects context."
            } else {
                "No objects in this scope. n namespace; :all"
            };
            cell(
                out,
                rect.x + 1,
                rect.y + 2,
                rect.width - 2,
                empty,
                color("muted"),
                bg,
            )
            .map_err(|_| "draw failed")?;
        }
    }
    row(
        out,
        h - 3,
        w,
        &format!(
            "{}{}",
            if app.sticky_status { "! " } else { "" },
            app.status
        ),
        if app.stale || app.sticky_status {
            color("warning")
        } else {
            color("muted")
        },
        bg,
    )
    .map_err(|_| "draw failed")?;
    row(
        out,
        h - 2,
        w,
        if w >= 80 {
            "Enter details | y YAML | l logs | f follow | m metrics | a actions | / filter"
        } else {
            "Enter inspect | / filter | : command | a actions"
        },
        color("accent"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    row(out, h - 1, w, if w >= 80 { ": resource | S sort | Tab focus | c context | n namespace | r refresh | ? help | q quit" }
        else { "q quit | ? help | Tab focus" }, color("muted"), bg).map_err(|_| "draw failed")?;
    match &app.modal {
        Modal::None => {}
        Modal::Filter(_, text, ..) => {
            row(
                out,
                h - 2,
                w,
                &format!("Filter > {text}_"),
                color("accent"),
                bg,
            )
            .map_err(|_| "draw failed")?;
            row(
                out,
                h - 1,
                w,
                "Enter keep | Esc restore | live filter",
                color("muted"),
                bg,
            )
            .map_err(|_| "draw failed")?;
        }
        modal => {
            let title = match modal {
                Modal::Input(label, _) | Modal::Picker(label, ..) => *label,
                Modal::Follow(..) => "LIVE LOGS / 60s / 256KiB",
                Modal::Containers(..) => "CHOOSE CONTAINER",
                Modal::Image(..) => "CHANGE DEPLOYMENT IMAGE",
                Modal::Detail(..) => "INSPECT / PgUp PgDown / Esc close",
                Modal::Metrics(..) => "METRICS / refresh 5s / m refresh / Esc close",
                Modal::Themes(..) => "THEMES / Enter save / Esc restore",
                Modal::NewTheme(..) => "CREATE CUSTOM THEME / from preview colors",
                Modal::Confirm(..) => "REVIEW MUTATION / explicit confirmation required",
                Modal::Scale(..) => "SCALE DEPLOYMENT",
                Modal::Help(..) => "HELP / PgUp PgDown / Esc close",
                Modal::None => "",
                Modal::Filter(..) => "",
            };
            let rect = Rect {
                x: 0,
                y: 3,
                width: w,
                height: h.saturating_sub(6),
            };
            panel(
                out,
                rect,
                title,
                true,
                if matches!(modal, Modal::Confirm(..)) {
                    color("warning")
                } else {
                    color("accent")
                },
                bg,
            )
            .map_err(|_| "draw failed")?;
            let height = usize::from(rect.height.saturating_sub(2));
            let lines = match modal {
                Modal::Input(label, text) => vec![
                    format!(" {label}: {text}_"),
                    " Enter apply | Esc cancel".into(),
                ],
                Modal::Follow(text, offset)
                | Modal::Detail(text, offset)
                | Modal::Metrics(text, offset) => {
                    let wrapped = wrap_text(text, usize::from(w.saturating_sub(2)));
                    let offset = (*offset).min(wrapped.lines().count().saturating_sub(1));
                    wrapped.lines().skip(offset).map(str::to_owned).collect()
                }
                Modal::Containers(_, name, names, index, previous, follow) => {
                    std::iter::once(format!(
                        " CONTAINER for {name} — {} — Enter select / Esc cancel",
                        if *follow {
                            "follow"
                        } else if *previous {
                            "previous"
                        } else {
                            "snapshot"
                        }
                    ))
                    .chain(
                        names
                            .iter()
                            .enumerate()
                            .skip(index.saturating_sub(height.saturating_sub(2)))
                            .map(|(i, name)| {
                                format!(" {} {name}", if i == *index { ">" } else { " " })
                            }),
                    )
                    .collect()
                }
                Modal::Image(_, _, name, _, container, text) => vec![
                    format!(
                        " CHANGE IMAGE {name} — {}",
                        container.as_deref().unwrap_or("enter container name")
                    ),
                    format!(
                        " {}: {text}_",
                        if container.is_some() {
                            "Image (tag/digest)"
                        } else {
                            "Container"
                        }
                    ),
                    " Enter continue to server dry-run/review | Esc cancel".into(),
                ],
                Modal::Picker(label, list, index) => {
                    std::iter::once(format!(" {label} — arrows / Enter select / Esc cancel"))
                        .chain(
                            list.iter()
                                .enumerate()
                                .skip(index.saturating_sub(height.saturating_sub(2)))
                                .map(|(i, s)| {
                                    format!(" {} {s}", if i == *index { ">" } else { " " })
                                }),
                        )
                        .collect()
                }
                Modal::Themes(list, index, _) => {
                    let mut lines = vec![
                        " THEMES - arrows preview; Enter save; N create custom; Esc restore".into(),
                        " PREVIEW: PRODUCTION | READ ONLY | STALE | ERROR | +added -removed".into(),
                    ];
                    lines.extend(
                        list.iter()
                            .enumerate()
                            .skip(index.saturating_sub(height.saturating_sub(3)))
                            .map(|(i, t)| {
                                format!(
                                    " {} {} ({})",
                                    if i == *index { ">" } else { " " },
                                    t.name,
                                    t.id
                                )
                            }),
                    );
                    lines
                }
                Modal::NewTheme(id, source, _) => vec![
                    format!("Palette: {}", source.name),
                    format!("New theme ID: {id}_"),
                    "lowercase letters, digits and hyphens; existing themes are preserved".into(),
                    "Enter create and select | Esc restore previous theme".into(),
                ],
                Modal::Confirm(p, text) => {
                    confirmation_lines(&p.summary, text, w.saturating_sub(1))
                }
                Modal::Scale(_, _, name, _, text) => vec![
                    format!(" SCALE {name} — enter replicas 0..20: {text}_"),
                    " Enter server dry-run/review | Esc cancel".into(),
                ],
                Modal::Help(offset) => {
                    std::iter::once(" HELP - PgUp/PgDown scroll; Esc close".into())
                        .chain(
                            include_str!("../docs/CLI_HELP.txt")
                                .lines()
                                .skip(*offset)
                                .map(|s| format!(" {s}")),
                        )
                        .collect()
                }
                Modal::None => vec![],
                Modal::Filter(..) => vec![],
            };
            for (index, line) in lines.iter().take(height).enumerate() {
                let selected = line.starts_with(" > ");
                cell(
                    out,
                    1,
                    4 + index as u16,
                    w.saturating_sub(2),
                    line,
                    if matches!(modal, Modal::Confirm(..)) {
                        color("warning")
                    } else if selected {
                        color("selection_text")
                    } else {
                        fg
                    },
                    if selected {
                        color("selection_background")
                    } else {
                        bg
                    },
                )
                .map_err(|_| "draw failed")?;
            }
            let hints = if let Modal::Confirm(prepared, text) = modal {
                if confirmation_fits(&prepared.summary, text, w, h) {
                    "Enter execute | Esc cancel"
                } else {
                    "Esc cancel | Enlarge terminal to review complete mutation."
                }
            } else if matches!(modal, Modal::Input("Command", _)) {
                "Esc cancel | Enter run | :pods :deploy :svc :nodes | :ns NAME | :sort FIELD"
            } else if matches!(modal, Modal::Metrics(..)) {
                "Esc close | m refresh | PgUp/PgDown scroll | Home/End"
            } else {
                "Esc close | Enter select | arrows / j k | PgUp PgDown scroll"
            };
            row(out, h - 2, w, hints, color("accent"), bg).map_err(|_| "draw failed")?;
            row(
                out,
                h - 1,
                w,
                &format!(
                    "{} | {freshness}",
                    if app.busy {
                        "Request in progress"
                    } else {
                        mode
                    }
                ),
                color("muted"),
                bg,
            )
            .map_err(|_| "draw failed")?;
        }
    }
    out.flush().map_err(|_| "draw failed".into())
}

fn confirmation_lines(summary: &str, text: &str, width: u16) -> Vec<String> {
    let content = format!("{summary}\nCONFIRM > {text}_\nEnter execute | Esc cancel");
    let available = usize::from(width).saturating_sub(2).max(1);
    content
        .lines()
        .flat_map(|line| {
            let chars: Vec<char> = line.chars().collect();
            if chars.is_empty() {
                return vec![" ".into()];
            }
            chars
                .chunks(available)
                .map(|chunk| format!(" {}", chunk.iter().collect::<String>()))
                .collect()
        })
        .collect()
}
fn confirmation_fits(summary: &str, text: &str, width: u16, height: u16) -> bool {
    width >= 28
        && height >= 10
        && confirmation_lines(summary, text, width.saturating_sub(1)).len()
            <= usize::from(height.saturating_sub(8))
}
fn scroll_offset(text: &str, offset: usize, code: KeyCode) -> usize {
    let (width, height) = terminal::size().unwrap_or((80, 24));
    let count = wrap_text(text, usize::from(width.saturating_sub(2)))
        .lines()
        .count();
    let page = usize::from(height.saturating_sub(8)).max(1);
    match code {
        KeyCode::Down | KeyCode::Char('j') => offset.saturating_add(1),
        KeyCode::Up | KeyCode::Char('k') => offset.saturating_sub(1),
        KeyCode::PageDown => offset.saturating_add(page),
        KeyCode::PageUp => offset.saturating_sub(page),
        KeyCode::Home => 0,
        KeyCode::End => count.saturating_sub(page),
        _ => offset,
    }
    .min(count.saturating_sub(1))
}
fn wrap_text(text: &str, width: usize) -> String {
    text.lines()
        .flat_map(|line| {
            let chars: Vec<char> = line.chars().collect();
            if chars.is_empty() {
                return vec![String::new()];
            }
            chars
                .chunks(width.max(1))
                .map(|chunk| chunk.iter().collect::<String>())
                .collect()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn run(options: Options) -> Result<()> {
    let first_run = options.theme.is_none() && themes::first_run()?;
    let theme = themes::find(options.theme.as_deref().unwrap_or(&themes::selected()))?;
    let _terminal = Terminal::enter(options.mouse)?;
    if options.splash && themes::animation_enabled() && !startup_animation(&theme, options.color)? {
        return Ok(());
    }
    let mut app = App {
        namespace: options.connection.namespace.clone(),
        context: options
            .connection
            .context
            .clone()
            .unwrap_or_else(|| "selected kubeconfig context".into()),
        server: "connecting...".into(),
        theme,
        options,
        kinds: crate::gateway::kinds(),
        kind_index: 0,
        items: vec![],
        selected: 0,
        filter: String::new(),
        sort: Sort::Name,
        left_focus: false,
        status: "Starting...".into(),
        stale: true,
        truncated: false,
        modal: Modal::None,
        epoch: 0,
        busy: false,
        command_epoch: None,
        connecting_epoch: None,
        watching: false,
        watch_blocked: false,
        sticky_status: false,
        namespace_override: false,
        write_inflight: false,
        connected: false,
        refresh: true,
        last_refresh: Instant::now(),
        metrics_history: crate::metrics::MetricsHistory::default(),
        last_metrics: Instant::now(),
        worker: worker(),
    };
    if first_run {
        themes::use_theme("catppuccin-mocha")?;
        app.theme_picker()?;
        app.status = "Welcome! Arrows preview, Enter chooses; Esc keeps Catppuccin Mocha.".into();
    }
    app.connect();
    draw(&app)?;
    let mut dirty = false;
    loop {
        if app.receive() {
            dirty = true;
        }
        if matches!(app.modal, Modal::Metrics(..))
            && !app.busy
            && app.last_metrics.elapsed() >= Duration::from_secs(5)
        {
            app.request_metrics();
            dirty = true;
        }
        if app.connected
            && !app.busy
            && !app.watch_blocked
            && matches!(app.modal, Modal::None)
            && (app.refresh
                || (!app.watching
                    && app.last_refresh.elapsed()
                        > Duration::from_secs(if app.stale { 10 } else { 5 })))
        {
            app.refresh = false;
            app.last_refresh = Instant::now();
            app.send(if app.options.poll || app.options.connection.demo {
                Task::List(app.kind(), app.namespace.clone())
            } else {
                Task::Watch(app.kind(), app.namespace.clone())
            });
            dirty = true;
        }
        // At most one redraw per event-loop tick; polling never blocks terminal input.
        if dirty {
            draw(&app)?;
            dirty = false;
        }
        if event::poll(Duration::from_millis(100)).map_err(|_| "input failed")? {
            match event::read().map_err(|_| "input failed")? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                    {
                        if app.write_inflight {
                            app.status =
                                "Mutation in progress; wait for the result before quitting.".into();
                            dirty = true;
                            continue;
                        }
                        break;
                    }
                    if app.key(key.code)? {
                        break;
                    }
                    dirty = true;
                }
                Event::Resize(_, _) => dirty = true,
                Event::Mouse(mouse) if matches!(app.modal, Modal::None) && !app.write_inflight => {
                    let (w, h) = terminal::size().map_err(|_| "terminal size failed")?;
                    let layout = Layout::new(w, h);
                    let resources = layout.sidebar.is_some_and(|rect| mouse.column < rect.width)
                        || (layout.sidebar.is_none() && app.left_focus);
                    match mouse.kind {
                        MouseEventKind::ScrollDown => {
                            if resources {
                                let index =
                                    (app.kind_index + 1).min(app.kinds.len().saturating_sub(1));
                                if index != app.kind_index {
                                    app.kind_index = index;
                                    app.changed();
                                }
                            } else {
                                app.selected =
                                    (app.selected + 1).min(app.visible().len().saturating_sub(1));
                            }
                        }
                        MouseEventKind::ScrollUp => {
                            if resources {
                                let index = app.kind_index.saturating_sub(1);
                                if index != app.kind_index {
                                    app.kind_index = index;
                                    app.changed();
                                }
                            } else {
                                app.selected = app.selected.saturating_sub(1);
                            }
                        }
                        MouseEventKind::Down(_) => {
                            let top = if resources { 4 } else { 5 };
                            if mouse.row >= top && mouse.row < h.saturating_sub(4) {
                                let row = usize::from(mouse.row - top);
                                if resources {
                                    let start = app
                                        .kind_index
                                        .saturating_sub(layout.kind_rows().saturating_sub(1));
                                    app.kind_index =
                                        (start + row).min(app.kinds.len().saturating_sub(1));
                                    app.left_focus = false;
                                    app.changed();
                                } else {
                                    let start = app
                                        .selected
                                        .saturating_sub(layout.table_rows().saturating_sub(1));
                                    app.selected =
                                        (start + row).min(app.visible().len().saturating_sub(1));
                                    app.left_focus = false;
                                }
                            }
                        }
                        _ => {}
                    }
                    dirty = true;
                }
                _ => {}
            }
        }
        // Render when new worker state arrives as well as on user input.
        if app.receive() {
            dirty = true;
        }
    }
    Ok(())
}

fn startup_animation(theme: &Theme, color: bool) -> Result<bool> {
    const LOGO: &[&str] = &[
        "            .--------.            ",
        "        .--'    |     '--.        ",
        "       /     ---+---      \\       ",
        "      |    /    |    \\     |      ",
        "      | --+----[O]----+--  |      ",
        "      |    \\    |    /     |      ",
        "       \\     ---+---      /       ",
        "        '--.    |     .--'        ",
        "            '--------'            ",
        "             KBORG               ",
        " Kubernetes under your control    ",
    ];
    let started = Instant::now();
    let mut frame = 0usize;
    while started.elapsed() < Duration::from_secs(2) {
        let (width, height) = terminal::size().map_err(|_| "terminal size failed")?;
        if width < 40 || height < 16 {
            return Ok(true);
        }
        let mut out = io::stdout();
        queue!(out, terminal::Clear(ClearType::All)).map_err(|_| "startup draw failed")?;
        let bg = if color {
            theme.get("background").color()
        } else {
            Color::Reset
        };
        let fg = if color {
            theme.get("accent").color()
        } else {
            Color::Reset
        };
        let y = height.saturating_sub(14) / 2;
        for (index, line) in LOGO.iter().enumerate() {
            let line = line.replace("[O]", &format!("[{}]", ["|", "/", "-", "\\"][frame % 4]));
            let padding = usize::from(width).saturating_sub(line.len()) / 2;
            row(
                &mut out,
                y + index as u16,
                width,
                &format!("{}{line}", " ".repeat(padding)),
                fg,
                bg,
            )
            .map_err(|_| "startup draw failed")?;
        }
        let progress = format!(
            "{}  Starting...  (any key skips)",
            ["|", "/", "-", "\\"][frame % 4]
        );
        row(
            &mut out,
            y + 12,
            width,
            &format!(
                "{}{}",
                " ".repeat(usize::from(width).saturating_sub(progress.len()) / 2),
                progress
            ),
            fg,
            bg,
        )
        .map_err(|_| "startup draw failed")?;
        out.flush().map_err(|_| "startup draw failed")?;
        frame += 1;
        if event::poll(Duration::from_millis(100)).map_err(|_| "startup input failed")?
            && let Event::Key(key) = event::read().map_err(|_| "startup input failed")?
            && key.kind == KeyEventKind::Press
        {
            return Ok(
                !(key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
            );
        }
    }
    Ok(true)
}

pub fn preview(theme: Theme, color: bool) -> Result<()> {
    if !io::stdout().is_terminal() {
        println!("{} ({}) — validated palette", theme.name, theme.id);
        return Ok(());
    }
    let mut out = io::stdout();
    let bg = if color {
        theme.get("background").color()
    } else {
        Color::Reset
    };
    for (role, text) in [
        ("text", "Resource: demo/api — 2/2 ready"),
        ("success", "SUCCESS rollout healthy"),
        ("warning", "PRODUCTION / READ ONLY / STALE"),
        ("danger", "ERROR destructive operation requires approval"),
        ("accent", "FOCUS > details | +added -removed"),
    ] {
        queue!(
            out,
            themes::ForegroundColor(if color {
                theme.get(role).color()
            } else {
                Color::Reset
            }),
            themes::BackgroundColor(bg),
            style::Print(format!("{text}\n"))
        )
        .map_err(|_| "preview failed")?;
    }
    execute!(out, style::ResetColor).map_err(|_| "preview failed".into())
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    type Harness = (
        App,
        mpsc::SyncSender<(u64, Reply)>,
        tokio::sync::mpsc::Receiver<(u64, Task)>,
    );
    fn harness() -> Harness {
        let (tx, commands) = tokio::sync::mpsc::channel(4);
        let (replies, rx) = mpsc::sync_channel(4);
        let watch = std::sync::Arc::new(WatchSlot::default());
        watch.invalidate(12);
        (
            App {
                options: Options {
                    connection: Connection::default(),
                    theme: None,
                    color: false,
                    mouse: false,
                    json: false,
                    splash: false,
                    poll: false,
                    yaml: false,
                    follow: false,
                    previous: false,
                    allow_tools: false,
                    command: vec![],
                },
                theme: themes::builtins().remove(0),
                context: "test".into(),
                server: "test".into(),
                namespace: "test".into(),
                kinds: crate::gateway::kinds(),
                kind_index: 0,
                items: vec![],
                selected: 0,
                filter: String::new(),
                sort: Sort::Name,
                left_focus: false,
                status: "waiting".into(),
                stale: false,
                truncated: false,
                modal: Modal::None,
                epoch: 12,
                busy: false,
                command_epoch: None,
                connecting_epoch: None,
                watching: false,
                watch_blocked: false,
                sticky_status: false,
                namespace_override: false,
                write_inflight: false,
                connected: true,
                refresh: false,
                last_refresh: Instant::now(),
                metrics_history: crate::metrics::MetricsHistory::default(),
                last_metrics: Instant::now(),
                worker: Worker {
                    tx,
                    rx,
                    watch,
                    logs: std::sync::Arc::new(LogSlot::default()),
                },
            },
            replies,
            commands,
        )
    }
    fn object(name: &str) -> Value {
        serde_json::json!({"kind":"Deployment","metadata":{"name":name,"namespace":"test","uid":format!("uid-{name}")}})
    }
    fn screen(app: &App, width: u16, height: u16) -> Vec<String> {
        let mut output = Vec::new();
        assert!(render(app, width, height, &mut output).is_ok());
        let mut cells = vec![vec![' '; usize::from(width)]; usize::from(height)];
        let (mut x, mut y, mut index) = (0usize, 0usize, 0usize);
        while index < output.len() {
            if output[index] == 27 {
                assert_eq!(output[index + 1], b'[');
                let start = index + 2;
                index = start;
                while index < output.len() && !output[index].is_ascii_alphabetic() {
                    index += 1;
                }
                let parameters = std::str::from_utf8(&output[start..index]).unwrap_or_default();
                match output[index] {
                    b'H' => {
                        let mut parameters = parameters
                            .split(';')
                            .map(|value| value.parse::<usize>().unwrap_or(1));
                        y = parameters.next().unwrap_or(1).saturating_sub(1);
                        x = parameters.next().unwrap_or(1).saturating_sub(1);
                    }
                    b'J' => cells.iter_mut().for_each(|row| row.fill(' ')),
                    b'm' => {}
                    other => panic!("unexpected terminal command: {other}"),
                }
            } else {
                assert!(output[index].is_ascii_graphic() || output[index] == b' ');
                assert!(
                    x < usize::from(width) && y < usize::from(height),
                    "paint outside {width}x{height}: {x},{y}"
                );
                cells[y][x] = char::from(output[index]);
                x += 1;
            }
            index += 1;
        }
        cells
            .into_iter()
            .map(|row| row.into_iter().collect())
            .collect()
    }
    #[test]
    fn responsive_frames_preserve_identity_mode_and_bounds() {
        let (mut app, _replies, _commands) = harness();
        app.items = vec![
            serde_json::json!({"kind":"Pod","metadata":{"name":"api","namespace":"test","uid":"pod1","creationTimestamp":"2026-10-10T07:00:00Z"},"spec":{"containers":[{"name":"app"}]},"status":{"phase":"Running","containerStatuses":[{"ready":true,"restartCount":4}]}}),
        ];
        for width in [28, 40, 60, 80, 88, 120] {
            for height in [10, 12, 24] {
                for focus in [false, true] {
                    app.left_focus = focus;
                    let frame = screen(&app, width, height);
                    assert!(frame[0].contains("PRODUCTION READ ONLY"));
                    assert!(frame[1].contains("CTX test | NS test"));
                    assert!(frame[3].starts_with('+'));
                }
            }
        }
        app.left_focus = false;
        let frame = screen(&app, 120, 24).join("\n");
        for heading in ["NAMESPACE", "NAME", "READY", "STATUS", "RSTS", "AGE"] {
            assert!(frame.contains(heading));
        }
        assert!(frame.contains("1/1") && frame.contains("Running"));
        app.options.connection.writable = true;
        assert!(screen(&app, 28, 10)[0].contains("PRODUCTION WRITE"));
        app.options.connection.demo = true;
        assert!(screen(&app, 28, 10)[0].contains("DEMO SAMPLE DATA"));
    }
    #[test]
    fn resize_reflows_details_and_confirmation_cannot_approve_hidden_review() {
        let (mut app, _replies, _commands) = harness();
        let text = "ABCDEFGHIJ".repeat(7);
        app.modal = Modal::Detail(text.clone(), 0);
        let frame = screen(&app, 28, 12);
        let displayed: String = frame
            .iter()
            .skip(4)
            .take(4)
            .map(|line| line[1..27].trim_end())
            .collect();
        assert!(displayed.starts_with(&text));
        assert!(confirmation_fits("Short review", "test/api", 80, 24));
        assert!(!confirmation_fits(
            &"long review ".repeat(30),
            "test/api",
            28,
            12
        ));
        assert!(!confirmation_fits("Review", "test/api", 20, 8));
        app.modal = Modal::Metrics("CPU: |.-#|\nMEM: |--#|".into(), 0);
        assert!(screen(&app, 60, 12).join("\n").contains("CPU: |.-#|"));
        app.modal = Modal::Containers(
            "test".into(),
            "api".into(),
            (0..20).map(|index| format!("container-{index}")).collect(),
            19,
            false,
            false,
        );
        assert!(screen(&app, 40, 12).join("\n").contains("> container-19"));
    }
    #[test]
    fn commands_navigate_aliases_scopes_and_sort_without_issuing_mutations() {
        let (mut app, _replies, mut commands) = harness();
        let epoch = app.epoch;
        assert_eq!(app.command(":deploy"), Ok(false));
        assert_eq!(app.kind(), "deployments.apps");
        assert_eq!(app.epoch, epoch + 1);
        assert_eq!(app.command("ns other"), Ok(false));
        assert_eq!(app.namespace, "other");
        assert_eq!(app.command("all"), Ok(false));
        assert_eq!(app.namespace, "*");
        assert_eq!(app.command("sort restarts"), Ok(false));
        assert_eq!(app.sort, Sort::Restarts);
        assert_eq!(app.command("pods; rm -rf /"), Ok(false));
        assert_eq!(app.kind(), "deployments.apps");
        assert!(commands.try_recv().is_err());
        assert_eq!(app.command("q"), Ok(true));
    }
    #[test]
    fn readiness_restarts_age_and_sort_use_real_fields_and_keep_uid() {
        let (mut app, _replies, _commands) = harness();
        let mut pod = object("z");
        pod["kind"] = "Pod".into();
        pod["spec"] = serde_json::json!({"containers":[{"name":"app"},{"name":"sidecar"}]});
        pod["status"] = serde_json::json!({"containerStatuses":[{"ready":true,"restartCount":10},{"ready":false,"restartCount":2}],"initContainerStatuses":[{"restartCount":1}]});
        pod["metadata"]["creationTimestamp"] = "2026-10-09T07:00:00Z".into();
        let now = "2026-10-10T07:00:00Z"
            .parse::<k8s_openapi::jiff::Timestamp>()
            .map(|timestamp| timestamp.as_second())
            .unwrap_or(0);
        assert_eq!(readiness(&pod), "1/2");
        assert_eq!(restarts(&pod), 13);
        assert_eq!(age(&pod, now), "1d");
        assert_eq!(age(&pod, 0), "future");
        assert_eq!(age(&object("missing-time"), now), "-");
        app.items = vec![pod, object("a")];
        app.selected = 1;
        assert_eq!(app.selected_uid().as_deref(), Some("uid-z"));
        app.set_sort(Sort::Restarts);
        assert_eq!(app.selected, 0);
        assert_eq!(app.selected_uid().as_deref(), Some("uid-z"));
        assert_eq!(display("long-name", 5), "long~");
        assert_eq!(display("\u{1b}[31mname", 20), "[31mname");
    }
    #[test]
    fn closed_metrics_view_does_not_reopen_on_late_reply() {
        let (mut app, replies, _commands) = harness();
        app.request_metrics();
        assert!(matches!(app.modal, Modal::Metrics(..)));
        assert!(app.key(KeyCode::Esc).is_ok());
        assert!(
            replies
                .send((
                    12,
                    Reply::Metrics(
                        crate::metrics::MetricsScope::Pods,
                        "test".into(),
                        Err("unavailable".into())
                    )
                ))
                .is_ok()
        );
        app.receive();
        assert!(matches!(app.modal, Modal::None));
        assert!(!app.stale);
    }
    #[test]
    fn theme_creation_captures_preview_palette_and_cancel_restores_previous_theme() {
        let (mut app, _replies, _commands) = harness();
        let previous = app.theme.clone();
        let palette = themes::builtins();
        app.theme = palette[1].clone();
        let preview = app.theme.clone();
        app.modal = Modal::Themes(palette, 1, previous.clone());
        assert!(app.key(KeyCode::Char('N')).is_ok());
        match &app.modal {
            Modal::NewTheme(id, source, old) => {
                assert!(id.is_empty());
                assert_eq!(source.id, preview.id);
                assert_eq!(old.id, previous.id);
            }
            _ => panic!("theme creation did not capture palette"),
        }
        assert!(app.key(KeyCode::Char('a')).is_ok());
        assert!(app.key(KeyCode::Char('-')).is_ok());
        assert!(app.key(KeyCode::Esc).is_ok());
        assert_eq!(app.theme.id, previous.id);
        assert!(matches!(app.modal, Modal::None));
    }
    #[test]
    fn live_filter_keeps_table_visible_and_cancel_restores_selected_uid() {
        let (mut app, _replies, _commands) = harness();
        app.items = vec![object("alpha"), object("beta")];
        app.selected = 1;
        assert!(app.key(KeyCode::Char('/')).is_ok());
        assert!(app.key(KeyCode::Char('a')).is_ok());
        assert!(app.key(KeyCode::Char('l')).is_ok());
        assert_eq!(app.visible().len(), 1);
        let frame = screen(&app, 60, 12).join("\n");
        assert!(frame.contains("alpha") && frame.contains("Filter > al_"));
        app.replace_items(vec![object("aardvark"), object("alpha"), object("beta")]);
        assert!(app.key(KeyCode::Esc).is_ok());
        assert_eq!(app.filter, "");
        assert_eq!(app.selected_uid().as_deref(), Some("uid-beta"));
    }
    #[test]
    fn long_context_does_not_hide_namespace_identity_and_actions_explain_capabilities() {
        let (mut app, _replies, _commands) = harness();
        app.context = "very-long-production-context-identifier".repeat(4);
        app.namespace = "payments-production-namespace".into();
        for width in [28, 40, 80, 120] {
            let frame = screen(&app, width, 12);
            assert!(frame[1].contains("CTX ") && frame[1].contains(" | NS "));
            assert!(frame[1].contains('~'));
        }
        app.items = vec![object("selected")];
        let actions = app.action_menu();
        assert!(actions[2].contains("[OFF]") && actions[2].contains("--allow-logs"));
        assert!(actions[7].contains("READ ONLY"));
        app.options.connection.writable = true;
        assert!(app.action_menu()[7].contains("Deployments only"));
        app.kind_index = 1;
        assert!(!app.action_menu()[7].contains("[OFF]"));
    }
    #[test]
    fn log_mailbox_is_bounded_and_cancel_discards_obsolete_producer() {
        let slot = LogSlot::default();
        let generation = slot.cancel();
        let line = "x".repeat(4096);
        for _ in 0..1000 {
            if !slot.append(generation, &line) {
                break;
            }
        }
        let text = slot.take().unwrap_or_default();
        assert!(text.len() <= crate::logs::MAX_LOG_BYTES + 1024);
        assert!(text.contains("display reached"));
        slot.cancel();
        assert!(!slot.append(generation, "obsolete private log"));
        assert!(slot.take().is_none());
    }
    #[test]
    fn escape_cancels_log_modal_and_scope_change_rejects_old_logs() {
        let (mut app, _replies, _commands) = harness();
        let generation = app.worker.logs.cancel();
        app.modal = Modal::Follow("live".into(), 0);
        assert!(app.key(KeyCode::Esc).is_ok());
        assert!(matches!(app.modal, Modal::None));
        assert!(!app.worker.logs.append(generation, "old"));
        let generation = app.worker.logs.cancel();
        app.changed();
        assert!(!app.worker.logs.append(generation, "different scope"));
    }
    #[test]
    fn image_form_captures_target_uid_and_readonly_never_opens_it() {
        let (mut app, _replies, mut commands) = harness();
        app.kind_index = 1;
        app.items = vec![object("first")];
        assert!(app.key(KeyCode::Char('e')).is_ok());
        assert!(matches!(app.modal, Modal::None));
        app.options.connection.writable = true;
        assert!(app.key(KeyCode::Char('e')).is_ok());
        app.items = vec![object("replacement")];
        for c in "app".chars() {
            assert!(app.key(KeyCode::Char(c)).is_ok());
        }
        assert!(app.key(KeyCode::Enter).is_ok());
        for c in "nginx:1.30".chars() {
            assert!(app.key(KeyCode::Char(c)).is_ok());
        }
        assert!(app.key(KeyCode::Enter).is_ok());
        match commands.try_recv() {
            Ok((_, Task::Prepare(_, ns, name, uid, Action::SetImage { container, image }))) => {
                assert_eq!(
                    (ns.as_str(), name.as_str(), uid.as_deref()),
                    ("test", "first", Some("uid-first"))
                );
                assert_eq!((container.as_str(), image.as_str()), ("app", "nginx:1.30"));
            }
            _ => panic!("image form changed selected target"),
        }
    }
    fn snapshot(name: &str) -> crate::watch::WatchSnapshot {
        crate::watch::WatchSnapshot {
            objects: vec![object(name)],
            resource_version: "rv".into(),
            stale: false,
            message: "LIVE".into(),
            terminal: false,
            truncated: false,
        }
    }
    #[test]
    fn confirmation_wrap_preserves_complete_target_and_effect() {
        let summary = format!(
            "target: {}/sample\n{}",
            "a".repeat(63),
            "effect ".repeat(20)
        );
        let lines = confirmation_lines(&summary, "typed-target", 80);
        assert!(lines.iter().all(|line| line.chars().count() <= 79));
        let displayed: String = lines.iter().map(|line| &line[1..]).collect();
        assert!(displayed.contains(&summary.replace('\n', "")));
        assert!(displayed.contains("CONFIRM > typed-target_"));
    }
    #[test]
    fn write_outcome_survives_epoch_and_watch_updates_until_acknowledged() {
        let (mut app, replies, _commands) = harness();
        app.busy = true;
        app.command_epoch = Some(11);
        app.write_inflight = true;
        assert!(
            replies
                .send((
                    11,
                    Reply::Failure("Mutation outcome UNKNOWN; reconcile before retrying".into())
                ))
                .is_ok()
        );
        assert!(app.receive());
        assert!(app.status.contains("outcome UNKNOWN"));
        assert!(app.stale && app.sticky_status);
        assert!(!app.busy && !app.write_inflight);
        app.worker.watch.publish(12, snapshot("fresh"));
        app.receive();
        assert!(app.status.contains("outcome UNKNOWN"));
        assert!(app.key(KeyCode::Char('?')).is_ok());
        assert!(!app.sticky_status);
    }
    #[test]
    fn namespace_changed_during_connect_is_not_overwritten_and_old_session_is_rejected() {
        let (mut app, replies, _commands) = harness();
        app.connected = false;
        app.busy = true;
        app.command_epoch = Some(12);
        app.connecting_epoch = Some(12);
        app.modal = Modal::Input("Namespace", "different".into());
        assert!(app.key(KeyCode::Enter).is_ok());
        assert_eq!(app.epoch, 13);
        assert!(
            replies
                .send((
                    11,
                    Reply::Connected(
                        "obsolete".into(),
                        "wrong".into(),
                        "wrong".into(),
                        crate::gateway::kinds()
                    )
                ))
                .is_ok()
        );
        app.receive();
        assert_eq!(app.context, "test");
        assert!(app.busy);
        assert!(
            replies
                .send((
                    12,
                    Reply::Connected(
                        "current".into(),
                        "server".into(),
                        "kubeconfig-default".into(),
                        crate::gateway::kinds()
                    )
                ))
                .is_ok()
        );
        app.receive();
        assert_eq!(app.context, "current");
        assert_eq!(app.namespace, "different");
        assert!(app.connected && !app.busy);
    }
    #[test]
    fn obsolete_watch_is_dropped_and_revocation_clears_the_view() {
        let (mut app, _replies, _commands) = harness();
        app.items = vec![object("old")];
        app.changed();
        app.worker.watch.publish(12, snapshot("obsolete"));
        assert!(!app.receive());
        assert!(app.items.is_empty());
        app.worker.watch.publish(13, snapshot("current"));
        assert!(app.receive());
        assert_eq!(app.items[0]["metadata"]["name"], "current");
        let mut denied = snapshot("discard");
        denied.objects.clear();
        denied.stale = true;
        denied.terminal = true;
        app.worker.watch.publish(13, denied);
        app.receive();
        assert!(app.items.is_empty() && app.stale && app.watch_blocked && !app.watching);
    }
    #[test]
    fn producer_burst_coalesces_without_waiting_for_ui_consumption() {
        let mailbox = std::sync::Arc::new(WatchSlot::default());
        mailbox.invalidate(7);
        let producer = mailbox.clone();
        let task = std::thread::spawn(move || {
            for n in 0..1000 {
                producer.publish(7, snapshot(&n.to_string()));
            }
        });
        assert!(task.join().is_ok());
        let last = mailbox
            .take()
            .unwrap_or_else(|| panic!("missing latest snapshot"));
        assert_eq!(last.0, 7);
        assert_eq!(last.1.objects[0]["metadata"]["name"], "999");
        assert!(mailbox.take().is_none());
    }
    #[test]
    fn watch_insertion_preserves_identity_and_scale_form_captures_name_and_uid() {
        let (mut app, _replies, mut commands) = harness();
        app.options.connection.writable = true;
        app.kind_index = 1;
        app.items = vec![object("b")];
        assert!(app.key(KeyCode::Char('s')).is_ok());
        app.replace_items(vec![object("a"), object("b")]);
        assert_eq!(app.selected, 1);
        // Even if the original row disappears, the form must not prepare a different row.
        app.replace_items(vec![object("a")]);
        assert!(app.key(KeyCode::Char('2')).is_ok());
        assert!(app.key(KeyCode::Enter).is_ok());
        match commands.try_recv() {
            Ok((_, Task::Prepare(_, ns, name, uid, Action::Scale(2)))) => {
                assert_eq!(ns, "test");
                assert_eq!(name, "b");
                assert_eq!(uid.as_deref(), Some("uid-b"));
            }
            _ => panic!("scale form changed its selected target"),
        }
    }
    #[test]
    fn missing_metrics_does_not_invalidate_resource_data() {
        let (mut app, replies, _commands) = harness();
        app.items = vec![object("a")];
        app.command_epoch = Some(12);
        app.busy = true;
        app.modal = Modal::Metrics("loading".into(), 0);
        assert!(
            replies
                .send((
                    12,
                    Reply::Metrics(
                        crate::metrics::MetricsScope::Pods,
                        "test".into(),
                        Err("Metrics API unavailable".into())
                    )
                ))
                .is_ok()
        );
        app.receive();
        assert_eq!(app.items.len(), 1);
        assert!(!app.stale);
        assert!(matches!(app.modal, Modal::Metrics(..)));
    }
    #[test]
    fn metrics_wrapping_preserves_long_identity_usage_and_measurement_time() {
        let text = format!(
            "test/{} container 125.5 12345678 2026-10-09T22:00:00Z 15s",
            "a".repeat(63)
        );
        let wrapped = wrap_text(&text, 48);
        assert!(wrapped.lines().all(|line| line.chars().count() <= 48));
        assert_eq!(wrapped.replace('\n', ""), text);
    }
    #[test]
    fn reauthentication_keeps_context_namespace_and_resource_identity() {
        let (mut app, replies, mut commands) = harness();
        app.kinds.swap(0, 10);
        app.kind_index = 0;
        app.watch_blocked = true;
        assert_eq!(app.kind(), "configmaps");
        assert!(app.key(KeyCode::Char('r')).is_ok());
        let epoch = app.epoch;
        match commands.try_recv() {
            Ok((e, Task::Connect(options))) => {
                assert_eq!(e, epoch);
                assert_eq!(options.context.as_deref(), Some("test"));
            }
            _ => panic!("reauthentication did not pin the current context"),
        }
        assert!(
            replies
                .send((
                    epoch,
                    Reply::Connected(
                        "test".into(),
                        "server".into(),
                        "default".into(),
                        crate::gateway::kinds()
                    )
                ))
                .is_ok()
        );
        app.receive();
        assert_eq!(app.namespace, "test");
        assert_eq!(app.kind(), "configmaps");
        assert!(app.refresh && !app.watch_blocked);
    }
}
