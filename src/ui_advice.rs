//! Explicit transfer review; only successfully painted lines count as viewed.
use crate::advice::PreparedAdvice;
use crossterm::event::KeyCode;
use std::collections::BTreeSet;

pub struct Review {
    pub prepared: Option<PreparedAdvice>,
    pub typed: String,
    pub epoch: u64,
    text: String,
    offset: usize,
    width: usize,
    viewed: BTreeSet<usize>,
}
impl Review {
    pub fn new(prepared: Option<PreparedAdvice>, text: String, epoch: u64) -> Self {
        Self {
            prepared,
            text,
            epoch,
            typed: String::new(),
            offset: 0,
            width: 0,
            viewed: BTreeSet::new(),
        }
    }
    fn wrapped(&self, width: usize) -> Vec<String> {
        super::wrap_text(&self.text, width.saturating_sub(2).max(1))
            .lines()
            .map(|line| format!("  {line}"))
            .collect()
    }
    pub fn lines(&self, width: usize, height: usize) -> Vec<String> {
        let lines = self.wrapped(width);
        let offset = self.offset.min(lines.len().saturating_sub(1));
        lines.into_iter().skip(offset).take(height).collect()
    }
    pub fn mark_drawn(&mut self, width: usize, height: usize) {
        if self.width != width {
            self.width = width;
            self.viewed.clear();
        }
        let count = self.wrapped(width).len();
        self.offset = self.offset.min(count.saturating_sub(1));
        self.viewed
            .extend(self.offset..self.offset.saturating_add(height).min(count));
    }
    pub fn remaining(&self, width: usize) -> usize {
        let total = self.wrapped(width).len();
        if self.width != width {
            total
        } else {
            total.saturating_sub(self.viewed.len())
        }
    }
    pub fn can_send(&self, width: u16, height: u16, epoch: u64) -> bool {
        width >= 28
            && height >= 10
            && epoch == self.epoch
            && self.remaining(usize::from(width.saturating_sub(2))) == 0
            && self
                .prepared
                .as_ref()
                .is_some_and(|prepared| self.typed == prepared.confirmation())
    }
    pub fn confirmation(&self) -> String {
        self.prepared
            .as_ref()
            .map(|prepared| prepared.confirmation())
            .unwrap_or_else(|| "LOCAL PREVIEW / nothing will be sent".into())
    }
    pub fn key(&mut self, key: KeyCode, width: usize, height: usize) {
        let total = self.wrapped(width).len();
        let page = height.saturating_sub(1).max(1);
        match key {
            KeyCode::Up => self.offset = self.offset.saturating_sub(1),
            KeyCode::Down => self.offset = self.offset.saturating_add(1),
            KeyCode::PageUp => self.offset = self.offset.saturating_sub(page),
            KeyCode::PageDown => self.offset = self.offset.saturating_add(page),
            KeyCode::Home => self.offset = 0,
            KeyCode::End => self.offset = total.saturating_sub(height.max(1)),
            KeyCode::Backspace => {
                self.typed.pop();
            }
            KeyCode::Char(c) if !c.is_control() && self.typed.len() + c.len_utf8() <= 320 => {
                self.typed.push(c)
            }
            _ => {}
        }
        self.offset = self.offset.min(total.saturating_sub(1));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jumping_to_end_or_resizing_never_approves_unpainted_transfer() {
        let mut review = Review::new(
            None,
            (0..100).map(|i| format!("diagnostic line {i}\n")).collect(),
            7,
        );
        review.mark_drawn(78, 10);
        assert_eq!(review.remaining(78), 90);
        review.key(KeyCode::End, 78, 10);
        review.mark_drawn(78, 10);
        assert_eq!(review.remaining(78), 80);
        assert!(!review.can_send(80, 24, 7));
        review.mark_drawn(38, 10);
        assert_eq!(review.remaining(38), 90);
        assert!(!review.can_send(40, 24, 8));
    }
    #[test]
    fn widening_at_end_paints_the_same_clamped_lines_that_are_acknowledged() {
        let mut review = Review::new(None, "diagnostic ".repeat(80), 7);
        review.key(KeyCode::End, 20, 3);
        assert!(review.offset > 10);
        let rendered = review.lines(1000, 3);
        assert_eq!(rendered.len(), 1);
        assert!(rendered[0].contains("diagnostic"));
        review.mark_drawn(1000, 3);
        assert_eq!(review.offset, 0);
        assert_eq!(review.remaining(1000), 0);
    }
}
