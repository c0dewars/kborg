//! Applying worker replies and mailbox updates to application state.
use super::*;

impl App {
    pub(super) fn receive(&mut self) -> bool {
        let mut changed = self.expire_table_metrics(Instant::now());
        if let Modal::Secrets(view) = &mut self.modal
            && view.tick()
        {
            changed = true;
        }
        let forward = self.worker.forward.text();
        if forward != self.last_forward {
            self.last_forward = forward;
            self.forward_selected = self
                .forward_selected
                .min(self.worker.forward.len().saturating_sub(1));
            changed = true;
        }
        while let Ok((epoch, reply)) = self.worker.rx.try_recv() {
            changed = true;
            // Background replies never end a user request's busy/write state.
            let command_result = self.command_epoch == Some(epoch)
                && !matches!(
                    &reply,
                    Reply::Metrics(..) | Reply::Overview(..) | Reply::ScanOutcome(..)
                );
            let write_result = command_result && self.write_inflight;
            if command_result {
                self.busy = false;
                self.write_inflight = false;
                self.command_epoch = None;
            }
            let connection_result =
                matches!(&reply, Reply::Connected(..)) && self.connecting_epoch == Some(epoch);
            if matches!(&reply, Reply::Connected(..)) && !connection_result {
                continue;
            }
            // A detached scan outcome always lands: it ends the ScanRunning modal.
            let scan_result = matches!(&reply, Reply::ScanOutcome(..));
            if epoch != self.epoch && !connection_result && !write_result && !scan_result {
                continue;
            }
            match reply {
                Reply::YamlDraft(draft) => {
                    self.modal = Modal::YamlEditor(Box::new(crate::ui_yaml::Editor::new(*draft)));
                    self.status = "Edit spec fields. Ctrl-S prepares a diff; Esc cancels.".into();
                }
                Reply::YamlPrepared(editor, result) => match result {
                    Ok(prepared) => {
                        let view = review_ui::Review::new(
                            prepared.summary().into(),
                            prepared.confirmation(),
                            self.epoch,
                        );
                        self.push_subview(Modal::YamlEditor(editor));
                        self.modal = Modal::WriteReview(Box::new(WriteReview {
                            plan: WritePlan::Yaml(Box::new(prepared)),
                            view,
                        }));
                        self.status = "Read each change, then type the target to apply.".into();
                    }
                    Err(error) => {
                        self.modal = Modal::YamlEditor(editor);
                        self.status = error;
                    }
                },
                Reply::NodePrepared(prepared) => {
                    let view = review_ui::Review::new(
                        prepared.summary().into(),
                        prepared.confirmation(),
                        self.epoch,
                    );
                    self.modal = Modal::WriteReview(Box::new(WriteReview {
                        plan: WritePlan::Node(prepared),
                        view,
                    }));
                    self.status = "Review the node and workload inventory before approval.".into();
                }
                Reply::NodeShellPrepared(prepared) => {
                    let view = review_ui::Review::new(
                        prepared.summary().into(),
                        prepared.confirmation(),
                        self.epoch,
                    );
                    self.modal = Modal::WriteReview(Box::new(WriteReview {
                        plan: WritePlan::NodeShell(prepared),
                        view,
                    }));
                    self.status =
                        "Read the whole privileged Pod, then type the confirmation.".into();
                }
                Reply::DebugPrepared(prepared) => {
                    let view = review_ui::Review::new(
                        prepared.summary().into(),
                        prepared.confirmation(),
                        self.epoch,
                    );
                    self.modal = Modal::WriteReview(Box::new(WriteReview {
                        plan: WritePlan::Debug(prepared),
                        view,
                    }));
                    self.status = "Read the debug container, then type the confirmation.".into();
                }
                Reply::NodeShellReady(reviewed) => {
                    let typed = reviewed.confirmation();
                    self.modal = Modal::None;
                    self.pending_session = Some((reviewed, typed));
                }
                Reply::ApplyPrepared(prepared) => {
                    let view = review_ui::Review::new(
                        prepared.summary().into(),
                        prepared.confirmation(),
                        self.epoch,
                    );
                    self.modal = Modal::WriteReview(Box::new(WriteReview {
                        plan: WritePlan::Apply(prepared),
                        view,
                    }));
                    self.status = "Read every change, then type the confirmation to apply.".into();
                }
                Reply::BatchPrepared(batch) => {
                    let view = review_ui::Review::new(
                        batch.summary().into(),
                        batch.confirmation(),
                        self.epoch,
                    );
                    self.status = format!(
                        "BATCH of {}: read every target, then type '{}' to execute.",
                        batch.len(),
                        batch.confirmation()
                    );
                    self.modal = Modal::WriteReview(Box::new(WriteReview {
                        plan: WritePlan::Batch(batch),
                        view,
                    }));
                }
                Reply::BatchOutcome(result) => {
                    self.subviews.clear();
                    self.stale = true;
                    self.refresh = true;
                    let text = match result {
                        Ok(report) => {
                            if report.complete() {
                                self.marks.clear();
                            } else {
                                for target in &report.succeeded {
                                    self.marks.remove(&target.uid);
                                }
                            }
                            report.text()
                        }
                        Err(error) => format!(
                            "Batch not started\n\n{error}\n\nMarks are kept; prepare and review again."
                        ),
                    };
                    self.modal = Modal::Detail(text, 0);
                    self.status = "Batch finished. Inspect the outcome before continuing.".into();
                }
                Reply::WriteOutcome(result) => {
                    self.subviews.clear();
                    self.stale = true;
                    self.refresh = true;
                    self.modal = Modal::Detail(
                        match result {
                            Ok(text) => text,
                            Err(error) => format!(
                                "Operation stopped\n\n{error}\n\nInspect current cluster state before retrying."
                            ),
                        },
                        0,
                    );
                    self.status =
                        "Operation finished. Inspect the outcome before continuing.".into();
                }
                Reply::Export(draft) => {
                    let previous = std::mem::replace(&mut self.modal, Modal::Export(draft));
                    self.push_subview(previous);
                    self.status = "Review the local export; Enter saves, Esc cancels.".into();
                }
                Reply::Connected(c, s, n, k) => {
                    let selected_kind = self.desired_resource.take().unwrap_or_else(|| self.kind());
                    self.connecting_epoch = None;
                    self.context = c;
                    self.server = s;
                    if !self.namespace_override {
                        self.namespace = n;
                    }
                    self.kinds = k;
                    let retained_kind = self
                        .kinds
                        .iter()
                        .position(|kind| kind.key() == selected_kind);
                    self.kind_index = retained_kind.unwrap_or(0);
                    self.desired_resource = retained_kind.is_none().then_some(selected_kind);
                    self.connected = true;
                    self.refresh = retained_kind.is_some();
                    self.watch_blocked = retained_kind.is_none();
                    self.status = if retained_kind.is_some() {
                        "Connected. READ ONLY unless explicitly enabled."
                    } else {
                        "Restoring selected resource through destination API discovery..."
                    }
                    .into();
                    if !self.options.connection.demo {
                        if retained_kind.is_some() && self.selection_ui.restoring.is_none() {
                            // Show rows first; discovery follows the watch.
                            self.pending_discovery = true;
                        } else {
                            self.send(Task::Discover);
                            if self.selection_ui.restoring.is_some() {
                                self.refresh = false;
                            }
                        }
                    } else {
                        self.finish_saved_selection_restore();
                    }
                }
                Reply::Items(value) => {
                    self.replace_items(value["items"].as_array().cloned().unwrap_or_default());
                    self.truncated = value["truncated"] == true;
                    self.selected = self.selected.min(self.visible().len().saturating_sub(1));
                    self.stale = false;
                    self.status = if self.bounded_poll {
                        format!(
                            "Showing the first {} objects: this scope is too large to watch live. Refreshing every 30s; /-l LABELS or :ns narrows it.",
                            self.items.len()
                        )
                    } else {
                        format!(
                            "{} objects | cached filter | refresh every 5s",
                            self.items.len()
                        )
                    };
                }
                Reply::ScanPrepared(result) => match result {
                    Ok(plan) => {
                        self.status =
                            "Review the scanner and images; Enter or type scan to start.".into();
                        self.modal = Modal::ScanConfirm(plan, String::new());
                    }
                    Err(error) => {
                        self.modal = Modal::Detail(
                            format!(
                                "IMAGE SCAN NOT STARTED\n\n{}",
                                safety::safe_lines(&error, 8192)
                            ),
                            0,
                        );
                    }
                },
                Reply::ScanStarted => {}
                Reply::ScanOutcome(result) => {
                    let canceled = self.scan_control.take().is_some_and(|c| c.canceled());
                    let text = match result {
                        Ok(report) => crate::scan::report_text(&report),
                        Err(error) => format!("IMAGE SCAN STOPPED\n\n{error}"),
                    };
                    self.status = if canceled {
                        "Scan canceled; partial results shown.".into()
                    } else {
                        "Scan finished. Read-only; nothing in the cluster was changed.".into()
                    };
                    self.modal = Modal::Detail(text, 0);
                }
                Reply::AdvicePrepared(result) => {
                    if !matches!(self.modal, Modal::AdviceLoading) {
                        continue;
                    }
                    self.modal = match result {
                        Ok((prepared, text)) => {
                            self.status = "Copilot preview ready; nothing sent.".into();
                            Modal::AdviceReview(Box::new(advice_ui::Review::new(
                                prepared, text, self.epoch,
                            )))
                        }
                        Err(error) => Modal::AdviceDocument(
                            "COPILOT SETUP / no request sent".into(),
                            error,
                            0,
                        ),
                    };
                }
                Reply::AdviceOutcome(result) => {
                    self.advice_cancel = None;
                    self.modal = match result {
                        Ok(output) => Modal::AdviceDocument(
                            "COPILOT ADVICE / review independently".into(),
                            format!(
                                "READ-ONLY ADVICE / no commands executed\n\n{}\n\nCheck suggestions against live state before taking action.",
                                safety::safe_lines(&output.answer, 64 * 1024)
                            ),
                            0,
                        ),
                        Err(error) => {
                            Modal::AdviceDocument("COPILOT REQUEST STOPPED".into(), error, 0)
                        }
                    };
                    self.status = "Copilot request finished. No automatic remediation.".into();
                }
                Reply::HelmPrepared(generation, result) => {
                    if generation != self.helm_generation
                        || !matches!(self.modal, Modal::HelmLoading(_))
                    {
                        continue;
                    }
                    self.modal = match result {
                        Ok(prepared) => {
                            self.status = "Helm review ready; no execution requested.".into();
                            self.sticky_status = false;
                            Modal::HelmReview(Box::new(helm_ui::Review::new(prepared, self.epoch)))
                        }
                        Err(error) => {
                            self.status = error.clone();
                            Modal::HelmDocument(
                                "HELM PREPARATION FAILED / no execution requested".into(),
                                error,
                                0,
                            )
                        }
                    };
                }
                Reply::HelmOutcome(generation, result) => {
                    if generation != self.helm_generation && !write_result {
                        continue;
                    }
                    self.helm_cancel = None;
                    self.subviews.retain(|modal| {
                        !matches!(
                            modal,
                            Modal::Helm(..)
                                | Modal::HelmForm(..)
                                | Modal::HelmReview(..)
                                | Modal::HelmLoading(..)
                        )
                    });
                    let (title, text) = match result {
                        Ok(outcome) => {
                            let title = if outcome.uncertain {
                                "HELM OUTCOME UNKNOWN"
                            } else if outcome.success
                                && outcome.summary.contains("STAGED / finalization pending")
                            {
                                "CRD MIGRATION STAGED / finalization pending"
                            } else if outcome.success {
                                "HELM OPERATION SUCCEEDED"
                            } else {
                                "HELM OPERATION FAILED"
                            };
                            (title.to_owned(), outcome.summary)
                        }
                        Err(error) => (
                            "HELM OUTCOME UNKNOWN / reconcile before retrying".into(),
                            error,
                        ),
                    };
                    self.status = title.clone();
                    self.sticky_status = true;
                    self.refresh = true;
                    self.stale = true;
                    self.watching = false;
                    self.worker.watch.invalidate(self.epoch);
                    self.modal = Modal::HelmOutcome(title, text, 0);
                }
                Reply::Helm(generation, result) => {
                    if generation != self.helm_generation
                        || !matches!(self.modal, Modal::HelmLoading(_))
                    {
                        continue;
                    }
                    self.modal = match result {
                        Ok(helm_ui::Response::Table(table)) => Modal::Helm(table),
                        Ok(helm_ui::Response::Document(title, text)) => {
                            Modal::HelmDocument(title, text, 0)
                        }
                        Err(error) => {
                            self.status = error.clone();
                            Modal::HelmDocument("HELM READ UNAVAILABLE".into(), error, 0)
                        }
                    };
                }
                Reply::Detail(text) => {
                    self.modal = Modal::Detail(text.replace('\t', " | "), 0);
                }
                Reply::Object(object) => self.modal = Modal::Overview(object, 0),
                Reply::Drill(object) => self.modal = Modal::PodContainers(object, 0),
                Reply::Directory(directory) => self.modal = Modal::Directory(directory, 0),
                Reply::Subjects(title, namespace, index) => {
                    let modal = std::mem::replace(&mut self.modal, Modal::None);
                    if !matches!(modal, Modal::None) {
                        self.push_subview(modal);
                    }
                    self.modal = Modal::Subjects(Box::new(crate::ui_subjects::View::new(
                        title, namespace, index,
                    )));
                }
                Reply::Secrets(index) => {
                    let modal = std::mem::replace(&mut self.modal, Modal::None);
                    if !matches!(modal, Modal::None) {
                        self.push_subview(modal);
                    }
                    self.modal = Modal::Secrets(Box::new(crate::ui_secrets::View::new(index)));
                }
                Reply::DashboardWorkloads(lists) => {
                    if let Modal::LiveOverview(dashboard) = &mut self.modal
                        && let Some(picker) = dashboard.picker.as_mut()
                        && picker.loading
                    {
                        picker.loading = false;
                        for (context, result) in lists {
                            match result {
                                Ok(found) => picker.entries.extend(found),
                                Err(error) => picker.errors.push(format!("{context}: {error}")),
                            }
                        }
                        // Pre-select the last tail's workloads by name; the
                        // fresh listing supplies their current UIDs.
                        if let Ok(Some(last)) = crate::dashboard_memory::load() {
                            for (index, entry) in picker.entries.iter().enumerate() {
                                if last.picks(entry)
                                    && picker.checked.len() < crate::workload_tail::MAX_SOURCES
                                {
                                    picker.checked.insert(index);
                                }
                            }
                        }
                    }
                }
                Reply::SecretValue(uid, key, value) => {
                    if let Modal::Secrets(view) = &mut self.modal {
                        view.reveal(&uid, &key, value);
                    }
                }
                Reply::SecretYaml(uid, doc) => {
                    if let Modal::Secrets(view) = &mut self.modal {
                        view.yaml(&uid, doc);
                    }
                }
                Reply::Overview(feed) => {
                    if matches!(self.modal, Modal::LiveOverview(..)) {
                        match feed.snapshot() {
                            Ok(rows) => {
                                if let Modal::LiveOverview(dashboard) = &mut self.modal {
                                    dashboard.update(rows);
                                }
                                self.overview = Some(feed);
                            }
                            Err(error) => {
                                self.modal =
                                    Modal::Detail(format!("Overview unavailable\n{error}"), 0);
                            }
                        }
                    }
                }
                Reply::SessionPrepared(prepared) => {
                    self.modal = Modal::SessionConfirm(prepared, String::new())
                }
                Reply::ForwardQueued(id) => {
                    self.forward_selected = self.worker.forward.index(id);
                    self.modal = Modal::ForwardView;
                    self.status =
                        "Forward approved. Esc returns to resources; :pf reopens sessions.".into();
                }
                Reply::Membership(target, generation) => {
                    if generation
                        != self
                            .worker
                            .logs
                            .generation
                            .load(std::sync::atomic::Ordering::SeqCst)
                        || !matches!(self.modal, Modal::Follow(_))
                    {
                        continue;
                    }
                    self.log_target = Some(target);
                    if let Modal::Follow(mut view) = std::mem::replace(&mut self.modal, Modal::None)
                    {
                        view.enable_all_containers(false);
                        let read = view.range;
                        self.reload_logs(view, read);
                    }
                }
                Reply::Containers(target, generation, names) => {
                    if self
                        .worker
                        .logs
                        .generation
                        .load(std::sync::atomic::Ordering::SeqCst)
                        != generation
                        || !matches!(self.modal, Modal::Containers(..))
                    {
                        continue;
                    }
                    if names.is_empty() {
                        self.status = "No containers found in the selected pod.".into();
                    } else {
                        self.modal = Modal::Containers(target, names, 0);
                    }
                }
                Reply::Logs(generation, text) => {
                    if self
                        .worker
                        .logs
                        .generation
                        .load(std::sync::atomic::Ordering::SeqCst)
                        == generation
                        && let Modal::Follow(view) = &mut self.modal
                    {
                        view.update(text);
                    }
                }
                Reply::LogFailure(generation, error) => {
                    if self
                        .worker
                        .logs
                        .generation
                        .load(std::sync::atomic::Ordering::SeqCst)
                        == generation
                        && matches!(self.modal, Modal::Follow(..) | Modal::Containers(..))
                    {
                        self.status = error.clone();
                        self.modal = Modal::Detail(format!("LOGS unavailable\n{error}"), 0);
                    } else if self
                        .worker
                        .logs
                        .generation
                        .load(std::sync::atomic::Ordering::SeqCst)
                        == generation
                        && let Modal::LiveOverview(dashboard) = &mut self.modal
                        && let Some(pane) = dashboard.logs.as_mut()
                    {
                        pane.view
                            .replace_source(format!("[Logs unavailable: {error}]"));
                    }
                }
                Reply::Following => {}
                Reply::Catalog(k) => {
                    let pending = self.desired_resource.take();
                    let current = self.kind();
                    let resource = pending.clone().unwrap_or_else(|| current.clone());
                    self.kinds = k;
                    if let Ok(index) = self.resolve_resource(&resource) {
                        self.kind_index = index;
                        // The resource already on screen keeps its live watch;
                        // only a pending or restored selection restarts it.
                        let same_view = pending.is_none()
                            && self.selection_ui.restoring.is_none()
                            && self.kind() == current;
                        if !same_view {
                            let restoring = self.selection_ui.restoring.take();
                            self.changed();
                            self.selection_ui.restoring = restoring;
                        }
                        self.status="API discovery complete; all discovered aliases available (Secrets excluded).".into();
                        self.finish_saved_selection_restore();
                    } else {
                        self.desired_resource = Some(resource.clone());
                        self.watch_blocked = true;
                        self.refresh = false;
                        self.status = format!(
                            "Resource '{resource}' is unavailable in this context; : selects another resource."
                        );
                    }
                }
                Reply::CatalogFailure(error) => {
                    self.status =
                        format!("Discovery unavailable: {error} | built-in resources retained");
                    self.sticky_status = true;
                    if self.selection_ui.restoring.take().is_some() {
                        self.refresh = false;
                        self.watch_blocked = true;
                        self.status =
                            format!("Saved selection could not revalidate discovery: {error}");
                    } else if self.desired_resource.is_none() {
                        self.refresh = true;
                        self.watch_blocked = false;
                    }
                }
                Reply::Namespaces(names) => {
                    self.known_namespaces = names.clone();
                    if matches!(self.modal, Modal::Picker("Namespace", ..)) {
                        let index = names
                            .iter()
                            .position(|name| {
                                name == &self.namespace || (name == "all" && self.namespace == "*")
                            })
                            .unwrap_or(0);
                        self.modal = Modal::Picker("Namespace", names, index);
                    }
                }
                Reply::Prepared(p) => self.modal = Modal::Confirm(p, String::new()),
                Reply::Notice(text) => {
                    self.sticky_status = true;
                    self.modal = Modal::None;
                    self.status = text;
                    self.refresh = true;
                }
                Reply::Failure(error) => {
                    if matches!(self.modal, Modal::LiveOverview(..)) {
                        self.overview = None;
                        self.subviews.clear();
                        self.modal = Modal::Detail(format!("Overview unavailable\n{error}"), 0);
                        self.status = error;
                        continue;
                    }
                    if matches!(self.modal, Modal::Follow(..)) {
                        self.worker.logs.cancel();
                        self.modal = Modal::Detail(format!("LOGS unavailable\n{error}"), 0);
                    }
                    if self.connecting_epoch == Some(epoch) {
                        self.connecting_epoch = None;
                    }
                    if write_result {
                        self.sticky_status = true;
                    }
                    self.stale = true;
                    if error.contains("API 401") || error.contains("API 403") {
                        self.items.clear();
                        self.items_generation += 1;
                        self.modal = Modal::None;
                    }
                    self.status = error;
                }
                Reply::Watching => {
                    self.watching = true;
                    // Keep an operation outcome (e.g. a shell session summary)
                    // visible until the user acknowledges it.
                    if !self.sticky_status {
                        self.status = "WATCH: synchronizing bounded snapshot...".into();
                    }
                }
                Reply::Metrics(scope, namespace, result) => {
                    let now = Instant::now();
                    // Retry from completion, so a stalled provider cannot keep
                    // the command queue continuously busy after its deadline.
                    if (scope == crate::metrics::MetricsScope::Nodes && self.kind() == "nodes")
                        || (scope == crate::metrics::MetricsScope::Pods
                            && self.kind() == "pods"
                            && namespace == self.namespace)
                    {
                        self.last_table_metrics = Some(now);
                    }

                    let selected_uid = self.selected_uid();
                    let text = match result {
                        Ok(snapshot) => {
                            self.table_metrics.record(&snapshot, &self.items, now);
                            self.metrics_history.record(&self.context, &snapshot, now);
                            format!("{}\n{}", self.metrics_history.text(now), snapshot.text())
                        }
                        Err(error) => {
                            self.table_metrics.fail(&error);
                            self.metrics_history.record_error(
                                &self.context,
                                scope,
                                &namespace,
                                now,
                            );
                            format!(
                                "METRICS unavailable\n{error}\nResource browsing remains available.\n{}",
                                self.metrics_history.text(now)
                            )
                        }
                    };
                    if let Some(uid) = selected_uid {
                        self.selected = self
                            .visible()
                            .iter()
                            .position(|object| object["metadata"]["uid"].as_str() == Some(&uid))
                            .unwrap_or(self.selected);
                    }
                    if let Modal::Metrics(_, offset) = &self.modal {
                        self.modal = Modal::Metrics(text, *offset);
                    }
                }
            }
        }
        let snapshot = self.worker.watch.take();
        if let Some((epoch, snapshot)) = snapshot
            && epoch == self.epoch
            && self.connected
        {
            changed = true;
            self.replace_items(snapshot.objects);
            self.selected = self.selected.min(self.visible().len().saturating_sub(1));
            self.stale = snapshot.stale;
            self.truncated = snapshot.truncated;
            self.watching = !snapshot.terminal;
            self.watch_blocked = snapshot.terminal;
            if snapshot.terminal && snapshot.truncated {
                // Too large to cache live: fall back to bounded periodic lists.
                self.bounded_poll = true;
                self.watch_blocked = false;
                self.refresh = true;
            } else if self.watch_blocked {
                self.refresh = false;
            }
            if !self.write_inflight && !self.sticky_status {
                self.status = snapshot.message;
            }
        }
        if matches!(self.modal, Modal::Follow(_))
            && let Some(text) = self.worker.logs.take()
            && let Modal::Follow(view) = &mut self.modal
        {
            view.update(text);
            changed = true;
        }
        if let Modal::LiveOverview(dashboard) = &mut self.modal
            && let Some(pane) = dashboard.logs.as_mut()
            && let Some(text) = self.worker.logs.take()
        {
            pane.view.update(text);
            changed = true;
        }
        if let Some(feed) = &self.overview {
            match feed.take_update() {
                Ok(Some(rows)) => {
                    if let Modal::LiveOverview(dashboard) = &mut self.modal {
                        dashboard.update(rows);
                        changed = true;
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    self.status = error;
                    self.overview = None;
                    changed = true;
                }
            }
        }
        changed |= self.finish_saved_workload_focus();
        changed
    }
    pub(super) fn open_dashboard(&mut self, names: Vec<String>) -> Result<()> {
        crate::overview::validate(&names)?;
        if names.len() > crate::overview::MAX_LIVE_CONTEXTS {
            return Err("The dashboard supports up to 9 explicit contexts.".into());
        }
        self.overview = None;
        let mut connection = self.options.connection.clone();
        connection.namespace = self.namespace.clone();
        self.modal = Modal::LiveOverview(dashboard_ui::Dashboard::new(
            &names,
            self.options.connection.demo,
        ));
        if let Err(error) = crate::dashboard_memory::remember_scope(&names, &self.namespace) {
            self.status = format!("Dashboard opened; it could not be remembered: {error}");
        }
        self.send(Task::Overview(connection, names));
        Ok(())
    }
    /// Enter opens the focused card's Pods, or the exact Pod's Events when a
    /// warning record is selected. Event drilldown binds name and UID, so a
    /// replacement Pod with the same name never inherits the old records.
    pub(super) fn open_dashboard_pods(&mut self, dashboard: dashboard_ui::Dashboard) -> Result<()> {
        let Some(selected) = dashboard.selected() else {
            self.modal = Modal::LiveOverview(dashboard);
            return Ok(());
        };
        let warning = dashboard.selected_warning();
        if self.busy {
            self.status = format!(
                "Wait for the current request before opening {}.",
                if warning.is_some() { "Events" } else { "Pods" }
            );
            self.modal = Modal::LiveOverview(dashboard);
            return Ok(());
        }
        let fields = match warning {
            Some(warning) => {
                let fields = crate::selectors::pod_events_field(&warning.pod, &warning.uid);
                if let Err(error) = crate::selectors::field_scope(&fields) {
                    self.modal = Modal::LiveOverview(dashboard);
                    self.status = format!("Cannot open Events for this warning: {error}");
                    return Ok(());
                }
                fields
            }
            None => String::new(),
        };
        let (resource, status) = match warning {
            Some(warning) => (
                "v1/events",
                format!("Events for Pod {} (exact UID) | Esc back", warning.pod),
            ),
            None => ("v1/pods", "Opening selected context Pods.".into()),
        };
        let context = selected.context.clone();
        let namespace = if let Some(warning) = warning {
            warning.namespace.clone()
        } else if selected.namespace.is_empty() {
            self.namespace.clone()
        } else {
            selected.namespace.clone()
        };
        self.push_view();
        self.overview = None;
        self.filter.clear();
        self.selector.clear();
        self.field_selector = fields;
        self.namespace = namespace;
        self.desired_resource = Some(resource.into());
        if context == self.context {
            if let Ok(index) = self.resolve_resource(resource) {
                self.kind_index = index;
                self.desired_resource = None;
            }
            self.changed();
        } else {
            self.options.connection.context = Some(context);
            self.connect();
        }
        self.namespace_override = true;
        self.status = status;
        Ok(())
    }
    pub(super) fn theme_picker(&mut self) -> Result<()> {
        // Previews keep ui.yaml color overrides; a theme they would make
        // unreadable is previewed without them.
        let all = themes::all()?
            .into_iter()
            .map(|theme| self.ui.apply(&theme).unwrap_or(theme))
            .collect::<Vec<_>>();
        let index = all.iter().position(|t| t.id == self.theme.id).unwrap_or(0);
        self.modal = Modal::Themes(all, index, self.theme.clone());
        Ok(())
    }
    /// `:ui reload`: re-read ui.yaml and re-apply its theme and colors. An
    /// invalid file keeps the current settings and explains why.
    pub(super) fn reload_ui(&mut self) -> Result<()> {
        let ui = match crate::ui_config::load() {
            Ok(ui) => ui,
            Err(error) => {
                self.modal = Modal::Detail(
                    format!(
                        "ui.yaml not reloaded\n{error}\n\nThe current layout and colors remain in use. Esc closes this notice."
                    ),
                    0,
                );
                return Ok(());
            }
        };
        let (theme, error) = super::ui_theme(&ui, self.options.theme.as_deref())?;
        crate::text::set_unicode(ui.text.unicode());
        self.config_watch.set_enabled(ui.reload);
        self.config_watch.watch_theme(&theme.id);
        self.ui = ui;
        self.theme = theme;
        match error {
            Some(error) => {
                self.modal = Modal::Detail(
                    format!(
                        "ui.yaml reloaded with a theme problem\n{error}\n\nThe saved theme is used without color overrides. Esc closes this notice."
                    ),
                    0,
                )
            }
            None => self.status = "ui.yaml reloaded.".into(),
        }
        Ok(())
    }
    pub(super) fn refresh_table_metrics(&mut self) {
        if self.busy || !matches!(self.kind().as_str(), "pods" | "nodes") {
            return;
        }
        self.last_table_metrics = Some(Instant::now());
        self.send(Task::Metrics(
            if self.kind() == "nodes" {
                crate::metrics::MetricsScope::Nodes
            } else {
                crate::metrics::MetricsScope::Pods
            },
            self.namespace.clone(),
            false,
        ));
    }
    pub(super) fn request_metrics(&mut self) {
        if self.busy {
            self.status = "Busy; wait for the current request.".into();
            return;
        }
        if !matches!(self.modal, Modal::Metrics(..)) {
            self.modal = Modal::Metrics("Loading Metrics API...".into(), 0);
        }
        self.last_metrics = Instant::now();
        if matches!(self.kind().as_str(), "pods" | "nodes") {
            self.last_table_metrics = Some(self.last_metrics);
        }
        self.send(Task::Metrics(
            if self.kind() == "nodes" {
                crate::metrics::MetricsScope::Nodes
            } else {
                crate::metrics::MetricsScope::Pods
            },
            self.namespace.clone(),
            self.metrics_resources,
        ));
    }
    pub(super) fn request_shell(&mut self) {
        self.request_session(false);
    }
    pub(super) fn request_session(&mut self, attach: bool) {
        if self.busy {
            self.status = "Busy; wait for the current request.".into();
            return;
        }
        if self.options.connection.demo {
            self.status = "Pod shells require a live cluster; DEMO has no remote processes.".into();
            return;
        }
        if !self.options.connection.writable
            || !(if attach {
                self.options.connection.allow_attach
            } else {
                self.options.connection.allow_exec
            })
        {
            self.status = if attach {
                "Pod attach requires permission (off in --read-only mode or by operator config)."
            } else {
                "Pod shell requires permission (off in --read-only mode or by operator config)."
            }
            .into();
            return;
        }
        if self.kind() != "pods" {
            self.status = "Pod shells apply to Pods; select a pod.".into();
            return;
        }
        if let Some(object) = self.visible().get(self.selected) {
            let names: Vec<_> = object["spec"]["containers"]
                .as_array()
                .map(|items| {
                    items
                        .iter()
                        .take(64)
                        .filter_map(|item| item["name"].as_str().map(str::to_owned))
                        .collect()
                })
                .unwrap_or_default();
            if names.is_empty() || self.selected_uid().is_none() {
                self.status =
                    "Pod identity or ordinary containers unavailable; refresh before shell review."
                        .into();
                return;
            }
            self.modal = Modal::SessionContainers(
                namespace(object).into(),
                name(object).into(),
                self.selected_uid(),
                names,
                0,
                attach,
            );
        }
    }
    pub(super) fn request_forward(&mut self) {
        if self.busy {
            self.status = "Wait for the current request before forwarding.".into();
            return;
        }
        if !self.options.connection.writable || !self.options.connection.allow_port_forward {
            self.status =
                "Port-forward requires permission (off in --read-only mode or by operator config)."
                    .into();
            return;
        }
        if self.options.connection.demo || self.kind() != "pods" {
            self.status = "Port-forward requires a selected live Pod.".into();
            return;
        }
        if let Some((_, namespace, pod)) = self.selected_target() {
            self.modal = Modal::ForwardForm(
                namespace,
                pod,
                self.selected_uid(),
                [
                    self.worker.forward.suggested_port().to_string(),
                    "80".into(),
                    "300".into(),
                ],
                0,
            );
        }
    }
    pub(super) fn request_log_group(
        &mut self,
        namespace: String,
        selection: crate::logs::PodLogSelection,
        previous: bool,
        follow: bool,
        dynamic: bool,
    ) {
        if self.busy {
            self.status = "Wait for the current request before capturing log membership.".into();
            return;
        }
        if !self.options.connection.logs {
            self.status = "Multi-Pod logs require permission (off in --read-only mode or by operator config).".into();
            return;
        }
        if !safety::valid_id(&namespace) {
            self.status = "Choose one explicit namespace for workload logs.".into();
            return;
        }
        if dynamic && (previous || !follow || self.options.log_read.head) {
            self.status =
                "Dynamic membership follows new records; choose a tail or time range, not head."
                    .into();
            return;
        }
        let (name, uid) = match &selection {
            crate::logs::PodLogSelection::Workload { name, uid, .. } => (name.clone(), uid.clone()),
            crate::logs::PodLogSelection::Selector(selector) => {
                (format!("selector:{selector}"), String::new())
            }
        };
        let target = LogTarget {
            namespace,
            pod: name,
            uid,
            container: None,
            previous,
            follow,
            all: true,
            dynamic,
            membership: None,
        };
        self.subviews
            .retain(|modal| !matches!(modal, Modal::Follow(_) | Modal::Containers(..)));
        let generation = self.worker.logs.cancel();
        let mut view = crate::ui_logs::LogView::new(
            if dynamic {
                "Capturing complete Pod membership; dynamic opt-in announces every join and departure..."
            } else {
                "Capturing complete fixed Pod membership; no new Pods will be added automatically..."
            }
            .into(),
            self.options.log_read,
        );
        view.following = follow && !previous && !self.options.log_read.head;
        view.previous = previous;
        self.log_target = Some(target.clone());
        self.modal = Modal::Follow(Box::new(view));
        self.send(Task::CaptureLogs(target, selection, generation));
    }
    pub(super) fn request_dynamic_workload_logs(&mut self) {
        if !supports_group_logs(&self.kind()) {
            self.status =
                "Use :logs-live on a selected workload, or :logs-live LABEL_SELECTOR.".into();
            return;
        }
        match (self.selected_target(), self.selected_uid()) {
            (Some((resource, namespace, name)), Some(uid)) => self.request_log_group(
                namespace,
                crate::logs::PodLogSelection::Workload {
                    resource,
                    name,
                    uid,
                },
                false,
                true,
                true,
            ),
            (Some(_), None) => {
                self.status =
                    "Workload UID unavailable; refresh before capturing log membership.".into()
            }
            (None, _) => self.status = "Select a workload first.".into(),
        }
    }
    pub(super) fn request_logs(&mut self, previous: bool, follow: bool) {
        if supports_group_logs(&self.kind()) {
            if let Some((resource, namespace, name)) = self.selected_target() {
                if let Some(uid) = self.selected_uid() {
                    self.request_log_group(
                        namespace,
                        crate::logs::PodLogSelection::Workload {
                            resource,
                            name,
                            uid,
                        },
                        previous,
                        follow,
                        false,
                    );
                } else {
                    self.status =
                        "Workload UID unavailable; refresh before capturing log membership.".into();
                }
            }
            return;
        }
        if !follow && !previous {
            self.request_log_picker(previous, follow);
            return;
        }
        if self.busy {
            self.status = "Busy; wait before opening logs.".into();
            return;
        }
        if !self.options.connection.logs {
            self.status = "Logs may contain sensitive data; opt in with permission (off in --read-only mode or by operator config).".into();
            return;
        }
        let Some(object) = self
            .visible()
            .get(self.selected)
            .map(|object| (*object).clone())
        else {
            return;
        };
        if self.kind() != "pods" {
            self.status = "Logs apply to core Pods; choose a Pod.".into();
            return;
        }
        let Some(uid) = self.selected_uid() else {
            self.status = "Pod UID unavailable; refresh before reading logs.".into();
            return;
        };
        let ordinary = ordinary_container_names(&object);
        let preferred =
            object["metadata"]["annotations"]["kubectl.kubernetes.io/default-container"]
                .as_str()
                .filter(|container| ordinary.iter().any(|name| name == container))
                .map(str::to_owned)
                .or_else(|| (ordinary.len() == 1).then(|| ordinary[0].clone()));
        let target = LogTarget {
            namespace: namespace(&object).into(),
            pod: name(&object).into(),
            uid,
            all: preferred.is_none(),
            dynamic: false,
            membership: None,
            container: preferred.or_else(|| ordinary.first().cloned()),
            previous,
            follow,
        };
        self.open_logs(target, self.options.log_read);
    }
    pub(super) fn request_log_picker(&mut self, previous: bool, follow: bool) {
        if self.busy {
            self.status = "Busy; wait before opening logs.".into();
            return;
        }
        if !self.options.connection.logs {
            self.status =
                "Logs require permission (off in --read-only mode or by operator config).".into();
            return;
        }
        if let Some((kind, namespace, pod)) = self.selected_target() {
            if kind != "pods" {
                self.status = "Logs apply to core Pods.".into();
                return;
            }
            let Some(uid) = self.selected_uid() else {
                self.status = "Pod UID unavailable; refresh first.".into();
                return;
            };
            let container = self
                .visible()
                .get(self.selected)
                .and_then(|object| ordinary_container_names(object).into_iter().next());
            let target = LogTarget {
                namespace,
                pod,
                container,
                uid,
                previous,
                follow,
                all: false,
                dynamic: false,
                membership: None,
            };
            self.subviews
                .retain(|modal| !matches!(modal, Modal::Follow(_) | Modal::Containers(..)));
            let generation = self.worker.logs.cancel();
            self.modal = Modal::Containers(target.clone(), vec![], 0);
            self.send(Task::Containers(target, generation));
        }
    }
    pub(super) fn open_logs(&mut self, target: LogTarget, read: crate::logs::ReadOptions) {
        if self.busy {
            self.status = "Busy; wait before opening logs.".into();
            self.close_subview();
            return;
        }
        self.subviews
            .retain(|modal| !matches!(modal, Modal::Follow(_) | Modal::Containers(..)));
        let generation = self.worker.logs.cancel();
        let mut view = crate::ui_logs::LogView::new("Connecting to logs...".into(), read);
        view.following = target.follow && !read.head && !target.previous;
        view.previous = target.previous;
        view.enable_all_containers(target.membership.is_none());
        self.log_target = Some(target.clone());
        self.modal = Modal::Follow(Box::new(view));
        if target.follow && !target.previous && !read.head {
            self.send(Task::Follow(
                target,
                generation,
                self.options.log_seconds,
                read,
            ));
        } else {
            self.send(Task::Logs(target, generation, read));
        }
    }
    pub(super) fn reload_logs(
        &mut self,
        mut view: Box<crate::ui_logs::LogView>,
        read: crate::logs::ReadOptions,
    ) {
        let Some(target) = self.log_target.clone() else {
            self.modal = Modal::Follow(view);
            return;
        };
        if self.busy {
            self.status = "Wait for the current log request before changing range.".into();
            self.modal = Modal::Follow(view);
            return;
        }
        let generation = self.worker.logs.cancel();
        view.replace_source("Connecting to logs...".into());
        view.following = target.follow && !target.previous && !read.head;
        self.modal = Modal::Follow(view);
        if target.follow && !target.previous && !read.head {
            self.send(Task::Follow(
                target,
                generation,
                self.options.log_seconds,
                read,
            ));
        } else {
            self.send(Task::Logs(target, generation, read));
        }
    }
    pub(super) fn request_helm(&mut self, request: helm_ui::Request, preserve_current: bool) {
        if self.busy {
            self.status = "Wait for the current request before reading Helm releases.".into();
            return;
        }
        if !self.options.connection.demo && !self.options.connection.allow_helm {
            self.status="Helm reads require permission (off in --read-only mode or by operator config). Stored release metadata and explicit values may contain confidential data.".into();
            return;
        }
        if preserve_current {
            let current = std::mem::replace(&mut self.modal, Modal::None);
            if !matches!(current, Modal::None) {
                self.push_subview(current);
            }
        }
        self.helm_generation = self.helm_generation.wrapping_add(1);
        self.modal = Modal::HelmLoading(request.label());
        self.send(Task::Helm(request, self.helm_generation));
    }
    pub(super) fn begin_helm_form(&mut self, intent: helm_ui::Intent) {
        if self.busy {
            self.status = "Wait for the current request before preparing Helm lifecycle.".into();
            return;
        }
        if !self.options.connection.writable
            || !self.options.connection.allow_helm
            || !self.options.connection.allow_helm_write
        {
            self.status="Helm lifecycle requires permission (off in --read-only mode or by operator config).".into();
            return;
        }
        if self.options.helm_tool.is_none() && !self.options.connection.demo {
            self.status="Choose the trusted native Helm executable explicitly with --helm-tool ABSOLUTE_PATH.".into();
            return;
        }
        let table = match &self.modal {
            Modal::Helm(table) => Some(table.as_ref()),
            _ => self.subviews.iter().rev().find_map(|modal| {
                if let Modal::Helm(table) = modal {
                    Some(table.as_ref())
                } else {
                    None
                }
            }),
        };
        let (namespace, storage) = table
            .map(|table| {
                (
                    table.namespace.clone(),
                    table.storage.or(self.options.helm_storage),
                )
            })
            .unwrap_or_else(|| (self.namespace.clone(), self.options.helm_storage));
        if intent.is_crd() && !self.options.connection.allow_helm_crds {
            self.status="Chart CRD actions require explicit permission (off in --read-only mode or by operator config) and a separate complete review.".into();
            return;
        }
        if matches!(intent, helm_ui::Intent::Test(_)) && !self.options.connection.allow_helm_hooks {
            self.status =
                "Helm tests require explicit permission (off in --read-only mode or by operator config) and a complete operation review."
                    .into();
            return;
        }
        let mut form = helm_ui::Form::new(
            intent,
            namespace,
            storage.unwrap_or(crate::helm::Storage::Secret),
            self.options.helm_values.as_deref(),
            self.options.connection.demo,
        );
        form.set_lifecycle_policy(
            self.options.connection.allow_helm_hooks,
            self.options.connection.allow_helm_crds,
            crate::helm_oci::Options {
                ca_file: self.options.helm_registry_ca.clone(),
                auth_file: self.options.helm_registry_auth.clone(),
            },
        );
        form.set_keyring(self.options.helm_keyring.clone());
        form.set_sigstore_policy(self.options.helm_sigstore_policy.clone());
        self.status = "Helm draft; no execution requested.".into();
        self.sticky_status = false;
        let current = std::mem::replace(&mut self.modal, Modal::None);
        if !matches!(current, Modal::None) {
            self.push_subview(current);
        }
        self.modal = Modal::HelmForm(Box::new(form));
    }
    pub(super) fn cancel_helm(&mut self) {
        if let Some(cancel) = self.helm_cancel.take() {
            let _ = cancel.send(());
        }
        if let Modal::HelmRunning(_, _, cancelled) = &mut self.modal {
            *cancelled = true;
        }
        self.status="Cancellation requested; remote changes may have started. Waiting for reconciled outcome.".into();
    }
    pub(super) fn action_menu(&self) -> Vec<String> {
        let kind = self.kind();
        let has_selection = !self.visible().is_empty();
        [
            "Object overview",
            "Inspect YAML",
            "Logs snapshot",
            "Previous logs",
            "Follow logs (bounded)",
            "Metrics",
            "Context overview",
            "Scale workload",
            "Change workload container image",
            "Restart workload",
            "Delete selected resource",
            "Inspect redacted JSON",
            "Interactive pod shell",
            "Metrics with requests and limits (extra reads)",
            "Edit workload resource budget",
        ]
        .iter()
        .enumerate()
        .map(|(index, title)| {
            let reason = if index == 5 || index == 6 || index == 13 {
                None
            } else if !has_selection {
                Some("select an object")
            } else if (2..=4).contains(&index) && !self.options.connection.logs {
                Some("off (read-only or operator config)")
            } else if (2..=4).contains(&index) && kind != "pods" && !supports_group_logs(&kind) {
                Some("Pods and supported workloads/services only")
            } else if ((7..=10).contains(&index) || index == 12 || index == 14)
                && !self.options.connection.writable
            {
                Some("read-only mode")
            } else if index == 7
                && !["deployments.apps", "statefulsets.apps"].contains(&kind.as_str())
            {
                Some("Deployments and StatefulSets only")
            } else if index == 9
                && !["deployments.apps", "statefulsets.apps", "daemonsets.apps"]
                    .contains(&kind.as_str())
            {
                Some("Deployments, StatefulSets and DaemonSets only")
            } else if (index == 8 || index == 14) && !crate::edit::supports_resource(&kind) {
                Some("Supported workloads only")
            } else if index == 10
                && !matches!(
                    kind.as_str(),
                    "pods" | "deployments.apps" | "services" | "jobs.batch"
                )
            {
                Some("unsupported resource")
            } else if index == 12 && !self.options.connection.allow_exec {
                Some("off (read-only or operator config)")
            } else if index == 12 && (kind != "pods" || self.options.connection.demo) {
                Some("live Pods only")
            } else {
                None
            };
            reason
                .map(|reason| format!("[OFF] {title}: {reason}"))
                .unwrap_or_else(|| format!("      {title}"))
        })
        .chain(self.plugin_menu())
        .collect()
    }
    fn action_allowed(&self, action: &Action) -> bool {
        match action {
            Action::Scale(_) => {
                ["deployments.apps", "statefulsets.apps"].contains(&self.kind().as_str())
            }
            Action::Restart => ["deployments.apps", "statefulsets.apps", "daemonsets.apps"]
                .contains(&self.kind().as_str()),
            Action::SetContainer { .. } | Action::SetImage { .. } | Action::SetResources { .. } => {
                crate::edit::supports_resource(&self.kind())
            }
            Action::Delete => matches!(
                self.kind().as_str(),
                "pods"
                    | "deployments.apps"
                    | "statefulsets.apps"
                    | "daemonsets.apps"
                    | "services"
                    | "jobs.batch"
                    | "cronjobs.batch"
            ),
            Action::Kill => self.kind() == "pods",
            Action::Trigger | Action::Suspend(_) => self.kind() == "cronjobs.batch",
            Action::Rollback(_) => ["deployments.apps", "statefulsets.apps", "daemonsets.apps"]
                .contains(&self.kind().as_str()),
        }
    }
    pub(super) fn prepare(&mut self, action: Action) {
        if !self.options.connection.writable {
            self.status =
                "enable write permission (off in --read-only mode) before requesting a mutation."
                    .into();
            return;
        }
        if !self.action_allowed(&action) {
            self.status = "This operation is disabled for the selected resource.".into();
            return;
        }
        if let Some((k, n, name)) = self.selected_target() {
            self.send(Task::Prepare(k, n, name, self.selected_uid(), action));
        }
    }
    /// Marks exist: prepare one reviewed batch over every marked object of
    /// the current table instead of the selected row.
    pub(super) fn prepare_batch(&mut self, action: Action) {
        if !self.options.connection.writable {
            self.status =
                "enable write permission (off in --read-only mode) before requesting a mutation."
                    .into();
            return;
        }
        let resource = self.kind();
        if !crate::gateway::batch_supported(&resource, &action) || !self.action_allowed(&action) {
            self.status = format!(
                "Bulk {} is not available here; batches cover delete, workload restart and CronJob suspend/resume.",
                action.operation_name()
            );
            return;
        }
        // Table order first, then marked rows the filter currently hides.
        let visible = self.visible_order();
        let mut shown = vec![false; self.items.len()];
        for &index in visible.iter() {
            if let Some(flag) = shown.get_mut(index) {
                *flag = true;
            }
        }
        let hidden = (0..self.items.len()).filter(|index| shown.get(*index) != Some(&true));
        let targets = visible
            .iter()
            .copied()
            .chain(hidden)
            .filter_map(|index| self.items.get(index))
            .filter_map(|object| {
                let uid = object["metadata"]["uid"].as_str()?;
                self.marks
                    .contains(uid)
                    .then(|| crate::gateway::BatchTarget {
                        namespace: namespace(object).to_owned(),
                        name: name(object).to_owned(),
                        uid: uid.to_owned(),
                    })
            })
            .collect::<Vec<_>>();
        let clear = self
            .keymap
            .primary("marks.clear")
            .unwrap_or_else(|| "Ctrl-\\".into());
        let missing = self.marks.len().saturating_sub(targets.len());
        if missing > 0 {
            self.status = format!(
                "{missing} marked object(s) are no longer listed; {clear} clears marks. Nothing was prepared."
            );
            return;
        }
        if targets.len() > crate::gateway::MAX_BATCH {
            self.status = format!(
                "{} marked; one batch covers at most {}. Unmark some rows ({clear} clears all).",
                targets.len(),
                crate::gateway::MAX_BATCH
            );
            return;
        }
        let count = targets.len();
        self.send(Task::BatchPrepare(Box::new(crate::gateway::BatchRequest {
            context: self.context.clone(),
            resource,
            action,
            targets,
        })));
        if self.busy {
            self.status = format!("Dry-running {count} marked objects for one batch review...");
        }
    }
    /// Status after marking: how many rows are marked and which keys act on
    /// all of them, using the effective keymap.
    pub(super) fn marks_status(&self) -> String {
        let count = self.marks.len();
        if count == 0 {
            return "Marks cleared.".into();
        }
        let kind = self.kind();
        let key = |action: &str| self.keymap.primary(action);
        let mut actions = vec![];
        if self.options.connection.writable {
            if crate::gateway::batch_supported(&kind, &Action::Delete)
                && let Some(key) = key("delete")
            {
                actions.push(format!("{key} deletes all"));
            }
            if crate::gateway::batch_supported(&kind, &Action::Restart)
                && let Some(key) = key("restart")
            {
                actions.push(format!("{key} restarts all"));
            }
            if crate::gateway::batch_supported(&kind, &Action::Suspend(true))
                && let Some(key) = key("suspend")
            {
                actions.push(format!("{key} suspends/resumes all"));
            }
        }
        let clear = key("marks.clear").unwrap_or_else(|| "Ctrl-\\".into());
        if actions.is_empty() {
            format!(
                "{count} marked | no bulk action here (write permission (off in --read-only mode); delete, restart, CronJob suspend) | {clear} clears"
            )
        } else {
            format!(
                "{count} marked: {} (one reviewed batch, max {}) | {clear} clears",
                actions.join(", "),
                crate::gateway::MAX_BATCH
            )
        }
    }
}
