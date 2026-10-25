//! Reviewed, bounded, read-only external views. This is supervision, not an OS sandbox.
//!
//! Tools receive only the redacted view on stdin. Captured output cannot control the
//! terminal, and it is never applied to Kubernetes. Interactive editors, privileged
//! tools and generic extension registration are deliberately outside this contract.
use crate::{Result, safety};
use serde_json::Value;
use std::{
    fs,
    hash::{DefaultHasher, Hash, Hasher},
    io::Read,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};
#[cfg(unix)]
use std::{
    io::Write,
    process::{Command, Stdio},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc,
    },
    thread,
    time::{SystemTime, UNIX_EPOCH},
};

pub const CONFIRMATION: &str = "RUN TOOL";
const MAX_INPUT: usize = 256 * 1024;
const MAX_OUTPUT: usize = 256 * 1024;
const MAX_EXECUTABLE: u64 = 128 * 1024 * 1024;
const REVIEW_LIFETIME: Duration = Duration::from_secs(60);
const PROCESS_BUDGET: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug)]
pub enum Format {
    Json,
    Yaml,
}

/// Private fields seal the executable, arguments and exact transfer after review.
pub struct Prepared {
    executable: PathBuf,
    argv: Vec<String>,
    executable_bytes: Arc<[u8]>,
    metadata: fs::Metadata,
    identifier: u64,
    input: Vec<u8>,
    prepared_at: Instant,
}

impl Prepared {
    pub fn summary(&self) -> String {
        let argv = self
            .argv
            .iter()
            .map(|arg| format!("{arg:?}"))
            .collect::<Vec<_>>()
            .join(" ");
        format!(
            "READ-ONLY TOOL VIEW\nExecutable: {}\nArguments: {argv}\nExecutable change identifier: {:016x} ({} bytes; not a publisher signature)\nTransfer: {} bytes of reviewed, redacted text on stdin\nLimits: 15 seconds; 256 KiB combined stdout/stderr\nEnvironment supplied by KBorg: private HOME/USERPROFILE/TMPDIR and C locale; private working directory\nOutput: captured and sanitized; never applied to Kubernetes\nThe trusted tool retains your OS user's filesystem/network permissions. Known-field redaction cannot identify every confidential CRD field. Inspect the complete input below before sharing it.\nApproval expires after 60 seconds. Type {CONFIRMATION} to run.\n",
            safety::safe_text(&self.executable.to_string_lossy(), 4096),
            self.identifier,
            self.executable_bytes.len(),
            self.input.len()
        )
    }

    /// The exact immutable transfer, already sanitized and redacted by `prepare`.
    pub fn input_preview(&self) -> &str {
        // All inputs originate from UTF-8 serializers; retain a safe fallback.
        std::str::from_utf8(&self.input).unwrap_or("[invalid transfer]")
    }
}

#[derive(Debug)]
pub struct Outcome {
    pub stdout: String,
    pub stderr: String,
    pub success: bool,
    pub status: String,
}

pub fn prepare(
    executable: &Path,
    argv: &[String],
    object: &Value,
    format: Format,
) -> Result<Prepared> {
    if !cfg!(unix) {
        return Err("external tool views require verified process-group cleanup; unsupported on this platform".into());
    }
    if !executable.is_absolute() {
        return Err(
            "--tool must name an absolute trusted executable; PATH lookup is disabled".into(),
        );
    }
    let display_path = executable
        .to_str()
        .ok_or("tool executable path must be UTF-8 for exact review")?;
    if display_path.len() > 4096 || safety::safe_text(display_path, 4096) != display_path {
        return Err("tool executable path exceeds limits or contains terminal controls".into());
    }
    if argv.len() > 64
        || argv.iter().map(String::len).sum::<usize>() > 16 * 1024
        || argv
            .iter()
            .any(|arg| arg.contains('\0') || safety::safe_text(arg, 4096) != *arg)
    {
        return Err("tool arguments exceed limits or contain terminal controls".into());
    }
    safety::checked_path(executable)?;
    let metadata = executable_metadata(executable)?;
    let bytes = safety::read_small(executable, MAX_EXECUTABLE)?;
    if bytes.starts_with(b"#!") {
        return Err(
            "tool views require a native executable; interpreter scripts are excluded".into(),
        );
    }
    if !same_metadata(&metadata, &executable_metadata(executable)?) {
        return Err("tool executable changed during review preparation".into());
    }
    let mut hasher = DefaultHasher::new();
    bytes.hash(&mut hasher);
    let input = project(object, format)?;
    Ok(Prepared {
        executable: executable.into(),
        argv: argv.to_vec(),
        executable_bytes: bytes.into(),
        metadata,
        identifier: hasher.finish(),
        input,
        prepared_at: Instant::now(),
    })
}

fn preflight(value: &Value, depth: usize, bytes: &mut usize, nodes: &mut usize) -> Result<()> {
    *nodes = nodes.saturating_add(1);
    *bytes = bytes.saturating_add(match value {
        Value::String(s) => s.len(),
        _ => 8,
    });
    if depth > 64 || *nodes > 16_384 || *bytes > MAX_INPUT {
        return Err("tool input exceeds 64 levels, 16384 values or 256 KiB".into());
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                *bytes = bytes.saturating_add(key.len());
                preflight(child, depth + 1, bytes, nodes)?;
            }
        }
        Value::Array(items) => {
            for child in items {
                preflight(child, depth + 1, bytes, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn project(object: &Value, format: Format) -> Result<Vec<u8>> {
    if object["kind"]
        .as_str()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("secret"))
    {
        return Err("Secrets cannot be sent to external tools".into());
    }
    preflight(object, 0, &mut 0, &mut 0)?;
    let mut projected = object.clone();
    safety::redact(&mut projected);
    let text = match format {
        Format::Json => {
            serde_json::to_string_pretty(&projected).map_err(|_| "cannot encode tool input")?
        }
        Format::Yaml => crate::inspect::yaml(&projected)?,
    };
    // Keys can contain bidi controls; JSON-escaped controls stay valid JSON/YAML.
    let mut escaped = String::with_capacity(text.len());
    for c in text.chars() {
        if safety::safe_text(&c.to_string(), 1).is_empty() && c != '\n' {
            escaped.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            escaped.push(c);
        }
        if escaped.len() > MAX_INPUT {
            return Err("formatted tool input exceeds 256 KiB".into());
        }
    }
    escaped.push('\n');
    if escaped.len() > MAX_INPUT {
        return Err("formatted tool input exceeds 256 KiB".into());
    }
    Ok(escaped.into_bytes())
}

fn executable_metadata(path: &Path) -> Result<fs::Metadata> {
    let metadata = trusted_metadata(path, MAX_EXECUTABLE)?;
    Ok(metadata)
}

/// A nonempty, non-symlink regular executable that is not group/world
/// writable. Shared by every explicitly trusted external tool.
pub(crate) fn trusted_metadata(path: &Path, max_bytes: u64) -> Result<fs::Metadata> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "cannot inspect tool executable")?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > max_bytes {
        return Err(format!(
            "tool must be a nonempty regular executable of at most {} MiB",
            max_bytes / (1024 * 1024)
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = metadata.permissions().mode();
        if mode & 0o111 == 0 || mode & 0o022 != 0 {
            return Err("tool must be executable and cannot be group/world writable".into());
        }
    }
    Ok(metadata)
}

pub(crate) fn same_metadata(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    let basic = a.len() == b.len() && a.modified().ok() == b.modified().ok();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        basic && a.dev() == b.dev() && a.ino() == b.ino() && a.mode() == b.mode()
    }
    #[cfg(not(unix))]
    {
        basic
    }
}

fn revalidate(prepared: &Prepared) -> Result<()> {
    safety::checked_path(&prepared.executable)?;
    if !same_metadata(
        &prepared.metadata,
        &executable_metadata(&prepared.executable)?,
    ) {
        return Err("tool executable changed; prepare a new review".into());
    }
    let mut file =
        fs::File::open(&prepared.executable).map_err(|_| "cannot reopen tool executable")?;
    let mut offset = 0;
    let mut buffer = [0u8; 16 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|_| "cannot recheck tool executable")?;
        if read == 0 {
            break;
        }
        if prepared.executable_bytes.get(offset..offset + read) != Some(&buffer[..read]) {
            return Err("tool executable contents changed; prepare a new review".into());
        }
        offset += read;
    }
    if offset != prepared.executable_bytes.len()
        || !same_metadata(
            &prepared.metadata,
            &file
                .metadata()
                .map_err(|_| "cannot recheck tool executable")?,
        )
    {
        return Err("tool executable changed; prepare a new review".into());
    }
    Ok(())
}

/// Consumes the review once. Callers must present `summary` and the full transfer
/// before collecting approval, and leave/restore any TUI terminal scope themselves.
pub fn run(prepared: Prepared, confirmation: &str, allowed: bool) -> Result<Outcome> {
    run_bounded(prepared, confirmation, allowed, PROCESS_BUDGET, MAX_OUTPUT)
}

fn run_bounded(
    prepared: Prepared,
    confirmation: &str,
    allowed: bool,
    budget: Duration,
    limit: usize,
) -> Result<Outcome> {
    if !allowed {
        return Err("external tool views require explicit --allow-tools".into());
    }
    if confirmation != CONFIRMATION {
        return Err("tool view canceled; exact RUN TOOL approval required".into());
    }
    if prepared.prepared_at.elapsed() > REVIEW_LIFETIME {
        return Err("tool review expired; prepare a new review".into());
    }
    revalidate(&prepared)?;
    #[cfg(unix)]
    {
        supervise(prepared, budget, limit)
    }
    #[cfg(not(unix))]
    {
        let _ = (prepared, budget, limit);
        Err("external tool views require verified process-group cleanup; unsupported on this platform".into())
    }
}

/// A private (0700) temporary directory removed on drop.
#[cfg(unix)]
pub(crate) struct PrivateDirectory(PathBuf);
#[cfg(unix)]
impl PrivateDirectory {
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
    pub(crate) fn create() -> Result<Self> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "cannot locate tool temporary directory")?;
        safety::checked_path(&root)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "invalid system time")?
            .as_nanos();
        let path = root.join(format!(
            "kborg-tool-{}-{stamp}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|_| "cannot create private tool working directory")?;
        Ok(Self(path))
    }
}
#[cfg(unix)]
impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(unix)]
enum Output {
    Bytes(bool, Vec<u8>),
    Done,
    Failed,
}

#[cfg(unix)]
fn reader(
    mut stream: impl Read + Send + 'static,
    stderr: bool,
    tx: mpsc::SyncSender<Output>,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match stream.read(&mut buffer) {
                Ok(0) => {
                    let _ = tx.send(Output::Done);
                    break;
                }
                Ok(read) => {
                    if tx
                        .send(Output::Bytes(stderr, buffer[..read].to_vec()))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    let _ = tx.send(Output::Failed);
                    break;
                }
            }
        }
    })
}

#[cfg(unix)]
struct Process {
    child: std::process::Child,
    killer: PathBuf,
    cleaned: bool,
}

#[cfg(unix)]
impl Process {
    fn cleanup(&mut self) -> Result<()> {
        if self.cleaned {
            return Ok(());
        }
        // The fixed OS utility is an argv-only signal sender, never a shell. The
        // tool has its own group; signal descendants even after its leader exits.
        let kill = Command::new(&self.killer)
            .env_clear()
            .current_dir("/")
            .args(["-KILL", "--", &format!("-{}", self.child.id())])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let until = Instant::now() + Duration::from_secs(1);
        let mut group_error = None;
        match kill {
            Ok(mut kill) => loop {
                match kill.try_wait() {
                    Ok(Some(_)) => break,
                    Err(_) => {
                        group_error = Some("cannot reap process-group cleanup");
                        break;
                    }
                    Ok(None) if Instant::now() >= until => {
                        let _ = kill.kill();
                        let _ = kill.try_wait();
                        group_error = Some("tool process-group cleanup timed out");
                        break;
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(5)),
                }
            },
            Err(_) => group_error = Some("cannot signal tool process group"),
        }
        // Always terminate/reap the direct child, including signal-helper errors.
        let _ = self.child.kill();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Err(_) => {
                    group_error = Some("cannot reap tool process");
                    break;
                }
                Ok(None) if Instant::now() >= until => {
                    group_error = Some("tool process cleanup timed out");
                    break;
                }
                Ok(None) => thread::sleep(Duration::from_millis(5)),
            }
        }
        self.cleaned = true;
        group_error.map_or(Ok(()), |error| Err(error.into()))
    }
}
#[cfg(unix)]
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(unix)]
fn supervise(prepared: Prepared, budget: Duration, limit: usize) -> Result<Outcome> {
    let directory = PrivateDirectory::create()?;
    let mut command = Command::new(&prepared.executable);
    command
        .args(&prepared.argv)
        .env_clear()
        .current_dir(&directory.0)
        .env("HOME", &directory.0)
        .env("USERPROFILE", &directory.0)
        .env("TMPDIR", &directory.0)
        .env("LANG", "C")
        .env("LC_ALL", "C");
    // Recheck immediately before spawn; handle-based execution race resistance
    // requires platform APIs and remains an explicit external hardening gate.
    revalidate(&prepared)?;
    let captured = capture(command, Some(prepared.input), budget, limit, None)?;
    Ok(Outcome {
        stdout: safety::safe_lines(&String::from_utf8_lossy(&captured.stdout), MAX_OUTPUT),
        stderr: safety::safe_lines(&String::from_utf8_lossy(&captured.stderr), MAX_OUTPUT),
        success: captured.status.success(),
        status: captured.status.code().map_or_else(
            || "terminated by signal".into(),
            |code| format!("exit {code}"),
        ),
    })
}

/// Raw, unsanitized output of one supervised process. Callers sanitize.
#[cfg(unix)]
pub(crate) struct Captured {
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) status: std::process::ExitStatus,
}

/// Runs a fully configured command (argv, environment and working directory
/// already set by the caller) in its own process group with a time budget, a
/// combined output limit and optional cancellation. The whole group is killed
/// and reaped on every exit path, and output pipes held by escaped
/// descendants fail the run instead of hanging it.
#[cfg(unix)]
pub(crate) fn capture(
    mut command: Command,
    input: Option<Vec<u8>>,
    budget: Duration,
    limit: usize,
    cancel: Option<&AtomicBool>,
) -> Result<Captured> {
    use std::os::unix::process::CommandExt;
    let killer = Path::new("/bin/kill")
        .canonicalize()
        .map_err(|_| "verified OS process-group cleanup is unavailable")?;
    safety::checked_path(&killer)?;
    executable_metadata(&killer)?;
    if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return Err("canceled before the tool started".into());
    }
    command
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    let child = command
        .spawn()
        .map_err(|_| "cannot start reviewed tool executable")?;
    let mut process = Process {
        child,
        killer,
        cleaned: false,
    };
    let writer = match input {
        Some(input) => {
            let mut stdin = process.child.stdin.take().ok_or("tool stdin unavailable")?;
            thread::spawn(move || {
                let _ = stdin.write_all(&input);
            })
        }
        None => thread::spawn(|| {}),
    };
    let (tx, rx) = mpsc::sync_channel(16);
    let stdout_reader = reader(
        process
            .child
            .stdout
            .take()
            .ok_or("tool stdout unavailable")?,
        false,
        tx.clone(),
    );
    let stderr_reader = reader(
        process
            .child
            .stderr
            .take()
            .ok_or("tool stderr unavailable")?,
        true,
        tx,
    );
    let start = Instant::now();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut done = 0;
    let mut status = None;
    let mut failure = None;
    while done < 2 || status.is_none() {
        if start.elapsed() >= budget {
            failure = Some("tool exceeded its time budget");
            break;
        }
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            failure = Some("canceled; the tool process group was stopped");
            break;
        }
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(Output::Bytes(is_stderr, bytes)) => {
                if stdout.len() + stderr.len() + bytes.len() > limit {
                    failure = Some("tool exceeded its combined output limit");
                    break;
                }
                if is_stderr {
                    stderr.extend(bytes);
                } else {
                    stdout.extend(bytes);
                }
            }
            Ok(Output::Done) => done += 1,
            Ok(Output::Failed) => {
                failure = Some("cannot read tool output");
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if done < 2 {
                    failure = Some("tool output capture stopped unexpectedly");
                    break;
                }
                thread::sleep(Duration::from_millis(5));
            }
        }
        if status.is_none() {
            status = process
                .child
                .try_wait()
                .map_err(|_| "cannot inspect tool process")?;
            if status.is_some() {
                process.cleanup()?;
            }
        }
    }
    let cleanup = process.cleanup();
    // Dropping the receiver releases writers blocked by bounded channel capacity.
    drop(rx);
    let drain_until = Instant::now() + Duration::from_millis(500);
    while !(writer.is_finished() && stdout_reader.is_finished() && stderr_reader.is_finished())
        && Instant::now() < drain_until
    {
        thread::sleep(Duration::from_millis(5));
    }
    let finished =
        writer.is_finished() && stdout_reader.is_finished() && stderr_reader.is_finished();
    if writer.is_finished() {
        let _ = writer.join();
    }
    if stdout_reader.is_finished() {
        let _ = stdout_reader.join();
    }
    if stderr_reader.is_finished() {
        let _ = stderr_reader.join();
    }
    cleanup?;
    if !finished {
        return Err(
            "tool retained pipe handles outside its supervised process group; view stopped".into(),
        );
    }
    if let Some(error) = failure {
        return Err(error.into());
    }
    let status = status.ok_or("tool exit status unavailable")?;
    Ok(Captured {
        stdout,
        stderr,
        status,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn input_redacts_values_and_sanitizes_terminal_and_bidi_controls() -> Result<()> {
        let object = json!({"kind":"Pod","spec":{"env":[{"value":"CANARY_PASSWORD"}]},"metadata":{"annotations":{"private":"CANARY_ANNOTATION"}},"\u{202e}key":"ok\u{1b}[31m"});
        for format in [Format::Json, Format::Yaml] {
            let bytes = project(&object, format)?;
            let text = String::from_utf8(bytes).map_err(|_| "invalid test UTF-8")?;
            assert!(!text.contains("CANARY"));
            assert!(!text.contains('\u{1b}'));
            assert!(!text.contains('\u{202e}'));
            if matches!(format, Format::Json) {
                assert!(serde_json::from_str::<Value>(&text).is_ok());
            }
        }
        assert!(project(&json!({"kind":"Secret"}), Format::Json).is_err());
        assert!(project(&json!({"large":"x".repeat(MAX_INPUT)}), Format::Json).is_err());
        Ok(())
    }

    #[cfg(unix)]
    fn fixture(object: Value) -> Result<Prepared> {
        let executable = std::env::current_exe().map_err(|_| "fixture path unavailable")?;
        prepare(
            &executable,
            &[
                "--ignored".into(),
                "--exact".into(),
                "handoff::tests::process_fixture".into(),
                "--nocapture".into(),
            ],
            &object,
            Format::Json,
        )
    }

    #[cfg(unix)]
    #[test]
    fn capability_confirmation_and_expiry_prevent_execution() -> Result<()> {
        assert!(prepare(Path::new("cat"), &[], &json!({}), Format::Json).is_err());
        let first = fixture(json!({"fixture":"copy"}))?;
        assert!(run(first, CONFIRMATION, false).is_err());
        let second = fixture(json!({"fixture":"copy"}))?;
        assert!(run(second, "yes", true).is_err());
        let mut expired = fixture(json!({"fixture":"copy"}))?;
        expired.prepared_at = Instant::now() - REVIEW_LIFETIME - Duration::from_secs(1);
        assert!(run(expired, CONFIRMATION, true).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn literal_arguments_redacted_input_and_minimal_environment_cross_process() -> Result<()> {
        let prepared = fixture(
            json!({"fixture":"copy","value":"CANARY_PASSWORD","safe":"$(touch SHOULD_NOT_EXIST); literal"}),
        )?;
        let outcome = run(prepared, CONFIRMATION, true)?;
        assert!(outcome.success, "fixture tool exited unsuccessfully");
        assert!(!outcome.stdout.contains("CANARY_PASSWORD"));
        assert!(
            outcome
                .stdout
                .contains("$(touch SHOULD_NOT_EXIST); literal")
        );
        assert!(
            outcome.stdout.contains("credential_environment_empty=true"),
            "fixture saw credential environment variables"
        );
        assert!(!outcome.stdout.contains('\u{1b}'));
        let private_home = outcome
            .stdout
            .lines()
            .find_map(|line| line.strip_prefix("private_home="))
            .ok_or("fixture did not report HOME")?;
        assert!(
            !Path::new(private_home).exists(),
            "temporary tool directory must be cleaned"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn descendants_cannot_keep_output_pipes_open_after_leader_exits() -> Result<()> {
        let prepared = fixture(json!({"fixture":"descendant"}))?;
        let started = Instant::now();
        let outcome = run_bounded(
            prepared,
            CONFIRMATION,
            true,
            Duration::from_secs(2),
            MAX_OUTPUT,
        )?;
        assert!(outcome.success, "fixture tool exited unsuccessfully");
        assert!(outcome.stdout.contains("descendant_pid="));
        assert!(started.elapsed() < Duration::from_secs(3));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn argv_is_passed_literally_without_shell_interpretation() -> Result<()> {
        let echo = Path::new("/bin/echo")
            .canonicalize()
            .map_err(|_| "system echo fixture unavailable")?;
        let argument = "$(touch SHOULD_NOT_EXIST); literal".to_string();
        let prepared = prepare(
            &echo,
            std::slice::from_ref(&argument),
            &json!({}),
            Format::Json,
        )?;
        let outcome = run(prepared, CONFIRMATION, true)?;
        assert!(outcome.success, "fixture tool exited unsuccessfully");
        assert_eq!(outcome.stdout.trim_end(), argument);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn capture_cancellation_stops_the_process_group_promptly() -> Result<()> {
        let executable = std::env::current_exe().map_err(|_| "fixture path unavailable")?;
        let mut command = Command::new(executable);
        command.args([
            "--ignored",
            "--exact",
            "handoff::tests::process_fixture",
            "--nocapture",
        ]);
        let cancel = std::sync::Arc::new(AtomicBool::new(false));
        let flag = cancel.clone();
        let canceller = thread::spawn(move || {
            thread::sleep(Duration::from_millis(150));
            flag.store(true, Ordering::Release);
        });
        let started = Instant::now();
        let result = capture(
            command,
            Some(b"{\"fixture\":\"sleep\"}".to_vec()),
            Duration::from_secs(20),
            MAX_OUTPUT,
            Some(&cancel),
        );
        let _ = canceller.join();
        assert!(result.is_err_and(|error| error.contains("canceled")));
        assert!(started.elapsed() < Duration::from_secs(5));
        // A flag set before spawn never starts the tool.
        assert!(
            capture(
                Command::new("/bin/echo"),
                None,
                Duration::from_secs(1),
                MAX_OUTPUT,
                Some(&cancel)
            )
            .is_err()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn floods_and_sleeping_children_are_bounded() -> Result<()> {
        let flood = fixture(json!({"fixture":"flood"}))?;
        let started = Instant::now();
        assert!(run_bounded(flood, CONFIRMATION, true, Duration::from_secs(2), 8192).is_err());
        assert!(started.elapsed() < Duration::from_secs(5));
        let sleeping = fixture(json!({"fixture":"sleep"}))?;
        let started = Instant::now();
        assert!(
            run_bounded(
                sleeping,
                CONFIRMATION,
                true,
                Duration::from_millis(100),
                MAX_OUTPUT
            )
            .is_err()
        );
        assert!(started.elapsed() < Duration::from_secs(3));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn executable_change_and_symlink_are_rejected() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = PrivateDirectory::create()?;
        let copied = directory.0.join("fixture");
        let source = std::env::current_exe().map_err(|_| "fixture path unavailable")?;
        fs::copy(&source, &copied).map_err(|_| "cannot copy fixture")?;
        fs::set_permissions(&copied, fs::Permissions::from_mode(0o700))
            .map_err(|_| "cannot set fixture permissions")?;
        let prepared = prepare(&copied, &[], &json!({}), Format::Json)?;
        let modified = fs::metadata(&copied)
            .and_then(|meta| meta.modified())
            .map_err(|_| "cannot inspect fixture modification time")?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(&copied)
            .map_err(|_| "cannot change fixture")?;
        file.write_all(b"changed")
            .and_then(|()| file.set_times(fs::FileTimes::new().set_modified(modified)))
            .map_err(|_| "cannot change fixture")?;
        assert!(run(prepared, CONFIRMATION, true).is_err());
        let link = directory.0.join("symlink");
        symlink(&source, &link).map_err(|_| "cannot create fixture symlink")?;
        assert!(prepare(&link, &[], &json!({}), Format::Json).is_err());
        let script = directory.0.join("script");
        fs::write(&script, b"#!/bin/sh\nexit 0\n").map_err(|_| "cannot create script fixture")?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700))
            .map_err(|_| "cannot set fixture permissions")?;
        assert!(prepare(&script, &[], &json!({}), Format::Json).is_err());
        Ok(())
    }

    // A native test executable provides portable subprocess fixtures without
    // introducing shell interpreters, scripts or dependencies in the handoff.
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn process_fixture() -> Result<()> {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|_| "fixture input failed")?;
        let value: Value = serde_json::from_str(&input).map_err(|_| "fixture JSON failed")?;
        match value["fixture"].as_str().unwrap_or("copy") {
            "descendant" => {
                let executable =
                    std::env::current_exe().map_err(|_| "fixture executable unavailable")?;
                let mut child = Command::new(executable)
                    .args([
                        "--ignored",
                        "--exact",
                        "handoff::tests::process_fixture",
                        "--nocapture",
                    ])
                    .stdin(Stdio::piped())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .map_err(|_| "cannot spawn descendant")?;
                child
                    .stdin
                    .take()
                    .ok_or("descendant stdin unavailable")?
                    .write_all(b"{\"fixture\":\"sleep\"}")
                    .map_err(|_| "cannot start descendant")?;
                println!("descendant_pid={}", child.id());
            }
            "sleep" => thread::sleep(Duration::from_secs(30)),
            "flood" => {
                let mut stdout = std::io::stdout().lock();
                for _ in 0..1024 {
                    if stdout.write_all(&[b'x'; 8192]).is_err() {
                        break;
                    }
                }
            }
            _ => {
                let empty = std::env::vars_os().all(|(key, _)| {
                    // macOS's runtime can add this user-encoding hint after
                    // exec even when the supplied environment was cleared.
                    matches!(
                        key.to_str(),
                        Some("HOME" | "USERPROFILE" | "TMPDIR" | "LANG" | "LC_ALL")
                    ) || (cfg!(target_os = "macos") && key == "__CF_USER_TEXT_ENCODING")
                });
                let names = std::env::vars_os()
                    .map(|(key, _)| key.to_string_lossy().into_owned())
                    .collect::<Vec<_>>()
                    .join(",");
                println!("environment_names={names}");
                println!(
                    "credential_environment_empty={empty}\n{input}\n\u{1b}]52;c;terminal-canary\u{7}"
                );
                println!(
                    "private_home={}",
                    std::env::var("HOME").map_err(|_| "fixture HOME unavailable")?
                );
                assert!(!Path::new("SHOULD_NOT_EXIST").exists());
            }
        }
        Ok(())
    }
}
