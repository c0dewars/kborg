//! Audit sink for company SIEMs: every change KBorg makes writes one JSON line
//! for its intent (after approval, before the change) and one for its outcome.
//!
//! Sinks come from the operator config: an append-only, private, size-capped
//! file whose records are hash chained (`prev` is the SHA-256 of the previous
//! line), and/or local syslog. Records carry identities, never values: no
//! Secret data, tokens, exec input/output or log contents, and errors are
//! reduced to a fixed class.
use crate::{Result, safety};
use serde_json::{Map, Value, json};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

/// `prev` of the first record ever written to a chain.
pub const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
/// Default rotation size of `audit_file`.
pub const DEFAULT_MAX_BYTES: u64 = 16 * 1024 * 1024;
/// Smallest accepted `audit_file_max_bytes`.
pub const MIN_MAX_BYTES: u64 = 64 * 1024;
/// Rotated files kept next to `audit_file` (`FILE.1` is the newest).
pub const KEEP: u32 = 5;
const MAX_LINE: usize = 8 * 1024;
const FIELD: usize = 253;

/// Where audit records go. Empty means no sink (the local spool still runs).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Settings {
    pub file: Option<PathBuf>,
    pub max_bytes: u64,
    pub syslog: bool,
    pub required: bool,
    /// Test hook: a datagram socket used instead of the system syslog socket.
    pub syslog_socket: Option<PathBuf>,
}
impl Settings {
    fn active(&self) -> bool {
        self.file.is_some() || self.syslog
    }
    /// Checks operator-supplied values; called when the config is loaded.
    pub fn validate(&self) -> Result<()> {
        if let Some(file) = &self.file {
            if !file.is_absolute() {
                return Err("audit_file must be an absolute path. Next: set audit_file to a full path such as /var/log/kborg/audit.jsonl".into());
            }
            if file.file_name().is_none() {
                return Err("audit_file must name a file, not a directory".into());
            }
        }
        if self.max_bytes != 0 && self.max_bytes < MIN_MAX_BYTES {
            return Err(format!(
                "audit_file_max_bytes must be at least {MIN_MAX_BYTES}"
            ));
        }
        if self.syslog && !cfg!(unix) {
            return Err("audit_syslog needs a Unix syslog socket; it is not available on this platform. Next: use audit_file".into());
        }
        if self.required && !self.active() {
            return Err("audit_required needs audit_file or audit_syslog. Next: set one of them, or remove audit_required".into());
        }
        Ok(())
    }
}

static SETTINGS: RwLock<Option<Settings>> = RwLock::new(None);
/// Confirmation mode recorded in each record. CLI confirmations are typed, so
/// the default is strict; the TUI sets one-key unless strict mode is on.
static STRICT: AtomicBool = AtomicBool::new(true);
static WARNING: Mutex<Option<String>> = Mutex::new(None);
static SEQUENCE: AtomicU64 = AtomicU64::new(0);
/// Serializes chain appends within this process (a file lock covers other
/// processes) and holds the last record hash per syslog destination (`None`
/// is the system socket) for the syslog-only chain.
static APPEND: Mutex<Vec<(Option<PathBuf>, String)>> = Mutex::new(Vec::new());

#[cfg(test)]
thread_local! {
    static OVERRIDE: std::cell::RefCell<Option<Settings>> = const { std::cell::RefCell::new(None) };
}
/// Per-thread sink for tests, so parallel tests never share a file.
#[cfg(test)]
pub fn set_thread_settings(settings: Option<Settings>) {
    OVERRIDE.with(|cell| *cell.borrow_mut() = settings);
}

pub fn configure(settings: Settings) {
    if let Ok(mut slot) = SETTINGS.write() {
        *slot = settings.active().then_some(settings);
    }
}
pub fn set_strict(strict: bool) {
    STRICT.store(strict, Ordering::Relaxed);
}
fn settings() -> Option<Settings> {
    #[cfg(test)]
    {
        if let Some(settings) = OVERRIDE.with(|cell| cell.borrow().clone()) {
            return settings.active().then_some(settings);
        }
    }
    SETTINGS.read().ok().and_then(|slot| slot.clone())
}
/// A warning about a record that could not be written (audit not required).
pub fn take_warning() -> Option<String> {
    WARNING.lock().ok().and_then(|mut slot| slot.take())
}
fn warn(text: String) {
    if let Ok(mut slot) = WARNING.lock() {
        *slot = Some(text);
    }
}

/// One change, described by identities only.
#[derive(Clone, Debug, Default)]
pub struct Event {
    pub action: String,
    pub context: String,
    pub server_host: String,
    pub namespace: String,
    pub kind: String,
    pub name: String,
    pub uid: String,
    pub review_sha256: String,
}
impl Event {
    /// `server` may be a URL; only its host and port are kept.
    pub fn new(action: &str, context: &str, server: &str) -> Self {
        Self {
            action: action.into(),
            context: context.into(),
            server_host: server_host(server),
            ..Self::default()
        }
    }
    pub fn target(mut self, namespace: &str, kind: &str, name: &str, uid: &str) -> Self {
        self.namespace = namespace.into();
        self.kind = kind.into();
        self.name = name.into();
        self.uid = uid.into();
        self
    }
    /// Records the SHA-256 of the exact review text; the text itself is never kept.
    pub fn review(mut self, text: &str) -> Self {
        self.review_sha256 = sha256(text.as_bytes());
        self
    }
}

pub fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Host (and port) of an API server URL, without scheme, credentials or path.
pub fn server_host(server: &str) -> String {
    let rest = server.split_once("://").map_or(server, |(_, rest)| rest);
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    safety::safe_text(host, FIELD)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    Ok,
    Failed,
    Unknown,
}
impl Outcome {
    fn label(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failed => "failed",
            Self::Unknown => "unknown",
        }
    }
}

/// A fixed error class; the error text itself never reaches the record.
pub fn error_class(error: &str) -> &'static str {
    let lower = error.to_ascii_lowercase();
    let any = |words: &[&str]| words.iter().any(|word| lower.contains(word));
    if any(&["timed out", "timeout", "deadline"]) {
        "timeout"
    } else if any(&["cancel"]) {
        "canceled"
    } else if any(&["forbidden", "unauthorized", "rbac", "permission"]) {
        "forbidden"
    } else if any(&["not found"]) {
        "not-found"
    } else if any(&["conflict", "changed since", "resourceversion", "replaced"]) {
        "conflict"
    } else if any(&["expired", "confirmation", "approval", "review"]) {
        "review-rejected"
    } else if any(&["audit"]) {
        "audit"
    } else if any(&["connect", "network", "unreachable"]) {
        "connection"
    } else {
        "error"
    }
}
fn outcome_of(error: &str) -> Outcome {
    let lower = error.to_ascii_lowercase();
    if lower.contains("unknown") || lower.contains("timed out") || lower.contains("timeout") {
        Outcome::Unknown
    } else {
        Outcome::Failed
    }
}

/// An intent that was recorded; its outcome must follow.
#[must_use = "record the outcome with finish or outcome"]
pub struct Pending {
    id: String,
    event: Option<Event>,
    required: bool,
}
impl Pending {
    /// Records the outcome of `result`. Errors only if audit is required.
    pub fn finish<T>(self, result: &Result<T>) -> Result<()> {
        match result {
            Ok(_) => self.outcome(Outcome::Ok, None),
            Err(error) => self.outcome(outcome_of(error), Some(error_class(error))),
        }
    }
    pub fn outcome(self, outcome: Outcome, error_class: Option<&str>) -> Result<()> {
        let Some(event) = self.event else {
            return Ok(());
        };
        let mut fields = Map::new();
        fields.insert("outcome".into(), json!(outcome.label()));
        fields.insert("error_class".into(), json!(error_class));
        match emit(&self.id, "outcome", &event, fields) {
            Ok(()) => Ok(()),
            Err(why) if self.required => Err(format!(
                "The change ran (outcome: {}) but its audit outcome record was not written: {why}. Next: fix the audit sink (audit_file permissions or disk space, syslog), then inspect the change in the cluster.",
                outcome.label()
            )),
            Err(why) => {
                warn(format!(
                    "Audit outcome record not written: {why}. Next: fix audit_file or audit_syslog in the operator config."
                ));
                Ok(())
            }
        }
    }
}

/// Records the intent of an approved change before it is sent. When audit is
/// required and the record cannot be written, the change must not run.
pub fn intent(event: Event) -> Result<Pending> {
    let Some(settings) = settings() else {
        return Ok(Pending {
            id: String::new(),
            event: None,
            required: false,
        });
    };
    let id = format!(
        "{}-{}-{}",
        unix_nanos(),
        std::process::id(),
        SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    match emit_with(&settings, &id, "intent", &event, Map::new()) {
        Ok(()) => Ok(Pending {
            id,
            event: Some(event),
            required: settings.required,
        }),
        Err(why) if settings.required => Err(format!(
            "Change blocked: the audit record could not be written ({why}). The operator config requires an audit record for every change (audit_required), so nothing was sent. Next: fix audit_file (path, permissions, disk space) or audit_syslog, then try again."
        )),
        Err(why) => {
            warn(format!(
                "Audit record not written: {why}. The change continues because audit_required is off. Next: fix audit_file or audit_syslog in the operator config."
            ));
            Ok(Pending {
                id,
                event: None,
                required: false,
            })
        }
    }
}

fn emit(id: &str, stage: &str, event: &Event, extra: Map<String, Value>) -> Result<()> {
    let Some(settings) = settings() else {
        return Ok(());
    };
    emit_with(&settings, id, stage, event, extra)
}

fn emit_with(
    settings: &Settings,
    id: &str,
    stage: &str,
    event: &Event,
    extra: Map<String, Value>,
) -> Result<()> {
    let mut record = Map::new();
    let text = |value: &str| json!(safety::safe_text(value, FIELD));
    record.insert("schema".into(), json!("kborg.audit/1"));
    record.insert("ts".into(), json!(rfc3339(unix_nanos())));
    record.insert("id".into(), text(id));
    record.insert("stage".into(), json!(stage));
    record.insert("kborg_version".into(), json!(env!("CARGO_PKG_VERSION")));
    record.insert("os_user".into(), text(&os_user()));
    record.insert("context".into(), text(&event.context));
    record.insert("server_host".into(), text(&event.server_host));
    record.insert("namespace".into(), text(&event.namespace));
    record.insert("kind".into(), text(&event.kind));
    record.insert("name".into(), text(&event.name));
    record.insert("uid".into(), text(&event.uid));
    record.insert("action".into(), text(&event.action));
    record.insert("review_sha256".into(), text(&event.review_sha256));
    record.insert(
        "confirmation".into(),
        json!(if STRICT.load(Ordering::Relaxed) {
            "strict"
        } else {
            "one-key"
        }),
    );
    let warning = stage == "outcome" && extra.get("outcome") != Some(&json!("ok"));
    record.extend(extra);
    let mut chains = APPEND.lock().map_err(|_| "audit writer unavailable")?;
    let key = &settings.syslog_socket;
    let line = match &settings.file {
        Some(path) => append_file(path, settings.max_bytes, record)?,
        None => {
            let prev = chains
                .iter()
                .find(|(socket, _)| socket == key)
                .map_or(GENESIS, |(_, hash)| hash.as_str())
                .to_owned();
            encode(record, &prev)?
        }
    };
    let hash = sha256(line.as_bytes());
    match chains.iter_mut().find(|(socket, _)| socket == key) {
        Some((_, last)) => *last = hash,
        None => chains.push((key.clone(), hash)),
    }
    drop(chains);
    if settings.syslog {
        send_syslog(settings.syslog_socket.as_deref(), &line, warning)?;
    }
    Ok(())
}

fn encode(mut record: Map<String, Value>, prev: &str) -> Result<String> {
    record.insert("prev".into(), json!(prev));
    let line =
        serde_json::to_string(&Value::Object(record)).map_err(|_| "cannot encode audit record")?;
    if line.len() > MAX_LINE || line.contains('\n') {
        return Err("audit record too large".into());
    }
    Ok(line)
}

fn os_user() -> String {
    ["USER", "LOGNAME", "USERNAME"]
        .iter()
        .find_map(|name| std::env::var(name).ok().filter(|value| !value.is_empty()))
        .unwrap_or_else(|| "unknown".into())
}
fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}
/// UTC RFC 3339 timestamp with milliseconds.
pub fn rfc3339(nanos: u128) -> String {
    let millis = (nanos / 1_000_000) as u64;
    let seconds = millis / 1000;
    let days = (seconds / 86_400) as i64;
    let rest = seconds % 86_400;
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{:03}Z",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60,
        millis % 1000
    )
}

/// Appends one chained record, rotating first when the file would exceed
/// `max_bytes`. Returns the line written.
fn append_file(path: &Path, max_bytes: u64, record: Map<String, Value>) -> Result<String> {
    let max_bytes = if max_bytes == 0 {
        DEFAULT_MAX_BYTES
    } else {
        max_bytes
    };
    safety::checked_path(path)?;
    let parent = path.parent().ok_or("audit_file has no directory")?;
    if !parent.is_dir() {
        return Err("audit_file directory does not exist".into());
    }
    let mut file = open_locked(path)?;
    let size = file
        .metadata()
        .map_err(|_| "cannot inspect audit_file")?
        .len();
    let prev = last_line_hash(&mut file, size)?;
    let line = encode(record, &prev)?;
    if size > 0 && size.saturating_add(line.len() as u64 + 1) > max_bytes {
        // Windows cannot rename an open file; Unix keeps the lock while rotating.
        #[cfg(not(unix))]
        drop(file);
        rotate(path)?;
        // The old handle (and its lock) now names the rotated file.
        file = open_locked(path)?;
    }
    file.write_all(format!("{line}\n").as_bytes())
        .and_then(|()| file.sync_data())
        .map_err(|_| "cannot write audit_file (disk full or read-only?)")?;
    Ok(line)
}

fn open_locked(path: &Path) -> Result<std::fs::File> {
    for _ in 0..8 {
        let mut options = std::fs::OpenOptions::new();
        options.read(true).append(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(path)
            .map_err(|_| "cannot open audit_file (check the path and permissions)")?;
        let meta = file.metadata().map_err(|_| "cannot inspect audit_file")?;
        if !meta.is_file() {
            return Err("audit_file is not a regular file".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if meta.permissions().mode() & 0o077 != 0 {
                return Err("audit_file is readable by other users; run chmod 600 on it".into());
            }
        }
        file.lock().map_err(|_| "cannot lock audit_file")?;
        // Another KBorg may have rotated the file while this one waited.
        if same_file(&file, path) {
            return Ok(file);
        }
    }
    Err("audit_file keeps changing; another process is rotating it".into())
}
#[cfg(unix)]
fn same_file(file: &std::fs::File, path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    match (file.metadata(), std::fs::symlink_metadata(path)) {
        (Ok(open), Ok(named)) => open.dev() == named.dev() && open.ino() == named.ino(),
        _ => false,
    }
}
#[cfg(not(unix))]
fn same_file(_: &std::fs::File, path: &Path) -> bool {
    path.is_file()
}

/// SHA-256 of the last complete line, or the genesis value for an empty file.
fn last_line_hash(file: &mut std::fs::File, size: u64) -> Result<String> {
    if size == 0 {
        return Ok(GENESIS.into());
    }
    let start = size.saturating_sub(2 * MAX_LINE as u64);
    file.seek(SeekFrom::Start(start))
        .map_err(|_| "cannot read audit_file")?;
    let mut tail = Vec::new();
    file.take(2 * MAX_LINE as u64)
        .read_to_end(&mut tail)
        .map_err(|_| "cannot read audit_file")?;
    if tail.last() != Some(&b'\n') {
        return Err("audit_file ends with a partial record; check it with kborg audit verify, then move it aside".into());
    }
    let body = &tail[..tail.len() - 1];
    let begin = body
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    if begin == 0 && start > 0 {
        return Err("audit_file last record is too long".into());
    }
    Ok(sha256(&body[begin..]))
}

fn rotate(path: &Path) -> Result<()> {
    let numbered = |n: u32| {
        let mut name = path.as_os_str().to_owned();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    };
    let oldest = numbered(KEEP);
    if oldest.exists() {
        std::fs::remove_file(&oldest).map_err(|_| "cannot remove the oldest rotated audit file")?;
    }
    for n in (1..KEEP).rev() {
        let from = numbered(n);
        if from.exists() {
            std::fs::rename(&from, numbered(n + 1)).map_err(|_| "cannot rotate audit_file")?;
        }
    }
    std::fs::rename(path, numbered(1)).map_err(|_| "cannot rotate audit_file".into())
}

/// Sends one record to the local syslog daemon (facility auth).
#[cfg(unix)]
fn send_syslog(socket: Option<&Path>, line: &str, warning: bool) -> Result<()> {
    use std::os::unix::net::UnixDatagram;
    // auth (4) * 8 + notice (5), or warning (4) for a failed or unknown outcome.
    let priority = if warning { 36 } else { 37 };
    let message = format!("<{priority}>kborg[{}]: {line}", std::process::id());
    let sender = UnixDatagram::unbound().map_err(|_| "cannot open a syslog socket")?;
    let candidates: Vec<&Path> = match socket {
        Some(path) => vec![path],
        None => vec![
            Path::new("/dev/log"),
            Path::new("/var/run/syslog"),
            Path::new("/var/run/log"),
        ],
    };
    for path in candidates {
        if sender.send_to(message.as_bytes(), path).is_ok() {
            return Ok(());
        }
    }
    Err("local syslog is not accepting messages (/dev/log, /var/run/syslog)".into())
}
#[cfg(not(unix))]
fn send_syslog(_: Option<&Path>, _: &str, _: bool) -> Result<()> {
    Err("syslog is not available on this platform".into())
}

/// Result of checking one or more audit files in order (oldest first).
#[derive(Debug)]
pub struct Verified {
    pub records: usize,
    pub first_prev: String,
    pub last_hash: String,
}

/// Checks the hash chain across `files` (oldest first): every record must be
/// valid JSON whose `prev` is the SHA-256 of the line before it. A removed,
/// inserted, reordered or edited line breaks the chain.
pub fn verify(files: &[PathBuf]) -> Result<Verified> {
    if files.is_empty() {
        return Err("usage: audit verify FILE [FILE...] (oldest first)".into());
    }
    let mut previous: Option<String> = None;
    let mut first_prev = String::new();
    let mut records = 0usize;
    for path in files {
        safety::checked_path(path)?;
        let file = std::fs::File::open(path).map_err(|_| {
            format!(
                "cannot open {}",
                safety::safe_text(&path.to_string_lossy(), 512)
            )
        })?;
        let label = safety::safe_text(&path.to_string_lossy(), 512);
        let mut reader = std::io::BufReader::new(file);
        let mut number = 0usize;
        loop {
            let mut raw = Vec::new();
            let read = std::io::BufRead::read_until(&mut reader, b'\n', &mut raw)
                .map_err(|_| format!("{label}: read failed"))?;
            if read == 0 {
                break;
            }
            number += 1;
            if raw.last() != Some(&b'\n') {
                return Err(format!(
                    "{label}:{number}: partial record (no newline); the file was truncated or is being written"
                ));
            }
            raw.pop();
            if raw.len() > MAX_LINE {
                return Err(format!("{label}:{number}: record too long"));
            }
            let line =
                std::str::from_utf8(&raw).map_err(|_| format!("{label}:{number}: not UTF-8"))?;
            let value: Value = serde_json::from_str(line)
                .map_err(|_| format!("{label}:{number}: not a JSON record (edited?)"))?;
            if value["schema"] != "kborg.audit/1" {
                return Err(format!("{label}:{number}: not a KBorg audit record"));
            }
            let prev = value["prev"]
                .as_str()
                .ok_or_else(|| format!("{label}:{number}: missing prev"))?;
            match &previous {
                Some(expected) if expected != prev => {
                    return Err(format!(
                        "{label}:{number}: CHAIN BROKEN: prev does not match the record before it. A record was removed, inserted or edited here."
                    ));
                }
                Some(_) => {}
                None => first_prev = prev.to_owned(),
            }
            previous = Some(sha256(line.as_bytes()));
            records += 1;
        }
    }
    Ok(Verified {
        records,
        first_prev,
        last_hash: previous.unwrap_or_else(|| GENESIS.into()),
    })
}

/// Test sink: a private temporary audit file for the current thread.
#[cfg(test)]
pub(crate) struct Capture {
    pub dir: PathBuf,
    pub file: PathBuf,
}
#[cfg(test)]
impl Capture {
    pub fn new() -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let dir = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "temp dir")?
            .join(format!(
                "kborg-audit-sink-{}-{}-{}",
                std::process::id(),
                unix_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        safety::ensure_private_dir(&dir)?;
        let file = dir.join("audit.jsonl");
        set_thread_settings(Some(Settings {
            file: Some(file.clone()),
            ..Settings::default()
        }));
        Ok(Self { dir, file })
    }
    pub fn raw(&self) -> String {
        std::fs::read_to_string(&self.file).unwrap_or_default()
    }
    pub fn records(&self) -> Vec<Value> {
        self.raw()
            .lines()
            .filter_map(|line| serde_json::from_str(line).ok())
            .collect()
    }
}
#[cfg(test)]
impl Drop for Capture {
    fn drop(&mut self) {
        set_thread_settings(None);
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests;
