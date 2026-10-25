//! `:` command handling.
use super::*;

impl App {
    pub(super) fn command(&mut self, text: &str) -> Result<bool> {
        let expanded = match crate::aliases::expand(text, &self.aliases) {
            Ok(text) => text,
            Err(error) => {
                self.status = error;
                return Ok(false);
            }
        };
        let words = match nav::words(&expanded) {
            Ok(words) => words,
            Err(error) => {
                self.status = error;
                return Ok(false);
            }
        };
        let Some(first) = words.first() else {
            return Ok(false);
        };
        let cmd = first.to_ascii_lowercase();
        let args = &words[1..];
        match (cmd.as_str(), args) {
            ("q" | "q!" | "qa" | "quit" | "exit", []) => return Ok(true),
            ("?" | "h" | "help", []) => {
                let scope = self
                    .subviews
                    .last()
                    .and_then(keyhelp::help_scope)
                    .unwrap_or(crate::keymap::Scope::Table);
                self.open_help(scope, keyhelp::Mode::Context);
            }
            ("keymap" | "keys", []) => {
                self.open_help(crate::keymap::Scope::Table, keyhelp::Mode::Keymap)
            }
            ("keymap" | "keys", [filter]) => {
                self.open_help(crate::keymap::Scope::Table, keyhelp::Mode::Keymap);
                if let Modal::Help(view) = &mut self.modal {
                    view.filter = filter.chars().take(64).collect();
                }
            }
            ("a" | "alias" | "aliases", []) => self.aliases(),
            ("actions", []) => self.modal = Modal::Picker("Actions", self.action_menu(), 0),
            ("export", []) => self.request_export(),
            ("save", []) => self.save_resource_selection()?,
            ("selections", []) => self.open_saved_selections()?,
            ("edit"|"yaml-edit",[])=>self.yaml_editor(),
            ("edit-container",[])=>self.editor(),
            ("cordon",[])=>self.node_maintenance(crate::node_maintenance::Request::Cordon),
            ("uncordon",[])=>self.node_maintenance(crate::node_maintenance::Request::Uncordon),
            ("drain",[])=>self.node_maintenance(crate::node_maintenance::Request::Drain{seconds:120}),
            ("ctx" | "context" | "contexts", []) => {
                let list=self.recent.order_contexts(contexts(&self.options.connection)?);
                let index=list.iter().position(|name|name==&self.context).unwrap_or(0);
                self.modal=Modal::Picker("Context (Pod view)",list,index);
            }
            ("ctx" | "context" | "contexts", [context]) => {
                let available = contexts(&self.options.connection)?;
                if available.contains(context) && !self.busy {
                    self.push_view();
                    self.options.connection.context = Some(context.clone());
                    self.connect();
                } else { self.status = "Unknown context or request in progress; use :ctx to choose.".into(); }
            }
            ("ns" | "namespace" | "namespaces", []) => self.namespace_picker(),
            ("ns" | "namespace" | "namespaces", [ns]) => {
                let ns = if ns == "all" { "*" } else { ns.as_str() };
                if ns == "*" || safety::valid_id(ns) {
                    self.push_view(); self.namespace = ns.into(); self.namespace_override = true;self.desired_resource=None;
                    if let Ok(index)=self.resolve_resource("v1/pods"){self.kind_index=index;}
                    self.filter.clear(); self.selector.clear();self.field_selector.clear(); self.changed();
                } else { self.status = "Invalid namespace.".into(); }
            }
            ("all", []) => return self.command("ns all"),
            ("sort", [key, order]) if ["asc", "desc"].contains(&order.as_str()) => {
                self.sort_by_title(key, Some(order == "desc"))
            }
            ("sort", [key]) if !["name", "namespace", "ns", "status", "restarts", "age", "ready", "cpu", "memory", "mem"].contains(&key.as_str()) => {
                self.sort_by_title(key, None)
            }
            ("sort", [key]) => {
                let sort = match key.as_str() { "name" => Some(Sort::Name), "namespace" | "ns" => Some(Sort::Namespace), "status" => Some(Sort::Status), "restarts" => Some(Sort::Restarts), "age" => Some(Sort::Age), "ready"=>Some(Sort::Ready), "cpu"=>Some(Sort::Cpu),"memory"|"mem"=>Some(Sort::Memory), _ => None };
                if let Some(sort) = sort { self.set_sort(sort); } else { self.status = "Sort: name | namespace | status | restarts | age | ready | cpu | memory".into(); }
            }
            ("themes", []) => self.theme_picker()?,
            ("ui", [action]) if action == "reload" => self.reload_ui()?,
            ("overview", []) => self.modal = Modal::Input(OVERVIEW_CONTEXTS_LABEL, self.context.clone()),
            ("metrics", []) => { self.metrics_resources = false; self.request_metrics(); }
            ("metrics",[mode]) if mode=="resources"=>{self.metrics_resources=true;self.request_metrics();}
            ("discover",[])=>self.send(Task::Discover),
            ("pf"|"portforward"|"portforwards",[])=>self.modal=Modal::ForwardView,
            ("scale", []) => return self.scale(),
            ("image", []) => return self.image(),
            ("restart", []) => self.prepare(Action::Restart),
            ("delete", []) => self.prepare(Action::Delete),
            ("kill", []) => self.kill(),
            ("debug", []) => self.debug_container(None),
            ("debug", [target]) => self.debug_container(Some(target.clone())),
            ("trigger", []) => self.prepare(Action::Trigger),
            ("suspend", []) => self.prepare(Action::Suspend(true)),
            ("resume", []) => self.prepare(Action::Suspend(false)),
            ("rollback" | "undo", []) => self.prepare(Action::Rollback(None)),
            ("rollback" | "undo", [revision]) => match revision.parse::<u64>() {
                Ok(revision) if revision > 0 => self.prepare(Action::Rollback(Some(revision))),
                _ => self.status = "Use :rollback [REVISION]".into(),
            },
            ("faults" | "errors", []) => return self.key_event(KeyCode::Char('z'), crossterm::event::KeyModifiers::CONTROL),
            ("ext" | "extension" | "extensions", []) => match crate::extensions::list() {
                Ok(entries) if entries.is_empty() => {
                    self.status = "No extensions registered; see kborg extensions register.".into()
                }
                Ok(entries) => {
                    let lines = entries
                        .iter()
                        .map(|entry| format!("{:<24} {}", entry.id, entry.executable.display()))
                        .collect();
                    self.modal = Modal::Picker("Extensions", lines, 0);
                }
                Err(error) => self.status = error,
            },
            ("ext" | "extension", [name]) => self.run_extension(name),
            ("bench" | "be" | "benchmark" | "benchmarks", []) => self.bench(200, 5, "/"),
            ("bench" | "be" | "benchmark" | "benchmarks", [requests, concurrency, rest @ ..]) if rest.len() <= 1 => {
                match (requests.parse(), concurrency.parse()) {
                    (Ok(requests), Ok(concurrency)) => self.bench(
                        requests,
                        concurrency,
                        rest.first().map_or("/", String::as_str),
                    ),
                    _ => self.status = "Use :bench [REQUESTS CONCURRENCY [PATH]]".into(),
                }
            }
            ("bench" | "be" | "benchmark" | "benchmarks", _) => self.status = "Use :bench [REQUESTS CONCURRENCY [PATH]]".into(),
            ("apply", [path]) => self.apply_manifest(std::path::PathBuf::from(path)),
            ("apply", _) => self.status = "Use :apply FILE.yaml, or a in :dir on a manifest".into(),
            ("cp-from", [remote]) => self.copy_file(true, false, remote, None),
            ("cp-from", [remote, local]) if remote != "-r" => {
                self.copy_file(true, false, remote, Some(local))
            }
            ("cp-from", [flag, remote]) if flag == "-r" => self.copy_file(true, true, remote, None),
            ("cp-from", [flag, remote, local]) if flag == "-r" => {
                self.copy_file(true, true, remote, Some(local))
            }
            ("cp-to", [local, remote]) => self.copy_file(false, false, local, Some(remote)),
            ("cp-to", [flag, local, remote]) if flag == "-r" => {
                self.copy_file(false, true, local, Some(remote))
            }
            ("cp" | "cp-from" | "cp-to", _) => {
                self.status = "Use :cp-from [-r] /CONTAINER/PATH [LOCAL] or :cp-to [-r] LOCAL /CONTAINER/PATH".into()
            }
            ("pulse"|"pulses"|"hz"|"pu",[])=>self.send(Task::Pulses(self.namespace.clone())),
            ("xray"|"xrays"|"xr"|"x",[resource])=>self.send(Task::Xray(resource.clone(),self.namespace.clone())),
            ("xray"|"xrays"|"xr"|"x",[resource,ns]) if ns=="all" || ns=="*" || safety::valid_id(ns)=>self.send(Task::Xray(resource.clone(),if ns=="all"{"*".into()}else{ns.clone()})),
            ("xray"|"xrays"|"xr"|"x",_)=>self.status="Use :xray po|svc|dp|rs|sts|ds [NAMESPACE]".into(),
            ("containers"|"container",[])=>{if self.kind()=="pods"{self.drilldown();}else{self.status="Select a Pod before opening its containers.".into();}},
            ("workload"|"workloads"|"wk",[])=>self.send(Task::Workloads(self.namespace.clone())),
            ("can",[subject])=>self.send(Task::Can(self.namespace.clone(),subject.clone())),
            ("subjects"|"subject"|"rbac",[])=>self.send(Task::Subjects(self.namespace.clone(),None,"RBAC SUBJECTS")),
            ("users"|"user"|"usr",[])=>self.send(Task::Subjects(self.namespace.clone(),Some("User"),"RBAC USERS")),
            ("groups"|"group"|"grp",[])=>self.send(Task::Subjects(self.namespace.clone(),Some("Group"),"RBAC GROUPS")),
            ("dir"|"dirs"|"d"|"ls",[path])=>self.send(Task::Directory(path.into())),
            ("dir"|"dirs"|"d"|"ls",[])=>self.send(Task::Directory(".".into())),
            ("screendump"|"screendumps"|"sd",[])=>match crate::screendump::list(){Ok(records)=>{let titles=records.iter().map(|record|record.title.clone()).collect();self.saved=records;self.modal=Modal::Picker("Saved resources",titles,0);},Err(error)=>self.status=error},
            ("cow",_)=>self.modal=Modal::Detail(format!(" {}\n < {} >\n  \\   ^__^\n   \\  (oo)\\_______\n      (__)\\       )\\\n          ||----w |\n          ||     ||", "_".repeat(args.join(" ").len().min(70)),safety::safe_text(&args.join(" "),70)),0),
            ("can",_)=>self.status="Use :can u:USER | g:GROUP | s:NAMESPACE:SERVICEACCOUNT".into(),
            ("rbacs"|"policies",[])=>self.send(Task::Subjects(self.namespace.clone(),None,"RBAC SUBJECTS")),
            ("rbacs"|"policies"|"users"|"groups"|"subjects",_)=>self.status="Use :subjects, :users or :groups, then Enter on a subject; :can u:USER, g:GROUP or s:NAMESPACE:SA for one subject.".into(),
            ("logs",[selector])=>self.request_log_group(self.namespace.clone(),crate::logs::PodLogSelection::Selector(selector.clone()),false,true,false),
            ("logs-live",[selector])=>self.request_log_group(self.namespace.clone(),crate::logs::PodLogSelection::Selector(selector.clone()),false,true,true),
            ("logs-live",[])=>self.request_dynamic_workload_logs(),
            ("ask", []) => {
                if self.ask_screen.is_none() {
                    self.status = "Nothing captured to ask about; press : on the screen you want to ask about.".into();
                } else {
                    self.modal = Modal::Input(ASK_LABEL, String::new());
                }
            }
            ("ask", words) => {
                let question = words.join(" ");
                self.ask(&question);
            }
            ("copilot",[])=>{
                if let Some((kind,namespace,name))=self.selected_target() {
                    if self.busy {self.status="Wait for the current request before preparing Copilot.".into();}
                    else {
                        self.modal=Modal::AdviceLoading;
                        self.send(Task::AdvicePrepare(kind,namespace,name,self.selected_uid(),self.options.copilot_policy.clone()));
                        if !self.busy {
                            self.modal=Modal::AdviceDocument("COPILOT NOT STARTED".into(),"Request queue unavailable; open Copilot again when the worker is available.".into(),0);
                        }
                    }
                }else{self.status="Select a resource before :copilot.".into();}
            }
            ("helm-install",[])=>self.begin_helm_form(helm_ui::Intent::Install),
            ("helm-crds",[])=>self.begin_helm_form(helm_ui::Intent::InstallCrds),
            ("helm-crds-upgrade",[])=>self.begin_helm_form(helm_ui::Intent::UpgradeCrds),
            ("helm-crds-delete",[])=>self.begin_helm_form(helm_ui::Intent::DeleteCrds),
            ("helm-crds-migrate",[])=>self.begin_helm_form(helm_ui::Intent::MigrateCrds),
            ("helm-test",[])=>{
                let selected=match &self.modal {Modal::Helm(table)=>table.current(),_=>self.subviews.iter().rev().find_map(|modal|if let Modal::Helm(table)=modal{table.current()}else{None})};
                if let Some(release)=selected {self.begin_helm_form(helm_ui::Intent::Test(release));}else{self.status="Select a release in :helm before :helm-test or press t in its table.".into();}
            }
            ("helm",[])=>self.request_helm(helm_ui::Request::List(self.namespace.clone(),self.options.helm_storage),false),
            ("helm",[ns]) if ns=="all" || ns=="*" || safety::valid_id(ns)=>self.request_helm(helm_ui::Request::List(if ns=="all"{"*".into()}else{ns.clone()},self.options.helm_storage),false),
            ("history",[name]) if crate::helm::valid_release_name(name)=>self.request_helm(helm_ui::Request::NamedHistory(self.namespace.clone(),name.clone(),self.options.helm_storage),false),
            ("history",[])=>{
                let selected=self.subviews.iter().rev().find_map(|modal|if let Modal::Helm(table)=modal{table.selected().cloned()}else{None});
                if let Some(release)=selected{self.request_helm(helm_ui::Request::History(release),false);}else{self.status="Use :history RELEASE or select a release in :helm and press Enter.".into();}
            }
            ("helm"|"history",_)=>self.status="Use :helm [NAMESPACE] or :history RELEASE; --allow-helm enables live release storage reads.".into(),
            ("lint" | "popeye" | "pop" | "sanitize", []) => self.send(Task::Lint(self.namespace.clone())),
            ("scans"|"scan",[])=>self.scan(false),
            ("scans"|"scan",[scope]) if matches!(scope.as_str(),"ns"|"namespace")=>self.scan(true),
            ("scans"|"scan",_)=>self.status="Use :scan for the selected Pod/workload or :scan ns for the namespace.".into(),
            ("secrets"|"secret"|"sec",[])=>self.secrets(),
            ("secrets"|"secret"|"sec",_)=>self.status="Use :secrets in the current namespace; values stay hidden until one key is revealed.".into(),
            _ => {
                let command = match nav::resource(&words) { Ok(command)=>command, Err(error)=>{self.status=error;return Ok(false);} };
                if !nav::valid_resource(&command.resource){self.status="Invalid resource command syntax.".into();return Ok(false);}
                let cross_context=command.context.as_ref().is_some_and(|context|context!=&self.context);
                let index = match self.resolve_resource(&command.resource) {Ok(index)=>Some(index),Err(_) if cross_context=>None,Err(error)=>{self.status=error;return Ok(false);}};
                if let Some(ns) = &command.namespace && ns != "all" && ns != "*" && !safety::valid_id(ns) { self.status="Invalid namespace.".into(); return Ok(false); }
                if let Err(error)=nav::Filter::parse(&command.filter) {self.status=error;return Ok(false);}
                if let Err(error)=crate::selectors::validate(&command.selector){self.status=error;return Ok(false);}
                if let Some(context)=&command.context {
                    if self.busy {self.status="Wait for the current request before changing context.".into();return Ok(false);}
                    if !contexts(&self.options.connection)?.contains(context) {self.status="Unknown context; use :ctx to choose.".into();return Ok(false);}
                }
                self.push_view();
                let requested_resource=index.map(|index|self.kinds[index].key()).unwrap_or_else(||command.resource.clone());
                if let Some(index)=index{self.kind_index=index;}
                self.desired_resource=cross_context.then_some(requested_resource);
                self.filter=command.filter;
                self.selector=command.selector;self.field_selector.clear();
                let explicit_namespace=command.namespace.is_some();
                if let Some(ns)=command.namespace {self.namespace=if ns=="all" {"*".into()} else {ns};self.namespace_override=true;}
                if let Some(context)=command.context.filter(|context|*context!=self.context) {
                    self.options.connection.context=Some(context);self.connect();
                    self.namespace_override=explicit_namespace;
                } else {self.changed();}
                self.status=format!("{} / {} | : for commands, / for filter",self.kind(),self.namespace);
            }
        }
        Ok(false)
    }
    pub(super) fn scale(&mut self) -> Result<bool> {
        if !self.options.connection.writable {
            self.status = "READ ONLY: scale requires --write.".into();
        } else if !["deployments.apps", "statefulsets.apps"].contains(&self.kind().as_str()) {
            self.status = "Scale supports Deployments and StatefulSets; other scale workflows are unavailable.".into();
        } else if let Some((k, n, name)) = self.selected_target() {
            self.modal = Modal::Scale(k, n, name, self.selected_uid(), String::new());
        }
        Ok(false)
    }
    pub(super) fn image(&mut self) -> Result<bool> {
        if !self.options.connection.writable {
            self.status = "READ ONLY: image edits require --write.".into();
        } else if !crate::edit::supports_resource(&self.kind()) {
            self.status =
                "Image edits support Deployment, StatefulSet, DaemonSet and CronJob containers."
                    .into();
        } else if let Some((k, n, name)) = self.selected_target() {
            self.modal = Modal::Image(k, n, name, self.selected_uid(), None, String::new());
        }
        Ok(false)
    }
    pub(super) fn suggestions(&self, text: &str) -> Vec<String> {
        let (prefix, token) = text
            .rsplit_once(' ')
            .map(|(prefix, token)| (format!("{prefix} "), token))
            .unwrap_or((String::new(), text));
        let mut values = if prefix.is_empty() {
            let mut values = nav::SPECIAL
                .iter()
                .map(|s| (*s).to_owned())
                .collect::<Vec<_>>();
            values.extend(self.aliases.keys().cloned());
            for kind in &self.kinds {
                values.extend([
                    kind.plural.clone(),
                    kind.kind.to_ascii_lowercase(),
                    kind.key(),
                ]);
                values.extend(kind.short_names.clone());
            }
            values
        } else if token.starts_with('@') || matches!(prefix.trim(), "ctx" | "context" | "contexts")
        {
            contexts(&self.options.connection)
                .unwrap_or_default()
                .into_iter()
                .map(|s| {
                    if token.starts_with('@') {
                        format!("@{s}")
                    } else {
                        s
                    }
                })
                .collect()
        } else if prefix.trim() == "sort" {
            vec![
                "name",
                "namespace",
                "status",
                "restarts",
                "age",
                "ready",
                "cpu",
                "memory",
            ]
            .into_iter()
            .map(str::to_owned)
            .collect()
        } else {
            let mut values = self.known_namespaces.clone();
            values.extend(["all".into(), self.namespace.clone()]);
            values
        };
        values.retain(|value| value.starts_with(token));
        values.sort();
        values.dedup();
        values
            .into_iter()
            .take(100)
            .map(|value| format!("{prefix}{value}"))
            .collect()
    }
    pub(super) fn remember_command(&mut self, text: &str) {
        let text = text.trim();
        if !text.is_empty() && self.history.last().is_none_or(|last| last != text) {
            self.history.push(text.into());
            if self.history.len() > 50 {
                self.history.remove(0);
            }
        }
        self.history_index = None;
        self.completion = None;
    }
    pub(super) fn editor(&mut self) {
        if self.busy {
            self.status = "Wait for the current request before editing.".into();
            return;
        }
        if !self.options.connection.writable {
            self.status =
                "READ ONLY: editing requires --write and a full server dry-run review.".into();
            return;
        }
        if !crate::edit::supports_resource(&self.kind()) {
            self.status="Controlled editing supports Deployment, StatefulSet, DaemonSet and CronJob containers. Other YAML fields are read only.".into();
            return;
        }
        if let Some(object) = self.visible().get(self.selected) {
            if self.selected_uid().is_none() {
                self.status = "Object UID unavailable; refresh before editing.".into();
                return;
            }
            match crate::edit::container_values(object) {
                Ok(values) => {
                    self.modal = Modal::EditContainers(
                        self.kind(),
                        namespace(object).into(),
                        name(object).into(),
                        self.selected_uid(),
                        values,
                        0,
                    )
                }
                Err(error) => self.status = error,
            }
        }
    }
    pub(super) fn drilldown(&mut self) {
        let Some(object) = self
            .visible()
            .get(self.selected)
            .map(|object| (*object).clone())
        else {
            return;
        };
        match (
            self.kind().as_str(),
            object["apiVersion"].as_str().unwrap_or(""),
            object["kind"].as_str().unwrap_or(""),
        ) {
            ("pods", "v1", "Pod") => self.send(Task::Drill(
                self.kind(),
                namespace(&object).into(),
                name(&object).into(),
            )),
            ("nodes", "v1", "Node") => {
                let node = name(&object).to_owned();
                if let Err(error) = self.command("v1/pods all") {
                    self.status = error;
                } else {
                    self.field_selector = format!("spec.nodeName={node}");
                    self.status = format!("Pods on node {node} | Esc back");
                }
            }
            ("namespaces", "v1", "Namespace") => {
                let namespace = name(&object).to_owned();
                if self.command(&format!("v1/pods {namespace}")).is_err() {
                    self.status = "Namespace navigation failed.".into();
                }
            }
            ("deployments.apps", "apps/v1", "Deployment")
            | ("statefulsets.apps", "apps/v1", "StatefulSet")
            | ("daemonsets.apps", "apps/v1", "DaemonSet")
            | ("replicasets.apps", "apps/v1", "ReplicaSet")
            | ("replicationcontrollers", "v1", "ReplicationController")
            | ("jobs.batch", "batch/v1", "Job")
            | ("cronjobs.batch", "batch/v1", "CronJob")
            | ("services", "v1", "Service") => match crate::insights::pod_selector(&object) {
                Ok(selector) => {
                    let namespace = namespace(&object);
                    if let Err(error) = self.command(&format!("v1/pods {namespace} '{selector}'")) {
                        self.status = error;
                    }
                }
                Err(error) => self.status = error,
            },
            _ => {
                if let Some((k, n, name)) = self.selected_target() {
                    self.send(Task::Inspect(k, n, name));
                }
            }
        }
    }
    pub(super) fn push_subview(&mut self, mut modal: Modal) {
        // A revealed Secret value never waits behind another view.
        if let Modal::Secrets(view) = &mut modal {
            view.hide();
        }
        if self.subviews.len() >= 16 {
            self.subviews.remove(0);
        }
        self.subviews.push(modal);
    }
    pub(super) fn close_subview(&mut self) {
        self.modal = self.subviews.pop().unwrap_or(Modal::None);
    }
    pub(super) fn selected_target(&self) -> Option<(String, String, String)> {
        self.visible().get(self.selected).map(|v| {
            (
                self.kind(),
                v["metadata"]["namespace"]
                    .as_str()
                    .unwrap_or(&self.namespace)
                    .into(),
                v["metadata"]["name"].as_str().unwrap_or("").into(),
            )
        })
    }
    /// Dashboard `L`: follow the selected warning's exact Pod in a pane, over a
    /// separate read-only connection to that card's context.
    /// A read-only log connection for one dashboard context: every write,
    /// interactive and Secret capability is cleared.
    pub(super) fn pane_connection(&self, context: &str, namespace: &str) -> Connection {
        let mut connection = self.options.connection.clone();
        if !connection.demo {
            connection.context = Some(context.into());
        }
        connection.namespace = namespace.into();
        connection.writable = false;
        connection.allow_exec = false;
        connection.allow_attach = false;
        connection.allow_port_forward = false;
        connection.allow_file_copy = false;
        connection.allow_apply = false;
        connection.allow_node_shell = false;
        connection.allow_debug = false;
        connection.allow_node_maintenance = false;
        connection.allow_yaml_edit = false;
        connection.allow_force_kill = false;
        connection.allow_secret_reveal = false;
        connection.allow_helm_write = false;
        connection.allow_helm_hooks = false;
        connection.allow_helm_crds = false;
        connection
    }
    /// Dashboard `t`: list workloads from every card's cluster, or close a
    /// running cross-cluster tail.
    pub(super) fn dashboard_tail(&mut self, dashboard: &mut dashboard_ui::Dashboard) {
        if dashboard.logs.take().is_some() {
            self.worker.logs.cancel();
            return;
        }
        if !self.options.connection.demo && !self.options.connection.logs {
            dashboard.set_notice("Workload tails require --allow-logs.".into(), true);
            return;
        }
        if self.busy {
            dashboard.set_notice(
                "Wait for the current request before opening a tail.".into(),
                true,
            );
            return;
        }
        let contexts = dashboard
            .rows
            .iter()
            .map(|row| {
                let namespace = if row.namespace.is_empty() {
                    self.namespace.clone()
                } else {
                    row.namespace.clone()
                };
                (
                    row.context.clone(),
                    Box::new(self.pane_connection(&row.context, &namespace)),
                )
            })
            .collect::<Vec<_>>();
        dashboard.picker = Some(dashboard_ui::TailPicker::loading());
        self.send(Task::DashboardWorkloads(contexts));
    }
    pub(super) fn start_dashboard_tail(
        &mut self,
        dashboard: &mut dashboard_ui::Dashboard,
        chosen: Vec<crate::workload_tail::WorkloadRef>,
    ) {
        if self.busy {
            dashboard.set_notice(
                "Wait for the current request before opening a tail.".into(),
                true,
            );
            return;
        }
        let _ = crate::dashboard_memory::remember_tail(&chosen);
        let sources = chosen
            .iter()
            .map(|workload| crate::workload_tail::TailSource {
                connection: self.pane_connection(&workload.context, &workload.namespace),
                workload: workload.clone(),
            })
            .collect::<Vec<_>>();
        let clusters = chosen
            .iter()
            .map(|w| w.context.as_str())
            .collect::<std::collections::BTreeSet<_>>()
            .len();
        self.subviews
            .retain(|modal| !matches!(modal, Modal::Follow(_) | Modal::Containers(..)));
        let generation = self.worker.logs.cancel();
        let read = crate::logs::ReadOptions {
            head: false,
            ..self.options.log_read
        };
        let mut view = crate::ui_logs::LogView::new("Connecting to workloads...".into(), read);
        view.following = true;
        dashboard.logs = Some(dashboard_ui::LogPane {
            title: format!(
                "TAIL {} workload{} across {clusters} cluster{} (timestamp order)",
                chosen.len(),
                if chosen.len() == 1 { "" } else { "s" },
                if clusters == 1 { "" } else { "s" }
            ),
            uid: String::new(),
            view: Box::new(view),
        });
        self.send(Task::DashboardTail(
            sources,
            generation,
            self.options.log_seconds,
            read,
        ));
    }
    pub(super) fn dashboard_logs(&mut self, dashboard: &mut dashboard_ui::Dashboard) {
        let selected = dashboard
            .selected_warning()
            .map(|w| (w.namespace.clone(), w.pod.clone(), w.uid.clone()));
        if let Some(pane) = dashboard.logs.take() {
            self.worker.logs.cancel();
            if selected.as_ref().is_none_or(|(_, _, uid)| *uid == pane.uid) {
                return;
            }
        }
        let Some((namespace, pod, uid)) = selected else {
            dashboard.set_notice(
                "Press w to select a warning Pod, then L to follow its logs.".into(),
                true,
            );
            return;
        };
        if !self.options.connection.demo && !self.options.connection.logs {
            dashboard.set_notice("Log panes require --allow-logs.".into(), true);
            return;
        }
        if uid.is_empty() || !safety::valid_id(&namespace) {
            dashboard.set_notice(
                "This warning has no exact Pod identity; logs are unavailable.".into(),
                true,
            );
            return;
        }
        if self.busy {
            dashboard.set_notice(
                "Wait for the current request before opening logs.".into(),
                true,
            );
            return;
        }
        let Some(context) = dashboard.selected().map(|row| row.context.clone()) else {
            return;
        };
        let connection = self.pane_connection(&context, &namespace);
        self.subviews
            .retain(|modal| !matches!(modal, Modal::Follow(_) | Modal::Containers(..)));
        let generation = self.worker.logs.cancel();
        let read = crate::logs::ReadOptions {
            head: false,
            ..self.options.log_read
        };
        let mut view = crate::ui_logs::LogView::new("Connecting to logs...".into(), read);
        view.following = true;
        dashboard.logs = Some(dashboard_ui::LogPane {
            title: format!("LOGS {context} / {namespace}/{pod} (exact UID)"),
            uid: uid.clone(),
            view: Box::new(view),
        });
        self.send(Task::DashboardLogs(
            Box::new(connection),
            namespace,
            pod,
            uid,
            generation,
            self.options.log_seconds,
            read,
        ));
    }
    pub(super) fn secrets(&mut self) {
        if !self.options.connection.allow_secret_reveal {
            self.status = "Secrets are excluded by default; start with --allow-secret-reveal to list keys and reveal one value at a time.".into();
            return;
        }
        if self.namespace == "all" || self.namespace == "*" {
            self.status = "Choose one namespace before opening :secrets.".into();
            return;
        }
        self.send(Task::Secrets(self.namespace.clone()));
    }
    /// k9s Ctrl-K: a reviewed, approved Pod delete with gracePeriodSeconds=0.
    pub(super) fn kill(&mut self) {
        if !self.options.connection.writable || !self.options.connection.allow_force_kill {
            self.status = "Force kill requires --write --allow-force-kill; Ctrl-D is a reviewed graceful delete.".into();
            return;
        }
        if self.kind() != "pods" {
            self.status = "Force kill applies to Pods only.".into();
            return;
        }
        self.prepare(Action::Kill);
    }
    /// Review handing the selected object to a registered read-only extension.
    pub(super) fn run_extension(&mut self, name: &str) {
        if !self.options.allow_extensions || !self.options.allow_tools {
            self.status = "Extensions require --allow-extensions --allow-tools.".into();
            return;
        }
        let Some(object) = self
            .visible()
            .get(self.selected)
            .map(|object| (*object).clone())
        else {
            self.status = "Select an object to hand to the extension.".into();
            return;
        };
        let review = crate::extensions::load(name).and_then(|registered| {
            let prepared = registered.prepare(&object, crate::handoff::Format::Json)?;
            let text = format!(
                "{}\n{}\nINPUT\n{}",
                registered.summary(),
                prepared.summary(),
                prepared.input_preview()
            );
            Ok((prepared, text))
        });
        match review {
            Ok((prepared, text)) => {
                let view =
                    review_ui::Review::new(text, crate::handoff::CONFIRMATION.into(), self.epoch);
                self.modal = Modal::WriteReview(Box::new(WriteReview {
                    plan: WritePlan::Tool(Box::new(prepared)),
                    view,
                }));
                self.status = "Read the whole input, then type RUN TOOL.".into();
            }
            Err(error) => self.status = error,
        }
    }
    /// Benchmark the selected listening port-forward over loopback.
    pub(super) fn bench(&mut self, requests: u32, concurrency: u32, path: &str) {
        let Some(port) = self.worker.forward.listening_port(self.forward_selected) else {
            self.status = "Select a LISTENING forward in :pf to benchmark it.".into();
            return;
        };
        match crate::bench::Plan::new(port, requests, concurrency, path) {
            Ok(plan) => {
                self.status = format!("Benchmarking 127.0.0.1:{port}{path}…");
                self.send(Task::Bench(plan));
            }
            Err(error) => self.status = error,
        }
    }
    /// `:scan` / `:scan ns`: read image references and detect the trusted
    /// scanner, then show a short review before anything runs.
    pub(super) fn scan(&mut self, namespace_scope: bool) {
        let Some(scanner) = self
            .options
            .scanner
            .clone()
            .filter(|_| self.options.allow_scan)
        else {
            self.status = "Image scanning requires --scanner ABS_PATH (Trivy or Grype) and --allow-scan. :lint checks misconfigurations.".into();
            return;
        };
        if self.options.connection.demo {
            self.status = "Image scanning is disabled in demo mode.".into();
            return;
        }
        let target = if namespace_scope {
            crate::scan::Target::Namespace(self.namespace.clone())
        } else {
            let kind = self.kind();
            let runs_containers = kind == "pods"
                || matches!(
                    kind.split('.').next(),
                    Some(
                        "deployments"
                            | "statefulsets"
                            | "daemonsets"
                            | "replicasets"
                            | "jobs"
                            | "cronjobs"
                    )
                );
            if !runs_containers {
                self.status =
                    "Select a Pod or workload to scan its images, or use :scan ns.".into();
                return;
            }
            let Some((resource, namespace, name)) = self.selected_target() else {
                self.status = "Select a Pod or workload first.".into();
                return;
            };
            crate::scan::Target::Object {
                resource,
                namespace,
                name,
            }
        };
        self.status = "Reading image references and checking the scanner…".into();
        self.send(Task::ScanPrepare(scanner, target));
    }
    /// Starts the reviewed scan detached from the worker queue.
    pub(super) fn start_scan(&mut self, plan: Box<crate::scan::Plan>, typed: String) {
        if self.busy {
            self.status = "Busy; wait for the current request, then press Enter again.".into();
            self.modal = Modal::ScanConfirm(plan, typed);
            return;
        }
        let control = std::sync::Arc::new(crate::scan::Control::default());
        let total = plan.images().len();
        self.scan_control = Some(control.clone());
        self.modal = Modal::ScanRunning(Instant::now(), total);
        self.send(Task::ScanRun(plan, typed, control));
        if !self.busy {
            self.scan_control = None;
            self.modal = Modal::Detail(
                "IMAGE SCAN NOT STARTED\n\nRequest queue unavailable; run :scan again.".into(),
                0,
            );
        }
    }
    pub(super) fn cancel_scan(&mut self) {
        if let Some(control) = &self.scan_control {
            control.cancel();
        }
        self.status = "Canceling the scan; stopping the scanner process group…".into();
    }
    /// Review an ephemeral debug container for the selected Pod.
    pub(super) fn debug_container(&mut self, target: Option<String>) {
        if !self.options.connection.writable || !self.options.connection.allow_debug {
            self.status = "Debug containers require --write --allow-debug.".into();
            return;
        }
        if self.options.connection.demo || self.kind() != "pods" {
            self.status = "Select a live Pod in :pods to debug it.".into();
            return;
        }
        let Some((_, namespace, pod)) = self.selected_target() else {
            self.status = "Select a Pod first.".into();
            return;
        };
        self.status = "Dry-running the debug container…".into();
        self.send(Task::DebugPrepare(
            namespace,
            pod,
            target,
            self.options.node_shell_image.clone(),
            self.options.session_seconds,
        ));
    }
    /// Capture what the user is looking at before the command prompt covers it.
    pub(super) fn capture_ask_screen(&mut self) {
        let (width, height) = terminal::size().unwrap_or((120, 40));
        self.ask_screen = snapshot::screen_text(self, width, height).ok();
    }
    /// Ask Copilot about the screen captured when the prompt opened. Without
    /// a policy and --allow-copilot-screen this shows a local preview only.
    pub(super) fn ask(&mut self, question: &str) {
        let Some(screen) = self.ask_screen.clone() else {
            self.status =
                "Nothing captured to ask about; open :ask from the screen you want to ask about."
                    .into();
            return;
        };
        let bundle = match crate::advice::screen_bundle(question, &screen) {
            Ok(bundle) => bundle,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        let policy = self.options.copilot_policy.clone();
        if policy.is_none() || !self.options.allow_copilot_screen {
            self.modal = Modal::AdviceDocument(
                "ASK COPILOT / local preview, nothing sent".into(),
                format!(
                    "Setup required to send: start KBorg with --copilot-policy ABS --allow-copilot-screen (see docs/COPILOT_INTEGRATION.md).\nThis is exactly what would be sent, after a review you must confirm:\n\n{bundle}"
                ),
                0,
            );
            return;
        }
        if self.busy {
            self.status = "Wait for the current request before asking Copilot.".into();
            return;
        }
        self.modal = Modal::AdviceLoading;
        self.send(Task::AskPrepare(question.to_owned(), screen, policy));
        if !self.busy {
            self.modal = Modal::AdviceDocument(
                "COPILOT NOT STARTED".into(),
                "Request queue unavailable; ask again when the worker is available.".into(),
                0,
            );
        }
    }
    /// Review a privileged node shell Pod for the selected Node.
    pub(super) fn node_shell(&mut self) {
        if !self.options.connection.writable || !self.options.connection.allow_node_shell {
            self.status = "Node shell requires --write --allow-node-shell.".into();
            return;
        }
        if self.options.connection.demo {
            self.status = "Node shell needs a live cluster.".into();
            return;
        }
        let Some((_, _, node)) = self.selected_target() else {
            self.status = "Select a Node first.".into();
            return;
        };
        let namespace = if self.namespace == "*" || self.namespace.is_empty() {
            "default".to_owned()
        } else {
            self.namespace.clone()
        };
        self.status = "Dry-running the node shell Pod…".into();
        self.send(Task::NodeShellPrepare(
            node,
            namespace,
            self.options.node_shell_image.clone(),
            self.options.session_seconds,
        ));
    }
    /// Dry-run and review a server-side apply of a local manifest.
    pub(super) fn apply_manifest(&mut self, path: std::path::PathBuf) {
        if !self.options.connection.writable || !self.options.connection.allow_apply {
            self.status = "Apply requires --write --allow-apply.".into();
            return;
        }
        if self.options.connection.demo {
            self.status = "Apply needs a live cluster; demo mode has no API server.".into();
            return;
        }
        self.status = "Dry-running every object in the manifest…".into();
        self.send(Task::ApplyPrepare(path, self.namespace.clone()));
    }
    /// Review a single-file copy for the selected Pod's default container.
    pub(super) fn copy_file(
        &mut self,
        download: bool,
        recursive: bool,
        first: &str,
        second: Option<&String>,
    ) {
        if self.busy {
            self.status = "Wait for the current request before copying.".into();
            return;
        }
        if !self.options.connection.writable
            || !self.options.connection.allow_file_copy
            || self.options.connection.demo
        {
            self.status = "File copy requires a live Pod, --write and --allow-file-copy.".into();
            return;
        }
        if self.kind() != "pods" {
            self.status = "Select a Pod in :pods first.".into();
            return;
        }
        let Some(object) = self
            .visible()
            .get(self.selected)
            .map(|object| (*object).clone())
        else {
            self.status = "Select a Pod first.".into();
            return;
        };
        let containers = object["spec"]["containers"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|container| container["name"].as_str())
            .collect::<Vec<_>>();
        let annotated =
            object["metadata"]["annotations"]["kubectl.kubernetes.io/default-container"]
                .as_str()
                .filter(|name| containers.contains(name));
        let Some(container) = annotated.or(containers.first().copied()).map(str::to_owned) else {
            self.status = "This Pod has no ordinary container.".into();
            return;
        };
        let request = if download {
            let local = second.map(std::path::PathBuf::from).unwrap_or_else(|| {
                std::env::var_os("HOME")
                    .map(|home| std::path::PathBuf::from(home).join("Downloads"))
                    .filter(|dir| dir.is_dir())
                    .unwrap_or_else(|| ".".into())
            });
            crate::ops::Request::CopyFrom {
                container,
                remote: first.to_owned(),
                local,
                recursive,
            }
        } else {
            let Some(remote) = second else {
                self.status = "Use :cp-to LOCAL_FILE /CONTAINER/FILE".into();
                return;
            };
            let local = std::path::Path::new(first);
            let upload = if recursive {
                crate::file_copy::read_upload_dir(local)
            } else {
                crate::file_copy::read_upload(local)
            };
            match upload {
                Ok(upload) => crate::ops::Request::CopyTo {
                    container,
                    upload,
                    remote: remote.clone(),
                },
                Err(error) => {
                    self.status = error;
                    return;
                }
            }
        };
        let namespace = object["metadata"]["namespace"]
            .as_str()
            .unwrap_or(&self.namespace)
            .to_owned();
        let pod = object["metadata"]["name"].as_str().unwrap_or("").to_owned();
        let uid = object["metadata"]["uid"].as_str().map(str::to_owned);
        self.send(Task::PrepareSession(namespace, pod, uid, request));
    }
    pub(super) fn yaml_editor(&mut self) {
        if self.busy {
            self.status = "Wait for the current request before editing.".into();
            return;
        }
        if !self.options.connection.writable || !self.options.connection.allow_yaml_edit {
            self.status = "YAML editing requires --write --allow-yaml-edit.".into();
            return;
        }
        if let Some((kind, ns, name)) = self.selected_target() {
            self.send(Task::YamlDraft(kind, ns, name, self.selected_uid()));
        }
    }
    pub(super) fn node_maintenance(&mut self, request: crate::node_maintenance::Request) {
        if self.busy {
            self.status = "Wait for the current request before node maintenance.".into();
            return;
        }
        if self.kind() != "nodes" {
            self.status = "Select a Node in :nodes first.".into();
            return;
        }
        if !self.options.connection.writable || !self.options.connection.allow_node_maintenance {
            self.status = "Node maintenance requires --write --allow-node-maintenance.".into();
            return;
        }
        if let Some((_, _, name)) = self.selected_target() {
            self.send(Task::NodePrepare(name, self.selected_uid(), request));
        }
    }
    pub(super) fn selected_uid(&self) -> Option<String> {
        self.visible()
            .get(self.selected)
            .and_then(|v| v["metadata"]["uid"].as_str())
            .map(str::to_owned)
    }
    pub(super) fn request_export(&mut self) {
        if self.busy || self.write_inflight {
            self.status = "Wait for the current request before exporting.".into();
            return;
        }
        if let Modal::Follow(view) = &self.modal {
            let source = if let Some(target) = &self.log_target {
                if let Some(membership) = &target.membership {
                    serde_json::json!({"context":self.context,"membership":membership})
                } else {
                    serde_json::json!({"context":self.context,"namespace":target.namespace,"pod":target.pod,"uid":target.uid,"container":target.container,"all_containers":target.all})
                }
            } else {
                serde_json::json!({"context":self.context,"source":"unavailable"})
            };
            let payload = format!("# Source: {}\n{}", source, view.export_text());
            match crate::exports::Draft::new(
                "logs",
                "log",
                "Captured logs / current filtered window",
                payload,
                "Application logs can contain credentials or other confidential data. This saves the retained window, including timestamps and source labels.",
            ) {
                Ok(draft) => {
                    let previous = std::mem::replace(&mut self.modal, Modal::Export(draft));
                    self.push_subview(previous);
                }
                Err(error) => self.status = error,
            }
        } else if let Modal::Helm(table) = &self.modal {
            if let Some(release) = table.current() {
                self.send(Task::ExportHelm(release));
            }
        } else if matches!(self.modal, Modal::None) {
            if let Some((kind, ns, name)) = self.selected_target() {
                self.send(Task::ExportResource(kind, ns, name));
            }
        } else {
            self.status = "Export from a resource table, Helm table or log view.".into();
        }
    }
    pub(super) fn replace_items(&mut self, items: Vec<Value>) {
        let identity = self.visible().get(self.selected).map(|v| {
            (
                v["metadata"]["namespace"].clone(),
                v["metadata"]["name"].clone(),
                v["metadata"]["uid"].clone(),
            )
        });
        self.items = items;
        self.items_generation += 1;
        self.selected = identity
            .and_then(|(ns, name, uid)| {
                self.visible().iter().position(|v| {
                    v["metadata"]["namespace"] == ns
                        && v["metadata"]["name"] == name
                        && v["metadata"]["uid"] == uid
                })
            })
            .unwrap_or_else(|| self.selected.min(self.visible().len().saturating_sub(1)));
        if let Some(uid) = self.restoring_uid.take() {
            self.selected = self
                .visible()
                .iter()
                .position(|object| object["metadata"]["uid"].as_str() == Some(&uid))
                .unwrap_or(self.selected);
        }
    }
    pub(super) fn send(&mut self, task: Task) {
        // Background refreshes (metrics) and the dashboard, which opens its
        // own connections, never hold the busy gate: user requests queue
        // behind them in the worker instead of being refused.
        let background = matches!(&task, Task::Metrics(..) | Task::Overview(..));
        if self.busy && !background && !matches!(&task, Task::Connect(_)) {
            self.status = "Busy; wait for the current request.".into();
            return;
        }
        let writes = matches!(
            &task,
            Task::Execute(..)
                | Task::BatchExecute(..)
                | Task::YamlExecute(..)
                | Task::ApplyExecute(..)
                | Task::NodeShellStart(..)
                | Task::DebugStart(..)
                | Task::NodeExecute(..)
                | Task::RunSession(..)
                | Task::RunForward(..)
                | Task::HelmExecute(..)
                | Task::AdviceExecute(..)
        );
        match self.worker.tx.try_send((self.epoch, task)) {
            Ok(()) if background => {}
            Ok(()) => {
                self.busy = true;
                self.command_epoch = Some(self.epoch);
                self.write_inflight = writes;
            }
            Err(_) => self.status = "Request queue is full/closed.".into(),
        }
    }
    pub(super) fn changed(&mut self) {
        self.worker.forward.cancel();
        self.selection_ui.restoring = None;
        self.selection_ui.focus = None;
        self.selection_ui.draft = None;
        self.epoch += 1;
        self.items.clear();
        self.items_generation += 1;
        self.selected = 0;
        self.stale = true;
        self.refresh = true;
        self.watching = false;
        self.watch_blocked = false;
        self.bounded_poll = false;
        self.sticky_status = false;
        self.worker.watch.invalidate(self.epoch);
        self.worker.logs.cancel();
        self.helm_generation = self.helm_generation.wrapping_add(1);
        self.metrics_history.clear();
        self.table_metrics.clear();
        self.last_table_metrics = None;
        self.overview = None;
        self.marks.clear();
        self.subviews.clear();
        self.log_target = None;
    }
    pub(super) fn connect(&mut self) {
        self.namespace_override = false;
        self.changed();
        self.connected = false;
        self.server = "connecting...".into();
        self.connecting_epoch = Some(self.epoch);
        self.send(Task::Connect(self.options.connection.clone()));
    }
    pub(super) fn expire_table_metrics(&mut self, now: Instant) -> bool {
        if !self.table_metrics.expired(now) {
            return false;
        }
        let uid = self.selected_uid();
        self.table_metrics.expire();
        if let Some(uid) = uid {
            self.selected = self
                .visible()
                .iter()
                .position(|object| object["metadata"]["uid"].as_str() == Some(&uid))
                .unwrap_or(self.selected);
        }
        true
    }
}
