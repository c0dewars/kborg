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
        MouseEvent, MouseEventKind,
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
    Metrics(crate::metrics::MetricsScope, String, bool),
    Inspect(String, String, String),
    Get(String, String, String),
    Yaml(String, String, String),
    Overview(Connection, Vec<String>),
    PrepareSession(String, String, String, Option<String>, u16),
    RunSession(Box<crate::ops::ReviewedOperation>, String),
    Containers(String, String, bool, bool),
    Logs(String, String, Option<String>, bool),
    Follow(String, String, Option<String>, u64, u32),
    Discover,
    Prepare(String, String, String, Option<String>, Action),
    Execute(Box<Prepared>, Approval),
}
enum Reply {
    Connected(String, String, String, Vec<Kind>),
    Items(Value),
    Detail(String),
    Object(Box<Value>),
    Overview(crate::overview::OverviewFeed),
    SessionPrepared(Box<crate::ops::ReviewedOperation>),
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
    buffer: crate::logs::RollingLog,
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
            // The newest bounded window remains available throughout a long session.
            state.buffer.push(line);
            state.dirty = true;
            return true;
        }
        false
    }
    fn take(&self) -> Option<String> {
        let mut state = self.state.lock().ok()?;
        if !state.dirty {
            return None;
        }
        state.dirty = false;
        Some(state.buffer.text())
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
                    Task::Metrics(scope, n, include_resources) => {
                        let result = if let Some(g) = gateway.as_ref() {
                            let future = async { if include_resources { crate::metrics::collect_with_allocations(g,scope,&n).await } else { crate::metrics::collect(g,scope,&n).await } };
                            match tokio::time::timeout(Duration::from_secs(if include_resources {35} else {20}), future).await {
                                Ok(result) => result,
                                Err(_) => Err("Metrics request timed out; resource browsing remains available.".into()),
                            }
                        } else { Err("Not connected; select a context or retry.".into()) };
                        Reply::Metrics(scope, n, result)
                    }
                    Task::RunSession(reviewed, typed) => {
                        if let Some(g) = gateway.as_ref() {
                            match crate::ops::run(g, *reviewed, &typed).await {
                                Ok(message) => Reply::Notice(message),
                                Err(error) => Reply::Failure(error),
                            }
                        } else { Reply::Failure("Session unavailable; reconnect before review.".into()) }
                    }
                    task => {
                        if let Some(g) = gateway.as_mut() {
                            let future = async {
                                Ok(match task {
                                    Task::List(k,n) => Reply::Items(g.list(&k,&n).await?),
                                    Task::Get(k,n,name) => Reply::Detail(serde_json::to_string_pretty(&g.get(&k,&n,&name).await?).map_err(|_| "serialization failed")?),
                                    Task::Inspect(k,n,name) => Reply::Object(Box::new(g.get(&k,&n,&name).await?)),
                                    Task::Yaml(k,n,name) => Reply::Detail(crate::inspect::yaml(&g.get(&k,&n,&name).await?)?),
                                    Task::Overview(options,names) => Reply::Overview(crate::overview::OverviewFeed::new(&options,&names)?),
                                    Task::PrepareSession(n,name,container,uid,seconds) => {
                                        let prepared = crate::ops::prepare(g,&n,&name,crate::ops::Request::InteractiveExec {container,argv:vec!["/bin/sh".into(),"-i".into()],seconds}).await?;
                                        if uid.as_deref().is_none_or(|uid| uid != prepared.uid()) { return Err("Selected pod was replaced; select and review again.".into()); }
                                        Reply::SessionPrepared(Box::new(prepared))
                                    },
                                    Task::Containers(n,name,previous,follow) => {
                                        let names = g.containers(&n,&name).await?;
                                        Reply::Containers(n,name,names,previous,follow)
                                    },
                                    Task::Logs(n,name,container,previous) => Reply::Detail(g.logs(&n,&name,container,previous).await?),
                                    Task::Follow(n,name,container,generation,seconds) => {
                                        if log_results.generation.load(std::sync::atomic::Ordering::SeqCst) != generation {return Ok(Reply::Following);}
                                        let mut feed = g.follow_logs_with_options(&n,&name,container,false,crate::logs::FollowOptions{seconds}).await?;
                                        let mailbox = log_results.clone();
                                        let job = tokio::spawn(async move {
                                            let mut count = 0usize;
                                            loop {
                                                match feed.next().await {
                                                    Ok(Some(line)) => {if !mailbox.append(generation,&line) {break;} count += 1; if count.is_multiple_of(64) { tokio::task::yield_now().await; }},
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
                                    Task::Connect(_) | Task::Watch(..) | Task::Metrics(..) | Task::RunSession(..) => return Err("invalid worker task".into()),
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

const OVERVIEW_CONTEXTS_LABEL: &str = "Overview contexts (comma separated, max 4)";
const OVERVIEW_INPUT_BYTES: usize = 4 * 253 + 3;
fn input_limit(label: &str) -> usize {
    if label == OVERVIEW_CONTEXTS_LABEL {
        OVERVIEW_INPUT_BYTES
    } else {
        100
    }
}

enum Modal {
    None,
    Input(&'static str, String),
    Filter(String, String, Option<String>, usize),
    Detail(String, usize),
    Overview(Box<Value>, usize),
    LiveOverview(String, usize),
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
    SessionContainers(String, String, Option<String>, Vec<String>, usize),
    SessionConfirm(Box<crate::ops::ReviewedOperation>, String),
    Scale(String, String, String, Option<String>, String),
    Resources(String, String, String, Option<String>, String),
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
    metrics_resources: bool,
    overview: Option<crate::overview::OverviewFeed>,
    pending_session: Option<(Box<crate::ops::ReviewedOperation>, String)>,
    worker: Worker,
}
impl App {
    fn mouse(&mut self, mouse: MouseEvent, width: u16, height: u16) {
        let layout = Layout::new(width, height);
        let resources = layout.sidebar.is_some_and(|rect| mouse.column < rect.width)
            || (layout.sidebar.is_none() && self.left_focus);
        match mouse.kind {
            MouseEventKind::ScrollDown if resources => self.move_resource(1),
            MouseEventKind::ScrollUp if resources => self.move_resource(-1),
            MouseEventKind::ScrollDown => {
                self.selected = (self.selected + 1).min(self.visible().len().saturating_sub(1))
            }
            MouseEventKind::ScrollUp => self.selected = self.selected.saturating_sub(1),
            MouseEventKind::Down(_)
                if resources && mouse.row >= 4 && mouse.row < height.saturating_sub(4) =>
            {
                let entries = navigation(&self.kinds);
                let start = nav_start(&self.kinds, self.kind_index, layout.kind_rows());
                if let Some(NavEntry::Resource(index)) =
                    entries.get(start + usize::from(mouse.row - 4))
                {
                    if self.kind_index != *index {
                        self.kind_index = *index;
                        self.changed();
                    }
                    self.left_focus = false;
                }
            }
            MouseEventKind::Down(_)
                if !resources
                    && mouse.row >= layout.table.y + 2
                    && mouse.row < layout.table.y + layout.table.height.saturating_sub(1) =>
            {
                let start = self
                    .selected
                    .saturating_sub(layout.table_rows().saturating_sub(1));
                self.selected = (start + usize::from(mouse.row - layout.table.y - 2))
                    .min(self.visible().len().saturating_sub(1));
                self.left_focus = false;
            }
            _ => {}
        }
    }
    fn move_resource(&mut self, offset: isize) {
        let order: Vec<_> = navigation(&self.kinds)
            .iter()
            .filter_map(|entry| {
                if let NavEntry::Resource(index) = entry {
                    Some(*index)
                } else {
                    None
                }
            })
            .collect();
        let current = order
            .iter()
            .position(|index| *index == self.kind_index)
            .unwrap_or(0);
        let next = current
            .saturating_add_signed(offset)
            .min(order.len().saturating_sub(1));
        if let Some(index) = order.get(next)
            && self.kind_index != *index
        {
            self.kind_index = *index;
            self.changed();
        }
    }
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
        let writes = matches!(&task, Task::Execute(..) | Task::RunSession(..));
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
        self.overview = None;
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
                Reply::Object(object) => self.modal = Modal::Overview(object, 0),
                Reply::Overview(feed) => {
                    if matches!(self.modal, Modal::LiveOverview(..)) {
                        match feed.snapshot() {
                            Ok(rows) => {
                                self.modal = Modal::LiveOverview(
                                    crate::overview::live_text(&rows).replace('\t', " | "),
                                    0,
                                );
                                self.overview = Some(feed);
                            }
                            Err(error) => {
                                self.modal =
                                    Modal::Detail(format!("Overview unavailable\n{error}"), 0);
                            }
                        }
                    }
                }
                Reply::SessionPrepared(prepared) => {
                    self.modal = Modal::SessionConfirm(prepared, String::new())
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
                    if matches!(self.modal, Modal::LiveOverview(..)) {
                        self.overview = None;
                        self.modal = Modal::Detail(format!("Overview unavailable\n{error}"), 0);
                        self.status = error;
                        continue;
                    }
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
        if let Some(feed) = &self.overview {
            match feed.take_update() {
                Ok(Some(rows)) => {
                    if let Modal::LiveOverview(_, offset) = &self.modal {
                        self.modal = Modal::LiveOverview(
                            crate::overview::live_text(&rows).replace('\t', " | "),
                            *offset,
                        );
                        changed = true;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    self.status = error;
                    self.overview = None;
                    changed = true;
                }
            }
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
            self.metrics_resources,
        ));
    }
    fn request_shell(&mut self) {
        if self.busy {
            self.status = "Busy; wait for the current request.".into();
            return;
        }
        if self.options.connection.demo {
            self.status = "Pod shells require a live cluster; DEMO has no remote processes.".into();
            return;
        }
        if !self.options.connection.writable || !self.options.connection.allow_exec {
            self.status =
                "Pod shell requires --write and the separate --allow-exec capability.".into();
            return;
        }
        if self.kind() != "pods" {
            self.status = "Pod shells apply to Pods; select a pod.".into();
            return;
        }
        if let Some(object) = self.visible().get(self.selected) {
            let names: Vec<_> = object["spec"]["containers"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .take(64)
                        .filter_map(|item| item["name"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            if names.is_empty() || self.selected_uid().is_none() {
                self.status =
                    "Pod identity or ordinary containers unavailable; refresh before shell review."
                        .into();
                return;
            }
            self.modal = Modal::SessionContainers(
                namespace(object).into(),
                name(object).into(),
                self.selected_uid(),
                names,
                0,
            );
        }
    }
    fn action_menu(&self) -> Vec<String> {
        let kind = self.kind();
        let has_selection = !self.visible().is_empty();
        [
            "Object overview",
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
            "Inspect redacted JSON",
            "Interactive pod shell",
            "Metrics with requests and limits (extra reads)",
            "Edit deployment resource budget",
        ]
        .iter()
        .enumerate()
        .map(|(index, title)| {
            let reason = if index == 5 || index == 6 || index == 13 {
                None
            } else if !has_selection {
                Some("select an object")
            } else if (2..=4).contains(&index) && !self.options.connection.logs {
                Some("requires --allow-logs")
            } else if (2..=4).contains(&index) && kind != "pods" {
                Some("Pods only")
            } else if ((7..=10).contains(&index) || index == 12 || index == 14)
                && !self.options.connection.writable
            {
                Some("READ ONLY; requires --write")
            } else if ((7..=9).contains(&index) || index == 14) && kind != "deployments.apps" {
                Some("Deployments only")
            } else if index == 10
                && !matches!(
                    kind.as_str(),
                    "pods" | "deployments.apps" | "services" | "jobs.batch"
                )
            {
                Some("unsupported resource")
            } else if index == 12 && !self.options.connection.allow_exec {
                Some("requires --allow-exec")
            } else if index == 12 && (kind != "pods" || self.options.connection.demo) {
                Some("live Pods only")
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
            Action::Scale(_)
            | Action::Restart
            | Action::SetImage { .. }
            | Action::SetResources { .. } => self.kind() == "deployments.apps",
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
            Modal::Follow(text, offset) => match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.worker.logs.cancel();
                }
                KeyCode::End => self.modal = Modal::Follow(text, usize::MAX),
                _ => {
                    let offset = if offset == usize::MAX {
                        scroll_offset(&text, 0, KeyCode::End)
                    } else {
                        offset
                    };
                    self.modal = Modal::Follow(text.clone(), scroll_offset(&text, offset, code));
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
                            self.modal = Modal::Follow("Connecting to logs...".into(), usize::MAX);
                            self.send(Task::Follow(
                                n,
                                name,
                                Some(container.clone()),
                                generation,
                                self.options.log_seconds,
                            ));
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
                    } else if label == OVERVIEW_CONTEXTS_LABEL {
                        let names: Vec<String> =
                            text.split(',').map(|name| name.trim().to_owned()).collect();
                        if names.len() > 4 {
                            self.status="TUI overview allows up to 4 explicit contexts; CLI supports up to 100.".into();
                        } else if let Err(error) = crate::overview::validate(&names) {
                            self.status = error;
                        } else {
                            self.modal = Modal::LiveOverview(
                                "Connecting independent context watches...".into(),
                                0,
                            );
                            let mut connection = self.options.connection.clone();
                            if self.namespace_override {
                                connection.namespace = self.namespace.clone();
                            }
                            self.send(Task::Overview(connection, names));
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
                    if !c.is_control()
                        && text.len().saturating_add(c.len_utf8()) <= input_limit(label)
                    {
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
            Modal::Overview(object, mut offset) => match code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Char('J') => {
                    self.modal = Modal::Detail(
                        serde_json::to_string_pretty(&object)
                            .map_err(|_| "serialization failed")?,
                        0,
                    )
                }
                KeyCode::Char('y') => self.modal = Modal::Detail(crate::inspect::yaml(&object)?, 0),
                _ => {
                    offset = scroll_offset(&overview_text(&object, &self.context), offset, code);
                    self.modal = Modal::Overview(object, offset);
                }
            },
            Modal::LiveOverview(text, offset) => match code {
                KeyCode::Esc | KeyCode::Char('q') => self.overview = None,
                _ => {
                    self.modal =
                        Modal::LiveOverview(text.clone(), scroll_offset(&text, offset, code))
                }
            },
            Modal::SessionContainers(n, name, uid, names, index) => match code {
                KeyCode::Esc => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    self.modal = Modal::SessionContainers(
                        n,
                        name,
                        uid,
                        names.clone(),
                        (index + 1).min(names.len().saturating_sub(1)),
                    )
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal =
                        Modal::SessionContainers(n, name, uid, names, index.saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(container) = names.get(index) {
                        self.send(Task::PrepareSession(
                            n,
                            name,
                            container.clone(),
                            uid,
                            self.options.session_seconds,
                        ));
                    }
                }
                _ => self.modal = Modal::SessionContainers(n, name, uid, names, index),
            },
            Modal::SessionConfirm(prepared, mut text) => match code {
                KeyCode::Esc => self.status = "Pod shell canceled.".into(),
                KeyCode::Enter => {
                    let (width, height) = terminal::size().map_err(|_| "terminal size failed")?;
                    if !confirmation_fits(prepared.summary(), &text, width, height) {
                        self.status = "Enlarge terminal to inspect the complete session review before approving.".into();
                        self.modal = Modal::SessionConfirm(prepared, text);
                    } else if text != prepared.confirmation() {
                        self.status = "Type the exact namespace/pod from the review.".into();
                        self.modal = Modal::SessionConfirm(prepared, text);
                    } else {
                        self.pending_session = Some((prepared, text));
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.modal = Modal::SessionConfirm(prepared, text);
                }
                KeyCode::Char(c) if c.is_ascii_graphic() && text.len() < 320 => {
                    text.push(c);
                    self.modal = Modal::SessionConfirm(prepared, text);
                }
                _ => self.modal = Modal::SessionConfirm(prepared, text),
            },
            Modal::Metrics(text, mut offset) => match code {
                KeyCode::Esc | KeyCode::Char('q') => {}
                KeyCode::Char('m') | KeyCode::Char('r') => {
                    self.modal = Modal::Metrics(text, offset);
                    self.request_metrics();
                }
                KeyCode::Char('M') => {
                    self.metrics_resources = true;
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
                                KeyCode::Char('J'),
                                KeyCode::Char('E'),
                                KeyCode::Char('M'),
                                KeyCode::Char('V'),
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
            Modal::Resources(k, n, name, uid, mut text) => match code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    if self.busy {
                        self.status = "Busy; wait for the current request before preparing the resource budget.".into();
                        self.modal = Modal::Resources(k, n, name, uid, text);
                        return Ok(false);
                    }
                    let fields: Vec<_> = text.split_whitespace().collect();
                    let result = if fields.len() == 5 {
                        let resources = crate::edit::ResourceEdit {
                            cpu_request: fields[1].into(),
                            memory_request: fields[2].into(),
                            cpu_limit: fields[3].into(),
                            memory_limit: fields[4].into(),
                        };
                        resources.validate(fields[0]).map(|_| Action::SetResources {
                            container: fields[0].into(),
                            resources,
                        })
                    } else {
                        Err("Enter five fields: container CPU_REQUEST MEMORY_REQUEST CPU_LIMIT MEMORY_LIMIT".into())
                    };
                    match result {
                        Ok(action) => self.send(Task::Prepare(k, n, name, uid, action)),
                        Err(error) => {
                            self.status = error;
                            self.modal = Modal::Resources(k, n, name, uid, text);
                        }
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.modal = Modal::Resources(k, n, name, uid, text);
                }
                KeyCode::Char(c) if (c.is_ascii_graphic() || c == ' ') && text.len() < 240 => {
                    text.push(c);
                    self.modal = Modal::Resources(k, n, name, uid, text);
                }
                _ => self.modal = Modal::Resources(k, n, name, uid, text),
            },
            Modal::Help(offset) => match code {
                KeyCode::Esc | KeyCode::Char('?') | KeyCode::Char('q') => {}
                _ => self.modal = Modal::Help(scroll_offset(TUI_HELP, offset, code)),
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
                    if self.left_focus {
                        self.move_resource(match code {
                            KeyCode::PageDown => page as isize,
                            KeyCode::PageUp => -(page as isize),
                            KeyCode::Home => -(self.kinds.len() as isize),
                            _ => self.kinds.len() as isize,
                        });
                        return Ok(false);
                    }
                    let len = self.visible().len();
                    let index = &mut self.selected;
                    *index = match code {
                        KeyCode::PageDown => index.saturating_add(page).min(len.saturating_sub(1)),
                        KeyCode::PageUp => index.saturating_sub(page),
                        KeyCode::Home => 0,
                        _ => len.saturating_sub(1),
                    };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.left_focus {
                        self.move_resource(1);
                    } else {
                        self.selected =
                            (self.selected + 1).min(self.visible().len().saturating_sub(1));
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.left_focus {
                        self.move_resource(-1);
                    } else {
                        self.selected = self.selected.saturating_sub(1);
                    }
                }
                KeyCode::Enter => {
                    if self.left_focus {
                        self.left_focus = false;
                    } else if let Some((k, n, name)) = self.selected_target() {
                        self.send(Task::Inspect(k, n, name));
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
                KeyCode::Char('J') => {
                    if let Some((k, n, name)) = self.selected_target() {
                        self.send(Task::Get(k, n, name));
                    }
                }
                KeyCode::Char('d') => {
                    if let Some((k, n, name)) = self.selected_target() {
                        self.send(Task::Inspect(k, n, name));
                    }
                }
                KeyCode::Char('S') => self.set_sort(self.sort.next()),
                KeyCode::Char('E') => self.request_shell(),
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
                KeyCode::Char('m') => {
                    self.metrics_resources = false;
                    self.request_metrics();
                }
                KeyCode::Char('M') => {
                    self.metrics_resources = true;
                    self.request_metrics();
                }
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
                    self.modal = Modal::Input(OVERVIEW_CONTEXTS_LABEL, self.context.clone());
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
                KeyCode::Char('V') => {
                    if !self.options.connection.writable {
                        self.status = "READ ONLY: resource budgets require --write.".into();
                    } else if self.kind() != "deployments.apps" {
                        self.status = "Resource budgets apply to deployments.".into();
                    } else if let Some((k, n, name)) = self.selected_target() {
                        self.modal =
                            Modal::Resources(k, n, name, self.selected_uid(), String::new());
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Health {
    Healthy,
    Attention,
    Neutral,
}
fn health(object: &Value) -> Health {
    let status = summary(object);
    if [
        "Failed",
        "Error",
        "Crash",
        "BackOff",
        "NotReady",
        "Pending",
        "Unknown",
        "Terminating",
    ]
    .iter()
    .any(|needle| status.contains(needle))
    {
        return Health::Attention;
    }
    match object["kind"].as_str().unwrap_or("") {
        "Pod" if object["status"]["phase"] == "Running" => {
            let statuses = object["status"]["containerStatuses"].as_array();
            let count = object["spec"]["containers"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            if count > 0
                && statuses.is_some_and(|statuses| {
                    statuses.len() == count && statuses.iter().all(|item| item["ready"] == true)
                })
            {
                Health::Healthy
            } else {
                Health::Attention
            }
        }
        "Node" => {
            if readiness(object) == "True" {
                Health::Healthy
            } else {
                Health::Attention
            }
        }
        "Deployment" | "StatefulSet" | "ReplicaSet" | "DaemonSet" => {
            let ready = readiness(object);
            if let Some((ready, desired)) = ready.split_once('/') {
                if ready == desired {
                    Health::Healthy
                } else {
                    Health::Attention
                }
            } else {
                Health::Neutral
            }
        }
        _ if [
            "Succeeded",
            "Complete",
            "Running",
            "Active",
            "Bound",
            "Ready",
        ]
        .contains(&status.as_str()) =>
        {
            Health::Healthy
        }
        _ => Health::Neutral,
    }
}
fn clean_value(value: &Value) -> String {
    value
        .as_str()
        .map(|value| safety::safe_text(value, 512))
        .unwrap_or_else(|| {
            if value.is_null() {
                "-".into()
            } else {
                safety::safe_text(&value.to_string(), 512)
            }
        })
}
fn overview_text(object: &Value, context: &str) -> String {
    let now = k8s_openapi::jiff::Timestamp::now().as_second();
    let kind = object["kind"].as_str().unwrap_or("Resource");
    let mut lines = vec![
        format!(
            "{} / {} / {}",
            kind.to_ascii_uppercase(),
            namespace(object),
            name(object)
        ),
        format!(
            "STATE {}   READY {}   RESTARTS {}   AGE {}",
            summary(object),
            readiness(object),
            restarts(object),
            age(object, now)
        ),
        String::new(),
        "IDENTITY".into(),
        format!("Context   {}", safety::safe_text(context, 256)),
        format!("Namespace {}", namespace(object)),
        format!("Name      {}", name(object)),
        format!("UID       {}", clean_value(&object["metadata"]["uid"])),
        format!(
            "Version   {}",
            clean_value(&object["metadata"]["resourceVersion"])
        ),
    ];
    if kind == "Pod" {
        lines.extend([
            String::new(),
            "SCHEDULING & NETWORK".into(),
            format!("Node      {}", clean_value(&object["spec"]["nodeName"])),
            format!("Pod IP    {}", clean_value(&object["status"]["podIP"])),
            format!("QoS       {}", clean_value(&object["status"]["qosClass"])),
            format!(
                "Account   {}",
                clean_value(&object["spec"]["serviceAccountName"])
            ),
        ]);
    }
    let pod_spec = if kind == "Pod" {
        &object["spec"]
    } else {
        &object["spec"]["template"]["spec"]
    };
    let mut remaining = 64usize;
    for (field, status_field, label) in [
        ("initContainers", "initContainerStatuses", "INIT CONTAINERS"),
        ("containers", "containerStatuses", "CONTAINERS"),
        (
            "ephemeralContainers",
            "ephemeralContainerStatuses",
            "EPHEMERAL CONTAINERS",
        ),
    ] {
        if let Some(containers) = pod_spec[field].as_array() {
            lines.extend([String::new(), label.into()]);
            for container in containers.iter().take(remaining) {
                let container_name = container["name"].as_str().unwrap_or("-");
                let status = object["status"][status_field]
                    .as_array()
                    .and_then(|statuses| {
                        statuses
                            .iter()
                            .find(|status| status["name"] == container_name)
                    });
                lines.push(format!(
                    "{}  ready:{}  restarts:{}",
                    safety::safe_text(container_name, 256),
                    status
                        .map(|status| clean_value(&status["ready"]))
                        .unwrap_or_else(|| "-".into()),
                    status
                        .map(|status| clean_value(&status["restartCount"]))
                        .unwrap_or_else(|| "-".into())
                ));
                lines.push(format!("  image {}", clean_value(&container["image"])));
                if let Some(status) = status {
                    for state in ["waiting", "terminated"] {
                        if !status["state"][state].is_null() {
                            lines.push(format!(
                                "  {} {}",
                                state,
                                clean_value(&status["state"][state]["reason"])
                            ));
                        }
                    }
                }
            }
            remaining = remaining.saturating_sub(containers.len());
            if remaining == 0 {
                lines.push(
                    "Container overview capped at 64 entries; J shows bounded redacted JSON."
                        .into(),
                );
                break;
            }
        }
    }
    if let Some(conditions) = object["status"]["conditions"].as_array() {
        lines.extend([String::new(), "CONDITIONS".into()]);
        lines.extend(conditions.iter().take(32).map(|condition| {
            format!(
                "{:<22} {:<8} {}",
                clean_value(&condition["type"]),
                clean_value(&condition["status"]),
                clean_value(&condition["reason"])
            )
        }));
    }
    if kind == "Service" {
        lines.extend([
            String::new(),
            "NETWORK".into(),
            format!("Type      {}", clean_value(&object["spec"]["type"])),
            format!("ClusterIP {}", clean_value(&object["spec"]["clusterIP"])),
        ]);
        if let Some(ports) = object["spec"]["ports"].as_array() {
            lines.extend(ports.iter().take(32).map(|port| {
                format!(
                    "Port      {} / {} -> {}",
                    clean_value(&port["port"]),
                    clean_value(&port["protocol"]),
                    clean_value(&port["targetPort"])
                )
            }));
        }
    }
    if kind == "Event" {
        lines.extend([
            String::new(),
            "EVENT".into(),
            format!("Reason    {}", clean_value(&object["reason"])),
            format!("Message   {}", clean_value(&object["message"])),
        ]);
    }
    if let Some(labels) = object["metadata"]["labels"].as_object()
        && !labels.is_empty()
    {
        lines.extend([String::new(), "LABELS".into()]);
        lines.extend(labels.iter().take(32).map(|(key, value)| {
            format!("{} = {}", safety::safe_text(key, 256), clean_value(value))
        }));
    }
    lines.extend([String::new(), "J redacted JSON   y YAML   Esc close".into()]);
    safety::safe_lines(&lines.join("\n"), 64 * 1024)
}
#[derive(Clone, Copy, Debug)]
enum NavEntry {
    Group(&'static str),
    Resource(usize),
}
fn resource_group(kind: &Kind) -> usize {
    match kind.kind.as_str() {
        "Pod" | "Deployment" | "StatefulSet" | "DaemonSet" | "ReplicaSet" | "Job" | "CronJob"
        | "Event" => 0,
        "Service" | "Ingress" | "EndpointSlice" => 1,
        "ConfigMap" | "PersistentVolumeClaim" | "Role" | "RoleBinding" | "ServiceAccount" => 2,
        "Node" | "Namespace" | "StorageClass" | "PersistentVolume" => 3,
        _ => 4,
    }
}
fn navigation(kinds: &[Kind]) -> Vec<NavEntry> {
    let mut entries = Vec::new();
    for (group, title) in [
        "WORKLOADS",
        "NETWORK",
        "CONFIG & ACCESS",
        "CLUSTER",
        "DISCOVERED",
    ]
    .iter()
    .enumerate()
    {
        let resources: Vec<_> = kinds
            .iter()
            .enumerate()
            .filter(|(_, kind)| resource_group(kind) == group)
            .map(|(index, _)| NavEntry::Resource(index))
            .collect();
        if !resources.is_empty() {
            entries.push(NavEntry::Group(title));
            entries.extend(resources);
        }
    }
    entries
}
fn nav_start(kinds: &[Kind], index: usize, rows: usize) -> usize {
    navigation(kinds)
        .iter()
        .position(|entry| matches!(entry, NavEntry::Resource(selected) if *selected == index))
        .unwrap_or(0)
        .saturating_sub(rows.saturating_sub(1))
}
fn resource_label(kind: &Kind) -> String {
    match kind.kind.as_str() {
        "PersistentVolumeClaim" => "Volume claims".into(),
        "ConfigMap" => "Config maps".into(),
        "StatefulSet" => "Stateful sets".into(),
        "DaemonSet" => "Daemon sets".into(),
        "ReplicaSet" => "Replica sets".into(),
        "CronJob" => "Cron jobs".into(),
        "RoleBinding" => "Role bindings".into(),
        "StorageClass" => "Storage classes".into(),
        _ => {
            let mut name = kind.plural.clone();
            if let Some(first) = name.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            name
        }
    }
}
const TUI_HELP: &str = "BROWSE\n  j / k, arrows       Move through resources and objects\n  Tab                 Focus resources / table\n  Enter / d           Human-readable object overview\n  J / y               Redacted JSON / YAML\n  /                   Live cache filter; Esc restores selection\n  :pods :deploy :svc   Jump directly to a resource\n  :ns NAME / :all      Select namespace / all namespaces\n  S / :sort FIELD     Sort; name, namespace, status, restarts, age\n  PgUp PgDown Home End Navigate pages\n\nOPERATE\n  a                   Visible actions with disabled reasons\n  l / L / f           Logs / previous logs / rolling follow\n  E                   Reviewed pod shell (--write --allow-exec)\n  m / M               Metrics / add requests-limit comparisons (extra reads)\n  c / n / o           Context / namespace / context overview\n  r / R / w           Refresh / discover / watch-poll\n  s / e / V           Scale / image / CPU-memory budget edit\n  x / Delete          Restart / delete\n                      Writes need --write and exact target approval\n\nPERSONALIZE\n  t                   Preview themes; Enter saves\n  N inside themes     Create a custom palette from the preview\n  q                   Quit; Esc closes dialogs\n\nSAFETY\n  Context, namespace and mode remain visible. DEMO is sample data.\n  STALE and TRUNCATED describe incomplete data.\n  Logs require --allow-logs. Secrets and generic patches are excluded.\n  Every mutation retains a full dry-run review and one-use approval.";

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
    preview: Option<Rect>,
}
impl Layout {
    fn new(width: u16, height: u16) -> Self {
        let sidebar_width = if width >= 88 { 24 } else { 0 };
        let preview_height = if width >= 70 && height >= 24 { 7 } else { 0 };
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
                height: height.saturating_sub(6 + preview_height),
            },
            preview: (preview_height > 0).then_some(Rect {
                x: sidebar_width,
                y: height.saturating_sub(3 + preview_height),
                width: width.saturating_sub(sidebar_width),
                height: preview_height,
            }),
        }
    }
    fn table_rows(self) -> usize {
        usize::from(self.table.height.saturating_sub(3))
    }
    fn kind_rows(self) -> usize {
        usize::from(
            self.sidebar
                .map(|rect| rect.height)
                .unwrap_or(self.table.height + self.preview.map(|rect| rect.height).unwrap_or(0))
                .saturating_sub(2),
        )
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
    let mut frame = Vec::new();
    render(app, w, h, &mut frame)?;
    let mut out = io::stdout().lock();
    out.write_all(&frame)
        .and_then(|_| out.flush())
        .map_err(|_| "draw failed".into())
}
fn modal_rect(modal: &Modal, width: u16, height: u16) -> Rect {
    let available = height.saturating_sub(6);
    let compact = matches!(
        modal,
        Modal::Input(..)
            | Modal::Picker(..)
            | Modal::Themes(..)
            | Modal::NewTheme(..)
            | Modal::Image(..)
            | Modal::Scale(..)
            | Modal::Resources(..)
            | Modal::SessionContainers(..)
    );
    let modal_width = if compact && width >= 80 {
        width.saturating_sub(10).min(84)
    } else {
        width
    };
    let desired = match modal {
        Modal::Input(..) => 6,
        Modal::NewTheme(..) | Modal::Image(..) => 8,
        Modal::Resources(..) => 10,
        Modal::Scale(..) => 6,
        Modal::Picker(_, items, _) | Modal::SessionContainers(_, _, _, items, _) => {
            (items.len() as u16).saturating_add(3)
        }
        Modal::Themes(items, ..) => (items.len() as u16).saturating_add(4),
        _ => available,
    };
    let modal_height = desired.min(available).max(2).min(available);
    Rect {
        x: width.saturating_sub(modal_width) / 2,
        y: 3 + available.saturating_sub(modal_height) / 2,
        width: modal_width,
        height: modal_height,
    }
}
fn input_lines(label: &str, text: &str, width: usize, height: usize) -> Vec<String> {
    if height == 0 {
        return Vec::new();
    }
    let mut lines = if height >= 3 {
        vec![
            format!("{} / {} bytes", text.len(), input_limit(label)),
            String::new(),
        ]
    } else {
        Vec::new()
    };
    let remaining = height.saturating_sub(lines.len()).max(1);
    let wrapped = wrap_text(&format!("> {text}_"), width.max(1));
    let entries: Vec<_> = wrapped.lines().collect();
    let start = entries.len().saturating_sub(remaining);
    let first_visible = lines.len();
    lines.extend(entries.iter().skip(start).map(|line| (*line).to_owned()));
    if start > 0
        && let Some(line) = lines.get_mut(first_visible)
        && line.chars().count() > 1
    {
        *line = format!("<{}", line.chars().skip(1).collect::<String>());
    }
    lines
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
    if w < 28 || h < 10 {
        for y in 0..h {
            row(out, y, w, "", fg, bg).map_err(|_| "draw failed")?;
        }
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
    let header = if w >= 64 {
        format!(" KBORG  /  {mode}")
    } else {
        format!("KBorg | {mode}")
    };
    row(
        out,
        0,
        w,
        &header,
        color("status_text"),
        color("status_background"),
    )
    .map_err(|_| "draw failed")?;
    if w >= 90 {
        let badge = format!(" {}  v{} ", app.theme.name, env!("CARGO_PKG_VERSION"));
        let width = badge.len().min(usize::from(w / 3)) as u16;
        cell(
            out,
            w - width,
            0,
            width,
            &badge,
            color("status_text"),
            color("status_background"),
        )
        .map_err(|_| "draw failed")?;
    }
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
    row(out, 2, w, "", fg, bg).map_err(|_| "draw failed")?;
    let healthy = visible
        .iter()
        .filter(|object| health(object) == Health::Healthy)
        .count();
    let attention = visible
        .iter()
        .filter(|object| health(object) == Health::Attention)
        .count();
    let resource = app
        .kinds
        .get(app.kind_index)
        .map(resource_label)
        .unwrap_or_else(|| app.kind());
    let bands = if w >= 64 {
        vec![
            (
                format!(
                    " {freshness}{} ",
                    if app.truncated { " / TRUNCATED" } else { "" }
                ),
                if app.stale || app.truncated {
                    "warning"
                } else {
                    "accent"
                },
            ),
            (format!(" {} {resource} ", visible.len()), "text"),
            (format!(" OK {healthy} "), "success"),
            (
                format!(" ATTENTION {attention} "),
                if attention > 0 { "warning" } else { "muted" },
            ),
            (
                format!(
                    " SORT:{}{}",
                    app.sort.label(),
                    if app.filter.is_empty() {
                        String::new()
                    } else {
                        format!("  /{}", app.filter)
                    }
                ),
                "muted",
            ),
        ]
    } else {
        vec![(
            format!(
                "{freshness}{} | {} {resource} | !{attention}",
                if app.truncated { " TRUNCATED" } else { "" },
                visible.len()
            ),
            "accent",
        )]
    };
    let mut x = 0u16;
    for (text, role) in bands {
        let width = (text.len() as u16).min(w.saturating_sub(x));
        if width == 0 {
            break;
        }
        cell(out, x, 2, width, &text, color(role), bg).map_err(|_| "draw failed")?;
        x += width;
    }
    let layout = Layout::new(w, h);
    let height = layout.table_rows();
    let sidebar = layout.sidebar.or_else(|| {
        app.left_focus.then_some(Rect {
            height: layout.table.height + layout.preview.map(|rect| rect.height).unwrap_or(0),
            ..layout.table
        })
    });
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
        let entries = navigation(&app.kinds);
        let start = nav_start(&app.kinds, app.kind_index, rows);
        for (r, entry) in entries.iter().skip(start).take(rows).enumerate() {
            let (text, selected, heading) = match entry {
                NavEntry::Group(title) => (format!(" {title}"), false, true),
                NavEntry::Resource(index) => (
                    format!(
                        " {} {}",
                        if *index == app.kind_index { ">" } else { " " },
                        resource_label(&app.kinds[*index])
                    ),
                    *index == app.kind_index,
                    false,
                ),
            };
            cell(
                out,
                rect.x + 1,
                rect.y + 1 + r as u16,
                rect.width - 2,
                &text,
                if selected {
                    color("selection_text")
                } else if heading {
                    color("accent")
                } else {
                    color("muted")
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
            color("selection_text"),
            color("selection_background"),
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
    if let Some(rect) = layout.preview
        && (layout.sidebar.is_some() || !app.left_focus)
    {
        panel(
            out,
            rect,
            "SELECTED / Enter overview",
            false,
            color("muted"),
            bg,
        )
        .map_err(|_| "draw failed")?;
        let lines = if let Some(object) = visible.get(app.selected) {
            let spec = if object["kind"] == "Pod" {
                &object["spec"]
            } else {
                &object["spec"]["template"]["spec"]
            };
            let image = spec["containers"]
                .as_array()
                .and_then(|containers| containers.first())
                .map(|container| clean_value(&container["image"]))
                .unwrap_or_else(|| "-".into());
            vec![
                format!("{} / {}", namespace(object), name(object)),
                format!(
                    "{}   ready {}   restarts {}   age {}",
                    summary(object),
                    readiness(object),
                    restarts(object),
                    age(object, k8s_openapi::jiff::Timestamp::now().as_second())
                ),
                if object["kind"] == "Pod" {
                    format!(
                        "Node {}   IP {}",
                        clean_value(&object["spec"]["nodeName"]),
                        clean_value(&object["status"]["podIP"])
                    )
                } else {
                    format!(
                        "Kind {}   version {}",
                        clean_value(&object["kind"]),
                        clean_value(&object["metadata"]["resourceVersion"])
                    )
                },
                format!("Image {image}"),
                "Enter overview   J JSON   y YAML   a actions".into(),
            ]
        } else {
            vec!["Select an object to inspect readiness, identity and image.".into()]
        };
        for (r, line) in lines.iter().take(usize::from(rect.height - 2)).enumerate() {
            cell(
                out,
                rect.x + 1,
                rect.y + 1 + r as u16,
                rect.width - 2,
                line,
                if r == 0 {
                    color("accent")
                } else if r == 4 {
                    color("muted")
                } else {
                    fg
                },
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
            " Enter overview  J JSON  y YAML  l logs  f follow  m metrics  a actions  / filter"
        } else {
            "Enter inspect | / filter | : command | a actions"
        },
        color("accent"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    row(out, h - 1, w, if w >= 80 { " : resource   S sort   Tab focus   c context   n namespace   r refresh   ? help   q quit" }
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
                Modal::Follow(..) => "LOG STREAM / rolling 256KiB / End follows latest",
                Modal::Containers(..) => "CHOOSE CONTAINER",
                Modal::Image(..) => "CHANGE DEPLOYMENT IMAGE",
                Modal::Detail(..) => "REDACTED INSPECTION / Esc close",
                Modal::Overview(..) => "OBJECT OVERVIEW / J JSON / y YAML / Esc close",
                Modal::LiveOverview(..) => "LIVE CONTEXT OVERVIEW / independent watches",
                Modal::SessionContainers(..) => "POD SHELL / choose ordinary container",
                Modal::SessionConfirm(..) => "REVIEW POD SHELL / privileged interactive session",
                Modal::Metrics(..) if app.metrics_resources => {
                    "METRICS + RESOURCE COMPARISONS / extra reads enabled"
                }
                Modal::Metrics(..) => "METRICS / M enables requests-limit comparisons",
                Modal::Themes(..) => "THEMES / Enter save / Esc restore",
                Modal::NewTheme(..) => "CREATE CUSTOM THEME / from preview colors",
                Modal::Confirm(..) => "REVIEW MUTATION / explicit confirmation required",
                Modal::Scale(..) => "SCALE DEPLOYMENT",
                Modal::Resources(..) => "RESOURCE BUDGET / reviewed deployment edit",
                Modal::Help(..) => "HELP / PgUp PgDown / Esc close",
                Modal::None => "",
                Modal::Filter(..) => "",
            };
            let rect = modal_rect(modal, w, h);
            panel(
                out,
                rect,
                title,
                true,
                if matches!(modal, Modal::Confirm(..) | Modal::SessionConfirm(..)) {
                    color("warning")
                } else {
                    color("accent")
                },
                bg,
            )
            .map_err(|_| "draw failed")?;
            let height = usize::from(rect.height.saturating_sub(2));
            let lines = match modal {
                Modal::Input(label, text) => input_lines(label, text, usize::from(rect.width.saturating_sub(2)), height),
                Modal::Detail(text, offset)
                | Modal::Metrics(text, offset)
                | Modal::LiveOverview(text, offset) => {
                    let wrapped = wrap_text(text, usize::from(rect.width.saturating_sub(2)));
                    let offset = (*offset).min(wrapped.lines().count().saturating_sub(1));
                    wrapped.lines().skip(offset).map(str::to_owned).collect()
                }
                Modal::Follow(text, offset) => {
                    let wrapped = wrap_text(text, usize::from(rect.width.saturating_sub(2)));
                    let offset = if *offset == usize::MAX {
                        wrapped.lines().count().saturating_sub(height)
                    } else {
                        (*offset).min(wrapped.lines().count().saturating_sub(1))
                    };
                    wrapped.lines().skip(offset).map(str::to_owned).collect()
                }
                Modal::Overview(object, offset) => {
                    let wrapped = wrap_text(
                        &overview_text(object, &app.context),
                        usize::from(rect.width.saturating_sub(2)),
                    );
                    wrapped
                        .lines()
                        .skip((*offset).min(wrapped.lines().count().saturating_sub(1)))
                        .map(str::to_owned)
                        .collect()
                }
                Modal::SessionContainers(_, name, _, names, index) => {
                    std::iter::once(format!("Pod {name} / direct /bin/sh -i / review required"))
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
                Modal::Containers(_, name, names, index, previous, follow) => {
                    std::iter::once(format!(
                        " CONTAINER for {name} | {} | Enter select / Esc cancel",
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
                        " CHANGE IMAGE {name} | {}",
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
                    std::iter::once(format!(" {label} | arrows / Enter select / Esc cancel"))
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
                Modal::Resources(_,_,name,_,text) => wrap_text(&format!("Deployment {name}\n\ncontainer CPU_REQUEST MEMORY_REQUEST CPU_LIMIT MEMORY_LIMIT\n\n> {text}_\n\nExample: app 100m 128Mi 500m 512Mi\nEnter server dry-run / full review; Esc cancel"), usize::from(rect.width.saturating_sub(2))).lines().map(str::to_owned).collect(),
                Modal::Scale(_, _, name, _, text) => vec![
                    format!(" SCALE {name} | enter replicas 0..20: {text}_"),
                    " Enter server dry-run/review | Esc cancel".into(),
                ],
                Modal::SessionConfirm(prepared, text) => {
                    confirmation_lines(prepared.summary(), text, w.saturating_sub(1))
                }
                Modal::Help(offset) => {
                    let wrapped = wrap_text(TUI_HELP, usize::from(rect.width.saturating_sub(2)));
                    wrapped
                        .lines()
                        .skip((*offset).min(wrapped.lines().count().saturating_sub(1)))
                        .map(str::to_owned)
                        .collect()
                }
                Modal::None => vec![],
                Modal::Filter(..) => vec![],
            };
            for (index, line) in lines.iter().take(height).enumerate() {
                let selected = line.starts_with(" > ");
                cell(
                    out,
                    rect.x + 1,
                    rect.y + 1 + index as u16,
                    rect.width.saturating_sub(2),
                    line,
                    if matches!(modal, Modal::Confirm(..) | Modal::SessionConfirm(..)) {
                        color("warning")
                    } else if selected {
                        color("selection_text")
                    } else if !line.is_empty() && line.chars().all(|c| !c.is_ascii_lowercase()) {
                        color("accent")
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
            } else if let Modal::SessionConfirm(prepared, text) = modal {
                if confirmation_fits(prepared.summary(), text, w, h) {
                    "Enter start session | Esc cancel | Ctrl-] ends session"
                } else {
                    "Esc cancel | Enlarge terminal to review complete session."
                }
            } else if matches!(modal, Modal::Input("Command", _)) {
                "Esc cancel | Enter run | :pods :deploy :svc :nodes | :ns NAME | :sort FIELD"
            } else if matches!(modal, Modal::LiveOverview(..)) {
                "Esc stops all context watches | PgUp/PgDown scroll"
            } else if matches!(modal, Modal::Overview(..)) {
                "Esc close | J redacted JSON | y YAML | PgUp/PgDown scroll"
            } else if matches!(modal, Modal::Metrics(..)) {
                "Esc close | m refresh | M requests/limits (extra reads) | PgUp/PgDown"
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
    let mut terminal_guard = Some(Terminal::enter(options.mouse)?);
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
        metrics_resources: false,
        overview: None,
        pending_session: None,
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
        if let Some((prepared, typed)) = app.pending_session.take() {
            app.worker.logs.cancel();
            app.overview = None;
            app.epoch += 1;
            app.worker.watch.invalidate(app.epoch);
            app.watching = false;
            app.stale = true;
            drop(terminal_guard.take());
            app.send(Task::RunSession(prepared, typed));
            while app.write_inflight {
                app.receive();
                if app.worker.tx.is_closed() && app.write_inflight {
                    app.write_inflight = false;
                    app.busy = false;
                    app.status =
                        "Session worker ended; outcome may be unknown. Reconcile before retrying."
                            .into();
                    app.sticky_status = true;
                    app.stale = true;
                }
                if app.write_inflight {
                    std::thread::sleep(Duration::from_millis(50));
                }
            }
            terminal_guard = Some(Terminal::enter(app.options.mouse)?);
            app.refresh = true;
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
                    app.mouse(mouse, w, h);
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
        println!("{} ({}) | validated palette", theme.name, theme.id);
        return Ok(());
    }
    let mut out = io::stdout();
    let bg = if color {
        theme.get("background").color()
    } else {
        Color::Reset
    };
    for (role, text) in [
        ("text", "Resource: demo/api | 2/2 ready"),
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
                    allow_extensions: false,
                    log_seconds: 3600,
                    interactive: false,
                    session_seconds: 900,
                    metrics_resources: false,
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
                metrics_resources: false,
                overview: None,
                pending_session: None,
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
        let mut painted = vec![vec![false; usize::from(width)]; usize::from(height)];
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
                painted[y][x] = true;
                x += 1;
            }
            index += 1;
        }
        assert!(
            painted.iter().flatten().all(|painted| *painted),
            "frame left stale cells behind"
        );
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
    fn navigation_groups_short_names_and_mouse_hits_visible_rows() {
        let (mut app, _replies, _commands) = harness();
        app.left_focus = true;
        let frame = screen(&app, 120, 32).join("\n");
        assert!(frame.contains("WORKLOADS") && frame.contains("NETWORK"));
        assert!(frame.contains("> Pods") && !frame.contains("statefulsets.apps"));
        let epoch = app.epoch;
        app.mouse(
            MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 3,
                row: 4,
                modifiers: KeyModifiers::NONE,
            },
            120,
            32,
        );
        assert_eq!(app.epoch, epoch); // A category heading is not a resource.
        app.mouse(
            MouseEvent {
                kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
                column: 3,
                row: 6,
                modifiers: KeyModifiers::NONE,
            },
            120,
            32,
        );
        assert_eq!(app.kind(), "deployments.apps");
        app.left_focus = true;
        app.move_resource(1);
        assert_eq!(app.kind(), "events");
    }
    #[test]
    fn polished_frame_shows_health_selection_preview_and_centered_actions_without_clear() {
        let (mut app, _replies, _commands) = harness();
        app.items = vec![
            serde_json::json!({"kind":"Pod","metadata":{"name":"api","namespace":"test","uid":"pod1"},"spec":{"nodeName":"worker-a","containers":[{"name":"app","image":"example/api:v2"}]},"status":{"phase":"Running","containerStatuses":[{"name":"app","ready":true,"restartCount":0}]}}),
        ];
        let frame = screen(&app, 120, 32).join("\n");
        for label in [
            "OK 1",
            "ATTENTION 0",
            "SELECTED",
            "worker-a",
            "example/api:v2",
        ] {
            assert!(frame.contains(label));
        }
        let mut output = Vec::new();
        assert!(render(&app, 120, 32, &mut output).is_ok());
        assert!(!output.windows(4).any(|window| window == b"\x1b[2J"));
        app.modal = Modal::Picker("Actions", app.action_menu(), 0);
        let rect = modal_rect(&app.modal, 120, 32);
        assert!(rect.x > 0 && rect.y > 3 && rect.width < 120);
        let frame = screen(&app, 120, 32).join("\n");
        assert!(frame.contains("[OFF]") && frame.contains("Interactive pod shell"));
    }
    #[test]
    fn human_overview_uses_redacted_object_fields_and_json_remains_accessible() {
        let (mut app, replies, mut commands) = harness();
        let mut pod = serde_json::json!({"kind":"Pod","metadata":{"name":"api","namespace":"test","uid":"pod1","resourceVersion":"9"},"spec":{"nodeName":"worker-a","containers":[{"name":"app","image":"example/api:v2"}],"password":"canary-secret"},"status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}],"containerStatuses":[{"name":"app","ready":true,"restartCount":3}]}});
        safety::redact(&mut pod);
        let text = overview_text(&pod, "current-context");
        for label in [
            "IDENTITY",
            "CONTAINERS",
            "SCHEDULING",
            "worker-a",
            "example/api:v2",
            "Ready",
        ] {
            assert!(text.contains(label));
        }
        assert!(!text.contains("canary-secret"));
        app.items = vec![pod.clone()];
        assert!(app.key(KeyCode::Enter).is_ok());
        assert!(matches!(commands.try_recv(), Ok((_, Task::Inspect(..)))));
        assert!(replies.send((12, Reply::Object(Box::new(pod)))).is_ok());
        app.receive();
        assert!(matches!(app.modal, Modal::Overview(..)));
        assert!(app.key(KeyCode::Char('J')).is_ok());
        assert!(matches!(app.modal, Modal::Detail(..)));
    }
    #[test]
    fn pod_shell_picker_preserves_captured_identity_and_capabilities() {
        let (mut app, _replies, mut commands) = harness();
        app.items = vec![
            serde_json::json!({"kind":"Pod","metadata":{"name":"first","namespace":"test","uid":"first-uid"},"spec":{"containers":[{"name":"app"}]}}),
        ];
        assert!(app.key(KeyCode::Char('E')).is_ok());
        assert!(matches!(app.modal, Modal::None));
        app.options.connection.writable = true;
        assert!(app.key(KeyCode::Char('E')).is_ok());
        assert!(matches!(app.modal, Modal::None));
        app.options.connection.allow_exec = true;
        assert!(app.key(KeyCode::Char('E')).is_ok());
        app.items = vec![object("replacement")];
        assert!(app.key(KeyCode::Enter).is_ok());
        match commands.try_recv() {
            Ok((_, Task::PrepareSession(namespace, pod, container, uid, seconds))) => {
                assert_eq!(
                    (
                        namespace.as_str(),
                        pod.as_str(),
                        container.as_str(),
                        uid.as_deref(),
                        seconds
                    ),
                    ("test", "first", "app", Some("first-uid"), 900)
                );
            }
            _ => panic!("shell picker changed target"),
        }
    }
    #[test]
    fn log_follow_defaults_to_newest_window_and_end_restores_tail() {
        let (mut app, _replies, _commands) = harness();
        let text = (0..100)
            .map(|index| format!("line-{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.modal = Modal::Follow(text, usize::MAX);
        assert!(screen(&app, 60, 12).join("\n").contains("line-99"));
        assert!(app.key(KeyCode::Home).is_ok());
        assert!(screen(&app, 60, 12).join("\n").contains("line-0"));
        assert!(app.key(KeyCode::End).is_ok());
        assert!(matches!(app.modal, Modal::Follow(_, usize::MAX)));
    }
    #[test]
    fn live_context_overview_updates_and_close_rejects_late_feed() -> Result<()> {
        let rt = runtime()?;
        let create = || {
            rt.block_on(async {
                let feed = crate::overview::OverviewFeed::new(
                    &Connection {
                        demo: true,
                        ..Default::default()
                    },
                    &["demo".into()],
                )?;
                tokio::time::sleep(Duration::from_millis(20)).await;
                Ok::<_, String>(feed)
            })
        };
        let (mut app, replies, _commands) = harness();
        app.modal = Modal::LiveOverview("Connecting...".into(), 0);
        assert!(replies.send((12, Reply::Overview(create()?))).is_ok());
        app.receive();
        assert!(app.overview.is_some());
        let frame = screen(&app, 80, 24).join("\n");
        assert!(frame.contains("demo"));
        assert!(frame.contains("CONTEXT | NAMESPACE | PODS | UNHEALTHY | STATE"));
        assert!(!frame.contains("CONTEXTNAMESPACE"));
        app.key(KeyCode::Esc)?;
        assert!(app.overview.is_none());
        assert!(replies.send((12, Reply::Overview(create()?))).is_ok());
        app.receive();
        assert!(app.overview.is_none() && matches!(app.modal, Modal::None));
        Ok(())
    }
    #[test]
    fn overview_input_accepts_long_names_and_four_full_utf8_names() {
        let (mut app, _replies, mut commands) = harness();
        let names = vec!["a".repeat(150), "b".repeat(160)];
        app.modal = Modal::Input(OVERVIEW_CONTEXTS_LABEL, String::new());
        for c in names.join(",").chars() {
            assert!(app.key(KeyCode::Char(c)).is_ok());
        }
        assert!(app.key(KeyCode::Enter).is_ok());
        match commands.try_recv() {
            Ok((_, Task::Overview(_, entered))) => assert_eq!(entered, names),
            _ => panic!("long overview names were not accepted"),
        }
        app.busy = false;
        app.command_epoch = None;
        let names: Vec<_> = ['a', 'b', 'c', 'd']
            .iter()
            .map(|prefix| format!("{prefix}{}", "é".repeat(126)))
            .collect();
        let text = names.join(",");
        assert!(names.iter().all(|name| name.len() == 253));
        assert_eq!(text.len(), OVERVIEW_INPUT_BYTES);
        app.modal = Modal::Input(OVERVIEW_CONTEXTS_LABEL, String::new());
        for c in text.chars() {
            assert!(app.key(KeyCode::Char(c)).is_ok());
        }
        assert!(app.key(KeyCode::Char('x')).is_ok());
        match &app.modal {
            Modal::Input(_, entered) => assert_eq!(entered, &text),
            _ => panic!("input modal changed"),
        }
        assert!(app.key(KeyCode::Enter).is_ok());
        match commands.try_recv() {
            Ok((_, Task::Overview(_, entered))) => assert_eq!(entered, names),
            _ => panic!("four maximum-length overview names were not accepted"),
        }
        app.busy = false;
        app.modal = Modal::Input(OVERVIEW_CONTEXTS_LABEL, "a,b,c,d,e".into());
        assert!(app.key(KeyCode::Enter).is_ok());
        assert!(commands.try_recv().is_err());
        assert!(app.status.contains("up to 4"));
    }
    #[test]
    fn overview_input_enforces_byte_boundary_and_keeps_active_tail_visible() {
        let (mut app, _replies, _commands) = harness();
        app.modal = Modal::Input(
            OVERVIEW_CONTEXTS_LABEL,
            "x".repeat(OVERVIEW_INPUT_BYTES - 1),
        );
        assert!(app.key(KeyCode::Char('é')).is_ok()); // Two bytes cannot cross the remaining one-byte budget.
        match &app.modal {
            Modal::Input(_, text) => assert_eq!(text.len(), OVERVIEW_INPUT_BYTES - 1),
            _ => panic!("input modal changed"),
        }
        assert!(app.key(KeyCode::Char('x')).is_ok());
        assert!(app.key(KeyCode::Backspace).is_ok());
        assert!(app.key(KeyCode::Backspace).is_ok());
        assert!(app.key(KeyCode::Char('é')).is_ok());
        match &app.modal {
            Modal::Input(_, text) => {
                assert_eq!(text.len(), OVERVIEW_INPUT_BYTES);
                assert!(text.ends_with('é'));
            }
            _ => panic!("input modal changed"),
        }
        let text = format!("{},ACTIVE-TAIL", "a".repeat(155));
        app.modal = Modal::Input(OVERVIEW_CONTEXTS_LABEL, text.clone());
        for (width, height) in [(28, 10), (48, 12), (80, 24), (120, 32)] {
            let rect = modal_rect(&app.modal, width, height);
            let lines = input_lines(
                OVERVIEW_CONTEXTS_LABEL,
                &text,
                usize::from(rect.width - 2),
                usize::from(rect.height - 2),
            );
            assert!(lines.join("").ends_with("ACTIVE-TAIL_"));
            assert!(screen(&app, width, height).join("\n").contains("TAIL_"));
        }
        assert_eq!(
            input_lines("Command", "abcdefgh", 10, 1),
            vec!["_".to_owned()]
        );
        app.modal = Modal::Input("Namespace", "x".repeat(99));
        assert!(app.key(KeyCode::Char('é')).is_ok());
        match &app.modal {
            Modal::Input(_, text) => assert_eq!(text.len(), 99),
            _ => panic!("namespace modal changed"),
        }
        assert_eq!(input_limit("Command"), 100);
        assert_eq!(input_limit("Namespace"), 100);
    }
    #[test]
    fn metrics_extra_resource_reads_require_explicit_uppercase_key() {
        let (mut app, _replies, mut commands) = harness();
        assert!(app.key(KeyCode::Char('m')).is_ok());
        assert!(matches!(
            commands.try_recv(),
            Ok((_, Task::Metrics(_, _, false)))
        ));
        app.busy = false;
        app.command_epoch = None;
        app.modal = Modal::None;
        assert!(app.key(KeyCode::Char('M')).is_ok());
        assert!(matches!(
            commands.try_recv(),
            Ok((_, Task::Metrics(_, _, true)))
        ));
        assert!(
            screen(&app, 80, 24)
                .join("\n")
                .contains("extra reads enabled")
        );
    }
    #[test]
    fn resource_budget_form_captures_uid_and_validates_before_preparation() {
        let (mut app, _replies, mut commands) = harness();
        app.kind_index = 1;
        app.items = vec![object("first")];
        assert!(app.key(KeyCode::Char('V')).is_ok());
        assert!(matches!(app.modal, Modal::None));
        app.options.connection.writable = true;
        assert!(app.key(KeyCode::Char('V')).is_ok());
        app.items = vec![object("replacement")];
        for character in "app 100m 128Mi 500m 512Mi".chars() {
            assert!(app.key(KeyCode::Char(character)).is_ok());
        }
        assert!(app.key(KeyCode::Enter).is_ok());
        match commands.try_recv() {
            Ok((
                _,
                Task::Prepare(
                    _,
                    namespace,
                    name,
                    uid,
                    Action::SetResources {
                        container,
                        resources,
                    },
                ),
            )) => {
                assert_eq!(
                    (namespace.as_str(), name.as_str(), uid.as_deref()),
                    ("test", "first", Some("uid-first"))
                );
                assert_eq!(container, "app");
                assert_eq!(
                    (
                        resources.cpu_request.as_str(),
                        resources.memory_request.as_str(),
                        resources.cpu_limit.as_str(),
                        resources.memory_limit.as_str()
                    ),
                    ("100m", "128Mi", "500m", "512Mi")
                );
            }
            _ => panic!("resource budget form changed target"),
        }
        app.busy = false;
        app.modal = Modal::Resources(
            "deployments.apps".into(),
            "test".into(),
            "first".into(),
            Some("uid-first".into()),
            "app 2 1Gi 1 512Mi".into(),
        );
        assert!(app.key(KeyCode::Enter).is_ok());
        assert!(matches!(app.modal, Modal::Resources(..)));
        assert!(commands.try_recv().is_err());
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
        assert!(text.len() <= crate::logs::MAX_LOG_BYTES);
        assert!(text.contains("older lines discarded"));
        assert!(slot.append(generation, "newest retained line"));
        assert!(
            slot.take()
                .unwrap_or_default()
                .ends_with("newest retained line\n")
        );
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
