//! Drawing: rows, panels, hints, modals and the main frame.
use super::*;

pub(super) fn row(
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
pub(super) fn cell(
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
pub(super) fn panel(
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
pub(super) fn table_hints(app: &App) -> Vec<(&'static str, &'static str)> {
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
pub(super) fn key_hints(
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
pub(super) fn ui_color(app: &App, role: &str) -> Color {
    if app.options.color && !app.theme.terminal {
        app.theme.get(role).color()
    } else {
        Color::Reset
    }
}
pub(super) fn render_advice_footer(
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
pub(super) fn render_helm_review_footer(
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
pub(super) fn render_write_footer(
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
pub(super) fn draw(app: &mut App) -> Result<()> {
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
pub(super) fn modal_rect(modal: &Modal, width: u16, height: u16) -> Rect {
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
pub(super) fn input_lines(label: &str, text: &str, width: usize, height: usize) -> Vec<String> {
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
pub(super) fn render_log_modal(
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
pub(super) fn render(app: &App, w: u16, h: u16, out: &mut impl Write) -> Result<()> {
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
                Modal::SessionConfirm(prepared, _) if prepared.is_file_copy() => {
                    "REVIEW FILE COPY / one file, never replaces"
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

pub(super) fn confirmation_display(summary: &str, text: &str) -> Result<String> {
    safety::review_display(&format!(
        "{summary}\nCONFIRM > {text}_\nEnter execute | Esc cancel"
    ))
}
pub(super) fn confirmation_lines(summary: &str, text: &str, width: u16) -> Vec<String> {
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
pub(super) fn confirmation_fits(summary: &str, text: &str, width: u16, height: u16) -> bool {
    confirmation_display(summary, text).is_ok()
        && width >= 28
        && height >= 10
        && confirmation_lines(summary, text, width.saturating_sub(1)).len()
            <= usize::from(height.saturating_sub(8))
}
pub(super) fn scroll_offset(text: &str, offset: usize, code: KeyCode) -> usize {
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
pub(super) fn wrap_text(text: &str, width: usize) -> String {
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
