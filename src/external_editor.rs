//! Opt-in `$VISUAL`/`$EDITOR` handoff for the YAML spec editor (k9s `e`).
//! The draft goes to a private create-only file that is always removed; the
//! edited text returns to KBorg's own validation, dry run and review.
use crate::{Result, safety};
use std::path::{Path, PathBuf};

/// The editor program and its arguments from `$VISUAL`, then `$EDITOR`.
/// Values are split on whitespace (for example `code --wait`); no shell runs.
pub fn command() -> Result<Vec<String>> {
    let value = ["VISUAL", "EDITOR"]
        .iter()
        .find_map(|name| {
            std::env::var(name)
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .ok_or("set $VISUAL or $EDITOR to use an external editor")?;
    parse(&value)
}

fn parse(value: &str) -> Result<Vec<String>> {
    let words = value
        .split_whitespace()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if words.is_empty()
        || words.len() > 16
        || words
            .iter()
            .any(|word| word.len() > 4096 || safety::safe_text(word, 4096) != *word)
    {
        return Err(
            "$VISUAL/$EDITOR must be a plain program name with at most 16 arguments".into(),
        );
    }
    Ok(words)
}

struct Removed(PathBuf);
impl Drop for Removed {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn scratch(directory: &Path) -> Result<PathBuf> {
    safety::ensure_private_dir(directory)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "clock error")?
        .as_nanos();
    Ok(directory.join(format!("spec-{}-{stamp}.yaml", std::process::id())))
}

/// Runs the editor on `text` in the foreground terminal and returns the
/// edited text. The caller must have left the TUI's alternate screen.
pub fn edit(text: &str, limit: u64) -> Result<String> {
    let words = command()?;
    let path = scratch(&safety::config_dir()?.join("edit"))?;
    safety::write_new(&path, text.as_bytes())?;
    let _removed = Removed(path.clone());
    let status = std::process::Command::new(&words[0])
        .args(&words[1..])
        .arg(&path)
        .status()
        .map_err(|_| format!("cannot start editor {}", safety::safe_text(&words[0], 200)))?;
    if !status.success() {
        return Err("editor exited with an error; draft unchanged".into());
    }
    let bytes = safety::read_small(&path, limit)?;
    String::from_utf8(bytes).map_err(|_| "edited file is not UTF-8; draft unchanged".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn editor_values_split_into_plain_arguments_without_a_shell() {
        assert_eq!(parse("vim"), Ok(vec!["vim".to_owned()]));
        assert_eq!(
            parse("code --wait"),
            Ok(vec!["code".to_owned(), "--wait".to_owned()])
        );
        assert!(parse("   ").is_err());
        assert!(parse("vim\u{1b}[2J").is_err());
        assert!(parse(&"a ".repeat(17)).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn scratch_files_are_private_create_only_and_removed() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "temp")?
            .join(format!("kborg-editor-test-{}", std::process::id()));
        let path = scratch(&root)?;
        safety::write_new(&path, b"spec: {}\n")?;
        let mode = std::fs::metadata(&path)
            .map_err(|_| "meta")?
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
        assert!(safety::write_new(&path, b"again").is_err());
        drop(Removed(path.clone()));
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
}
