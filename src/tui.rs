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
mod commands;
#[path = "ui_dashboard.rs"]
mod dashboard_ui;
mod format;
#[path = "ui_helm.rs"]
mod helm_ui;
mod keyhelp;
mod keys;
#[path = "ui_navigation.rs"]
mod nav;
mod render;
mod replies;
#[path = "ui_review.rs"]
mod review_ui;
#[path = "ui_selections.rs"]
mod selections_ui;
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
    Apply(Box<crate::manifest_apply::PreparedApply>),
    /// A registered read-only extension; it runs outside the TUI screen.
    Tool(Box<crate::handoff::Prepared>),
    NodeShell(Box<crate::node_shell::Prepared>),
    Debug(Box<crate::debug_container::Prepared>),
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
    hotkeys: crate::hotkeys::Hotkeys,
    /// keymap.yaml: effective keys of every registered action.
    keymap: crate::keymap::Keymap,
    /// Ctrl-Z: show only objects that need attention (k9s faults view).
    faults_only: bool,
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
    /// The scope exceeded the live-watch cap: show the first bounded page
    /// through periodic lists instead of an empty, blocked table.
    bounded_poll: bool,
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
    metrics_resources: bool,
    overview: Option<crate::overview::OverviewFeed>,
    selection_ui: selections_ui::State,
    pending_session: Option<(Box<crate::ops::ReviewedOperation>, String)>,
    /// Ctrl-E in the YAML editor: hand the draft to $EDITOR on the next loop.
    pending_editor: bool,
    pending_tool: Option<(Box<crate::handoff::Prepared>, String)>,
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
            && (self.ui.header.show || matches!(self.modal, Modal::LiveOverview(..)))
            && matches!(
                mouse.kind,
                MouseEventKind::Down(crossterm::event::MouseButton::Left)
            )
            && topbar::help_at(width, mouse.column)
        {
            if let Err(error) = self.press_help() {
                self.status = error;
            }
            return;
        }
        if !matches!(self.modal, Modal::None) {
            return;
        }
        let layout = Layout::for_ui(width, height, &self.ui);
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
                    return;
                }
                self.selected = (start + usize::from(line / layout.step.max(1)))
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
        self.visible_order()
            .iter()
            .filter_map(|&index| self.items.get(index))
            .collect()
    }
    /// Filtered, sorted indices into `items`. Recomputing compiles the filter
    /// and builds one sort key per object, so the result is memoized against
    /// the filter, sort and the items generation.
    fn visible_order(&self) -> std::rc::Rc<Vec<usize>> {
        use std::hash::{Hash, Hasher};
        let metrics_sort = matches!(self.sort, Sort::Cpu | Sort::Memory);
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
        let fingerprint = hasher.finish();
        if !metrics_sort
            && let Some((key, order)) = &*self.visible_cache.borrow()
            && *key == fingerprint
        {
            return order.clone();
        }
        let filter = nav::Filter::parse(&self.filter);
        let match_all = self.filter.is_empty() && filter.is_ok();
        let mut order: Vec<usize> = (0..self.items.len())
            .filter(|&index| {
                !self.faults_only || crate::workload_actions::faulty(&self.items[index])
            })
            .filter(|&index| {
                match_all || {
                    let object = &self.items[index];
                    let text =
                        format!("{} {} {}", namespace(object), name(object), summary(object));
                    filter.as_ref().is_ok_and(|filter| filter.matches(&text))
                }
            })
            .collect();
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
        if self.sort_desc {
            order.reverse();
        }
        let order = std::rc::Rc::new(order);
        if !metrics_sort {
            *self.visible_cache.borrow_mut() = Some((fingerprint, order.clone()));
        }
        order
    }
    /// The configured view for the current resource, if any.
    fn current_view(&self) -> Option<&crate::views::View> {
        let kind = self.kinds.get(self.kind_index)?;
        crate::views::find(&self.views, &kind.group, &kind.version, &kind.plural)
    }
    /// Text of the column titled `title` for sorting by any column.
    fn column_text(&self, object: &Value, title: &str) -> String {
        let now = k8s_openapi::jiff::Timestamp::now().as_second();
        if let Some(spec) = self
            .current_view()
            .and_then(|view| view.columns.iter().find(|spec| spec.title == title))
            && spec.path.is_some()
        {
            return format::custom_value(spec, object, now);
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

pub fn run(options: Options) -> Result<()> {
    let (ui, mut ui_error) = match crate::ui_config::load() {
        Ok(ui) => (ui, None),
        Err(error) => (crate::ui_config::UiConfig::default(), Some(error)),
    };
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
    let (views, view_error) = match crate::views::load() {
        Ok(views) => (views, None),
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
        hotkeys,
        keymap,
        faults_only: false,
        views,
        ui,
        wide: false,
        sort_desc: false,
        sort_column: None,
        recent: crate::favorites::load(),
        last_scope: (String::new(), String::new(), usize::MAX),
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
        bounded_poll: false,
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
        metrics_resources: false,
        overview: None,
        selection_ui: Default::default(),
        pending_session: None,
        pending_editor: false,
        pending_tool: None,
        worker,
    };
    if first_run {
        themes::use_theme(themes::DEFAULT_THEME)?;
        app.theme_picker()?;
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
        app.connecting_epoch = Some(app.epoch);
        app.server = "connecting...".into();
        app.send(Task::Connect(app.options.connection.clone()));
    } else {
        app.connect();
    }
    draw(&mut app)?;
    let mut dirty = false;
    let mut last_activity = Instant::now();
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
        if let Some((prepared, typed)) = app.pending_tool.take() {
            drop(terminal_guard.take());
            let allowed = app.options.allow_extensions && app.options.allow_tools;
            let result = crate::handoff::run(*prepared, &typed, allowed);
            terminal_guard = Some(Terminal::enter(app.options.mouse)?);
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
                    )
                } else {
                    Task::Watch(
                        app.kind(),
                        app.namespace.clone(),
                        app.selector.clone(),
                        app.field_selector.clone(),
                    )
                },
            );
            dirty = true;
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
        if event::poll(loop_tick(&app, last_activity)).map_err(|_| "input failed")? {
            last_activity = Instant::now();
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
        app.sync_scope();
    }
    Ok(())
}
