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
        Event, KeyCode, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
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
#[path = "ui_charts.rs"]
mod charts_ui;
mod commands;
mod connection_prompt;
mod context_settings;
#[path = "ui_dashboard.rs"]
mod dashboard_ui;
mod doc_search;
mod format;
mod header_block;
#[path = "ui_helm.rs"]
mod helm_ui;
mod input;
mod keyhelp;
mod keys;
#[path = "ui_live_reload.rs"]
mod live_reload;
#[path = "ui_navigation.rs"]
mod nav;
mod palette;
mod plugin_run;
mod render;
mod replies;
#[path = "ui_review.rs"]
mod review_ui;
#[path = "ui_selections.rs"]
mod selections_ui;
mod snapshot;
mod startup;
mod table_keys;
#[path = "ui_metrics.rs"]
mod table_metrics;
#[cfg(test)]
mod tests;
#[path = "ui_topbar.rs"]
mod topbar;
mod tty;
mod worker;
use format::*;
use render::*;
use startup::*;
pub use startup::{play_startup_animation, preview};
use std::{
    io::{self, IsTerminal, Write},
    sync::mpsc::{self, Receiver},
    time::{Duration, Instant},
};
use tty::*;
use worker::*;

const OVERVIEW_CONTEXTS_LABEL: &str = "Overview contexts (comma separated, max 9)";
const ASK_LABEL: &str = "Ask Copilot about this screen";
const FOLLOW_UP_LABEL: &str = "Follow-up question (same screen)";
const SECRET_NAME_LABEL: &str = "New Secret name";
const SECRET_KEY_LABEL: &str = "New Secret key";
const SECRET_VALUE_LABEL: &str = "New Secret value";
/// A typed Secret value; a longer one comes from a file (`@PATH`).
const SECRET_VALUE_BYTES: usize = 4096;
/// Title of a Copilot answer document; `f` there asks a follow-up.
const ADVICE_TITLE: &str = "COPILOT ADVICE / review independently";
const OVERVIEW_INPUT_BYTES: usize = crate::overview::MAX_LIVE_CONTEXTS * 254 - 1;
fn input_limit(label: &str) -> usize {
    if label == OVERVIEW_CONTEXTS_LABEL {
        OVERVIEW_INPUT_BYTES
    } else if label == "Command" {
        nav::COMMAND_BYTES
    } else if label == ASK_LABEL || label == FOLLOW_UP_LABEL {
        crate::advice::MAX_QUESTION
    } else if label == SECRET_VALUE_LABEL {
        SECRET_VALUE_BYTES
    } else if label == SECRET_KEY_LABEL || label == SECRET_NAME_LABEL {
        253
    } else {
        100
    }
}

/// The reverse of the last change that has one, prepared on `z` or `:revert`
/// as a normal reviewed change of the same object.
#[derive(Clone, Debug, PartialEq)]
struct Undo {
    resource: String,
    namespace: String,
    name: String,
    uid: String,
    action: Action,
    /// What the reverse does, for the status line.
    label: String,
}

/// A screen capture and the questions and answers about it so far.
#[derive(Clone)]
struct AskThread {
    screen: String,
    /// The keys of the view the screen was captured in (`keyhelp::ask_key_reference`).
    keys: String,
    turns: Vec<(String, String)>,
    /// Context/scope generation; a follow-up after a switch is refused.
    epoch: u64,
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
    Help(Box<keyhelp::HelpView>),
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
    /// The whole-Secret editor: values on screen, never on disk or in :ask.
    SecretEditor(Box<crate::ui_yaml::Editor<crate::secret_document::SecretDoc>>),
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
    /// Reviewed image scan: plan and typed confirmation (Enter or "scan").
    ScanConfirm(Box<crate::scan::Plan>, String),
    /// A running image scan: start time and image count.
    ScanRunning(Instant, usize),
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
    Apply(Box<crate::manifest_apply::PreparedApply>),
    /// A registered read-only extension; it runs outside the TUI screen.
    Tool(Box<crate::handoff::Prepared>),
    NodeShell(Box<crate::node_shell::Prepared>),
    Debug(Box<crate::debug_container::Prepared>),
    /// One reviewed action on every marked object of the current kind.
    Batch(Box<crate::gateway::PreparedBatch>),
    /// Set or remove one key of a Secret, or delete it.
    Secret(Box<crate::secret_edit::Prepared>),
}
/// The Secret a value or key prompt is for.
struct SecretDraft {
    namespace: String,
    name: String,
    uid: String,
    /// `None` while the key of a new entry is being typed.
    key: Option<String>,
    /// Tab shows the typed value; it is hidden otherwise.
    visible: bool,
    /// The Secret does not exist yet: the value creates it.
    create: bool,
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
/// One precomputed primary sort key per object; every object in a sort
/// uses the same variant.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
enum SortKey {
    Text(String),
    Number(i64),
    Pair((u64, u64)),
    Descending(std::cmp::Reverse<u64>),
    Usage(std::cmp::Reverse<Option<u64>>),
}
/// What a left click on a region of the screen does.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ClickAction {
    /// Press a key, as if typed: the first alternative the view takes.
    Keys(Vec<crate::keymap::Key>),
    /// Sort the table by this column title; again reverses.
    Sort(String),
}
#[derive(Clone, Debug)]
struct Click {
    x: u16,
    y: u16,
    width: u16,
    action: ClickAction,
}
/// (items generation, allocation address, length): what `filter_text` was built from.
type FilterTextKey = (u64, usize, usize);
/// Two clicks on one row within this time open it, like Enter.
const DOUBLE_CLICK: Duration = Duration::from_millis(400);

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
    /// Shared with the watch cache (`Arc`): a new snapshot copies pointers.
    items: Vec<std::sync::Arc<Value>>,
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
    hotkeys: crate::hotkeys::Hotkeys,
    /// Registered k9s plugins and their shortcut bindings.
    plugins: crate::plugins::Catalog,
    /// keymap.yaml: effective keys of every registered action.
    keymap: crate::keymap::Keymap,
    /// Ctrl-Z: show only objects that need attention (k9s faults view).
    faults_only: bool,
    /// The screen as it was when the command prompt opened, for `:ask`.
    /// `/` search in the open describe, YAML, JSON or other document view.
    doc_search: doc_search::DocSearch,
    /// Cluster, user, version and usage shown by the k9s-style header block.
    header: header_block::State,
    /// The tour tip on screen (`:tour`, and after the first-run theme picker).
    tour: Option<usize>,
    /// The reverse of the change being executed; kept as `undo` if it succeeds.
    undo_pending: Option<Undo>,
    /// The reverse of the last successful change that has one.
    undo: Option<Undo>,
    /// First run: start the tour when the theme picker closes.
    tour_after_themes: bool,
    ask_screen: Option<String>,
    /// Pods per Node for the PODS column: when counted, counts, truncated.
    node_pods: Option<(Instant, std::collections::BTreeMap<String, usize>, bool)>,
    node_pods_requested: Option<Instant>,
    /// Default views of custom resources from their CRD's printer columns,
    /// keyed `group/version/plural`, and the resources already asked for
    /// (once per connection, whatever the answer).
    crd_views: crate::views::Views,
    crd_views_requested: std::collections::HashSet<String>,
    /// Events of the object the describe view shows, by its UID.
    overview_events: Option<(String, std::result::Result<Vec<Value>, String>)>,
    /// Captured with `ask_screen`: the keys of that view, for Copilot.
    ask_keys: Option<String>,
    /// The Secret and key a value prompt in front belongs to.
    secret_draft: Option<SecretDraft>,
    /// Masked text of the documents that show values, by the hash of the
    /// shown text: what a Copilot screen capture takes instead.
    masked_documents: Vec<(u64, String)>,
    /// The status line when the screen was captured, for `:explain`.
    ask_status: Option<String>,
    /// The answered `:ask` conversation a follow-up continues.
    ask_thread: Option<AskThread>,
    /// The screen question in flight: its question and the thread it extends.
    ask_inflight: Option<(String, AskThread)>,
    /// k9s-format views.yaml: per-resource columns and default sort.
    views: crate::views::Views,
    /// Data-only ui.yaml: borders, chrome rows, table style, color overrides.
    ui: crate::ui_config::UiConfig,
    /// Ctrl-W: wide columns.
    wide: bool,
    /// Reverse the current sort (pressing the same sort key again).
    sort_desc: bool,
    /// Sort by a column outside `Sort`, by its title (custom or NODE/IP/…).
    sort_column: Option<String>,
    /// What a left click does, per region of the frame drawn last.
    clicks: std::cell::RefCell<Vec<Click>>,
    /// Accessible mode: where the terminal cursor goes after a frame (the
    /// selected row), so screen readers follow the selection.
    focus: std::cell::Cell<Option<(u16, u16)>>,
    /// Copy mode: the mouse is released so the terminal can select text.
    copy_mode: bool,
    /// Time and screen row of the last row click, to tell a double click.
    last_click: Option<(Instant, u16)>,
    /// Recently used namespaces and contexts for digit favorites and :ctx.
    recent: crate::favorites::Recent,
    /// Last (context, namespace, kind) seen by `sync_scope`.
    last_scope: (String, String, usize),
    known_namespaces: Vec<String>,
    restoring_uid: Option<String>,
    subviews: Vec<Modal>,
    log_target: Option<LogTarget>,
    helm_generation: u64,
    helm_cancel: Option<tokio::sync::oneshot::Sender<()>>,
    advice_cancel: Option<tokio::sync::oneshot::Sender<()>>,
    scan_control: Option<std::sync::Arc<crate::scan::Control>>,
    helm_return: Option<(String, crate::helm::Storage)>,
    helm_elapsed: u64,
    last_forward: String,
    forward_selected: usize,
    desired_resource: Option<String>,
    status: String,
    stale: bool,
    truncated: bool,
    /// The watch's initial LIST is still arriving; rows shown so far are real
    /// but incomplete.
    list_loading: bool,
    modal: Modal,
    epoch: u64,
    busy: bool,
    command_epoch: Option<u64>,
    connecting_epoch: Option<u64>,
    watching: bool,
    watch_blocked: bool,
    /// The scope exceeded the live-watch cap: show the first bounded page
    /// through periodic lists instead of an empty, blocked table.
    bounded_poll: bool,
    /// Columns of the table scrolled out to the left (Left/Right), clamped
    /// by each paint, and how many the last paint hid left and right.
    table_scroll: std::cell::Cell<usize>,
    table_hidden: std::cell::Cell<(usize, usize)>,
    /// Pages of a bounded-poll scope: for each page read so far, its start
    /// token (the first page has none) and the position of its first object;
    /// `list_page` is the page shown.
    list_pages: Vec<(Option<String>, usize)>,
    list_page: usize,
    /// The next page's start token and the server's count of the objects
    /// after this page, from the last bounded list.
    list_next: Option<String>,
    list_remaining: Option<i64>,
    /// API discovery waits until the first watch has been requested, so the
    /// initial rows never queue behind a slow catalog.
    pending_discovery: bool,
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
    /// Bumped whenever `items` is replaced or cleared; keys the order cache.
    items_generation: u64,
    /// Memoized filter-and-sort order of `items`, keyed by a fingerprint.
    visible_cache: std::cell::RefCell<Option<(u64, std::rc::Rc<Vec<usize>>)>>,
    /// Every item in sort order, before the filter (see `sorted_all`).
    sorted_cache: std::cell::RefCell<Option<(u64, std::rc::Rc<Vec<usize>>)>>,
    /// The text a filter matches per item ("namespace name status"), for the
    /// items as they are now. Building it reads every object, so it is kept
    /// while a filter is typed: each further key costs only the matching.
    filter_text: std::cell::RefCell<Option<(FilterTextKey, std::rc::Rc<Vec<String>>)>>,
    /// (fingerprint, (healthy, attention)) of the visible rows.
    health_cache: std::cell::RefCell<Option<(u64, (usize, usize))>>,
    metrics_resources: bool,
    /// `g` in :metrics: chart the selected Pod or Node instead of the text view.
    metrics_chart: bool,
    overview: Option<crate::overview::OverviewFeed>,
    selection_ui: selections_ui::State,
    pending_session: Option<(Box<crate::ops::ReviewedOperation>, String)>,
    /// Ctrl-E in the YAML editor: hand the draft to $EDITOR on the next loop.
    pending_editor: bool,
    pending_copilot_setup: bool,
    pending_copilot_question: Option<String>,
    pending_tool: Option<(Box<crate::handoff::Prepared>, String)>,
    /// Ctrl-P command palette, drawn over the current view while open.
    palette: Option<Box<palette::Palette>>,
    /// Recently opened objects (in memory only) for the palette.
    visited: std::collections::VecDeque<palette::Visited>,
    /// Live config reload: polls the config folder on the loop tick.
    config_watch: crate::config_watch::Watcher,
    /// Per-context settings (contexts/NAME.yaml) for the current context.
    ctx_settings: context_settings::State,
    worker: Worker,
}
impl App {
    fn copy_text(&self, text: &str) -> Result<()> {
        let mut stdout = io::stdout().lock();
        crate::clipboard::write_terminal(&mut stdout, &safety::safe_text(text, 253))
            .map_err(|_| "clipboard request failed".into())
    }
    /// Registers a clickable region of the frame being drawn.
    fn add_click(&self, x: u16, y: u16, width: u16, action: ClickAction) {
        if width > 0 {
            self.clicks.borrow_mut().push(Click {
                x,
                y,
                width,
                action,
            });
        }
    }
    /// A region that presses a key written as in a hint: `d`, `Ctrl-D`,
    /// or alternatives such as `y/Enter`.
    fn add_key_click(&self, x: u16, y: u16, width: u16, shortcut: &str) {
        let keys: Vec<_> = if shortcut == "/" {
            vec![crate::keymap::Key::Char('/')]
        } else {
            shortcut
                .split('/')
                .filter_map(crate::keymap::Key::parse)
                .collect()
        };
        if !keys.is_empty() {
            self.add_click(x, y, width, ClickAction::Keys(keys));
        }
    }
    /// Makes the keys named in a hint row clickable. Segments are separated
    /// by ` | ` or two spaces and start with their key: `y/Enter execute`.
    fn add_hint_clicks(&self, y: u16, text: &str) {
        let mut x = 0u16;
        for segment in text.split(" | ").flat_map(|part| {
            // Keep the separators' widths so columns stay exact.
            let mut pieces = Vec::new();
            let mut rest = part;
            while let Some(at) = rest.find("  ") {
                pieces.push((&rest[..at], 2u16));
                rest = rest[at + 2..].trim_start_matches(' ');
            }
            pieces.push((rest, 3u16));
            pieces
        }) {
            let (segment, separator) = segment;
            let lead = text_cells(segment) - text_cells(segment.trim_start());
            let body = segment.trim();
            if let Some(shortcut) = body.split_whitespace().next()
                && body.contains(' ')
            {
                self.add_key_click(x + lead, y, text_cells(body), shortcut);
            }
            x = x.saturating_add(text_cells(segment) + separator);
        }
    }
    fn click_at(&self, column: u16, row: u16) -> Option<ClickAction> {
        self.clicks
            .borrow()
            .iter()
            .rev()
            .find(|click| {
                click.y == row && column >= click.x && column < click.x.saturating_add(click.width)
            })
            .map(|click| click.action.clone())
    }
    /// True where a letter is an action, not text being typed.
    fn letters_are_actions(&self) -> bool {
        if self.doc_search.typing() {
            return false;
        }
        palette::command_prompt_allowed(&self.modal)
            || (!review_ui::strict()
                && matches!(
                    self.modal,
                    Modal::Confirm(..)
                        | Modal::SessionConfirm(..)
                        | Modal::WriteReview(..)
                        | Modal::HelmReview(..)
                        | Modal::AdviceReview(..)
                ))
    }
    /// A mouse event; `true` quits, like a key.
    fn mouse(&mut self, mouse: MouseEvent, width: u16, height: u16) -> Result<bool> {
        let left = matches!(mouse.kind, MouseEventKind::Down(MouseButton::Left));
        if matches!(self.modal, Modal::None | Modal::LiveOverview(..))
            && mouse.row == 0
            && (self.ui.header.show || matches!(self.modal, Modal::LiveOverview(..)))
            && left
            && topbar::help_at(width, mouse.column)
        {
            if let Err(error) = self.press_help() {
                self.status = error;
            }
            return Ok(false);
        }
        // The palette owns the keyboard while it is open.
        if self.palette.is_some() {
            return Ok(false);
        }
        if width >= 28
            && height >= 10
            && self.mouse_captured()
            && matches!(
                mouse.kind,
                MouseEventKind::Down(MouseButton::Left)
                    | MouseEventKind::Drag(MouseButton::Left)
                    | MouseEventKind::Up(MouseButton::Left)
            )
        {
            let top = self.view_top(width, height);
            if let Modal::Follow(view) = &mut self.modal {
                let viewport = log_viewport(log_panel_area(view, top, width, height));
                if view.mouse_selection(mouse, viewport, self.ui.local_time()) {
                    return Ok(false);
                }
            }
        }
        if left && let Some(action) = self.click_at(mouse.column, mouse.row) {
            match action {
                ClickAction::Sort(title) => {
                    if matches!(self.modal, Modal::None) {
                        self.sort_by_title(&title, None);
                    }
                    return Ok(false);
                }
                ClickAction::Keys(keys) => {
                    // Where letters are typed text, only Enter, Esc and
                    // Ctrl keys are pressed for a click.
                    let letters = self.letters_are_actions();
                    let Some(key) = keys
                        .into_iter()
                        .find(|key| letters || !matches!(key, crate::keymap::Key::Char(_)))
                    else {
                        return Ok(false);
                    };
                    let (code, modifiers) = key.event();
                    return self.key_event(code, modifiers);
                }
            }
        }
        if matches!(self.modal, Modal::LiveOverview(..)) {
            return Ok(false);
        }
        // Ctrl or Shift with the wheel scrolls sideways: the table's columns,
        // or the lines of the log view. A sideways wheel does it unmodified.
        let sideways = mouse
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::SHIFT);
        let horizontal = match mouse.kind {
            MouseEventKind::ScrollDown if sideways => Some(true),
            MouseEventKind::ScrollUp if sideways => Some(false),
            MouseEventKind::ScrollRight => Some(true),
            MouseEventKind::ScrollLeft => Some(false),
            _ => None,
        };
        if let Some(right) = horizontal {
            match self.modal {
                Modal::None => self.scroll_columns(right),
                Modal::Follow(_) => {
                    let code = if right { KeyCode::Right } else { KeyCode::Left };
                    for _ in 0..3 {
                        if self.key_event(code, KeyModifiers::NONE)? {
                            return Ok(true);
                        }
                    }
                }
                _ => {}
            }
            return Ok(false);
        }
        if !matches!(self.modal, Modal::None) {
            // The wheel scrolls the view in front, three lines a notch.
            let code = match mouse.kind {
                MouseEventKind::ScrollDown => KeyCode::Down,
                MouseEventKind::ScrollUp => KeyCode::Up,
                _ => return Ok(false),
            };
            if matches!(self.modal, Modal::Input(..) | Modal::Filter(..)) {
                return Ok(false);
            }
            for _ in 0..3 {
                if self.key_event(code, KeyModifiers::NONE)? {
                    return Ok(true);
                }
            }
            return Ok(false);
        }
        let layout = self.layout(width, height);
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
                let line = mouse.row - layout.table.y - 2;
                if !line.is_multiple_of(layout.step.max(1)) {
                    return Ok(false);
                }
                let row = start + usize::from(line / layout.step.max(1));
                if row >= self.visible().len() {
                    return Ok(false);
                }
                self.selected = row;
                // A second click on the same row opens it, like Enter.
                let again = left
                    && self
                        .last_click
                        .is_some_and(|(at, row)| row == mouse.row && at.elapsed() <= DOUBLE_CLICK);
                self.last_click = (left && !again).then(|| (Instant::now(), mouse.row));
                if again {
                    return self.key_event(KeyCode::Enter, KeyModifiers::NONE);
                }
            }
            _ => {}
        }
        Ok(false)
    }
    /// Copy mode releases the mouse so the terminal's own selection works;
    /// the same key (or `:mouse`) takes it back.
    fn toggle_copy_mode(&mut self) {
        if !self.options.mouse {
            self.status = "The mouse is not captured in this session; select text as usual.".into();
            return;
        }
        self.copy_mode = !self.copy_mode;
        let key = self
            .keymap
            .primary("copy.mode")
            .unwrap_or_else(|| ":mouse".into());
        let result = if self.copy_mode {
            execute!(io::stdout(), DisableMouseCapture)
        } else {
            execute!(io::stdout(), EnableMouseCapture)
        };
        if result.is_ok()
            && let Modal::Follow(view) = &mut self.modal
        {
            view.clear_selection();
        }
        self.status = if result.is_err() {
            self.copy_mode = !self.copy_mode;
            "The terminal refused the mouse mode change.".into()
        } else if self.copy_mode {
            format!(
                "COPY MODE: select text with the mouse and copy it. {key} returns the mouse to KBorg."
            )
        } else {
            "Copy mode off: clicks and the wheel work in KBorg again.".into()
        };
    }
    /// Whether KBorg holds the mouse now.
    fn mouse_captured(&self) -> bool {
        self.options.mouse && !self.copy_mode
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
    /// The first `count` rows in table order, without collecting the rest.
    fn first_visible(&self, count: usize) -> Vec<&Value> {
        self.visible_order()
            .iter()
            .take(count)
            .filter_map(|&index| self.items.get(index).map(|object| &**object))
            .collect()
    }
    fn visible(&self) -> Vec<&Value> {
        self.visible_order()
            .iter()
            .filter_map(|&index| self.items.get(index).map(|object| &**object))
            .collect()
    }
    /// Filtered, sorted indices into `items`. Recomputing compiles the filter
    /// and builds one sort key per object, so the result is memoized against
    /// the filter, sort and the items generation.
    fn visible_order(&self) -> std::rc::Rc<Vec<usize>> {
        let metrics_sort = matches!(self.sort, Sort::Cpu | Sort::Memory);
        let fingerprint = self.visible_fingerprint();
        if !metrics_sort
            && let Some((key, order)) = &*self.visible_cache.borrow()
            && *key == fingerprint
        {
            return order.clone();
        }
        self.sorted_order(fingerprint, metrics_sort)
    }
    /// (healthy, needing attention) among the visible rows, for the summary
    /// line. Classifying an object builds its status text, so the counts are
    /// memoized like the row order instead of recomputed on every frame.
    fn health_counts(&self) -> (usize, usize) {
        let fingerprint = self.visible_fingerprint();
        if let Some((key, counts)) = *self.health_cache.borrow()
            && key == fingerprint
        {
            return counts;
        }
        let mut counts = (0, 0);
        for &index in self.visible_order().iter() {
            match self.items.get(index).map(|object| health(object)) {
                Some(Health::Healthy) => counts.0 += 1,
                Some(Health::Attention) => counts.1 += 1,
                _ => {}
            }
        }
        *self.health_cache.borrow_mut() = Some((fingerprint, counts));
        counts
    }
    /// Identifies the filtered, sorted row set: filter, sort and the items
    /// generation.
    fn visible_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.filter.hash(&mut hasher);
        self.faults_only.hash(&mut hasher);
        self.sort_desc.hash(&mut hasher);
        self.sort_column.hash(&mut hasher);
        self.kind_index.hash(&mut hasher);
        (self.sort as u8).hash(&mut hasher);
        // O(1): every replacement of `items` bumps the generation; the
        // allocation address and length guard direct reassignment.
        self.items_generation.hash(&mut hasher);
        (self.items.as_ptr() as usize).hash(&mut hasher);
        self.items.len().hash(&mut hasher);
        hasher.finish()
    }
    /// All item indices in the current sort order, ascending. Cached per
    /// items generation and sort; CPU/memory sorts follow live samples and
    /// are computed every time.
    fn sorted_all(&self, metrics_sort: bool) -> std::rc::Rc<Vec<usize>> {
        use std::hash::{Hash, Hasher};
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.sort_column.hash(&mut hasher);
        self.kind_index.hash(&mut hasher);
        (self.sort as u8).hash(&mut hasher);
        self.items_generation.hash(&mut hasher);
        (self.items.as_ptr() as usize).hash(&mut hasher);
        self.items.len().hash(&mut hasher);
        let key = hasher.finish();
        if !metrics_sort
            && let Some((cached, order)) = &*self.sorted_cache.borrow()
            && *cached == key
        {
            return order.clone();
        }
        let mut order: Vec<usize> = (0..self.items.len()).collect();
        order.sort_by_cached_key(|&index| {
            let object = &self.items[index];
            let usage = |pick: fn(&table_metrics::Usage) -> u64| {
                std::cmp::Reverse(self.table_metrics.sort_usage(object).map(pick))
            };
            if let Some(title) = &self.sort_column {
                let text = self.column_text(object, title);
                let primary = match text
                    .trim_end_matches(|c: char| c.is_ascii_alphabetic())
                    .parse::<i64>()
                {
                    Ok(number) if !text.is_empty() => SortKey::Number(number),
                    _ => SortKey::Text(text),
                };
                return (
                    primary,
                    namespace(object).to_owned(),
                    name(object).to_owned(),
                );
            }
            let primary = match self.sort {
                Sort::Name => SortKey::Text(name(object).to_owned()),
                Sort::Namespace => SortKey::Text(namespace(object).to_owned()),
                Sort::Status => SortKey::Text(summary(object)),
                Sort::Restarts => SortKey::Descending(std::cmp::Reverse(restarts(object))),
                Sort::Ready => SortKey::Pair(ready_sort_key(object)),
                Sort::Cpu => SortKey::Usage(usage(|usage| usage.cpu_nanocores)),
                Sort::Memory => SortKey::Usage(usage(|usage| usage.memory_bytes)),
                Sort::Age => SortKey::Number(timestamp(object).unwrap_or(i64::MAX)),
            };
            (
                primary,
                namespace(object).to_owned(),
                name(object).to_owned(),
            )
        });
        let order = std::rc::Rc::new(order);
        if !metrics_sort {
            *self.sorted_cache.borrow_mut() = Some((key, order.clone()));
        }
        order
    }
    /// The filter text of every item, built once per items generation.
    fn filter_texts(&self) -> std::rc::Rc<Vec<String>> {
        let key = (
            self.items_generation,
            self.items.as_ptr() as usize,
            self.items.len(),
        );
        if let Some((cached, texts)) = &*self.filter_text.borrow()
            && *cached == key
        {
            return texts.clone();
        }
        let texts = std::rc::Rc::new(
            self.items
                .iter()
                .map(|object| format!("{} {} {}", namespace(object), name(object), summary(object)))
                .collect::<Vec<_>>(),
        );
        *self.filter_text.borrow_mut() = Some((key, texts.clone()));
        texts
    }
    fn sorted_order(&self, fingerprint: u64, metrics_sort: bool) -> std::rc::Rc<Vec<usize>> {
        let filter = nav::Filter::parse(&self.filter);
        let match_all = self.filter.is_empty() && filter.is_ok();
        // No filter: nothing to match, and the cached text is given back.
        let texts = if match_all {
            self.filter_text.borrow_mut().take();
            None
        } else {
            Some(self.filter_texts())
        };
        // Every item in sort order first (kept while the items and the sort
        // stay the same), then the filter: typing a filter never sorts again.
        let mut order: Vec<usize> = self
            .sorted_all(metrics_sort)
            .iter()
            .copied()
            .filter(|&index| {
                !self.faults_only || crate::workload_actions::faulty(&self.items[index])
            })
            .filter(|&index| {
                match_all
                    || texts.as_ref().is_some_and(|texts| {
                        filter
                            .as_ref()
                            .is_ok_and(|filter| filter.matches(&texts[index]))
                    })
            })
            .collect();
        if self.sort_desc {
            order.reverse();
        }
        let order = std::rc::Rc::new(order);
        if !metrics_sort {
            *self.visible_cache.borrow_mut() = Some((fingerprint, order.clone()));
        }
        order
    }
    /// Metadata rows unless a views.yaml column for this resource reads more
    /// than metadata (or the built-in IMAGES column, which reads the spec).
    /// The gateway still keeps full objects for kinds not known to need only
    /// metadata.
    fn watch_projection(&self) -> crate::watch::Projection {
        let needs_more = self.current_view().is_some_and(|view| {
            view.columns.iter().any(|spec| match &spec.path {
                Some(path) => !crate::views::metadata_path(path),
                None => spec.title == "IMAGES" || spec.computed.is_some(),
            })
        });
        if needs_more {
            crate::watch::Projection::Full
        } else {
            crate::watch::Projection::Metadata
        }
    }
    /// The PODS cell of a Node: its count, `N+` when the count stopped at its
    /// bound, `-` before the first count.
    fn node_pods_text(&self, node: &Value) -> Option<String> {
        let (_, counts, truncated) = self.node_pods.as_ref()?;
        let name = node["metadata"]["name"].as_str()?;
        let count = counts.get(name).copied().unwrap_or(0);
        Some(if *truncated {
            format!("{count}+")
        } else {
            count.to_string()
        })
    }
    /// Whether the table in front shows the PODS column (Nodes by default).
    fn shows_node_pods(&self) -> bool {
        self.kind() == "nodes"
            && self.current_view().is_some_and(|view| {
                view.columns
                    .iter()
                    .any(|column| column.computed == Some(crate::views::Computed::Pods))
            })
    }
    /// The view for the current resource: views.yaml first, then the
    /// built-in k9s-style columns for the kinds that have them, then a
    /// custom resource's CRD printer columns.
    fn current_view(&self) -> Option<&crate::views::View> {
        let kind = self.kinds.get(self.kind_index)?;
        let find = |views| crate::views::find(views, &kind.group, &kind.version, &kind.plural);
        find(&self.views)
            .or_else(|| find(crate::views::builtin()))
            .or_else(|| find(&self.crd_views))
    }
    /// Ask once per connection for the CRD columns of the custom resource in
    /// front, unless views.yaml or a built-in view already sets its columns.
    fn request_printer_columns(&mut self) {
        let Some(kind) = self.kinds.get(self.kind_index) else {
            return;
        };
        if kind.group.is_empty() || self.current_view().is_some() {
            return;
        }
        let key = format!("{}/{}/{}", kind.group, kind.version, kind.plural);
        if self.crd_views_requested.insert(key.clone()) {
            self.send(Task::PrinterColumns(key));
        }
    }
    /// Text of the column titled `title` for sorting by any column.
    fn column_text(&self, object: &Value, title: &str) -> String {
        let now = k8s_openapi::jiff::Timestamp::now().as_second();
        if let Some(spec) = self
            .current_view()
            .and_then(|view| view.columns.iter().find(|spec| spec.title == title))
            && (spec.path.is_some() || spec.computed.is_some())
        {
            let usage = self.table_metrics.usage(object, Instant::now());
            return format::custom_value(
                spec,
                object,
                now,
                usage,
                self.node_pods_text(object).as_deref(),
            );
        }
        format::builtin_column(title).map_or_else(String::new, |column| column.value(object, now))
    }
    /// Record recent namespaces/contexts and apply a view's default sort when
    /// the resource changes. Called once per loop iteration; cheap when idle.
    fn sync_scope(&mut self) {
        if self.connected
            && (self.context != self.last_scope.0 || self.namespace != self.last_scope.1)
            && self.recent.visit(&self.context, &self.namespace)
        {
            let _ = crate::favorites::save(&self.recent);
        }
        if self.kind_index != self.last_scope.2 {
            self.table_scroll.set(0);
            if let Some((title, descending)) =
                self.current_view().and_then(|view| view.sort.clone())
            {
                self.sort_column = Some(title);
                self.sort_desc = descending;
            } else if self.sort_column.is_some() {
                self.sort_column = None;
                self.sort_desc = false;
            }
        }
        self.last_scope = (
            self.context.clone(),
            self.namespace.clone(),
            self.kind_index,
        );
    }
    /// The title of the column the table is sorted by.
    fn sort_title(&self) -> String {
        self.sort_column.clone().unwrap_or_else(|| {
            match self.sort {
                Sort::Name => "NAME",
                Sort::Namespace => "NAMESPACE",
                Sort::Status => "STATUS",
                Sort::Restarts => "RSTS",
                Sort::Age => "AGE",
                Sort::Ready => "READY",
                Sort::Cpu => "CPU",
                Sort::Memory => "MEM",
            }
            .into()
        })
    }
    /// Sort by the column right (or left) of the current sort column, as
    /// the table shows them now; wraps around at the ends.
    fn sort_step(&mut self, forward: bool, width: u16, height: u16) {
        let rect = self.layout(width, height).table;
        let custom: Vec<crate::views::ColumnSpec> = self
            .current_view()
            .map(|view| view.columns.clone())
            .unwrap_or_default();
        let columns = table_columns(self, usize::from(rect.width.saturating_sub(5)));
        if columns.is_empty() {
            return;
        }
        let current = self.sort_title();
        let at = columns.iter().position(|(column, _)| {
            column_matches(*column, column_title(*column, &custom), &current)
        });
        let next = match at {
            Some(at) if forward => (at + 1) % columns.len(),
            Some(at) => (at + columns.len() - 1) % columns.len(),
            None => 0,
        };
        let title = column_title(columns[next].0, &custom).to_owned();
        self.sort_by_title(&title, Some(false));
    }
    /// Keep the sort column and set its direction.
    fn sort_direction(&mut self, descending: bool) {
        let title = self.sort_title();
        self.sort_by_title(&title, Some(descending));
    }
    /// Sort by any column title; `None` order toggles when already sorted by it.
    fn sort_by_title(&mut self, title: &str, descending: Option<bool>) {
        let title = title.to_ascii_uppercase();
        let enum_sort = match title.as_str() {
            "NAME" => Some(Sort::Name),
            "NAMESPACE" | "NS" => Some(Sort::Namespace),
            "STATUS" => Some(Sort::Status),
            "RSTS" | "RESTARTS" => Some(Sort::Restarts),
            "AGE" => Some(Sort::Age),
            "READY" => Some(Sort::Ready),
            "CPU" | "CPU(M)" => Some(Sort::Cpu),
            "MEM" | "MEMORY" => Some(Sort::Memory),
            _ => None,
        };
        if let Some(sort) = enum_sort {
            self.set_sort(sort);
            if let Some(descending) = descending {
                self.sort_desc = descending;
            }
            return;
        }
        let known = format::builtin_column(&title).is_some()
            || self
                .current_view()
                .is_some_and(|view| view.columns.iter().any(|spec| spec.title == title));
        if !known {
            self.status =
                format!("No column {title} here; :sort takes a column title from the header.");
            return;
        }
        let toggled = self.sort_column.as_deref() == Some(title.as_str());
        self.sort_desc = descending.unwrap_or(toggled && !self.sort_desc);
        self.sort_column = Some(title.clone());
        self.selected = 0;
        self.status = format!(
            "Sorted by {title} {}.",
            if self.sort_desc {
                "descending"
            } else {
                "ascending"
            }
        );
    }
    fn set_sort(&mut self, sort: Sort) {
        let uid = self.selected_uid();
        self.sort_desc = self.sort == sort && self.sort_column.is_none() && !self.sort_desc;
        self.sort_column = None;
        self.sort = sort;
        self.selected = uid
            .and_then(|uid| {
                self.visible()
                    .iter()
                    .position(|v| v["metadata"]["uid"].as_str() == Some(uid.as_str()))
            })
            .unwrap_or(0);
        self.status = format!(
            "Sorted by {}{}. Press again to reverse.",
            sort.label(),
            match (
                matches!(sort, Sort::Restarts | Sort::Cpu | Sort::Memory),
                self.sort_desc
            ) {
                (true, false) | (false, true) => " (highest first)",
                _ => "",
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
        lines.extend(self.hotkeys.iter().map(|hotkey| {
            format!(
                "{:<34} :{}  {}",
                format!("hotkey {}", hotkey.shortcut),
                hotkey.command,
                hotkey.description
            )
        }));
        self.modal = Modal::Picker("Aliases", lines, 0);
    }
}

/// The theme for this session: `--theme`, else ui.yaml `theme`, else the saved
/// choice, with ui.yaml color overrides. A ui.yaml problem is reported and
/// falls back to the saved theme without overrides.
fn ui_theme(ui: &crate::ui_config::UiConfig, cli: Option<&str>) -> Result<(Theme, Option<String>)> {
    let saved = || themes::find(cli.map_or_else(themes::selected, str::to_owned).as_str());
    let (base, mut error) = match (cli, &ui.theme) {
        (None, Some(id)) => match themes::find(id) {
            Ok(theme) => (theme, None),
            Err(error) => (saved()?, Some(format!("ui.yaml {error}"))),
        },
        _ => (saved()?, None),
    };
    let theme = match ui.apply(&base) {
        Ok(theme) => theme,
        Err(failure) => {
            error = error.or(Some(failure));
            base
        }
    };
    Ok((theme, error))
}

/// The describe view's text for one object, for `kborg describe`.
pub fn describe_overview(object: &Value, context: &str) -> String {
    // The view's own key hints mean nothing outside the TUI.
    format::overview_text(object, context)
        .lines()
        .filter(|line| !line.starts_with("J JSON"))
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn run(mut options: Options) -> Result<()> {
    let (ui, mut ui_error) = match crate::ui_config::load() {
        Ok(ui) => (ui, None),
        Err(error) => (crate::ui_config::UiConfig::default(), Some(error)),
    };
    crate::text::set_unicode(ui.text.unicode());
    review_ui::set_strict(options.strict_confirm);
    crate::audit_sink::set_strict(options.strict_confirm);
    let first_run = options.theme.is_none() && ui.theme.is_none() && themes::first_run()?;
    let (theme, theme_error) = ui_theme(&ui, options.theme.as_deref())?;
    ui_error = ui_error.or(theme_error);
    let mut terminal_guard = Some(Terminal::enter(options.mouse)?);
    if options.splash
        && ui.splash.unwrap_or_else(themes::animation_enabled)
        && !startup_animation(&theme, options.color)?
    {
        return Ok(());
    }
    if !options.connection.demo {
        // Resolve remembered dashboards before consent so every startup target
        // is named before any worker/client or auth helper can be started.
        if options.dashboard_last
            && options.dashboard.is_empty()
            && let Some(last) = crate::dashboard_memory::load()?
        {
            options.connection.namespace = last.namespace;
            options.dashboard = last.contexts;
        }
        if !connection_prompt::confirm(&mut options, &theme)? {
            return Ok(());
        }
    }
    let (aliases, alias_error) = match crate::aliases::load() {
        Ok(aliases) => (aliases, None),
        Err(error) => (Default::default(), Some(error)),
    };
    let (hotkeys, hotkey_error) = match crate::hotkeys::load() {
        Ok(hotkeys) => (hotkeys, None),
        Err(error) => (Default::default(), Some(error)),
    };
    let (keymap, keymap_error) = match crate::keymap::load()
        .and_then(|keymap| keymap.check_hotkeys(&hotkeys).map(|()| keymap))
    {
        Ok(keymap) => (keymap, None),
        Err(error) => (Default::default(), Some(error)),
    };
    let plugins = crate::plugins::Catalog::load(&keymap, &hotkeys);
    let (views, view_error) = match crate::views::load() {
        Ok(views) => (views, None),
        Err(error) => (Default::default(), Some(error)),
    };
    let initial_selector = options.selector.clone();
    let config_watch = crate::config_watch::Watcher::from_env(&theme.id, ui.reload);
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
        hotkeys,
        plugins,
        keymap,
        faults_only: false,
        doc_search: doc_search::DocSearch::default(),
        header: header_block::State::default(),
        ask_screen: None,
        tour: None,
        tour_after_themes: false,
        undo_pending: None,
        undo: None,
        node_pods: None,
        node_pods_requested: None,
        crd_views: Default::default(),
        crd_views_requested: Default::default(),
        overview_events: None,
        ask_keys: None,
        secret_draft: None,
        masked_documents: Vec::new(),
        ask_status: None,
        ask_thread: None,
        ask_inflight: None,
        views,
        ui,
        wide: false,
        sort_desc: false,
        sort_column: None,
        clicks: std::cell::RefCell::new(Vec::new()),
        focus: std::cell::Cell::new(None),
        copy_mode: false,
        last_click: None,
        recent: crate::favorites::load(),
        last_scope: (String::new(), String::new(), usize::MAX),
        known_namespaces: vec![],
        restoring_uid: None,
        subviews: vec![],
        log_target: None,
        helm_generation: 0,
        helm_cancel: None,
        advice_cancel: None,
        scan_control: None,
        helm_return: None,
        helm_elapsed: 0,
        last_forward: worker.forward.text(),
        forward_selected: 0,
        desired_resource: None,
        status: "Starting...".into(),
        stale: true,
        truncated: false,
        list_loading: false,
        modal: Modal::None,
        epoch: 0,
        busy: false,
        command_epoch: None,
        connecting_epoch: None,
        watching: false,
        watch_blocked: false,
        bounded_poll: false,
        table_scroll: std::cell::Cell::new(0),
        table_hidden: std::cell::Cell::new((0, 0)),
        list_pages: vec![(None, 0)],
        list_page: 0,
        list_next: None,
        list_remaining: None,
        pending_discovery: false,
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
        items_generation: 0,
        visible_cache: Default::default(),
        sorted_cache: Default::default(),
        filter_text: Default::default(),
        health_cache: Default::default(),
        metrics_resources: false,
        metrics_chart: false,
        overview: None,
        selection_ui: Default::default(),
        pending_session: None,
        pending_editor: false,
        pending_copilot_setup: false,
        pending_copilot_question: None,
        pending_tool: None,
        palette: None,
        visited: Default::default(),
        config_watch,
        ctx_settings: context_settings::State::new(
            safety::config_dir()
                .ok()
                .map(|dir| dir.join(crate::context_config::DIR_NAME)),
        ),
        worker,
    };
    if first_run {
        themes::use_theme(themes::DEFAULT_THEME)?;
        app.theme_picker()?;
        app.tour_after_themes = true;
        app.status = "Welcome! Arrows preview, Enter chooses; Esc keeps Flexoki Dark.".into();
    }
    if let Some(error) = ui_error {
        app.modal = Modal::Detail(
            format!(
                "Custom UI settings unavailable\n{error}\n\nThe built-in layout and the saved theme remain in use. Fix ui.yaml, then :ui reload. Esc closes this notice."
            ),
            0,
        );
    }
    if let Some(error) = view_error {
        app.modal = Modal::Detail(
            format!(
                "Custom views unavailable\n{error}\n\nDefault columns remain available. Esc closes this notice."
            ),
            0,
        );
    }
    if let Some(note) = app.keymap.notes().first() {
        // Not an error: the user's keys are in force. Say once what moved.
        app.status = format!(
            "keymap.yaml: {note}{}. :keymap lists your keys.",
            match app.keymap.notes().len() {
                1 => String::new(),
                more => format!(" (and {} more)", more - 1),
            }
        );
        app.sticky_status = true;
    }
    if let Some(error) = keymap_error {
        app.modal = Modal::Detail(
            format!(
                "Custom keymap unavailable\n{error}\n\nDefault keys remain available; :keymap lists them. Esc closes this notice."
            ),
            0,
        );
    }
    if let Some(error) = hotkey_error {
        app.modal = Modal::Detail(
            format!(
                "Custom hotkeys unavailable\n{error}\n\nBuilt-in keys remain available. Esc closes this notice."
            ),
            0,
        );
    }
    if let Some(error) = alias_error {
        app.modal = Modal::Detail(
            format!(
                "Custom aliases unavailable\n{error}\n\nBuilt-in navigation remains available. Esc closes this notice."
            ),
            0,
        );
    }
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
    if !app.options.dashboard.is_empty() && matches!(app.modal, Modal::None) {
        // Open the dashboard at once and connect the main view alongside it,
        // so the cards never wait for the default context.
        let names = std::mem::take(&mut app.options.dashboard);
        if let Err(error) = app.open_dashboard(names) {
            app.status = error;
        }
        app.prepare_context_settings();
        app.connecting_epoch = Some(app.epoch);
        app.server = "connecting...".into();
        app.send(Task::Connect(app.options.connection.clone()));
    } else {
        app.connect();
    }
    draw(&mut app)?;
    let mut dirty = false;
    let mut last_activity = Instant::now();
    let mut input = input::Input::new(input::Terminal);
    loop {
        if app.receive() {
            dirty = true;
        }
        if std::mem::take(&mut app.pending_copilot_setup) {
            drop(terminal_guard.take());
            let setup = crate::copilot_setup::interactive();
            terminal_guard = Some(Terminal::enter(app.mouse_captured())?);
            app.finish_copilot_setup(setup);
            draw(&mut app)?;
            continue;
        }
        if std::mem::take(&mut app.pending_editor)
            && let Modal::YamlEditor(editor) = &mut app.modal
        {
            let text = editor.text();
            drop(terminal_guard.take());
            let result =
                crate::external_editor::edit(&text, crate::manifest_edit::MAX_DOCUMENT as u64);
            terminal_guard = Some(Terminal::enter(app.mouse_captured())?);
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
        if std::mem::take(&mut app.ctx_settings.pending_edit) {
            match app.context_settings_text() {
                Ok((context, text)) => {
                    drop(terminal_guard.take());
                    let result = crate::external_editor::edit(&text, 4096);
                    terminal_guard = Some(Terminal::enter(app.mouse_captured())?);
                    app.finish_context_edit(&context, result);
                }
                Err(error) => app.status = error,
            }
            dirty = true;
        }
        if let Some((prepared, typed)) = app.pending_tool.take() {
            drop(terminal_guard.take());
            let allowed = app.options.allow_extensions && app.options.allow_tools;
            let result = crate::handoff::run(*prepared, &typed, allowed);
            terminal_guard = Some(Terminal::enter(app.mouse_captured())?);
            app.modal = Modal::Detail(
                match result {
                    Ok(outcome) => format!(
                        "TOOL OUTPUT ({})\n{}{}",
                        outcome.status,
                        outcome.stdout,
                        if outcome.stderr.is_empty() {
                            String::new()
                        } else {
                            format!("\n\nTool stderr:\n{}", outcome.stderr)
                        }
                    ),
                    Err(error) => format!("Tool did not run\n{error}"),
                },
                0,
            );
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
            terminal_guard = Some(Terminal::enter(app.mouse_captured())?);
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
                        > Duration::from_secs(if app.bounded_poll {
                            30
                        } else if app.stale {
                            10
                        } else {
                            5
                        })))
        {
            app.refresh = false;
            app.last_refresh = Instant::now();
            app.send(
                if app.options.poll || app.options.connection.demo || app.bounded_poll {
                    Task::List(
                        app.kind(),
                        app.namespace.clone(),
                        app.selector.clone(),
                        app.field_selector.clone(),
                        app.list_start(),
                    )
                } else {
                    Task::Watch(
                        app.kind(),
                        app.namespace.clone(),
                        app.selector.clone(),
                        app.field_selector.clone(),
                        app.watch_projection(),
                    )
                },
            );
            dirty = true;
        }
        if app.connected && !app.options.connection.demo && matches!(app.modal, Modal::None) {
            app.request_printer_columns();
        }
        if app.pending_discovery && app.connected && !app.busy {
            app.pending_discovery = false;
            app.send(Task::Discover);
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
        if app.poll_header_info() {
            dirty = true;
        }
        if app.poll_config(Instant::now()) {
            dirty = true;
        }
        if let Modal::HelmRunning(_, started, _) | Modal::ScanRunning(started, _) = &app.modal {
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
        if input.poll(loop_tick(&app, last_activity))? {
            last_activity = Instant::now();
            // A split escape sequence that is not a mouse report is dropped.
            let Some(event) = input.read()? else {
                continue;
            };
            match event {
                Event::Key(key) if key.kind == KeyEventKind::Press => {
                    if key.modifiers.contains(KeyModifiers::CONTROL)
                        && key.code == KeyCode::Char('c')
                        && !matches!(app.modal, Modal::YamlEditor(_) | Modal::SecretEditor(_))
                    {
                        if matches!(app.modal, Modal::AdviceRunning) {
                            let _ = app.key_event(KeyCode::Char('c'), KeyModifiers::CONTROL);
                            dirty = true;
                            continue;
                        } else if matches!(app.modal, Modal::ScanRunning(..)) {
                            app.cancel_scan();
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
                    let pasted = match &mut app.modal {
                        Modal::YamlEditor(editor) => Some(editor.paste(&text)),
                        Modal::SecretEditor(editor) => Some(editor.paste(&text)),
                        _ => None,
                    };
                    if let Some(result) = pasted {
                        if let Err(error) = result {
                            app.status = error;
                        }
                        dirty = true;
                    }
                }
                Event::Resize(_, _) => {
                    if let Modal::Follow(view) = &mut app.modal {
                        view.clear_selection();
                    }
                    dirty = true;
                }
                // Only a log selection uses pointer drags.
                Event::Mouse(mouse)
                    if !app.write_inflight
                        && match mouse.kind {
                            MouseEventKind::Moved => false,
                            MouseEventKind::Drag(MouseButton::Left) => {
                                app.mouse_captured()
                                    && matches!(&app.modal, Modal::Follow(view) if view.dragging())
                            }
                            MouseEventKind::Drag(_) => false,
                            _ => true,
                        } =>
                {
                    let (w, h) = terminal::size().map_err(|_| "terminal size failed")?;
                    if app.mouse(mouse, w, h)? {
                        break;
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
        app.sync_scope();
    }
    Ok(())
}
