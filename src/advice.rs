//! Explicitly reviewed advisory transfers through a constrained native Copilot runtime.
//! Legacy `preflight`/`diagnose` remain metadata-only and cannot send diagnostics.
//! The policy-based `prepare`/`run` path independently validates fresh containment.
use crate::{Result, copilot, safety};
use ring::digest::{Context, SHA256};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_NATIVE: u64 = 256 * 1024 * 1024;
const MAX_PREVIEW: usize = 64 * 1024;
const MAX_TOKEN_FILE: u64 = 16 * 1024;
const PROBE_OUTPUT: u64 = 4096;

pub struct PreparedPreview {
    host: String,
    bundle: Value,
    created: Instant,
}
impl PreparedPreview {
    pub fn preview(&self) -> Result<String> {
        let text = serde_json::to_string_pretty(&self.bundle)
            .map_err(|_| "cannot encode minimized preview")?;
        if text.len() > MAX_PREVIEW {
            return Err("minimized diagnostic preview exceeds64 KiB".into());
        }
        Ok(text)
    }
    pub fn confirmation(&self) -> String {
        format!("SEND MINIMIZED DIAGNOSTICS TO {}", self.host)
    }
    pub fn summary(&self) -> String {
        format!(
            "Copilot advisory preview for {}. No tools, remediation or cluster credentials are authorized. Diagnostic strings may still be confidential or contain prompt injection. Inspect the exact transfer.\nApproval expires after60 seconds. Type: {}\nThis local preview cannot send. A separate explicit Copilot policy and fresh contained prepare/run review are required.",
            self.host,
            self.confirmation()
        )
    }
}
pub fn prepare_preview(object: &Value, hostname: &str) -> Result<PreparedPreview> {
    let host = copilot::host(hostname)?;
    let prepared = PreparedPreview {
        bundle: json!({"approved_host":host,"diagnostic_bundle":copilot::minimize(object)}),
        host,
        created: Instant::now(),
    };
    prepared.preview()?;
    Ok(prepared)
}

#[derive(Debug, Serialize)]
pub struct PreflightReport {
    pub schema_version: u32,
    pub approved_host: String,
    pub native_bytes: u64,
    pub native_sha256: String,
    pub publisher_digest_matches: bool,
    pub native_codesign_valid: bool,
    pub credential_metadata_checked: bool,
    pub credentials_read: bool,
    pub sandbox_fixture: SandboxFixture,
    pub diagnosis_enabled: bool,
    pub blockers: Vec<String>,
}
#[derive(Debug, Default, Serialize)]
pub struct SandboxFixture {
    pub allowed_read: bool,
    pub denied_read: bool,
    pub denied_write: bool,
    pub denied_executable: bool,
    pub denied_loopback: bool,
    pub note: String,
}

/// Inspects an explicitly selected native SDK. A SHA pin must be obtained through
/// an independently trusted publisher channel, not copied from this report.
/// Every invocation creates and destroys fresh canary paths; no proofs are cached.
pub fn preflight(
    native_sdk: &Path,
    hostname: &str,
    expected_sha256: Option<&str>,
    token_path: Option<&Path>,
) -> Result<PreflightReport> {
    let host = copilot::host(hostname)?;
    let identity = fingerprint(native_sdk)?;
    let expected = expected_sha256.map(str::to_ascii_lowercase);
    if expected
        .as_ref()
        .is_some_and(|hash| hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("publisher SDK digest must be64 hexadecimal SHA-256 characters".into());
    }
    let publisher_digest_matches = expected.as_deref() == Some(identity.1.as_str());
    let native_codesign_valid = cfg!(target_os = "macos")
        && probe_command(
            Path::new("/usr/bin/codesign"),
            &[
                "--verify".into(),
                "--strict".into(),
                native_sdk.to_string_lossy().into_owned(),
            ],
            None,
        )
        .is_ok_and(|r| r.success);
    let credential_metadata_checked = if let Some(path) = token_path {
        credential_metadata(path)?;
        true
    } else {
        false
    };
    let sandbox_fixture = sandbox_fixture();
    let mut blockers=vec![
        "This legacy metadata-only probe does not validate the policy-based authenticated broker; it denies ALL networking and cannot enable transfers.".into(),
        "This legacy probe does not read credentials or select an account. The supported prepare/run path requires an explicit host/account-bound policy.".into(),
        "Canary fixtures demonstrate specific OS denials, not an independent security audit. Apple's sandbox-exec/SBPL interface is deprecated/private and may change.".into(),
    ];
    if !publisher_digest_matches {
        blockers.push(
            "Native SDK bytes lack a matching independently publisher-verified SHA-256 pin.".into(),
        );
    }
    if !native_codesign_valid {
        blockers.push("Installed native SDK fails, or cannot complete, strict macOS code-signature verification.".into());
    }
    if !sandbox_fixture.allowed_read
        || !sandbox_fixture.denied_read
        || !sandbox_fixture.denied_write
        || !sandbox_fixture.denied_executable
        || !sandbox_fixture.denied_loopback
    {
        blockers.push("Current sandbox canary preflight did not establish every required denial; no fallback to unsandboxed execution is permitted.".into());
    }
    Ok(PreflightReport {
        schema_version: 1,
        approved_host: host,
        native_bytes: identity.0,
        native_sha256: identity.1,
        publisher_digest_matches,
        native_codesign_valid,
        credential_metadata_checked,
        credentials_read: false,
        sandbox_fixture,
        diagnosis_enabled: false,
        blockers,
    })
}

/// Legacy metadata-only entrypoint. Use the separately contained policy-based
/// prepare/run flow for an explicitly reviewed transfer; no fallback occurs here.
pub fn diagnose(
    prepared: PreparedPreview,
    confirmation: &str,
    allowed: bool,
    native_sdk: &Path,
    expected_sha256: Option<&str>,
    token_path: &Path,
) -> Result<String> {
    if !allowed {
        return Err("Copilot advice requires an explicit capability; default is disabled".into());
    }
    if prepared.created.elapsed() > Duration::from_secs(60)
        || confirmation != prepared.confirmation()
    {
        return Err("Copilot transfer approval expired or exact host confirmation changed".into());
    }
    let report = preflight(
        native_sdk,
        &prepared.host,
        expected_sha256,
        Some(token_path),
    )?;
    Err(format!(
        "Legacy metadata-only diagnosis cannot send; use explicit policy-based prepare/run: {}",
        report.blockers.join(" ")
    ))
}

fn fingerprint(path: &Path) -> Result<(u64, String)> {
    if !path.is_absolute()
        || path
            .to_str()
            .is_none_or(|s| safety::safe_text(s, 4096) != s)
    {
        return Err("native SDK path must be absolute, UTF-8 and control-free".into());
    }
    safety::checked_path(path)?;
    let before = fs::symlink_metadata(path).map_err(|_| "cannot inspect native SDK")?;
    if !before.is_file() || before.len() < 4 || before.len() > MAX_NATIVE {
        return Err("native SDK must be a regular4-byte..256-MiB executable".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = before.permissions().mode();
        if mode & 0o111 == 0 || mode & 0o022 != 0 {
            return Err("native SDK must be executable and not group/world writable".into());
        }
    }
    let mut file = fs::File::open(path).map_err(|_| "cannot open native SDK")?;
    let mut context = Context::new(&SHA256);
    let mut bytes = 4u64;
    let mut chunk = [0u8; 16384];
    let mut header = [0u8; 4];
    file.read_exact(&mut header)
        .map_err(|_| "native SDK header is truncated")?;
    context.update(&header);
    loop {
        let count = file
            .read(&mut chunk)
            .map_err(|_| "cannot hash native SDK")?;
        if count == 0 {
            break;
        }
        bytes = bytes.saturating_add(count as u64);
        if bytes > MAX_NATIVE {
            return Err("native SDK grew beyond256 MiB".into());
        }
        context.update(&chunk[..count]);
    }
    if !matches!(
        header,
        [0xcf, 0xfa, 0xed, 0xfe]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xca, 0xfe, 0xba, 0xbf]
            | [0xbe, 0xba, 0xfe, 0xca]
            | [0xbf, 0xba, 0xfe, 0xca]
    ) {
        return Err("Copilot advice preflight requires a native Mach-O executable, not a script or npm symlink".into());
    }
    let after = file.metadata().map_err(|_| "cannot recheck native SDK")?;
    if bytes != before.len()
        || before.len() != after.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err("native SDK changed during fingerprinting".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.mode() != after.mode()
        {
            return Err("native SDK identity changed during fingerprinting".into());
        }
    }
    let hash = context
        .finish()
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    Ok((bytes, hash))
}
fn credential_metadata(path: &Path) -> Result<()> {
    if !path.is_absolute() {
        return Err("explicit Copilot token-file path must be absolute".into());
    }
    safety::checked_path(path)?;
    let meta = fs::symlink_metadata(path)
        .map_err(|_| "cannot inspect explicit credential-file metadata")?;
    if !meta.is_file() || meta.len() == 0 || meta.len() > MAX_TOKEN_FILE {
        return Err("explicit token file must be a nonempty regular file at most16 KiB".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if meta.permissions().mode() & 0o777 != 0o600 || meta.nlink() != 1 {
            return Err("explicit token file must have0600 permissions and a single link".into());
        }
    }
    Ok(())
}

struct ProbeOutput {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}
fn probe_command(program: &Path, args: &[String], directory: Option<&Path>) -> Result<ProbeOutput> {
    let mut command = Command::new(program);
    command
        .args(args)
        .env_clear()
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(directory) = directory {
        command
            .current_dir(directory)
            .env("HOME", directory)
            .env("TMPDIR", directory)
            .env("PATH", "");
    }
    let mut child = command
        .spawn()
        .map_err(|_| "cannot launch fixed OS preflight utility")?;
    let stdout = child.stdout.take().ok_or("preflight stdout unavailable")?;
    let stderr = child.stderr.take().ok_or("preflight stderr unavailable")?;
    let collect = |reader: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            reader
                .take(PROBE_OUTPUT + 1)
                .read_to_end(&mut bytes)
                .map(|_| bytes)
        })
    };
    let out = collect(Box::new(stdout));
    let err = collect(Box::new(stderr));
    let deadline = Instant::now() + Duration::from_secs(3);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => thread::sleep(Duration::from_millis(5)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("OS preflight utility exceeded its3-second budget".into());
            }
        }
    };
    let stdout = out
        .join()
        .map_err(|_| "preflight stdout capture failed")?
        .map_err(|_| "preflight stdout read failed")?;
    let stderr = err
        .join()
        .map_err(|_| "preflight stderr capture failed")?
        .map_err(|_| "preflight stderr read failed")?;
    if stdout.len() as u64 > PROBE_OUTPUT || stderr.len() as u64 > PROBE_OUTPUT {
        return Err("OS preflight utility output exceeds4 KiB".into());
    }
    Ok(ProbeOutput {
        success: status.success(),
        stdout,
        stderr,
    })
}
struct ProbeDirectory(PathBuf);
impl Drop for ProbeDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn sandbox_fixture() -> SandboxFixture {
    if !cfg!(target_os = "macos") {
        return SandboxFixture {
            note: "No verified sandbox backend on this platform; diagnosis disabled.".into(),
            ..Default::default()
        };
    }
    let result = (|| -> Result<SandboxFixture> {
        let root = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "temporary root unavailable")?;
        safety::checked_path(&root)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "invalid system time")?
            .as_nanos();
        let directory = ProbeDirectory(root.join(format!(
            "kborg-advice-preflight-{}-{stamp}",
            std::process::id()
        )));
        safety::ensure_private_dir(&directory.0)?;
        let allowed = directory.0.join("allowed");
        let denied = directory.0.join("denied");
        safety::write_new(&allowed.join("canary"), b"KBORG_ALLOWED_CANARY")?;
        safety::write_new(&denied.join("canary"), b"KBORG_FAKE_CREDENTIAL")?;
        let quote = |path: &Path| {
            serde_json::to_string(&path.to_string_lossy())
                .map_err(|_| "cannot encode preflight path")
        };
        let profile = format!(
            "(version 1)(deny default)(import \"system.sb\")(deny mach-lookup)(deny network*)\n(allow process-exec (literal \"/bin/cat\") (literal \"/usr/bin/touch\") (literal \"/usr/bin/nc\"))\n(allow file-read* (literal \"/bin/cat\") (literal \"/usr/bin/touch\") (literal \"/usr/bin/nc\") (subpath {}))\n(allow file-map-executable (literal \"/bin/cat\") (literal \"/usr/bin/touch\") (literal \"/usr/bin/nc\"))\n(allow file-write* (subpath {}))\n(allow file-read-metadata (path-ancestors {}))\n",
            quote(&allowed)?,
            quote(&allowed)?,
            quote(&allowed)?
        );
        let policy = directory.0.join("probe.sb");
        safety::write_new(&policy, profile.as_bytes())?;
        let probe = |program: &str, arguments: &[&str]| -> Result<ProbeOutput> {
            let mut args = vec![
                "-f".into(),
                policy.to_string_lossy().into_owned(),
                program.into(),
            ];
            args.extend(arguments.iter().map(|s| (*s).to_owned()));
            probe_command(Path::new("/usr/bin/sandbox-exec"), &args, Some(&allowed))
        };
        let yes = probe(
            "/bin/cat",
            &[allowed
                .join("canary")
                .to_str()
                .ok_or("invalid canary path")?],
        )?;
        if !yes.success || yes.stdout != b"KBORG_ALLOWED_CANARY" {
            return Ok(SandboxFixture {
                note: "Sandbox baseline could not be established; denial probes remain unverified."
                    .into(),
                ..Default::default()
            });
        }
        let no = probe(
            "/bin/cat",
            &[denied
                .join("canary")
                .to_str()
                .ok_or("invalid canary path")?],
        )?;
        let write = probe(
            "/usr/bin/touch",
            &[denied
                .join("outside-write")
                .to_str()
                .ok_or("invalid canary path")?],
        )?;
        let exec = probe("/bin/echo", &["forbidden-executable"])?;
        let network = probe("/usr/bin/nc", &["-v", "-z", "-w", "1", "127.0.0.1", "9"])?;
        let blocked = |output: &ProbeOutput| {
            !output.success
                && String::from_utf8_lossy(&output.stderr)
                    .to_lowercase()
                    .contains("operation not permitted")
        };
        Ok(SandboxFixture {allowed_read:yes.success&&yes.stdout==b"KBORG_ALLOWED_CANARY",denied_read:blocked(&no),denied_write:blocked(&write)&&!denied.join("outside-write").exists(),denied_executable:blocked(&exec),denied_loopback:blocked(&network),note:"Fresh fake-canary checks only. All network denied, not a validated inference-domain allowlist or external security audit.".into()})
    })();
    result.unwrap_or_else(|error| SandboxFixture {
        note: error,
        ..Default::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn preview_is_minimized_host_bound_and_never_executable() -> Result<()> {
        let preview = prepare_preview(
            &json!({"kind":"Pod","metadata":{"name":"NAME_CANARY","namespace":"NS_CANARY"},"spec":{"containers":[{"env":[{"value":"CREDENTIAL_CANARY"}]}]},"status":{"phase":"Pending"}}),
            "github.com",
        )?;
        let text = preview.preview()?;
        assert!(!text.contains("CANARY"));
        assert!(text.contains("Pending"));
        assert_eq!(
            preview.confirmation(),
            "SEND MINIMIZED DIAGNOSTICS TO github.com"
        );
        assert!(prepare_preview(&json!({}), "github.com@evil.example").is_err());
        assert!(
            diagnose(
                preview,
                "yes",
                true,
                Path::new("/does/not/exist"),
                None,
                Path::new("/does/not/exist")
            )
            .is_err()
        );
        Ok(())
    }
}

#[path = "copilot_process.rs"]
mod process;
#[path = "copilot_proxy.rs"]
mod proxy;
#[path = "copilot_runtime.rs"]
mod runtime;
pub use proxy::NetworkReport;
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Options {
    pub schema_version: u32,
    pub executable: PathBuf,
    pub expected_sha256: String,
    pub host: String,
    /// "github.com", "ghe.com" or "server"; inferred for the first two.
    #[serde(default)]
    pub deployment: Option<String>,
    /// GitHub Enterprise Server only: permit RFC 1918/CGNAT/ULA destinations.
    #[serde(default)]
    pub allow_private_network: bool,
    /// GitHub Enterprise Server only: absolute PEM bundle for an internal CA.
    #[serde(default)]
    pub tls_ca_file: Option<PathBuf>,
    pub credential_file: Option<PathBuf>,
    pub gh_cli: Option<GhCredential>,
    pub model: String,
    pub seconds: u16,
}
#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GhCredential {
    pub executable: PathBuf,
    pub expected_sha256: String,
    pub account: String,
    pub config_dir: PathBuf,
}
fn valid_pin(pin: &str) -> bool {
    pin.len() == 64
        && pin
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}
fn valid_account(account: &str) -> bool {
    !account.is_empty()
        && account.len() <= 39
        && account
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
        && !account.starts_with('-')
        && !account.ends_with('-')
}
impl Options {
    pub fn deployment(&self) -> Result<copilot::Deployment> {
        copilot::Deployment::parse(&self.host, self.deployment.as_deref())
    }
    fn validate(&self) -> Result<()> {
        if self.schema_version != 1 {
            return Err("unsupported Copilot policy schema; expected1".into());
        }
        let deployment = self.deployment()?;
        let server = matches!(deployment, copilot::Deployment::EnterpriseServer(_));
        if self.allow_private_network && !server {
            return Err(
                "allow_private_network applies only to a GitHub Enterprise Server deployment"
                    .into(),
            );
        }
        match &self.tls_ca_file {
            Some(_) if !server => {
                return Err(
                    "tls_ca_file applies only to a GitHub Enterprise Server deployment".into(),
                );
            }
            Some(path) if !path.is_absolute() => {
                return Err("tls_ca_file must be an absolute path".into());
            }
            _ => {}
        }
        if !valid_pin(&self.expected_sha256) {
            return Err("Copilot policy needs a lowercase64-hex SHA-256 pin obtained independently from a trusted publisher source".into());
        }
        if !self.executable.is_absolute() {
            return Err("Copilot executable path must be absolute".into());
        }
        match (&self.credential_file,&self.gh_cli) {
            (Some(path),None) if path.is_absolute()=>{},
            (None,Some(gh)) if gh.executable.is_absolute()&&gh.config_dir.is_absolute()&&valid_pin(&gh.expected_sha256)&&valid_account(&gh.account)=>{},
            _=>return Err("Copilot policy requires exactly one absolute credential_file or gh_cli with absolute executable/config_dir, SHA-256 pin and explicit account".into()),
        }
        if self.model.is_empty()
            || self.model.len() > 80
            || !self
                .model
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
            || !self.model.as_bytes()[0].is_ascii_alphanumeric()
        {
            return Err("Copilot model must be one explicit bounded model ID, such as auto".into());
        }
        if !(1..=120).contains(&self.seconds) {
            return Err("Copilot run budget must be1..120 seconds".into());
        }
        Ok(())
    }
}
fn authority_metadata(path: &Path, directory: bool) -> Result<std::fs::Metadata> {
    if !path.is_absolute() {
        return Err("Copilot authority paths must be absolute".into());
    }
    safety::checked_path(path)?;
    let metadata =
        fs::symlink_metadata(path).map_err(|_| "cannot inspect selected Copilot authority")?;
    if (directory && !metadata.is_dir()) || (!directory && !metadata.is_file()) {
        return Err("Copilot authority has an unexpected file type".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.permissions().mode() & 0o022 != 0 || (!directory && metadata.nlink() != 1) {
            return Err("Copilot policy/credential authority must not be group/world writable or multiply linked".into());
        }
    }
    Ok(metadata)
}
fn authority_read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let before = authority_metadata(path, false)?;
    let bytes = safety::read_small(path, limit)?;
    let after = authority_metadata(path, false)?;
    if before.len() != after.len() || before.modified().ok() != after.modified().ok() {
        return Err("Copilot authority changed while reading".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.mode() != after.mode()
        {
            return Err("Copilot authority identity changed while reading".into());
        }
    }
    Ok(bytes)
}
pub fn load_options(path: &Path) -> Result<Options> {
    let options:Options=serde_json::from_slice(&authority_read(path,16*1024)?).map_err(|_|"Copilot policy must be strict schema1 JSON with executable, expected_sha256, host, exactly one credential_file/gh_cli, model and seconds")?;
    options.validate()?;
    Ok(options)
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Credential {
    schema_version: u32,
    host: String,
    account: String,
    token: String,
}
fn credential_hash(credential: &Credential) -> String {
    let mut digest = Context::new(&SHA256);
    for field in [&credential.host, &credential.account, &credential.token] {
        digest.update(field.as_bytes());
        digest.update(&[0]);
    }
    digest
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn valid_token(token: &str, dedicated: bool) -> bool {
    token.len() >= 32
        && token.len() <= 2048
        && token
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && (token.starts_with("github_pat_")
            || (!dedicated && (token.starts_with("gho_") || token.starts_with("ghu_"))))
}
impl Credential {
    fn read(path: &Path, host: &str) -> Result<(Self, String)> {
        credential_metadata(path)?;
        let bytes = authority_read(path, MAX_TOKEN_FILE)?;
        let credential:Self=serde_json::from_slice(&bytes).map_err(|_|"Dedicated credential must be strict JSON with schema_version, host, account and token; contents suppressed")?;
        if credential.schema_version != 1
            || credential.host != host
            || !valid_account(&credential.account)
            || !valid_token(&credential.token, true)
        {
            return Err("Dedicated credential requires schema1, the policy's exact host, explicit account and a fine-grained PAT; no implicit credential fallback".into());
        }
        let hash = credential_hash(&credential);
        Ok((credential, hash))
    }
}
fn selected_credential(
    options: &Options,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<(Credential, String)> {
    if let Some(path) = &options.credential_file {
        return Credential::read(path, &options.host);
    }
    let gh = options
        .gh_cli
        .as_ref()
        .ok_or("explicit credential source is missing")?;
    authority_metadata(&gh.config_dir, true)?;
    if fingerprint(&gh.executable)?.1 != gh.expected_sha256 {
        return Err("Selected gh executable no longer matches its trust pin".into());
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .ok_or("Explicit gh extraction requires current user HOME for its keychain")?;
    authority_metadata(&home, true)?;
    // Snapshot the trusted exact gh executable before invoking it. Its HOME/config
    // are available only in this separate operator-authorized credential process.
    let workspace = runtime::credential_workspace()?;
    let native = workspace.path().join("gh");
    fs::copy(&gh.executable, &native).map_err(|_| "cannot snapshot selected gh executable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&native, fs::Permissions::from_mode(0o500))
            .map_err(|_| "cannot protect selected gh snapshot")?;
    }
    if fingerprint(&native)?.1 != gh.expected_sha256 {
        return Err("Selected gh executable changed during snapshot".into());
    }
    let mut command = Command::new(&native);
    command
        .args([
            "auth",
            "token",
            "--hostname",
            &options.host,
            "--user",
            &gh.account,
        ])
        .env_clear()
        .env("PATH", "")
        .env("HOME", home)
        .env("GH_CONFIG_DIR", &gh.config_dir)
        .env("GH_HOST", &options.host)
        .env("GH_PROMPT_DISABLED", "1")
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .current_dir(workspace.path());
    let capture = process::invoke(command, vec![], Duration::from_secs(10), stop, 4096)?;
    if !capture.success {
        return Err("Explicit pinned gh credential lookup failed for selected host/account; details suppressed; no fallback".into());
    }
    let token = std::str::from_utf8(&capture.stdout)
        .map_err(|_| "Selected gh returned invalid token bytes; suppressed")?
        .trim()
        .to_owned();
    if !valid_token(&token, false) {
        return Err("Selected gh account token is unsupported for Copilot (OAuth/user-to-server/fine-grained token required); no fallback".into());
    }
    let credential = Credential {
        schema_version: 1,
        host: options.host.clone(),
        account: gh.account.clone(),
        token,
    };
    let hash = credential_hash(&credential);
    Ok((credential, hash))
}
pub struct PreparedAdvice {
    options: Options,
    bundle: String,
    summary: String,
    created: Instant,
    credential_hash: String,
    account: String,
}
impl PreparedAdvice {
    pub fn preview(&self) -> Result<String> {
        Ok(self.bundle.clone())
    }
    pub fn summary(&self) -> &str {
        &self.summary
    }
    pub fn confirmation(&self) -> String {
        format!(
            "SEND MINIMIZED DIAGNOSTICS TO {} AS {}",
            self.options.host, self.account
        )
    }
}
#[derive(Debug, Serialize)]
pub struct AdviceOutput {
    pub schema_version: u32,
    pub host: String,
    pub account: String,
    pub model: String,
    pub answer: String,
    pub notice: String,
    pub network: NetworkReport,
}
pub async fn prepare(object: &Value, options: Options) -> Result<PreparedAdvice> {
    options.validate()?;
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Err(
            "Contained Copilot advice is currently macOS ARM64-only; no unsandboxed fallback"
                .into(),
        );
    }
    let (_, pin) = fingerprint(&options.executable)?;
    if pin != options.expected_sha256 {
        return Err("Copilot executable changed or does not match the independently selected SHA-256 trust pin".into());
    }
    if !probe_command(
        Path::new("/usr/bin/codesign"),
        &[
            "--verify".into(),
            "--strict".into(),
            options.executable.to_string_lossy().into_owned(),
        ],
        None,
    )?
    .success
    {
        return Err("Copilot native code signature verification failed; a signature check does not establish publisher identity".into());
    }
    runtime::validate_native(&options)?;
    let (credential, credential_hash) = selected_credential(
        &options,
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
    )?;
    let bundle = serde_json::to_string_pretty(&copilot::minimize(object))
        .map_err(|_| "cannot encode minimized Copilot bundle")?;
    if bundle.len() > MAX_PREVIEW {
        return Err("minimized Copilot bundle exceeds64KiB".into());
    }
    let deployment = options.deployment()?;
    let mut prepared = PreparedAdvice {
        options,
        bundle,
        summary: String::new(),
        created: Instant::now(),
        credential_hash,
        account: credential.account,
    };
    prepared.summary = format!(
        "COPILOT DIAGNOSTIC TRANSFER / NOT SENT\nDeployment: {}\nHost: {}\nCredential account label: {} (operator supplied, not an independently verified account identity)\nModel: {} | wall/CPU budget {}s\nNative executable SHA-256: {}\nThe supplied pin must come from an independently trusted publisher source. Matching bytes and native code-sign validation do not establish publisher identity.\n\nOnly the exact minimized status bundle below is authorized. No Kubernetes credentials, resource names, messages, manifests or logs are included. Unknown strings are replaced with Other. Numeric counters can still be confidential.\n\nWhole-process macOS OS policy: empty private HOME, restricted reads, workspace-only writes, no child executables, and no direct networking. Authenticated local broker permits only exact approved TLS CONNECT authorities and matching SNI: {}. These provider services process the diagnostics.{}\nTool/MCP/instruction/auto-login switches are defense in depth, not a sandbox. The selected credential source is explicit and host/account-bound. A chosen gh account may have authority broader than Copilot; KBorg cannot independently introspect all grants. No ambient environment token or automatic account fallback is used. Only the separate pinned gh credential lookup, when explicitly selected, can access its user HOME/config and keychain. The Copilot child cannot access them.\n\nOutput is untrusted advice. No suggested command is executed or automatically approved. Approval expires after5minutes.\nType exactly: {}",
        deployment.label(),
        prepared.options.host,
        prepared.account,
        prepared.options.model,
        prepared.options.seconds,
        prepared.options.expected_sha256,
        deployment.authorities().join(", "),
        match &deployment {
            copilot::Deployment::GitHub => String::new(),
            copilot::Deployment::EnterpriseCloud(_) => {
                " Requests stay on this tenant's region-specific GHE.com endpoints.".into()
            }
            copilot::Deployment::EnterpriseServer(_) => format!(
                " The GHES instance forwards the bundle to its administrator-configured model provider, which KBorg cannot see or verify.{}{}",
                if prepared.options.allow_private_network {
                    " Private enterprise addresses are permitted for this host only; loopback, link-local and metadata addresses stay blocked."
                } else {
                    ""
                },
                if prepared.options.tls_ca_file.is_some() {
                    " TLS verification for this host uses the pinned policy CA bundle."
                } else {
                    ""
                }
            ),
        },
        prepared.confirmation()
    );
    Ok(prepared)
}
pub async fn run(
    prepared: PreparedAdvice,
    confirmation: &str,
    mut cancel: Option<tokio::sync::oneshot::Receiver<()>>,
) -> Result<AdviceOutput> {
    if confirmation != prepared.confirmation()
        || prepared.created.elapsed() > Duration::from_secs(300)
    {
        return Err(
            "Copilot transfer approval expired or exact host/account confirmation changed".into(),
        );
    }
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let done = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    struct Guard(
        std::sync::Arc<std::sync::atomic::AtomicBool>,
        std::sync::Arc<std::sync::atomic::AtomicBool>,
    );
    impl Drop for Guard {
        fn drop(&mut self) {
            use std::sync::atomic::Ordering;
            self.0.store(true, Ordering::Release);
            let deadline = Instant::now() + Duration::from_secs(5);
            while !self.1.load(Ordering::Acquire) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    struct Done(std::sync::Arc<std::sync::atomic::AtomicBool>);
    impl Drop for Done {
        fn drop(&mut self) {
            self.0.store(true, std::sync::atomic::Ordering::Release);
        }
    }
    let _guard = Guard(stop.clone(), done.clone());
    let worker_stop = stop.clone();
    let mut worker = tokio::task::spawn_blocking(move || {
        let _done = Done(done);
        runtime::execute(prepared, worker_stop)
    });
    let canceled = async {
        if let Some(receiver) = &mut cancel {
            let _ = receiver.await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    tokio::select! {result=&mut worker=>result.map_err(|_|"Copilot worker ended unexpectedly; no remediation is authorized".to_owned())?,_=canceled=>{stop.store(true,std::sync::atomic::Ordering::SeqCst);let _=worker.await;Err("Copilot request canceled; a reviewed diagnostic request may already have reached the provider".into())}}
}
#[cfg(test)]
mod policy_tests {
    use super::*;
    use crate::test_paths::abs;
    fn policy() -> Value {
        json!({"schema_version":1,"executable":abs("/native/copilot"),"expected_sha256":"a".repeat(64),"host":"github.com","gh_cli":{"executable":abs("/native/gh"),"expected_sha256":"b".repeat(64),"account":"c0dewars","config_dir":abs("/explicit/gh")},"model":"auto","seconds":60})
    }
    #[test]
    fn strict_policy_selects_one_host_account_source_and_rejects_duplicates() -> Result<()> {
        let valid = policy();
        let options: Options =
            serde_json::from_value(valid.clone()).map_err(|_| "valid policy failed")?;
        options.validate()?;
        for changed in [
            json!({"host":"evil.example"}),
            json!({"credential_file":abs("/second/source")}),
            json!({"seconds":121}),
        ] {
            let mut value = valid.clone();
            for (key, value2) in changed.as_object().ok_or("object")? {
                value[key] = value2.clone();
            }
            let result = serde_json::from_value::<Options>(value);
            assert!(result.is_err() || result.is_ok_and(|options| options.validate().is_err()));
        }
        let duplicate = serde_json::to_string(&valid)
            .map_err(|_| "encode")?
            .replacen(
                "\"schema_version\":1",
                "\"schema_version\":1,\"schema_version\":1",
                1,
            );
        assert!(serde_json::from_str::<Options>(&duplicate).is_err());
        let mut unknown = valid;
        unknown["gh_cli"]["token"] = "ignored_secret".into();
        assert!(serde_json::from_value::<Options>(unknown).is_err());
        Ok(())
    }
    #[test]
    fn enterprise_deployments_are_explicit_and_server_only_options_stay_scoped() -> Result<()> {
        let options = |changes: Value| -> Result<Options> {
            let mut value = policy();
            for (key, change) in changes.as_object().ok_or("object")? {
                value[key] = change.clone();
            }
            serde_json::from_value(value).map_err(|_| "policy".into())
        };
        let cloud = options(json!({"host":"acme.ghe.com"}))?;
        cloud.validate()?;
        assert_eq!(
            cloud.deployment()?,
            copilot::Deployment::EnterpriseCloud("acme.ghe.com".into())
        );
        assert!(
            options(json!({"host":"git.corp.example"}))?
                .validate()
                .is_err()
        );
        let server = options(
            json!({"host":"git.corp.example","deployment":"server","allow_private_network":true,"tls_ca_file":abs("/etc/kborg/corp-ca.pem")}),
        )?;
        server.validate()?;
        assert_eq!(
            server.deployment()?.authorities(),
            vec!["git.corp.example".to_owned()]
        );
        for invalid in [
            json!({"allow_private_network":true}),
            json!({"host":"acme.ghe.com","allow_private_network":true}),
            json!({"tls_ca_file":abs("/etc/kborg/corp-ca.pem")}),
            json!({"host":"git.corp.example","deployment":"server","tls_ca_file":"relative.pem"}),
        ] {
            assert!(options(invalid)?.validate().is_err());
        }
        Ok(())
    }
    #[test]
    fn token_sources_cannot_silently_accept_classic_or_ambient_authority() {
        assert!(valid_token(&format!("gho_{}", "a".repeat(36)), false));
        assert!(!valid_token(&format!("gho_{}", "a".repeat(36)), true));
        assert!(!valid_token(&format!("ghp_{}", "a".repeat(36)), false));
        assert!(!valid_token("secret\ncanary", false));
        let first = Credential {
            schema_version: 1,
            host: "github.com".into(),
            account: "first".into(),
            token: format!("gho_{}", "a".repeat(36)),
        };
        let mut second = Credential {
            schema_version: 1,
            host: first.host.clone(),
            account: first.account.clone(),
            token: first.token.clone(),
        };
        assert_eq!(credential_hash(&first), credential_hash(&second));
        second.account = "other".into();
        assert_ne!(credential_hash(&first), credential_hash(&second));
        second.account = first.account.clone();
        second.token.push('b');
        assert_ne!(credential_hash(&first), credential_hash(&second));
    }
}

#[cfg(all(test, unix))]
mod authority_tests {
    use super::*;
    #[test]
    fn writable_or_linked_authority_is_rejected_before_parsing() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let root = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "temp")?
            .join(format!(
                "kborg-policy-test-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| "clock")?
                    .as_nanos()
            ));
        safety::ensure_private_dir(&root)?;
        struct Cleanup(PathBuf);
        impl Drop for Cleanup {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let _cleanup = Cleanup(root.clone());
        let file = root.join("policy.json");
        safety::write_new(&file, b"{}")?;
        assert!(authority_read(&file, 4096).is_ok());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o660)).map_err(|_| "mode")?;
        assert!(authority_read(&file, 4096).is_err());
        fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).map_err(|_| "mode")?;
        fs::hard_link(&file, root.join("second-link")).map_err(|_| "link")?;
        assert!(authority_read(&file, 4096).is_err());
        Ok(())
    }
}
