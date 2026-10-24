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
        self, DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
    },
    execute, queue,
    style::{self, Color},
    terminal::{
        self, BeginSynchronizedUpdate, ClearType, DisableLineWrap, EnableLineWrap,
        EndSynchronizedUpdate, EnterAlternateScreen, LeaveAlternateScreen,
    },
};
use serde_json::Value;
#[path = "ui_advice.rs"]
mod advice_ui;
#[path = "ui_dashboard.rs"]
mod dashboard_ui;
#[path = "ui_helm.rs"]
mod helm_ui;
#[path = "ui_navigation.rs"]
mod nav;
#[path = "ui_review.rs"]
mod review_ui;
#[path = "ui_selections.rs"]
mod selections_ui;
#[path = "ui_metrics.rs"]
mod table_metrics;
#[path = "ui_topbar.rs"]
mod topbar;
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
            EnableBracketedPaste,
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
                EndSynchronizedUpdate,
                LeaveAlternateScreen,
                DisableBracketedPaste,
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
            EndSynchronizedUpdate,
            LeaveAlternateScreen,
            DisableBracketedPaste,
            EnableLineWrap,
            DisableMouseCapture,
            cursor::Show,
            style::ResetColor
        );
    }
}

enum Task {
    Connect(Connection),
    List(String, String, String, String),
    Watch(String, String, String, String),
    Metrics(crate::metrics::MetricsScope, String, bool),
    Inspect(String, String, String),
    Drill(String, String, String),
    Get(String, String, String),
    Yaml(String, String, String),
    ExportResource(String, String, String),
    ExportHelm(crate::helm::Release),
    YamlDraft(String, String, String, Option<String>),
    YamlPrepare(Box<crate::ui_yaml::Editor>),
    YamlExecute(Box<crate::manifest_edit::PreparedYaml>, String),
    NodePrepare(String, Option<String>, crate::node_maintenance::Request),
    NodeExecute(Box<crate::node_maintenance::ReviewedNode>, String),
    Overview(Connection, Vec<String>),
    PrepareSession(String, String, Option<String>, crate::ops::Request),
    RunSession(Box<crate::ops::ReviewedOperation>, String),
    RunForward(Box<crate::ops::ReviewedOperation>, String, u64),
    CaptureLogs(LogTarget, crate::logs::PodLogSelection, u64),
    Containers(LogTarget, u64),
    Logs(LogTarget, u64, crate::logs::ReadOptions),
    Follow(LogTarget, u64, u32, crate::logs::ReadOptions),
    /// Dashboard `t`: list workloads per (context, read-only connection).
    DashboardWorkloads(Vec<(String, Box<Connection>)>),
    /// Cross-cluster workload tail: sources, generation, seconds, range.
    DashboardTail(
        Vec<crate::workload_tail::TailSource>,
        u64,
        u32,
        crate::logs::ReadOptions,
    ),
    /// Dashboard pane: connection, namespace, Pod, UID, generation, seconds, range.
    DashboardLogs(
        Box<Connection>,
        String,
        String,
        String,
        u64,
        u32,
        crate::logs::ReadOptions,
    ),
    Discover,
    Namespaces,
    Pulses(String),
    Secrets(String),
    /// RBAC subject list: namespace scope, subject kind filter, view title.
    Subjects(String, Option<&'static str>, &'static str),
    SecretValue(String, String, String, String),
    Xray(String, String),
    Workloads(String),
    Can(String, String),
    Directory(std::path::PathBuf),
    Manifest(std::path::PathBuf),
    Helm(helm_ui::Request, u64),
    HelmPrepare(Box<helm_ui::WriteDraft>, u64),
    AdvicePrepare(
        String,
        String,
        String,
        Option<String>,
        Option<std::path::PathBuf>,
    ),
    AdviceExecute(
        Box<crate::advice::PreparedAdvice>,
        String,
        tokio::sync::oneshot::Receiver<()>,
    ),
    HelmExecute(
        Box<crate::helm_lifecycle::Prepared>,
        String,
        u64,
        tokio::sync::oneshot::Receiver<()>,
    ),
    Prepare(String, String, String, Option<String>, Action),
    Execute(Box<Prepared>, Approval),
}
enum Reply {
    Connected(String, String, String, Vec<Kind>),
    Items(Value),
    Detail(String),
    Object(Box<Value>),
    Export(crate::exports::Draft),
    YamlDraft(Box<crate::manifest_edit::Draft>),
    YamlPrepared(
        Box<crate::ui_yaml::Editor>,
        Result<crate::manifest_edit::PreparedYaml>,
    ),
    NodePrepared(Box<crate::node_maintenance::ReviewedNode>),
    WriteOutcome(Result<String>),
    Drill(Box<Value>),
    Directory(crate::navigation_views::Directory),
    Secrets(crate::secrets::SecretIndex),
    Subjects(
        &'static str,
        String,
        crate::navigation_views::SubjectIndex,
    ),
    DashboardWorkloads(Vec<(String, Result<Vec<crate::workload_tail::WorkloadRef>>)>),
    SecretValue(String, String, Result<crate::secrets::Revealed>),
    Helm(u64, Result<helm_ui::Response>),
    HelmPrepared(u64, Result<Box<crate::helm_lifecycle::Prepared>>),
    AdvicePrepared(Result<(Option<crate::advice::PreparedAdvice>, String)>),
    AdviceOutcome(Result<crate::advice::AdviceOutput>),
    HelmOutcome(u64, Result<crate::helm_lifecycle::Outcome>),
    Overview(crate::overview::OverviewFeed),
    SessionPrepared(Box<crate::ops::ReviewedOperation>),
    ForwardQueued(u64),
    Membership(LogTarget, u64),
    Containers(LogTarget, u64, Vec<String>),
    Logs(u64, String),
    LogFailure(u64, String),
    Following,
    Catalog(Vec<Kind>),
    CatalogFailure(String),
    Namespaces(Vec<String>),
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
    forward: std::sync::Arc<ForwardSlot>,
}
const MAX_FORWARDS: usize = 8;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ForwardPhase {
    Starting,
    Listening,
    Stopping,
    Ended,
    Failed,
}
impl ForwardPhase {
    fn active(self) -> bool {
        matches!(self, Self::Starting | Self::Listening | Self::Stopping)
    }
    fn label(self) -> &'static str {
        match self {
            Self::Starting => "STARTING",
            Self::Listening => "LISTENING",
            Self::Stopping => "STOPPING",
            Self::Ended => "ENDED",
            Self::Failed => "FAILED",
        }
    }
}
struct ForwardEntry {
    id: u64,
    target: crate::ops::ForwardTarget,
    phase: ForwardPhase,
    message: String,
    cancel: Option<tokio::sync::oneshot::Sender<()>>,
}
#[derive(Default)]
struct ForwardState {
    generation: u64,
    next_id: u64,
    entries: Vec<ForwardEntry>,
}
#[derive(Default)]
struct ForwardSlot {
    state: std::sync::Mutex<ForwardState>,
}
impl ForwardSlot {
    fn generation(&self) -> u64 {
        self.state.lock().map(|s| s.generation).unwrap_or(u64::MAX)
    }
    fn begin(
        &self,
        generation: u64,
        epoch: u64,
        watch: &WatchSlot,
        target: crate::ops::ForwardTarget,
    ) -> Result<(u64, tokio::sync::oneshot::Receiver<()>)> {
        // Allocation, collision reservation and cancellation share one lock. Scope
        // cancellation invalidates approvals before any queued work can bind again.
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Forward status unavailable")?;
        if state.generation != generation
            || watch.epoch.load(std::sync::atomic::Ordering::SeqCst) != epoch
        {
            return Err(
                "Forward canceled before start: context or scope changed; review again.".into(),
            );
        }
        if state
            .entries
            .iter()
            .any(|entry| entry.phase.active() && entry.target.local_port == target.local_port)
        {
            return Err(format!(
                "Local port {} is reserved by another forward; stop it and wait for cleanup, or choose a different port.",
                target.local_port
            ));
        }
        if state.entries.len() >= MAX_FORWARDS {
            if let Some(index) = state.entries.iter().position(|entry| !entry.phase.active()) {
                state.entries.remove(index);
            } else {
                return Err(
                    "Eight forwards are active; stop a session and wait for cleanup first.".into(),
                );
            }
        }
        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or("Forward session IDs exhausted")?;
        let id = state.next_id;
        let (sender, receiver) = tokio::sync::oneshot::channel();
        state.entries.push(ForwardEntry {
            id,
            target,
            phase: ForwardPhase::Starting,
            message: "Preparing reviewed loopback listener...".into(),
            cancel: Some(sender),
        });
        Ok((id, receiver))
    }
    fn cancel(&self) -> u64 {
        let Ok(mut state) = self.state.lock() else {
            return u64::MAX;
        };
        state.generation = state.generation.wrapping_add(1);
        for entry in &mut state.entries {
            if entry.phase.active() {
                entry.phase = ForwardPhase::Stopping;
                entry.message =
                    "Cancellation requested; waiting for listener and client cleanup.".into();
                entry.cancel.take();
            }
        }
        state.generation
    }
    fn cancel_selected(&self, selected: usize) -> bool {
        let Ok(mut state) = self.state.lock() else {
            return false;
        };
        let Some(entry) = state.entries.get_mut(selected) else {
            return false;
        };
        if matches!(
            entry.phase,
            ForwardPhase::Ended | ForwardPhase::Failed | ForwardPhase::Stopping
        ) {
            return false;
        }
        entry.phase = ForwardPhase::Stopping;
        entry.message = "Cancellation requested; waiting for listener and client cleanup.".into();
        entry.cancel.take();
        true
    }
    fn listening(&self, id: u64, text: &str) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "Forward status unavailable")?;
        let entry = state
            .entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or("Forward was canceled before bind")?;
        if entry.phase != ForwardPhase::Starting {
            return Err("Forward was canceled before bind".into());
        }
        entry.phase = ForwardPhase::Listening;
        entry.message = text.into();
        Ok(())
    }
    fn finish(&self, id: u64, result: Result<String>) {
        if let Ok(mut state) = self.state.lock()
            && let Some(entry) = state.entries.iter_mut().find(|entry| entry.id == id)
        {
            entry.phase = if result.is_ok() {
                ForwardPhase::Ended
            } else {
                ForwardPhase::Failed
            };
            entry.cancel.take();
            entry.message = match result {
                Ok(message) => message,
                Err(error) => format!("Forward stopped: {error}"),
            };
        }
    }
    fn suggested_port(&self) -> u16 {
        self.state
            .lock()
            .ok()
            .and_then(|state| {
                (8080..=8088).find(|port| {
                    !state
                        .entries
                        .iter()
                        .any(|entry| entry.phase.active() && entry.target.local_port == *port)
                })
            })
            .unwrap_or(8080)
    }
    fn active_count(&self) -> usize {
        self.state
            .lock()
            .map(|s| {
                s.entries
                    .iter()
                    .filter(|entry| entry.phase.active())
                    .count()
            })
            .unwrap_or(0)
    }
    fn len(&self) -> usize {
        self.state.lock().map(|s| s.entries.len()).unwrap_or(0)
    }
    fn index(&self, id: u64) -> usize {
        self.state
            .lock()
            .ok()
            .and_then(|s| s.entries.iter().position(|entry| entry.id == id))
            .unwrap_or(0)
    }
    fn text(&self) -> String {
        self.lines(usize::MAX, 200, 64).join("\n")
    }
    fn lines(&self, selected: usize, width: usize, height: usize) -> Vec<String> {
        let Ok(state) = self.state.lock() else {
            return vec!["Forward status unavailable.".into()];
        };
        let count = state
            .entries
            .iter()
            .filter(|entry| entry.phase.active())
            .count();
        let mut lines = vec![
            format!("{count}/{MAX_FORWARDS} active   /   j k select   s stop   F new   Esc close"),
            String::new(),
        ];
        if state.entries.is_empty() {
            lines.push("No forwards. Select a Pod and press F to review a new session.".into());
        } else {
            lines.push("  STATE      LOCAL             POD / REMOTE".into());
            let rows = height.saturating_sub(7).max(1);
            let start = selected
                .min(state.entries.len().saturating_sub(1))
                .saturating_sub(rows.saturating_sub(1));
            for (index, entry) in state.entries.iter().enumerate().skip(start).take(rows) {
                lines.push(format!(
                    "{} {:<10} 127.0.0.1:{:<5}  {}/{}:{}",
                    if index == selected { ">" } else { " " },
                    entry.phase.label(),
                    entry.target.local_port,
                    entry.target.namespace,
                    entry.target.pod,
                    entry.target.remote_port
                ));
            }
            if let Some(entry) = state.entries.get(selected) {
                lines.push(String::new());
                lines.extend(
                    wrap_text(
                        &format!(
                            "Session {} / {}s maximum / {}",
                            entry.id, entry.target.seconds, entry.message
                        ),
                        width,
                    )
                    .lines()
                    .map(str::to_owned),
                );
            }
        }
        lines.push(String::new());
        lines.push(
            "Loopback only / 4 clients per session / scope changes and exit stop all.".into(),
        );
        lines
    }
}
impl Drop for Worker {
    fn drop(&mut self) {
        self.logs.cancel();
        self.forward.cancel();
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
    let forward = std::sync::Arc::new(ForwardSlot::default());
    forward.cancel();
    let forward_results = forward.clone();
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
                    Task::Watch(k, n, selector,fields) => {
                        match gateway.as_ref().ok_or("not connected; select a context or retry".to_string()).and_then(|g| g.watch_fields(&k, &n, &selector,&fields)) {
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
                    Task::YamlPrepare(editor)=>{
                        let result=if let Some(g)=gateway.as_ref() {match tokio::time::timeout(Duration::from_secs(35),g.prepare_yaml(&editor.draft,&editor.text())).await {Ok(result)=>result,Err(_)=>Err("YAML preparation timed out; draft retained.".into())}} else {Err("Not connected.".into())};
                        Reply::YamlPrepared(editor,result)
                    }
                    Task::YamlExecute(prepared,typed)=>Reply::WriteOutcome(if let Some(g)=gateway.as_mut(){g.execute_yaml(*prepared,&typed).await}else{Err("Not connected; no YAML write started.".into())}),
                    Task::NodeExecute(prepared,typed)=>Reply::WriteOutcome(if let Some(g)=gateway.as_ref(){crate::node_maintenance::execute(g,*prepared,&typed).await.map(|report|report.summary())}else{Err("Not connected; no node operation started.".into())}),
                    Task::RunSession(reviewed, typed) => {
                        if let Some(g) = gateway.as_ref() {
                            match crate::ops::run(g, *reviewed, &typed).await {
                                Ok(message) => Reply::Notice(message),
                                Err(error) => Reply::Failure(error),
                            }
                        } else { Reply::Failure("Session unavailable; reconnect before review.".into()) }
                    }
                    Task::RunForward(reviewed,typed,generation)=>{
                        if let Some(gateway)=gateway.as_ref() {
                            let gateway=gateway.clone();let slot=forward_results.clone();
                            match reviewed.forward_target().ok_or_else(||"Review does not authorize port forwarding".to_string()).and_then(|target|slot.begin(generation,epoch,&watch_results,target)) {
                                Ok((id,cancel))=>{
                                    tokio::spawn(async move {
                                        let bound=slot.clone();
                                        let result=crate::ops::run_forward(&gateway,*reviewed,&typed,cancel,move|message|bound.listening(id,message)).await;
                                        slot.finish(id,result);
                                    });
                                    Reply::ForwardQueued(id)
                                }
                                Err(error)=>Reply::Failure(error),
                            }
                        }else{Reply::Failure("Not connected; review again after reconnecting.".into())}
                    }
                    Task::AdvicePrepare(kind,namespace,name,uid,policy)=>{
                        let result=async {
                            if epoch!=watch_results.epoch.load(std::sync::atomic::Ordering::SeqCst){return Err("Copilot preview canceled: context/scope changed.".into());}
                            let gateway=gateway.as_ref().ok_or("Not connected; select a resource first.")?;
                            let object=tokio::time::timeout(Duration::from_secs(15),gateway.get(&kind,&namespace,&name)).await.map_err(|_|"Diagnostic read timed out")??;
                            if uid.as_deref().is_none_or(|expected|object["metadata"]["uid"].as_str()!=Some(expected)){return Err("Selected object was replaced; select and preview again.".into());}
                            if let Some(policy)=policy.filter(|_|!gateway.is_demo()) {
                                let options=crate::advice::load_options(&policy)?;
                                let prepared=crate::advice::prepare(&object,options).await?;
                                let text=format!("01 REVIEW TRANSFER    /    02 Ask Copilot    /    03 Read advice\n\n{}\n\nEXACT DIAGNOSTIC PAYLOAD\n{}",prepared.summary(),prepared.preview()?);
                                Ok((Some(prepared),text))
                            } else {
                                let preview=crate::advice::prepare_preview(&object,"github.com")?;
                                Ok((None,format!("01 LOCAL PREVIEW    /    No request sent\n\nCOPILOT ADVISORY\nReview diagnostic status before sharing it with GitHub Copilot.\nNames, namespaces, cluster identity, specs, logs and credentials are omitted.\nOnly known status terms are shared. Counters can still reveal workload state.\n\n{}\n\nEXACT DIAGNOSTIC PAYLOAD\n{}",if gateway.is_demo(){"DEMO / sample data. Sending is disabled in demo mode."}else{"Setup required: launch with --copilot-policy ABS. See docs/COPILOT_INTEGRATION.md."},preview.preview()?)))
                            }
                        }.await;
                        Reply::AdvicePrepared(result)
                    }
                    Task::AdviceExecute(prepared,typed,cancel)=>{
                        let result=if epoch!=watch_results.epoch.load(std::sync::atomic::Ordering::SeqCst){Err("Copilot transfer rejected: reviewed context/scope changed.".into())}else{crate::advice::run(*prepared,&typed,Some(cancel)).await};
                        Reply::AdviceOutcome(result)
                    }
                    Task::HelmPrepare(draft,generation)=>{
                        let result=if epoch!=watch_results.epoch.load(std::sync::atomic::Ordering::SeqCst){Err("Helm preparation canceled: context/scope changed.".into())}
                        else if let Some(gateway)=gateway.as_ref(){
                            match crate::helm_lifecycle::prepare(gateway,draft.options.clone(),draft.request.clone()).await{
                                Ok(prepared)=>helm_ui::validate_prepared(&prepared,&draft).map(|()|Box::new(prepared)),Err(error)=>Err(error)
                            }
                        }else{Err("Not connected; reconnect before preparing Helm.".into())};
                        Reply::HelmPrepared(generation,result)
                    }
                    Task::HelmExecute(prepared,typed,generation,cancel)=>{
                        let result=if epoch!=watch_results.epoch.load(std::sync::atomic::Ordering::SeqCst){Err("Helm execution rejected: reviewed context/scope is stale.".into())}
                        else if let Some(gateway)=gateway.as_ref(){crate::helm_lifecycle::run(gateway,*prepared,&typed,Some(cancel)).await}
                        else{Err("Helm execution unavailable: reconnect and review again.".into())};
                        Reply::HelmOutcome(generation,result)
                    }
                    Task::Helm(request,generation)=>{
                        let result=if let Some(gateway)=gateway.as_ref(){match tokio::time::timeout(Duration::from_secs(23),helm_ui::read(gateway,request)).await{Ok(result)=>result,Err(_)=>Err("Helm read deadline exceeded; metadata may be incomplete. Retry explicitly.".into())}}else{Err("Not connected; select a context before reading Helm releases.".into())};
                        Reply::Helm(generation,result)
                    }
                    Task::DashboardWorkloads(contexts)=>{
                        let lists=futures_util::future::join_all(contexts.into_iter().map(|(context,connection)| async move {
                            let result=crate::workload_tail::list_workloads(*connection,context.clone()).await;
                            (context,result)
                        })).await;
                        Reply::DashboardWorkloads(lists)
                    }
                    Task::DashboardTail(sources,generation,seconds,read)=>{
                        let opened=crate::workload_tail::open_all(sources,crate::logs::FollowOptions{seconds},read).await;
                        let mailbox=log_results.clone();
                        let job=tokio::spawn(async move {
                            crate::workload_tail::stream(opened,|line| mailbox.append(generation,line)).await;
                        });
                        if let Ok(mut handle)=log_results.abort.lock() {
                            if log_results.generation.load(std::sync::atomic::Ordering::SeqCst)==generation {*handle=Some(job.abort_handle());} else {job.abort();}
                        } else {job.abort();}
                        Reply::Following
                    }
                    Task::DashboardLogs(connection,namespace,pod,uid,generation,seconds,read)=>{
                        let opened=async {
                            let pane=tokio::time::timeout(Duration::from_secs(20),Gateway::connect(*connection)).await.map_err(|_|"connection timed out".to_owned())??;
                            tokio::time::timeout(Duration::from_secs(25),pane.follow_merged_logs(&namespace,&pod,&uid,crate::logs::FollowOptions{seconds},read)).await.map_err(|_|"log open timed out".to_owned())?
                        }.await;
                        match opened {
                            Ok(mut feed)=>{
                                let mailbox=log_results.clone();
                                let job=tokio::spawn(async move {
                                    let mut count=0usize;
                                    loop {
                                        match feed.next().await {
                                            Ok(Some(line))=>{if !mailbox.append(generation,&line){break;} count+=1; if count.is_multiple_of(64){tokio::task::yield_now().await;}},
                                            Ok(None)=>{mailbox.append(generation,"[Log stream ended]");break;},
                                            Err(error)=>{mailbox.append(generation,&format!("[Stopped: {error}]"));break;}
                                        }
                                    }
                                });
                                if let Ok(mut handle)=log_results.abort.lock() {
                                    if log_results.generation.load(std::sync::atomic::Ordering::SeqCst)==generation {*handle=Some(job.abort_handle());} else {job.abort();}
                                } else {job.abort();}
                                Reply::Following
                            }
                            Err(error)=>Reply::LogFailure(generation,error),
                        }
                    }
                    Task::Directory(path)=>match crate::navigation_views::directory(&path){Ok(directory)=>Reply::Directory(directory),Err(error)=>Reply::Failure(error)},
                    Task::Manifest(path)=>match crate::navigation_views::preview_manifest(&path){Ok(text)=>Reply::Detail(text),Err(error)=>Reply::Failure(error)},
                    task => {
                        if let Some(g) = gateway.as_mut() {
                            let log_generation=match &task{Task::CaptureLogs(_,_,generation)|Task::Containers(_,generation)|Task::Logs(_,generation,_)|Task::Follow(_,generation,_,_)=>Some(*generation),_=>None};
                            let future = async {
                                Ok(match task {
                                    Task::List(k,n,selector,fields) => Reply::Items(g.list_fields(&k,&n,&selector,&fields).await?),
                                    Task::Namespaces => {
                                        let value = g.list("namespaces","*").await?;
                                        let mut names = vec!["all".to_owned()];
                                        names.extend(value["items"].as_array().into_iter().flatten().filter_map(|v|v["metadata"]["name"].as_str().map(str::to_owned)));
                                        Reply::Namespaces(names)
                                    },
                                    Task::Pulses(n)=>Reply::Detail(crate::insights::pulses(g,&n).await?),
                                    Task::Secrets(n)=>Reply::Secrets(g.secret_index(&n).await?),
                                    Task::SecretValue(n,name,uid,key)=>{let value=g.secret_value(&n,&name,&uid,&key).await;Reply::SecretValue(uid,key,value)},
                                    Task::Xray(k,n)=>Reply::Detail(crate::insights::xray(g,&k,&n).await?),
                                    Task::Workloads(n)=>Reply::Detail(crate::insights::workloads(g,&n).await?),
                                    Task::Can(n,subject)=>Reply::Detail(crate::navigation_views::can(g,&n,&subject).await?),
                                    Task::Subjects(n,kind,title)=>Reply::Subjects(title,n.clone(),crate::navigation_views::subjects(g,&n,kind).await?),
                                    Task::Get(k,n,name) => Reply::Detail(serde_json::to_string_pretty(&g.get(&k,&n,&name).await?).map_err(|_| "serialization failed")?),
                                    Task::Inspect(k,n,name) => Reply::Object(Box::new(g.get(&k,&n,&name).await?)),
                                    Task::Drill(k,n,name)=>Reply::Drill(Box::new(g.get(&k,&n,&name).await?)),
                                    Task::Yaml(k,n,name) => Reply::Detail(crate::inspect::yaml(&g.get(&k,&n,&name).await?)?),
                                    Task::ExportResource(k,n,name) => Reply::Export(crate::exports::resource(&g.get(&k,&n,&name).await?)?),
                                    Task::ExportHelm(release) => Reply::Export(crate::exports::helm(&crate::helm::detail(g,&release).await?)?),
                                    Task::YamlDraft(kind,ns,name,uid)=>{
                                        let draft=g.yaml_draft(&kind,&ns,&name).await?;
                                        if uid.as_deref().is_none_or(|uid|Some(uid)!=draft.original["metadata"]["uid"].as_str()) {return Err("Selected resource was replaced; reopen YAML editor.".into());}
                                        Reply::YamlDraft(Box::new(draft))
                                    },
                                    Task::NodePrepare(name,uid,request)=>{
                                        let prepared=crate::node_maintenance::prepare(g,&name,request).await?;
                                        if uid.as_deref().is_none_or(|uid|uid!=prepared.uid()) {return Err("Selected node was replaced; select it again.".into());}
                                        Reply::NodePrepared(Box::new(prepared))
                                    },
                                    Task::Overview(options,names) => Reply::Overview(crate::overview::OverviewFeed::new(&options,&names)?),
                                    Task::PrepareSession(n,name,uid,request) => {
                                        let prepared = crate::ops::prepare(g,&n,&name,request).await?;
                                        if uid.as_deref().is_none_or(|uid| uid != prepared.uid()) { return Err("Selected pod was replaced; select and review again.".into()); }
                                        Reply::SessionPrepared(Box::new(prepared))
                                    },
                                    Task::CaptureLogs(mut target,selection,generation)=>{
                                        let membership=g.capture_log_membership(&target.namespace,selection.clone()).await?;
                                        target.membership=Some(if target.dynamic{membership.into_dynamic(selection)}else{membership});
                                        Reply::Membership(target,generation)
                                    },
                                    Task::Containers(target,generation) => {
                                        let value=g.get("pods",&target.namespace,&target.pod).await?;
                                        if value["metadata"]["uid"].as_str()!=Some(&target.uid) {return Err("Selected pod was replaced; select it again.".into());}
                                        Reply::Containers(target,generation,std::iter::once("*".to_owned()).chain(pod_container_names(&value)).collect())
                                    },
                                    Task::Logs(target,generation,read) => {
                                        let text=if let Some(membership)=&target.membership{g.membership_logs(membership,target.previous,read).await?}
                                        else if target.all {g.merged_logs(&target.namespace,&target.pod,&target.uid,target.previous,read).await?}
                                        else {
                                            let before=g.get("pods",&target.namespace,&target.pod).await?;
                                            if before["metadata"]["uid"].as_str()!=Some(&target.uid) {return Err("Selected pod was replaced; select it again.".into());}
                                            let text=g.logs_with_options(&target.namespace,&target.pod,target.container.clone(),target.previous,read).await?;
                                            let after=g.get("pods",&target.namespace,&target.pod).await?;
                                            if after["metadata"]["uid"].as_str()!=Some(&target.uid) {return Err("Pod changed while reading logs; output discarded.".into());}
                                            text
                                        };
                                        Reply::Logs(generation,text)
                                    },
                                    Task::Follow(target,generation,seconds,read) => {
                                        if log_results.generation.load(std::sync::atomic::Ordering::SeqCst) != generation {return Ok(Reply::Following);}
                                        let mut feed=if let Some(membership)=target.membership.as_ref().filter(|m|m.is_dynamic()){ActiveLogFeed::Dynamic(Box::new(g.follow_dynamic_logs(membership,crate::logs::FollowOptions{seconds},read).await?))}else if let Some(membership)=&target.membership{ActiveLogFeed::Merged(g.follow_membership_logs(membership,crate::logs::FollowOptions{seconds},read).await?)}else if target.all {ActiveLogFeed::Merged(g.follow_merged_logs(&target.namespace,&target.pod,&target.uid,crate::logs::FollowOptions{seconds},read).await?)}else{ActiveLogFeed::Single(Box::new(g.follow_logs_resumable(&target.namespace,&target.pod,target.container,&target.uid,crate::logs::FollowOptions{seconds},read).await?))};
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
                                            if log_results.generation.load(std::sync::atomic::Ordering::SeqCst) == generation { *handle = Some(job.abort_handle()); } else {job.abort();}
                                        } else {job.abort();}
                                        Reply::Following
                                    },
                                    Task::Discover => match tokio::time::timeout(Duration::from_secs(20),g.discover()).await{Ok(Ok(kinds))=>Reply::Catalog(kinds),Ok(Err(error))=>Reply::CatalogFailure(error),Err(_)=>Reply::CatalogFailure("Discovery deadline exceeded; built-in resources remain available.".into())},
                                    Task::Prepare(k,n,name,uid,a) => {
                                        let prepared = g.prepare(&k,&n,&name,a).await?;
                                        if uid.as_deref().is_some_and(|u| u != prepared.target.uid) { return Err("selected object was replaced; reselect it before approval".into()); }
                                        Reply::Prepared(Box::new(prepared))
                                    },
                                    Task::Execute(p,a) => Reply::Notice(g.execute(*p,a).await?),
                                    Task::YamlPrepare(..) | Task::YamlExecute(..) | Task::NodeExecute(..) | Task::DashboardLogs(..) | Task::DashboardWorkloads(..) | Task::DashboardTail(..) | Task::Connect(_) | Task::Watch(..) | Task::Metrics(..) | Task::RunSession(..) | Task::RunForward(..) | Task::Directory(_) | Task::Manifest(_) | Task::Helm(..) | Task::HelmPrepare(..) | Task::HelmExecute(..) | Task::AdvicePrepare(..) | Task::AdviceExecute(..) => return Err("invalid worker task".into()),
                                })
                            };
                            match tokio::time::timeout(Duration::from_secs(25), future).await {
                                Ok(Ok(reply)) => reply,
                                Ok(Err(e)) => if let Some(generation)=log_generation {Reply::LogFailure(generation,e)}else{Reply::Failure(e)},
                                Err(_) => if let Some(generation)=log_generation{Reply::LogFailure(generation,"Log request timed out; choose a range to retry.".into())}else{Reply::Failure("request timed out; data STALE, mutation outcome potentially UNKNOWN".into())},
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
        forward,
    }
}

const OVERVIEW_CONTEXTS_LABEL: &str = "Overview contexts (comma separated, max 9)";
const OVERVIEW_INPUT_BYTES: usize = crate::overview::MAX_LIVE_CONTEXTS * 254 - 1;
fn input_limit(label: &str) -> usize {
    if label == OVERVIEW_CONTEXTS_LABEL {
        OVERVIEW_INPUT_BYTES
    } else if label == "Command" {
        nav::COMMAND_BYTES
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
    LiveOverview(dashboard_ui::Dashboard),
    Metrics(String, usize),
    Follow(Box<crate::ui_logs::LogView>),
    Containers(LogTarget, Vec<String>, usize),
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
    SessionContainers(String, String, Option<String>, Vec<String>, usize, bool),
    SessionConfirm(Box<crate::ops::ReviewedOperation>, String),
    Scale(String, String, String, Option<String>, String),
    Resources(String, String, String, Option<String>, String),
    Help(usize),
    EditContainers(
        String,
        String,
        String,
        Option<String>,
        Vec<crate::edit::ContainerValues>,
        usize,
    ),
    Editor(Box<ContainerDraft>),
    YamlEditor(Box<crate::ui_yaml::Editor>),
    WriteReview(Box<WriteReview>),
    WriteRunning,
    Export(crate::exports::Draft),
    PodContainers(Box<Value>, usize),
    Directory(crate::navigation_views::Directory, usize),
    Secrets(Box<crate::ui_secrets::View>),
    Subjects(Box<crate::ui_subjects::View>),
    ForwardForm(String, String, Option<String>, [String; 3], usize),
    ForwardView,
    Helm(Box<helm_ui::Table>),
    HelmLoading(&'static str),
    HelmDocument(String, String, usize),
    HelmForm(Box<helm_ui::Form>),
    AdviceReview(Box<advice_ui::Review>),
    AdviceLoading,
    AdviceRunning,
    AdviceDocument(String, String, usize),
    HelmReview(Box<helm_ui::Review>),
    HelmRunning(String, Instant, bool),
    HelmOutcome(String, String, usize),
}
struct ContainerDraft {
    kind: String,
    namespace: String,
    name: String,
    uid: Option<String>,
    container: String,
    fields: [String; 5],
    active: usize,
    resources: bool,
}
enum WritePlan {
    Yaml(Box<crate::manifest_edit::PreparedYaml>),
    Node(Box<crate::node_maintenance::ReviewedNode>),
}
struct WriteReview {
    plan: WritePlan,
    view: review_ui::Review,
}
#[derive(Clone)]
struct LogTarget {
    namespace: String,
    pod: String,
    container: Option<String>,
    uid: String,
    previous: bool,
    follow: bool,
    all: bool,
    /// Explicit `:logs-live` opt-in: announced Pod joins and departures.
    dynamic: bool,
    membership: Option<crate::logs::LogMembership>,
}
enum ActiveLogFeed {
    Single(Box<crate::logs::LogFeed>),
    Merged(crate::logs::MergedLogFeed),
    Dynamic(Box<crate::logs::DynamicLogFeed>),
}
impl ActiveLogFeed {
    async fn next(&mut self) -> Result<Option<String>> {
        match self {
            Self::Single(feed) => feed.next().await,
            Self::Merged(feed) => feed.next().await,
            Self::Dynamic(feed) => feed.next().await,
        }
    }
}
#[derive(Clone)]
struct ViewState {
    resource: String,
    namespace: String,
    context: String,
    filter: String,
    selector: String,
    field_selector: String,
    selected_uid: Option<String>,
}
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
enum Sort {
    #[default]
    Name,
    Namespace,
    Status,
    Restarts,
    Age,
    Ready,
    Cpu,
    Memory,
}
impl Sort {
    fn label(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::Namespace => "namespace",
            Self::Status => "status",
            Self::Restarts => "restarts",
            Self::Age => "age",
            Self::Ready => "ready",
            Self::Cpu => "cpu",
            Self::Memory => "memory",
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
    selector: String,
    field_selector: String,
    sort: Sort,
    history: Vec<String>,
    history_index: Option<usize>,
    history_draft: String,
    completion: Option<(String, Vec<String>, usize)>,
    command_cursor: usize,
    back: Vec<ViewState>,
    forward: Vec<ViewState>,
    marks: std::collections::BTreeSet<String>,
    saved: Vec<crate::screendump::Record>,
    aliases: crate::aliases::Aliases,
    known_namespaces: Vec<String>,
    restoring_uid: Option<String>,
    subviews: Vec<Modal>,
    log_target: Option<LogTarget>,
    helm_generation: u64,
    helm_cancel: Option<tokio::sync::oneshot::Sender<()>>,
    advice_cancel: Option<tokio::sync::oneshot::Sender<()>>,
    helm_return: Option<(String, crate::helm::Storage)>,
    helm_elapsed: u64,
    last_forward: String,
    forward_selected: usize,
    desired_resource: Option<String>,
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
    last_table_metrics: Option<Instant>,
    table_metrics: table_metrics::TableMetrics,
    metrics_resources: bool,
    overview: Option<crate::overview::OverviewFeed>,
    selection_ui: selections_ui::State,
    pending_session: Option<(Box<crate::ops::ReviewedOperation>, String)>,
    /// Ctrl-E in the YAML editor: hand the draft to $EDITOR on the next loop.
    pending_editor: bool,
    worker: Worker,
}
impl App {
    fn copy_text(&self, text: &str) -> Result<()> {
        use base64::Engine;
        let encoded =
            base64::engine::general_purpose::STANDARD.encode(safety::safe_text(text, 253));
        let mut stdout = io::stdout().lock();
        write!(stdout, "\x1b]52;c;{encoded}\x07")
            .and_then(|_| stdout.flush())
            .map_err(|_| "clipboard request failed".into())
    }
    fn mouse(&mut self, mouse: MouseEvent, width: u16, height: u16) {
        if matches!(self.modal, Modal::None | Modal::LiveOverview(..))
            && !self.write_inflight
            && mouse.row == 0
            && matches!(
                mouse.kind,
                MouseEventKind::Down(crossterm::event::MouseButton::Left)
            )
            && topbar::help_at(width, mouse.column)
        {
            if let Err(error) = self.key(KeyCode::Char('?')) {
                self.status = error;
            }
            return;
        }
        if !matches!(self.modal, Modal::None) {
            return;
        }
        let layout = Layout::new(width, height);
        match mouse.kind {
            MouseEventKind::ScrollDown => {
                self.selected = (self.selected + 1).min(self.visible().len().saturating_sub(1));
            }
            MouseEventKind::ScrollUp => self.selected = self.selected.saturating_sub(1),
            MouseEventKind::Down(_)
                if mouse.row >= layout.table.y + 2
                    && mouse.row < layout.table.y + layout.table.height.saturating_sub(1) =>
            {
                let start = self
                    .selected
                    .saturating_sub(layout.table_rows().saturating_sub(1));
                self.selected = (start + usize::from(mouse.row - layout.table.y - 2))
                    .min(self.visible().len().saturating_sub(1));
            }
            _ => {}
        }
    }
    fn kind(&self) -> String {
        if let Some(resource) = &self.desired_resource {
            return resource.clone();
        }
        self.kinds
            .get(self.kind_index)
            .map(Kind::key)
            .unwrap_or_else(|| "pods".into())
    }
    fn visible(&self) -> Vec<&Value> {
        let filter = nav::Filter::parse(&self.filter);
        let mut items: Vec<_> = self
            .items
            .iter()
            .filter(|object| {
                let text = format!("{} {} {}", namespace(object), name(object), summary(object));
                filter.as_ref().is_ok_and(|filter| filter.matches(&text))
            })
            .collect();
        items.sort_by(|a, b| {
            let ordering = match self.sort {
                Sort::Name => name(a).cmp(name(b)),
                Sort::Namespace => namespace(a).cmp(namespace(b)),
                Sort::Status => summary(a).cmp(&summary(b)),
                Sort::Restarts => restarts(b).cmp(&restarts(a)),
                Sort::Ready => ready_sort_key(a).cmp(&ready_sort_key(b)),
                Sort::Cpu => self
                    .table_metrics
                    .sort_usage(b)
                    .map(|usage| usage.cpu_nanocores)
                    .cmp(
                        &self
                            .table_metrics
                            .sort_usage(a)
                            .map(|usage| usage.cpu_nanocores),
                    ),
                Sort::Memory => self
                    .table_metrics
                    .sort_usage(b)
                    .map(|usage| usage.memory_bytes)
                    .cmp(
                        &self
                            .table_metrics
                            .sort_usage(a)
                            .map(|usage| usage.memory_bytes),
                    ),
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
            if matches!(sort, Sort::Restarts | Sort::Cpu | Sort::Memory) {
                " (highest first)"
            } else {
                ""
            }
        );
    }
    fn view_state(&self) -> ViewState {
        ViewState {
            resource: self.kind(),
            namespace: self.namespace.clone(),
            context: self.context.clone(),
            filter: self.filter.clone(),
            selector: self.selector.clone(),
            field_selector: self.field_selector.clone(),
            selected_uid: self.selected_uid(),
        }
    }
    fn push_view(&mut self) {
        let view = self.view_state();
        self.forward.clear();
        if self.back.last().is_none_or(|last| {
            last.resource != view.resource
                || last.namespace != view.namespace
                || last.context != view.context
                || last.filter != view.filter
                || last.selector != view.selector
                || last.field_selector != view.field_selector
        }) {
            self.back.push(view);
            if self.back.len() > 32 {
                self.back.remove(0);
            }
        }
    }
    fn restore_view(&mut self, view: &ViewState) -> Result<()> {
        if self.busy || self.connecting_epoch.is_some() {
            return Err("Wait for the current request before restoring a view.".into());
        }
        let cross_context = !self.connected
            || self.context != view.context
            || self
                .options
                .connection
                .context
                .as_ref()
                .is_some_and(|context| context != &view.context);
        let index = if cross_context {
            None
        } else {
            Some(self.resolve_resource(&view.resource)?)
        };
        self.namespace = view.namespace.clone();
        self.filter = view.filter.clone();
        self.selector = view.selector.clone();
        self.field_selector = view.field_selector.clone();
        if cross_context {
            self.desired_resource = Some(view.resource.clone());
            self.options.connection.context = Some(view.context.clone());
            self.connect();
            self.namespace_override = true;
        } else {
            if let Some(index) = index {
                self.kind_index = index;
            }
            self.desired_resource = None;
            self.changed();
        }
        self.restoring_uid = view.selected_uid.clone();
        Ok(())
    }
    fn go_back(&mut self) {
        if let Some(view) = self.back.last().cloned() {
            let current = self.view_state();
            match self.restore_view(&view) {
                Ok(()) => {
                    self.back.pop();
                    self.forward.push(current);
                    self.status = "Returned to previous view.".into();
                }
                Err(error) => self.status = error,
            }
        } else if !self.filter.is_empty() || !self.selector.is_empty() {
            self.filter.clear();
            self.selector.clear();
            self.changed();
        } else {
            self.status = "At the first view. :resource navigates; :q quits.".into();
        }
    }
    fn go_forward(&mut self) {
        if let Some(view) = self.forward.last().cloned() {
            let current = self.view_state();
            match self.restore_view(&view) {
                Ok(()) => {
                    self.forward.pop();
                    self.back.push(current);
                }
                Err(error) => self.status = error,
            }
        } else {
            self.status = "No next view. [ goes back; ] goes forward.".into();
        }
    }
    fn last_view(&mut self) {
        if let Some(view) = self.back.last().cloned() {
            let current = self.view_state();
            match self.restore_view(&view) {
                Ok(()) => {
                    self.back.pop();
                    self.back.push(current);
                }
                Err(error) => self.status = error,
            }
        } else {
            self.status = "No previous view.".into();
        }
    }
    fn resolve_resource(&self, resource: &str) -> Result<usize> {
        let resource = resource.to_ascii_lowercase();
        let parts = resource.split('/').collect::<Vec<_>>();
        let (resource, version, group) = match parts.as_slice() {
            [version, plural] => ((*plural).to_owned(), Some(*version), Some("")),
            [group, version, plural] => (format!("{plural}.{group}"), Some(*version), Some(*group)),
            [_] => (resource.clone(), None, None),
            _ => return Err("Use RESOURCE, VERSION/RESOURCE or GROUP/VERSION/RESOURCE.".into()),
        };
        if let Some((index, _)) = self.kinds.iter().enumerate().find(|(_, kind)| {
            kind.key() == resource
                && version.is_none_or(|version| kind.version == version)
                && group.is_none_or(|group| kind.group == group)
        }) {
            return Ok(index);
        }
        let matches: Vec<_> = self
            .kinds
            .iter()
            .enumerate()
            .filter(|(_, kind)| {
                version.is_none_or(|version| kind.version == version)
                    && group.is_none_or(|group| kind.group == group)
                    && (kind.key() == resource
                        || kind.plural == resource
                        || kind.kind.eq_ignore_ascii_case(&resource)
                        || kind.short_names.contains(&resource))
            })
            .map(|(index, _)| index)
            .collect();
        match matches.as_slice() {
            [index] => Ok(*index),
            [] => Err(format!(
                "Unknown alias '{resource}'. Ctrl-A lists resources; :discover refreshes discovery."
            )),
            _ => Err("Ambiguous alias; use the full resource.group name.".into()),
        }
    }
    fn namespace_picker(&mut self) {
        self.modal = Modal::Picker("Namespace", vec![], 0);
        self.status = "Loading namespaces...".into();
        self.send(Task::Namespaces);
    }
    fn aliases(&mut self) {
        let mut lines = vec![];
        let mut kinds = self.kinds.iter().collect::<Vec<_>>();
        kinds.sort_by_key(|kind| kind.key());
        for kind in kinds {
            let mut aliases = kind.short_names.clone();
            aliases.sort();
            aliases.dedup();
            lines.push(format!(
                "{:<34} {} {}",
                kind.key(),
                kind.kind.to_ascii_lowercase(),
                aliases.join(", ")
            ));
        }
        lines.extend(
            self.aliases
                .iter()
                .map(|(alias, target)| format!("{alias:<34} {target}")),
        );
        self.modal = Modal::Picker("Aliases", lines, 0);
    }
    fn command(&mut self, text: &str) -> Result<bool> {
        let expanded = match crate::aliases::expand(text, &self.aliases) {
            Ok(text) => text,
            Err(error) => {
                self.status = error;
                return Ok(false);
            }
        };
        let words = match nav::words(&expanded) {
            Ok(words) => words,
            Err(error) => {
                self.status = error;
                return Ok(false);
            }
        };
        let Some(first) = words.first() else {
            return Ok(false);
        };
        let cmd = first.to_ascii_lowercase();
        let args = &words[1..];
        match (cmd.as_str(), args) {
            ("q" | "q!" | "qa" | "quit" | "exit", []) => return Ok(true),
            ("?" | "h" | "help", []) => self.modal = Modal::Help(0),
            ("a" | "alias" | "aliases", []) => self.aliases(),
            ("actions", []) => self.modal = Modal::Picker("Actions", self.action_menu(), 0),
            ("export", []) => self.request_export(),
            ("save", []) => self.save_resource_selection()?,
            ("selections", []) => self.open_saved_selections()?,
            ("edit"|"yaml-edit",[])=>self.yaml_editor(),
            ("edit-container",[])=>self.editor(),
            ("cordon",[])=>self.node_maintenance(crate::node_maintenance::Request::Cordon),
            ("uncordon",[])=>self.node_maintenance(crate::node_maintenance::Request::Uncordon),
            ("drain",[])=>self.node_maintenance(crate::node_maintenance::Request::Drain{seconds:120}),
            ("ctx" | "context" | "contexts", []) => {
                let list=contexts(&self.options.connection)?;
                let index=list.iter().position(|name|name==&self.context).unwrap_or(0);
                self.modal=Modal::Picker("Context (Pod view)",list,index);
            }
            ("ctx" | "context" | "contexts", [context]) => {
                let available = contexts(&self.options.connection)?;
                if available.contains(context) && !self.busy {
                    self.push_view();
                    self.options.connection.context = Some(context.clone());
                    self.connect();
                } else { self.status = "Unknown context or request in progress; use :ctx to choose.".into(); }
            }
            ("ns" | "namespace" | "namespaces", []) => self.namespace_picker(),
            ("ns" | "namespace" | "namespaces", [ns]) => {
                let ns = if ns == "all" { "*" } else { ns.as_str() };
                if ns == "*" || safety::valid_id(ns) {
                    self.push_view(); self.namespace = ns.into(); self.namespace_override = true;self.desired_resource=None;
                    if let Ok(index)=self.resolve_resource("v1/pods"){self.kind_index=index;}
                    self.filter.clear(); self.selector.clear();self.field_selector.clear(); self.changed();
                } else { self.status = "Invalid namespace.".into(); }
            }
            ("all", []) => return self.command("ns all"),
            ("sort", [key]) => {
                let sort = match key.as_str() { "name" => Some(Sort::Name), "namespace" | "ns" => Some(Sort::Namespace), "status" => Some(Sort::Status), "restarts" => Some(Sort::Restarts), "age" => Some(Sort::Age), "ready"=>Some(Sort::Ready), "cpu"=>Some(Sort::Cpu),"memory"|"mem"=>Some(Sort::Memory), _ => None };
                if let Some(sort) = sort { self.set_sort(sort); } else { self.status = "Sort: name | namespace | status | restarts | age | ready | cpu | memory".into(); }
            }
            ("themes", []) => self.theme_picker()?,
            ("overview", []) => self.modal = Modal::Input(OVERVIEW_CONTEXTS_LABEL, self.context.clone()),
            ("metrics", []) => { self.metrics_resources = false; self.request_metrics(); }
            ("metrics",[mode]) if mode=="resources"=>{self.metrics_resources=true;self.request_metrics();}
            ("discover",[])=>self.send(Task::Discover),
            ("pf"|"portforward"|"portforwards",[])=>self.modal=Modal::ForwardView,
            ("scale", []) => return self.scale(),
            ("image", []) => return self.image(),
            ("restart", []) => self.prepare(Action::Restart),
            ("delete", []) => self.prepare(Action::Delete),
            ("kill", []) => self.kill(),
            ("pulse"|"pulses"|"hz"|"pu",[])=>self.send(Task::Pulses(self.namespace.clone())),
            ("xray"|"xrays"|"xr"|"x",[resource])=>self.send(Task::Xray(resource.clone(),self.namespace.clone())),
            ("xray"|"xrays"|"xr"|"x",[resource,ns]) if ns=="all" || ns=="*" || safety::valid_id(ns)=>self.send(Task::Xray(resource.clone(),if ns=="all"{"*".into()}else{ns.clone()})),
            ("xray"|"xrays"|"xr"|"x",_)=>self.status="Use :xray po|svc|dp|rs|sts|ds [NAMESPACE]".into(),
            ("containers"|"container",[])=>{if self.kind()=="pods"{self.drilldown();}else{self.status="Select a Pod before opening its containers.".into();}},
            ("workload"|"workloads"|"wk",[])=>self.send(Task::Workloads(self.namespace.clone())),
            ("can",[subject])=>self.send(Task::Can(self.namespace.clone(),subject.clone())),
            ("subjects"|"subject"|"rbac",[])=>self.send(Task::Subjects(self.namespace.clone(),None,"RBAC SUBJECTS")),
            ("users"|"user"|"usr",[])=>self.send(Task::Subjects(self.namespace.clone(),Some("User"),"RBAC USERS")),
            ("groups"|"group"|"grp",[])=>self.send(Task::Subjects(self.namespace.clone(),Some("Group"),"RBAC GROUPS")),
            ("dir"|"dirs"|"d"|"ls",[path])=>self.send(Task::Directory(path.into())),
            ("dir"|"dirs"|"d"|"ls",[])=>self.send(Task::Directory(".".into())),
            ("screendump"|"screendumps"|"sd",[])=>match crate::screendump::list(){Ok(records)=>{let titles=records.iter().map(|record|record.title.clone()).collect();self.saved=records;self.modal=Modal::Picker("Saved resources",titles,0);},Err(error)=>self.status=error},
            ("cow",_)=>self.modal=Modal::Detail(format!(" {}\n < {} >\n  \\   ^__^\n   \\  (oo)\\_______\n      (__)\\       )\\\n          ||----w |\n          ||     ||", "_".repeat(args.join(" ").len().min(70)),safety::safe_text(&args.join(" "),70)),0),
            ("can",_)=>self.status="Use :can u:USER | g:GROUP | s:NAMESPACE:SERVICEACCOUNT".into(),
            ("rbacs"|"policies",[])=>self.send(Task::Subjects(self.namespace.clone(),None,"RBAC SUBJECTS")),
            ("rbacs"|"policies"|"users"|"groups"|"subjects",_)=>self.status="Use :subjects, :users or :groups, then Enter on a subject; :can u:USER, g:GROUP or s:NAMESPACE:SA for one subject.".into(),
            ("logs",[selector])=>self.request_log_group(self.namespace.clone(),crate::logs::PodLogSelection::Selector(selector.clone()),false,true,false),
            ("logs-live",[selector])=>self.request_log_group(self.namespace.clone(),crate::logs::PodLogSelection::Selector(selector.clone()),false,true,true),
            ("logs-live",[])=>self.request_dynamic_workload_logs(),
            ("copilot",[])=>{
                if let Some((kind,namespace,name))=self.selected_target() {
                    if self.busy {self.status="Wait for the current request before preparing Copilot.".into();}
                    else {
                        self.modal=Modal::AdviceLoading;
                        self.send(Task::AdvicePrepare(kind,namespace,name,self.selected_uid(),self.options.copilot_policy.clone()));
                        if !self.busy {
                            self.modal=Modal::AdviceDocument("COPILOT NOT STARTED".into(),"Request queue unavailable; open Copilot again when the worker is available.".into(),0);
                        }
                    }
                }else{self.status="Select a resource before :copilot.".into();}
            }
            ("helm-install",[])=>self.begin_helm_form(helm_ui::Intent::Install),
            ("helm-crds",[])=>self.begin_helm_form(helm_ui::Intent::InstallCrds),
            ("helm-crds-upgrade",[])=>self.begin_helm_form(helm_ui::Intent::UpgradeCrds),
            ("helm-crds-delete",[])=>self.begin_helm_form(helm_ui::Intent::DeleteCrds),
            ("helm-crds-migrate",[])=>self.begin_helm_form(helm_ui::Intent::MigrateCrds),
            ("helm-test",[])=>{
                let selected=match &self.modal {Modal::Helm(table)=>table.current(),_=>self.subviews.iter().rev().find_map(|modal|if let Modal::Helm(table)=modal{table.current()}else{None})};
                if let Some(release)=selected {self.begin_helm_form(helm_ui::Intent::Test(release));}else{self.status="Select a release in :helm before :helm-test or press t in its table.".into();}
            }
            ("helm",[])=>self.request_helm(helm_ui::Request::List(self.namespace.clone(),self.options.helm_storage),false),
            ("helm",[ns]) if ns=="all" || ns=="*" || safety::valid_id(ns)=>self.request_helm(helm_ui::Request::List(if ns=="all"{"*".into()}else{ns.clone()},self.options.helm_storage),false),
            ("history",[name]) if crate::helm::valid_release_name(name)=>self.request_helm(helm_ui::Request::NamedHistory(self.namespace.clone(),name.clone(),self.options.helm_storage),false),
            ("history",[])=>{
                let selected=self.subviews.iter().rev().find_map(|modal|if let Modal::Helm(table)=modal{table.selected().cloned()}else{None});
                if let Some(release)=selected{self.request_helm(helm_ui::Request::History(release),false);}else{self.status="Use :history RELEASE or select a release in :helm and press Enter.".into();}
            }
            ("helm"|"history",_)=>self.status="Use :helm [NAMESPACE] or :history RELEASE; --allow-helm enables live release storage reads.".into(),
            ("benchmarks"|"benchmark"|"be"|"scans"|"scan"|"pop"|"popeye",_)=>self.status="This k9s view requires an external scanner/benchmark integration; it is unavailable. No command was executed.".into(),
            ("secrets"|"secret"|"sec",[])=>self.secrets(),
            ("secrets"|"secret"|"sec",_)=>self.status="Use :secrets in the current namespace; values stay hidden until one key is revealed.".into(),
            _ => {
                let command = match nav::resource(&words) { Ok(command)=>command, Err(error)=>{self.status=error;return Ok(false);} };
                if !nav::valid_resource(&command.resource){self.status="Invalid resource command syntax.".into();return Ok(false);}
                let cross_context=command.context.as_ref().is_some_and(|context|context!=&self.context);
                let index = match self.resolve_resource(&command.resource) {Ok(index)=>Some(index),Err(_) if cross_context=>None,Err(error)=>{self.status=error;return Ok(false);}};
                if let Some(ns) = &command.namespace && ns != "all" && ns != "*" && !safety::valid_id(ns) { self.status="Invalid namespace.".into(); return Ok(false); }
                if let Err(error)=nav::Filter::parse(&command.filter) {self.status=error;return Ok(false);}
                if let Err(error)=crate::selectors::validate(&command.selector){self.status=error;return Ok(false);}
                if let Some(context)=&command.context {
                    if self.busy {self.status="Wait for the current request before changing context.".into();return Ok(false);}
                    if !contexts(&self.options.connection)?.contains(context) {self.status="Unknown context; use :ctx to choose.".into();return Ok(false);}
                }
                self.push_view();
                let requested_resource=index.map(|index|self.kinds[index].key()).unwrap_or_else(||command.resource.clone());
                if let Some(index)=index{self.kind_index=index;}
                self.desired_resource=cross_context.then_some(requested_resource);
                self.filter=command.filter;
                self.selector=command.selector;self.field_selector.clear();
                let explicit_namespace=command.namespace.is_some();
                if let Some(ns)=command.namespace {self.namespace=if ns=="all" {"*".into()} else {ns};self.namespace_override=true;}
                if let Some(context)=command.context.filter(|context|*context!=self.context) {
                    self.options.connection.context=Some(context);self.connect();
                    self.namespace_override=explicit_namespace;
                } else {self.changed();}
                self.status=format!("{} / {} | : for commands, / for filter",self.kind(),self.namespace);
            }
        }
        Ok(false)
    }
    fn scale(&mut self) -> Result<bool> {
        if !self.options.connection.writable {
            self.status = "READ ONLY: scale requires --write.".into();
        } else if !["deployments.apps", "statefulsets.apps"].contains(&self.kind().as_str()) {
            self.status = "Scale supports Deployments and StatefulSets; other scale workflows are unavailable.".into();
        } else if let Some((k, n, name)) = self.selected_target() {
            self.modal = Modal::Scale(k, n, name, self.selected_uid(), String::new());
        }
        Ok(false)
    }
    fn image(&mut self) -> Result<bool> {
        if !self.options.connection.writable {
            self.status = "READ ONLY: image edits require --write.".into();
        } else if !crate::edit::supports_resource(&self.kind()) {
            self.status =
                "Image edits support Deployment, StatefulSet, DaemonSet and CronJob containers."
                    .into();
        } else if let Some((k, n, name)) = self.selected_target() {
            self.modal = Modal::Image(k, n, name, self.selected_uid(), None, String::new());
        }
        Ok(false)
    }
    fn suggestions(&self, text: &str) -> Vec<String> {
        let (prefix, token) = text
            .rsplit_once(' ')
            .map(|(prefix, token)| (format!("{prefix} "), token))
            .unwrap_or((String::new(), text));
        let mut values = if prefix.is_empty() {
            let mut values = nav::SPECIAL
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>();
            values.extend(self.aliases.keys().cloned());
            for kind in &self.kinds {
                values.extend([
                    kind.plural.clone(),
                    kind.kind.to_ascii_lowercase(),
                    kind.key(),
                ]);
                values.extend(kind.short_names.clone());
            }
            values
        } else if token.starts_with('@') || matches!(prefix.trim(), "ctx" | "context" | "contexts")
        {
            contexts(&self.options.connection)
                .unwrap_or_default()
                .into_iter()
                .map(|s| {
                    if token.starts_with('@') {
                        format!("@{s}")
                    } else {
                        s
                    }
                })
                .collect()
        } else if prefix.trim() == "sort" {
            vec![
                "name",
                "namespace",
                "status",
                "restarts",
                "age",
                "ready",
                "cpu",
                "memory",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        } else {
            let mut values = self.known_namespaces.clone();
            values.extend(["all".into(), self.namespace.clone()]);
            values
        };
        values.retain(|value| value.starts_with(token));
        values.sort();
        values.dedup();
        values
            .into_iter()
            .take(100)
            .map(|value| format!("{prefix}{value}"))
            .collect()
    }
    fn remember_command(&mut self, text: &str) {
        let text = text.trim();
        if !text.is_empty() && self.history.last().is_none_or(|last| last != text) {
            self.history.push(text.into());
            if self.history.len() > 50 {
                self.history.remove(0);
            }
        }
        self.history_index = None;
        self.completion = None;
    }
    fn editor(&mut self) {
        if self.busy {
            self.status = "Wait for the current request before editing.".into();
            return;
        }
        if !self.options.connection.writable {
            self.status =
                "READ ONLY: editing requires --write and a full server dry-run review.".into();
            return;
        }
        if !crate::edit::supports_resource(&self.kind()) {
            self.status="Controlled editing supports Deployment, StatefulSet, DaemonSet and CronJob containers. Other YAML fields are read only.".into();
            return;
        }
        if let Some(object) = self.visible().get(self.selected) {
            if self.selected_uid().is_none() {
                self.status = "Object UID unavailable; refresh before editing.".into();
                return;
            }
            match crate::edit::container_values(object) {
                Ok(values) => {
                    self.modal = Modal::EditContainers(
                        self.kind(),
                        namespace(object).into(),
                        name(object).into(),
                        self.selected_uid(),
                        values,
                        0,
                    )
                }
                Err(error) => self.status = error,
            }
        }
    }
    fn drilldown(&mut self) {
        let Some(object) = self
            .visible()
            .get(self.selected)
            .map(|object| (*object).clone())
        else {
            return;
        };
        match (
            self.kind().as_str(),
            object["apiVersion"].as_str().unwrap_or(""),
            object["kind"].as_str().unwrap_or(""),
        ) {
            ("pods", "v1", "Pod") => self.send(Task::Drill(
                self.kind(),
                namespace(&object).into(),
                name(&object).into(),
            )),
            ("nodes", "v1", "Node") => {
                let node = name(&object).to_owned();
                if let Err(error) = self.command("v1/pods all") {
                    self.status = error;
                } else {
                    self.field_selector = format!("spec.nodeName={node}");
                    self.status = format!("Pods on node {node} | Esc back");
                }
            }
            ("namespaces", "v1", "Namespace") => {
                let namespace = name(&object).to_owned();
                if self.command(&format!("v1/pods {namespace}")).is_err() {
                    self.status = "Namespace navigation failed.".into();
                }
            }
            ("deployments.apps", "apps/v1", "Deployment")
            | ("statefulsets.apps", "apps/v1", "StatefulSet")
            | ("daemonsets.apps", "apps/v1", "DaemonSet")
            | ("replicasets.apps", "apps/v1", "ReplicaSet")
            | ("replicationcontrollers", "v1", "ReplicationController")
            | ("jobs.batch", "batch/v1", "Job")
            | ("cronjobs.batch", "batch/v1", "CronJob")
            | ("services", "v1", "Service") => match crate::insights::pod_selector(&object) {
                Ok(selector) => {
                    let namespace = namespace(&object);
                    if let Err(error) = self.command(&format!("v1/pods {namespace} '{selector}'")) {
                        self.status = error;
                    }
                }
                Err(error) => self.status = error,
            },
            _ => {
                if let Some((k, n, name)) = self.selected_target() {
                    self.send(Task::Inspect(k, n, name));
                }
            }
        }
    }
    fn push_subview(&mut self, modal: Modal) {
        if self.subviews.len() >= 16 {
            self.subviews.remove(0);
        }
        self.subviews.push(modal);
    }
    fn close_subview(&mut self) {
        self.modal = self.subviews.pop().unwrap_or(Modal::None);
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
    /// Dashboard `L`: follow the selected warning's exact Pod in a pane, over a
    /// separate read-only connection to that card's context.
    /// A read-only log connection for one dashboard context: every write,
    /// interactive and Secret capability is cleared.
    fn pane_connection(&self, context: &str, namespace: &str) -> Connection {
        let mut connection = self.options.connection.clone();
        if !connection.demo {
            connection.context = Some(context.into());
        }
        connection.namespace = namespace.into();
        connection.writable = false;
        connection.allow_exec = false;
        connection.allow_attach = false;
        connection.allow_port_forward = false;
        connection.allow_node_maintenance = false;
        connection.allow_yaml_edit = false;
        connection.allow_force_kill = false;
        connection.allow_secret_reveal = false;
        connection.allow_helm_write = false;
        connection.allow_helm_hooks = false;
        connection.allow_helm_crds = false;
        connection
    }
    /// Dashboard `t`: list workloads from every card's cluster, or close a
    /// running cross-cluster tail.
    fn dashboard_tail(&mut self, dashboard: &mut dashboard_ui::Dashboard) {
        if dashboard.logs.take().is_some() {
            self.worker.logs.cancel();
            return;
        }
        if !self.options.connection.demo && !self.options.connection.logs {
            dashboard.set_notice("Workload tails require --allow-logs.".into(), true);
            return;
        }
        if self.busy {
            dashboard.set_notice(
                "Wait for the current request before opening a tail.".into(),
                true,
            );
            return;
        }
        let contexts = dashboard
            .rows
            .iter()
            .map(|row| {
                let namespace = if row.namespace.is_empty() {
                    self.namespace.clone()
                } else {
                    row.namespace.clone()
                };
                (
                    row.context.clone(),
                    Box::new(self.pane_connection(&row.context, &namespace)),
                )
            })
            .collect::<Vec<_>>();
        dashboard.picker = Some(dashboard_ui::TailPicker::loading());
        self.send(Task::DashboardWorkloads(contexts));
    }
    fn start_dashboard_tail(
        &mut self,
        dashboard: &mut dashboard_ui::Dashboard,
        chosen: Vec<crate::workload_tail::WorkloadRef>,
    ) {
        if self.busy {
            dashboard.set_notice(
                "Wait for the current request before opening a tail.".into(),
                true,
            );
            return;
        }
        let _ = crate::dashboard_memory::remember_tail(&chosen);
        let sources = chosen
            .iter()
            .map(|workload| crate::workload_tail::TailSource {
                connection: self.pane_connection(&workload.context, &workload.namespace),
                workload: workload.clone(),
            })
            .collect::<Vec<_>>();
        let clusters = chosen
            .iter()
            .map(|w| w.context.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        self.subviews
            .retain(|modal| !matches!(modal, Modal::Follow(_) | Modal::Containers(..)));
        let generation = self.worker.logs.cancel();
        let read = crate::logs::ReadOptions {
            head: false,
            ..self.options.log_read
        };
        let mut view = crate::ui_logs::LogView::new("Connecting to workloads...".into(), read);
        view.following = true;
        dashboard.logs = Some(dashboard_ui::LogPane {
            title: format!(
                "TAIL {} workload{} across {clusters} cluster{} (timestamp order)",
                chosen.len(),
                if chosen.len() == 1 { "" } else { "s" },
                if clusters == 1 { "" } else { "s" }
            ),
            uid: String::new(),
            view: Box::new(view),
        });
        self.send(Task::DashboardTail(
            sources,
            generation,
            self.options.log_seconds,
            read,
        ));
    }
    fn dashboard_logs(&mut self, dashboard: &mut dashboard_ui::Dashboard) {
        let selected = dashboard
            .selected_warning()
            .map(|w| (w.namespace.clone(), w.pod.clone(), w.uid.clone()));
        if let Some(pane) = dashboard.logs.take() {
            self.worker.logs.cancel();
            if selected.as_ref().is_none_or(|(_, _, uid)| *uid == pane.uid) {
                return;
            }
        }
        let Some((namespace, pod, uid)) = selected else {
            dashboard.set_notice(
                "Press w to select a warning Pod, then L to follow its logs.".into(),
                true,
            );
            return;
        };
        if !self.options.connection.demo && !self.options.connection.logs {
            dashboard.set_notice("Log panes require --allow-logs.".into(), true);
            return;
        }
        if uid.is_empty() || !safety::valid_id(&namespace) {
            dashboard.set_notice(
                "This warning has no exact Pod identity; logs are unavailable.".into(),
                true,
            );
            return;
        }
        if self.busy {
            dashboard.set_notice(
                "Wait for the current request before opening logs.".into(),
                true,
            );
            return;
        }
        let Some(context) = dashboard.selected().map(|row| row.context.clone()) else {
            return;
        };
        let connection = self.pane_connection(&context, &namespace);
        self.subviews
            .retain(|modal| !matches!(modal, Modal::Follow(_) | Modal::Containers(..)));
        let generation = self.worker.logs.cancel();
        let read = crate::logs::ReadOptions {
            head: false,
            ..self.options.log_read
        };
        let mut view = crate::ui_logs::LogView::new("Connecting to logs...".into(), read);
        view.following = true;
        dashboard.logs = Some(dashboard_ui::LogPane {
            title: format!("LOGS {context} / {namespace}/{pod} (exact UID)"),
            uid: uid.clone(),
            view: Box::new(view),
        });
        self.send(Task::DashboardLogs(
            Box::new(connection),
            namespace,
            pod,
            uid,
            generation,
            self.options.log_seconds,
            read,
        ));
    }
    fn secrets(&mut self) {
        if !self.options.connection.allow_secret_reveal {
            self.status = "Secrets are excluded by default; start with --allow-secret-reveal to list keys and reveal one value at a time.".into();
            return;
        }
        if self.namespace == "all" || self.namespace == "*" {
            self.status = "Choose one namespace before opening :secrets.".into();
            return;
        }
        self.send(Task::Secrets(self.namespace.clone()));
    }
    /// k9s Ctrl-K: a reviewed, approved Pod delete with gracePeriodSeconds=0.
    fn kill(&mut self) {
        if !self.options.connection.writable || !self.options.connection.allow_force_kill {
            self.status = "Force kill requires --write --allow-force-kill; Ctrl-D is a reviewed graceful delete.".into();
            return;
        }
        if self.kind() != "pods" {
            self.status = "Force kill applies to Pods only.".into();
            return;
        }
        self.prepare(Action::Kill);
    }
    fn yaml_editor(&mut self) {
        if self.busy {
            self.status = "Wait for the current request before editing.".into();
            return;
        }
        if !self.options.connection.writable || !self.options.connection.allow_yaml_edit {
            self.status = "YAML editing requires --write --allow-yaml-edit.".into();
            return;
        }
        if let Some((kind, ns, name)) = self.selected_target() {
            self.send(Task::YamlDraft(kind, ns, name, self.selected_uid()));
        }
    }
    fn node_maintenance(&mut self, request: crate::node_maintenance::Request) {
        if self.busy {
            self.status = "Wait for the current request before node maintenance.".into();
            return;
        }
        if self.kind() != "nodes" {
            self.status = "Select a Node in :nodes first.".into();
            return;
        }
        if !self.options.connection.writable || !self.options.connection.allow_node_maintenance {
            self.status = "Node maintenance requires --write --allow-node-maintenance.".into();
            return;
        }
        if let Some((_, _, name)) = self.selected_target() {
            self.send(Task::NodePrepare(name, self.selected_uid(), request));
        }
    }
    fn selected_uid(&self) -> Option<String> {
        self.visible()
            .get(self.selected)
            .and_then(|v| v["metadata"]["uid"].as_str())
            .map(str::to_owned)
    }
    fn request_export(&mut self) {
        if self.busy || self.write_inflight {
            self.status = "Wait for the current request before exporting.".into();
            return;
        }
        if let Modal::Follow(view) = &self.modal {
            let source = if let Some(target) = &self.log_target {
                if let Some(membership) = &target.membership {
                    serde_json::json!({"context":self.context,"membership":membership})
                } else {
                    serde_json::json!({"context":self.context,"namespace":target.namespace,"pod":target.pod,"uid":target.uid,"container":target.container,"all_containers":target.all})
                }
            } else {
                serde_json::json!({"context":self.context,"source":"unavailable"})
            };
            let payload = format!("# Source: {}\n{}", source, view.export_text());
            match crate::exports::Draft::new(
                "logs",
                "log",
                "Captured logs / current filtered window",
                payload,
                "Application logs can contain credentials or other confidential data. This saves the retained window, including timestamps and source labels.",
            ) {
                Ok(draft) => {
                    let previous = std::mem::replace(&mut self.modal, Modal::Export(draft));
                    self.push_subview(previous);
                }
                Err(error) => self.status = error,
            }
        } else if let Modal::Helm(table) = &self.modal {
            if let Some(release) = table.current() {
                self.send(Task::ExportHelm(release));
            }
        } else if matches!(self.modal, Modal::None) {
            if let Some((kind, ns, name)) = self.selected_target() {
                self.send(Task::ExportResource(kind, ns, name));
            }
        } else {
            self.status = "Export from a resource table, Helm table or log view.".into();
        }
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
        if let Some(uid) = self.restoring_uid.take() {
            self.selected = self
                .visible()
                .iter()
                .position(|object| object["metadata"]["uid"].as_str() == Some(&uid))
                .unwrap_or(self.selected);
        }
    }
    fn send(&mut self, task: Task) {
        if self.busy && !matches!(&task, Task::Connect(_)) {
            self.status = "Busy; wait for the current request.".into();
            return;
        }
        let writes = matches!(
            &task,
            Task::Execute(..)
                | Task::YamlExecute(..)
                | Task::NodeExecute(..)
                | Task::RunSession(..)
                | Task::RunForward(..)
                | Task::HelmExecute(..)
                | Task::AdviceExecute(..)
        );
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
        self.worker.forward.cancel();
        self.selection_ui.restoring = None;
        self.selection_ui.focus = None;
        self.selection_ui.draft = None;
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
        self.helm_generation = self.helm_generation.wrapping_add(1);
        self.metrics_history.clear();
        self.table_metrics.clear();
        self.last_table_metrics = None;
        self.overview = None;
        self.marks.clear();
        self.subviews.clear();
        self.log_target = None;
    }
    fn connect(&mut self) {
        self.namespace_override = false;
        self.changed();
        self.connected = false;
        self.server = "connecting...".into();
        self.connecting_epoch = Some(self.epoch);
        self.send(Task::Connect(self.options.connection.clone()));
    }
    fn expire_table_metrics(&mut self, now: Instant) -> bool {
        if !self.table_metrics.expired(now) {
            return false;
        }
        let uid = self.selected_uid();
        self.table_metrics.expire();
        if let Some(uid) = uid {
            self.selected = self
                .visible()
                .iter()
                .position(|object| object["metadata"]["uid"].as_str() == Some(&uid))
                .unwrap_or(self.selected);
        }
        true
    }
    fn receive(&mut self) -> bool {
        let mut changed = self.expire_table_metrics(Instant::now());
        let forward = self.worker.forward.text();
        if forward != self.last_forward {
            self.last_forward = forward;
            self.forward_selected = self
                .forward_selected
                .min(self.worker.forward.len().saturating_sub(1));
            changed = true;
        }
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
                Reply::YamlDraft(draft) => {
                    self.modal = Modal::YamlEditor(Box::new(crate::ui_yaml::Editor::new(*draft)));
                    self.status = "Edit spec fields. Ctrl-S prepares a diff; Esc cancels.".into();
                }
                Reply::YamlPrepared(editor, result) => match result {
                    Ok(prepared) => {
                        let view = review_ui::Review::new(
                            prepared.summary().into(),
                            prepared.confirmation(),
                            self.epoch,
                        );
                        self.push_subview(Modal::YamlEditor(editor));
                        self.modal = Modal::WriteReview(Box::new(WriteReview {
                            plan: WritePlan::Yaml(Box::new(prepared)),
                            view,
                        }));
                        self.status = "Read each change, then type the target to apply.".into();
                    }
                    Err(error) => {
                        self.modal = Modal::YamlEditor(editor);
                        self.status = error;
                    }
                },
                Reply::NodePrepared(prepared) => {
                    let view = review_ui::Review::new(
                        prepared.summary().into(),
                        prepared.confirmation(),
                        self.epoch,
                    );
                    self.modal = Modal::WriteReview(Box::new(WriteReview {
                        plan: WritePlan::Node(prepared),
                        view,
                    }));
                    self.status = "Review the node and workload inventory before approval.".into();
                }
                Reply::WriteOutcome(result) => {
                    self.subviews.clear();
                    self.stale = true;
                    self.refresh = true;
                    self.modal = Modal::Detail(
                        match result {
                            Ok(text) => text,
                            Err(error) => format!(
                                "Operation stopped\n\n{error}\n\nInspect current cluster state before retrying."
                            ),
                        },
                        0,
                    );
                    self.status =
                        "Operation finished. Inspect the outcome before continuing.".into();
                }
                Reply::Export(draft) => {
                    let previous = std::mem::replace(&mut self.modal, Modal::Export(draft));
                    self.push_subview(previous);
                    self.status = "Review the local export; Enter saves, Esc cancels.".into();
                }
                Reply::Connected(c, s, n, k) => {
                    let selected_kind = self.desired_resource.take().unwrap_or_else(|| self.kind());
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
                    self.desired_resource = retained_kind.is_none().then_some(selected_kind);
                    self.connected = true;
                    self.refresh = retained_kind.is_some();
                    self.watch_blocked = retained_kind.is_none();
                    self.status = if retained_kind.is_some() {
                        "Connected. READ ONLY unless explicitly enabled."
                    } else {
                        "Restoring selected resource through destination API discovery..."
                    }
                    .into();
                    if !self.options.connection.demo {
                        self.send(Task::Discover);
                        if self.selection_ui.restoring.is_some() {
                            self.refresh = false;
                        }
                    } else {
                        self.finish_saved_selection_restore();
                    }
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
                Reply::AdvicePrepared(result) => {
                    if !matches!(self.modal, Modal::AdviceLoading) {
                        continue;
                    }
                    self.modal = match result {
                        Ok((prepared, text)) => {
                            self.status = "Copilot preview ready; nothing sent.".into();
                            Modal::AdviceReview(Box::new(advice_ui::Review::new(
                                prepared, text, self.epoch,
                            )))
                        }
                        Err(error) => Modal::AdviceDocument(
                            "COPILOT SETUP / no request sent".into(),
                            error,
                            0,
                        ),
                    };
                }
                Reply::AdviceOutcome(result) => {
                    self.advice_cancel = None;
                    self.modal = match result {
                        Ok(output) => Modal::AdviceDocument(
                            "COPILOT ADVICE / review independently".into(),
                            format!(
                                "READ-ONLY ADVICE / no commands executed\n\n{}\n\nCheck suggestions against live state before taking action.",
                                safety::safe_lines(&output.answer, 64 * 1024)
                            ),
                            0,
                        ),
                        Err(error) => {
                            Modal::AdviceDocument("COPILOT REQUEST STOPPED".into(), error, 0)
                        }
                    };
                    self.status = "Copilot request finished. No automatic remediation.".into();
                }
                Reply::HelmPrepared(generation, result) => {
                    if generation != self.helm_generation
                        || !matches!(self.modal, Modal::HelmLoading(_))
                    {
                        continue;
                    }
                    self.modal = match result {
                        Ok(prepared) => {
                            self.status = "Helm review ready; no execution requested.".into();
                            self.sticky_status = false;
                            Modal::HelmReview(Box::new(helm_ui::Review::new(prepared, self.epoch)))
                        }
                        Err(error) => {
                            self.status = error.clone();
                            Modal::HelmDocument(
                                "HELM PREPARATION FAILED / no execution requested".into(),
                                error,
                                0,
                            )
                        }
                    };
                }
                Reply::HelmOutcome(generation, result) => {
                    if generation != self.helm_generation && !write_result {
                        continue;
                    }
                    self.helm_cancel = None;
                    self.subviews.retain(|modal| {
                        !matches!(
                            modal,
                            Modal::Helm(..)
                                | Modal::HelmForm(..)
                                | Modal::HelmReview(..)
                                | Modal::HelmLoading(..)
                        )
                    });
                    let (title, text) = match result {
                        Ok(outcome) => {
                            let title = if outcome.uncertain {
                                "HELM OUTCOME UNKNOWN"
                            } else if outcome.success
                                && outcome.summary.contains("STAGED / finalization pending")
                            {
                                "CRD MIGRATION STAGED / finalization pending"
                            } else if outcome.success {
                                "HELM OPERATION SUCCEEDED"
                            } else {
                                "HELM OPERATION FAILED"
                            };
                            (title.to_owned(), outcome.summary)
                        }
                        Err(error) => (
                            "HELM OUTCOME UNKNOWN / reconcile before retrying".into(),
                            error,
                        ),
                    };
                    self.status = title.clone();
                    self.sticky_status = true;
                    self.refresh = true;
                    self.stale = true;
                    self.watching = false;
                    self.worker.watch.invalidate(self.epoch);
                    self.modal = Modal::HelmOutcome(title, text, 0);
                }
                Reply::Helm(generation, result) => {
                    if generation != self.helm_generation
                        || !matches!(self.modal, Modal::HelmLoading(_))
                    {
                        continue;
                    }
                    self.modal = match result {
                        Ok(helm_ui::Response::Table(table)) => Modal::Helm(table),
                        Ok(helm_ui::Response::Document(title, text)) => {
                            Modal::HelmDocument(title, text, 0)
                        }
                        Err(error) => {
                            self.status = error.clone();
                            Modal::HelmDocument("HELM READ UNAVAILABLE".into(), error, 0)
                        }
                    };
                }
                Reply::Detail(text) => {
                    self.modal = Modal::Detail(text.replace('\t', " | "), 0);
                }
                Reply::Object(object) => self.modal = Modal::Overview(object, 0),
                Reply::Drill(object) => self.modal = Modal::PodContainers(object, 0),
                Reply::Directory(directory) => self.modal = Modal::Directory(directory, 0),
                Reply::Subjects(title, namespace, index) => {
                    let modal = std::mem::replace(&mut self.modal, Modal::None);
                    if !matches!(modal, Modal::None) {
                        self.push_subview(modal);
                    }
                    self.modal = Modal::Subjects(Box::new(crate::ui_subjects::View::new(
                        title, namespace, index,
                    )));
                }
                Reply::Secrets(index) => {
                    let modal = std::mem::replace(&mut self.modal, Modal::None);
                    if !matches!(modal, Modal::None) {
                        self.push_subview(modal);
                    }
                    self.modal = Modal::Secrets(Box::new(crate::ui_secrets::View::new(index)));
                }
                Reply::DashboardWorkloads(lists) => {
                    if let Modal::LiveOverview(dashboard) = &mut self.modal
                        && let Some(picker) = dashboard.picker.as_mut()
                        && picker.loading
                    {
                        picker.loading = false;
                        for (context, result) in lists {
                            match result {
                                Ok(found) => picker.entries.extend(found),
                                Err(error) => picker.errors.push(format!("{context}: {error}")),
                            }
                        }
                        // Pre-select the last tail's workloads by name; the
                        // fresh listing supplies their current UIDs.
                        if let Ok(Some(last)) = crate::dashboard_memory::load() {
                            for (index, entry) in picker.entries.iter().enumerate() {
                                if last.picks(entry)
                                    && picker.checked.len() < crate::workload_tail::MAX_SOURCES
                                {
                                    picker.checked.insert(index);
                                }
                            }
                        }
                    }
                }
                Reply::SecretValue(uid, key, value) => {
                    if let Modal::Secrets(view) = &mut self.modal {
                        view.reveal(&uid, &key, value);
                    }
                }
                Reply::Overview(feed) => {
                    if matches!(self.modal, Modal::LiveOverview(..)) {
                        match feed.snapshot() {
                            Ok(rows) => {
                                if let Modal::LiveOverview(dashboard) = &mut self.modal {
                                    dashboard.update(rows);
                                }
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
                Reply::ForwardQueued(id) => {
                    self.forward_selected = self.worker.forward.index(id);
                    self.modal = Modal::ForwardView;
                    self.status =
                        "Forward approved. Esc returns to resources; :pf reopens sessions.".into();
                }
                Reply::Membership(target, generation) => {
                    if generation
                        != self
                            .worker
                            .logs
                            .generation
                            .load(std::sync::atomic::Ordering::SeqCst)
                        || !matches!(self.modal, Modal::Follow(_))
                    {
                        continue;
                    }
                    self.log_target = Some(target);
                    if let Modal::Follow(mut view) = std::mem::replace(&mut self.modal, Modal::None)
                    {
                        view.enable_all_containers(false);
                        let read = view.range;
                        self.reload_logs(view, read);
                    }
                }
                Reply::Containers(target, generation, names) => {
                    if self
                        .worker
                        .logs
                        .generation
                        .load(std::sync::atomic::Ordering::SeqCst)
                        != generation
                        || !matches!(self.modal, Modal::Containers(..))
                    {
                        continue;
                    }
                    if names.is_empty() {
                        self.status = "No containers found in the selected pod.".into();
                    } else {
                        self.modal = Modal::Containers(target, names, 0);
                    }
                }
                Reply::Logs(generation, text) => {
                    if self
                        .worker
                        .logs
                        .generation
                        .load(std::sync::atomic::Ordering::SeqCst)
                        == generation
                        && let Modal::Follow(view) = &mut self.modal
                    {
                        view.update(text);
                    }
                }
                Reply::LogFailure(generation, error) => {
                    if self
                        .worker
                        .logs
                        .generation
                        .load(std::sync::atomic::Ordering::SeqCst)
                        == generation
                        && matches!(self.modal, Modal::Follow(..) | Modal::Containers(..))
                    {
                        self.status = error.clone();
                        self.modal = Modal::Detail(format!("LOGS unavailable\n{error}"), 0);
                    } else if self
                        .worker
                        .logs
                        .generation
                        .load(std::sync::atomic::Ordering::SeqCst)
                        == generation
                        && let Modal::LiveOverview(dashboard) = &mut self.modal
                        && let Some(pane) = dashboard.logs.as_mut()
                    {
                        pane.view
                            .replace_source(format!("[Logs unavailable: {error}]"));
                    }
                }
                Reply::Following => {}
                Reply::Catalog(k) => {
                    let resource = self.desired_resource.take().unwrap_or_else(|| self.kind());
                    self.kinds = k;
                    if let Ok(index) = self.resolve_resource(&resource) {
                        self.kind_index = index;
                        let restoring = self.selection_ui.restoring.take();
                        self.changed();
                        self.selection_ui.restoring = restoring;
                        self.status="API discovery complete; all discovered aliases available (Secrets excluded).".into();
                        self.finish_saved_selection_restore();
                    } else {
                        self.desired_resource = Some(resource.clone());
                        self.watch_blocked = true;
                        self.refresh = false;
                        self.status = format!(
                            "Resource '{resource}' is unavailable in this context; : selects another resource."
                        );
                    }
                }
                Reply::CatalogFailure(error) => {
                    self.status =
                        format!("Discovery unavailable: {error} | built-in resources retained");
                    self.sticky_status = true;
                    if self.selection_ui.restoring.take().is_some() {
                        self.refresh = false;
                        self.watch_blocked = true;
                        self.status =
                            format!("Saved selection could not revalidate discovery: {error}");
                    } else if self.desired_resource.is_none() {
                        self.refresh = true;
                        self.watch_blocked = false;
                    }
                }
                Reply::Namespaces(names) => {
                    self.known_namespaces = names.clone();
                    if matches!(self.modal, Modal::Picker("Namespace", ..)) {
                        let index = names
                            .iter()
                            .position(|name| {
                                name == &self.namespace || (name == "all" && self.namespace == "*")
                            })
                            .unwrap_or(0);
                        self.modal = Modal::Picker("Namespace", names, index);
                    }
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
                        self.subviews.clear();
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
                    // Retry from completion, so a stalled provider cannot keep
                    // the command queue continuously busy after its deadline.
                    if (scope == crate::metrics::MetricsScope::Nodes && self.kind() == "nodes")
                        || (scope == crate::metrics::MetricsScope::Pods
                            && self.kind() == "pods"
                            && namespace == self.namespace)
                    {
                        self.last_table_metrics = Some(now);
                    }

                    let selected_uid = self.selected_uid();
                    let text = match result {
                        Ok(snapshot) => {
                            self.table_metrics.record(&snapshot, &self.items, now);
                            self.metrics_history.record(&self.context, &snapshot, now);
                            format!("{}\n{}", self.metrics_history.text(now), snapshot.text())
                        }
                        Err(error) => {
                            self.table_metrics.fail(&error);
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
                    if let Some(uid) = selected_uid {
                        self.selected = self
                            .visible()
                            .iter()
                            .position(|object| object["metadata"]["uid"].as_str() == Some(&uid))
                            .unwrap_or(self.selected);
                    }
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
        if matches!(self.modal, Modal::Follow(_))
            && let Some(text) = self.worker.logs.take()
            && let Modal::Follow(view) = &mut self.modal
        {
            view.update(text);
            changed = true;
        }
        if let Modal::LiveOverview(dashboard) = &mut self.modal
            && let Some(pane) = dashboard.logs.as_mut()
            && let Some(text) = self.worker.logs.take()
        {
            pane.view.update(text);
            changed = true;
        }
        if let Some(feed) = &self.overview {
            match feed.take_update() {
                Ok(Some(rows)) => {
                    if let Modal::LiveOverview(dashboard) = &mut self.modal {
                        dashboard.update(rows);
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
        changed |= self.finish_saved_workload_focus();
        changed
    }
    fn open_dashboard(&mut self, names: Vec<String>) -> Result<()> {
        crate::overview::validate(&names)?;
        if names.len() > crate::overview::MAX_LIVE_CONTEXTS {
            return Err("The dashboard supports up to 9 explicit contexts.".into());
        }
        if self.busy {
            self.status = "Wait for the current request before opening the dashboard.".into();
            return Ok(());
        }
        self.overview = None;
        let mut connection = self.options.connection.clone();
        connection.namespace = self.namespace.clone();
        self.modal = Modal::LiveOverview(dashboard_ui::Dashboard::new(
            &names,
            self.options.connection.demo,
        ));
        if let Err(error) = crate::dashboard_memory::remember_scope(&names, &self.namespace) {
            self.status = format!("Dashboard opened; it could not be remembered: {error}");
        }
        self.send(Task::Overview(connection, names));
        Ok(())
    }
    /// Enter opens the focused card's Pods, or the exact Pod's Events when a
    /// warning record is selected. Event drilldown binds name and UID, so a
    /// replacement Pod with the same name never inherits the old records.
    fn open_dashboard_pods(&mut self, dashboard: dashboard_ui::Dashboard) -> Result<()> {
        let Some(selected) = dashboard.selected() else {
            self.modal = Modal::LiveOverview(dashboard);
            return Ok(());
        };
        let warning = dashboard.selected_warning();
        if self.busy {
            self.status = format!(
                "Wait for the current request before opening {}.",
                if warning.is_some() { "Events" } else { "Pods" }
            );
            self.modal = Modal::LiveOverview(dashboard);
            return Ok(());
        }
        let fields = match warning {
            Some(warning) => {
                let fields = crate::selectors::pod_events_field(&warning.pod, &warning.uid);
                if let Err(error) = crate::selectors::field_scope(&fields) {
                    self.modal = Modal::LiveOverview(dashboard);
                    self.status = format!("Cannot open Events for this warning: {error}");
                    return Ok(());
                }
                fields
            }
            None => String::new(),
        };
        let (resource, status) = match warning {
            Some(warning) => (
                "v1/events",
                format!("Events for Pod {} (exact UID) | Esc back", warning.pod),
            ),
            None => ("v1/pods", "Opening selected context Pods.".into()),
        };
        let context = selected.context.clone();
        let namespace = if let Some(warning) = warning {
            warning.namespace.clone()
        } else if selected.namespace.is_empty() {
            self.namespace.clone()
        } else {
            selected.namespace.clone()
        };
        self.push_view();
        self.overview = None;
        self.filter.clear();
        self.selector.clear();
        self.field_selector = fields;
        self.namespace = namespace;
        self.desired_resource = Some(resource.into());
        if context == self.context {
            if let Ok(index) = self.resolve_resource(resource) {
                self.kind_index = index;
                self.desired_resource = None;
            }
            self.changed();
        } else {
            self.options.connection.context = Some(context);
            self.connect();
        }
        self.namespace_override = true;
        self.status = status;
        Ok(())
    }
    fn theme_picker(&mut self) -> Result<()> {
        let all = themes::all()?;
        let index = all.iter().position(|t| t.id == self.theme.id).unwrap_or(0);
        self.modal = Modal::Themes(all, index, self.theme.clone());
        Ok(())
    }
    fn refresh_table_metrics(&mut self) {
        if self.busy || !matches!(self.kind().as_str(), "pods" | "nodes") {
            return;
        }
        self.last_table_metrics = Some(Instant::now());
        self.send(Task::Metrics(
            if self.kind() == "nodes" {
                crate::metrics::MetricsScope::Nodes
            } else {
                crate::metrics::MetricsScope::Pods
            },
            self.namespace.clone(),
            false,
        ));
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
        if matches!(self.kind().as_str(), "pods" | "nodes") {
            self.last_table_metrics = Some(self.last_metrics);
        }
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
        self.request_session(false);
    }
    fn request_session(&mut self, attach: bool) {
        if self.busy {
            self.status = "Busy; wait for the current request.".into();
            return;
        }
        if self.options.connection.demo {
            self.status = "Pod shells require a live cluster; DEMO has no remote processes.".into();
            return;
        }
        if !self.options.connection.writable
            || !(if attach {
                self.options.connection.allow_attach
            } else {
                self.options.connection.allow_exec
            })
        {
            self.status = if attach {
                "Pod attach requires --write and --allow-attach."
            } else {
                "Pod shell requires --write and --allow-exec."
            }
            .into();
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
                attach,
            );
        }
    }
    fn request_forward(&mut self) {
        if self.busy {
            self.status = "Wait for the current request before forwarding.".into();
            return;
        }
        if !self.options.connection.writable || !self.options.connection.allow_port_forward {
            self.status = "Port-forward requires --write and --allow-forward.".into();
            return;
        }
        if self.options.connection.demo || self.kind() != "pods" {
            self.status = "Port-forward requires a selected live Pod.".into();
            return;
        }
        if let Some((_, namespace, pod)) = self.selected_target() {
            self.modal = Modal::ForwardForm(
                namespace,
                pod,
                self.selected_uid(),
                [
                    self.worker.forward.suggested_port().to_string(),
                    "80".into(),
                    "300".into(),
                ],
                0,
            );
        }
    }
    fn request_log_group(
        &mut self,
        namespace: String,
        selection: crate::logs::PodLogSelection,
        previous: bool,
        follow: bool,
        dynamic: bool,
    ) {
        if self.busy {
            self.status = "Wait for the current request before capturing log membership.".into();
            return;
        }
        if !self.options.connection.logs {
            self.status = "Multi-Pod logs require --allow-logs.".into();
            return;
        }
        if !safety::valid_id(&namespace) {
            self.status = "Choose one explicit namespace for workload logs.".into();
            return;
        }
        if dynamic && (previous || !follow || self.options.log_read.head) {
            self.status =
                "Dynamic membership follows new records; choose a tail or time range, not head."
                    .into();
            return;
        }
        let (name, uid) = match &selection {
            crate::logs::PodLogSelection::Workload { name, uid, .. } => (name.clone(), uid.clone()),
            crate::logs::PodLogSelection::Selector(selector) => {
                (format!("selector:{selector}"), String::new())
            }
        };
        let target = LogTarget {
            namespace,
            pod: name,
            uid,
            container: None,
            previous,
            follow,
            all: true,
            dynamic,
            membership: None,
        };
        self.subviews
            .retain(|modal| !matches!(modal, Modal::Follow(_) | Modal::Containers(..)));
        let generation = self.worker.logs.cancel();
        let mut view = crate::ui_logs::LogView::new(
            if dynamic {
                "Capturing complete Pod membership; dynamic opt-in announces every join and departure..."
            } else {
                "Capturing complete fixed Pod membership; no new Pods will be added automatically..."
            }
            .into(),
            self.options.log_read,
        );
        view.following = follow && !previous && !self.options.log_read.head;
        view.previous = previous;
        self.log_target = Some(target.clone());
        self.modal = Modal::Follow(Box::new(view));
        self.send(Task::CaptureLogs(target, selection, generation));
    }
    fn request_dynamic_workload_logs(&mut self) {
        if !supports_group_logs(&self.kind()) {
            self.status =
                "Use :logs-live on a selected workload, or :logs-live LABEL_SELECTOR.".into();
            return;
        }
        match (self.selected_target(), self.selected_uid()) {
            (Some((resource, namespace, name)), Some(uid)) => self.request_log_group(
                namespace,
                crate::logs::PodLogSelection::Workload {
                    resource,
                    name,
                    uid,
                },
                false,
                true,
                true,
            ),
            (Some(_), None) => {
                self.status =
                    "Workload UID unavailable; refresh before capturing log membership.".into()
            }
            (None, _) => self.status = "Select a workload first.".into(),
        }
    }
    fn request_logs(&mut self, previous: bool, follow: bool) {
        if supports_group_logs(&self.kind()) {
            if let Some((resource, namespace, name)) = self.selected_target() {
                if let Some(uid) = self.selected_uid() {
                    self.request_log_group(
                        namespace,
                        crate::logs::PodLogSelection::Workload {
                            resource,
                            name,
                            uid,
                        },
                        previous,
                        follow,
                        false,
                    );
                } else {
                    self.status =
                        "Workload UID unavailable; refresh before capturing log membership.".into();
                }
            }
            return;
        }
        if !follow && !previous {
            self.request_log_picker(previous, follow);
            return;
        }
        if self.busy {
            self.status = "Busy; wait before opening logs.".into();
            return;
        }
        if !self.options.connection.logs {
            self.status = "Logs may contain sensitive data; opt in with --allow-logs.".into();
            return;
        }
        let Some(object) = self
            .visible()
            .get(self.selected)
            .map(|object| (*object).clone())
        else {
            return;
        };
        if self.kind() != "pods" {
            self.status = "Logs apply to core Pods; choose a Pod.".into();
            return;
        }
        let Some(uid) = self.selected_uid() else {
            self.status = "Pod UID unavailable; refresh before reading logs.".into();
            return;
        };
        let ordinary = ordinary_container_names(&object);
        let preferred =
            object["metadata"]["annotations"]["kubectl.kubernetes.io/default-container"]
                .as_str()
                .filter(|container| ordinary.iter().any(|name| name == container))
                .map(str::to_owned)
                .or_else(|| (ordinary.len() == 1).then(|| ordinary[0].clone()));
        let target = LogTarget {
            namespace: namespace(&object).into(),
            pod: name(&object).into(),
            uid,
            all: preferred.is_none(),
            dynamic: false,
            membership: None,
            container: preferred.or_else(|| ordinary.first().cloned()),
            previous,
            follow,
        };
        self.open_logs(target, self.options.log_read);
    }
    fn request_log_picker(&mut self, previous: bool, follow: bool) {
        if self.busy {
            self.status = "Busy; wait before opening logs.".into();
            return;
        }
        if !self.options.connection.logs {
            self.status = "Logs require --allow-logs.".into();
            return;
        }
        if let Some((kind, namespace, pod)) = self.selected_target() {
            if kind != "pods" {
                self.status = "Logs apply to core Pods.".into();
                return;
            }
            let Some(uid) = self.selected_uid() else {
                self.status = "Pod UID unavailable; refresh first.".into();
                return;
            };
            let container = self
                .visible()
                .get(self.selected)
                .and_then(|object| ordinary_container_names(object).into_iter().next());
            let target = LogTarget {
                namespace,
                pod,
                container,
                uid,
                previous,
                follow,
                all: false,
                dynamic: false,
                membership: None,
            };
            self.subviews
                .retain(|modal| !matches!(modal, Modal::Follow(_) | Modal::Containers(..)));
            let generation = self.worker.logs.cancel();
            self.modal = Modal::Containers(target.clone(), vec![], 0);
            self.send(Task::Containers(target, generation));
        }
    }
    fn open_logs(&mut self, target: LogTarget, read: crate::logs::ReadOptions) {
        if self.busy {
            self.status = "Busy; wait before opening logs.".into();
            self.close_subview();
            return;
        }
        self.subviews
            .retain(|modal| !matches!(modal, Modal::Follow(_) | Modal::Containers(..)));
        let generation = self.worker.logs.cancel();
        let mut view = crate::ui_logs::LogView::new("Connecting to logs...".into(), read);
        view.following = target.follow && !read.head && !target.previous;
        view.previous = target.previous;
        view.enable_all_containers(target.membership.is_none());
        self.log_target = Some(target.clone());
        self.modal = Modal::Follow(Box::new(view));
        if target.follow && !target.previous && !read.head {
            self.send(Task::Follow(
                target,
                generation,
                self.options.log_seconds,
                read,
            ));
        } else {
            self.send(Task::Logs(target, generation, read));
        }
    }
    fn reload_logs(
        &mut self,
        mut view: Box<crate::ui_logs::LogView>,
        read: crate::logs::ReadOptions,
    ) {
        let Some(target) = self.log_target.clone() else {
            self.modal = Modal::Follow(view);
            return;
        };
        if self.busy {
            self.status = "Wait for the current log request before changing range.".into();
            self.modal = Modal::Follow(view);
            return;
        }
        let generation = self.worker.logs.cancel();
        view.replace_source("Connecting to logs...".into());
        view.following = target.follow && !target.previous && !read.head;
        self.modal = Modal::Follow(view);
        if target.follow && !target.previous && !read.head {
            self.send(Task::Follow(
                target,
                generation,
                self.options.log_seconds,
                read,
            ));
        } else {
            self.send(Task::Logs(target, generation, read));
        }
    }
    fn request_helm(&mut self, request: helm_ui::Request, preserve_current: bool) {
        if self.busy {
            self.status = "Wait for the current request before reading Helm releases.".into();
            return;
        }
        if !self.options.connection.demo && !self.options.connection.allow_helm {
            self.status="Helm reads require --allow-helm. Stored release metadata and explicit values may contain confidential data.".into();
            return;
        }
        if preserve_current {
            let current = std::mem::replace(&mut self.modal, Modal::None);
            if !matches!(current, Modal::None) {
                self.push_subview(current);
            }
        }
        self.helm_generation = self.helm_generation.wrapping_add(1);
        self.modal = Modal::HelmLoading(request.label());
        self.send(Task::Helm(request, self.helm_generation));
    }
    fn begin_helm_form(&mut self, intent: helm_ui::Intent) {
        if self.busy {
            self.status = "Wait for the current request before preparing Helm lifecycle.".into();
            return;
        }
        if !self.options.connection.writable
            || !self.options.connection.allow_helm
            || !self.options.connection.allow_helm_write
        {
            self.status="Helm lifecycle requires --write --allow-helm --allow-helm-write and explicit --helm-tool for live execution.".into();
            return;
        }
        if self.options.helm_tool.is_none() && !self.options.connection.demo {
            self.status="Choose the trusted native Helm executable explicitly with --helm-tool ABSOLUTE_PATH.".into();
            return;
        }
        let table = match &self.modal {
            Modal::Helm(table) => Some(table.as_ref()),
            _ => self.subviews.iter().rev().find_map(|modal| {
                if let Modal::Helm(table) = modal {
                    Some(table.as_ref())
                } else {
                    None
                }
            }),
        };
        let (namespace, storage) = table
            .map(|table| {
                (
                    table.namespace.clone(),
                    table.storage.or(self.options.helm_storage),
                )
            })
            .unwrap_or_else(|| (self.namespace.clone(), self.options.helm_storage));
        if intent.is_crd() && !self.options.connection.allow_helm_crds {
            self.status="Chart CRD actions require explicit --allow-helm-crds and a separate complete review.".into();
            return;
        }
        if matches!(intent, helm_ui::Intent::Test(_)) && !self.options.connection.allow_helm_hooks {
            self.status =
                "Helm tests require explicit --allow-helm-hooks and a complete operation review."
                    .into();
            return;
        }
        let mut form = helm_ui::Form::new(
            intent,
            namespace,
            storage.unwrap_or(crate::helm::Storage::Secret),
            self.options.helm_values.as_deref(),
            self.options.connection.demo,
        );
        form.set_lifecycle_policy(
            self.options.connection.allow_helm_hooks,
            self.options.connection.allow_helm_crds,
            crate::helm_oci::Options {
                ca_file: self.options.helm_registry_ca.clone(),
                auth_file: self.options.helm_registry_auth.clone(),
            },
        );
        form.set_keyring(self.options.helm_keyring.clone());
        form.set_sigstore_policy(self.options.helm_sigstore_policy.clone());
        self.status = "Helm draft; no execution requested.".into();
        self.sticky_status = false;
        let current = std::mem::replace(&mut self.modal, Modal::None);
        if !matches!(current, Modal::None) {
            self.push_subview(current);
        }
        self.modal = Modal::HelmForm(Box::new(form));
    }
    fn cancel_helm(&mut self) {
        if let Some(cancel) = self.helm_cancel.take() {
            let _ = cancel.send(());
        }
        if let Modal::HelmRunning(_, _, cancelled) = &mut self.modal {
            *cancelled = true;
        }
        self.status="Cancellation requested; remote changes may have started. Waiting for reconciled outcome.".into();
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
            "Scale workload",
            "Change workload container image",
            "Restart workload",
            "Delete selected resource",
            "Inspect redacted JSON",
            "Interactive pod shell",
            "Metrics with requests and limits (extra reads)",
            "Edit workload resource budget",
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
            } else if (2..=4).contains(&index) && kind != "pods" && !supports_group_logs(&kind) {
                Some("Pods and supported workloads/services only")
            } else if ((7..=10).contains(&index) || index == 12 || index == 14)
                && !self.options.connection.writable
            {
                Some("READ ONLY; requires --write")
            } else if index == 7
                && !["deployments.apps", "statefulsets.apps"].contains(&kind.as_str())
            {
                Some("Deployments and StatefulSets only")
            } else if index == 9
                && !["deployments.apps", "statefulsets.apps", "daemonsets.apps"]
                    .contains(&kind.as_str())
            {
                Some("Deployments, StatefulSets and DaemonSets only")
            } else if (index == 8 || index == 14) && !crate::edit::supports_resource(&kind) {
                Some("Supported workloads only")
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
            Action::Scale(_) => {
                ["deployments.apps", "statefulsets.apps"].contains(&self.kind().as_str())
            }
            Action::Restart => ["deployments.apps", "statefulsets.apps", "daemonsets.apps"]
                .contains(&self.kind().as_str()),
            Action::SetContainer { .. } | Action::SetImage { .. } | Action::SetResources { .. } => {
                crate::edit::supports_resource(&self.kind())
            }
            Action::Delete => matches!(
                self.kind().as_str(),
                "pods"
                    | "deployments.apps"
                    | "statefulsets.apps"
                    | "daemonsets.apps"
                    | "services"
                    | "jobs.batch"
                    | "cronjobs.batch"
            ),
            Action::Kill => self.kind() == "pods",
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
        self.key_with_approval_size(code, terminal::size)
    }
    fn key_with_approval_size(
        &mut self,
        code: KeyCode,
        approval_size: impl FnOnce() -> io::Result<(u16, u16)>,
    ) -> Result<bool> {
        if matches!(self.modal, Modal::AdviceRunning) {
            self.status = "Copilot request running; Ctrl-C cancels and waits for cleanup.".into();
            return Ok(false);
        }
        if matches!(self.modal, Modal::HelmRunning(..)) {
            self.status="Helm is running asynchronously. Ctrl-C requests cancellation; wait for the final outcome.".into();
            return Ok(false);
        }
        if self.write_inflight {
            self.status =
                "Operation in progress; wait for the result before navigating or quitting.".into();
            return Ok(false);
        }
        if code == KeyCode::Char(':')
            && match &self.modal {
                Modal::None
                | Modal::Detail(..)
                | Modal::Overview(..)
                | Modal::PodContainers(..)
                | Modal::Metrics(..)
                | Modal::LiveOverview(..)
                | Modal::Directory(..)
                | Modal::ForwardView
                | Modal::HelmDocument(..)
                | Modal::AdviceDocument(..)
                | Modal::Help(..) => true,
                Modal::Follow(view) => !view.is_filtering(),
                Modal::Helm(table) => !table.is_filtering(),
                _ => false,
            }
        {
            let modal = std::mem::replace(&mut self.modal, Modal::None);
            if !matches!(modal, Modal::None) {
                self.push_subview(modal);
            }
            self.command_cursor = 0;
            self.history_index = None;
            self.completion = None;
            self.modal = Modal::Input("Command", String::new());
            return Ok(false);
        }
        self.sticky_status = false;
        let modal = std::mem::replace(&mut self.modal, Modal::None);
        match modal {
            Modal::YamlEditor(mut editor) => {
                match editor.key(crossterm::event::KeyEvent::new(code, KeyModifiers::NONE)) {
                    Ok(crate::ui_yaml::Event::Cancel) => self.close_subview(),
                    Ok(crate::ui_yaml::Event::Review) => self.send(Task::YamlPrepare(editor)),
                    Ok(crate::ui_yaml::Event::Continue | crate::ui_yaml::Event::External) => {
                        self.modal = Modal::YamlEditor(editor)
                    }
                    Err(error) => {
                        self.status = error;
                        self.modal = Modal::YamlEditor(editor);
                    }
                }
            }
            Modal::WriteRunning => self.modal = Modal::WriteRunning,
            Modal::WriteReview(mut review) => match code {
                KeyCode::Esc => self.close_subview(),
                KeyCode::Enter => {
                    let (width, height) =
                        approval_size().map_err(|_| "cannot read terminal size for approval")?;
                    if !self.busy && review.view.ready(width, height, self.epoch) {
                        self.modal = Modal::WriteRunning;
                        let task = match review.plan {
                            WritePlan::Yaml(prepared) => {
                                Task::YamlExecute(prepared, review.view.typed)
                            }
                            WritePlan::Node(prepared) => {
                                Task::NodeExecute(prepared, review.view.typed)
                            }
                        };
                        self.send(task);
                        if !self.write_inflight {
                            self.modal = Modal::Detail(
                                "Operation not queued. Prepare and review again.".into(),
                                0,
                            );
                        }
                    } else {
                        self.status =
                            "Read every review line and type the exact target before applying."
                                .into();
                        self.modal = Modal::WriteReview(review);
                    }
                }
                _ => {
                    let (width, height) = terminal::size().unwrap_or((80, 24));
                    review.view.key(
                        code,
                        usize::from(width.saturating_sub(2)),
                        usize::from(height.saturating_sub(8)),
                    );
                    self.modal = Modal::WriteReview(review);
                }
            },
            Modal::Export(draft) => match code {
                KeyCode::Esc => self.close_subview(),
                KeyCode::Enter => {
                    let (width, height) =
                        approval_size().map_err(|_| "cannot read terminal size for approval")?;
                    let review = safety::review_display(&draft.review());
                    if let Err(error) = review {
                        self.status = error;
                        self.modal = Modal::Export(draft);
                    } else if width < 28
                        || height < 10
                        || wrap_text(
                            &review.unwrap_or_default(),
                            usize::from(width.saturating_sub(2)),
                        )
                        .lines()
                        .count()
                            > usize::from(height.saturating_sub(8))
                    {
                        self.status="Enlarge the terminal to review the full export destination before saving.".into();
                        self.modal = Modal::Export(draft);
                    } else {
                        self.status = match draft.save() {
                            Ok(path) => format!(
                                "Saved {}",
                                path.display()
                                    .to_string()
                                    .chars()
                                    .flat_map(char::escape_default)
                                    .collect::<String>()
                            ),
                            Err(error) => error,
                        };
                        self.modal =
                            Modal::AdviceDocument("EXPORT RESULT".into(), self.status.clone(), 0);
                    }
                }
                _ => self.modal = Modal::Export(draft),
            },
            Modal::AdviceReview(mut review) => match code {
                KeyCode::Esc => self.close_subview(),
                KeyCode::Enter => {
                    let (width, height) =
                        approval_size().map_err(|_| "cannot read terminal size for approval")?;
                    if self.busy || !review.can_send(width, height, self.epoch) {
                        self.status="Read every preview line and type the exact transfer confirmation. Local previews cannot send.".into();
                        self.modal = Modal::AdviceReview(review);
                    } else if let Some(prepared) = review.prepared.take() {
                        let (cancel, receiver) = tokio::sync::oneshot::channel();
                        self.advice_cancel = Some(cancel);
                        self.modal = Modal::AdviceRunning;
                        self.send(Task::AdviceExecute(
                            Box::new(prepared),
                            review.typed,
                            receiver,
                        ));
                        if !self.write_inflight {
                            self.advice_cancel = None;
                            self.modal = Modal::AdviceDocument(
                                "COPILOT NOT STARTED".into(),
                                "Request queue unavailable; prepare and review again.".into(),
                                0,
                            );
                        }
                    }
                }
                _ => {
                    let (width, height) = terminal::size().unwrap_or((80, 24));
                    review.key(
                        code,
                        usize::from(width.saturating_sub(2)),
                        usize::from(height.saturating_sub(8)),
                    );
                    self.modal = Modal::AdviceReview(review);
                }
            },
            Modal::AdviceLoading => {
                if !matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                    self.modal = Modal::AdviceLoading;
                }
            }
            Modal::AdviceRunning => self.modal = Modal::AdviceRunning,
            Modal::AdviceDocument(title, text, offset) => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                    self.close_subview();
                } else {
                    let offset = scroll_offset(&text, offset, code);
                    self.modal = Modal::AdviceDocument(title, text, offset);
                }
            }
            Modal::HelmForm(mut form) => match code {
                KeyCode::Esc => self.close_subview(),
                KeyCode::Enter => {
                    let tool = self.options.helm_tool.clone().or_else(|| {
                        self.options
                            .connection
                            .demo
                            .then(|| std::path::PathBuf::from("/demo/helm"))
                    });
                    if self.busy {
                        self.status = "Wait for the current request; Helm draft retained.".into();
                        self.modal = Modal::HelmForm(form);
                    } else if let Some(tool) = tool {
                        match form.draft(tool, self.options.helm_timeout_seconds) {
                            Ok(draft) => {
                                self.helm_return =
                                    Some((draft.options.namespace.clone(), draft.options.storage));
                                self.push_subview(Modal::HelmForm(form));
                                self.helm_generation = self.helm_generation.wrapping_add(1);
                                self.modal = Modal::HelmLoading(
                                    "Validating local files and preparing sealed Helm review...",
                                );
                                self.status =
                                    "Preparing Helm review; no execution requested.".into();
                                self.send(Task::HelmPrepare(Box::new(draft), self.helm_generation));
                            }
                            Err(error) => {
                                self.status = error;
                                self.modal = Modal::HelmForm(form);
                            }
                        }
                    } else {
                        self.status =
                            "Helm tool path missing; restart with --helm-tool ABSOLUTE_PATH."
                                .into();
                        self.modal = Modal::HelmForm(form);
                    }
                }
                _ => {
                    form.key(code);
                    self.modal = Modal::HelmForm(form);
                }
            },
            Modal::HelmReview(mut review) => match code {
                KeyCode::Esc => {
                    self.helm_generation = self.helm_generation.wrapping_add(1);
                    self.close_subview();
                }
                KeyCode::Enter => {
                    let (width, height) =
                        approval_size().map_err(|_| "cannot read terminal size for approval")?;
                    if self.busy || !review.can_execute(width, height, self.epoch) {
                        self.status="Read every review line (PgUp/PgDown), keep this session, and type the exact confirmation shown below.".into();
                        self.modal = Modal::HelmReview(review);
                    } else {
                        let confirmation = review.prepared.confirmation();
                        let (cancel, receiver) = tokio::sync::oneshot::channel();
                        self.helm_cancel = Some(cancel);
                        self.helm_elapsed = 0;
                        self.modal = Modal::HelmRunning(confirmation, Instant::now(), false);
                        self.send(Task::HelmExecute(
                            review.prepared,
                            review.typed,
                            self.helm_generation,
                            receiver,
                        ));
                        if !self.write_inflight {
                            self.helm_cancel = None;
                            self.modal=Modal::HelmOutcome("HELM NOT STARTED".into(),"The command queue is unavailable; no Helm execution was queued. Prepare and review again.".into(),0);
                        } else {
                            self.status =
                                "Helm execution in progress; Ctrl-C requests cancellation.".into();
                        }
                    }
                }
                _ => {
                    let (width, height) = terminal::size().unwrap_or((80, 24));
                    review.key(
                        code,
                        usize::from(width.saturating_sub(2)),
                        usize::from(height.saturating_sub(8)),
                    );
                    self.modal = Modal::HelmReview(review);
                }
            },
            Modal::HelmRunning(target, started, cancelled) => {
                self.modal = Modal::HelmRunning(target, started, cancelled)
            }
            Modal::HelmOutcome(title, text, offset) => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                    if let Some((namespace, storage)) = self.helm_return.take() {
                        self.request_helm(helm_ui::Request::List(namespace, Some(storage)), false);
                    } else {
                        self.close_subview();
                    }
                } else {
                    let offset = scroll_offset(&text, offset, code);
                    self.modal = Modal::HelmOutcome(title, text, offset);
                }
            }
            Modal::HelmLoading(label) => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                    self.helm_generation = self.helm_generation.wrapping_add(1);
                    self.close_subview();
                } else {
                    self.modal = Modal::HelmLoading(label);
                }
            }
            Modal::Helm(mut table) => match table.key(code) {
                helm_ui::Action::Close => {
                    self.helm_generation = self.helm_generation.wrapping_add(1);
                    self.close_subview();
                }
                helm_ui::Action::Request(request) => {
                    self.modal = Modal::Helm(table);
                    self.request_helm(request, true);
                }
                helm_ui::Action::Lifecycle(intent) => {
                    self.modal = Modal::Helm(table);
                    self.begin_helm_form(*intent);
                }
                helm_ui::Action::Metadata(release) => {
                    let text = crate::inspect::yaml(
                        &serde_json::to_value(release)
                            .map_err(|_| "Helm metadata encoding failed")?,
                    )?;
                    self.push_subview(Modal::Helm(table));
                    self.modal =
                        Modal::HelmDocument("HELM SAFE REVISION METADATA / YAML".into(), text, 0);
                }
                helm_ui::Action::Unavailable(reason) => {
                    self.status = reason.into();
                    self.modal = Modal::Helm(table);
                }
                helm_ui::Action::None => self.modal = Modal::Helm(table),
            },
            Modal::HelmDocument(title, text, offset) => {
                if matches!(code, KeyCode::Esc | KeyCode::Char('q')) {
                    self.close_subview();
                } else {
                    let offset = scroll_offset(&text, offset, code);
                    self.modal = Modal::HelmDocument(title, text, offset);
                }
            }
            Modal::ForwardView => match code {
                KeyCode::Esc | KeyCode::Char('q') => self.close_subview(),
                KeyCode::Down | KeyCode::Char('j') => {
                    self.forward_selected = (self.forward_selected + 1)
                        .min(self.worker.forward.len().saturating_sub(1));
                    self.modal = Modal::ForwardView;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.forward_selected = self.forward_selected.saturating_sub(1);
                    self.modal = Modal::ForwardView;
                }
                KeyCode::Char('s') => {
                    self.status = if self.worker.forward.cancel_selected(self.forward_selected) {
                        "Stopping selected forward; waiting for listener and client cleanup."
                    } else {
                        "Selected forward is already stopped or stopping."
                    }
                    .into();
                    self.modal = Modal::ForwardView;
                }
                KeyCode::Char('F') => {
                    self.modal = Modal::ForwardView;
                    self.request_forward();
                }
                _ => self.modal = Modal::ForwardView,
            },
            Modal::ForwardForm(namespace, pod, uid, mut fields, mut index) => match code {
                KeyCode::Esc => {}
                KeyCode::Tab | KeyCode::Down => {
                    index = (index + 1) % 3;
                    self.modal = Modal::ForwardForm(namespace, pod, uid, fields, index);
                }
                KeyCode::BackTab | KeyCode::Up => {
                    index = if index == 0 { 2 } else { index - 1 };
                    self.modal = Modal::ForwardForm(namespace, pod, uid, fields, index);
                }
                KeyCode::Backspace => {
                    fields[index].pop();
                    self.modal = Modal::ForwardForm(namespace, pod, uid, fields, index);
                }
                KeyCode::Char(c) if c.is_ascii_digit() && fields[index].len() < 5 => {
                    fields[index].push(c);
                    self.modal = Modal::ForwardForm(namespace, pod, uid, fields, index);
                }
                KeyCode::Enter => {
                    let parsed = fields
                        .iter()
                        .map(|field| field.parse::<u16>())
                        .collect::<std::result::Result<Vec<_>, _>>();
                    if self.busy {
                        self.status =
                            "Wait for the current request; forwarding draft retained.".into();
                        self.modal = Modal::ForwardForm(namespace, pod, uid, fields, index);
                    } else if let Ok(ports) = parsed
                        && ports[0] >= 1024
                        && ports[1] > 0
                        && (1..=300).contains(&ports[2])
                        && uid.is_some()
                    {
                        self.send(Task::PrepareSession(
                            namespace,
                            pod,
                            uid,
                            crate::ops::Request::PortForward {
                                local_port: ports[0],
                                remote_port: ports[1],
                                seconds: ports[2],
                            },
                        ));
                    } else {
                        self.status="Use local port 1024..65535, remote port 1..65535 and duration 1..300s; Pod UID required.".into();
                        self.modal = Modal::ForwardForm(namespace, pod, uid, fields, index);
                    }
                }
                _ => self.modal = Modal::ForwardForm(namespace, pod, uid, fields, index),
            },
            Modal::Subjects(mut view) => match view.key(code) {
                crate::ui_subjects::Outcome::Close => self.close_subview(),
                crate::ui_subjects::Outcome::Stay => self.modal = Modal::Subjects(view),
                crate::ui_subjects::Outcome::Open(subject) => {
                    let namespace = view.namespace.clone();
                    self.push_subview(Modal::Subjects(view));
                    self.send(Task::Can(namespace, subject));
                }
            },
            Modal::Secrets(mut view) => match view.key(code) {
                crate::ui_secrets::Outcome::Close => self.close_subview(),
                crate::ui_secrets::Outcome::Stay => self.modal = Modal::Secrets(view),
                crate::ui_secrets::Outcome::Reveal(namespace, name, uid, key) => {
                    self.modal = Modal::Secrets(view);
                    self.send(Task::SecretValue(namespace, name, uid, key));
                }
            },
            Modal::Directory(directory, mut index) => match code {
                KeyCode::Esc | KeyCode::Char('q') => self.close_subview(),
                KeyCode::Down | KeyCode::Char('j') => {
                    index = (index + 1).min(directory.entries.len().saturating_sub(1));
                    self.modal = Modal::Directory(directory, index);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal = Modal::Directory(directory, index.saturating_sub(1))
                }
                KeyCode::Enter | KeyCode::Char('y' | 'v') => {
                    if let Some(entry) = directory.entries.get(index) {
                        let path = entry.path.clone();
                        let is_directory = entry.directory;
                        self.push_subview(Modal::Directory(directory, index));
                        self.send(if is_directory {
                            Task::Directory(path)
                        } else {
                            Task::Manifest(path)
                        });
                    }
                }
                _ => self.modal = Modal::Directory(directory, index),
            },
            Modal::PodContainers(object, mut index) => {
                let names = pod_log_choices(&object);
                match code {
                    KeyCode::Char('s' | 'a') => {
                        if names.get(index).is_some_and(|name| name == "*") {
                            self.status = "Choose a specific container for shell or attach.".into();
                            self.modal = Modal::PodContainers(object, index);
                            return Ok(false);
                        }
                        let attach = code == KeyCode::Char('a');
                        if self.busy
                            || !self.options.connection.writable
                            || !(if attach {
                                self.options.connection.allow_attach
                            } else {
                                self.options.connection.allow_exec
                            })
                            || self.options.connection.demo
                        {
                            self.status = if attach {
                                "Attach requires a live Pod, --write and --allow-attach."
                            } else {
                                "Shell requires a live Pod, --write and --allow-exec."
                            }
                            .into();
                            self.modal = Modal::PodContainers(object, index);
                        } else if let Some(container) = names.get(index) {
                            let namespace = namespace(&object).to_owned();
                            let pod = name(&object).to_owned();
                            let uid = object["metadata"]["uid"].as_str().map(str::to_owned);
                            let request = if attach {
                                crate::ops::Request::InteractiveAttach {
                                    container: container.clone(),
                                    seconds: self.options.session_seconds,
                                }
                            } else {
                                crate::ops::Request::InteractiveExec {
                                    container: container.clone(),
                                    argv: vec!["/bin/sh".into(), "-i".into()],
                                    seconds: self.options.session_seconds,
                                }
                            };
                            self.push_subview(Modal::PodContainers(object, index));
                            self.send(Task::PrepareSession(namespace, pod, uid, request));
                        }
                    }
                    KeyCode::Char('f') => {
                        self.push_subview(Modal::PodContainers(object, index));
                        self.modal = Modal::ForwardView;
                    }
                    KeyCode::Char('F') => {
                        if self.busy
                            || !self.options.connection.writable
                            || !self.options.connection.allow_port_forward
                            || self.options.connection.demo
                        {
                            self.status =
                                "Port-forward requires a live Pod, --write and --allow-forward."
                                    .into();
                            self.modal = Modal::PodContainers(object, index);
                        } else {
                            let namespace = namespace(&object).to_owned();
                            let pod = name(&object).to_owned();
                            let uid = object["metadata"]["uid"].as_str().map(str::to_owned);
                            self.push_subview(Modal::PodContainers(object, index));
                            self.modal = Modal::ForwardForm(
                                namespace,
                                pod,
                                uid,
                                [
                                    self.worker.forward.suggested_port().to_string(),
                                    "80".into(),
                                    "300".into(),
                                ],
                                0,
                            );
                        }
                    }
                    KeyCode::Esc | KeyCode::Char('q') => self.close_subview(),
                    KeyCode::Down | KeyCode::Char('j') => {
                        index = (index + 1).min(names.len().saturating_sub(1));
                        self.modal = Modal::PodContainers(object, index);
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        self.modal = Modal::PodContainers(object, index.saturating_sub(1))
                    }
                    KeyCode::Enter | KeyCode::Char('l') | KeyCode::Char('L' | 'p') => {
                        if !self.options.connection.logs {
                            self.status = "Logs require --allow-logs.".into();
                            self.modal = Modal::PodContainers(object, index);
                        } else if let Some(container) = names.get(index) {
                            let n = namespace(&object).to_owned();
                            let pod = name(&object).to_owned();
                            let container = container.clone();
                            self.push_subview(Modal::PodContainers(object, index));
                            let uid = self.subviews.last().and_then(|modal| {
                                if let Modal::PodContainers(object, _) = modal {
                                    object["metadata"]["uid"].as_str().map(str::to_owned)
                                } else {
                                    None
                                }
                            });
                            if let Some(uid) = uid {
                                self.open_logs(
                                    LogTarget {
                                        namespace: n,
                                        pod,
                                        all: container == "*",
                                        dynamic: false,
                                        membership: None,
                                        container: if container == "*" {
                                            self.subviews.last().and_then(|modal| {
                                                if let Modal::PodContainers(object, _) = modal {
                                                    ordinary_container_names(object)
                                                        .into_iter()
                                                        .next()
                                                } else {
                                                    None
                                                }
                                            })
                                        } else {
                                            Some(container)
                                        },
                                        uid,
                                        previous: matches!(code, KeyCode::Char('L' | 'p')),
                                        follow: !matches!(code, KeyCode::Char('L' | 'p')),
                                    },
                                    self.options.log_read,
                                );
                            } else {
                                self.status =
                                    "Pod UID unavailable; refresh before reading logs.".into();
                                self.close_subview();
                            }
                        }
                    }
                    KeyCode::Char('d') => {
                        let text = overview_text(&object, &self.context);
                        self.push_subview(Modal::PodContainers(object, index));
                        self.modal = Modal::Detail(text, 0);
                    }
                    KeyCode::Char('y' | 'v') => {
                        let text = crate::inspect::yaml(&object)?;
                        self.push_subview(Modal::PodContainers(object, index));
                        self.modal = Modal::Detail(text, 0);
                    }
                    _ => self.modal = Modal::PodContainers(object, index),
                }
            }
            Modal::EditContainers(k, n, name, uid, mut values, mut index) => match code {
                KeyCode::Esc => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    index = (index + 1).min(values.len().saturating_sub(1));
                    self.modal = Modal::EditContainers(k, n, name, uid, values, index);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    index = index.saturating_sub(1);
                    self.modal = Modal::EditContainers(k, n, name, uid, values, index);
                }
                KeyCode::Enter if index < values.len() => {
                    let value = values.remove(index);
                    self.modal = Modal::Editor(Box::new(ContainerDraft {
                        kind: k,
                        namespace: n,
                        name,
                        uid,
                        container: value.name,
                        fields: [
                            value.image,
                            value.resources.cpu_request,
                            value.resources.memory_request,
                            value.resources.cpu_limit,
                            value.resources.memory_limit,
                        ],
                        active: 0,
                        resources: false,
                    }));
                }
                _ => self.modal = Modal::EditContainers(k, n, name, uid, values, index),
            },
            Modal::Editor(mut draft) => match code {
                KeyCode::Esc => {}
                KeyCode::F(2) => {
                    draft.resources = !draft.resources;
                    if !draft.resources {
                        draft.active = 0;
                    }
                    self.modal = Modal::Editor(draft);
                }
                KeyCode::Tab | KeyCode::Down => {
                    draft.active = (draft.active + 1) % if draft.resources { 5 } else { 1 };
                    self.modal = Modal::Editor(draft);
                }
                KeyCode::BackTab | KeyCode::Up => {
                    draft.active = if draft.active == 0 {
                        if draft.resources { 4 } else { 0 }
                    } else {
                        draft.active - 1
                    };
                    self.modal = Modal::Editor(draft);
                }
                KeyCode::Backspace => {
                    draft.fields[draft.active].pop();
                    self.modal = Modal::Editor(draft);
                }
                KeyCode::Char(c)
                    if c.is_ascii_graphic()
                        && draft.fields[draft.active].len()
                            < if draft.active == 0 { 512 } else { 128 } =>
                {
                    draft.fields[draft.active].push(c);
                    self.modal = Modal::Editor(draft);
                }
                KeyCode::Enter => {
                    let edit = crate::edit::ContainerEdit {
                        image: draft.fields[0].clone(),
                        resources: draft.resources.then(|| crate::edit::ResourceEdit {
                            cpu_request: draft.fields[1].clone(),
                            memory_request: draft.fields[2].clone(),
                            cpu_limit: draft.fields[3].clone(),
                            memory_limit: draft.fields[4].clone(),
                        }),
                    };
                    if self.busy {
                        self.status = "Busy; draft retained. Wait before preparing.".into();
                        self.modal = Modal::Editor(draft);
                    } else if let Err(error) = edit.validate(&draft.container) {
                        self.status = error;
                        self.modal = Modal::Editor(draft);
                    } else {
                        self.send(Task::Prepare(
                            draft.kind,
                            draft.namespace,
                            draft.name,
                            draft.uid,
                            Action::SetContainer {
                                container: draft.container,
                                edit,
                            },
                        ));
                    }
                }
                _ => self.modal = Modal::Editor(draft),
            },
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
                KeyCode::Enter => {
                    if let Some(selector) = text.strip_prefix("-l ") {
                        self.push_view();
                        self.selector = selector.trim().into();
                        self.filter.clear();
                        self.changed();
                    } else if let Err(error) = nav::Filter::parse(&text) {
                        self.status = error;
                        self.modal = Modal::Filter(original, text, original_uid, original_index);
                    }
                }
                KeyCode::Backspace => {
                    text.pop();
                    self.filter = text.clone();
                    self.selected = 0;
                    self.modal = Modal::Filter(original, text, original_uid, original_index);
                }
                KeyCode::Char(c) if !c.is_control() && text.len() + c.len_utf8() <= 512 => {
                    text.push(c);
                    self.filter = text.clone();
                    self.selected = 0;
                    self.modal = Modal::Filter(original, text, original_uid, original_index);
                }
                _ => self.modal = Modal::Filter(original, text, original_uid, original_index),
            },
            Modal::Follow(mut view) => {
                use crate::ui_logs::LogAction;
                if self.busy && !view.is_filtering() && matches!(code, KeyCode::Char('0'..='6')) {
                    self.status = "Wait for the current log request before changing range.".into();
                    self.modal = Modal::Follow(view);
                    return Ok(false);
                }
                match view.key(code) {
                    LogAction::Close => {
                        self.worker.logs.cancel();
                        self.log_target = None;
                        self.close_subview();
                    }
                    LogAction::Reload(read) => self.reload_logs(view, read),
                    LogAction::AllContainers => {
                        if self.busy {
                            self.status =
                                "Wait for the current request before changing log sources.".into();
                            self.modal = Modal::Follow(view);
                        } else if let Some(target) = &mut self.log_target {
                            if target.all && target.container.is_none() {
                                self.status="No preferred single container; choose one from the Pod container view.".into();
                                self.modal = Modal::Follow(view);
                            } else {
                                target.all = !target.all;
                                view.replace_source("Connecting to selected log sources...".into());
                                let read = view.range;
                                self.reload_logs(view, read);
                            }
                        } else {
                            self.modal = Modal::Follow(view);
                        }
                    }
                    LogAction::Clear => {
                        if let Ok(mut state) = self.worker.logs.state.lock() {
                            state.buffer = crate::logs::RollingLog::default();
                            state.dirty = false;
                        }
                        self.modal = Modal::Follow(view);
                    }
                    LogAction::Mark => {
                        if view.following {
                            let generation = self
                                .worker
                                .logs
                                .generation
                                .load(std::sync::atomic::Ordering::SeqCst);
                            self.worker.logs.append(
                                generation,
                                "[---------------- KBorg log mark ----------------]",
                            );
                        }
                        self.modal = Modal::Follow(view);
                    }
                    LogAction::None => self.modal = Modal::Follow(view),
                }
            }
            Modal::Containers(target, names, mut index) => match code {
                KeyCode::Esc => {
                    self.worker.logs.cancel();
                    self.close_subview();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    index = (index + 1).min(names.len().saturating_sub(1));
                    self.modal = Modal::Containers(target, names, index);
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal = Modal::Containers(target, names, index.saturating_sub(1))
                }
                KeyCode::Enter => {
                    if let Some(container) = names.get(index) {
                        let mut target = target;
                        target.all = container == "*";
                        if !target.all {
                            target.container = Some(container.clone());
                        }
                        self.open_logs(target, self.options.log_read);
                    }
                }
                _ => self.modal = Modal::Containers(target, names, index),
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
            Modal::Input("Command", mut text) => {
                self.command_cursor = self.command_cursor.min(text.len());
                match code {
                    KeyCode::Esc => {
                        self.completion = None;
                        self.history_index = None;
                        self.close_subview();
                    }
                    KeyCode::Enter => {
                        self.remember_command(&text);
                        return self.command(&text);
                    }
                    KeyCode::Up => {
                        if !self.history.is_empty() {
                            let index = self
                                .history_index
                                .map(|i| i.saturating_sub(1))
                                .unwrap_or_else(|| {
                                    self.history_draft = text.clone();
                                    self.history.len() - 1
                                });
                            self.history_index = Some(index);
                            text = self.history[index].clone();
                            self.command_cursor = text.len();
                            self.completion = None;
                        }
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Down => {
                        if let Some(index) = self.history_index {
                            if index + 1 < self.history.len() {
                                self.history_index = Some(index + 1);
                                text = self.history[index + 1].clone();
                            } else {
                                self.history_index = None;
                                text = self.history_draft.clone();
                            }
                            self.command_cursor = text.len();
                            self.completion = None;
                        }
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Tab | KeyCode::BackTab => {
                        let (base, values, index) = self.completion.take().unwrap_or_else(|| {
                            (
                                text.clone(),
                                self.suggestions(&text),
                                if code == KeyCode::BackTab {
                                    usize::MAX
                                } else {
                                    0
                                },
                            )
                        });
                        if !values.is_empty() {
                            let index = if index == usize::MAX {
                                values.len() - 1
                            } else {
                                index % values.len()
                            };
                            text = values[index].clone();
                            self.command_cursor = text.len();
                            self.completion = Some((
                                base,
                                values.clone(),
                                if code == KeyCode::BackTab {
                                    index.checked_sub(1).unwrap_or(values.len() - 1)
                                } else {
                                    (index + 1) % values.len()
                                },
                            ));
                        }
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Left => {
                        self.command_cursor = text[..self.command_cursor]
                            .char_indices()
                            .next_back()
                            .map(|(i, _)| i)
                            .unwrap_or(0);
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Right => {
                        self.command_cursor = text[self.command_cursor..]
                            .chars()
                            .next()
                            .map(|c| self.command_cursor + c.len_utf8())
                            .unwrap_or(text.len());
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Home => {
                        self.command_cursor = 0;
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::End => {
                        self.command_cursor = text.len();
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Backspace => {
                        if let Some((previous, _)) =
                            text[..self.command_cursor].char_indices().next_back()
                        {
                            text.drain(previous..self.command_cursor);
                            self.command_cursor = previous;
                        }
                        self.completion = None;
                        self.history_index = None;
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Delete => {
                        if let Some(c) = text[self.command_cursor..].chars().next() {
                            text.drain(self.command_cursor..self.command_cursor + c.len_utf8());
                        }
                        self.completion = None;
                        self.modal = Modal::Input("Command", text);
                    }
                    KeyCode::Char(c)
                        if !c.is_control() && text.len() + c.len_utf8() <= nav::COMMAND_BYTES =>
                    {
                        text.insert(self.command_cursor, c);
                        self.command_cursor += c.len_utf8();
                        self.completion = None;
                        self.history_index = None;
                        self.modal = Modal::Input("Command", text);
                    }
                    _ => self.modal = Modal::Input("Command", text),
                }
            }
            Modal::Input(label, mut text) => match code {
                KeyCode::Esc => {
                    if matches!(
                        label,
                        selections_ui::SAVE_LABEL | selections_ui::DELETE_LABEL
                    ) {
                        self.selection_ui.draft = None;
                        self.selection_ui.deleting = None;
                        self.restore_selection_view();
                    }
                }
                KeyCode::Enter => {
                    if label == selections_ui::SAVE_LABEL {
                        self.commit_selection_name(&text);
                    } else if label == selections_ui::DELETE_LABEL {
                        self.delete_selection_name(&text);
                    } else if label == "Command" {
                        return self.command(&text);
                    } else if label == "Filter" {
                        self.filter = text;
                        self.selected = 0;
                    } else if label == OVERVIEW_CONTEXTS_LABEL {
                        let names: Vec<String> =
                            text.split(',').map(|name| name.trim().to_owned()).collect();
                        if names.len() > crate::overview::MAX_LIVE_CONTEXTS {
                            self.status="The dashboard allows up to 9 explicit contexts; CLI snapshots support up to 100.".into();
                        } else if let Err(error) = crate::overview::validate(&names) {
                            self.status = error;
                        } else {
                            self.open_dashboard(names)?;
                        }
                    } else if label == "Namespace" {
                        if text == "*" || safety::valid_id(&text) {
                            self.namespace = text;
                            self.namespace_override = true;
                            self.desired_resource = None;
                            if let Ok(index) = self.resolve_resource("v1/pods") {
                                self.kind_index = index;
                            }
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
                KeyCode::Esc | KeyCode::Char('q') => self.close_subview(),
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
            Modal::LiveOverview(mut dashboard) => {
                dashboard.notice = None;
                if let Some(picker) = dashboard.picker.as_mut() {
                    match picker.key(code) {
                        dashboard_ui::PickerOutcome::Stay => {}
                        dashboard_ui::PickerOutcome::Cancel => dashboard.picker = None,
                        dashboard_ui::PickerOutcome::Start(chosen) => {
                            dashboard.picker = None;
                            self.start_dashboard_tail(&mut dashboard, chosen);
                        }
                    }
                    self.modal = Modal::LiveOverview(dashboard);
                    return Ok(false);
                }
                if let (Some(pane), KeyCode::Char('e' | 'W')) = (dashboard.logs.as_mut(), code) {
                    pane.view.key(code);
                    self.modal = Modal::LiveOverview(dashboard);
                    return Ok(false);
                }
                match code {
                    KeyCode::Char('?') => dashboard.help = !dashboard.help,
                    KeyCode::Esc if dashboard.help => dashboard.help = false,
                    KeyCode::Esc if dashboard.logs.is_some() => {
                        dashboard.logs = None;
                        self.worker.logs.cancel();
                    }
                    KeyCode::Esc if dashboard.warning.is_some() => dashboard.warning = None,
                    KeyCode::Esc | KeyCode::Char('q') => {
                        if dashboard.logs.is_some() {
                            self.worker.logs.cancel();
                        }
                        self.overview = None;
                        return Ok(false);
                    }
                    KeyCode::Char('L') if !dashboard.help => self.dashboard_logs(&mut dashboard),
                    KeyCode::Char('t') if !dashboard.help => self.dashboard_tail(&mut dashboard),
                    KeyCode::Enter if !dashboard.help => {
                        if dashboard.logs.take().is_some() {
                            self.worker.logs.cancel();
                        }
                        self.open_dashboard_pods(dashboard)?;
                        return Ok(false);
                    }
                    KeyCode::Char('c') if !dashboard.help => {
                        self.overview = None;
                        self.modal = Modal::Input(
                            OVERVIEW_CONTEXTS_LABEL,
                            dashboard
                                .rows
                                .iter()
                                .map(|row| row.context.as_str())
                                .collect::<Vec<_>>()
                                .join(","),
                        );
                        return Ok(false);
                    }
                    KeyCode::Char('s') if !dashboard.help => {
                        self.modal = Modal::LiveOverview(dashboard);
                        self.save_dashboard_selection()?;
                        return Ok(false);
                    }
                    KeyCode::Char('B') if !dashboard.help => {
                        self.modal = Modal::LiveOverview(dashboard);
                        self.open_saved_selections()?;
                        return Ok(false);
                    }
                    _ if !dashboard.help => dashboard.key(code),
                    _ => {}
                }
                self.modal = Modal::LiveOverview(dashboard);
            }
            Modal::SessionContainers(n, name, uid, names, index, attach) => match code {
                KeyCode::Esc => {}
                KeyCode::Down | KeyCode::Char('j') => {
                    self.modal = Modal::SessionContainers(
                        n,
                        name,
                        uid,
                        names.clone(),
                        (index + 1).min(names.len().saturating_sub(1)),
                        attach,
                    )
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.modal = Modal::SessionContainers(
                        n,
                        name,
                        uid,
                        names,
                        index.saturating_sub(1),
                        attach,
                    )
                }
                KeyCode::Enter => {
                    if let Some(container) = names.get(index) {
                        let request = if attach {
                            crate::ops::Request::InteractiveAttach {
                                container: container.clone(),
                                seconds: self.options.session_seconds,
                            }
                        } else {
                            crate::ops::Request::InteractiveExec {
                                container: container.clone(),
                                argv: vec!["/bin/sh".into(), "-i".into()],
                                seconds: self.options.session_seconds,
                            }
                        };
                        self.send(Task::PrepareSession(n, name, uid, request));
                    }
                }
                _ => self.modal = Modal::SessionContainers(n, name, uid, names, index, attach),
            },
            Modal::SessionConfirm(prepared, mut text) => match code {
                KeyCode::Esc => self.status = "Session review canceled.".into(),
                KeyCode::Enter => {
                    let (width, height) = terminal::size().map_err(|_| "terminal size failed")?;
                    if !confirmation_fits(prepared.summary(), &text, width, height) {
                        self.status = "Enlarge terminal to inspect the complete session review before approving.".into();
                        self.modal = Modal::SessionConfirm(prepared, text);
                    } else if text != prepared.confirmation() {
                        self.status = "Type the exact namespace/pod from the review.".into();
                        self.modal = Modal::SessionConfirm(prepared, text);
                    } else {
                        if prepared.is_port_forward() {
                            let generation = self.worker.forward.generation();
                            self.send(Task::RunForward(prepared, text, generation));
                        } else {
                            self.pending_session = Some((prepared, text));
                        }
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
                KeyCode::Esc => {
                    if label == selections_ui::PICKER_LABEL {
                        self.restore_selection_view();
                    }
                }
                KeyCode::Delete | KeyCode::Char('d') if label == selections_ui::PICKER_LABEL => {
                    self.confirm_selection_delete(index);
                }
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
                            match index {
                                2 => self.request_logs(false, false),
                                3 => self.request_logs(true, false),
                                4 => self.request_logs(false, true),
                                6 => return self.command("overview"),
                                7 => return self.scale(),
                                8 => return self.image(),
                                9 => self.prepare(Action::Restart),
                                10 => self.prepare(Action::Delete),
                                _ => {
                                    let key = match index {
                                        0 => KeyCode::Char('d'),
                                        1 => KeyCode::Char('y'),
                                        5 => KeyCode::Char('m'),
                                        11 => KeyCode::Char('J'),
                                        12 => KeyCode::Char('s'),
                                        13 => KeyCode::Char('M'),
                                        14 => KeyCode::Char('V'),
                                        _ => KeyCode::Null,
                                    };
                                    return self.key(key);
                                }
                            }
                        } else if label == "Aliases" {
                            if let Some(resource) = value.split_whitespace().next() {
                                return self.command(resource);
                            }
                        } else if label == selections_ui::PICKER_LABEL {
                            if let Some(selection) = self.selection_ui.records.get(index).cloned()
                                && let Err(error) = self.open_selection(selection)
                            {
                                self.status = error;
                                self.modal = Modal::Picker(label, list, index);
                            }
                        } else if label == "Saved resources" {
                            if let Some(record) = self.saved.get(index) {
                                match crate::screendump::read(&record.id) {
                                    Ok(text) => self.modal = Modal::Detail(text, 0),
                                    Err(error) => self.status = error,
                                };
                            }
                        } else if label.starts_with("Context") {
                            if !self.busy {
                                self.push_view();
                                if label == "Context (Pod view)"
                                    && let Ok(index) = self.resolve_resource("pods")
                                {
                                    self.kind_index = index;
                                    self.desired_resource = None;
                                }
                                self.options.connection.context = Some(value.clone());
                                self.connect();
                            } else {
                                self.status =
                                    "Wait for the current request before changing context.".into();
                            }
                        } else {
                            self.push_view();
                            self.namespace = if value == "all" {
                                "*".into()
                            } else {
                                value.clone()
                            };
                            self.namespace_override = true;
                            self.desired_resource = None;
                            if let Ok(index) = self.resolve_resource("v1/pods") {
                                self.kind_index = index;
                            }
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
                    if !self.filter.is_empty() {
                        let uid = self.selected_uid();
                        self.filter.clear();
                        self.selected = uid
                            .and_then(|uid| {
                                self.visible().iter().position(|object| {
                                    object["metadata"]["uid"].as_str() == Some(&uid)
                                })
                            })
                            .unwrap_or(0);
                    } else if !self.selector.is_empty() {
                        self.selector.clear();
                        self.changed();
                    } else {
                        self.go_back();
                    }
                }
                KeyCode::Char('[') => self.go_back(),
                KeyCode::Char(']') => self.go_forward(),
                KeyCode::Char('-') => self.last_view(),
                KeyCode::Tab | KeyCode::BackTab => {
                    self.command_cursor = 0;
                    self.modal = Modal::Input("Command", String::new());
                }
                KeyCode::PageDown | KeyCode::PageUp | KeyCode::Home | KeyCode::End => {
                    let page = terminal::size()
                        .map(|(w, h)| Layout::new(w, h).table_rows())
                        .unwrap_or(15)
                        .max(1);
                    let len = self.visible().len();
                    self.selected = match code {
                        KeyCode::PageDown => self
                            .selected
                            .saturating_add(page)
                            .min(len.saturating_sub(1)),
                        KeyCode::PageUp => self.selected.saturating_sub(page),
                        KeyCode::Home => 0,
                        _ => len.saturating_sub(1),
                    };
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.selected = (self.selected + 1).min(self.visible().len().saturating_sub(1))
                }
                KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
                KeyCode::Enter => {
                    self.drilldown();
                }
                KeyCode::Char('/') => {
                    self.modal = Modal::Filter(
                        self.filter.clone(),
                        self.filter.clone(),
                        self.selected_uid(),
                        self.selected,
                    )
                }
                KeyCode::Char(':') => {
                    self.history_index = None;
                    self.completion = None;
                    self.command_cursor = 0;
                    self.modal = Modal::Input("Command", String::new());
                }
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
                KeyCode::Char('S') => self.set_sort(Sort::Status),
                KeyCode::Char('N') => self.set_sort(Sort::Name),
                KeyCode::Char('A') => self.set_sort(Sort::Age),
                KeyCode::Char('E') => self.request_shell(),
                KeyCode::Char('n') => {
                    if let Some((_, namespace, _)) = self.selected_target() {
                        self.copy_text(&namespace)?;
                        self.status =
                            "Namespace sent to terminal clipboard (OSC52 support required).".into();
                    }
                }
                KeyCode::Char('c') => {
                    if self.kind() == "nodes" && self.options.connection.writable {
                        self.node_maintenance(crate::node_maintenance::Request::Cordon);
                        return Ok(false);
                    }
                    if let Some(object) = self.visible().get(self.selected) {
                        self.copy_text(name(object))?;
                        self.status =
                            "Name sent to terminal clipboard (OSC52 support required).".into();
                    }
                }
                KeyCode::Char('t') => {
                    self.status = "File transfer is unavailable. Use :themes for appearance.".into()
                }
                KeyCode::Char('b') => self.save_resource_selection()?,
                KeyCode::Char('B') => self.open_saved_selections()?,
                KeyCode::Char('?') => self.modal = Modal::Help(0),
                KeyCode::Char('D') => self.open_dashboard(vec![self.context.clone()])?,
                KeyCode::Char('u') => {
                    self.node_maintenance(crate::node_maintenance::Request::Uncordon)
                }
                KeyCode::Char('z') => self.status =
                    "Inspection is already redacted; additional k9s sanitization is unavailable."
                        .into(),
                KeyCode::Char('r') => {
                    if self.kind() == "nodes" {
                        self.node_maintenance(crate::node_maintenance::Request::Drain {
                            seconds: 120,
                        });
                        return Ok(false);
                    }
                    if crate::edit::supports_resource(&self.kind()) {
                        self.prepare(Action::Restart);
                        return Ok(false);
                    }
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
                KeyCode::Char('R') => self.set_sort(Sort::Ready),
                KeyCode::Char('m') => {
                    self.metrics_resources = false;
                    self.request_metrics();
                }
                KeyCode::Char('M' | 'C') => {
                    if matches!(self.kind().as_str(), "pods" | "nodes") {
                        self.set_sort(if code == KeyCode::Char('C') {
                            Sort::Cpu
                        } else {
                            Sort::Memory
                        });
                        if !self.busy && self.last_table_metrics.is_none() {
                            self.refresh_table_metrics();
                        }
                    } else {
                        self.status="CPU/memory usage sorting applies to Pod and Node tables; :metrics shows exact samples.".into();
                    }
                }
                KeyCode::Char('w') => {
                    if let Some((_, namespace, _)) = self.selected_target() {
                        return self.command(&format!("ns {namespace}"));
                    }
                }
                KeyCode::Char('0') => return self.command("ns all"),
                KeyCode::Char('1'..='9') => {
                    self.status =
                        "Namespace favorites are not configured; :ns opens the namespace picker."
                            .into()
                }
                KeyCode::Char(' ') => {
                    if let Some(uid) = self.selected_uid() {
                        if !self.marks.remove(&uid) {
                            self.marks.insert(uid);
                        }
                        self.selected =
                            (self.selected + 1).min(self.visible().len().saturating_sub(1));
                        self.status = format!(
                            "{} marked | mutations still review one selected UID",
                            self.marks.len()
                        );
                    }
                }
                KeyCode::Char('a') => self.request_session(true),
                KeyCode::Char('y') | KeyCode::Char('v') => {
                    if let Some((k, n, name)) = self.selected_target() {
                        self.send(Task::Yaml(k, n, name));
                    }
                }
                KeyCode::Char('o') => {
                    if self.kind() != "pods" {
                        self.status = "Show Node applies to core Pods.".into();
                        return Ok(false);
                    }
                    let node = self
                        .visible()
                        .get(self.selected)
                        .and_then(|object| object["spec"]["nodeName"].as_str())
                        .map(str::to_owned);
                    if let Some(node) = node {
                        return self.command(&format!("v1/nodes /^{node}$"));
                    }
                    self.status =
                        "Show Node requires a scheduled Pod. :overview opens multiple contexts."
                            .into();
                }
                KeyCode::Char('e') => self.yaml_editor(),
                KeyCode::Char('V') => {
                    if !self.options.connection.writable {
                        self.status = "READ ONLY: resource budgets require --write.".into();
                    } else if !crate::edit::supports_resource(&self.kind()) {
                        self.status =
                            "Resource budgets apply to supported workload containers.".into();
                    } else if let Some((k, n, name)) = self.selected_target() {
                        self.modal =
                            Modal::Resources(k, n, name, self.selected_uid(), String::new());
                    }
                }
                KeyCode::Char('l') => self.request_logs(false, true),
                KeyCode::Char('L' | 'p') => self.request_logs(true, false),
                KeyCode::Char('s') => {
                    if self.kind() == "pods" {
                        self.request_shell();
                    } else if self.kind() == "nodes" {
                        self.status =
                            "Node shell is unavailable; select a Pod for a reviewed shell.".into();
                    } else {
                        return self.scale();
                    }
                }
                KeyCode::Char('f') => self.modal = Modal::ForwardView,
                KeyCode::Char('F') => self.request_forward(),
                KeyCode::Char('x') => {
                    self.status =
                        "Use :xray RESOURCE for relationships or :restart for a reviewed rollout."
                            .into()
                }
                KeyCode::Delete => self.prepare(Action::Delete),
                _ => {}
            },
        }
        Ok(false)
    }
    fn key_event(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<bool> {
        if matches!(self.modal, Modal::AdviceRunning)
            && modifiers.contains(KeyModifiers::CONTROL)
            && code == KeyCode::Char('c')
        {
            if let Some(cancel) = self.advice_cancel.take() {
                let _ = cancel.send(());
            }
            self.status = "Canceling Copilot; waiting for process and connection cleanup.".into();
            return Ok(false);
        }
        if matches!(self.modal, Modal::HelmRunning(..))
            && modifiers.contains(KeyModifiers::CONTROL)
            && code == KeyCode::Char('c')
        {
            self.cancel_helm();
            return Ok(false);
        }
        if self.write_inflight {
            return self.key(code);
        }
        if matches!(self.modal, Modal::YamlEditor(..)) {
            if let Modal::YamlEditor(mut editor) = std::mem::replace(&mut self.modal, Modal::None) {
                match editor.key(crossterm::event::KeyEvent::new(code, modifiers)) {
                    Ok(crate::ui_yaml::Event::Cancel) => self.close_subview(),
                    Ok(crate::ui_yaml::Event::Review) => {
                        if self.busy {
                            self.modal = Modal::YamlEditor(editor);
                            self.status = "Wait for the current request; draft retained.".into();
                        } else {
                            self.send(Task::YamlPrepare(editor));
                        }
                    }
                    Ok(crate::ui_yaml::Event::External) => {
                        if self.options.external_editor {
                            self.pending_editor = true;
                        } else {
                            self.status = "Start KBorg with --allow-external-editor to edit drafts in $EDITOR.".into();
                        }
                        self.modal = Modal::YamlEditor(editor);
                    }
                    Ok(crate::ui_yaml::Event::Continue) => self.modal = Modal::YamlEditor(editor),
                    Err(error) => {
                        self.status = error;
                        self.modal = Modal::YamlEditor(editor);
                    }
                }
            }
            return Ok(false);
        }
        if modifiers.contains(KeyModifiers::CONTROL) {
            if let Modal::WriteReview(review) = &mut self.modal {
                if matches!(code, KeyCode::Char('u' | 'w')) {
                    review.view.typed.clear();
                }
                return Ok(false);
            }
            if let Modal::AdviceReview(review) = &mut self.modal {
                if matches!(code, KeyCode::Char('u' | 'w')) {
                    review.typed.clear();
                }
                return Ok(false);
            }
            if let Modal::HelmForm(form) = &mut self.modal {
                if matches!(code, KeyCode::Char('u' | 'w')) {
                    form.clear();
                }
                return Ok(false);
            }
            if let Modal::HelmReview(review) = &mut self.modal {
                if matches!(code, KeyCode::Char('u' | 'w')) {
                    review.typed.clear();
                }
                return Ok(false);
            }
            if let Modal::Helm(table) = &self.modal {
                if code == KeyCode::Char('r') {
                    let request = table.refresh();
                    self.request_helm(request, false);
                } else if code == KeyCode::Char('d') {
                    if let Some(current) = table.current() {
                        self.begin_helm_form(helm_ui::Intent::Uninstall(current));
                    }
                } else if code == KeyCode::Char('s') {
                    self.request_export();
                }
                return Ok(false);
            }
            if matches!(self.modal, Modal::Follow(_)) {
                if code == KeyCode::Char('r') {
                    if self.busy {
                        self.status = "Wait for the current log request before refreshing.".into();
                        return Ok(false);
                    }
                    if let Modal::Follow(view) = std::mem::replace(&mut self.modal, Modal::None) {
                        let read = view.range;
                        self.reload_logs(view, read);
                    }
                } else if code == KeyCode::Char('s') {
                    self.request_export();
                }
                return Ok(false);
            }
            if matches!(self.modal, Modal::Input("Command", _)) {
                match code {
                    KeyCode::Char('u' | 'w') => {
                        self.command_cursor = 0;
                        self.completion = None;
                        self.modal = Modal::Input("Command", String::new());
                        return Ok(false);
                    }
                    KeyCode::Char('f') => return self.key(KeyCode::Tab),
                    KeyCode::Char('e') => return self.key(KeyCode::Enter),
                    KeyCode::Char('p') => return self.key(KeyCode::Up),
                    KeyCode::Char('n') => return self.key(KeyCode::Down),
                    _ => return Ok(false),
                }
            }
            if let Modal::Input(_, text) = &mut self.modal {
                if code == KeyCode::Char('u') {
                    text.clear();
                }
                return Ok(false);
            }
            if let Modal::Editor(draft) = &mut self.modal {
                if matches!(code, KeyCode::Char('u' | 'w')) {
                    draft.fields[draft.active].clear();
                }
                return Ok(false);
            }
            if let Modal::ForwardForm(_, _, _, fields, index) = &mut self.modal {
                if matches!(code, KeyCode::Char('u' | 'w')) {
                    fields[*index].clear();
                }
                return Ok(false);
            }
            if matches!(self.modal, Modal::None) {
                match code {
                    KeyCode::Char('a') => self.aliases(),
                    KeyCode::Char('d') => self.prepare(Action::Delete),
                    KeyCode::Char('k') => self.kill(),
                    KeyCode::Char('r') => {
                        if self.connected {
                            self.refresh = true;
                        } else if !self.busy {
                            self.connect();
                        }
                    }
                    KeyCode::Char('s') => {
                        let values = self.visible().into_iter().cloned().collect::<Vec<_>>();
                        match crate::screendump::save(
                            &self.context,
                            &self.namespace,
                            &self.kind(),
                            &values,
                        ) {
                            Ok(record) => {
                                self.status =
                                    format!("Saved {} | :sd opens saved resources", record.title)
                            }
                            Err(error) => self.status = error,
                        };
                    }
                    KeyCode::Char(' ') => {
                        let uids = self
                            .visible()
                            .iter()
                            .take(self.selected + 1)
                            .filter_map(|object| {
                                object["metadata"]["uid"].as_str().map(str::to_owned)
                            })
                            .collect::<Vec<_>>();
                        self.marks.extend(uids);
                        self.status = format!(
                            "{} marked | actions still target one selected UID",
                            self.marks.len()
                        );
                    }
                    KeyCode::Char('\\') => {
                        self.marks.clear();
                        self.status = "Marks cleared.".into();
                    }
                    _ => {
                        self.status =
                            "This k9s shortcut is not available here; ? lists supported actions."
                                .into()
                    }
                }
            }
            return Ok(false);
        }
        if modifiers.contains(KeyModifiers::ALT) {
            return Ok(false);
        }
        self.key(code)
    }
}

fn name(object: &Value) -> &str {
    object["metadata"]["name"].as_str().unwrap_or("-")
}
fn pod_container_names(object: &Value) -> Vec<String> {
    ["containers", "initContainers", "ephemeralContainers"]
        .iter()
        .flat_map(|field| object["spec"][field].as_array().into_iter().flatten())
        .filter_map(|container| container["name"].as_str().map(str::to_owned))
        .take(128)
        .collect()
}
fn supports_group_logs(resource: &str) -> bool {
    [
        "deployments.apps",
        "statefulsets.apps",
        "daemonsets.apps",
        "replicasets.apps",
        "jobs.batch",
        "cronjobs.batch",
        "services",
        "replicationcontrollers",
    ]
    .contains(&resource)
}
fn ordinary_container_names(object: &Value) -> Vec<String> {
    object["spec"]["containers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|container| container["name"].as_str().map(str::to_owned))
        .take(128)
        .collect()
}
fn pod_log_choices(object: &Value) -> Vec<String> {
    std::iter::once("*".to_owned())
        .chain(pod_container_names(object))
        .collect()
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
fn ready_sort_key(object: &Value) -> (u64, u64) {
    let readiness = readiness(object);
    if let Some((ready, total)) = readiness.split_once('/') {
        (ready.parse().unwrap_or(0), total.parse().unwrap_or(0))
    } else {
        (u64::from(readiness == "True"), 0)
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
const TUI_HELP: &str = "NAVIGATE LIKE K9S\n  :pod :pods :po           Singular, plural or discovered short name\n  :pod NAMESPACE          Resource in namespace; all means all namespaces\n  :pod /api|web           Resource plus regex filter\n  :pod app=api,tier=web    Server-side label selector\n  :pod @CONTEXT           Switch context; context names retain their case\n  :ctx / :ctx NAME        Context picker (Pod view) / direct switch (same view)\n  :ns / :ns NAME          Namespace picker / direct switch (Pod view)\n  Ctrl-A / :aliases       Resource alias picker; Enter navigates\n  :pulses / :pu           Read-only cluster health snapshot\n  :xray po|svc|dp [NS]    Ownership and service relationship snapshot\n  :screendump / :sd       Browse saved redacted resource snapshots\n  :can u:NAME             Read-only RBAC subject view\n  :dir PATH               Browse local YAML directory; no apply\n  :helm [NAMESPACE]       Native release metadata (--allow-helm)\n  :history RELEASE        Selected release revision history\n  :q :q! :qa :Q :quit     Quit (in-flight mutations must finish)\n\nCOMMAND PROMPT\n  : / Tab                 Open prompt; Tab completes; Shift-Tab reverses\n  Up / Down               Recall in-session command history\n  Left Right Home End     Edit at cursor; Ctrl-U clears\n  Enter / Esc             Run command / cancel without changing view\n  [ / ] / -               Previous / next / last view\n  Esc                     Close view or return to previous resource\n\nFILTER\n  /PATTERN                Case-insensitive regex over namespace/name/status\n  /!PATTERN               Inverse regex\n  /-f PATTERN             Fuzzy subsequence match\n  /-l LABEL_SELECTOR      Apply server-side label selector on Enter\n  Enter / Esc             Keep / restore previous filter and selected UID\n\nRESOURCE TABLE\n  j k / arrows            Move; PgUp PgDown Home End page\n  Enter                   Drill into containers, related Pods or namespace\n  d                       Describe selected object (redacted overview)\n  D                       Dashboard for current context and namespace\n  b / B                   Save current selection / open saved selections\n  y / v                   View redacted YAML; J views JSON\n  c / n                   Copy name / namespace via terminal OSC52 clipboard\n  w / 0                   Warp to selected namespace / all namespaces\n  N / A / S / R           Sort by name / age / status / readiness\n  C / M                   Sort Pods/Nodes by CPU / memory, highest first\n  CPU/MEM snapshots refresh every 15s while the table is visible; - means\n  unknown, never zero. ~ indicates Metrics API omitted UID; creation checked.\n  Table CPU/MEM labels are rounded; d or m shows exact measured quantities.\n  Space / Ctrl-Space      Mark / mark range (visual only; no bulk mutations)\n  Ctrl-Backslash          Clear marks\n  Ctrl-S / :export        Save visible snapshot / selected resource YAML\n  Ctrl-R / :discover      Refresh / API discovery; r restarts workloads\n  l / p                   Follow logs / previous logs (--allow-logs)\n  Pod s / a / f / F       Shell / attach / forwards / new forward\n  Workload s / r          Scale Deployment-StatefulSet / reviewed restart\n  e / :edit-container     YAML spec editor / focused container editor\n  Ctrl-D                  Reviewed deletion\n  Ctrl-K / :kill          Reviewed zero-grace Pod kill (--allow-force-kill)\n  o                       Show selected Pod's Node\n\nHELM AND LOGS\n  Helm Enter / r          Release revision history\n  History Enter / v       Explicit selected revision stored values\n  Helm i / u / t / c      Install / upgrade / test / separately install chart CRDs\n  History r / Ctrl-D      Rollback selected revision / uninstall current release\n  Helm lifecycle needs --write --allow-helm --allow-helm-write and --helm-tool.\n  Sources: local .tgz or explicit OCI :TAG / @sha256:DIGEST.
  --helm-keyring requires native Helm provenance verification during prepare.\n  Hooks/CRDs require --allow-helm-hooks / --allow-helm-crds; default OFF.\n  :helm-crds prepares create-only Namespaced CRDs, retained on uninstall.
  :helm-test runs stored tests; :helm-crds-upgrade reviews schema changes.
  :helm-crds-delete is explicit, checks live instances and needs stronger approval.\n  :helm-crds-migrate reviews storage rewrites; controller proof gates finalization.\n  :copilot previews selected diagnostics; explicit policy and transfer approval to send.\n  Every review line must be painted before exact namespace/release approval.\n  Helm d / m / y          Metadata / resource identities / metadata YAML\n  Helm Ctrl-R / /         Refresh / filter; Esc returns to previous view\n  Log a                   Toggle all ordinary containers / preferred container\n  Workload l / p          Fixed captured Pod membership / previous logs\n  :logs LABEL_SELECTOR    Explicit namespace-scoped multi-Pod log capture\n  Group reloads keep the same Pod UIDs; close and reopen to capture new Pods.\n  :logs-live [SELECTOR]   Opt in to announced Pod joins/departures (10s refresh)\n  Log 0..6                Tail / head / last 1m,5m,15m,30m,1h\n  Log s w t f             Autoscroll / wrap / timestamps / fullscreen\n  Log e / W / Ctrl-S      Error matches / warning matches / local export\n  Pod l honors the default-container annotation, otherwise merges multiple\n  ordinary containers. Each merged line carries a namespace/Pod/container.\n\nKBORG COMMANDS\n  :actions                Discover actions with capability reasons\n  :scale :image :restart  Explicit reviewed workload operations\n  :metrics [resources]    Metrics / opt into requests-limit comparisons\n  :overview               Dashboard of up to four explicit contexts\n  Dashboard 1..4 / Enter   Focus a context / open its Pods\n  Dashboard c / s / B      Choose contexts / save / open saved selections\n  Dashboard ?             Contextual keymap and health evidence meaning\n  :themes                 Theme preview and custom palette creation\n\nLIMITS AND SAFETY\n  Context, namespace and mode remain visible; DEMO is sample data.\n  STALE and TRUNCATED indicate incomplete data. Regex uses bounded Rust regex\n  syntax; look-around and backreferences are unavailable.\n  :secrets lists key names only with --allow-secret-reveal; x reveals one\n  key for 30s. Values are never exported, copied or kept after Esc.\n  e edits existing namespaced non-Secret spec fields with --allow-yaml-edit.\n  Hidden values, metadata and status are preserved; no resource creation.\n  :edit-container retains focused image/resource forms.\n  Node c/u/r reviews cordon/uncordon/drain with --allow-node-maintenance.\n  Pod file transfer, node shell, benchmarks and scans remain unavailable.\n  Helm release reads need --allow-helm. Values are stored overrides with\n  known fields redacted; custom keys can remain confidential. Complete\n  review is mandatory for lifecycle changes. Raw storage, chart files and notes are excluded.\n  Visual marks never authorize bulk writes. Every mutation requires --write\n  plus one-use approval; shell/attach/forward need separate capabilities.";

#[derive(Clone, Copy, Debug)]
struct Rect {
    x: u16,
    y: u16,
    width: u16,
    height: u16,
}
#[derive(Clone, Copy, Debug)]
struct Layout {
    table: Rect,
}
impl Layout {
    fn new(width: u16, height: u16) -> Self {
        Self {
            table: Rect {
                x: 0,
                y: 3,
                width,
                height: height.saturating_sub(6),
            },
        }
    }
    fn table_rows(self) -> usize {
        usize::from(self.table.height.saturating_sub(3))
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
    Node,
    Ip,
    Cpu,
    Memory,
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
            Self::Node => "NODE",
            Self::Ip => "IP",
            Self::Cpu => "CPU(m)",
            Self::Memory => "MEM",
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
            Self::Node => clean_value(&object["spec"]["nodeName"]),
            Self::Ip => clean_value(&object["status"]["podIP"]),
            Self::Cpu | Self::Memory => "-".into(),
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
    table_line_with_metrics(columns, object, now, None)
}
fn table_line_with_metrics(
    columns: &[(Column, usize)],
    object: Option<&Value>,
    now: i64,
    usage: Option<&table_metrics::Usage>,
) -> String {
    columns
        .iter()
        .map(|(column, width)| {
            let text = object
                .map(|object| match (column, usage) {
                    (Column::Cpu, Some(usage)) => format!(
                        "{}{}",
                        table_metrics::compact_cpu(usage.cpu_nanocores),
                        if usage.identity_verified { "" } else { "~" }
                    ),
                    (Column::Memory, Some(usage)) => format!(
                        "{}{}",
                        table_metrics::compact_memory(usage.memory_bytes),
                        if usage.identity_verified { "" } else { "~" }
                    ),
                    _ => column.value(object, now),
                })
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
fn blend(a: themes::Rgb, b: themes::Rgb, percent: u16) -> themes::Rgb {
    let channel =
        |a: u8, b: u8| ((u16::from(a) * (100 - percent) + u16::from(b) * percent) / 100) as u8;
    themes::Rgb(channel(a.0, b.0), channel(a.1, b.1), channel(a.2, b.2))
}
fn helm_heading(line: &str) -> bool {
    let text = line.trim_start();
    [
        "TARGET  ",
        "POLICY  ",
        "SOURCE  ",
        "TRUST  ",
        "01 ",
        "DETAILS",
        "Details",
        "OPERATION",
        "CONNECTION",
        "CHANGE PLAN",
        "APPROVAL",
        "FULL REVIEW",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
        || (text.chars().any(|c| c.is_ascii_alphabetic())
            && text.chars().all(|c| !c.is_ascii_lowercase()))
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
    // Only these trusted, single-cell box glyphs bypass data's ASCII display
    // policy. Non-UTF-8 locales keep the conservative ASCII frame.
    let unicode = ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.is_empty())
        .is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("utf-8") || value.contains("utf8")
        });
    let (tl, tr, bl, br, horizontal, vertical) = if unicode {
        ("╭", "╮", "╰", "╯", "─", "│")
    } else {
        ("+", "+", "+", "+", "-", "|")
    };
    let mut line = |y, text: String| {
        queue!(
            out,
            cursor::MoveTo(rect.x, y),
            themes::ForegroundColor(fg),
            themes::BackgroundColor(bg),
            style::Print(text)
        )
    };
    line(
        rect.y,
        format!(
            "{tl}{title}{}{tr}",
            horizontal.repeat(width.saturating_sub(title.len()))
        ),
    )?;
    for y in rect.y + 1..rect.y + rect.height - 1 {
        line(y, format!("{vertical}{}{vertical}", " ".repeat(width)))?;
    }
    line(
        rect.y + rect.height - 1,
        format!("{bl}{}{br}", horizontal.repeat(width)),
    )
}

/// The actions that actually work for the selected resource type with the
/// capabilities this session was started with. Disabled actions are omitted
/// so the row never advertises a key that would only print a refusal.
fn table_hints(app: &App) -> Vec<(&'static str, &'static str)> {
    let connection = &app.options.connection;
    let kind = app.kind();
    let write = connection.writable;
    let logs = connection.logs || connection.demo;
    let mut hints = vec![("?", "all keys")];
    let drill = match kind.as_str() {
        "pods" => Some("containers"),
        "nodes" | "namespaces" => Some("pods"),
        kind if supports_group_logs(kind) => Some("pods"),
        _ => None,
    };
    if let Some(drill) = drill {
        hints.push(("Enter", drill));
    }
    match kind.as_str() {
        "pods" => {
            if logs {
                hints.push(("l", "logs"));
            }
            if write && connection.allow_exec {
                hints.push(("s", "shell"));
            }
            if write && connection.allow_port_forward {
                hints.push(("F", "forward"));
            }
        }
        "nodes" => {
            if write && connection.allow_node_maintenance {
                hints.extend([("c", "cordon"), ("u", "uncordon"), ("r", "drain")]);
            }
        }
        kind if supports_group_logs(kind) => {
            if logs {
                hints.push(("l", "logs"));
            }
            if write && ["deployments.apps", "statefulsets.apps"].contains(&kind) {
                hints.push(("s", "scale"));
            }
            if write && crate::edit::supports_resource(kind) {
                hints.push(("r", "restart"));
            }
        }
        _ => {}
    }
    hints.extend([
        ("d", "describe"),
        ("y", "yaml"),
        ("/", "filter"),
        (":", "command"),
    ]);
    if write && connection.allow_yaml_edit {
        hints.push(("e", "edit"));
    }
    let deletable = matches!(
        kind.as_str(),
        "pods"
            | "deployments.apps"
            | "statefulsets.apps"
            | "daemonsets.apps"
            | "services"
            | "jobs.batch"
            | "cronjobs.batch"
    );
    if write && deletable {
        hints.push(("Ctrl-D", "delete"));
    }
    if write && connection.allow_force_kill && kind == "pods" {
        hints.push(("Ctrl-K", "kill"));
    }
    hints.push(("D", "dashboard"));
    hints
}
fn key_hints(
    out: &mut impl Write,
    y: u16,
    width: u16,
    hints: &[(&str, &str)],
    colors: [Color; 3],
) -> io::Result<()> {
    let [key, text, bg] = colors;
    row(out, y, width, "", text, bg)?;
    let mut x = 1;
    for (shortcut, label) in hints {
        let label = format!(" {label}{}", if width < 50 { " " } else { "   " });
        // Keep complete actions together; a clipped shortcut is misleading.
        if x + shortcut.len() as u16 + label.len() as u16 > width {
            break;
        }
        cell(out, x, y, shortcut.len() as u16, shortcut, key, bg)?;
        x += shortcut.len() as u16;
        cell(out, x, y, label.len() as u16, &label, text, bg)?;
        x += label.len() as u16;
    }
    Ok(())
}
fn ui_color(app: &App, role: &str) -> Color {
    if app.options.color && !app.theme.terminal {
        app.theme.get(role).color()
    } else {
        Color::Reset
    }
}
fn render_advice_footer(
    out: &mut impl Write,
    review: &advice_ui::Review,
    w: u16,
    h: u16,
    colors: [Color; 3],
) -> io::Result<()> {
    let [accent, warning, bg] = colors;
    let remaining = review.remaining(usize::from(w.saturating_sub(2)));
    let label = if review.prepared.is_some() {
        format!("{} > {}_", review.confirmation(), review.typed)
    } else {
        review.confirmation()
    };
    row(out, h - 2, w, &label, warning, bg)?;
    row(
        out,
        h - 1,
        w,
        &format!(
            " {remaining} unread | PgUp/PgDown review | {} | Esc back",
            if review.prepared.is_some() {
                "Enter send approved payload"
            } else {
                "setup required to send"
            }
        ),
        accent,
        bg,
    )
}
fn render_helm_review_footer(
    out: &mut impl Write,
    review: &helm_ui::Review,
    w: u16,
    h: u16,
    colors: [Color; 4],
) -> io::Result<()> {
    let [warning, accent, success, bg] = colors;
    let width = usize::from(w.saturating_sub(2));
    let remaining = review.remaining(width);
    let (painted, total) = review.progress(width);
    let text = format!(
        "CONFIRM {} > {}_",
        review.prepared.confirmation(),
        review.typed
    );
    let chars = text.chars().collect::<Vec<_>>();
    let start = chars.len().saturating_sub(usize::from(w));
    row(
        out,
        h - 2,
        w,
        &chars.into_iter().skip(start).collect::<String>(),
        warning,
        bg,
    )?;
    let progress = if w < 40 {
        format!("{remaining} unread | Enter run | Esc")
    } else if w < 80 {
        format!("{remaining} unread | PgUp/Dn review | Enter run | Esc")
    } else {
        format!(
            " {remaining} unread  |  {painted}/{total} displayed  |  [PgUp/PgDn] review  [Enter] execute  [Esc] cancel"
        )
    };
    row(
        out,
        h - 1,
        w,
        &progress,
        if remaining == 0 { success } else { accent },
        bg,
    )
}
fn render_write_footer(
    out: &mut impl Write,
    review: &review_ui::Review,
    w: u16,
    h: u16,
    colors: [Color; 3],
) -> io::Result<()> {
    let [accent, warning, bg] = colors;
    row(
        out,
        h - 2,
        w,
        &format!(" {} > {}_", review.confirmation, review.typed),
        warning,
        bg,
    )?;
    row(
        out,
        h - 1,
        w,
        &format!(
            " {} unread / PgUp PgDown review / Enter apply / Esc back",
            review.remaining(usize::from(w.saturating_sub(2)))
        ),
        accent,
        bg,
    )
}
fn draw(app: &mut App) -> Result<()> {
    let (w, h) = terminal::size().map_err(|_| "cannot read terminal size")?;
    let mut frame = Vec::new();
    // Keep complete frames atomic on supporting terminals. Older terminals ignore
    // this standard private mode; no feature or input behavior depends on it.
    queue!(&mut frame, BeginSynchronizedUpdate).map_err(|_| "draw failed")?;
    render(app, w, h, &mut frame)?;
    queue!(&mut frame, EndSynchronizedUpdate).map_err(|_| "draw failed")?;
    let mut out = io::stdout().lock();
    out.write_all(&frame)
        .and_then(|_| out.flush())
        .map_err(|_| "draw failed")?;
    if w >= 28 && h >= 10 {
        let advice_colors = [
            ui_color(app, "accent"),
            ui_color(app, "warning"),
            ui_color(app, "background"),
        ];
        if let Modal::WriteReview(review) = &mut app.modal {
            review
                .view
                .mark_drawn(usize::from(w - 2), usize::from(h - 8));
            render_write_footer(&mut out, &review.view, w, h, advice_colors)
                .and_then(|_| out.flush())
                .map_err(|_| "draw failed")?;
        }
        if let Modal::AdviceReview(review) = &mut app.modal {
            review.mark_drawn(usize::from(w - 2), usize::from(h - 8));
            render_advice_footer(&mut out, review, w, h, advice_colors)
                .and_then(|_| out.flush())
                .map_err(|_| "draw failed")?;
        }
        let changed = if let Modal::HelmReview(review) = &mut app.modal {
            let before = review.remaining(usize::from(w - 2));
            review.mark_drawn(usize::from(w - 2), usize::from(h - 8));
            before != review.remaining(usize::from(w - 2))
        } else {
            false
        };
        if changed && let Modal::HelmReview(review) = &app.modal {
            // Content has already been successfully written and flushed. Only
            // now acknowledge those lines and repaint the small progress footer.
            render_helm_review_footer(
                &mut out,
                review,
                w,
                h,
                [
                    ui_color(app, "warning"),
                    ui_color(app, "accent"),
                    ui_color(app, "success"),
                    ui_color(app, "background"),
                ],
            )
            .and_then(|_| out.flush())
            .map_err(|_| "draw failed")?;
        }
    }
    Ok(())
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
            | Modal::EditContainers(..)
            | Modal::Editor(..)
            | Modal::ForwardForm(..)
            | Modal::HelmForm(..)
    );
    let modal_width = if compact && width >= 80 {
        width.saturating_sub(12).min(100)
    } else {
        width
    };
    let desired = match modal {
        Modal::Input(..) => 6,
        Modal::NewTheme(..) | Modal::Image(..) => 8,
        Modal::Resources(..) => 10,
        Modal::Editor(..) => 13,
        Modal::ForwardForm(..) => 9,
        Modal::HelmForm(form) => (form.fields.len() as u16).saturating_add(13),
        Modal::EditContainers(_, _, _, _, values, _) => (values.len() as u16).saturating_add(3),
        Modal::Scale(..) => 6,
        Modal::Picker(_, items, _) | Modal::SessionContainers(_, _, _, items, _, _) => {
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
fn render_log_modal(
    app: &App,
    view: &crate::ui_logs::LogView,
    w: u16,
    h: u16,
    out: &mut impl Write,
) -> Result<()> {
    let color = |role: &str| {
        if app.options.color && !app.theme.terminal {
            app.theme.get(role).color()
        } else {
            Color::Reset
        }
    };
    let bg = color("background");
    let y = if view.fullscreen { 2 } else { 3 };
    let rect = Rect {
        x: 0,
        y,
        width: w,
        height: h.saturating_sub(y + 2),
    };
    let title = app
        .log_target
        .as_ref()
        .map(|target| {
            if let Some(membership) = &target.membership {
                let details = view.title();
                let details = if w < 80 {
                    details.strip_prefix("Logs | ").unwrap_or(&details)
                } else {
                    &details
                };
                return format!(
                    "FIXED{} {} Pods / {} containers | {} | {} | {}",
                    view.severity_badge(),
                    membership.pods().len(),
                    membership.source_count(),
                    details,
                    membership.namespace(),
                    membership.description()
                );
            }
            format!(
                "LOGS{} {}/{} / {} | {}",
                view.severity_badge(),
                target.namespace,
                target.pod,
                if target.all {
                    "ALL ordinary containers"
                } else {
                    target.container.as_deref().unwrap_or("default")
                },
                view.title()
            )
        })
        .unwrap_or_else(|| format!("LOGS {}", view.title()));
    panel(out, rect, &title, true, color("accent"), bg).map_err(|_| "draw failed")?;
    for (index, line) in view
        .render(
            usize::from(w.saturating_sub(2)),
            usize::from(rect.height.saturating_sub(2)),
        )
        .iter()
        .enumerate()
    {
        let role = match crate::ui_logs::severity(line) {
            Some("error") => "danger",
            Some("warning") => "warning",
            _ => "text",
        };
        cell(out, 1, y + 1 + index as u16, w - 2, line, color(role), bg)
            .map_err(|_| "draw failed")?;
    }
    row(out, h - 2, w, view.help(), color("accent"), bg).map_err(|_| "draw failed")?;
    row(
        out,
        h - 1,
        w,
        "0 tail 1 head 2 1m 3 5m 4 15m 5 30m 6 1h | Esc back",
        color("muted"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    out.flush().map_err(|_| "draw failed".into())
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
    queue!(out, cursor::Hide).map_err(|_| "draw failed")?;
    // Surfaces are derived from the selected palette, so every theme keeps its
    // identity and existing terminal-depth/NO_COLOR handling remains central.
    let surface = if app.options.color && !t.terminal {
        blend(t.get("background"), t.get("text"), 3).color()
    } else {
        Color::Reset
    };
    let raised = if app.options.color && !t.terminal {
        blend(t.get("background"), t.get("text"), 6).color()
    } else {
        Color::Reset
    };
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
    let border = if app.options.color && !t.terminal {
        blend(t.get("background"), t.get("muted"), 45).color()
    } else {
        Color::Reset
    };
    row(out, 0, w, "", fg, surface).map_err(|_| "draw failed")?;
    cell(
        out,
        0,
        0,
        8.min(w),
        " KBORG ",
        color("selection_text"),
        color("selection_background"),
    )
    .map_err(|_| "draw failed")?;
    let title = match &app.modal {
        Modal::LiveOverview(..) => "Dashboard".to_owned(),
        Modal::Helm(..) | Modal::HelmForm(..) | Modal::HelmReview(..) => "Helm releases".to_owned(),
        Modal::ForwardView | Modal::ForwardForm(..) => "Port forwards".to_owned(),
        Modal::Themes(..) => "Appearance".to_owned(),
        Modal::Help(..) => "Keymap".to_owned(),
        _ => app
            .kinds
            .get(app.kind_index)
            .map(resource_label)
            .unwrap_or_else(|| app.kind()),
    };
    if w >= 64 {
        cell(out, 10, 0, w.saturating_sub(16), &title, fg, surface).map_err(|_| "draw failed")?;
        cell(out, w - 4, 0, 4, " ? ", color("accent"), surface).map_err(|_| "draw failed")?;
    }
    if let Modal::LiveOverview(dashboard) = &app.modal {
        return dashboard_ui::render(app, dashboard, w, h, out);
    }
    row(out, 1, w, "", fg, bg).map_err(|_| "draw failed")?;
    let mode_color = if app.options.connection.writable && !app.options.connection.demo {
        color("warning")
    } else {
        color("muted")
    };
    let mode_width = (mode.len() as u16 + 1).min(w);
    let identity_width = if w >= 64 {
        w.saturating_sub(mode_width + 1)
    } else {
        w
    };
    cell(
        out,
        0,
        1,
        identity_width,
        &identity_line(
            &app.context,
            &app.namespace,
            &app.server,
            usize::from(identity_width),
        ),
        fg,
        bg,
    )
    .map_err(|_| "draw failed")?;
    cell(
        out,
        w - mode_width,
        if w >= 64 { 1 } else { 0 },
        mode_width,
        &format!(" {mode}"),
        mode_color,
        if w >= 64 { bg } else { surface },
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
    let filtered =
        !app.filter.is_empty() || !app.selector.is_empty() || !app.field_selector.is_empty();
    let filter_hint = [
        (!app.filter.is_empty()).then(|| format!(" /{}", app.filter)),
        (!app.selector.is_empty()).then(|| format!(" LABEL:{}", app.selector)),
        (!app.field_selector.is_empty()).then(|| format!(" FIELD:{}", app.field_selector)),
    ]
    .into_iter()
    .flatten()
    .collect::<String>();
    let mut bands = if w >= 64 {
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
            (
                format!(
                    "  {} {}  ",
                    visible.len(),
                    if visible.len() == 1 {
                        app.kinds
                            .get(app.kind_index)
                            .map(|kind| kind.kind.as_str())
                            .unwrap_or(&resource)
                    } else {
                        &resource
                    }
                ),
                "text",
            ),
            (format!(" {healthy} healthy "), "success"),
            (
                format!(
                    " {attention} {} attention ",
                    if attention == 1 { "needs" } else { "need" }
                ),
                if attention > 0 { "warning" } else { "muted" },
            ),
            (
                format!(" sort: {}{}", app.sort.label(), filter_hint,),
                "muted",
            ),
        ]
    } else {
        vec![(
            format!(
                "{freshness}{} | {} {resource}{} | !{attention}",
                if app.truncated { " TRUNCATED" } else { "" },
                visible.len(),
                if filtered { " FILTERED" } else { "" },
            ),
            "accent",
        )]
    };
    if app.worker.forward.active_count() > 0 {
        bands.push((
            format!("  {} forwards", app.worker.forward.active_count()),
            "accent",
        ));
    }
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
    {
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
            if !filtered { "" } else { " FILTERED" }
        );
        let title = if w >= 90 && matches!(app.kind().as_str(), "pods" | "nodes") {
            format!("{title} | {}", app.table_metrics.label(Instant::now()))
        } else {
            title
        };
        panel(out, rect, "", false, border, bg).map_err(|_| "draw failed")?;
        cell(
            out,
            rect.x + 1,
            rect.y,
            (title.len() as u16 + 2).min(rect.width - 2),
            &format!(" {title} "),
            color("accent"),
            bg,
        )
        .map_err(|_| "draw failed")?;
        let namespaced = app
            .kinds
            .get(app.kind_index)
            .is_some_and(|kind| kind.namespaced);
        let column_width = usize::from(rect.width - 5);
        let measured = matches!(app.kind().as_str(), "pods" | "nodes") && column_width >= 70;
        let node_columns = app.kind() == "pods" && column_width >= 130;
        let ip_column = node_columns && column_width >= 155;
        let reserved = if measured { 20 } else { 0 }
            + if node_columns { 23 } else { 0 }
            + if ip_column { 17 } else { 0 };
        let mut cols = columns(column_width.saturating_sub(reserved), namespaced);
        if measured {
            cols.push((Column::Cpu, 9));
            cols.push((Column::Memory, 9));
        }
        if node_columns {
            cols.push((Column::Node, 22));
        }
        if ip_column {
            cols.push((Column::Ip, 16));
        }
        let now = k8s_openapi::jiff::Timestamp::now().as_second();
        cell(
            out,
            rect.x + 1,
            rect.y + 1,
            rect.width - 2,
            &format!("   {}", table_line(&cols, None, now)),
            color("muted"),
            surface,
        )
        .map_err(|_| "draw failed")?;
        let start = app.selected.saturating_sub(height.saturating_sub(1));
        for (r, object) in visible.iter().skip(start).take(height).enumerate() {
            let selected = start + r == app.selected;
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
                    "{}{} {}",
                    if start + r == app.selected { ">" } else { " " },
                    if object["metadata"]["uid"]
                        .as_str()
                        .is_some_and(|uid| app.marks.contains(uid))
                    {
                        "*"
                    } else {
                        " "
                    },
                    table_line_with_metrics(
                        &cols,
                        Some(object),
                        now,
                        app.table_metrics.usage(object, Instant::now())
                    )
                ),
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
            if !selected {
                let mut x = rect.x + 4;
                for (column, width) in &cols {
                    if matches!(column, Column::Status) {
                        cell(
                            out,
                            x,
                            rect.y + 2 + r as u16,
                            *width as u16,
                            &status,
                            if unhealthy {
                                color("warning")
                            } else if health(object) == Health::Healthy {
                                color("success")
                            } else {
                                color("muted")
                            },
                            bg,
                        )
                        .map_err(|_| "draw failed")?;
                    }
                    x += *width as u16 + 1;
                }
            }
        }
        if visible.is_empty() && height > 0 {
            let empty = if !app.filter.is_empty() {
                "No matches. Esc clears the filter."
            } else if app.busy {
                "Loading resources..."
            } else if !app.connected {
                "Not connected. Ctrl-R retries; :ctx selects context."
            } else if app.watch_blocked {
                "Access unavailable. Ctrl-R retries; :ctx selects context."
            } else {
                "No objects in this scope. :ns selects namespace."
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
            " {}{}",
            if app.sticky_status { "! " } else { "" },
            app.status
        ),
        if app.stale || app.sticky_status {
            color("warning")
        } else {
            color("muted")
        },
        surface,
    )
    .map_err(|_| "draw failed")?;
    // Keep the everyday workspace quiet. The complete keyboard map lives behind ?.
    row(out, h - 2, w, "", color("muted"), bg).map_err(|_| "draw failed")?;
    row(out, h - 1, w, "", color("muted"), bg).map_err(|_| "draw failed")?;
    match &app.modal {
        Modal::None => {
            key_hints(
                out,
                h - 1,
                w,
                &table_hints(app),
                [color("accent"), color("muted"), bg],
            )
            .map_err(|_| "draw failed")?;
        }
        Modal::Input("Command", text) => {
            let suggestions = app.suggestions(
                app.completion
                    .as_ref()
                    .map(|(base, _, _)| base.as_str())
                    .unwrap_or(text),
            );
            if h >= 12 && !suggestions.is_empty() {
                row(
                    out,
                    h - 4,
                    w,
                    &format!(
                        " {}",
                        suggestions
                            .iter()
                            .take(7)
                            .map(|value| format!(":{value}"))
                            .collect::<Vec<_>>()
                            .join("   ")
                    ),
                    color("accent"),
                    bg,
                )
                .map_err(|_| "draw failed")?;
            }
            row(
                out,
                h - 2,
                w,
                " Tab complete  Up/Down history  Enter go  Esc cancel",
                color("muted"),
                bg,
            )
            .map_err(|_| "draw failed")?;
            let cursor = app.command_cursor.min(text.len());
            let prompt = format!(":{}|{}", &text[..cursor], &text[cursor..]);
            let chars = prompt.chars().collect::<Vec<_>>();
            let cursor_chars = text[..cursor].chars().count() + 1;
            let start = cursor_chars.saturating_sub(usize::from(w).saturating_sub(2));
            row(
                out,
                h - 1,
                w,
                &chars
                    .into_iter()
                    .skip(start)
                    .take(usize::from(w))
                    .collect::<String>(),
                color("selection_text"),
                color("selection_background"),
            )
            .map_err(|_| "draw failed")?;
        }
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
                "Enter apply | Esc restore | regex /! /-f /-l",
                color("muted"),
                bg,
            )
            .map_err(|_| "draw failed")?;
        }
        modal => {
            let title = match modal {
                Modal::Input(label, _) | Modal::Picker(label, ..) => *label,
                Modal::Follow(view) => return render_log_modal(app, view, w, h, out),
                Modal::Helm(table) => table.title(),
                Modal::HelmForm(form) => form.intent.label(),
                Modal::AdviceReview(review) => {
                    if review.prepared.is_some() {
                        "COPILOT / review diagnostic transfer"
                    } else {
                        "COPILOT / local preview only"
                    }
                }
                Modal::AdviceLoading => "COPILOT / preparing local preview",
                Modal::AdviceRunning => "COPILOT / advisory request in progress",
                Modal::AdviceDocument(title, ..) => title.as_str(),
                Modal::HelmReview(_) => "HELM REVIEW / approval required",
                Modal::HelmRunning(..) => "HELM EXECUTION / asynchronous reviewed operation",
                Modal::HelmOutcome(title, ..) => title.as_str(),
                Modal::HelmLoading(_) => "HELM / bounded storage read",
                Modal::HelmDocument(title, ..) => title.as_str(),
                Modal::Containers(..) => "CHOOSE CONTAINER",
                Modal::Image(..) => "CHANGE DEPLOYMENT IMAGE",
                Modal::Detail(..) => "REDACTED INSPECTION / Esc close",
                Modal::Overview(..) => "OBJECT OVERVIEW / J JSON / y YAML / Esc close",
                Modal::LiveOverview(..) => "LIVE CONTEXT OVERVIEW / independent watches",
                Modal::SessionContainers(..) => "POD SHELL / choose ordinary container",
                Modal::SessionConfirm(prepared, _) if prepared.is_port_forward() => {
                    "REVIEW PORT FORWARD / loopback listener"
                }
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
                Modal::EditContainers(..) => "EDIT WORKLOAD / choose ordinary container",
                Modal::Editor(..) => "EDIT CONTAINER / server dry-run and review required",
                Modal::Export(_) => "EXPORT / review local file",
                Modal::YamlEditor(_) => "YAML EDITOR / spec fields",
                Modal::WriteReview(_) => "REVIEW CHANGES / approval required",
                Modal::WriteRunning => "APPLYING REVIEWED CHANGES",
                Modal::PodContainers(..) => "CONTAINERS / Enter logs / d describe / Esc back",
                Modal::Directory(..) => {
                    "DIRECTORY / Enter open / YAML and JSON previews / Esc back"
                }
                Modal::Secrets(..) => "SECRETS / values hidden / x reveals one key",
                Modal::Subjects(view) => view.title,
                Modal::ForwardView => "PORT FORWARDS / independently reviewed sessions",
                Modal::ForwardForm(..) => "PORT FORWARD / loopback only / review required",
                Modal::None => "",
                Modal::Filter(..) => "",
            };
            let rect = modal_rect(modal, w, h);
            let modal_bg = if matches!(modal, Modal::HelmForm(..)) {
                surface
            } else {
                bg
            };
            panel(out, rect, "", false, border, modal_bg).map_err(|_| "draw failed")?;
            cell(
                out,
                rect.x + 1,
                rect.y,
                (title.len() as u16 + 2).min(rect.width.saturating_sub(2)),
                &format!(" {title} "),
                if matches!(modal, Modal::HelmReview(..)) || matches!(modal,Modal::HelmForm(form) if matches!(form.intent,helm_ui::Intent::DeleteCrds | helm_ui::Intent::Uninstall(_))) {
                    color("warning")
                } else {
                    color("accent")
                },
                modal_bg,
            )
            .map_err(|_| "draw failed")?;
            let height = usize::from(rect.height.saturating_sub(2));
            let lines = match modal {
                Modal::YamlEditor(editor)=>editor.viewport(usize::from(rect.width.saturating_sub(2)),height),
                Modal::WriteReview(review)=>review.view.lines(usize::from(rect.width.saturating_sub(2)),height),
                Modal::WriteRunning=>vec!["Applying the approved operation...".into(),"Wait for the outcome before navigating or quitting.".into(),"No automatic retries. If interrupted, inspect cluster state.".into()],
                Modal::Export(draft)=>wrap_text(&safety::review_display(&draft.review()).unwrap_or_else(|error| format!("REVIEW BLOCKED\n\n{error}\n\nEsc returns.")),usize::from(rect.width.saturating_sub(2))).lines().map(str::to_owned).collect(),
                Modal::AdviceReview(review)=>review.lines(usize::from(rect.width.saturating_sub(2)),height),
                Modal::AdviceLoading=>vec!["Reading selected status and preparing the minimized preview...".into(),"No inference request is sent until you review and approve.".into()],
                Modal::AdviceRunning=>vec!["COPILOT ADVISORY / read-only".into(),String::new(),"Waiting for the bounded Copilot response...".into(),"Ctrl-C cancels; wait for process and connection cleanup.".into(),"No commands or Kubernetes changes are authorized.".into()],
                Modal::AdviceDocument(_,text,offset)=>wrap_text(text,usize::from(rect.width.saturating_sub(2))).lines().skip(*offset).map(str::to_owned).collect(),
                Modal::HelmForm(form)=>form.lines(usize::from(rect.width.saturating_sub(2)),height),
                Modal::HelmReview(review)=>review.lines(usize::from(rect.width.saturating_sub(2)),height),
                Modal::HelmRunning(target,started,cancelled)=>wrap_text(&format!("Reviewed target {target}\n\n{} | elapsed {}s | Helm command budget {}s\nFinal validation has its own bounded 120-second deadline.\nThe UI remains responsive while native Helm runs.\nCtrl-C requests local cancellation; remote changes may already exist.\nWaiting for the final success, failure or UNKNOWN outcome.\nDo not launch another operation until this result is reconciled.",if *cancelled{"CANCELLATION REQUESTED"}else{"RUNNING"},started.elapsed().as_secs(),app.options.helm_timeout_seconds),usize::from(rect.width.saturating_sub(2))).lines().map(str::to_owned).collect(),
                Modal::HelmOutcome(_,text,offset)=>wrap_text(text,usize::from(rect.width.saturating_sub(2))).lines().skip(*offset).map(str::to_owned).collect(),
                Modal::Helm(table)=>table.lines(usize::from(rect.width.saturating_sub(2)),height),
                Modal::HelmLoading(label)=>vec![(*label).into(),String::new(),"Esc cancels this view; late results cannot reopen it.".into()],
                Modal::HelmDocument(_,text,offset)=>wrap_text(text,usize::from(rect.width.saturating_sub(2))).lines().skip(*offset).map(str::to_owned).collect(),
                Modal::Input(label, text) => input_lines(label, text, usize::from(rect.width.saturating_sub(2)), height),
                Modal::EditContainers(_,_,name,_,values,index)=>std::iter::once(format!("{name} | Enter choose / Esc cancel")).chain(values.iter().enumerate().skip(index.saturating_sub(height.saturating_sub(2))).map(|(i,value)|format!("{} {}  {}",if i==*index{">"}else{" "},value.name,value.image))).collect(),
                Modal::Editor(draft)=>{
                    let mut lines=vec![format!("{} / {} / {}",draft.namespace,draft.name,draft.container),"Tab field | Ctrl-U clear | F2 resource budget | Enter review | Esc cancel".into(),String::new()];
                    let labels=["Image","CPU request","Memory request","CPU limit","Memory limit"];
                    for (i,label) in labels.iter().enumerate().take(if draft.resources{5}else{1}) {lines.push(format!("{} {label}: {}{}",if i==draft.active{">"}else{" "},draft.fields[i],if i==draft.active{"_"}else{""}));}
                    if !draft.resources {lines.push("Resource budget unchanged. F2 enables all four budget fields.".into());}
                    lines.push("Other fields remain unchanged. Approval follows a fresh UID-checked dry run.".into());
                    lines
                }
                Modal::PodContainers(object,index)=>{
                    let names=pod_log_choices(object);
                    let mut lines=vec![format!("Pod {}/{} | {}",namespace(object),name(object),summary(object)),String::new(),"   NAME                     READY  RESTARTS  STATE / IMAGE".into()];
                    for (i,name) in names.iter().enumerate().skip(index.saturating_sub(height.saturating_sub(4))) {
                        if name=="*"{lines.push(format!("{} ALL ordinary containers ({}) | Enter merged logs",if i==*index{">"}else{" "},ordinary_container_names(object).len()));continue;}
                        let status=["containerStatuses","initContainerStatuses","ephemeralContainerStatuses"].iter().flat_map(|field|object["status"][field].as_array().into_iter().flatten()).find(|status|status["name"].as_str()==Some(name));
                        let ready=status.map(|v|v["ready"]==true).unwrap_or(false);
                        let restarts=status.and_then(|v|v["restartCount"].as_u64()).unwrap_or(0);
                        let state=status.and_then(|v|v["state"].as_object()).and_then(|state|state.keys().next()).map(String::as_str).unwrap_or("unknown");
                        let image=["containers","initContainers","ephemeralContainers"].iter().flat_map(|field|object["spec"][field].as_array().into_iter().flatten()).find(|container|container["name"].as_str()==Some(name)).map(|container|clean_value(&container["image"])).unwrap_or_default();
                        lines.push(format!("{} {:<24} {:<6} {:>8}  {state} / {image}",if i==*index{">"}else{" "},display(name,24),if ready{"true"}else{"false"},restarts));
                    }
                    lines
                }
                Modal::Directory(directory,index)=>std::iter::once(format!("{}{}",directory.path.display(),if directory.truncated{" [TRUNCATED]"}else{""})).chain(std::iter::once("Read-only; Enter directory or redacted manifest preview".into())).chain(directory.entries.iter().enumerate().skip(index.saturating_sub(height.saturating_sub(3))).map(|(i,entry)|format!("{} {}{}",if i==*index{">"}else{" "},entry.name,if entry.directory{"/"}else{""}))).collect(),
                Modal::Secrets(view)=>view.lines(usize::from(rect.width.saturating_sub(2)),height),
                Modal::Subjects(view)=>view.lines(usize::from(rect.width.saturating_sub(2)),height),
                Modal::ForwardView=>app.worker.forward.lines(app.forward_selected,usize::from(rect.width.saturating_sub(2)),height),
                Modal::ForwardForm(namespace,pod,_,fields,index)=>{let mut lines=vec![format!("Pod {namespace}/{pod}"),"Tab field / Ctrl-U clear / Enter review / Esc cancel".into(),String::new()];for(i,label)in["Local port (1024..65535)","Remote port (1..65535)","Duration seconds (1..300)"].iter().enumerate(){lines.push(format!("{} {label}: {}{}",if i==*index{">"}else{" "},fields[i],if i==*index{"_"}else{""}));}lines},
                Modal::LiveOverview(_) => Vec::new(),
                Modal::Detail(text, offset)
                | Modal::Metrics(text, offset) => {
                    let wrapped = wrap_text(text, usize::from(rect.width.saturating_sub(2)));
                    let offset = (*offset).min(wrapped.lines().count().saturating_sub(1));
                    wrapped.lines().skip(offset).map(str::to_owned).collect()
                }
                Modal::Follow(view)=>view.render(usize::from(rect.width.saturating_sub(2)),height),
                Modal::Overview(object, offset) => {
                    let wrapped = wrap_text(
                        &format!("{}{}",overview_text(object, &app.context),app.table_metrics.usage(object,Instant::now()).map(|usage|format!("\n\nMEASURED USAGE\nCPU {}m | Memory {}B ({})\nTimestamp {} | Window {}\nBound UID {} | {}",crate::metrics::format_millicores(usage.cpu_nanocores),usage.memory_bytes,crate::metrics::format_memory(usage.memory_bytes),usage.timestamp,usage.window,usage.identity_uid,if usage.identity_verified{"Metrics API UID verified"}else{"Metrics API UID absent; entire sample window checked against current object creation"})).unwrap_or_default()),
                        usize::from(rect.width.saturating_sub(2)),
                    );
                    wrapped
                        .lines()
                        .skip((*offset).min(wrapped.lines().count().saturating_sub(1)))
                        .map(str::to_owned)
                        .collect()
                }
                Modal::SessionContainers(_, name, _, names, index,attach) => {
                    std::iter::once(format!("Pod {name} / {} / review required",if *attach{"attach"}else{"direct /bin/sh -i"}))
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
                Modal::Containers(target, names, index) => {
                    std::iter::once(format!(
                        " CONTAINER for {} | {} | Enter select / Esc cancel",
                        target.pod,
                        if target.follow {
                            "follow"
                        } else if target.previous {
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
                                format!(" {} {}", if i == *index { ">" } else { " " },if name=="*"{"ALL ordinary containers"}else{name})
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
                    let help = if *label == selections_ui::PICKER_LABEL {
                        " Enter open | d remove | Esc back".into()
                    } else {
                        format!(" {label} | arrows / Enter select / Esc cancel")
                    };
                    std::iter::once(help)
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
                let heading = helm_heading(line);
                let table_header = matches!(modal, Modal::Helm(..))
                    && line.contains("REV ")
                    && (line.contains("STATUS") || line.contains("STATE"));
                let warning = line.contains("DESTRUCTIVE")
                    || line.starts_with("ATTENTION:")
                    || line.starts_with("UNKNOWN")
                    || line.starts_with("  IMPACT");
                let line_fg = if selected {
                    color("selection_text")
                } else if warning {
                    color("warning")
                } else if line.contains("Sigstore verified")
                    || line.contains("OpenPGP verified")
                    || line.contains("OpenPGP + Sigstore verified")
                {
                    color("success")
                } else if line.contains("signature not checked") {
                    color("warning")
                } else if matches!(modal, Modal::WriteReview(_)) && line.starts_with("+ ") {
                    color("success")
                } else if matches!(modal, Modal::WriteReview(_)) && line.starts_with("- ") {
                    color("danger")
                } else if heading {
                    color("accent")
                } else if matches!(modal, Modal::Confirm(..) | Modal::SessionConfirm(..)) {
                    color("warning")
                } else {
                    fg
                };
                let line_bg = if selected {
                    color("selection_background")
                } else if table_header || line.trim_start().starts_with("01 ") {
                    raised
                } else {
                    modal_bg
                };
                cell(
                    out,
                    rect.x + 1,
                    rect.y + 1 + index as u16,
                    rect.width.saturating_sub(2),
                    line,
                    line_fg,
                    line_bg,
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
            } else if matches!(modal, Modal::YamlEditor(_)) {
                "Arrows move  Tab indent  Ctrl-S review  Ctrl-E $EDITOR  Esc cancel"
            } else if matches!(modal, Modal::WriteReview(_)) {
                "PgUp/PgDown read all changes  Type target  Enter apply  Esc back"
            } else if matches!(modal, Modal::WriteRunning) {
                "Operation in progress / wait for final outcome"
            } else if matches!(modal, Modal::Input("Command", _)) {
                "Esc cancel | Enter run | :pods :deploy :svc :nodes | :ns NAME | :sort FIELD"
            } else if matches!(modal, Modal::LiveOverview(..)) {
                "1..9 focus | Enter Pods | w warnings | ? keymap | Esc back"
            } else if matches!(modal, Modal::Overview(..)) {
                "Esc close | J redacted JSON | y YAML | PgUp/PgDown scroll"
            } else if matches!(modal, Modal::AdviceRunning) {
                "Ctrl-C cancel | waiting for advice | no automatic execution"
            } else if matches!(modal, Modal::AdviceDocument(..) | Modal::AdviceLoading) {
                "PgUp/PgDown scroll | Esc back"
            } else if matches!(modal, Modal::HelmForm(..)) {
                if w < 40 {
                    "Tab next  Enter review  Esc"
                } else if w < 80 {
                    "[Tab] next  [Enter] review  [Esc] back"
                } else {
                    " [Tab] next field   [Shift-Tab] previous   [Ctrl-U] clear   [Enter] prepare review   [Esc] back"
                }
            } else if matches!(modal, Modal::HelmRunning(..)) {
                "Ctrl-C request cancellation | wait for final outcome"
            } else if matches!(modal, Modal::HelmOutcome(..)) {
                "PgUp/PgDown scroll | Esc refreshes release inventory | never auto-retry UNKNOWN"
            } else if let Modal::Helm(table) = modal {
                table.help()
            } else if matches!(modal, Modal::HelmDocument(..)) {
                "Stored revision read | PgUp/PgDown scroll | Esc back"
            } else if matches!(modal, Modal::Subjects(..)) {
                "Enter bindings and rules | / filter | j k PgUp PgDn move | Esc back"
            } else if matches!(modal, Modal::Secrets(..)) {
                "Enter open keys | x reveal or hide one key (30s) | j k move | Esc back"
            } else if matches!(modal, Modal::PodContainers(..)) {
                "Enter logs | s shell | a attach | d describe | j k move | Esc back"
            } else if matches!(
                modal,
                Modal::Picker(..)
                    | Modal::Containers(..)
                    | Modal::SessionContainers(..)
                    | Modal::EditContainers(..)
            ) {
                "Enter choose | j k move | Esc cancel"
            } else if matches!(modal, Modal::Directory(..)) {
                "Enter open | j k move | Esc back"
            } else if matches!(modal, Modal::ForwardView) {
                "F new forward | s stop selected | j k move | Esc back"
            } else if matches!(modal, Modal::Themes(..)) {
                "Arrows preview | Enter save | N new custom theme | Esc restore"
            } else if matches!(modal, Modal::Detail(..) | Modal::Help(..)) {
                "PgUp PgDown scroll | : command | Esc close"
            } else if matches!(modal, Modal::Metrics(..)) {
                "Esc close | m refresh | M requests/limits (extra reads) | PgUp/PgDown"
            } else {
                "Esc close | Enter select | arrows / j k | PgUp PgDown scroll"
            };
            if matches!(modal, Modal::HelmForm(..)) {
                let hints: &[(&str, &str)] = if w < 40 {
                    &[("Tab", "next"), ("Enter", "review"), ("Esc", "")]
                } else if w < 60 {
                    &[("Tab", "next"), ("Enter", "review"), ("Esc", "back")]
                } else {
                    &[
                        ("Tab", "next field"),
                        ("Shift-Tab", "previous"),
                        ("Ctrl-U", "clear"),
                        ("Enter", "review"),
                        ("Esc", "back"),
                    ]
                };
                key_hints(out, h - 2, w, hints, [color("accent"), color("muted"), bg])
                    .map_err(|_| "draw failed")?;
            } else {
                row(out, h - 2, w, hints, color("muted"), bg).map_err(|_| "draw failed")?;
            }
            // Connection, mode and freshness already remain visible above the
            // modal. Reserve the last line for in-flight state and approvals.
            row(
                out,
                h - 1,
                w,
                if app.busy { "Request in progress" } else { "" },
                color("muted"),
                bg,
            )
            .map_err(|_| "draw failed")?;
            if let Modal::AdviceReview(review) = modal {
                render_advice_footer(out, review, w, h, [color("accent"), color("warning"), bg])
                    .map_err(|_| "draw failed")?;
            }
            if let Modal::WriteReview(review) = modal {
                render_write_footer(
                    out,
                    &review.view,
                    w,
                    h,
                    [color("accent"), color("warning"), bg],
                )
                .map_err(|_| "draw failed")?;
            }
            if let Modal::YamlEditor(editor) = modal {
                let (cx, cy) = editor.cursor();
                queue!(
                    out,
                    cursor::MoveTo(
                        (rect.x + 1 + cx as u16).min(w - 2),
                        (rect.y + 1 + cy as u16).min(h - 3)
                    ),
                    cursor::Show
                )
                .map_err(|_| "draw failed")?;
            } else {
                queue!(out, cursor::Hide).map_err(|_| "draw failed")?;
            }
            if let Modal::HelmReview(review) = modal {
                render_helm_review_footer(
                    out,
                    review,
                    w,
                    h,
                    [color("warning"), color("accent"), color("success"), bg],
                )
                .map_err(|_| "draw failed")?;
            }
        }
    }
    out.flush().map_err(|_| "draw failed".into())
}

fn confirmation_display(summary: &str, text: &str) -> Result<String> {
    safety::review_display(&format!(
        "{summary}\nCONFIRM > {text}_\nEnter execute | Esc cancel"
    ))
}
fn confirmation_lines(summary: &str, text: &str, width: u16) -> Vec<String> {
    let content = confirmation_display(summary, text)
        .unwrap_or_else(|error| format!("REVIEW BLOCKED\n\n{error}\n\nEsc returns."));
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
    confirmation_display(summary, text).is_ok()
        && width >= 28
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
    let (aliases, alias_error) = match crate::aliases::load() {
        Ok(aliases) => (aliases, None),
        Err(error) => (Default::default(), Some(error)),
    };
    let initial_selector = options.selector.clone();
    let worker = worker();
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
        selector: initial_selector,
        field_selector: String::new(),
        sort: Sort::Name,
        history: vec![],
        history_index: None,
        history_draft: String::new(),
        completion: None,
        command_cursor: 0,
        back: vec![],
        forward: vec![],
        marks: Default::default(),
        saved: vec![],
        aliases,
        known_namespaces: vec![],
        restoring_uid: None,
        subviews: vec![],
        log_target: None,
        helm_generation: 0,
        helm_cancel: None,
        advice_cancel: None,
        helm_return: None,
        helm_elapsed: 0,
        last_forward: worker.forward.text(),
        forward_selected: 0,
        desired_resource: None,
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
        last_table_metrics: None,
        table_metrics: Default::default(),
        metrics_resources: false,
        overview: None,
        selection_ui: Default::default(),
        pending_session: None,
        pending_editor: false,
        worker,
    };
    if first_run {
        themes::use_theme(themes::DEFAULT_THEME)?;
        app.theme_picker()?;
        app.status = "Welcome! Arrows preview, Enter chooses; Esc keeps Flexoki Dark.".into();
    }
    if let Some(error) = alias_error {
        app.modal = Modal::Detail(
            format!(
                "Custom aliases unavailable\n{error}\n\nBuilt-in navigation remains available. Esc closes this notice."
            ),
            0,
        );
    }
    app.connect();
    if app.options.dashboard_last && app.options.dashboard.is_empty() {
        match crate::dashboard_memory::load() {
            Ok(Some(last)) => {
                app.namespace = last.namespace;
                app.options.dashboard = last.contexts;
            }
            Ok(None) => {
                app.modal = Modal::Input(OVERVIEW_CONTEXTS_LABEL, app.context.clone());
                app.status = "Choose up to nine contexts for the dashboard; KBorg remembers them for next time.".into();
            }
            Err(error) => app.status = format!("Last dashboard unavailable: {error}"),
        }
    }
    draw(&mut app)?;
    let mut dirty = false;
    loop {
        if app.receive() {
            dirty = true;
        }
        if std::mem::take(&mut app.pending_editor)
            && let Modal::YamlEditor(editor) = &mut app.modal
        {
            let text = editor.text();
            drop(terminal_guard.take());
            let result =
                crate::external_editor::edit(&text, crate::manifest_edit::MAX_DOCUMENT as u64);
            terminal_guard = Some(Terminal::enter(app.options.mouse)?);
            if let Modal::YamlEditor(editor) = &mut app.modal {
                app.status = match result.and_then(|edited| editor.replace_text(&edited)) {
                    Ok(()) => {
                        "Draft updated from $EDITOR. Ctrl-S prepares the reviewed diff.".into()
                    }
                    Err(error) => error,
                };
            }
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
        // `--contexts`: open the dashboard once the startup connection settles,
        // whether or not the default context itself is reachable.
        if !app.options.dashboard.is_empty() && !app.busy && matches!(app.modal, Modal::None) {
            let names = std::mem::take(&mut app.options.dashboard);
            if let Err(error) = app.open_dashboard(names) {
                app.status = error;
            }
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
                Task::List(
                    app.kind(),
                    app.namespace.clone(),
                    app.selector.clone(),
                    app.field_selector.clone(),
                )
            } else {
                Task::Watch(
                    app.kind(),
                    app.namespace.clone(),
                    app.selector.clone(),
                    app.field_selector.clone(),
                )
            });
            dirty = true;
        }
        if app.connected
            && !app.busy
            && !app.stale
            && matches!(app.modal, Modal::None)
            && matches!(app.kind().as_str(), "pods" | "nodes")
            && app
                .last_table_metrics
                .is_none_or(|last| last.elapsed() >= Duration::from_secs(15))
        {
            app.refresh_table_metrics();
            dirty = true;
        }
        if let Modal::HelmRunning(_, started, _) = &app.modal {
            let elapsed = started.elapsed().as_secs();
            if elapsed != app.helm_elapsed {
                app.helm_elapsed = elapsed;
                dirty = true;
            }
        }
        // At most one redraw per event-loop tick; polling never blocks terminal input.
        if dirty {
            draw(&mut app)?;
            dirty = false;
        }
        if event::poll(Duration::from_millis(100)).map_err(|_| "input failed")? {
            match event::read().map_err(|_| "input failed")? {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                        && !matches!(app.modal, Modal::YamlEditor(_))
                    {
                        if matches!(app.modal, Modal::AdviceRunning) {
                            let _ = app.key_event(KeyCode::Char('c'), KeyModifiers::CONTROL);
                            dirty = true;
                            continue;
                        } else if matches!(app.modal, Modal::HelmRunning(..)) {
                            app.cancel_helm();
                            dirty = true;
                            continue;
                        }
                        if app.write_inflight {
                            app.status =
                                "Mutation in progress; wait for the result before quitting.".into();
                            dirty = true;
                            continue;
                        }
                        break;
                    }
                    if app.key_event(key.code, key.modifiers)? {
                        break;
                    }
                    dirty = true;
                }
                Event::Paste(text) => {
                    if let Modal::YamlEditor(editor) = &mut app.modal {
                        if let Err(error) = editor.paste(&text) {
                            app.status = error;
                        }
                        dirty = true;
                    }
                }
                Event::Resize(_, _) => dirty = true,
                Event::Mouse(mouse)
                    if matches!(app.modal, Modal::None | Modal::LiveOverview(..))
                        && !app.write_inflight =>
                {
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
        let forward = std::sync::Arc::new(ForwardSlot::default());
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
                    all_containers: false,
                    dynamic_membership: false,
                    dashboard: Vec::new(),
                    dashboard_last: false,
                    external_editor: false,
                    helm_storage: None,
                    helm_tool: None,
                    helm_values: None,
                    helm_registry_ca: None,
                    helm_registry_auth: None,
                    helm_keyring: None,
                    helm_sigstore_policy: None,
                    copilot_policy: None,
                    helm_timeout_seconds: 300,
                    allow_tools: false,
                    allow_extensions: false,
                    log_seconds: 3600,
                    interactive: false,
                    session_seconds: 900,
                    metrics_resources: false,
                    selector: String::new(),
                    log_read: crate::logs::ReadOptions::default(),
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
                selector: String::new(),
                field_selector: String::new(),
                sort: Sort::Name,
                history: vec![],
                history_index: None,
                history_draft: String::new(),
                completion: None,
                command_cursor: 0,
                back: vec![],
                forward: vec![],
                marks: Default::default(),
                saved: vec![],
                aliases: Default::default(),
                known_namespaces: vec![],
                restoring_uid: None,
                subviews: vec![],
                log_target: None,
                helm_generation: 0,
                helm_cancel: None,
                advice_cancel: None,
                helm_return: None,
                helm_elapsed: 0,
                last_forward: forward.text(),
                forward_selected: 0,
                desired_resource: None,
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
                last_table_metrics: None,
                table_metrics: Default::default(),
                metrics_resources: false,
                overview: None,
                selection_ui: Default::default(),
                pending_session: None,
                pending_editor: false,
                worker: Worker {
                    tx,
                    rx,
                    watch,
                    logs: std::sync::Arc::new(LogSlot::default()),
                    forward,
                },
            },
            replies,
            commands,
        )
    }
    #[test]
    fn copilot_closed_queue_and_late_previews_do_not_leave_loading_views() -> Result<()> {
        let (mut app, replies, commands) = harness();
        app.items = vec![
            serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"test","namespace":"test","uid":"test-uid"}}),
        ];
        drop(commands);
        app.command("copilot")?;
        assert!(!app.busy);
        assert!(
            matches!(app.modal, Modal::AdviceDocument(ref title,..) if title.contains("NOT STARTED"))
        );
        app.modal = Modal::AdviceLoading;
        app.key(KeyCode::Esc)?;
        replies
            .send((
                app.epoch,
                Reply::AdvicePrepared(Ok((None, "late preview".into()))),
            ))
            .map_err(|_| "reply closed")?;
        app.receive();
        assert!(matches!(app.modal, Modal::None));
        app.modal = Modal::AdviceLoading;
        replies
            .send((
                app.epoch - 1,
                Reply::AdvicePrepared(Ok((None, "stale preview".into()))),
            ))
            .map_err(|_| "reply closed")?;
        app.receive();
        assert!(matches!(app.modal, Modal::AdviceLoading));
        Ok(())
    }
    #[test]
    fn copilot_cancellation_waits_for_cleanup_before_unlocking_navigation() -> Result<()> {
        let (mut app, replies, _commands) = harness();
        let (cancel, mut receiver) = tokio::sync::oneshot::channel();
        app.advice_cancel = Some(cancel);
        app.modal = Modal::AdviceRunning;
        app.busy = true;
        app.write_inflight = true;
        app.command_epoch = Some(app.epoch);
        app.key_event(KeyCode::Char('c'), KeyModifiers::CONTROL)?;
        assert!(receiver.try_recv().is_ok());
        assert!(app.busy && app.write_inflight && matches!(app.modal, Modal::AdviceRunning));
        app.key(KeyCode::Esc)?;
        assert!(matches!(app.modal, Modal::AdviceRunning));
        replies
            .send((
                app.epoch,
                Reply::AdviceOutcome(Err("Canceled after cleanup".into())),
            ))
            .map_err(|_| "reply closed")?;
        app.receive();
        assert!(!app.busy && !app.write_inflight && app.advice_cancel.is_none());
        assert!(
            matches!(app.modal,Modal::AdviceDocument(ref title,..) if title.contains("STOPPED"))
        );
        Ok(())
    }
    fn demo_helm_prepared() -> Result<Box<crate::helm_lifecycle::Prepared>> {
        crate::cli::runtime()?.block_on(async {
            let gateway = Gateway::connect(Connection {
                demo: true,
                writable: true,
                allow_helm: true,
                allow_helm_write: true,
                ..Default::default()
            })
            .await?;
            crate::helm_lifecycle::prepare(
                &gateway,
                crate::helm_lifecycle::Options {
                    executable: "/demo/helm".into(),
                    namespace: "demo".into(),
                    release: "demo-new".into(),
                    storage: crate::helm::Storage::Secret,
                    seconds: 300,
                    allow_hooks: false,
                    allow_crds: false,
                    registry: Default::default(),
                    keyring: None,
                    sigstore_policy: None,
                },
                crate::helm_lifecycle::Request::Install {
                    chart: "/demo/chart.tgz".into(),
                    values: None,
                },
            )
            .await
            .map(Box::new)
        })
    }
    #[test]
    fn chart_crd_command_requires_explicit_flag_and_propagates_registry_policy() -> Result<()> {
        let (mut app, _, mut commands) = harness();
        app.options.connection.demo = true;
        app.options.connection.writable = true;
        app.options.connection.allow_helm = true;
        app.options.connection.allow_helm_write = true;
        app.command("helm-crds")?;
        assert!(matches!(app.modal, Modal::None));
        assert!(app.status.contains("--allow-helm-crds"));
        app.options.connection.allow_helm_crds = true;
        app.options.connection.allow_helm_hooks = true;
        app.options.helm_registry_ca = Some("/private/registry-ca.pem".into());
        app.options.helm_registry_auth = Some("/private/registry-auth.json".into());
        app.command("helm-crds")?;
        let Modal::HelmForm(form) = &mut app.modal else {
            return Err("missing chart CRD form".into());
        };
        form.fields[1].1 = "my.api".into();
        let draft = form.draft("/demo/helm".into(), 300)?;
        assert!(draft.options.allow_hooks && draft.options.allow_crds);
        assert_eq!(draft.options.registry.ca_file, app.options.helm_registry_ca);
        assert_eq!(
            draft.options.registry.auth_file,
            app.options.helm_registry_auth
        );
        assert!(matches!(
            draft.request,
            crate::helm_lifecycle::Request::InstallCrds { .. }
        ));
        assert!(commands.try_recv().is_err());
        Ok(())
    }
    #[test]
    fn helm_install_inherits_visible_release_table_scope() -> Result<()> {
        let (mut app, _, mut commands) = harness();
        app.options.connection.demo = true;
        app.options.connection.writable = true;
        app.options.connection.allow_helm = true;
        app.options.connection.allow_helm_write = true;
        app.namespace = "default".into();
        app.options.helm_storage = Some(crate::helm::Storage::Secret);
        app.modal = Modal::Helm(Box::new(helm_ui::Table::new(
            crate::helm::Snapshot {
                releases: vec![],
                truncated: false,
                issues: vec![],
                source: "test".into(),
            },
            None,
            "staging".into(),
            Some(crate::helm::Storage::ConfigMap),
        )));
        app.key(KeyCode::Char('i'))?;
        let Modal::HelmForm(mut form) = std::mem::replace(&mut app.modal, Modal::None) else {
            return Err("missing install form".into());
        };
        assert_eq!(form.fields[0].1, "staging");
        form.fields[1].1 = "new-release".into();
        let draft = form.draft("/demo/helm".into(), 300)?;
        assert_eq!(draft.options.namespace, "staging");
        assert_eq!(draft.options.storage, crate::helm::Storage::ConfigMap);
        app.subviews.clear();
        app.begin_helm_form(helm_ui::Intent::Install);
        assert!(matches!(app.modal,Modal::HelmForm(ref form) if form.fields[0].1=="default"));
        assert!(commands.try_recv().is_err());
        Ok(())
    }
    #[test]
    fn schema_commands_require_capabilities_and_keep_visible_helm_scope() -> Result<()> {
        let (mut app, _, mut commands) = harness();
        app.options.connection.demo = true;
        app.options.connection.writable = true;
        app.options.connection.allow_helm = true;
        app.options.connection.allow_helm_write = true;
        app.command("helm-crds-delete")?;
        assert!(matches!(app.modal, Modal::None));
        assert!(app.status.contains("--allow-helm-crds"));
        app.options.connection.allow_helm_crds = true;
        app.options.helm_keyring = Some("/private/trusted.gpg".into());
        app.subviews.push(Modal::Helm(Box::new(helm_ui::Table::new(
            crate::helm::Snapshot {
                releases: vec![],
                truncated: false,
                issues: vec![],
                source: "test".into(),
            },
            None,
            "staging".into(),
            Some(crate::helm::Storage::ConfigMap),
        ))));
        app.command("helm-crds-upgrade")?;
        let Modal::HelmForm(form) = &mut app.modal else {
            return Err("schema upgrade form missing".into());
        };
        assert_eq!(form.fields[0].1, "staging");
        assert!(matches!(form.intent, helm_ui::Intent::UpgradeCrds));
        form.fields[1].1 = "schema-owner".into();
        let draft = form.draft("/demo/helm".into(), 300)?;
        assert_eq!(draft.options.storage, crate::helm::Storage::ConfigMap);
        assert_eq!(draft.options.keyring, app.options.helm_keyring);
        assert!(matches!(
            draft.request,
            crate::helm_lifecycle::Request::UpgradeCrds { .. }
        ));
        assert!(
            commands.try_recv().is_err(),
            "forms never enqueue writes before review"
        );
        Ok(())
    }
    #[test]
    fn helm_status_tracks_current_draft_review_and_execution() -> Result<()> {
        let (mut app, replies, mut commands) = harness();
        app.options.connection.demo = true;
        app.options.connection.writable = true;
        app.options.connection.allow_helm = true;
        app.options.connection.allow_helm_write = true;
        app.status = "HELM OPERATION SUCCEEDED".into();
        app.sticky_status = true;
        app.begin_helm_form(helm_ui::Intent::Install);
        assert_eq!(app.status, "Helm draft; no execution requested.");
        assert!(!app.sticky_status);
        let Modal::HelmForm(form) = &mut app.modal else {
            return Err("missing Helm draft".into());
        };
        form.fields[0].1 = "demo".into();
        form.fields[1].1 = "demo-new".into();
        app.key_with_approval_size(KeyCode::Enter, || Ok((80, 24)))?;
        assert_eq!(app.status, "Preparing Helm review; no execution requested.");
        assert!(matches!(
            commands.try_recv(),
            Ok((_, Task::HelmPrepare(..)))
        ));
        replies
            .send((
                app.epoch,
                Reply::HelmPrepared(app.helm_generation, Ok(demo_helm_prepared()?)),
            ))
            .map_err(|_| "reply closed")?;
        app.receive();
        assert_eq!(app.status, "Helm review ready; no execution requested.");
        let Modal::HelmReview(review) = &mut app.modal else {
            return Err("missing Helm review".into());
        };
        review.typed = review.prepared.confirmation();
        review.mark_drawn(78, 500);
        app.key_with_approval_size(KeyCode::Enter, || Ok((80, 24)))?;
        assert!(app.write_inflight);
        assert_eq!(
            app.status,
            "Helm execution in progress; Ctrl-C requests cancellation."
        );
        assert!(!screen(&app, 80, 24).join("\n").contains("SUCCEEDED"));
        Ok(())
    }
    #[test]
    fn helm_lifecycle_gates_cancel_review_stale_scope_and_unknown_outcome() -> Result<()> {
        let (mut app, replies, mut commands) = harness();
        app.begin_helm_form(helm_ui::Intent::Install);
        assert!(matches!(app.modal, Modal::None));
        assert!(app.status.contains("--allow-helm-write"));
        assert!(commands.try_recv().is_err());
        app.options.connection.demo = true;
        app.options.connection.writable = true;
        app.options.connection.allow_helm = true;
        app.options.connection.allow_helm_write = true;
        app.begin_helm_form(helm_ui::Intent::Install);
        assert!(matches!(app.modal, Modal::HelmForm(_)));
        app.key(KeyCode::Esc)?;
        assert!(commands.try_recv().is_err());
        app.modal = Modal::HelmReview(Box::new(helm_ui::Review::new(
            demo_helm_prepared()?,
            app.epoch,
        )));
        assert!(screen(&app, 28, 10).join("\n").contains("unread"));
        app.key_with_approval_size(KeyCode::Enter, || Ok((80, 24)))?;
        assert!(commands.try_recv().is_err());
        if let Modal::HelmReview(review) = &mut app.modal {
            review.typed = review.prepared.confirmation();
            review.mark_drawn(78, 500);
            review.epoch = app.epoch - 1;
        }
        app.key_with_approval_size(KeyCode::Enter, || Ok((80, 24)))?;
        assert!(commands.try_recv().is_err());
        if let Modal::HelmReview(review) = &mut app.modal {
            review.epoch = app.epoch;
        }
        app.helm_return = Some(("demo".into(), crate::helm::Storage::Secret));
        app.key_with_approval_size(KeyCode::Enter, || Ok((80, 24)))?;
        assert!(app.write_inflight && matches!(app.modal, Modal::HelmRunning(..)));
        let (generation, mut cancel) = match commands.try_recv() {
            Ok((_, Task::HelmExecute(_, typed, generation, cancel))) => {
                assert_eq!(typed, "demo/demo-new");
                (generation, cancel)
            }
            _ => return Err("missing reviewed Helm execution".into()),
        };
        app.key_event(KeyCode::Char('c'), KeyModifiers::CONTROL)?;
        assert!(cancel.try_recv().is_ok());
        assert!(app.write_inflight);
        replies
            .send((
                app.epoch,
                Reply::HelmOutcome(
                    generation,
                    Ok(crate::helm_lifecycle::Outcome {
                        success: false,
                        summary: "UNKNOWN: verify release state before retrying".into(),
                        uncertain: true,
                    }),
                ),
            ))
            .map_err(|_| "reply closed")?;
        app.receive();
        assert!(!app.write_inflight && !app.busy);
        assert!(matches!(app.modal,Modal::HelmOutcome(ref title,..) if title.contains("UNKNOWN")));
        assert!(screen(&app, 80, 24).join("\n").contains("UNKNOWN"));
        app.key(KeyCode::Esc)?;
        assert!(
            matches!(commands.try_recv(),Ok((_,Task::Helm(helm_ui::Request::List(namespace,Some(crate::helm::Storage::Secret)),_))) if namespace=="demo")
        );
        Ok(())
    }
    #[test]
    fn unavailable_helm_queue_never_leaves_a_running_operation_modal() -> Result<()> {
        let (mut app, _, commands) = harness();
        drop(commands);
        let mut review = helm_ui::Review::new(demo_helm_prepared()?, app.epoch);
        review.typed = review.prepared.confirmation();
        review.mark_drawn(78, 500);
        app.modal = Modal::HelmReview(Box::new(review));
        app.key_with_approval_size(KeyCode::Enter, || Ok((80, 24)))?;
        assert!(!app.write_inflight && !app.busy);
        assert!(
            matches!(app.modal,Modal::HelmOutcome(ref title,..) if title.contains("NOT STARTED"))
        );
        Ok(())
    }
    #[test]
    fn approval_enter_requires_known_painted_dimensions_before_export_or_dispatch() -> Result<()> {
        struct Directory(std::path::PathBuf);
        impl Drop for Directory {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "test clock unavailable")?
            .as_nanos();
        let path = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "test temporary root unavailable")?
            .join(format!("kborg-export-size-{}-{stamp}", std::process::id()));
        std::fs::create_dir(&path).map_err(|_| "test directory creation failed")?;
        let directory = Directory(path);
        safety::ensure_private_dir(&directory.0)?;
        let (mut app, _, mut commands) = harness();
        for (width, height) in [(20, 100), (27, 100), (120, 9)] {
            let draft = crate::exports::fixture_at(&directory.0.join("exports"))?;
            let destination = draft.path().to_owned();
            app.modal = Modal::Export(draft);
            let painted = screen(&app, width, height).join("\n");
            assert!(painted.contains("needs 28x10"));
            assert!(!painted.contains("File (escaped)"));
            app.key_with_approval_size(KeyCode::Enter, || Ok((width, height)))?;
            assert!(!destination.exists());
            assert!(matches!(app.modal, Modal::Export(_)));
            assert!(commands.try_recv().is_err());
        }
        let draft = crate::exports::fixture_at(&directory.0.join("exports"))?;
        let destination = draft.path().to_owned();
        app.modal = Modal::Export(draft);
        assert!(
            app.key_with_approval_size(KeyCode::Enter, || Err(io::Error::other("fixture")))
                .is_err()
        );
        assert!(!destination.exists());
        assert!(!directory.0.join("exports").exists());
        let draft = crate::exports::fixture_at(&directory.0.join("exports"))?;
        let destination = draft.path().to_owned();
        app.modal = Modal::Export(draft);
        assert!(screen(&app, 120, 34).join("\n").contains("File (escaped)"));
        app.key_with_approval_size(KeyCode::Enter, || Ok((120, 34)))?;
        assert_eq!(
            std::fs::read_to_string(destination).map_err(|_| "test export unavailable")?,
            "kind: Pod\n"
        );
        let mut review = helm_ui::Review::new(demo_helm_prepared()?, app.epoch);
        review.typed = review.prepared.confirmation();
        review.mark_drawn(78, 500);
        app.modal = Modal::HelmReview(Box::new(review));
        assert!(
            app.key_with_approval_size(KeyCode::Enter, || Err(io::Error::other("fixture")))
                .is_err()
        );
        assert!(!app.write_inflight);
        assert!(commands.try_recv().is_err());
        Ok(())
    }
    #[test]
    fn queued_helm_execution_rejects_stale_epoch_before_running() -> Result<()> {
        let worker = worker();
        worker.watch.invalidate(99);
        let (_, cancel) = tokio::sync::oneshot::channel();
        worker
            .tx
            .blocking_send((
                12,
                Task::HelmExecute(demo_helm_prepared()?, "demo/demo-new".into(), 5, cancel),
            ))
            .map_err(|_| "queue closed")?;
        let (_, reply) = worker
            .rx
            .recv_timeout(Duration::from_secs(2))
            .map_err(|_| "worker timeout")?;
        assert!(matches!(reply,Reply::HelmOutcome(5,Err(error)) if error.contains("stale")));
        Ok(())
    }
    #[test]
    fn group_log_title_keeps_range_visible_before_long_workload_identity() -> Result<()> {
        let (mut app, _, _) = harness();
        let namespace = format!("kborg-{}", "a".repeat(56));
        let membership = crate::logs::LogMembership::new(
            &namespace,
            format!(
                "workload deployments.apps / ui-lifecycle-worker UID {}; fixed membership",
                "1".repeat(36)
            ),
            ["one", "two"]
                .into_iter()
                .map(|name| crate::logs::LogPod {
                    name: name.into(),
                    uid: format!("uid-{name}"),
                    containers: vec!["worker".into()],
                })
                .collect(),
            vec![],
            "test-connection",
        )?;
        app.log_target = Some(LogTarget {
            namespace,
            pod: "ui-lifecycle-worker".into(),
            container: None,
            uid: "workload-uid".into(),
            previous: false,
            follow: true,
            all: true,
            dynamic: false,
            membership: Some(membership),
        });
        let view = crate::ui_logs::LogView::new(
            "[Log stream merge fixed membership]\nrecord".into(),
            crate::logs::ReadOptions::for_key('3').ok_or("missing range")?,
        );
        app.modal = Modal::Follow(Box::new(view));
        for width in [48, 80, 140] {
            let frame = screen(&app, width, 12);
            let title = &frame[3];
            for expected in ["FIXED 2 Pods", "2 containers", "last 300s"] {
                assert!(
                    title.contains(expected),
                    "{width}: missing {expected}: {title}"
                );
            }
            assert!(!title.contains("111111111111111111111111111111111111"));
        }
        Ok(())
    }
    #[test]
    fn logs_live_is_an_explicit_dynamic_opt_in_and_rejects_head_snapshots() {
        let (mut app, _replies, mut commands) = harness();
        app.options.connection.logs = true;
        app.namespace = "default".into();
        assert!(app.command("logs app=web").is_ok());
        match commands.try_recv() {
            Ok((_, Task::CaptureLogs(target, _, _))) => assert!(!target.dynamic),
            _ => panic!("fixed capture missing"),
        }
        app.busy = false;
        app.command_epoch = None;
        assert!(app.command("logs-live app=web").is_ok());
        match commands.try_recv() {
            Ok((
                _,
                Task::CaptureLogs(target, crate::logs::PodLogSelection::Selector(selector), _),
            )) => {
                assert!(target.dynamic && target.follow && !target.previous);
                assert_eq!(selector, "app=web");
            }
            _ => panic!("dynamic capture missing"),
        }
        app.busy = false;
        app.command_epoch = None;
        app.options.log_read.head = true;
        assert!(app.command("logs-live app=web").is_ok());
        assert!(
            commands.try_recv().is_err(),
            "head snapshots cannot be dynamic"
        );
        assert!(app.status.contains("not head"));
    }
    #[test]
    fn workload_logs_capture_once_and_range_reload_retains_exact_pod_membership() -> Result<()> {
        let (object, membership) = crate::cli::runtime()?.block_on(async {
            let gateway = Gateway::connect(Connection {
                demo: true,
                logs: true,
                ..Default::default()
            })
            .await?;
            let object = gateway.get("deployments.apps", "demo", "api").await?;
            let uid = object["metadata"]["uid"]
                .as_str()
                .ok_or("demo workload UID missing")?
                .to_owned();
            let membership = gateway
                .capture_log_membership(
                    "demo",
                    crate::logs::PodLogSelection::Workload {
                        resource: "deployments.apps".into(),
                        name: "api".into(),
                        uid,
                    },
                )
                .await?;
            Ok::<_, String>((object, membership))
        })?;
        let (mut app, replies, mut commands) = harness();
        app.options.connection.logs = true;
        app.kind_index = app.resolve_resource("deployments.apps")?;
        app.items = vec![object];
        app.request_logs(false, true);
        let (target, generation) = match commands.try_recv() {
            Ok((
                _,
                Task::CaptureLogs(
                    target,
                    crate::logs::PodLogSelection::Workload { resource, .. },
                    generation,
                ),
            )) => {
                assert_eq!(resource, "deployments.apps");
                (target, generation)
            }
            _ => return Err("missing fixed membership capture".into()),
        };
        app.items[0]["metadata"]["uid"] = "replacement".into();
        let mut target = target;
        target.membership = Some(membership.clone());
        replies
            .send((app.epoch, Reply::Membership(target, generation)))
            .map_err(|_| "reply closed")?;
        app.receive();
        let initial = match commands.try_recv() {
            Ok((_, Task::Follow(target, _, _, _))) => target.membership.ok_or("membership lost")?,
            _ => return Err("missing captured follow".into()),
        };
        assert_eq!(initial.pods()[0].uid, membership.pods()[0].uid);
        app.busy = false;
        app.key(KeyCode::Char('3'))?;
        match commands.try_recv() {
            Ok((_, Task::Follow(target, _, _, read))) => {
                assert_eq!(
                    read,
                    crate::logs::ReadOptions::for_key('3').ok_or("range missing")?
                );
                assert_eq!(
                    target.membership.ok_or("range lost membership")?.pods()[0].uid,
                    initial.pods()[0].uid
                );
            }
            _ => return Err("range recaptured membership unexpectedly".into()),
        };
        assert!(screen(&app, 120, 24).join("\n").contains("FIXED"));
        Ok(())
    }
    #[test]
    fn helm_reads_require_capability_and_late_results_do_not_reopen_views() -> Result<()> {
        let (mut app, replies, mut commands) = harness();
        app.command("helm")?;
        assert!(commands.try_recv().is_err());
        assert!(app.status.contains("--allow-helm"));
        app.options.connection.allow_helm = true;
        app.command("helm")?;
        let generation = match commands.try_recv() {
            Ok((_, Task::Helm(helm_ui::Request::List(ns, None), generation))) => {
                assert_eq!(ns, "test");
                generation
            }
            _ => return Err("missing Helm read".into()),
        };
        assert!(matches!(app.modal, Modal::HelmLoading(_)));
        app.key(KeyCode::Esc)?;
        replies
            .send((
                app.epoch,
                Reply::Helm(
                    generation,
                    Ok(helm_ui::Response::Document(
                        "old".into(),
                        "late secret read".into(),
                    )),
                ),
            ))
            .map_err(|_| "reply closed")?;
        app.receive();
        assert!(matches!(app.modal, Modal::None));
        assert!(!screen(&app, 80, 24).join("\n").contains("late secret read"));
        app.command("history my.api")?;
        assert!(
            matches!(commands.try_recv(),Ok((_,Task::Helm(helm_ui::Request::NamedHistory(_,name,None),_))) if name=="my.api")
        );
        Ok(())
    }
    #[test]
    fn pod_log_defaults_and_source_toggle_preserve_captured_identity() -> Result<()> {
        let (mut app, _replies, mut commands) = harness();
        app.options.connection.logs = true;
        let mut pod = object("api");
        pod["apiVersion"] = "v1".into();
        pod["kind"] = "Pod".into();
        pod["spec"] = serde_json::json!({"containers":[{"name":"app"},{"name":"sidecar"}],"initContainers":[{"name":"init"}]});
        app.items = vec![pod.clone()];
        app.subviews
            .push(Modal::Follow(Box::new(crate::ui_logs::LogView::new(
                "older session".into(),
                Default::default(),
            ))));
        app.request_logs(false, true);
        assert!(
            !app.subviews
                .iter()
                .any(|modal| matches!(modal, Modal::Follow(_)))
        );
        let uid = match commands.try_recv() {
            Ok((_, Task::Follow(target, _, _, _))) => {
                assert!(target.all);
                assert_eq!(target.container.as_deref(), Some("app"));
                target.uid
            }
            _ => return Err("missing merged follow".into()),
        };
        app.busy = false;
        app.items[0]["metadata"]["uid"] = "replacement".into();
        app.key(KeyCode::Char('a'))?;
        match commands.try_recv() {
            Ok((_, Task::Follow(target, _, _, _))) => {
                assert!(!target.all);
                assert_eq!(target.uid, uid);
                assert_eq!(target.container.as_deref(), Some("app"));
            }
            _ => return Err("missing single follow".into()),
        };
        app.busy = false;
        app.key(KeyCode::Esc)?;
        pod["metadata"]["annotations"] =
            serde_json::json!({"kubectl.kubernetes.io/default-container":"sidecar"});
        app.items = vec![pod];
        app.request_logs(false, true);
        assert!(
            matches!(commands.try_recv(),Ok((_,Task::Follow(target,_,_,_))) if !target.all && target.container.as_deref()==Some("sidecar"))
        );
        assert_eq!(
            pod_log_choices(&app.items[0]),
            vec!["*", "app", "sidecar", "init"]
        );
        Ok(())
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
                    b'h' | b'l' if parameters == "?25" => {}
                    other => panic!("unexpected terminal command: {other}"),
                }
            } else {
                let Some(character) = std::str::from_utf8(&output[index..])
                    .ok()
                    .and_then(|text| text.chars().next())
                else {
                    panic!("expected a valid terminal UTF-8 character");
                };
                assert!(
                    character.is_ascii_graphic()
                        || character == ' '
                        || matches!(character, '╭' | '╮' | '╰' | '╯' | '─' | '│')
                );
                assert!(
                    x < usize::from(width) && y < usize::from(height),
                    "paint outside {width}x{height}: {x},{y}"
                );
                cells[y][x] = character;
                painted[y][x] = true;
                x += 1;
                index += character.len_utf8() - 1;
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
    fn destination_only_resource_command_waits_for_destination_discovery() {
        let (mut app, replies, mut commands) = harness();
        app.options.connection.demo = true;
        assert!(app.command("widget team-cmd app=api @demo").is_ok());
        assert_eq!(app.desired_resource.as_deref(), Some("widget"));
        assert_eq!(
            (app.namespace.as_str(), app.selector.as_str()),
            ("team-cmd", "app=api")
        );
        assert!(matches!(commands.try_recv(), Ok((_, Task::Connect(_)))));
        app.options.connection.demo = false;
        assert!(
            replies
                .send((
                    app.epoch,
                    Reply::Connected(
                        "demo".into(),
                        "server".into(),
                        "wrong-default".into(),
                        crate::gateway::kinds()
                    )
                ))
                .is_ok()
        );
        app.receive();
        assert_eq!(app.kind(), "widget");
        assert!(app.watch_blocked);
        assert!(matches!(commands.try_recv(), Ok((_, Task::Discover))));
        let mut widget = app.kinds[0].clone();
        widget.plural = "widgets".into();
        widget.kind = "Widget".into();
        widget.group = "example.test".into();
        let mut catalog = crate::gateway::kinds();
        catalog.push(widget);
        assert!(replies.send((app.epoch, Reply::Catalog(catalog))).is_ok());
        app.receive();
        assert_eq!(app.kind(), "widgets.example.test");
        assert_eq!(app.namespace, "team-cmd");
        assert_eq!(app.selector, "app=api");
        assert!(!app.watch_blocked);
        app.connected = false;
        app.options.connection.context = Some("failed-context".into());
        let mut prior = app.view_state();
        prior.context = "demo".into();
        app.back.push(prior);
        app.go_back();
        match commands.try_recv() {
            Ok((_, Task::Connect(connection))) => {
                assert_eq!(connection.context.as_deref(), Some("demo"))
            }
            _ => panic!("failed connection history did not reconnect destination"),
        }
    }
    #[test]
    fn custom_kind_collisions_use_inspection_instead_of_builtin_drilldowns() {
        for (plural, kind) in [
            ("nodes", "Node"),
            ("pods", "Pod"),
            ("namespaces", "Namespace"),
            ("deployments", "Deployment"),
        ] {
            let (mut app, _replies, mut commands) = harness();
            let mut custom = app.kinds[0].clone();
            custom.plural = plural.into();
            custom.kind = kind.into();
            custom.group = "example.test".into();
            app.kinds.push(custom.clone());
            app.kind_index = app.kinds.len() - 1;
            app.items = vec![
                serde_json::json!({"apiVersion":"example.test/v1","kind":kind,"metadata":{"name":"custom-a","namespace":"test","uid":"custom-uid"},"spec":{"selector":{"matchLabels":{"app":"api"}},"nodeName":"worker-a"}}),
            ];
            assert!(app.key(KeyCode::Enter).is_ok());
            match commands.try_recv() {
                Ok((_, Task::Inspect(resource, _, _))) => assert_eq!(resource, custom.key()),
                _ => panic!("custom kind entered a built-in subview"),
            }
            assert_eq!(app.kind(), custom.key());
            assert!(app.field_selector.is_empty());
        }
    }
    #[test]
    fn history_restores_destination_crd_catalog_and_keeps_unavailable_entries() {
        let (mut app, replies, mut commands) = harness();
        let mut widget = app.kinds[0].clone();
        widget.plural = "widgets".into();
        widget.kind = "Widget".into();
        widget.group = "a.test".into();
        let mut gadget = widget.clone();
        gadget.plural = "gadgets".into();
        gadget.kind = "Gadget".into();
        gadget.group = "b.test".into();
        app.context = "B".into();
        app.namespace = "team-b".into();
        app.kinds.push(gadget.clone());
        app.kind_index = app.kinds.len() - 1;
        app.filter = "gadget".into();
        app.selector = "app=b".into();
        app.items = vec![
            serde_json::json!({"kind":"Gadget","metadata":{"name":"gadget-b","namespace":"team-b","uid":"b-selected"}}),
        ];
        app.back.push(ViewState {
            resource: widget.key(),
            namespace: "team-a".into(),
            context: "A".into(),
            filter: "widget".into(),
            selector: "app=a".into(),
            field_selector: String::new(),
            selected_uid: Some("a-selected".into()),
        });
        fn destination(
            app: &mut App,
            replies: &mpsc::SyncSender<(u64, Reply)>,
            commands: &mut tokio::sync::mpsc::Receiver<(u64, Task)>,
            context: &str,
            kind: Kind,
        ) {
            match commands.try_recv() {
                Ok((_, Task::Connect(connection))) => {
                    assert_eq!(connection.context.as_deref(), Some(context))
                }
                _ => panic!("history did not connect to destination"),
            }
            assert!(
                replies
                    .send((
                        app.epoch,
                        Reply::Connected(
                            context.into(),
                            "server".into(),
                            "wrong-default".into(),
                            crate::gateway::kinds()
                        )
                    ))
                    .is_ok()
            );
            app.receive();
            assert_eq!(app.kind(), kind.key());
            assert!(app.desired_resource.is_some());
            assert!(matches!(commands.try_recv(), Ok((_, Task::Discover))));
            let mut catalog = crate::gateway::kinds();
            catalog.push(kind.clone());
            assert!(replies.send((app.epoch, Reply::Catalog(catalog))).is_ok());
            app.receive();
            assert_eq!(app.kind(), kind.key());
            assert!(app.desired_resource.is_none());
        }
        app.go_back();
        assert_eq!(app.kind(), widget.key());
        assert_eq!((app.back.len(), app.forward.len()), (0, 1));
        destination(&mut app, &replies, &mut commands, "A", widget);
        assert_eq!(
            (
                app.namespace.as_str(),
                app.filter.as_str(),
                app.selector.as_str()
            ),
            ("team-a", "widget", "app=a")
        );
        app.replace_items(vec![serde_json::json!({"kind":"Widget","metadata":{"name":"widget-a","namespace":"team-a","uid":"a-selected"}})]);
        assert_eq!(app.selected_uid().as_deref(), Some("a-selected"));
        app.go_forward();
        assert_eq!(app.kind(), gadget.key());
        destination(&mut app, &replies, &mut commands, "B", gadget);
        assert_eq!(
            (
                app.namespace.as_str(),
                app.filter.as_str(),
                app.selector.as_str()
            ),
            ("team-b", "gadget", "app=b")
        );
        let mut unavailable = app.view_state();
        unavailable.resource = "missing.b.test".into();
        app.back.push(unavailable.clone());
        app.forward.push(unavailable);
        let stacks = (app.back.len(), app.forward.len());
        let resource = app.kind();
        app.go_back();
        assert_eq!((app.back.len(), app.forward.len()), stacks);
        assert_eq!(app.kind(), resource);
        app.go_forward();
        assert_eq!((app.back.len(), app.forward.len()), stacks);
        assert_eq!(app.kind(), resource);
    }
    fn forward_target(local_port: u16) -> crate::ops::ForwardTarget {
        crate::ops::ForwardTarget {
            namespace: "test".into(),
            pod: "server".into(),
            local_port,
            remote_port: 80,
            seconds: 300,
        }
    }
    #[test]
    fn queued_forward_cannot_recreate_canceled_or_different_scope_session() {
        let slot = ForwardSlot::default();
        let watch = WatchSlot::default();
        watch.invalidate(12);
        let generation = slot.generation();
        slot.cancel();
        assert!(
            slot.begin(generation, 12, &watch, forward_target(8080))
                .is_err()
        );
        let generation = slot.generation();
        watch.invalidate(13);
        assert!(
            slot.begin(generation, 12, &watch, forward_target(8080))
                .is_err()
        );
        let (id, mut receiver) = slot
            .begin(generation, 13, &watch, forward_target(8080))
            .unwrap_or_else(|e| panic!("{e}"));
        slot.cancel();
        assert!(matches!(
            receiver.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
        assert!(slot.listening(id, "late bind").is_err());
        assert!(
            slot.begin(generation, 13, &watch, forward_target(8081))
                .is_err()
        );
        assert_eq!(
            slot.active_count(),
            1,
            "stopping remains active until cleanup completes"
        );
        slot.finish(id, Ok("closed".into()));
        assert_eq!(slot.active_count(), 0);
    }
    #[test]
    fn forwards_stop_independently_and_reserve_ports_until_cleanup() {
        let slot = ForwardSlot::default();
        let watch = WatchSlot::default();
        let generation = slot.generation();
        let (a, mut a_cancel) = slot
            .begin(generation, 0, &watch, forward_target(8080))
            .unwrap_or_else(|e| panic!("{e}"));
        let (b, mut b_cancel) = slot
            .begin(generation, 0, &watch, forward_target(8081))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(slot.listening(a, "A bound").is_ok());
        assert!(slot.listening(b, "B bound").is_ok());
        assert_eq!(slot.active_count(), 2);
        assert!(slot.cancel_selected(0));
        assert!(matches!(
            a_cancel.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
        assert!(matches!(
            b_cancel.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert!(
            slot.begin(generation, 0, &watch, forward_target(8080))
                .is_err()
        );
        slot.finish(a, Ok("A closed".into()));
        let (c, _c_cancel) = slot
            .begin(generation, 0, &watch, forward_target(8080))
            .unwrap_or_else(|e| panic!("{e}"));
        assert_ne!(a, c);
        assert_eq!(slot.active_count(), 2);
        assert!(slot.lines(1, 120, 30).join("\n").contains("B bound"));
        slot.cancel();
        assert!(matches!(
            b_cancel.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
    }
    #[test]
    fn concurrent_forward_reservations_cannot_claim_the_same_listener() {
        let slot = std::sync::Arc::new(ForwardSlot::default());
        let watch = std::sync::Arc::new(WatchSlot::default());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let attempts = (0..8)
            .map(|_| {
                let (slot, watch, barrier) = (slot.clone(), watch.clone(), barrier.clone());
                std::thread::spawn(move || {
                    barrier.wait();
                    slot.begin(0, 0, &watch, forward_target(8080)).is_ok()
                })
            })
            .collect::<Vec<_>>();
        let winners = attempts
            .into_iter()
            .map(|attempt| attempt.join().unwrap_or(false))
            .filter(|won| *won)
            .count();
        assert_eq!(winners, 1);
        assert_eq!(slot.active_count(), 1);
    }
    #[test]
    fn forwards_have_bounded_history_and_stale_ids_cannot_overwrite_new_sessions() {
        let slot = ForwardSlot::default();
        let watch = WatchSlot::default();
        let generation = slot.generation();
        let mut handles = Vec::new();
        for port in 8080..8088 {
            handles.push(
                slot.begin(generation, 0, &watch, forward_target(port))
                    .unwrap_or_else(|e| panic!("{e}")),
            );
        }
        assert_eq!(slot.active_count(), MAX_FORWARDS);
        assert!(
            slot.begin(generation, 0, &watch, forward_target(8090))
                .is_err()
        );
        let old = handles[0].0;
        slot.finish(old, Ok("old ended".into()));
        let (new, _cancel) = slot
            .begin(generation, 0, &watch, forward_target(8090))
            .unwrap_or_else(|e| panic!("{e}"));
        slot.finish(old, Err("stale failure".into()));
        assert!(slot.listening(old, "stale bind").is_err());
        assert_eq!(slot.len(), MAX_FORWARDS);
        assert_eq!(slot.active_count(), MAX_FORWARDS);
        assert!(slot.listening(new, "new bound").is_ok());
        assert!(!slot.text().contains("stale"));
    }
    #[test]
    fn forward_view_selects_and_stops_only_highlighted_session() {
        let (mut app, _replies, _commands) = harness();
        let generation = app.worker.forward.generation();
        let watch = &app.worker.watch;
        let epoch = watch.epoch.load(std::sync::atomic::Ordering::SeqCst);
        let (_, mut a) = app
            .worker
            .forward
            .begin(generation, epoch, watch, forward_target(8080))
            .unwrap_or_else(|e| panic!("{e}"));
        let (_, mut b) = app
            .worker
            .forward
            .begin(generation, epoch, watch, forward_target(8081))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(app.command("pf").is_ok());
        assert!(app.key(KeyCode::Char('j')).is_ok());
        assert_eq!(app.forward_selected, 1);
        assert!(app.key(KeyCode::Char('s')).is_ok());
        assert!(matches!(
            a.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        ));
        assert!(matches!(
            b.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
        app.changed();
        assert!(matches!(
            a.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Closed)
        ));
    }
    #[test]
    fn familiar_commands_complete_recall_and_keep_scoped_back_history() {
        let (mut app, _replies, _commands) = harness();
        app.aliases
            .insert("apis".into(), "v1/pods test /api|web".into());
        assert!(app.command("apis").is_ok());
        assert_eq!(
            (app.kind(), app.namespace.as_str(), app.filter.as_str()),
            ("pods".into(), "test", "api|web")
        );
        assert!(app.command("nodes").is_ok());
        assert!(app.key(KeyCode::Char('[')).is_ok());
        assert_eq!(app.filter, "api|web");
        assert!(app.key(KeyCode::Char(']')).is_ok());
        assert_eq!(app.kind(), "nodes");
        app.modal = Modal::None;
        assert!(app.key(KeyCode::Char(':')).is_ok());
        for c in "dep".chars() {
            assert!(app.key(KeyCode::Char(c)).is_ok());
        }
        assert!(screen(&app, 48, 12).join("\n").contains(":deploy"));
        assert!(app.key(KeyCode::Tab).is_ok());
        assert!(app.key(KeyCode::Enter).is_ok());
        assert_eq!(app.kind(), "deployments.apps");
        assert_eq!(app.history.last().map(String::as_str), Some("deploy"));
        assert!(app.key(KeyCode::Char(':')).is_ok());
        assert!(app.key(KeyCode::Char('é')).is_ok());
        assert!(app.key(KeyCode::Up).is_ok());
        assert!(matches!(&app.modal,Modal::Input("Command",text) if text=="deploy"));
        assert!(app.key(KeyCode::Down).is_ok());
        assert!(matches!(&app.modal,Modal::Input("Command",text) if text=="é"));
        assert!(app.key(KeyCode::Left).is_ok());
        assert!(app.key(KeyCode::Delete).is_ok());
        assert!(matches!(&app.modal,Modal::Input("Command",text) if text.is_empty()));
    }
    #[test]
    fn explicit_core_gvr_is_unambiguous_and_discovered_aliases_are_usable() {
        let (mut app, _replies, _commands) = harness();
        let mut custom = app.kinds[0].clone();
        custom.group = "example.test".into();
        custom.short_names = vec!["custompo".into()];
        app.kinds.push(custom);
        assert_eq!(app.resolve_resource("pods"), Ok(0));
        assert_eq!(app.resolve_resource("po"), Ok(0));
        assert_eq!(app.resolve_resource("v1/pods"), Ok(0));
        assert_eq!(app.resolve_resource("custompo"), Ok(app.kinds.len() - 1));
        assert!(app.command("secret").is_ok());
        assert!(app.status.contains("excluded"));
        app.items = vec![object("api"), object("web"), object("worker")];
        app.filter = "api|web".into();
        assert_eq!(app.visible().len(), 2);
        app.filter = "!api|web".into();
        assert_eq!(
            app.visible().iter().map(|v| name(v)).collect::<Vec<_>>(),
            vec!["worker"]
        );
        app.filter = "-f wkr".into();
        assert_eq!(app.visible().len(), 1);
        app.filter = "[".into();
        assert!(app.visible().is_empty());
    }
    #[test]
    fn drilldown_uses_node_fields_and_workload_labels_and_escape_restores_scope() {
        let (mut app, _replies, mut commands) = harness();
        app.command("nodes").unwrap_or(false);
        app.items = vec![
            serde_json::json!({"apiVersion":"v1","kind":"Node","metadata":{"name":"worker-a","uid":"node-uid"}}),
        ];
        assert!(app.key(KeyCode::Enter).is_ok());
        assert_eq!(
            (
                app.kind(),
                app.namespace.as_str(),
                app.field_selector.as_str()
            ),
            ("pods".into(), "*", "spec.nodeName=worker-a")
        );
        assert!(app.key(KeyCode::Esc).is_ok());
        assert_eq!(app.kind(), "nodes");
        assert!(app.field_selector.is_empty());
        app.command("deploy").unwrap_or(false);
        app.items = vec![
            serde_json::json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"api","namespace":"team","uid":"dep"},"spec":{"selector":{"matchLabels":{"app":"api"}}}}),
        ];
        assert!(app.key(KeyCode::Enter).is_ok());
        assert_eq!(
            (app.kind(), app.namespace.as_str(), app.selector.as_str()),
            ("pods".into(), "team", "app=api")
        );
        app.items = vec![
            serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"api-a","namespace":"team","uid":"pod"}}),
        ];
        assert!(app.key(KeyCode::Enter).is_ok());
        assert!(matches!(commands.try_recv(), Ok((_, Task::Drill(_, _, _)))));
    }
    #[test]
    fn late_log_snapshot_is_rejected_and_static_marks_keep_original_lines() {
        let (mut app, replies, _commands) = harness();
        let old = app.worker.logs.cancel();
        let current = app.worker.logs.cancel();
        let mut view = crate::ui_logs::LogView::new(
            "current snapshot".into(),
            crate::logs::ReadOptions::default(),
        );
        view.following = false;
        app.modal = Modal::Follow(Box::new(view));
        assert!(
            replies
                .send((app.epoch, Reply::Logs(old, "obsolete".into())))
                .is_ok()
        );
        app.receive();
        assert!(!screen(&app, 80, 24).join("\n").contains("obsolete"));
        assert!(
            replies
                .send((app.epoch, Reply::Logs(current, "current snapshot".into())))
                .is_ok()
        );
        app.receive();
        assert!(app.key(KeyCode::Char('m')).is_ok());
        app.receive();
        let frame = screen(&app, 80, 24).join("\n");
        assert!(frame.contains("current snapshot") && frame.contains("log mark"));
        app.busy = true;
        assert!(app.key(KeyCode::Char('2')).is_ok());
        assert!(screen(&app, 80, 24).join("\n").contains("current snapshot"));
    }
    #[test]
    fn responsive_frames_preserve_identity_mode_and_bounds() {
        let (mut app, _replies, _commands) = harness();
        app.items = vec![
            serde_json::json!({"kind":"Pod","metadata":{"name":"api","namespace":"test","uid":"pod1","creationTimestamp":"2026-10-10T07:00:00Z"},"spec":{"containers":[{"name":"app"}]},"status":{"phase":"Running","containerStatuses":[{"ready":true,"restartCount":4}]}}),
        ];
        for width in [28, 40, 60, 80, 88, 120] {
            for height in [10, 12, 24] {
                {
                    let frame = screen(&app, width, height);
                    assert!(frame[..2].join(" ").contains("PRODUCTION READ ONLY"));
                    assert!(frame[1].contains("CTX test | NS test"));
                    assert!(frame[3].starts_with(['+', '╭']));
                }
            }
        }
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
            .map(|line| {
                line.chars()
                    .skip(1)
                    .take(26)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
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
            LogTarget {
                namespace: "test".into(),
                pod: "api".into(),
                container: None,
                uid: "uid".into(),
                previous: false,
                follow: false,
                all: false,
                dynamic: false,
                membership: None,
            },
            (0..20).map(|index| format!("container-{index}")).collect(),
            19,
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
        assert_eq!(app.kind(), "pods");
        assert_eq!(app.namespace, "other");
        assert_eq!(app.command("all"), Ok(false));
        assert_eq!(app.namespace, "*");
        assert_eq!(app.command("sort restarts"), Ok(false));
        assert_eq!(app.sort, Sort::Restarts);
        assert_eq!(app.command("pods; rm -rf /"), Ok(false));
        assert_eq!(app.kind(), "pods");
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
    fn table_metrics_sort_exact_usage_and_missing_rows_last_preserving_uid() -> Result<()> {
        let (mut app, replies, mut commands) = harness();
        let pod = |name: &str, uid: &str| serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":"test","uid":uid},"spec":{"containers":[{"name":"app"}]}});
        app.items = vec![
            pod("big", "big-uid"),
            pod("small", "small-uid"),
            pod("unknown", "unknown-uid"),
        ];
        let row = |name: &str, uid: &str, cpu: u64, memory: u64| crate::metrics::MetricsRow {
            name: name.into(),
            namespace: Some("test".into()),
            container: Some("app".into()),
            timestamp: "2026-10-22T12:00:30Z".into(),
            window: "30s".into(),
            cpu_raw: format!("{cpu}n"),
            memory_raw: memory.to_string(),
            cpu_nanocores: cpu,
            cpu_millicores: crate::metrics::format_millicores(cpu),
            memory_bytes: memory,
            rounded_up: false,
            identity_uid: Some(uid.into()),
            allocations: None,
        };
        let snapshot = crate::metrics::MetricsSnapshot {
            api_version: "metrics.k8s.io/v1beta1",
            scope: crate::metrics::MetricsScope::Pods,
            namespace: Some("test".into()),
            source: "demo (test)",
            items: vec![
                row("big", "big-uid", 100_000_001, 2_097_152),
                row("small", "small-uid", 9_000_001, 67_108_864),
            ],
            truncated: false,
            allocation_status: None,
        };
        app.selected = 1;
        replies
            .send((
                app.epoch,
                Reply::Metrics(
                    crate::metrics::MetricsScope::Pods,
                    "test".into(),
                    Ok(snapshot),
                ),
            ))
            .map_err(|_| "reply closed")?;
        app.receive();
        app.last_table_metrics = Some(Instant::now());
        app.key(KeyCode::Char('C'))?;
        assert_eq!(
            app.visible()
                .iter()
                .map(|object| name(object))
                .collect::<Vec<_>>(),
            vec!["big", "small", "unknown"]
        );
        assert_eq!(app.selected_uid().as_deref(), Some("small-uid"));
        app.key(KeyCode::Char('M'))?;
        assert_eq!(
            app.visible()
                .iter()
                .map(|object| name(object))
                .collect::<Vec<_>>(),
            vec!["small", "big", "unknown"]
        );
        let frame = screen(&app, 120, 24).join("\n");
        assert!(frame.contains("CPU(m)"));
        assert!(frame.contains("MEM"));
        assert!(frame.contains("9        "));
        assert!(frame.contains("64Mi"));
        let usage = table_metrics::Usage {
            cpu_nanocores: 72_574_100,
            memory_bytes: 1_268_680 * 1024,
            timestamp: "now".into(),
            window: "30s".into(),
            identity_uid: "small-uid".into(),
            identity_verified: false,
        };
        let compact = table_line_with_metrics(
            &[(Column::Cpu, 9), (Column::Memory, 9)],
            Some(&app.items[1]),
            0,
            Some(&usage),
        );
        assert_eq!(
            compact, "72.57~    1.21Gi~  ",
            "each tilde must be the UID marker, with units intact"
        );
        assert!(commands.try_recv().is_err());
        // Expiry reorders missing values by name but preserves the selected UID.
        let before = app.selected_uid();
        assert!(app.expire_table_metrics(Instant::now() + Duration::from_secs(46)));
        assert_eq!(app.selected_uid(), before);
        app.items[1]["metadata"]["uid"] = "new-uid".into();
        assert!(
            app.table_metrics
                .usage(&app.items[1], Instant::now())
                .is_none()
        );
        app.changed();
        assert!(app.table_metrics.label(Instant::now()).contains("loading"));
        Ok(())
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
        assert!(app.action_menu()[7].contains("Deployments"));
        app.kind_index = 1;
        assert!(!app.action_menu()[7].contains("[OFF]"));
    }
    #[test]
    fn resource_table_uses_every_column_and_mouse_never_changes_resource() {
        let (mut app, _replies, _commands) = harness();
        app.items = vec![object("a"), object("b")];
        for (w, h) in [(28, 10), (48, 12), (120, 32)] {
            let layout = Layout::new(w, h);
            assert_eq!(layout.table.x, 0);
            assert_eq!(layout.table.width, w);
            let frame = screen(&app, w, h).join("\n");
            assert!(!frame.contains("RESOURCES [Tab]") && !frame.contains("WORKLOADS"));
        }
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
        assert_eq!(app.selected, 1);
        assert_eq!(app.kind(), "pods");
        app.selector = "app=api".into();
        let frame = screen(&app, 80, 24).join("\n");
        assert!(frame.contains("FILTERED") && frame.contains("LABEL:app=api"));
        app.selector.clear();
        app.field_selector = "spec.nodeName=worker-a".into();
        let frame = screen(&app, 120, 24).join("\n");
        assert!(frame.contains("FILTERED") && frame.contains("FIELD:spec.nodeName=worker-a"));
        assert!(screen(&app, 28, 10).join("\n").contains("FILTERED"));
    }
    #[test]
    fn full_width_frame_shows_health_and_centered_actions_without_clear() {
        let (mut app, _replies, _commands) = harness();
        app.items = vec![
            serde_json::json!({"kind":"Pod","metadata":{"name":"api","namespace":"test","uid":"pod1"},"spec":{"nodeName":"worker-a","containers":[{"name":"app","image":"example/api:v2"}]},"status":{"phase":"Running","containerStatuses":[{"name":"app","ready":true,"restartCount":0}]}}),
        ];
        let frame = screen(&app, 120, 32).join("\n");
        for label in ["1 healthy", "0 need attention", "NAMESPACE", "api"] {
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
        assert!(app.key(KeyCode::Char('d')).is_ok());
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
            Ok((
                _,
                Task::PrepareSession(
                    namespace,
                    pod,
                    uid,
                    crate::ops::Request::InteractiveExec {
                        container, seconds, ..
                    },
                ),
            )) => {
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
        app.modal = Modal::Follow(Box::new(crate::ui_logs::LogView::new(
            text,
            crate::logs::ReadOptions::default(),
        )));
        assert!(screen(&app, 60, 12).join("\n").contains("line-99"));
        assert!(app.key(KeyCode::Home).is_ok());
        assert!(screen(&app, 60, 12).join("\n").contains("line-0"));
        assert!(app.key(KeyCode::End).is_ok());
        assert!(matches!(app.modal,Modal::Follow(view) if view.autoscroll));
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
        app.modal = Modal::LiveOverview(dashboard_ui::Dashboard::new(&["demo".into()], true));
        assert!(replies.send((12, Reply::Overview(create()?))).is_ok());
        app.receive();
        assert!(app.overview.is_some());
        let frame = screen(&app, 80, 24).join("\n");
        assert!(frame.contains("demo"));
        assert!(frame.contains("2 Pods"));
        assert!(frame.contains("NEEDS ATTENTION"));
        assert!(frame.contains("WARNINGS / LAST 15m"));
        assert!(!frame.contains(":pods"));
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
        let names: Vec<_> = ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i']
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
        app.modal = Modal::Input(OVERVIEW_CONTEXTS_LABEL, "a,b,c,d,e,f,g,h,i,j".into());
        assert!(app.key(KeyCode::Enter).is_ok());
        assert!(commands.try_recv().is_err());
        assert!(app.status.contains("up to 9"));
    }
    #[test]
    fn dashboard_focus_help_and_drill_preserve_explicit_scope() {
        let (mut app, _replies, mut commands) = harness();
        app.namespace = "selected-space".into();
        app.namespace_override = false;
        assert!(app.key(KeyCode::Char('D')).is_ok());
        match commands.try_recv() {
            Ok((_, Task::Overview(connection, names))) => {
                assert_eq!(connection.namespace, "selected-space");
                assert_eq!(names, vec![app.context.clone()]);
            }
            _ => panic!("dashboard did not open independent reads"),
        }
        app.busy = false;
        app.command_epoch = None;
        app.modal = Modal::LiveOverview(dashboard_ui::Dashboard::new(
            &["local".into(), "other cluster".into()],
            false,
        ));
        assert!(app.key(KeyCode::Char('2')).is_ok());
        assert!(app.key(KeyCode::Char('?')).is_ok());
        assert!(
            matches!(&app.modal, Modal::LiveOverview(dashboard) if dashboard.help && dashboard.selected == 1)
        );
        assert!(screen(&app, 100, 28).join("\n").contains("DASHBOARD KEYS"));
        assert!(app.key(KeyCode::Esc).is_ok());
        assert!(
            matches!(&app.modal, Modal::LiveOverview(dashboard) if !dashboard.help && dashboard.selected == 1)
        );
        app.filter = "old-filter".into();
        app.selector = "app=old".into();
        assert!(app.key(KeyCode::Enter).is_ok());
        match commands.try_recv() {
            Ok((_, Task::Connect(connection))) => {
                assert_eq!(connection.context.as_deref(), Some("other cluster"));
            }
            _ => panic!("focused context was not opened"),
        }
        assert_eq!(app.namespace, "selected-space");
        assert!(app.namespace_override);
        assert!(app.filter.is_empty() && app.selector.is_empty());
        assert_eq!(app.desired_resource.as_deref(), Some("v1/pods"));
        assert!(app.overview.is_none());
    }
    #[test]
    fn dashboard_log_pane_follows_exact_warning_pod_and_closes_first() -> Result<()> {
        let (mut app, replies, mut commands) = harness();
        app.options.connection.logs = true;
        app.options.connection.writable = true;
        app.options.connection.allow_force_kill = true;
        let mut row = crate::overview::ContextSummary::connecting("other cluster".into(), false);
        row.state = crate::dashboard_health::EvidenceState::Live;
        row.health = Some(crate::dashboard_health::HealthSummary {
            ready: 1,
            unready: 1,
            pending: 0,
            failed: 0,
            succeeded: 0,
            terminating: 0,
            unknown: 0,
            restarts: 3,
            oom_killed: 0,
            restart_alerts: Vec::new(),
            restart_alert_count: 0,
            warnings: crate::dashboard_health::WarningSummary {
                state: crate::dashboard_health::EvidenceState::Live,
                count: Some(1),
                recent: vec![crate::dashboard_health::WarningEvidence {
                    namespace: "payments".into(),
                    pod: "api-7d9f".into(),
                    uid: "0f0b21f0-af23-4c1e-9a77-3546a8e1e804".into(),
                    reason: "BackOff".into(),
                    occurrences: 4,
                    observed_unix_seconds: 1,
                }],
                message: None,
            },
        });
        let mut dashboard = dashboard_ui::Dashboard::new(&["other cluster".into()], false);
        dashboard.update(vec![row]);
        app.modal = Modal::LiveOverview(dashboard);
        app.key(KeyCode::Char('L'))?;
        assert!(commands.try_recv().is_err());
        app.key(KeyCode::Char('w'))?;
        app.key(KeyCode::Char('L'))?;
        let generation = match commands.try_recv() {
            Ok((_, Task::DashboardLogs(connection, ns, pod, uid, generation, _, _))) => {
                assert_eq!(connection.context.as_deref(), Some("other cluster"));
                assert!(!connection.writable && !connection.allow_force_kill);
                assert_eq!(
                    (ns.as_str(), pod.as_str(), uid.as_str()),
                    (
                        "payments",
                        "api-7d9f",
                        "0f0b21f0-af23-4c1e-9a77-3546a8e1e804"
                    )
                );
                generation
            }
            _ => return Err("dashboard log task not sent".into()),
        };
        replies
            .send((app.epoch, Reply::Following))
            .map_err(|_| "reply closed")?;
        app.receive();
        assert!(app.worker.logs.append(generation, "api-7d9f/app: boom"));
        app.receive();
        let text = screen(&app, 120, 40).join("\n");
        assert!(text.contains("LOGS other cluster / payments/api-7d9f") && text.contains("boom"));
        app.key(KeyCode::Esc)?;
        assert!(
            matches!(&app.modal, Modal::LiveOverview(d) if d.logs.is_none() && d.warning.is_some())
        );
        assert!(!app.worker.logs.append(generation, "late"));
        Ok(())
    }
    #[test]
    fn dashboard_tail_lists_every_cluster_read_only_and_merges_selected_workloads() -> Result<()> {
        let (mut app, replies, mut commands) = harness();
        app.options.connection.logs = true;
        app.options.connection.writable = true;
        app.namespace = "shop".into();
        let names = vec!["east".to_owned(), "west".to_owned()];
        app.modal = Modal::LiveOverview(dashboard_ui::Dashboard::new(&names, false));
        app.key(KeyCode::Char('t'))?;
        match commands.try_recv() {
            Ok((_, Task::DashboardWorkloads(contexts))) => {
                assert_eq!(contexts.len(), 2);
                for (context, connection) in &contexts {
                    assert_eq!(connection.context.as_deref(), Some(context.as_str()));
                    assert!(!connection.writable && !connection.allow_exec);
                    assert_eq!(connection.namespace, "shop");
                }
            }
            _ => return Err("workload listing not requested".into()),
        }
        let workload = |context: &str, name: &str| crate::workload_tail::WorkloadRef {
            context: context.into(),
            resource: "deployments".into(),
            namespace: "shop".into(),
            name: name.into(),
            uid: format!("uid-{context}-{name}"),
        };
        replies
            .send((
                app.epoch,
                Reply::DashboardWorkloads(vec![
                    ("east".into(), Ok(vec![workload("east", "api")])),
                    ("west".into(), Ok(vec![workload("west", "api")])),
                ]),
            ))
            .map_err(|_| "reply closed")?;
        app.receive();
        assert!(
            screen(&app, 120, 40)
                .join("\n")
                .contains("west/shop/deploy/api")
        );
        app.key(KeyCode::Char(' '))?;
        app.key(KeyCode::Down)?;
        app.key(KeyCode::Char(' '))?;
        app.key(KeyCode::Enter)?;
        let generation = match commands.try_recv() {
            Ok((_, Task::DashboardTail(sources, generation, _, _))) => {
                assert_eq!(
                    sources
                        .iter()
                        .map(|s| s.workload.context.as_str())
                        .collect::<Vec<_>>(),
                    ["east", "west"]
                );
                assert!(sources.iter().all(|s| !s.connection.writable
                    && s.connection.context.as_deref() == Some(s.workload.context.as_str())));
                generation
            }
            _ => return Err("tail not started".into()),
        };
        replies
            .send((app.epoch, Reply::Following))
            .map_err(|_| "reply closed")?;
        app.receive();
        assert!(app.worker.logs.append(
            generation,
            "2026-10-24T10:00:00Z [east] [shop/api-1:app] ERROR boom"
        ));
        assert!(app.worker.logs.append(
            generation,
            "2026-10-24T10:00:01Z [west] [shop/api-2:app] INFO ok"
        ));
        app.receive();
        let text = screen(&app, 120, 40).join("\n");
        assert!(text.contains("TAIL 2 workloads across 2 clusters") && text.contains("[west]"));
        app.key(KeyCode::Char('e'))?;
        let text = screen(&app, 120, 40).join("\n");
        assert!(text.contains("ERROR boom") && !text.contains("INFO ok"));
        app.key(KeyCode::Char('t'))?;
        assert!(matches!(&app.modal, Modal::LiveOverview(d) if d.logs.is_none()));
        assert!(!app.worker.logs.append(generation, "late"));
        Ok(())
    }
    /// Every foreground the UI paints, on every surface it paints it on:
    /// plain background, the 3% header surface, 6% raised rows and the 2%/5%
    /// dashboard card tints. Status and hint text must stay at WCAG AA 4.5:1;
    /// selection text is checked on its own background.
    #[test]
    fn every_builtin_theme_keeps_rendered_text_legible_on_every_surface() {
        let mut failures = Vec::new();
        for theme in themes::all().unwrap_or_default() {
            if theme.terminal {
                continue;
            }
            let background = theme.get("background");
            let text = theme.get("text");
            for (surface, tint) in [
                ("background", 0),
                ("card", 2),
                ("header", 3),
                ("focused card", 5),
                ("raised", 6),
            ] {
                let base = blend(background, text, tint);
                for role in ["text", "muted", "accent", "success", "warning", "danger"] {
                    let ratio = themes::contrast(theme.get(role), base);
                    if ratio < 4.5 {
                        failures.push(format!("{} {role} on {surface}: {ratio:.2}", theme.id));
                    }
                }
            }
            let ratio = themes::contrast(
                theme.get("selection_text"),
                theme.get("selection_background"),
            );
            if ratio < 4.5 {
                failures.push(format!("{} selection: {ratio:.2}", theme.id));
            }
        }
        assert!(failures.is_empty(), "{failures:#?}");
    }
    #[test]
    fn table_hints_show_only_actions_that_work_for_this_kind_and_session() -> Result<()> {
        let (mut app, _replies, _commands) = harness();
        let keys = |app: &App| {
            table_hints(app)
                .into_iter()
                .map(|(key, _)| key)
                .collect::<Vec<_>>()
        };
        app.desired_resource = Some("pods".into());
        let read_only = keys(&app);
        assert!(read_only.contains(&"?") && read_only.contains(&"Enter"));
        for hidden in ["l", "s", "Ctrl-D", "Ctrl-K", "e"] {
            assert!(!read_only.contains(&hidden), "{hidden}");
        }
        app.options.connection.logs = true;
        app.options.connection.writable = true;
        app.options.connection.allow_exec = true;
        app.options.connection.allow_force_kill = true;
        let pods = keys(&app);
        for shown in ["l", "s", "Ctrl-D", "Ctrl-K"] {
            assert!(pods.contains(&shown), "{shown}");
        }
        app.desired_resource = Some("deployments.apps".into());
        let deployments = table_hints(&app);
        assert!(deployments.contains(&("s", "scale")) && deployments.contains(&("r", "restart")));
        assert!(!deployments.iter().any(|(key, _)| *key == "Ctrl-K"));
        app.desired_resource = Some("nodes".into());
        assert!(!keys(&app).contains(&"c"));
        app.options.connection.allow_node_maintenance = true;
        assert!(table_hints(&app).contains(&("c", "cordon")));
        app.modal = Modal::None;
        let bottom = screen(&app, 120, 24).pop().unwrap_or_default();
        assert!(bottom.contains("? all keys") && bottom.contains("c cordon"));
        Ok(())
    }
    #[test]
    fn secrets_are_opt_in_hidden_by_default_and_reveal_one_key() -> Result<()> {
        let (mut app, replies, mut commands) = harness();
        app.namespace = "demo".into();
        app.command("secrets")?;
        assert!(app.status.contains("--allow-secret-reveal"));
        assert!(commands.try_recv().is_err());
        app.options.connection.allow_secret_reveal = true;
        app.command("secrets")?;
        assert!(matches!(commands.try_recv(), Ok((_, Task::Secrets(ns))) if ns == "demo"));
        let data = crate::secrets::demo_data();
        replies
            .send((
                app.epoch,
                Reply::Secrets(crate::secrets::SecretIndex {
                    namespace: "demo".into(),
                    items: vec![crate::secrets::entry("api", "u1", None, Some(&data))],
                    truncated: false,
                }),
            ))
            .map_err(|_| "reply closed")?;
        app.receive();
        app.key(KeyCode::Enter)?;
        app.key(KeyCode::Char('x'))?;
        assert!(matches!(
            commands.try_recv(),
            Ok((_, Task::SecretValue(ns, name, uid, key)))
                if (ns.as_str(), name.as_str(), uid.as_str(), key.as_str()) == ("demo", "api", "u1", "blob.bin")
        ));
        assert!(
            !screen(&app, 120, 30)
                .join("\n")
                .contains("not-a-real-password")
        );
        replies
            .send((
                app.epoch,
                Reply::SecretValue(
                    "u1".into(),
                    "blob.bin".into(),
                    Ok(crate::secrets::reveal(&data["blob.bin"])),
                ),
            ))
            .map_err(|_| "reply closed")?;
        app.receive();
        assert!(screen(&app, 120, 30).join("\n").contains("binary, 4 bytes"));
        app.key(KeyCode::Esc)?;
        app.key(KeyCode::Esc)?;
        assert!(matches!(app.modal, Modal::None));
        Ok(())
    }
    #[test]
    fn dashboard_warning_opens_exact_pod_events_and_esc_clears_selection_first() {
        let (mut app, _replies, mut commands) = harness();
        app.namespace = "*".into();
        let mut row = crate::overview::ContextSummary::connecting("other cluster".into(), false);
        row.state = crate::dashboard_health::EvidenceState::Live;
        row.namespace = "*".into();
        row.health = Some(crate::dashboard_health::HealthSummary {
            ready: 1,
            unready: 1,
            pending: 0,
            failed: 0,
            succeeded: 0,
            terminating: 0,
            unknown: 0,
            restarts: 3,
            oom_killed: 0,
            restart_alerts: Vec::new(),
            restart_alert_count: 0,
            warnings: crate::dashboard_health::WarningSummary {
                state: crate::dashboard_health::EvidenceState::Live,
                count: Some(1),
                recent: vec![crate::dashboard_health::WarningEvidence {
                    namespace: "payments".into(),
                    pod: "api-7d9f".into(),
                    uid: "0f0b21f0-af23-4c1e-9a77-3546a8e1e804".into(),
                    reason: "BackOff".into(),
                    occurrences: 4,
                    observed_unix_seconds: 1,
                }],
                message: None,
            },
        });
        let mut dashboard = dashboard_ui::Dashboard::new(&["other cluster".into()], false);
        dashboard.update(vec![row]);
        app.modal = Modal::LiveOverview(dashboard);
        assert!(app.key(KeyCode::Char('w')).is_ok());
        assert!(
            screen(&app, 120, 30)
                .join("\n")
                .contains("> BackOff / payments/api-7d9f")
        );
        assert!(app.key(KeyCode::Esc).is_ok());
        assert!(
            matches!(&app.modal, Modal::LiveOverview(dashboard) if dashboard.warning.is_none())
        );
        assert!(app.key(KeyCode::Char('w')).is_ok());
        assert!(app.key(KeyCode::Enter).is_ok());
        match commands.try_recv() {
            Ok((_, Task::Connect(connection))) => {
                assert_eq!(connection.context.as_deref(), Some("other cluster"));
            }
            _ => panic!("warning context was not opened"),
        }
        assert_eq!(app.namespace, "payments");
        assert_eq!(app.desired_resource.as_deref(), Some("v1/events"));
        assert_eq!(
            app.field_selector,
            crate::selectors::pod_events_field("api-7d9f", "0f0b21f0-af23-4c1e-9a77-3546a8e1e804")
        );
        assert!(app.overview.is_none());
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
        assert_eq!(input_limit("Command"), nav::COMMAND_BYTES);
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
        assert!(app.command("metrics resources").is_ok());
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
        app.modal = Modal::Follow(Box::new(crate::ui_logs::LogView::new(
            "live".into(),
            crate::logs::ReadOptions::default(),
        )));
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
        assert!(app.command("image").is_ok());
        assert!(matches!(app.modal, Modal::None));
        app.options.connection.writable = true;
        assert!(app.command("image").is_ok());
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
    fn mutation_and_session_reviews_never_approve_lossy_unicode_or_overflow() -> Result<()> {
        let summary = "context: 東京\\u{e9}\nDirect argv: [\"echo\",\"é\"]";
        let lines = confirmation_lines(summary, "test/api", 79);
        let displayed = lines.iter().map(|line| &line[1..]).collect::<String>();
        assert!(displayed.is_ascii());
        assert!(displayed.contains("\\u{6771}\\u{4eac}\\\\u{e9}"));
        assert!(displayed.contains("\\u{e9}"));
        assert!(!confirmation_fits(summary, "test/api", 28, 10));
        assert!(confirmation_fits(summary, "test/api", 120, 40));
        assert!(!confirmation_fits(
            &"é".repeat(200_000),
            "test/api",
            65535,
            65535
        ));
        // Real data cell rendering cannot silently replace the escaped characters.
        assert_eq!(display(&displayed, displayed.len()), displayed);
        Ok(())
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
        assert!(app.connected && app.busy); // Automatic discovery follows the connection.
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
        assert!(app.command("scale").is_ok());
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
        app.last_table_metrics = Instant::now().checked_sub(Duration::from_secs(30));
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
        assert!(
            app.last_table_metrics
                .is_some_and(|last| last.elapsed() < Duration::from_secs(1)),
            "retry quiet period must begin after the failed response, not request start"
        );
        assert!(!app.busy);
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
    #[test]
    fn saved_selection_rejects_mode_mismatch_without_changing_scope() -> Result<()> {
        let (mut app, _, _) = harness();
        let before = (app.epoch, app.namespace.clone(), app.context.clone());
        let saved = crate::selections::Selection {
            name: "Offline dashboard".into(),
            demo: !app.options.connection.demo,
            target: crate::selections::Target::Dashboard {
                contexts: vec!["demo".into()],
                namespace: "demo".into(),
            },
        };
        assert!(app.open_selection(saved).is_err());
        assert_eq!(
            before,
            (app.epoch, app.namespace.clone(), app.context.clone())
        );
        Ok(())
    }

    #[test]
    fn saved_scope_must_exist_in_destination_catalog_before_refresh() {
        let (mut app, _, _) = harness();
        app.selection_ui.restoring = Some(crate::selections::Selection {
            name: "Unavailable custom kind".into(),
            demo: true,
            target: crate::selections::Target::Resource {
                context: "demo".into(),
                namespace: "test".into(),
                kind: "widgets.example.test".into(),
                label_selector: String::new(),
                workload: None,
            },
        });
        app.finish_saved_selection_restore();
        assert!(!app.refresh && app.watch_blocked);
        assert!(app.status.contains("unavailable"));
    }

    #[test]
    fn saved_workload_waits_for_fresh_list_and_rejects_replacement_uid() {
        let (mut app, _, _) = harness();
        app.selection_ui.focus = Some(crate::selections::Workload {
            namespace: "test".into(),
            name: "api".into(),
            uid: "old-uid".into(),
        });
        app.items =
            vec![serde_json::json!({"metadata":{"namespace":"test","name":"api","uid":"new-uid"}})];
        app.stale = true;
        assert!(!app.finish_saved_workload_focus());
        assert!(app.selection_ui.focus.is_some());
        app.stale = false;
        assert!(app.finish_saved_workload_focus());
        assert!(
            matches!(app.modal, Modal::Detail(ref text, _) if text.contains("SAVED WORKLOAD UNAVAILABLE"))
        );
        assert!(!app.write_inflight);
    }

    #[test]
    fn scope_change_clears_pending_saved_workload_identity() {
        let (mut app, _, _) = harness();
        app.selection_ui.focus = Some(crate::selections::Workload {
            namespace: "test".into(),
            name: "api".into(),
            uid: "old-uid".into(),
        });
        app.changed();
        assert!(app.selection_ui.focus.is_none());
    }

    #[test]
    fn saved_picker_keeps_its_actions_when_watch_status_changes() {
        let (mut app, _, _) = harness();
        app.modal = Modal::Picker(
            selections_ui::PICKER_LABEL,
            vec!["Local workspace | Dashboard | 1 context".into()],
            0,
        );
        app.status = "Watch updated: 12 resources".into();
        for (width, height) in [(120, 34), (48, 16)] {
            let mut frame = Vec::new();
            render(&app, width, height, &mut frame).unwrap_or_else(|error| panic!("{error}"));
            let frame = String::from_utf8(frame).unwrap_or_else(|error| panic!("{error}"));
            assert!(frame.contains("Enter open | d remove | Esc back"));
            assert!(frame.contains("Watch updated"));
        }
    }
}
