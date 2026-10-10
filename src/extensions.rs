//! Data-only registration for explicitly reviewed local read-only views.
//! Registration does not execute a program or grant a KBorg capability. Every
//! invocation still requires the complete handoff input preview and RUN TOOL.
use crate::{Result, handoff, safety};
#[cfg(unix)]
use ring::digest::{Context as Digest, SHA256};
use serde::{Deserialize, Serialize};
use serde_json::Value;
#[cfg(unix)]
use std::io::Read;
use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

pub const CONFIRMATION: &str = "REGISTER EXTENSION";
pub const MAX_ENTRIES: usize = 64;
const MAX_ENTRY: u64 = 64 * 1024;
const MAX_EXECUTABLE: u64 = 128 * 1024 * 1024;
const REVIEW_LIFETIME: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    sha256: String,
    length: u64,
    device: u64,
    inode: u64,
    mode: u32,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    id: String,
    executable: PathBuf,
    arguments: Vec<String>,
    identity: Identity,
}

/// Private fields make an approval apply to this exact executable and argv.
pub struct Registration {
    manifest: Manifest,
    prepared_at: Instant,
}
impl Registration {
    pub fn summary(&self) -> String {
        let entry = &self.manifest;
        format!(
            "REGISTER LOCAL READ-ONLY VIEW\nName: {}\nNative executable: {}\nExact argv: {:?}\nExecutable SHA-256: {}\nSize: {} bytes\nRegistration runs nothing and grants no cluster, write, exec or network capability in KBorg.\nArguments remain literal on every invocation; object metadata never substitutes into argv.\nEvery invocation requires --allow-extensions, --allow-tools and a new whole-input RUN TOOL approval.\nExecution uses the existing supervised handoff: 15 seconds, 256 KiB captured output, minimized environment, no output applied to Kubernetes. The trusted program retains OS filesystem/network permissions; this is not an OS sandbox.\nExecutable changes invalidate registration. Approval expires in 60 seconds.\nType {CONFIRMATION} to save this private local registration.\n",
            entry.id,
            entry.executable.display(),
            entry.arguments,
            entry.identity.sha256,
            entry.identity.length,
        )
    }
    /// Directory is explicit so tests/managed callers do not alter user defaults.
    /// Existing registrations are never silently replaced.
    pub fn save(self, directory: &Path, confirmation: &str) -> Result<()> {
        if confirmation != CONFIRMATION || self.prepared_at.elapsed() > REVIEW_LIFETIME {
            return Err("extension registration approval is invalid or expired".into());
        }
        validate_manifest(&self.manifest)?;
        revalidate(&self.manifest)?;
        if !directory.is_absolute() {
            return Err("extension registry directory must be absolute".into());
        }
        safety::ensure_private_dir(directory)?;
        check_directory(directory)?;
        let _lock = registry_lock(directory)?;
        if entry_paths(directory)?.len() >= MAX_ENTRIES {
            return Err("extension registry reached 64 entries; remove an entry first".into());
        }
        let bytes = serde_json::to_vec_pretty(&self.manifest)
            .map_err(|_| "cannot encode extension registration")?;
        if bytes.len() as u64 > MAX_ENTRY {
            return Err("extension registration exceeds 64 KiB".into());
        }
        safety::write_new(&entry_path(directory, &self.manifest.id)?, &bytes)
    }
}

pub fn prepare_registration(
    id: &str,
    executable: &Path,
    arguments: &[String],
) -> Result<Registration> {
    if !cfg!(unix) {
        return Err("registered tool views require verified Unix process cleanup; unsupported on this platform".into());
    }
    validate_id(id)?;
    validate_command(executable, arguments)?;
    let identity = fingerprint(executable)?;
    Ok(Registration {
        manifest: Manifest {
            schema: 1,
            id: id.into(),
            executable: executable.into(),
            arguments: arguments.to_vec(),
            identity,
        },
        prepared_at: Instant::now(),
    })
}
pub fn registry_dir() -> Result<PathBuf> {
    Ok(safety::config_dir()?.join("extensions"))
}
#[derive(Clone, Debug, Serialize)]
pub struct EntrySummary {
    pub id: String,
    pub executable: PathBuf,
    pub arguments: Vec<String>,
    /// Registered fingerprint; current contents are checked on load/invocation.
    pub sha256: String,
}
pub fn list() -> Result<Vec<EntrySummary>> {
    list_in(&registry_dir()?)
}
pub fn list_in(directory: &Path) -> Result<Vec<EntrySummary>> {
    if !directory.is_absolute() {
        return Err("extension registry directory must be absolute".into());
    }
    if !directory.exists() {
        safety::checked_path(directory)?;
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for path in entry_paths(directory)? {
        let manifest = read_entry(&path)?;
        entries.push(EntrySummary {
            id: manifest.id,
            executable: manifest.executable,
            arguments: manifest.arguments,
            sha256: manifest.identity.sha256,
        });
    }
    entries.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(entries)
}

/// A loaded entry is sealed; prepare revalidates it before handing off any input.
pub struct Registered {
    manifest: Manifest,
}
impl Registered {
    pub fn summary(&self) -> String {
        format!(
            "Registered view: {}\nNative executable: {}\nExact argv: {:?}\nRegistered SHA-256: {}\n",
            self.manifest.id,
            self.manifest.executable.display(),
            self.manifest.arguments,
            self.manifest.identity.sha256
        )
    }
    pub fn prepare(&self, object: &Value, format: handoff::Format) -> Result<handoff::Prepared> {
        revalidate(&self.manifest)?;
        let prepared = handoff::prepare(
            &self.manifest.executable,
            &self.manifest.arguments,
            object,
            format,
        )?;
        // Check again after handoff captured its immutable executable/input.
        revalidate(&self.manifest)?;
        Ok(prepared)
    }
}
pub fn load(id: &str) -> Result<Registered> {
    load_from(&registry_dir()?, id)
}
pub fn load_from(directory: &Path, id: &str) -> Result<Registered> {
    let path = entry_path(directory, id)?;
    // Validate the finite directory before opening an entry, not only on list.
    entry_paths(directory)?;
    let manifest = read_entry(&path)?;
    revalidate(&manifest)?;
    Ok(Registered { manifest })
}
pub fn remove(id: &str) -> Result<()> {
    remove_from(&registry_dir()?, id)
}
pub fn remove_from(directory: &Path, id: &str) -> Result<()> {
    check_directory(directory)?;
    let _lock = registry_lock(directory)?;
    let path = entry_path(directory, id)?;
    check_entry(&path)?;
    // Local explicit removal also works for stale/corrupt registrations. No
    // executable is read or run; the target is only the validated ID filename.
    fs::remove_file(path).map_err(|_| "cannot remove extension registration".into())
}

fn validate_id(id: &str) -> Result<()> {
    if safety::valid_id(id) {
        Ok(())
    } else {
        Err("extension name must be 1..63 lowercase letters/digits/hyphens".into())
    }
}
fn entry_path(directory: &Path, id: &str) -> Result<PathBuf> {
    validate_id(id)?;
    if !directory.is_absolute() {
        return Err("extension registry directory must be absolute".into());
    }
    safety::checked_path(directory)?;
    Ok(directory.join(format!("{id}.json")))
}
fn check_directory(directory: &Path) -> Result<()> {
    if !directory.is_absolute() {
        return Err("extension registry directory must be absolute".into());
    }
    safety::checked_path(directory)?;
    let metadata =
        fs::symlink_metadata(directory).map_err(|_| "extension registry directory unavailable")?;
    if !metadata.is_dir() {
        return Err("extension registry must be a directory".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("extension registry directory must be private (0700)".into());
        }
    }
    Ok(())
}
fn entry_paths(directory: &Path) -> Result<Vec<PathBuf>> {
    check_directory(directory)?;
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory).map_err(|_| "cannot list extension registry")? {
        let path = entry
            .map_err(|_| "cannot list extension registry entry")?
            .path();
        if path.file_name().and_then(|name| name.to_str()) == Some(".registry-lock") {
            check_lock(&path)?;
            continue;
        }
        if paths.len() >= MAX_ENTRIES {
            return Err("extension registry exceeds 64 entries".into());
        }
        let id = path
            .file_stem()
            .and_then(|id| id.to_str())
            .ok_or("invalid extension registry filename")?;
        validate_id(id)?;
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            return Err("extension registry contains an unexpected filename".into());
        }
        check_entry(&path)?;
        paths.push(path);
    }
    Ok(paths)
}
fn check_lock(path: &Path) -> Result<()> {
    safety::checked_path(path)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "cannot inspect extension registry lock")?;
    if !metadata.is_file() || metadata.len() != 0 {
        return Err("invalid extension registry lock".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("extension registry lock must be private".into());
        }
    }
    Ok(())
}
fn registry_lock(directory: &Path) -> Result<fs::File> {
    let path = directory.join(".registry-lock");
    safety::checked_path(&path)?;
    let mut options = fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .map_err(|_| "cannot open extension registry lock")?;
    check_lock(&path)?;
    file.try_lock()
        .map_err(|_| "extension registry is busy; retry after the current local change")?;
    Ok(file)
}
fn check_entry(path: &Path) -> Result<()> {
    safety::checked_path(path)?;
    let metadata = fs::symlink_metadata(path).map_err(|_| "extension registration unavailable")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > MAX_ENTRY {
        return Err("extension registration must be a bounded regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("extension registration must be private (0600)".into());
        }
    }
    Ok(())
}
fn read_entry(path: &Path) -> Result<Manifest> {
    check_entry(path)?;
    let bytes = safety::read_small(path, MAX_ENTRY)?;
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|_| "invalid extension registration (details suppressed)")?;
    validate_manifest(&manifest)?;
    if path.file_stem().and_then(|id| id.to_str()) != Some(manifest.id.as_str()) {
        return Err("extension filename and registered name differ".into());
    }
    Ok(manifest)
}
fn validate_command(executable: &Path, arguments: &[String]) -> Result<()> {
    if !executable.is_absolute()
        || executable
            .to_str()
            .is_none_or(|path| path.len() > 4096 || safety::safe_text(path, 4096) != path)
    {
        return Err(
            "extension needs an absolute bounded native executable; PATH lookup is disabled".into(),
        );
    }
    if arguments.len() > 64
        || arguments.iter().map(String::len).sum::<usize>() > 16 * 1024
        || arguments
            .iter()
            .any(|argument| argument.len() > 4096 || safety::safe_text(argument, 4096) != *argument)
    {
        return Err("extension argv exceeds bounds or contains terminal controls".into());
    }
    safety::checked_path(executable)
}
fn validate_manifest(manifest: &Manifest) -> Result<()> {
    if manifest.schema != 1 {
        return Err("unsupported extension registry schema".into());
    }
    validate_id(&manifest.id)?;
    validate_command(&manifest.executable, &manifest.arguments)?;
    if manifest.identity.sha256.len() != 64
        || !manifest
            .identity
            .sha256
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        || manifest.identity.length == 0
        || manifest.identity.length > MAX_EXECUTABLE
    {
        return Err("extension executable fingerprint is malformed".into());
    }
    Ok(())
}
fn revalidate(manifest: &Manifest) -> Result<()> {
    validate_manifest(manifest)?;
    if fingerprint(&manifest.executable)? != manifest.identity {
        return Err("registered executable changed; remove and review the extension again".into());
    }
    Ok(())
}
fn fingerprint(path: &Path) -> Result<Identity> {
    validate_command(path, &[])?;
    let before = fs::symlink_metadata(path).map_err(|_| "cannot inspect extension executable")?;
    if !before.is_file() || before.len() == 0 || before.len() > MAX_EXECUTABLE {
        return Err("extension needs a regular native executable of at most 128 MiB".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.mode() & 0o111 == 0 || before.mode() & 0o022 != 0 {
            return Err(
                "extension executable must be executable and cannot be group/world writable".into(),
            );
        }
        let mut file = fs::File::open(path).map_err(|_| "cannot read extension executable")?;
        let mut digest = Digest::new(&SHA256);
        let mut length = 0u64;
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let count = file
                .read(&mut buffer)
                .map_err(|_| "cannot hash extension executable")?;
            if count == 0 {
                break;
            }
            if length == 0 && (count < 4 || !native_magic(&buffer[..4])) {
                return Err(
                    "extensions require native binaries; interpreter scripts are excluded".into(),
                );
            }
            length += count as u64;
            if length > MAX_EXECUTABLE {
                return Err("extension executable exceeds 128 MiB".into());
            }
            digest.update(&buffer[..count]);
        }
        let after =
            fs::symlink_metadata(path).map_err(|_| "cannot recheck extension executable")?;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.mode() != after.mode()
            || before.len() != length
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
        {
            return Err("extension executable changed during identity check".into());
        }
        let sha256 = digest
            .finish()
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Identity {
            sha256,
            length,
            device: before.dev(),
            inode: before.ino(),
            mode: before.mode(),
            modified_seconds: before.mtime(),
            modified_nanoseconds: before.mtime_nsec(),
        })
    }
    #[cfg(not(unix))]
    {
        Err("native extension fingerprint support is unavailable on this platform".into())
    }
}
#[cfg(unix)]
fn native_magic(magic: &[u8]) -> bool {
    matches!(
        magic,
        b"\x7fELF"
            | b"\xfe\xed\xfa\xce"
            | b"\xce\xfa\xed\xfe"
            | b"\xfe\xed\xfa\xcf"
            | b"\xcf\xfa\xed\xfe"
            | b"\xca\xfe\xba\xbe"
            | b"\xbe\xba\xfe\xca"
            | b"\xca\xfe\xba\xbf"
            | b"\xbf\xba\xfe\xca"
    )
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use serde_json::json;
    use std::os::unix::fs::{PermissionsExt, symlink};

    struct Temporary(PathBuf);
    impl Temporary {
        fn new() -> Result<Self> {
            static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let root = std::env::temp_dir()
                .canonicalize()
                .map_err(|_| "fixture temp root missing")?;
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_err(|_| "fixture time unavailable")?
                .as_nanos();
            let path = root.join(format!(
                "kborg-extension-tests-{}-{stamp}-{sequence}",
                std::process::id()
            ));
            safety::ensure_private_dir(&path)?;
            Ok(Self(path))
        }
    }
    impl Drop for Temporary {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn native() -> Result<PathBuf> {
        Path::new("/bin/echo")
            .canonicalize()
            .map_err(|_| "native echo unavailable".into())
    }

    #[test]
    fn registering_lists_and_removes_data_without_executing_or_granting_capabilities() -> Result<()>
    {
        let directory = Temporary::new()?;
        let executable = native()?;
        let arguments = vec![
            "$(touch SHOULD_NOT_EXIST); literal ${namespace}".into(),
            "--write".into(),
        ];
        let registration = prepare_registration("literal-view", &executable, &arguments)?;
        assert!(registration.summary().contains("SHA-256"));
        registration.save(&directory.0, CONFIRMATION)?;
        assert!(!directory.0.join("SHOULD_NOT_EXIST").exists());
        let entries = list_in(&directory.0)?;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].arguments, arguments);
        let entry = load_from(&directory.0, "literal-view")?;
        assert!(entry.summary().contains("literal-view"));
        let view = entry.prepare(&json!({"metadata":{"namespace":"test","annotations":{"secret":"CANARY"}},"spec":{"env":[{"value":"CANARY"}]}}), handoff::Format::Json)?;
        assert!(!view.input_preview().contains("CANARY"));
        assert!(handoff::run(view, handoff::CONFIRMATION, false).is_err());
        let view = entry.prepare(
            &json!({"metadata":{"namespace":"real-namespace"}}),
            handoff::Format::Json,
        )?;
        let outcome = handoff::run(view, handoff::CONFIRMATION, true)?;
        assert!(outcome.success);
        assert!(outcome.stdout.contains("literal ${namespace}"));
        assert!(!outcome.stdout.contains("real-namespace"));
        assert!(
            entry
                .prepare(&json!({"kind":"Secret"}), handoff::Format::Json)
                .is_err()
        );
        remove_from(&directory.0, "literal-view")?;
        assert!(list_in(&directory.0)?.is_empty());
        Ok(())
    }

    #[test]
    fn approval_expiry_invalid_paths_scripts_symlinks_and_controls_never_register() -> Result<()> {
        let directory = Temporary::new()?;
        let executable = native()?;
        for id in ["../escape", "x/y", "", "Upper", "name-"] {
            assert!(prepare_registration(id, &executable, &[]).is_err());
        }
        assert!(prepare_registration("valid", Path::new("echo"), &[]).is_err());
        assert!(prepare_registration("valid", &executable, &["\u{1b}[2J".into()]).is_err());
        assert!(prepare_registration("valid", &executable, &vec!["x".into(); 65]).is_err());
        let registry = directory.0.join("registry");
        let wrong = prepare_registration("wrong", &executable, &[])?;
        assert!(wrong.save(&registry, "yes").is_err());
        assert!(!registry.exists());
        let mut expired = prepare_registration("expired", &executable, &[])?;
        expired.prepared_at = Instant::now() - REVIEW_LIFETIME - Duration::from_secs(1);
        assert!(expired.save(&registry, CONFIRMATION).is_err());
        let script = directory.0.join("script");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").map_err(|_| "cannot write script fixture")?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700))
            .map_err(|_| "cannot set script fixture permissions")?;
        assert!(prepare_registration("script", &script, &[]).is_err());
        let link = directory.0.join("linked");
        symlink(&executable, &link).map_err(|_| "cannot write symlink fixture")?;
        assert!(prepare_registration("linked", &link, &[]).is_err());
        assert!(list_in(Path::new("relative-registry")).is_err());
        Ok(())
    }

    #[test]
    fn executable_updates_invalidate_loaded_and_prepared_registration_even_if_mtime_is_restored()
    -> Result<()> {
        use std::io::{Seek, Write};
        let directory = Temporary::new()?;
        let copied = directory.0.join("native");
        fs::copy(native()?, &copied).map_err(|_| "cannot copy executable fixture")?;
        fs::set_permissions(&copied, fs::Permissions::from_mode(0o700))
            .map_err(|_| "cannot set fixture permissions")?;
        let registry = directory.0.join("registry");
        prepare_registration("view", &copied, &[])?.save(&registry, CONFIRMATION)?;
        let loaded = load_from(&registry, "view")?;
        let pending = prepare_registration("pending", &copied, &[])?;
        let previous = safety::read_small(&copied, MAX_EXECUTABLE)?
            .last()
            .copied()
            .ok_or("fixture empty")?;
        let mtime = fs::metadata(&copied)
            .and_then(|metadata| metadata.modified())
            .map_err(|_| "fixture metadata unavailable")?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(&copied)
            .map_err(|_| "cannot change fixture")?;
        file.seek(std::io::SeekFrom::End(-1))
            .and_then(|_| file.write_all(&[previous ^ 0xff]))
            .and_then(|_| file.set_times(fs::FileTimes::new().set_modified(mtime)))
            .map_err(|_| "cannot change fixture")?;
        assert!(loaded.prepare(&json!({}), handoff::Format::Json).is_err());
        assert!(load_from(&registry, "view").is_err());
        assert!(pending.save(&registry, CONFIRMATION).is_err());
        // Stale entries remain visible and explicitly removable without running.
        assert_eq!(list_in(&registry)?.len(), 1);
        remove_from(&registry, "view")?;
        Ok(())
    }

    #[test]
    fn registry_bounds_schema_private_permissions_and_duplicate_fields_are_enforced() -> Result<()>
    {
        let directory = Temporary::new()?;
        let executable = native()?;
        let entry = prepare_registration("template", &executable, &[])?.manifest;
        for index in 0..MAX_ENTRIES {
            let mut manifest = entry.clone();
            manifest.id = format!("view-{index}");
            safety::write_new(
                &entry_path(&directory.0, &manifest.id)?,
                &serde_json::to_vec(&manifest).map_err(|_| "cannot encode fixture")?,
            )?;
        }
        assert_eq!(list_in(&directory.0)?.len(), MAX_ENTRIES);
        assert!(
            prepare_registration("over-limit", &executable, &[])?
                .save(&directory.0, CONFIRMATION)
                .is_err()
        );
        let path = entry_path(&directory.0, "view-0")?;
        let mut injected = serde_json::to_value(&entry).map_err(|_| "cannot encode fixture")?;
        injected["id"] = "view-0".into();
        injected["allow_write"] = true.into();
        fs::write(
            &path,
            serde_json::to_vec(&injected).map_err(|_| "cannot encode fixture")?,
        )
        .map_err(|_| "cannot write fixture")?;
        assert!(load_from(&directory.0, "view-0").is_err());
        remove_from(&directory.0, "view-0")?;
        let raw = serde_json::to_string(&entry).map_err(|_| "cannot encode fixture")?;
        let duplicate = raw.replacen("\"schema\":1", "\"schema\":1,\"schema\":1", 1);
        assert!(serde_json::from_str::<Manifest>(&duplicate).is_err());
        let path = entry_path(&directory.0, "view-1")?;
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .map_err(|_| "cannot set fixture permissions")?;
        assert!(list_in(&directory.0).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .map_err(|_| "cannot set fixture permissions")?;
        safety::write_new(&directory.0.join("unexpected.txt"), b"unsafe")?;
        assert!(list_in(&directory.0).is_err());
        Ok(())
    }

    #[test]
    fn concurrent_registry_mutations_are_bounded_by_nonblocking_native_file_lock() -> Result<()> {
        let directory = Temporary::new()?;
        let executable = native()?;
        let held = registry_lock(&directory.0)?;
        assert!(
            prepare_registration("view", &executable, &[])?
                .save(&directory.0, CONFIRMATION)
                .is_err()
        );
        assert!(list_in(&directory.0)?.is_empty());
        drop(held);
        prepare_registration("view", &executable, &[])?.save(&directory.0, CONFIRMATION)?;
        let raw = safety::read_small(&entry_path(&directory.0, "view")?, MAX_ENTRY)?;
        assert!(
            prepare_registration("view", &executable, &["changed".into()])?
                .save(&directory.0, CONFIRMATION)
                .is_err()
        );
        assert_eq!(
            safety::read_small(&entry_path(&directory.0, "view")?, MAX_ENTRY)?,
            raw
        );
        Ok(())
    }
}
