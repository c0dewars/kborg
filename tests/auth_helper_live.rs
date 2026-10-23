//! Live local ExecCredential acceptance using only the harness's scoped reader.
//! The provider is a fixed native fixture, not a claim of cloud-account login.
#![cfg(unix)]

use base64::{Engine, engine::general_purpose::STANDARD};
use kborg::{
    Result, auth,
    cli::runtime,
    gateway::{Connection, Gateway, review_auth_helper},
    safety,
};
use serde_json::{Value, json};
use std::{
    fs,
    io::{Seek, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

// No credentials are compiled into this executable. It only emits the private
// fixture response on stdout, which the product captures as strict JSON.
const HELPER_SOURCE: &str = r#"
use std::{fs::{File, OpenOptions}, io::{Read, Write}, os::unix::fs::OpenOptionsExt, path::PathBuf};
fn run() -> Result<(), ()> {
    let home = PathBuf::from(std::env::var_os("HOME").ok_or(())?);
    let info = std::env::var("KUBERNETES_EXEC_INFO").map_err(|_| ())?;
    if !info.contains("client.authentication.k8s.io/v1") || !info.contains("\"interactive\":false") {
        return Err(());
    }
    // Verify the fixture receives the reviewed minimum environment rather than
    // inheriting a kubeconfig or cloud credential variable from the test host.
    if std::env::vars_os().any(|(key, _)| !matches!(key.to_str(), Some("HOME" | "USERPROFILE" | "TMPDIR" | "LANG" | "LC_ALL" | "KUBERNETES_EXEC_INFO" | "__CF_USER_TEXT_ENCODING"))) {
        return Err(());
    }
    let mut count = OpenOptions::new().append(true).create(true).mode(0o600)
        .open(home.join("invocations")).map_err(|_| ())?;
    count.write_all(b"invoked\n").map_err(|_| ())?;
    let mut bytes = Vec::new();
    File::open(home.join("exec-response.json")).map_err(|_| ())?
        .take(16_385).read_to_end(&mut bytes).map_err(|_| ())?;
    if bytes.is_empty() || bytes.len() > 16_384 { return Err(()); }
    std::io::stdout().lock().write_all(&bytes).map_err(|_| ())?;
    bytes.fill(0);
    Ok(())
}
fn main() { if run().is_err() { std::process::exit(1); } }
"#;

struct PrivateFixture(PathBuf);
impl PrivateFixture {
    fn create() -> Result<Self> {
        let root = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "local auth fixture temporary root unavailable")?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "local auth fixture clock unavailable")?
            .as_nanos();
        let path = root.join(format!("kborg-auth-live-{}-{stamp}", std::process::id()));
        safety::ensure_private_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for PrivateFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn compile_helper(directory: &Path) -> Result<PathBuf> {
    let compiler = PathBuf::from(
        std::env::var_os("KBORG_AUTH_FIXTURE_RUSTC")
            .ok_or("harness must supply the explicitly trusted auth-fixture compiler")?,
    );
    if !compiler.is_absolute() {
        return Err("auth-fixture compiler must be an explicit absolute path".into());
    }
    // Keep the trusted shim's argv[0]: rustup selects rustc from the shim name.
    // Canonicalizing to the rustup binary would instead invoke its CLI parser.
    if !fs::metadata(&compiler)
        .map_err(|_| "trusted auth-fixture compiler unavailable")?
        .is_file()
    {
        return Err("trusted auth-fixture compiler must be a file".into());
    }
    let source = directory.join("provider.rs");
    safety::write_new(&source, HELPER_SOURCE.as_bytes())?;
    let executable = directory.join("provider");
    let mut child = Command::new(compiler)
        .args(["--edition=2024", "--crate-name", "kborg_auth_fixture"])
        .arg(&source)
        .arg("-o")
        .arg(&executable)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "cannot start trusted auth-fixture compiler")?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => {
                return Err(
                    "native auth fixture compilation failed; diagnostics suppressed".into(),
                );
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("native auth fixture compilation timed out or failed".into());
            }
        }
    }
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))
        .map_err(|_| "cannot protect native auth fixture")?;
    Ok(executable)
}
fn write_config(path: &Path, configuration: &Value) -> Result<()> {
    let bytes =
        serde_json::to_vec(configuration).map_err(|_| "cannot encode auth fixture config")?;
    safety::write_new(path, &bytes)
}
fn invocation_count(directory: &Path) -> Result<usize> {
    let path = directory.join("invocations");
    if !path.exists() {
        return Ok(0);
    }
    let bytes = safety::read_small(&path, 1024)?;
    Ok(bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .count())
}

#[test]
#[ignore = "requires uniquely owned Docker Desktop reader fixtures; scripts/test-local.py"]
fn reviewed_native_exec_credentials_connect_revoke_reconnect_and_invalidate_trust() -> Result<()> {
    let fixture = PrivateFixture::create()?;
    let fixture_path = fixture.0.clone();
    let result = (|| -> Result<()> {
        let reader_path = PathBuf::from(
            std::env::var_os("KBORG_TEST_READER_CONFIG")
                .ok_or("missing owned local reader fixture config")?,
        );
        let mut configuration: Value =
            serde_json::from_slice(&safety::read_small(&reader_path, 1024 * 1024)?)
                .map_err(|_| "owned reader fixture config is invalid")?;
        if configuration["contexts"].as_array().map(Vec::len) != Some(1)
            || configuration["clusters"].as_array().map(Vec::len) != Some(1)
            || configuration["users"].as_array().map(Vec::len) != Some(1)
            || configuration["current-context"] != "kborg-test"
        {
            return Err("auth acceptance requires the isolated single-reader fixture".into());
        }
        let namespace = configuration["contexts"][0]["context"]["namespace"]
            .as_str()
            .filter(|namespace| {
                namespace.starts_with("kborg-tests-") && safety::valid_id(namespace)
            })
            .ok_or("owned auth acceptance namespace is invalid")?
            .to_owned();
        let token = configuration["users"][0]["user"]["token"]
            .as_str()
            .filter(|token| !token.is_empty() && token.len() < 12 * 1024)
            .ok_or("scoped fixture reader token is missing or oversized")?
            .to_owned();
        let executable = compile_helper(&fixture.0)?;
        let expires = k8s_openapi::jiff::Timestamp::from_second(
            k8s_openapi::jiff::Timestamp::now().as_second() + 120,
        )
        .map_err(|_| "cannot create bounded fixture credential expiry")?
        .to_string();
        let response = json!({
            "apiVersion":"client.authentication.k8s.io/v1", "kind":"ExecCredential",
            "status":{"token":token,"expirationTimestamp":expires}
        });
        safety::write_new(
            &fixture.0.join("exec-response.json"),
            &serde_json::to_vec(&response).map_err(|_| "cannot encode private fixture response")?,
        )?;
        configuration["users"][0]["user"] = json!({"exec":{
            "apiVersion":"client.authentication.k8s.io/v1", "command":executable,
            "interactiveMode":"Never", "provideClusterInfo":true
        }});
        let helper_config = fixture.0.join("helper.kubeconfig");
        write_config(&helper_config, &configuration)?;
        let policy = fixture.0.join("policy.json");
        let options = Connection {
            kubeconfig: Some(helper_config),
            context: Some("kborg-test".into()),
            auth_helper_policy: Some(policy.clone()),
            allow_auth_helper: true,
            ..Default::default()
        };
        let registration = review_auth_helper(
            &options,
            Some(&executable),
            &[("HOME".into(), fixture.0.to_string_lossy().into_owned())],
        )?;
        assert!(
            !registration.summary().contains(&token),
            "review exposed fixture credentials"
        );
        registration.save(&policy, auth::CONFIRMATION)?;
        let manifest = safety::read_small(&policy, 64 * 1024)?;
        assert!(
            !String::from_utf8_lossy(&manifest).contains(&token),
            "policy saved fixture credentials"
        );
        assert_eq!(invocation_count(&fixture.0)?, 0);
        runtime()?.block_on(async {
            let mut disabled = options.clone();
            disabled.allow_auth_helper = false;
            assert!(Gateway::connect(disabled).await.is_err());
            assert_eq!(invocation_count(&fixture.0)?, 0);
            let gateway = Gateway::connect(options.clone()).await?;
            let pods = gateway.list("pods", &namespace).await?;
            assert!(
                pods["items"]
                    .as_array()
                    .is_some_and(|pods| !pods.is_empty())
            );
            assert_eq!(invocation_count(&fixture.0)?, 1);
            let denied = gateway
                .list("pods", "kube-system")
                .await
                .err()
                .ok_or("scoped reader unexpectedly read another namespace")?;
            assert!(denied.contains("403"));
            assert!(!denied.contains(&token));
            let blocked = gateway
                .list("pods", &namespace)
                .await
                .err()
                .ok_or("revoked helper session sent another authorized read")?;
            assert!(blocked.contains("401"));
            assert!(!blocked.contains(&token));
            assert_eq!(
                invocation_count(&fixture.0)?,
                1,
                "helper refreshed without explicit reconnect"
            );
            let reconnected = Gateway::connect(options.clone()).await?;
            assert!(reconnected.list("pods", &namespace).await.is_ok());
            assert_eq!(invocation_count(&fixture.0)?, 2);
            for (name, replacement) in [
                ("server", json!("https://127.0.0.1:1")),
                (
                    "certificate-authority-data",
                    json!(STANDARD.encode(b"CHANGED-TEST-CA")),
                ),
            ] {
                let mut changed = configuration.clone();
                changed["clusters"][0]["cluster"][name] = replacement;
                let path = fixture.0.join(format!("changed-{name}.kubeconfig"));
                write_config(&path, &changed)?;
                let mut changed_options = options.clone();
                changed_options.kubeconfig = Some(path);
                let error = Gateway::connect(changed_options)
                    .await
                    .err()
                    .ok_or("changed cluster trust was accepted")?;
                assert!(error.contains("trust binding changed"));
                assert!(!error.contains(&token));
                assert_eq!(
                    invocation_count(&fixture.0)?,
                    2,
                    "changed cluster trust invoked helper"
                );
            }
            let last_byte = safety::read_small(&executable, 128 * 1024 * 1024)?
                .last()
                .copied()
                .ok_or("native fixture empty")?;
            let mtime = fs::metadata(&executable)
                .and_then(|metadata| metadata.modified())
                .map_err(|_| "native fixture metadata unavailable")?;
            let mut file = fs::OpenOptions::new()
                .write(true)
                .open(&executable)
                .map_err(|_| "cannot alter native fixture")?;
            file.seek(std::io::SeekFrom::End(-1))
                .and_then(|_| file.write_all(&[last_byte ^ 0xff]))
                .and_then(|_| file.set_times(fs::FileTimes::new().set_modified(mtime)))
                .map_err(|_| "cannot alter native fixture")?;
            let changed = Gateway::connect(options)
                .await
                .err()
                .ok_or("changed executable trust was accepted")?;
            assert!(changed.contains("executable identity changed"));
            assert!(!changed.contains(&token));
            assert_eq!(
                invocation_count(&fixture.0)?,
                2,
                "changed executable invoked helper"
            );
            Ok(())
        })
    })();
    drop(fixture);
    assert!(
        !fixture_path.exists(),
        "private auth fixture credentials were not removed"
    );
    result
}
