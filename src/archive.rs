//! Minimal, strict tar for directory copies to and from Pods.
//!
//! Only regular files and directories are written or extracted. Absolute
//! paths, `..`, links, devices and anything outside the single top-level
//! directory are refused, so a hostile container cannot write outside the
//! new local directory. Extraction always targets a directory KBorg has just
//! created, and a failure removes it.
use crate::{Result, safety};
use std::{
    fs,
    io::Write,
    path::{Component, Path, PathBuf},
};

pub const MAX_ENTRIES: usize = 10_000;
const BLOCK: usize = 512;

fn octal(field: &[u8]) -> Result<u64> {
    let text = std::str::from_utf8(field).map_err(|_| "malformed tar header")?;
    let text = text.trim_matches(|c: char| c == '\0' || c == ' ');
    if text.is_empty() {
        return Ok(0);
    }
    u64::from_str_radix(text, 8).map_err(|_| "malformed tar number".into())
}

fn text(field: &[u8]) -> String {
    let end = field.iter().position(|b| *b == 0).unwrap_or(field.len());
    String::from_utf8_lossy(&field[..end]).into_owned()
}

/// A relative path inside the archive's top directory, minus that directory.
fn inner_path(name: &str, top: &str) -> Result<Option<PathBuf>> {
    if name.contains('\0') || safety::safe_text(name, 4096) != name {
        return Err("archive path contains control characters".into());
    }
    let path = Path::new(name.trim_start_matches("./"));
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_owned()),
            Component::CurDir => {}
            _ => {
                return Err(format!(
                    "archive path {name:?} is absolute or leaves its directory"
                ));
            }
        }
    }
    let mut parts = parts.into_iter();
    if parts.next().is_none_or(|first| first != top) {
        return Err(format!("archive entry {name:?} is outside {top}"));
    }
    let rest: PathBuf = parts.collect();
    Ok((!rest.as_os_str().is_empty()).then_some(rest))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Extracted {
    pub files: usize,
    pub directories: usize,
    pub bytes: u64,
    /// Links, devices and other entries that were not extracted.
    pub skipped: usize,
}

/// Extract `archive` (entries under `top/`) into the new directory `target`.
pub fn extract(archive: &[u8], top: &str, target: &Path) -> Result<Extracted> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(target)
        .map_err(|_| "cannot create the local directory (it may already exist)")?;
    let result = extract_into(archive, top, target);
    if result.is_err() {
        let _ = fs::remove_dir_all(target);
    }
    result
}

fn private_dirs(path: &Path) -> Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .map_err(|_| "cannot create an extracted directory".into())
}

fn extract_into(archive: &[u8], top: &str, target: &Path) -> Result<Extracted> {
    let mut done = Extracted::default();
    let mut at = 0;
    let mut long_name: Option<String> = None;
    let mut entries = 0;
    while at + BLOCK <= archive.len() {
        let header = &archive[at..at + BLOCK];
        if header.iter().all(|b| *b == 0) {
            break;
        }
        entries += 1;
        if entries > MAX_ENTRIES {
            return Err(format!("directory has more than {MAX_ENTRIES} entries"));
        }
        let size = usize::try_from(octal(&header[124..136])?).map_err(|_| "tar entry too large")?;
        let body_start = at + BLOCK;
        let body_end = body_start
            .checked_add(size)
            .filter(|end| *end <= archive.len())
            .ok_or("truncated tar archive")?;
        let body = &archive[body_start..body_end];
        at = body_start + size.div_ceil(BLOCK) * BLOCK;
        let kind = header[156];
        let name = match long_name.take() {
            Some(name) => name,
            None => {
                let name = text(&header[0..100]);
                let prefix = text(&header[345..500]);
                if &header[257..262] == b"ustar" && !prefix.is_empty() {
                    format!("{prefix}/{name}")
                } else {
                    name
                }
            }
        };
        match kind {
            b'L' => {
                long_name = Some(text(body));
                continue;
            }
            b'x' => {
                // PAX: honour only the path record.
                let records = String::from_utf8_lossy(body);
                long_name = records
                    .lines()
                    .find_map(|line| line.split_once(" path=").map(|(_, path)| path.to_owned()));
                continue;
            }
            b'g' => continue,
            b'0' | 0 | b'5' => {}
            _ => {
                done.skipped += 1;
                continue;
            }
        }
        let Some(relative) = inner_path(&name, top)? else {
            continue;
        };
        let path = target.join(&relative);
        if kind == b'5' {
            private_dirs(&path)?;
            done.directories += 1;
            continue;
        }
        if let Some(parent) = path.parent() {
            private_dirs(parent)?;
        }
        let mut options = fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            let executable = octal(&header[100..108]).unwrap_or(0) & 0o100 != 0;
            options.mode(if executable { 0o700 } else { 0o600 });
        }
        let mut file = options
            .open(&path)
            .map_err(|_| format!("cannot create {} (duplicate entry?)", relative.display()))?;
        file.write_all(body)
            .map_err(|_| "cannot write an extracted file")?;
        done.files += 1;
        done.bytes += body.len() as u64;
    }
    Ok(done)
}

fn header(name: &str, size: u64, mode: u32, kind: u8, mtime: u64) -> Result<[u8; BLOCK]> {
    let mut block = [0u8; BLOCK];
    let (prefix, name) = if name.len() <= 100 {
        ("", name)
    } else {
        let split = name[..name.len().min(156)]
            .rfind('/')
            .filter(|split| name.len() - split - 1 <= 100 && *split <= 155)
            .ok_or_else(|| format!("path too long for the archive: {name}"))?;
        (&name[..split], &name[split + 1..])
    };
    block[..name.len()].copy_from_slice(name.as_bytes());
    let mut field = |range: std::ops::Range<usize>, value: String| {
        let bytes = value.as_bytes();
        block[range.start..range.start + bytes.len()].copy_from_slice(bytes);
    };
    field(100..108, format!("{mode:07o}"));
    field(108..116, "0000000".into());
    field(116..124, "0000000".into());
    field(124..136, format!("{size:011o}"));
    field(136..148, format!("{mtime:011o}"));
    block[156] = kind;
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    block[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
    block[148..156].copy_from_slice(b"        ");
    let sum: u32 = block.iter().map(|b| u32::from(*b)).sum();
    block[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    Ok(block)
}

/// Archive local directory `root` as `top/…`: regular files and directories
/// only, symlinks refused. Returns the archive and its regular-file count.
pub fn create(root: &Path, top: &str, max_bytes: usize) -> Result<(Vec<u8>, usize)> {
    let mut out = Vec::new();
    let mut files = 0;
    let mut stack = vec![(root.to_path_buf(), top.to_owned())];
    let mut entries = 0;
    while let Some((path, name)) = stack.pop() {
        entries += 1;
        if entries > MAX_ENTRIES {
            return Err(format!("directory has more than {MAX_ENTRIES} entries"));
        }
        let meta = fs::symlink_metadata(&path).map_err(|_| "cannot inspect a local file")?;
        let mtime = meta
            .modified()
            .ok()
            .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
            .map_or(0, |age| age.as_secs());
        #[cfg(unix)]
        let executable = {
            use std::os::unix::fs::PermissionsExt;
            meta.permissions().mode() & 0o100 != 0
        };
        #[cfg(not(unix))]
        let executable = false;
        if meta.file_type().is_symlink() {
            return Err(format!(
                "{} is a symlink; KBorg copies regular files and directories only",
                path.display()
            ));
        }
        if meta.is_dir() {
            out.extend_from_slice(&header(&format!("{name}/"), 0, 0o755, b'5', mtime)?);
            let mut children = fs::read_dir(&path)
                .map_err(|_| "cannot read a local directory")?
                .filter_map(|entry| entry.ok())
                .collect::<Vec<_>>();
            children.sort_by_key(|entry| std::cmp::Reverse(entry.file_name()));
            for child in children {
                let child_name = child.file_name().to_string_lossy().into_owned();
                if safety::safe_text(&child_name, 255) != child_name {
                    return Err("a local file name contains control characters".into());
                }
                stack.push((child.path(), format!("{name}/{child_name}")));
            }
        } else if meta.is_file() {
            let bytes = fs::read(&path).map_err(|_| "cannot read a local file")?;
            out.extend_from_slice(&header(
                &name,
                bytes.len() as u64,
                if executable { 0o755 } else { 0o644 },
                b'0',
                mtime,
            )?);
            out.extend_from_slice(&bytes);
            out.resize(out.len().div_ceil(BLOCK) * BLOCK, 0);
            files += 1;
        } else {
            return Err(format!(
                "{} is not a regular file or directory",
                path.display()
            ));
        }
        if out.len() > max_bytes {
            return Err(format!("directory exceeds {} MiB", max_bytes >> 20));
        }
    }
    out.extend_from_slice(&[0u8; BLOCK * 2]);
    Ok((out, files))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("kborg-archive-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir
    }
    #[test]
    fn round_trip_keeps_files_and_refuses_existing_targets() -> Result<()> {
        let source = scratch("src");
        fs::create_dir_all(source.join("conf/nested")).map_err(|_| "mkdir")?;
        fs::write(source.join("conf/app.yaml"), b"a: 1").map_err(|_| "write")?;
        fs::write(source.join("conf/nested/big.bin"), vec![7u8; 5000]).map_err(|_| "write")?;
        let (archive, files) = create(&source.join("conf"), "conf", 1 << 20)?;
        assert_eq!(files, 2);
        let target = scratch("dst");
        let done = extract(&archive, "conf", &target)?;
        assert_eq!((done.files, done.bytes), (2, 5004));
        assert_eq!(
            fs::read(target.join("nested/big.bin")).map_err(|_| "read")?,
            vec![7u8; 5000]
        );
        assert!(
            extract(&archive, "conf", &target).is_err(),
            "never extracts into an existing directory"
        );
        let _ = fs::remove_dir_all(&source);
        let _ = fs::remove_dir_all(&target);
        Ok(())
    }
    #[test]
    fn hostile_entries_are_refused_and_links_skipped() -> Result<()> {
        let mut archive = Vec::new();
        archive.extend_from_slice(&header("conf/../../etc/x", 1, 0o644, b'0', 0)?);
        archive.extend_from_slice(&[b'x'; BLOCK]);
        archive.extend_from_slice(&[0u8; BLOCK * 2]);
        let target = scratch("evil");
        assert!(extract(&archive, "conf", &target).is_err());
        assert!(
            !target.exists(),
            "failed extraction removes the new directory"
        );
        let mut archive = Vec::new();
        archive.extend_from_slice(&header("/etc/passwd", 0, 0o644, b'0', 0)?);
        archive.extend_from_slice(&[0u8; BLOCK * 2]);
        assert!(extract(&archive, "conf", &target).is_err());
        let mut archive = Vec::new();
        archive.extend_from_slice(&header("conf/link", 0, 0o777, b'2', 0)?);
        archive.extend_from_slice(&[0u8; BLOCK * 2]);
        let done = extract(&archive, "conf", &target)?;
        assert_eq!(done.skipped, 1);
        assert!(!target.join("link").exists());
        let _ = fs::remove_dir_all(&target);
        Ok(())
    }
}
