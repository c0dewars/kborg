//! Explicit saved navigation, independent of operation authorization.
use super::*;
use crate::selections::{Selection, Target, Workload};

pub(super) const SAVE_LABEL: &str = "Save selection / name";
pub(super) const DELETE_LABEL: &str = "Delete selection / type exact name";
pub(super) const PICKER_LABEL: &str = "Saved selections";

#[derive(Default)]
pub(super) struct State {
    pub draft: Option<Selection>,
    pub records: Vec<Selection>,
    pub deleting: Option<String>,
    pub restoring: Option<Selection>,
    pub focus: Option<Workload>,
}

impl App {
    pub(super) fn open_saved_selections(&mut self) -> Result<()> {
        if self.busy || self.write_inflight {
            self.status = "Wait for the current request before opening saved selections.".into();
            return Ok(());
        }
        let records = match crate::selections::list() {
            Ok(records) => records,
            Err(error) => {
                let previous = std::mem::replace(
                    &mut self.modal,
                    Modal::AdviceDocument("SAVED SELECTIONS UNAVAILABLE".into(), error, 0),
                );
                self.push_subview(previous);
                self.status =
                    "Saved selections could not be read; stored preferences were retained.".into();
                return Ok(());
            }
        };
        let mut labels: Vec<String> = records
            .iter()
            .map(|record| match &record.target {
                Target::Dashboard {
                    contexts,
                    namespace,
                } => format!(
                    "{}  |  Dashboard  |  {} context{}  |  {}{}",
                    record.name,
                    contexts.len(),
                    if contexts.len() == 1 { "" } else { "s" },
                    namespace,
                    if record.demo { "  |  DEMO" } else { "" }
                ),
                Target::Resource {
                    context,
                    namespace,
                    kind,
                    workload,
                    ..
                } => format!(
                    "{}  |  {}  |  {}/{}{}{}",
                    record.name,
                    kind,
                    context,
                    namespace,
                    workload
                        .as_ref()
                        .map(|item| format!(" / {}", item.name))
                        .unwrap_or_default(),
                    if record.demo { "  |  DEMO" } else { "" }
                ),
            })
            .collect();
        if records.is_empty() {
            labels.push(
                "No saved selections yet. Use b in a resource view or s in the dashboard.".into(),
            );
        }
        self.selection_ui.records = records;
        let previous = std::mem::replace(&mut self.modal, Modal::Picker(PICKER_LABEL, labels, 0));
        self.push_subview(previous);
        self.status = "Enter opens after revalidation; d removes a selection; Esc returns.".into();
        Ok(())
    }

    pub(super) fn save_dashboard_selection(&mut self) -> Result<()> {
        let Modal::LiveOverview(dashboard) = &self.modal else {
            return Ok(());
        };
        let selection = Selection {
            name: String::new(),
            demo: self.options.connection.demo,
            target: Target::Dashboard {
                contexts: dashboard
                    .rows
                    .iter()
                    .map(|row| row.context.clone())
                    .collect(),
                namespace: self.namespace.clone(),
            },
        };
        self.begin_selection_save(selection);
        Ok(())
    }

    pub(super) fn save_resource_selection(&mut self) -> Result<()> {
        if self.busy || self.stale || !self.connected {
            self.status = "Wait for a current resource view before saving its selection.".into();
            return Ok(());
        }
        if !self.field_selector.is_empty() {
            self.status =
                "Save from a resource or workload view without a relationship filter.".into();
            return Ok(());
        }
        let kind = self.kind();
        let namespaced = self
            .kinds
            .iter()
            .find(|item| item.key() == kind)
            .is_some_and(|item| item.namespaced);
        let workload = self.visible().get(self.selected).and_then(|object| {
            let supported = matches!(
                object["kind"].as_str(),
                Some("Deployment" | "StatefulSet" | "DaemonSet" | "ReplicaSet" | "Job" | "CronJob")
            );
            supported.then(|| Workload {
                namespace: object["metadata"]["namespace"]
                    .as_str()
                    .unwrap_or("")
                    .into(),
                name: object["metadata"]["name"].as_str().unwrap_or("").into(),
                uid: object["metadata"]["uid"].as_str().unwrap_or("").into(),
            })
        });
        self.begin_selection_save(Selection {
            name: String::new(),
            demo: self.options.connection.demo,
            target: Target::Resource {
                context: self.context.clone(),
                namespace: if namespaced {
                    self.namespace.clone()
                } else {
                    "*".into()
                },
                kind,
                label_selector: self.selector.clone(),
                workload,
            },
        });
        Ok(())
    }

    pub(super) fn begin_selection_save(&mut self, selection: Selection) {
        self.selection_ui.draft = Some(selection);
        let previous = std::mem::replace(&mut self.modal, Modal::Input(SAVE_LABEL, String::new()));
        self.push_subview(previous);
        self.status = "Save navigation only. Credentials, filters over cached rows and approvals are excluded.".into();
    }

    pub(super) fn commit_selection_name(&mut self, text: &str) {
        let Some(mut selection) = self.selection_ui.draft.clone() else {
            return;
        };
        selection.name = text.to_owned();
        match crate::selections::save(&selection) {
            Ok(()) => {
                self.selection_ui.draft = None;
                self.restore_selection_view();
                self.status = format!("Selection '{}' saved locally.", selection.name);
                if let Modal::LiveOverview(dashboard) = &mut self.modal {
                    dashboard.set_notice(self.status.clone(), false);
                }
            }
            Err(error) => {
                self.status = error;
                self.modal = Modal::Input(SAVE_LABEL, text.to_owned());
            }
        }
    }

    pub(super) fn restore_selection_view(&mut self) {
        self.modal = self.subviews.pop().unwrap_or(Modal::None);
        if let Modal::LiveOverview(dashboard) = &mut self.modal
            && let Some(feed) = &self.overview
            && let Ok(rows) = feed.snapshot()
        {
            dashboard.update(rows);
        }
    }

    pub(super) fn confirm_selection_delete(&mut self, index: usize) {
        if let Some(record) = self.selection_ui.records.get(index) {
            self.selection_ui.deleting = Some(record.name.clone());
            self.status = format!(
                "Remove saved selection '{}'; cluster resources stay unchanged.",
                record.name
            );
            self.modal = Modal::Input(DELETE_LABEL, String::new());
        }
    }

    pub(super) fn delete_selection_name(&mut self, text: &str) {
        if self.selection_ui.deleting.as_deref() != Some(text) {
            self.status = "Type the exact saved selection name to remove it.".into();
            self.modal = Modal::Input(DELETE_LABEL, text.into());
            return;
        }
        match crate::selections::delete(text) {
            Ok(()) => {
                self.selection_ui.deleting = None;
                self.restore_selection_view();
                self.status = format!("Selection '{text}' removed.");
                if let Modal::LiveOverview(dashboard) = &mut self.modal {
                    dashboard.set_notice(self.status.clone(), false);
                }
            }
            Err(error) => {
                self.status = error;
                self.modal = Modal::Input(DELETE_LABEL, text.into());
            }
        }
    }

    pub(super) fn open_selection(&mut self, selection: Selection) -> Result<()> {
        if self.busy || self.write_inflight {
            return Err("Wait for the current request before restoring a selection.".into());
        }
        selection.validate_contexts(&self.options.connection)?;
        self.selection_ui.restoring = None;
        self.selection_ui.focus = None;
        match &selection.target {
            Target::Dashboard {
                contexts,
                namespace,
            } => {
                self.namespace = namespace.clone();
                self.namespace_override = true;
                self.changed();
                self.open_dashboard(contexts.clone())?;
            }
            Target::Resource {
                context,
                namespace,
                kind,
                label_selector,
                ..
            } => {
                self.push_view();
                self.options.connection.context = Some(context.clone());
                self.options.connection.namespace = namespace.clone();
                self.namespace = namespace.clone();
                self.desired_resource = Some(kind.clone());
                self.filter.clear();
                self.selector = label_selector.clone();
                self.field_selector.clear();
                self.modal = Modal::None;
                self.connect();
                self.namespace_override = true;
                self.selection_ui.restoring = Some(selection);
                self.refresh = false;
                self.status =
                    "Revalidating saved resource against destination API discovery...".into();
            }
        }
        Ok(())
    }

    pub(super) fn finish_saved_selection_restore(&mut self) {
        let Some(selection) = self.selection_ui.restoring.take() else {
            return;
        };
        match selection.validate_scope(&self.kinds) {
            Ok(()) => {
                if let Target::Resource { workload, .. } = selection.target {
                    self.selection_ui.focus = workload;
                }
                self.refresh = true;
                self.watch_blocked = false;
                self.status = "Saved scope revalidated; loading current resources.".into();
            }
            Err(error) => {
                self.refresh = false;
                self.watch_blocked = true;
                self.status = format!("Saved selection unavailable: {error}");
                self.sticky_status = true;
            }
        }
    }

    pub(super) fn finish_saved_workload_focus(&mut self) -> bool {
        if self.stale || !self.connected || self.selection_ui.restoring.is_some() {
            return false;
        }
        let Some(workload) = self.selection_ui.focus.take() else {
            return false;
        };
        if let Some(index) = self
            .visible()
            .iter()
            .position(|item| workload.matches(item))
        {
            self.selected = index;
            self.status = "Saved workload identity verified against the current list.".into();
        } else {
            self.status = "Saved workload is absent, replaced or outside this bounded list; choose a current resource.".into();
            self.modal = Modal::Detail(
                format!(
                    "SAVED WORKLOAD UNAVAILABLE\n\n{}/{}\n\nThe saved UID was not found in the current resource list. No replacement was opened. Esc returns to the current list.",
                    workload.namespace, workload.name
                ),
                0,
            );
        }
        self.sticky_status = true;
        true
    }
}
