//! Helm navigation keeps release storage identities separate from Kubernetes objects.
use crate::{
    Result,
    gateway::Gateway,
    helm::{self, Release, Snapshot, Storage},
    safety,
};
use crossterm::event::KeyCode;

#[derive(Clone)]
pub enum Request {
    List(String, Option<Storage>),
    NamedHistory(String, String, Option<Storage>),
    History(Release),
    Detail(Release),
    Resources(Release),
    Values(Release),
}
impl Request {
    pub fn label(&self) -> &'static str {
        match self {
            Self::List(..) => "Reading Helm release metadata...",
            Self::NamedHistory(..) | Self::History(_) => "Reading release revision history...",
            Self::Detail(_) => "Reading selected revision metadata...",
            Self::Resources(_) => "Reading selected revision resource identities...",
            Self::Values(_) => "Reading and redacting selected revision stored values...",
        }
    }
}
pub enum Response {
    Table(Box<Table>),
    Document(String, String),
}

pub async fn read(gateway: &Gateway, request: Request) -> Result<Response> {
    Ok(match request {
        Request::List(namespace, storage) => Response::Table(Box::new(Table::new(
            helm::list_with_storage(gateway, &namespace, storage).await?,
            None,
            namespace,
            storage,
        ))),
        Request::NamedHistory(namespace, name, storage) => {
            let latest = helm::list_with_storage(gateway, &namespace, storage).await?;
            let release = helm::select_release(&latest, &name, storage)?;
            let snapshot = helm::history(gateway, &release).await?;
            Response::Table(Box::new(Table::new(
                snapshot,
                Some(release),
                namespace,
                storage,
            )))
        }
        Request::History(release) => {
            let storage = Some(release.storage);
            let snapshot = helm::history(gateway, &release).await?;
            let namespace = release.namespace.clone();
            Response::Table(Box::new(Table::new(
                snapshot,
                Some(release),
                namespace,
                storage,
            )))
        }
        Request::Detail(release) => Response::Document(
            "HELM REVISION METADATA".into(),
            helm::detail(gateway, &release).await?.text(),
        ),
        Request::Resources(release) => {
            let detail = helm::detail(gateway, &release).await?;
            let mut lines = vec![
                format!(
                    "{}/{} | revision {} | {} storage",
                    release.namespace,
                    release.name,
                    release.revision,
                    release.storage.name()
                ),
                "Resource identities only; no rendered manifest bodies.".into(),
                String::new(),
            ];
            lines.extend(detail.resources.iter().map(|resource| {
                format!(
                    "{} {} {}/{}",
                    resource.api_version,
                    resource.kind,
                    if resource.namespace.is_empty() {
                        "[namespace omitted / cluster scoped]"
                    } else {
                        &resource.namespace
                    },
                    resource.name
                )
            }));
            if detail.resources_truncated {
                lines.push("INCOMPLETE resource references".into());
            }
            lines.extend(detail.issues);
            Response::Document("HELM RELEASE RESOURCES".into(), lines.join("\n"))
        }
        Request::Values(release) => Response::Document(
            "HELM STORED VALUES / known fields redacted".into(),
            format!(
                "{} storage\n{}",
                release.storage.name(),
                helm::values(gateway, &release).await?.text()?
            ),
        ),
    })
}

pub enum Action {
    None,
    Close,
    Request(Request),
    Metadata(Release),
    Unavailable(&'static str),
}
pub struct Table {
    pub snapshot: Snapshot,
    pub parent: Option<Release>,
    pub namespace: String,
    pub storage: Option<Storage>,
    selected: usize,
    filter: String,
    draft: Option<String>,
    revision_sort: bool,
}
impl Table {
    pub fn new(
        snapshot: Snapshot,
        parent: Option<Release>,
        namespace: String,
        storage: Option<Storage>,
    ) -> Self {
        Self {
            snapshot,
            parent,
            namespace,
            storage,
            selected: 0,
            filter: String::new(),
            draft: None,
            revision_sort: true,
        }
    }
    pub fn title(&self) -> &'static str {
        if self.parent.is_some() {
            "HELM HISTORY / selected stored revisions"
        } else {
            "HELM / releases by namespace and storage"
        }
    }
    pub fn is_filtering(&self) -> bool {
        self.draft.is_some()
    }
    pub fn selected(&self) -> Option<&Release> {
        self.rows().get(self.selected).copied()
    }
    pub fn refresh(&self) -> Request {
        self.parent
            .clone()
            .map(Request::History)
            .unwrap_or_else(|| Request::List(self.namespace.clone(), self.storage))
    }
    fn rows(&self) -> Vec<&Release> {
        let filter = super::nav::Filter::parse(&self.filter).ok();
        let mut rows = self
            .snapshot
            .releases
            .iter()
            .filter(|release| {
                filter.as_ref().is_none_or(|filter| {
                    filter.matches(&format!(
                        "{} {} {} {} {} {} {}",
                        release.namespace,
                        release.name,
                        release.revision,
                        release.status,
                        release.chart_name,
                        release.chart_version,
                        release.storage.name()
                    ))
                })
            })
            .collect::<Vec<_>>();
        if self.parent.is_some() && self.revision_sort {
            rows.sort_by_key(|release| std::cmp::Reverse(release.revision));
        } else {
            rows.sort_by(|a, b| {
                (&a.namespace, &a.name, a.storage)
                    .cmp(&(&b.namespace, &b.name, b.storage))
                    .then_with(|| b.revision.cmp(&a.revision))
            });
        }
        rows
    }
    pub fn key(&mut self, key: KeyCode) -> Action {
        if let Some(draft) = &mut self.draft {
            match key {
                KeyCode::Esc => self.draft = None,
                KeyCode::Enter => match super::nav::Filter::parse(draft) {
                    Ok(_) => {
                        self.filter = self.draft.take().unwrap_or_default();
                        self.selected = 0;
                    }
                    Err(_) => {
                        return Action::Unavailable(
                            "Invalid Helm filter; use a valid regex, !inverse, or -f fuzzy pattern.",
                        );
                    }
                },
                KeyCode::Backspace => {
                    draft.pop();
                }
                KeyCode::Char(c) if !c.is_control() && draft.len() + c.len_utf8() <= 512 => {
                    draft.push(c)
                }
                _ => {}
            }
            return Action::None;
        }
        match key {
            KeyCode::Esc | KeyCode::Char('q') if !self.filter.is_empty() => {
                self.filter.clear();
                self.selected = 0;
            }
            KeyCode::Esc | KeyCode::Char('q') => return Action::Close,
            KeyCode::Char('/') => self.draft = Some(self.filter.clone()),
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.rows().len().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::PageDown => {
                self.selected = (self.selected + 10).min(self.rows().len().saturating_sub(1))
            }
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(10),
            KeyCode::Home | KeyCode::Char('g') => self.selected = 0,
            KeyCode::End | KeyCode::Char('G') => {
                self.selected = self.rows().len().saturating_sub(1)
            }
            KeyCode::Char('N') => {
                self.revision_sort = !self.revision_sort;
                self.selected = 0;
            }
            KeyCode::Char('r') if self.parent.is_some() => {
                return Action::Unavailable(
                    "Helm rollback is unavailable; revision history and stored values are read-only.",
                );
            }
            KeyCode::Enter | KeyCode::Char('r' | 'v' | 'd' | 'm' | 'y') => {
                if let Some(release) = self.selected().cloned() {
                    return match key {
                        KeyCode::Enter if self.parent.is_none() => {
                            Action::Request(Request::History(release))
                        }
                        KeyCode::Char('r') => Action::Request(Request::History(release)),
                        KeyCode::Enter | KeyCode::Char('v') => {
                            Action::Request(Request::Values(release))
                        }
                        KeyCode::Char('d') => Action::Request(Request::Detail(release)),
                        KeyCode::Char('m') => Action::Request(Request::Resources(release)),
                        _ => Action::Metadata(release),
                    };
                }
            }
            KeyCode::Delete => {
                return Action::Unavailable(
                    "Helm uninstall is unavailable; no release mutation was requested.",
                );
            }
            _ => {}
        }
        Action::None
    }
    pub fn lines(&self, width: usize, height: usize) -> Vec<String> {
        let rows = self.rows();
        let mut lines = vec![if let Some(parent) = &self.parent {
            format!(
                "{}/{} | {} storage | {} revisions | {}",
                parent.namespace,
                parent.name,
                parent.storage.name(),
                rows.len(),
                self.snapshot.source
            )
        } else {
            format!(
                "Namespace {} | {} releases | {} | {}",
                self.namespace,
                rows.len(),
                self.snapshot.source,
                if self.snapshot.truncated {
                    "INCOMPLETE"
                } else {
                    "bounded metadata"
                }
            )
        }];
        let issue_count = self.snapshot.issues.len();
        if issue_count > 0 {
            lines.push(format!(
                "ATTENTION: {issue_count} storage/read issues | {}",
                self.snapshot.issues[0]
            ));
        }
        if let Some(draft) = &self.draft {
            lines.push(format!(
                "/{}_| Enter filter | Esc cancel",
                tail(draft, width.saturating_sub(35))
            ));
        } else if !self.filter.is_empty() {
            lines.push(format!("FILTER /{} | Esc clear", self.filter));
        }
        let wide = width >= 114;
        let medium = width >= 64;
        let revision_width = rows
            .iter()
            .map(|release| release.revision.to_string().len())
            .max()
            .unwrap_or(3)
            .max(3);
        let name_width = if wide {
            26usize.saturating_sub(revision_width - 3)
        } else if medium {
            width.saturating_sub(25 + revision_width)
        } else {
            width.saturating_sub(14 + revision_width)
        }
        .max(4);
        lines.push(if wide{"   NAMESPACE        NAME                       REV STATUS           CHART                        APP       STORAGE"}else if medium{"   NAMESPACE / NAME                          REV STATUS     STORAGE"}else{"   NAME         REV ST STATE (S/CM)"}.into());
        let reserved = lines.len() + 1;
        let capacity = height.saturating_sub(reserved).max(1);
        let start = self.selected.saturating_sub(capacity.saturating_sub(1));
        if rows.is_empty() {
            lines.push("No releases match this view. Ctrl-R refreshes metadata.".into());
        }
        for (index, release) in rows.iter().enumerate().skip(start).take(capacity) {
            let mark = if index == self.selected { " >" } else { "  " };
            lines.push(if wide {
                format!(
                    "{mark} {:<16} {:<name_width$} {:>revision_width$} {:<16} {:<28} {:<9} {}",
                    clip(&release.namespace, 16),
                    clip(&release.name, name_width),
                    release.revision,
                    clip(&release.status, 16),
                    clip(
                        &format!("{}-{}", release.chart_name, release.chart_version),
                        28
                    ),
                    clip(&release.app_version, 9),
                    release.storage.name()
                )
            } else if medium {
                format!(
                    "{mark} {:<name_width$} {:>revision_width$} {:<10} {}",
                    clip(
                        &format!("{}/{}", release.namespace, release.name),
                        name_width
                    ),
                    release.revision,
                    clip(&release.status, 10),
                    release.storage.name()
                )
            } else {
                format!(
                    "{mark} {:<name_width$} {:>revision_width$} {:<2} {}",
                    clip(&release.name, name_width),
                    release.revision,
                    if release.storage == Storage::Secret {
                        "S"
                    } else {
                        "CM"
                    },
                    clip(&release.status, 6)
                )
            });
        }
        lines
    }
    pub fn help(&self) -> &'static str {
        if self.parent.is_some() {
            "Enter/v stored values | d metadata | m resources | y YAML | / filter | Esc back"
        } else {
            "Enter/r history | v stored values | d metadata | m resources | / filter | Esc back"
        }
    }
}
fn clip(text: &str, width: usize) -> String {
    safety::safe_text(text, width).chars().take(width).collect()
}
fn tail(text: &str, width: usize) -> String {
    text.chars()
        .rev()
        .take(width)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn release(name: &str, revision: u32, storage: Storage) -> Release {
        Release {
            namespace: "default".into(),
            name: name.into(),
            revision,
            status: "deployed".into(),
            chart_name: "web".into(),
            chart_version: "1.0".into(),
            app_version: "2".into(),
            updated: "now".into(),
            storage,
            uid: format!("uid-{name}-{revision}"),
            resource_version: "1".into(),
        }
    }
    fn snapshot(releases: Vec<Release>) -> Snapshot {
        Snapshot {
            releases,
            truncated: false,
            issues: vec![],
            source: "test".into(),
        }
    }
    #[test]
    fn familiar_list_and_history_keys_capture_selected_revision() {
        let r = release("web", 3, Storage::Secret);
        let mut table = Table::new(
            snapshot(vec![r.clone()]),
            None,
            "default".into(),
            Some(Storage::Secret),
        );
        assert!(matches!(
            table.refresh(),
            Request::List(_, Some(Storage::Secret))
        ));
        assert!(
            matches!(table.key(KeyCode::Enter),Action::Request(Request::History(release)) if release.uid==r.uid)
        );
        let mut history = Table::new(
            snapshot(vec![release("web", 1, Storage::Secret), r.clone()]),
            Some(r.clone()),
            "default".into(),
            None,
        );
        assert_eq!(history.selected().map(|release| release.revision), Some(3));
        history.key(KeyCode::Down);
        assert!(
            matches!(history.key(KeyCode::Enter),Action::Request(Request::Values(release)) if release.revision==1)
        );
        assert!(matches!(
            history.key(KeyCode::Char('r')),
            Action::Unavailable(_)
        ));
    }
    #[test]
    fn duplicate_drivers_require_explicit_selection() {
        let s = snapshot(vec![
            release("web", 1, Storage::Secret),
            release("web", 2, Storage::ConfigMap),
        ]);
        assert!(helm::select_release(&s, "web", None).is_err());
        assert_eq!(
            helm::select_release(&s, "web", Some(Storage::ConfigMap))
                .map(|release| release.revision),
            Ok(2)
        );
    }
    #[test]
    fn filter_cancel_and_compact_frames_are_bounded() {
        let mut table = Table::new(
            snapshot(vec![
                release("web", 1, Storage::Secret),
                release("api", 1, Storage::Secret),
            ]),
            None,
            "default".into(),
            None,
        );
        table.key(KeyCode::Char('/'));
        table.key(KeyCode::Char('w'));
        table.key(KeyCode::Esc);
        assert_eq!(table.rows().len(), 2);
        table.key(KeyCode::Char('/'));
        for c in "^default web ".chars() {
            table.key(KeyCode::Char(c));
        }
        table.key(KeyCode::Enter);
        assert_eq!(table.rows().len(), 1);
        assert!(matches!(table.key(KeyCode::Esc), Action::None));
        assert_eq!(table.rows().len(), 2);
        assert!(table.lines(26, 4).iter().all(|line| !line.contains('\x1b')));
    }
}
