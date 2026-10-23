//! Immutable inputs for offline verification of exact packaged chart bytes.
//! Cryptography, certificate identity/time/SCT and log proof validation belong
//! to the explicitly trusted, hash-pinned Cosign 3.1.3 supervisor, never this parser.
use crate::{Result, safety};
use base64::{Engine, engine::general_purpose::STANDARD};
use ring::digest::{Context, SHA256, digest};
use serde::Deserialize;
use serde_json::Value;
use std::{
    fs,
    io::Read,
    path::{Path, PathBuf},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub const VERSION: &str = "v3.1.3";
const CHART_LIMIT: u64 = 8 * 1024 * 1024;
const POLICY_LIMIT: u64 = 64 * 1024;
const BUNDLE_LIMIT: u64 = 4 * 1024 * 1024;
const TRUST_LIMIT: u64 = 1024 * 1024;
const TOOL_LIMIT: u64 = 256 * 1024 * 1024;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Policy {
    schema: u8,
    executable: PathBuf,
    sha256: String,
    bundle: PathBuf,
    #[serde(default)]
    expires_at: Option<u64>,
    trust: Trust,
}
#[derive(Clone, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case", deny_unknown_fields)]
enum Trust {
    PublicKey {
        key: PathBuf,
        trusted_root: PathBuf,
    },
    Identity {
        trusted_root: PathBuf,
        identity: String,
        issuer: String,
    },
}
#[derive(Clone)]
struct Input {
    source: PathBuf,
    sealed: PathBuf,
    sha256: String,
    limit: u64,
}
#[derive(Clone)]
pub struct Prepared {
    executable: PathBuf,
    executable_hash: String,
    inputs: Vec<Input>,
    root: PathBuf,
    argv: Vec<String>,
    trust_summary: String,
    expires_at: Option<u64>,
    created: Instant,
}
impl Prepared {
    pub fn executable(&self) -> &Path {
        &self.executable
    }
    pub fn working_dir(&self) -> &Path {
        &self.root
    }
    pub fn argv(&self) -> Vec<String> {
        self.argv.clone()
    }
    pub fn summary(&self) -> String {
        let hashes = self
            .inputs
            .iter()
            .map(|input| {
                format!(
                    "{} SHA-256: {}\n",
                    input
                        .sealed
                        .file_name()
                        .and_then(|s| s.to_str())
                        .unwrap_or("input"),
                    input.sha256
                )
            })
            .collect::<String>();
        format!(
            "Required Sigstore chart-blob verification\nSubject: exact packaged chart bytes, NOT an OCI manifest or registry-referrer signature.\nVerifier: {} ({VERSION}), SHA-256 {}\n{}{}The supplied public trust root is explicit authority; no TUF/keyserver/registry discovery. Signature, artifact digest, transparency proof and applicable certificate identity/SCT/time checks are required. No bypass flags.\nOffline verification is not an OS/network sandbox. Tool retains OS-user permissions. Source/private inputs and exact verifier are rechecked before execution; review expires after 15 minutes.\n",
            safety::safe_text(&self.executable.to_string_lossy(), 4096),
            self.executable_hash,
            self.trust_summary,
            hashes
        )
    }
    pub fn check(&self) -> Result<()> {
        if self.created.elapsed() > Duration::from_secs(900) {
            return Err("Sigstore verification review expired; prepare again".into());
        }
        expiry(self.expires_at)?;
        if executable(&self.executable)? != self.executable_hash {
            return Err("trusted Cosign changed after review; no lifecycle write sent".into());
        }
        for input in &self.inputs {
            for path in [&input.source, &input.sealed] {
                if matches!(
                    input.sealed.file_name().and_then(|name| name.to_str()),
                    Some("policy.json" | "trusted-root.json" | "trusted-public.pem")
                ) {
                    authority(path)?;
                }
                if file_hash(path, input.limit)? != input.sha256 {
                    return Err("Sigstore policy, chart, bundle or public trust input changed; prepare again".into());
                }
            }
        }
        Ok(())
    }
}
/// Require a successful supervised `version --json`, then this exact version
/// before accepting any verification result. Raw verifier diagnostics are private.
pub fn validate_version(stdout: &[u8]) -> Result<()> {
    if stdout.len() > 64 * 1024 {
        return Err("Cosign version report exceeded 64 KiB".into());
    }
    let value: Value =
        serde_json::from_slice(stdout).map_err(|_| "invalid Cosign version report")?;
    if value["gitVersion"] != VERSION {
        return Err(format!(
            "Sigstore requires exactly Cosign {VERSION}; older versions include known bundle-verification bypasses"
        ));
    }
    Ok(())
}
fn expiry(value: Option<u64>) -> Result<()> {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before Unix epoch")?
        .as_secs();
    if value.is_some_and(|expiry| expiry <= now) {
        return Err("Sigstore policy expired; obtain a fresh explicitly trusted policy".into());
    }
    Ok(())
}
fn path(path: &Path) -> Result<()> {
    let text = path.to_str().ok_or("Sigstore input path must be UTF-8")?;
    if !path.is_absolute() || text.len() > 4096 || safety::safe_text(text, 4096) != text {
        return Err("Sigstore requires bounded absolute paths without terminal controls".into());
    }
    safety::checked_path(path)
}
fn hash(bytes: &[u8]) -> String {
    digest(&SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn same(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        a.dev() == b.dev()
            && a.ino() == b.ino()
            && a.mode() == b.mode()
            && a.len() == b.len()
            && a.modified().ok() == b.modified().ok()
    }
    #[cfg(not(unix))]
    {
        a.len() == b.len() && a.modified().ok() == b.modified().ok()
    }
}
fn open(pathname: &Path, limit: u64) -> Result<fs::File> {
    if !cfg!(any(
        target_os = "macos",
        all(
            target_os = "linux",
            any(target_arch = "x86_64", target_arch = "aarch64")
        )
    )) {
        return Err("Sigstore verification supports macOS and Linux x86-64/ARM64 only".into());
    }
    path(pathname)?;
    let before =
        fs::symlink_metadata(pathname).map_err(|_| "Sigstore input is missing or unreadable")?;
    if !before.is_file() || before.len() == 0 || before.len() > limit {
        return Err("Sigstore input must be a nonempty bounded regular file".into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x4);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x800);
    }
    let file = options
        .open(pathname)
        .map_err(|_| "cannot open Sigstore input")?;
    let after = file
        .metadata()
        .map_err(|_| "cannot inspect Sigstore input")?;
    if !after.is_file() || !same(&before, &after) {
        return Err("Sigstore input changed while opening".into());
    }
    Ok(file)
}
fn read(path: &Path, limit: u64) -> Result<Vec<u8>> {
    let file = open(path, limit)?;
    let before = file.metadata().map_err(|_| "Sigstore metadata")?;
    let mut bytes = Vec::new();
    (&file)
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "cannot read bounded Sigstore input")?;
    if bytes.len() as u64 > limit
        || !same(&before, &file.metadata().map_err(|_| "Sigstore metadata")?)
    {
        return Err("Sigstore input changed or exceeded its bound".into());
    }
    Ok(bytes)
}
fn file_hash(path: &Path, limit: u64) -> Result<String> {
    let mut file = open(path, limit)?;
    let before = file.metadata().map_err(|_| "Sigstore metadata")?;
    let mut context = Context::new(&SHA256);
    let mut size = 0u64;
    let mut buffer = [0; 64 * 1024];
    loop {
        let count = file
            .read(&mut buffer)
            .map_err(|_| "cannot hash Sigstore input")?;
        if count == 0 {
            break;
        }
        size += count as u64;
        if size > limit {
            return Err("Sigstore input exceeded hash bound".into());
        }
        context.update(&buffer[..count]);
    }
    if !same(&before, &file.metadata().map_err(|_| "Sigstore metadata")?) {
        return Err("Sigstore input changed while hashing".into());
    }
    Ok(context
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
fn executable(path: &Path) -> Result<String> {
    let mut file = open(path, TOOL_LIMIT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = file
            .metadata()
            .map_err(|_| "Cosign metadata")?
            .permissions()
            .mode();
        if mode & 0o111 == 0 || mode & 0o6022 != 0 {
            return Err(
                "Cosign must be executable, non-setuid and not group/world writable".into(),
            );
        }
    }
    let mut prefix = [0; 4];
    file.read_exact(&mut prefix)
        .map_err(|_| "invalid Cosign executable")?;
    if prefix != *b"\x7fELF"
        && !matches!(
            prefix,
            [0xcf, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
        )
    {
        return Err(
            "Cosign must be a trusted native Mach-O/ELF executable; scripts are excluded".into(),
        );
    }
    file_hash(path, TOOL_LIMIT)
}
fn authority(path: &Path) -> Result<()> {
    let _file = open(path, TRUST_LIMIT)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if _file
            .metadata()
            .map_err(|_| "Sigstore trust metadata")?
            .permissions()
            .mode()
            & 0o022
            != 0
        {
            return Err(
                "Sigstore policy and public trust-authority files may not be group/world writable"
                    .into(),
            );
        }
    }
    Ok(())
}
fn public_input(bytes: &[u8]) -> Result<()> {
    if bytes
        .windows(b"PRIVATE KEY".len())
        .any(|part| part == b"PRIVATE KEY")
    {
        return Err("Sigstore trust inputs must not contain private keys".into());
    }
    Ok(())
}
fn bundle(bytes: &[u8], chart: &[u8]) -> Result<()> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|_| "invalid Sigstore bundle JSON; content suppressed")?;
    if !matches!(
        value["mediaType"].as_str(),
        Some(
            "application/vnd.dev.sigstore.bundle.v0.3+json"
                | "application/vnd.dev.sigstore.bundle+json;version=0.3"
        )
    ) || value.get("dsseEnvelope").is_some()
    {
        return Err("Sigstore requires a current v0.3 message-signature bundle; legacy bundles and DSSE attestations are not accepted".into());
    }
    let digest = &value["messageSignature"]["messageDigest"];
    let expected = digest["digest"]
        .as_str()
        .and_then(|s| STANDARD.decode(s).ok())
        .ok_or("Sigstore bundle omitted a valid artifact digest")?;
    if digest["algorithm"] != "SHA2_256"
        || expected.as_slice() != ring::digest::digest(&SHA256, chart).as_ref()
    {
        return Err(
            "Sigstore bundle does not bind the exact chart SHA-256; no chart accepted".into(),
        );
    }
    let entries=value["verificationMaterial"]["tlogEntries"].as_array().filter(|values|!values.is_empty()&&values.len()<=16).ok_or("Sigstore bundle requires bounded transparency-log entries; bypass verification is unavailable")?;
    if entries.iter().any(|entry| {
        entry.get("inclusionProof").is_none() && entry.get("inclusionPromise").is_none()
    }) {
        return Err("Sigstore bundle omitted transparency evidence".into());
    }
    Ok(())
}
/// Build private immutable verification inputs. The caller owns/removes the
/// enclosing workspace and must supervise this executable/version/argv before
/// review and again before any lifecycle write, requiring successful exit.
pub fn prepare(chart: &Path, policy_path: &Path, directory: &Path) -> Result<Prepared> {
    authority(policy_path)?;
    let policy_bytes = read(policy_path, POLICY_LIMIT)?;
    let policy:Policy=serde_json::from_slice(&policy_bytes).map_err(|_|"invalid strict schema 1 Sigstore policy; duplicate/unknown fields or malformed trust mode")?;
    if policy.schema != 1 || !hex(&policy.sha256) {
        return Err(
            "Sigstore policy requires schema 1 and exact lowercase verifier SHA-256".into(),
        );
    }
    expiry(policy.expires_at)?;
    let actual = executable(&policy.executable)?;
    if actual != policy.sha256 {
        return Err("Cosign executable SHA-256 does not match explicit policy".into());
    }
    let chart_bytes = read(chart, CHART_LIMIT)?;
    let bundle_bytes = read(&policy.bundle, BUNDLE_LIMIT)?;
    bundle(&bundle_bytes, &chart_bytes)?;
    let (trusted_root, key, identity) = match &policy.trust {
        Trust::PublicKey { key, trusted_root } => (trusted_root, Some(key), None),
        Trust::Identity {
            trusted_root,
            identity,
            issuer,
        } => {
            for text in [identity, issuer] {
                if text.is_empty() || text.len() > 2048 || safety::safe_text(text, 2048) != *text {
                    return Err("Sigstore certificate identity and issuer must be bounded exact nonempty strings".into());
                }
            }
            let url: http::Uri = issuer
                .parse()
                .map_err(|_| "Sigstore OIDC issuer must be an HTTPS URL")?;
            if url.scheme_str() != Some("https")
                || url.authority().is_none()
                || url.authority().is_some_and(|a| a.as_str().contains('@'))
            {
                return Err(
                    "Sigstore OIDC issuer must be a bounded HTTPS URL without userinfo".into(),
                );
            }
            (trusted_root, None, Some((identity, issuer)))
        }
    };
    authority(trusted_root)?;
    let root_bytes = read(trusted_root, TRUST_LIMIT)?;
    public_input(&root_bytes)?;
    let root_value: Value = serde_json::from_slice(&root_bytes)
        .map_err(|_| "invalid Sigstore public trusted-root JSON")?;
    if root_value["mediaType"] != "application/vnd.dev.sigstore.trustedroot+json;version=0.1"
        || root_value["tlogs"]
            .as_array()
            .is_none_or(|logs| logs.is_empty() || logs.len() > 16)
    {
        return Err(
            "Sigstore requires an explicit current public trusted root with transparency-log keys"
                .into(),
        );
    }
    let key_bytes = if let Some(path) = key {
        authority(path)?;
        let bytes = read(path, TRUST_LIMIT)?;
        public_input(&bytes)?;
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| "Sigstore public key must be PEM")?
            .trim();
        if !text.starts_with("-----BEGIN PUBLIC KEY-----\n")
            || !text.ends_with("-----END PUBLIC KEY-----")
            || text.matches("-----BEGIN").count() != 1
        {
            return Err("Sigstore explicit key must contain exactly one PEM PUBLIC KEY".into());
        }
        Some(bytes)
    } else {
        None
    };
    path(directory)?;
    let parent = directory
        .parent()
        .ok_or("Sigstore workspace parent missing")?;
    safety::ensure_private_dir(parent)?;
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(directory)
        .map_err(|_| "Sigstore workspace must be a new private directory")?;
    let build = (|| {
        let mut inputs = Vec::new();
        for (source, name, bytes, limit) in [
            (
                policy_path,
                "policy.json",
                policy_bytes.as_slice(),
                POLICY_LIMIT,
            ),
            (chart, "chart.tgz", chart_bytes.as_slice(), CHART_LIMIT),
            (
                &policy.bundle,
                "bundle.sigstore.json",
                bundle_bytes.as_slice(),
                BUNDLE_LIMIT,
            ),
            (
                trusted_root,
                "trusted-root.json",
                root_bytes.as_slice(),
                TRUST_LIMIT,
            ),
        ] {
            let sealed = directory.join(name);
            safety::write_new(&sealed, bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&sealed, fs::Permissions::from_mode(0o400))
                    .map_err(|_| "cannot seal Sigstore input")?;
            }
            inputs.push(Input {
                source: source.into(),
                sealed,
                sha256: hash(bytes),
                limit,
            });
        }
        let mut argv = vec![
            "verify-blob".into(),
            "--bundle".into(),
            directory
                .join("bundle.sigstore.json")
                .to_string_lossy()
                .into_owned(),
            "--trusted-root".into(),
            directory
                .join("trusted-root.json")
                .to_string_lossy()
                .into_owned(),
            "--offline=true".into(),
        ];
        let trust_summary = if let (Some(key), Some(bytes)) = (key, key_bytes) {
            let sealed = directory.join("trusted-public.pem");
            safety::write_new(&sealed, &bytes)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&sealed, fs::Permissions::from_mode(0o400))
                    .map_err(|_| "cannot seal Sigstore public key")?;
            }
            inputs.push(Input {
                source: key.into(),
                sealed: sealed.clone(),
                sha256: hash(&bytes),
                limit: TRUST_LIMIT,
            });
            argv.extend(["--key".into(), sealed.to_string_lossy().into_owned()]);
            "Trust: explicit public-key signature AND transparency evidence; no certificate identity assertion.\n".into()
        } else if let Some((identity, issuer)) = identity {
            argv.extend([
                "--certificate-identity".into(),
                identity.clone(),
                "--certificate-oidc-issuer".into(),
                issuer.clone(),
            ]);
            format!("Exact certificate identity: {identity}\nExact OIDC issuer: {issuer}\n")
        } else {
            return Err("Sigstore trust selection missing".into());
        };
        argv.push(directory.join("chart.tgz").to_string_lossy().into_owned());
        Ok(Prepared {
            executable: policy.executable,
            executable_hash: actual,
            inputs,
            root: directory.into(),
            argv,
            trust_summary,
            expires_at: policy.expires_at,
            created: Instant::now(),
        })
    })();
    if build.is_err() {
        let _ = fs::remove_dir_all(directory);
    }
    build
}

/// Both invocations are supervised with scrubbed environment and a private
/// verification-input-only cwd without Kubernetes configuration. Dropping this future signals and reaps its process
/// group before the caller removes the enclosing workspace.
pub async fn verify(prepared: &Prepared) -> Result<()> {
    prepared.check()?;
    let version = invoke(prepared, vec!["version".into(), "--json".into()]).await?;
    validate_version(&version)?;
    invoke(prepared, prepared.argv()).await?;
    prepared.check()?;
    Ok(())
}
async fn invoke(prepared: &Prepared, argv: Vec<String>) -> Result<Vec<u8>> {
    use std::sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    };
    let stop = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicBool::new(false));
    struct Guard(Arc<AtomicBool>, Arc<AtomicBool>);
    impl Drop for Guard {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
            let until = Instant::now() + Duration::from_secs(3);
            while !self.1.load(Ordering::Acquire) && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
    struct Done(Arc<AtomicBool>);
    impl Drop for Done {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Release);
        }
    }
    let _guard = Guard(stop.clone(), done.clone());
    let prepared = prepared.clone();
    tokio::task::spawn_blocking(move || {
        let _done = Done(done);
        supervise(&prepared, &argv, Duration::from_secs(15), stop)
    })
    .await
    .map_err(|_| "Sigstore verifier supervision worker failed".to_owned())?
}
#[cfg(not(unix))]
fn supervise(
    _: &Prepared,
    _: &[String],
    _: Duration,
    _: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<Vec<u8>> {
    Err("Sigstore native process cleanup is unsupported on this platform".into())
}
#[cfg(unix)]
fn supervise(
    prepared: &Prepared,
    argv: &[String],
    budget: Duration,
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
) -> Result<Vec<u8>> {
    use std::{
        os::unix::process::CommandExt,
        process::{Command, Stdio},
        sync::{atomic::Ordering, mpsc},
        thread,
    };
    if stop.load(Ordering::Acquire) {
        return Err("Sigstore verification canceled before spawn".into());
    }
    prepared.check()?;
    let killer = Path::new("/bin/kill")
        .canonicalize()
        .map_err(|_| "Sigstore process-group cleanup unavailable")?;
    let _ = executable(&killer)?;
    if stop.load(Ordering::Acquire) {
        return Err("Sigstore verification canceled before spawn".into());
    }
    let child = Command::new(&prepared.executable)
        .args(argv)
        .env_clear()
        .current_dir(&prepared.root)
        .env("HOME", prepared.root.join("home"))
        .env("TMPDIR", &prepared.root)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("SIGSTORE_NO_CACHE", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|_| "cannot start explicitly trusted Cosign verifier")?;
    struct Process {
        child: std::process::Child,
        killer: PathBuf,
        cleaned: bool,
    }
    impl Process {
        fn cleanup(&mut self) -> Result<()> {
            if self.cleaned {
                return Ok(());
            }
            let mut failure = None;
            match Command::new(&self.killer)
                .env_clear()
                .current_dir("/")
                .args(["-KILL", "--", &format!("-{}", self.child.id())])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(mut signal) => {
                    let until = Instant::now() + Duration::from_secs(1);
                    loop {
                        match signal.try_wait() {
                            Ok(Some(_)) => break,
                            Ok(None) if Instant::now() < until => {
                                thread::sleep(Duration::from_millis(5))
                            }
                            _ => {
                                let _ = signal.kill();
                                let _ = signal.try_wait();
                                failure = Some("Sigstore group cleanup failed");
                                break;
                            }
                        }
                    }
                }
                Err(_) => failure = Some("cannot signal Sigstore process group"),
            }
            let _ = self.child.kill();
            let until = Instant::now() + Duration::from_secs(1);
            loop {
                match self.child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < until => thread::sleep(Duration::from_millis(5)),
                    _ => {
                        failure = Some("cannot confirm Cosign process reaping");
                        break;
                    }
                }
            }
            self.cleaned = true;
            if let Some(error) = failure {
                Err(error.into())
            } else {
                Ok(())
            }
        }
    }
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.cleanup();
        }
    }
    let mut process = Process {
        child,
        killer,
        cleaned: false,
    };
    let stdout = process
        .child
        .stdout
        .take()
        .ok_or("Cosign stdout unavailable")?;
    let stderr = process
        .child
        .stderr
        .take()
        .ok_or("Cosign stderr unavailable")?;
    let (tx, rx) = mpsc::sync_channel::<(bool, Result<Vec<u8>>)>(8);
    let reader = |mut stream: Box<dyn Read + Send>,
                  out: bool,
                  tx: mpsc::SyncSender<(bool, Result<Vec<u8>>)>| {
        thread::spawn(move || {
            let mut buffer = [0; 8192];
            loop {
                match stream.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(count) => {
                        if tx.send((out, Ok(buffer[..count].to_vec()))).is_err() {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = tx.send((out, Err("Cosign output read failed".into())));
                        break;
                    }
                }
            }
        })
    };
    let _stdout = reader(Box::new(stdout), true, tx.clone());
    let _stderr = reader(Box::new(stderr), false, tx.clone());
    drop(tx);
    let until = Instant::now() + budget;
    let mut bytes = 0usize;
    let mut captured = Vec::new();
    let mut exited = None;
    let mut disconnected = false;
    let result = (|| {
        loop {
            if stop.load(Ordering::Acquire) {
                return Err("Sigstore verification canceled; no lifecycle write sent".into());
            }
            if Instant::now() >= until {
                return Err("Sigstore verifier exceeded 15 seconds; no automatic retry".into());
            }
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok((stdout, data)) => {
                    let data = data?;
                    bytes = bytes.saturating_add(data.len());
                    if bytes > 256 * 1024 {
                        return Err(
                            "Cosign verifier output exceeded 256 KiB; diagnostics suppressed"
                                .into(),
                        );
                    }
                    if stdout {
                        captured.extend(data);
                    }
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => disconnected = true,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if exited.is_none() {
                exited = process
                    .child
                    .try_wait()
                    .map_err(|_| "cannot inspect Cosign status")?;
            }
            if let Some(status) = exited {
                // Close descendants even if they retained stdout/stderr. Wait
                // only within the same finite output/time budget for readers.
                process.cleanup()?;
                if disconnected {
                    return if status.success() {
                        Ok(captured)
                    } else {
                        Err("Cosign rejected the chart signature, transparency evidence, trust root, identity or validity; no lifecycle write sent; private diagnostics suppressed".into())
                    };
                }
            } else if disconnected {
                thread::sleep(Duration::from_millis(5));
            }
        }
    })();
    process.cleanup()?;
    drop(rx);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicU64, Ordering};
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Result<Self> {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir()
                .canonicalize()
                .map_err(|_| "temp")?
                .join(format!(
                    "kborg-sigstore-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            safety::ensure_private_dir(&root)?;
            Ok(Self(root))
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn bundle_value(chart: &[u8]) -> Value {
        json!({"mediaType":"application/vnd.dev.sigstore.bundle.v0.3+json","messageSignature":{"messageDigest":{"algorithm":"SHA2_256","digest":STANDARD.encode(digest(&SHA256,chart).as_ref())},"signature":"structural-fixture-only"},"verificationMaterial":{"tlogEntries":[{"inclusionProof":{}}]}})
    }
    fn fixture(root: &Path) -> Result<(PathBuf, PathBuf)> {
        let chart = root.join("chart.tgz");
        let bundle = root.join("bundle.json");
        let trust = root.join("root.json");
        let policy = root.join("policy.json");
        safety::write_new(&chart, b"fixture chart")?;
        safety::write_new(
            &bundle,
            bundle_value(b"fixture chart").to_string().as_bytes(),
        )?;
        safety::write_new(&trust,json!({"mediaType":"application/vnd.dev.sigstore.trustedroot+json;version=0.1","tlogs":[{}]}).to_string().as_bytes())?;
        let executable = std::env::current_exe().map_err(|_| "test exe")?;
        safety::write_new(&policy,json!({"schema":1,"executable":executable,"sha256":file_hash(&executable,TOOL_LIMIT)?,"bundle":bundle,"trust":{"mode":"identity","trusted_root":trust,"identity":"fixture@invalid.example","issuer":"https://issuer.invalid.example"}}).to_string().as_bytes())?;
        Ok((chart, policy))
    }
    #[cfg(unix)]
    #[test]
    fn strict_policy_rejects_unknown_duplicate_expired_and_weak_trust_fields() -> Result<()> {
        let root = Temp::new()?;
        let (chart, path) = fixture(&root.0)?;
        let original = read(&path, POLICY_LIMIT)?;
        for mode in 0..6 {
            let mut policy: Value = serde_json::from_slice(&original).map_err(|_| "fixture")?;
            match mode {
                0 => policy["insecure_ignore_tlog"] = json!(true),
                1 => policy["expires_at"] = json!(1),
                2 => policy["trust"]["issuer"] = json!(""),
                3 => policy["trust"]["identity_regexp"] = json!(".*"),
                4 => policy["trust"]["trusted_root"] = Value::Null,
                _ => policy["sha256"] = json!("00"),
            };
            fs::write(&path, policy.to_string()).map_err(|_| "fixture")?;
            assert!(
                prepare(&chart, &path, &root.0.join("verify")).is_err(),
                "{mode}"
            );
            assert!(!root.0.join("verify").exists());
        }
        assert!(serde_json::from_str::<Policy>("{\"schema\":1,\"schema\":1}").is_err());
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn staging_binds_source_private_bytes_and_only_safe_fixed_arguments() -> Result<()> {
        let root = Temp::new()?;
        let (chart, policy) = fixture(&root.0)?;
        let prepared = prepare(&chart, &policy, &root.0.join("verify"))?;
        prepared.check()?;
        assert!(
            prepared
                .argv()
                .iter()
                .all(|arg| !arg.starts_with("--insecure") && !arg.contains("regexp"))
        );
        assert!(prepared.summary().contains("NOT an OCI manifest"));
        assert!(!prepared.working_dir().join("connection.json").exists());
        fs::write(root.0.join("bundle.json"), b"changed").map_err(|_| "fixture")?;
        assert!(prepared.check().is_err());
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn trust_authority_permissions_are_checked_before_staging_and_after_review() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let root = Temp::new()?;
        let (chart, policy) = fixture(&root.0)?;
        for target in [&policy, &root.0.join("root.json")] {
            fs::set_permissions(target, fs::Permissions::from_mode(0o666)).map_err(|_| "chmod")?;
            assert!(prepare(&chart, &policy, &root.0.join("refused")).is_err());
            assert!(!root.0.join("refused").exists());
            fs::set_permissions(target, fs::Permissions::from_mode(0o600)).map_err(|_| "chmod")?;
        }
        let prepared = prepare(&chart, &policy, &root.0.join("verify"))?;
        fs::set_permissions(root.0.join("root.json"), fs::Permissions::from_mode(0o620))
            .map_err(|_| "chmod")?;
        assert!(prepared.check().is_err());
        fs::set_permissions(root.0.join("root.json"), fs::Permissions::from_mode(0o600))
            .map_err(|_| "chmod")?;
        let key = root.0.join("public.pem");
        safety::write_new(
            &key,
            b"-----BEGIN PUBLIC KEY-----\nU1RSVUNUVVJBTC1GS VhUVVJF\n-----END PUBLIC KEY-----\n",
        )?;
        let mut value: Value =
            serde_json::from_slice(&read(&policy, POLICY_LIMIT)?).map_err(|_| "fixture")?;
        value["trust"] =
            json!({"mode":"public-key","key":key,"trusted_root":root.0.join("root.json")});
        fs::write(&policy, value.to_string()).map_err(|_| "fixture")?;
        fs::set_permissions(&key, fs::Permissions::from_mode(0o622)).map_err(|_| "chmod")?;
        assert!(prepare(&chart, &policy, &root.0.join("key-refused")).is_err());
        assert!(!root.0.join("key-refused").exists());
        fs::set_permissions(&key, fs::Permissions::from_mode(0o600)).map_err(|_| "chmod")?;
        let public = prepare(&chart, &policy, &root.0.join("key-verified"))?;
        fs::set_permissions(&key, fs::Permissions::from_mode(0o666)).map_err(|_| "chmod")?;
        assert!(public.check().is_err());
        Ok(())
    }
    #[test]
    fn bundle_subject_requires_current_sha256_message_and_transparency() -> Result<()> {
        let good = bundle_value(b"chart");
        bundle(good.to_string().as_bytes(), b"chart")?;
        for mode in 0..5 {
            let mut value = good.clone();
            match mode {
                0 => {
                    value["mediaType"] =
                        json!("application/vnd.dev.sigstore.bundle+json;version=0.1")
                }
                1 => value["dsseEnvelope"] = json!({}),
                2 => value["messageSignature"]["messageDigest"]["algorithm"] = json!("SHA2_512"),
                3 => value["verificationMaterial"]["tlogEntries"] = json!([]),
                _ => value["verificationMaterial"]["tlogEntries"] = json!([{}]),
            };
            assert!(bundle(value.to_string().as_bytes(), b"chart").is_err());
        }
        assert!(bundle(good.to_string().as_bytes(), b"different chart").is_err());
        Ok(())
    }
    #[test]
    fn verifier_version_is_exact_and_private_keys_are_refused() -> Result<()> {
        validate_version(br#"{"gitVersion":"v3.1.3"}"#)?;
        for value in [
            br#"{"gitVersion":"v3.1.2"}"#.as_slice(),
            b"v3.1.3",
            br#"{"gitVersion":"v3.1.3-evil"}"#,
        ] {
            assert!(validate_version(value).is_err());
        }
        assert!(public_input(b"-----BEGIN PRIVATE KEY-----").is_err());
        Ok(())
    }
    #[test]
    fn bounded_file_reads_reject_symlinks_and_named_pipes() -> Result<()> {
        let root = Temp::new()?;
        let path = root.0.join("file");
        safety::write_new(&path, b"small")?;
        assert!(read(&path, 4).is_err());
        #[cfg(unix)]
        {
            let link = root.0.join("link");
            std::os::unix::fs::symlink(&path, &link).map_err(|_| "symlink")?;
            assert!(read(&link, 100).is_err());
            let fifo = root.0.join("pipe");
            let status = std::process::Command::new("/usr/bin/mkfifo")
                .arg(&fifo)
                .status()
                .map_err(|_| "mkfifo")?;
            assert!(status.success());
            assert!(read(&fifo, 100).is_err());
        }
        Ok(())
    }
    #[test]
    #[ignore = "isolated subprocess helper"]
    fn process_fixture() -> Result<()> {
        use std::io::Write;
        let cwd = std::env::current_dir().map_err(|_| "cwd")?;
        let name = cwd.file_name().and_then(|s| s.to_str()).ok_or("cwd")?;
        fs::write(cwd.join("pid"), std::process::id().to_string()).map_err(|_| "pid")?;
        assert!(std::env::var_os("KUBECONFIG").is_none());
        assert!(std::env::var_os("GH_TOKEN").is_none());
        assert!(std::env::var_os("AWS_SECRET_ACCESS_KEY").is_none());
        if name == "flood" {
            for _ in 0..100 {
                std::io::stdout()
                    .write_all(&[b'x'; 8192])
                    .map_err(|_| "stdout")?;
            }
        } else if name.starts_with("quiet") {
            std::thread::sleep(Duration::from_secs(60));
        } else {
            println!("credential_environment_empty=true");
        }
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn native_boundary_scrubs_credentials_limits_output_and_reaps_on_timeout() -> Result<()> {
        use std::sync::{Arc, atomic::AtomicBool};
        let root = Temp::new()?;
        let executable = std::env::current_exe().map_err(|_| "exe")?;
        let args = [
            "--ignored",
            "--exact",
            "helm_sigstore::tests::process_fixture",
            "--nocapture",
        ]
        .map(str::to_owned);
        for mode in ["normal", "flood", "quiet"] {
            let cwd = root.0.join(mode);
            safety::ensure_private_dir(&cwd)?;
            let prepared = Prepared {
                executable: executable.clone(),
                executable_hash: file_hash(&executable, TOOL_LIMIT)?,
                inputs: vec![],
                root: cwd.clone(),
                argv: vec![],
                trust_summary: String::new(),
                expires_at: None,
                created: Instant::now(),
            };
            let result = supervise(
                &prepared,
                &args,
                Duration::from_millis(500),
                Arc::new(AtomicBool::new(false)),
            );
            match mode {
                "normal" => assert!(
                    String::from_utf8_lossy(&result?).contains("credential_environment_empty=true")
                ),
                "flood" => assert!(result.err().is_some_and(|error| error.contains("256 KiB"))),
                _ => {
                    assert!(
                        result
                            .err()
                            .is_some_and(|error| error.contains("15 seconds"))
                    );
                    let pid = fs::read_to_string(cwd.join("pid")).map_err(|_| "pid")?;
                    let alive = std::process::Command::new("/bin/kill")
                        .args(["-0", pid.trim()])
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .status()
                        .map_err(|_| "kill probe")?;
                    assert!(!alive.success());
                }
            }
        }
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn dropping_async_verification_reaps_the_active_child_before_returning() -> Result<()> {
        let root = Temp::new()?;
        let cwd = root.0.join("quiet-cancel");
        safety::ensure_private_dir(&cwd)?;
        let executable = std::env::current_exe().map_err(|_| "exe")?;
        let prepared = Prepared {
            executable: executable.clone(),
            executable_hash: file_hash(&executable, TOOL_LIMIT)?,
            inputs: vec![],
            root: cwd.clone(),
            argv: vec![],
            trust_summary: String::new(),
            expires_at: None,
            created: Instant::now(),
        };
        let argv = [
            "--ignored",
            "--exact",
            "helm_sigstore::tests::process_fixture",
            "--nocapture",
        ]
        .map(str::to_owned)
        .to_vec();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "runtime")?;
        let started = Instant::now();
        runtime.block_on(async {
            let future=invoke(&prepared,argv);tokio::pin!(future);
            let running=async {let until=Instant::now()+Duration::from_secs(2);while !cwd.join("pid").exists(){if Instant::now()>=until{return Err("Sigstore fixture child did not start");}tokio::time::sleep(Duration::from_millis(5)).await;}Ok(())};
            tokio::select!{result=&mut future=>return Err(format!("quiet verification ended before cancellation: {result:?}")),ready=running=>ready.map_err(str::to_owned)?};
            // Leaving this scope drops the actual pinned verification future.
            Ok::<_,String>(())
        })?;
        assert!(started.elapsed() < Duration::from_secs(4));
        let pid = fs::read_to_string(cwd.join("pid")).map_err(|_| "pid")?;
        let alive = std::process::Command::new("/bin/kill")
            .args(["-0", pid.trim()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|_| "kill probe")?;
        assert!(!alive.success());
        Ok(())
    }
    #[test]
    #[ignore = "requires generated real fixtures via KBORG_SIGSTORE_FIXTURES and KBORG_SIGSTORE_IDENTITY_FIXTURES"]
    fn authentic_sigstore_fixture_positive_and_cryptographic_negatives() -> Result<()> {
        let public = PathBuf::from(
            std::env::var("KBORG_SIGSTORE_FIXTURES").map_err(|_| "public fixture path missing")?,
        );
        let identity = PathBuf::from(
            std::env::var("KBORG_SIGSTORE_IDENTITY_FIXTURES")
                .map_err(|_| "identity fixture path missing")?,
        );
        let root = Temp::new()?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "runtime")?;
        let original: Value =
            serde_json::from_slice(&read(&public.join("first.policy.json"), POLICY_LIMIT)?)
                .map_err(|_| "fixture")?;
        for (index, name) in [
            "public-key",
            "wrong-key",
            "wrong-root",
            "tampered-signature",
        ]
        .iter()
        .enumerate()
        {
            let mut policy = original.clone();
            if *name == "wrong-key" {
                policy["trust"]["key"] = json!(public.join("chart-1.pub"));
            }
            if *name == "wrong-root" {
                policy["trust"]["trusted_root"] = json!(public.join("wrong-root.json"));
            }
            if *name == "tampered-signature" {
                let bundle_path = PathBuf::from(policy["bundle"].as_str().ok_or("bundle")?);
                let mut value: Value = serde_json::from_slice(&read(&bundle_path, BUNDLE_LIMIT)?)
                    .map_err(|_| "bundle")?;
                let mut signature = STANDARD
                    .decode(
                        value["messageSignature"]["signature"]
                            .as_str()
                            .ok_or("sig")?,
                    )
                    .map_err(|_| "signature")?;
                *signature.last_mut().ok_or("signature")? ^= 1;
                value["messageSignature"]["signature"] = json!(STANDARD.encode(signature));
                let path = root.0.join("tampered.bundle.json");
                safety::write_new(&path, value.to_string().as_bytes())?;
                policy["bundle"] = json!(path);
            }
            let path = root.0.join(format!("{name}.policy.json"));
            safety::write_new(&path, policy.to_string().as_bytes())?;
            let prepared = prepare(
                &public.join("first.tgz"),
                &path,
                &root.0.join(format!("verify-{index}")),
            )?;
            let result = runtime.block_on(verify(&prepared));
            assert_eq!(result.is_ok(), *name == "public-key", "{name}: {result:?}");
        }
        for (index, name) in ["identity", "wrong-identity", "wrong-issuer"]
            .iter()
            .enumerate()
        {
            let prepared = prepare(
                &identity.join("chart.tgz"),
                &identity.join(format!("{name}.policy.json")),
                &root.0.join(format!("identity-{index}")),
            )?;
            let result = runtime.block_on(verify(&prepared));
            assert_eq!(result.is_ok(), *name == "identity", "{name}: {result:?}");
        }
        Ok(())
    }
}
