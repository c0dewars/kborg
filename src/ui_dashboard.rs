//! Bounded cluster cards. Health and warning evidence remain independently labelled.
use super::{App, Rect, blend, cell, panel, row, ui_color};
use crate::{
    Result,
    dashboard_health::{EvidenceState, WarningEvidence},
    overview::ContextSummary,
};
use crossterm::{event::KeyCode, style::Color};
use std::{
    io::Write,
    time::{SystemTime, UNIX_EPOCH},
};

/// Live logs in the dashboard: one exact warning Pod, or a cross-cluster
/// workload tail, each followed over read-only connections.
pub(super) struct LogPane {
    /// Exact Pod UID for a warning pane; empty for a workload tail.
    pub uid: String,
    pub title: String,
    pub view: Box<crate::ui_logs::LogView>,
}

/// `t`: choose workloads from every card's cluster for one merged tail.
pub(super) struct TailPicker {
    pub entries: Vec<crate::workload_tail::WorkloadRef>,
    pub errors: Vec<String>,
    pub checked: std::collections::BTreeSet<usize>,
    pub cursor: usize,
    pub loading: bool,
    pub notice: Option<String>,
}

pub(super) enum PickerOutcome {
    Stay,
    Cancel,
    Start(Vec<crate::workload_tail::WorkloadRef>),
}

impl TailPicker {
    pub fn loading() -> Self {
        Self {
            entries: Vec::new(),
            errors: Vec::new(),
            checked: Default::default(),
            cursor: 0,
            loading: true,
            notice: None,
        }
    }
    pub fn key(&mut self, code: KeyCode) -> PickerOutcome {
        self.notice = None;
        match code {
            KeyCode::Esc | KeyCode::Char('q' | 't') => return PickerOutcome::Cancel,
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor = (self.cursor + 1).min(self.entries.len().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.cursor = self.cursor.saturating_sub(1),
            KeyCode::Char(' ') if !self.entries.is_empty() => {
                if !self.checked.remove(&self.cursor) {
                    if self.checked.len() >= crate::workload_tail::MAX_SOURCES {
                        self.notice = Some(format!(
                            "At most {} workloads per tail.",
                            crate::workload_tail::MAX_SOURCES
                        ));
                    } else {
                        self.checked.insert(self.cursor);
                    }
                }
            }
            KeyCode::Enter => {
                let chosen = if self.checked.is_empty() {
                    self.entries.get(self.cursor).cloned().into_iter().collect()
                } else {
                    self.checked
                        .iter()
                        .filter_map(|index| self.entries.get(*index).cloned())
                        .collect::<Vec<_>>()
                };
                if !chosen.is_empty() {
                    return PickerOutcome::Start(chosen);
                }
            }
            _ => {}
        }
        PickerOutcome::Stay
    }
    fn lines(&self, height: usize) -> Vec<(String, &'static str)> {
        let mut lines = vec![(
            format!(
                "Space select (max {})   Enter tail selected   Esc cancel   {} selected",
                crate::workload_tail::MAX_SOURCES,
                self.checked.len()
            ),
            "muted",
        )];
        if self.loading {
            lines.push((
                "Listing Deployments and StatefulSets in every card's cluster...".into(),
                "text",
            ));
        }
        for error in &self.errors {
            lines.push((error.clone(), "warning"));
        }
        if !self.loading && self.entries.is_empty() {
            lines.push((
                "No Deployments or StatefulSets found in the dashboard namespace.".into(),
                "muted",
            ));
        }
        let room = height.saturating_sub(lines.len() + usize::from(self.notice.is_some()));
        let start = self.cursor.saturating_sub(room.saturating_sub(1));
        for (index, entry) in self.entries.iter().enumerate().skip(start).take(room) {
            lines.push((
                format!(
                    "{} [{}] {}",
                    if index == self.cursor { ">" } else { " " },
                    if self.checked.contains(&index) {
                        "x"
                    } else {
                        " "
                    },
                    entry.label()
                ),
                if index == self.cursor {
                    "accent"
                } else {
                    "text"
                },
            ));
        }
        if let Some(notice) = &self.notice {
            lines.push((notice.clone(), "warning"));
        }
        lines
    }
}

pub(super) struct Dashboard {
    pub rows: Vec<ContextSummary>,
    pub selected: usize,
    /// Index into the focused card's recent warnings, chosen with `w`.
    pub warning: Option<usize>,
    pub help: bool,
    pub notice: Option<(String, bool)>,
    pub logs: Option<LogPane>,
    pub picker: Option<TailPicker>,
    /// k10s focus mode: pressing the focused card's number again shows it alone.
    pub focus: bool,
}
impl Dashboard {
    pub fn new(names: &[String], demo: bool) -> Self {
        Self {
            rows: names
                .iter()
                .cloned()
                .map(|name| ContextSummary::connecting(name, demo))
                .collect(),
            selected: 0,
            warning: None,
            help: false,
            notice: None,
            logs: None,
            picker: None,
            focus: false,
        }
    }
    pub fn selected(&self) -> Option<&ContextSummary> {
        self.rows.get(self.selected)
    }
    fn warnings(&self) -> &[WarningEvidence] {
        self.selected()
            .and_then(|row| row.health.as_ref())
            .map_or(&[], |health| health.warnings.recent.as_slice())
    }
    pub fn selected_warning(&self) -> Option<&WarningEvidence> {
        self.warning.and_then(|index| self.warnings().get(index))
    }
    pub fn set_notice(&mut self, text: String, warning: bool) {
        self.notice = Some((text, warning));
    }
    pub fn update(&mut self, rows: Vec<ContextSummary>) {
        let selected = self.selected().map(|row| row.context.clone());
        let warning = self
            .selected_warning()
            .map(|w| (w.namespace.clone(), w.uid.clone(), w.reason.clone()));
        self.rows = rows;
        self.selected = selected
            .and_then(|name| self.rows.iter().position(|row| row.context == name))
            .unwrap_or(self.selected.min(self.rows.len().saturating_sub(1)));
        // Follow the same record across refreshes; a vanished record clears
        // the selection instead of silently moving to a different Pod.
        self.warning = warning.and_then(|(namespace, uid, reason)| {
            self.warnings()
                .iter()
                .position(|w| w.namespace == namespace && w.uid == uid && w.reason == reason)
        });
    }
    pub fn key(&mut self, key: KeyCode) {
        let count = self.rows.len();
        if count == 0 {
            return;
        }
        if key == KeyCode::Char('w') {
            let available = self.warnings().len();
            self.warning = match self.warning {
                _ if available == 0 => None,
                None => Some(0),
                Some(index) if index + 1 < available => Some(index + 1),
                Some(_) => None,
            };
            return;
        }
        let before = self.selected;
        self.card_key(key, count);
        if self.selected != before {
            self.warning = None;
        }
    }
    fn card_key(&mut self, key: KeyCode, count: usize) {
        match key {
            KeyCode::Char('1'..='9') => {
                if let KeyCode::Char(number) = key {
                    let selected = number as usize - '1' as usize;
                    if selected == self.selected && count > 1 {
                        self.focus = !self.focus;
                    } else if selected < count {
                        self.selected = selected;
                    }
                }
            }
            KeyCode::Right | KeyCode::Down | KeyCode::Char('j' | 'l') | KeyCode::Tab => {
                self.selected = (self.selected + 1) % count
            }
            KeyCode::Left | KeyCode::Up | KeyCode::Char('k' | 'h') | KeyCode::BackTab => {
                self.selected = (self.selected + count - 1) % count
            }
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = count - 1,
            _ => {}
        }
    }
}

fn count(value: Option<usize>) -> String {
    value.map_or_else(|| "--".into(), |value| value.to_string())
}

fn state_label(state: &EvidenceState) -> &'static str {
    match state {
        EvidenceState::Connecting => "LOADING",
        EvidenceState::Live => "LIVE",
        EvidenceState::Stale => "STALE",
        EvidenceState::Denied => "DENIED",
        EvidenceState::Unavailable => "UNAVAILABLE",
        EvidenceState::Truncated => "INCOMPLETE",
    }
}

fn health_label(summary: &ContextSummary) -> (&'static str, &'static str) {
    match summary.state {
        EvidenceState::Connecting => ("LOADING / health unknown", "muted"),
        EvidenceState::Stale => ("STALE / health unknown", "warning"),
        EvidenceState::Denied => ("ACCESS DENIED / health unknown", "danger"),
        EvidenceState::Unavailable => ("UNAVAILABLE / health unknown", "warning"),
        EvidenceState::Truncated => ("INCOMPLETE / bounded Pod sample", "warning"),
        EvidenceState::Live => {
            if summary.health.is_none() {
                ("UNKNOWN / awaiting Pod evidence", "warning")
            } else if summary.pods == Some(0) {
                ("NO PODS / selected namespace", "muted")
            } else if summary.unhealthy.is_some_and(|value| value > 0) {
                ("NEEDS ATTENTION", "warning")
            } else if summary
                .health
                .as_ref()
                .is_some_and(|health| health.ready == 0 && health.succeeded > 0)
            {
                ("COMPLETED / no active Pods", "muted")
            } else if summary.unhealthy == Some(0) {
                ("PODS READY", "success")
            } else {
                ("UNKNOWN / awaiting Pod evidence", "warning")
            }
        }
    }
}

fn observation(summary: &ContextSummary, now: u64) -> String {
    let label = if summary.demo {
        "SAMPLE"
    } else {
        state_label(&summary.state)
    };
    let Some(observed) = summary.observed_unix_seconds else {
        return format!("{label} / no Pod observation");
    };
    let age = now.saturating_sub(observed);
    if age < 60 {
        format!("{label} / observed {age}s ago")
    } else {
        format!("{label} / observed {}m ago", age / 60)
    }
}

/// Two columns only when every card fits. Small terminals retain a complete
/// focused card; numeric focus remains stable as the terminal is resized.
fn cards(width: u16, height: u16, count: usize, selected: usize) -> Vec<(usize, Rect)> {
    let mut area = Rect {
        x: 0,
        y: 4,
        width,
        height: height.saturating_sub(7),
    };
    if count == 0 {
        return vec![];
    }
    if count == 1 && width >= 96 {
        // Room for Node/app/restart/event detail under Pod health.
        area.height = area.height.min(18);
    }
    // Three columns once five or more clusters share a wide terminal.
    let columns = if width >= 150 && count >= 5 {
        3
    } else if width >= 76 && count > 1 {
        2
    } else {
        1
    };
    let rows = count.div_ceil(columns);
    if (usize::from(area.height) + 1) / rows < 9 {
        return vec![(selected.min(count - 1), area)];
    }
    let card_width = width.saturating_add(1) / columns as u16 - 1;
    let card_height = (area.height + 1) / rows as u16 - 1;
    (0..count)
        .map(|index| {
            let column = index % columns;
            let row = index / columns;
            (
                index,
                Rect {
                    x: column as u16 * (card_width + 1),
                    y: area.y + row as u16 * (card_height + 1),
                    width: if column == columns - 1 {
                        width - column as u16 * (card_width + 1)
                    } else {
                        card_width
                    },
                    height: if row == rows - 1 {
                        area.height - row as u16 * (card_height + 1)
                    } else {
                        card_height
                    },
                },
            )
        })
        .collect()
}

/// The log pane takes a bit under half the dashboard and never fewer than 6 rows.
fn pane_height(height: u16) -> u16 {
    (height.saturating_sub(7) * 9 / 20)
        .max(6)
        .min(height.saturating_sub(4))
}

fn picker_pane(app: &App, picker: &TailPicker, rect: Rect, out: &mut impl Write) -> Result<()> {
    let color = |role| ui_color(app, role);
    let bg = color("background");
    panel(
        out,
        rect,
        "TAIL WORKLOADS ACROSS CLUSTERS",
        true,
        color("accent"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    let height = usize::from(rect.height.saturating_sub(2));
    for (index, (text, role)) in picker.lines(height).iter().take(height).enumerate() {
        cell(
            out,
            rect.x + 1,
            rect.y + 1 + index as u16,
            rect.width.saturating_sub(2),
            text,
            color(role),
            bg,
        )
        .map_err(|_| "draw failed")?;
    }
    Ok(())
}

fn log_pane(app: &App, pane: &LogPane, rect: Rect, out: &mut impl Write) -> Result<()> {
    let color = |role| ui_color(app, role);
    let bg = color("background");
    panel(out, rect, &pane.title, true, color("accent"), bg).map_err(|_| "draw failed")?;
    let inner = usize::from(rect.width.saturating_sub(2));
    let lines = pane
        .view
        .render(inner, usize::from(rect.height.saturating_sub(2)));
    for (index, text) in lines
        .iter()
        .take(usize::from(rect.height.saturating_sub(2)))
        .enumerate()
    {
        cell(
            out,
            rect.x + 1,
            rect.y + 1 + index as u16,
            rect.width.saturating_sub(2),
            text,
            color("text"),
            bg,
        )
        .map_err(|_| "draw failed")?;
    }
    Ok(())
}

/// Node and app readiness lines: headline Node and app rows first, then
/// named degraded workloads. Unavailable evidence is labelled, never zero.
fn infra_lines(
    infra: &crate::cluster_health::InfraSummary,
    health: Option<&crate::dashboard_health::HealthSummary>,
    demo: bool,
) -> Vec<(String, &'static str)> {
    let label = |state: &EvidenceState| {
        if demo && matches!(state, EvidenceState::Live) {
            "SAMPLE"
        } else {
            state_label(state)
        }
    };
    let nodes = &infra.nodes;
    let node_line = if matches!(nodes.state, EvidenceState::Live) {
        let usage = match (nodes.cpu_percent, nodes.memory_percent) {
            (Some(cpu), Some(memory)) => format!("   CPU {cpu}%   MEM {memory}%"),
            _ if matches!(nodes.metrics, EvidenceState::Live) => "   usage unknown".into(),
            _ => format!("   metrics {}", label(&nodes.metrics)),
        };
        let cordoned = if nodes.cordoned > 0 {
            format!("   {} cordoned", nodes.cordoned)
        } else {
            String::new()
        };
        (
            format!(
                "NODES {}/{} ready{cordoned}{usage}",
                nodes.ready, nodes.total
            ),
            if nodes.ready < nodes.total {
                "warning"
            } else {
                "text"
            },
        )
    } else {
        (format!("NODES {}", label(&nodes.state)), "muted")
    };
    let apps = &infra.workloads;
    let mut lines = vec![node_line];
    let mut details = Vec::new();
    if matches!(apps.state, EvidenceState::Live) {
        lines.push((
            format!(
                "APPS {} deploy   {} sts   {} degraded",
                apps.deployments, apps.statefulsets, apps.degraded
            ),
            if apps.degraded > 0 { "warning" } else { "text" },
        ));
        details.extend(
            apps.named
                .iter()
                .map(|name| (format!("  {name}"), "warning")),
        );
        if apps.degraded > apps.named.len() {
            details.push((
                format!("  +{} more degraded", apps.degraded - apps.named.len()),
                "muted",
            ));
        }
    } else {
        lines.push((format!("APPS {}", label(&apps.state)), "muted"));
    }
    if let Some(health) = health.filter(|health| health.restart_alert_count > 0) {
        lines.push((
            format!(
                "RESTARTS {} Pod{} at >={} restarts",
                health.restart_alert_count,
                if health.restart_alert_count == 1 {
                    ""
                } else {
                    "s"
                },
                crate::dashboard_health::RESTART_ALERT
            ),
            "warning",
        ));
        details.extend(
            health
                .restart_alerts
                .iter()
                .map(|(pod, count)| (format!("  {pod} ({count})"), "warning")),
        );
    }
    let events = &infra.events;
    match events.state {
        EvidenceState::Live if events.count > 0 => {
            lines.push((
                format!(
                    "EVENTS {} non-Pod warning{} / 15m",
                    events.count,
                    if events.count == 1 { "" } else { "s" }
                ),
                "warning",
            ));
            details.extend(
                events
                    .recent
                    .iter()
                    .map(|text| (format!("  {text}"), "warning")),
            );
        }
        EvidenceState::Live | EvidenceState::Connecting => {}
        ref state => lines.push((format!("EVENTS {}", label(state)), "muted")),
    }
    lines.extend(details);
    lines
}

fn card(
    app: &App,
    dashboard: &Dashboard,
    index: usize,
    rect: Rect,
    out: &mut impl Write,
    now: u64,
) -> Result<()> {
    let summary = &dashboard.rows[index];
    let selected = index == dashboard.selected;
    let warning = dashboard.warning;
    let color = |role| ui_color(app, role);
    let bg = if app.options.color && !app.theme.terminal {
        blend(
            app.theme.get("background"),
            app.theme.get("text"),
            if selected { 5 } else { 2 },
        )
        .color()
    } else {
        Color::Reset
    };
    let border = if selected {
        color("accent")
    } else {
        color("muted")
    };
    panel(
        out,
        rect,
        &format!("{}  {}", index + 1, summary.context),
        selected,
        border,
        bg,
    )
    .map_err(|_| "draw failed")?;
    if rect.height < 4 || rect.width < 4 {
        return Ok(());
    }
    let split = rect.width >= 96 && rect.height >= 12;
    let width = if split {
        (rect.width - 7) / 2
    } else {
        rect.width - 4
    };
    let x = rect.x + 2;
    let mut line = |offset: u16, warnings: bool, text: &str, role: &'static str| -> Result<()> {
        if offset < rect.height - 1 {
            let x = if split && warnings { x + width + 3 } else { x };
            cell(out, x, rect.y + offset, width, text, color(role), bg)
                .map_err(|_| "draw failed")?;
        }
        Ok(())
    };
    let (health, role) = health_label(summary);
    line(1, false, health, role)?;
    let ready = summary.health.as_ref().map(|health| health.ready);
    line(
        2,
        false,
        &format!(
            "{} {}   {} ready   {} attention",
            count(summary.pods),
            if summary.pods == Some(1) {
                "Pod"
            } else {
                "Pods"
            },
            count(ready),
            count(summary.unhealthy)
        ),
        "text",
    )?;
    let expanded = rect.height >= 12;
    let infra = summary.infra.as_ref().filter(|_| expanded);
    let warning_row = if split {
        1
    } else if infra.is_some() && rect.height >= 14 {
        8
    } else if expanded {
        6
    } else {
        4
    };
    if let Some(infra) = infra {
        let rows = infra_lines(infra, summary.health.as_ref(), summary.demo);
        let (start, limit) = if split {
            (7, rect.height.saturating_sub(3))
        } else if warning_row == 8 {
            (6, 8)
        } else {
            (0, 0)
        };
        for (offset, (text, role)) in rows.iter().enumerate() {
            let at = start + offset as u16;
            if at >= limit {
                break;
            }
            line(at, false, text, role)?;
        }
    }
    if let Some(health) = &summary.health {
        line(
            3,
            false,
            &format!(
                "{} restarts   {} OOM evidence",
                health.restarts, health.oom_killed
            ),
            if health.oom_killed > 0 {
                "warning"
            } else {
                "muted"
            },
        )?;
        if expanded {
            line(
                4,
                false,
                &format!(
                    "{} pending   {} failed   {} completed",
                    health.pending, health.failed, health.succeeded
                ),
                "muted",
            )?;
            if split {
                line(
                    5,
                    false,
                    &format!(
                        "{} unready   {} terminating   {} unknown",
                        health.unready, health.terminating, health.unknown
                    ),
                    "muted",
                )?;
            }
        }
        line(warning_row, true, "WARNINGS / LAST 15m", "accent")?;
        let warnings = &health.warnings;
        let evidence = if summary.demo && matches!(warnings.state, EvidenceState::Live) {
            "SAMPLE"
        } else {
            state_label(&warnings.state)
        };
        let warning_status = match warnings.count {
            Some(0) if matches!(warnings.state, EvidenceState::Live) => {
                "No recent Pod warnings".into()
            }
            Some(value) => format!(
                "{value} warning {} / {evidence}",
                if value == 1 { "record" } else { "records" }
            ),
            None => format!("{evidence} / warning count unknown"),
        };
        line(
            warning_row + 1,
            true,
            &warning_status,
            if warnings.count == Some(0) && matches!(warnings.state, EvidenceState::Live) {
                "muted"
            } else {
                "warning"
            },
        )?;
        let available = rect.height.saturating_sub(warning_row + 4) as usize;
        if let Some(message) = &warnings.message {
            if available > 0 {
                line(warning_row + 2, true, message, "muted")?;
            }
        } else {
            for (offset, evidence) in warnings.recent.iter().take(available).enumerate() {
                let pod = if summary.namespace == "*" {
                    format!("{}/{}", evidence.namespace, evidence.pod)
                } else {
                    evidence.pod.clone()
                };
                let chosen = selected && warning == Some(offset);
                line(
                    warning_row + 2 + offset as u16,
                    true,
                    &format!(
                        "{}{} / {}",
                        if chosen { "> " } else { "" },
                        evidence.reason,
                        pod
                    ),
                    if chosen { "accent" } else { "warning" },
                )?;
            }
        }
    } else {
        if let Some(error) = &summary.error {
            line(3, false, error, "muted")?;
        }
        line(warning_row, true, "WARNINGS / LAST 15m", "accent")?;
        line(
            warning_row + 1,
            true,
            "Unknown / Pod evidence unavailable",
            "muted",
        )?;
    }
    if rect.height >= 8 {
        line(rect.height - 2, false, &observation(summary, now), "muted")?;
    }
    Ok(())
}

pub(super) fn render(
    app: &App,
    dashboard: &Dashboard,
    width: u16,
    height: u16,
    out: &mut impl Write,
) -> Result<()> {
    let color = |role| ui_color(app, role);
    let bg = color("background");
    for y in 1..height {
        row(out, y, width, "", color("text"), bg).map_err(|_| "draw failed")?;
    }
    let mode = if app.options.connection.demo {
        "DEMO SAMPLE DATA"
    } else {
        "READ ONLY"
    };
    let namespace = if app.namespace == "*" {
        "all namespaces"
    } else {
        app.namespace.as_str()
    };
    let mode_width = (mode.len() as u16 + 2).min(width);
    cell(
        out,
        1,
        1,
        width.saturating_sub(mode_width + 1),
        &format!(
            "{} {}  /  {namespace}",
            dashboard.rows.len(),
            if dashboard.rows.len() == 1 {
                "context"
            } else {
                "contexts"
            }
        ),
        color("muted"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    cell(
        out,
        width - mode_width,
        1,
        mode_width,
        mode,
        color("muted"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    row(
        out,
        2,
        width,
        if width < 42 {
            " CONTEXT OVERVIEW"
        } else if width < 64 {
            " CONTEXT OVERVIEW / Pod health"
        } else {
            " CONTEXT OVERVIEW / Pod health and recent warnings"
        },
        color("text"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    if dashboard.help {
        let rect = Rect {
            x: 0,
            y: 4,
            width,
            height: height.saturating_sub(7),
        };
        panel(out, rect, "DASHBOARD KEYS", true, color("accent"), bg).map_err(|_| "draw failed")?;
        let help = [
            "1..9             Focus a card; its number again toggles full screen",
            "Arrows / Tab     Move between cards",
            "Enter            Open focused context Pods",
            "w                Select next recent warning",
            "Enter on warning Open that Pod's Events (exact UID)",
            "L on warning     Follow that Pod's logs in a pane (exact UID)",
            "t                Tail chosen workloads across every card's cluster",
            "e / W in a pane  Show only error / warning lines",
            "Esc on warning   Close the log pane, then clear the selection",
            "c                Choose up to nine context names",
            "s                Save these contexts and namespace",
            "B                Open saved selections",
            "? / Esc          Close this keymap",
            "",
            "Pod watches and recent warning Events are independent.",
            "Unknown, denied, stale and incomplete evidence stays explicit.",
            "OOM evidence is current or previous container termination.",
            "Restarts are current container counters, not a rate.",
            "Dashboard reads never enable write capabilities.",
        ];
        for (index, text) in help
            .iter()
            .take(rect.height.saturating_sub(2) as usize)
            .enumerate()
        {
            cell(
                out,
                2,
                rect.y + 1 + index as u16,
                width.saturating_sub(4),
                text,
                color("text"),
                bg,
            )
            .map_err(|_| "draw failed")?;
        }
    } else {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |time| time.as_secs());
        let pane = (dashboard.logs.is_some() || dashboard.picker.is_some())
            .then(|| ((), pane_height(height)));
        let card_height = height.saturating_sub(pane.map_or(0, |(_, h)| h + 1));
        let mut cards_bottom = 4;
        if card_height >= 12 || pane.is_none() {
            let layout = if dashboard.focus {
                vec![(
                    dashboard.selected,
                    Rect {
                        x: 0,
                        y: 4,
                        width,
                        height: card_height.saturating_sub(7),
                    },
                )]
            } else {
                cards(width, card_height, dashboard.rows.len(), dashboard.selected)
            };
            for (index, rect) in layout {
                cards_bottom = cards_bottom.max(rect.y + rect.height);
                card(app, dashboard, index, rect, out, now)?;
            }
        }
        // The pane starts directly under the cards and fills down to the
        // status rows, so a short single card leaves no blank band.
        if pane.is_some() {
            let pane_y = cards_bottom.min(height.saturating_sub(9));
            let rect = Rect {
                x: 0,
                y: pane_y,
                width,
                height: height.saturating_sub(3).saturating_sub(pane_y),
            };
            if let Some(picker) = &dashboard.picker {
                picker_pane(app, picker, rect, out)?;
            } else if let Some(pane) = &dashboard.logs {
                log_pane(app, pane, rect, out)?;
            }
        }
    }
    if let Some((notice, warning)) = &dashboard.notice {
        row(
            out,
            height - 3,
            width,
            &format!(" {notice}"),
            color(if *warning { "warning" } else { "accent" }),
            bg,
        )
        .map_err(|_| "draw failed")?;
    }
    let selected = dashboard
        .selected()
        .map(|row| row.context.as_str())
        .unwrap_or("no context");
    let warning = dashboard
        .selected_warning()
        .map(|w| format!("   warning {} / {}", w.reason, w.pod))
        .unwrap_or_default();
    row(
        out,
        height - 2,
        width,
        &format!(
            " {} of {}  {}{warning}",
            dashboard.selected + 1,
            dashboard.rows.len(),
            selected
        ),
        color("muted"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    row(
        out,
        height - 1,
        width,
        if dashboard.help {
            " ? keymap   Esc back"
        } else if dashboard.picker.is_some() {
            " Space select   Enter tail   Esc cancel"
        } else if dashboard
            .logs
            .as_ref()
            .is_some_and(|pane| pane.uid.is_empty())
        {
            " t close tail   e errors   W warnings   Esc close tail"
        } else if dashboard.logs.is_some() {
            " L close logs   w next warning   Enter Events   Esc close logs"
        } else if dashboard.warning.is_some() {
            " w next warning   Enter Events   L live logs   Esc clear"
        } else {
            " 1..9 focus   Enter Pods   w warnings   t tail workloads   ? keymap   Esc back"
        },
        color("accent"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    out.flush().map_err(|_| "draw failed".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn number_again_toggles_focus_and_cards_show_node_and_app_readiness() {
        let names = vec!["north".into(), "south".into()];
        let mut dashboard = Dashboard::new(&names, false);
        dashboard.key(KeyCode::Char('2'));
        assert!(!dashboard.focus);
        dashboard.key(KeyCode::Char('2'));
        assert!(dashboard.focus);
        dashboard.key(KeyCode::Char('2'));
        assert!(!dashboard.focus);
        let mut infra = crate::cluster_health::InfraSummary::connecting();
        infra.nodes.state = EvidenceState::Live;
        infra.nodes.ready = 2;
        infra.nodes.total = 3;
        infra.nodes.metrics = EvidenceState::Denied;
        infra.workloads.state = EvidenceState::Denied;
        let lines = infra_lines(&infra, None, false);
        assert_eq!(lines[0].0, "NODES 2/3 ready   metrics DENIED");
        assert_eq!(lines[0].1, "warning");
        assert_eq!(lines[1].0, "APPS DENIED");
        infra.workloads = crate::cluster_health::WorkloadEvidence {
            state: EvidenceState::Live,
            deployments: 9,
            statefulsets: 1,
            degraded: 6,
            named: vec![
                "deploy/a".into(),
                "deploy/b".into(),
                "deploy/c".into(),
                "sts/d".into(),
            ],
        };
        let lines = infra_lines(&infra, None, false);
        assert_eq!(lines[1].0, "APPS 9 deploy   1 sts   6 degraded");
        assert_eq!(
            lines.last().map(|line| line.0.as_str()),
            Some("  +2 more degraded")
        );
    }
    #[test]
    fn focus_survives_updates_and_never_selects_missing_cards() {
        let names = vec!["north".into(), "south".into()];
        let mut dashboard = Dashboard::new(&names, false);
        dashboard.key(KeyCode::Char('2'));
        dashboard.key(KeyCode::Char('4'));
        assert_eq!(
            dashboard.selected().map(|row| row.context.as_str()),
            Some("south")
        );
        dashboard.update(
            names
                .into_iter()
                .rev()
                .map(|name| ContextSummary::connecting(name, false))
                .collect(),
        );
        assert_eq!(
            dashboard.selected().map(|row| row.context.as_str()),
            Some("south")
        );
        dashboard.key(KeyCode::Left);
        assert_eq!(
            dashboard.selected().map(|row| row.context.as_str()),
            Some("north")
        );
    }
    fn with_warnings(context: &str, uids: &[&str]) -> ContextSummary {
        let mut row = ContextSummary::connecting(context.into(), false);
        row.state = EvidenceState::Live;
        row.health = Some(crate::dashboard_health::HealthSummary {
            ready: 0,
            unready: 0,
            pending: 0,
            failed: 0,
            succeeded: 0,
            terminating: 0,
            unknown: 0,
            restarts: 0,
            oom_killed: 0,
            restart_alerts: Vec::new(),
            restart_alert_count: 0,
            warnings: crate::dashboard_health::WarningSummary {
                state: EvidenceState::Live,
                count: Some(uids.len()),
                recent: uids
                    .iter()
                    .map(|uid| WarningEvidence {
                        namespace: "default".into(),
                        pod: "api".into(),
                        uid: (*uid).into(),
                        reason: "BackOff".into(),
                        occurrences: 1,
                        observed_unix_seconds: 1,
                    })
                    .collect(),
                message: None,
            },
        });
        row
    }
    #[test]
    fn warning_selection_follows_exact_uid_and_never_moves_to_a_replacement() {
        let mut dashboard = Dashboard::new(&["north".into(), "south".into()], false);
        dashboard.update(vec![
            with_warnings("north", &["a", "b"]),
            with_warnings("south", &[]),
        ]);
        dashboard.key(KeyCode::Char('w'));
        dashboard.key(KeyCode::Char('w'));
        assert_eq!(
            dashboard.selected_warning().map(|w| w.uid.as_str()),
            Some("b")
        );
        // Reordered refresh keeps the same record.
        dashboard.update(vec![
            with_warnings("north", &["b", "a"]),
            with_warnings("south", &[]),
        ]);
        assert_eq!(dashboard.warning, Some(0));
        // A same-name replacement Pod has a new UID; selection clears.
        dashboard.update(vec![
            with_warnings("north", &["c", "a"]),
            with_warnings("south", &[]),
        ]);
        assert!(dashboard.selected_warning().is_none());
        dashboard.key(KeyCode::Char('w'));
        dashboard.key(KeyCode::Char('w'));
        dashboard.key(KeyCode::Char('w'));
        assert!(dashboard.warning.is_none(), "cycling wraps to no selection");
        dashboard.key(KeyCode::Char('w'));
        dashboard.key(KeyCode::Char('2'));
        assert!(
            dashboard.warning.is_none(),
            "changing card clears selection"
        );
        dashboard.key(KeyCode::Char('w'));
        assert!(
            dashboard.warning.is_none(),
            "card without warnings selects nothing"
        );
    }
    #[test]
    fn grid_is_bounded_at_all_supported_terminal_sizes() {
        for width in 28..180 {
            for height in 10..55 {
                for count in 1..=crate::overview::MAX_LIVE_CONTEXTS {
                    let layout = cards(width, height, count, count - 1);
                    assert!(!layout.is_empty());
                    for (index, rect) in &layout {
                        assert!(*index < count);
                        assert!(rect.x + rect.width <= width);
                        assert!(rect.y + rect.height <= height - 3);
                        assert!(rect.width >= 2 && rect.height >= 3);
                    }
                    for pair in layout.windows(2) {
                        let (a, b) = (pair[0].1, pair[1].1);
                        assert!(a.x + a.width <= b.x || a.y + a.height <= b.y);
                    }
                }
            }
        }
    }
    #[test]
    fn missing_or_unavailable_evidence_never_claims_ready() {
        let mut row = ContextSummary::connecting("local".into(), false);
        for state in [
            EvidenceState::Connecting,
            EvidenceState::Stale,
            EvidenceState::Denied,
            EvidenceState::Unavailable,
            EvidenceState::Truncated,
            EvidenceState::Live,
        ] {
            row.state = state;
            row.pods = Some(0);
            row.unhealthy = Some(0);
            assert_ne!(health_label(&row).1, "success");
        }
    }
    #[test]
    fn completed_only_scope_is_not_reported_as_ready_running_pods() {
        let mut row = ContextSummary::connecting("local".into(), false);
        row.state = EvidenceState::Live;
        row.pods = Some(1);
        row.unhealthy = Some(0);
        row.health = Some(crate::dashboard_health::HealthSummary {
            ready: 0,
            unready: 0,
            pending: 0,
            failed: 0,
            succeeded: 1,
            terminating: 0,
            unknown: 0,
            restarts: 0,
            oom_killed: 0,
            restart_alerts: Vec::new(),
            restart_alert_count: 0,
            warnings: crate::dashboard_health::WarningSummary::connecting(),
        });
        assert_eq!(health_label(&row), ("COMPLETED / no active Pods", "muted"));
        let mut dashboard = Dashboard::new(&["local".into()], false);
        dashboard.set_notice("Selection saved locally.".into(), false);
        dashboard.update(vec![row]);
        assert_eq!(
            dashboard.notice.as_ref().map(|(text, _)| text.as_str()),
            Some("Selection saved locally.")
        );
    }
}
