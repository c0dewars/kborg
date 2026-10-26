//! Mutation review: every wrapped line must be painted before approval.
use crossterm::event::KeyCode;
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};

/// Strict mode: reviews need their typed confirmation phrase. Off by default,
/// where `y`/Enter approves; an operator config can turn it on.
static STRICT: AtomicBool = AtomicBool::new(false);
pub fn set_strict(strict: bool) {
    STRICT.store(strict, Ordering::Relaxed);
}
thread_local! {
    /// Per-thread override, so tests of both modes can run in parallel.
    static OVERRIDE: std::cell::Cell<Option<bool>> = const { std::cell::Cell::new(None) };
}
pub fn strict() -> bool {
    OVERRIDE
        .with(std::cell::Cell::get)
        .unwrap_or_else(|| STRICT.load(Ordering::Relaxed))
}
#[cfg(test)]
pub fn set_thread_strict(strict: Option<bool>) {
    OVERRIDE.with(|cell| cell.set(strict));
}
pub struct Review {
    pub typed: String,
    pub epoch: u64,
    pub confirmation: String,
    text: String,
    display_valid: bool,
    offset: usize,
    width: usize,
    viewed: BTreeSet<usize>,
}
impl Review {
    pub fn new(text: String, confirmation: String, epoch: u64) -> Self {
        let display = crate::safety::review_display(&text);
        Self {
            typed: String::new(),
            epoch,
            confirmation,
            display_valid: display.is_ok(),
            text: display
                .unwrap_or_else(|error| format!("REVIEW BLOCKED\n\n{error}\n\nEsc returns.")),
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
            && self.display_valid
            && self.epoch == epoch
            && self.remaining(usize::from(width - 2)) == 0
            && self.typed == self.confirmation
    }
    /// One-key approval (the default outside strict mode): the review was on
    /// screen, so take it as read and fill in the exact confirmation.
    pub fn accept(&mut self) {
        self.typed = self.confirmation.clone();
        self.viewed = (0..self.wrapped(self.width).len()).collect();
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
            // Space: batch confirmations read like `delete 7 objects`.
            KeyCode::Char(ch) if (ch.is_ascii_graphic() || ch == ' ') && self.typed.len() < 320 => {
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
    fn unicode_review_accounts_for_every_escaped_cell_and_overflow_blocks_approval() {
        let mut review = Review::new("context: 東京\\u{e9}".into(), "ns/app".into(), 3);
        review.typed = "ns/app".into();
        let expected = crate::safety::review_display("context: 東京\\u{e9}")
            .unwrap_or_else(|error| panic!("{error}"));
        let lines = review.wrapped(26);
        assert!(lines.iter().all(|line| line.is_ascii() && line.len() <= 26));
        assert_eq!(lines.concat(), expected.replace('\n', ""));
        review.mark_drawn(26, 2);
        assert!(!review.ready(28, 10, 3));
        while review.remaining(26) > 0 {
            review.key(KeyCode::PageDown, 26, 2);
            review.mark_drawn(26, 2);
        }
        assert!(review.ready(28, 10, 3));
        let mut excessive = Review::new("é".repeat(200_000), "ns/app".into(), 3);
        excessive.typed = "ns/app".into();
        excessive.mark_drawn(200, 200);
        assert!(
            excessive
                .lines(200, 200)
                .concat()
                .contains("REVIEW BLOCKED")
        );
        assert!(!excessive.ready(202, 208, 3));
    }

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
