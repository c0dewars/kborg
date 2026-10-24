//! Organization-managed policy: a root-owned file at a fixed system path that
//! caps what any KBorg session may do. Users cannot loosen it with flags,
//! `--config` or environment variables. An untrusted policy file fails closed.
use crate::{Result, safety};
use serde::Deserialize;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Capabilities an organization can deny.
pub const CAPABILITIES: &[&str] = &[
    "write",
    "logs",
    "exec",
    "attach",
    "port-forward",
    "node-maintenance",
    "yaml-edit",
    "force-kill",
    "secret-reveal",
    "helm",
    "helm-write",
    "helm-hooks",
    "helm-crds",
    "tools",
    "extensions",
    "auth-helper",
    "external-editor",
    "copilot",
];

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub schema_version: u32,
    /// Capabilities no session may enable.
    #[serde(default)]
    pub deny: Vec<String>,
    /// When present, the only Kubernetes API server URLs any connection may
    /// reach. Context names are user-chosen aliases, so the policy binds to
    /// the server actually reached; insecure TLS and proxies are refused.
    #[serde(default)]
    pub allowed_servers: Option<Vec<String>>,
    /// Shown with every refusal, for example "Ask #platform-team".
    #[serde(default)]
    pub contact: Option<String>,
}

pub fn system_path() -> PathBuf {
    if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/KBorg/policy.json")
    } else {
        PathBuf::from("/etc/kborg/policy.json")
    }
}

impl Policy {
    pub fn parse(bytes: &[u8]) -> Result<Self> {
        let policy: Self = serde_json::from_slice(bytes)
            .map_err(|_| "organization policy must be strict schema 1 JSON")?;
        if policy.schema_version != 1 {
            return Err("unsupported organization policy schema".into());
        }
        if let Some(unknown) = policy
            .deny
            .iter()
            .find(|capability| !CAPABILITIES.contains(&capability.as_str()))
        {
            return Err(format!(
                "organization policy denies unknown capability {}",
                safety::safe_text(unknown, 64)
            ));
        }
        if policy.allowed_servers.as_ref().is_some_and(|servers| {
            servers.is_empty()
                || servers.len() > 1000
                || servers.iter().any(|server| {
                    !server.starts_with("https://")
                        || server.len() > 512
                        || safety::safe_text(server, 512) != *server
                })
        }) {
            return Err(
                "organization policy allowed_servers must list 1..1000 https:// API server URLs"
                    .into(),
            );
        }
        if policy.contact.as_ref().is_some_and(|contact| {
            contact.len() > 200 || safety::safe_text(contact, 200) != *contact
        }) {
            return Err("organization policy contact must be short plain text".into());
        }
        Ok(policy)
    }
    pub fn denies(&self, capability: &str) -> bool {
        self.deny.iter().any(|denied| denied == capability)
    }
    fn refusal(&self, what: &str) -> String {
        format!(
            "{what} is disabled by your organization's KBorg policy ({}).{}",
            system_path().display(),
            self.contact
                .as_ref()
                .map(|contact| format!(" {contact}"))
                .unwrap_or_default()
        )
    }
    /// Refuses any requested capability the policy denies.
    pub fn enforce(&self, options: &crate::cli::Options) -> Result<()> {
        let connection = &options.connection;
        let requested = [
            ("write", connection.writable, "Write mode (--write)"),
            ("logs", connection.logs, "Log access (--allow-logs)"),
            ("exec", connection.allow_exec, "Pod shells (--allow-exec)"),
            ("attach", connection.allow_attach, "Attach (--allow-attach)"),
            (
                "port-forward",
                connection.allow_port_forward,
                "Port forwarding (--allow-forward)",
            ),
            (
                "node-maintenance",
                connection.allow_node_maintenance,
                "Node maintenance (--allow-node-maintenance)",
            ),
            (
                "yaml-edit",
                connection.allow_yaml_edit,
                "YAML editing (--allow-yaml-edit)",
            ),
            (
                "force-kill",
                connection.allow_force_kill,
                "Force kill (--allow-force-kill)",
            ),
            (
                "secret-reveal",
                connection.allow_secret_reveal,
                "Secret reveal (--allow-secret-reveal)",
            ),
            (
                "helm",
                connection.allow_helm,
                "Helm release access (--allow-helm)",
            ),
            (
                "helm-write",
                connection.allow_helm_write,
                "Helm lifecycle (--allow-helm-write)",
            ),
            (
                "helm-hooks",
                connection.allow_helm_hooks,
                "Helm hooks (--allow-helm-hooks)",
            ),
            (
                "helm-crds",
                connection.allow_helm_crds,
                "Helm CRD actions (--allow-helm-crds)",
            ),
            (
                "tools",
                options.allow_tools,
                "External tool views (--allow-tools)",
            ),
            (
                "extensions",
                options.allow_extensions,
                "Registered extensions (--allow-extensions)",
            ),
            (
                "auth-helper",
                connection.allow_auth_helper,
                "Authentication helpers (--allow-auth-helper)",
            ),
            (
                "external-editor",
                options.external_editor,
                "The external editor (--allow-external-editor)",
            ),
            (
                "copilot",
                options.copilot_policy.is_some(),
                "Copilot advice (--copilot-policy)",
            ),
        ];
        for (capability, wanted, label) in requested {
            if wanted && self.denies(capability) {
                return Err(self.refusal(label));
            }
        }
        Ok(())
    }
    pub fn check_server(&self, server: &str) -> Result<()> {
        match &self.allowed_servers {
            Some(allowed)
                if !allowed
                    .iter()
                    .any(|allowed| normalize_server(allowed) == normalize_server(server)) =>
            {
                Err(self.refusal(&format!(
                    "The API server {}",
                    safety::safe_text(server, 512)
                )))
            }
            _ => Ok(()),
        }
    }
}

/// Scheme and host compare case-insensitively; a trailing slash is ignored.
fn normalize_server(server: &str) -> String {
    let server = server.trim().trim_end_matches('/');
    match server.split_once("://") {
        Some((scheme, rest)) => {
            let (host, path) = rest.split_once('/').unwrap_or((rest, ""));
            let path = if path.is_empty() {
                String::new()
            } else {
                format!("/{path}")
            };
            format!(
                "{}://{}{path}",
                scheme.to_ascii_lowercase(),
                host.to_ascii_lowercase()
            )
        }
        None => server.to_ascii_lowercase(),
    }
}

/// The file and its directory must belong to root and be writable by no one
/// else, so an unprivileged user cannot replace or edit the policy.
#[cfg(unix)]
fn trusted(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;
    let check = |path: &Path, what: &str| -> Result<()> {
        let meta = std::fs::symlink_metadata(path)
            .map_err(|_| format!("cannot inspect organization policy {what}"))?;
        if meta.file_type().is_symlink() || meta.uid() != 0 || meta.mode() & 0o022 != 0 {
            return Err(format!(
                "organization policy {what} {} must be owned by root and not group/world writable; refusing to start",
                path.display()
            ));
        }
        Ok(())
    };
    check(path, "file")?;
    check(
        path.parent()
            .ok_or("organization policy has no directory")?,
        "directory",
    )
}
#[cfg(not(unix))]
fn trusted(_path: &Path) -> Result<()> {
    Err("organization policy files are supported on macOS and Linux".into())
}

fn load_at(path: &Path) -> Result<Option<Policy>> {
    match std::fs::symlink_metadata(path) {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cannot inspect organization policy; refusing to start".into()),
        Ok(_) => {}
    }
    trusted(path)?;
    let bytes = safety::read_small(path, 64 * 1024)?;
    Policy::parse(&bytes).map(Some)
}

static ACTIVE: OnceLock<Option<Policy>> = OnceLock::new();

/// Loads the system policy once per process. Errors are fatal at startup.
pub fn load() -> Result<Option<&'static Policy>> {
    if let Some(policy) = ACTIVE.get() {
        return Ok(policy.as_ref());
    }
    let policy = load_at(&system_path())?;
    Ok(ACTIVE.get_or_init(|| policy).as_ref())
}

/// Server check for every new cluster connection; a no-op without a policy.
pub fn check_server(server: &str) -> Result<()> {
    match ACTIVE.get().and_then(Option::as_ref) {
        Some(policy) => policy.check_server(server),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn policy_parses_strictly_and_denies_requested_capabilities() -> Result<()> {
        let policy = Policy::parse(
            br#"{"schema_version":1,"deny":["write","secret-reveal","copilot"],"allowed_servers":["https://staging.example.com:6443/"],"contact":"Ask #platform."}"#,
        )?;
        let mut options = crate::config::tests::options();
        assert!(policy.enforce(&options).is_ok());
        options.connection.writable = true;
        let refusal = policy.enforce(&options).err().unwrap_or_default();
        assert!(refusal.contains("Write mode") && refusal.contains("Ask #platform."));
        options.connection.writable = false;
        options.copilot_policy = Some("/policy.json".into());
        assert!(policy.enforce(&options).is_err());
        // Context names are irrelevant: only the resolved server counts.
        assert!(
            policy
                .check_server("https://STAGING.example.com:6443")
                .is_ok()
        );
        assert!(
            policy
                .check_server("https://prod.example.com:6443")
                .is_err()
        );
        assert!(
            policy
                .check_server("https://staging.example.com:6444")
                .is_err()
        );
        for bad in [
            &br#"{"schema_version":1,"deny":["everything"]}"#[..],
            br#"{"schema_version":2}"#,
            br#"{"schema_version":1,"allowed_servers":[]}"#,
            br#"{"schema_version":1,"allowed_servers":["http://plain.example"]}"#,
            br#"{"schema_version":1,"allowed_contexts":["staging"]}"#,
            br#"{"schema_version":1,"bypass":true}"#,
        ] {
            assert!(Policy::parse(bad).is_err());
        }
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    fn user_owned_policy_files_fail_closed_and_absent_files_mean_no_policy() -> Result<()> {
        let root = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "temp")?
            .join(format!("kborg-org-policy-{}", std::process::id()));
        safety::ensure_private_dir(&root)?;
        let path = root.join("policy.json");
        assert_eq!(load_at(&path)?, None);
        safety::write_new(&path, br#"{"schema_version":1}"#)?;
        // Owned by the test user, not root: must refuse rather than ignore.
        assert!(load_at(&path).is_err());
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
}
