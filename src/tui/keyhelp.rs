//! Generated help, keymap listing and hint rows. Everything here comes from
//! the keymap registry with the user's effective keys, so a remapped key is
//! shown where it works and the replaced key disappears.
use super::*;
use crate::keymap::{self, Action, Key, Scope, Translation};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Mode {
    /// Keys of the view help was opened from.
    Context,
    /// Every key of every view plus the command reference.
    All,
    /// `:keymap`: one row per action with its keys and scope.
    Keymap,
}

pub(super) struct HelpView {
    pub(super) scope: Scope,
    pub(super) mode: Mode,
    pub(super) offset: usize,
    pub(super) filter: String,
    pub(super) typing: bool,
}

/// The view whose keys `?` explains when pressed in `modal`.
pub(super) fn help_scope(modal: &Modal) -> Option<Scope> {
    match modal {
        Modal::LiveOverview(..) | Modal::YamlEditor(..) | Modal::Help(..) => None,
        modal => translated_scope(modal),
    }
}

/// Views whose keys go through keymap translation; `None` keeps the raw key
/// (table keys dispatch by name, prompts and reviews keep fixed keys).
pub(super) fn translated_scope(modal: &Modal) -> Option<Scope> {
    Some(match modal {
        Modal::Follow(view) if !view.is_filtering() => Scope::Log,
        Modal::LiveOverview(dashboard) if dashboard.picker.is_none() => Scope::Dashboard,
        Modal::ForwardView => Scope::Forward,
        Modal::PodContainers(..) => Scope::Containers,
        Modal::Detail(..)
        | Modal::HelmDocument(..)
        | Modal::AdviceDocument(..)
        | Modal::HelmOutcome(..) => Scope::Detail,
        Modal::Overview(..) => Scope::Overview,
        Modal::Metrics(..) => Scope::Metrics,
        Modal::YamlEditor(..) => Scope::Editor,
        Modal::Help(view) if !view.typing => Scope::Help,
        _ => return None,
    })
}

impl App {
    pub(super) fn open_help(&mut self, scope: Scope, mode: Mode) {
        let modal = std::mem::replace(&mut self.modal, Modal::None);
        if !matches!(modal, Modal::None) {
            self.push_subview(modal);
        }
        self.modal = Modal::Help(Box::new(HelpView {
            scope,
            mode,
            offset: 0,
            filter: String::new(),
            typing: false,
        }));
    }
    /// Remapped keys become the built-in key of their action; built-in keys
    /// that keymap.yaml moved away are dropped (`None`).
    pub(super) fn translate_key(
        &self,
        code: KeyCode,
        modifiers: KeyModifiers,
    ) -> Option<(KeyCode, KeyModifiers)> {
        let (Some(scope), Some(key)) = (
            translated_scope(&self.modal),
            Key::from_event(code, modifiers),
        ) else {
            return Some((code, modifiers));
        };
        match self.keymap.translate(scope, key) {
            Translation::Pass => Some((code, modifiers)),
            Translation::To(target) => Some(target.event()),
            Translation::Swallow => None,
        }
    }
    /// Sends the user's help key, so mouse clicks behave like the keyboard.
    pub(super) fn press_help(&mut self) -> Result<bool> {
        match self.keymap.keys("help").first() {
            Some(key) => {
                let (code, modifiers) = key.event();
                self.key_event(code, modifiers)
            }
            None => {
                self.open_help(Scope::Table, Mode::Context);
                Ok(false)
            }
        }
    }
}

/// Handles a key inside help; `true` closes it.
pub(super) fn help_key(app: &App, view: &mut HelpView, code: KeyCode) -> bool {
    if view.typing {
        match code {
            KeyCode::Esc => {
                view.typing = false;
                view.filter.clear();
            }
            KeyCode::Enter => view.typing = false,
            KeyCode::Backspace => {
                view.filter.pop();
            }
            KeyCode::Char(c) if !c.is_control() && view.filter.len() + c.len_utf8() <= 64 => {
                view.filter.push(c)
            }
            _ => {}
        }
        view.offset = 0;
        return false;
    }
    match code {
        KeyCode::Esc if !view.filter.is_empty() => {
            view.filter.clear();
            view.offset = 0;
        }
        KeyCode::Esc | KeyCode::Char('q' | '?') => return true,
        KeyCode::Tab => {
            view.mode = match view.mode {
                Mode::Context => Mode::All,
                Mode::All | Mode::Keymap => Mode::Context,
            };
            view.offset = 0;
        }
        KeyCode::Char('/') => view.typing = true,
        _ => view.offset = scroll_offset(&help_body(app, view), view.offset, code),
    }
    false
}

fn entry(action: &Action, keys: &[Key], custom: bool, with_kind: bool) -> String {
    let description = match keymap::class_label(action.kinds) {
        Some(kind) if with_kind => format!("{kind}: {}", action.description),
        _ => action.description.to_owned(),
    };
    let marker = if custom {
        " *"
    } else if action.fixed {
        " (fixed)"
    } else {
        ""
    };
    format!("  {:<16} {description}{marker}", keymap::label(keys))
}

fn section(app: &App, scope: Scope, class: Option<u8>, out: &mut Vec<String>) {
    out.push(scope.title().to_owned());
    let mut entries: Vec<_> = app
        .keymap
        .entries(scope)
        .filter(|(action, _, _)| class.is_none_or(|class| action.kinds & class != 0))
        .collect();
    // For one resource kind, its kind-specific actions come first.
    if class.is_some() {
        entries.sort_by_key(|(action, _, _)| action.kinds == keymap::ANY);
    }
    for (action, keys, custom) in entries {
        out.push(entry(action, keys, custom, class.is_none()));
    }
    out.push(String::new());
}

fn hotkeys(app: &App, out: &mut Vec<String>) {
    if app.hotkeys.is_empty() {
        return;
    }
    out.push("HOTKEYS (hotkeys.yaml; checked before table keys)".into());
    for hotkey in &app.hotkeys {
        out.push(format!(
            "  {:<16} :{}  {}",
            hotkey.shortcut.to_string(),
            hotkey.command,
            hotkey.description
        ));
    }
    out.push(String::new());
}

fn footer(app: &App, out: &mut Vec<String>, mode: Mode) {
    if app.keymap.any_custom() {
        out.push("* remapped in keymap.yaml".into());
    }
    let all = app
        .keymap
        .primary("help.all")
        .unwrap_or_else(|| "(unbound)".into());
    let filter = app
        .keymap
        .primary("help.filter")
        .unwrap_or_else(|| "(unbound)".into());
    out.push(format!(
        "{all} {} | {filter} filter | :keymap lists every action | keymap.yaml remaps keys",
        if mode == Mode::Context {
            "every key and command"
        } else {
            "keys of this view"
        }
    ));
}

/// Help text before filtering and wrapping.
fn help_lines(app: &App, view: &HelpView) -> Vec<String> {
    let mut out = Vec::new();
    match view.mode {
        Mode::Context => {
            if view.scope == Scope::Table {
                let kind = app.kind();
                let kind = kind
                    .split('.')
                    .next()
                    .unwrap_or_default()
                    .to_ascii_uppercase();
                out.push(format!("KEYS FOR THE {kind} TABLE"));
            } else {
                out.push(format!("KEYS FOR {}", view.scope.title()));
            }
            out.push(String::new());
            let class = crate::keymap::kind_class(&app.kind());
            for scope in view.scope.layers().iter().rev() {
                section(
                    app,
                    *scope,
                    (*scope == Scope::Table).then_some(class),
                    &mut out,
                );
            }
            if view.scope == Scope::Table {
                hotkeys(app, &mut out);
            }
        }
        Mode::All => {
            out.push("EVERY KEY (your current bindings)".into());
            out.push(String::new());
            for scope in Scope::ALL {
                section(app, scope, None, &mut out);
            }
            hotkeys(app, &mut out);
            out.extend(COMMAND_HELP.lines().map(str::to_owned));
            out.push(String::new());
        }
        Mode::Keymap => {
            out.push("KEYMAP / every action, its keys and view; / filters".into());
            out.push(format!(
                "{:<22} {:<18} {:<10} DESCRIPTION",
                "ACTION", "KEYS", "VIEW"
            ));
            for action in keymap::ACTIONS {
                let keys = app.keymap.keys(action.name);
                let marker = if app.keymap.is_custom(action.name) {
                    " *"
                } else if action.fixed {
                    " (fixed)"
                } else {
                    ""
                };
                let description = match keymap::class_label(action.kinds) {
                    Some(kind) => format!("{kind}: {}", action.description),
                    None => action.description.to_owned(),
                };
                out.push(format!(
                    "{:<22} {:<18} {:<10} {description}{marker}",
                    action.name,
                    keymap::label(keys),
                    action.scope.name()
                ));
            }
            for hotkey in &app.hotkeys {
                out.push(format!(
                    "{:<22} {:<18} {:<10} :{} {}",
                    "hotkey",
                    hotkey.shortcut.to_string(),
                    "table",
                    hotkey.command,
                    hotkey.description
                ));
            }
            out.push(String::new());
        }
    }
    footer(app, &mut out, view.mode);
    out
}

/// The help body as displayed: filtered by `/` text, before wrapping.
pub(super) fn help_body(app: &App, view: &HelpView) -> String {
    let lines = help_lines(app, view);
    if view.filter.is_empty() && !view.typing {
        return lines.join("\n");
    }
    let needle = view.filter.to_ascii_lowercase();
    let mut out = vec![format!(
        "FILTER /{}{}",
        view.filter,
        if view.typing { "_" } else { "" }
    )];
    out.extend(
        lines
            .into_iter()
            .filter(|line| !line.trim().is_empty() && line.to_ascii_lowercase().contains(&needle)),
    );
    out.join("\n")
}

fn key_of(app: &App, action: &str) -> String {
    app.keymap
        .primary(action)
        .unwrap_or_else(|| "(unbound)".into())
}

/// `key label` pairs for bound actions; unbound actions are omitted.
fn pairs(app: &App, parts: &[(&str, &str)]) -> Vec<String> {
    parts
        .iter()
        .filter_map(|(action, label)| {
            app.keymap
                .primary(action)
                .map(|key| format!("{key} {label}"))
        })
        .collect()
}

/// The generated bottom hint row of a modal, for views with registered keys.
pub(super) fn modal_hint(app: &App, modal: &Modal) -> Option<String> {
    let scroll = format!(
        "{} {} scroll",
        key_of(app, "detail.page-up"),
        key_of(app, "detail.page-down")
    );
    Some(match modal {
        Modal::Detail(..) => {
            let mut parts = vec![scroll];
            parts.extend(pairs(app, &[("command", "command"), ("help", "keys")]));
            parts.push("Esc close".into());
            parts.join(" | ")
        }
        Modal::Help(view) if view.typing => "Type to filter | Enter keep | Esc clear".into(),
        Modal::Help(view) => {
            let mut parts = pairs(
                app,
                &[
                    (
                        "help.all",
                        if view.mode == Mode::Context {
                            "all keys"
                        } else {
                            "this view"
                        },
                    ),
                    ("help.filter", "filter"),
                ],
            );
            parts.push(scroll);
            parts.push("Esc close".into());
            parts.join(" | ")
        }
        Modal::Overview(..) => {
            let mut parts = vec!["Esc close".to_owned()];
            parts.extend(pairs(
                app,
                &[
                    ("overview.json", "redacted JSON"),
                    ("overview.yaml", "YAML"),
                ],
            ));
            parts.push(scroll);
            parts.extend(pairs(app, &[("help", "keys")]));
            parts.join(" | ")
        }
        Modal::Metrics(..) => {
            let mut parts = vec!["Esc close".to_owned()];
            parts.extend(pairs(
                app,
                &[
                    ("metrics.refresh", "refresh"),
                    ("metrics.resources", "requests/limits (extra reads)"),
                ],
            ));
            parts.push(scroll);
            parts.join(" | ")
        }
        Modal::ForwardView => {
            let mut parts = pairs(
                app,
                &[
                    ("forward.new", "new forward"),
                    ("forward.stop", "stop selected"),
                    ("forward.bench", "benchmark"),
                ],
            );
            parts.push(format!(
                "{} {} move",
                key_of(app, "forward.down"),
                key_of(app, "forward.up")
            ));
            parts.extend(pairs(app, &[("help", "keys")]));
            parts.push("Esc back".into());
            parts.join(" | ")
        }
        Modal::PodContainers(..) => {
            let mut parts = vec!["Enter logs".to_owned()];
            parts.extend(pairs(
                app,
                &[
                    ("containers.shell", "shell"),
                    ("containers.attach", "attach"),
                    ("containers.describe", "describe"),
                ],
            ));
            parts.push(format!(
                "{} {} move",
                key_of(app, "containers.down"),
                key_of(app, "containers.up")
            ));
            parts.extend(pairs(app, &[("help", "keys")]));
            parts.push("Esc back".into());
            parts.join(" | ")
        }
        Modal::YamlEditor(..) => {
            let mut parts = vec!["Arrows move".to_owned(), "Tab indent".to_owned()];
            parts.extend(pairs(
                app,
                &[("editor.review", "review"), ("editor.external", "$EDITOR")],
            ));
            parts.push("Esc cancel".into());
            parts.join("  ")
        }
        _ => return None,
    })
}

/// The two log-view hint rows: actions, then the range keys.
pub(super) fn log_hints(app: &App) -> (String, String) {
    let mut parts = pairs(
        app,
        &[
            ("log.filter", "filter"),
            ("log.errors", "errors"),
            ("log.warnings", "warnings"),
            ("log.autoscroll", "scroll"),
            ("log.timestamps", "time"),
            ("log.wrap", "wrap"),
            ("log.export", "export"),
            ("help", "keys"),
        ],
    );
    parts.push("Esc back".into());
    let labels = ["tail", "head", "1m", "5m", "15m", "30m", "1h"];
    let mut ranges: Vec<String> = app
        .keymap
        .keys("log.range")
        .iter()
        .zip(labels)
        .map(|(key, label)| format!("{key} {label}"))
        .collect();
    ranges.push("| Esc back".into());
    (parts.join(" | "), ranges.join(" "))
}

/// Dashboard keymap overlay lines, generated from the registry.
pub(super) fn dashboard_help_lines(app: &App) -> Vec<String> {
    let mut lines: Vec<String> = app
        .keymap
        .entries(Scope::Dashboard)
        .map(|(action, keys, custom)| entry(action, keys, custom, false).trim_start().to_owned())
        .collect();
    lines.extend([
        "Enter on a warning opens that Pod's Events (exact UID)".to_owned(),
        "Esc closes the log pane, then clears the warning selection".to_owned(),
        format!("{} / Esc closes this keymap", key_of(app, "help")),
        String::new(),
        "Pod watches and recent warning Events are independent.".into(),
        "Unknown, denied, stale and incomplete evidence stays explicit.".into(),
        "OOM evidence is current or previous container termination.".into(),
        "Restarts are current container counters, not a rate.".into(),
        "Dashboard reads never enable write capabilities.".into(),
    ]);
    lines
}

/// The dashboard bottom row for its current state.
pub(super) fn dashboard_hint(app: &App, dashboard: &dashboard_ui::Dashboard) -> String {
    let parts: Vec<String> = if dashboard.help {
        let mut parts = pairs(app, &[("help", "keymap")]);
        parts.push("Esc back".into());
        parts
    } else if dashboard.picker.is_some() {
        vec![
            "Space select".into(),
            "Enter tail".into(),
            "Esc cancel".into(),
        ]
    } else if dashboard
        .logs
        .as_ref()
        .is_some_and(|pane| pane.uid.is_empty())
    {
        let mut parts = pairs(
            app,
            &[
                ("dashboard.tail", "close tail"),
                ("dashboard.errors", "errors"),
                ("dashboard.warnings", "warnings"),
            ],
        );
        parts.push("Esc close tail".into());
        parts
    } else if dashboard.logs.is_some() {
        let mut parts = pairs(
            app,
            &[
                ("dashboard.logs", "close logs"),
                ("dashboard.warning", "next warning"),
            ],
        );
        parts.extend(["Enter Events".into(), "Esc close logs".into()]);
        parts
    } else if dashboard.warning.is_some() {
        let mut parts = pairs(app, &[("dashboard.warning", "next warning")]);
        parts.push("Enter Events".into());
        parts.extend(pairs(app, &[("dashboard.logs", "live logs")]));
        parts.push("Esc clear".into());
        parts
    } else {
        let mut parts = vec![format!(
            "{} focus",
            keymap::label(app.keymap.keys("dashboard.focus"))
        )];
        parts.push("Enter Pods".into());
        parts.extend(pairs(
            app,
            &[
                ("dashboard.warning", "warnings"),
                ("dashboard.tail", "tail workloads"),
                ("help", "keymap"),
            ],
        ));
        parts.push("Esc back".into());
        parts
    };
    format!(" {}", parts.join("   "))
}
