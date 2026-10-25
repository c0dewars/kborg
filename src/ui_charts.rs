//! Chart panels: `:metrics` chart mode for the selected Pod or Node, and the
//! dashboard's expanded trend panel for the focused card. Both draw bounded,
//! session-local history through `crate::chart`; nothing here collects data.
use super::{App, Rect, cell, chart_row, ui_color};
use crate::{
    Result,
    chart::{self, SpanKind},
    metrics::{ChartSample, HISTORY_RETENTION},
};
use crossterm::style::Color;
use serde_json::Value;
use std::{io::Write, time::Instant};

/// One chart to draw: a header line, then the rasterized series.
pub(super) struct Plot<'a> {
    pub header: String,
    pub points: Vec<(f64, f64)>,
    pub label: &'a dyn Fn(f64) -> String,
    pub binary: bool,
    pub reference: Option<f64>,
    pub y_floor: Option<f64>,
    pub role: &'a str,
}

/// The x window for samples `max_age` seconds old: at least one minute,
/// rounded up to 30 seconds and never more than `limit`.
fn window(max_age: f64, limit: f64) -> f64 {
    ((max_age / 30.0).ceil() * 30.0).clamp(60.0, limit.max(60.0))
}

/// Draws `plot` into `rect`: header on the first row, chart below.
pub(super) fn draw(
    app: &App,
    out: &mut impl Write,
    rect: Rect,
    plot: &Plot,
    span_seconds: f64,
    bg: Color,
) -> Result<()> {
    if rect.height == 0 || rect.width == 0 {
        return Ok(());
    }
    cell(
        out,
        rect.x,
        rect.y,
        rect.width,
        &plot.header,
        ui_color(app, "text"),
        bg,
    )
    .map_err(|_| "draw failed")?;
    let left = chart::age_label(span_seconds);
    let middle = chart::age_label(span_seconds / 2.0);
    let spec = chart::Spec {
        width: usize::from(rect.width),
        height: usize::from(rect.height.saturating_sub(1)),
        style: app.ui.charts.style.resolve(),
        axes: app.ui.charts.show_axes,
        x_range: (-span_seconds, 0.0),
        y_floor: plot.y_floor,
        reference: plot.reference,
        binary: plot.binary,
        y_label: plot.label,
        x_labels: [&left, &middle, "now"],
    };
    let frame = chart::render(&spec, &plot.points);
    let series = ui_color(app, plot.role);
    let color = |kind| match kind {
        SpanKind::Plot => series,
        SpanKind::Reference => ui_color(app, "warning"),
        SpanKind::Label | SpanKind::Axis => ui_color(app, "muted"),
    };
    for (index, spans) in frame.rows.iter().enumerate() {
        let y = rect.y + 1 + index as u16;
        if y >= rect.y + rect.height {
            break;
        }
        chart_row(out, rect.x, y, rect.width, spans, color, bg).map_err(|_| "draw failed")?;
    }
    Ok(())
}

/// Requests, limits and allocatable of the selected object, when every
/// container states them. CPU in millicores, memory in bytes.
#[derive(Debug, Default, PartialEq)]
pub(super) struct Denominators {
    pub cpu: Vec<(&'static str, f64)>,
    pub memory: Vec<(&'static str, f64)>,
}
pub(super) fn denominators(object: &Value, node: bool) -> Denominators {
    let cpu = |raw: &Value| {
        crate::metrics::normalize_cpu_quantity(raw.as_str()?)
            .ok()
            .map(|(n, _)| n as f64 / 1e6)
    };
    let memory = |raw: &Value| {
        crate::metrics::normalize_memory_quantity(raw.as_str()?)
            .ok()
            .map(|(n, _)| n as f64)
    };
    let mut result = Denominators::default();
    if node {
        let allocatable = &object["status"]["allocatable"];
        result
            .cpu
            .extend(cpu(&allocatable["cpu"]).map(|v| ("allocatable", v)));
        result
            .memory
            .extend(memory(&allocatable["memory"]).map(|v| ("allocatable", v)));
        return result;
    }
    let containers = object["spec"]["containers"].as_array();
    for basis in ["requests", "limits"] {
        let label = if basis == "requests" {
            "request"
        } else {
            "limit"
        };
        let sum = |parse: &dyn Fn(&Value) -> Option<f64>, resource: &str| {
            let containers = containers?;
            if containers.is_empty() {
                return None;
            }
            containers.iter().try_fold(0.0, |total, container| {
                Some(total + parse(&container["resources"][basis][resource])?)
            })
        };
        result.cpu.extend(sum(&cpu, "cpu").map(|v| (label, v)));
        result
            .memory
            .extend(sum(&memory, "memory").map(|v| (label, v)));
    }
    result
}

fn percents(latest: Option<f64>, bases: &[(&str, f64)], format: &dyn Fn(f64) -> String) -> String {
    bases
        .iter()
        .filter(|(_, base)| *base > 0.0)
        .map(|(label, base)| match latest {
            Some(value) => format!(
                " | {} of {label} {}",
                chart::percent_label(value / base * 100.0),
                format(*base)
            ),
            None => format!(" | {label} {}", format(*base)),
        })
        .collect()
}

/// The reference line: the first basis, when it does not flatten the series.
fn reference(peak: f64, bases: &[(&str, f64)]) -> Option<f64> {
    bases
        .first()
        .map(|(_, base)| *base)
        .filter(|base| *base > 0.0 && (peak == 0.0 || *base <= peak * 4.0))
}

/// `:metrics` chart mode: CPU and memory over the session history of the
/// selected Pod or Node, drawn inside the modal's inner `rect`.
pub(super) fn metrics(app: &App, out: &mut impl Write, rect: Rect, bg: Color) -> Result<()> {
    let text = |out: &mut _, y: u16, line: &str, role: &str| -> Result<()> {
        if y < rect.y + rect.height {
            cell(out, rect.x, y, rect.width, line, ui_color(app, role), bg)
                .map_err(|_| String::from("draw failed"))
        } else {
            Ok(())
        }
    };
    let kind = app.kind();
    let node = kind == "nodes";
    let visible = app.visible();
    let object = visible.get(app.selected).copied();
    let target = object
        .filter(|_| node || kind == "pods")
        .and_then(|object| {
            let name = object["metadata"]["name"].as_str()?;
            let namespace = object["metadata"]["namespace"].as_str();
            Some((object, namespace, name))
        });
    let Some((object, namespace, name)) = target else {
        text(
            out,
            rect.y,
            "CHART needs a selected Pod or Node: open :metrics from :pods or :nodes.",
            "warning",
        )?;
        return text(out, rect.y + 1, "g returns to the text view.", "muted");
    };
    let now = Instant::now();
    let samples =
        app.metrics_history
            .object_samples(if node { None } else { namespace }, name, now);
    let measured = samples.iter().filter(|s| s.usage.is_some()).count();
    let title = format!(
        "CHART {} {} | {measured} samples, last {} | refreshes every 5s",
        if node { "Node" } else { "Pod" },
        namespace.map_or(name.to_owned(), |ns| format!("{ns}/{name}")),
        chart::age_label(HISTORY_RETENTION.as_secs_f64()).trim_start_matches('-'),
    );
    text(out, rect.y, &title, "accent")?;
    if measured == 0 {
        text(
            out,
            rect.y + 1,
            "No measurements yet for this object; samples arrive with each refresh.",
            "muted",
        )?;
        return text(
            out,
            rect.y + 2,
            "Missing samples are gaps, never zero. g returns to the text view.",
            "muted",
        );
    }
    let span = window(
        samples
            .iter()
            .map(|s| s.age.as_secs_f64())
            .fold(0.0, f64::max),
        HISTORY_RETENTION.as_secs_f64(),
    );
    let series = |pick: fn(&ChartSample) -> Option<f64>| -> Vec<(f64, f64)> {
        samples
            .iter()
            .map(|s| (-s.age.as_secs_f64(), pick(s).unwrap_or(f64::NAN)))
            .collect()
    };
    let cpu = series(|s| s.usage.map(|(cpu, _)| cpu as f64 / 1e6));
    let memory = series(|s| s.usage.map(|(_, bytes)| bytes as f64));
    let bases = denominators(object, node);
    let latest = |points: &[(f64, f64)]| points.last().map(|p| p.1).filter(|v| v.is_finite());
    let peak = |points: &[(f64, f64)]| {
        points
            .iter()
            .map(|p| p.1)
            .filter(|v| v.is_finite())
            .fold(0.0, f64::max)
    };
    let cpu_label = |v: f64| chart::millicores_label(v);
    let memory_label = |v: f64| chart::bytes_label(v);
    let header = |name: &str, points: &[(f64, f64)], format: &dyn Fn(f64) -> String, bases| {
        let value = latest(points);
        format!(
            "{name} {}{}",
            value.map_or("gap (last sample missing)".into(), |v| format!(
                "latest {}",
                format(v)
            )),
            percents(value, bases, format)
        )
    };
    let body = rect.height.saturating_sub(2);
    let first = body / 2;
    let plots = [
        Plot {
            header: header("CPU", &cpu, &cpu_label, &bases.cpu),
            reference: reference(peak(&cpu), &bases.cpu),
            points: cpu,
            label: &cpu_label,
            binary: false,
            y_floor: None,
            role: &app.ui.charts.cpu_role,
        },
        Plot {
            header: header("MEMORY", &memory, &memory_label, &bases.memory),
            reference: reference(peak(&memory), &bases.memory),
            points: memory,
            label: &memory_label,
            binary: true,
            y_floor: None,
            role: &app.ui.charts.memory_role,
        },
    ];
    for (index, plot) in plots.iter().enumerate() {
        let area = Rect {
            x: rect.x,
            y: rect.y + 1 + if index == 0 { 0 } else { first },
            width: rect.width,
            height: if index == 0 { first } else { body - first },
        };
        draw(app, out, area, plot, span, bg)?;
    }
    text(
        out,
        rect.y + rect.height - 1,
        "Gaps are errors or missing samples. Dashed line: first request/limit or allocatable.",
        "muted",
    )
}

/// The dashboard chart panel: four trend charts for the focused card.
pub(super) fn dashboard(
    app: &App,
    out: &mut impl Write,
    rect: Rect,
    trend: Option<&super::dashboard_ui::Trend>,
    context: &str,
    bg: Color,
) -> Result<()> {
    if rect.height < 4 || rect.width < 20 {
        return Ok(());
    }
    let inner = Rect {
        x: rect.x + 1,
        y: rect.y + 1,
        width: rect.width - 2,
        height: rect.height - 2,
    };
    super::panel(
        out,
        rect,
        &format!("CHARTS {context}"),
        true,
        ui_color(app, "accent"),
        bg,
        &app.ui,
    )
    .map_err(|_| "draw failed")?;
    let now = Instant::now();
    let points = trend.map(|trend| trend.points(now)).unwrap_or_default();
    if points.len() < 2 {
        return cell(
            out,
            inner.x,
            inner.y,
            inner.width,
            "Collecting trend samples (one every 15s); charts appear after two.",
            ui_color(app, "muted"),
            bg,
        )
        .map_err(|_| "draw failed".into());
    }
    let span = window(
        points.first().map_or(0.0, |point| {
            now.saturating_duration_since(point.at).as_secs_f64()
        }),
        super::dashboard_ui::TREND_WINDOW.as_secs_f64(),
    );
    let series = |pick: &dyn Fn(&super::dashboard_ui::TrendPoint) -> Option<u64>| {
        points
            .iter()
            .map(|point| {
                (
                    -now.saturating_duration_since(point.at).as_secs_f64(),
                    pick(point).map_or(f64::NAN, |v| v as f64),
                )
            })
            .collect::<Vec<_>>()
    };
    let count = |v: f64| chart::count_label(v);
    let percent = |v: f64| chart::percent_label(v);
    let latest = |points: &[(f64, f64)], format: &dyn Fn(f64) -> String| {
        points
            .last()
            .map(|p| p.1)
            .filter(|v| v.is_finite())
            .map_or("unknown".into(), format)
    };
    let attention = series(&|p| Some(p.attention));
    let restarts = series(&|p| p.restarts_delta);
    let cpu = series(&|p| p.cpu);
    let memory = series(&|p| p.memory);
    let plots = [
        Plot {
            header: format!(
                "PODS NEEDING ATTENTION  latest {}",
                latest(&attention, &count)
            ),
            points: attention,
            label: &count,
            binary: false,
            reference: None,
            // Counts: at least 0..2, so the middle tick is a whole number.
            y_floor: Some(2.0),
            role: "warning",
        },
        Plot {
            header: format!(
                "RESTARTS DELTA per sample  latest {}",
                latest(&restarts, &|v| format!("+{}", count(v)))
            ),
            points: restarts,
            label: &count,
            binary: false,
            reference: None,
            // Counts: at least 0..2, so the middle tick is a whole number.
            y_floor: Some(2.0),
            role: "danger",
        },
        Plot {
            header: format!(
                "NODE CPU % of allocatable  latest {}",
                latest(&cpu, &percent)
            ),
            points: cpu,
            label: &percent,
            binary: false,
            reference: None,
            y_floor: Some(100.0),
            role: &app.ui.charts.cpu_role,
        },
        Plot {
            header: format!(
                "NODE MEMORY % of allocatable  latest {}",
                latest(&memory, &percent)
            ),
            points: memory,
            label: &percent,
            binary: false,
            reference: None,
            y_floor: Some(100.0),
            role: &app.ui.charts.memory_role,
        },
    ];
    // 2x2 grid when wide, else four stacked charts.
    let grid = inner.width >= 100;
    let (columns, rows) = if grid { (2u16, 2u16) } else { (1, 4) };
    let cell_width = inner.width / columns;
    let cell_height = inner.height / rows;
    for (index, plot) in plots.iter().enumerate() {
        let index = index as u16;
        let (column, row) = (index % columns, index / columns);
        let area = Rect {
            x: inner.x + column * cell_width,
            y: inner.y + row * cell_height,
            width: if column + 1 == columns {
                inner.width - column * cell_width
            } else {
                cell_width.saturating_sub(2)
            },
            height: if row + 1 == rows {
                inner.height - row * cell_height
            } else {
                cell_height
            },
        };
        draw(app, out, area, plot, span, bg)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn denominators_need_every_container_and_use_base_units() {
        let pod = serde_json::json!({"spec":{"containers":[
            {"resources":{"requests":{"cpu":"250m","memory":"128Mi"},"limits":{"cpu":"1"}}},
            {"resources":{"requests":{"cpu":"50m","memory":"64Mi"}}}
        ]}});
        let bases = denominators(&pod, false);
        assert_eq!(bases.cpu, vec![("request", 300.0)]);
        assert_eq!(bases.memory, vec![("request", 192.0 * 1024.0 * 1024.0)]);
        let node = serde_json::json!({"status":{"allocatable":{"cpu":"3500m","memory":"7Gi"}}});
        let bases = denominators(&node, true);
        assert_eq!(bases.cpu, vec![("allocatable", 3500.0)]);
        assert_eq!(
            bases.memory,
            vec![("allocatable", 7.0 * 1024.0 * 1024.0 * 1024.0)]
        );
        assert_eq!(
            denominators(&serde_json::json!({}), false),
            Denominators::default()
        );
        let label = |v: f64| chart::millicores_label(v);
        assert_eq!(
            percents(Some(125.0), &[("request", 250.0)], &label),
            " | 50% of request 250m"
        );
        assert_eq!(reference(125.0, &[("limit", 1000.0)]), None);
        assert_eq!(reference(125.0, &[("request", 250.0)]), Some(250.0));
        assert_eq!(window(0.0, 600.0), 60.0);
        assert_eq!(window(61.0, 600.0), 90.0);
        assert_eq!(window(5000.0, 600.0), 600.0);
    }
}
