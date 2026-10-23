//! In-memory YAML editor; no shell, temporary file or external editor is involved.
use crate::{
    Result,
    manifest_edit::{Draft, MAX_DOCUMENT},
    safety,
};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use std::cell::Cell;

pub enum Event {
    Continue,
    Review,
    Cancel,
}
pub struct Editor {
    pub draft: Draft,
    lines: Vec<String>,
    row: usize,
    column: usize,
    top: Cell<usize>,
    left: Cell<usize>,
}
impl Editor {
    pub fn new(draft: Draft) -> Self {
        let lines = draft
            .text()
            .trim_end_matches('\n')
            .split('\n')
            .map(str::to_owned)
            .collect();
        Self {
            draft,
            lines,
            row: 0,
            column: 0,
            top: Cell::new(0),
            left: Cell::new(0),
        }
    }
    pub fn text(&self) -> String {
        format!("{}\n", self.lines.join("\n"))
    }
    pub fn position(&self) -> (usize, usize) {
        (self.row + 1, self.column + 1)
    }
    pub fn line_count(&self) -> usize {
        self.lines.len()
    }
    fn index(&self) -> usize {
        self.lines[self.row]
            .char_indices()
            .nth(self.column)
            .map_or(self.lines[self.row].len(), |(index, _)| index)
    }
    fn clamp(&mut self) {
        self.column = self.column.min(self.lines[self.row].chars().count());
    }
    pub fn key(&mut self, key: KeyEvent) -> Result<Event> {
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            return Ok(match key.code {
                KeyCode::Char('s') => Event::Review,
                KeyCode::Char('c') => Event::Cancel,
                KeyCode::Home => {
                    self.row = 0;
                    self.column = 0;
                    Event::Continue
                }
                KeyCode::End => {
                    self.row = self.lines.len() - 1;
                    self.column = self.lines[self.row].chars().count();
                    Event::Continue
                }
                _ => Event::Continue,
            });
        }
        match key.code {
            KeyCode::Esc => return Ok(Event::Cancel),
            KeyCode::Up => {
                self.row = self.row.saturating_sub(1);
                self.clamp();
            }
            KeyCode::Down => {
                self.row = (self.row + 1).min(self.lines.len() - 1);
                self.clamp();
            }
            KeyCode::PageUp => {
                self.row = self.row.saturating_sub(12);
                self.clamp();
            }
            KeyCode::PageDown => {
                self.row = (self.row + 12).min(self.lines.len() - 1);
                self.clamp();
            }
            KeyCode::Left => {
                if self.column > 0 {
                    self.column -= 1;
                } else if self.row > 0 {
                    self.row -= 1;
                    self.column = self.lines[self.row].chars().count();
                }
            }
            KeyCode::Right => {
                if self.column < self.lines[self.row].chars().count() {
                    self.column += 1;
                } else if self.row + 1 < self.lines.len() {
                    self.row += 1;
                    self.column = 0;
                }
            }
            KeyCode::Home => self.column = 0,
            KeyCode::End => self.column = self.lines[self.row].chars().count(),
            KeyCode::Backspace => {
                if self.column > 0 {
                    let end = self.index();
                    self.column -= 1;
                    let start = self.index();
                    self.lines[self.row].replace_range(start..end, "");
                } else if self.row > 0 {
                    let current = self.lines.remove(self.row);
                    self.row -= 1;
                    self.column = self.lines[self.row].chars().count();
                    self.lines[self.row].push_str(&current);
                }
            }
            KeyCode::Delete => {
                let at = self.index();
                if at < self.lines[self.row].len() {
                    let len = self.lines[self.row][at..]
                        .chars()
                        .next()
                        .map_or(0, char::len_utf8);
                    self.lines[self.row].replace_range(at..at + len, "");
                } else if self.row + 1 < self.lines.len() {
                    let next = self.lines.remove(self.row + 1);
                    self.lines[self.row].push_str(&next);
                }
            }
            KeyCode::Enter => self.paste("\n")?,
            KeyCode::Tab => self.paste("  ")?,
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::ALT) => {
                self.paste(&c.to_string())?
            }
            _ => {}
        }
        Ok(Event::Continue)
    }
    pub fn paste(&mut self, text: &str) -> Result<()> {
        if text.chars().any(|c| {
            !c.is_ascii() || (c != '\n' && safety::safe_text(&c.to_string(), 1).is_empty())
        }) {
            return Err("use plain text and YAML Unicode escapes; paste cannot contain controls or raw Unicode".into());
        }
        if self.text().len().saturating_add(text.len()) > MAX_DOCUMENT {
            return Err("YAML editor is limited to 64 KiB".into());
        }
        let at = self.index();
        let tail = self.lines[self.row].split_off(at);
        let added: Vec<&str> = text.split('\n').collect();
        self.lines[self.row].push_str(added[0]);
        if added.len() == 1 {
            self.column += added[0].chars().count();
            self.lines[self.row].push_str(&tail);
        } else {
            let current = self.row;
            for (index, line) in added.iter().enumerate().skip(1) {
                self.lines.insert(current + index, (*line).into());
            }
            self.row += added.len() - 1;
            self.column = self.lines[self.row].chars().count();
            self.lines[self.row].push_str(&tail);
        }
        Ok(())
    }
    pub fn viewport(&self, width: usize, height: usize) -> Vec<String> {
        let height = height.max(1);
        let gutter = self.lines.len().to_string().len() + 3;
        let content_width = width.saturating_sub(gutter).max(1);
        if self.row < self.top.get() {
            self.top.set(self.row);
        }
        if self.row >= self.top.get() + height {
            self.top.set(self.row + 1 - height);
        }
        if self.column < self.left.get() {
            self.left.set(self.column);
        }
        if self.column >= self.left.get() + content_width {
            self.left.set(self.column + 1 - content_width);
        }
        self.lines
            .iter()
            .enumerate()
            .skip(self.top.get())
            .take(height)
            .map(|(index, line)| {
                format!(
                    "{}{:>digits$} {}",
                    if index == self.row { ">" } else { " " },
                    index + 1,
                    line.chars()
                        .skip(self.left.get())
                        .take(content_width)
                        .collect::<String>(),
                    digits = gutter - 3
                )
            })
            .collect()
    }
    /// Cursor position relative to the last viewport (column, row).
    pub fn cursor(&self) -> (usize, usize) {
        (
            self.column.saturating_sub(self.left.get()) + self.lines.len().to_string().len() + 2,
            self.row.saturating_sub(self.top.get()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multiline_paste_cursor_delete_and_size_limits_preserve_buffer() -> Result<()> {
        let draft = Draft::new(
            serde_json::json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"web","namespace":"demo","uid":"u","resourceVersion":"1"},"spec":{"replicas":1}}),
            crate::gateway::kinds()
                .into_iter()
                .find(|k| k.kind == "Deployment")
                .ok_or("fixture")?,
            "x".into(),
        )?;
        let mut editor = Editor::new(draft);
        editor.key(KeyEvent::new(KeyCode::End, KeyModifiers::CONTROL))?;
        let before = editor.text();
        assert!(editor.paste("\u{1b}[2J").is_err());
        assert!(editor.paste(&"x".repeat(MAX_DOCUMENT)).is_err());
        assert_eq!(editor.text(), before);
        editor.paste("\n# note\n# two")?;
        assert!(editor.text().ends_with("# two\n"));
        editor.key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE))?;
        editor.key(KeyEvent::new(KeyCode::Backspace, KeyModifiers::NONE))?;
        assert!(editor.text().ends_with("# note# two\n"));
        assert!(matches!(
            editor.key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))?,
            Event::Review
        ));
        let viewport = editor.viewport(40, 4);
        assert!(viewport.len() <= 4);
        assert!(editor.cursor().1 < 4);
        Ok(())
    }
}
