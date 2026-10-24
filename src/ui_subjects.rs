//! `:subjects`, `:users`, `:groups`: declared RBAC subjects with their
//! bindings and roles. Enter opens the subject's `:can` evidence.
use crate::navigation_views::{SubjectIndex, SubjectRow};
use crossterm::event::KeyCode;

pub struct View {
    pub title: &'static str,
    pub namespace: String,
    index: SubjectIndex,
    selected: usize,
    filter: String,
    typing: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    /// `:can` argument for the chosen subject.
    Open(String),
}

impl View {
    pub fn new(title: &'static str, namespace: String, index: SubjectIndex) -> Self {
        Self {
            title,
            namespace,
            index,
            selected: 0,
            filter: String::new(),
            typing: false,
        }
    }
    fn visible(&self) -> Vec<&SubjectRow> {
        let filter = self.filter.to_lowercase();
        self.index
            .rows
            .iter()
            .filter(|row| {
                filter.is_empty()
                    || row.name.to_lowercase().contains(&filter)
                    || row.namespace.to_lowercase().contains(&filter)
                    || row.roles.iter().any(|role| role.to_lowercase().contains(&filter))
            })
            .collect()
    }
    pub fn key(&mut self, code: KeyCode) -> Outcome {
        if self.typing {
            match code {
                KeyCode::Esc => {
                    self.filter.clear();
                    self.typing = false;
                }
                KeyCode::Enter => self.typing = false,
                KeyCode::Backspace => {
                    self.filter.pop();
                }
                KeyCode::Char(c) if !c.is_control() && self.filter.len() < 64 => self.filter.push(c),
                _ => {}
            }
            self.selected = 0;
            return Outcome::Stay;
        }
        let count = self.visible().len();
        match code {
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.selected = 0;
            }
            KeyCode::Esc | KeyCode::Char('q') => return Outcome::Close,
            KeyCode::Char('/') => self.typing = true,
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::PageDown => self.selected = (self.selected + 10).min(count.saturating_sub(1)),
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => self.selected = count.saturating_sub(1),
            KeyCode::Enter => {
                if let Some(row) = self.visible().get(self.selected) {
                    return Outcome::Open(row.can_argument());
                }
            }
            _ => {}
        }
        Outcome::Stay
    }
    pub fn lines(&self, width: usize, height: usize) -> Vec<String> {
        let clip = |text: String| text.chars().take(width).collect::<String>();
        let rows = self.visible();
        let mut lines = vec![clip(format!(
            "{} subjects{} | namespace scope {} | declared bindings only, not an authorization decision",
            rows.len(),
            if self.index.incomplete { " [INCOMPLETE]" } else { "" },
            self.namespace
        ))];
        for error in &self.index.errors {
            lines.push(clip(format!("  unavailable: {error}")));
        }
        lines.push(clip(if self.typing {
            format!("Filter > {}_", self.filter)
        } else if self.filter.is_empty() {
            "Enter show bindings and rules | / filter | j k move | Esc back".into()
        } else {
            format!("Filter: {} | Esc clears | Enter show bindings and rules", self.filter)
        }));
        lines.push(clip(format!(
            "   {:<15} {:<34} {:<18} {:>8}  ROLES",
            "KIND", "NAME", "NAMESPACE", "BINDINGS"
        )));
        let room = height.saturating_sub(lines.len());
        let start = self.selected.saturating_sub(room.saturating_sub(1));
        for (index, row) in rows.iter().enumerate().skip(start).take(room) {
            let more = row.role_count.saturating_sub(row.roles.len());
            lines.push(clip(format!(
                "{} {:<15} {:<34} {:<18} {:>8}  {}{}",
                if index == self.selected { ">" } else { " " },
                row.kind,
                row.name.chars().take(34).collect::<String>(),
                if row.namespace.is_empty() { "-" } else { row.namespace.as_str() },
                row.bindings,
                row.roles.join(", "),
                if more > 0 { format!(" +{more}") } else { String::new() }
            )));
        }
        if rows.is_empty() {
            lines.push(if self.filter.is_empty() {
                "  No subjects are bound in this scope.".into()
            } else {
                "  No subjects match the filter.".into()
            });
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn view() -> View {
        let bindings = vec![serde_json::json!({"metadata":{"namespace":"shop"},"roleRef":{"kind":"ClusterRole","name":"view"},"subjects":[{"kind":"User","name":"alice"},{"kind":"Group","name":"devs"},{"kind":"ServiceAccount","name":"api"}]})];
        View::new(
            "RBAC SUBJECTS",
            "shop".into(),
            SubjectIndex {
                rows: crate::navigation_views::subject_rows(&bindings, None),
                incomplete: false,
                errors: Vec::new(),
            },
        )
    }
    #[test]
    fn filter_narrows_rows_and_enter_opens_can_for_the_selection() {
        let mut view = view();
        assert!(view.lines(120, 20).join("\n").contains("3 subjects"));
        view.key(KeyCode::Char('/'));
        for c in "dev".chars() {
            view.key(KeyCode::Char(c));
        }
        view.key(KeyCode::Enter);
        assert!(view.lines(120, 20).join("\n").contains("1 subjects"));
        assert_eq!(view.key(KeyCode::Enter), Outcome::Open("g:devs".into()));
        view.key(KeyCode::Esc);
        view.key(KeyCode::Char('j'));
        view.key(KeyCode::Char('j'));
        assert_eq!(view.key(KeyCode::Enter), Outcome::Open("u:alice".into()));
        assert_eq!(view.key(KeyCode::Esc), Outcome::Close);
    }
}
