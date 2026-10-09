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
    fn enter() -> Result<Self> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err("interactive mode needs a terminal; use list/get --json or --help".into());
        }
        terminal::enable_raw_mode().map_err(|_| "cannot enable raw mode")?;
        if execute!(
            io::stdout(),
            EnterAlternateScreen,
            DisableLineWrap,
            EnableMouseCapture,
            cursor::Hide
        )
        .is_err()
        {
            let _ = terminal::disable_raw_mode();
            return Err("cannot enter alternate screen".into());
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
            eprintln!("K8Borg encountered an internal error; no object contents recorded.");
        }));
        Ok(Self)
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
    Logs(String, String, bool),
    Discover,
    Prepare(String, String, String, Option<String>, Action),
    Execute(Box<Prepared>, Approval),
}
enum Reply {
    Connected(String, String, String, Vec<Kind>),
    Items(Value),
    Detail(String),
    Catalog(Vec<Kind>),
    Prepared(Box<Prepared>),
    Notice(String),
    Failure(String),
    Watching,
    Metrics(Result<String>),
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

struct Worker {
    tx: tokio::sync::mpsc::Sender<(u64, Task)>,
    rx: Receiver<(u64, Reply)>,
    watch: WatchMailbox,
}
fn worker() -> Worker {
    let (tx, mut commands) = tokio::sync::mpsc::channel::<(u64, Task)>(4);
    let (results, rx) = mpsc::sync_channel(4);
    let watch: WatchMailbox = std::sync::Arc::new(WatchSlot::default());
    let watch_results = watch.clone();
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
                    task => {
                        if let Some(g) = gateway.as_mut() {
                            let future = async {
                                Ok(match task {
                                    Task::List(k,n) => Reply::Items(g.list(&k,&n).await?),
                                    Task::Get(k,n,name) => Reply::Detail(serde_json::to_string_pretty(&g.get(&k,&n,&name).await?).map_err(|_| "serialization failed")?),
                                    Task::Logs(n,name,previous) => Reply::Detail(g.logs(&n,&name,None,previous).await?),
                                    Task::Metrics(scope,n) => Reply::Metrics(crate::metrics::collect(g,scope,&n).await.map(|v| v.text())),
                                    Task::Discover => Reply::Catalog(g.discover().await?),
                                    Task::Prepare(k,n,name,uid,a) => {
                                        let prepared = g.prepare(&k,&n,&name,a).await?;
                                        if uid.as_deref().is_some_and(|u| u != prepared.target.uid) { return Err("selected object was replaced; reselect it before approval".into()); }
                                        Reply::Prepared(Box::new(prepared))
                                    },
                                    Task::Execute(p,a) => Reply::Notice(g.execute(*p,a).await?),
                                    Task::Connect(_) | Task::Watch(..) => return Err("invalid worker task".into()),
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
    Worker { tx, rx, watch }
}

enum Modal {
    None,
    Input(&'static str, String),
    Detail(String, usize),
    Picker(&'static str, Vec<String>, usize),
    Themes(Vec<Theme>, usize, Theme),
    Confirm(Box<Prepared>, String),
    Scale(String, String, String, Option<String>, String),
    Help(usize),
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
        self.items
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
            .collect()
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
                Reply::Detail(text) => self.modal = Modal::Detail(text, 0),
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
                Reply::Metrics(result) => match result {
                    Ok(text) => {
                        let width = terminal::size()
                            .map(|(w, _)| usize::from(w).saturating_sub(2))
                            .unwrap_or(78)
                            .max(1);
                        self.modal = Modal::Detail(wrap_text(&text, width), 0);
                    }
                    Err(error) => {
                        let width = terminal::size()
                            .map(|(w, _)| usize::from(w).saturating_sub(2))
                            .unwrap_or(78)
                            .max(1);
                        self.modal = Modal::Detail(
                            wrap_text(
                                &format!(
                                    "METRICS unavailable\n{error}\nResource browsing remains available."
                                ),
                                width,
                            ),
                            0,
                        );
                    }
                },
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
        changed
    }
    fn theme_picker(&mut self) -> Result<()> {
        let all = themes::all()?;
        let index = all.iter().position(|t| t.id == self.theme.id).unwrap_or(0);
        self.modal = Modal::Themes(all, index, self.theme.clone());
        Ok(())
    }
    fn prepare(&mut self, action: Action) {
        if !self.options.connection.writable {
            self.status = "READ ONLY: enable --write before requesting a mutation.".into();
            return;
        }
        let allowed = match &action {
            Action::Scale(_) | Action::Restart => self.kind() == "deployments.apps",
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
            Modal::Input(label, mut text) => match code {
                KeyCode::Esc => {}
                KeyCode::Enter => {
                    if label == "Filter" {
                        self.filter = text;
                        self.selected = 0;
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
                KeyCode::Down | KeyCode::Char('j') => {
                    offset = (offset + 1).min(text.lines().count().saturating_sub(1));
                    self.modal = Modal::Detail(text, offset);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal = Modal::Detail(text, offset.saturating_sub(1));
                }
                KeyCode::PageDown => self.modal = Modal::Detail(text, (offset + 15).min(10000)),
                KeyCode::PageUp => self.modal = Modal::Detail(text, offset.saturating_sub(15)),
                _ => self.modal = Modal::Detail(text, offset),
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
                        if label == "Context" {
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
                _ => self.modal = Modal::Themes(list, index, old),
            },
            Modal::Confirm(p, mut text) => match code {
                KeyCode::Esc => self.status = "Operation canceled.".into(),
                KeyCode::Enter => {
                    let (width, height) = terminal::size().map_err(|_| "terminal size failed")?;
                    if confirmation_lines(&p.summary, &text, width).len()
                        > height.saturating_sub(5) as usize
                    {
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
                KeyCode::PageDown => self.modal = Modal::Help((offset + 15).min(40)),
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
                KeyCode::Down | KeyCode::Char('j') => {
                    if self.left_focus {
                        self.kind_index =
                            (self.kind_index + 1).min(self.kinds.len().saturating_sub(1));
                        self.changed();
                    } else {
                        self.selected =
                            (self.selected + 1).min(self.visible().len().saturating_sub(1));
                    }
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.left_focus {
                        self.kind_index = self.kind_index.saturating_sub(1);
                        self.changed();
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
                KeyCode::Char('/') => self.modal = Modal::Input("Filter", self.filter.clone()),
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
                KeyCode::Char('m') => self.send(Task::Metrics(
                    if self.kind() == "nodes" {
                        crate::metrics::MetricsScope::Nodes
                    } else {
                        crate::metrics::MetricsScope::Pods
                    },
                    self.namespace.clone(),
                )),
                KeyCode::Char('w') => {
                    if self.options.connection.demo {
                        self.status =
                            "DEMO uses synthetic polling; live watches require a cluster.".into();
                    } else {
                        self.options.poll = !self.options.poll;
                        self.changed();
                    }
                }
                KeyCode::Char('l') | KeyCode::Char('L') => {
                    if let Some((k, n, name)) = self.selected_target() {
                        if k == "pods" {
                            self.send(Task::Logs(n, name, code == KeyCode::Char('L')));
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

fn display(text: &str, width: usize) -> String {
    safety::safe_text(text, width)
        .chars()
        .map(|c| if c.is_ascii() { c } else { '?' })
        .take(width)
        .collect()
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
        style::SetForegroundColor(fg),
        style::SetBackgroundColor(bg),
        style::Print(format!(
            "{:<width$}",
            display(text, width as usize),
            width = width as usize
        ))
    )
}
fn draw(app: &App) -> Result<()> {
    let (w, h) = terminal::size().map_err(|_| "cannot read terminal size")?;
    let mut out = io::stdout().lock();
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
        style::SetBackgroundColor(bg),
        terminal::Clear(ClearType::All)
    )
    .map_err(|_| "draw failed")?;
    if w < 50 || h < 12 {
        row(
            &mut out,
            0,
            w,
            "K8Borg: enlarge terminal to at least 50x12 (q to quit)",
            fg,
            bg,
        )
        .map_err(|_| "draw failed")?;
        out.flush().map_err(|_| "draw failed")?;
        return Ok(());
    }
    let mode = if app.options.connection.demo {
        "DEMO / SAMPLE DATA"
    } else if app.options.connection.writable {
        "PRODUCTION / WRITE ENABLED"
    } else {
        "PRODUCTION / READ ONLY"
    };
    let header = format!(" K8Borg >_  | {mode} | {}", app.theme.name);
    row(
        &mut out,
        0,
        w,
        &header,
        color("status_text"),
        color("status_background"),
    )
    .map_err(|_| "draw failed")?;
    row(
        &mut out,
        1,
        w,
        &format!(
            " context: {} | ns: {} | {}",
            app.context, app.namespace, app.server
        ),
        color("accent"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    row(
        &mut out,
        2,
        w,
        &format!(
            " [c] context [n] namespace [t] theme [m] metrics [w] {} | {}{}",
            if app.options.poll || app.options.connection.demo {
                "POLL"
            } else if app.watching && !app.stale && !app.truncated {
                "LIVE"
            } else {
                "WATCH"
            },
            if app.busy { "LOADING " } else { "" },
            if app.stale { "STALE" } else { "" }
        ),
        color("muted"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    let height = h.saturating_sub(7) as usize;
    let left = w.min(28) as usize;
    row(
        &mut out,
        3,
        w,
        &format!(
            " {:<left$}| NAMESPACE       NAME / STATUS",
            "RESOURCES (Tab)",
            left = left - 1
        ),
        color("muted"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    let visible = app.visible();
    let start = app.selected.saturating_sub(height.saturating_sub(1));
    let kstart = app.kind_index.saturating_sub(height.saturating_sub(1));
    for r in 0..height {
        let kind = app.kinds.get(kstart + r).map(Kind::key).unwrap_or_default();
        let marker = if app.kind_index == kstart + r {
            if app.left_focus { ">" } else { "*" }
        } else {
            " "
        };
        let mut line = format!(
            "{marker}{:<width$}|",
            display(&kind, left - 2),
            width = left - 2
        );
        if let Some(object) = visible.get(start + r) {
            let namespace = object["metadata"]["namespace"].as_str().unwrap_or("-");
            let name = object["metadata"]["name"].as_str().unwrap_or("-");
            line += &format!(
                " {:<15} {}  [{}]",
                display(namespace, 15),
                name,
                summary(object)
            );
        }
        let selected = !app.left_focus && start + r == app.selected && r + start < visible.len();
        row(
            &mut out,
            4 + r as u16,
            w,
            &line,
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
    row(
        &mut out,
        h - 3,
        w,
        &format!(
            " {}{} | filter: {}",
            app.status,
            if app.truncated { " [TRUNCATED]" } else { "" },
            app.filter
        ),
        if app.stale {
            color("warning")
        } else {
            color("muted")
        },
        bg,
    )
    .map_err(|_| "draw failed")?;
    row(
        &mut out,
        h - 2,
        w,
        " Enter details | l logs | / filter | r refresh | s scale | x restart | Del delete",
        color("accent"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    row(
        &mut out,
        h - 1,
        w,
        " q quit | AI / plugins / exec / forwarding disabled | No telemetry",
        color("muted"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    match &app.modal {
        Modal::None => {}
        modal => {
            for y in 3..h - 2 {
                row(&mut out, y, w, "", fg, bg).map_err(|_| "draw failed")?;
            }
            let lines = match modal {
                Modal::Input(label, text) => vec![
                    format!(" {label}: {text}_"),
                    " Enter apply | Esc cancel".into(),
                ],
                Modal::Detail(text, offset) => std::iter::once(
                    " DETAILS / LOGS / METRICS — PgUp/PgDown scroll; Esc close".into(),
                )
                .chain(text.lines().skip(*offset).map(|s| format!(" {s}")))
                .collect(),
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
                        " THEMES — arrows preview; Enter save; Esc restore".into(),
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
                Modal::Confirm(p, text) => confirmation_lines(&p.summary, text, w),
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
            };
            for (index, line) in lines.iter().take((h - 5) as usize).enumerate() {
                row(
                    &mut out,
                    3 + index as u16,
                    w,
                    line,
                    if matches!(modal, Modal::Confirm(..)) {
                        color("warning")
                    } else {
                        fg
                    },
                    bg,
                )
                .map_err(|_| "draw failed")?;
            }
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
    let _terminal = Terminal::enter()?;
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
                    match mouse.kind {
                        MouseEventKind::ScrollDown => {
                            app.selected =
                                (app.selected + 1).min(app.visible().len().saturating_sub(1));
                        }
                        MouseEventKind::ScrollUp => app.selected = app.selected.saturating_sub(1),
                        MouseEventKind::Down(_) => {
                            let (w, h) = terminal::size().map_err(|_| "terminal size failed")?;
                            if mouse.row >= 4 && mouse.row < h.saturating_sub(3) {
                                let row = (mouse.row - 4) as usize;
                                let height = h.saturating_sub(7) as usize;
                                if mouse.column < w.min(28) {
                                    let start =
                                        app.kind_index.saturating_sub(height.saturating_sub(1));
                                    app.kind_index =
                                        (start + row).min(app.kinds.len().saturating_sub(1));
                                    app.left_focus = false;
                                    app.changed();
                                } else {
                                    let start =
                                        app.selected.saturating_sub(height.saturating_sub(1));
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
        "             K8BORG               ",
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
            style::SetForegroundColor(if color {
                theme.get(role).color()
            } else {
                Color::Reset
            }),
            style::SetBackgroundColor(bg),
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
                    json: false,
                    splash: false,
                    poll: false,
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
                worker: Worker { tx, rx, watch },
            },
            replies,
            commands,
        )
    }
    fn object(name: &str) -> Value {
        serde_json::json!({"kind":"Deployment","metadata":{"name":name,"namespace":"test","uid":format!("uid-{name}")}})
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
        assert!(
            replies
                .send((12, Reply::Metrics(Err("Metrics API unavailable".into()))))
                .is_ok()
        );
        app.receive();
        assert_eq!(app.items.len(), 1);
        assert!(!app.stale);
        assert!(matches!(app.modal, Modal::Detail(..)));
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
