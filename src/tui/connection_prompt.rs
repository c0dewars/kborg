//! Local-only startup consent. No worker, client, or credential helper exists
//! until this screen returns an explicit Yes.
use super::*;
use crate::{context_config, gateway::ConnectionCatalog};
use std::path::Path;

struct StartupScope<'a> {
    directory: Option<&'a Path>,
    single_namespace: &'a str,
    dashboard_namespace: &'a str,
    dashboard: bool,
    nodes_default: bool,
    writable: bool,
}

struct Prompt {
    targets: Vec<usize>,
    picker: bool,
    single_selection: bool,
    cursor: usize,
    filter: String,
    yes: bool,
    offset: usize,
}
#[derive(Debug, PartialEq, Eq)]
enum Decision {
    Wait,
    Connect,
    Quit,
}
impl Prompt {
    fn new(catalog: &ConnectionCatalog, dashboard: &[String]) -> Result<Self> {
        let mut targets = Vec::new();
        if let Some(selected) = catalog.selected {
            targets.push(selected);
        }
        for name in dashboard {
            let index = catalog.contexts.iter().position(|entry| &entry.name == name)
                .ok_or("Dashboard context missing from kubeconfig. Next: run your cluster CLI or choose existing contexts with --contexts.")?;
            if !targets.contains(&index) {
                targets.push(index);
            }
        }
        Ok(Self {
            cursor: targets.first().copied().unwrap_or(0),
            picker: targets.is_empty(),
            single_selection: targets.is_empty(),
            targets,
            filter: String::new(),
            yes: true,
            offset: 0,
        })
    }
    fn matches(&self, catalog: &ConnectionCatalog) -> Vec<usize> {
        let filter = self.filter.to_lowercase();
        catalog
            .contexts
            .iter()
            .enumerate()
            .filter(|(_, entry)| entry.name.to_lowercase().contains(&filter))
            .map(|(i, _)| i)
            .collect()
    }
    fn open_picker(&mut self) {
        self.picker = true;
        self.cursor = self.targets.first().copied().unwrap_or(0);
        self.filter.clear();
        self.yes = true;
    }
    fn key(
        &mut self,
        key: event::KeyEvent,
        catalog: &ConnectionCatalog,
        bottom: bool,
        page: usize,
    ) -> Decision {
        if key.kind != KeyEventKind::Press {
            return Decision::Wait;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Decision::Quit;
        }
        if key.code == KeyCode::Esc {
            if self.picker && !self.targets.is_empty() {
                self.picker = false;
                return Decision::Wait;
            }
            return Decision::Quit;
        }
        if key
            .modifiers
            .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
        {
            return Decision::Wait;
        }
        if self.picker {
            let matches = self.matches(catalog);
            let position = matches
                .iter()
                .position(|index| *index == self.cursor)
                .unwrap_or(0);
            let last = matches.len().saturating_sub(1);
            let next = match key.code {
                KeyCode::Up => position.saturating_sub(1),
                KeyCode::Down => (position + 1).min(last),
                KeyCode::PageUp => position.saturating_sub(page),
                KeyCode::PageDown => (position + page).min(last),
                KeyCode::Home => 0,
                KeyCode::End => last,
                _ => position,
            };
            match key.code {
                KeyCode::Up
                | KeyCode::Down
                | KeyCode::Home
                | KeyCode::End
                | KeyCode::PageUp
                | KeyCode::PageDown => {
                    if let Some(index) = matches.get(next) {
                        self.cursor = *index;
                    }
                }
                KeyCode::Enter => {
                    if let Some(index) = matches.get(position) {
                        self.targets = vec![*index];
                        self.single_selection = true;
                        self.picker = false;
                        self.yes = true;
                        self.offset = 0;
                    }
                }
                KeyCode::Backspace => {
                    self.filter.pop();
                    self.cursor = self.matches(catalog).first().copied().unwrap_or(0);
                }
                KeyCode::Char(c) if !c.is_control() && self.filter.len() + c.len_utf8() <= 253 => {
                    self.filter.push(c);
                    self.cursor = self.matches(catalog).first().copied().unwrap_or(0);
                }
                _ => {}
            }
            return Decision::Wait;
        }
        match key.code {
            KeyCode::Char('n' | 'N' | 'c' | 'C') => self.open_picker(),
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => self.yes = !self.yes,
            KeyCode::Char('y' | 'Y') if bottom => return Decision::Connect,
            KeyCode::Enter if !self.yes => self.open_picker(),
            KeyCode::Enter if bottom => return Decision::Connect,
            KeyCode::Down => self.offset = self.offset.saturating_add(1),
            KeyCode::Up => self.offset = self.offset.saturating_sub(1),
            KeyCode::PageDown => self.offset = self.offset.saturating_add(page),
            KeyCode::PageUp => self.offset = self.offset.saturating_sub(page),
            _ => {}
        }
        Decision::Wait
    }
    fn review(&self, catalog: &ConnectionCatalog, scope: &StartupScope<'_>) -> Result<String> {
        let mut text = String::new();
        let single = self.single_selection || !scope.dashboard;
        for index in &self.targets {
            let context = &catalog.contexts[*index];
            let config = scope.directory.map_or(Ok(None), |directory| {
                context_config::load_in(directory, &context.name)
            });
            let settings_invalid = config.is_err();
            let config = config.unwrap_or(None);
            let explicit_namespace = if single {
                scope.single_namespace
            } else {
                scope.dashboard_namespace
            };
            let namespace = if !explicit_namespace.is_empty() {
                explicit_namespace.into()
            } else if single {
                config
                    .as_ref()
                    .and_then(context_config::ContextConfig::namespace)
                    .unwrap_or_else(|| {
                        if scope.nodes_default {
                            "*".into()
                        } else {
                            context.namespace.clone()
                        }
                    })
            } else {
                context.namespace.clone()
            };
            let namespace = match namespace.as_str() {
                "*" => "All namespaces",
                name => name,
            };
            let writable = scope.writable
                && !settings_invalid
                && !config.as_ref().is_some_and(|config| config.read_only);
            text.push_str(&format!(
                "Context: {}\nCluster: {}\nAPI host: {}\nNamespace: {namespace}\n",
                context.name, context.cluster, context.server,
            ));
            if single {
                let view = config
                    .as_ref()
                    .and_then(|config| config.view.as_deref())
                    .unwrap_or(if scope.nodes_default { "Nodes" } else { "Pods" });
                text.push_str(&format!("Opens in: {view}\n"));
            }
            text.push_str(if writable {
                "Access: read and write; changes still require review.\n"
            } else {
                "Access: read only.\n"
            });
            if context.insecure_tls {
                text.push_str("TLS verification is disabled by this kubeconfig.\n");
            }
            text.push('\n');
        }
        text.push_str(&format!("Kubeconfig: {}\n", catalog.source));
        text.push_str("Configured login helpers may run after you connect.\n\n");
        text.push_str("Connect to this kubeconfig context?");
        safety::review_display(&text)
    }
}
fn wrap(text: &str, width: usize) -> Vec<String> {
    // Review text is reversible ASCII; byte width equals terminal-cell width.
    text.lines()
        .flat_map(|line| {
            if line.is_empty() {
                vec![String::new()]
            } else {
                line.as_bytes()
                    .chunks(width.max(1))
                    .map(|part| String::from_utf8_lossy(part).into_owned())
                    .collect()
            }
        })
        .collect()
}
fn paint(
    prompt: &mut Prompt,
    catalog: &ConnectionCatalog,
    scope: &StartupScope<'_>,
    theme: &Theme,
    color: bool,
) -> Result<(bool, usize)> {
    let (width, height) = terminal::size().map_err(|_| "Cannot read terminal size")?;
    let mut out = Vec::new();
    let themed = color && !theme.terminal;
    let color = |role| {
        if themed {
            theme.get(role).color()
        } else {
            Color::Reset
        }
    };
    let (fg, bg) = (color("text"), color("background"));
    let (selected_fg, selected_bg) = (color("selection_text"), color("selection_background"));
    let muted = color("muted");
    let selected = |out: &mut Vec<u8>, x, y, width, text: &str| -> io::Result<()> {
        if !themed {
            queue!(out, style::SetAttribute(style::Attribute::Reverse))?;
        }
        cell(out, x, y, width, text, selected_fg, selected_bg)?;
        if !themed {
            queue!(out, style::SetAttribute(style::Attribute::NoReverse))?;
        }
        Ok(())
    };
    queue!(
        out,
        terminal::BeginSynchronizedUpdate,
        themes::BackgroundColor(bg),
        terminal::Clear(ClearType::All)
    )
    .map_err(|_| "Connection prompt draw failed")?;
    if width < 40 || height < 10 {
        row(&mut out, 0, width, "Resize to 40x10; Esc exits", fg, bg)
            .map_err(|_| "Connection prompt draw failed")?;
        present(out)?;
        return Ok((false, 1));
    }
    let page = usize::from(height - 5);
    let title = if prompt.picker {
        "KBorg | Choose an existing context"
    } else {
        "KBorg | Connect to Kubernetes"
    };
    row(&mut out, 0, width, title, color("accent"), bg)
        .map_err(|_| "Connection prompt draw failed")?;
    let bottom;
    if prompt.picker {
        let matches = prompt.matches(catalog);
        let position = matches
            .iter()
            .position(|index| *index == prompt.cursor)
            .unwrap_or(0);
        let start = position.saturating_sub(page.saturating_sub(1));
        row(
            &mut out,
            1,
            width,
            &format!(
                "{}/{} contexts  /  Filter: {}",
                if matches.is_empty() { 0 } else { position + 1 },
                matches.len(),
                safety::safe_text(&prompt.filter, 253),
            ),
            muted,
            bg,
        )
        .map_err(|_| "Connection prompt draw failed")?;
        for (line, index) in matches.iter().skip(start).take(page).enumerate() {
            let active = *index == prompt.cursor;
            let text = format!(
                "{} {} {}",
                if active { ">" } else { " " },
                if Some(*index) == catalog.selected {
                    "*"
                } else {
                    " "
                },
                header_block::context_display(
                    &catalog.contexts[*index].name,
                    usize::from(width - 4)
                ),
            );
            let y = line as u16 + 2;
            if active {
                selected(&mut out, 0, y, width, &text)
            } else {
                row(&mut out, y, width, &text, fg, bg)
            }
            .map_err(|_| "Connection prompt draw failed")?;
        }
        if matches.is_empty() {
            row(
                &mut out,
                2,
                width,
                "No matching context. Backspace edits the filter.",
                fg,
                bg,
            )
            .map_err(|_| "Connection prompt draw failed")?;
        }
        bottom = false;
        row(
            &mut out,
            height - 2,
            width,
            "Enter review  /  Arrows or PgUp/PgDn move",
            muted,
            bg,
        )
        .map_err(|_| "Connection prompt draw failed")?;
        row(
            &mut out,
            height - 1,
            width,
            if prompt.targets.is_empty() {
                "Type to filter  /  Esc exit"
            } else {
                "Type to filter  /  Esc back  /  * startup target"
            },
            muted,
            bg,
        )
        .map_err(|_| "Connection prompt draw failed")?;
    } else {
        let lines = wrap(&prompt.review(catalog, scope)?, usize::from(width));
        prompt.offset = prompt.offset.min(lines.len().saturating_sub(page));
        bottom = prompt.offset + page >= lines.len();
        for (line, text) in lines.iter().skip(prompt.offset).take(page).enumerate() {
            row(&mut out, line as u16 + 2, width, text, fg, bg)
                .map_err(|_| "Connection prompt draw failed")?;
        }
        let buttons = if prompt.yes {
            "[ Connect ]    Choose context"
        } else {
            "  Connect    [ Choose context ]"
        };
        row(&mut out, height - 2, width, buttons, fg, bg)
            .map_err(|_| "Connection prompt draw failed")?;
        if bottom && prompt.yes {
            selected(&mut out, 0, height - 2, 11, "[ Connect ]")
                .map_err(|_| "Connection prompt draw failed")?;
        } else if !prompt.yes {
            selected(&mut out, 13, height - 2, 18, "[ Choose context ]")
                .map_err(|_| "Connection prompt draw failed")?;
        }
        row(
            &mut out,
            height - 1,
            width,
            if bottom {
                "Enter confirm  /  C choose  /  Esc exit"
            } else {
                "Down/PgDn read  /  C choose  /  Esc exit"
            },
            muted,
            bg,
        )
        .map_err(|_| "Connection prompt draw failed")?;
    }
    present(out)?;
    Ok((bottom, page))
}
fn present(mut frame: Vec<u8>) -> Result<()> {
    queue!(frame, terminal::EndSynchronizedUpdate).map_err(|_| "Connection prompt draw failed")?;
    let mut out = io::stdout().lock();
    out.write_all(&frame)
        .and_then(|()| out.flush())
        .map_err(|_| "Connection prompt flush failed".into())
}
pub(super) fn confirm(
    options: &mut Options,
    theme: &Theme,
    single_namespace: String,
    directory: Option<&Path>,
) -> Result<bool> {
    let connection = Connection {
        namespace: single_namespace.clone(),
        ..options.connection.clone()
    };
    let catalog = crate::gateway::connection_catalog(&connection)?;
    let mut prompt = Prompt::new(&catalog, &options.dashboard)?;
    let scope = StartupScope {
        directory,
        single_namespace: &single_namespace,
        dashboard_namespace: &options.connection.namespace,
        dashboard: options.dashboard_last || !options.dashboard.is_empty(),
        nodes_default: options.selector.is_empty(),
        writable: options.connection.writable,
    };
    loop {
        let (bottom, page) = paint(&mut prompt, &catalog, &scope, theme, options.color)?;
        match event::read().map_err(|_| "Connection prompt input failed")? {
            Event::Key(key) => match prompt.key(key, &catalog, bottom, page) {
                Decision::Quit => return Ok(false),
                Decision::Wait => {}
                Decision::Connect => {
                    let Some(index) = prompt.targets.first() else {
                        continue;
                    };
                    // Pin the explicitly approved name; never change the user's
                    // kubeconfig/current-context or ask for copied credentials.
                    options.connection.context = Some(catalog.contexts[*index].name.clone());
                    options.connection.startup_config_fingerprint = Some(catalog.fingerprint);
                    if prompt.single_selection {
                        options.dashboard.clear();
                        options.dashboard_last = false;
                        options.connection.namespace = single_namespace;
                    }
                    return Ok(true);
                }
            },
            Event::Resize(..) => {}
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn catalog() -> ConnectionCatalog {
        ConnectionCatalog {
            fingerprint: [0; 32],
            contexts: vec![
                crate::gateway::ContextSummary {
                    name: "company-prod".into(),
                    cluster: "production".into(),
                    server: "https://prod.example.test".into(),
                    insecure_tls: false,
                    namespace: "team".into(),
                },
                crate::gateway::ContextSummary {
                    name: "company-dev".into(),
                    cluster: "development".into(),
                    server: "https://dev.example.test".into(),
                    insecure_tls: false,
                    namespace: "default".into(),
                },
            ],
            selected: Some(0),
            source: "~/.kube/config".into(),
        }
    }
    fn key(code: KeyCode) -> event::KeyEvent {
        event::KeyEvent::new(code, KeyModifiers::NONE)
    }
    #[test]
    fn yes_no_picker_and_cancel_require_explicit_consent() -> Result<()> {
        let catalog = catalog();
        let mut prompt = Prompt::new(&catalog, &[])?;
        assert_eq!(
            prompt.key(key(KeyCode::Enter), &catalog, false, 10),
            Decision::Wait
        );
        assert_eq!(
            prompt.key(key(KeyCode::Char('n')), &catalog, true, 10),
            Decision::Wait
        );
        assert!(prompt.picker);
        for c in "dev".chars() {
            prompt.key(key(KeyCode::Char(c)), &catalog, true, 10);
        }
        assert_eq!(
            prompt.key(key(KeyCode::Enter), &catalog, true, 10),
            Decision::Wait
        );
        assert!(!prompt.picker);
        assert_eq!(prompt.targets, vec![1]);
        assert_eq!(
            prompt.key(key(KeyCode::Enter), &catalog, true, 10),
            Decision::Connect
        );
        assert_eq!(
            prompt.key(key(KeyCode::Esc), &catalog, true, 10),
            Decision::Quit
        );
        Ok(())
    }
    #[test]
    fn multi_context_and_no_current_are_reviewed_before_connecting() -> Result<()> {
        let mut catalog = catalog();
        let mut prompt = Prompt::new(&catalog, &["company-dev".into()])?;
        let scope = StartupScope {
            directory: None,
            single_namespace: "",
            dashboard_namespace: "",
            dashboard: true,
            nodes_default: true,
            writable: false,
        };
        assert_eq!(prompt.targets, vec![0, 1]);
        let review = prompt.review(&catalog, &scope)?;
        assert!(review.contains("company-prod") && review.contains("company-dev"));
        assert!(review.contains("Access: read only") && review.contains("Namespace: team"));
        assert!(wrap(&review, 40).iter().all(|line| line.len() <= 40));
        assert!(Prompt::new(&catalog, &["unknown".into()]).is_err());
        catalog.contexts[0].insecure_tls = true;
        assert!(
            prompt
                .review(&catalog, &scope)?
                .contains("TLS verification is disabled")
        );
        prompt.key(key(KeyCode::Char('c')), &catalog, true, 10);
        prompt.key(key(KeyCode::End), &catalog, false, 10);
        assert_eq!(prompt.cursor, 1);
        assert_eq!(
            prompt.key(key(KeyCode::Esc), &catalog, false, 10),
            Decision::Wait
        );
        assert_eq!(prompt.targets, vec![0, 1]);
        prompt.key(key(KeyCode::Char('c')), &catalog, true, 10);
        assert_eq!(prompt.cursor, 0);
        prompt.key(key(KeyCode::PageDown), &catalog, false, 10);
        assert_eq!(
            prompt.key(key(KeyCode::Enter), &catalog, false, 10),
            Decision::Wait
        );
        let review = prompt.review(&catalog, &scope)?;
        assert!(review.contains("Namespace: All namespaces") && review.contains("Opens in: Nodes"));
        let explicit = StartupScope {
            single_namespace: "payments",
            ..scope
        };
        assert!(
            prompt
                .review(&catalog, &explicit)?
                .contains("Namespace: payments")
        );
        catalog.selected = None;
        assert!(Prompt::new(&catalog, &[])?.picker);
        Ok(())
    }
}
