//! k9s-compatible data-only hotkeys.yaml. A hotkey types a KBorg command, so
//! it reaches the same parser, aliases and review gates as `:` input.
use crate::{Result, safety};
use crossterm::event::{KeyCode, KeyModifiers};
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(rename = "hotKeys", alias = "hotkeys")]
    hot_keys: BTreeMap<String, Entry>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Entry {
    short_cut: String,
    #[serde(default)]
    description: String,
    command: String,
    /// Accepted for k9s files; KBorg history is unchanged either way.
    #[serde(default)]
    #[allow(dead_code)]
    keep_history: bool,
    #[serde(default, rename = "override")]
    #[allow(dead_code)]
    overrides: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Shortcut {
    Shift(u8),
    Alt(char),
    Ctrl(char),
    F(u8),
}

/// US-layout characters terminals send for Shift-0 … Shift-9.
const SHIFTED_DIGITS: &[u8; 10] = b")!@#$%^&*(";
/// Ctrl keys that KBorg or the terminal already use.
const RESERVED_CTRL: &str = "acdefhijkmnpqrsuwz";

impl Shortcut {
    pub fn parse(text: &str) -> Option<Self> {
        let (prefix, key) = text.split_once('-').unwrap_or(("", text));
        let mut chars = key.chars();
        let single = chars.next().filter(|_| chars.next().is_none());
        match (prefix.to_ascii_lowercase().as_str(), single) {
            ("shift", Some(digit @ '0'..='9')) => Some(Self::Shift(digit as u8 - b'0')),
            ("alt", Some(c)) if c.is_ascii_alphanumeric() => {
                Some(Self::Alt(c.to_ascii_lowercase()))
            }
            ("ctrl", Some(c))
                if c.is_ascii_alphabetic() && !RESERVED_CTRL.contains(c.to_ascii_lowercase()) =>
            {
                Some(Self::Ctrl(c.to_ascii_lowercase()))
            }
            ("", _) => {
                let number = key.strip_prefix(['F', 'f'])?.parse::<u8>().ok()?;
                (1..=12).contains(&number).then_some(Self::F(number))
            }
            _ => None,
        }
    }

    pub fn matches(self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        match (self, code) {
            (Self::Shift(digit), KeyCode::Char(c)) => {
                c == char::from(SHIFTED_DIGITS[usize::from(digit)])
                    || (c == char::from(b'0' + digit) && modifiers.contains(KeyModifiers::SHIFT))
            }
            (Self::Alt(key), KeyCode::Char(c)) => {
                modifiers.contains(KeyModifiers::ALT) && c.to_ascii_lowercase() == key
            }
            (Self::Ctrl(key), KeyCode::Char(c)) => {
                modifiers.contains(KeyModifiers::CONTROL) && c.to_ascii_lowercase() == key
            }
            (Self::F(number), KeyCode::F(pressed)) => number == pressed,
            _ => false,
        }
    }
}

impl std::fmt::Display for Shortcut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shift(digit) => write!(f, "Shift-{digit}"),
            Self::Alt(key) => write!(f, "Alt-{key}"),
            Self::Ctrl(key) => write!(f, "Ctrl-{key}"),
            Self::F(number) => write!(f, "F{number}"),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hotkey {
    pub shortcut: Shortcut,
    pub description: String,
    pub command: String,
}
pub type Hotkeys = Vec<Hotkey>;

fn parse(text: &str) -> Result<Hotkeys> {
    if text.len() > 64 * 1024 {
        return Err("hotkeys.yaml exceeds 64 KiB".into());
    }
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_reader_input_bytes: Some(64*1024), max_events: 5000, max_aliases: 0, max_anchors: 0,
            max_depth: 4, max_inclusion_depth: 0, max_documents: 1, max_nodes: 1024,
            max_total_scalar_bytes: 64*1024, max_total_comment_bytes: 64*1024, max_merge_keys: 0,
        },
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys: serde_saphyr::MergeKeyPolicy::Error,
        with_snippet: false,
    };
    let file: File = serde_saphyr::from_str_with_options(text, options).map_err(|_| {
        "invalid hotkeys.yaml; expected hotKeys: entries with shortCut, description and command"
    })?;
    if file.hot_keys.len() > 64 {
        return Err("hotkeys.yaml exceeds 64 hotkeys".into());
    }
    let mut hotkeys = Hotkeys::new();
    for (name, entry) in file.hot_keys {
        let shortcut = Shortcut::parse(entry.short_cut.trim()).ok_or_else(|| {
            format!(
                "hotkey {}: shortCut must be Shift-0..9, Alt-<letter or digit>, F1..F12 or a free Ctrl-<letter>",
                safety::safe_text(&name, 63)
            )
        })?;
        if hotkeys.iter().any(|hotkey| hotkey.shortcut == shortcut) {
            return Err(format!("hotkeys.yaml binds {shortcut} twice"));
        }
        let command = entry
            .command
            .trim()
            .trim_start_matches(':')
            .trim()
            .to_owned();
        if command.is_empty()
            || command.len() > 2048
            || safety::safe_text(&command, 2048) != command
        {
            return Err("hotkey commands must be bounded single-line plain text".into());
        }
        if entry.description.len() > 120
            || safety::safe_text(&entry.description, 120) != entry.description
        {
            return Err(
                "hotkey descriptions must be single-line plain text up to 120 bytes".into(),
            );
        }
        hotkeys.push(Hotkey {
            shortcut,
            description: entry.description,
            command,
        });
    }
    hotkeys.sort_by_key(|hotkey| hotkey.shortcut);
    Ok(hotkeys)
}

pub fn load() -> Result<Hotkeys> {
    let path = safety::config_dir()?.join("hotkeys.yaml");
    safety::checked_path(&path)?;
    if !path.exists() {
        return Ok(Hotkeys::new());
    }
    let bytes = safety::read_small(&path, 64 * 1024)?;
    parse(std::str::from_utf8(&bytes).map_err(|_| "hotkeys.yaml must be UTF-8")?)
}

pub fn find(hotkeys: &[Hotkey], code: KeyCode, modifiers: KeyModifiers) -> Option<&Hotkey> {
    hotkeys
        .iter()
        .find(|hotkey| hotkey.shortcut.matches(code, modifiers))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn k9s_hotkeys_map_keys_to_commands() -> Result<()> {
        let hotkeys = parse(
            "hotKeys:\n  shift-0:\n    shortCut: Shift-0\n    description: Viewing pods\n    command: pods\n    keepHistory: true\n  deploys:\n    shortCut: F3\n    description: Deployments\n    command: ':dp kube-system'\n  nodes:\n    shortCut: alt-n\n    command: no\n",
        )?;
        assert_eq!(
            hotkeys
                .iter()
                .map(|hotkey| (hotkey.shortcut.to_string(), hotkey.command.as_str()))
                .collect::<Vec<_>>(),
            vec![
                ("Shift-0".to_owned(), "pods"),
                ("Alt-n".to_owned(), "no"),
                ("F3".to_owned(), "dp kube-system"),
            ]
        );
        let none = KeyModifiers::NONE;
        assert_eq!(
            find(&hotkeys, KeyCode::Char(')'), none).map(|hotkey| hotkey.command.as_str()),
            Some("pods")
        );
        assert!(find(&hotkeys, KeyCode::Char('0'), KeyModifiers::SHIFT).is_some());
        assert!(find(&hotkeys, KeyCode::Char('0'), none).is_none());
        assert!(find(&hotkeys, KeyCode::Char('n'), KeyModifiers::ALT).is_some());
        assert!(find(&hotkeys, KeyCode::Char('n'), none).is_none());
        assert!(find(&hotkeys, KeyCode::F(3), none).is_some());
        Ok(())
    }
    #[test]
    fn unsafe_or_conflicting_hotkeys_are_rejected() {
        for yaml in [
            "hotKeys:\n  a:\n    shortCut: Ctrl-d\n    command: pods",
            "hotKeys:\n  a:\n    shortCut: F13\n    command: pods",
            "hotKeys:\n  a:\n    shortCut: Shift-a\n    command: pods",
            "hotKeys:\n  a:\n    shortCut: F1\n    command: pods\n  b:\n    shortCut: f1\n    command: no",
            "hotKeys:\n  a:\n    shortCut: F1\n    command: \"pods\\u001b[2J\"",
            "hotKeys:\n  a:\n    shortCut: F1\n    command: ' '",
            "hotKeys:\n  a:\n    shortCut: F1\n    command: pods\n    shell: true",
            "hotKeys:\n  a: &k\n    shortCut: F1\n    command: pods\n  b: *k",
        ] {
            assert!(parse(yaml).is_err(), "{yaml}");
        }
    }
}
