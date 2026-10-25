//! Named keyboard actions and the data-only keymap.yaml that remaps them.
//!
//! Every built-in key is an entry in [`ACTIONS`]: a stable name, a scope
//! (the view it works in), its default keys and a one-line description. The
//! TUI dispatches table keys through this registry, translates remapped keys
//! in the other views, and generates the hint row and `?` help from it, so
//! what is shown is always what works. keymap.yaml can only move keys between
//! registered actions: it never adds behavior, and fixed keys (Esc, Enter,
//! Ctrl-C, review and approval input) cannot be rebound.
use crate::{Result, safety};
use crossterm::event::{KeyCode, KeyModifiers};
use serde::Deserialize;
use std::collections::BTreeMap;

/// A view or context in which a set of actions is active.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Active in every browsing view (table, logs, documents, dashboard...).
    Global,
    Table,
    Log,
    Dashboard,
    Forward,
    Containers,
    Detail,
    Overview,
    Metrics,
    Editor,
    Help,
    Review,
    Prompt,
    Filter,
}

impl Scope {
    pub const ALL: [Scope; 14] = [
        Scope::Global,
        Scope::Table,
        Scope::Log,
        Scope::Dashboard,
        Scope::Forward,
        Scope::Containers,
        Scope::Detail,
        Scope::Overview,
        Scope::Metrics,
        Scope::Editor,
        Scope::Help,
        Scope::Review,
        Scope::Prompt,
        Scope::Filter,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Scope::Global => "global",
            Scope::Table => "table",
            Scope::Log => "log",
            Scope::Dashboard => "dashboard",
            Scope::Forward => "forward",
            Scope::Containers => "containers",
            Scope::Detail => "detail",
            Scope::Overview => "overview",
            Scope::Metrics => "metrics",
            Scope::Editor => "editor",
            Scope::Help => "help",
            Scope::Review => "review",
            Scope::Prompt => "prompt",
            Scope::Filter => "filter",
        }
    }
    pub fn title(self) -> &'static str {
        match self {
            Scope::Global => "EVERYWHERE",
            Scope::Table => "RESOURCE TABLE",
            Scope::Log => "LOG VIEWER",
            Scope::Dashboard => "DASHBOARD",
            Scope::Forward => "PORT FORWARDS (f / :pf)",
            Scope::Containers => "POD CONTAINERS (Enter on a Pod)",
            Scope::Detail => "DESCRIBE, YAML AND DOCUMENT VIEWS",
            Scope::Overview => "OBJECT OVERVIEW",
            Scope::Metrics => "METRICS (:metrics)",
            Scope::Editor => "YAML EDITOR (e)",
            Scope::Help => "HELP",
            Scope::Review => "REVIEWS AND APPROVALS (fixed)",
            Scope::Prompt => "COMMAND PROMPT (fixed)",
            Scope::Filter => "FILTER PROMPT (fixed)",
        }
    }
    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|scope| scope.name() == name)
    }
    /// The action name prefix inside keymap.yaml `scopes:` sections.
    fn prefix(self) -> &'static str {
        match self {
            Scope::Global | Scope::Table => "",
            Scope::Log => "log.",
            Scope::Dashboard => "dashboard.",
            Scope::Forward => "forward.",
            Scope::Containers => "containers.",
            Scope::Detail => "detail.",
            Scope::Overview => "overview.",
            Scope::Metrics => "metrics.",
            Scope::Editor => "editor.",
            Scope::Help => "help.",
            Scope::Review => "review.",
            Scope::Prompt => "prompt.",
            Scope::Filter => "filter.",
        }
    }
    /// Every scope whose actions are live while this view is focused, most
    /// general first. Keys must be unique across all of them.
    pub fn layers(self) -> &'static [Scope] {
        match self {
            Scope::Global => &[Scope::Global],
            Scope::Table => &[Scope::Global, Scope::Table],
            Scope::Log => &[Scope::Global, Scope::Log],
            Scope::Dashboard => &[Scope::Global, Scope::Dashboard],
            Scope::Forward => &[Scope::Global, Scope::Forward],
            Scope::Containers => &[Scope::Global, Scope::Containers],
            Scope::Detail => &[Scope::Global, Scope::Detail],
            Scope::Overview => &[Scope::Global, Scope::Detail, Scope::Overview],
            Scope::Metrics => &[Scope::Global, Scope::Detail, Scope::Metrics],
            Scope::Help => &[Scope::Global, Scope::Detail, Scope::Help],
            Scope::Editor => &[Scope::Editor],
            Scope::Review => &[Scope::Review],
            Scope::Prompt => &[Scope::Prompt],
            Scope::Filter => &[Scope::Filter],
        }
    }
    /// Views where plain characters are typed text; only modified keys can be
    /// bound there.
    fn typed(self) -> bool {
        matches!(self, Scope::Editor)
    }
}

/// Resource-kind classes for table actions whose key depends on the kind.
pub const POD: u8 = 1;
pub const NODE: u8 = 2;
pub const CRONJOB: u8 = 4;
pub const WORKLOAD: u8 = 8;
pub const OTHER: u8 = 16;
pub const ANY: u8 = POD | NODE | CRONJOB | WORKLOAD | OTHER;

pub fn kind_class(kind: &str) -> u8 {
    match kind {
        "pods" => POD,
        "nodes" => NODE,
        "cronjobs.batch" => CRONJOB,
        kind if crate::edit::supports_resource(kind) => WORKLOAD,
        _ => OTHER,
    }
}
pub fn class_label(kinds: u8) -> Option<&'static str> {
    Some(match kinds {
        ANY => return None,
        POD => "Pod",
        NODE => "Node",
        CRONJOB => "CronJob",
        WORKLOAD => "Workload",
        k if k == WORKLOAD | OTHER => "Workload/other",
        k if k == POD | CRONJOB | OTHER => "Pod/CronJob/other",
        k if k == ANY & !NODE => "Non-node",
        _ => "Some kinds",
    })
}

pub struct Action {
    pub name: &'static str,
    pub scope: Scope,
    pub keys: &'static [&'static str],
    /// Kind classes (table only) in which this action owns its keys.
    pub kinds: u8,
    /// Shown for discoverability; keymap.yaml cannot rebind it.
    pub fixed: bool,
    pub description: &'static str,
}

const fn act(
    name: &'static str,
    scope: Scope,
    keys: &'static [&'static str],
    description: &'static str,
) -> Action {
    Action {
        name,
        scope,
        keys,
        kinds: ANY,
        fixed: false,
        description,
    }
}
const fn kind(
    name: &'static str,
    kinds: u8,
    keys: &'static [&'static str],
    description: &'static str,
) -> Action {
    Action {
        name,
        scope: Scope::Table,
        keys,
        kinds,
        fixed: false,
        description,
    }
}
const fn fixed(
    name: &'static str,
    scope: Scope,
    keys: &'static [&'static str],
    description: &'static str,
) -> Action {
    Action {
        name,
        scope,
        keys,
        kinds: ANY,
        fixed: true,
        description,
    }
}

const DIGITS: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8", "9"];
use Scope::{
    Containers, Dashboard, Detail, Editor, Filter, Forward, Global, Help, Log, Metrics, Overview,
    Prompt, Review, Table,
};

/// The single source of truth for KBorg's keyboard.
pub const ACTIONS: &[Action] = &[
    act(
        "help",
        Global,
        &["?"],
        "Help for this view; Tab in help shows every key",
    ),
    act(
        "command",
        Global,
        &[":"],
        "Command prompt (:pods, :ns, :keymap, ...)",
    ),
    // Resource table: actions, most kind-specific first.
    act(
        "filter",
        Table,
        &["/"],
        "Filter rows: regex, !inverse, -f fuzzy, -l selector",
    ),
    act(
        "describe",
        Table,
        &["d"],
        "Describe the selected object (redacted)",
    ),
    act("yaml", Table, &["y", "v"], "View redacted YAML"),
    act("json", Table, &["J"], "View redacted JSON"),
    act(
        "edit",
        Table,
        &["e"],
        "YAML spec editor (--allow-yaml-edit)",
    ),
    act(
        "resources",
        Table,
        &["V"],
        "Edit container resource requests/limits (--write)",
    ),
    act("logs", Table, &["l"], "Follow logs (--allow-logs)"),
    act(
        "logs.previous",
        Table,
        &["L", "p"],
        "Previous container logs (--allow-logs)",
    ),
    kind(
        "shell",
        POD,
        &["s"],
        "Shell into the selected Pod (--allow-exec)",
    ),
    act(
        "exec",
        Table,
        &["E"],
        "Shell into the selected Pod (alternate key)",
    ),
    act(
        "attach",
        Table,
        &["a"],
        "Attach to the selected Pod (--allow-attach)",
    ),
    act(
        "forwards",
        Table,
        &["f"],
        "Port-forward list; b there benchmarks (:pf, :bench)",
    ),
    act(
        "forward",
        Table,
        &["F"],
        "New loopback port-forward (--allow-forward)",
    ),
    kind("node.show", POD, &["o"], "Jump to the selected Pod's Node"),
    kind(
        "node.shell",
        NODE,
        &["s"],
        "Reviewed node shell Pod (--allow-node-shell)",
    ),
    kind(
        "cordon",
        NODE,
        &["c"],
        "Review cordon (--allow-node-maintenance)",
    ),
    kind(
        "uncordon",
        NODE,
        &["u"],
        "Review uncordon (--allow-node-maintenance)",
    ),
    kind(
        "drain",
        NODE,
        &["r"],
        "Review drain (--allow-node-maintenance)",
    ),
    kind(
        "scale",
        WORKLOAD | OTHER,
        &["s"],
        "Scale a Deployment/StatefulSet (reviewed)",
    ),
    kind(
        "restart",
        WORKLOAD,
        &["r"],
        "Reviewed rollout restart; with marks, all marked (:rollback undoes)",
    ),
    kind(
        "trigger",
        CRONJOB,
        &["t"],
        "Trigger a Job from the CronJob now (reviewed)",
    ),
    kind(
        "suspend",
        CRONJOB,
        &["s"],
        "Suspend or resume the CronJob, or all marked (reviewed)",
    ),
    kind(
        "refresh.r",
        POD | CRONJOB | OTHER,
        &["r"],
        "Refresh; reconnects a blocked watch",
    ),
    act("refresh", Table, &["Ctrl-R"], "Refresh the current table"),
    kind(
        "copy.name",
        ANY & !NODE,
        &["c"],
        "Copy name (OSC52 clipboard)",
    ),
    act(
        "copy.namespace",
        Table,
        &["n"],
        "Copy namespace (OSC52 clipboard)",
    ),
    act(
        "warp",
        Table,
        &["w"],
        "Switch to the selected object's namespace",
    ),
    act("ns.all", Table, &["0"], "All namespaces"),
    act(
        "ns.recent",
        Table,
        DIGITS,
        "Jump to your nine most recent namespaces",
    ),
    act(
        "sort.name",
        Table,
        &["N"],
        "Sort by name (again reverses; :sort COLUMN)",
    ),
    act("sort.age", Table, &["A"], "Sort by age"),
    act("sort.status", Table, &["S"], "Sort by status"),
    act("sort.ready", Table, &["R"], "Sort by readiness"),
    act(
        "sort.cpu",
        Table,
        &["C"],
        "Sort Pods/Nodes by CPU, highest first",
    ),
    act(
        "sort.memory",
        Table,
        &["M"],
        "Sort Pods/Nodes by memory, highest first",
    ),
    act(
        "toggle.wide",
        Table,
        &["Ctrl-W"],
        "Wide columns (Node, IP, views.yaml wide)",
    ),
    act(
        "toggle.faults",
        Table,
        &["Ctrl-Z"],
        "Only objects that need attention (:faults)",
    ),
    act(
        "mark",
        Table,
        &["Space"],
        "Mark row; delete/restart/suspend act on all marked (reviewed batch)",
    ),
    act(
        "mark.range",
        Table,
        &["Ctrl-Space"],
        "Mark rows up to the cursor",
    ),
    act("marks.clear", Table, &["Ctrl-\\"], "Clear marks"),
    act(
        "metrics",
        Table,
        &["m"],
        "Metrics for the selection (:metrics)",
    ),
    act(
        "dashboard",
        Table,
        &["D"],
        "Dashboard for this context and namespace",
    ),
    act(
        "selection.save",
        Table,
        &["b"],
        "Save the current selection",
    ),
    act("selection.open", Table, &["B"], "Open saved selections"),
    act(
        "snapshot",
        Table,
        &["Ctrl-S"],
        "Save visible rows as a redacted snapshot (:sd)",
    ),
    act(
        "aliases",
        Table,
        &["Ctrl-A"],
        "Resource alias and hotkey picker (:aliases)",
    ),
    act(
        "delete",
        Table,
        &["Ctrl-D", "Delete"],
        "Reviewed deletion; with marks, one batch for all marked",
    ),
    act(
        "kill",
        Table,
        &["Ctrl-K"],
        "Reviewed zero-grace Pod kill (--allow-force-kill)",
    ),
    act(
        "scan",
        Table,
        &[],
        "Scan selected images with --scanner (:scan; unbound by default)",
    ),
    // Resource table: navigation.
    act("quit", Table, &["q"], "Quit (:q also works)"),
    fixed(
        "back",
        Table,
        &["Esc"],
        "Clear filter, then selector, then go back",
    ),
    fixed(
        "drilldown",
        Table,
        &["Enter"],
        "Drill into containers, related Pods or namespace",
    ),
    act(
        "prompt",
        Table,
        &["Tab", "Shift-Tab"],
        "Open the command prompt with completion",
    ),
    act("down", Table, &["j", "Down"], "Move down"),
    act("up", Table, &["k", "Up"], "Move up"),
    act("page.down", Table, &["PgDown"], "Page down"),
    act("page.up", Table, &["PgUp"], "Page up"),
    act("top", Table, &["Home"], "First row"),
    act("bottom", Table, &["End"], "Last row"),
    act("view.back", Table, &["["], "Previous view"),
    act("view.forward", Table, &["]"], "Next view"),
    act("view.last", Table, &["-"], "Toggle to the last view"),
    // Log viewer.
    act(
        "log.close",
        Log,
        &["q"],
        "Close logs (Esc also clears a filter, then closes)",
    ),
    act(
        "log.filter",
        Log,
        &["/"],
        "Filter lines (Enter also opens it)",
    ),
    act("log.errors", Log, &["e"], "Only error lines"),
    act("log.warnings", Log, &["W"], "Only warning lines"),
    act("log.autoscroll", Log, &["s"], "Pause or resume autoscroll"),
    act("log.timestamps", Log, &["t"], "Timestamps"),
    act("log.wrap", Log, &["w"], "Wrap long lines"),
    act("log.fullscreen", Log, &["f"], "Full screen"),
    act("log.lock", Log, &["L"], "Lock the horizontal column"),
    act("log.clear", Log, &["C"], "Clear the visible buffer"),
    act("log.mark", Log, &["m"], "Insert a mark line"),
    act("log.all", Log, &["a"], "Toggle all ordinary containers"),
    act(
        "log.range",
        Log,
        &["0", "1", "2", "3", "4", "5", "6"],
        "Tail, head, last 1m 5m 15m 30m 1h",
    ),
    act("log.latest", Log, &["G", "End"], "Jump to the latest line"),
    act("log.first", Log, &["g", "Home"], "Jump to the first line"),
    act("log.up", Log, &["k", "Up"], "Scroll up"),
    act("log.down", Log, &["j", "Down"], "Scroll down"),
    act("log.page-up", Log, &["PgUp"], "Page up"),
    act("log.page-down", Log, &["PgDown"], "Page down"),
    act("log.left", Log, &["h", "Left"], "Scroll left"),
    act("log.right", Log, &["l", "Right"], "Scroll right"),
    act("log.reload", Log, &["Ctrl-R"], "Reload the current range"),
    act(
        "log.export",
        Log,
        &["Ctrl-S"],
        "Export visible lines to a local file",
    ),
    // Dashboard.
    act(
        "dashboard.focus",
        Dashboard,
        DIGITS,
        "Focus a card; its number again is full screen",
    ),
    act(
        "dashboard.next",
        Dashboard,
        &["l", "Right", "j", "Down", "Tab"],
        "Next card",
    ),
    act(
        "dashboard.prev",
        Dashboard,
        &["h", "Left", "k", "Up", "Shift-Tab"],
        "Previous card",
    ),
    act("dashboard.first", Dashboard, &["Home"], "First card"),
    act("dashboard.last", Dashboard, &["End"], "Last card"),
    fixed(
        "dashboard.open",
        Dashboard,
        &["Enter"],
        "Open the context's Pods (or warning Events)",
    ),
    act(
        "dashboard.warning",
        Dashboard,
        &["w"],
        "Select the next recent warning",
    ),
    act(
        "dashboard.logs",
        Dashboard,
        &["L"],
        "Follow the warning Pod's logs in a pane",
    ),
    act(
        "dashboard.tail",
        Dashboard,
        &["t"],
        "Tail chosen workloads across every cluster",
    ),
    act(
        "dashboard.errors",
        Dashboard,
        &["e"],
        "Log pane: only error lines",
    ),
    act(
        "dashboard.warnings",
        Dashboard,
        &["W"],
        "Log pane: only warning lines",
    ),
    act(
        "dashboard.contexts",
        Dashboard,
        &["c"],
        "Choose up to nine contexts",
    ),
    act(
        "dashboard.save",
        Dashboard,
        &["s"],
        "Save these contexts and namespace",
    ),
    act(
        "dashboard.saved",
        Dashboard,
        &["B"],
        "Open saved selections",
    ),
    act(
        "dashboard.chart",
        Dashboard,
        &["G"],
        "Charts for the focused card: attention, restarts, Node CPU and memory",
    ),
    act(
        "dashboard.close",
        Dashboard,
        &["q"],
        "Close the dashboard (Esc closes panes first)",
    ),
    // Port forwards.
    act(
        "forward.new",
        Forward,
        &["F"],
        "New port-forward for the selected Pod",
    ),
    act("forward.stop", Forward, &["s"], "Stop the selected forward"),
    act(
        "forward.bench",
        Forward,
        &["b"],
        "Benchmark the selected forward (:bench N C PATH)",
    ),
    act("forward.down", Forward, &["j", "Down"], "Move down"),
    act("forward.up", Forward, &["k", "Up"], "Move up"),
    act("forward.close", Forward, &["q"], "Close (Esc also works)"),
    // Pod containers.
    act(
        "containers.logs",
        Containers,
        &["l"],
        "Follow container logs (Enter also works)",
    ),
    act(
        "containers.previous",
        Containers,
        &["L", "p"],
        "Previous container logs",
    ),
    act(
        "containers.shell",
        Containers,
        &["s"],
        "Shell into the container (--allow-exec)",
    ),
    act(
        "containers.attach",
        Containers,
        &["a"],
        "Attach to the container (--allow-attach)",
    ),
    act(
        "containers.forwards",
        Containers,
        &["f"],
        "Port-forward list",
    ),
    act(
        "containers.forward",
        Containers,
        &["F"],
        "New port-forward (--allow-forward)",
    ),
    act(
        "containers.describe",
        Containers,
        &["d"],
        "Describe the Pod",
    ),
    act("containers.yaml", Containers, &["y", "v"], "Pod YAML"),
    act("containers.down", Containers, &["j", "Down"], "Move down"),
    act("containers.up", Containers, &["k", "Up"], "Move up"),
    act(
        "containers.close",
        Containers,
        &["q"],
        "Back (Esc also works)",
    ),
    // Describe/YAML/documents, also inherited by overview, metrics and help.
    act("detail.close", Detail, &["q"], "Close (Esc also works)"),
    act("detail.down", Detail, &["j", "Down"], "Scroll down"),
    act("detail.up", Detail, &["k", "Up"], "Scroll up"),
    act("detail.page-down", Detail, &["PgDown"], "Page down"),
    act("detail.page-up", Detail, &["PgUp"], "Page up"),
    act("detail.top", Detail, &["Home"], "Top"),
    act("detail.bottom", Detail, &["End"], "Bottom"),
    act("overview.json", Overview, &["J"], "Redacted JSON"),
    act("overview.yaml", Overview, &["y"], "Redacted YAML"),
    act("metrics.refresh", Metrics, &["m", "r"], "Refresh metrics"),
    act(
        "metrics.resources",
        Metrics,
        &["M"],
        "Compare requests/limits (extra reads)",
    ),
    act(
        "metrics.chart",
        Metrics,
        &["g"],
        "Chart CPU and memory over time for the selected Pod or Node",
    ),
    // YAML editor: plain keys are typed text, so only modified keys bind.
    act(
        "editor.review",
        Editor,
        &["Ctrl-S"],
        "Server dry-run and review the draft",
    ),
    act(
        "editor.external",
        Editor,
        &["Ctrl-E"],
        "Edit the draft in $EDITOR (--allow-external-editor)",
    ),
    fixed("editor.cancel", Editor, &["Esc"], "Discard the draft"),
    fixed("editor.indent", Editor, &["Tab"], "Indent"),
    // Help viewer.
    act(
        "help.all",
        Help,
        &["Tab"],
        "Switch between this view's keys and every key",
    ),
    act("help.filter", Help, &["/"], "Filter help lines"),
    // Reviews: typed confirmation input and approval keys never move.
    fixed(
        "review.read",
        Review,
        &["PgDown", "PgUp"],
        "Read every review line before approving",
    ),
    fixed(
        "review.clear",
        Review,
        &["Ctrl-U"],
        "Clear the typed confirmation",
    ),
    fixed(
        "review.approve",
        Review,
        &["Enter"],
        "Approve after typing the exact confirmation",
    ),
    fixed("review.cancel", Review, &["Esc"], "Cancel without changes"),
    fixed(
        "prompt.complete",
        Prompt,
        &["Tab", "Shift-Tab"],
        "Complete / reverse completion",
    ),
    fixed(
        "prompt.history",
        Prompt,
        &["Up", "Down"],
        "Recall command history",
    ),
    fixed("prompt.clear", Prompt, &["Ctrl-U"], "Clear the prompt"),
    fixed("prompt.run", Prompt, &["Enter"], "Run the command"),
    fixed("prompt.cancel", Prompt, &["Esc"], "Cancel"),
    fixed("filter.apply", Filter, &["Enter"], "Keep the filter"),
    fixed(
        "filter.cancel",
        Filter,
        &["Esc"],
        "Restore the previous filter and selection",
    ),
];

/// A key as KBorg sees it, independent of how the terminal encodes it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Ctrl(char),
    Alt(char),
    F(u8),
    Enter,
    Esc,
    Tab,
    BackTab,
    Backspace,
    Delete,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
}

impl Key {
    pub fn from_event(code: KeyCode, modifiers: KeyModifiers) -> Option<Self> {
        Some(match code {
            KeyCode::Char(c) if modifiers.contains(KeyModifiers::CONTROL) => {
                Key::Ctrl(c.to_ascii_lowercase())
            }
            KeyCode::Char(c) if modifiers.contains(KeyModifiers::ALT) => {
                Key::Alt(c.to_ascii_lowercase())
            }
            KeyCode::Char(c) => Key::Char(c),
            KeyCode::F(n) => Key::F(n),
            KeyCode::Enter => Key::Enter,
            KeyCode::Esc => Key::Esc,
            KeyCode::Tab => Key::Tab,
            KeyCode::BackTab => Key::BackTab,
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Delete => Key::Delete,
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::PageUp => Key::PageUp,
            KeyCode::PageDown => Key::PageDown,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            _ => return None,
        })
    }
    pub fn event(self) -> (KeyCode, KeyModifiers) {
        let none = KeyModifiers::NONE;
        match self {
            Key::Char(c) => (KeyCode::Char(c), none),
            Key::Ctrl(c) => (KeyCode::Char(c), KeyModifiers::CONTROL),
            Key::Alt(c) => (KeyCode::Char(c), KeyModifiers::ALT),
            Key::F(n) => (KeyCode::F(n), none),
            Key::Enter => (KeyCode::Enter, none),
            Key::Esc => (KeyCode::Esc, none),
            Key::Tab => (KeyCode::Tab, none),
            Key::BackTab => (KeyCode::BackTab, KeyModifiers::SHIFT),
            Key::Backspace => (KeyCode::Backspace, none),
            Key::Delete => (KeyCode::Delete, none),
            Key::Up => (KeyCode::Up, none),
            Key::Down => (KeyCode::Down, none),
            Key::Left => (KeyCode::Left, none),
            Key::Right => (KeyCode::Right, none),
            Key::PageUp => (KeyCode::PageUp, none),
            Key::PageDown => (KeyCode::PageDown, none),
            Key::Home => (KeyCode::Home, none),
            Key::End => (KeyCode::End, none),
        }
    }
    /// Parses `L`, `Shift-N`, `Ctrl-D`, `Alt-x`, `F5`, `Space`, `Delete`,
    /// `PgDown`, `Shift-Tab` and similar k9s/Omarchy-style names.
    pub fn parse(text: &str) -> Option<Self> {
        let mut chars = text.chars();
        if let (Some(c), None) = (chars.next(), chars.next()) {
            return c.is_ascii_graphic().then_some(Key::Char(c));
        }
        let lower = text.to_ascii_lowercase();
        let named = match lower.as_str() {
            "space" => Some(Key::Char(' ')),
            "enter" | "return" => Some(Key::Enter),
            "esc" | "escape" => Some(Key::Esc),
            "tab" => Some(Key::Tab),
            "shift-tab" | "backtab" => Some(Key::BackTab),
            "backspace" => Some(Key::Backspace),
            "delete" | "del" => Some(Key::Delete),
            "up" => Some(Key::Up),
            "down" => Some(Key::Down),
            "left" => Some(Key::Left),
            "right" => Some(Key::Right),
            "pgup" | "pageup" => Some(Key::PageUp),
            "pgdown" | "pagedown" | "pgdn" => Some(Key::PageDown),
            "home" => Some(Key::Home),
            "end" => Some(Key::End),
            _ => None,
        };
        if named.is_some() {
            return named;
        }
        if let Some(number) = lower.strip_prefix('f')
            && let Ok(number) = number.parse::<u8>()
        {
            return (1..=12).contains(&number).then_some(Key::F(number));
        }
        let (prefix, rest) = text.split_once('-')?;
        let mut chars = rest.chars();
        let single = chars.next().filter(|_| chars.next().is_none());
        match (prefix.to_ascii_lowercase().as_str(), single) {
            ("shift", Some(c)) if c.is_ascii_alphabetic() => {
                Some(Key::Char(c.to_ascii_uppercase()))
            }
            ("ctrl", _) if rest.eq_ignore_ascii_case("space") => Some(Key::Ctrl(' ')),
            ("ctrl", Some(c)) if c.is_ascii_alphanumeric() || "\\[]".contains(c) => {
                Some(Key::Ctrl(c.to_ascii_lowercase()))
            }
            ("alt", Some(c)) if c.is_ascii_alphanumeric() => Some(Key::Alt(c.to_ascii_lowercase())),
            _ => None,
        }
    }
    /// Keys that keep their meaning everywhere: closing, approving, the
    /// terminal interrupt and session escape, and Ctrl keys terminals fold
    /// into Enter, Tab, Backspace or Esc.
    fn reserved(self) -> bool {
        matches!(
            self,
            Key::Esc | Key::Enter | Key::Ctrl('c' | 'm' | 'i' | 'h' | 'j' | '[' | ']')
        )
    }
    fn modified(self) -> bool {
        matches!(self, Key::Ctrl(_) | Key::Alt(_) | Key::F(_))
    }
}

impl std::fmt::Display for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Key::Char(' ') => write!(f, "Space"),
            Key::Char(c) => write!(f, "{c}"),
            Key::Ctrl(' ') => write!(f, "Ctrl-Space"),
            Key::Ctrl(c) => write!(f, "Ctrl-{}", c.to_ascii_uppercase()),
            Key::Alt(c) => write!(f, "Alt-{c}"),
            Key::F(n) => write!(f, "F{n}"),
            Key::Enter => write!(f, "Enter"),
            Key::Esc => write!(f, "Esc"),
            Key::Tab => write!(f, "Tab"),
            Key::BackTab => write!(f, "Shift-Tab"),
            Key::Backspace => write!(f, "Backspace"),
            Key::Delete => write!(f, "Delete"),
            Key::Up => write!(f, "Up"),
            Key::Down => write!(f, "Down"),
            Key::Left => write!(f, "Left"),
            Key::Right => write!(f, "Right"),
            Key::PageUp => write!(f, "PgUp"),
            Key::PageDown => write!(f, "PgDown"),
            Key::Home => write!(f, "Home"),
            Key::End => write!(f, "End"),
        }
    }
}

/// Where a key goes after remapping in a translated (non-table) view.
#[derive(Debug, PartialEq, Eq)]
pub enum Translation {
    /// Not remapped: hand the original key to the view.
    Pass,
    /// A remapped action: hand the view the action's built-in key instead.
    To(Key),
    /// A built-in key that keymap.yaml moved away: ignore it.
    Swallow,
}

/// The effective keys of every registered action.
#[derive(Clone, Debug)]
pub struct Keymap {
    keys: Vec<Vec<Key>>,
    custom: Vec<bool>,
}

impl Default for Keymap {
    fn default() -> Self {
        Self {
            keys: ACTIONS
                .iter()
                .map(|action| {
                    action
                        .keys
                        .iter()
                        .filter_map(|key| Key::parse(key))
                        .collect()
                })
                .collect(),
            custom: vec![false; ACTIONS.len()],
        }
    }
}

fn defaults(index: usize) -> Vec<Key> {
    ACTIONS[index]
        .keys
        .iter()
        .filter_map(|key| Key::parse(key))
        .collect()
}

pub fn find(name: &str) -> Option<usize> {
    ACTIONS.iter().position(|action| action.name == name)
}

impl Keymap {
    pub fn keys(&self, name: &str) -> &[Key] {
        find(name).map_or(&[], |index| self.keys[index].as_slice())
    }
    pub fn matches(&self, name: &str, key: Key) -> bool {
        self.keys(name).contains(&key)
    }
    /// The first key of an action, for hint rows; `None` when unbound.
    pub fn primary(&self, name: &str) -> Option<String> {
        self.keys(name).first().map(Key::to_string)
    }
    /// Every key of an action, compact enough for a help column.
    pub fn label(&self, name: &str) -> String {
        find(name).map_or_else(String::new, |index| label(&self.keys[index]))
    }
    pub fn is_custom(&self, name: &str) -> bool {
        find(name).is_some_and(|index| self.custom[index])
    }
    pub fn any_custom(&self) -> bool {
        self.custom.iter().any(|custom| *custom)
    }
    /// The table action (and the key's position in its list) for `key` when
    /// the selected resource has kind class `class`. When no action owns the
    /// key for this kind, an action owning it for another kind answers, so its
    /// handler can explain why it does not apply here.
    pub fn table_action(&self, key: Key, class: u8) -> Option<(&'static str, usize)> {
        let lookup = |class_filter: u8| {
            ACTIONS.iter().enumerate().find_map(|(index, action)| {
                (matches!(action.scope, Scope::Global | Scope::Table)
                    && action.kinds & class_filter != 0)
                    .then(|| self.keys[index].iter().position(|bound| *bound == key))
                    .flatten()
                    .map(|position| (action.name, position))
            })
        };
        lookup(class).or_else(|| lookup(ANY))
    }
    /// Maps a key pressed in `scope` onto the built-in key its action uses.
    pub fn translate(&self, scope: Scope, key: Key) -> Translation {
        let members = || {
            ACTIONS
                .iter()
                .enumerate()
                .filter(move |(_, action)| scope.layers().contains(&action.scope))
        };
        for (index, _) in members() {
            if let Some(position) = self.keys[index].iter().position(|bound| *bound == key) {
                let builtin = defaults(index);
                if builtin.contains(&key) {
                    return Translation::Pass;
                }
                return builtin
                    .get(position)
                    .or(builtin.first())
                    .map_or(Translation::Swallow, |target| Translation::To(*target));
            }
        }
        if members().any(|(index, _)| defaults(index).contains(&key)) {
            Translation::Swallow
        } else {
            Translation::Pass
        }
    }
    /// Actions of a scope with their effective keys.
    pub fn entries(&self, scope: Scope) -> impl Iterator<Item = (&'static Action, &[Key], bool)> {
        ACTIONS
            .iter()
            .enumerate()
            .filter(move |(_, action)| action.scope == scope)
            .map(|(index, action)| (action, self.keys[index].as_slice(), self.custom[index]))
    }
    fn validate(&self) -> Result<()> {
        for (index, keys) in self.keys.iter().enumerate() {
            let action = &ACTIONS[index];
            for (position, key) in keys.iter().enumerate() {
                if keys[..position].contains(key) {
                    return Err(format!("keymap.yaml lists {key} twice for {}", action.name));
                }
                if self.custom[index] && key.reserved() {
                    return Err(format!(
                        "keymap.yaml cannot bind {key} (Esc, Enter, Ctrl-C and Ctrl-] stay fixed)"
                    ));
                }
                if self.custom[index] && action.scope.typed() && !key.modified() {
                    return Err(format!(
                        "{}: the YAML editor types plain keys; use Ctrl-, Alt- or F keys",
                        action.name
                    ));
                }
            }
        }
        for scope in Scope::ALL {
            let members: Vec<usize> = (0..ACTIONS.len())
                .filter(|index| scope.layers().contains(&ACTIONS[*index].scope))
                .collect();
            for (offset, first) in members.iter().enumerate() {
                for second in &members[offset + 1..] {
                    if ACTIONS[*first].kinds & ACTIONS[*second].kinds == 0 {
                        continue;
                    }
                    if let Some(key) = self.keys[*first]
                        .iter()
                        .find(|key| self.keys[*second].contains(key))
                    {
                        return Err(format!(
                            "keymap.yaml binds {key} to both {} and {} in the {} view",
                            ACTIONS[*first].name,
                            ACTIONS[*second].name,
                            scope.name()
                        ));
                    }
                }
            }
        }
        Ok(())
    }
    /// hotkeys.yaml runs before table keys; a remapped key must not hide one.
    pub fn check_hotkeys(&self, hotkeys: &crate::hotkeys::Hotkeys) -> Result<()> {
        for hotkey in hotkeys {
            let (code, modifiers) = hotkey.shortcut.event();
            let Some(key) = Key::from_event(code, modifiers) else {
                continue;
            };
            if let Some((name, _)) = self.table_action(key, ANY)
                && self.is_custom(name)
            {
                return Err(format!(
                    "keymap.yaml binds {key} for {name}, but hotkeys.yaml already uses {}",
                    hotkey.shortcut
                ));
            }
        }
        Ok(())
    }
}

pub fn label(keys: &[Key]) -> String {
    match keys {
        [] => "(unbound)".into(),
        [first, .., last] if keys.len() > 5 => format!("{first}..{last}"),
        keys => keys
            .iter()
            .map(Key::to_string)
            .collect::<Vec<_>>()
            .join(" / "),
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Binding {
    One(String),
    Many(Vec<String>),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(default)]
    keys: BTreeMap<String, Binding>,
    /// Optional per-scope sections whose action names omit the scope prefix.
    #[serde(default)]
    scopes: BTreeMap<String, BTreeMap<String, Binding>>,
}

const MAX_BYTES: usize = 64 * 1024;
const MAX_BINDINGS: usize = 256;
const MAX_KEYS: usize = 9;

/// Parses and validates keymap.yaml text over the built-in defaults.
pub fn parse(text: &str) -> Result<Keymap> {
    if text.len() > MAX_BYTES {
        return Err("keymap.yaml exceeds 64 KiB".into());
    }
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_reader_input_bytes: Some(64*1024), max_events: 5000, max_aliases: 0, max_anchors: 0,
            max_depth: 5, max_inclusion_depth: 0, max_documents: 1, max_nodes: 2048,
            max_total_scalar_bytes: 64*1024, max_merge_keys: 0,
        },
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys: serde_saphyr::MergeKeyPolicy::Error,
        with_snippet: false,
    };
    let file: File = serde_saphyr::from_str_with_options(text, options).map_err(|_| {
        "invalid keymap.yaml; expected keys: {ACTION: KEY or [KEYS]} and optional scopes: sections"
    })?;
    let mut entries = Vec::new();
    for (name, binding) in file.keys {
        entries.push((name, binding));
    }
    for (scope_name, bindings) in file.scopes {
        let scope = Scope::from_name(&scope_name).ok_or_else(|| {
            format!(
                "keymap.yaml: unknown scope {}",
                safety::safe_text(&scope_name, 63)
            )
        })?;
        for (name, binding) in bindings {
            entries.push((format!("{}{name}", scope.prefix()), binding));
        }
    }
    if entries.len() > MAX_BINDINGS {
        return Err("keymap.yaml exceeds 256 bindings".into());
    }
    let mut keymap = Keymap::default();
    for (name, binding) in entries {
        let shown = safety::safe_text(&name, 63);
        let index = find(&name).ok_or_else(|| {
            format!("keymap.yaml: unknown action {shown}; :keymap lists every action")
        })?;
        if ACTIONS[index].fixed {
            return Err(format!(
                "keymap.yaml: {shown} is fixed so remapping cannot weaken reviews or approvals"
            ));
        }
        if keymap.custom[index] {
            return Err(format!("keymap.yaml binds {shown} twice"));
        }
        let texts = match binding {
            Binding::One(text) => vec![text],
            Binding::Many(texts) => texts,
        };
        if texts.len() > MAX_KEYS {
            return Err(format!("keymap.yaml: {shown} lists more than 9 keys"));
        }
        let mut keys = Vec::new();
        for text in texts {
            let trimmed = text.trim();
            let key = (trimmed.len() <= 32)
                .then(|| Key::parse(trimmed))
                .flatten()
                .ok_or_else(|| {
                    format!(
                        "keymap.yaml: {shown}: {} is not a key; use L, Shift-N, Ctrl-D, Alt-x, F5, Space, Delete or PgDown",
                        safety::safe_text(trimmed, 32)
                    )
                })?;
            keys.push(key);
        }
        keymap.keys[index] = keys;
        keymap.custom[index] = true;
    }
    keymap.validate()?;
    Ok(keymap)
}

pub fn load() -> Result<Keymap> {
    let path = safety::config_dir()?.join("keymap.yaml");
    safety::checked_path(&path)?;
    if !path.exists() {
        return Ok(Keymap::default());
    }
    let bytes = safety::read_small(&path, 64 * 1024)?;
    parse(std::str::from_utf8(&bytes).map_err(|_| "keymap.yaml must be UTF-8")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_defaults_parse_and_never_collide_within_a_view() -> Result<()> {
        for action in ACTIONS {
            for key in action.keys {
                assert!(Key::parse(key).is_some(), "{}: {key}", action.name);
            }
            assert!(
                action.name.len() <= 63
                    && action
                        .name
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b"-.".contains(&b))
            );
            assert!(action.description.len() <= 72, "{}", action.name);
        }
        for (index, action) in ACTIONS.iter().enumerate() {
            assert_eq!(find(action.name), Some(index), "duplicate {}", action.name);
        }
        Keymap::default().validate()
    }

    #[test]
    fn keys_parse_and_display_round_trip() {
        for (text, key, shown) in [
            ("L", Key::Char('L'), "L"),
            ("Shift-N", Key::Char('N'), "N"),
            ("ctrl-d", Key::Ctrl('d'), "Ctrl-D"),
            ("Alt-X", Key::Alt('x'), "Alt-x"),
            ("F5", Key::F(5), "F5"),
            ("Space", Key::Char(' '), "Space"),
            ("Ctrl-Space", Key::Ctrl(' '), "Ctrl-Space"),
            ("Ctrl-\\", Key::Ctrl('\\'), "Ctrl-\\"),
            ("Delete", Key::Delete, "Delete"),
            ("pgdown", Key::PageDown, "PgDown"),
            ("Shift-Tab", Key::BackTab, "Shift-Tab"),
            ("-", Key::Char('-'), "-"),
        ] {
            assert_eq!(Key::parse(text), Some(key), "{text}");
            assert_eq!(key.to_string(), shown);
            assert_eq!(Key::parse(shown), Some(key));
            let (code, modifiers) = key.event();
            assert_eq!(Key::from_event(code, modifiers), Some(key));
        }
        for bad in [
            "", "Ctrl-", "Hyper-x", "F13", "Shift-1", "LL", "Ctrl-é", " ",
        ] {
            assert_eq!(Key::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn keymap_yaml_remaps_actions_with_flat_or_scoped_names() -> Result<()> {
        let keymap = parse(
            "keys:\n  logs: L\n  logs.previous: [p, Alt-p]\n  sort.name: Shift-X\n  delete: Ctrl-X\n  log.wrap: Alt-w\nscopes:\n  dashboard:\n    tail: T\n",
        )?;
        assert_eq!(keymap.keys("logs"), [Key::Char('L')]);
        assert_eq!(keymap.label("logs.previous"), "p / Alt-p");
        assert_eq!(keymap.keys("sort.name"), [Key::Char('X')]);
        assert!(keymap.is_custom("delete") && !keymap.is_custom("describe"));
        assert_eq!(keymap.table_action(Key::Char('L'), POD), Some(("logs", 0)));
        assert_eq!(keymap.table_action(Key::Char('l'), POD), None);
        assert_eq!(keymap.table_action(Key::Ctrl('d'), POD), None);
        assert_eq!(
            keymap.translate(Scope::Log, Key::Alt('w')),
            Translation::To(Key::Char('w'))
        );
        assert_eq!(
            keymap.translate(Scope::Log, Key::Char('w')),
            Translation::Swallow
        );
        assert_eq!(
            keymap.translate(Scope::Log, Key::Char('j')),
            Translation::Pass
        );
        assert_eq!(
            keymap.translate(Scope::Dashboard, Key::Char('T')),
            Translation::To(Key::Char('t'))
        );
        // Unbinding with an empty list is allowed for remappable actions.
        assert!(parse("keys:\n  quit: []\n")?.keys("quit").is_empty());
        Ok(())
    }

    #[test]
    fn kind_specific_keys_share_a_letter_without_colliding() {
        let keymap = Keymap::default();
        assert_eq!(keymap.table_action(Key::Char('s'), POD), Some(("shell", 0)));
        assert_eq!(
            keymap.table_action(Key::Char('s'), NODE),
            Some(("node.shell", 0))
        );
        assert_eq!(
            keymap.table_action(Key::Char('s'), CRONJOB),
            Some(("suspend", 0))
        );
        assert_eq!(
            keymap.table_action(Key::Char('s'), WORKLOAD),
            Some(("scale", 0))
        );
        assert_eq!(
            keymap.table_action(Key::Char('r'), NODE),
            Some(("drain", 0))
        );
        assert_eq!(
            keymap.table_action(Key::Char('7'), POD),
            Some(("ns.recent", 6))
        );
        // A key owned only by another kind still reaches its handler.
        assert_eq!(
            keymap.table_action(Key::Char('t'), POD),
            Some(("trigger", 0))
        );
    }

    #[test]
    fn invalid_or_unsafe_keymaps_are_rejected() {
        for yaml in [
            // Unknown action, scope and field.
            "keys:\n  launch-missiles: x\n",
            "scopes:\n  nowhere:\n    x: y\n",
            "keys:\n  logs: L\nextra: true\n",
            // Duplicates within a view, including against kept defaults.
            "keys:\n  logs: d\n",
            "keys:\n  logs: x\n  yaml: x\n",
            "keys:\n  log.wrap: e\n",
            "keys:\n  help: j\n",
            "keys:\n  logs: [x, x]\n",
            // Same action twice through keys and scopes, or a YAML duplicate key.
            "keys:\n  log.wrap: Alt-w\nscopes:\n  log:\n    wrap: Alt-q\n",
            "keys:\n  logs: x\n  logs: z\n",
            // Fixed keys and fixed actions.
            "keys:\n  logs: Esc\n",
            "keys:\n  logs: Enter\n",
            "keys:\n  logs: Ctrl-C\n",
            "keys:\n  logs: Ctrl-]\n",
            "keys:\n  drilldown: o\n",
            "keys:\n  review.approve: y\n",
            "scopes:\n  review:\n    cancel: q\n",
            // Plain keys would be typed into the YAML editor.
            "keys:\n  editor.review: r\n",
            // Unparseable, too many, control characters, anchors.
            "keys:\n  logs: Hyper-L\n",
            "keys:\n  ns.recent: [a, b, c, d, e, f, g, h, i, x]\n",
            "keys:\n  logs: \"\\u001b[2J\"\n",
            "keys:\n  logs: &k x\n  yaml: *k\n",
            "keys: [logs]\n",
        ] {
            assert!(parse(yaml).is_err(), "{yaml}");
        }
        assert!(parse(&format!("keys:\n  logs: L\n# {}\n", "x".repeat(MAX_BYTES))).is_err());
    }

    #[test]
    fn documented_example_is_valid() -> Result<()> {
        // Windows checkouts may convert the docs to CRLF.
        let docs = include_str!("../docs/K9S_COMPATIBILITY.md").replace("\r\n", "\n");
        let section = docs
            .split("## Custom keymap")
            .nth(1)
            .ok_or("missing Custom keymap section")?;
        let yaml = section
            .split("```yaml\n")
            .nth(1)
            .and_then(|rest| rest.split("```").next())
            .ok_or("missing example")?;
        let keymap = parse(yaml)?;
        assert_eq!(keymap.keys("logs"), [Key::Char('L')]);
        assert!(keymap.keys("quit").is_empty());
        assert_eq!(keymap.keys("editor.review"), [Key::Ctrl('r')]);
        Ok(())
    }

    #[test]
    fn remapped_keys_cannot_hide_hotkeys() -> Result<()> {
        let keymap = parse("keys:\n  logs: F3\n")?;
        let hotkeys = vec![crate::hotkeys::Hotkey {
            shortcut: crate::hotkeys::Shortcut::F(3),
            description: String::new(),
            command: "pods".into(),
        }];
        assert!(keymap.check_hotkeys(&hotkeys).is_err());
        assert!(Keymap::default().check_hotkeys(&hotkeys).is_ok());
        Ok(())
    }
}
