//! `:secrets` view: values stay hidden until one key is explicitly revealed,
//! and a revealed value is dropped after REVEAL_SECONDS or on any navigation.
use crate::secrets::{Revealed, SecretIndex};
use crossterm::event::KeyCode;
use std::time::{Duration, Instant};

pub const REVEAL_SECONDS: u64 = 30;

pub struct View {
    pub index: SecretIndex,
    selected: usize,
    /// Secret whose keys are listed, with the selected key row.
    open: Option<(usize, usize)>,
    revealed: Option<(String, String, Revealed, Instant)>,
    pending: Option<(String, String)>,
    pub notice: String,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    /// namespace, name, uid, key
    Reveal(String, String, String, String),
}

impl View {
    pub fn new(index: SecretIndex) -> Self {
        Self {
            index,
            selected: 0,
            open: None,
            revealed: None,
            pending: None,
            notice: String::new(),
        }
    }
    fn hide(&mut self) {
        self.revealed = None;
        self.pending = None;
    }
    fn expired(&self) -> bool {
        self.revealed
            .as_ref()
            .is_some_and(|(.., at)| at.elapsed() >= Duration::from_secs(REVEAL_SECONDS))
    }
    pub fn tick(&mut self) {
        if self.expired() {
            self.revealed = None;
            self.notice = "Revealed value hidden after 30 seconds.".into();
        }
    }
    fn current_key(&self) -> Option<(&crate::secrets::SecretEntry, &str)> {
        let (secret, key) = self.open?;
        let entry = self.index.items.get(secret)?;
        entry.keys.get(key).map(|(name, _)| (entry, name.as_str()))
    }
    pub fn key(&mut self, code: KeyCode) -> Outcome {
        self.tick();
        match (self.open, code) {
            (None, KeyCode::Esc | KeyCode::Char('q')) => return Outcome::Close,
            (None, KeyCode::Down | KeyCode::Char('j')) => {
                self.selected = (self.selected + 1).min(self.index.items.len().saturating_sub(1));
            }
            (None, KeyCode::Up | KeyCode::Char('k')) => {
                self.selected = self.selected.saturating_sub(1)
            }
            (None, KeyCode::Enter | KeyCode::Char('x')) => {
                if self.index.items.get(self.selected).is_some() {
                    self.open = Some((self.selected, 0));
                }
            }
            (Some(_), KeyCode::Esc | KeyCode::Char('q')) => {
                self.hide();
                self.open = None;
            }
            (Some((secret, key)), KeyCode::Down | KeyCode::Char('j')) => {
                self.hide();
                let last = self
                    .index
                    .items
                    .get(secret)
                    .map_or(0, |entry| entry.keys.len().saturating_sub(1));
                self.open = Some((secret, (key + 1).min(last)));
            }
            (Some((secret, key)), KeyCode::Up | KeyCode::Char('k')) => {
                self.hide();
                self.open = Some((secret, key.saturating_sub(1)));
            }
            (Some(_), KeyCode::Enter | KeyCode::Char('x')) => {
                let Some((entry, key)) = self.current_key() else {
                    return Outcome::Stay;
                };
                let target = (entry.uid.clone(), key.to_owned());
                if self
                    .revealed
                    .as_ref()
                    .is_some_and(|(uid, k, ..)| (uid, k) == (&target.0, &target.1))
                {
                    self.hide();
                    self.notice = "Value hidden.".into();
                    return Outcome::Stay;
                }
                let outcome = Outcome::Reveal(
                    self.index.namespace.clone(),
                    entry.name.clone(),
                    target.0.clone(),
                    target.1.clone(),
                );
                self.hide();
                self.pending = Some(target);
                self.notice = "Reading one key...".into();
                return outcome;
            }
            _ => {}
        }
        Outcome::Stay
    }
    /// Applies a reveal only if it still matches the selected key.
    pub fn reveal(&mut self, uid: &str, key: &str, result: crate::Result<Revealed>) {
        let expected = self.pending.take();
        if expected.as_ref().map(|(u, k)| (u.as_str(), k.as_str())) != Some((uid, key))
            || self.current_key().map(|(entry, k)| (entry.uid.as_str(), k)) != Some((uid, key))
        {
            return;
        }
        match result {
            Ok(value) => {
                self.revealed = Some((uid.into(), key.into(), value, Instant::now()));
                self.notice = format!("Revealed for {REVEAL_SECONDS}s; x hides it now.");
            }
            Err(error) => self.notice = error,
        }
    }
    pub fn lines(&self, width: usize, height: usize) -> Vec<String> {
        let clip = |text: &str| text.chars().take(width).collect::<String>();
        let mut lines = Vec::new();
        match self
            .open
            .and_then(|(secret, key)| Some((self.index.items.get(secret)?, key)))
        {
            None => {
                lines.push(clip(&format!(
                    "Namespace {} | {} Secrets{} | values are never listed",
                    self.index.namespace,
                    self.index.items.len(),
                    if self.index.truncated {
                        " [TRUNCATED]"
                    } else {
                        ""
                    }
                )));
                lines.push(clip("Enter keys / Esc close"));
                lines.push(clip(
                    "   NAME                               TYPE                         KEYS",
                ));
                for (i, entry) in self
                    .index
                    .items
                    .iter()
                    .enumerate()
                    .skip(self.selected.saturating_sub(height.saturating_sub(4)))
                {
                    lines.push(clip(&format!(
                        "{} {:<34} {:<28} {}",
                        if i == self.selected { ">" } else { " " },
                        entry.name,
                        entry.secret_type,
                        entry.keys.len()
                    )));
                }
                if self.index.items.is_empty() {
                    lines.push(clip("  No Secrets in this namespace."));
                }
            }
            Some((entry, selected)) => {
                lines.push(clip(&format!(
                    "Secret {}/{} | {}",
                    self.index.namespace, entry.name, entry.secret_type
                )));
                lines.push(clip(&format!(
                    "x reveal one key for {REVEAL_SECONDS}s / x again hides / Esc back. Values are not exported or copied."
                )));
                lines.push(String::new());
                let shown = self
                    .revealed
                    .as_ref()
                    .filter(|(uid, ..)| *uid == entry.uid && !self.expired());
                for (i, (key, size)) in entry.keys.iter().enumerate() {
                    let marker = if i == selected { ">" } else { " " };
                    match shown.filter(|(_, k, ..)| k == key) {
                        Some((_, _, Revealed::Text(text), _)) => {
                            lines.push(clip(&format!("{marker} {key} ({size} bytes):")));
                            lines.extend(text.lines().map(|line| clip(&format!("    {line}"))));
                        }
                        Some((_, _, Revealed::Binary(n), _)) => lines.push(clip(&format!(
                            "{marker} {key}: binary, {n} bytes; not displayed"
                        ))),
                        Some((_, _, Revealed::TooLarge(n), _)) => lines.push(clip(&format!(
                            "{marker} {key}: {n} bytes exceeds the display limit"
                        ))),
                        None => {
                            lines.push(clip(&format!("{marker} {key}: [hidden] ({size} bytes)")))
                        }
                    }
                }
                if entry.keys.is_empty() {
                    lines.push(clip("  This Secret has no data keys."));
                }
            }
        }
        if !self.notice.is_empty() {
            lines.push(String::new());
            lines.push(clip(&self.notice));
        }
        lines.truncate(height);
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn view() -> View {
        let data = crate::secrets::demo_data();
        View::new(SecretIndex {
            namespace: "demo".into(),
            items: vec![crate::secrets::entry("api", "u1", None, Some(&data))],
            truncated: false,
        })
    }
    #[test]
    fn values_stay_hidden_until_one_key_is_revealed_and_toggle_off() {
        let mut view = view();
        assert!(!view.lines(100, 30).join("\n").contains("demo-user"));
        assert_eq!(view.key(KeyCode::Enter), Outcome::Stay);
        // Keys are sorted: blob.bin, ca.crt, password, username.
        for _ in 0..3 {
            view.key(KeyCode::Down);
        }
        assert_eq!(
            view.key(KeyCode::Char('x')),
            Outcome::Reveal("demo".into(), "api".into(), "u1".into(), "username".into())
        );
        view.reveal("u1", "username", Ok(Revealed::Text("demo-user".into())));
        let text = view.lines(100, 30).join("\n");
        assert!(text.contains("demo-user") && text.contains("password: [hidden]"));
        assert_eq!(view.key(KeyCode::Char('x')), Outcome::Stay);
        assert!(!view.lines(100, 30).join("\n").contains("demo-user"));
    }
    #[test]
    fn moving_or_late_replies_never_show_a_value() {
        let mut view = view();
        view.key(KeyCode::Enter);
        view.key(KeyCode::Char('x'));
        view.key(KeyCode::Down);
        view.reveal("u1", "blob.bin", Ok(Revealed::Text("late".into())));
        assert!(!view.lines(100, 30).join("\n").contains("late"));
        view.key(KeyCode::Char('x'));
        view.reveal("u1", "ca.crt", Ok(Revealed::Text("cert".into())));
        assert!(view.lines(100, 30).join("\n").contains("cert"));
        view.key(KeyCode::Esc);
        assert!(!view.lines(100, 30).join("\n").contains("cert"));
        assert_eq!(view.key(KeyCode::Esc), Outcome::Close);
    }
}
