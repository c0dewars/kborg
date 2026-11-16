//! The k9s-style header block of the resource table: context, cluster, user,
//! namespace and Kubernetes version with cluster CPU/MEM, one per line; the
//! recent namespaces with their number keys; the key hints in columns; and
//! the KBORG logo where the terminal has room to spare. Detail views, reviews
//! and the dashboard keep the compact three-row bar, so their content keeps
//! the height it had.
use super::*;
use crate::ui_config::{Chrome, UiConfig};

/// figlet "small", ASCII only so it draws in every locale.
const LOGO: [&str; 4] = [
    r" _  _____  ___  ___  ___ ",
    r"| |/ / _ )/ _ \| _ \/ __|",
    r"| ' <| _ \ (_) |   / (_ |",
    r"|_|\_\___/\___/|_|_\\___|",
];
const LOGO_WIDTH: u16 = 25;
/// Cells between the block's columns.
const GAP: u16 = 3;
const LABEL_WIDTH: u16 = 11;
/// Rows above a view's panel when the three-row bar is shown.
pub(super) const BAR_ROWS: u16 = 3;
/// Rows a view keeps around its content: panel borders, the status line, the
/// hint row and the approval row.
pub(super) const VIEW_FRAME_ROWS: u16 = 5;
/// Widest recent-namespace name shown before it is cut with `~`.
const NAMESPACE_WIDTH: usize = 18;

#[derive(Default)]
pub(super) struct State {
    pub(super) cluster: String,
    pub(super) user: String,
    pub(super) version: Option<String>,
    /// Cluster (CPU %, memory %) of allocatable; `None` until first read.
    pub(super) usage: Option<(Option<u64>, Option<u64>)>,
    last_poll: Option<Instant>,
}
impl State {
    pub(super) fn apply(&mut self, info: HeaderInfo) {
        self.cluster = info.cluster;
        self.user = info.user;
        if info.version.is_some() {
            self.version = info.version;
        }
        if info.usage.is_some() {
            self.usage = info.usage;
        }
    }
    /// A new connection: forget the previous cluster's values.
    pub(super) fn reset(&mut self) {
        *self = Self::default();
    }
    /// Usage refreshes every 15 seconds; a cluster that cannot report it
    /// (no metrics-server, Nodes denied) is asked again only once a minute.
    fn interval(&self) -> Duration {
        Duration::from_secs(if self.usage == Some((None, None)) {
            60
        } else {
            15
        })
    }
}

impl App {
    /// The chrome rows of the frame drawn now. The block stays on every
    /// view, so the app keeps one header; only the dashboard (its own fixed
    /// layout) and full-screen logs give its rows to their content.
    pub(super) fn chrome(&self, width: u16, height: u16) -> Chrome {
        let open = match &self.modal {
            Modal::None => false,
            Modal::LiveOverview(..) => return UiConfig::default().chrome(),
            Modal::Follow(view) if view.fullscreen => return self.ui.chrome(),
            _ => true,
        };
        self.ui
            .block_chrome(width, height, open)
            .unwrap_or_else(|| self.ui.chrome())
    }
    /// The first row of a view's panel: below the header block when the
    /// terminal has room for it, else below the three-row bar.
    pub(super) fn view_top(&self, width: u16, height: u16) -> u16 {
        self.ui
            .block_chrome(width, height, true)
            .map_or(BAR_ROWS, |chrome| chrome.top)
    }
    /// `view_top` for the terminal as it is now (key handlers).
    pub(super) fn view_top_now(&self) -> u16 {
        let (width, height) = terminal::size().unwrap_or((80, 24));
        self.view_top(width, height)
    }
    pub(super) fn layout(&self, width: u16, height: u16) -> Layout {
        Layout::for_chrome(width, height, &self.chrome(width, height), &self.ui)
    }
    /// Ask the worker for the block's data when it is on screen and due.
    /// True when a request was sent.
    pub(super) fn poll_header_info(&mut self) -> bool {
        let (width, height) = terminal::size().unwrap_or((0, 0));
        let usage = self.ui.header.usage;
        if !self.connected
            || self.busy
            || self.stale
            || !matches!(self.modal, Modal::None)
            || self.ui.block_chrome(width, height, false).is_none()
            || (!usage && !self.header.cluster.is_empty())
            || self
                .header
                .last_poll
                .is_some_and(|last| last.elapsed() < self.header.interval())
        {
            return false;
        }
        self.header.last_poll = Some(Instant::now());
        self.send(Task::HeaderInfo(
            usage && self.header.version.is_none(),
            usage,
        ));
        true
    }
}

/// `75%` with the role its level deserves, or `n/a` when unknown.
fn percent(value: Option<u64>) -> (String, &'static str) {
    match value {
        Some(value) if value >= 90 => (format!("{value}%"), "danger"),
        Some(value) if value >= 80 => (format!("{value}%"), "warning"),
        Some(value) => (format!("{value}%"), "text"),
        None => ("n/a".into(), "muted"),
    }
}

/// The namespace as the block and the table title name it.
pub(super) fn namespace_label(namespace: &str) -> &str {
    if namespace == "*" { "all" } else { namespace }
}

/// The five info lines as (label, value spans in theme roles).
fn info_lines(app: &App, value_width: usize) -> Vec<(&'static str, Vec<(String, &'static str)>)> {
    let known = |value: &str, role: &'static str| {
        if value.is_empty() {
            vec![("-".to_owned(), "muted")]
        } else {
            vec![(display(value, value_width), role)]
        }
    };
    // Context, cluster and user names keep their ending when cut: in an EKS
    // ARN or a GKE name the cluster's own name is the last part.
    let identity = |value: &str| {
        if value.is_empty() {
            vec![("-".to_owned(), "muted")]
        } else {
            vec![(crate::text::shown_middle(value, value_width), "text")]
        }
    };
    let mut lines = vec![
        ("Context", identity(&app.context)),
        ("Cluster", identity(&app.header.cluster)),
        ("User", identity(&app.header.user)),
        (
            "Namespace",
            known(namespace_label(&app.namespace), "accent"),
        ),
    ];
    if app.ui.header.usage {
        let mut spans = known(app.header.version.as_deref().unwrap_or(""), "text");
        if let Some((cpu, memory)) = app.header.usage {
            for (label, value) in [("CPU", cpu), ("MEM", memory)] {
                let (text, role) = percent(value);
                spans.push((format!("   {label} "), "muted"));
                spans.push((text, role));
            }
        }
        lines.push(("K8s", spans));
    }
    lines
}

/// Recent namespaces as (key, name, current): all namespaces first, then the
/// most recent ones, as many as the block has rows.
fn namespace_entries(app: &App) -> Vec<(String, String, bool)> {
    let mut entries = Vec::new();
    if let Some(key) = app.keymap.primary("ns.all") {
        entries.push((key, "all".to_owned(), app.namespace == "*"));
    }
    let keys = app.keymap.keys("ns.recent");
    for (namespace, key) in app.recent.namespaces(&app.context).iter().zip(keys) {
        entries.push((
            key.to_string(),
            display(namespace, NAMESPACE_WIDTH),
            *namespace == app.namespace,
        ));
    }
    entries.truncate(usize::from(UiConfig::BLOCK_ROWS));
    entries
}

/// True while the resource table is in front (alone or under its prompt).
fn on_table(app: &App) -> bool {
    matches!(
        app.modal,
        Modal::None | Modal::Input("Command", _) | Modal::Filter(..)
    )
}

/// What a view other than the table says about itself in the block's middle
/// column: its name, and for logs and object overviews what it shows.
fn view_lines(app: &App) -> Vec<(&'static str, String)> {
    let mut lines = vec![("View", screen_title(app, &app.modal))];
    match &app.modal {
        Modal::Follow(_) => {
            if let Some(target) = &app.log_target {
                lines.push(("Pod", format!("{}/{}", target.namespace, target.pod)));
                lines.push((
                    "Container",
                    if target.all {
                        "all".into()
                    } else {
                        target.container.clone().unwrap_or_else(|| "default".into())
                    },
                ));
                lines.push((
                    "Logs",
                    if target.previous {
                        "previous container"
                    } else if target.follow {
                        "following"
                    } else {
                        "snapshot"
                    }
                    .into(),
                ));
            }
        }
        Modal::Overview(object, _) => {
            for (label, value) in [
                ("Kind", &object["kind"]),
                ("Name", &object["metadata"]["name"]),
                ("Namespace", &object["metadata"]["namespace"]),
            ] {
                if let Some(value) = value.as_str() {
                    lines.push((label, value.to_owned()));
                }
            }
        }
        _ => {}
    }
    lines
}

/// The key hints of the view in front: the table's actions, or the keys of
/// the open view from the keymap, so the header always lists what works here.
fn view_hints(app: &App) -> Vec<(String, String)> {
    if on_table(app) {
        return table_hints(app)
            .into_iter()
            .map(|(key, label)| (key, label.to_owned()))
            .collect();
    }
    let mut hints = Vec::new();
    let mut push = |key: Option<String>, label: &str| {
        if let Some(key) = key {
            hints.push((key, label.to_owned()));
        }
    };
    if doc_search::document_text(app, &app.modal).is_some() {
        push(Some("/".into()), "search");
    }
    if let Some(scope) = keyhelp::translated_scope(&app.modal) {
        // Moving around needs no hint; everything else the view can do does.
        const MOVES: [&str; 8] = [
            "down",
            "up",
            "page-down",
            "page-up",
            "top",
            "bottom",
            "left",
            "right",
        ];
        for (action, keys, _) in app.keymap.entries(scope) {
            let name = action
                .name
                .strip_prefix(scope.prefix())
                .unwrap_or(action.name);
            if MOVES.contains(&name) || keys.is_empty() {
                continue;
            }
            push(
                app.keymap.primary(action.name),
                &name.replace(['.', '-'], " "),
            );
        }
        if keyhelp::help_scope(&app.modal).is_some() {
            push(app.keymap.primary("help"), "all keys");
        }
    }
    if palette::command_prompt_allowed(&app.modal) {
        push(app.keymap.primary("command"), "command");
    }
    push(Some("Esc".into()), "back");
    hints
}

/// Hints whose action removes or evicts something: drawn in the danger color.
pub(super) fn risky(label: &str) -> bool {
    ["delete", "kill", "drain", "uninstall"]
        .iter()
        .any(|word| label.starts_with(word))
}

/// Draws the block's five rows from `y`. `color` maps a theme role.
pub(super) fn render(
    app: &App,
    out: &mut impl Write,
    width: u16,
    y: u16,
    color: &dyn Fn(&str) -> Color,
) -> io::Result<()> {
    let bg = color("background");
    for line in 0..UiConfig::BLOCK_ROWS {
        row(out, y + line, width, "", color("text"), bg)?;
    }
    // Info: a third of the width, between 40 and 52 cells, or wider for long
    // context, cluster or user names (up to half the width), so the whole
    // name shows when there is room; the key columns move right.
    let longest = [&app.context, &app.header.cluster, &app.header.user]
        .iter()
        .map(|value| text_cells(value))
        .max()
        .unwrap_or(0);
    let info_width = (width / 3).clamp(40, 52).max(
        LABEL_WIDTH
            .saturating_add(longest)
            .saturating_add(1)
            .min(width / 2),
    );
    let value_width = usize::from(info_width - LABEL_WIDTH);
    for (line, (label, values)) in info_lines(app, value_width).into_iter().enumerate() {
        let y = y + line as u16;
        cell(out, 1, y, LABEL_WIDTH, label, color("muted"), bg)?;
        let items: Vec<_> = values
            .into_iter()
            .map(|(text, role)| (text, color(role), bg))
            .collect();
        spans(out, 1 + LABEL_WIDTH, y, 1 + info_width, &items)?;
    }
    let key = color(&app.ui.hints.key_role);
    let label = color(&app.ui.hints.text_role);
    let mut x = 1 + info_width + GAP;
    // Other views name themselves and what they show.
    if !on_table(app) {
        let lines = view_lines(app);
        let label_width = lines
            .iter()
            .map(|(label, _)| text_cells(label))
            .max()
            .unwrap_or(0)
            + 2;
        let value_width = lines
            .iter()
            .map(|(_, value)| text_cells(value))
            .max()
            .unwrap_or(0)
            .min(34);
        if x + label_width + value_width < width {
            for (line, (name, value)) in lines.iter().enumerate() {
                let y = y + line as u16;
                cell(out, x, y, label_width, name, color("muted"), bg)?;
                cell(
                    out,
                    x + label_width,
                    y,
                    value_width,
                    value,
                    if line == 0 {
                        color("accent")
                    } else {
                        color("text")
                    },
                    bg,
                )?;
            }
            x += label_width + value_width + GAP;
        }
    }
    // Recent namespaces with the keys that switch to them (table only: the
    // number keys mean something else in other views).
    if app.ui.header.namespaces && on_table(app) {
        let entries = namespace_entries(app);
        let key_width = entries
            .iter()
            .map(|(key, ..)| text_cells(key) + 2)
            .max()
            .unwrap_or(0);
        let name_width = entries
            .iter()
            .map(|(_, name, _)| text_cells(name))
            .max()
            .unwrap_or(0);
        if !entries.is_empty() && x + key_width + 1 + name_width < width {
            for (line, (shortcut, name, current)) in entries.iter().enumerate() {
                let y = y + line as u16;
                app.add_key_click(x, y, key_width + 1 + text_cells(name), shortcut);
                cell(out, x, y, key_width, &format!("<{shortcut}>"), key, bg)?;
                cell(
                    out,
                    x + key_width + 1,
                    y,
                    name_width,
                    name,
                    if *current { color("accent") } else { label },
                    bg,
                )?;
            }
            x += key_width + 1 + name_width + GAP;
        }
    }
    // Key hints, five to a column, as many columns as fit. `?` lists the rest.
    let hints = if app.ui.hints.show {
        view_hints(app)
    } else {
        Vec::new()
    };
    for column in hints.chunks(usize::from(UiConfig::BLOCK_ROWS)) {
        let key_width = column
            .iter()
            .map(|(key, _)| text_cells(key) + 2)
            .max()
            .unwrap_or(0);
        let label_width = column
            .iter()
            .map(|(_, label)| text_cells(label))
            .max()
            .unwrap_or(0);
        // Keep complete hints together; a clipped shortcut is misleading.
        if x + key_width + 1 + label_width >= width {
            break;
        }
        for (line, (shortcut, text)) in column.iter().enumerate() {
            let y = y + line as u16;
            // Every hint is a button; the ones that destroy something are red.
            app.add_key_click(x, y, key_width + 1 + text_cells(text), shortcut);
            let (key, label) = if risky(text) {
                (color("danger"), color("danger"))
            } else {
                (key, label)
            };
            cell(out, x, y, key_width, &format!("<{shortcut}>"), key, bg)?;
            cell(out, x + key_width + 1, y, label_width, text, label, bg)?;
        }
        x += key_width + 1 + label_width + GAP;
    }
    // The logo only takes room nothing else wants.
    if app.ui.header.logo && x + LOGO_WIDTH + 1 < width {
        let logo_x = width - LOGO_WIDTH - 2;
        for (line, text) in LOGO.iter().enumerate() {
            cell(
                out,
                logo_x,
                y + line as u16,
                LOGO_WIDTH,
                text,
                color("accent"),
                bg,
            )?;
        }
    }
    Ok(())
}
