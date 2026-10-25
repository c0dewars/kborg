//! A small terminal chart rasterizer: series of `(x, y)` samples become rows
//! of single-cell glyphs with optional axes.
//!
//! Output is split into spans. Axis labels are ordinary text and must be drawn
//! through the normal ASCII display policy. Plot, axis-line and reference
//! glyphs are generated only here, from a fixed trusted set
//! ([`trusted_glyph`]), so the terminal renderer may print them directly the
//! same way it prints border glyphs. Nothing from cluster data can become a
//! glyph span.
//!
//! Gaps are explicit: a non-finite `y` is never interpolated, joined or drawn
//! as zero.

/// How a chart is rasterized.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// Braille dots (U+2800 block): 2x4 dots per cell, a connected line.
    Braille,
    /// Eighth blocks (U+2581..U+2588): an area chart with 8 levels per cell.
    Blocks,
    /// Printable ASCII only: `#` columns, one level per cell.
    Ascii,
}

/// What a span is, so the renderer can color it from theme roles.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpanKind {
    /// Axis label text: drawn through the normal ASCII display path.
    Label,
    /// Axis lines (trusted glyphs).
    Axis,
    /// The series itself (trusted glyphs).
    Plot,
    /// A horizontal reference line such as a limit (trusted glyphs).
    Reference,
}

/// A run of text of one kind. Glyph spans can only be built in this module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Span {
    kind: SpanKind,
    text: String,
}
impl Span {
    fn label(text: impl Into<String>) -> Self {
        Self {
            kind: SpanKind::Label,
            text: text.into(),
        }
    }
    fn glyphs(kind: SpanKind, text: String) -> Self {
        Self { kind, text }
    }
    pub fn kind(&self) -> SpanKind {
        self.kind
    }
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Width in terminal cells; every glyph and label character is one cell.
    pub fn width(&self) -> usize {
        self.text.chars().count()
    }
}

/// One rasterized chart: `rows` top to bottom, each a sequence of spans.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Frame {
    pub rows: Vec<Vec<Span>>,
}
impl Frame {
    /// Plain text of the whole frame, one line per row (tests and dumps).
    pub fn text(&self) -> String {
        self.rows
            .iter()
            .map(|row| row.iter().map(Span::text).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Glyphs the renderer may print without the ASCII data policy: printable
/// ASCII, the braille block, eighth blocks and the few axis glyphs used here.
pub fn trusted_glyph(c: char) -> bool {
    c == ' '
        || c.is_ascii_graphic()
        || ('\u{2800}'..='\u{28FF}').contains(&c)
        || ('\u{2581}'..='\u{2588}').contains(&c)
        || matches!(c, '│' | '└' | '─' | '┈')
}

/// Chart geometry and labelling.
pub struct Spec<'a> {
    /// Total width in cells, axis labels included.
    pub width: usize,
    /// Total height in rows, the x axis included.
    pub height: usize,
    pub style: Style,
    pub axes: bool,
    /// The x interval shown, left to right.
    pub x_range: (f64, f64),
    /// The y axis covers at least `0..=y_floor` (e.g. a known limit).
    pub y_floor: Option<f64>,
    /// Draws a dashed horizontal line at this value when it fits.
    pub reference: Option<f64>,
    /// Use binary (powers of two) "nice" axis maxima, for bytes.
    pub binary: bool,
    /// Formats a y value with its unit.
    pub y_label: &'a dyn Fn(f64) -> String,
    /// Left, middle and right x axis labels.
    pub x_labels: [&'a str; 3],
}

const BLOCKS: [char; 9] = [' ', '▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];

/// The smallest "nice" value at least `value`: 1, 1.2, 1.5, 2, 2.5, 3, 4, 5,
/// 6 or 8 x 10^k, or for binary
/// quantities 2^k and 1.5 x 2^k.
pub fn nice_ceiling(value: f64, binary: bool) -> f64 {
    if !value.is_finite() || value <= 0.0 {
        return 1.0;
    }
    if binary {
        let mut step = 1.0f64;
        while step * 2.0 <= value {
            step *= 2.0;
        }
        while step > value {
            step /= 2.0;
        }
        return [step, step * 1.5, step * 2.0]
            .into_iter()
            .find(|candidate| *candidate >= value)
            .unwrap_or(step * 2.0);
    }
    let exponent = value.log10().floor();
    let base = 10f64.powf(exponent);
    [1.0, 1.2, 1.5, 2.0, 2.5, 3.0, 4.0, 5.0, 6.0, 8.0, 10.0]
        .into_iter()
        .map(|factor| factor * base)
        .find(|candidate| *candidate >= value * (1.0 - 1e-12))
        .unwrap_or(10.0 * base)
}

/// Rasterize one series. Points outside `x_range` are skipped; non-finite or
/// negative `y` are gaps (metrics are never negative).
pub fn render(spec: &Spec, points: &[(f64, f64)]) -> Frame {
    let finite = |y: f64| y.is_finite() && y >= 0.0;
    let peak = points
        .iter()
        .map(|(_, y)| *y)
        .filter(|y| finite(*y))
        .fold(0.0f64, f64::max);
    let floor = spec.y_floor.filter(|v| finite(*v)).unwrap_or(0.0);
    let reference = spec.reference.filter(|v| finite(*v) && *v > 0.0);
    let top = nice_ceiling(peak.max(floor).max(reference.unwrap_or(0.0)), spec.binary);
    let labels = [
        (spec.y_label)(top),
        (spec.y_label)(top / 2.0),
        (spec.y_label)(0.0),
    ];
    let label_width = if spec.axes {
        labels.iter().map(|l| l.chars().count()).max().unwrap_or(0)
    } else {
        0
    };
    let axis_cols = if spec.axes { label_width + 1 } else { 0 };
    let axis_rows = if spec.axes { 2 } else { 0 };
    let cols = spec.width.saturating_sub(axis_cols);
    let rows = spec.height.saturating_sub(axis_rows);
    if cols < 2 || rows < 1 {
        return Frame {
            rows: vec![vec![Span::label(fit("enlarge for chart", spec.width))]],
        };
    }
    let (x0, x1) = spec.x_range;
    let span = if (x1 - x0).is_finite() && x1 > x0 {
        x1 - x0
    } else {
        1.0
    };
    let (sub_x, sub_y) = match spec.style {
        Style::Braille => (2, 4),
        Style::Blocks => (1, 8),
        Style::Ascii => (1, 1),
    };
    let grid_w = cols * sub_x;
    let grid_h = rows * sub_y;
    let column = |x: f64| -> Option<usize> {
        if !x.is_finite() || x < x0 - span * 1e-9 || x > x1 + span * 1e-9 {
            return None;
        }
        let position = ((x - x0) / span * (grid_w - 1) as f64).round();
        Some((position.max(0.0) as usize).min(grid_w - 1))
    };
    // Sub-cell height of a value, 0..=grid_h.
    let level = |y: f64| -> f64 { (y / top * grid_h as f64).clamp(0.0, grid_h as f64) };
    let mut cells = vec![vec![' '; cols]; rows];
    match spec.style {
        Style::Braille => {
            let mut dots = vec![vec![false; grid_w]; grid_h];
            let mut plot = |x: usize, y: f64| {
                // Dot row from the top; a zero value sits on the bottom row.
                let from_bottom = (y.round() as usize).min(grid_h).saturating_sub(1);
                dots[grid_h - 1 - from_bottom][x] = true;
            };
            let mut previous: Option<(usize, f64)> = None;
            for (x, y) in points {
                let Some(gx) = column(*x) else {
                    previous = None;
                    continue;
                };
                if !finite(*y) {
                    previous = None;
                    continue;
                }
                let gy = level(*y).max(1.0);
                match previous {
                    Some((px, py)) if gx >= px => {
                        let steps = (gx - px).max((gy - py).abs().ceil() as usize).max(1);
                        for step in 0..=steps {
                            let t = step as f64 / steps as f64;
                            let sx = px as f64 + (gx - px) as f64 * t;
                            plot(sx.round() as usize, py + (gy - py) * t);
                        }
                    }
                    _ => plot(gx, gy),
                }
                previous = Some((gx, gy));
            }
            for (row, line) in cells.iter_mut().enumerate() {
                for (col, cell) in line.iter_mut().enumerate() {
                    let mut bits = 0u32;
                    for (dx, dy, bit) in [
                        (0, 0, 0x01),
                        (0, 1, 0x02),
                        (0, 2, 0x04),
                        (1, 0, 0x08),
                        (1, 1, 0x10),
                        (1, 2, 0x20),
                        (0, 3, 0x40),
                        (1, 3, 0x80),
                    ] {
                        if dots[row * 4 + dy][col * 2 + dx] {
                            bits |= bit;
                        }
                    }
                    if bits != 0 {
                        *cell = char::from_u32(0x2800 + bits).unwrap_or(' ');
                    }
                }
            }
        }
        Style::Blocks | Style::Ascii => {
            let mut heights: Vec<Option<f64>> = vec![None; cols];
            let mut set = |col: usize, value: f64| {
                heights[col] = Some(heights[col].map_or(value, |h: f64| h.max(value)));
            };
            // Columns between two consecutive measurements follow the line
            // joining them, like the braille style; gaps stay empty.
            let mut previous: Option<(usize, f64)> = None;
            for (x, y) in points {
                let (Some(col), true) = (column(*x), finite(*y)) else {
                    previous = None;
                    continue;
                };
                let value = level(*y);
                if let Some((pc, pv)) = previous.filter(|(pc, _)| col > *pc) {
                    for between in pc + 1..col {
                        let t = (between - pc) as f64 / (col - pc) as f64;
                        set(between, pv + (value - pv) * t);
                    }
                }
                set(col, value);
                previous = Some((col, value));
            }
            for (col, height) in heights.iter().enumerate() {
                let Some(height) = height else { continue };
                // A measured non-zero value is never drawn as nothing.
                let units = (height.round() as usize).max(usize::from(*height > 0.0));
                for (row, line) in cells.iter_mut().enumerate() {
                    let from_bottom = rows - 1 - row;
                    let filled = units.saturating_sub(from_bottom * sub_y).min(sub_y);
                    line[col] = match spec.style {
                        Style::Blocks => BLOCKS[filled],
                        _ if filled > 0 => '#',
                        _ if from_bottom == 0 => '_',
                        _ => ' ',
                    };
                }
            }
        }
    }
    if let Some(value) = reference {
        let from_bottom = ((value / top * rows as f64).round() as usize).clamp(1, rows) - 1;
        let dash = if spec.style == Style::Ascii {
            '-'
        } else {
            '┈'
        };
        let line = &mut cells[rows - 1 - from_bottom];
        for (col, cell) in line.iter_mut().enumerate() {
            if *cell == ' ' && col % 2 == 0 {
                *cell = dash;
            }
        }
    }
    let ascii = spec.style == Style::Ascii;
    let (vertical, corner, horizontal) = if ascii {
        ('|', '+', '-')
    } else {
        ('│', '└', '─')
    };
    let reference_row =
        reference.map(|value| rows - ((value / top * rows as f64).round() as usize).clamp(1, rows));
    let mut frame = Frame::default();
    for (index, line) in cells.into_iter().enumerate() {
        let mut row = Vec::new();
        if spec.axes {
            let label = if index == 0 {
                labels[0].as_str()
            } else if rows >= 3 && index == rows / 2 {
                labels[1].as_str()
            } else if rows >= 2 && index == rows - 1 {
                labels[2].as_str()
            } else {
                ""
            };
            row.push(Span::label(format!("{label:>label_width$}")));
            row.push(Span::glyphs(SpanKind::Axis, vertical.to_string()));
        }
        // Split the plot line so reference dashes can take their own color.
        let mut current = String::new();
        let mut current_kind = SpanKind::Plot;
        for glyph in line {
            let kind = if reference_row == Some(index) && matches!(glyph, '┈' | '-') {
                SpanKind::Reference
            } else {
                SpanKind::Plot
            };
            if kind != current_kind && !current.is_empty() {
                row.push(Span::glyphs(current_kind, std::mem::take(&mut current)));
            }
            current_kind = kind;
            current.push(glyph);
        }
        if !current.is_empty() {
            row.push(Span::glyphs(current_kind, current));
        }
        frame.rows.push(row);
    }
    if spec.axes {
        frame.rows.push(vec![
            Span::label(" ".repeat(label_width)),
            Span::glyphs(
                SpanKind::Axis,
                format!("{corner}{}", horizontal.to_string().repeat(cols)),
            ),
        ]);
        let [left, middle, right] = spec.x_labels;
        let mut axis = vec![' '; cols];
        let mut place = |start: usize, text: &str| {
            for (offset, c) in text.chars().enumerate() {
                if let Some(slot) = axis.get_mut(start + offset) {
                    *slot = c;
                }
            }
        };
        let middle_len = middle.chars().count();
        let right_len = right.chars().count();
        if cols >= left.chars().count() + middle_len + right_len + 4 {
            place((cols / 2).saturating_sub(middle_len / 2), middle);
        }
        place(cols.saturating_sub(right_len), right);
        place(0, left);
        frame.rows.push(vec![
            Span::label(" ".repeat(label_width + 1)),
            Span::label(axis.into_iter().collect::<String>()),
        ]);
    }
    frame
}

fn fit(text: &str, width: usize) -> String {
    text.chars().take(width).collect()
}

/// Millicores with a unit: `125m`, `2.5m`, `1500m`.
pub fn millicores_label(millicores: f64) -> String {
    if millicores == 0.0 {
        "0".into()
    } else if millicores < 10.0 {
        trim(format!("{millicores:.1}")) + "m"
    } else {
        format!("{millicores:.0}m")
    }
}

/// Bytes as KiB, MiB or GiB with a unit: `64Mi`, `1.5Gi`.
pub fn bytes_label(bytes: f64) -> String {
    const KI: f64 = 1024.0;
    if bytes == 0.0 {
        return "0".into();
    }
    let (value, unit) = if bytes >= KI * KI * KI {
        (bytes / (KI * KI * KI), "Gi")
    } else if bytes >= KI * KI {
        (bytes / (KI * KI), "Mi")
    } else {
        (bytes / KI, "Ki")
    };
    if value < 10.0 {
        trim(format!("{value:.1}")) + unit
    } else {
        format!("{value:.0}{unit}")
    }
}

/// A plain count: `3`, or `1.5` between whole numbers.
pub fn count_label(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        trim(format!("{value:.1}"))
    }
}

/// Percent with a unit: `42%`.
pub fn percent_label(percent: f64) -> String {
    format!("{percent:.0}%")
}

fn trim(text: String) -> String {
    if text.contains('.') {
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    } else {
        text
    }
}

/// A relative age label for the x axis: `-10m`, `-45s`.
pub fn age_label(seconds: f64) -> String {
    let seconds = seconds.abs().round() as u64;
    if seconds == 0 {
        "now".into()
    } else if seconds >= 120 && seconds.is_multiple_of(60) {
        format!("-{}m", seconds / 60)
    } else if seconds >= 120 {
        format!("-{}m{}s", seconds / 60, seconds % 60)
    } else {
        format!("-{seconds}s")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec<'a>(
        style: Style,
        axes: bool,
        width: usize,
        height: usize,
        label: &'a dyn Fn(f64) -> String,
    ) -> Spec<'a> {
        Spec {
            width,
            height,
            style,
            axes,
            x_range: (0.0, 3.0),
            y_floor: None,
            reference: None,
            binary: false,
            y_label: label,
            x_labels: ["-3s", "", "now"],
        }
    }
    fn plot_rows(frame: &Frame) -> Vec<String> {
        frame
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .filter(|span| matches!(span.kind(), SpanKind::Plot | SpanKind::Reference))
                    .map(Span::text)
                    .collect()
            })
            .collect()
    }

    #[test]
    fn nice_ceiling_scales_to_round_maxima() {
        assert_eq!(nice_ceiling(0.0, false), 1.0);
        assert_eq!(nice_ceiling(f64::NAN, false), 1.0);
        assert_eq!(nice_ceiling(137.0, false), 150.0);
        assert_eq!(nice_ceiling(250.0, false), 250.0);
        assert_eq!(nice_ceiling(200.0, false), 200.0);
        assert_eq!(nice_ceiling(0.45, false), 0.5);
        assert_eq!(nice_ceiling(6.5, false), 8.0);
        assert_eq!(nice_ceiling(9.0, false), 10.0);
        let mi = 1024.0 * 1024.0;
        assert_eq!(nice_ceiling(64.0 * mi, true), 64.0 * mi);
        assert_eq!(nice_ceiling(65.0 * mi, true), 96.0 * mi);
        assert_eq!(nice_ceiling(100.0 * mi, true), 128.0 * mi);
    }

    #[test]
    fn braille_output_for_a_known_series() {
        let label = |v: f64| format!("{v:.0}");
        let s = spec(Style::Braille, false, 2, 1, &label);
        // Four points over four dot columns: bottom, rising to the top.
        let frame = render(&s, &[(0.0, 0.25), (1.0, 0.5), (2.0, 0.75), (3.0, 1.0)]);
        assert_eq!(frame.text(), "\u{2860}\u{280A}");
        // Each dot column holds exactly one lit dot of a monotonic line.
        let flat = render(&s, &[(0.0, 1.0), (3.0, 1.0)]);
        assert_eq!(flat.text(), "\u{2809}\u{2809}");
    }

    #[test]
    fn blocks_and_ascii_scale_columns_to_the_axis_maximum() {
        let label = |v: f64| format!("{v:.0}");
        let points = [(0.0, 0.0), (1.0, 50.0), (2.0, 100.0), (3.0, 25.0)];
        let blocks = render(&spec(Style::Blocks, false, 4, 2, &label), &points);
        assert_eq!(plot_rows(&blocks), vec!["  █ ", " ██▄"]);
        let ascii = render(&spec(Style::Ascii, false, 4, 4, &label), &points);
        assert_eq!(plot_rows(&ascii), vec!["  # ", "  # ", " ## ", "_###"]);
        assert!(ascii.text().chars().all(|c| c == '\n' || c.is_ascii()));
    }

    #[test]
    fn empty_nan_and_one_point_series_never_invent_values() {
        let label = |v: f64| format!("{v:.0}");
        for style in [Style::Braille, Style::Blocks, Style::Ascii] {
            let empty = render(&spec(style, false, 6, 2, &label), &[]);
            assert!(empty.text().chars().all(|c| c == ' ' || c == '\n'));
            let gaps = render(
                &spec(style, false, 6, 2, &label),
                &[(0.0, f64::NAN), (1.5, f64::INFINITY), (3.0, -1.0)],
            );
            assert!(gaps.text().chars().all(|c| c == ' ' || c == '\n'));
            let one = render(&spec(style, false, 6, 2, &label), &[(3.0, 5.0)]);
            let glyphs: Vec<char> = one.text().chars().filter(|c| !c.is_whitespace()).collect();
            assert!(!glyphs.is_empty(), "{style:?} dropped a single point");
            assert!(one.text().lines().all(|line| line.chars().count() == 6));
        }
        // A gap separates segments instead of joining them.
        let joined = render(
            &spec(Style::Braille, false, 8, 1, &label),
            &[(0.0, 1.0), (1.0, f64::NAN), (3.0, 1.0)],
        );
        assert_eq!(joined.text().chars().filter(|c| *c != ' ').count(), 2);
    }

    #[test]
    fn axes_carry_units_and_time_labels_and_fit_the_width() {
        let label = |v: f64| millicores_label(v);
        let mut s = spec(Style::Ascii, true, 30, 6, &label);
        s.x_labels = ["-10m", "-5m", "now"];
        s.reference = Some(150.0);
        let frame = render(&s, &[(0.0, 40.0), (1.5, 120.0), (3.0, 80.0)]);
        let text = frame.text();
        assert!(text.contains("150m|"), "{text}");
        assert!(text.contains(" 75m|"), "{text}");
        assert!(text.contains("   0|"), "{text}");
        assert!(text.contains("+-----"), "{text}");
        assert!(text.contains("-10m") && text.contains("-5m") && text.ends_with("now"));
        assert!(
            frame
                .rows
                .iter()
                .flatten()
                .any(|span| span.kind() == SpanKind::Reference)
        );
        for row in &frame.rows {
            assert_eq!(row.iter().map(Span::width).sum::<usize>(), 30);
        }
        // Unicode styles use box glyphs for the axes, all from the trusted set.
        s.style = Style::Braille;
        let frame = render(&s, &[(0.0, 40.0), (3.0, 80.0)]);
        assert!(frame.text().contains('│') && frame.text().contains('└'));
        for span in frame.rows.iter().flatten() {
            if span.kind() != SpanKind::Label {
                assert!(span.text().chars().all(trusted_glyph));
            }
        }
        // Too small: a short notice instead of a broken chart.
        s.width = 6;
        assert_eq!(render(&s, &[]).text(), "enlarg");
    }

    #[test]
    fn unit_labels() {
        assert_eq!(millicores_label(0.0), "0");
        assert_eq!(millicores_label(2.5), "2.5m");
        assert_eq!(millicores_label(125.0), "125m");
        let mi = 1024.0 * 1024.0;
        assert_eq!(bytes_label(64.0 * mi), "64Mi");
        assert_eq!(bytes_label(1.5 * 1024.0 * mi), "1.5Gi");
        assert_eq!(bytes_label(512.0 * 1024.0), "512Ki");
        assert_eq!(percent_label(41.6), "42%");
        assert_eq!(count_label(2.5), "2.5");
        assert_eq!(count_label(3.0), "3");
        assert_eq!(age_label(600.0), "-10m");
        assert_eq!(age_label(45.0), "-45s");
        assert_eq!(age_label(0.0), "now");
        assert!(trusted_glyph('⣿') && trusted_glyph('▁') && trusted_glyph('#'));
        assert!(!trusted_glyph('é') && !trusted_glyph('\u{1b}') && !trusted_glyph('\u{202e}'));
    }
}
