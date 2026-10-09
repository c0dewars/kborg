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
    if let Some(path) = std::env::var_os("K8BORG_CONFIG_DIR") {
        let path = PathBuf::from(path);
        if !path.is_absolute() {
            return Err("K8BORG_CONFIG_DIR must be absolute".into());
        }
        checked_path(&path)?;
        return Ok(path);
    }
    #[cfg(windows)]
    let base = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .map(|p| p.join("K8Borg"));
    #[cfg(target_os = "macos")]
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|p| p.join("Library/Application Support/K8Borg"));
    #[cfg(all(unix, not(target_os = "macos")))]
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .map(|p| p.join("k8borg"))
        .or_else(|| {
            std::env::var_os("HOME")
                .map(PathBuf::from)
                .map(|p| p.join(".config/k8borg"))
        });
    let path = base.ok_or("user config directory unavailable; set K8BORG_CONFIG_DIR")?;
    if !path.is_absolute() {
        return Err("user config directory must be absolute".into());
    }
    checked_path(&path)?;
    Ok(path)
}

/// Create without overwriting. Settings use a replaceable non-sensitive file separately.
pub fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    checked_path(path)?;
    if let Some(parent) = path.parent() {
        let mut builder = fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(parent)
            .map_err(|_| "cannot create directory")?;
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

#[cfg(test)]
mod tests {
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
