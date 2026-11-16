//! A mouse selection refers to the cells of one rendered log viewport.
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};

/// Log text starts one cell inside the left edge of the panel.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Viewport {
    first_row: u16,
    pub columns: u16,
    pub rows: u16,
    unicode: bool,
}

impl Viewport {
    pub fn new(first_row: u16, columns: u16, rows: u16) -> Self {
        Self {
            first_row,
            columns,
            rows,
            unicode: crate::text::unicode(),
        }
    }

    fn point(self, mouse: MouseEvent, row_count: usize) -> Point {
        Point {
            row: usize::from(mouse.row.saturating_sub(self.first_row))
                .min(row_count.saturating_sub(1)),
            column: usize::from(mouse.column.saturating_sub(1))
                .min(usize::from(self.columns.saturating_sub(1))),
        }
    }

    pub(super) fn contains(self, mouse: MouseEvent) -> bool {
        mouse.column > 0
            && mouse.column <= self.columns
            && mouse.row >= self.first_row
            && mouse.row - self.first_row < self.rows
    }
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Point {
    row: usize,
    column: usize,
}

pub(super) struct Selection {
    viewport: Viewport,
    pub(super) lines: Vec<String>,
    start: Point,
    end: Point,
    pub(super) dragging: bool,
}

impl Selection {
    pub(super) fn start(viewport: Viewport, mouse: MouseEvent, lines: Vec<String>) -> Option<Self> {
        if usize::from(mouse.row - viewport.first_row) >= lines.len() {
            return None;
        }
        let start = viewport.point(mouse, lines.len());
        Some(Self {
            viewport,
            lines,
            start,
            end: start,
            dragging: true,
        })
    }

    pub(super) fn matches(&self, viewport: Viewport) -> bool {
        self.viewport == viewport
    }

    pub(super) fn update(&mut self, mouse: MouseEvent) {
        self.end = self.viewport.point(mouse, self.lines.len());
        self.dragging = mouse.kind != MouseEventKind::Up(MouseButton::Left);
    }

    pub(super) fn is_empty(&self) -> bool {
        self.start == self.end
    }

    pub(super) fn highlight(&self, row: usize) -> Option<(u16, u16, &str)> {
        if self.is_empty() {
            return None;
        }
        let first = self.start.min(self.end);
        let last = self.start.max(self.end);
        if !(first.row..=last.row).contains(&row) {
            return None;
        }
        let lower = if row == first.row { first.column } else { 0 };
        let upper = if row == last.row {
            last.column + 1
        } else {
            usize::from(self.viewport.columns)
        };
        let text = &self.lines[row];
        // Endpoints landing on either cell of a wide glyph select it whole.
        let mut offset = 0;
        let mut column = 0;
        let mut selected = None;
        for glyph in crate::text::graphemes(text) {
            let end = column + crate::text::cluster_width(glyph);
            if column >= upper {
                break;
            }
            if end > lower {
                let selected = selected.get_or_insert((offset, column, 0, 0));
                selected.2 = offset + glyph.len();
                selected.3 = end;
            }
            column = end;
            offset += glyph.len();
        }
        selected.map(|(start, column, end, end_column)| {
            (
                column as u16,
                (end_column - column) as u16,
                &text[start..end],
            )
        })
    }

    pub(super) fn text(&self) -> Option<String> {
        if self.is_empty() {
            return None;
        }
        let first = self.start.row.min(self.end.row);
        let last = self.start.row.max(self.end.row);
        let text = (first..=last)
            .map(|row| self.highlight(row).map_or("", |(_, _, text)| text))
            .collect::<Vec<_>>()
            .join("\n");
        (!text.is_empty()).then_some(text)
    }
}
