//! Local-only startup consent. No worker, client, or credential helper exists
//! until this screen returns an explicit Yes.
use super::*;
use crate::gateway::ConnectionCatalog;

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
        if key.code == KeyCode::Esc
            || (key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c'))
        {
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
            match key.code {
                KeyCode::Up => {
                    if let Some(index) = matches.get(position.saturating_sub(1)) {
                        self.cursor = *index;
                    }
                }
                KeyCode::Down => {
                    if let Some(index) =
                        matches.get((position + 1).min(matches.len().saturating_sub(1)))
                    {
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
            KeyCode::Char('n' | 'N') => self.picker = true,
            KeyCode::Left | KeyCode::Right | KeyCode::Tab => self.yes = !self.yes,
            KeyCode::Char('y' | 'Y') if bottom => return Decision::Connect,
            KeyCode::Enter if !self.yes => self.picker = true,
            KeyCode::Enter if bottom => return Decision::Connect,
            KeyCode::Down => self.offset = self.offset.saturating_add(1),
            KeyCode::Up => self.offset = self.offset.saturating_sub(1),
            KeyCode::PageDown => self.offset = self.offset.saturating_add(page),
            KeyCode::PageUp => self.offset = self.offset.saturating_sub(page),
            _ => {}
        }
        Decision::Wait
    }
    fn review(&self, catalog: &ConnectionCatalog, writable: bool) -> Result<String> {
        let mut text = format!("Kubeconfig: {}\n\n", catalog.source);
        for index in &self.targets {
            let context = &catalog.contexts[*index];
            text.push_str(&format!(
                "Context: {}\nCluster: {}\nAPI host: {}\nNamespace: {}\n\n",
                context.name, context.cluster, context.server, context.namespace
            ));
            if context.insecure_tls {
                text.push_str("TLS verification is disabled by this kubeconfig.\n\n");
            }
        }
        text.push_str("Use the credentials already configured by your cluster CLI.\nNo token setup is needed in KBorg.\nConfigured login helpers may run after you choose Yes.\n\n");
        text.push_str(if writable {
            "Access: read and write; changes still require review.\n"
        } else {
            "Access: read only.\n"
        });
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
    options: &Options,
    theme: &Theme,
) -> Result<(bool, usize)> {
    let (width, height) = terminal::size().map_err(|_| "Cannot read terminal size")?;
    let mut out = io::stdout();
    let fg = if options.color {
        theme.get("text").color()
    } else {
        Color::Reset
    };
    let bg = if options.color {
        theme.get("background").color()
    } else {
        Color::Reset
    };
    execute!(
        out,
        themes::BackgroundColor(bg),
        terminal::Clear(ClearType::All)
    )
    .map_err(|_| "Connection prompt draw failed")?;
    if width < 40 || height < 10 {
        row(&mut out, 0, width, "Resize to 40x10; Esc exits", fg, bg)
            .map_err(|_| "Connection prompt draw failed")?;
        out.flush().map_err(|_| "Connection prompt flush failed")?;
        return Ok((false, 1));
    }
    let page = usize::from(height - 5);
    let title = if prompt.picker {
        "KBorg | Choose an existing context"
    } else {
        "KBorg | Connect to Kubernetes"
    };
    row(&mut out, 0, width, title, fg, bg).map_err(|_| "Connection prompt draw failed")?;
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
            &format!("Filter: {}", safety::safe_text(&prompt.filter, 253)),
            fg,
            bg,
        )
        .map_err(|_| "Connection prompt draw failed")?;
        for (line, index) in matches.iter().skip(start).take(page).enumerate() {
            let text = format!(
                "{} {}",
                if *index == prompt.cursor { ">" } else { " " },
                catalog.contexts[*index].name
            );
            row(&mut out, line as u16 + 2, width, &text, fg, bg)
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
            "Type to filter | Arrows select | Enter reviews | Esc exits",
            fg,
            bg,
        )
        .map_err(|_| "Connection prompt draw failed")?;
    } else {
        let lines = wrap(
            &prompt.review(catalog, options.connection.writable)?,
            usize::from(width),
        );
        prompt.offset = prompt.offset.min(lines.len().saturating_sub(page));
        bottom = prompt.offset + page >= lines.len();
        for (line, text) in lines.iter().skip(prompt.offset).take(page).enumerate() {
            row(&mut out, line as u16 + 2, width, text, fg, bg)
                .map_err(|_| "Connection prompt draw failed")?;
        }
        let buttons = if bottom {
            if prompt.yes {
                "[ Yes ]   No     Enter / Y connects; N chooses another"
            } else {
                "  Yes   [ No ]   Enter chooses another; Y connects"
            }
        } else {
            "Down / PgDn: review remaining details before connecting"
        };
        row(&mut out, height - 2, width, buttons, fg, bg)
            .map_err(|_| "Connection prompt draw failed")?;
        row(
            &mut out,
            height - 1,
            width,
            "Left/Right: Yes/No | N: another context | Esc: exit",
            fg,
            bg,
        )
        .map_err(|_| "Connection prompt draw failed")?;
    }
    out.flush().map_err(|_| "Connection prompt flush failed")?;
    Ok((bottom, page))
}
pub(super) fn confirm(options: &mut Options, theme: &Theme) -> Result<bool> {
    let catalog = crate::gateway::connection_catalog(&options.connection)?;
    let mut prompt = Prompt::new(&catalog, &options.dashboard)?;
    loop {
        let (bottom, page) = paint(&mut prompt, &catalog, options, theme)?;
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
        let prompt = Prompt::new(&catalog, &["company-dev".into()])?;
        assert_eq!(prompt.targets, vec![0, 1]);
        let review = prompt.review(&catalog, false)?;
        assert!(review.contains("company-prod") && review.contains("company-dev"));
        assert!(review.contains("Access: read only"));
        assert!(wrap(&review, 40).iter().all(|line| line.len() <= 40));
        assert!(Prompt::new(&catalog, &["unknown".into()]).is_err());
        catalog.contexts[0].insecure_tls = true;
        assert!(
            prompt
                .review(&catalog, false)?
                .contains("TLS verification is disabled")
        );
        catalog.selected = None;
        assert!(Prompt::new(&catalog, &[])?.picker);
        Ok(())
    }
}
