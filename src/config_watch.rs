//! Live config reload. A running TUI polls the few small files in the KBorg
//! config folder (size, mtime, inode and ctime from one `stat` each, no file
//! reads) every 500 ms on the event-loop tick. A file is reported only after
//! it has stayed unchanged for 200 ms, so editor save patterns (write a temp
//! file and rename it over, or truncate and write) are applied once, complete.
//! No platform watcher or extra crate is needed and network folders work.
//!
//! Detection only: the caller re-validates a reported file with the same strict
//! loader used at startup, after [`Watcher::trusted`] repeats the file-trust
//! checks, and keeps the last good settings when either fails.
use crate::{Result, safety};
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime},
};

/// Idle poll interval.
pub const POLL: Duration = Duration::from_millis(500);
/// Poll interval while a change is settling.
pub const SETTLING_POLL: Duration = Duration::from_millis(100);
/// A changed file must stay unchanged this long before it is applied.
pub const SETTLE: Duration = Duration::from_millis(200);
/// `KBORG_CONFIG_RELOAD=off` (or `0`, `false`) turns live reload off.
pub const ENV: &str = "KBORG_CONFIG_RELOAD";

/// One watched config source, in apply order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ConfigFile {
    Ui,
    /// appearance.json: the theme picker's saved choice.
    Appearance,
    /// The active custom theme file, themes/ID.json.
    Theme,
    Aliases,
    Hotkeys,
    Keymap,
    Views,
    /// The extension/plugin registry folder (entries added or removed).
    Plugins,
}

impl ConfigFile {
    pub const ALL: [Self; 8] = [
        Self::Ui,
        Self::Appearance,
        Self::Theme,
        Self::Aliases,
        Self::Hotkeys,
        Self::Keymap,
        Self::Views,
        Self::Plugins,
    ];
    /// The name shown to the user.
    pub fn name(self) -> &'static str {
        match self {
            Self::Ui => "ui.yaml",
            Self::Appearance => "appearance.json",
            Self::Theme => "theme file",
            Self::Aliases => "aliases.yaml",
            Self::Hotkeys => "hotkeys.yaml",
            Self::Keymap => "keymap.yaml",
            Self::Views => "views.yaml",
            Self::Plugins => "plugin registry",
        }
    }
    fn relative(self) -> &'static str {
        match self {
            Self::Ui => "ui.yaml",
            Self::Appearance => "appearance.json",
            Self::Theme => "themes",
            Self::Aliases => "aliases.yaml",
            Self::Hotkeys => "hotkeys.yaml",
            Self::Keymap => "keymap.yaml",
            Self::Views => "views.yaml",
            Self::Plugins => "extensions",
        }
    }
    /// The loaders' own size bounds.
    fn limit(self) -> u64 {
        match self {
            Self::Ui => 16 * 1024,
            Self::Appearance => 4096,
            _ => safety::MAX_FILE,
        }
    }
    /// Changes that alter what keys or commands do; they wait until no modal
    /// (and no typed confirmation) is open.
    pub fn changes_keys(self) -> bool {
        matches!(
            self,
            Self::Aliases | Self::Hotkeys | Self::Keymap | Self::Plugins
        )
    }
}

/// What one `stat` says about a path; `None` in [`Tracked`] means missing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    len: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64, u32),
}

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = fs::symlink_metadata(path).ok()?;
    #[cfg(unix)]
    let identity = {
        use std::os::unix::fs::MetadataExt;
        (
            meta.dev(),
            meta.ino(),
            meta.ctime(),
            meta.ctime_nsec(),
            meta.mode(),
        )
    };
    Some(Stamp {
        len: meta.len(),
        modified: meta.modified().ok(),
        #[cfg(unix)]
        identity,
    })
}

struct Tracked {
    file: ConfigFile,
    path: PathBuf,
    /// The state last reported (or seen at start).
    seen: Option<Stamp>,
    /// A newer state and when it was first observed.
    candidate: Option<(Option<Stamp>, Instant)>,
}

/// Polls the config folder; see the module documentation.
pub struct Watcher {
    dir: Option<PathBuf>,
    tracked: Vec<Tracked>,
    enabled: bool,
    next_poll: Option<Instant>,
    pending: Vec<ConfigFile>,
    /// A deferred key change was already announced.
    announced: bool,
}

impl Default for Watcher {
    fn default() -> Self {
        Self::disabled()
    }
}

impl Watcher {
    /// A watcher that never reports anything (tests, unavailable folder).
    pub fn disabled() -> Self {
        Self {
            dir: None,
            tracked: Vec::new(),
            enabled: false,
            next_poll: None,
            pending: Vec::new(),
            announced: false,
        }
    }
    /// Watch `dir`; the current state of every file is the baseline.
    pub fn new(dir: &Path, theme_id: &str, enabled: bool) -> Self {
        let tracked = ConfigFile::ALL
            .into_iter()
            .map(|file| {
                let path = if file == ConfigFile::Theme {
                    theme_path(dir, theme_id)
                } else {
                    dir.join(file.relative())
                };
                Tracked {
                    file,
                    seen: stamp(&path),
                    path,
                    candidate: None,
                }
            })
            .collect();
        Self {
            dir: Some(dir.to_path_buf()),
            tracked,
            enabled,
            next_poll: None,
            pending: Vec::new(),
            announced: false,
        }
    }
    /// The session watcher: the KBorg config folder unless `KBORG_CONFIG_RELOAD`
    /// is off or ui.yaml says `reload: false`.
    pub fn from_env(theme_id: &str, ui_reload: bool) -> Self {
        let off = std::env::var(ENV).is_ok_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "0" | "off" | "false" | "no"
            )
        });
        match safety::config_dir() {
            Ok(dir) => Self::new(&dir, theme_id, ui_reload && !off),
            Err(_) => Self::disabled(),
        }
    }
    pub fn enabled(&self) -> bool {
        self.enabled && self.dir.is_some()
    }
    /// `ui.yaml` `reload:` after a reload. Re-enabling starts from the current
    /// state, so edits made while off are not replayed.
    pub fn set_enabled(&mut self, enabled: bool) {
        if enabled && !self.enabled {
            for tracked in &mut self.tracked {
                tracked.seen = stamp(&tracked.path);
                tracked.candidate = None;
            }
        }
        if !enabled {
            self.pending.clear();
        }
        self.enabled = enabled;
    }
    /// Follow the active theme's file (built-in themes have none).
    pub fn watch_theme(&mut self, theme_id: &str) {
        let Some(dir) = &self.dir else { return };
        let path = theme_path(dir, theme_id);
        if let Some(tracked) = self
            .tracked
            .iter_mut()
            .find(|tracked| tracked.file == ConfigFile::Theme && tracked.path != path)
        {
            tracked.seen = stamp(&path);
            tracked.path = path;
            tracked.candidate = None;
        }
    }
    /// The watched path of `file`.
    pub fn path(&self, file: ConfigFile) -> Option<&Path> {
        self.tracked
            .iter()
            .find(|tracked| tracked.file == file)
            .map(|tracked| tracked.path.as_path())
    }
    pub fn dir(&self) -> Option<&Path> {
        self.dir.as_deref()
    }
    /// Check for changes when due. True when settled changes wait to be taken.
    pub fn poll(&mut self, now: Instant) -> bool {
        if !self.enabled() {
            return false;
        }
        if self.next_poll.is_some_and(|next| now < next) {
            return !self.pending.is_empty();
        }
        let mut settling = false;
        for tracked in &mut self.tracked {
            let current = stamp(&tracked.path);
            if current == tracked.seen {
                tracked.candidate = None;
                continue;
            }
            match tracked.candidate {
                Some((candidate, since)) if candidate == current => {
                    if now.saturating_duration_since(since) >= SETTLE {
                        tracked.seen = current;
                        tracked.candidate = None;
                        if !self.pending.contains(&tracked.file) {
                            self.pending.push(tracked.file);
                        }
                    } else {
                        settling = true;
                    }
                }
                _ => {
                    tracked.candidate = Some((current, now));
                    settling = true;
                }
            }
        }
        self.pending.sort();
        self.next_poll = Some(now + if settling { SETTLING_POLL } else { POLL });
        !self.pending.is_empty()
    }
    /// Remove and return the settled changes `ready` accepts, in apply order;
    /// the rest stay pending.
    pub fn take(&mut self, ready: impl Fn(ConfigFile) -> bool) -> Vec<ConfigFile> {
        let (taken, kept) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition(|file| ready(*file));
        self.pending = kept;
        if self.pending.is_empty() {
            self.announced = false;
        }
        taken
    }
    /// Settled changes still waiting (deferred key changes).
    pub fn waiting(&self) -> &[ConfigFile] {
        &self.pending
    }
    /// True once per deferral: the caller announces it on the status line.
    pub fn announce(&mut self) -> bool {
        !self.pending.is_empty() && !std::mem::replace(&mut self.announced, true)
    }
    /// The file-trust checks repeated on every reload: no symlink anywhere in
    /// the path, a regular file within the loader's size bound and, on Unix,
    /// owned by the config folder's owner and not writable by group or others.
    /// A missing file is trusted (the loader then uses defaults).
    pub fn trusted(&self, file: ConfigFile) -> Result<()> {
        let path = self.path(file).ok_or_else(|| {
            format!(
                "{} is not watched. Next: restart KBorg to load it.",
                file.name()
            )
        })?;
        trusted(file, path, self.dir.as_deref())
    }
}

fn theme_path(dir: &Path, theme_id: &str) -> PathBuf {
    let themes = dir.join("themes");
    if safety::valid_id(theme_id) {
        themes.join(format!("{theme_id}.json"))
    } else {
        themes.join(".none")
    }
}

fn trusted(file: ConfigFile, path: &Path, dir: Option<&Path>) -> Result<()> {
    let name = file.name();
    if safety::checked_path(path).is_err() {
        return Err(format!(
            "{name} refused: the path contains a symlink. Next: replace the link with a regular file in the KBorg config folder."
        ));
    }
    let meta = match fs::symlink_metadata(path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => {
            return Err(format!(
                "{name} refused: it cannot be inspected. Next: check the config folder permissions."
            ));
        }
    };
    if file == ConfigFile::Plugins {
        // The registry folder's own checks run in the extension loader.
        return Ok(());
    }
    if !meta.is_file() {
        return Err(format!(
            "{name} refused: not a regular file. Next: replace it with a regular file."
        ));
    }
    if meta.len() > file.limit() {
        return Err(format!(
            "{name} refused: larger than {} KiB. Next: shorten the file.",
            file.limit() / 1024
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.mode() & 0o022 != 0 {
            return Err(format!(
                "{name} refused: other users can write it (mode {:o}). Next: run chmod 600 on it.",
                meta.mode() & 0o777
            ));
        }
        if let Some(owner) = dir.and_then(|dir| fs::metadata(dir).ok()).map(|d| d.uid())
            && meta.uid() != owner
        {
            return Err(format!(
                "{name} refused: owned by another user. Next: recreate it as the config folder's owner."
            ));
        }
    }
    let _ = dir;
    Ok(())
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;
    use crate::errors::NEXT;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::atomic::{AtomicU64, Ordering};

    pub(crate) struct Folder(pub PathBuf);
    impl Drop for Folder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    pub(crate) fn folder() -> Result<Folder> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "temp dir")?;
        let path = root.join(format!(
            "kborg-watch-{}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        safety::ensure_private_dir(&path)?;
        Ok(Folder(path))
    }
    fn write(path: &Path, text: &str) -> Result<()> {
        fs::write(path, text).map_err(|_| "write")?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|_| "chmod")?;
        Ok(())
    }
    /// Poll at `now + at` milliseconds, past any scheduled interval.
    fn at(watcher: &mut Watcher, start: Instant, ms: u64) -> bool {
        watcher.next_poll = None;
        watcher.poll(start + Duration::from_millis(ms))
    }

    #[test]
    fn a_change_is_reported_only_after_it_stays_stable() -> Result<()> {
        let dir = folder()?;
        let ui = dir.0.join("ui.yaml");
        write(&ui, "version: 1\n")?;
        let mut watcher = Watcher::new(&dir.0, "nord", true);
        let start = Instant::now();
        assert!(!at(&mut watcher, start, 0), "the baseline is not a change");
        write(&ui, "version: 1\nborders: ascii\n")?;
        assert!(
            !at(&mut watcher, start, 10),
            "first sighting only starts the clock"
        );
        assert!(!at(&mut watcher, start, 150), "not stable for 200 ms yet");
        // A second write restarts the settle time.
        write(&ui, "version: 1\nborders: square\n")?;
        assert!(!at(&mut watcher, start, 260));
        assert!(!at(&mut watcher, start, 400));
        assert!(at(&mut watcher, start, 470));
        assert_eq!(watcher.take(|_| true), [ConfigFile::Ui]);
        assert!(!at(&mut watcher, start, 1000), "reported once");
        Ok(())
    }

    #[test]
    fn polling_is_rate_limited_and_faster_while_settling() -> Result<()> {
        let dir = folder()?;
        let mut watcher = Watcher::new(&dir.0, "nord", true);
        let start = Instant::now();
        watcher.poll(start);
        assert_eq!(watcher.next_poll, Some(start + POLL));
        write(&dir.0.join("views.yaml"), "views: {}\n")?;
        // Not due yet: the new file is not even looked at.
        assert!(!watcher.poll(start + Duration::from_millis(100)));
        assert!(watcher.tracked.iter().all(|t| t.candidate.is_none()));
        let later = start + POLL;
        watcher.poll(later);
        assert_eq!(watcher.next_poll, Some(later + SETTLING_POLL));
        // Worst case from save to report stays under one second.
        assert!(POLL + SETTLE + SETTLING_POLL < Duration::from_secs(1));
        Ok(())
    }

    #[test]
    fn rename_over_and_truncate_then_write_each_apply_once() -> Result<()> {
        let dir = folder()?;
        let keymap = dir.0.join("keymap.yaml");
        write(&keymap, "keys:\n  logs: l\n")?;
        let mut watcher = Watcher::new(&dir.0, "nord", true);
        let start = Instant::now();
        at(&mut watcher, start, 0);
        // Editor pattern 1: write a temp sibling, then rename it over.
        let temp = dir.0.join(".keymap.yaml.swp");
        write(&temp, "keys:\n  logs: L\n")?;
        assert!(!at(&mut watcher, start, 10), "the temp file is not watched");
        fs::rename(&temp, &keymap).map_err(|_| "rename")?;
        assert!(!at(&mut watcher, start, 20));
        assert!(at(&mut watcher, start, 230));
        assert_eq!(watcher.take(|_| true), [ConfigFile::Keymap]);
        // Editor pattern 2: truncate (an empty file is seen) then write.
        fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&keymap)
            .map_err(|_| "truncate")?;
        assert!(!at(&mut watcher, start, 300));
        write(&keymap, "keys:\n  logs: Ctrl-L\n")?;
        assert!(
            !at(&mut watcher, start, 400),
            "the empty state never settled"
        );
        assert!(at(&mut watcher, start, 610));
        assert_eq!(watcher.take(|_| true), [ConfigFile::Keymap]);
        // Deleting a file is a change too (the loader then uses defaults).
        fs::remove_file(&keymap).map_err(|_| "remove")?;
        at(&mut watcher, start, 700);
        assert!(at(&mut watcher, start, 910));
        assert_eq!(watcher.take(|_| true), [ConfigFile::Keymap]);
        Ok(())
    }

    #[test]
    fn key_changes_can_wait_while_others_apply() -> Result<()> {
        let dir = folder()?;
        let mut watcher = Watcher::new(&dir.0, "nord", true);
        let start = Instant::now();
        at(&mut watcher, start, 0);
        write(&dir.0.join("keymap.yaml"), "keys: {}\n")?;
        write(&dir.0.join("ui.yaml"), "version: 1\n")?;
        at(&mut watcher, start, 10);
        assert!(at(&mut watcher, start, 220));
        assert_eq!(watcher.take(|file| !file.changes_keys()), [ConfigFile::Ui]);
        assert_eq!(watcher.waiting(), [ConfigFile::Keymap]);
        assert!(watcher.announce() && !watcher.announce(), "announced once");
        assert_eq!(watcher.take(|_| true), [ConfigFile::Keymap]);
        assert!(watcher.waiting().is_empty());
        Ok(())
    }

    #[test]
    fn disabled_watchers_report_nothing_and_resume_from_the_current_state() -> Result<()> {
        let dir = folder()?;
        let mut watcher = Watcher::new(&dir.0, "nord", false);
        let start = Instant::now();
        write(&dir.0.join("ui.yaml"), "version: 1\n")?;
        assert!(!at(&mut watcher, start, 0));
        assert!(!at(&mut watcher, start, 500));
        watcher.set_enabled(true);
        assert!(!at(&mut watcher, start, 600));
        assert!(
            !at(&mut watcher, start, 900),
            "edits made while off are not replayed"
        );
        assert!(!Watcher::disabled().poll(start));
        Ok(())
    }

    #[test]
    fn the_active_custom_theme_file_is_followed() -> Result<()> {
        let dir = folder()?;
        safety::ensure_private_dir(&dir.0.join("themes"))?;
        let mut watcher = Watcher::new(&dir.0, "nord", true);
        let start = Instant::now();
        at(&mut watcher, start, 0);
        watcher.watch_theme("mine");
        assert_eq!(
            watcher.path(ConfigFile::Theme),
            Some(dir.0.join("themes/mine.json").as_path())
        );
        write(&dir.0.join("themes/mine.json"), "{}")?;
        at(&mut watcher, start, 10);
        assert!(at(&mut watcher, start, 220));
        assert_eq!(watcher.take(|_| true), [ConfigFile::Theme]);
        // Ids that are not theme ids never become paths.
        watcher.watch_theme("../ui");
        assert_eq!(
            watcher.path(ConfigFile::Theme),
            Some(dir.0.join("themes/.none").as_path())
        );
        Ok(())
    }

    #[test]
    fn trust_failures_are_refused_with_a_next_step() -> Result<()> {
        let dir = folder()?;
        let watcher = Watcher::new(&dir.0, "nord", true);
        assert!(watcher.trusted(ConfigFile::Ui).is_ok(), "missing is fine");
        let ui = dir.0.join("ui.yaml");
        write(&ui, "version: 1\n")?;
        assert!(watcher.trusted(ConfigFile::Ui).is_ok());
        fs::set_permissions(&ui, fs::Permissions::from_mode(0o620)).map_err(|_| "chmod")?;
        let error = watcher.trusted(ConfigFile::Ui).err().unwrap_or_default();
        assert!(
            error.contains("other users can write it") && error.contains(NEXT),
            "{error}"
        );
        fs::remove_file(&ui).map_err(|_| "rm")?;
        // A symlink swapped in for the file.
        let target = dir.0.join("elsewhere.yaml");
        write(&target, "version: 1\n")?;
        std::os::unix::fs::symlink(&target, &ui).map_err(|_| "symlink")?;
        let error = watcher.trusted(ConfigFile::Ui).err().unwrap_or_default();
        assert!(error.contains("symlink") && error.contains(NEXT), "{error}");
        fs::remove_file(&ui).map_err(|_| "rm")?;
        // Over the loader's size bound.
        write(&ui, &"#".repeat(17 * 1024))?;
        let error = watcher.trusted(ConfigFile::Ui).err().unwrap_or_default();
        assert!(
            error.contains("larger than 16 KiB") && error.contains(NEXT),
            "{error}"
        );
        fs::remove_file(&ui).map_err(|_| "rm")?;
        // Not a regular file.
        fs::create_dir(&ui).map_err(|_| "mkdir")?;
        let error = watcher.trusted(ConfigFile::Ui).err().unwrap_or_default();
        assert!(
            error.contains("not a regular file") && error.contains(NEXT),
            "{error}"
        );
        Ok(())
    }
}
