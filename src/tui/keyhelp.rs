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
        // The dashboard (and its tail picker) toggle their own overlay; in
        // forms `?` is typed text.
        Modal::LiveOverview(..)
        | Modal::YamlEditor(..)
        | Modal::Help(..)
        | Modal::HelmForm(..)
        | Modal::ForwardForm(..) => None,
        modal => translated_scope(modal),
    }
}

/// Views whose keys go through keymap translation; `None` keeps the raw key
/// (table keys dispatch by name, prompts and reviews keep fixed keys).
pub(super) fn translated_scope(modal: &Modal) -> Option<Scope> {
    Some(match modal {
        Modal::Follow(view) if !view.is_filtering() => Scope::Log,
        Modal::LiveOverview(dashboard) if dashboard.picker.is_none() => Scope::Dashboard,
        Modal::LiveOverview(..) => Scope::Tail,
        Modal::ForwardView => Scope::Forward,
        Modal::ForwardForm(..) => Scope::ForwardForm,
        Modal::Helm(table) if !table.is_filtering() => {
            if table.parent.is_some() {
                Scope::HelmHistory
            } else {
                Scope::Helm
            }
        }
        Modal::HelmForm(..) => Scope::HelmForm,
        Modal::Subjects(view) if !view.is_typing() => Scope::Subjects,
        Modal::Picker(label, ..) if *label == selections_ui::PICKER_LABEL => Scope::Selections,
        Modal::Picker(..) => Scope::Picker,
        Modal::Themes(..) => Scope::Themes,
        Modal::PodContainers(..) => Scope::Containers,
        Modal::Detail(..)
        | Modal::HelmDocument(..)
        | Modal::AdviceDocument(..)
        | Modal::HelmOutcome(..) => Scope::Detail,
        Modal::Overview(..) => Scope::Overview,
        Modal::Metrics(..) => Scope::Metrics,
        Modal::YamlEditor(..) => Scope::Editor,
        Modal::Help(view) if !view.typing => Scope::Help,
        Modal::Secrets(..) => Scope::Secrets,
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

fn section(app: &App, scope: Scope, out: &mut Vec<String>) {
    out.push(scope.title().to_owned());
    for (action, keys, custom) in app.keymap.entries(scope) {
        out.push(entry(action, keys, custom, true));
    }
    out.push(String::new());
}

/// The resource table's actions as context help groups them. Every table
/// action is listed once; `table_help_groups_cover_every_table_action` checks it.
pub(super) const TABLE_GROUPS: &[(&str, &[&str])] = &[
    (
        "ACTIONS ON THE SELECTED",
        &[
            "drilldown",
            "logs",
            "logs.previous",
            "shell",
            "exec",
            "attach",
            "forward",
            "node.show",
            "node.shell",
            "cordon",
            "uncordon",
            "drain",
            "scale",
            "restart",
            "trigger",
            "suspend",
            "edit",
            "resources",
            "delete",
            "kill",
            "scan",
        ],
    ),
    (
        "LOOK AT IT",
        &[
            "describe",
            "yaml",
            "json",
            "metrics",
            "copy.name",
            "copy.namespace",
        ],
    ),
    (
        "FIND AND SORT",
        &[
            "filter",
            "toggle.faults",
            "sort.name",
            "sort.age",
            "sort.status",
            "sort.ready",
            "sort.cpu",
            "sort.memory",
            "toggle.wide",
        ],
    ),
    (
        "SEVERAL ROWS AT ONCE",
        &["mark", "mark.range", "marks.clear"],
    ),
    (
        "NAMESPACES AND OTHER VIEWS",
        &[
            "ns.all",
            "ns.recent",
            "warp",
            "prompt",
            "aliases",
            "view.back",
            "view.forward",
            "view.last",
            "dashboard",
            "forwards",
            "selection.save",
            "selection.open",
            "snapshot",
        ],
    ),
    (
        "MOVE AND REFRESH",
        &[
            "down",
            "up",
            "page.down",
            "page.up",
            "top",
            "bottom",
            "back",
            "refresh",
            "refresh.r",
            "quit",
        ],
    ),
];

/// Kinds a reviewed deletion exists for.
pub(super) fn deletable(kind: &str) -> bool {
    matches!(
        kind,
        "pods"
            | "deployments.apps"
            | "statefulsets.apps"
            | "daemonsets.apps"
            | "services"
            | "jobs.batch"
            | "cronjobs.batch"
    )
}

/// What Enter opens for a kind, or `None` when it opens nothing.
fn drilldown_label(kind: &str) -> Option<&'static str> {
    match kind {
        "pods" => Some("Containers of the selected Pod"),
        "nodes" => Some("Pods on the selected Node"),
        "namespaces" => Some("Pods in the selected namespace"),
        kind if supports_group_logs(kind) => Some("Pods of the selected object"),
        _ => None,
    }
}

/// Whether a table action does anything for the kind in front: context help
/// lists only these. The key itself may still be bound (and say why not).
fn applies(app: &App, action: &Action) -> bool {
    let kind = app.kind();
    let kind = kind.as_str();
    if action.kinds & keymap::kind_class(kind) == 0 {
        return false;
    }
    let namespaced = app
        .kinds
        .get(app.kind_index)
        .is_none_or(|kind| kind.namespaced);
    let logs = kind == "pods" || supports_group_logs(kind);
    match action.name {
        "drilldown" => drilldown_label(kind).is_some(),
        "logs" | "logs.previous" => logs,
        "exec" | "attach" | "forward" | "kill" => kind == "pods",
        "scan" => logs,
        "scale" => matches!(kind, "deployments.apps" | "statefulsets.apps"),
        "resources" => crate::edit::supports_resource(kind),
        "edit" => namespaced && kind != "secrets",
        "delete" => deletable(kind),
        "metrics" | "sort.cpu" | "sort.memory" => matches!(kind, "pods" | "nodes"),
        "copy.namespace" | "warp" => namespaced,
        "toggle.wide" => kind == "pods" || app.current_view().is_some(),
        _ => true,
    }
}

/// Context help for the resource table: the actions that apply to this kind,
/// grouped by what they are for, with a note where this session cannot run one.
fn table_sections(app: &App, out: &mut Vec<String>) {
    let entries: Vec<_> = app.keymap.entries(Scope::Table).collect();
    let kind = app.kind();
    let one = app.kinds.get(app.kind_index).map_or_else(
        || "OBJECT".to_owned(),
        |kind| kind.kind.to_ascii_uppercase(),
    );
    let grouped = |name: &str| TABLE_GROUPS.iter().any(|(_, names)| names.contains(&name));
    let line = |action: &Action, keys: &[Key], custom: bool, out: &mut Vec<String>| {
        let mut text = entry(action, keys, custom, false);
        if action.name == "drilldown"
            && let Some(label) = drilldown_label(&kind)
        {
            text = format!("  {:<16} {label} (fixed)", keymap::label(keys));
        }
        if let Some(reason) = app.palette_off_reason(action) {
            text.push_str(&format!("  [{reason}]"));
        }
        out.push(text);
    };
    for (index, (title, names)) in TABLE_GROUPS.iter().enumerate() {
        let rows: Vec<_> = names
            .iter()
            .filter_map(|name| entries.iter().find(|(action, _, _)| action.name == *name))
            .filter(|(action, _, _)| applies(app, action))
            .collect();
        if rows.is_empty() {
            continue;
        }
        out.push(if index == 0 {
            format!("{title} {one}")
        } else {
            (*title).to_owned()
        });
        for (action, keys, custom) in rows {
            line(action, keys, *custom, out);
        }
        out.push(String::new());
    }
    // An action added to the registry without a group still shows.
    let rest: Vec<_> = entries
        .iter()
        .filter(|(action, _, _)| !grouped(action.name) && applies(app, action))
        .collect();
    if !rest.is_empty() {
        out.push("MORE".to_owned());
        for (action, keys, custom) in rest {
            line(action, keys, *custom, out);
        }
        out.push(String::new());
    }
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
            for scope in view.scope.layers().iter().rev() {
                if *scope == Scope::Table {
                    table_sections(app, &mut out);
                } else {
                    section(app, *scope, &mut out);
                }
            }
            if matches!(view.scope, Scope::Detail | Scope::Overview | Scope::Metrics) {
                out.extend(
                    [
                        "SEARCH THE TEXT (fixed keys)",
                        "  /                Search what is shown; ignores case",
                        "  n / N            Next / previous match",
                        "  Esc              Clear the search, then close the view",
                        "",
                    ]
                    .map(str::to_owned),
                );
            }
            if view.scope == Scope::Table {
                hotkeys(app, &mut out);
            }
            if matches!(view.scope, Scope::Table | Scope::Containers | Scope::Helm) {
                out.extend(app.plugin_help(Some(view.scope)));
            }
        }
        Mode::All => {
            out.push("EVERY KEY (your current bindings)".into());
            out.push(String::new());
            for scope in Scope::ALL {
                section(app, scope, &mut out);
            }
            hotkeys(app, &mut out);
            out.extend(app.plugin_help(None));
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
            for binding in &app.plugins.bindings {
                let description = app
                    .plugins
                    .meta(&binding.id)
                    .map(|meta| safety::safe_text(&meta.description, 120))
                    .unwrap_or_default();
                out.push(format!(
                    "{:<22} {:<18} {:<10} :ext {} {description}",
                    "plugin",
                    binding.key.to_string(),
                    binding.scope.name(),
                    binding.id,
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
            parts.extend(pairs(
                app,
                &[
                    ("command", "command"),
                    ("palette", "palette"),
                    ("help", "keys"),
                ],
            ));
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
                    (
                        "metrics.chart",
                        if app.metrics_chart {
                            "text view"
                        } else {
                            "chart"
                        },
                    ),
                    ("metrics.resources", "requests/limits (extra reads)"),
                ],
            ));
            if !app.metrics_chart {
                parts.push(scroll);
            }
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
        Modal::Helm(table) if table.is_filtering() => {
            "Type to filter | Enter keep | Esc cancel".into()
        }
        Modal::Helm(table) if table.parent.is_some() => {
            let mut parts = vec!["Enter values".to_owned()];
            parts.extend(pairs(
                app,
                &[
                    ("helm.history.rollback", "rollback"),
                    ("helm.history.test", "test"),
                    ("helm.history.upgrade", "upgrade"),
                    ("helm.history.uninstall", "uninstall"),
                    ("helm.history.filter", "filter"),
                    ("help", "keys"),
                ],
            ));
            parts.push("Esc back".into());
            parts.join(" | ")
        }
        Modal::Helm(..) => {
            let mut parts = vec!["Enter history".to_owned()];
            parts.extend(pairs(
                app,
                &[
                    ("helm.install", "install"),
                    ("helm.upgrade", "upgrade"),
                    ("helm.test", "test"),
                    ("helm.crds", "CRDs"),
                    ("helm.uninstall", "uninstall"),
                    ("helm.filter", "filter"),
                    ("help", "keys"),
                ],
            ));
            parts.push("Esc back".into());
            parts.join(" | ")
        }
        Modal::ForwardForm(..) => forward_form_hint(app),
        Modal::Subjects(view) if view.is_typing() => {
            "Type to filter | Enter keep | Esc clear".into()
        }
        Modal::Subjects(..) => {
            let mut parts = vec!["Enter bindings and rules".to_owned()];
            parts.extend(pairs(app, &[("subjects.filter", "filter")]));
            parts.push(format!(
                "{} {} {} {} move",
                key_of(app, "subjects.down"),
                key_of(app, "subjects.up"),
                key_of(app, "subjects.page-up"),
                key_of(app, "subjects.page-down")
            ));
            parts.extend(pairs(app, &[("help", "keys")]));
            parts.push("Esc back".into());
            parts.join(" | ")
        }
        Modal::Picker(label, ..) => {
            let selections = *label == selections_ui::PICKER_LABEL;
            let mut parts = vec![
                if selections {
                    "Enter open"
                } else {
                    "Enter choose"
                }
                .to_owned(),
            ];
            if selections {
                parts.extend(pairs(app, &[("selections.delete", "remove")]));
            }
            parts.push(format!(
                "{} {} move",
                key_of(app, "picker.down"),
                key_of(app, "picker.up")
            ));
            parts.extend(pairs(app, &[("help", "keys")]));
            parts.push(if selections { "Esc back" } else { "Esc cancel" }.into());
            parts.join(" | ")
        }
        Modal::Themes(..) => {
            let mut parts = vec![format!(
                "{} {} preview",
                key_of(app, "themes.down"),
                key_of(app, "themes.up")
            )];
            parts.push("Enter save".into());
            parts.extend(pairs(
                app,
                &[("themes.new", "new custom theme"), ("help", "keys")],
            ));
            parts.push("Esc restore".into());
            parts.join(" | ")
        }
        Modal::Secrets(view) => {
            use crate::ui_secrets::Mode as SecretsMode;
            let mode = view.mode();
            let mut parts = if mode == SecretsMode::List {
                vec!["Enter keys".to_owned()]
            } else {
                Vec::new()
            };
            let reveal = if mode == SecretsMode::List {
                "keys"
            } else {
                "reveal/hide key (30s)"
            };
            parts.extend(pairs(app, &[("secrets.reveal", reveal)]));
            if mode != SecretsMode::Yaml {
                parts.extend(pairs(app, &[("secrets.yaml", "masked YAML")]));
            }
            parts.push(format!(
                "{} {} move",
                key_of(app, "secrets.down"),
                key_of(app, "secrets.up")
            ));
            if mode == SecretsMode::Yaml {
                parts.push(format!(
                    "{} {} scroll",
                    key_of(app, "secrets.page-up"),
                    key_of(app, "secrets.page-down")
                ));
            }
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

/// The port-forward form's key line, also shown inside the form.
pub(super) fn forward_form_hint(app: &App) -> String {
    let mut parts = pairs(
        app,
        &[
            ("forward.form.next", "field"),
            ("forward.form.clear", "clear"),
        ],
    );
    parts.extend(["Enter review".into(), "Esc cancel".into()]);
    parts.join(" | ")
}

/// Helm form key hints, narrowing with the terminal width.
pub(super) fn helm_form_hints(app: &App, width: u16) -> Vec<(String, &'static str)> {
    let mut hints = Vec::new();
    let mut push = |action: &str, label: &'static str| {
        if let Some(key) = app.keymap.primary(action) {
            hints.push((key, label));
        }
    };
    if width < 40 {
        push("helm.form.next", "next");
        hints.push(("Enter".into(), "review"));
        hints.push(("Esc".into(), ""));
    } else if width < 60 {
        push("helm.form.next", "next");
        hints.push(("Enter".into(), "review"));
        hints.push(("Esc".into(), "back"));
    } else {
        push("helm.form.next", "next field");
        push("helm.form.prev", "previous");
        push("helm.form.clear", "clear");
        hints.push(("Enter".into(), "review"));
        hints.push(("Esc".into(), "back"));
    }
    hints
}

/// The first line of a picker, naming its keys.
pub(super) fn picker_line(app: &App, label: &str) -> String {
    if label == selections_ui::PICKER_LABEL {
        let mut parts = vec!["Enter open".to_owned()];
        parts.extend(pairs(app, &[("selections.delete", "remove")]));
        parts.push("Esc back".into());
        format!(" {}", parts.join(" | "))
    } else {
        format!(
            " {label} | {} {} / Enter select / Esc cancel",
            key_of(app, "picker.down"),
            key_of(app, "picker.up")
        )
    }
}

/// The theme picker's first line.
pub(super) fn themes_line(app: &App) -> String {
    let new = app
        .keymap
        .primary("themes.new")
        .map(|key| format!(" {key} create custom;"))
        .unwrap_or_default();
    format!(
        " THEMES - {} {} preview; Enter save;{new} Esc restore",
        key_of(app, "themes.down"),
        key_of(app, "themes.up")
    )
}

/// The subjects view's in-table key line.
pub(super) fn subjects_line(app: &App) -> String {
    let mut parts = vec!["Enter show bindings and rules".to_owned()];
    parts.extend(pairs(app, &[("subjects.filter", "filter")]));
    parts.push(format!(
        "{} {} move",
        key_of(app, "subjects.down"),
        key_of(app, "subjects.up")
    ));
    parts.push("Esc back".into());
    parts.join(" | ")
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

/// Tail picker overlay lines, generated from the registry.
fn tail_help_lines(app: &App) -> Vec<String> {
    let mut lines: Vec<String> = app
        .keymap
        .entries(Scope::Tail)
        .map(|(action, keys, custom)| entry(action, keys, custom, false).trim_start().to_owned())
        .collect();
    lines.extend([
        "Esc cancels the picker; nothing is tailed until Enter".to_owned(),
        format!("{} / Esc closes this keymap", key_of(app, "help")),
    ]);
    lines
}

/// The dashboard overlay title for its current state.
pub(super) fn dashboard_help_title(dashboard: &dashboard_ui::Dashboard) -> &'static str {
    if dashboard.picker.is_some() {
        "TAIL PICKER KEYS"
    } else {
        "DASHBOARD KEYS"
    }
}

/// Dashboard keymap overlay lines, generated from the registry.
pub(super) fn dashboard_help_lines(app: &App, dashboard: &dashboard_ui::Dashboard) -> Vec<String> {
    if dashboard.picker.is_some() {
        return tail_help_lines(app);
    }
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
        let mut parts = pairs(app, &[("tail.select", "select")]);
        parts.push("Enter tail".into());
        parts.extend(pairs(app, &[("help", "keymap")]));
        parts.push("Esc cancel".into());
        parts
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
                (
                    "dashboard.chart",
                    if dashboard.chart {
                        "close charts"
                    } else {
                        "charts"
                    },
                ),
                ("help", "keymap"),
            ],
        ));
        parts.push("Esc back".into());
        parts
    };
    format!(" {}", parts.join("   "))
}
