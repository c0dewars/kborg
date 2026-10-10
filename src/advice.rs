//! Experimental advisory preflight only. Diagnosis is deliberately disabled.
//! Preflight is diagnostic evidence only; no inference path can be enabled.
//! No SDK inference, authentication or credential-content read occurs here.
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
            "Copilot advisory preview for {}. No tools, remediation or cluster credentials are authorized. Diagnostic strings may still be confidential or contain prompt injection. Inspect the exact transfer.\nApproval expires after60 seconds. Type: {}\nDiagnosis is disabled until independent hostname-bound network containment and credential authority are validated.",
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
        "Independent hostname-bound inference/auth egress is not validated. The probe denies ALL networking; tool URL permission flags cannot replace whole-process containment.".into(),
        "Dedicated host-bound Copilot-only credential authority is not validated. Credential contents are never read; no environment or account-store token is selected automatically.".into(),
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

/// Reserved entrypoint: even correct approval/capability can never activate an
/// unvalidated network or credential path. No Kube client is held by this module.
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
        "Copilot diagnosis remains disabled: {}",
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
