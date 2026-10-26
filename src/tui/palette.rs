//! Ctrl-P command palette: a fuzzy-search overlay over the keymap actions of
//! the current view, `:` commands, resource kinds, recent contexts and
//! namespaces, and recently opened objects.
//!
//! The palette never runs anything itself. Choosing an action delivers that
//! action exactly as its key would (table actions by name, other views by
//! their built-in key past keymap translation); choosing a command types it
//! into the `:` prompt and presses Enter. Every opt-in flag, review and typed
//! confirmation therefore still applies.
use super::*;
use crate::keymap::{self, Key, Scope};

/// Recently opened objects kept in memory for the palette.
pub(super) const MAX_VISITED: usize = 50;
const MAX_QUERY: usize = 64;
/// Table actions that count as opening the selected object.
const VISIT_ACTIONS: &[&str] = &[
    "describe",
    "yaml",
    "json",
    "edit",
    "resources",
    "logs",
    "logs.previous",
    "shell",
    "exec",
    "attach",
    "drilldown",
    "metrics",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Visited {
    pub(super) kind: String,
    pub(super) namespace: String,
    pub(super) name: String,
    pub(super) uid: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum Run {
    /// A keymap action of the view the palette was opened over.
    Action(&'static str),
    /// A complete `:` command, typed and run.
    Command(String),
    /// A `:` command that needs arguments: the prompt opens pre-filled.
    Prefill(String),
    /// Jump to a recently opened object.
    Object(Visited),
}

#[derive(Clone, Debug)]
pub(super) struct Entry {
    pub(super) key: String,
    pub(super) name: String,
    pub(super) description: String,
    pub(super) category: &'static str,
    pub(super) run: Run,
    /// Why the entry cannot run here; it is listed but refused.
    pub(super) off: Option<String>,
    /// Higher is closer to the current view.
    relevance: u8,
    /// Position in a most-recently-used list; lower is more recent.
    recency: usize,
}

pub(super) struct Palette {
    pub(super) query: String,
    pub(super) entries: Vec<Entry>,
    pub(super) selected: usize,
    /// The view the palette opened over; it closes if that view changes.
    pub(super) scope: Scope,
}

impl Palette {
    /// Entries matching the query, best first.
    pub(super) fn results(&self) -> Vec<&Entry> {
        rank(&self.query, &self.entries)
    }
}

/// Ranks `entries` for `query`: fuzzy score, then scope relevance, then MRU,
/// then registry order.
pub(super) fn rank<'a>(query: &str, entries: &'a [Entry]) -> Vec<&'a Entry> {
    let mut scored: Vec<(i32, usize, &Entry)> = entries
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| {
            let name = fuzzy_score(query, &entry.name).map(|score| score + 10);
            let other = fuzzy_score(query, &entry.description)
                .max(fuzzy_score(query, entry.category))
                .max(fuzzy_score(query, &entry.key));
            name.max(other).map(|score| (score, index, entry))
        })
        .collect();
    scored.sort_by(|a, b| {
        b.0.cmp(&a.0)
            .then(b.2.relevance.cmp(&a.2.relevance))
            .then(a.2.recency.cmp(&b.2.recency))
            .then(a.1.cmp(&b.1))
    });
    scored.into_iter().map(|(_, _, entry)| entry).collect()
}

fn word_start(text: &[char], index: usize) -> bool {
    index == 0 || matches!(text[index - 1], ' ' | '.' | '-' | '_' | ':' | '/' | '@')
}

/// Case-insensitive fuzzy subsequence score: every query character must
/// appear in order. Word starts and consecutive characters earn bonuses and
/// gaps cost a little, so `sc` ranks `scale` above `describe`. The best of
/// every starting position is kept. `None` means no match.
pub(super) fn fuzzy_score(query: &str, text: &str) -> Option<i32> {
    let query: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if query.is_empty() {
        return Some(0);
    }
    let text: Vec<char> = text.chars().map(|c| c.to_ascii_lowercase()).collect();
    let mut best = None;
    for start in (0..text.len()).filter(|&index| text[index] == query[0]) {
        let mut score = 0i32;
        let mut previous: Option<usize> = None;
        let mut position = start;
        let mut matched = true;
        for wanted in &query {
            let Some(index) = (position..text.len()).find(|&index| text[index] == *wanted) else {
                matched = false;
                break;
            };
            score += 1;
            if word_start(&text, index) {
                score += 8;
            }
            match previous {
                Some(previous) if index == previous + 1 => score += 5,
                Some(previous) => score -= ((index - previous - 1) as i32).min(3),
                None => score -= (index as i32).min(3),
            }
            previous = Some(index);
            position = index + 1;
        }
        if matched {
            if query.len() == text.len() {
                score += 20;
            }
            best = best.max(Some(score));
        }
    }
    best
}

/// The scope the palette opens over in `modal`, or `None` where it does not
/// open: typed input, forms, reviews and running operations keep focus.
pub(super) fn palette_scope(modal: &Modal) -> Option<Scope> {
    match modal {
        Modal::None => Some(Scope::Table),
        modal => {
            keyhelp::translated_scope(modal).filter(|scope| scope.layers().contains(&Scope::Global))
        }
    }
}

/// Views where `:` opens the command prompt on top of the view.
pub(super) fn command_prompt_allowed(modal: &Modal) -> bool {
    match modal {
        Modal::None
        | Modal::Detail(..)
        | Modal::Overview(..)
        | Modal::PodContainers(..)
        | Modal::Metrics(..)
        | Modal::LiveOverview(..)
        | Modal::Directory(..)
        | Modal::ForwardView
        | Modal::HelmDocument(..)
        | Modal::Picker(..)
        | Modal::AdviceDocument(..) => true,
        Modal::Help(view) => !view.typing,
        Modal::Subjects(view) => !view.is_typing(),
        Modal::Follow(view) => !view.is_filtering(),
        Modal::Helm(table) => !table.is_filtering(),
        _ => false,
    }
}

/// One `:` command per COMMAND_HELP row segment (`:a / :b ARG`), with its
/// description; segments whose first word needs an argument pre-fill.
fn help_commands() -> Vec<(String, String, bool)> {
    let mut out: Vec<(String, String, bool)> = Vec::new();
    for line in COMMAND_HELP.lines() {
        let Some(rest) = line.strip_prefix("  :") else {
            continue;
        };
        let line = format!(":{rest}");
        // Usage and description are separated by a run of spaces; rows with
        // a full-width usage or prose stop after their leading :commands.
        let (usage, description) = match line.find("  ") {
            Some(split) => (line[..split].to_owned(), line[split..].trim().to_owned()),
            // A full-width usage column: one space, then a capitalized
            // description.
            None if [23, 24].iter().any(|&at| {
                line.as_bytes().get(at) == Some(&b' ')
                    && line
                        .as_bytes()
                        .get(at + 1)
                        .is_some_and(u8::is_ascii_uppercase)
            }) =>
            {
                let at = if line.as_bytes().get(23) == Some(&b' ') {
                    23
                } else {
                    24
                };
                (line[..at].to_owned(), line[at..].trim().to_owned())
            }
            None => {
                let words: Vec<&str> = line.split(' ').collect();
                let count = words
                    .iter()
                    .take_while(|word| word.starts_with(':'))
                    .count();
                (words[..count].join(" "), words[count..].join(" "))
            }
        };
        for segment in usage.split(" / ") {
            let mut words = segment.split_whitespace();
            let Some(command) = words.next().and_then(|word| word.strip_prefix(':')) else {
                continue;
            };
            if command.is_empty() || out.iter().any(|(name, _, _)| name == command) {
                continue;
            }
            let needs_argument = words.next().is_some_and(|word| {
                word.chars().any(|c| c.is_ascii_uppercase()) && !word.starts_with('[')
            });
            out.push((command.to_owned(), description.clone(), needs_argument));
        }
    }
    out
}

impl App {
    /// Remembers the selected object as recently opened (bounded MRU).
    pub(super) fn remember_visit(&mut self, action: &str) {
        if !VISIT_ACTIONS.contains(&action) {
            return;
        }
        let Some((kind, namespace, name)) = self.selected_target() else {
            return;
        };
        let uid = self.selected_uid().unwrap_or_default();
        let namespaced = self
            .visible()
            .get(self.selected)
            .is_some_and(|object| object["metadata"]["namespace"].is_string());
        let visit = Visited {
            kind,
            namespace: if namespaced { namespace } else { String::new() },
            name,
            uid,
        };
        self.visited.retain(|old| *old != visit);
        self.visited.push_front(visit);
        self.visited.truncate(MAX_VISITED);
    }

    pub(super) fn open_palette(&mut self) {
        let Some(scope) = palette_scope(&self.modal) else {
            return;
        };
        self.palette = Some(Box::new(Palette {
            query: String::new(),
            entries: self.palette_entries(scope),
            selected: 0,
            scope,
        }));
    }

    /// Why a keymap action cannot run in this view, reusing the action
    /// menu's capability reasons.
    pub(super) fn palette_off_reason(&self, action: &keymap::Action) -> Option<String> {
        let connection = &self.options.connection;
        let name = action.name;
        if action.scope == Scope::Table {
            let kind = self.kind();
            let class = keymap::kind_class(&kind);
            if action.kinds & class == 0 {
                let label = keymap::class_label(action.kinds).unwrap_or("Some kinds");
                return Some(format!("{label} tables only"));
            }
            let selection = matches!(
                name,
                "describe"
                    | "yaml"
                    | "json"
                    | "edit"
                    | "resources"
                    | "logs"
                    | "logs.previous"
                    | "shell"
                    | "exec"
                    | "attach"
                    | "forward"
                    | "node.show"
                    | "node.shell"
                    | "cordon"
                    | "uncordon"
                    | "drain"
                    | "scale"
                    | "restart"
                    | "trigger"
                    | "suspend"
                    | "copy.name"
                    | "copy.namespace"
                    | "warp"
                    | "mark"
                    | "delete"
                    | "kill"
                    | "metrics"
            );
            let write = matches!(
                name,
                "edit"
                    | "resources"
                    | "shell"
                    | "exec"
                    | "attach"
                    | "forward"
                    | "node.shell"
                    | "cordon"
                    | "uncordon"
                    | "drain"
                    | "scale"
                    | "restart"
                    | "trigger"
                    | "suspend"
                    | "delete"
                    | "kill"
            );
            let reason = if selection && self.visible().is_empty() {
                Some("select an object")
            } else if matches!(name, "logs" | "logs.previous") && !connection.logs {
                Some("off (read-only or operator config)")
            } else if write && !connection.writable {
                Some("read-only mode")
            } else if name == "scale"
                && !["deployments.apps", "statefulsets.apps"].contains(&kind.as_str())
            {
                Some("Deployments and StatefulSets only")
            } else if name == "restart" && !crate::edit::supports_resource(&kind) {
                Some("Supported workloads only")
            } else if name == "scan" && self.options.scanner.is_none() {
                Some("needs trivy or grype")
            } else if (matches!(name, "shell" | "exec") && !connection.allow_exec)
                || (name == "attach" && !connection.allow_attach)
                || (name == "forward" && !connection.allow_port_forward)
                || (name == "edit" && !connection.allow_yaml_edit)
                || (name == "kill" && !connection.allow_force_kill)
                || (name == "node.shell" && !connection.allow_node_shell)
                || (matches!(name, "cordon" | "uncordon" | "drain")
                    && !connection.allow_node_maintenance)
                || (name == "scan" && !self.options.allow_scan)
            {
                Some("off (read-only or operator config)")
            } else {
                None
            };
            return reason.map(str::to_owned);
        }
        let helm_write = matches!(
            name.strip_prefix("helm.history.")
                .or(name.strip_prefix("helm.")),
            Some("install" | "upgrade" | "uninstall" | "rollback")
        );
        let off = (helm_write && (!connection.writable || !connection.allow_helm_write))
            || (name.ends_with(".test")
                && name.starts_with("helm.")
                && !connection.allow_helm_hooks)
            || (name.ends_with(".crds")
                && name.starts_with("helm.")
                && !connection.allow_helm_crds);
        let reason = off.then_some("off (read-only or operator config)");
        reason.map(str::to_owned)
    }

    pub(super) fn palette_entries(&self, scope: Scope) -> Vec<Entry> {
        let mut entries = Vec::new();
        let layers = scope.layers();
        for action in keymap::ACTIONS {
            if action.name == "palette" || !layers.contains(&action.scope) {
                continue;
            }
            let keys = self.keymap.keys(action.name);
            let description = match keymap::class_label(action.kinds) {
                Some(kind) => format!("{kind}: {}", action.description),
                None => action.description.to_owned(),
            };
            entries.push(Entry {
                key: keymap::label(keys),
                name: action.name.to_owned(),
                description,
                category: action.scope.name(),
                run: Run::Action(action.name),
                off: self.palette_off_reason(action),
                relevance: if action.scope == scope { 4 } else { 3 },
                recency: usize::MAX,
            });
        }
        let commands_off = (!command_prompt_allowed(&self.modal))
            .then(|| "close this view first; : is not available here".to_owned());
        let command_key = self.keymap.primary("command").unwrap_or_else(|| ":".into());
        let mut command =
            |name: String, description: String, run: Run, category, relevance, recency| {
                entries.push(Entry {
                    key: command_key.clone(),
                    name,
                    description,
                    category,
                    run,
                    off: commands_off.clone(),
                    relevance,
                    recency,
                })
            };
        let help = help_commands();
        for (name, description, needs_argument) in &help {
            let run = if *needs_argument {
                Run::Prefill(format!("{name} "))
            } else {
                Run::Command(name.clone())
            };
            command(
                format!(":{name}"),
                description.clone(),
                run,
                "command",
                1,
                usize::MAX,
            );
        }
        for special in nav::SPECIAL {
            if special.len() > 2 && !help.iter().any(|(name, _, _)| name == special) {
                command(
                    format!(":{special}"),
                    format!("Run :{special} (see ? Tab for the command reference)"),
                    Run::Command((*special).to_owned()),
                    "command",
                    1,
                    usize::MAX,
                );
            }
        }
        for kind in &self.kinds {
            let mut aliases = vec![kind.kind.to_ascii_lowercase()];
            aliases.extend(kind.short_names.iter().cloned());
            command(
                format!(":{}", kind.key()),
                format!("{} resources ({})", kind.kind, aliases.join(", ")),
                Run::Command(kind.key()),
                "resource",
                1,
                usize::MAX,
            );
        }
        for alias in self.aliases.keys() {
            command(
                format!(":{alias}"),
                "Alias from aliases.yaml".into(),
                Run::Command(alias.clone()),
                "resource",
                1,
                usize::MAX,
            );
        }
        let plain = |text: &str| {
            !text.is_empty()
                && !text
                    .chars()
                    .any(|c| c.is_whitespace() || c.is_control() || c == '\'')
        };
        for (index, context) in self.recent.contexts.iter().enumerate() {
            if plain(context) {
                command(
                    format!("@{context}"),
                    "Switch to this recent context".into(),
                    Run::Command(format!("ctx {context}")),
                    "context",
                    1,
                    index,
                );
            }
        }
        for (index, namespace) in self.recent.namespaces(&self.context).iter().enumerate() {
            if safety::valid_id(namespace) {
                command(
                    format!("ns {namespace}"),
                    "Switch to this recent namespace".into(),
                    Run::Command(format!("ns {namespace}")),
                    "namespace",
                    1,
                    index,
                );
            }
        }
        for (index, visit) in self.visited.iter().enumerate() {
            let kind = visit.kind.split('.').next().unwrap_or_default();
            let place = if visit.namespace.is_empty() {
                String::new()
            } else {
                format!(" in {}", visit.namespace)
            };
            command(
                format!("{kind}/{}", visit.name),
                format!("Recently opened {}{place}", visit.kind),
                Run::Object(visit.clone()),
                "recent",
                2,
                index,
            );
        }
        entries
    }

    /// Handles a key while the palette is open. Returns the run result of a
    /// chosen entry (`true` quits).
    pub(super) fn palette_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> Result<bool> {
        let Some(mut palette) = self.palette.take() else {
            return Ok(false);
        };
        if palette_scope(&self.modal) != Some(palette.scope) {
            self.status = "The view changed; the palette closed.".into();
            return Ok(false);
        }
        let count = palette.results().len();
        let ctrl = modifiers.contains(KeyModifiers::CONTROL);
        match code {
            KeyCode::Esc => return Ok(false),
            KeyCode::Char('c') if ctrl => return Ok(false),
            KeyCode::Up | KeyCode::BackTab => palette.selected = palette.selected.saturating_sub(1),
            KeyCode::Char('p') if ctrl => palette.selected = palette.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Tab => {
                palette.selected = (palette.selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::Char('n') if ctrl => {
                palette.selected = (palette.selected + 1).min(count.saturating_sub(1))
            }
            KeyCode::PageUp => palette.selected = palette.selected.saturating_sub(10),
            KeyCode::PageDown => {
                palette.selected = (palette.selected + 10).min(count.saturating_sub(1))
            }
            KeyCode::Home => palette.selected = 0,
            KeyCode::End => palette.selected = count.saturating_sub(1),
            KeyCode::Char('u' | 'w') if ctrl => {
                palette.query.clear();
                palette.selected = 0;
            }
            KeyCode::Backspace => {
                palette.query.pop();
                palette.selected = 0;
            }
            KeyCode::Enter => {
                let Some(entry) = palette
                    .results()
                    .get(palette.selected)
                    .map(|entry| (*entry).clone())
                else {
                    self.palette = Some(palette);
                    return Ok(false);
                };
                if let Some(reason) = &entry.off {
                    self.status = format!("[OFF] {}: {reason}", entry.name);
                    self.palette = Some(palette);
                    return Ok(false);
                }
                return self.run_palette_entry(palette.scope, entry.run);
            }
            KeyCode::Char(c)
                if !ctrl
                    && !modifiers.contains(KeyModifiers::ALT)
                    && !c.is_control()
                    && palette.query.len() + c.len_utf8() <= MAX_QUERY =>
            {
                palette.query.push(c);
                palette.selected = 0;
            }
            _ => {
                let key = Key::from_event(code, modifiers);
                if key.is_some_and(|key| self.keymap.matches("palette", key)) {
                    return Ok(false);
                }
            }
        }
        self.palette = Some(palette);
        Ok(false)
    }

    /// Runs a chosen entry through the same path as its key or command.
    fn run_palette_entry(&mut self, scope: Scope, run: Run) -> Result<bool> {
        match run {
            Run::Action(name) => self.run_palette_action(scope, name),
            Run::Command(text) => self.type_command(&text, true),
            Run::Prefill(text) => self.type_command(&text, false),
            Run::Object(visit) => {
                let place = if visit.namespace.is_empty() {
                    String::new()
                } else {
                    format!(" {}", visit.namespace)
                };
                // The filter text is namespace, name and status separated by
                // spaces, so the name is matched between them.
                let filter = format!("\\s{}\\s", regex::escape(&visit.name));
                let quit = self.type_command(&format!("{}{place} /{filter}", visit.kind), true)?;
                if !visit.uid.is_empty() {
                    self.restoring_uid = Some(visit.uid);
                }
                Ok(quit)
            }
        }
    }

    pub(super) fn run_palette_action(&mut self, scope: Scope, name: &'static str) -> Result<bool> {
        let Some(index) = keymap::find(name) else {
            return Ok(false);
        };
        let action = &keymap::ACTIONS[index];
        if scope == Scope::Table && matches!(self.modal, Modal::None) {
            // Table keys dispatch by action name, so this is the key's path.
            return self.table_action(name, 0);
        }
        let Some(key) = action.keys.first().and_then(|key| Key::parse(key)) else {
            self.status = format!("{name} has no built-in key in this view.");
            return Ok(false);
        };
        let (code, modifiers) = key.event();
        self.translated_key_event(code, modifiers)
    }

    /// Opens the `:` prompt the way its key does, types `text`, and presses
    /// Enter when `run` is set.
    fn type_command(&mut self, text: &str, run: bool) -> Result<bool> {
        if !command_prompt_allowed(&self.modal) {
            self.status = "Close this view first; : is not available here.".into();
            return Ok(false);
        }
        if matches!(self.modal, Modal::None) {
            self.table_action("command", 0)?;
        } else {
            self.key(KeyCode::Char(':'))?;
        }
        if !matches!(self.modal, Modal::Input("Command", _)) {
            return Ok(false);
        }
        self.command_cursor = text.len();
        self.completion = None;
        self.modal = Modal::Input("Command", text.to_owned());
        if run {
            return self.key(KeyCode::Enter);
        }
        Ok(false)
    }
}

/// Draws the palette over the current frame.
pub(super) fn render_palette(
    app: &App,
    palette: &Palette,
    w: u16,
    h: u16,
    out: &mut impl Write,
    colors: [Color; 5],
) -> io::Result<()> {
    let [fg, bg, muted, selection_fg, selection_bg] = colors;
    let width = w.saturating_sub(4).clamp(24, 120);
    let height = h.saturating_sub(4).clamp(6, 22);
    let rect = Rect {
        x: (w - width) / 2,
        y: (h - height) / 2,
        width,
        height,
    };
    panel(out, rect, "COMMAND PALETTE", true, fg, bg, &app.ui)?;
    let inner = rect.width - 2;
    let x = rect.x + 1;
    let results = palette.results();
    cell(
        out,
        x,
        rect.y + 1,
        inner,
        &format!(
            " > {}_   {} of {}",
            palette.query,
            results.len(),
            palette.entries.len()
        ),
        fg,
        bg,
    )?;
    let rows = usize::from(rect.height.saturating_sub(4));
    let start = palette.selected.saturating_sub(rows.saturating_sub(1));
    let wide = inner >= 90;
    let (key_width, name_width, category_width) = if wide { (14, 24, 11) } else { (10, 18, 0) };
    for (line, entry) in results.iter().skip(start).take(rows).enumerate() {
        let selected = start + line == palette.selected;
        let description = match &entry.off {
            Some(reason) => format!("[OFF] {reason}"),
            None => entry.description.clone(),
        };
        // A marker as well as colors, so the selection shows without color.
        let marker = if selected { '>' } else { ' ' };
        let text = if wide {
            format!(
                "{marker}{:<key_width$} {:<name_width$} {:<category_width$} {description}",
                display(&entry.key, key_width),
                display(&entry.name, name_width),
                entry.category,
            )
        } else {
            format!(
                "{marker}{:<key_width$} {:<name_width$} {description}",
                display(&entry.key, key_width),
                display(&entry.name, name_width),
            )
        };
        let (text_fg, text_bg) = if selected {
            (selection_fg, selection_bg)
        } else if entry.off.is_some() {
            (muted, bg)
        } else {
            (fg, bg)
        };
        cell(
            out,
            x,
            rect.y + 2 + line as u16,
            inner,
            &text,
            text_fg,
            text_bg,
        )?;
    }
    if results.is_empty() {
        cell(
            out,
            x,
            rect.y + 2,
            inner,
            " No match; Backspace edits, Esc closes",
            muted,
            bg,
        )?;
    }
    let hint = if wide {
        " Type to filter | Enter run | Up/Down Ctrl-N/Ctrl-P move | Esc close"
    } else {
        " Enter run | Ctrl-N/P move | Esc close"
    };
    cell(out, x, rect.y + rect.height - 2, inner, hint, muted, bg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, relevance: u8, recency: usize) -> Entry {
        Entry {
            key: String::new(),
            name: name.into(),
            description: String::new(),
            category: "test",
            run: Run::Command(name.into()),
            off: None,
            relevance,
            recency,
        }
    }
    fn names(query: &str, entries: &[Entry]) -> Vec<String> {
        rank(query, entries)
            .into_iter()
            .map(|entry| entry.name.clone())
            .collect()
    }

    #[test]
    fn fuzzy_needs_an_ordered_subsequence_and_ignores_case() {
        assert!(fuzzy_score("scl", "Scale").is_some());
        assert!(fuzzy_score("lcs", "scale").is_none());
        assert!(fuzzy_score("xyz", "scale").is_none());
        assert_eq!(fuzzy_score("", "anything"), Some(0));
        assert_eq!(fuzzy_score("  ", "anything"), Some(0));
    }

    #[test]
    fn word_starts_and_consecutive_characters_rank_higher() {
        // Word start and consecutive beat a mid-word match.
        assert!(fuzzy_score("sc", "scale") > fuzzy_score("sc", "describe"));
        // Two word starts beat two adjacent mid-word letters.
        assert!(fuzzy_score("lp", "logs.previous") > fuzzy_score("lp", "help"));
        // Consecutive beats scattered.
        assert!(fuzzy_score("log", "logs") > fuzzy_score("log", "lxoxg"));
        // An exact match beats a longer name with the same prefix.
        assert!(fuzzy_score("refresh", "refresh") > fuzzy_score("refresh", "refresh.r"));
        // The best start is kept, not the first occurrence.
        let restart = fuzzy_score("rest", "restart").unwrap_or_default();
        assert!(fuzzy_score("rest", "rollout restart").unwrap_or_default() >= restart - 3);
    }

    #[test]
    fn ties_break_by_relevance_then_recency_then_order() {
        let entries = vec![
            entry("ns b", 1, 1),
            entry("ns a", 1, 0),
            entry("ns c", 4, usize::MAX),
        ];
        assert_eq!(names("", &entries), ["ns c", "ns a", "ns b"]);
        assert_eq!(names("ns", &entries), ["ns c", "ns a", "ns b"]);
        // A better match wins over relevance.
        assert_eq!(names("nsb", &entries)[0], "ns b");
        let same = vec![entry("one", 1, usize::MAX), entry("two", 1, usize::MAX)];
        assert_eq!(names("", &same), ["one", "two"]);
    }

    #[test]
    fn names_rank_above_description_matches() {
        let mut described = entry("describe", 1, usize::MAX);
        described.description = "Scale a Deployment".into();
        let entries = vec![described, entry("scale", 1, usize::MAX)];
        assert_eq!(names("scale", &entries)[0], "scale");
        assert_eq!(names("scale", &entries).len(), 2);
    }

    #[test]
    fn command_reference_rows_become_commands_with_arguments_prefilled() {
        let commands = help_commands();
        let find = |name: &str| commands.iter().find(|(command, _, _)| command == name);
        assert!(find("pod").is_some_and(|(_, _, prefill)| !prefill));
        assert!(find("sort").is_some_and(
            |(_, description, prefill)| *prefill && description.starts_with("Sort by")
        ));
        assert!(find("history").is_some_and(|(_, _, prefill)| *prefill));
        assert!(
            find("cordon").is_some_and(|(_, description, _)| description.starts_with("Reviewed"))
        );
        assert!(find("helm").is_some_and(|(_, _, prefill)| !prefill));
        assert!(find("keys").is_some());
        let mut unique = commands.iter().map(|(name, _, _)| name).collect::<Vec<_>>();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), commands.len());
    }
}
