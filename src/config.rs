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
    pub allow_exec: bool,
    #[serde(default)]
    pub allow_attach: bool,
    #[serde(default)]
    pub allow_forward: bool,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: 1,
            profile: Profile::ReadOnly,
            context: None,
            namespace: None,
            theme: Some("catppuccin-mocha".into()),
            no_splash: false,
            poll: false,
            allow_logs: false,
            allow_exec: false,
            allow_attach: false,
            allow_forward: false,
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
    // A file may constrain log access, but never enable it by itself.
    if options.connection.logs && !config.allow_logs {
        return Err("configuration forbids log access".into());
    }
    if (options.connection.allow_exec && !config.allow_exec)
        || (options.connection.allow_attach && !config.allow_attach)
        || (options.connection.allow_port_forward && !config.allow_forward)
    {
        return Err("configuration forbids requested subresource capability".into());
    }
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
    }
}
