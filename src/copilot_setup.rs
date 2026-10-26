//! Discoverable Copilot onboarding. Setup never reads Kubernetes or sends a prompt.
use crate::{Result, advice, safety};
use std::{
    io::{self, BufRead, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

pub const TITLE: &str = "COPILOT SETUP";
pub enum Outcome {
    Ready(PathBuf),
    AccountSignedIn { host: String, account: String },
}
pub fn supported() -> bool {
    cfg!(all(target_os = "macos", target_arch = "aarch64"))
}
pub fn saved_policy() -> Option<PathBuf> {
    safety::config_dir()
        .ok()
        .map(|root| root.join("copilot-policy.json"))
        .filter(|path| path.is_file())
}
pub fn status_text(explicit: Option<&Path>) -> String {
    if !supported() {
        return "Copilot answers inside KBorg are not available on this platform yet.\n\nThe contained integration currently runs on macOS Apple Silicon only. Windows, Linux/WSL and Intel macOS do not have a validated containment backend; login alone cannot enable it. No question, screen, token or cluster data has been sent.\n\nYou can use the official GitHub Copilot CLI separately: install it from https://docs.github.com/copilot/how-tos/copilot-cli/set-up-copilot-cli/install-copilot-cli, then run `copilot login` (or `copilot login --host TENANT.ghe.com`). Review anything you copy into it yourself.\n\nPress S to sign in to GitHub with a one-time code and link (GitHub.com or your company domain). This stores the account in the official GitHub CLI only; it does not enable unsupported in-app answers.\n\nKBorg's Kubernetes features continue to work without Copilot. Esc returns.\n".into();
    }
    let path = explicit.map(Path::to_path_buf).or_else(saved_policy);
    match path {
        Some(path) => match advice::load_options(&path) {
            Ok(options) => format!("Copilot connection saved\n\nHost: {}\nAccount: {}\nModel: {}\n\nUse :ask QUESTION from the screen you want help with, or :copilot for minimized selected-resource diagnostics. Every transfer is reviewed first; answers never execute commands. Credentials and containment are rechecked for each request. A saved policy does not prove an active subscription or enterprise entitlement.\n\nS runs setup / verifies the saved connection. Esc returns.", options.host, options.gh_cli.as_ref().map(|gh|gh.account.as_str()).unwrap_or("dedicated credential"), options.model),
            Err(error) => format!("Copilot connection needs attention\n\n{}\n\nS opens setup. Setup can repair or change the connection after review, retaining a private backup. Esc returns.", safety::safe_text(&error, 4096)),
        },
        None => "Connect GitHub Copilot\n\nPress S to start guided setup, or run `kborg copilot setup` in a terminal.\n\nKBorg finds your installed Copilot and GitHub CLIs, lets you choose github.com or TENANT.ghe.com, and reuses a stored GitHub account. If you are not signed in, it offers GitHub's browser login. No token copying or JSON editing is required.\n\nThe currently validated contained runtime is native Copilot 1.0.91 on Apple Silicon. A compatible official installation and a Copilot entitlement are required. You approve the exact installed executables before they run; KBorg pins them and writes the connection locally. No cluster data is sent during setup.\n\nS: start setup | Esc: return".into(),
    }
}
fn input(label: &str, default: &str) -> Result<String> {
    if default.is_empty() {
        print!("{label}: ");
    } else {
        print!("{label} [{default}]: ");
    }
    io::stdout()
        .flush()
        .map_err(|_| "Cannot display setup prompt")?;
    let mut line = String::new();
    let count = io::stdin()
        .lock()
        .take(4097)
        .read_line(&mut line)
        .map_err(|_| "Cannot read setup input")?;
    if count == 0 {
        return Err("Copilot setup canceled; no connection saved".into());
    }
    if line.len() > 4096 {
        return Err("Setup input is too long".into());
    }
    let value = line.trim();
    if value.is_empty() {
        Ok(default.into())
    } else if safety::safe_text(value, 4096) == value {
        Ok(value.into())
    } else {
        Err("Setup input contains control characters".into())
    }
}
fn confirm(label: &str) -> Result<bool> {
    Ok(matches!(
        input(label, "no")?.to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}
fn executable(name: &str) -> Result<PathBuf> {
    let found = crate::extensions::resolve_on_path(name).or_else(|| {
        if cfg!(windows) {
            crate::extensions::resolve_on_path(&format!("{name}.exe"))
        } else {
            None
        }
    });
    let default = found
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .unwrap_or_default();
    let selected = PathBuf::from(input(&format!("Official {name} executable"), &default)?);
    if !selected.is_absolute() {
        return Err(format!(
            "Install official {name}, then choose its absolute native executable path. No program was run."
        ));
    }
    let path = selected
        .canonicalize()
        .map_err(|_| format!("Cannot find {name}; install it and run setup again"))?;
    safety::checked_path(&path)?;
    Ok(path)
}
fn gh_config() -> Result<PathBuf> {
    let path = if let Some(path) = std::env::var_os("GH_CONFIG_DIR") {
        PathBuf::from(path)
    } else if let Some(path) = std::env::var_os("XDG_CONFIG_HOME") {
        PathBuf::from(path).join("gh")
    } else if cfg!(windows) {
        PathBuf::from(std::env::var_os("APPDATA").ok_or("APPDATA is unavailable")?)
            .join("GitHub CLI")
    } else {
        PathBuf::from(
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .ok_or("Home directory is unavailable")?,
        )
        .join(".config/gh")
    };
    if !path.is_absolute() {
        return Err("GitHub CLI configuration directory must be absolute".into());
    }
    Ok(path)
}
fn stored_accounts(directory: &Path, host: &str) -> Result<Vec<String>> {
    let path = directory.join("hosts.yml");
    if !path.exists() {
        return Ok(vec![]);
    }
    let bytes = safety::read_small(&path, 64 * 1024)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "GitHub account metadata is not UTF-8")?;
    let value: serde_json::Value = serde_saphyr::from_str(text)
        .map_err(|_| "Cannot read stored GitHub accounts; use gh auth login and retry")?;
    Ok(account_names(&value, host))
}
fn account_names(value: &serde_json::Value, host: &str) -> Vec<String> {
    let mut accounts = vec![];
    if let Some(current) = value[host]["user"].as_str() {
        accounts.push(current.to_owned());
    }
    if let Some(users) = value[host]["users"].as_object() {
        for name in users.keys() {
            if !accounts.contains(name) {
                accounts.push(name.clone());
            }
        }
    }
    accounts.retain(|name| advice::valid_account(name));
    accounts.truncate(100);
    accounts
}
fn browser_login(gh: &Path, pin: &str, config: &Path, host: &str) -> Result<()> {
    if login_fingerprint(gh)? != pin {
        return Err("GitHub CLI changed after approval; run setup again before login".into());
    }
    let root = safety::config_dir()?.join("copilot-login");
    safety::ensure_private_dir(&root)?;
    let mut command = Command::new(gh);
    command
        .args([
            "auth",
            "login",
            "--hostname",
            host,
            "--web",
            "--git-protocol",
            "https",
            "--skip-ssh-key",
        ])
        .current_dir(root)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    for (key, _) in std::env::vars_os() {
        let name = key.to_string_lossy().to_ascii_uppercase();
        if name.ends_with("TOKEN")
            || name.ends_with("TOKEN_FILE")
            || matches!(
                name.as_str(),
                "KUBECONFIG" | "GH_DEBUG" | "DEBUG" | "GH_PROMPT_DISABLED"
            )
            || [
                "AWS_",
                "AZURE_",
                "GOOGLE_",
                "GCP_",
                "OPENAI_",
                "ANTHROPIC_",
                "DYLD_",
                "LD_",
            ]
            .iter()
            .any(|prefix| name.starts_with(prefix))
        {
            command.env_remove(key);
        }
    }
    command
        .env("GH_CONFIG_DIR", config)
        .env("GH_HOST", host)
        .env("GH_NO_UPDATE_NOTIFIER", "1")
        .env("GH_TELEMETRY", "0");
    // Recheck after prompts/environment preparation, immediately before spawn.
    if login_fingerprint(gh)? != pin {
        return Err("GitHub CLI changed after approval; login refused".into());
    }
    println!(
        "GitHub will display a one-time code and verification link below. Open the link, enter the code, authorize the account, then return here. No token copying is needed."
    );
    let mut child = command
        .spawn()
        .map_err(|_| "Cannot start trusted GitHub browser login")?;
    let deadline = Instant::now() + Duration::from_secs(300);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                return if status.success() {
                    Ok(())
                } else {
                    Err("GitHub login did not finish; run setup again when ready".into())
                };
            }
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("GitHub login timed out or stopped; no KBorg policy saved".into());
            }
        }
    }
}
// Login is an explicit trusted-program handoff, not contained inference.
// Check bytes without executing a discovered PATH candidate before approval.
fn login_fingerprint(path: &Path) -> Result<String> {
    use ring::digest::{Context, SHA256};
    safety::checked_path(path)?;
    let before = std::fs::metadata(path).map_err(|_| "Cannot inspect GitHub CLI")?;
    if !path.is_absolute()
        || !before.is_file()
        || before.len() < 4
        || before.len() > 256 * 1024 * 1024
    {
        return Err("GitHub CLI must be a bounded native executable".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if before.permissions().mode() & 0o111 == 0 || before.permissions().mode() & 0o022 != 0 {
            return Err("GitHub CLI must be executable and not group/world writable".into());
        }
    }
    let mut file = std::fs::File::open(path).map_err(|_| "Cannot read GitHub CLI")?;
    let mut header = [0; 4];
    file.read_exact(&mut header)
        .map_err(|_| "Truncated GitHub CLI")?;
    let native = if cfg!(windows) {
        header[..2] == *b"MZ"
    } else if cfg!(target_os = "macos") {
        matches!(
            header,
            [0xcf, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xca, 0xfe, 0xba, 0xbf]
                | [0xbe, 0xba, 0xfe, 0xca]
                | [0xbf, 0xba, 0xfe, 0xca]
        )
    } else {
        header == *b"\x7fELF"
    };
    if !native {
        return Err(
            "Choose the official native gh executable, not a script or command wrapper".into(),
        );
    }
    let mut hash = Context::new(&SHA256);
    hash.update(&header);
    let mut count = 4u64;
    let mut chunk = [0; 16384];
    loop {
        let length = file
            .read(&mut chunk)
            .map_err(|_| "Cannot fingerprint GitHub CLI")?;
        if length == 0 {
            break;
        }
        count += length as u64;
        if count > 256 * 1024 * 1024 {
            return Err("GitHub CLI changed during inspection".into());
        }
        hash.update(&chunk[..length]);
    }
    let after = file.metadata().map_err(|_| "Cannot recheck GitHub CLI")?;
    if count != before.len()
        || after.len() != before.len()
        || before.modified().ok() != after.modified().ok()
    {
        return Err("GitHub CLI changed during inspection".into());
    }
    Ok(hash
        .finish()
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
fn choose_host() -> Result<String> {
    println!("1. GitHub.com\n2. Company GitHub Enterprise Cloud (*.ghe.com)");
    let choice = input("Where do you use Copilot?", "1")?;
    let domain = if choice == "2" {
        input("Company domain (for example company.ghe.com)", "")?
    } else {
        String::new()
    };
    setup_host(&choice, &domain)
}
fn select_account(gh: &Path, pin: &str, config: &Path, host: &str) -> Result<String> {
    let mut accounts = stored_accounts(config, host)?;
    let login = if accounts.is_empty() {
        confirm("No stored account. Get a one-time GitHub login code/link? yes/no")?
    } else {
        println!("Stored accounts: {}", accounts.join(", "));
        match input("1 reuse a stored account / 2 sign in with a new code", "1")?.as_str() {
            "1" => false,
            "2" => true,
            _ => return Err("Setup canceled; no connection changed".into()),
        }
    };
    if login {
        browser_login(gh, pin, config, host)?;
        accounts = stored_accounts(config, host)?;
    }
    if accounts.is_empty() {
        return Err("No stored account selected. Run setup again to finish GitHub login. No KBorg policy was saved.".into());
    }
    println!("Stored accounts: {}", accounts.join(", "));
    let account = input("Account to use", &accounts[0])?;
    if !accounts.contains(&account) {
        return Err("Choose one of the stored accounts; no account was switched".into());
    }
    Ok(account)
}
fn account_sign_in() -> Result<Outcome> {
    println!("{}", status_text(None));
    if !confirm("Continue with GitHub account sign-in only? yes/no")? {
        return Err("Sign-in canceled; nothing was sent or changed".into());
    }
    let host = choose_host()?;
    let gh = executable("gh")?;
    let pin = login_fingerprint(&gh)?;
    let config = gh_config()?;
    println!(
        "{}",
        safety::review_display(&format!(
            "GitHub account login\nHost: {host}\nExecutable: {}\n\nUse only an official GitHub CLI installation you trust. It runs with your user permissions and manages its own credential storage. No cluster context is passed. This does not enable in-app Copilot answers on this platform.",
            gh.display()
        ))?
    );
    if !confirm("Trust this GitHub CLI for account login? yes/no")? {
        return Err("Sign-in canceled".into());
    }
    let account = select_account(&gh, &pin, &config, &host)?;
    let path = safety::config_dir()?.join("copilot-account.json");
    let value = serde_json::to_vec_pretty(
        &serde_json::json!({"schema_version":1,"host":host,"account":account}),
    )
    .map_err(|_| "Cannot encode account selection")?;
    safety::atomic_write(&path, &value)?;
    println!(
        "GitHub account selected. In-app Copilot answers still require a supported containment backend; no model request was made."
    );
    Ok(Outcome::AccountSignedIn { host, account })
}
fn setup_host(choice: &str, domain: &str) -> Result<String> {
    match choice {
        "1" => Ok("github.com".into()),
        "2" => match crate::copilot::Deployment::parse(domain, Some("ghe.com"))? {
            crate::copilot::Deployment::EnterpriseCloud(host) => Ok(host),
            _ => Err("Company login requires a TENANT.ghe.com hostname".into()),
        },
        _ => Err("Choose 1 for GitHub.com or 2 for your company's Enterprise Cloud".into()),
    }
}
fn save_policy(destination: &Path, original: Option<&[u8]>, bytes: &[u8]) -> Result<()> {
    if let Some(original) = original {
        if safety::read_small(destination, 16 * 1024)? != original {
            return Err(
                "Copilot connection changed during setup; nothing replaced. Review it again."
                    .into(),
            );
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "Clock unavailable")?
            .as_nanos();
        let backup = destination.with_file_name(format!("copilot-policy.{stamp}.bak.json"));
        safety::write_new(&backup, original)?;
        safety::atomic_write(destination, bytes)?;
    } else {
        safety::write_new(destination, bytes)?;
    }
    Ok(())
}
pub fn interactive() -> Result<Outcome> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err(
            "Copilot setup needs an interactive terminal; use `kborg copilot status` for readiness"
                .into(),
        );
    }
    if !supported() {
        return account_sign_in();
    }
    println!("Connect GitHub Copilot — no Kubernetes data is sent during setup.\n");
    let destination = safety::config_dir()?.join("copilot-policy.json");
    let original = if destination.exists() {
        Some(safety::read_small(&destination, 16 * 1024)?)
    } else {
        None
    };
    if original.is_some() {
        match advice::load_options(&destination) {
            Ok(options) => {
                println!(
                    "Saved connection: {} / {}",
                    options.host,
                    options
                        .gh_cli
                        .as_ref()
                        .map(|gh| gh.account.as_str())
                        .unwrap_or("dedicated credential")
                );
                match input("1 keep and verify / 2 change host or account", "1")?.as_str() {
                    "1" => {
                        advice::validate_setup(&options)?;
                        return Ok(Outcome::Ready(destination));
                    }
                    "2" => {}
                    _ => {
                        return Err(
                            "Setup canceled; the existing connection was not changed".into()
                        );
                    }
                }
            }
            Err(_) => println!(
                "The existing connection needs repair. Setup can replace it after review; a private backup is retained."
            ),
        }
    }
    let host = choose_host()?;
    let native = executable("copilot")?;
    let gh = executable("gh")?;
    let native_pin = advice::setup_fingerprint(&native)?;
    let gh_pin = login_fingerprint(&gh)?;
    let config = gh_config()?;
    println!(
        "{}",
        safety::review_display(&format!(
            "Host: {host}\nCopilot: {}\nGitHub CLI: {}\n\nOnly continue if these are official installations you trust. Matching local hashes pin these bytes; they do not establish publisher identity. GitHub login may request account permissions; review them in your browser. Model access still requires your Copilot plan and company policy.\n",
            native.display(),
            gh.display()
        ))?
    );
    if !confirm("Trust these installed tools for this Copilot connection? yes/no")? {
        return Err("Copilot setup canceled; no connection saved".into());
    }
    let account = select_account(&gh, &gh_pin, &config, &host)?;
    let policy = serde_json::json!({"schema_version":1,"executable":native,"expected_sha256":native_pin,"host":host,"gh_cli":{"executable":gh,"expected_sha256":gh_pin,"account":account,"config_dir":config},"model":"auto","seconds":60});
    let options: advice::Options =
        serde_json::from_value(policy.clone()).map_err(|_| "Cannot encode Copilot connection")?;
    println!(
        "Checking the pinned runtime, sandbox and selected stored account. No model request is made..."
    );
    advice::validate_setup(&options)?;
    if !confirm("Save this connection for reviewed Copilot requests? yes/no")? {
        return Err("Copilot setup canceled; no connection saved".into());
    }
    let bytes = serde_json::to_vec_pretty(&policy).map_err(|_| "Cannot encode connection")?;
    save_policy(&destination, original.as_deref(), &bytes)?;
    println!(
        "Copilot connection saved. Return to a resource screen and use :ask QUESTION. Every transfer is reviewed first."
    );
    Ok(Outcome::Ready(destination))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_discovery_never_returns_tokens_and_stays_host_bound() {
        let data = serde_json::json!({"github.com":{"user":"owner","oauth_token":"SECRET","users":{"owner":{"oauth_token":"SECRET"},"second":{},"bad name":{}}},"other.ghe.com":{"user":"other"}});
        assert_eq!(account_names(&data, "github.com"), vec!["owner", "second"]);
        assert_eq!(account_names(&data, "other.ghe.com"), vec!["other"]);
        assert!(account_names(&data, "absent.ghe.com").is_empty());
    }
    #[test]
    fn enterprise_managed_accounts_are_accepted_without_weakening_argument_bounds() {
        let data = serde_json::json!({"company.ghe.com":{"user":"mona-cat_octo"}});
        assert_eq!(
            account_names(&data, "company.ghe.com"),
            vec!["mona-cat_octo"]
        );
        assert!(advice::valid_account("2abvd19d_admin"));
        for value in ["--help", "a b", "a\nb", "_admin", "name_", "user/other"] {
            assert!(!advice::valid_account(value));
        }
    }
    #[test]
    fn company_login_rejects_unknown_hosts_and_injection() {
        assert_eq!(
            setup_host("1", "").unwrap_or_else(|error| panic!("setup fixture failed: {error:?}")),
            "github.com"
        );
        assert_eq!(
            setup_host("2", "company.ghe.com")
                .unwrap_or_else(|error| panic!("setup fixture failed: {error:?}")),
            "company.ghe.com"
        );
        for host in [
            "github.com",
            "ghe.com",
            "company.ghe.com.evil.test",
            "https://company.ghe.com",
            "company.ghe.com/path",
            "company.ghe.com\n",
        ] {
            assert!(setup_host("2", host).is_err(), "{host:?}");
        }
        assert!(setup_host("cancel", "").is_err());
    }
    #[test]
    fn policy_replacement_keeps_backup_and_refuses_concurrent_changes() {
        let root = std::env::temp_dir().join(format!(
            "kborg-setup-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_else(|error| panic!("setup fixture failed: {error:?}"))
                .as_nanos()
        ));
        safety::ensure_private_dir(&root)
            .unwrap_or_else(|error| panic!("setup fixture failed: {error:?}"));
        let path = root.join("copilot-policy.json");
        save_policy(&path, None, b"first")
            .unwrap_or_else(|error| panic!("setup fixture failed: {error:?}"));
        assert!(save_policy(&path, None, b"overwrite").is_err());
        assert!(save_policy(&path, Some(b"stale"), b"overwrite").is_err());
        save_policy(&path, Some(b"first"), b"second")
            .unwrap_or_else(|error| panic!("setup fixture failed: {error:?}"));
        assert_eq!(
            std::fs::read(&path).unwrap_or_else(|error| panic!("setup fixture failed: {error:?}")),
            b"second"
        );
        let files: Vec<_> = std::fs::read_dir(&root)
            .unwrap_or_else(|error| panic!("setup fixture failed: {error:?}"))
            .map(|entry| {
                entry
                    .unwrap_or_else(|error| panic!("setup fixture failed: {error:?}"))
                    .path()
            })
            .collect();
        assert_eq!(files.len(), 2);
        let backup = files
            .iter()
            .find(|file| **file != path)
            .unwrap_or_else(|| panic!("setup fixture backup is missing"));
        assert_eq!(
            std::fs::read(backup).unwrap_or_else(|error| panic!("setup fixture failed: {error:?}")),
            b"first"
        );
        for file in files {
            std::fs::remove_file(file)
                .unwrap_or_else(|error| panic!("setup fixture failed: {error:?}"));
        }
        std::fs::remove_dir(root).unwrap_or_else(|error| panic!("setup fixture failed: {error:?}"));
    }
    #[test]
    fn unsupported_platform_has_actionable_visible_status() {
        if !supported() {
            let text = status_text(None);
            assert!(
                text.contains("not available")
                    && text.contains("copilot login")
                    && text.contains("No question")
            );
        }
    }
}
