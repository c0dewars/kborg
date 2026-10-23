//! Explicit, data-only bookmarks. Reading preferences never navigates or enables a capability.
use crate::{Result, gateway, safety, selectors};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, File, OpenOptions},
    path::Path,
};

pub const MAX_RECORDS: usize = 16;
pub const MAX_CONTEXTS: usize = 4;
const MAX_BYTES: u64 = 64 * 1024;
const FILE_NAME: &str = "v1.json";
const LOCK_NAME: &str = ".lock";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Selection {
    pub name: String,
    /// Offline choices cannot silently become live choices with the same context name.
    pub demo: bool,
    pub target: Target,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "view", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Target {
    Dashboard {
        contexts: Vec<String>,
        namespace: String,
    },
    Resource {
        context: String,
        namespace: String,
        /// Canonical discovered plural, including its API group when present.
        kind: String,
        label_selector: String,
        workload: Option<Workload>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Workload {
    pub namespace: String,
    pub name: String,
    pub uid: String,
}

impl Workload {
    /// Selection is optional: a recreated object never inherits the old focus.
    pub fn matches(&self, object: &Value) -> bool {
        self.validate().is_ok()
            && object["metadata"]["namespace"].as_str() == Some(&self.namespace)
            && object["metadata"]["name"].as_str() == Some(&self.name)
            && object["metadata"]["uid"].as_str() == Some(&self.uid)
    }

    fn validate(&self) -> Result<()> {
        if !safety::valid_id(&self.namespace)
            || !dns_name(&self.name)
            || self.uid.is_empty()
            || self.uid.len() > 253
            || !self
                .uid
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        {
            return Err("saved workload requires a valid namespace, name and UID".into());
        }
        Ok(())
    }
}

fn dns_name(value: &str) -> bool {
    !value.is_empty() && value.len() <= 253 && value.split('.').all(safety::valid_id)
}

fn resource_key(value: &str) -> bool {
    match value.split_once('.') {
        Some((plural, group)) => safety::valid_id(plural) && dns_name(group),
        None => safety::valid_id(value),
    }
}

fn valid_context(context: &str) -> bool {
    !context.trim().is_empty() && context.len() <= 253 && safety::safe_text(context, 253) == context
}

fn validate_name(name: &str) -> Result<()> {
    if name.is_empty()
        || name.trim() != name
        || name.len() > 128
        || name.chars().count() > 64
        || safety::safe_text(name, 64) != name
    {
        return Err(
            "selection name must be 1..64 plain characters (128 bytes), without surrounding spaces"
                .into(),
        );
    }
    Ok(())
}

fn validate_namespace(namespace: &str) -> Result<()> {
    if namespace != "*" && !safety::valid_id(namespace) {
        return Err("saved namespace must be a namespace name or * for all namespaces".into());
    }
    Ok(())
}

fn workload_kind(kind: &str) -> bool {
    matches!(
        kind,
        "pods"
            | "deployments.apps"
            | "statefulsets.apps"
            | "daemonsets.apps"
            | "replicasets.apps"
            | "jobs.batch"
            | "cronjobs.batch"
            | "replicationcontrollers"
    )
}

impl Selection {
    pub fn validate(&self) -> Result<()> {
        validate_name(&self.name)?;
        match &self.target {
            Target::Dashboard {
                contexts,
                namespace,
            } => {
                if contexts.is_empty() || contexts.len() > MAX_CONTEXTS {
                    return Err("a saved dashboard requires 1..4 contexts".into());
                }
                let mut unique = std::collections::HashSet::new();
                if contexts
                    .iter()
                    .any(|context| !valid_context(context) || !unique.insert(context))
                {
                    return Err("saved dashboard contexts must be valid and distinct".into());
                }
                validate_namespace(namespace)?;
            }
            Target::Resource {
                context,
                namespace,
                kind,
                label_selector,
                workload,
            } => {
                if !valid_context(context) || !resource_key(kind) {
                    return Err(
                        "saved resource context or canonical resource kind is invalid".into(),
                    );
                }
                validate_namespace(namespace)?;
                selectors::validate(label_selector)?;
                if let Some(workload) = workload {
                    workload.validate()?;
                    if !workload_kind(kind)
                        || (namespace != "*" && namespace != &workload.namespace)
                    {
                        return Err(
                            "saved workload is outside its resource kind or namespace".into()
                        );
                    }
                }
            }
        }
        Ok(())
    }

    /// Re-read context names from the active kubeconfig, without creating clients or
    /// executing authentication helpers. Call on every explicit reopen, before changing UI scope.
    pub fn validate_contexts(&self, connection: &gateway::Connection) -> Result<()> {
        self.validate()?;
        if self.demo != connection.demo {
            return Err("saved selection belongs to a different live/demo mode".into());
        }
        // The offline dashboard intentionally supports named synthetic contexts.
        // They remain data-only and cannot cross the live/demo binding above.
        if connection.demo {
            return Ok(());
        }
        let available = gateway::contexts(connection)?;
        let contexts = match &self.target {
            Target::Dashboard { contexts, .. } => contexts.iter().collect::<Vec<_>>(),
            Target::Resource { context, .. } => vec![context],
        };
        if contexts.iter().any(|context| !available.contains(context)) {
            return Err(
                "saved context is unavailable in the active kubeconfig; current scope retained"
                    .into(),
            );
        }
        Ok(())
    }

    /// The catalog must come from the newly selected target context, never from the
    /// previous view. No stale alias or resource definition is restored from disk.
    pub fn validate_scope(&self, kinds: &[gateway::Kind]) -> Result<()> {
        self.validate()?;
        if let Target::Resource {
            kind,
            namespace,
            workload,
            ..
        } = &self.target
        {
            let matches = kinds
                .iter()
                .filter(|candidate| candidate.key() == *kind)
                .collect::<Vec<_>>();
            if matches.len() != 1 {
                return Err(
                    "saved resource kind is unavailable or ambiguous in the target context".into(),
                );
            }
            if !matches[0].namespaced && (namespace != "*" || workload.is_some()) {
                return Err(
                    "saved cluster resource requires all-namespace scope and no workload focus"
                        .into(),
                );
            }
        }
        Ok(())
    }
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Document {
    schema_version: u32,
    selections: Vec<Selection>,
}

fn decode(bytes: &[u8]) -> Result<Vec<Selection>> {
    if bytes.len() as u64 > MAX_BYTES {
        return Err("saved selections exceed the 64 KiB limit".into());
    }
    let document: Document =
        serde_json::from_slice(bytes).map_err(|_| "invalid saved selections; file was retained")?;
    if document.schema_version != 1 {
        return Err("unsupported saved selection schema; no implicit migration".into());
    }
    if document.selections.len() > MAX_RECORDS {
        return Err("saved selections exceed the 16-record limit".into());
    }
    let mut unique = std::collections::HashSet::new();
    for selection in &document.selections {
        selection.validate()?;
        if !unique.insert(&selection.name) {
            return Err("duplicate saved selection names are ambiguous".into());
        }
    }
    Ok(document.selections)
}

fn private_metadata(path: &Path, directory: bool) -> Result<Option<fs::Metadata>> {
    safety::checked_path(path)?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cannot inspect saved selection storage".into()),
    };
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err("saved selection storage has an unexpected file type".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(
                "saved selection storage must be private (directory 0700, files 0600)".into(),
            );
        }
    }
    Ok(Some(metadata))
}

fn list_at(directory: &Path) -> Result<Vec<Selection>> {
    if private_metadata(directory, true)?.is_none() {
        return Ok(Vec::new());
    }
    let path = directory.join(FILE_NAME);
    if private_metadata(&path, false)?.is_none() {
        return Ok(Vec::new());
    }
    decode(&safety::read_small(&path, MAX_BYTES)?)
}

/// List only. This does not create storage, restore scope or perform cluster requests.
pub fn list() -> Result<Vec<Selection>> {
    list_at(&safety::config_dir()?.join("selections"))
}

fn mutation_lock(directory: &Path) -> Result<File> {
    safety::ensure_private_dir(directory)?;
    private_metadata(directory, true)?;
    let path = directory.join(LOCK_NAME);
    private_metadata(&path, false)?;
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options
        .open(&path)
        .map_err(|_| "cannot open saved selection lock")?;
    if private_metadata(&path, false)?.is_none_or(|metadata| metadata.len() != 0) {
        return Err("invalid saved selection lock".into());
    }
    file.try_lock()
        .map_err(|_| "another saved selection update is in progress; retry shortly")?;
    Ok(file)
}

fn persist(directory: &Path, selections: Vec<Selection>) -> Result<()> {
    let document = Document {
        schema_version: 1,
        selections,
    };
    let mut bytes =
        serde_json::to_vec_pretty(&document).map_err(|_| "cannot encode saved selections")?;
    bytes.push(b'\n');
    decode(&bytes)?;
    safety::atomic_write(&directory.join(FILE_NAME), &bytes)
}

fn save_at(directory: &Path, selection: &Selection) -> Result<()> {
    selection.validate()?;
    let _lock = mutation_lock(directory)?;
    let mut selections = list_at(directory)?;
    if selections.iter().any(|saved| saved.name == selection.name) {
        return Err(
            "selection name already exists; choose another name or explicitly delete it first"
                .into(),
        );
    }
    if selections.len() >= MAX_RECORDS {
        return Err("16 selections saved; explicitly delete one before saving more".into());
    }
    selections.push(selection.clone());
    persist(directory, selections)
}

/// Call only after an explicit save action. Duplicate names and corrupt files are retained.
pub fn save(selection: &Selection) -> Result<()> {
    save_at(&safety::config_dir()?.join("selections"), selection)
}

fn delete_at(directory: &Path, name: &str) -> Result<()> {
    validate_name(name)?;
    // A nonexistent record must not create a preferences directory or lock file.
    if !list_at(directory)?.iter().any(|saved| saved.name == name) {
        return Err("saved selection no longer exists".into());
    }
    let _lock = mutation_lock(directory)?;
    let mut selections = list_at(directory)?;
    let index = selections
        .iter()
        .position(|saved| saved.name == name)
        .ok_or("saved selection no longer exists")?;
    selections.remove(index);
    persist(directory, selections)
}

/// Delete only the exact explicitly selected record; no filesystem paths derive from its name.
pub fn delete(name: &str) -> Result<()> {
    delete_at(&safety::config_dir()?.join("selections"), name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};

    static SERIAL: AtomicU64 = AtomicU64::new(0);

    fn directory(label: &str) -> std::path::PathBuf {
        std::env::temp_dir()
            .canonicalize()
            .unwrap_or_else(|error| panic!("{error}"))
            .join(format!(
                "kborg-selections-{label}-{}-{}",
                std::process::id(),
                SERIAL.fetch_add(1, Ordering::Relaxed)
            ))
    }

    fn resource() -> Selection {
        Selection {
            name: "Application API".into(),
            demo: false,
            target: Target::Resource {
                context: "docker-desktop".into(),
                namespace: "demo".into(),
                kind: "deployments.apps".into(),
                label_selector: "app in (api,web),!disabled".into(),
                workload: Some(Workload {
                    namespace: "demo".into(),
                    name: "api".into(),
                    uid: "1234-abcd".into(),
                }),
            },
        }
    }

    fn document(records: Vec<Selection>) -> Vec<u8> {
        serde_json::to_vec(&Document {
            schema_version: 1,
            selections: records,
        })
        .unwrap_or_else(|error| panic!("{error}"))
    }

    #[test]
    fn explicit_private_save_delete_and_read_without_side_effects() -> Result<()> {
        let dir = directory("roundtrip");
        assert!(list_at(&dir)?.is_empty());
        assert!(!dir.exists());
        assert!(delete_at(&dir, "missing").is_err());
        assert!(!dir.exists());
        let record = resource();
        save_at(&dir, &record)?;
        assert_eq!(list_at(&dir)?, vec![record.clone()]);
        let before = fs::read(dir.join(FILE_NAME)).map_err(|_| "read fixture")?;
        assert!(save_at(&dir, &record).is_err());
        assert!(delete_at(&dir, "missing").is_err());
        assert_eq!(
            before,
            fs::read(dir.join(FILE_NAME)).map_err(|_| "read fixture")?
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for (path, mode) in [
                (dir.clone(), 0o700),
                (dir.join(FILE_NAME), 0o600),
                (dir.join(LOCK_NAME), 0o600),
            ] {
                assert_eq!(
                    fs::metadata(path)
                        .map_err(|_| "metadata")?
                        .permissions()
                        .mode()
                        & 0o777,
                    mode
                );
            }
        }
        delete_at(&dir, &record.name)?;
        assert!(list_at(&dir)?.is_empty());
        fs::remove_dir_all(dir).map_err(|_| "cleanup")?;
        Ok(())
    }

    #[test]
    fn strict_schema_rejects_extra_fields_duplicates_and_future_versions() {
        for bytes in [
            br#"{"schema_version":2,"selections":[]}"#.as_slice(),
            br#"{"schema_version":1,"selections":[],"allow_write":true}"#,
            br#"{"schema_version":1,"schema_version":1,"selections":[]}"#,
            br#"{"schema_version":1,"selections":[],"selections":[]}"#,
            b"not JSON",
        ] {
            assert!(decode(bytes).is_err());
        }
        for extra in [
            "writable",
            "token",
            "approval",
            "copilot_consent",
            "kubeconfig",
        ] {
            let mut record = serde_json::to_value(resource())
                .unwrap_or_else(|_| panic!("fixture encoding failed"));
            record[extra] = json!(true);
            assert!(
                decode(
                    &serde_json::to_vec(&json!({"schema_version":1,"selections":[record]}))
                        .unwrap_or_else(|_| panic!("fixture encoding failed"))
                )
                .is_err()
            );
        }
        let mut nested =
            serde_json::to_value(resource()).unwrap_or_else(|_| panic!("fixture encoding failed"));
        nested["target"]["credentials"] = json!("untrusted");
        assert!(
            decode(
                &serde_json::to_vec(&json!({"schema_version":1,"selections":[nested]}))
                    .unwrap_or_else(|_| panic!("fixture encoding failed"))
            )
            .is_err()
        );
        assert!(decode(&document(vec![resource(), resource()])).is_err());
        assert!(decode(&vec![b' '; MAX_BYTES as usize + 1]).is_err());
    }

    #[test]
    fn names_contexts_scopes_selectors_and_record_counts_are_bounded() -> Result<()> {
        let long_group = [
            "a".repeat(63),
            "b".repeat(63),
            "c".repeat(63),
            "d".repeat(61),
        ]
        .join(".");
        assert!(resource_key(&format!("{}.{}", "p".repeat(63), long_group)));
        assert!(!resource_key(&format!("{}.example.io", "p".repeat(64))));
        assert!(!resource_key(&format!("pods.{long_group}x")));
        let mut record = resource();
        for invalid in ["", " spaces ", "hidden\u{202e}", "line\nfeed"] {
            record.name = invalid.into();
            assert!(record.validate().is_err());
        }
        record.name = "é".repeat(65);
        assert!(record.validate().is_err());
        record = resource();
        if let Target::Resource {
            namespace,
            label_selector,
            ..
        } = &mut record.target
        {
            *namespace = "other".into();
            *label_selector = "app in (api".into();
        }
        assert!(record.validate().is_err());
        for contexts in [
            vec![],
            vec!["x".into(); 2],
            vec!["a".into(), "b".into(), "c".into(), "d".into(), "e".into()],
            vec!["bad\ncontext".into()],
        ] {
            record.target = Target::Dashboard {
                contexts,
                namespace: "*".into(),
            };
            assert!(record.validate().is_err());
        }
        record.target = Target::Dashboard {
            contexts: vec!["desktop".into(), "local".into()],
            namespace: "*".into(),
        };
        record.validate()?;
        let dir = directory("limit");
        for index in 0..MAX_RECORDS {
            record.name = format!("Selection {index}");
            save_at(&dir, &record)?;
        }
        record.name = "Overflow".into();
        assert!(save_at(&dir, &record).is_err());
        assert_eq!(list_at(&dir)?.len(), MAX_RECORDS);
        fs::remove_dir_all(dir).map_err(|_| "cleanup")?;
        Ok(())
    }

    #[test]
    fn reopen_rechecks_current_kubeconfig_and_mode_without_client_requests() -> Result<()> {
        let dir = directory("contexts");
        let path = dir.join("config");
        let write_config = |context: &str| {
            safety::atomic_write(&path, &serde_json::to_vec(&json!({"apiVersion":"v1","kind":"Config","contexts":[{"name":context,"context":{"cluster":"unused"}}]})).unwrap_or_else(|_| panic!("fixture encoding failed")))
        };
        write_config("docker-desktop")?;
        let connection = gateway::Connection {
            kubeconfig: Some(path.clone()),
            ..Default::default()
        };
        let mut record = resource();
        record.validate_contexts(&connection)?;
        write_config("different-context")?;
        assert!(record.validate_contexts(&connection).is_err());
        let demo = gateway::Connection {
            demo: true,
            ..Default::default()
        };
        assert!(record.validate_contexts(&demo).is_err());
        record.demo = true;
        record.target = Target::Dashboard {
            contexts: vec!["demo".into(), "demo-secondary".into()],
            namespace: "*".into(),
        };
        record.validate_contexts(&demo)?;
        assert!(record.validate_contexts(&connection).is_err());
        fs::remove_dir_all(dir).map_err(|_| "cleanup")?;
        Ok(())
    }

    #[test]
    fn scope_requires_current_canonical_discovery_and_focus_requires_exact_uid() -> Result<()> {
        let mut record = resource();
        let kinds = gateway::kinds();
        record.validate_scope(&kinds)?;
        assert!(record.validate_scope(&[]).is_err());
        let Target::Resource {
            workload: Some(workload),
            ..
        } = &record.target
        else {
            panic!("fixture")
        };
        let mut object = json!({"metadata":{"namespace":"demo","name":"api","uid":"1234-abcd"}});
        assert!(workload.matches(&object));
        for field in ["namespace", "name", "uid"] {
            let mut changed = object.clone();
            changed["metadata"][field] = json!("changed");
            assert!(!workload.matches(&changed));
        }
        object["metadata"]
            .as_object_mut()
            .unwrap_or_else(|| panic!("fixture object missing"))
            .remove("uid");
        assert!(!workload.matches(&object));
        record.target = Target::Resource {
            context: "docker-desktop".into(),
            namespace: "demo".into(),
            kind: "nodes".into(),
            label_selector: String::new(),
            workload: None,
        };
        assert!(record.validate_scope(&kinds).is_err());
        if let Target::Resource { namespace, .. } = &mut record.target {
            *namespace = "*".into();
        }
        record.validate_scope(&kinds)?;
        Ok(())
    }

    #[test]
    fn corrupt_file_and_concurrent_mutation_are_never_overwritten() -> Result<()> {
        let dir = directory("retained");
        save_at(&dir, &resource())?;
        let held = mutation_lock(&dir)?;
        assert!(delete_at(&dir, &resource().name).is_err());
        drop(held);
        safety::atomic_write(&dir.join(FILE_NAME), b"corrupt preferences")?;
        assert!(list_at(&dir).is_err());
        assert!(save_at(&dir, &resource()).is_err());
        assert!(delete_at(&dir, &resource().name).is_err());
        assert_eq!(
            fs::read(dir.join(FILE_NAME)).map_err(|_| "read fixture")?,
            b"corrupt preferences"
        );
        fs::remove_dir_all(dir).map_err(|_| "cleanup")?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn nonprivate_files_and_symlink_storage_are_rejected() -> Result<()> {
        use std::os::unix::{fs::PermissionsExt, fs::symlink};
        let dir = directory("permissions");
        save_at(&dir, &resource())?;
        fs::set_permissions(dir.join(FILE_NAME), fs::Permissions::from_mode(0o644))
            .map_err(|_| "chmod")?;
        assert!(list_at(&dir).is_err());
        assert!(save_at(&dir, &resource()).is_err());
        fs::set_permissions(dir.join(FILE_NAME), fs::Permissions::from_mode(0o600))
            .map_err(|_| "chmod")?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).map_err(|_| "chmod")?;
        assert!(list_at(&dir).is_err());
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700)).map_err(|_| "chmod")?;
        let alias = dir.with_extension("link");
        symlink(&dir, &alias).map_err(|_| "symlink")?;
        assert!(list_at(&alias).is_err());
        assert!(save_at(&alias, &resource()).is_err());
        fs::remove_file(alias).map_err(|_| "cleanup")?;
        fs::remove_dir_all(dir).map_err(|_| "cleanup")?;
        Ok(())
    }
}
