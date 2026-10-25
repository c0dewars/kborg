//! Resource-table actions, dispatched by name through the keymap registry so
//! keymap.yaml remaps them and the replaced key stops working.
use super::*;
use crate::keymap::{Key, Scope};

impl App {
    pub(super) fn table_key(&mut self, key: Key) -> Result<bool> {
        let class = crate::keymap::kind_class(&self.kind());
        if let Some((action, index)) = self.keymap.table_action(key, class) {
            return self.table_action(action, index);
        }
        match key {
            Key::Char('x') => {
                self.status =
                    "Use :xray RESOURCE for relationships or :restart for a reviewed rollout."
                        .into()
            }
            Key::Char('z') => {
                self.status =
                    "Inspection is already redacted; additional k9s sanitization is unavailable."
                        .into()
            }
            Key::Ctrl(_) => {
                let help = self
                    .keymap
                    .primary("help")
                    .unwrap_or_else(|| ":help".into());
                self.status = format!(
                    "This k9s shortcut is not available here; {help} lists supported actions."
                )
            }
            _ => {}
        }
        Ok(false)
    }
    /// Runs the action a built-in key has for the selected kind, for menus
    /// that name actions by their default keys.
    pub(super) fn default_table_key(&mut self, key: Key) -> Result<bool> {
        let class = crate::keymap::kind_class(&self.kind());
        match crate::keymap::Keymap::default().table_action(key, class) {
            Some((action, index)) => self.table_action(action, index),
            None => Ok(false),
        }
    }
    pub(super) fn table_action(&mut self, action: &str, index: usize) -> Result<bool> {
        match action {
            "help" => self.open_help(Scope::Table, keyhelp::Mode::Context),
            "command" | "prompt" => {
                self.capture_ask_screen();
                self.history_index = None;
                self.completion = None;
                self.command_cursor = 0;
                self.modal = Modal::Input("Command", String::new());
            }
            "quit" => return Ok(true),
            "back" => {
                if !self.filter.is_empty() {
                    let uid = self.selected_uid();
                    self.filter.clear();
                    self.selected = uid
                        .and_then(|uid| {
                            self.visible()
                                .iter()
                                .position(|object| object["metadata"]["uid"].as_str() == Some(&uid))
                        })
                        .unwrap_or(0);
                } else if !self.selector.is_empty() {
                    self.selector.clear();
                    self.changed();
                } else {
                    self.go_back();
                }
            }
            "view.back" => self.go_back(),
            "view.forward" => self.go_forward(),
            "view.last" => self.last_view(),
            "page.down" | "page.up" | "top" | "bottom" => {
                let page = terminal::size()
                    .map(|(w, h)| Layout::for_ui(w, h, &self.ui).table_rows())
                    .unwrap_or(15)
                    .max(1);
                let len = self.visible().len();
                self.selected = match action {
                    "page.down" => self
                        .selected
                        .saturating_add(page)
                        .min(len.saturating_sub(1)),
                    "page.up" => self.selected.saturating_sub(page),
                    "top" => 0,
                    _ => len.saturating_sub(1),
                };
            }
            "down" => {
                self.selected = (self.selected + 1).min(self.visible().len().saturating_sub(1))
            }
            "up" => self.selected = self.selected.saturating_sub(1),
            "drilldown" => self.drilldown(),
            "filter" => {
                self.modal = Modal::Filter(
                    self.filter.clone(),
                    self.filter.clone(),
                    self.selected_uid(),
                    self.selected,
                )
            }
            "json" => {
                if let Some((k, n, name)) = self.selected_target() {
                    self.send(Task::Get(k, n, name));
                }
            }
            "describe" => {
                if let Some((k, n, name)) = self.selected_target() {
                    self.send(Task::Inspect(k, n, name));
                }
            }
            "yaml" => {
                if let Some((k, n, name)) = self.selected_target() {
                    self.send(Task::Yaml(k, n, name));
                }
            }
            "sort.status" => self.set_sort(Sort::Status),
            "sort.name" => self.set_sort(Sort::Name),
            "sort.age" => self.set_sort(Sort::Age),
            "sort.ready" => self.set_sort(Sort::Ready),
            "sort.cpu" | "sort.memory" => {
                if matches!(self.kind().as_str(), "pods" | "nodes") {
                    self.set_sort(if action == "sort.cpu" {
                        Sort::Cpu
                    } else {
                        Sort::Memory
                    });
                    if !self.busy && self.last_table_metrics.is_none() {
                        self.refresh_table_metrics();
                    }
                } else {
                    self.status = "CPU/memory usage sorting applies to Pod and Node tables; :metrics shows exact samples.".into();
                }
            }
            "exec" | "shell" => self.request_shell(),
            "node.shell" => self.node_shell(),
            "suspend" if !self.marks.is_empty() => {
                // One direction for the whole batch, taken from the marked rows.
                let states = self
                    .items
                    .iter()
                    .filter(|cron| {
                        cron["metadata"]["uid"]
                            .as_str()
                            .is_some_and(|uid| self.marks.contains(uid))
                    })
                    .map(|cron| cron["spec"]["suspend"] == true)
                    .collect::<std::collections::BTreeSet<_>>();
                match states.into_iter().collect::<Vec<_>>().as_slice() {
                    [suspended] => self.prepare_batch(Action::Suspend(!suspended)),
                    [] => self.prepare_batch(Action::Suspend(true)),
                    _ => {
                        self.status = "Marked CronJobs mix suspended and active; mark only one state to suspend or resume them together.".into()
                    }
                }
            }
            "suspend" => {
                let suspended = self
                    .visible()
                    .get(self.selected)
                    .is_some_and(|cron| cron["spec"]["suspend"] == true);
                self.prepare(Action::Suspend(!suspended));
            }
            "scale" => return self.scale(),
            "copy.namespace" => {
                if let Some((_, namespace, _)) = self.selected_target() {
                    self.copy_text(&namespace)?;
                    self.status =
                        "Namespace sent to terminal clipboard (OSC52 support required).".into();
                }
            }
            "cordon" | "copy.name" => {
                if self.kind() == "nodes" && self.options.connection.writable {
                    self.node_maintenance(crate::node_maintenance::Request::Cordon);
                    return Ok(false);
                }
                if let Some(object) = self.visible().get(self.selected) {
                    self.copy_text(name(object))?;
                    self.status =
                        "Name sent to terminal clipboard (OSC52 support required).".into();
                }
            }
            "uncordon" => self.node_maintenance(crate::node_maintenance::Request::Uncordon),
            "trigger" => {
                if self.kind() == "cronjobs.batch" {
                    self.prepare(Action::Trigger);
                } else {
                    self.status = format!(
                        "Copy files with :cp-from / :cp-to (--allow-file-copy). {} triggers a selected CronJob.",
                        self.keymap
                            .primary("trigger")
                            .unwrap_or_else(|| ":trigger".into())
                    )
                }
            }
            "selection.save" => self.save_resource_selection()?,
            "selection.open" => self.open_saved_selections()?,
            "dashboard" => self.open_dashboard(vec![self.context.clone()])?,
            "drain" => {
                if self.kind() == "nodes" {
                    self.node_maintenance(crate::node_maintenance::Request::Drain { seconds: 120 });
                }
            }
            "restart" if crate::edit::supports_resource(&self.kind()) => {
                if self.marks.is_empty() {
                    self.prepare(Action::Restart);
                } else {
                    self.prepare_batch(Action::Restart);
                }
            }
            "restart" => {}
            "refresh.r" => {
                if self.watch_blocked {
                    self.options.connection.context = Some(self.context.clone());
                    self.connect();
                    self.namespace_override = true;
                } else if self.connected {
                    self.refresh = true;
                } else if !self.busy {
                    self.connect();
                }
            }
            "refresh" => {
                if self.connected {
                    self.refresh = true;
                } else if !self.busy {
                    self.connect();
                }
            }
            "metrics" => {
                self.metrics_resources = false;
                self.request_metrics();
            }
            "warp" => {
                if let Some((_, namespace, _)) = self.selected_target() {
                    return self.command(&format!("ns {namespace}"));
                }
            }
            "ns.all" => return self.command("ns all"),
            "ns.recent" => return self.recent_namespace(index),
            "mark" => {
                if let Some(uid) = self.selected_uid() {
                    if !self.marks.remove(&uid) {
                        self.marks.insert(uid);
                    }
                    self.selected = (self.selected + 1).min(self.visible().len().saturating_sub(1));
                    self.status = self.marks_status();
                }
            }
            "mark.range" => {
                let uids = self
                    .visible()
                    .iter()
                    .take(self.selected + 1)
                    .filter_map(|object| object["metadata"]["uid"].as_str().map(str::to_owned))
                    .collect::<Vec<_>>();
                self.marks.extend(uids);
                self.status = self.marks_status();
            }
            "marks.clear" => {
                self.marks.clear();
                self.status = self.marks_status();
            }
            "attach" => self.request_session(true),
            "node.show" => {
                if self.kind() != "pods" {
                    self.status = "Show Node applies to core Pods.".into();
                    return Ok(false);
                }
                let node = self
                    .visible()
                    .get(self.selected)
                    .and_then(|object| object["spec"]["nodeName"].as_str())
                    .map(str::to_owned);
                if let Some(node) = node {
                    return self.command(&format!("v1/nodes /^{node}$"));
                }
                self.status =
                    "Show Node requires a scheduled Pod. :overview opens multiple contexts.".into();
            }
            "edit" => self.yaml_editor(),
            "resources" => {
                if !self.options.connection.writable {
                    self.status = "READ ONLY: resource budgets require --write.".into();
                } else if !crate::edit::supports_resource(&self.kind()) {
                    self.status = "Resource budgets apply to supported workload containers.".into();
                } else if let Some((k, n, name)) = self.selected_target() {
                    self.modal = Modal::Resources(k, n, name, self.selected_uid(), String::new());
                }
            }
            "logs" => self.request_logs(false, true),
            "logs.previous" => self.request_logs(true, false),
            "forwards" => self.modal = Modal::ForwardView,
            "forward" => self.request_forward(),
            "delete" if !self.marks.is_empty() => self.prepare_batch(Action::Delete),
            "delete" => self.prepare(Action::Delete),
            "kill" => self.kill(),
            "scan" => self.scan(false),
            "aliases" => self.aliases(),
            "toggle.wide" => {
                self.wide = !self.wide;
                self.status = if self.wide {
                    format!(
                        "Wide columns on (Node, IP and |W view columns). {} turns them off.",
                        self.keymap.primary("toggle.wide").unwrap_or_default()
                    )
                } else {
                    "Wide columns off.".into()
                };
            }
            "toggle.faults" => {
                self.faults_only = !self.faults_only;
                self.selected = 0;
                self.status = if self.faults_only {
                    format!(
                        "Errors only: unhealthy Pods, degraded workloads, NotReady Nodes, warnings. {} shows all.",
                        self.keymap
                            .primary("toggle.faults")
                            .unwrap_or_else(|| ":faults".into())
                    )
                } else {
                    "Showing all objects.".into()
                };
            }
            "snapshot" => {
                let values = self.visible().into_iter().cloned().collect::<Vec<_>>();
                match crate::screendump::save(&self.context, &self.namespace, &self.kind(), &values)
                {
                    Ok(record) => {
                        self.status = format!("Saved {} | :sd opens saved resources", record.title)
                    }
                    Err(error) => self.status = error,
                };
            }
            _ => {}
        }
        Ok(false)
    }
    fn recent_namespace(&mut self, index: usize) -> Result<bool> {
        let recent = self.recent.namespaces(&self.context);
        if let Some(namespace) = recent.get(index).cloned() {
            return self.command(&format!("ns {namespace}"));
        }
        let keys = self.keymap.keys("ns.recent").to_vec();
        self.status = if recent.is_empty() {
            format!(
                "No recent namespaces yet; :ns opens the picker. {} then jump to the last nine.",
                crate::keymap::label(&keys)
            )
        } else {
            format!(
                "Recent: {}",
                recent
                    .iter()
                    .enumerate()
                    .map(|(i, ns)| match keys.get(i) {
                        Some(key) => format!("{key} {ns}"),
                        None => format!("{} {ns}", i + 1),
                    })
                    .collect::<Vec<_>>()
                    .join("  ")
            )
        };
        Ok(false)
    }
}
