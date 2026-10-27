//! Per-context settings in the TUI (`contexts/NAME.yaml`, see
//! `crate::context_config`): applied before every connect so read-only and
//! strict confirmations reach the gateway, re-checked against the context the
//! connection actually selected, and live-reloaded with the other config
//! files. Restrictions only tighten: KBorg never turns writes back on that the
//! command line or the operator config turned off.
use super::*;
use crate::context_config::{self, Badge, ContextConfig};
use crate::errors::NEXT;
use std::path::PathBuf;

#[derive(Default)]
pub(super) struct State {
    /// The contexts folder; `None` turns per-context settings off.
    pub(super) directory: Option<PathBuf>,
    /// The context whose settings are applied.
    pub(super) context: Option<String>,
    pub(super) config: Option<ContextConfig>,
    /// The file exists but is invalid or untrusted: the context is read-only.
    pub(super) error: Option<String>,
    /// KBorg turned writes off for this context (and may turn them back on).
    forced_read_only: bool,
    /// KBorg turned strict confirmations on for this context.
    forced_strict: bool,
    /// A context theme replaced the session theme.
    theme: Option<Theme>,
    /// The next connection applies the context's default view.
    pub(super) view_pending: bool,
    /// `:ctx-config` asked for $EDITOR on the next loop.
    pub(super) pending_edit: bool,
    /// Edited text that failed validation, reopened by the next `:ctx-config`.
    draft: Option<(String, String)>,
}

impl State {
    pub(super) fn new(directory: Option<PathBuf>) -> Self {
        Self {
            directory,
            ..Self::default()
        }
    }
    pub(super) fn badge(&self) -> Option<Badge> {
        if self.error.is_some() {
            return Some(Badge {
                label: "CTX CONFIG?".into(),
                color: "danger".into(),
            });
        }
        self.config.as_ref().and_then(ContextConfig::badge)
    }
}

fn set_strict(strict: bool) {
    #[cfg(test)]
    review_ui::set_thread_strict(Some(strict));
    #[cfg(not(test))]
    review_ui::set_strict(strict);
}

impl App {
    /// Before a connect: apply the settings of the context it selects.
    pub(super) fn prepare_context_settings(&mut self) {
        if self.ctx_settings.directory.is_none() {
            return;
        }
        let Ok(target) = crate::gateway::selected_context(&self.options.connection) else {
            return;
        };
        let switching = self.ctx_settings.context.as_deref() != Some(target.as_str());
        if switching {
            // Explicit navigation (`:dp @ctx`, dashboard drill-in) keeps its resource.
            self.ctx_settings.view_pending = self.desired_resource.is_none();
            self.load_context_settings(&target);
        }
    }

    /// After a connect: the context the gateway really selected. True when
    /// the connection was restarted because that context restricts writes.
    pub(super) fn connected_context_settings(&mut self, context: &str) -> bool {
        if self.ctx_settings.directory.is_none()
            || self.ctx_settings.context.as_deref() == Some(context)
        {
            return false;
        }
        self.ctx_settings.view_pending = self.desired_resource.is_none();
        if self.load_context_settings(context) {
            self.options.connection.context = Some(context.into());
            self.connect();
            return true;
        }
        false
    }

    /// The context's default namespace unless `--namespace` (or the operator
    /// config) chose one.
    pub(super) fn context_namespace(&self) -> Option<String> {
        if !self.options.connection.namespace.is_empty() {
            return None;
        }
        self.ctx_settings
            .config
            .as_ref()
            .and_then(ContextConfig::namespace)
    }

    /// The default view, once per context switch.
    pub(super) fn take_context_view(&mut self) -> Option<String> {
        if !std::mem::take(&mut self.ctx_settings.view_pending) || self.desired_resource.is_some() {
            return None;
        }
        self.ctx_settings
            .config
            .as_ref()
            .and_then(|config| config.view.clone())
    }

    /// Read and apply the settings for `context`. True when write access
    /// changed, so the gateway must reconnect to pick it up.
    pub(super) fn load_context_settings(&mut self, context: &str) -> bool {
        let Some(directory) = self.ctx_settings.directory.clone() else {
            return false;
        };
        let (config, error) = match context_config::load_in(&directory, context) {
            Ok(config) => (config, None),
            Err(error) => (None, Some(error)),
        };
        self.config_watch
            .watch_context(context_config::path_in(&directory, context).ok().as_deref());
        let state = &mut self.ctx_settings;
        state.context = Some(context.into());
        let read_only = error.is_some() || config.as_ref().is_some_and(|c| c.read_only);
        let strict = config.as_ref().is_some_and(|c| c.strict_confirmations);
        let was_writable = self.options.connection.writable;
        if read_only && self.options.connection.writable {
            self.options.connection.writable = false;
            state.forced_read_only = true;
        } else if !read_only && state.forced_read_only {
            // Only undo what KBorg itself turned off.
            self.options.connection.writable = true;
            state.forced_read_only = false;
        }
        if strict && !state.forced_strict && !self.options.strict_confirm {
            state.forced_strict = true;
            set_strict(true);
        } else if !strict && state.forced_strict {
            state.forced_strict = false;
            set_strict(self.options.strict_confirm);
        }
        state.config = config;
        state.error = error.clone();
        let theme_error = self.apply_context_theme();
        let file = context_config::file_name(context).unwrap_or_default();
        if let Some(error) = error {
            self.status = format!(
                "contexts/{} refused, context is READ ONLY: {error}. {NEXT} :ctx-config fixes it.",
                safety::safe_text(&file, 80)
            );
            self.sticky_status = true;
        } else if let Some(error) = theme_error {
            self.status = format!(
                "contexts/{} theme unavailable: {error}. {NEXT} pick an id from kborg theme list.",
                safety::safe_text(&file, 80)
            );
        }
        was_writable != self.options.connection.writable
    }

    /// Use the context's theme, or go back to the session theme. Called after
    /// anything else replaced `self.theme` (ui.yaml or theme reloads).
    pub(super) fn apply_context_theme(&mut self) -> Option<String> {
        let wanted = self
            .ctx_settings
            .config
            .as_ref()
            .and_then(|config| config.theme.clone());
        match wanted {
            Some(id) => match themes::find(&id) {
                Ok(theme) => {
                    let theme = self.ui.apply(&theme).unwrap_or(theme);
                    self.config_watch.watch_theme(&theme.id);
                    self.ctx_settings.theme = Some(theme.clone());
                    self.theme = theme;
                    None
                }
                Err(error) => {
                    self.restore_session_theme();
                    Some(error)
                }
            },
            None => {
                self.restore_session_theme();
                None
            }
        }
    }

    fn restore_session_theme(&mut self) {
        if self.ctx_settings.theme.take().is_some()
            && let Ok((theme, _)) = ui_theme(&self.ui, self.options.theme.as_deref())
        {
            self.config_watch.watch_theme(&theme.id);
            self.theme = theme;
        }
    }

    /// Reconnect after write access changed, keeping the namespace and view.
    pub(super) fn reconnect_for_context_settings(&mut self) {
        if let Some(context) = self.ctx_settings.context.clone() {
            let namespace = self.namespace.clone();
            self.options.connection.context = Some(context);
            self.connect();
            self.namespace = namespace;
            self.namespace_override = true;
        }
    }

    /// Live reload of the current context's file.
    pub(super) fn reload_context_settings(&mut self) -> std::result::Result<bool, String> {
        let Some(context) = self.ctx_settings.context.clone() else {
            return Ok(false);
        };
        if self.load_context_settings(&context) {
            self.reconnect_for_context_settings();
        }
        match &self.ctx_settings.error {
            Some(error) => Err(format!(
                "context settings refused, context is READ ONLY: {error}. {NEXT} :ctx-config fixes it."
            )),
            None => Ok(true),
        }
    }

    /// `:ctx-config`: open this context's file in $VISUAL/$EDITOR.
    pub(super) fn edit_context_settings(&mut self) {
        let Some(directory) = self.ctx_settings.directory.clone() else {
            self.status = format!(
                "Per-context settings are unavailable: no KBorg config folder. {NEXT} set KBORG_CONFIG_DIR."
            );
            return;
        };
        let context = self
            .ctx_settings
            .context
            .clone()
            .unwrap_or_else(|| self.context.clone());
        let path = match context_config::path_in(&directory, &context) {
            Ok(path) => path,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        if !self.options.external_editor {
            self.status = format!(
                "The external editor is off (operator config). Edit {} yourself; it applies within 1 s.",
                safety::safe_text(&path.display().to_string(), 200)
            );
            return;
        }
        self.ctx_settings.pending_edit = true;
    }

    /// The text to hand to the editor: a rejected draft, the file, or the template.
    pub(super) fn context_settings_text(&self) -> Result<(String, String)> {
        let directory = self
            .ctx_settings
            .directory
            .as_ref()
            .ok_or("per-context settings are unavailable")?;
        let context = self
            .ctx_settings
            .context
            .clone()
            .unwrap_or_else(|| self.context.clone());
        if let Some((draft_context, text)) = &self.ctx_settings.draft
            && *draft_context == context
        {
            return Ok((context, text.clone()));
        }
        let text = context_config::text_in(directory, &context)?;
        Ok((context, text))
    }

    /// Store what came back from $EDITOR and apply it at once.
    pub(super) fn finish_context_edit(&mut self, context: &str, edited: Result<String>) {
        let Some(directory) = self.ctx_settings.directory.clone() else {
            return;
        };
        let edited = match edited {
            Ok(edited) => edited,
            Err(error) => {
                self.status = error;
                return;
            }
        };
        match context_config::save_in(&directory, context, &edited) {
            Ok(config) => {
                self.ctx_settings.draft = None;
                let summary = config.summary();
                if self.ctx_settings.context.as_deref() == Some(context) {
                    if self.load_context_settings(context) {
                        self.reconnect_for_context_settings();
                    }
                    // KBorg's own save; the watcher need not apply it again.
                    self.config_watch.watch_context(None);
                    self.config_watch.watch_context(
                        context_config::path_in(&directory, context).ok().as_deref(),
                    );
                }
                self.status = format!(
                    "Saved settings for {}: {}.",
                    safety::safe_text(context, 80),
                    if summary.is_empty() {
                        "defaults"
                    } else {
                        &summary
                    }
                );
                self.sticky_status = false;
            }
            Err(error) => {
                self.ctx_settings.draft = Some((context.into(), edited));
                self.status = format!(
                    "Context settings not saved: {error}. {NEXT} :ctx-config reopens your edit."
                );
                self.sticky_status = true;
            }
        }
    }
}
