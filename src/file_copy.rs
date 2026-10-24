//! Reviewed single-file copy between the local machine and one Pod container.
//!
//! Each copy moves one regular file of at most [`MAX_BYTES`], in memory, so the
//! reviewed bytes are exactly the bytes sent and no partial file is ever left.
//! The container runs a fixed argv: `cat` to read, and a no-clobber `sh`
//! redirect to write; paths are passed as arguments, never parsed by a shell.
//! Local files are created new with private permissions and never overwritten.
use crate::{Result, safety};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

pub const MAX_BYTES: usize = 64 * 1024 * 1024;
pub const DEADLINE: Duration = Duration::from_secs(120);
const MAX_STDERR: usize = 4096;
/// Upload write size and kube stdin pipe capacity.
pub const STDIN_CHUNK: usize = 32 * 1024;
const MAX_REMOTE_PATH: usize = 4096;

/// An absolute container path without control characters, `.`/`..`
/// segments or a trailing slash.
pub fn validate_remote(path: &str) -> Result<()> {
    if !path.starts_with('/')
        || path.len() < 2
        || path.len() > MAX_REMOTE_PATH
        || path.ends_with('/')
        || safety::safe_text(path, MAX_REMOTE_PATH) != path
        || path
            .split('/')
            .skip(1)
            .any(|segment| segment.is_empty() || segment == "." || segment == "..")
    {
        return Err(
            "container path must be an absolute file path without ., .. or empty segments".into(),
        );
    }
    Ok(())
}

fn file_name(remote: &str) -> &str {
    remote.rsplit('/').next().unwrap_or(remote)
}

pub fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Resolve the directories the user named (macOS temp and some home
/// directories sit behind symlinks), keeping the final component as given so
/// a symlinked file itself is still refused.
fn resolve_parent(local: &Path) -> Result<PathBuf> {
    let name = local
        .file_name()
        .ok_or("local path needs a file or directory name")?;
    let parent = local
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let parent = fs::canonicalize(parent).map_err(|_| "local directory does not exist")?;
    if !parent.is_dir() {
        return Err("local directory does not exist".into());
    }
    Ok(parent.join(name))
}

/// Where a download lands: inside `local` when it is an existing directory,
/// otherwise at `local` itself. The file must not exist yet.
pub fn download_target(remote: &str, local: &Path) -> Result<PathBuf> {
    validate_remote(remote)?;
    let local = resolve_parent(local)?;
    let is_dir = fs::symlink_metadata(&local).is_ok_and(|meta| meta.is_dir());
    let target = if is_dir {
        local.join(file_name(remote))
    } else {
        local
    };
    if fs::symlink_metadata(&target).is_ok() {
        return Err(format!(
            "{} already exists; KBorg never overwrites local files",
            safety::safe_text(&target.to_string_lossy(), 512)
        ));
    }
    Ok(target)
}

/// A local file read once at review time; the same bytes are uploaded.
pub struct Upload {
    pub local: PathBuf,
    pub bytes: Vec<u8>,
    pub sha256: String,
}

pub fn read_upload(local: &Path) -> Result<Upload> {
    let fail = |error: String| format!("cannot upload {}: {error}", local.display());
    let resolved = resolve_parent(local).map_err(fail)?;
    let bytes = safety::read_small(&resolved, MAX_BYTES as u64).map_err(fail)?;
    Ok(Upload {
        local: resolved,
        sha256: sha256(&bytes),
        bytes,
    })
}

pub fn read_argv(remote: &str) -> Vec<String> {
    vec!["cat".into(), "--".into(), remote.into()]
}

/// `set -C` makes `>` refuse to replace an existing file. The extra `cat`
/// gives the writer an ordinary pipe: BusyBox `cat > file` reading the
/// container runtime's stdin directly can stop early. `wc -c` reports the
/// written size so a short write is never reported as success.
pub fn write_argv(remote: &str) -> Vec<String> {
    vec![
        "sh".into(),
        "-c".into(),
        "set -C && cat | cat > \"$1\" && wc -c < \"$1\"".into(),
        "kborg-cp".into(),
        remote.into(),
    ]
}

/// Create `target` new (0600 on Unix) with `bytes`; on any failure the file is removed.
pub fn write_download(target: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(target)
        .map_err(|_| "cannot create the local file (it may already exist)")?;
    if file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .is_err()
    {
        drop(file);
        let _ = fs::remove_file(target);
        return Err("cannot write the local file; partial file removed".into());
    }
    Ok(())
}

async fn read_limited(reader: impl AsyncRead + Unpin, limit: usize) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    reader
        .take(limit as u64 + 1)
        .read_to_end(&mut data)
        .await
        .map_err(|_| "remote stream read failed")?;
    Ok(data)
}

fn remote_failure(stderr: &[u8]) -> String {
    let detail = safety::safe_text(String::from_utf8_lossy(stderr).trim(), 300);
    if detail.is_empty() {
        "container command failed; the container may lack cat/sh or the path may be unreadable"
            .into()
    } else {
        format!("container command failed: {detail}")
    }
}

/// Read the remote file from a `read_argv` process.
pub async fn receive(mut process: kube::api::AttachedProcess) -> Result<Vec<u8>> {
    let stdout = process.stdout().ok_or("stdout stream unavailable")?;
    let stderr = process.stderr().ok_or("stderr stream unavailable")?;
    let status = process.take_status().ok_or("process status unavailable")?;
    let result = tokio::time::timeout(DEADLINE, async {
        let (out, err) = futures_util::future::try_join(
            read_limited(stdout, MAX_BYTES),
            read_limited(stderr, MAX_STDERR),
        )
        .await?;
        if out.len() > MAX_BYTES {
            return Err(format!(
                "remote file exceeds {} MiB; nothing was saved",
                MAX_BYTES >> 20
            ));
        }
        let status = status.await.ok_or("remote status unknown")?;
        if status.status.as_deref() != Some("Success") {
            return Err(remote_failure(&err));
        }
        Ok(out)
    })
    .await
    .map_err(|_| "copy timed out; nothing was saved".to_owned());
    process.abort();
    result?
}

/// Stream `bytes` to a `write_argv` process and wait for its status. A
/// container-side failure (for example an existing file) is reported from
/// its own status and stderr rather than as a broken stream.
pub async fn send(mut process: kube::api::AttachedProcess, bytes: &[u8]) -> Result<()> {
    let mut stdin = process.stdin().ok_or("stdin stream unavailable")?;
    let stdout = process.stdout().ok_or("stdout stream unavailable")?;
    let stderr = process.stderr().ok_or("stderr stream unavailable")?;
    let status = process.take_status().ok_or("process status unavailable")?;
    let result = tokio::time::timeout(DEADLINE, async {
        let write = async {
            for chunk in bytes.chunks(STDIN_CHUNK) {
                stdin.write_all(chunk).await?;
            }
            stdin.shutdown().await
        };
        let (written, out, err) = tokio::join!(
            write,
            read_limited(stdout, MAX_STDERR),
            read_limited(stderr, MAX_STDERR)
        );
        let err = err.unwrap_or_default();
        match (status.await, written) {
            (Some(status), Ok(())) if status.status.as_deref() == Some("Success") => {
                let size = String::from_utf8_lossy(&out.unwrap_or_default())
                    .trim()
                    .parse::<usize>();
                if size == Ok(bytes.len()) {
                    Ok(())
                } else {
                    Err(
                        "the container file size does not match the reviewed bytes; it is partial"
                            .into(),
                    )
                }
            }
            (Some(_), _) => Err(remote_failure(&err)),
            (None, Err(_)) => {
                Err("upload stream failed; the container file may be partial".to_owned())
            }
            (None, Ok(())) => {
                Err("remote status unknown; the container file may be partial".to_owned())
            }
        }
    })
    .await
    .map_err(|_| "copy timed out; the container file may be partial".to_owned());
    process.abort();
    result?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn remote_paths_are_absolute_plain_and_normalized() {
        for good in ["/etc/hosts", "/var/log/app.log", "/a b/c"] {
            assert!(validate_remote(good).is_ok(), "{good}");
        }
        for bad in [
            "etc/hosts",
            "/",
            "/tmp/",
            "/tmp//x",
            "/tmp/../etc/passwd",
            "/tmp/./x",
            "/tmp/\u{1b}[2J",
            "/tmp/a\nb",
        ] {
            assert!(validate_remote(bad).is_err(), "{bad:?}");
        }
        assert_eq!(
            write_argv("/tmp/it's \"x\"; rm -rf /"),
            vec![
                "sh",
                "-c",
                "set -C && cat | cat > \"$1\" && wc -c < \"$1\"",
                "kborg-cp",
                "/tmp/it's \"x\"; rm -rf /"
            ],
            "the path is an argument, never shell text"
        );
    }
    #[test]
    fn downloads_never_overwrite_and_land_in_directories() -> Result<()> {
        let dir = std::env::temp_dir().join(format!("kborg-cp-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).map_err(|_| "cannot create test dir")?;
        let dir = fs::canonicalize(&dir).map_err(|_| "canonicalize failed")?;
        let target = download_target("/var/log/app.log", &dir)?;
        assert_eq!(target, dir.join("app.log"));
        write_download(&target, b"hello")?;
        assert_eq!(fs::read(&target).map_err(|_| "read failed")?, b"hello");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&target)
                .map_err(|_| "stat failed")?
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        assert!(download_target("/var/log/app.log", &dir).is_err());
        assert!(write_download(&target, b"again").is_err());
        assert_eq!(fs::read(&target).map_err(|_| "read failed")?, b"hello");
        assert!(download_target("/x", &dir.join("missing/child")).is_err());
        let upload = read_upload(&target)?;
        assert_eq!(
            upload.sha256,
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert!(read_upload(&dir).is_err(), "directories are not uploaded");
        #[cfg(unix)]
        {
            let link = dir.join("link");
            std::os::unix::fs::symlink(&target, &link).map_err(|_| "symlink failed")?;
            assert!(read_upload(&link).is_err(), "symlinks are not followed");
            assert!(download_target("/x", &link).is_err());
        }
        let _ = fs::remove_dir_all(&dir);
        Ok(())
    }
}
