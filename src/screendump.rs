//! Explicit Ctrl-S snapshots, stored privately and read back through redaction.
use crate::{Result, safety};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_BYTES: u64 = 256 * 1024;
const MAX_FILES: usize = 50;
const LOCK: &str = ".snapshot-lock";
static SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Clone, Debug)]
pub struct Record {
    pub id: String,
    pub title: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Snapshot {
    schema_version: u32,
    context: String,
    namespace: String,
    resource: String,
    items: Vec<Value>,
}
fn valid_id(id: &str) -> bool {
    id.len() <= 100
        && id.starts_with("snapshot-")
        && id.ends_with(".json")
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || b.is_ascii_lowercase() || b"-.".contains(&b))
}
fn title(id: &str, snapshot: &Snapshot) -> String {
    safety::safe_text(
        &format!(
            "{} | {} / {} / {} | {} objects",
            id.trim_end_matches(".json"),
            snapshot.context,
            snapshot.namespace,
            snapshot.resource,
            snapshot.items.len()
        ),
        1024,
    )
}
fn decode(bytes: &[u8]) -> Result<Snapshot> {
    let snapshot: Snapshot = serde_json::from_slice(bytes).map_err(|_| "invalid saved snapshot")?;
    if snapshot.schema_version != 1
        || snapshot.items.len() > 1000
        || [&snapshot.context, &snapshot.namespace, &snapshot.resource]
            .iter()
            .any(|s| s.len() > 253 || safety::safe_text(s, 253) != **s)
    {
        return Err("invalid saved snapshot metadata".into());
    }
    Ok(snapshot)
}
fn list_at(directory: &Path) -> Result<Vec<Record>> {
    safety::checked_path(directory)?;
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for entry in std::fs::read_dir(directory)
        .map_err(|_| "cannot open screendump directory")?
        .take(MAX_FILES + 2)
    {
        let path = entry.map_err(|_| "cannot read screendump entry")?.path();
        if path.file_name().and_then(|s| s.to_str()) == Some(LOCK) {
            check_lock(&path)?;
            continue;
        }
        if records.len() == MAX_FILES {
            return Err("screendump limit reached; archive files before saving more".into());
        }
        let id = path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or("invalid screendump filename")?;
        if !valid_id(id) {
            return Err("unexpected screendump entry; inspect configuration directory".into());
        }
        let snapshot = decode(&safety::read_small(&path, MAX_BYTES)?)?;
        records.push(Record {
            id: id.into(),
            title: title(id, &snapshot),
        });
    }
    records.sort_by(|a, b| b.id.cmp(&a.id));
    Ok(records)
}
pub fn list() -> Result<Vec<Record>> {
    list_at(&safety::config_dir()?.join("screendumps"))
}

fn check_lock(path: &Path) -> Result<()> {
    safety::checked_path(path)?;
    let meta = std::fs::symlink_metadata(path).map_err(|_| "cannot inspect snapshot lock")?;
    if !meta.is_file() || meta.len() != 0 {
        return Err("invalid snapshot lock".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err("snapshot lock must be private".into());
        }
    }
    Ok(())
}
fn save_lock(directory: &Path) -> Result<std::fs::File> {
    safety::ensure_private_dir(directory)?;
    let path = directory.join(LOCK);
    safety::checked_path(&path)?;
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .map_err(|_| "cannot open snapshot lock")?;
    check_lock(&path)?;
    // A lock can outlive its holder briefly: a process spawned elsewhere in
    // this program shares the descriptor until its exec. Retry for up to
    // half a second before reporting a concurrent save.
    for attempt in 0..20 {
        if file.try_lock().is_ok() {
            return Ok(file);
        }
        if attempt < 19 {
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
    }
    Err("another snapshot is being saved; retry shortly".into())
}
fn save_at(
    directory: &Path,
    context: &str,
    namespace: &str,
    resource: &str,
    items: &[Value],
) -> Result<Record> {
    if items.len() > 1000 {
        return Err("screendump exceeds 1000 objects; narrow the view".into());
    }
    let _lock = save_lock(directory)?;
    if list_at(directory)?.len() >= MAX_FILES {
        return Err("50 screendumps saved; archive files before saving more".into());
    }
    let mut snapshot = Snapshot {
        schema_version: 1,
        context: safety::safe_text(context, 253),
        namespace: safety::safe_text(namespace, 253),
        resource: safety::safe_text(resource, 253),
        items: items.to_vec(),
    };
    snapshot.items.iter_mut().for_each(safety::redact);
    let bytes = serde_json::to_vec(&snapshot).map_err(|_| "cannot encode screendump")?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err("screendump exceeds 256 KiB; narrow the view".into());
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock before epoch")?
        .as_nanos();
    let id = format!(
        "snapshot-{now}-{}-{}.json",
        std::process::id(),
        SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    );
    safety::write_new(&directory.join(&id), &bytes)?;
    Ok(Record {
        title: title(&id, &snapshot),
        id,
    })
}
pub fn save(context: &str, namespace: &str, resource: &str, items: &[Value]) -> Result<Record> {
    save_at(
        &safety::config_dir()?.join("screendumps"),
        context,
        namespace,
        resource,
        items,
    )
}
fn read_at(directory: &Path, id: &str) -> Result<String> {
    if !valid_id(id) {
        return Err("invalid screendump id".into());
    }
    let snapshot = decode(&safety::read_small(&directory.join(id), MAX_BYTES)?)?;
    let heading = title(id, &snapshot);
    Ok(format!(
        "SAVED SNAPSHOT | {heading}\nHistorical redacted resource data; this is not live state.\n\n{}",
        crate::inspect::yaml(&serde_json::json!({"items":snapshot.items}))?
    ))
}
pub fn read(id: &str) -> Result<String> {
    read_at(&safety::config_dir()?.join("screendumps"), id)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistent_snapshot_roundtrip_redacts_and_rejects_paths() -> Result<()> {
        let dir = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "test directory")?
            .join(format!(
                "kborg-screendump-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| "clock")?
                    .as_nanos()
            ));
        let record = save_at(
            &dir,
            "ctx",
            "default",
            "pods",
            &[
                serde_json::json!({"kind":"Pod", "metadata":{"name":"demo"},"data":{"password":"never-show"}}),
            ],
        )?;
        assert_eq!(list_at(&dir)?.len(), 1);
        let text = read_at(&dir, &record.id)?;
        assert!(text.contains("Historical"));
        assert!(!text.contains("never-show"));
        assert!(read_at(&dir, "../../credentials").is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(dir.join(&record.id))
                    .map_err(|_| "stat")?
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
        let held = save_lock(&dir)?;
        assert!(save_at(&dir, "ctx", "default", "pods", &[]).is_err());
        assert_eq!(list_at(&dir)?.len(), 1);
        drop(held);
        for _ in 1..MAX_FILES {
            save_at(&dir, "ctx", "default", "pods", &[])?;
        }
        assert_eq!(list_at(&dir)?.len(), MAX_FILES);
        assert!(save_at(&dir, "ctx", "default", "pods", &[]).is_err());
        assert_eq!(list_at(&dir)?.len(), MAX_FILES);
        std::fs::remove_dir_all(dir).map_err(|_| "cleanup")?;
        Ok(())
    }
}
