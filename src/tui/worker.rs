//! Background worker: tasks, replies, mailboxes for watches/logs/forwards and the async loop.
use super::*;

pub(super) enum Task {
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
    ApplyPrepare(std::path::PathBuf, String),
    NodeShellPrepare(String, String, String, u16),
    Bench(crate::bench::Plan),
    Lint(String),
    /// Read image references and detect the trusted scanner: scanner path, target.
    ScanPrepare(std::path::PathBuf, crate::scan::Target),
    /// Run a reviewed scan detached from the worker queue.
    ScanRun(
        Box<crate::scan::Plan>,
        String,
        std::sync::Arc<crate::scan::Control>,
    ),
    NodeShellStart(Box<crate::node_shell::Prepared>, String),
    DebugPrepare(String, String, Option<String>, String, u16),
    DebugStart(Box<crate::debug_container::Prepared>, String),
    ApplyExecute(Box<crate::manifest_apply::PreparedApply>, String),
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
pub(super) enum Reply {
    Connected(String, String, String, Vec<Kind>),
    Items(Value),
    Detail(String),
    ScanPrepared(Result<Box<crate::scan::Plan>>),
    ScanStarted,
    ScanOutcome(Result<crate::scan::Report>),
    Object(Box<Value>),
    Export(crate::exports::Draft),
    YamlDraft(Box<crate::manifest_edit::Draft>),
    YamlPrepared(
        Box<crate::ui_yaml::Editor>,
        Result<crate::manifest_edit::PreparedYaml>,
    ),
    NodePrepared(Box<crate::node_maintenance::ReviewedNode>),
    ApplyPrepared(Box<crate::manifest_apply::PreparedApply>),
    NodeShellPrepared(Box<crate::node_shell::Prepared>),
    NodeShellReady(Box<crate::ops::ReviewedOperation>),
    DebugPrepared(Box<crate::debug_container::Prepared>),
    WriteOutcome(Result<String>),
    Drill(Box<Value>),
    Directory(crate::navigation_views::Directory),
    Secrets(crate::secrets::SecretIndex),
    Subjects(&'static str, String, crate::navigation_views::SubjectIndex),
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
pub(super) struct WatchSlot {
    pub(super) epoch: std::sync::atomic::AtomicU64,
    pub(super) snapshot: std::sync::Mutex<Option<(u64, crate::watch::WatchSnapshot)>>,
    pub(super) abort: std::sync::Mutex<Option<tokio::task::AbortHandle>>,
}
impl WatchSlot {
    pub(super) fn invalidate(&self, epoch: u64) {
        self.epoch.store(epoch, std::sync::atomic::Ordering::SeqCst);
        if let Ok(mut abort) = self.abort.lock()
            && let Some(handle) = abort.take()
        {
            handle.abort();
        }
        self.clear();
    }
    pub(super) fn clear(&self) {
        if let Ok(mut slot) = self.snapshot.lock() {
            *slot = None;
        }
    }
    pub(super) fn publish(&self, epoch: u64, snapshot: crate::watch::WatchSnapshot) {
        if let Ok(mut slot) = self.snapshot.lock()
            && self.epoch.load(std::sync::atomic::Ordering::SeqCst) == epoch
        {
            *slot = Some((epoch, snapshot));
        }
    }
    pub(super) fn take(&self) -> Option<(u64, crate::watch::WatchSnapshot)> {
        self.snapshot.lock().ok().and_then(|mut slot| slot.take())
    }
}
pub(super) type WatchMailbox = std::sync::Arc<WatchSlot>;

#[derive(Default)]
pub(super) struct LogState {
    pub(super) buffer: crate::logs::RollingLog,
    pub(super) dirty: bool,
}
#[derive(Default)]
pub(super) struct LogSlot {
    pub(super) generation: std::sync::atomic::AtomicU64,
    pub(super) state: std::sync::Mutex<LogState>,
    pub(super) abort: std::sync::Mutex<Option<tokio::task::AbortHandle>>,
}
impl LogSlot {
    pub(super) fn cancel(&self) -> u64 {
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
    pub(super) fn append(&self, generation: u64, line: &str) -> bool {
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
    pub(super) fn take(&self) -> Option<String> {
        let mut state = self.state.lock().ok()?;
        if !state.dirty {
            return None;
        }
        state.dirty = false;
        Some(state.buffer.text())
    }
}

pub(super) struct Worker {
    pub(super) tx: tokio::sync::mpsc::Sender<(u64, Task)>,
    pub(super) rx: Receiver<(u64, Reply)>,
    pub(super) watch: WatchMailbox,
    pub(super) logs: std::sync::Arc<LogSlot>,
    pub(super) forward: std::sync::Arc<ForwardSlot>,
}
pub(super) const MAX_FORWARDS: usize = 8;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ForwardPhase {
    Starting,
    Listening,
    Stopping,
    Ended,
    Failed,
}
impl ForwardPhase {
    pub(super) fn active(self) -> bool {
        matches!(self, Self::Starting | Self::Listening | Self::Stopping)
    }
    pub(super) fn label(self) -> &'static str {
        match self {
            Self::Starting => "STARTING",
            Self::Listening => "LISTENING",
            Self::Stopping => "STOPPING",
            Self::Ended => "ENDED",
            Self::Failed => "FAILED",
        }
    }
}
pub(super) struct ForwardEntry {
    pub(super) id: u64,
    pub(super) target: crate::ops::ForwardTarget,
    pub(super) phase: ForwardPhase,
    pub(super) message: String,
    pub(super) cancel: Option<tokio::sync::oneshot::Sender<()>>,
}
#[derive(Default)]
pub(super) struct ForwardState {
    pub(super) generation: u64,
    pub(super) next_id: u64,
    pub(super) entries: Vec<ForwardEntry>,
}
#[derive(Default)]
pub(super) struct ForwardSlot {
    pub(super) state: std::sync::Mutex<ForwardState>,
}
impl ForwardSlot {
    pub(super) fn generation(&self) -> u64 {
        self.state.lock().map(|s| s.generation).unwrap_or(u64::MAX)
    }
    pub(super) fn begin(
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
    pub(super) fn cancel(&self) -> u64 {
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
    pub(super) fn cancel_selected(&self, selected: usize) -> bool {
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
    pub(super) fn listening(&self, id: u64, text: &str) -> Result<()> {
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
    pub(super) fn finish(&self, id: u64, result: Result<String>) {
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
    pub(super) fn suggested_port(&self) -> u16 {
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
    pub(super) fn active_count(&self) -> usize {
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
    pub(super) fn len(&self) -> usize {
        self.state.lock().map(|s| s.entries.len()).unwrap_or(0)
    }
    /// Loopback port of the selected forward while it is listening.
    pub(super) fn listening_port(&self, selected: usize) -> Option<u16> {
        let state = self.state.lock().ok()?;
        let entry = state.entries.get(selected)?;
        matches!(entry.phase, ForwardPhase::Listening).then_some(entry.target.local_port)
    }
    pub(super) fn index(&self, id: u64) -> usize {
        self.state
            .lock()
            .ok()
            .and_then(|s| s.entries.iter().position(|entry| entry.id == id))
            .unwrap_or(0)
    }
    pub(super) fn text(&self) -> String {
        self.lines(usize::MAX, 200, 64).join("\n")
    }
    pub(super) fn lines(&self, selected: usize, width: usize, height: usize) -> Vec<String> {
        let Ok(state) = self.state.lock() else {
            return vec!["Forward status unavailable.".into()];
        };
        let count = state
            .entries
            .iter()
            .filter(|entry| entry.phase.active())
            .count();
        let mut lines = vec![
            format!(
                "{count}/{MAX_FORWARDS} active   /   j k select   s stop   b bench   F new   Esc close"
            ),
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
pub(super) fn worker() -> Worker {
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
                    Task::ApplyPrepare(path,namespace)=>match gateway.as_ref() {
                        Some(g)=>match tokio::time::timeout(Duration::from_secs(120),g.prepare_apply(&path,&namespace)).await {
                            Ok(Ok(prepared))=>Reply::ApplyPrepared(Box::new(prepared)),
                            Ok(Err(error))=>Reply::Failure(error),
                            Err(_)=>Reply::Failure("Apply review timed out; nothing was written.".into()),
                        },
                        None=>Reply::Failure("Not connected.".into()),
                    },
                    Task::NodeShellPrepare(node,namespace,image,seconds)=>match gateway.as_ref() {
                        Some(g)=>match crate::node_shell::prepare(g,&node,&namespace,&image,seconds).await {
                            Ok(prepared)=>Reply::NodeShellPrepared(Box::new(prepared)),
                            Err(error)=>Reply::Failure(error),
                        },
                        None=>Reply::Failure("Not connected.".into()),
                    },
                    Task::NodeShellStart(prepared,typed)=>match gateway.as_ref() {
                        Some(g)=>match crate::node_shell::start(g,*prepared,&typed).await {
                            Ok(reviewed)=>Reply::NodeShellReady(Box::new(reviewed)),
                            Err(error)=>Reply::Failure(error),
                        },
                        None=>Reply::Failure("Not connected; nothing was created.".into()),
                    },
                    Task::Bench(plan)=>Reply::Detail(crate::bench::run(&plan).await.text(&plan)),
                    Task::ScanPrepare(scanner,target)=>{
                        let result=async {
                            let gateway=gateway.as_ref().ok_or("Not connected; select a context first.")?;
                            let (scope,images)=tokio::time::timeout(Duration::from_secs(25),crate::scan::gather(gateway,&target)).await.map_err(|_|"Reading image references timed out.")??;
                            tokio::task::spawn_blocking(move||crate::scan::prepare(&scanner,scope,images)).await.map_err(|_|"Scanner detection failed.".to_owned())?.map(Box::new)
                        }.await;
                        Reply::ScanPrepared(result)
                    }
                    Task::ScanRun(plan,typed,control)=>{
                        // Detached so watches, metrics and other requests keep flowing
                        // while the scanner runs for minutes; the outcome arrives later.
                        let sender=results.clone();
                        tokio::spawn(async move {
                            let outcome=tokio::task::spawn_blocking(move||crate::scan::run(*plan,&typed,true,&control)).await.unwrap_or_else(|_|Err("Scan worker failed.".into()));
                            let _=sender.send((epoch,Reply::ScanOutcome(outcome)));
                        });
                        Reply::ScanStarted
                    }
                    Task::DebugPrepare(namespace,pod,target,image,seconds)=>match gateway.as_ref() {
                        Some(g)=>match crate::debug_container::prepare(g,&namespace,&pod,target.as_deref(),&image,seconds).await {
                            Ok(prepared)=>Reply::DebugPrepared(Box::new(prepared)),
                            Err(error)=>Reply::Failure(error),
                        },
                        None=>Reply::Failure("Not connected.".into()),
                    },
                    Task::DebugStart(prepared,typed)=>match gateway.as_ref() {
                        Some(g)=>match crate::debug_container::start(g,*prepared,&typed).await {
                            Ok(reviewed)=>Reply::NodeShellReady(Box::new(reviewed)),
                            Err(error)=>Reply::Failure(error),
                        },
                        None=>Reply::Failure("Not connected; nothing was added.".into()),
                    },
                    Task::ApplyExecute(prepared,typed)=>Reply::WriteOutcome(if let Some(g)=gateway.as_ref(){g.execute_apply(*prepared,&typed).await}else{Err("Not connected; nothing was applied.".into())}),
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
                    // Each dashboard card opens its own read-only connection, so the
                    // dashboard never waits for the main view's connection.
                    Task::Overview(options,names)=>match crate::overview::OverviewFeed::new(&options,&names){Ok(feed)=>Reply::Overview(feed),Err(error)=>Reply::Failure(error)},
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
                                    Task::Lint(n)=>{let (found,notes)=crate::lint::collect(g,&n).await?;Reply::Detail(crate::lint::report(&g.context,&n,&found,&notes))},
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
                                    Task::YamlPrepare(..) | Task::YamlExecute(..) | Task::ApplyPrepare(..) | Task::ApplyExecute(..) | Task::NodeShellPrepare(..) | Task::NodeShellStart(..) | Task::DebugPrepare(..) | Task::DebugStart(..) | Task::ScanPrepare(..) | Task::ScanRun(..) | Task::Bench(..) | Task::NodeExecute(..) | Task::DashboardLogs(..) | Task::DashboardWorkloads(..) | Task::DashboardTail(..) | Task::Overview(..) | Task::Connect(_) | Task::Watch(..) | Task::Metrics(..) | Task::RunSession(..) | Task::RunForward(..) | Task::Directory(_) | Task::Manifest(_) | Task::Helm(..) | Task::HelmPrepare(..) | Task::HelmExecute(..) | Task::AdvicePrepare(..) | Task::AdviceExecute(..) => return Err("invalid worker task".into()),
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
