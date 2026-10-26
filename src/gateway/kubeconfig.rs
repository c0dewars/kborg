//! Kubeconfig discovery, kubectl-style multi-file merge, contexts and auth helper review.
use super::*;

/// Kubeconfig files in kubectl's order: `--kubeconfig`, then each entry of
/// `$KUBECONFIG`, then `~/.kube/config`.
pub(super) fn config_paths(options: &Connection) -> Result<Vec<PathBuf>> {
    if let Some(path) = &options.kubeconfig {
        return Ok(vec![path.clone()]);
    }
    if let Some(env) = std::env::var_os("KUBECONFIG") {
        let paths: Vec<_> = std::env::split_paths(&env)
            .filter(|path| !path.as_os_str().is_empty())
            .collect();
        if paths.len() > MAX_KUBECONFIG_FILES {
            return Err(format!(
                "KUBECONFIG lists more than {MAX_KUBECONFIG_FILES} files. Next: pass --kubeconfig with one file"
            ));
        }
        if !paths.is_empty() {
            return Ok(paths);
        }
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or("Home directory unavailable, so ~/.kube/config cannot be found. Next: set HOME or pass --kubeconfig FILE")?;
    Ok(vec![PathBuf::from(home).join(".kube/config")])
}
const MAX_KUBECONFIG_FILES: usize = 16;
/// Merges kubeconfig files like kubectl: the first file to define a cluster,
/// user or context name wins, and the first `current-context` wins. Listed
/// files that do not exist are skipped, as kubectl does; at least one must.
pub(super) fn read_config(options: &Connection) -> Result<Kubeconfig> {
    merge_config_files(config_paths(options)?)
}
pub(super) fn merge_config_files(paths: Vec<PathBuf>) -> Result<Kubeconfig> {
    let single = paths.len() == 1;
    let mut merged: Option<Kubeconfig> = None;
    for path in paths {
        if !path.exists() {
            if single {
                return Err(format!(
                    "No kubeconfig found at {}. Next: try `kborg demo` to explore without a cluster, or point --kubeconfig / $KUBECONFIG at your cluster config.",
                    safety::safe_text(&path.to_string_lossy(), 512)
                ));
            }
            continue;
        }
        let next = read_config_file(&path)?;
        merged = Some(match merged {
            None => next,
            Some(base) => base
                .merge(next)
                .map_err(|_| "Kubeconfig files disagree on kind or apiVersion. Next: fix the file that is not apiVersion v1, or pass --kubeconfig with one file")?,
        });
    }
    merged.ok_or_else(|| {
        "None of the KUBECONFIG files exist. Next: fix $KUBECONFIG or pass --kubeconfig FILE".into()
    })
}
/// One kubeconfig file, validated on its own; relative file references are
/// resolved against that file's directory before any merge.
pub(super) fn read_config_file(path: &std::path::Path) -> Result<Kubeconfig> {
    let bytes = safety::read_small(path, 1024 * 1024)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "Kubeconfig must be UTF-8. Next: save the file as UTF-8")?;
    let parent = path
        .parent()
        .ok_or("Invalid kubeconfig path. Next: pass --kubeconfig with a file path")?;
    parse_config_text(text, parent)
}
/// Kubeconfig text read from a file in `parent`: names validated, relative
/// file references resolved against `parent`.
pub(crate) fn parse_config_text(text: &str, parent: &std::path::Path) -> Result<Kubeconfig> {
    // Same decoding as `Kubeconfig::from_yaml`, but names are checked across
    // every YAML document before the first-wins merge could drop a duplicate.
    let documents: Vec<Kubeconfig> = serde_saphyr::from_multiple(text)
        .map_err(|_| "Invalid kubeconfig YAML (details suppressed to protect credentials). Next: check it with `kubectl config view`")?;
    for names in [
        documents
            .iter()
            .flat_map(|doc| doc.contexts.iter().map(|v| v.name.as_str()))
            .collect::<Vec<_>>(),
        documents
            .iter()
            .flat_map(|doc| doc.clusters.iter().map(|v| v.name.as_str()))
            .collect(),
        documents
            .iter()
            .flat_map(|doc| doc.auth_infos.iter().map(|v| v.name.as_str()))
            .collect(),
    ] {
        let mut unique = std::collections::HashSet::new();
        for name in names {
            if name.is_empty() || name.len() > 253 || safety::safe_text(name, 253) != name {
                return Err(
                    "Kubeconfig names must be bounded text without terminal controls. Next: rename the context, cluster or user (253 characters at most, no control characters)".into(),
                );
            }
            if !unique.insert(name) {
                return Err("Duplicate kubeconfig names are ambiguous and forbidden. Next: rename the duplicate context, cluster or user entry".into());
            }
        }
    }
    let mut raw = documents
        .into_iter()
        .try_fold(Kubeconfig::default(), Kubeconfig::merge)
        .map_err(|_| "Kubeconfig documents disagree on kind or apiVersion. Next: keep one apiVersion v1 Config per file")?;
    for cluster in &mut raw.clusters {
        if let Some(c) = &mut cluster.cluster
            && let Some(p) = &mut c.certificate_authority
        {
            *p = resolve_file(parent, p)?;
        }
    }
    for user in &mut raw.auth_infos {
        if let Some(u) = &mut user.auth_info {
            for p in [
                &mut u.client_certificate,
                &mut u.client_key,
                &mut u.token_file,
            ]
            .into_iter()
            .flatten()
            {
                *p = resolve_file(parent, p)?;
            }
        }
    }
    Ok(raw)
}
pub(super) fn resolve_file(parent: &std::path::Path, input: &str) -> Result<String> {
    let path = PathBuf::from(input);
    let path = if path.is_absolute() {
        path
    } else {
        parent.join(path)
    };
    Ok(path.to_string_lossy().into())
}
/// Context, cluster and user names plus the kubeconfig path list, for k9s
/// plugin variables (`$CONTEXT`, `$CLUSTER`, `$USER`, `$KUBECONFIG`). Names
/// only; no credential is read into the result.
pub fn plugin_identity(
    options: &Connection,
    context: Option<&str>,
) -> Result<(String, String, String, String)> {
    if options.demo {
        return Ok(("demo".into(), "demo".into(), "demo".into(), String::new()));
    }
    let paths = config_paths(options)?;
    let raw = merge_config_files(paths.clone())?;
    let name = context
        .map(str::to_owned)
        .or_else(|| options.context.clone())
        .or(raw.current_context.clone())
        .ok_or(
            "No kubeconfig context selected for the plugin. Next: pick one with :ctx or --context.",
        )?;
    let entry = raw
        .contexts
        .iter()
        .find(|entry| entry.name == name)
        .and_then(|entry| entry.context.as_ref())
        .ok_or("Plugin context not found in kubeconfig. Next: check the name with :ctx or kubectl config get-contexts.")?;
    let kubeconfig = std::env::join_paths(&paths)
        .map_err(|_| "KUBECONFIG paths cannot be joined for the plugin. Next: remove separators or unusual characters from the KUBECONFIG paths.")?
        .to_string_lossy()
        .into_owned();
    Ok((
        name.clone(),
        entry.cluster.clone(),
        entry.user.clone().unwrap_or_default(),
        kubeconfig,
    ))
}
pub fn contexts(options: &Connection) -> Result<Vec<String>> {
    if options.demo {
        return Ok(vec!["demo".into()]);
    }
    Ok(read_config(options)?
        .contexts
        .into_iter()
        .map(|c| c.name)
        .take(100)
        .collect())
}

/// Local metadata only: loading this catalog never creates a Kubernetes client
/// or executes an authentication helper. Credentials are not returned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContextSummary {
    pub name: String,
    pub cluster: String,
    pub server: String,
    pub insecure_tls: bool,
    pub namespace: String,
}
pub struct ConnectionCatalog {
    /// Internal digest only; never log credential-bearing configuration.
    pub fingerprint: [u8; 32],
    pub contexts: Vec<ContextSummary>,
    pub selected: Option<usize>,
    pub source: String,
}
/// Hash the exact parsed configuration used by the client. Sorting nested
/// object keys makes equivalent serialized maps stable across executions.
/// Relative file references have already been resolved; external credential
/// file contents can still rotate, as they do for kubectl.
pub(super) fn config_fingerprint(raw: &Kubeconfig) -> Result<[u8; 32]> {
    fn canonical(value: Value) -> Value {
        match value {
            Value::Object(map) => {
                let sorted: std::collections::BTreeMap<_, _> = map.into_iter().collect();
                Value::Object(
                    sorted
                        .into_iter()
                        .map(|(key, value)| (key, canonical(value)))
                        .collect(),
                )
            }
            Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
            other => other,
        }
    }
    let value = serde_json::to_value(raw).map_err(|_| "Cannot bind kubeconfig approval. Next: restart KBorg to review the configuration; if this repeats, report the failure without attaching credentials")?;
    let bytes =
        serde_json::to_vec(&canonical(value)).map_err(|_| "Cannot bind kubeconfig approval. Next: restart KBorg to review the configuration; if this repeats, report the failure without attaching credentials")?;
    ring::digest::digest(&ring::digest::SHA256, &bytes)
        .as_ref()
        .try_into()
        .map_err(|_| "Cannot bind kubeconfig approval. Next: restart KBorg to review the configuration; if this repeats, report the failure without attaching credentials".into())
}

pub fn connection_catalog(options: &Connection) -> Result<ConnectionCatalog> {
    let raw = read_config(options)?;
    catalog_from_config(options, &raw)
}
fn catalog_from_config(options: &Connection, raw: &Kubeconfig) -> Result<ConnectionCatalog> {
    let contexts: Vec<_> = raw
        .contexts
        .iter()
        .filter_map(|named| {
            named.context.as_ref().map(|context| ContextSummary {
                name: named.name.clone(),
                cluster: context.cluster.clone(),
                server: raw
                    .clusters
                    .iter()
                    .find(|cluster| cluster.name == context.cluster)
                    .and_then(|cluster| cluster.cluster.as_ref())
                    .and_then(|cluster| cluster.server.as_deref())
                    .and_then(|server| server.parse::<http::Uri>().ok())
                    .and_then(|uri| {
                        uri.authority().map(|authority| {
                            format!(
                                "{}://{}",
                                uri.scheme_str().unwrap_or("https"),
                                authority.as_str().rsplit('@').next().unwrap_or("invalid")
                            )
                        })
                    })
                    .unwrap_or_else(|| "(missing or invalid server)".into()),
                insecure_tls: raw
                    .clusters
                    .iter()
                    .find(|cluster| cluster.name == context.cluster)
                    .and_then(|cluster| cluster.cluster.as_ref())
                    .is_some_and(|cluster| cluster.insecure_skip_tls_verify == Some(true)),
                namespace: if options.namespace.is_empty() {
                    context
                        .namespace
                        .clone()
                        .unwrap_or_else(|| "default".into())
                } else {
                    options.namespace.clone()
                },
            })
        })
        .collect();
    if contexts.is_empty() {
        return Err("No contexts found in kubeconfig. Next: run your company's cluster login CLI (for example orbit-use), then start kborg in the same terminal.".into());
    }
    let selected_name = options
        .context
        .as_ref()
        .or(raw.current_context.as_ref())
        .filter(|name| !name.is_empty());
    let selected =
        selected_name.and_then(|name| contexts.iter().position(|context| &context.name == name));
    if selected_name.is_some() && selected.is_none() {
        return Err("Selected context is missing from kubeconfig. Next: run your cluster CLI again, or choose an existing context with --context NAME.".into());
    }
    let source = if let Some(path) = &options.kubeconfig {
        path.to_string_lossy().into_owned()
    } else if std::env::var_os("KUBECONFIG").is_some_and(|value| !value.is_empty()) {
        "KUBECONFIG (existing configured files)".into()
    } else {
        "~/.kube/config".into()
    };
    Ok(ConnectionCatalog {
        fingerprint: config_fingerprint(raw)?,
        contexts,
        selected,
        source,
    })
}

/// Prepare an exact local trust review without running a kubeconfig command.
pub fn review_auth_helper(
    options: &Connection,
    executable: Option<&std::path::Path>,
    explicit_env: &[(String, String)],
) -> Result<crate::auth::PreparedPolicy> {
    if options.demo {
        return Err("The offline demo has no authentication helper. Next: select a real context with --context".into());
    }
    let raw = read_config(options)?;
    let name = options
        .context
        .as_ref()
        .or(raw.current_context.as_ref())
        .ok_or("No auth helper context selected. Next: pass --context NAME")?;
    let context = raw
        .contexts
        .iter()
        .find(|context| &context.name == name)
        .and_then(|context| context.context.as_ref())
        .ok_or("Context not found in kubeconfig. Next: list contexts with `kubectl config get-contexts` and pass --context NAME")?;
    let cluster = raw
        .clusters
        .iter()
        .find(|cluster| cluster.name == context.cluster)
        .and_then(|cluster| cluster.cluster.as_ref())
        .ok_or("The context's cluster entry is missing from kubeconfig. Next: fix the cluster name in that context or re-add the cluster (for example with your cloud CLI)")?;
    let user_name = context
        .user
        .as_deref()
        .ok_or("The context has no user. Next: set a user for this context in kubeconfig")?;
    let user = raw
        .auth_infos
        .iter()
        .find(|user| user.name == user_name)
        .and_then(|user| user.auth_info.as_ref())
        .ok_or("The context's user entry is missing from kubeconfig. Next: fix the user name in that context or re-add the user")?;
    validate_helper_user(user)?;
    let exec = user
        .exec
        .as_ref()
        .ok_or("The selected kubeconfig user has no ExecCredential helper. Next: pick a context whose user has an exec helper; token or certificate users need no review")?;
    crate::auth::prepare(
        crate::auth::Binding::new(name, user_name, cluster, exec)?,
        executable,
        explicit_env,
    )
}

pub(super) fn validate_helper_user(user: &kube::config::AuthInfo) -> Result<()> {
    if user.auth_provider.is_some()
        || user.username.is_some()
        || user.password.is_some()
        || user.token.is_some()
        || user.token_file.is_some()
        || user.client_certificate.is_some()
        || user.client_certificate_data.is_some()
        || user.client_key.is_some()
        || user.client_key_data.is_some()
        || user.impersonate.is_some()
        || user.impersonate_groups.is_some()
        || user.impersonate_uid.is_some()
        || user.impersonate_user_extra.is_some()
        || !user.other.is_empty()
    {
        return Err("ExecCredential helpers cannot be mixed with other authentication, legacy providers or impersonation. Next: remove the other credentials from this kubeconfig user, or use a separate user entry".into());
    }
    Ok(())
}

/// One line per check of the selected context, for `kborg doctor`. Reads the
/// kubeconfig only: nothing is executed and no credential is printed.
pub fn audit(options: &Connection) -> Result<Vec<String>> {
    let raw = read_config(options)?;
    let mut lines = Vec::new();
    let name = options
        .context
        .clone()
        .or_else(|| raw.current_context.clone())
        .ok_or("No context selected: kubeconfig has no current-context. Next: pass --context NAME or run `kubectl config use-context NAME`")?;
    lines.push(format!(
        "contexts: {} in kubeconfig; selected: {}",
        raw.contexts.len(),
        safety::safe_text(&name, 253)
    ));
    let context = raw
        .contexts
        .iter()
        .find(|c| c.name == name)
        .and_then(|c| c.context.as_ref())
        .ok_or("Context not found in kubeconfig. Next: list contexts with `kubectl config get-contexts` and pass --context NAME")?;
    if let Some(cluster) = raw
        .clusters
        .iter()
        .find(|c| c.name == context.cluster)
        .and_then(|c| c.cluster.as_ref())
    {
        let server = cluster.server.as_deref().unwrap_or("-");
        let host = server
            .split("://")
            .nth(1)
            .unwrap_or(server)
            .split(['/', '?'])
            .next()
            .unwrap_or_default();
        lines.push(format!("server: {}", safety::safe_text(host, 200)));
        if cluster.insecure_skip_tls_verify == Some(true) {
            lines.push("tls: insecure-skip-tls-verify is set (certificate not verified, as kubectl does with this setting)".into());
        }
        if let Some(proxy) = &cluster.proxy_url {
            let proxy = proxy.rsplit('@').next().unwrap_or_default();
            lines.push(format!("proxy: {}", safety::safe_text(proxy, 200)));
        }
    }
    let user = context
        .user
        .as_ref()
        .and_then(|user| raw.auth_infos.iter().find(|u| &u.name == user))
        .and_then(|u| u.auth_info.as_ref());
    let auth = match user {
        None => "none (anonymous)".to_owned(),
        Some(user) if user.exec.is_some() => {
            let command = user
                .exec
                .as_ref()
                .and_then(|exec| exec.command.clone())
                .unwrap_or_default();
            let found = crate::extensions::resolve_on_path(&command);
            format!(
                "exec helper `{}` ({}); KBorg runs it like kubectl",
                safety::safe_text(&command, 200),
                match found {
                    Some(path) => format!(
                        "found at {}",
                        safety::safe_text(&path.to_string_lossy(), 300)
                    ),
                    None => "NOT FOUND on PATH. Next: install it or add it to PATH".into(),
                }
            )
        }
        Some(user) if user.auth_provider.is_some() => {
            "legacy auth-provider (kubectl's provider; may need the provider plugin)".into()
        }
        Some(user) if user.token.is_some() || user.token_file.is_some() => "bearer token".into(),
        Some(user)
            if user.client_certificate.is_some() || user.client_certificate_data.is_some() =>
        {
            "client certificate".into()
        }
        Some(user) if user.username.is_some() => "basic auth".into(),
        Some(_) => "none (anonymous)".into(),
    };
    lines.push(format!("auth: {auth}"));
    if user.is_some_and(|user| user.impersonate.is_some() || user.impersonate_groups.is_some()) {
        lines.push(
            "impersonation: set in kubeconfig (requests run as the impersonated user)".into(),
        );
    }
    Ok(lines)
}

#[cfg(test)]
mod connection_catalog_tests {
    use super::*;
    #[test]
    fn existing_context_and_credentials_need_no_extra_setup() -> Result<()> {
        let raw = parse_config_text(
            r#"apiVersion: v1
kind: Config
current-context: company-prod
clusters:
- name: company
  cluster: {server: https://example.test}
contexts:
- name: company-prod
  context: {cluster: company, user: company-user, namespace: team}
- name: company-dev
  context: {cluster: company, user: company-user}
users:
- name: company-user
  user:
    token: NEVER-DISPLAY-THIS
    exec:
      apiVersion: client.authentication.k8s.io/v1
      command: NEVER-RUN-THIS-DURING-PREVIEW
      interactiveMode: Never
"#,
            std::path::Path::new("."),
        )?;
        let mut options = Connection::default();
        let catalog = catalog_from_config(&options, &raw)?;
        assert_eq!(catalog.selected, Some(0));
        assert_eq!(catalog.contexts[0].name, "company-prod");
        assert_eq!(catalog.contexts[0].namespace, "team");
        assert!(!format!("{:?}", catalog.contexts).contains("NEVER-"));
        options.context = Some("company-dev".into());
        options.namespace = "override".into();
        let catalog = catalog_from_config(&options, &raw)?;
        assert_eq!(catalog.selected, Some(1));
        assert_eq!(catalog.contexts[1].namespace, "override");
        options.context = Some("absent".into());
        assert!(catalog_from_config(&options, &raw).is_err());
        options.context = None;
        let mut no_current = raw;
        no_current.current_context = None;
        assert_eq!(catalog_from_config(&options, &no_current)?.selected, None);
        Ok(())
    }
}
