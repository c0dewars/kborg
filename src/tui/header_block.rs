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
    /// The chrome rows of the frame drawn now. The block shows with the
    /// resource table and its command or filter prompt; every other view
    /// keeps the bar, and the dashboard its own fixed layout.
    pub(super) fn chrome(&self, width: u16, height: u16) -> Chrome {
        let prompt = match &self.modal {
            Modal::None => Some(false),
            Modal::Input("Command", _) | Modal::Filter(..) => Some(true),
            Modal::LiveOverview(..) => return UiConfig::default().chrome(),
            _ => None,
        };
        prompt
            .and_then(|prompt| self.ui.block_chrome(width, height, prompt))
            .unwrap_or_else(|| self.ui.chrome())
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
    let mut lines = vec![
        ("Context", known(&app.context, "text")),
        ("Cluster", known(&app.header.cluster, "text")),
        ("User", known(&app.header.user, "text")),
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
    // Info: a third of the width, between 40 and 52 cells.
    let info_width = (width / 3).clamp(40, 52);
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
    // Recent namespaces with the keys that switch to them.
    if app.ui.header.namespaces {
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
        table_hints(app)
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
