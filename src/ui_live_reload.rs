//! Applying live config reloads. `config_watch` reports a saved file once it
//! is stable; here it is trust-checked, re-validated with the startup loader
//! and applied, or refused with the last good settings kept and a status line
//! that names the file, the line or field and a next step.
use super::*;
use crate::config_watch::ConfigFile;
use crate::errors::NEXT;

fn same_theme(a: &Theme, b: &Theme) -> bool {
    a.id == b.id && a.name == b.name && a.terminal == b.terminal && a.colors == b.colors
}

/// "Kept the last good settings" in every refusal, then its next step.
fn refused(file: &str, error: &str, next: &str) -> String {
    format!("{file} not applied: {error}. The last good settings stay. {NEXT} {next}")
}

fn next_step(file: ConfigFile) -> &'static str {
    match file {
        ConfigFile::Ui => "fix it and save again; kborg ui validate shows the problem.",
        ConfigFile::Keymap => "fix it and save again; :keymap lists action names and keys.",
        ConfigFile::Hotkeys => "fix it and save again, or pick a key no action uses.",
        ConfigFile::Plugins => "check the registry with kborg extensions list.",
        _ => "fix it and save again.",
    }
}

impl App {
    /// One live-reload step for the event loop; true when the screen changed.
    pub(super) fn poll_config(&mut self, now: Instant) -> bool {
        if !self.config_watch.poll(now) {
            return false;
        }
        // Key changes never land under a typed confirmation or any other
        // dialog; theme changes wait while the theme picker previews.
        let quiet = matches!(self.modal, Modal::None | Modal::LiveOverview(..));
        let keys_ready = quiet && !self.write_inflight;
        let picking = matches!(self.modal, Modal::Themes(..) | Modal::NewTheme(..));
        let files = self.config_watch.take(|file| {
            if file.changes_keys() {
                keys_ready
            } else {
                !picking
                    || !matches!(
                        file,
                        ConfigFile::Ui | ConfigFile::Appearance | ConfigFile::Theme
                    )
            }
        });
        if files.is_empty() {
            if self.config_watch.announce() {
                let names = self
                    .config_watch
                    .waiting()
                    .iter()
                    .map(|file| file.name())
                    .collect::<Vec<_>>()
                    .join(", ");
                self.status = format!("{names} changed; applies when this dialog closes.");
                return true;
            }
            return false;
        }
        let mut applied = Vec::new();
        let mut failures = Vec::new();
        let mut keys_done = false;
        for file in files {
            let result = match file {
                ConfigFile::Hotkeys | ConfigFile::Keymap if keys_done => continue,
                ConfigFile::Hotkeys | ConfigFile::Keymap => {
                    keys_done = true;
                    self.apply_keys(file)
                }
                _ => self.apply_config(file),
            };
            match result {
                Ok(true) => applied.push(file.name()),
                Ok(false) => {}
                Err(error) => failures.push(error),
            }
        }
        if let Some(first) = failures.first() {
            self.status = if failures.len() > 1 {
                format!("{first} (+{} more)", failures.len() - 1)
            } else {
                first.clone()
            };
            self.sticky_status = true;
        } else if !applied.is_empty() {
            self.status = format!(
                "Applied {}{}.",
                applied.join(", "),
                if self.config_watch.enabled() {
                    ""
                } else {
                    "; live reload is now off (ui.yaml reload: false)"
                }
            );
            self.sticky_status = false;
        } else {
            return false;
        }
        true
    }

    fn watched(&self, file: ConfigFile) -> std::result::Result<std::path::PathBuf, String> {
        self.config_watch.trusted(file)?;
        self.config_watch
            .path(file)
            .map(std::path::Path::to_path_buf)
            .ok_or_else(|| format!("{} is not watched. {NEXT} restart KBorg.", file.name()))
    }

    /// Apply one changed file; Ok(false) when nothing visible changed.
    fn apply_config(&mut self, file: ConfigFile) -> std::result::Result<bool, String> {
        let path = self.watched(file)?;
        let fail = |error: String| refused(file.name(), &error, next_step(file));
        match file {
            ConfigFile::Ui => {
                let ui = crate::ui_config::load_from(&path).map_err(fail)?;
                let (theme, error) = ui_theme(&ui, self.options.theme.as_deref()).map_err(fail)?;
                if ui.text != self.ui.text {
                    crate::text::set_unicode(ui.text.unicode());
                }
                self.config_watch.set_enabled(ui.reload);
                self.config_watch.watch_theme(&theme.id);
                self.ui = ui;
                self.theme = theme;
                *self.visible_cache.borrow_mut() = None;
                if let Some(error) = error {
                    return Err(format!(
                        "ui.yaml applied without its theme settings: {error}. {NEXT} fix theme or colors in ui.yaml and save again."
                    ));
                }
                Ok(true)
            }
            ConfigFile::Appearance | ConfigFile::Theme => {
                if file == ConfigFile::Theme && path.exists() {
                    let name = format!(
                        "themes/{}",
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .unwrap_or("theme")
                    );
                    crate::themes::load_file(&path)
                        .map_err(|error| refused(&name, &error, next_step(file)))?;
                }
                let (theme, error) =
                    ui_theme(&self.ui, self.options.theme.as_deref()).map_err(fail)?;
                if let Some(error) = error {
                    return Err(refused(file.name(), &error, "fix ui.yaml theme or colors."));
                }
                if same_theme(&theme, &self.theme) {
                    // KBorg's own picker save, or a change --theme overrides.
                    return Ok(false);
                }
                self.config_watch.watch_theme(&theme.id);
                self.theme = theme;
                Ok(true)
            }
            ConfigFile::Aliases => {
                self.aliases = crate::aliases::load_from(&path).map_err(fail)?;
                Ok(true)
            }
            ConfigFile::Views => {
                self.views = crate::views::load_from(&path).map_err(fail)?;
                *self.visible_cache.borrow_mut() = None;
                // The next scope sync re-applies the view's sortColumn.
                self.last_scope.2 = usize::MAX;
                Ok(true)
            }
            ConfigFile::Plugins => {
                let plugins = crate::plugins::Catalog::load_in(&path, &self.keymap, &self.hotkeys);
                if let Some(error) = plugins.error.clone() {
                    return Err(fail(error));
                }
                self.replace_plugins(plugins);
                Ok(true)
            }
            ConfigFile::Hotkeys | ConfigFile::Keymap => self.apply_keys(file),
        }
    }

    /// keymap.yaml and hotkeys.yaml are checked together (a hotkey may not
    /// take an action's key); plugin shortcuts are re-bound on top.
    fn apply_keys(&mut self, changed: ConfigFile) -> std::result::Result<bool, String> {
        let load = |file: ConfigFile| -> std::result::Result<std::path::PathBuf, String> {
            self.watched(file)
        };
        let hotkeys_path = load(ConfigFile::Hotkeys)?;
        let keymap_path = load(ConfigFile::Keymap)?;
        let hotkeys = crate::hotkeys::load_from(&hotkeys_path)
            .map_err(|error| refused("hotkeys.yaml", &error, next_step(ConfigFile::Hotkeys)))?;
        let keymap = crate::keymap::load_from(&keymap_path)
            .map_err(|error| refused("keymap.yaml", &error, next_step(ConfigFile::Keymap)))?;
        keymap
            .check_hotkeys(&hotkeys)
            .map_err(|error| refused(changed.name(), &error, next_step(changed)))?;
        let plugins = match self.config_watch.dir() {
            Some(dir) => {
                crate::plugins::Catalog::load_in(&dir.join("extensions"), &keymap, &hotkeys)
            }
            None => crate::plugins::Catalog::load(&keymap, &hotkeys),
        };
        self.keymap = keymap;
        self.hotkeys = hotkeys;
        self.replace_plugins(plugins);
        Ok(true)
    }

    fn replace_plugins(&mut self, plugins: crate::plugins::Catalog) {
        let directory = self.plugins.directory.take();
        self.plugins = plugins;
        self.plugins.directory = directory;
    }
}
