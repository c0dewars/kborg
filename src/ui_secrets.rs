//! `:secrets` view: values stay hidden until one key is explicitly revealed,
//! and a revealed value is dropped after REVEAL_SECONDS or on any navigation.
//! `y`/`d` open a masked YAML of one Secret in which `x` reveals one key inline.
use crate::secrets::{MaskedSecret, Revealed, SecretIndex};
use crossterm::event::KeyCode;
use std::cell::Cell;
use std::time::{Duration, Instant};

pub const REVEAL_SECONDS: u64 = 30;

/// Which part of the view has focus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    List,
    Keys,
    Yaml,
}

struct YamlView {
    doc: MaskedSecret,
    key: usize,
    /// First painted body line; `None` keeps the selected key in view.
    offset: Option<usize>,
}

pub struct View {
    pub index: SecretIndex,
    selected: usize,
    /// Secret whose keys are listed, with the selected key row.
    open: Option<(usize, usize)>,
    yaml: Option<YamlView>,
    /// UID of the Secret whose masked YAML was requested.
    yaml_pending: Option<String>,
    revealed: Option<(String, String, Revealed, Instant)>,
    pending: Option<(String, String)>,
    pub notice: String,
    /// First body line and body height of the last paint, for paging.
    painted: Cell<(usize, usize)>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    Stay,
    Close,
    /// namespace, name, uid, key
    Reveal(String, String, String, String),
    /// namespace, name, uid: fetch the masked YAML
    Yaml(String, String, String),
}

impl View {
    pub fn new(index: SecretIndex) -> Self {
        Self {
            index,
            selected: 0,
            open: None,
            yaml: None,
            yaml_pending: None,
            revealed: None,
            pending: None,
            notice: String::new(),
            painted: Cell::new((0, 1)),
        }
    }
    pub fn mode(&self) -> Mode {
        if self.yaml.is_some() {
            Mode::Yaml
        } else if self.open.is_some() {
            Mode::Keys
        } else {
            Mode::List
        }
    }
    /// Drops any revealed value (its bytes are overwritten on drop).
    pub fn hide(&mut self) {
        self.revealed = None;
        self.pending = None;
    }
    fn expired(&self) -> bool {
        self.revealed
            .as_ref()
            .is_some_and(|(.., at)| at.elapsed() >= Duration::from_secs(REVEAL_SECONDS))
    }
    /// Hides an expired value; `true` when the view changed.
    pub fn tick(&mut self) -> bool {
        if self.expired() {
            self.revealed = None;
            self.notice = format!("Revealed value hidden after {REVEAL_SECONDS} seconds.");
            return true;
        }
        false
    }
    /// uid, Secret name and key of the selected key row.
    fn current_key(&self) -> Option<(&str, &str, &str)> {
        if let Some(yaml) = &self.yaml {
            let (key, _) = yaml.doc.keys.get(yaml.key)?;
            return Some((&yaml.doc.uid, &yaml.doc.name, key));
        }
        let (secret, key) = self.open?;
        let entry = self.index.items.get(secret)?;
        entry
            .keys
            .get(key)
            .map(|(name, _)| (entry.uid.as_str(), entry.name.as_str(), name.as_str()))
    }
    /// Toggles the reveal of the selected key.
    fn toggle_reveal(&mut self) -> Outcome {
        let Some((uid, name, key)) = self.current_key() else {
            return Outcome::Stay;
        };
        let (uid, name, key) = (uid.to_owned(), name.to_owned(), key.to_owned());
        if self
            .revealed
            .as_ref()
            .is_some_and(|(u, k, ..)| (u, k) == (&uid, &key))
        {
            self.hide();
            self.notice = "Value hidden.".into();
            return Outcome::Stay;
        }
        self.hide();
        self.pending = Some((uid.clone(), key.clone()));
        self.notice = "Reading one key...".into();
        Outcome::Reveal(self.index.namespace.clone(), name, uid, key)
    }
    fn request_yaml(&mut self, secret: usize) -> Outcome {
        let Some(entry) = self.index.items.get(secret) else {
            return Outcome::Stay;
        };
        let outcome = Outcome::Yaml(
            self.index.namespace.clone(),
            entry.name.clone(),
            entry.uid.clone(),
        );
        self.yaml_pending = Some(entry.uid.clone());
        self.hide();
        self.notice = "Reading masked YAML...".into();
        outcome
    }
    pub fn key(&mut self, code: KeyCode) -> Outcome {
        self.tick();
        if self.yaml.is_some() {
            let (start, page) = self.painted.get();
            let page = page.max(1);
            let move_key = |view: &mut Self, down: bool| {
                view.hide();
                if let Some(yaml) = view.yaml.as_mut() {
                    yaml.key = if down {
                        (yaml.key + 1).min(yaml.doc.keys.len().saturating_sub(1))
                    } else {
                        yaml.key.saturating_sub(1)
                    };
                    yaml.offset = None;
                }
            };
            match code {
                KeyCode::Esc | KeyCode::Char('q') => {
                    self.hide();
                    self.yaml = None;
                    self.notice.clear();
                }
                KeyCode::Down | KeyCode::Char('j') => move_key(self, true),
                KeyCode::Up | KeyCode::Char('k') => move_key(self, false),
                KeyCode::PageDown | KeyCode::PageUp => {
                    if let Some(yaml) = self.yaml.as_mut() {
                        yaml.offset = Some(if code == KeyCode::PageDown {
                            start + page
                        } else {
                            start.saturating_sub(page)
                        });
                    }
                }
                KeyCode::Enter | KeyCode::Char('x') => return self.toggle_reveal(),
                _ => {}
            }
            return Outcome::Stay;
        }
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
            (None, KeyCode::Char('y' | 'd')) => return self.request_yaml(self.selected),
            (Some((secret, _)), KeyCode::Char('y' | 'd')) => return self.request_yaml(secret),
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
            (Some(_), KeyCode::Enter | KeyCode::Char('x')) => return self.toggle_reveal(),
            _ => {}
        }
        Outcome::Stay
    }
    /// Applies a masked YAML only if it is still the one requested.
    pub fn yaml(&mut self, uid: &str, result: crate::Result<MaskedSecret>) {
        if self.yaml_pending.as_deref() != Some(uid) {
            return;
        }
        self.yaml_pending = None;
        match result {
            Ok(doc) if doc.uid == uid => {
                self.hide();
                self.yaml = Some(YamlView {
                    doc,
                    key: 0,
                    offset: None,
                });
                self.notice.clear();
            }
            Ok(_) => self.notice = "Secret was replaced; reopen :secrets".into(),
            Err(error) => self.notice = error,
        }
    }
    /// Applies a reveal only if it still matches the selected key.
    pub fn reveal(&mut self, uid: &str, key: &str, result: crate::Result<Revealed>) {
        let expected = self.pending.take();
        if expected.as_ref().map(|(u, k)| (u.as_str(), k.as_str())) != Some((uid, key))
            || self.current_key().map(|(u, _, k)| (u, k)) != Some((uid, key))
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
    #[cfg(test)]
    fn age_reveal(&mut self, seconds: u64) {
        if let Some((.., at)) = self.revealed.as_mut()
            && let Some(earlier) = at.checked_sub(Duration::from_secs(seconds))
        {
            *at = earlier;
        }
    }
    /// The revealed value for `uid`/`key`, while it is still valid.
    fn shown(&self, uid: &str, key: &str) -> Option<&Revealed> {
        self.revealed
            .as_ref()
            .filter(|(u, k, ..)| u == uid && k == key && !self.expired())
            .map(|(.., value, _)| value)
    }
    fn yaml_lines(&self, yaml: &YamlView, width: usize, height: usize) -> Vec<String> {
        let clip = |text: &str| text.chars().take(width).collect::<String>();
        let mut body = Vec::new();
        let mut selected_line = 0;
        let mut key_lines = yaml.doc.keys.iter().enumerate().peekable();
        for (number, line) in yaml.doc.lines.iter().enumerate() {
            let Some((index, (key, _))) = key_lines.next_if(|(_, (_, at))| *at == number) else {
                body.push(format!("  {line}"));
                continue;
            };
            let marker = if index == yaml.key { ">" } else { " " };
            if index == yaml.key {
                selected_line = body.len();
            }
            match self.shown(&yaml.doc.uid, key) {
                Some(Revealed::Text(text)) => {
                    body.push(format!(
                        "{marker}   {key}: |-  # REVEALED for {REVEAL_SECONDS}s; x hides"
                    ));
                    body.extend(text.lines().map(|line| format!("      {line}")));
                }
                Some(Revealed::Binary(n, hex)) => body.push(format!(
                    "{marker}   {key}: \"<binary: {n} bytes>\"  # REVEALED hex: {hex}"
                )),
                Some(Revealed::TooLarge(n)) => body.push(format!(
                    "{marker}   {key}: \"<{n} bytes exceeds the display limit>\""
                )),
                None => body.push(format!("{marker} {line}")),
            }
        }
        let mut lines = vec![
            clip(&format!(
                "Secret {}/{} | masked YAML | values hidden",
                yaml.doc.namespace, yaml.doc.name
            )),
            clip(&format!(
                "x reveals the selected key for {REVEAL_SECONDS}s / j k select key / Esc back. Never exported or copied."
            )),
        ];
        let notice = if self.notice.is_empty() { 0 } else { 2 };
        let page = height.saturating_sub(lines.len() + notice).max(1);
        let last = body.len().saturating_sub(page);
        let start = match yaml.offset {
            Some(offset) => offset.min(last),
            None => {
                let (previous, _) = self.painted.get();
                if selected_line < previous {
                    selected_line
                } else if selected_line >= previous + page {
                    selected_line + 1 - page
                } else {
                    previous.min(last)
                }
            }
        };
        self.painted.set((start, page));
        lines.extend(body.iter().skip(start).take(page).map(|line| clip(line)));
        lines
    }
    pub fn lines(&self, width: usize, height: usize) -> Vec<String> {
        let clip = |text: &str| text.chars().take(width).collect::<String>();
        let mut lines = Vec::new();
        if let Some(yaml) = &self.yaml {
            lines = self.yaml_lines(yaml, width, height);
        } else {
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
                    lines.push(clip("Enter keys / y masked YAML / Esc close"));
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
                        "x reveal one key for {REVEAL_SECONDS}s / x again hides / y masked YAML / Esc back. Values are not exported or copied."
                    )));
                    lines.push(String::new());
                    for (i, (key, size)) in entry.keys.iter().enumerate() {
                        let marker = if i == selected { ">" } else { " " };
                        match self.shown(&entry.uid, key) {
                            Some(Revealed::Text(text)) => {
                                lines.push(clip(&format!("{marker} {key} ({size} bytes):")));
                                lines.extend(text.lines().map(|line| clip(&format!("    {line}"))));
                            }
                            Some(Revealed::Binary(n, hex)) => lines.push(clip(&format!(
                                "{marker} {key}: binary, {n} bytes; hex {hex}"
                            ))),
                            Some(Revealed::TooLarge(n)) => lines.push(clip(&format!(
                                "{marker} {key}: {n} bytes exceeds the display limit"
                            ))),
                            None => lines
                                .push(clip(&format!("{marker} {key}: [hidden] ({size} bytes)"))),
                        }
                    }
                    if entry.keys.is_empty() {
                        lines.push(clip("  This Secret has no data keys."));
                    }
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
    fn demo_yaml(view: &mut View) -> MaskedSecret {
        let mut secret = crate::secrets::demo_secret("demo");
        secret.metadata.uid = Some("u1".into());
        secret.metadata.name = Some("api".into());
        let doc = crate::secrets::masked(&secret);
        assert_eq!(
            view.key(KeyCode::Char('y')),
            Outcome::Yaml("demo".into(), "api".into(), "u1".into())
        );
        doc
    }
    #[test]
    fn yaml_view_masks_values_and_reveals_one_key_bound_to_its_uid() {
        let mut view = view();
        let doc = demo_yaml(&mut view);
        // A reply for another UID (a replaced Secret) is ignored.
        let mut other = doc.clone();
        other.uid = "u2".into();
        view.yaml("u2", Ok(other));
        assert_eq!(view.mode(), Mode::List);
        view.yaml("u1", Ok(doc));
        assert_eq!(view.mode(), Mode::Yaml);
        let text = view.lines(120, 40).join("\n");
        assert!(text.contains("kind: Secret") && text.contains("password: \"<hidden: 19 bytes>\""));
        assert!(!text.contains("not-a-real-password") && !text.contains("demo-user"));
        // Keys are sorted: blob.bin, ca.crt, password, username.
        view.key(KeyCode::Down);
        view.key(KeyCode::Down);
        assert_eq!(
            view.key(KeyCode::Char('x')),
            Outcome::Reveal("demo".into(), "api".into(), "u1".into(), "password".into())
        );
        // A value for a different UID or key never paints.
        view.reveal("u2", "password", Ok(Revealed::Text("wrong-uid".into())));
        assert!(!view.lines(120, 40).join("\n").contains("wrong-uid"));
        view.key(KeyCode::Char('x'));
        view.reveal(
            "u1",
            "password",
            Ok(crate::secrets::reveal(b"not-a-real-password")),
        );
        let text = view.lines(120, 40).join("\n");
        assert!(text.contains("REVEALED") && text.contains("not-a-real-password"));
        assert!(!text.contains("demo-user"), "only one key is revealed");
        // Expiry hides it; tick reports the change for a repaint.
        view.age_reveal(REVEAL_SECONDS);
        assert!(
            !view
                .lines(120, 40)
                .join("\n")
                .contains("not-a-real-password")
        );
        assert!(view.tick());
        assert!(view.revealed.is_none() && view.notice.contains("30 seconds"));
        // Binary values show a hex preview; Esc drops the value and the YAML.
        view.key(KeyCode::Up);
        view.key(KeyCode::Up);
        view.key(KeyCode::Char('x'));
        view.reveal(
            "u1",
            "blob.bin",
            Ok(crate::secrets::reveal(&[0, 159, 146, 150])),
        );
        assert!(view.lines(120, 40).join("\n").contains("hex: 00 9f 92 96"));
        assert_eq!(view.key(KeyCode::Esc), Outcome::Stay);
        assert_eq!(view.mode(), Mode::List);
        assert!(view.revealed.is_none() && view.yaml.is_none());
    }
    #[test]
    fn yaml_from_key_list_and_errors_stay_in_place() {
        let mut view = view();
        view.key(KeyCode::Enter);
        assert_eq!(
            view.key(KeyCode::Char('d')),
            Outcome::Yaml("demo".into(), "api".into(), "u1".into())
        );
        view.yaml("u1", Err("Secret was replaced; reopen :secrets".into()));
        assert_eq!(view.mode(), Mode::Keys);
        assert!(view.lines(120, 40).join("\n").contains("replaced"));
        // Unrequested replies are ignored.
        view.yaml(
            "u1",
            Ok(crate::secrets::masked(&crate::secrets::demo_secret("demo"))),
        );
        assert_eq!(view.mode(), Mode::Keys);
    }
    #[test]
    fn long_yaml_pages_and_follows_the_selected_key() {
        let mut view = view();
        let mut doc = demo_yaml(&mut view);
        let extra: Vec<String> = (0..50).map(|i| format!("# filler {i}")).collect();
        let shift = extra.len();
        doc.lines.splice(0..0, extra);
        for (_, line) in &mut doc.keys {
            *line += shift;
        }
        view.yaml("u1", Ok(doc));
        let first = view.lines(120, 12).join("\n");
        assert!(
            first.contains("> ") && first.contains("blob.bin"),
            "{first}"
        );
        view.key(KeyCode::PageUp);
        assert!(view.lines(120, 12).join("\n").contains("filler"));
        view.key(KeyCode::Down);
        assert!(view.lines(120, 12).join("\n").contains("> "));
    }
}
