//! Explicit data-only configuration. Capability opt-ins remain command-line actions.
use crate::{Result, cli::Options, safety};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Deserialize, Serialize, Default, PartialEq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    #[default]
    ReadOnly,
    Operator,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    pub profile: Profile,
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub theme: Option<String>,
    #[serde(default)]
    pub no_splash: bool,
    #[serde(default)]
    pub poll: bool,
    #[serde(default)]
    pub allow_logs: bool,
    #[serde(default)]
    pub allow_helm: bool,
    #[serde(default)]
    pub allow_helm_write: bool,
    #[serde(default)]
    pub allow_helm_hooks: bool,
    #[serde(default)]
    pub allow_helm_crds: bool,
    #[serde(default)]
    pub allow_exec: bool,
    #[serde(default)]
    pub allow_attach: bool,
    #[serde(default)]
    pub allow_forward: bool,
    #[serde(default)]
    pub allow_file_copy: bool,
    #[serde(default)]
    pub allow_tools: bool,
    #[serde(default)]
    pub allow_extensions: bool,
    #[serde(default)]
    pub allow_auth_helper: bool,
    #[serde(default)]
    pub allow_secret_reveal: bool,
    #[serde(default)]
    pub allow_force_kill: bool,
    #[serde(default)]
    pub allow_yaml_edit: bool,
    #[serde(default)]
    pub allow_node_maintenance: bool,
    #[serde(default)]
    pub allow_external_editor: bool,
    #[serde(default)]
    pub allow_apply: bool,
    #[serde(default)]
    pub allow_node_shell: bool,
    #[serde(default)]
    pub allow_debug: bool,
    #[serde(default)]
    pub allow_scan: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: 1,
            profile: Profile::ReadOnly,
            context: None,
            namespace: None,
            theme: Some(crate::themes::DEFAULT_THEME.into()),
            no_splash: false,
            poll: false,
            allow_logs: false,
            allow_helm: false,
            allow_helm_write: false,
            allow_helm_hooks: false,
            allow_helm_crds: false,
            allow_exec: false,
            allow_attach: false,
            allow_forward: false,
            allow_file_copy: false,
            allow_tools: false,
            allow_extensions: false,
            allow_auth_helper: false,
            allow_secret_reveal: false,
            allow_force_kill: false,
            allow_yaml_edit: false,
            allow_node_maintenance: false,
            allow_external_editor: false,
            allow_apply: false,
            allow_node_shell: false,
            allow_debug: false,
            allow_scan: false,
        }
    }
}
pub fn load(path: &Path) -> Result<Config> {
    let config: Config = serde_json::from_slice(&safety::read_small(path, 16 * 1024)?)
        .map_err(|_| "invalid configuration; expected strict schema version 1 JSON")?;
    if config.schema_version != 1 {
        return Err("unsupported configuration schema version; no implicit migration".into());
    }
    if let Some(context) = &config.context
        && (context.is_empty()
            || context.len() > 253
            || safety::safe_text(context, 253) != *context)
    {
        return Err("invalid configured context".into());
    }
    if let Some(ns) = &config.namespace
        && ns != "*"
        && !safety::valid_id(ns)
    {
        return Err("invalid configured namespace".into());
    }
    if let Some(theme) = &config.theme {
        crate::themes::find(theme)?;
    }
    Ok(config)
}
pub fn apply(config: Config, options: &mut Options) -> Result<()> {
    if config.profile == Profile::ReadOnly && options.connection.writable {
        return Err(
            "read-only profile forbids --write; use a reviewed operator configuration".into(),
        );
    }
    // Validate every constraint before changing the caller's options. Files may
    // constrain capabilities, but they never enable them without explicit flags.
    if options.connection.allow_helm && !config.allow_helm {
        return Err("configuration forbids Helm release storage access".into());
    }
    if options.connection.allow_helm_write && !config.allow_helm_write {
        return Err("configuration forbids Helm lifecycle actions".into());
    }
    if options.connection.allow_helm_hooks && !config.allow_helm_hooks {
        return Err("configuration forbids Helm hooks".into());
    }
    if options.connection.allow_helm_crds && !config.allow_helm_crds {
        return Err("configuration forbids Helm CRD actions".into());
    }
    if options.connection.logs && !config.allow_logs {
        return Err("configuration forbids log access".into());
    }
    if (options.connection.allow_exec && !config.allow_exec)
        || (options.connection.allow_attach && !config.allow_attach)
        || (options.connection.allow_port_forward && !config.allow_forward)
        || (options.connection.allow_file_copy && !config.allow_file_copy)
    {
        return Err("configuration forbids requested subresource capability".into());
    }
    if options.allow_tools && !config.allow_tools {
        return Err("configuration forbids external tool views".into());
    }
    if options.connection.allow_auth_helper && !config.allow_auth_helper {
        return Err("configuration forbids authentication helpers".into());
    }
    if options.allow_extensions && !config.allow_extensions {
        return Err("configuration forbids registered extensions".into());
    }
    // Secret reveal needs no --write, so a read-only profile must name it too.
    if options.connection.allow_secret_reveal && !config.allow_secret_reveal {
        return Err("configuration forbids Secret reveal".into());
    }
    if (options.connection.allow_force_kill && !config.allow_force_kill)
        || (options.connection.allow_yaml_edit && !config.allow_yaml_edit)
        || (options.connection.allow_node_maintenance && !config.allow_node_maintenance)
        || (options.connection.allow_apply && !config.allow_apply)
        || (options.connection.allow_node_shell && !config.allow_node_shell)
        || (options.connection.allow_debug && !config.allow_debug)
    {
        return Err("configuration forbids the requested write capability".into());
    }
    // Scanning is read-only for the cluster but runs a local tool with network
    // access, so a read-only profile must name it too.
    if options.allow_scan && !config.allow_scan {
        return Err("configuration forbids image scanning".into());
    }
    if options.external_editor && !config.allow_external_editor {
        return Err("configuration forbids the external editor".into());
    }
    if options.connection.context.is_none() {
        options.connection.context = config.context;
    }
    if options.connection.namespace.is_empty() {
        options.connection.namespace = config.namespace.unwrap_or_default();
    }
    if options.theme.is_none() {
        options.theme = config.theme;
    }
    options.splash &= !config.no_splash;
    options.poll |= config.poll;
    Ok(())
}
pub fn init(path: &Path) -> Result<()> {
    safety::write_new(
        path,
        &serde_json::to_vec_pretty(&Config::default()).map_err(|_| "cannot encode config")?,
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_executable_unknown_and_future_schema_fields() {
        assert!(
            serde_json::from_str::<Config>(
                r#"{"schema_version":1,"profile":"read-only","command":"sh"}"#
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<Config>(r#"{"schema_version":1,"profile":"admin"}"#).is_err()
        );
        for text in [
            r#"{"schema_version":1,"profile":"read-only","allow_tools":false,"allow_tools":true}"#,
            r#"{"schema_version":1,"profile":"read-only","namespace":null,"namespace":"prod"}"#,
            r#"{"schema_version":1,"schema_version":2,"profile":"read-only"}"#,
        ] {
            assert!(serde_json::from_str::<Config>(text).is_err());
        }
    }

    fn options() -> Options {
        Options {
            connection: crate::gateway::Connection::default(),
            theme: None,
            color: true,
            mouse: true,
            json: false,
            allow_tools: false,
            allow_extensions: false,
            allow_scan: false,
            scanner: None,
            splash: true,
            poll: false,
            yaml: false,
            follow: false,
            previous: false,
            all_containers: false,
            dynamic_membership: false,
            dashboard: Vec::new(),
            dashboard_last: false,
            external_editor: false,
            helm_storage: None,
            helm_tool: None,
            helm_values: None,
            helm_registry_ca: None,
            helm_registry_auth: None,
            helm_keyring: None,
            helm_sigstore_policy: None,
            copilot_policy: None,
            helm_timeout_seconds: 300,
            log_seconds: crate::logs::DEFAULT_FOLLOW_SECONDS,
            log_read: crate::logs::ReadOptions::default(),
            selector: String::new(),
            interactive: false,
            metrics_resources: false,
            session_seconds: 900,
            node_shell_image: String::new(),
            recursive: false,
            command: vec![],
        }
    }

    #[test]
    fn helm_extension_configuration_constrains_but_never_enables_capabilities() -> Result<()> {
        let allowed = || Config {
            profile: Profile::Operator,
            allow_helm: true,
            allow_helm_write: true,
            allow_helm_hooks: true,
            allow_helm_crds: true,
            ..Config::default()
        };
        let mut disabled = options();
        apply(allowed(), &mut disabled)?;
        assert!(!disabled.connection.allow_helm_hooks);
        assert!(!disabled.connection.allow_helm_crds);
        for (hooks, crds) in [(true, false), (false, true)] {
            let mut selected = options();
            selected.connection.allow_helm_hooks = hooks;
            selected.connection.allow_helm_crds = crds;
            let mut denied = allowed();
            denied.allow_helm_hooks = false;
            denied.allow_helm_crds = false;
            assert!(apply(denied, &mut selected).is_err());
            assert!(selected.connection.context.is_none());
            apply(allowed(), &mut selected)?;
            assert_eq!(selected.connection.allow_helm_hooks, hooks);
            assert_eq!(selected.connection.allow_helm_crds, crds);
        }
        Ok(())
    }
    #[test]
    fn helm_write_permission_requires_both_cli_opt_in_and_operator_config() -> Result<()> {
        let mut selected = options();
        selected.connection.allow_helm_write = true;
        assert!(apply(Config::default(), &mut selected).is_err());
        let mut selected = options();
        selected.connection.allow_helm_write = true;
        selected.connection.writable = true;
        assert!(
            apply(
                Config {
                    allow_helm_write: true,
                    ..Config::default()
                },
                &mut selected
            )
            .is_err()
        );
        apply(
            Config {
                profile: Profile::Operator,
                allow_helm_write: true,
                ..Config::default()
            },
            &mut selected,
        )?;
        let mut disabled = options();
        apply(
            Config {
                profile: Profile::Operator,
                allow_helm_write: true,
                ..Config::default()
            },
            &mut disabled,
        )?;
        assert!(!disabled.connection.allow_helm_write);
        assert!(!disabled.connection.writable);
        Ok(())
    }
    #[test]
    fn files_constrain_capabilities_and_cli_scope_wins() -> Result<()> {
        let config = Config {
            context: Some("file-context".into()),
            namespace: Some("file-ns".into()),
            allow_logs: true,
            allow_exec: true,
            allow_attach: true,
            allow_forward: true,
            allow_tools: true,
            ..Config::default()
        };
        let mut opts = options();
        opts.connection.context = Some("cli-context".into());
        opts.connection.namespace = "cli-ns".into();
        opts.theme = Some("k8borg-dark".into());
        apply(config, &mut opts)?;
        assert_eq!(opts.connection.context.as_deref(), Some("cli-context"));
        assert_eq!(opts.connection.namespace, "cli-ns");
        assert_eq!(opts.theme.as_deref(), Some("k8borg-dark"));
        assert!(
            !opts.connection.logs
                && !opts.connection.allow_exec
                && !opts.connection.allow_attach
                && !opts.connection.allow_port_forward
                && !opts.allow_tools
        );

        let mut denied = options();
        denied.allow_tools = true;
        assert!(
            apply(
                Config {
                    context: Some("must-not-apply".into()),
                    ..Config::default()
                },
                &mut denied
            )
            .is_err()
        );
        assert!(denied.connection.context.is_none());
        let mut helm = options();
        helm.connection.allow_helm = true;
        assert!(apply(Config::default(), &mut helm).is_err());
        let mut allowed_helm = options();
        allowed_helm.connection.allow_helm = true;
        apply(
            Config {
                allow_helm: true,
                ..Config::default()
            },
            &mut allowed_helm,
        )?;
        assert!(allowed_helm.connection.allow_helm);
        let mut writable = options();
        writable.connection.writable = true;
        assert!(apply(Config::default(), &mut writable).is_err());
        let newer: [fn(&mut Options); 9] = [
            |o| o.connection.allow_secret_reveal = true,
            |o| o.connection.allow_force_kill = true,
            |o| o.connection.allow_yaml_edit = true,
            |o| o.connection.allow_node_maintenance = true,
            |o| o.connection.allow_file_copy = true,
            |o| o.connection.allow_apply = true,
            |o| o.connection.allow_node_shell = true,
            |o| o.connection.allow_debug = true,
            |o| o.external_editor = true,
        ];
        for enable in newer {
            let mut opts = options();
            enable(&mut opts);
            assert!(apply(Config::default(), &mut opts).is_err());
        }
        let mut reveal = options();
        reveal.connection.allow_secret_reveal = true;
        apply(
            Config {
                allow_secret_reveal: true,
                ..Config::default()
            },
            &mut reveal,
        )?;
        assert!(reveal.connection.allow_secret_reveal);
        let mut scan = options();
        scan.allow_scan = true;
        assert!(apply(Config::default(), &mut scan).is_err());
        assert!(!Config::default().allow_scan);
        let mut scan = options();
        scan.allow_scan = true;
        apply(
            Config {
                allow_scan: true,
                ..Config::default()
            },
            &mut scan,
        )?;
        assert!(scan.allow_scan);
        assert!(
            serde_json::from_str::<Config>(
                r#"{"schema_version":1,"profile":"read-only","allow_scan":true}"#
            )
            .map_err(|_| "allow_scan must parse")?
            .allow_scan
        );
        Ok(())
    }
}
