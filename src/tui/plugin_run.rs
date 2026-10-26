//! Registered extensions and k9s plugins in the TUI: shortcut dispatch,
//! variables from the focused view, and the review of the exact argv.
use super::*;
use crate::{keymap::Scope, plugins};

/// Built-in rows of the `:actions` menu; plugin rows follow them.
pub(super) const ACTION_ROWS: usize = 15;

/// What a plugin runs on: the selected table row, one container of a Pod,
/// or the selected Helm release.
enum Target {
    Table,
    Container(Box<Value>, String),
    Helm(Box<crate::helm::Release>),
}

fn target_of(modal: &Modal) -> Target {
    match modal {
        // The "ALL containers" row has no single container: empty name.
        Modal::PodContainers(object, index) => Target::Container(
            object.clone(),
            pod_log_choices(object)
                .get(*index)
                .filter(|container| *container != "*")
                .cloned()
                .unwrap_or_default(),
        ),
        Modal::Helm(table) if table.parent.is_none() => table
            .current()
            .map_or(Target::Table, |release| Target::Helm(Box::new(release))),
        _ => Target::Table,
    }
}

impl App {
    /// The view a plugin shortcut is checked in, for the focused modal.
    fn plugin_scope(&self) -> Option<Scope> {
        match &self.modal {
            Modal::None => Some(Scope::Table),
            Modal::PodContainers(..) => Some(Scope::Containers),
            Modal::Helm(table) if !table.is_filtering() && table.parent.is_none() => {
                Some(Scope::Helm)
            }
            _ => None,
        }
    }
    /// Runs the plugin bound to this key in the focused view. Bindings never
    /// shadow a registered action, a hotkey or a reserved key.
    pub(super) fn plugin_key(&mut self, code: KeyCode, modifiers: KeyModifiers) -> bool {
        let Some(scope) = self.plugin_scope() else {
            return false;
        };
        let Some(key) = crate::keymap::Key::from_event(code, modifiers) else {
            return false;
        };
        let Some(id) = self.plugins.find(scope, key).map(str::to_owned) else {
            return false;
        };
        if scope == Scope::Table
            && !self
                .plugins
                .meta(&id)
                .is_some_and(|meta| plugins::applies(&meta.scopes, &self.view_names()))
        {
            return false;
        }
        let modal = std::mem::replace(&mut self.modal, Modal::None);
        let target = target_of(&modal);
        let restore = !matches!(modal, Modal::None);
        if restore {
            self.push_subview(modal);
        }
        if !self.run_extension_on(&id, target) && restore {
            self.close_subview();
        }
        true
    }
    /// Names a k9s `scopes:` entry can use for the current table resource.
    pub(super) fn view_names(&self) -> Vec<String> {
        let key = self.kind();
        let mut names = vec![key.clone()];
        if let Some(plural) = key.split('.').next() {
            names.push(plural.to_owned());
        }
        if let Some(kind) = self.kinds.iter().find(|kind| kind.key() == key) {
            names.push(kind.kind.to_ascii_lowercase());
            names.extend(
                kind.short_names
                    .iter()
                    .map(|name| name.to_ascii_lowercase()),
            );
        }
        names
    }
    /// Object, view names and variable values for a plugin run.
    fn plugin_input(
        &self,
        target: &Target,
        needed: &std::collections::BTreeSet<String>,
    ) -> Result<(Value, Vec<String>, plugins::Variables)> {
        let context = self.context.clone();
        match target {
            Target::Table => {
                let object = self
                    .visible()
                    .get(self.selected)
                    .map(|object| (*object).clone())
                    .ok_or("Select an object to hand to the extension.")?;
                let mut values = plugins::Variables::for_object(&object, &self.kind(), &context);
                values.set("FILTER", self.filter.clone());
                for name in needed {
                    if let Some(title) = name.strip_prefix("COL-") {
                        let title = title.replace('_', " ");
                        let shown = format::builtin_column(&title).is_some()
                            || self
                                .current_view()
                                .is_some_and(|view| view.columns.iter().any(|c| c.title == title));
                        if shown {
                            values.set(name, self.column_text(&object, &title));
                        }
                    }
                }
                Ok((object, self.view_names(), values))
            }
            Target::Container(_, container) if container.is_empty() => {
                Err("Choose one container for the plugin.".into())
            }
            Target::Container(pod, container) => {
                let mut values = plugins::Variables::for_object(pod, "pods", &context);
                let spec = ["containers", "initContainers", "ephemeralContainers"]
                    .iter()
                    .flat_map(|field| pod["spec"][field].as_array().into_iter().flatten())
                    .find(|spec| spec["name"].as_str() == Some(container));
                let status = [
                    "containerStatuses",
                    "initContainerStatuses",
                    "ephemeralContainerStatuses",
                ]
                .iter()
                .flat_map(|field| pod["status"][field].as_array().into_iter().flatten())
                .find(|status| status["name"].as_str() == Some(container));
                values.set("NAME", container.clone());
                values.set("CONTAINER", container.clone());
                values.set("FILTER", "");
                values.column("NAME", container.clone());
                values.column(
                    "IMAGE",
                    spec.and_then(|spec| spec["image"].as_str())
                        .unwrap_or_default(),
                );
                values.column(
                    "READY",
                    status.map_or("false", |status| {
                        if status["ready"] == true {
                            "true"
                        } else {
                            "false"
                        }
                    }),
                );
                values.column(
                    "RESTARTS",
                    status
                        .and_then(|status| status["restartCount"].as_u64())
                        .unwrap_or(0)
                        .to_string(),
                );
                values.column(
                    "STATE",
                    status
                        .and_then(|status| status["state"].as_object())
                        .and_then(|state| state.keys().next().cloned())
                        .unwrap_or_default(),
                );
                Ok((
                    (**pod).clone(),
                    vec!["containers".into(), "container".into()],
                    values,
                ))
            }
            Target::Helm(release) => {
                let object = serde_json::json!({
                    "kind": "HelmRelease",
                    "metadata": {"name": release.name, "namespace": release.namespace},
                    "revision": release.revision,
                    "status": release.status,
                    "chart": format!("{}-{}", release.chart_name, release.chart_version),
                    "appVersion": release.app_version,
                    "updated": release.updated,
                });
                let mut values = plugins::Variables::for_object(&object, "helm", &context);
                values.set("RESOURCE_VERSION", "");
                values.set("FILTER", "");
                for (title, value) in [
                    ("NAME", release.name.clone()),
                    ("NAMESPACE", release.namespace.clone()),
                    ("REVISION", release.revision.to_string()),
                    ("STATUS", release.status.clone()),
                    (
                        "CHART",
                        format!("{}-{}", release.chart_name, release.chart_version),
                    ),
                    ("APP VERSION", release.app_version.clone()),
                ] {
                    values.column(title, value);
                }
                Ok((object, vec!["helm".into()], values))
            }
        }
    }
    /// `:ext NAME`: runs on the view behind the command prompt (a Pod's
    /// container list, the Helm release list, otherwise the table).
    pub(super) fn run_extension(&mut self, name: &str) -> bool {
        let target = self.subviews.last().map_or(Target::Table, target_of);
        let behind = !matches!(target, Target::Table);
        let opened = self.run_extension_on(name, target);
        if !opened && behind && matches!(self.modal, Modal::None) {
            self.close_subview();
        }
        opened
    }
    /// Review handing the focused object to a registered extension. Returns
    /// whether the review opened.
    fn run_extension_on(&mut self, name: &str, target: Target) -> bool {
        if !self.options.allow_extensions || !self.options.allow_tools {
            self.status =
                "Extensions require permission (off in --read-only mode or by operator config)."
                    .into();
            return false;
        }
        let review = match &self.plugins.directory {
            Some(directory) => crate::extensions::load_from(directory, name),
            None => crate::extensions::load(name),
        }
        .and_then(|registered| {
            let needed = registered.variables();
            let (object, names, mut values) = self.plugin_input(&target, &needed)?;
            if let Some(meta) = registered.plugin()
                && !plugins::applies(&meta.scopes, &names)
            {
                return Err(format!(
                    "Plugin {name} applies to {}; it does not run here.",
                    meta.scopes.join(", ")
                ));
            }
            values.add_identity(&self.options.connection, Some(&self.context), &needed)?;
            let prepared =
                registered.prepare_with(&object, crate::handoff::Format::Json, &values)?;
            let text = format!(
                "{}\n{}\n{}\nINPUT\n{}",
                registered.summary(),
                crate::extensions::argv_review(&registered, &prepared),
                prepared.summary(),
                prepared.input_preview()
            );
            Ok((prepared, text))
        });
        match review {
            Ok((prepared, text)) => {
                let phrase = prepared.confirmation().to_owned();
                let view = review_ui::Review::new(text, phrase.clone(), self.epoch);
                self.modal = Modal::WriteReview(Box::new(WriteReview {
                    plan: WritePlan::Tool(Box::new(prepared)),
                    view,
                }));
                self.status = format!("Read the exact argv and input, then type {phrase}.");
                true
            }
            Err(error) => {
                self.status = safety::safe_text(&error, 512);
                false
            }
        }
    }
    /// Action-menu rows for registered plugins, after the built-in actions.
    pub(super) fn plugin_menu(&self) -> Vec<String> {
        let names = self.view_names();
        self.plugins
            .entries
            .iter()
            .map(|(id, meta)| {
                let key = self
                    .plugins
                    .key_for(id, Scope::Table)
                    .map(|key| format!(" ({key})"))
                    .unwrap_or_default();
                let title = format!(
                    "Plugin {id}{key}: {}",
                    if meta.description.is_empty() {
                        "registered k9s plugin"
                    } else {
                        &meta.description
                    }
                );
                if !self.options.allow_extensions || !self.options.allow_tools {
                    format!("[OFF] {title}: off (read-only or operator config)")
                } else if !plugins::applies(&meta.scopes, &names) {
                    format!("[OFF] {title}: applies to {}", meta.scopes.join(", "))
                } else {
                    format!("      {title}")
                }
            })
            .collect()
    }
    /// `?` help lines for plugins usable in `scope` (all scopes when `None`).
    pub(super) fn plugin_help(&self, scope: Option<Scope>) -> Vec<String> {
        if self.plugins.entries.is_empty() && self.plugins.error.is_none() {
            return Vec::new();
        }
        let mut out = vec!["PLUGINS (registered k9s plugins; :ext NAME runs any of them)".into()];
        if let Some(error) = &self.plugins.error {
            out.push(format!("  unavailable: {}", safety::safe_text(error, 200)));
        }
        for (id, meta) in &self.plugins.entries {
            let keys: Vec<String> = self
                .plugins
                .bindings
                .iter()
                .filter(|binding| {
                    &binding.id == id && scope.is_none_or(|scope| binding.scope == scope)
                })
                .map(|binding| {
                    if scope.is_some() {
                        binding.key.to_string()
                    } else {
                        format!("{} ({})", binding.key, binding.scope.name())
                    }
                })
                .collect();
            let shown_here = scope.is_none_or(|scope| match scope {
                Scope::Table => plugins::applies(&meta.scopes, &self.view_names()),
                Scope::Containers => plugins::applies(&meta.scopes, &["containers".into()]),
                Scope::Helm => plugins::applies(&meta.scopes, &["helm".into()]),
                _ => false,
            });
            if !shown_here {
                continue;
            }
            out.push(format!(
                "  {:<16} {}  {}{}",
                if keys.is_empty() {
                    "(no key)".into()
                } else {
                    keys.join(" / ")
                },
                safety::safe_text(&meta.description, plugins::MAX_DESCRIPTION),
                format_args!(":ext {id}"),
                if meta.dangerous { "  [dangerous]" } else { "" }
            ));
        }
        out.extend(
            self.plugins
                .notices
                .iter()
                .map(|notice| format!("  note: {}", safety::safe_text(notice, 200))),
        );
        out.push(String::new());
        out
    }
}
