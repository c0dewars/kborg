//! Contained trusted Copilot CLI process; no ambient provider authority is used.
use super::{AdviceOutput, Options, PreparedAdvice, process, proxy};
use crate::{Result, safety};
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
pub(super) struct Workspace(PathBuf);
impl Workspace {
    fn cleanup(&self) -> Result<()> {
        fs::remove_dir_all(&self.0).map_err(|_|"Copilot private workspace cleanup failed; inspect local temporary storage before retrying".to_owned())
    }
    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}
pub(super) fn credential_workspace() -> Result<Workspace> {
    create_workspace()
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
fn quote(path: &Path) -> Result<String> {
    serde_json::to_string(&path.to_string_lossy())
        .map_err(|_| "cannot encode Copilot containment path".into())
}
fn profile(executables: &[&Path], state: &Path, port: u16) -> Result<String> {
    let literals = executables
        .iter()
        .map(|path| quote(path).map(|path| format!("(literal {path})")))
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    let metadata = executables
        .iter()
        .map(|path| quote(path).map(|path| format!("(path-ancestors {path})")))
        .collect::<Result<Vec<_>>>()?
        .join(" ");
    Ok(format!(
        "(version 1)(deny default)(import \"system.sb\")(deny mach-lookup)(deny network*)(deny process-fork)(deny file-read*)(deny file-write*)(deny file-map-executable)\n(allow process-exec {literals})(allow file-map-executable {literals} (subpath \"/System/Library\") (subpath \"/usr/lib\") (subpath {}))(allow sysctl-read)\n(allow file-read* {literals} (subpath \"/System/Library\") (subpath \"/usr/lib\") (subpath \"/usr/share/locale\") (subpath \"/private/var/db/timezone\") (literal \"/dev/null\") (literal \"/dev/random\") (literal \"/dev/urandom\") (subpath {}))\n(allow file-read-metadata {metadata} (path-ancestors {}))\n(allow file-write* (subpath {}))(allow network-outbound (remote tcp \"localhost:{port}\"))\n",
        quote(&state.join("Library/Caches/copilot/pkg/darwin-arm64/1.0.91"))?,
        quote(state)?,
        quote(state)?,
        quote(state)?
    ))
}
fn create_workspace() -> Result<Workspace> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut random = [0u8; 16];
    SystemRandom::new()
        .fill(&mut random)
        .map_err(|_| "cannot create private Copilot workspace nonce")?;
    let suffix = random
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let root = std::env::temp_dir()
        .canonicalize()
        .map_err(|_| "Copilot temporary root unavailable")?;
    let workspace = Workspace(root.join(format!("kborg-copilot-{}-{suffix}", std::process::id())));
    safety::ensure_private_dir(&workspace.0)?;
    Ok(workspace)
}
fn environment(command: &mut Command, state: &Path) {
    command
        .env_clear()
        .env("PATH", "")
        .env("HOME", state)
        .env("TMPDIR", state)
        .env("COPILOT_HOME", state)
        .env("GH_CONFIG_DIR", state)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("COPILOT_AUTO_UPDATE", "false")
        .env("COPILOT_DISABLE_TELEMETRY", "true")
        .env("DO_NOT_TRACK", "1")
        .current_dir(state);
}
/// Host selection and credential delivery per deployment. GHES uses the
/// documented offline provider mode; its token is only the provider token.
fn deployment_environment(
    command: &mut Command,
    deployment: &crate::copilot::Deployment,
    token: &str,
    state: &Path,
    ca_file: Option<&Path>,
) -> Result<()> {
    use crate::copilot::Deployment;
    match deployment {
        Deployment::GitHub => {
            command
                .env("GH_HOST", "github.com")
                .env("KBORG_COPILOT_SELECTED_TOKEN", token);
        }
        Deployment::EnterpriseCloud(host) => {
            command
                .env("GH_HOST", host)
                .env("COPILOT_GH_HOST", host)
                .env("KBORG_COPILOT_SELECTED_TOKEN", token);
        }
        Deployment::EnterpriseServer(host) => {
            command
                .env("GH_HOST", host)
                .env("COPILOT_OFFLINE", "true")
                .env("COPILOT_PROVIDER_GHES_HOST", host)
                .env("COPILOT_PROVIDER_GHES_TOKEN", token)
                .env("KBORG_COPILOT_SELECTED_TOKEN", token);
        }
    }
    if let Some(ca_file) = ca_file {
        // Copied into the private state area the sandbox may read; the
        // policy path itself stays outside the child's view.
        let bytes = super::authority_read(ca_file, 256 * 1024)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| "tls_ca_file must be PEM text")?;
        if !text.contains("-----BEGIN CERTIFICATE-----") || text.contains("PRIVATE KEY") {
            return Err("tls_ca_file must contain only PEM certificates".into());
        }
        let copy = state.join("enterprise-ca.pem");
        safety::write_new(&copy, &bytes)?;
        command
            .env("NODE_EXTRA_CA_CERTS", &copy)
            .env("SSL_CERT_FILE", &copy);
    }
    Ok(())
}
fn sandbox(policy: &Path, executable: &Path, args: &[&str], state: &Path) -> Command {
    let mut command = Command::new("/usr/bin/sandbox-exec");
    command.arg("-f").arg(policy).arg(executable).args(args);
    environment(&mut command, state);
    command
}
fn snapshot(options: &Options, workspace: &Workspace) -> Result<(PathBuf, PathBuf)> {
    let tools = workspace.0.join("tools");
    let state = workspace.0.join("state");
    safety::ensure_private_dir(&tools)?;
    safety::ensure_private_dir(&state)?;
    let native = tools.join("copilot");
    fs::copy(&options.executable, &native)
        .map_err(|_| "cannot snapshot pinned native Copilot executable")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&native, fs::Permissions::from_mode(0o500))
            .map_err(|_| "cannot seal Copilot executable permissions")?;
    }
    if super::fingerprint(&native)?.1 != options.expected_sha256 {
        return Err("Native Copilot executable bytes changed after review".into());
    }
    Ok((native, state))
}
fn policy_checks(
    workspace: &Workspace,
    state: &Path,
    port: u16,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let policy = workspace.0.join("canaries.sb");
    safety::write_new(
        &policy,
        profile(
            &[
                Path::new("/bin/cat"),
                Path::new("/usr/bin/touch"),
                Path::new("/usr/bin/nc"),
            ],
            state,
            port,
        )?
        .as_bytes(),
    )?;
    let allowed = state.join("canary");
    let denied = workspace.0.join("outside-canary");
    let outside_write = workspace.0.join("outside-write");
    safety::write_new(&allowed, b"KBORG_ALLOWED_CANARY")?;
    safety::write_new(&denied, b"KBORG_FAKE_CREDENTIAL_CANARY")?;
    let probe = |exe: &str, args: &[&str]| {
        process::invoke(
            sandbox(&policy, Path::new(exe), args, state),
            vec![],
            Duration::from_secs(3),
            stop.clone(),
            4096,
        )
    };
    let yes = probe(
        "/bin/cat",
        &[allowed.to_str().ok_or("invalid canary path")?],
    )?;
    if !yes.success || yes.stdout != b"KBORG_ALLOWED_CANARY" {
        return Err(format!(
            "Copilot OS-policy positive baseline failed; no credential process permitted: {}",
            safety::safe_text(&String::from_utf8_lossy(&yes.stderr), 1200)
        ));
    }
    let blocked = |capture: process::Capture| {
        !capture.success
            && String::from_utf8_lossy(&capture.stderr)
                .to_ascii_lowercase()
                .contains("operation not permitted")
    };
    if !blocked(probe(
        "/bin/cat",
        &[denied.to_str().ok_or("invalid canary path")?],
    )?) || !blocked(probe(
        "/usr/bin/touch",
        &[outside_write.to_str().ok_or("invalid canary path")?],
    )?) || outside_write.exists()
        || !blocked(probe("/bin/echo", &["FORBIDDEN_EXEC_CANARY"])?)
    {
        return Err(
            "Fresh Copilot OS read/write/exec denials failed; no credential process permitted"
                .into(),
        );
    }
    let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
        .map_err(|_| "cannot establish network-denial canary")?;
    listener
        .set_nonblocking(true)
        .map_err(|_| "cannot bound canary socket")?;
    let outside_port = listener
        .local_addr()
        .map_err(|_| "invalid canary socket")?
        .port()
        .to_string();
    if !blocked(probe(
        "/usr/bin/nc",
        &["-v", "-w", "1", "-z", "127.0.0.1", &outside_port],
    )?) || listener.accept().is_ok()
    {
        return Err("Fresh direct-network denial failed; no credential process permitted".into());
    }
    if !probe(
        "/usr/bin/nc",
        &["-w", "1", "-z", "127.0.0.1", &port.to_string()],
    )?
    .success
    {
        return Err(
            "Positive broker-port OS baseline failed; no credential process permitted".into(),
        );
    }
    Ok(())
}
fn version(native: &Path, policy: &Path, state: &Path, stop: Arc<AtomicBool>) -> Result<()> {
    let version = process::invoke(
        sandbox(policy, native, &["--no-auto-update", "--version"], state),
        vec![],
        Duration::from_secs(5),
        stop.clone(),
        4096,
    )?;
    if !version.success {
        return Err(format!(
            "Pinned Copilot version probe failed under actual OS policy (no credential used): {}",
            safety::safe_text(&String::from_utf8_lossy(&version.stderr), 1200)
        ));
    }
    let text = String::from_utf8_lossy(&version.stdout);
    if !text
        .lines()
        .any(|line| line.trim() == "GitHub Copilot CLI 1.0.91.")
    {
        return Err(format!(
            "Reviewed Copilot runtime version1.0.91 required; token-free version output: {}",
            safety::safe_text(&text, 512)
        ));
    }
    let help = process::invoke(
        sandbox(policy, native, &["--help"], state),
        vec![],
        Duration::from_secs(5),
        stop.clone(),
        65536,
    )?;
    if !help.success {
        return Err("Pinned native Copilot full startup failed under OS policy before credential access; no fallback".into());
    }
    let help = String::from_utf8_lossy(&help.stdout);
    for flag in [
        "--available-tools",
        "--disable-builtin-mcps",
        "--no-custom-instructions",
        "--no-remote",
        "--no-ask-user",
    ] {
        if !help.contains(flag) {
            return Err(format!(
                "Pinned native runtime lacks required public invocation switch {flag}"
            ));
        }
    }
    let parsed = process::invoke(
        sandbox(
            policy,
            native,
            &[
                "--help",
                "--no-auto-login",
                "--auth-token-env",
                "KBORG_COPILOT_SELECTED_TOKEN",
                "--available-tools=",
            ],
            state,
        ),
        vec![],
        Duration::from_secs(5),
        stop,
        65536,
    )?;
    if !parsed.success {
        return Err("Pinned native rejected required hidden authentication/tool switches before credential access".into());
    }
    Ok(())
}
pub(super) fn validate_native(options: &Options) -> Result<()> {
    let workspace = create_workspace()?;
    let (native, state) = snapshot(options, &workspace)?;
    let deployment = options.deployment()?;
    let broker = proxy::Broker::start(30, deployment.authorities(), options.allow_private_network)?;
    let stop = Arc::new(AtomicBool::new(false));
    policy_checks(&workspace, &state, broker.port, stop.clone())?;
    let policy = workspace.0.join("native.sb");
    safety::write_new(
        &policy,
        profile(&[&native], &state, broker.port)?.as_bytes(),
    )?;
    version(&native, &policy, &state, stop)
}
pub(super) fn execute(prepared: PreparedAdvice, stop: Arc<AtomicBool>) -> Result<AdviceOutput> {
    if !cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        return Err("No validated whole-process Copilot sandbox exists on this platform".into());
    }
    let workspace = create_workspace()?;
    let (native, state) = snapshot(&prepared.options, &workspace)?;
    let deployment = prepared.options.deployment()?;
    let broker = proxy::Broker::start(
        prepared.options.seconds.saturating_add(30),
        deployment.authorities(),
        prepared.options.allow_private_network,
    )?;
    let result = (|| -> Result<AdviceOutput> {
        policy_checks(&workspace, &state, broker.port, stop.clone())?;
        let policy = workspace.0.join("native.sb");
        safety::write_new(
            &policy,
            profile(&[&native], &state, broker.port)?.as_bytes(),
        )?;
        version(&native, &policy, &state, stop.clone())?;
        let (credential, hash) = super::selected_credential(&prepared.options, stop.clone())?;
        if hash != prepared.credential_hash
            || credential.account != prepared.account
            || credential.host != prepared.options.host
        {
            return Err(
                "Selected Copilot credential identity changed after review; prepare again".into(),
            );
        }
        // Fixed shell code establishes OS CPU/file/fd limits before entering the
        // whole-process policy. All dynamic values are positional arguments.
        let mut command = Command::new("/bin/sh");
        command.args(["-c","ulimit -t \"$1\" || exit 120; ulimit -c 0 || exit 120; ulimit -f 131072 || exit 120; ulimit -n 1024 || exit 120; shift; exec /usr/bin/sandbox-exec \"$@\"","kborg-contained-copilot",&prepared.options.seconds.to_string(),"-f"])
        .arg(&policy).arg(&native).args(["--no-auto-login","--auth-token-env","KBORG_COPILOT_SELECTED_TOKEN","--available-tools=","--disable-builtin-mcps","--no-custom-instructions","--no-ask-user","--no-auto-update","--no-remote","--no-remote-export","--no-bash-env","--no-color","--stream","off","--silent","--log-level","none","--model",&prepared.options.model]);
        environment(&mut command, &state);
        deployment_environment(
            &mut command,
            &deployment,
            &credential.token,
            &state,
            prepared.options.tls_ca_file.as_deref(),
        )?;
        command
            .env("HTTPS_PROXY", broker.url())
            .env("HTTP_PROXY", broker.url())
            .env("https_proxy", broker.url())
            .env("http_proxy", broker.url())
            .env("NO_PROXY", "")
            .env("no_proxy", "");
        if stop.load(Ordering::Acquire) {
            return Err("Copilot request canceled before launch".into());
        }
        // The CLI takes a piped prompt instead of -p, never both: the fixed
        // instruction and the reviewed data go on stdin, which also keeps the
        // data out of the process argument list.
        let input = format!("{}\n", prepared.preview()?).into_bytes();
        let capture = process::invoke(
            command,
            input,
            Duration::from_secs(u64::from(prepared.options.seconds)),
            stop,
            65536,
        )?;
        if !capture.success {
            let diagnostic = format!(
                "{} {}",
                String::from_utf8_lossy(&capture.stderr),
                String::from_utf8_lossy(&capture.stdout)
            )
            .to_ascii_lowercase();
            let category =
                if diagnostic.contains("unknown option") || diagnostic.contains("invalid option") {
                    "reviewed CLI argument rejected"
                } else if diagnostic.contains("prompt") && diagnostic.contains("required") {
                    "noninteractive prompt required"
                } else if diagnostic.contains("operation not permitted")
                    || diagnostic.contains("permission denied")
                {
                    "OS policy denied an operation"
                } else if diagnostic.contains("proxy")
                    || diagnostic.contains("connect")
                    || diagnostic.contains("fetch")
                {
                    "provider connection failed"
                } else if diagnostic.contains("auth")
                    || diagnostic.contains("token")
                    || diagnostic.contains("login")
                {
                    "selected-account authentication failed"
                } else {
                    "provider/runtime failure"
                };
            let markers = [
                "401",
                "403",
                "404",
                "429",
                "model",
                "tools",
                "prompt",
                "permission",
                "certificate",
                "sandbox",
                "ENOMEM",
                "EACCES",
                "EPERM",
                "quota",
                "billing",
                "account",
                "authentication",
                "token",
                "access",
                "fetch",
                "network",
                "fatal",
                "error",
                "unknown",
                "argument",
                "invalid",
                "entitlement",
            ]
            .into_iter()
            .filter(|word| diagnostic.contains(&word.to_ascii_lowercase()))
            .collect::<Vec<_>>()
            .join(",");
            let network = broker.report();
            return Err(format!(
                "Contained Copilot request failed: {category}; approved tunnels {}, denied connections {}; sent {} received {}; exit {:?}/signal {:?}; static markers {markers}. Provider diagnostics suppressed; no fallback attempted",
                network.approved_tunnels,
                network.denied_connections,
                network.sent_bytes,
                network.received_bytes,
                capture.exit_code,
                capture.signal
            ));
        }
        use base64::Engine;
        let encoded = base64::engine::general_purpose::STANDARD.encode(credential.token.as_bytes());
        if [capture.stdout.as_slice(), capture.stderr.as_slice()]
            .iter()
            .any(|bytes| {
                bytes
                    .windows(credential.token.len())
                    .any(|window| window == credential.token.as_bytes())
                    || bytes
                        .windows(encoded.len())
                        .any(|window| window == encoded.as_bytes())
            })
        {
            return Err(
                "Copilot output contained selected credential material; response suppressed".into(),
            );
        }
        let answer = ascii_punctuation(&safety::safe_lines(
            std::str::from_utf8(&capture.stdout).map_err(|_| "Copilot response was not UTF-8")?,
            65536,
        ));
        if answer.trim().is_empty() {
            return Err("Copilot returned no advisory answer".into());
        }
        Ok(AdviceOutput{schema_version:1,host:prepared.options.host,account:prepared.account,model:prepared.options.model,answer,notice:"Untrusted advisory response. No suggested command was executed or approved. Credential account is explicitly selected; broader gh authority may be reused only when that source is chosen. Native CLI and publisher pin are trusted operator inputs; tool flags are not containment. Temporary native/session files are removed after completion.".into(),network:broker.report()})
    })();
    drop(broker);
    workspace.cleanup()?;
    result
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;
    #[test]
    #[ignore = "explicit token-free macOS policy canary against verified official native fixture"]
    fn actual_native_policy_canaries() -> Result<()> {
        let options = Options {
            schema_version: 1,
            executable: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(".local/copilot-native-1.0.91/copilot"),
            expected_sha256: "87f04922933c139cf4af7cb6a80b96161428618a6275fea4b8dfe7e7a69c9518"
                .into(),
            host: "github.com".into(),
            deployment: None,
            allow_private_network: false,
            tls_ca_file: None,
            credential_file: Some(PathBuf::from("/not/read/by/this/test")),
            gh_cli: None,
            model: "auto".into(),
            seconds: 60,
        };
        validate_native(&options)
    }
}
#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
mod tls_tests {
    use super::*;
    #[test]
    #[ignore = "explicit public fake-token TLS canary; no stored credential or diagnostics"]
    fn native_tls_rejects_fake_token_without_platform_abort() -> Result<()> {
        let options = Options {
            schema_version: 1,
            executable: PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join(".local/copilot-native-1.0.91/copilot"),
            expected_sha256: "87f04922933c139cf4af7cb6a80b96161428618a6275fea4b8dfe7e7a69c9518"
                .into(),
            host: "github.com".into(),
            deployment: None,
            allow_private_network: false,
            tls_ca_file: None,
            credential_file: None,
            gh_cli: None,
            model: "auto".into(),
            seconds: 20,
        };
        let workspace = create_workspace()?;
        let (native, state) = snapshot(&options, &workspace)?;
        let broker = proxy::Broker::start(20, options.deployment()?.authorities(), false)?;
        let policy = workspace.0.join("tls.sb");
        safety::write_new(
            &policy,
            profile(&[&native], &state, broker.port)?.as_bytes(),
        )?;
        let mut command = sandbox(
            &policy,
            &native,
            &[
                "-p",
                "Answer only HELLO. No tools authorized.",
                "--no-auto-login",
                "--auth-token-env",
                "KBORG_COPILOT_SELECTED_TOKEN",
                "--available-tools=",
                "--disable-builtin-mcps",
                "--no-custom-instructions",
                "--no-ask-user",
                "--no-auto-update",
                "--silent",
                "--stream",
                "off",
                "--log-level",
                "error",
            ],
            &state,
        );
        command
            .env("GH_HOST", "github.com")
            .env(
                "KBORG_COPILOT_SELECTED_TOKEN",
                "github_pat_PUBLIC_FAKE_TOKEN_NOT_A_CREDENTIAL_0000000000000000",
            )
            .env("HTTPS_PROXY", broker.url())
            .env("HTTP_PROXY", broker.url())
            .env("https_proxy", broker.url())
            .env("http_proxy", broker.url())
            .env("NO_PROXY", "")
            .env("no_proxy", "");
        let output = process::invoke(
            command,
            vec![],
            Duration::from_secs(15),
            Arc::new(AtomicBool::new(false)),
            65536,
        )?;
        let lower = format!(
            "{} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .to_lowercase();
        assert!(!output.success);
        assert!(output.signal.is_none());
        assert!(broker.report().approved_tunnels > 0);
        assert!(
            lower.contains("401")
                || lower.contains("bad credentials")
                || lower.contains("authentication")
        );
        Ok(())
    }
}

/// Models use typographic punctuation; the TUI shows non-ASCII as `?`.
fn ascii_punctuation(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\u{2018}' | '\u{2019}' | '\u{2032}' => out.push('\''),
            '\u{201c}' | '\u{201d}' => out.push('"'),
            '\u{2010}'..='\u{2015}' | '\u{2212}' => out.push('-'),
            '\u{2026}' => out.push_str("..."),
            '\u{2022}' => out.push('*'),
            '\u{00a0}' => out.push(' '),
            '\u{2192}' => out.push_str("->"),
            c => out.push(c),
        }
    }
    out
}
#[cfg(test)]
mod punctuation_tests {
    #[test]
    fn answers_use_ascii_punctuation() {
        assert_eq!(
            super::ascii_punctuation(
                "It doesn\u{2019}t \u{201c}crash\u{201d} \u{2014} wait\u{2026}"
            ),
            "It doesn't \"crash\" - wait..."
        );
    }
}
