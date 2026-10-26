//! Data-only registration for explicitly reviewed local read-only views.
//! Registration does not execute a program or grant a KBorg capability. Every
//! invocation still requires the complete handoff input preview and RUN TOOL.
use crate::{Result, handoff, plugins, safety};
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
    /// Schema 2: k9s plugin fields; arguments may reference `$VARIABLES`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plugin: Option<plugins::Meta>,
}

/// Private fields make an approval apply to this exact executable and argv.
pub struct Registration {
    manifest: Manifest,
    prepared_at: Instant,
}
impl Registration {
    pub fn summary(&self) -> String {
        let entry = &self.manifest;
        if let Some(meta) = &entry.plugin {
            return plugin_summary(entry, meta);
        }
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
            plugin: None,
        },
        prepared_at: Instant::now(),
    })
}

/// Schema 2: a k9s plugin whose arguments may reference `$VARIABLES`. The
/// executable is still an explicit absolute native file pinned by SHA-256.
pub fn prepare_plugin_registration(
    id: &str,
    executable: &Path,
    arguments: &[String],
    meta: plugins::Meta,
) -> Result<Registration> {
    let mut registration = prepare_registration(id, executable, arguments)?;
    registration.manifest.schema = 2;
    registration.manifest.plugin = Some(meta);
    validate_manifest(&registration.manifest)?;
    Ok(registration)
}

fn plugin_summary(entry: &Manifest, meta: &plugins::Meta) -> String {
    let variables = plugins::referenced(&entry.arguments)
        .unwrap_or_default()
        .into_iter()
        .map(|name| format!("${name}"))
        .collect::<Vec<_>>();
    let confirmation = if meta.dangerous || plugins::is_shell(&entry.executable) {
        format!("{} {}", DANGEROUS, entry.id)
    } else {
        format!(
            "{} (or {} {} when the expanded arguments look like a cluster write)",
            handoff::CONFIRMATION,
            DANGEROUS,
            entry.id
        )
    };
    let shell = if plugins::is_shell(&entry.executable) {
        "\nSHELL: this plugin runs a shell script. Substituted values are limited to letters, digits and ._:/@=+,- and may not start with '-'; review the script itself before registering.\n"
    } else {
        ""
    };
    format!(
        "REGISTER K9S PLUGIN\nName: {}\nDescription: {}\nNative executable: {}\nArgument template: {:?}\nVariables: {}\nScopes: {}\nShortcut: {} (bound only where the key is free; see ? help)\nConfirmation on every run: {confirmation}\nk9s background/confirm/overwriteOutput are accepted: KBorg always shows the full review, runs in the foreground and captures the output.\n{shell}Variables expand inside each argument only: never through a shell, never split into more arguments, never globbed. Values from the cluster are refused when they contain control characters, or start with '-' where an option could begin.\nExecutable SHA-256: {}\nSize: {} bytes\nRegistration runs nothing. Every run requires --allow-extensions, --allow-tools and a fresh review of the exact expanded argv and stdin.\nExecution: 15 seconds, 256 KiB captured output, minimized environment, output never applied to Kubernetes. The trusted program keeps your OS user's filesystem/network permissions; this is not an OS sandbox.\nExecutable changes invalidate registration. Approval expires in 60 seconds.\nType {CONFIRMATION} to save this private local registration.\n",
        entry.id,
        safety::safe_text(&meta.description, plugins::MAX_DESCRIPTION),
        entry.executable.display(),
        entry.arguments,
        if variables.is_empty() {
            "none".into()
        } else {
            variables.join(" ")
        },
        if meta.scopes.is_empty() {
            "all".into()
        } else {
            meta.scopes.join(", ")
        },
        meta.short_cut.as_deref().unwrap_or("none"),
        entry.identity.sha256,
        entry.identity.length,
    )
}

/// Stronger phrase for dangerous or likely-writing plugin runs.
pub const DANGEROUS: &str = "RUN DANGEROUS";
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub plugin: Option<plugins::Meta>,
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
            plugin: manifest.plugin,
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
        if let Some(meta) = &self.manifest.plugin {
            return format!(
                "Registered k9s plugin: {} ({})\nNative executable: {}\nArgument template: {:?}\nRegistered SHA-256: {}\n",
                self.manifest.id,
                safety::safe_text(&meta.description, plugins::MAX_DESCRIPTION),
                self.manifest.executable.display(),
                self.manifest.arguments,
                self.manifest.identity.sha256
            );
        }
        format!(
            "Registered view: {}\nNative executable: {}\nExact argv: {:?}\nRegistered SHA-256: {}\n",
            self.manifest.id,
            self.manifest.executable.display(),
            self.manifest.arguments,
            self.manifest.identity.sha256
        )
    }
    pub fn prepare(&self, object: &Value, format: handoff::Format) -> Result<handoff::Prepared> {
        self.prepare_with(object, format, &plugins::Variables::default())
    }
    pub fn plugin(&self) -> Option<&plugins::Meta> {
        self.manifest.plugin.as_ref()
    }
    /// Variables the argument template references (none for schema 1).
    pub fn variables(&self) -> std::collections::BTreeSet<String> {
        if self.manifest.plugin.is_none() {
            return Default::default();
        }
        plugins::referenced(&self.manifest.arguments).unwrap_or_default()
    }
    /// Schema 1 arguments stay literal. Schema 2 arguments expand `values`
    /// element by element; dangerous or likely-writing runs need the
    /// stronger `RUN DANGEROUS NAME` phrase.
    pub fn prepare_with(
        &self,
        object: &Value,
        format: handoff::Format,
        values: &plugins::Variables,
    ) -> Result<handoff::Prepared> {
        revalidate(&self.manifest)?;
        let (argv, danger) = match &self.manifest.plugin {
            None => (self.manifest.arguments.clone(), None),
            Some(meta) => {
                let argv =
                    plugins::expand(&self.manifest.executable, &self.manifest.arguments, values)?;
                let danger = plugins::danger(meta, &self.manifest.executable, &argv);
                (argv, danger)
            }
        };
        let mut prepared = handoff::prepare(&self.manifest.executable, &argv, object, format)?;
        if danger.is_some() {
            prepared = prepared.require(format!("{DANGEROUS} {}", self.manifest.id));
        }
        // Check again after handoff captured its immutable executable/input.
        revalidate(&self.manifest)?;
        Ok(prepared)
    }
    /// Why this run needs the stronger phrase, for the review text.
    pub fn danger(&self, argv: &[String]) -> Option<&'static str> {
        self.manifest
            .plugin
            .as_ref()
            .and_then(|meta| plugins::danger(meta, &self.manifest.executable, argv))
    }
}

/// The exact expanded argv, one numbered element per line, for reviews.
pub fn argv_review(registered: &Registered, prepared: &handoff::Prepared) -> String {
    let mut out = format!(
        "EXACT ARGV ({} elements; each line is one argument, passed without a shell)\n  [0] {}\n",
        prepared.argv().len() + 1,
        safety::safe_text(&registered.manifest.executable.to_string_lossy(), 4096)
    );
    for (index, argument) in prepared.argv().iter().enumerate() {
        out.push_str(&format!("  [{}] {argument:?}\n", index + 1));
    }
    if let Some(reason) = registered.danger(prepared.argv()) {
        out.push_str(&format!(
            "DANGEROUS: {reason}. Type {} to run.\n",
            prepared.confirmation()
        ));
    }
    out
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
    match (manifest.schema, &manifest.plugin) {
        (1, None) => {}
        (2, Some(meta)) => {
            meta.validate()?;
            plugins::referenced(&manifest.arguments)?;
        }
        _ => return Err("unsupported extension registry schema".into()),
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

/// One k9s plugin entry, parsed but not trusted: `command` is only a name
/// until the user registers an explicit absolute executable for it.
#[derive(Clone, Debug)]
pub struct K9sPlugin {
    pub name: String,
    pub id: String,
    pub command: String,
    pub args: Vec<String>,
    pub meta: plugins::Meta,
    pipes: bool,
}

fn parse_k9s(text: &str) -> Result<Vec<K9sPlugin>> {
    #[derive(Deserialize)]
    struct PluginFile {
        plugins: std::collections::BTreeMap<String, Value>,
    }
    if text.len() > 256 * 1024 {
        return Err("plugins.yaml exceeds 256 KiB".into());
    }
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_reader_input_bytes: Some(256*1024), max_events: 50_000, max_aliases: 0, max_anchors: 0,
            max_depth: 8, max_inclusion_depth: 0, max_documents: 1, max_nodes: 20_000,
            max_total_scalar_bytes: 256*1024, max_merge_keys: 0,
        },
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys: serde_saphyr::MergeKeyPolicy::Error,
        with_snippet: false,
    };
    let file: PluginFile = serde_saphyr::from_str_with_options(text, options)
        .map_err(|_| "invalid plugins.yaml; expected a k9s plugins: mapping")?;
    let text = |value: &Value, limit: usize| {
        match value {
            Value::String(text) => text.clone(),
            Value::Null => String::new(),
            other => other.to_string(),
        }
        .chars()
        .take(limit)
        .collect::<String>()
    };
    Ok(file
        .plugins
        .iter()
        .take(256)
        .map(|(name, plugin)| {
            let name = safety::safe_text(name, 63);
            K9sPlugin {
                id: name.to_ascii_lowercase().replace([' ', '_'], "-"),
                command: text(&plugin["command"], 4096),
                args: plugin["args"]
                    .as_array()
                    .map(|args| args.iter().map(|arg| text(arg, 4097)).collect())
                    .unwrap_or_default(),
                meta: plugins::Meta {
                    description: safety::safe_text(
                        &text(&plugin["description"], 512),
                        plugins::MAX_DESCRIPTION,
                    ),
                    short_cut: plugin["shortCut"]
                        .as_str()
                        .map(|key| key.trim().chars().take(17).collect()),
                    scopes: plugin["scopes"]
                        .as_array()
                        .map(|scopes| {
                            scopes
                                .iter()
                                .take(17)
                                .map(|scope| text(scope, 64).trim().to_ascii_lowercase())
                                .collect()
                        })
                        .unwrap_or_default(),
                    background: plugin["background"] == true,
                    confirm: plugin["confirm"] == true,
                    dangerous: plugin["dangerous"] == true,
                    overwrite_output: plugin["overwriteOutput"] == true,
                },
                pipes: plugin.get("pipes").is_some(),
                name,
            }
        })
        .collect())
}

/// The named k9s plugin from `plugins.yaml`, validated for `register-k9s`.
pub fn k9s_plugin(text: &str, name: &str) -> Result<K9sPlugin> {
    let plugin = parse_k9s(text)?
        .into_iter()
        .find(|plugin| plugin.name == name || plugin.id == name)
        .ok_or("plugins.yaml has no plugin with that name")?;
    if let Some(reason) = blocking(&plugin).into_iter().next() {
        return Err(format!("plugin {}: {reason}", plugin.id));
    }
    Ok(plugin)
}

/// Problems that stop registration, whatever executable is chosen.
fn blocking(plugin: &K9sPlugin) -> Vec<String> {
    let mut reasons = Vec::new();
    if plugin.pipes {
        reasons.push("uses pipes; KBorg runs one executable with its own argv".into());
    }
    if plugin.command.is_empty() || plugin.command.contains('$') {
        reasons.push("command must name a program, not a variable".into());
    }
    if let Err(error) = plugins::referenced(&plugin.args) {
        reasons.push(error);
    }
    if let Err(error) = plugin.meta.validate() {
        reasons.push(error);
    }
    if plugin.args.len() > 64 || plugin.args.iter().any(|arg| arg.len() > 4096) {
        reasons.push("more than 64 arguments or an argument over 4096 bytes".into());
    }
    if !safety::valid_id(&plugin.id) {
        reasons
            .push("name cannot be a KBorg extension id (lowercase letters, digits and -)".into());
    }
    reasons
}

/// A built-in KBorg feature that covers what a common plugin does.
fn equivalent(plugin: &K9sPlugin) -> Option<&'static str> {
    let id = plugin.id.as_str();
    Some(if id.contains("debug") {
        ":debug (reviewed ephemeral debug container)"
    } else if id.contains("event") {
        ":events, or Enter/d on the object"
    } else if id.contains("value") {
        "Helm history v (stored values, redacted)"
    } else if id.contains("log") {
        "l / :logs (built-in bounded logs)"
    } else {
        return None;
    })
}

/// Finds a command on PATH for the import report only. Registration still
/// needs the absolute path named explicitly and pins its SHA-256.
pub fn resolve_on_path(command: &str) -> Option<PathBuf> {
    if command.contains('/') {
        return Path::new(command)
            .is_absolute()
            .then(|| PathBuf::from(command));
    }
    std::env::split_paths(&std::env::var_os("PATH")?).find_map(|directory| {
        let candidate = directory.join(command);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let metadata = fs::metadata(&candidate).ok()?;
            (metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
                .then(|| candidate.canonicalize().ok())
                .flatten()
        }
        #[cfg(not(unix))]
        {
            candidate.is_file().then_some(candidate)
        }
    })
}

/// Explain how each entry of a k9s `plugins.yaml` maps to KBorg. Nothing is
/// registered or run. Plugins that run one program get a `register-k9s`
/// command with the program's absolute path (found on PATH by `resolve`);
/// shell scripts and pipes are flagged for review.
pub fn import_k9s(
    text: &str,
    file: &str,
    resolve: &dyn Fn(&str) -> Option<PathBuf>,
) -> Result<String> {
    let entries = parse_k9s(text)?;
    let keymap = crate::keymap::Keymap::default();
    let mut ready = Vec::new();
    let mut skipped = Vec::new();
    for plugin in &entries {
        let mut reasons = blocking(plugin);
        let program = Path::new(&plugin.command);
        if plugins::is_shell(program) {
            reasons.insert(
                0,
                "wraps a shell script; needs review (register a dedicated executable, or see docs for pinned shells)".into(),
            );
        }
        if !reasons.is_empty() {
            let hint = equivalent(plugin)
                .map(|feature| format!("; built-in: {feature}"))
                .unwrap_or_default();
            skipped.push(format!("  {}: {}{hint}", plugin.id, reasons.join("; ")));
            continue;
        }
        let tool = resolve(&plugin.command);
        let mut notes = Vec::new();
        let command = safety::safe_text(
            program
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or(&plugin.command),
            255,
        );
        if tool.is_none() {
            notes.push(format!(
                "{command} was not found on PATH; install it or pass its absolute path"
            ));
        }
        let catalog = plugins::Catalog::new(
            vec![(plugin.id.clone(), plugin.meta.clone())],
            &keymap,
            &Vec::new(),
        );
        notes.extend(catalog.notices.iter().map(|notice| {
            notice
                .split_once(": ")
                .map_or(notice.clone(), |(_, rest)| rest.to_owned())
        }));
        if plugin.meta.background {
            notes.push("background: KBorg runs it in the foreground and shows the output".into());
        }
        if plugin.meta.dangerous {
            notes.push(format!(
                "dangerous: every run needs {DANGEROUS} {}",
                plugin.id
            ));
        }
        let variables = plugins::referenced(&plugin.args)
            .unwrap_or_default()
            .into_iter()
            .map(|name| format!("${name}"))
            .collect::<Vec<_>>();
        let mut line = format!(
            "  {} [{}{}]{}\n    kborg extensions register-k9s {} {} --tool {}\n",
            plugin.id,
            if plugin.meta.scopes.is_empty() {
                "all".into()
            } else {
                plugin.meta.scopes.join(",")
            },
            catalog
                .bindings
                .first()
                .map(|binding| format!("; {}", binding.key))
                .unwrap_or_default(),
            if plugin.meta.description.is_empty() {
                String::new()
            } else {
                format!(" {}", plugin.meta.description)
            },
            safety::safe_text(file, 4096),
            plugin.id,
            tool.map_or_else(
                || format!("/ABSOLUTE/PATH/TO/{command}"),
                |path| safety::safe_text(&path.to_string_lossy(), 4096)
            ),
        );
        if !variables.is_empty() {
            line.push_str(&format!("    variables: {}\n", variables.join(" ")));
        }
        for note in notes {
            line.push_str(&format!("    note: {note}\n"));
        }
        ready.push(line);
    }
    let mut out = format!(
        "k9s plugins: {} total, {} can be registered, {} need changes or review\n",
        ready.len() + skipped.len(),
        ready.len(),
        skipped.len()
    );
    if !ready.is_empty() {
        out.push_str("\nRegister each (every registration shows its own review and pins the executable's SHA-256):\n");
        for line in &ready {
            out.push_str(line);
        }
    }
    if !skipped.is_empty() {
        out.push_str("\nNot importable as-is:\n");
        for line in &skipped {
            out.push_str(line);
            out.push('\n');
        }
    }
    out.push_str("\nRegistered plugins run with :ext NAME or their shortCut (shown in ? help) and need --allow-extensions --allow-tools. Variables expand inside each argument, never through a shell; every run shows the exact argv and the redacted object sent on stdin, and its output is shown, never applied.\n");
    Ok(out)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    const COMMUNITY: &str = include_str!("../tests/fixtures/k9s/community-plugins.yaml");
    fn fake_path(command: &str) -> Option<PathBuf> {
        (command != "dive").then(|| PathBuf::from(format!("/opt/tools/bin/{command}")))
    }

    #[test]
    fn k9s_community_plugins_are_classified_without_registering_anything() -> Result<()> {
        let report = import_k9s(COMMUNITY, "plugins.yaml", &fake_path)?;
        assert!(
            report
                .starts_with("k9s plugins: 7 total, 4 can be registered, 3 need changes or review"),
            "{report}"
        );
        for line in [
            "kborg extensions register-k9s plugins.yaml stern --tool /opt/tools/bin/stern",
            "kborg extensions register-k9s plugins.yaml log-full --tool /opt/tools/bin/kubectl",
            "kborg extensions register-k9s plugins.yaml helm-values --tool /opt/tools/bin/helm",
            "kborg extensions register-k9s plugins.yaml dive --tool /ABSOLUTE/PATH/TO/dive",
            "stern [pods; Ctrl-L] Logs <Stern>",
            "variables: $CONTEXT $FILTER $NAMESPACE",
            "dive was not found on PATH",
            "d is taken by containers.describe in the containers view; use :ext dive",
            "v is taken by helm.values in the helm view; use :ext helm-values",
            "debug: wraps a shell script; needs review",
            "built-in: :debug",
            "watch-events: wraps a shell script",
            "blame: wraps a shell script",
        ] {
            assert!(report.contains(line), "missing {line:?} in\n{report}");
        }
        assert!(
            import_k9s(
                "plugins:\n  a: &x {command: /x}\n  b: *x\n",
                "p",
                &fake_path
            )
            .is_err()
        );
        let blocked = "plugins:\n  env:\n    command: /usr/bin/env\n    args: [$HOME]\n  piped:\n    command: /usr/bin/jq\n    pipes: [x]\n";
        let unknown = import_k9s(blocked, "p", &fake_path)?;
        assert!(unknown.contains("unknown plugin variable $HOME"));
        assert!(unknown.contains("uses pipes"));
        // register-k9s refuses what the importer flags as blocked.
        assert!(k9s_plugin(COMMUNITY, "stern").is_ok());
        assert!(k9s_plugin(blocked, "env").is_err());
        assert!(k9s_plugin(blocked, "piped").is_err());
        assert!(k9s_plugin(COMMUNITY, "missing").is_err());
        Ok(())
    }

    #[test]
    fn registered_community_plugins_expand_exact_argv_and_refuse_injection() -> Result<()> {
        let directory = Temporary::new()?;
        let executable = native()?;
        let mut values = plugins::Variables::for_object(
            &json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"api-7d9","namespace":"shop"}}),
            "pods",
            "docker-desktop",
        );
        values.set("FILTER", "api");
        values.set("KUBECONFIG", "/Users/me/.kube/config");
        for (name, expected) in [
            (
                "stern",
                vec![
                    "--tail",
                    "50",
                    "api",
                    "-n",
                    "shop",
                    "--context",
                    "docker-desktop",
                ],
            ),
            (
                "log-full",
                vec![
                    "logs",
                    "api-7d9",
                    "-n",
                    "shop",
                    "--context",
                    "docker-desktop",
                    "--kubeconfig",
                    "/Users/me/.kube/config",
                ],
            ),
        ] {
            let plugin = k9s_plugin(COMMUNITY, name)?;
            let registration =
                prepare_plugin_registration(&plugin.id, &executable, &plugin.args, plugin.meta)?;
            assert!(registration.summary().contains("REGISTER K9S PLUGIN"));
            assert!(registration.summary().contains("never through a shell"));
            registration.save(&directory.0, CONFIRMATION)?;
            let entry = load_from(&directory.0, name)?;
            let prepared = entry.prepare_with(
                &json!({"metadata":{"name":"api-7d9"}}),
                handoff::Format::Json,
                &values,
            )?;
            assert_eq!(prepared.argv(), expected.as_slice());
            assert_eq!(prepared.confirmation(), handoff::CONFIRMATION);
            let review = argv_review(&entry, &prepared);
            assert!(review.contains("[1] \"--tail\"") || review.contains("[1] \"logs\""));
            assert!(review.contains("passed without a shell"));
        }
        assert_eq!(list_in(&directory.0)?.len(), 2);
        assert!(
            list_in(&directory.0)?
                .iter()
                .all(|entry| entry.plugin.is_some())
        );
        let stern = load_from(&directory.0, "stern")?;
        // A hostile name or filter never reaches argv as an option or a control.
        for hostile in ["--exec=/bin/sh", "-n", "a\nb", "x\u{1b}]52;c;AAAA\u{7}"] {
            let mut bad = values.clone();
            bad.set("FILTER", hostile);
            assert!(
                stern
                    .prepare_with(&json!({}), handoff::Format::Json, &bad)
                    .is_err(),
                "{hostile:?}"
            );
        }
        // `$(...)` stays literal text inside one argument.
        let mut literal = values.clone();
        literal.set("FILTER", "$(touch SHOULD_NOT_EXIST)");
        let prepared = stern.prepare_with(&json!({}), handoff::Format::Json, &literal)?;
        assert_eq!(prepared.argv()[2], "$(touch SHOULD_NOT_EXIST)");
        let outcome = handoff::run(prepared, handoff::CONFIRMATION, true)?;
        assert!(outcome.stdout.contains("$(touch SHOULD_NOT_EXIST)"));
        assert!(!directory.0.join("SHOULD_NOT_EXIST").exists());

        // dangerous: true needs RUN DANGEROUS NAME, and RUN TOOL is refused.
        let mut meta = k9s_plugin(COMMUNITY, "dive")?.meta;
        meta.dangerous = true;
        prepare_plugin_registration("risky", &executable, &["$COL-IMAGE".into()], meta)?
            .save(&directory.0, CONFIRMATION)?;
        let risky = load_from(&directory.0, "risky")?;
        let mut image = values.clone();
        image.column("IMAGE", "nginx:1.27");
        let prepared = risky.prepare_with(&json!({}), handoff::Format::Json, &image)?;
        assert_eq!(prepared.confirmation(), "RUN DANGEROUS risky");
        assert!(
            argv_review(&risky, &prepared).contains("DANGEROUS: the plugin is marked dangerous")
        );
        assert!(
            prepared
                .summary()
                .contains("Type RUN DANGEROUS risky to run")
        );
        assert!(handoff::run(prepared, handoff::CONFIRMATION, true).is_err());
        let prepared = risky.prepare_with(&json!({}), handoff::Format::Json, &image)?;
        assert!(handoff::run(prepared, "RUN DANGEROUS risky", true)?.success);
        // Unknown variables never register; schema 2 without plugin is invalid.
        assert!(
            prepare_plugin_registration(
                "bad",
                &executable,
                &["$HOME".into()],
                plugins::Meta::default()
            )
            .is_err()
        );
        let mut manifest = prepare_registration("plain", &executable, &[])?.manifest;
        manifest.schema = 2;
        assert!(validate_manifest(&manifest).is_err());
        Ok(())
    }
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
