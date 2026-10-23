//! Mutation review: every wrapped line must be painted before approval.
use crossterm::event::KeyCode;
use std::collections::BTreeSet;
pub struct Review {
    pub typed: String,
    pub epoch: u64,
    pub confirmation: String,
    text: String,
    offset: usize,
    width: usize,
    viewed: BTreeSet<usize>,
}
impl Review {
    pub fn new(text: String, confirmation: String, epoch: u64) -> Self {
        Self {
            typed: String::new(),
            epoch,
            confirmation,
            text,
            offset: 0,
            width: 0,
            viewed: BTreeSet::new(),
        }
    }
    fn wrapped(&self, width: usize) -> Vec<String> {
        let width = width.max(1);
        let mut output = Vec::new();
        for line in self.text.lines() {
            let chars = line.chars().collect::<Vec<_>>();
            if chars.is_empty() {
                output.push(String::new());
                continue;
            }
            let mut start = 0;
            while start < chars.len() {
                let mut end = (start + width).min(chars.len());
                if end < chars.len()
                    && let Some(space) = chars[start..end].iter().rposition(|ch| ch.is_whitespace())
                    && space >= width / 2
                {
                    end = start + space + 1;
                }
                output.push(chars[start..end].iter().collect());
                start = end;
            }
        }
        output
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
        let count = self.wrapped(width).len();
        if width != self.width {
            count
        } else {
            count.saturating_sub(self.viewed.len())
        }
    }
    pub fn ready(&self, width: u16, height: u16, epoch: u64) -> bool {
        width >= 28
            && height >= 10
            && self.epoch == epoch
            && self.remaining(usize::from(width - 2)) == 0
            && self.typed == self.confirmation
    }
    pub fn key(&mut self, key: KeyCode, width: usize, height: usize) {
        let count = self.wrapped(width).len();
        let page = height.saturating_sub(1).max(1);
        match key {
            KeyCode::Up => self.offset = self.offset.saturating_sub(1),
            KeyCode::Down => self.offset += 1,
            KeyCode::PageUp => self.offset = self.offset.saturating_sub(page),
            KeyCode::PageDown => self.offset = self.offset.saturating_add(page),
            KeyCode::Home => self.offset = 0,
            KeyCode::End => self.offset = count.saturating_sub(height.max(1)),
            KeyCode::Backspace => {
                self.typed.pop();
            }
            KeyCode::Char(ch) if ch.is_ascii_graphic() && self.typed.len() < 320 => {
                self.typed.push(ch)
            }
            _ => {}
        }
        self.offset = self.offset.min(count.saturating_sub(1));
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn wrapping_preserves_every_review_character_and_prefers_word_boundaries() {
        let text = "Node identity and scheduling stay bound. VeryLongUnbrokenIdentityValue";
        let review = Review::new(text.into(), "nodes/test".into(), 1);
        for width in 1..80 {
            let lines = review.wrapped(width);
            assert_eq!(lines.concat(), text);
            assert!(lines.iter().all(|line| line.chars().count() <= width));
        }
        assert_eq!(review.wrapped(20)[0], "Node identity and ");
    }
    #[test]
    fn full_painted_review_is_required_and_resizing_invalidates_it() {
        let mut review = Review::new(
            (0..60).map(|i| format!("changed field {i}\n")).collect(),
            "ns/app".into(),
            3,
        );
        review.typed = "ns/app".into();
        review.mark_drawn(78, 16);
        assert!(!review.ready(80, 24, 3));
        review.key(KeyCode::End, 78, 16);
        review.mark_drawn(78, 16);
        assert!(!review.ready(80, 24, 3));
        review.key(KeyCode::Home, 78, 16);
        for _ in 0..4 {
            review.mark_drawn(78, 16);
            review.key(KeyCode::PageDown, 78, 16);
        }
        assert!(review.ready(80, 24, 3));
        assert!(!review.ready(80, 24, 4));
        assert!(!review.ready(40, 24, 3));
    }
}
