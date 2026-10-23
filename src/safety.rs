use crate::Result;
use serde_json::Value;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub const MAX_FILE: u64 = 64 * 1024;

/// Plain text only. No untrusted terminal escape, C1, bidi or overwrite controls.
pub fn safe_text(input: &str, max_chars: usize) -> String {
    input
        .chars()
        .filter(|c| {
            !c.is_control()
                && !matches!(*c,
        '\u{061c}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' |
        '\u{2060}'..='\u{206f}' | '\u{feff}')
        })
        .take(max_chars)
        .collect()
}

pub fn safe_lines(input: &str, max_chars: usize) -> String {
    let limited: String = input.chars().take(max_chars).collect();
    limited
        .lines()
        .map(|line| safe_text(line, 4096))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Approval text must be complete. Reject instead of silently sanitizing or
/// clipping target/effect information before a human confirmation.
pub fn review_text(input: &str, max_chars: usize) -> Result<String> {
    let shown = safe_lines(input, max_chars);
    if shown != input {
        return Err(
            "operation review contains controls or exceeds its display budget; approval blocked"
                .into(),
        );
    }
    Ok(shown)
}

/// Reversible ASCII presentation for approval screens, whose data cells are
/// deliberately ASCII-only. Escape before wrapping/counting painted lines, never
/// after: otherwise different Unicode inputs would both be displayed as `?`.
/// This affects presentation only; the sealed operation and confirmation stay exact.
pub fn review_display(input: &str) -> Result<String> {
    const LIMIT: usize = 1024 * 1024;
    const NOTICE: &str =
        "Escaped review: \\\\ is a literal backslash; \\u{...} is a Unicode code point.\n\n";
    let failure = "Review display exceeds 1 MiB; approval blocked. Reduce the operation scope.";
    if input.len() > LIMIT {
        return Err(failure.into());
    }
    let needs_escape = |ch: char| ch == '\\' || !ch.is_ascii() || (ch.is_control() && ch != '\n');
    if !input.chars().any(needs_escape) {
        return Ok(input.into());
    }
    let mut output = String::from(NOTICE);
    for ch in input.chars() {
        let escaped = if ch == '\\' {
            "\\\\".into()
        } else if needs_escape(ch) {
            ch.escape_unicode().to_string()
        } else {
            ch.to_string()
        };
        if output.len().saturating_add(escaped.len()) > LIMIT {
            return Err(failure.into());
        }
        output.push_str(&escaped);
    }
    Ok(output)
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 63
        && id.as_bytes()[0].is_ascii_alphanumeric()
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        && !id.ends_with('-')
}

pub fn checked_path(path: &Path) -> Result<()> {
    for parent in path.ancestors() {
        match fs::symlink_metadata(parent) {
            Ok(meta) => {
                if meta.file_type().is_symlink() {
                    return Err("symlink paths are not permitted".into());
                }
                #[cfg(windows)]
                {
                    use std::os::windows::fs::MetadataExt;
                    if meta.file_attributes() & 0x400 != 0 {
                        return Err("reparse-point paths are not permitted".into());
                    }
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err("cannot inspect path safely".into()),
        }
    }
    Ok(())
}

pub fn read_small(path: &Path, limit: u64) -> Result<Vec<u8>> {
    checked_path(path)?;
    if !fs::symlink_metadata(path)
        .map_err(|_| "cannot inspect file")?
        .is_file()
    {
        return Err("not a regular file".into());
    }
    let file = fs::File::open(path).map_err(|_| "cannot open file".to_string())?;
    if !file
        .metadata()
        .map_err(|_| "cannot inspect file")?
        .is_file()
    {
        return Err("not a regular file".into());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read file")?;
    if bytes.len() as u64 > limit {
        return Err(format!("file exceeds {limit} byte limit"));
    }
    Ok(bytes)
}

pub fn config_dir() -> Result<PathBuf> {
    if let Some(path) =
        std::env::var_os("KBORG_CONFIG_DIR").or_else(|| std::env::var_os("K8BORG_CONFIG_DIR"))
    {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("KBORG_CONFIG_DIR must be absolute".into());
        }
        checked_path(&path)?;
        return Ok(path);
    }
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("KBorg"));
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|p| p.join("Library/Application Support/KBorg"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .map(|p| p.join("kborg"))
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|p| p.join(".config/kborg"))
        });
    let path = base.ok_or("user config directory unavailable; set KBORG_CONFIG_DIR")?;
    if !path.is_absolute() {
        return Err("user config directory must be absolute".into());
    }
    checked_path(&path)?;
    // Reuse existing pre-rename settings without moving sensitive files.
    let legacy = path.with_file_name(if cfg!(windows) || cfg!(target_os = "macos") {
        "K8Borg"
    } else {
        "k8borg"
    });
    if !path.exists() {
        checked_path(&legacy)?;
        if legacy.exists() {
            return Ok(legacy);
        }
    }
    Ok(path)
}

/// Create without overwriting. Settings use a replaceable non-sensitive file separately.
pub fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    checked_path(path)?;
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        ensure_private_dir(parent)?;
    }
    checked_path(path)?;
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|_| "cannot create file (it may already exist)")?;
    file.write_all(bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| "cannot write file".into())
}

/// Conservative display/export projection; Secrets are rejected by the gateway itself.
/// This is not a universal sensitive-business-data detector.
pub fn redact(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, v) in map {
                let lower = key.to_ascii_lowercase();
                if [
                    "data",
                    "stringdata",
                    "binarydata",
                    "annotations",
                    "managedfields",
                    "value",
                    "token",
                    "password",
                    "client-key-data",
                ]
                .contains(&lower.as_str())
                    || lower.contains("secret")
                    || lower.contains("credential")
                {
                    *v = Value::String("[REDACTED]".into());
                } else {
                    redact(v);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(redact),
        Value::String(s) => *s = safe_lines(s, 16384),
        _ => {}
    }
}

/// Create new directory components with restrictive Unix modes. Existing parent
/// directories are not chmod'ed: they may be shared user configuration roots.
pub fn ensure_private_dir(path: &Path) -> Result<()> {
    checked_path(path)?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|_| "cannot create directory")?;
    checked_path(path)?;
    if !fs::metadata(path)
        .map_err(|_| "cannot inspect directory")?
        .is_dir()
    {
        return Err("not a directory".into());
    }
    Ok(())
}

/// Sync a new sibling and replace the destination in one rename operation.
/// No unlink-before-rename gap; does not claim crash durability on every filesystem.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQUENCE: AtomicU64 = AtomicU64::new(0);
    checked_path(path)?;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    ensure_private_dir(parent)?;
    let filename = path.file_name().ok_or("destination filename required")?;
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.is_file() => {
            return Err("replacement target must be a regular file".into());
        }
        Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
            return Err("cannot inspect replacement target".into());
        }
        _ => {}
    }
    let mut stage_name = std::ffi::OsString::from(".");
    stage_name.push(filename);
    stage_name.push(format!(
        ".{}-{}.tmp",
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    ));
    let stage = parent.join(stage_name);
    // create_new prevents overwriting a leftover or attacker-created staging file.
    write_new(&stage, bytes)?;
    let commit = checked_path(path)
        .and_then(|()| checked_path(&stage))
        .and_then(|()| {
            fs::rename(&stage, path)
                .map_err(|_| "cannot commit replacement; previous file retained".into())
        });
    if let Err(error) = commit {
        let _ = fs::remove_file(&stage);
        return Err(error);
    }
    #[cfg(unix)]
    {
        fs::File::open(parent)
            .and_then(|dir| dir.sync_all())
            .map_err(
                |_| "replacement committed, but directory durability could not be confirmed",
            )?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn approval_display_is_unambiguous_ascii_and_bounds_expansion() -> Result<()> {
        let input = "context: pröd\narg: 東京\\u{e9}\u{1b}]52;c;canary\u{7}\u{202e}";
        let shown = review_display(input)?;
        assert!(shown.is_ascii());
        assert!(shown.contains("pr\\u{f6}d"));
        assert!(shown.contains("\\u{6771}\\u{4eac}\\\\u{e9}"));
        assert!(shown.contains("\\u{1b}]52;c;canary\\u{7}\\u{202e}"));
        assert!(!shown.contains('\u{1b}'));
        assert_ne!(review_display("é")?, review_display("\\u{e9}")?);
        assert_ne!(review_display("é")?, review_display("ö")?);
        assert_eq!(
            review_display("ordinary\nASCII target")?,
            "ordinary\nASCII target"
        );
        // Input fits its byte limit, but escaped output does not. Never truncate.
        assert!(review_display(&"é".repeat(200_000)).is_err());
        assert!(review_display(&"a".repeat(1024 * 1024 + 1)).is_err());
        Ok(())
    }

    #[test]
    fn approval_preview_rejects_long_aliases_and_never_omits_effects() -> Result<()> {
        let summary = format!(
            "context: {}\nUID:123\nresourceVersion:7\n- limits.cpu: 100m\n+ limits.cpu: 200m\nType test/app to execute",
            "a".repeat(5000)
        );
        assert!(review_text(&summary, 16_384).is_err());
        let many_lines = "short line\n".repeat(3000);
        assert!(review_text(many_lines.trim_end(), 16_384).is_err());
        let ordinary = "context: local\nUID:123\n- limits.cpu:100m\n+ limits.cpu:200m\nType test/app to execute";
        assert_eq!(review_text(ordinary, 16_384)?, ordinary);
        assert!(review_text("target: test/app\u{001b}[2J\neffect:delete", 16_384).is_err());
        Ok(())
    }
    fn storage_test_dir(label: &str) -> PathBuf {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_else(|e| panic!("{e}"))
            .as_nanos();
        std::env::temp_dir()
            .canonicalize()
            .unwrap_or_else(|e| panic!("test temporary root: {e}"))
            .join(format!("kborg-{label}-{}-{stamp}", std::process::id()))
    }
    #[test]
    fn atomic_repeated_replacement_and_leftover_stage_preserve_settings() {
        let dir = storage_test_dir("atomic");
        let target = dir.join("appearance.json");
        atomic_write(&target, b"original").unwrap_or_else(|e| panic!("{e}"));
        write_new(&dir.join("appearance-interrupted.tmp"), b"partial")
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            read_small(&target, 100).unwrap_or_else(|e| panic!("{e}")),
            b"original"
        );
        for value in [b"second".as_slice(), b"third".as_slice()] {
            atomic_write(&target, value).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                read_small(&target, 100).unwrap_or_else(|e| panic!("{e}")),
                value
            );
        }
        fs::remove_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    }
    #[test]
    fn failed_stage_preserves_existing_destination() {
        let dir = storage_test_dir("failed");
        let target = dir.join("appearance.json");
        atomic_write(&target, b"original").unwrap_or_else(|e| panic!("{e}"));
        // A staging path too long for the filesystem fails before any destination mutation.
        let invalid = dir.join("x".repeat(300));
        assert!(atomic_write(&invalid, b"replacement").is_err());
        assert_eq!(
            read_small(&target, 100).unwrap_or_else(|e| panic!("{e}")),
            b"original"
        );
        assert!(atomic_write(&dir, b"replacement").is_err());
        fs::remove_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    }
    #[cfg(unix)]
    #[test]
    fn new_storage_has_private_unix_modes() {
        use std::os::unix::fs::PermissionsExt;
        let dir = storage_test_dir("modes");
        let target = dir.join("nested/appearance.json");
        atomic_write(&target, b"{}").unwrap_or_else(|e| panic!("{e}"));
        for directory in [&dir, &dir.join("nested")] {
            assert_eq!(
                fs::metadata(directory)
                    .unwrap_or_else(|e| panic!("{e}"))
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
        assert_eq!(
            fs::metadata(&target)
                .unwrap_or_else(|e| panic!("{e}"))
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        fs::remove_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    }

    use super::*;
    #[test]
    fn sanitizes_terminal_and_bidi_controls() {
        let output = safe_text("ok\x1b]52;c;payload\x07\r\u{009b}\u{202e}x", 100);
        assert!(!output.chars().any(char::is_control));
        assert!(!output.contains('\u{202e}'));
    }
    #[test]
    fn blocks_path_ids() {
        for id in ["../x", "-x", "x/y", "", "X", "x-"] {
            assert!(!valid_id(id));
        }
    }
    #[test]
    fn removes_nested_literals() {
        let mut value = serde_json::json!({"spec":{"env":[{"name":"PASSWORD","value":"canary"}]},"metadata":{"annotations":{"x":"canary"}}});
        redact(&mut value);
        assert!(!value.to_string().contains("canary"));
    }
}
