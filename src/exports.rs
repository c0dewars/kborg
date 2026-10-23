//! Explicit local exports. Files are private, bounded and never overwritten.
use crate::{Result, safety};
use std::{
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: usize = 512 * 1024;
const MAX_FILES: usize = 100;
static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub struct Draft {
    pub title: String,
    pub notice: &'static str,
    path: PathBuf,
    text: String,
}
impl Draft {
    pub fn new(
        kind: &str,
        extension: &str,
        title: &str,
        text: String,
        notice: &'static str,
    ) -> Result<Self> {
        Self::at(
            &safety::config_dir()?.join("exports"),
            kind,
            extension,
            title,
            text,
            notice,
        )
    }
    fn at(
        directory: &Path,
        kind: &str,
        extension: &str,
        title: &str,
        text: String,
        notice: &'static str,
    ) -> Result<Self> {
        if directory.to_str().is_none() {
            return Err(
                "export directory must have a UTF-8 path so its destination can be reviewed".into(),
            );
        }
        if !matches!(
            (kind, extension),
            ("logs", "log") | ("resource", "yaml") | ("helm", "json")
        ) || text.len() > MAX_BYTES
        {
            return Err("export exceeds its 512 KiB limit or uses an unsupported format".into());
        }
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "clock unavailable")?
            .as_millis();
        let serial = SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(Self {
            title: safety::safe_text(title, 256),
            notice,
            path: directory.join(format!("{kind}-{now}-{serial}.{extension}")),
            text,
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn review(&self) -> String {
        let destination = self
            .path
            .to_string_lossy()
            .chars()
            .flat_map(char::escape_default)
            .collect::<String>();
        format!(
            "{}\n\n{}\n\nFile (escaped): \"{}\"\nSize: {} bytes\n\nEnter saves this captured snapshot. Esc cancels.\nPrivate file; existing files are never replaced.",
            self.title,
            self.notice,
            destination,
            self.text.len()
        )
    }
    pub fn save(self) -> Result<PathBuf> {
        let parent = self.path.parent().ok_or("export has no directory")?;
        safety::ensure_private_dir(parent)?;
        let count = std::fs::read_dir(parent)
            .map_err(|_| "cannot inspect export directory")?
            .take(MAX_FILES)
            .count();
        if count >= MAX_FILES {
            return Err("100 exports saved; archive files before saving more".into());
        }
        safety::write_new(&self.path, self.text.as_bytes())?;
        Ok(self.path)
    }
}

pub fn resource(object: &serde_json::Value) -> Result<Draft> {
    if object["kind"].as_str() == Some("Secret") {
        return Err("Secret exports are disabled".into());
    }
    let mut object = object.clone();
    safety::redact(&mut object);
    Draft::new(
        "resource",
        "yaml",
        "Selected resource / redacted YAML",
        crate::inspect::yaml(&object)?,
        "Known sensitive fields are masked. Custom fields may still contain confidential data.",
    )
}
pub fn helm(detail: &crate::helm::Detail) -> Result<Draft> {
    let mut value = serde_json::to_value(detail).map_err(|_| "cannot encode Helm metadata")?;
    safety::redact(&mut value);
    Draft::new(
        "helm",
        "json",
        "Helm revision / metadata and resource identities",
        serde_json::to_string_pretty(&value).map_err(|_| "cannot encode Helm export")?,
        "Exports selected revision metadata and resource references. Values, notes, chart files and raw manifests are excluded.",
    )
}

#[cfg(test)]
pub(crate) fn fixture_at(directory: &Path) -> Result<Draft> {
    Draft::at(
        directory,
        "resource",
        "yaml",
        "Export fixture",
        "kind: Pod\n".into(),
        "Private test snapshot",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn private_exports_are_bounded_and_never_replace_files() -> Result<()> {
        let dir = std::fs::canonicalize(std::env::temp_dir())
            .map_err(|_| "temporary directory")?
            .join(format!(
                "kborg-exports-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
        let draft = Draft::at(
            &dir,
            "logs",
            "log",
            "logs",
            "captured\n".into(),
            "sensitive",
        )?;
        let path = draft.path.clone();
        let saved = draft.save()?;
        assert_eq!(saved, path);
        assert_eq!(
            std::fs::read_to_string(&path).map_err(|_| "read")?,
            "captured\n"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path)
                    .map_err(|_| "metadata")?
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        assert!(safety::write_new(&path, b"replace").is_err());
        assert!(Draft::at(&dir, "logs", "log", "logs", "x".repeat(MAX_BYTES + 1), "").is_err());
        std::fs::remove_dir_all(dir).map_err(|_| "cleanup")?;
        Ok(())
    }
}
