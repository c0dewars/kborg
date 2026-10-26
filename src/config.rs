//! Operator configuration: defaults, and capabilities a company turns off.
use crate::{Result, cli::Options, safety};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Deserialize, Serialize, Default, PartialEq, Debug)]
#[serde(rename_all = "kebab-case")]
pub enum Profile {
    ReadOnly,
    #[default]
    Operator,
}
fn yes() -> bool {
    true
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema_version: u32,
    #[serde(default)]
    pub profile: Profile,
    /// Company strict mode: every change needs its typed confirmation phrase
    /// instead of one key.
    #[serde(default)]
    pub strict_confirmations: bool,
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
    #[serde(default = "yes")]
    pub allow_logs: bool,
    #[serde(default = "yes")]
    pub allow_helm: bool,
    #[serde(default = "yes")]
    pub allow_helm_write: bool,
    #[serde(default = "yes")]
    pub allow_helm_hooks: bool,
    #[serde(default = "yes")]
    pub allow_helm_crds: bool,
    #[serde(default = "yes")]
    pub allow_exec: bool,
    #[serde(default = "yes")]
    pub allow_attach: bool,
    #[serde(default = "yes")]
    pub allow_forward: bool,
    #[serde(default = "yes")]
    pub allow_file_copy: bool,
    #[serde(default = "yes")]
    pub allow_tools: bool,
    #[serde(default = "yes")]
    pub allow_extensions: bool,
    #[serde(default = "yes")]
    pub allow_auth_helper: bool,
    #[serde(default = "yes")]
    pub allow_secret_reveal: bool,
    #[serde(default = "yes")]
    pub allow_force_kill: bool,
    #[serde(default = "yes")]
    pub allow_yaml_edit: bool,
    #[serde(default = "yes")]
    pub allow_node_maintenance: bool,
    #[serde(default = "yes")]
    pub allow_external_editor: bool,
    #[serde(default = "yes")]
    pub allow_apply: bool,
    #[serde(default = "yes")]
    pub allow_node_shell: bool,
    #[serde(default = "yes")]
    pub allow_copilot_screen: bool,
    #[serde(default = "yes")]
    pub allow_debug: bool,
    #[serde(default = "yes")]
    pub allow_scan: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: 1,
            profile: Profile::Operator,
            strict_confirmations: false,
            context: None,
            namespace: None,
            theme: Some(crate::themes::DEFAULT_THEME.into()),
            no_splash: false,
            poll: false,
            allow_logs: true,
            allow_helm: true,
            allow_helm_write: true,
            allow_helm_hooks: true,
            allow_helm_crds: true,
            allow_exec: true,
            allow_attach: true,
            allow_forward: true,
            allow_file_copy: true,
            allow_tools: true,
            allow_extensions: true,
            allow_auth_helper: true,
            allow_secret_reveal: true,
            allow_force_kill: true,
            allow_yaml_edit: true,
            allow_node_maintenance: true,
            allow_external_editor: true,
            allow_apply: true,
            allow_node_shell: true,
            allow_copilot_screen: true,
            allow_debug: true,
            allow_scan: true,
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
    // An operator configuration only takes capabilities away: everything is
    // on by default, and a field set to false (or the read-only profile)
    // turns that capability off whatever the command line says.
    let c = &mut options.connection;
    if config.profile == Profile::ReadOnly {
        c.writable = false;
    }
    c.allow_helm &= config.allow_helm;
    c.allow_helm_write &= config.allow_helm_write;
    c.allow_helm_hooks &= config.allow_helm_hooks;
    c.allow_helm_crds &= config.allow_helm_crds;
    c.logs &= config.allow_logs;
    c.allow_exec &= config.allow_exec;
    c.allow_attach &= config.allow_attach;
    c.allow_port_forward &= config.allow_forward;
    c.allow_file_copy &= config.allow_file_copy;
    c.allow_auth_helper &= config.allow_auth_helper;
    c.allow_secret_reveal &= config.allow_secret_reveal;
    c.allow_force_kill &= config.allow_force_kill;
    c.allow_yaml_edit &= config.allow_yaml_edit;
    c.allow_node_maintenance &= config.allow_node_maintenance;
    c.allow_apply &= config.allow_apply;
    c.allow_node_shell &= config.allow_node_shell;
    c.allow_debug &= config.allow_debug;
    options.allow_tools &= config.allow_tools;
    options.allow_extensions &= config.allow_extensions;
    options.allow_scan &= config.allow_scan;
    options.allow_copilot_screen &= config.allow_copilot_screen;
    options.external_editor &= config.allow_external_editor;
    options.strict_confirm |= config.strict_confirmations;
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

    /// Options as the CLI builds them: everything on.
    fn options() -> Options {
        let mut options = Options {
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
            allow_copilot_screen: false,
            strict_confirm: false,
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
        };
        crate::cli::enable_everything(&mut options);
        options
    }
    fn all_on(o: &Options) -> bool {
        let c = &o.connection;
        c.writable
            && c.logs
            && c.allow_helm
            && c.allow_helm_write
            && c.allow_helm_hooks
            && c.allow_helm_crds
            && c.allow_exec
            && c.allow_attach
            && c.allow_port_forward
            && c.allow_file_copy
            && c.allow_node_shell
            && c.allow_debug
            && c.allow_node_maintenance
            && c.allow_yaml_edit
            && c.allow_apply
            && c.allow_force_kill
            && c.allow_secret_reveal
            && o.allow_tools
            && o.allow_extensions
            && o.allow_scan
            && o.external_editor
            && o.allow_copilot_screen
    }

    #[test]
    fn default_and_minimal_configs_keep_everything_on() -> Result<()> {
        let mut opts = options();
        assert!(all_on(&opts));
        apply(Config::default(), &mut opts)?;
        assert!(all_on(&opts) && !opts.strict_confirm);
        let minimal: Config =
            serde_json::from_str(r#"{"schema_version":1}"#).map_err(|_| "minimal config")?;
        assert_eq!(minimal.profile, Profile::Operator);
        let mut opts = options();
        apply(minimal, &mut opts)?;
        assert!(all_on(&opts));
        Ok(())
    }

    #[test]
    fn each_false_field_turns_its_capability_off() -> Result<()> {
        type Check = (&'static str, fn(&Options) -> bool);
        let off: [Check; 22] = [
            ("allow_logs", |o| o.connection.logs),
            ("allow_helm", |o| o.connection.allow_helm),
            ("allow_helm_write", |o| o.connection.allow_helm_write),
            ("allow_helm_hooks", |o| o.connection.allow_helm_hooks),
            ("allow_helm_crds", |o| o.connection.allow_helm_crds),
            ("allow_exec", |o| o.connection.allow_exec),
            ("allow_attach", |o| o.connection.allow_attach),
            ("allow_forward", |o| o.connection.allow_port_forward),
            ("allow_file_copy", |o| o.connection.allow_file_copy),
            ("allow_tools", |o| o.allow_tools),
            ("allow_extensions", |o| o.allow_extensions),
            ("allow_secret_reveal", |o| o.connection.allow_secret_reveal),
            ("allow_force_kill", |o| o.connection.allow_force_kill),
            ("allow_yaml_edit", |o| o.connection.allow_yaml_edit),
            ("allow_node_maintenance", |o| {
                o.connection.allow_node_maintenance
            }),
            ("allow_external_editor", |o| o.external_editor),
            ("allow_apply", |o| o.connection.allow_apply),
            ("allow_node_shell", |o| o.connection.allow_node_shell),
            ("allow_copilot_screen", |o| o.allow_copilot_screen),
            ("allow_debug", |o| o.connection.allow_debug),
            ("allow_scan", |o| o.allow_scan),
            ("allow_auth_helper", |o| !o.connection.allow_auth_helper),
        ];
        for (field, enabled) in off {
            let config: Config =
                serde_json::from_str(&format!(r#"{{"schema_version":1,"{field}":false}}"#))
                    .map_err(|_| format!("{field} must parse"))?;
            let mut opts = options();
            apply(config, &mut opts)?;
            if field == "allow_auth_helper" {
                assert!(enabled(&opts), "{field}");
            } else {
                assert!(!enabled(&opts), "{field} stayed on");
            }
            assert!(opts.connection.writable, "{field} must not touch writes");
        }
        Ok(())
    }

    #[test]
    fn read_only_profile_and_strict_confirmations() -> Result<()> {
        let config: Config = serde_json::from_str(
            r#"{"schema_version":1,"profile":"read-only","strict_confirmations":true}"#,
        )
        .map_err(|_| "profile must parse")?;
        let mut opts = options();
        apply(config, &mut opts)?;
        assert!(!opts.connection.writable && opts.strict_confirm);
        assert!(opts.connection.logs && opts.allow_scan);
        Ok(())
    }

    #[test]
    fn files_fill_scope_and_cli_scope_wins() -> Result<()> {
        let config = || Config {
            context: Some("file-context".into()),
            namespace: Some("file-ns".into()),
            ..Config::default()
        };
        let mut opts = options();
        opts.connection.context = Some("cli-context".into());
        opts.connection.namespace = "cli-ns".into();
        opts.theme = Some("k8borg-dark".into());
        apply(config(), &mut opts)?;
        assert_eq!(opts.connection.context.as_deref(), Some("cli-context"));
        assert_eq!(opts.connection.namespace, "cli-ns");
        assert_eq!(opts.theme.as_deref(), Some("k8borg-dark"));
        let mut opts = options();
        apply(config(), &mut opts)?;
        assert_eq!(opts.connection.context.as_deref(), Some("file-context"));
        assert_eq!(opts.connection.namespace, "file-ns");
        Ok(())
    }
}
