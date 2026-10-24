#[path = "discovery.rs"]
mod discovery;
use crate::{
    Result,
    policy::{Action, Approval, Policy, Target},
    safety,
};
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api, Client, Config,
    api::{
        ApiResource, DeleteParams, DynamicObject, ListParams, Patch, PatchParams, Preconditions,
    },
    config::{KubeConfigOptions, Kubeconfig},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const MAX_OBJECTS: usize = 1000;
const MAX_BODY: usize = 8 * 1024 * 1024;
const MAX_LIST_PAGES: usize = 10;
const OPERATION_BUDGET: Duration = Duration::from_secs(15);

#[derive(Clone, Default)]
pub struct Connection {
    pub kubeconfig: Option<PathBuf>,
    pub context: Option<String>,
    pub namespace: String,
    pub demo: bool,
    pub writable: bool,
    pub logs: bool,
    pub allow_helm: bool,
    pub allow_helm_write: bool,
    pub allow_helm_hooks: bool,
    pub allow_helm_crds: bool,
    pub allow_exec: bool,
    pub allow_attach: bool,
    pub allow_port_forward: bool,
    pub allow_node_maintenance: bool,
    pub allow_yaml_edit: bool,
    pub allow_force_kill: bool,
    pub allow_secret_reveal: bool,
    pub allow_auth_helper: bool,
    pub auth_helper_policy: Option<PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Kind {
    pub plural: String,
    pub kind: String,
    pub group: String,
    pub version: String,
    pub namespaced: bool,
    #[serde(default)]
    pub short_names: Vec<String>,
}
impl Kind {
    fn ar(&self) -> ApiResource {
        ApiResource {
            group: self.group.clone(),
            version: self.version.clone(),
            api_version: if self.group.is_empty() {
                self.version.clone()
            } else {
                format!("{}/{}", self.group, self.version)
            },
            kind: self.kind.clone(),
            plural: self.plural.clone(),
        }
    }
    pub fn key(&self) -> String {
        if self.group.is_empty() {
            self.plural.clone()
        } else {
            format!("{}.{}", self.plural, self.group)
        }
    }
}
fn builtin_aliases(kind: &str) -> &'static [&'static str] {
    match kind {
        "Pod" => &["po"],
        "Deployment" => &["deploy", "dp"],
        "Service" => &["svc"],
        "Job" => &["jo"],
        "Role" => &["ro"],
        "RoleBinding" => &["rb"],
        "ClusterRole" => &["cr"],
        "ClusterRoleBinding" => &["crb"],
        "Event" => &["ev"],
        "StatefulSet" => &["sts"],
        "DaemonSet" => &["ds"],
        "ReplicaSet" => &["rs"],
        "CronJob" => &["cj"],
        "Ingress" => &["ing"],
        "ConfigMap" => &["cm"],
        "PersistentVolumeClaim" => &["pvc"],
        "Node" => &["no"],
        "Namespace" => &["ns"],
        "StorageClass" => &["sc"],
        "ServiceAccount" => &["sa"],
        "PersistentVolume" => &["pv"],
        "Endpoints" => &["ep"],
        "ReplicationController" => &["rc"],
        "HorizontalPodAutoscaler" => &["hpa"],
        "PodDisruptionBudget" => &["pdb"],
        "NetworkPolicy" => &["netpol", "np"],
        "ResourceQuota" => &["quota"],
        "LimitRange" => &["limits"],
        "CustomResourceDefinition" => &["crd", "crds"],
        "PriorityClass" => &["pc"],
        "CertificateSigningRequest" => &["csr"],
        "VolumeAttachment" => &["va"],
        _ => &[],
    }
}
pub fn kinds() -> Vec<Kind> {
    [
        ("pods", "Pod", "", "v1", true),
        ("deployments", "Deployment", "apps", "v1", true),
        ("services", "Service", "", "v1", true),
        ("events", "Event", "", "v1", true),
        ("statefulsets", "StatefulSet", "apps", "v1", true),
        ("daemonsets", "DaemonSet", "apps", "v1", true),
        ("replicasets", "ReplicaSet", "apps", "v1", true),
        ("jobs", "Job", "batch", "v1", true),
        ("cronjobs", "CronJob", "batch", "v1", true),
        ("ingresses", "Ingress", "networking.k8s.io", "v1", true),
        ("configmaps", "ConfigMap", "", "v1", true),
        (
            "persistentvolumeclaims",
            "PersistentVolumeClaim",
            "",
            "v1",
            true,
        ),
        ("nodes", "Node", "", "v1", false),
        ("namespaces", "Namespace", "", "v1", false),
        (
            "storageclasses",
            "StorageClass",
            "storage.k8s.io",
            "v1",
            false,
        ),
        ("roles", "Role", "rbac.authorization.k8s.io", "v1", true),
        ("serviceaccounts", "ServiceAccount", "", "v1", true),
        ("persistentvolumes", "PersistentVolume", "", "v1", false),
        ("endpoints", "Endpoints", "", "v1", true),
        (
            "replicationcontrollers",
            "ReplicationController",
            "",
            "v1",
            true,
        ),
        ("resourcequotas", "ResourceQuota", "", "v1", true),
        ("limitranges", "LimitRange", "", "v1", true),
        (
            "endpointslices",
            "EndpointSlice",
            "discovery.k8s.io",
            "v1",
            true,
        ),
        (
            "horizontalpodautoscalers",
            "HorizontalPodAutoscaler",
            "autoscaling",
            "v2",
            true,
        ),
        (
            "poddisruptionbudgets",
            "PodDisruptionBudget",
            "policy",
            "v1",
            true,
        ),
        (
            "networkpolicies",
            "NetworkPolicy",
            "networking.k8s.io",
            "v1",
            true,
        ),
        (
            "customresourcedefinitions",
            "CustomResourceDefinition",
            "apiextensions.k8s.io",
            "v1",
            false,
        ),
        (
            "clusterroles",
            "ClusterRole",
            "rbac.authorization.k8s.io",
            "v1",
            false,
        ),
        (
            "clusterrolebindings",
            "ClusterRoleBinding",
            "rbac.authorization.k8s.io",
            "v1",
            false,
        ),
        (
            "priorityclasses",
            "PriorityClass",
            "scheduling.k8s.io",
            "v1",
            false,
        ),
        ("runtimeclasses", "RuntimeClass", "node.k8s.io", "v1", false),
        (
            "certificatesigningrequests",
            "CertificateSigningRequest",
            "certificates.k8s.io",
            "v1",
            false,
        ),
        (
            "volumeattachments",
            "VolumeAttachment",
            "storage.k8s.io",
            "v1",
            false,
        ),
        (
            "apiservices",
            "APIService",
            "apiregistration.k8s.io",
            "v1",
            false,
        ),
        (
            "rolebindings",
            "RoleBinding",
            "rbac.authorization.k8s.io",
            "v1",
            true,
        ),
    ]
    .into_iter()
    .map(|(p, k, g, v, n)| Kind {
        plural: p.into(),
        kind: k.into(),
        group: g.into(),
        version: v.into(),
        namespaced: n,
        short_names: builtin_aliases(k).iter().map(|s| (*s).into()).collect(),
    })
    .collect()
}

/// Kubeconfig files in kubectl's order: `--kubeconfig`, then each entry of
/// `$KUBECONFIG`, then `~/.kube/config`.
fn config_paths(options: &Connection) -> Result<Vec<PathBuf>> {
    if let Some(path) = &options.kubeconfig {
        return Ok(vec![path.clone()]);
    }
    if let Some(env) = std::env::var_os("KUBECONFIG") {
        let paths: Vec<_> = std::env::split_paths(&env)
            .filter(|path| !path.as_os_str().is_empty())
            .collect();
        if paths.len() > MAX_KUBECONFIG_FILES {
            return Err(format!(
                "KUBECONFIG lists more than {MAX_KUBECONFIG_FILES} files; pass --kubeconfig with one file"
            ));
        }
        if !paths.is_empty() {
            return Ok(paths);
        }
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or("home directory unavailable")?;
    Ok(vec![PathBuf::from(home).join(".kube/config")])
}
const MAX_KUBECONFIG_FILES: usize = 16;
/// Merges kubeconfig files like kubectl: the first file to define a cluster,
/// user or context name wins, and the first `current-context` wins. Listed
/// files that do not exist are skipped, as kubectl does; at least one must.
fn read_config(options: &Connection) -> Result<Kubeconfig> {
    merge_config_files(config_paths(options)?)
}
fn merge_config_files(paths: Vec<PathBuf>) -> Result<Kubeconfig> {
    let single = paths.len() == 1;
    let mut merged: Option<Kubeconfig> = None;
    for path in paths {
        if !single && !path.exists() {
            continue;
        }
        let next = read_config_file(&path)?;
        merged = Some(match merged {
            None => next,
            Some(base) => base
                .merge(next)
                .map_err(|_| "kubeconfig files disagree on kind or apiVersion")?,
        });
    }
    merged.ok_or_else(|| "none of the KUBECONFIG files exist".into())
}
/// One kubeconfig file, validated on its own; relative file references are
/// resolved against that file's directory before any merge.
fn read_config_file(path: &std::path::Path) -> Result<Kubeconfig> {
    let bytes = safety::read_small(path, 1024 * 1024)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "kubeconfig must be UTF-8")?;
    let mut raw = Kubeconfig::from_yaml(text)
        .map_err(|_| "invalid kubeconfig YAML (details suppressed to protect credentials)")?;
    for names in [
        raw.contexts
            .iter()
            .map(|v| v.name.as_str())
            .collect::<Vec<_>>(),
        raw.clusters.iter().map(|v| v.name.as_str()).collect(),
        raw.auth_infos.iter().map(|v| v.name.as_str()).collect(),
    ] {
        let mut unique = std::collections::HashSet::new();
        for name in names {
            if name.is_empty() || name.len() > 253 || safety::safe_text(name, 253) != name {
                return Err(
                    "kubeconfig names must be bounded text without terminal controls".into(),
                );
            }
            if !unique.insert(name) {
                return Err("duplicate kubeconfig names are ambiguous and forbidden".into());
            }
        }
    }
    let parent = path.parent().ok_or("invalid kubeconfig path")?;
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
fn resolve_file(parent: &std::path::Path, input: &str) -> Result<String> {
    let path = PathBuf::from(input);
    let path = if path.is_absolute() {
        path
    } else {
        parent.join(path)
    };
    Ok(path.to_string_lossy().into())
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

/// Prepare an exact local trust review without running a kubeconfig command.
pub fn review_auth_helper(
    options: &Connection,
    executable: Option<&std::path::Path>,
    explicit_env: &[(String, String)],
) -> Result<crate::auth::PreparedPolicy> {
    if options.demo {
        return Err("offline demo has no authentication helper".into());
    }
    let raw = read_config(options)?;
    let name = options
        .context
        .as_ref()
        .or(raw.current_context.as_ref())
        .ok_or("select an auth helper context explicitly")?;
    let context = raw
        .contexts
        .iter()
        .find(|context| &context.name == name)
        .and_then(|context| context.context.as_ref())
        .ok_or("context not found")?;
    let cluster = raw
        .clusters
        .iter()
        .find(|cluster| cluster.name == context.cluster)
        .and_then(|cluster| cluster.cluster.as_ref())
        .ok_or("cluster not found")?;
    let user_name = context
        .user
        .as_deref()
        .ok_or("context authentication user missing")?;
    let user = raw
        .auth_infos
        .iter()
        .find(|user| user.name == user_name)
        .and_then(|user| user.auth_info.as_ref())
        .ok_or("authentication user missing")?;
    validate_helper_user(user)?;
    let exec = user
        .exec
        .as_ref()
        .ok_or("selected kubeconfig user has no ExecCredential helper")?;
    crate::auth::prepare(
        crate::auth::Binding::new(name, user_name, cluster, exec)?,
        executable,
        explicit_env,
    )
}

fn validate_helper_user(user: &kube::config::AuthInfo) -> Result<()> {
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
        return Err("ExecCredential helpers cannot be mixed with other authentication, legacy providers or impersonation".into());
    }
    Ok(())
}

#[derive(Clone)]
pub struct Gateway {
    client: Option<Client>,
    pub context: String,
    pub server: String,
    pub namespace: String,
    session: String,
    policy: Policy,
    logs: bool,
    allow_helm: bool,
    allow_helm_write: bool,
    allow_helm_hooks: bool,
    allow_helm_crds: bool,
    lifecycle_connection: Option<crate::helm_lifecycle::StaticConnection>,
    operation_capabilities: [bool; 3],
    allow_node_maintenance: bool,
    allow_yaml_edit: bool,
    allow_force_kill: bool,
    allow_secret_reveal: bool,
    demo: Vec<Value>,
    pub kinds: Vec<Kind>,
}
#[derive(Clone)]
pub struct Prepared {
    pub target: Target,
    pub action: Action,
    pub summary: String,
    patch: Value,
    kind: Kind,
    created: std::time::Instant,
    reviewed_target: Target,
    reviewed_action: Action,
    reviewed_summary: String,
}

impl Gateway {
    pub async fn connect(options: Connection) -> Result<Self> {
        if options.demo {
            let namespace = if options.namespace.is_empty() {
                "demo".into()
            } else {
                options.namespace
            };
            if namespace != "*" && !safety::valid_id(&namespace) {
                return Err("invalid namespace".into());
            }
            return Ok(Self {
                client: None,
                context: "demo".into(),
                server: "offline sample data".into(),
                namespace,
                session: "demo".into(),
                policy: Policy {
                    writable: options.writable,
                },
                logs: true,
                allow_helm: true,
                allow_helm_write: options.allow_helm_write,
                allow_helm_hooks: options.allow_helm_hooks,
                allow_helm_crds: options.allow_helm_crds,
                lifecycle_connection: None,
                allow_node_maintenance: options.allow_node_maintenance,
                allow_yaml_edit: options.allow_yaml_edit,
                allow_force_kill: options.allow_force_kill,
                allow_secret_reveal: options.allow_secret_reveal,
                operation_capabilities: [
                    options.allow_exec,
                    options.allow_attach,
                    options.allow_port_forward,
                ],
                demo: demo_objects(),
                kinds: kinds(),
            });
        }
        let mut raw = read_config(&options)?;
        let name = options
            .context
            .clone()
            .or_else(|| raw.current_context.clone())
            .ok_or("select a context with --context")?;
        let context = raw
            .contexts
            .iter()
            .find(|c| c.name == name)
            .and_then(|c| c.context.as_ref())
            .ok_or("context not found")?
            .clone();
        let cluster = raw
            .clusters
            .iter()
            .find(|c| c.name == context.cluster)
            .and_then(|c| c.cluster.as_ref())
            .ok_or("cluster not found")?
            .clone();
        let user = context
            .user
            .as_ref()
            .and_then(|name| raw.auth_infos.iter().find(|u| &u.name == name))
            .and_then(|u| u.auth_info.as_ref())
            .cloned();
        if cluster.insecure_skip_tls_verify == Some(true) {
            return Err("insecure TLS is forbidden".into());
        }
        if cluster.proxy_url.is_some() {
            return Err(
                "proxy configurations need a reviewed adapter; unsupported in this iteration"
                    .into(),
            );
        }
        if let Some(u) = &user {
            if u.auth_provider.is_some() {
                return Err("legacy authentication providers remain disabled; use a reviewed ExecCredential helper or short-lived token/certificate kubeconfig".into());
            }
            if u.impersonate.is_some()
                || u.impersonate_groups.is_some()
                || u.impersonate_uid.is_some()
                || u.impersonate_user_extra.is_some()
            {
                return Err("impersonation is disabled".into());
            }
            for path in [&u.client_certificate, &u.client_key, &u.token_file]
                .into_iter()
                .flatten()
            {
                safety::read_small(&PathBuf::from(path), 1024 * 1024)?;
            }
        }
        if let Some(path) = &cluster.certificate_authority {
            safety::read_small(&PathBuf::from(path), 1024 * 1024)?;
        }
        let auth_gate = if let Some(user) = &user
            && let Some(exec) = &user.exec
        {
            if !options.allow_auth_helper {
                return Err("authentication helper is unreviewed/disabled; review it with auth-helper review and enable --allow-auth-helper --auth-helper-policy FILE explicitly".into());
            }
            validate_helper_user(user)?;
            let policy = options
                .auth_helper_policy
                .clone()
                .ok_or("--allow-auth-helper requires --auth-helper-policy FILE")?;
            let user_name = context.user.as_deref().ok_or("helper user missing")?;
            let binding = crate::auth::Binding::new(&name, user_name, &cluster, exec)?;
            let credentials =
                tokio::task::spawn_blocking(move || crate::auth::acquire(&policy, &binding))
                    .await
                    .map_err(|_| "auth helper worker failed; credentials suppressed")??;
            let (auth, gate) = credentials.into_auth()?;
            let selected_user = raw
                .auth_infos
                .iter_mut()
                .find(|user| user.name == user_name)
                .ok_or("helper user missing")?;
            // Kube's automatic ExecCredential refresh is never configured. The
            // supervised result is the only credential source for this client.
            selected_user.auth_info = Some(auth);
            gate
        } else {
            crate::auth::AuthGate::default()
        };
        let mut config = Config::from_custom_kubeconfig(
            raw,
            &KubeConfigOptions {
                context: Some(name.clone()),
                ..Default::default()
            },
        )
        .await
        .map_err(|_| "cannot load cluster credentials/CA (details suppressed)")?;
        if config.cluster_url.scheme_str() != Some("https")
            || config.cluster_url.authority().is_none()
            || config
                .cluster_url
                .authority()
                .is_some_and(|a| a.as_str().contains('@'))
            || config.cluster_url.query().is_some()
        {
            return Err("cluster URL must be HTTPS without userinfo or query credentials".into());
        }
        config.accept_invalid_certs = false;
        config.disable_compression = true;
        config.default_retry = false;
        config.proxy_url = None;
        config.connect_timeout = Some(Duration::from_secs(5));
        // Normal reads remain bounded by BodyBudgetLayer. Quiet log/TTY
        // sessions use their own explicit lifetime rather than an idle cutoff.
        config.read_timeout = None;
        config.write_timeout = Some(Duration::from_secs(10));
        let lifecycle_connection = if options.allow_helm_write {
            Some(crate::helm_lifecycle::StaticConnection::from_config(
                &mut config,
                auth_gate.clone(),
            )?)
        } else {
            None
        };
        let server = config.cluster_url.to_string();
        let namespace = if options.namespace.is_empty() {
            config.default_namespace.clone()
        } else {
            options.namespace
        };
        let builder = kube::client::ClientBuilder::try_from(config)
            .map_err(|_| "cannot initialize verified TLS client")?;
        let client = builder
            .with_layer(&crate::transport::BodyBudgetLayer::default())
            .with_layer(&auth_gate)
            .build();
        let serial = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock error")?
            .as_nanos();
        Ok(Self {
            client: Some(client),
            session: format!("{name}:{serial}"),
            context: name,
            server,
            namespace,
            policy: Policy {
                writable: options.writable,
            },
            logs: options.logs,
            allow_helm: options.allow_helm,
            allow_helm_write: options.allow_helm_write,
            allow_helm_hooks: options.allow_helm_hooks,
            allow_helm_crds: options.allow_helm_crds,
            lifecycle_connection,
            allow_node_maintenance: options.allow_node_maintenance,
            allow_yaml_edit: options.allow_yaml_edit,
            allow_force_kill: options.allow_force_kill,
            allow_secret_reveal: options.allow_secret_reveal,
            operation_capabilities: [
                options.allow_exec,
                options.allow_attach,
                options.allow_port_forward,
            ],
            demo: vec![],
            kinds: kinds(),
        })
    }
    pub fn is_demo(&self) -> bool {
        self.client.is_none()
    }
    pub(crate) fn helm_reader(&self, namespace: &str) -> Result<crate::helm::Reader> {
        if !self.allow_helm {
            return Err("Helm release storage access requires --allow-helm".into());
        }
        crate::helm::Reader::new(
            self.client
                .clone()
                .ok_or("offline Helm data uses the demo reader")?,
            namespace,
        )
    }
    pub(crate) fn helm_lifecycle_access(
        &self,
    ) -> Result<(Client, crate::helm_lifecycle::StaticConnection, String)> {
        self.helm_lifecycle_session()?;
        Ok((
            self.client
                .clone()
                .ok_or("Helm lifecycle needs a real selected cluster")?,
            self.lifecycle_connection
                .clone()
                .ok_or("Helm lifecycle has no static validated connection; reconnect explicitly")?,
            self.session.clone(),
        ))
    }
    pub(crate) fn helm_lifecycle_session(&self) -> Result<&str> {
        if !self.policy.writable || !self.allow_helm_write || !self.allow_helm {
            return Err("Helm lifecycle requires --write --allow-helm --allow-helm-write".into());
        }
        Ok(&self.session)
    }
    pub(crate) fn helm_extension_access(&self, hooks: bool, crds: bool) -> Result<()> {
        self.helm_lifecycle_session()?;
        if hooks && !self.allow_helm_hooks {
            return Err("Helm hooks require explicit --allow-helm-hooks".into());
        }
        if crds && !self.allow_helm_crds {
            return Err("Helm CRD actions require explicit --allow-helm-crds".into());
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn for_test_client(client: Client, namespace: &str) -> Self {
        Self {
            client: Some(client),
            context: "test".into(),
            server: "https://example.test".into(),
            namespace: namespace.into(),
            session: "test".into(),
            policy: Policy { writable: false },
            logs: false,
            allow_helm: false,
            allow_helm_write: false,
            allow_helm_hooks: false,
            allow_helm_crds: false,
            lifecycle_connection: None,
            operation_capabilities: [false; 3],
            allow_node_maintenance: false,
            allow_yaml_edit: false,
            allow_force_kill: false,
            allow_secret_reveal: false,
            demo: Vec::new(),
            kinds: kinds(),
        }
    }
    #[cfg(test)]
    pub(crate) fn for_helm_test_client(client: Client, namespace: &str) -> Self {
        let mut gateway = Self::for_test_client(client, namespace);
        gateway.allow_helm = true;
        gateway
    }
    #[cfg(test)]
    pub(crate) fn for_helm_lifecycle_test_client(
        client: Client,
        namespace: &str,
        connection: crate::helm_lifecycle::StaticConnection,
    ) -> Self {
        let mut gateway = Self::for_helm_test_client(client, namespace);
        gateway.policy.writable = true;
        gateway.allow_helm_write = true;
        gateway.lifecycle_connection = Some(connection);
        gateway
    }
    #[cfg(test)]
    pub(crate) fn for_helm_extension_test_client(
        client: Client,
        namespace: &str,
        connection: crate::helm_lifecycle::StaticConnection,
        hooks: bool,
        crds: bool,
    ) -> Self {
        let mut gateway = Self::for_helm_lifecycle_test_client(client, namespace, connection);
        gateway.allow_helm_hooks = hooks;
        gateway.allow_helm_crds = crds;
        gateway
    }
    pub fn watch(&self, key: &str, ns: &str) -> Result<crate::watch::WatchFeed> {
        self.watch_selector(key, ns, "")
    }
    pub fn watch_selector(
        &self,
        key: &str,
        ns: &str,
        selector: &str,
    ) -> Result<crate::watch::WatchFeed> {
        self.watch_fields(key, ns, selector, "")
    }
    pub fn watch_fields(
        &self,
        key: &str,
        ns: &str,
        selector: &str,
        fields: &str,
    ) -> Result<crate::watch::WatchFeed> {
        crate::selectors::validate(selector)?;
        let scope = crate::selectors::field_scope(fields)?;
        let kind = self.find(key)?.clone();
        if let Some(scope) = scope
            && (kind.kind != scope.kind() || !kind.group.is_empty())
        {
            return Err(format!(
                "this field selector requires core {}s",
                scope.kind()
            ));
        }
        Ok(crate::watch::WatchFeed::with_filters(
            self.api(&kind, ns)?,
            kind,
            selector.into(),
            fields.into(),
        ))
    }
    pub(crate) fn metrics_api(
        &self,
        scope: crate::metrics::MetricsScope,
        ns: &str,
    ) -> Result<Api<DynamicObject>> {
        let kind = Kind {
            plural: if matches!(scope, crate::metrics::MetricsScope::Pods) {
                "pods"
            } else {
                "nodes"
            }
            .into(),
            kind: if matches!(scope, crate::metrics::MetricsScope::Pods) {
                "PodMetrics"
            } else {
                "NodeMetrics"
            }
            .into(),
            group: "metrics.k8s.io".into(),
            version: "v1beta1".into(),
            namespaced: matches!(scope, crate::metrics::MetricsScope::Pods),
            short_names: Vec::new(),
        };
        self.api(&kind, ns)
    }
    pub(crate) fn find(&self, key: &str) -> Result<&Kind> {
        if let Some(kind) = self.kinds.iter().find(|k| {
            k.key() == key
                || (if k.group.is_empty() {
                    format!("{}/{}", k.version, k.plural)
                } else {
                    format!("{}/{}/{}", k.group, k.version, k.plural)
                }) == key
        }) {
            return Ok(kind);
        }
        let mut matches = self.kinds.iter().filter(|k| {
            k.plural == key
                || k.kind.eq_ignore_ascii_case(key)
                || k.short_names.iter().any(|name| name == key)
        });
        let first = matches
            .next()
            .ok_or("resource kind not available; Secrets are never exposed")?;
        if matches.next().is_some() {
            return Err("resource alias is ambiguous; use resource.group".into());
        }
        Ok(first)
    }
    fn api(&self, k: &Kind, ns: &str) -> Result<Api<DynamicObject>> {
        if ns != "*" && !safety::valid_id(ns) {
            return Err("invalid namespace".into());
        }
        let client = self.client.clone().ok_or("offline backend")?;
        if k.plural == "secrets" || k.kind == "Secret" {
            return Err("Secret access is disabled".into());
        }
        Ok(if k.namespaced && ns != "*" {
            Api::namespaced_with(client, ns, &k.ar())
        } else {
            Api::all_with(client, &k.ar())
        })
    }
    fn secret_api(&self, ns: &str) -> Result<Option<Api<k8s_openapi::api::core::v1::Secret>>> {
        if !self.allow_secret_reveal {
            return Err("Secret inspection requires --allow-secret-reveal".into());
        }
        if ns == "*" || !safety::valid_id(ns) {
            return Err("Secret inspection requires one explicit namespace".into());
        }
        if self.is_demo() {
            return Ok(None);
        }
        let client = self.client.clone().ok_or("offline backend")?;
        Ok(Some(Api::namespaced(client, ns)))
    }
    /// Secret names, types and key sizes for one namespace. Values are dropped
    /// before this returns.
    pub async fn secret_index(&self, ns: &str) -> Result<crate::secrets::SecretIndex> {
        let Some(api) = self.secret_api(ns)? else {
            let data = crate::secrets::demo_data();
            return Ok(crate::secrets::SecretIndex {
                namespace: ns.into(),
                items: vec![crate::secrets::entry(
                    "api-credentials",
                    "demo-secret-uid",
                    Some("Opaque"),
                    Some(&data),
                )],
                truncated: false,
            });
        };
        let params = ListParams::default().limit(crate::secrets::MAX_SECRETS as u32);
        let list = tokio::time::timeout(OPERATION_BUDGET, api.list(&params))
            .await
            .map_err(|_| "Secret list timed out")?
            .map_err(api_error)?;
        let truncated = list
            .metadata
            .continue_
            .as_deref()
            .is_some_and(|c| !c.is_empty());
        let items = list
            .items
            .iter()
            .map(|secret| {
                let data = secret.data.as_ref().map(|data| {
                    data.iter()
                        .map(|(key, value)| (key.clone(), value.0.clone()))
                        .collect()
                });
                crate::secrets::entry(
                    secret.metadata.name.as_deref().unwrap_or(""),
                    secret.metadata.uid.as_deref().unwrap_or(""),
                    secret.type_.as_deref(),
                    data.as_ref(),
                )
            })
            .collect();
        Ok(crate::secrets::SecretIndex {
            namespace: ns.into(),
            items,
            truncated,
        })
    }
    /// Fetches one key of one Secret, refusing a replacement with another UID.
    pub async fn secret_value(
        &self,
        ns: &str,
        name: &str,
        uid: &str,
        key: &str,
    ) -> Result<crate::secrets::Revealed> {
        crate::secrets::validate_request(ns, name, key)?;
        let Some(api) = self.secret_api(ns)? else {
            if name != "api-credentials" || uid != "demo-secret-uid" {
                return Err("Secret not found".into());
            }
            return crate::secrets::demo_data()
                .get(key)
                .map(|bytes| crate::secrets::reveal(bytes))
                .ok_or_else(|| "Secret key not found".into());
        };
        let secret = tokio::time::timeout(OPERATION_BUDGET, api.get(name))
            .await
            .map_err(|_| "Secret read timed out")?
            .map_err(api_error)?;
        if secret.metadata.uid.as_deref() != Some(uid) {
            return Err("Secret was replaced; reopen :secrets".into());
        }
        secret
            .data
            .as_ref()
            .and_then(|data| data.get(key))
            .map(|bytes| crate::secrets::reveal(&bytes.0))
            .ok_or_else(|| "Secret key not found".into())
    }
    pub async fn discover(&mut self) -> Result<Vec<Kind>> {
        if let Some(client) = &self.client {
            self.kinds = discovery::collect(client).await?;
        }
        Ok(self.kinds.clone())
    }
    pub async fn list(&self, key: &str, ns: &str) -> Result<Value> {
        self.list_selector(key, ns, "").await
    }
    pub async fn list_selector(&self, key: &str, ns: &str, selector: &str) -> Result<Value> {
        self.list_fields(key, ns, selector, "").await
    }
    pub async fn list_fields(
        &self,
        key: &str,
        ns: &str,
        selector: &str,
        fields: &str,
    ) -> Result<Value> {
        crate::selectors::validate(selector)?;
        let scope = crate::selectors::field_scope(fields)?;
        let kind = self.find(key)?;
        if let Some(scope) = scope
            && (kind.kind != scope.kind() || !kind.group.is_empty())
        {
            return Err(format!(
                "this field selector requires core {}s",
                scope.kind()
            ));
        }
        if self.is_demo() {
            let mut items: Vec<Value> = self
                .demo
                .iter()
                .filter(|v| {
                    v["kind"] == kind.kind
                        && (ns == "*" || v["metadata"]["namespace"] == ns || !kind.namespaced)
                        && crate::selectors::matches(selector, &v["metadata"]["labels"])
                        && scope.is_none_or(|scope| scope.matches(v))
                })
                .cloned()
                .collect();
            items.iter_mut().for_each(safety::redact);
            return Ok(json!({"items":items,"truncated":false,"source":"demo"}));
        }
        let api = self.api(kind, ns)?;
        let mut items = Vec::new();
        let mut continuation = None;
        let mut total_bytes = 0;
        let deadline = tokio::time::Instant::now() + OPERATION_BUDGET;
        let mut pages = 0;
        let mut seen_tokens = std::collections::HashSet::new();
        loop {
            let params = ListParams {
                limit: Some(200),
                label_selector: (!selector.is_empty()).then(|| selector.to_owned()),
                field_selector: (!fields.is_empty()).then(|| fields.to_owned()),
                continue_token: continuation,
                ..Default::default()
            };
            let list = tokio::time::timeout_at(deadline, api.list(&params))
                .await
                .map_err(|_| "list budget exceeded; narrow namespace scope")?
                .map_err(api_error)?;
            pages += 1;
            for item in list.items {
                let mut value = serde_json::to_value(item).map_err(|_| "cannot decode object")?;
                value["kind"] = json!(kind.kind);
                value["apiVersion"] = json!(kind.ar().api_version);
                safety::redact(&mut value);
                total_bytes += value.to_string().len();
                if items.len() >= MAX_OBJECTS || total_bytes > MAX_BODY {
                    return Ok(json!({"items":items,"truncated":true,"source":"cluster"}));
                }
                items.push(value);
            }
            continuation = list.metadata.continue_.filter(|s| !s.is_empty());
            if continuation.is_none() {
                break;
            }
            if pages >= MAX_LIST_PAGES {
                return Ok(json!({"items":items,"truncated":true,"source":"cluster"}));
            }
            if let Some(token) = &continuation
                && !seen_tokens.insert(token.clone())
            {
                return Err("server repeated a pagination token; listing stopped".into());
            }
            tokio::time::timeout_at(deadline, tokio::time::sleep(Duration::from_millis(200)))
                .await
                .map_err(|_| "list budget exceeded; narrow namespace scope")?;
        }
        Ok(json!({"items":items,"truncated":false,"source":"cluster"}))
    }
    async fn raw_get(&self, key: &str, ns: &str, name: &str) -> Result<Value> {
        if name.is_empty()
            || name.len() > 253
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        {
            return Err("invalid object name".into());
        }
        let kind = self.find(key)?;
        if self.is_demo() {
            if kind.namespaced && ns == "*" {
                return Err("select a concrete namespace".into());
            }
            return self
                .demo
                .iter()
                .find(|v| {
                    v["kind"] == kind.kind
                        && v["metadata"]["name"] == name
                        && (!kind.namespaced || v["metadata"]["namespace"] == ns)
                })
                .cloned()
                .ok_or_else(|| "demo object not found".into());
        }
        if kind.namespaced && ns == "*" {
            return Err("select a concrete namespace".into());
        }
        let mut value =
            serde_json::to_value(self.api(kind, ns)?.get(name).await.map_err(api_error)?)
                .map_err(|_| "cannot decode object")?;
        value["kind"] = json!(kind.kind);
        value["apiVersion"] = json!(kind.ar().api_version);
        Ok(value)
    }
    pub async fn get(&self, key: &str, ns: &str, name: &str) -> Result<Value> {
        let mut value = self.raw_get(key, ns, name).await?;
        safety::redact(&mut value);
        Ok(value)
    }
    /// Container names from the pod spec, including init and ephemeral containers.
    pub async fn containers(&self, ns: &str, name: &str) -> Result<Vec<String>> {
        let pod = self.get("pods", ns, name).await?;
        let mut names = Vec::new();
        for field in ["containers", "initContainers", "ephemeralContainers"] {
            if let Some(containers) = pod["spec"][field].as_array() {
                for container in containers {
                    if let Some(name) = container["name"].as_str().filter(|n| safety::valid_id(n)) {
                        if !names.iter().any(|n| n == name) {
                            names.push(name.to_owned());
                        }
                        if names.len() > 1000 {
                            return Err("container listing exceeded limit".into());
                        }
                    }
                }
            }
        }
        if self.is_demo() && names.is_empty() {
            names.push("app".into());
        }
        Ok(names)
    }
    /// Explicit opt-in is checked here immediately before opening the log request.
    pub async fn follow_logs(
        &self,
        ns: &str,
        name: &str,
        container: Option<String>,
        previous: bool,
    ) -> Result<crate::logs::LogFeed> {
        self.follow_logs_with_options(
            ns,
            name,
            container,
            previous,
            crate::logs::FollowOptions::default(),
        )
        .await
    }
    pub async fn follow_logs_with_options(
        &self,
        ns: &str,
        name: &str,
        container: Option<String>,
        previous: bool,
        options: crate::logs::FollowOptions,
    ) -> Result<crate::logs::LogFeed> {
        self.follow_logs_with_read_options(
            ns,
            name,
            container,
            previous,
            options,
            crate::logs::ReadOptions::default(),
        )
        .await
    }
    pub async fn follow_logs_with_read_options(
        &self,
        ns: &str,
        name: &str,
        container: Option<String>,
        previous: bool,
        options: crate::logs::FollowOptions,
        read_options: crate::logs::ReadOptions,
    ) -> Result<crate::logs::LogFeed> {
        let options = options.validate()?;
        let read_options = read_options.validate()?;
        if !self.logs {
            return Err("logs may contain sensitive data; opt in with --allow-logs".into());
        }
        if !safety::valid_id(ns)
            || name.is_empty()
            || name.len() > 253
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
            || container.as_deref().is_some_and(|c| !safety::valid_id(c))
        {
            return Err("invalid log target or container".into());
        }
        if previous {
            return Err("previous container logs support snapshots only; omit --follow".into());
        }
        if self.is_demo() {
            let text = self
                .logs_with_options(ns, name, container, false, read_options)
                .await?;
            return crate::logs::LogFeed::with_options(
                futures_util::io::Cursor::new(text.into_bytes()),
                options,
            );
        }
        let pods: Api<Pod> = Api::namespaced(self.client.clone().ok_or("no client")?, ns);
        let stream = tokio::time::timeout(
            OPERATION_BUDGET,
            pods.log_stream(name, &read_options.params(container, false, true)?),
        )
        .await
        .map_err(|_| "log connection timeout")?
        .map_err(api_error)?;
        crate::logs::LogFeed::with_read_options(stream, options, read_options)
    }
    /// Explicitly bind reconnection to a pod selected by UID. Extra pod metadata
    /// reads occur only in this workflow; ordinary log-only RBAC remains usable.
    pub async fn follow_logs_resumable(
        &self,
        ns: &str,
        name: &str,
        container: Option<String>,
        uid: &str,
        options: crate::logs::FollowOptions,
        read_options: crate::logs::ReadOptions,
    ) -> Result<crate::logs::LogFeed> {
        let feed = self
            .follow_logs_with_read_options(
                ns,
                name,
                container.clone(),
                false,
                options,
                read_options,
            )
            .await?;
        if self.is_demo() || read_options.head {
            return Ok(feed);
        }
        let api = Api::namespaced(self.client.clone().ok_or("no client")?, ns);
        // Validate after opening, before any body data is delivered to callers.
        crate::logs::verify_uid(&api, name, uid).await?;
        feed.with_resume(api, name, uid, container, read_options)
    }
    pub async fn logs(
        &self,
        ns: &str,
        name: &str,
        container: Option<String>,
        previous: bool,
    ) -> Result<String> {
        self.logs_with_options(
            ns,
            name,
            container,
            previous,
            crate::logs::ReadOptions::default(),
        )
        .await
    }
    pub async fn logs_with_options(
        &self,
        ns: &str,
        name: &str,
        container: Option<String>,
        previous: bool,
        options: crate::logs::ReadOptions,
    ) -> Result<String> {
        let options = options.validate()?;
        if !safety::valid_id(ns)
            || name.is_empty()
            || name.len() > 253
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        {
            return Err("invalid log target".into());
        }
        if container.as_deref().is_some_and(|c| !safety::valid_id(c)) {
            return Err("invalid log container".into());
        }
        if !self.logs {
            return Err("logs may contain sensitive data; opt in with --allow-logs".into());
        }
        if self.is_demo() {
            if !self.demo.iter().any(|v| {
                v["kind"] == "Pod"
                    && v["metadata"]["namespace"] == ns
                    && v["metadata"]["name"] == name
            }) {
                return Err("demo pod not found in selected namespace".into());
            }
            return Ok(crate::logs::snapshot_text(
                "2026-10-09T20:00:00Z INFO KBorg demo started\n2026-10-09T20:00:01Z WARN connection retry\n2026-10-09T20:00:02Z INFO request served",
                options,
            ));
        }
        let pods: Api<Pod> = Api::namespaced(self.client.clone().ok_or("no client")?, ns);
        let output = pods
            .logs(name, &options.params(container, previous, false)?)
            .await
            .map_err(api_error)?;
        Ok(crate::logs::snapshot_text(&output, options))
    }
    /// Explicit Pod-wide read: ordinary containers only, one captured Pod UID,
    /// bounded source count and a shared session/display budget.
    pub async fn merged_logs(
        &self,
        ns: &str,
        name: &str,
        uid: &str,
        previous: bool,
        read_options: crate::logs::ReadOptions,
    ) -> Result<String> {
        let read_options = read_options.validate()?;
        if !self.logs {
            return Err("logs may contain sensitive data; opt in with --allow-logs".into());
        }
        crate::logs::validate_merge_target(ns, name, uid)?;
        let feed = if self.is_demo() {
            self.demo_merged_logs(
                (ns, name, uid),
                previous,
                crate::logs::FollowOptions { seconds: 15 },
                read_options,
            )
            .await?
        } else {
            let api = Api::namespaced(self.client.clone().ok_or("no client")?, ns);
            crate::logs::MergedLogFeed::open(api, name, uid, previous, None, read_options).await?
        };
        feed.snapshot(read_options).await
    }
    pub async fn follow_merged_logs(
        &self,
        ns: &str,
        name: &str,
        uid: &str,
        options: crate::logs::FollowOptions,
        read_options: crate::logs::ReadOptions,
    ) -> Result<crate::logs::MergedLogFeed> {
        let options = options.validate()?;
        let read_options = read_options.validate()?;
        if !self.logs {
            return Err("logs may contain sensitive data; opt in with --allow-logs".into());
        }
        crate::logs::validate_merge_target(ns, name, uid)?;
        if self.is_demo() {
            return self
                .demo_merged_logs((ns, name, uid), false, options, read_options)
                .await;
        }
        let api = Api::namespaced(self.client.clone().ok_or("no client")?, ns);
        crate::logs::MergedLogFeed::open(api, name, uid, false, Some(options), read_options).await
    }
    async fn demo_merged_logs(
        &self,
        target: (&str, &str, &str),
        previous: bool,
        options: crate::logs::FollowOptions,
        read_options: crate::logs::ReadOptions,
    ) -> Result<crate::logs::MergedLogFeed> {
        let (ns, name, uid) = target;
        let pod: Pod = serde_json::from_value(self.raw_get("pods", ns, name).await?)
            .map_err(|_| "invalid demo Pod for merged logs")?;
        let names = crate::logs::ordinary_container_names(&pod, ns, name, uid)?;
        let mut sources = Vec::with_capacity(names.len());
        for container in names {
            let text = self
                .logs_with_options(ns, name, Some(container.clone()), previous, read_options)
                .await?;
            let feed = crate::logs::LogFeed::with_read_options(
                futures_util::io::Cursor::new(text.into_bytes()),
                options,
                read_options,
            );
            sources.push((container, feed));
        }
        crate::logs::MergedLogFeed::from_sources(ns, name, uid, sources, options)
    }
    /// Capture one complete, connection-bound Pod membership snapshot. A reload
    /// reuses these sources; discovering new Pods is an explicit new capture.
    pub async fn capture_log_membership(
        &self,
        namespace: &str,
        selection: crate::logs::PodLogSelection,
    ) -> Result<crate::logs::LogMembership> {
        use crate::logs::{LogMembership, LogParent, PodLogSelection};
        if !self.logs {
            return Err("logs may contain sensitive data; opt in with --allow-logs".into());
        }
        if !safety::valid_id(namespace) {
            return Err("workload logs require one concrete namespace".into());
        }
        let deadline = tokio::time::Instant::now() + OPERATION_BUDGET;
        let mut bytes = 0;
        let mut parents = Vec::new();
        let (description, selectors) = match selection {
            PodLogSelection::Selector(selector) => {
                crate::selectors::validate(&selector)?;
                if selector.trim().is_empty() {
                    return Err("workload logs require an explicit nonempty Pod selector".into());
                }
                (
                    format!("selector {selector}; fixed membership"),
                    vec![(selector, None)],
                )
            }
            PodLogSelection::Workload {
                resource,
                name,
                uid,
            } => {
                crate::logs::validate_merge_target(namespace, &name, &uid)?;
                let kind = self.find(&resource)?.clone();
                if !matches!(
                    (
                        kind.group.as_str(),
                        kind.version.as_str(),
                        kind.kind.as_str()
                    ),
                    (
                        "apps",
                        "v1",
                        "Deployment" | "StatefulSet" | "DaemonSet" | "ReplicaSet"
                    ) | ("batch", "v1", "Job" | "CronJob")
                        | ("", "v1", "Service" | "ReplicationController")
                ) {
                    return Err("workload logs support native Deployments, StatefulSets, DaemonSets, ReplicaSets, Jobs, CronJobs, Services and ReplicationControllers".into());
                }
                let object =
                    tokio::time::timeout_at(deadline, self.raw_get(&kind.key(), namespace, &name))
                        .await
                        .map_err(|_| "workload log membership lookup timed out")??;
                crate::logs::membership_bytes(&mut bytes, &object)?;
                if object["metadata"]["namespace"] != namespace
                    || object["metadata"]["name"] != name
                    || object["metadata"]["uid"] != uid
                {
                    return Err(
                        "selected workload identity changed or escaped namespace; select it again"
                            .into(),
                    );
                }
                parents.push(LogParent {
                    resource: kind.key(),
                    name: name.clone(),
                    uid: uid.clone(),
                });
                let selectors = if kind.kind == "CronJob" {
                    let jobs = if self.is_demo() {
                        self.demo
                            .iter()
                            .filter(|job| {
                                job["apiVersion"] == "batch/v1"
                                    && job["kind"] == "Job"
                                    && job["metadata"]["namespace"] == namespace
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                    } else {
                        let api: Api<k8s_openapi::api::batch::v1::Job> =
                            Api::namespaced(self.client.clone().ok_or("no client")?, namespace);
                        let list = tokio::time::timeout_at(
                            deadline,
                            api.list(&ListParams {
                                limit: Some((crate::logs::MAX_LOG_JOBS + 1) as u32),
                                ..Default::default()
                            }),
                        )
                        .await
                        .map_err(|_| "CronJob child inventory timed out")?
                        .map_err(api_error)?;
                        if list
                            .metadata
                            .resource_version
                            .as_deref()
                            .is_none_or(|version| {
                                version.is_empty()
                                    || version.len() > 253
                                    || safety::safe_text(version, 253) != version
                            })
                        {
                            return Err("CronJob child inventory omitted a valid resourceVersion; complete membership cannot be established".into());
                        }
                        if list
                            .metadata
                            .continue_
                            .as_ref()
                            .is_some_and(|token| !token.is_empty())
                            || list.items.len() > crate::logs::MAX_LOG_JOBS
                        {
                            return Err("CronJob logs require a complete child Job inventory of at most 64 Jobs; use an explicit Pod selector".into());
                        }
                        list.items
                            .into_iter()
                            .map(|job| {
                                serde_json::to_value(job)
                                    .map_err(|_| "invalid CronJob child Job".to_owned())
                            })
                            .collect::<Result<Vec<_>>>()?
                    };
                    let mut selectors = Vec::new();
                    for mut job in jobs {
                        crate::logs::membership_bytes(&mut bytes, &job)?;
                        if job["metadata"]["namespace"] != namespace {
                            return Err("CronJob child inventory escaped namespace".into());
                        }
                        if !crate::logs::owned_by(&job, "batch/v1", "CronJob", &uid) {
                            continue;
                        }
                        job["apiVersion"] = json!("batch/v1");
                        job["kind"] = json!("Job");
                        let name = job["metadata"]["name"]
                            .as_str()
                            .ok_or("child Job name missing")?
                            .to_owned();
                        let uid = job["metadata"]["uid"]
                            .as_str()
                            .ok_or("child Job UID missing")?
                            .to_owned();
                        crate::logs::validate_merge_target(namespace, &name, &uid)?;
                        selectors.push((
                            crate::insights::pod_selector(&job)?,
                            Some(LogParent {
                                resource: "jobs.batch".into(),
                                name,
                                uid,
                            }),
                        ));
                    }
                    selectors
                } else {
                    vec![(crate::insights::pod_selector(&object)?, None)]
                };
                (
                    format!("{}/{name}; UID {uid}; fixed membership", kind.key()),
                    selectors,
                )
            }
        };
        let mut members = Vec::new();
        for (selector, owner) in selectors {
            let selected = if self.is_demo() {
                let mut selected = Vec::new();
                for value in self.demo.iter().filter(|pod| {
                    pod["apiVersion"] == "v1"
                        && pod["kind"] == "Pod"
                        && pod["metadata"]["namespace"] == namespace
                        && crate::selectors::matches(&selector, &pod["metadata"]["labels"])
                }) {
                    crate::logs::membership_bytes(&mut bytes, value)?;
                    if owner.as_ref().is_some_and(|owner| {
                        !crate::logs::owned_by(value, "batch/v1", "Job", &owner.uid)
                    }) {
                        continue;
                    }
                    let pod: Pod = serde_json::from_value(value.clone())
                        .map_err(|_| "invalid demo Pod membership")?;
                    selected.push(crate::logs::log_pod(&pod, namespace)?);
                }
                selected
            } else {
                let api = Api::namespaced(self.client.clone().ok_or("no client")?, namespace);
                crate::logs::selected_pods(
                    &api,
                    &selector,
                    owner.as_ref().map(|owner| owner.uid.as_str()),
                    deadline,
                    &mut bytes,
                )
                .await?
            };
            if !selected.is_empty()
                && let Some(owner) = owner
            {
                parents.push(owner);
            }
            members.extend(selected);
            if members.len() > crate::logs::MAX_LOG_PODS {
                return Err("workload log membership exceeds 8 Pods; narrow the selector".into());
            }
        }
        LogMembership::new(namespace, description, members, parents, &self.session)
    }
    fn log_membership_guards(
        &self,
        membership: &crate::logs::LogMembership,
    ) -> Result<Vec<crate::logs::IdentityGuard>> {
        membership.verify_connection(&self.session)?;
        let mut guards = Vec::new();
        for parent in &membership.parents {
            guards.push(crate::logs::IdentityGuard::Parent {
                api: self.api(self.find(&parent.resource)?, membership.namespace())?,
                name: parent.name.clone(),
                uid: parent.uid.clone(),
            });
        }
        Ok(guards)
    }
    pub async fn membership_logs(
        &self,
        membership: &crate::logs::LogMembership,
        previous: bool,
        read: crate::logs::ReadOptions,
    ) -> Result<String> {
        let feed = self
            .open_membership_logs(membership, previous, None, read)
            .await?;
        feed.snapshot(read).await
    }
    pub async fn follow_membership_logs(
        &self,
        membership: &crate::logs::LogMembership,
        follow: crate::logs::FollowOptions,
        read: crate::logs::ReadOptions,
    ) -> Result<crate::logs::MergedLogFeed> {
        self.open_membership_logs(membership, false, Some(follow), read)
            .await
    }
    /// Explicit opt-in: announced joins/departures on top of the same bounds.
    pub async fn follow_dynamic_logs(
        &self,
        membership: &crate::logs::LogMembership,
        follow: crate::logs::FollowOptions,
        read: crate::logs::ReadOptions,
    ) -> Result<crate::logs::DynamicLogFeed> {
        if !membership.is_dynamic() {
            return Err("dynamic workload logs require an explicit dynamic capture".into());
        }
        let read = read.validate()?;
        let follow = follow.validate()?;
        if read.head {
            return Err(
                "dynamic membership follows new records; choose a tail or time range, not head"
                    .into(),
            );
        }
        let feed = self
            .open_membership_logs(membership, false, Some(follow), read)
            .await?;
        let api = if self.is_demo() {
            None
        } else {
            Some(Api::namespaced(
                self.client.clone().ok_or("no client")?,
                membership.namespace(),
            ))
        };
        crate::logs::DynamicLogFeed::new(self.clone(), api, membership.clone(), feed, read, follow)
    }
    async fn open_membership_logs(
        &self,
        membership: &crate::logs::LogMembership,
        previous: bool,
        follow: Option<crate::logs::FollowOptions>,
        read: crate::logs::ReadOptions,
    ) -> Result<crate::logs::MergedLogFeed> {
        if !self.logs {
            return Err("logs may contain sensitive data; opt in with --allow-logs".into());
        }
        membership.verify_connection(&self.session)?;
        if self.is_demo() {
            let read = read.validate()?;
            let options = follow
                .unwrap_or(crate::logs::FollowOptions { seconds: 15 })
                .validate()?;
            if previous && follow.is_some() {
                return Err("previous container logs support snapshots only; omit follow".into());
            }
            let mut sources = Vec::new();
            for member in membership.pods() {
                let pod: Pod = serde_json::from_value(
                    self.raw_get("v1/pods", membership.namespace(), &member.name)
                        .await?,
                )
                .map_err(|_| "invalid demo Pod membership")?;
                let current = crate::logs::log_pod(&pod, membership.namespace())?;
                if current.uid != member.uid || current.containers != member.containers {
                    return Err(
                        "captured demo Pod membership changed; capture sources again".into(),
                    );
                }
                for container in &member.containers {
                    let text = self
                        .logs_with_options(
                            membership.namespace(),
                            &member.name,
                            Some(container.clone()),
                            previous,
                            read,
                        )
                        .await?;
                    sources.push((
                        member.name.clone(),
                        container.clone(),
                        crate::logs::LogFeed::with_read_options(
                            futures_util::io::Cursor::new(text.into_bytes()),
                            options,
                            read,
                        ),
                    ));
                }
            }
            return crate::logs::MergedLogFeed::from_membership_sources(
                membership,
                sources,
                tokio::time::Instant::now() + Duration::from_secs(u64::from(options.seconds)),
                Vec::new(),
            );
        }
        let api = Api::namespaced(
            self.client.clone().ok_or("no client")?,
            membership.namespace(),
        );
        crate::logs::MergedLogFeed::open_membership(
            api,
            membership,
            self.log_membership_guards(membership)?,
            previous,
            follow,
            read,
        )
        .await
    }
    pub(crate) fn operation_api(&self, namespace: &str, capability: usize) -> Result<Api<Pod>> {
        if !self.policy.writable {
            return Err("READ ONLY: operations require --write".into());
        }
        if !self
            .operation_capabilities
            .get(capability)
            .copied()
            .unwrap_or(false)
        {
            return Err("operation requires its explicit capability flag".into());
        }
        if !safety::valid_id(namespace)
            || ["kube-system", "kube-public", "kube-node-lease"].contains(&namespace)
        {
            return Err("operation namespace is prohibited".into());
        }
        let client = self
            .client
            .clone()
            .ok_or("remote operations are unavailable in demo mode")?;
        Ok(Api::namespaced(client, namespace))
    }
    pub(crate) fn node_maintenance_access(&self) -> Result<Client> {
        if !self.policy.writable || !self.allow_node_maintenance {
            return Err("node maintenance requires --write --allow-node-maintenance".into());
        }
        self.client
            .clone()
            .ok_or_else(|| "node maintenance requires a real selected cluster".into())
    }
    #[cfg(test)]
    pub(crate) fn for_node_maintenance_test_client(client: Client) -> Self {
        let mut gateway = Self::for_test_client(client, "default");
        gateway.policy.writable = true;
        gateway.allow_node_maintenance = true;
        gateway
    }
    pub(crate) fn operation_session(&self) -> &str {
        &self.session
    }
    /// A safe editing projection backed by an unredacted, private baseline.
    pub async fn yaml_draft(
        &self,
        key: &str,
        ns: &str,
        name: &str,
    ) -> Result<crate::manifest_edit::Draft> {
        let kind = self.find(key)?.clone();
        if !kind.namespaced || kind.kind.eq_ignore_ascii_case("Secret") {
            return Err("YAML editing supports namespaced non-Secret spec resources".into());
        }
        let original = self.raw_get(key, ns, name).await?;
        crate::manifest_edit::Draft::new(original, kind, self.session.clone())
    }
    fn yaml_edit_access(&self) -> Result<()> {
        if !self.policy.writable || !self.allow_yaml_edit {
            return Err("YAML edits require --write --allow-yaml-edit".into());
        }
        Ok(())
    }
    pub async fn prepare_yaml(
        &self,
        draft: &crate::manifest_edit::Draft,
        edited: &str,
    ) -> Result<crate::manifest_edit::PreparedYaml> {
        self.yaml_edit_access()?;
        if draft.session != self.session {
            return Err("session changed; reopen the YAML editor".into());
        }
        let current = self
            .raw_get(&draft.resource(), draft.namespace(), draft.name())
            .await?;
        draft.unchanged(&current)?;
        let (patch, expected_spec, diff) = draft.changes(edited)?;
        let summary = format!(
            "REVIEW YAML SPEC EDIT\nContext: {}\nServer: {}\nResource: {} {}/{}\nUID: {}\nResourceVersion: {}\n\n{}\n\nOnly the listed spec paths change. Hidden values, metadata and status are preserved.\nSpec edits may start workloads or change networking; server RBAC and admission still apply.\n{}\nType {}/{} to apply. Review expires after 60 seconds.",
            self.context,
            self.server,
            draft.resource(),
            draft.namespace(),
            draft.name(),
            draft.original["metadata"]["uid"].as_str().unwrap_or(""),
            draft.original["metadata"]["resourceVersion"]
                .as_str()
                .unwrap_or(""),
            diff,
            if self.is_demo() {
                "Demo validation only; no cluster contacted."
            } else {
                "Server dry-run passed. Admission may behave differently on the real write."
            },
            draft.namespace(),
            draft.name()
        );
        let prepared =
            crate::manifest_edit::PreparedYaml::new(draft.clone(), patch, expected_spec, summary)?;
        if !self.is_demo() {
            let response =
                tokio::time::timeout(OPERATION_BUDGET, self.perform_yaml(&prepared, true))
                    .await
                    .map_err(|_| "YAML dry-run timed out")??;
            crate::manifest_edit::check_admission(
                &response,
                &prepared.expected_spec,
                &draft.original,
            )?;
        }
        Ok(prepared)
    }
    async fn perform_yaml(
        &self,
        prepared: &crate::manifest_edit::PreparedYaml,
        dry_run: bool,
    ) -> Result<Value> {
        let draft = &prepared.draft;
        let api = self.api(&draft.kind, draft.namespace())?;
        let url = format!(
            "{}/{}?fieldValidation=Strict{}",
            api.resource_url(),
            draft.name(),
            if dry_run { "&dryRun=All" } else { "" }
        );
        let request = http::Request::builder()
            .method("PATCH")
            .uri(url)
            .header("Content-Type", "application/json-patch+json")
            .body(
                serde_json::to_vec(&prepared.patch)
                    .map_err(|_| "cannot encode reviewed YAML patch")?,
            )
            .map_err(|_| "cannot construct YAML patch request")?;
        self.client
            .as_ref()
            .ok_or("offline backend")?
            .request::<Value>(request)
            .await
            .map_err(api_error)
    }
    pub async fn execute_yaml(
        &mut self,
        prepared: crate::manifest_edit::PreparedYaml,
        typed: &str,
    ) -> Result<String> {
        self.yaml_edit_access()?;
        if prepared.draft.session != self.session
            || prepared.created.elapsed() > Duration::from_secs(60)
        {
            return Err("YAML review expired or session changed; prepare and review again".into());
        }
        if typed != prepared.confirmation() {
            return Err("confirmation must exactly match namespace/name".into());
        }
        let draft = &prepared.draft;
        let current = self
            .raw_get(&draft.resource(), draft.namespace(), draft.name())
            .await?;
        draft.unchanged(&current)?;
        if self.is_demo() {
            for object in &mut self.demo {
                if object["metadata"]["uid"] == draft.original["metadata"]["uid"] {
                    object["spec"] = prepared.expected_spec.clone();
                    object["metadata"]["resourceVersion"] = json!(format!(
                        "{}y",
                        draft.original["metadata"]["resourceVersion"]
                            .as_str()
                            .unwrap_or("")
                    ));
                }
            }
            return Ok("Demo YAML edit applied; no cluster contacted".into());
        }
        let response = tokio::time::timeout(OPERATION_BUDGET, self.perform_yaml(&prepared, true))
            .await
            .map_err(|_| "YAML revalidation timed out; no write sent")??;
        crate::manifest_edit::check_admission(&response, &prepared.expected_spec, &draft.original)?;
        if prepared.created.elapsed() > Duration::from_secs(60) {
            return Err("YAML review expired during revalidation; review again".into());
        }
        let target = Target {
            session: self.session.clone(),
            resource: draft.resource(),
            namespace: draft.namespace().into(),
            name: draft.name().into(),
            uid: draft.original["metadata"]["uid"]
                .as_str()
                .unwrap_or("")
                .into(),
            version: draft.original["metadata"]["resourceVersion"]
                .as_str()
                .unwrap_or("")
                .into(),
        };
        let audit = start_operation_audit(&target, "yaml-edit", &self.context)?;
        let result =
            match tokio::time::timeout(OPERATION_BUDGET, self.perform_yaml(&prepared, false)).await
            {
                Ok(result) => result,
                Err(_) => Err(
                    "YAML outcome UNKNOWN after timeout; inspect server state before retrying"
                        .into(),
                ),
            };
        let result = result.and_then(|response| {
            crate::manifest_edit::check_applied(&response, &prepared.expected_spec, &draft.original)
        });
        finish_audit(
            &audit,
            if result.is_ok() {
                "succeeded"
            } else {
                "failed-or-unknown"
            },
        )?;
        result?;
        Ok("YAML edit accepted by API server; inspect current state and rollout. No automatic retry.".into())
    }
    pub async fn prepare(
        &self,
        key: &str,
        ns: &str,
        name: &str,
        action: Action,
    ) -> Result<Prepared> {
        if !self.policy.writable {
            return Err("READ ONLY: mutations are disabled; opt in with --write".into());
        }
        if action == Action::Kill && !self.allow_force_kill {
            return Err("Force kill requires --write --allow-force-kill".into());
        }
        let object = self.raw_get(key, ns, name).await?;
        let kind = self.find(key)?;
        if !matches!(
            (kind.plural.as_str(), kind.group.as_str()),
            ("pods", "")
                | ("deployments" | "statefulsets" | "daemonsets", "apps")
                | ("services", "")
                | ("jobs" | "cronjobs", "batch")
        ) {
            return Err("mutations on this API group/resource are disabled".into());
        }
        let target = Target {
            session: self.session.clone(),
            resource: kind.plural.clone(),
            namespace: ns.into(),
            name: name.into(),
            uid: object["metadata"]["uid"].as_str().unwrap_or("").into(),
            version: object["metadata"]["resourceVersion"]
                .as_str()
                .unwrap_or("")
                .into(),
        };
        self.policy.check(&target, &action)?;
        for identity in [&target.uid, &target.version] {
            if identity.len() > 256 || safety::safe_text(identity, 256) != *identity {
                return Err("operation identity exceeds its bounded plain-text limit".into());
            }
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock error")?
            .as_secs()
            .to_string();
        let patch = match &action {
            Action::Scale(replicas) => {
                json!({"metadata":{"uid":target.uid,"resourceVersion":target.version},"spec":{"replicas":replicas}})
            }
            Action::Restart => {
                crate::edit::expected_restart(&object, &timestamp)?;
                json!({"metadata":{"uid":target.uid,"resourceVersion":target.version},"spec":{"template":{"metadata":{"annotations":{"kborg.dev/restartedAt":timestamp}}}}})
            }
            Action::SetImage { container, image } => {
                crate::edit::expected_spec(&object, container, image)?;
                json!({"metadata":{"uid":target.uid,"resourceVersion":target.version},"spec":crate::edit::container_patch_spec(&object, container, Some(image), None)?})
            }
            Action::SetResources {
                container,
                resources,
            } => {
                crate::edit::expected_resources(&object, container, resources)?;
                json!({"metadata":{"uid":target.uid,"resourceVersion":target.version},"spec":crate::edit::container_patch_spec(&object, container, None, Some(resources))?})
            }
            Action::SetContainer { container, edit } => {
                crate::edit::expected_container(&object, container, edit)?;
                json!({"metadata":{"uid":target.uid,"resourceVersion":target.version},"spec":crate::edit::container_patch_spec(&object, container, Some(&edit.image), edit.resources.as_ref())?})
            }
            Action::Delete | Action::Kill => Value::Null,
        };
        let effect = match &action {
            Action::Scale(n) => format!("scale replicas {} -> {n}", object["spec"]["replicas"]),
            Action::Restart => crate::edit::expected_restart(&object, &timestamp)?.1,
            Action::SetImage { container, image } => {
                crate::edit::expected_spec(&object, container, image)?.1
            }
            Action::SetResources {
                container,
                resources,
            } => crate::edit::expected_resources(&object, container, resources)?.1,
            Action::SetContainer { container, edit } => {
                crate::edit::expected_container(&object, container, edit)?.1
            }
            Action::Delete => {
                "delete object; background cascading; server default grace period; no force".into()
            }
            Action::Kill => "FORCE KILL Pod: gracePeriodSeconds=0; the API object is removed at once without waiting for container shutdown; the kubelet may still be stopping processes and a controller may create a replacement".into(),
        };
        let summary = format!(
            "{}\nserver: {}\ncontext: {}\ntarget: {}/{} ({})\nUID: {}\nresourceVersion: {}\n{}\nType {}/{} to execute. Approval expires after 60 seconds.",
            if self.is_demo() {
                "DEMO"
            } else {
                "PRODUCTION / WRITE ENABLED"
            },
            self.server,
            self.context,
            ns,
            name,
            key,
            target.uid,
            target.version,
            effect,
            ns,
            name
        );
        let summary = safety::review_text(&summary, 16_384)?;
        let prepared = Prepared {
            reviewed_target: target.clone(),
            reviewed_action: action.clone(),
            reviewed_summary: summary.clone(),
            target,
            action,
            summary,
            patch,
            kind: kind.clone(),
            created: std::time::Instant::now(),
        };
        if !self.is_demo() {
            let response = self.perform(&prepared, true).await?;
            match &prepared.action {
                Action::SetImage { container, image } => {
                    let (expected, _) = crate::edit::expected_spec(&object, container, image)?;
                    crate::edit::check_admission(&response, &expected, &object)?;
                }
                Action::SetResources {
                    container,
                    resources,
                } => {
                    let (expected, _) =
                        crate::edit::expected_resources(&object, container, resources)?;
                    crate::edit::check_resource_admission(
                        &response, &expected, &object, container,
                    )?;
                }
                Action::SetContainer { container, edit } => {
                    let (expected, _) = crate::edit::expected_container(&object, container, edit)?;
                    if edit.resources.is_some() {
                        crate::edit::check_resource_admission(
                            &response, &expected, &object, container,
                        )?;
                    } else {
                        crate::edit::check_admission(&response, &expected, &object)?;
                    }
                }
                Action::Scale(n) if response["spec"]["replicas"] != *n => {
                    return Err(
                        "admission changed the requested replica count; operation blocked".into(),
                    );
                }
                Action::Restart => {
                    crate::edit::check_metadata(&response, &object)?;
                    let (expected, _) = crate::edit::expected_restart(&object, &timestamp)?;
                    if response["spec"] != expected {
                        return Err("admission changed additional workload fields during dry-run; operation blocked for review".into());
                    }
                }
                _ => {}
            }
        }
        Ok(prepared)
    }
    fn delete_params(prepared: &Prepared, dry_run: bool) -> DeleteParams {
        DeleteParams {
            dry_run,
            preconditions: Some(Preconditions {
                uid: Some(prepared.target.uid.clone()),
                resource_version: Some(prepared.target.version.clone()),
            }),
            grace_period_seconds: (prepared.action == Action::Kill).then_some(0),
            ..DeleteParams::background()
        }
    }
    async fn perform(&self, prepared: &Prepared, dry_run: bool) -> Result<Value> {
        let api = self.api(&prepared.kind, &prepared.target.namespace)?;
        match &prepared.action {
            Action::Delete | Action::Kill => {
                api.delete(
                    &prepared.target.name,
                    &Self::delete_params(prepared, dry_run),
                )
                .await
                .map_err(api_error)?;
                Ok(Value::Null)
            }
            Action::Scale(_) => {
                let response = api
                    .patch_scale(
                        &prepared.target.name,
                        &PatchParams {
                            dry_run,
                            ..Default::default()
                        },
                        &Patch::Merge(&prepared.patch),
                    )
                    .await
                    .map_err(api_error)?;
                serde_json::to_value(response).map_err(|_| "cannot decode scale response".into())
            }
            Action::SetImage { .. } | Action::SetResources { .. } | Action::SetContainer { .. } => {
                let response = api
                    .patch(
                        &prepared.target.name,
                        &PatchParams {
                            dry_run,
                            ..Default::default()
                        },
                        &Patch::Strategic(&prepared.patch),
                    )
                    .await
                    .map_err(api_error)?;
                serde_json::to_value(response).map_err(|_| "cannot decode workload response".into())
            }
            Action::Restart => {
                let response = api
                    .patch(
                        &prepared.target.name,
                        &PatchParams {
                            dry_run,
                            ..Default::default()
                        },
                        &Patch::Merge(&prepared.patch),
                    )
                    .await
                    .map_err(api_error)?;
                serde_json::to_value(response)
                    .map_err(|_| "cannot decode deployment response".into())
            }
        }
    }
    pub async fn execute(&mut self, prepared: Prepared, mut approval: Approval) -> Result<String> {
        if prepared.target != prepared.reviewed_target
            || prepared.action != prepared.reviewed_action
            || prepared.summary != prepared.reviewed_summary
        {
            return Err("prepared review changed; prepare and review again".into());
        }
        self.policy.check(&prepared.target, &prepared.action)?;
        if prepared.created.elapsed() > Duration::from_secs(60) {
            return Err("review expired; prepare and review again".into());
        }
        if prepared.target.session != self.session {
            return Err("session changed; review again".into());
        }
        approval.consume(&prepared.target, &prepared.action)?;
        if self.is_demo() {
            match prepared.action {
                Action::Delete | Action::Kill => self
                    .demo
                    .retain(|v| v["metadata"]["uid"] != prepared.target.uid),
                Action::Scale(n) => {
                    for v in &mut self.demo {
                        if v["metadata"]["uid"] == prepared.target.uid {
                            v["spec"]["replicas"] = json!(n);
                        }
                    }
                }
                Action::SetImage { container, image } => {
                    for object in &mut self.demo {
                        if object["metadata"]["uid"] == prepared.target.uid {
                            object["spec"] =
                                crate::edit::expected_spec(object, &container, &image)?.0;
                        }
                    }
                }
                Action::SetResources {
                    container,
                    resources,
                } => {
                    for v in &mut self.demo {
                        if v["metadata"]["uid"] == prepared.target.uid {
                            v["spec"] =
                                crate::edit::expected_resources(v, &container, &resources)?.0;
                            v["metadata"]["resourceVersion"] =
                                json!(format!("{}r", prepared.target.version));
                        }
                    }
                }
                Action::SetContainer { container, edit } => {
                    for object in &mut self.demo {
                        if object["metadata"]["uid"] == prepared.target.uid {
                            object["spec"] =
                                crate::edit::expected_container(object, &container, &edit)?.0;
                            object["metadata"]["resourceVersion"] =
                                json!(format!("{}e", prepared.target.version));
                        }
                    }
                }
                Action::Restart => {
                    for object in &mut self.demo {
                        if object["metadata"]["uid"] == prepared.target.uid {
                            let timestamp = prepared.patch["spec"]["template"]["metadata"]["annotations"]["kborg.dev/restartedAt"].as_str().ok_or("restart review timestamp missing")?;
                            object["spec"] = crate::edit::expected_restart(object, timestamp)?.0;
                            object["metadata"]["resourceVersion"] =
                                json!(format!("{}r", prepared.target.version));
                        }
                    }
                }
            }
            return Ok("Demo operation completed (no cluster contacted)".into());
        }
        let audit = start_audit(&prepared, &self.context)?;
        let outcome =
            tokio::time::timeout(Duration::from_secs(15), self.perform(&prepared, false)).await;
        let result = match outcome {
            Ok(value) => value,
            Err(_) => {
                Err("outcome UNKNOWN after timeout; reconcile server state before retrying".into())
            }
        };
        let status = if result.is_ok() {
            "succeeded"
        } else {
            "failed-or-unknown"
        };
        finish_audit(&audit, status)?;
        result?;
        Ok(
            "Operation accepted by API server; inspect rollout/current state. No automatic retry."
                .into(),
        )
    }
}
fn start_audit(prepared: &Prepared, context: &str) -> Result<PathBuf> {
    start_operation_audit(&prepared.target, prepared.action.operation_name(), context)
}
fn start_operation_audit(target: &Target, action: &str, context: &str) -> Result<PathBuf> {
    let dir = safety::config_dir()?.join("audit");
    safety::checked_path(&dir)?;
    if dir.exists()
        && std::fs::read_dir(&dir)
            .map_err(|_| "cannot inspect audit spool")?
            .take(1001)
            .count()
            >= 1000
    {
        return Err("audit spool full; archive it before new writes".into());
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock error")?
        .as_nanos();
    let path = dir.join(format!("{stamp}-{}.jsonl", std::process::id()));
    let record = json!({"schema_version":1,"stage":"intent","context":context,"namespace":target.namespace,"resource":target.resource,"name":target.name,"uid":target.uid,"resourceVersion":target.version,"action":action,"timestamp_unix_ns":stamp.to_string()});
    safety::write_new(&path, format!("{record}\n").as_bytes())?;
    Ok(path)
}
fn finish_audit(path: &std::path::Path, status: &str) -> Result<()> {
    use std::io::Write;
    safety::checked_path(path)?;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|_| "operation may have executed; audit result storage failed")?;
    writeln!(file, "{}", json!({"stage":"outcome","status":status}))
        .and_then(|_| file.sync_all())
        .map_err(|_| "operation may have executed; audit result storage failed".into())
}
fn api_error(error: kube::Error) -> String {
    match error {
        kube::Error::Api(response)=>format!("Kubernetes API {} ({}); server message suppressed",response.code,safety::safe_text(&response.reason,80)),
        _=>"Kubernetes transport/response failure; data may be stale and write outcomes may be unknown".into(),
    }
}
fn demo_objects() -> Vec<Value> {
    let object = |kind: &str, name: &str, status: Value, spec: Value| {
        let resource = kinds().into_iter().find(|resource| resource.kind == kind);
        let version = resource
            .as_ref()
            .map(|resource| resource.ar().api_version)
            .unwrap_or_else(|| "v1".into());
        let mut value = json!({"apiVersion":version,"kind":kind,"metadata":{"name":name,"uid":format!("demo-{kind}-{name}"),"resourceVersion":"1","creationTimestamp":"2026-10-10T07:00:00Z"},"spec":spec,"status":status});
        if resource.is_some_and(|resource| resource.namespaced) {
            value["metadata"]["namespace"] = json!("demo");
        }
        value
    };
    let mut objects = vec![
        object(
            "Pod",
            "api-7d4f9",
            json!({"phase":"Running","containerStatuses":[{"name":"app","ready":true,"restartCount":0,"state":{"running":{}}}]}),
            json!({"nodeName":"demo-node","containers":[{"name":"app","image":"example/demo:v1","resources":{"requests":{"cpu":"250m","memory":"128Mi"},"limits":{"cpu":"1","memory":"512Mi"}}}]}),
        ),
        object(
            "Pod",
            "worker-6a1bc",
            json!({"phase":"Running","containerStatuses":[{"name":"app","ready":false,"restartCount":7,"state":{"waiting":{"reason":"CrashLoopBackOff"}}}]}),
            json!({"nodeName":"demo-node","containers":[{"name":"app","image":"example/demo:v1","resources":{"requests":{"cpu":"250m","memory":"128Mi"},"limits":{"cpu":"1","memory":"512Mi"}}}]}),
        ),
        object(
            "Deployment",
            "api",
            json!({"readyReplicas":2,"availableReplicas":2,"updatedReplicas":2}),
            json!({"replicas":2,"template":{"spec":{"containers":[{"name":"app","image":"example/demo:v1"}]}}}),
        ),
        object(
            "Service",
            "api",
            json!({}),
            json!({"type":"ClusterIP","clusterIP":"10.96.10.4","ports":[{"port":8080}]}),
        ),
        object("Namespace", "demo", json!({"phase":"Active"}), json!({})),
        object(
            "StatefulSet",
            "redis",
            json!({"readyReplicas":1}),
            json!({"replicas":1,"serviceName":"redis","updateStrategy":{"type":"RollingUpdate"},"template":{"spec":{"containers":[{"name":"redis","image":"redis:7","resources":{"requests":{"cpu":"100m","memory":"64Mi"},"limits":{"cpu":"500m","memory":"256Mi"}}}]}}}),
        ),
        object(
            "DaemonSet",
            "collector",
            json!({"numberReady":1,"desiredNumberScheduled":1}),
            json!({"updateStrategy":{"type":"RollingUpdate"},"template":{"spec":{"containers":[{"name":"collector","image":"example/collector:v1","resources":{"requests":{"cpu":"25m","memory":"32Mi"},"limits":{"cpu":"100m","memory":"128Mi"}}}]}}}),
        ),
        object(
            "ReplicaSet",
            "api-7d4f9",
            json!({"readyReplicas":2}),
            json!({"replicas":2}),
        ),
        object(
            "Job",
            "schema-migrate",
            json!({"succeeded":1,"conditions":[{"type":"Complete","status":"True"}]}),
            json!({"completions":1}),
        ),
        object(
            "CronJob",
            "nightly-backup",
            json!({"active":[]}),
            json!({"schedule":"0 2 * * *","suspend":false,"jobTemplate":{"spec":{"template":{"spec":{"restartPolicy":"Never","containers":[{"name":"backup","image":"example/backup:v1","resources":{"requests":{"cpu":"100m","memory":"64Mi"},"limits":{"cpu":"500m","memory":"256Mi"}}}]}}}}}),
        ),
        object(
            "Ingress",
            "api",
            json!({}),
            json!({"rules":[{"host":"api.demo.local"}]}),
        ),
        object("ConfigMap", "api-config", json!({}), json!({})),
        object(
            "PersistentVolumeClaim",
            "redis-data",
            json!({"phase":"Bound"}),
            json!({"storageClassName":"standard","resources":{"requests":{"storage":"1Gi"}}}),
        ),
        object(
            "Node",
            "demo-node",
            json!({"conditions":[{"type":"Ready","status":"True"}],"capacity":{"cpu":"4","memory":"8Gi"},"allocatable":{"cpu":"3500m","memory":"7Gi"}}),
            json!({"unschedulable":false}),
        ),
        object("Role", "viewer", json!({}), json!({})),
        object("RoleBinding", "viewer", json!({}), json!({})),
        object("StorageClass", "standard", json!({}), json!({})),
    ];
    objects.extend([
        object("ServiceAccount", "reader", json!({}), json!({})),
        object("PersistentVolume", "demo-volume", json!({"phase":"Bound"}), json!({"capacity":{"storage":"1Gi"},"storageClassName":"standard","accessModes":["ReadWriteOnce"],"claimRef":{"namespace":"demo","name":"redis-data"}})),
        object("Endpoints", "api", json!({}), json!({})),
        object("ReplicationController", "legacy-worker", json!({"readyReplicas":1}), json!({"replicas":1,"selector":{"app":"legacy-worker"},"template":{"metadata":{"labels":{"app":"legacy-worker"}},"spec":{"containers":[{"name":"worker","image":"example/worker:v1"}]}}})),
        object("ResourceQuota", "team-budget", json!({"hard":{"requests.cpu":"4","requests.memory":"8Gi"},"used":{"requests.cpu":"500m","requests.memory":"256Mi"}}), json!({"hard":{"requests.cpu":"4","requests.memory":"8Gi"}})),
        object("LimitRange", "container-defaults", json!({}), json!({"limits":[{"type":"Container","default":{"cpu":"500m","memory":"256Mi"},"defaultRequest":{"cpu":"100m","memory":"64Mi"}}]})),
        object("EndpointSlice", "api-demo", json!({}), json!({})),
        object("HorizontalPodAutoscaler", "api", json!({"currentReplicas":2,"desiredReplicas":2}), json!({"scaleTargetRef":{"apiVersion":"apps/v1","kind":"Deployment","name":"api"},"minReplicas":2,"maxReplicas":5,"metrics":[{"type":"Resource","resource":{"name":"cpu","target":{"type":"Utilization","averageUtilization":75}}}]})),
        object("PodDisruptionBudget", "api", json!({"disruptionsAllowed":1,"currentHealthy":2,"desiredHealthy":1}), json!({"minAvailable":1,"selector":{"matchLabels":{"app":"api"}}})),
        object("NetworkPolicy", "api-ingress", json!({}), json!({"podSelector":{"matchLabels":{"app":"api"}},"policyTypes":["Ingress"],"ingress":[{"ports":[{"protocol":"TCP","port":8080}]}]})),
        object("CustomResourceDefinition", "widgets.demo.local", json!({"conditions":[{"type":"Established","status":"True"}]}), json!({"group":"demo.local","scope":"Namespaced","names":{"plural":"widgets","singular":"widget","kind":"Widget"},"versions":[{"name":"v1","served":true,"storage":true,"schema":{"openAPIV3Schema":{"type":"object"}}}]})),
        object("ClusterRole", "demo-reader", json!({}), json!({})),
        object("ClusterRoleBinding", "demo-reader", json!({}), json!({})),
        object("PriorityClass", "batch-priority", json!({}), json!({})),
        object("RuntimeClass", "standard", json!({}), json!({})),
        object("CertificateSigningRequest", "demo-client", json!({"conditions":[]}), json!({"signerName":"kubernetes.io/kube-apiserver-client","usages":["client auth"],"username":"demo-user","request":"synthetic-example-no-private-key"})),
        object("VolumeAttachment", "demo-attachment", json!({"attached":true}), json!({"attacher":"demo.local/storage","nodeName":"demo-node","source":{"persistentVolumeName":"demo-volume"}})),
        object("APIService", "v1beta1.metrics.k8s.io", json!({"conditions":[{"type":"Available","status":"True"}]}), json!({"group":"metrics.k8s.io","version":"v1beta1","groupPriorityMinimum":100,"versionPriority":100,"service":{"name":"metrics-server","namespace":"kube-system"}})),
    ]);
    let mut event = object("Event", "worker-backoff", json!({}), json!({}));
    event["type"] = json!("Warning");
    event["reason"] = json!("BackOff");
    event["message"] = json!("Back-off restarting failed container app in worker-6a1bc");
    event["count"] = json!(7);
    event["involvedObject"] = json!({"kind":"Pod","namespace":"demo","name":"worker-6a1bc"});
    objects.push(event);
    for object in &mut objects {
        match object["kind"].as_str().unwrap_or("") {
            "Pod" if object["metadata"]["name"] == "api-7d4f9" => {
                object["metadata"]["labels"] = json!({"app":"api"});
                object["metadata"]["ownerReferences"] = json!([{"apiVersion":"apps/v1","kind":"ReplicaSet","name":"api-7d4f9","uid":"demo-ReplicaSet-api-7d4f9","controller":true}]);
            }
            "Pod" => {
                object["metadata"]["labels"] = json!({"app":"worker"});
            }
            "ReplicaSet" => {
                object["metadata"]["labels"] = json!({"app":"api"});
                object["metadata"]["ownerReferences"] = json!([{"apiVersion":"apps/v1","kind":"Deployment","name":"api","uid":"demo-Deployment-api","controller":true}]);
            }
            "Deployment" => {
                object["metadata"]["labels"] = json!({"app":"api"});
                object["spec"]["selector"] = json!({"matchLabels":{"app":"api"}});
                object["spec"]["template"]["metadata"]["labels"] = json!({"app":"api"});
            }
            "Service" => {
                object["spec"]["selector"] = json!({"app":"api"});
            }
            "Role" | "ClusterRole" => {
                object["rules"] = json!([{"apiGroups":[""],"resources":["pods","services"],"verbs":["get","list","watch"]}]);
            }
            "RoleBinding" => {
                object["subjects"] = json!([{"kind":"ServiceAccount","name":"reader","namespace":"demo"},{"kind":"User","apiGroup":"rbac.authorization.k8s.io","name":"demo-user"}]);
                object["roleRef"] =
                    json!({"apiGroup":"rbac.authorization.k8s.io","kind":"Role","name":"viewer"});
            }
            "ClusterRoleBinding" => {
                object["subjects"] = json!([{"kind":"Group","apiGroup":"rbac.authorization.k8s.io","name":"demo-viewers"}]);
                object["roleRef"] = json!({"apiGroup":"rbac.authorization.k8s.io","kind":"ClusterRole","name":"demo-reader"});
            }
            "ServiceAccount" => {
                object["automountServiceAccountToken"] = json!(false);
            }
            "Endpoints" => {
                object["subsets"] = json!([{"addresses":[{"ip":"10.0.0.10","targetRef":{"kind":"Pod","namespace":"demo","name":"api-7d4f9"}}],"ports":[{"port":8080,"protocol":"TCP"}]}]);
            }
            "EndpointSlice" => {
                object["metadata"]["labels"] = json!({"kubernetes.io/service-name":"api"});
                object["addressType"] = json!("IPv4");
                object["endpoints"] = json!([{"addresses":["10.0.0.10"],"conditions":{"ready":true},"targetRef":{"kind":"Pod","namespace":"demo","name":"api-7d4f9"}}]);
                object["ports"] = json!([{"port":8080,"protocol":"TCP"}]);
            }
            "PriorityClass" => {
                object["value"] = json!(1000);
                object["globalDefault"] = json!(false);
            }
            "RuntimeClass" => {
                object["handler"] = json!("runc");
            }
            _ => {}
        }
        if object["kind"] == "StorageClass" {
            object["provisioner"] = json!("demo.local/storage");
        }
        if object["kind"] == "ConfigMap" {
            object["data"] = json!({"endpoint":"synthetic-example"});
        }
    }
    objects
}

pub fn summary(object: &Value) -> String {
    if object["metadata"]["deletionTimestamp"].is_string() {
        return "Terminating".into();
    }
    match object["kind"].as_str().unwrap_or("") {
        "Pod" => {
            if let Some(statuses) = object["status"]["initContainerStatuses"].as_array() {
                for (index, status) in statuses.iter().enumerate() {
                    // Restartable init containers are sidecars: running is sufficient
                    // to proceed, even though they never terminate successfully.
                    let sidecar =
                        object["spec"]["initContainers"]
                            .as_array()
                            .is_some_and(|containers| {
                                containers.iter().any(|c| {
                                    c["name"] == status["name"] && c["restartPolicy"] == "Always"
                                })
                            });
                    if status["state"]["terminated"]["exitCode"] == 0
                        || (sidecar && status["started"] == true)
                    {
                        continue;
                    }
                    if let Some(reason) = status["state"]["waiting"]["reason"]
                        .as_str()
                        .or_else(|| status["state"]["terminated"]["reason"].as_str())
                    {
                        return safety::safe_text(&format!("Init:{reason}"), 100);
                    }
                    return format!("Init:{index}/{}", statuses.len());
                }
            }
            let state = object["status"]["containerStatuses"]
                .as_array()
                .and_then(|a| {
                    a.iter()
                        .find_map(|v| v["state"]["waiting"]["reason"].as_str())
                        .or_else(|| {
                            a.iter().find_map(|v| {
                                (v["state"]["terminated"]["exitCode"]
                                    .as_i64()
                                    .is_some_and(|n| n != 0))
                                .then(|| {
                                    v["state"]["terminated"]["reason"]
                                        .as_str()
                                        .unwrap_or("Error")
                                })
                            })
                        })
                });
            let phase = object["status"]["phase"].as_str().unwrap_or("Unknown");
            let status = state.unwrap_or_else(|| {
                if phase == "Running"
                    && (object["status"]["containerStatuses"]
                        .as_array()
                        .is_some_and(|statuses| statuses.iter().any(|v| v["ready"] == false))
                        || object["status"]["conditions"]
                            .as_array()
                            .is_some_and(|conditions| {
                                conditions.iter().any(|condition| {
                                    condition["type"] == "Ready" && condition["status"] != "True"
                                })
                            }))
                {
                    "NotReady"
                } else if phase == "Succeeded" {
                    "Completed"
                } else {
                    phase
                }
            });
            safety::safe_text(status, 100)
        }
        "Deployment" | "StatefulSet" | "ReplicaSet" => format!(
            "{}/{} ready",
            object["status"]["readyReplicas"].as_i64().unwrap_or(0),
            object["spec"]["replicas"].as_i64().unwrap_or(1)
        ),
        "DaemonSet" => format!(
            "{}/{} ready",
            object["status"]["numberReady"].as_u64().unwrap_or(0),
            object["status"]["desiredNumberScheduled"]
                .as_u64()
                .unwrap_or(0)
        ),
        "Node" => {
            let ready = object["status"]["conditions"]
                .as_array()
                .and_then(|conditions| conditions.iter().find(|c| c["type"] == "Ready"));
            let status = match ready.and_then(|c| c["status"].as_str()) {
                Some("True") => "Ready",
                Some("False") => "NotReady",
                _ => "Unknown",
            };
            format!(
                "{status}{}",
                if object["spec"]["unschedulable"] == true {
                    ",SchedulingDisabled"
                } else {
                    ""
                }
            )
        }
        "Job" => {
            let condition = object["status"]["conditions"]
                .as_array()
                .and_then(|conditions| {
                    conditions.iter().find(|c| {
                        c["status"] == "True" && (c["type"] == "Failed" || c["type"] == "Complete")
                    })
                });
            match condition.and_then(|c| c["type"].as_str()) {
                Some("Failed") => "Failed".into(),
                Some("Complete") => "Complete".into(),
                _ => format!(
                    "{}/{} complete",
                    object["status"]["succeeded"].as_u64().unwrap_or(0),
                    object["spec"]["completions"].as_u64().unwrap_or(1)
                ),
            }
        }
        "CronJob" => {
            if object["spec"]["suspend"] == true {
                "Suspended".into()
            } else {
                format!(
                    "{} | {} active",
                    safety::safe_text(object["spec"]["schedule"].as_str().unwrap_or("-"), 50),
                    object["status"]["active"].as_array().map_or(0, Vec::len)
                )
            }
        }
        "Service" => safety::safe_text(object["spec"]["type"].as_str().unwrap_or("ClusterIP"), 100),
        "Ingress" => object["spec"]["rules"]
            .as_array()
            .map(|rules| {
                rules
                    .iter()
                    .filter_map(|r| r["host"].as_str())
                    .take(3)
                    .map(|host| safety::safe_text(host, 50))
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "-".into()),
        "Event" => safety::safe_text(object["reason"].as_str().unwrap_or("Unknown"), 100),
        "ConfigMap" => "Data redacted".into(),
        "StorageClass" => safety::safe_text(object["provisioner"].as_str().unwrap_or("-"), 100),
        _ => safety::safe_text(object["status"]["phase"].as_str().unwrap_or("-"), 100),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::runtime;
    type LogRequests = std::sync::Arc<std::sync::Mutex<Vec<String>>>;
    fn log_capture_gateway(replies: Vec<(u16, Value)>) -> (Gateway, LogRequests) {
        let replies = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
            replies,
        )));
        let requests: LogRequests = Default::default();
        let recorded = requests.clone();
        let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
            recorded
                .lock()
                .unwrap_or_else(|e| panic!("{e}"))
                .push(request.uri().to_string());
            let (status, value) = replies
                .lock()
                .unwrap_or_else(|e| panic!("{e}"))
                .pop_front()
                .unwrap_or_else(|| panic!("unexpected workload log request"));
            async move {
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(status)
                        .body(http_body_util::Full::new(bytes::Bytes::from(
                            value.to_string(),
                        )))
                        .unwrap_or_else(|e| panic!("{e}")),
                )
            }
        });
        let mut gateway = Gateway::for_test_client(Client::new(service, "default"), "default");
        gateway.logs = true;
        (gateway, requests)
    }
    fn log_capture_pod(name: &str, uid: &str) -> Value {
        json!({"apiVersion":"v1","kind":"Pod","metadata":{"namespace":"default","name":name,"uid":uid,"labels":{"app":"web","team":"prod"}},"spec":{"containers":[{"name":"main","image":"fixture"},{"name":"sidecar","image":"fixture"}],"initContainers":[{"name":"init","image":"fixture"}]}})
    }
    #[test]
    fn workload_logs_capture_real_selector_complete_membership_and_connection_identity()
    -> Result<()> {
        runtime()?.block_on(async {
            let owner=json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"namespace":"default","name":"different-than-label","uid":"deployment-uid"},"spec":{"selector":{"matchLabels":{"app":"web"},"matchExpressions":[{"key":"team","operator":"In","values":["prod"]}]}}});
            let pods=json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"10"},"items":[log_capture_pod("one","uid-one"),log_capture_pod("two","uid-two")]});
            let (mut gateway,requests)=log_capture_gateway(vec![(200,owner),(200,pods)]);
            let membership=gateway.capture_log_membership("default",crate::logs::PodLogSelection::Workload{resource:"deployments.apps".into(),name:"different-than-label".into(),uid:"deployment-uid".into()}).await?;
            assert_eq!(membership.pods().len(),2);assert_eq!(membership.source_count(),4);
            assert!(membership.pods().iter().all(|pod|pod.containers==["main","sidecar"]));
            {let requests=requests.lock().map_err(|_|"requests")?;assert_eq!(requests.len(),2);assert!(requests[1].contains("labelSelector="));assert!(requests[1].contains("web")&&requests[1].contains("team")&&requests[1].contains("prod"));assert!(!requests[1].contains("different-than-label"));}
            gateway.session="reconnected".into();
            assert!(gateway.membership_logs(&membership,false,Default::default()).await.is_err_and(|error|error.contains("another connection")));
            assert_eq!(requests.lock().map_err(|_|"requests")?.len(),2);
            Ok(())
        })
    }
    #[test]
    fn workload_logs_reject_partial_foreign_namespace_selector_escapes_and_stale_workload()
    -> Result<()> {
        runtime()?.block_on(async {
            for variant in 0..4 {
                let mut pod=log_capture_pod("one","uid-one");
                if variant==0{pod["metadata"]["namespace"]=json!("foreign");}
                if variant==1{pod["metadata"]["labels"]["app"]=json!("other");}
                let mut list=json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"10"},"items":[pod]});
                if variant==2{list["metadata"]["continue"]=json!("more");}
                let response=if variant==3{(403,json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":403,"reason":"Forbidden","message":"CANARY private diagnostic"}))}else{(200,list)};
                let (gateway,requests)=log_capture_gateway(vec![response]);
                let error=gateway.capture_log_membership("default",crate::logs::PodLogSelection::Selector("app=web".into())).await.err().ok_or("invalid or partial log scope accepted")?;
                assert!(!error.contains("CANARY"));assert_eq!(requests.lock().map_err(|_|"requests")?.len(),1);
            }
            let owner=json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"namespace":"default","name":"web","uid":"replaced"},"spec":{"selector":{"matchLabels":{"app":"web"}}}});
            let (gateway,requests)=log_capture_gateway(vec![(200,owner)]);
            assert!(gateway.capture_log_membership("default",crate::logs::PodLogSelection::Workload{resource:"deployments.apps".into(),name:"web".into(),uid:"captured".into()}).await.is_err());
            assert_eq!(requests.lock().map_err(|_|"requests")?.len(),1);
            let (mut gateway,requests)=log_capture_gateway(Vec::new());gateway.logs=false;
            assert!(gateway.capture_log_membership("default",crate::logs::PodLogSelection::Selector("app=web".into())).await.is_err_and(|error|error.contains("--allow-logs")));
            gateway.logs=true;
            assert!(gateway.capture_log_membership("*",crate::logs::PodLogSelection::Selector("app=web".into())).await.is_err());
            assert!(gateway.capture_log_membership("default",crate::logs::PodLogSelection::Selector(String::new())).await.is_err());
            gateway.kinds.push(Kind{plural:"deployments".into(),kind:"Deployment".into(),group:"example.io".into(),version:"v1".into(),namespaced:true,short_names:Vec::new()});
            assert!(gateway.capture_log_membership("default",crate::logs::PodLogSelection::Workload{resource:"deployments.example.io".into(),name:"web".into(),uid:"uid".into()}).await.is_err());
            assert!(requests.lock().map_err(|_|"requests")?.is_empty());
            Ok(())
        })
    }
    #[test]
    fn cronjob_logs_follow_controller_uid_chain_and_exclude_unrelated_matching_pods() -> Result<()>
    {
        runtime()?.block_on(async {
            let owner=json!({"apiVersion":"batch/v1","kind":"CronJob","metadata":{"namespace":"default","name":"nightly","uid":"cron-uid"},"spec":{}});
            let job=|name:&str,uid:&str,cron:&str|json!({"apiVersion":"batch/v1","kind":"Job","metadata":{"namespace":"default","name":name,"uid":uid,"ownerReferences":[{"apiVersion":"batch/v1","kind":"CronJob","name":"nightly","uid":cron,"controller":true}]},"spec":{"selector":{"matchLabels":{"app":"web"}},"template":{"spec":{"containers":[{"name":"main","image":"fixture"}],"restartPolicy":"Never"}}}});
            let jobs=json!({"apiVersion":"batch/v1","kind":"JobList","metadata":{"resourceVersion":"10"},"items":[job("owned","job-uid","cron-uid"),job("other","other-job","different-cron")]});
            let mut related=log_capture_pod("owned-pod","owned-pod-uid");related["metadata"]["ownerReferences"]=json!([{"apiVersion":"batch/v1","kind":"Job","name":"owned","uid":"job-uid","controller":true}]);
            let mut unrelated=log_capture_pod("unrelated","other-pod-uid");unrelated["metadata"]["ownerReferences"]=json!([{"apiVersion":"batch/v1","kind":"Job","name":"other","uid":"other-job","controller":true}]);
            let pods=json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"11"},"items":[related,unrelated]});
            let (gateway,requests)=log_capture_gateway(vec![(200,owner),(200,jobs),(200,pods)]);
            let membership=gateway.capture_log_membership("default",crate::logs::PodLogSelection::Workload{resource:"cronjobs.batch".into(),name:"nightly".into(),uid:"cron-uid".into()}).await?;
            assert_eq!(membership.pods().len(),1);assert_eq!(membership.pods()[0].name,"owned-pod");assert_eq!(membership.parents.len(),2);
            assert_eq!(membership.parents[1].uid,"job-uid");assert_eq!(requests.lock().map_err(|_|"requests")?.len(),3);
            Ok(())
        })
    }
    #[test]
    fn merged_logs_gate_before_pod_reads_and_reject_unbound_identity() -> Result<()> {
        runtime()?.block_on(async {
            let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let recorded = calls.clone();
            let service = tower::service_fn(move |_: http::Request<kube::client::Body>| {
                recorded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async {
                    Ok::<_, std::convert::Infallible>(http::Response::new(
                        http_body_util::Full::new(bytes::Bytes::new()),
                    ))
                }
            });
            let mut gateway = Gateway::for_test_client(Client::new(service, "default"), "default");
            assert!(
                gateway
                    .merged_logs("default", "app", "uid", false, Default::default())
                    .await
                    .is_err_and(|error| error.contains("--allow-logs"))
            );
            assert!(
                gateway
                    .follow_merged_logs(
                        "default",
                        "app",
                        "uid",
                        Default::default(),
                        Default::default()
                    )
                    .await
                    .is_err_and(|error| error.contains("--allow-logs"))
            );
            gateway.logs = true;
            assert!(
                gateway
                    .merged_logs("default", "app", "", false, Default::default())
                    .await
                    .is_err()
            );
            assert!(
                gateway
                    .follow_merged_logs("*", "app", "uid", Default::default(), Default::default())
                    .await
                    .is_err()
            );
            assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
            Ok(())
        })
    }
    #[test]
    fn log_ranges_reach_the_api_and_unbound_reads_keep_log_only_permissions() -> Result<()> {
        runtime()?.block_on(async {
            let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
            let recorded = requests.clone();
            let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
                recorded
                    .lock()
                    .unwrap_or_else(|e| panic!("{e}"))
                    .push(request.uri().to_string());
                async move {
                    Ok::<_, std::convert::Infallible>(http::Response::new(
                        http_body_util::Full::new(bytes::Bytes::from_static(
                            b"first\nsecond\nthird\nfourth\n",
                        )),
                    ))
                }
            });
            let mut gateway = Gateway::for_test_client(Client::new(service, "default"), "default");
            assert!(gateway.logs("default", "app", None, false).await.is_err());
            assert!(requests.lock().unwrap_or_else(|e| panic!("{e}")).is_empty());
            gateway.logs = true;
            let range = crate::logs::ReadOptions {
                tail_lines: 2,
                since_seconds: Some(60),
                head: false,
            };
            assert_eq!(
                gateway
                    .logs_with_options("default", "app", Some("main".into()), false, range)
                    .await?,
                "third\nfourth\n"
            );
            let head = crate::logs::ReadOptions {
                tail_lines: 2,
                head: true,
                ..Default::default()
            };
            let mut feed = gateway
                .follow_logs_with_read_options(
                    "default",
                    "app",
                    None,
                    false,
                    Default::default(),
                    head,
                )
                .await?;
            assert_eq!(feed.next().await?, Some("first".into()));
            assert_eq!(feed.next().await?, Some("second".into()));
            assert_eq!(feed.next().await?, None);
            let requests = requests.lock().unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(requests.len(), 2); // No additional Pod GET for plain logs.
            assert!(requests[0].contains("tailLines=2"));
            assert!(requests[0].contains("sinceSeconds=60"));
            assert!(requests[0].contains("container=main"));
            assert!(requests[0].contains("timestamps=true"));
            assert!(requests[1].contains("limitBytes=262144"));
            assert!(!requests[1].contains("tailLines"));
            assert!(!requests[1].contains("follow=true"));
            Ok(())
        })
    }
    #[test]
    fn offline_resource_edit_is_sealed_and_preserves_container_fields() -> Result<()> {
        runtime()?.block_on(async {
            let mut gateway=Gateway::connect(Connection{demo:true,writable:true,..Default::default()}).await?;
            let action=Action::SetResources{container:"app".into(),resources:crate::edit::ResourceEdit{cpu_request:"100m".into(),memory_request:"64Mi".into(),cpu_limit:"500m".into(),memory_limit:"128Mi".into()}};
            let prepared=gateway.prepare("deployments","demo","api",action).await?;
            assert!(prepared.summary.contains("+ requests.cpu: 100m"));
            let approval=Approval::confirm(prepared.target.clone(),prepared.action.clone(),"demo/api")?;
            gateway.execute(prepared,approval).await?;
            let object=gateway.get("deployments","demo","api").await?;
            assert_eq!(object["spec"]["template"]["spec"]["containers"][0]["image"],"example/demo:v1");
            assert_eq!(object["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]["memory"],"128Mi");
            Ok(())
        })
    }
    #[test]
    fn summaries_report_init_readiness_deletion_and_node_health() {
        let pod = json!({"kind":"Pod","status":{"phase":"Running","containerStatuses":[{"ready":false}]}});
        assert_eq!(summary(&pod), "NotReady");
        assert_eq!(
            summary(
                &json!({"kind":"Pod","status":{"phase":"Running","containerStatuses":[{"ready":true}],"conditions":[{"type":"Ready","status":"False"}]}})
            ),
            "NotReady"
        );
        assert_eq!(
            summary(&json!({"kind":"Pod","status":{"phase":"Succeeded"}})),
            "Completed"
        );
        let mut init = pod.clone();
        init["status"]["initContainerStatuses"] =
            json!([{"name":"init","state":{"waiting":{"reason":"ImagePullBackOff"}}}]);
        assert_eq!(summary(&init), "Init:ImagePullBackOff");
        init["metadata"]["deletionTimestamp"] = json!("2026-10-10T08:00:00Z");
        assert_eq!(summary(&init), "Terminating");
        let node = json!({"kind":"Node","spec":{"unschedulable":true},"status":{"conditions":[{"type":"Ready","status":"False"}]}});
        assert_eq!(summary(&node), "NotReady,SchedulingDisabled");
        let hostile = json!({"kind":"Event","reason":"bad\u{001b}[2J\u{202e}"});
        assert_eq!(summary(&hostile), "bad[2J");
    }
    #[test]
    fn demo_covers_every_browsable_kind_with_correct_scope_and_version() -> Result<()> {
        runtime()?.block_on(async {
            let gateway = Gateway::connect(Connection {
                demo: true,
                ..Default::default()
            })
            .await?;
            for kind in kinds() {
                let list = gateway.list(&kind.key(), "demo").await?;
                let items = list["items"].as_array().ok_or("demo list missing")?;
                assert!(!items.is_empty(), "demo missing {}", kind.kind);
                for item in items {
                    assert_eq!(item["apiVersion"], kind.ar().api_version);
                    assert_eq!(item["metadata"]["namespace"].is_string(), kind.namespaced);
                }
            }
            Ok(())
        })
    }
    #[test]
    fn offline_image_edit_is_reviewed_and_updates_the_sample() {
        runtime().unwrap_or_else(|e| panic!("{e}")).block_on(async {
            let mut gateway = Gateway::connect(Connection {
                demo: true,
                writable: true,
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("{e}"));
            let reviewed = gateway
                .prepare(
                    "deployments",
                    "demo",
                    "api",
                    Action::SetImage {
                        container: "app".into(),
                        image: "example/demo:v2".into(),
                    },
                )
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            let approval =
                Approval::confirm(reviewed.target.clone(), reviewed.action.clone(), "demo/api")
                    .unwrap_or_else(|e| panic!("{e}"));
            gateway
                .execute(reviewed, approval)
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            let object = gateway
                .get("deployments", "demo", "api")
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(
                object["spec"]["template"]["spec"]["containers"][0]["image"],
                "example/demo:v2"
            );
        });
    }
    #[test]
    fn offline_details_and_logs_respect_exact_namespace_and_target() {
        runtime().unwrap_or_else(|e| panic!("{e}")).block_on(async {
            let gateway = Gateway::connect(Connection {
                demo: true,
                logs: true,
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("{e}"));
            assert!(gateway.get("pods", "demo", "api-7d4f9").await.is_ok());
            assert!(gateway.get("pods", "other", "api-7d4f9").await.is_err());
            assert!(gateway.get("pods", "*", "api-7d4f9").await.is_err());
            assert!(gateway.logs("demo", "api-7d4f9", None, false).await.is_ok());
            assert!(
                gateway
                    .logs("other", "api-7d4f9", None, false)
                    .await
                    .is_err()
            );
            assert!(gateway.logs("demo", "missing", None, false).await.is_err());
            let scoped = Gateway::connect(Connection {
                demo: true,
                namespace: "other".into(),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(scoped.namespace, "other");
            assert!(
                scoped
                    .get("pods", &scoped.namespace, "api-7d4f9")
                    .await
                    .is_err()
            );
            assert!(
                scoped
                    .list("pods", &scoped.namespace)
                    .await
                    .unwrap_or_else(|e| panic!("{e}"))["items"]
                    .as_array()
                    .is_some_and(Vec::is_empty)
            );
        });
    }
    #[test]
    fn malicious_auth_and_insecure_tls_are_rejected_before_network() {
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap_or_else(|e| panic!("test temporary root: {e}"))
            .join(format!("kborg-security-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
        let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
        for (i, user, cluster) in [
            (
                0,
                json!({"exec":{"command":"evil","apiVersion":"client.authentication.k8s.io/v1"}}),
                json!({"server":"https://127.0.0.1:1"}),
            ),
            (
                1,
                json!({"token":"FAKE"}),
                json!({"server":"https://127.0.0.1:1","insecure-skip-tls-verify":true}),
            ),
            (
                2,
                json!({"token":"FAKE","as":"administrator"}),
                json!({"server":"https://127.0.0.1:1"}),
            ),
            (
                3,
                json!({"token":"FAKE"}),
                json!({"server":"https://127.0.0.1:1/?access_token=CANARY"}),
            ),
        ] {
            let path = dir.join(format!("{i}.json"));
            let config = json!({"apiVersion":"v1","kind":"Config","current-context":"test","clusters":[{"name":"test","cluster":cluster}],"users":[{"name":"test","user":user}],"contexts":[{"name":"test","context":{"cluster":"test","user":"test","namespace":"default"}}]});
            std::fs::write(&path, config.to_string()).unwrap_or_else(|e| panic!("{e}"));
            let result = rt.block_on(Gateway::connect(Connection {
                kubeconfig: Some(path.clone()),
                ..Default::default()
            }));
            assert!(result.is_err());
            std::fs::remove_file(path).unwrap_or_else(|e| panic!("{e}"));
        }
        std::fs::remove_dir(dir).unwrap_or_else(|e| panic!("{e}"));
    }
    #[test]
    fn oversized_api_body_fails_before_json_decode() {
        let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
        rt.block_on(async {
            let service = tower::service_fn(|_request: http::Request<kube::client::Body>| async {
                let body = http_body_util::Full::new(bytes::Bytes::from(vec![b' '; MAX_BODY + 1]));
                Ok::<_, std::convert::Infallible>(http::Response::new(
                    http_body_util::Limited::new(body, MAX_BODY),
                ))
            });
            let client = Client::new(service, "default");
            let request = http::Request::get("/api/v1/pods")
                .body(vec![])
                .unwrap_or_else(|e| panic!("{e}"));
            assert!(client.request::<Value>(request).await.is_err());
        });
    }
    #[test]
    fn demo_never_reads_missing_credentials_and_readonly_fails_closed() {
        let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
        rt.block_on(async {
            let g = Gateway::connect(Connection {
                demo: true,
                kubeconfig: Some(PathBuf::from("missing-file")),
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("{e}"));
            assert!(g.list("pods", "demo").await.is_ok());
            assert!(g.list("secrets", "demo").await.is_err());
            assert!(
                g.prepare("deployments", "demo", "api", Action::Restart)
                    .await
                    .is_err()
            );
        });
    }
    #[test]
    fn kubeconfig_files_merge_first_wins_and_skip_missing_entries() -> Result<()> {
        let root = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "temp")?
            .join(format!("kborg-kubeconfig-merge-{}", std::process::id()));
        safety::ensure_private_dir(&root)?;
        let file = |name: &str, current: &str, server: &str, extra: &str| -> Result<PathBuf> {
            let path = root.join(name);
            let text = format!(
                "apiVersion: v1\nkind: Config\ncurrent-context: {current}\nclusters:\n- name: shared\n  cluster:\n    server: {server}\n    certificate-authority: ca.pem\n- name: {extra}\n  cluster:\n    server: https://{extra}.example\ncontexts:\n- name: shared\n  context:\n    cluster: shared\n    user: u\n- name: {extra}\n  context:\n    cluster: {extra}\n    user: u\nusers:\n- name: u\n  user:\n    token: t\n"
            );
            let _ = std::fs::remove_file(&path);
            safety::write_new(&path, text.as_bytes())?;
            Ok(path)
        };
        let first = file("a.yaml", "shared", "https://first.example", "east")?;
        let second = file("b.yaml", "west", "https://second.example", "west")?;
        let merged = merge_config_files(vec![first, root.join("missing.yaml"), second])?;
        assert_eq!(merged.current_context.as_deref(), Some("shared"));
        let names = merged.contexts.iter().map(|c| c.name.as_str()).collect::<Vec<_>>();
        assert_eq!(names, ["shared", "east", "west"]);
        let shared = merged
            .clusters
            .iter()
            .find(|c| c.name == "shared")
            .and_then(|c| c.cluster.as_ref())
            .ok_or("shared cluster")?;
        assert_eq!(shared.server.as_deref(), Some("https://first.example"));
        assert_eq!(
            shared.certificate_authority.as_deref(),
            Some(root.join("ca.pem").to_string_lossy().as_ref())
        );
        assert!(merge_config_files(vec![root.join("missing.yaml"), root.join("gone.yaml")]).is_err());
        assert!(merge_config_files(vec![root.join("only-missing.yaml")]).is_err());
        let _ = std::fs::remove_dir_all(&root);
        Ok(())
    }
    #[test]
    fn force_kill_is_pod_only_opt_in_and_uses_zero_grace() {
        let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
        rt.block_on(async {
            let mut g = Gateway::connect(Connection {
                demo: true,
                writable: true,
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("{e}"));
            let name = "api-7d4f9".to_owned();
            assert!(g.get("pods", "demo", &name).await.is_ok());
            assert!(
                g.prepare("pods", "demo", &name, Action::Kill)
                    .await
                    .is_err()
            );
            g.allow_force_kill = true;
            assert!(
                g.prepare("deployments", "demo", "api", Action::Kill)
                    .await
                    .is_err()
            );
            let prepared = g
                .prepare("pods", "demo", &name, Action::Kill)
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            assert!(prepared.summary.contains("FORCE KILL"));
            assert_eq!(
                Gateway::delete_params(&prepared, false).grace_period_seconds,
                Some(0)
            );
            let approval = Approval::confirm(
                prepared.target.clone(),
                prepared.action.clone(),
                &format!("demo/{name}"),
            )
            .unwrap_or_else(|e| panic!("{e}"));
            g.execute(prepared, approval)
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            assert!(g.get("pods", "demo", &name).await.is_err());
        });
    }
    #[test]
    fn prepared_review_cannot_be_changed_after_dry_run() {
        let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
        rt.block_on(async {
            let mut g = Gateway::connect(Connection {
                demo: true,
                writable: true,
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("{e}"));
            let mut prepared = g
                .prepare("deployments", "demo", "api", Action::Scale(2))
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            prepared.action = Action::Delete;
            let approval =
                Approval::confirm(prepared.target.clone(), prepared.action.clone(), "demo/api")
                    .unwrap_or_else(|e| panic!("{e}"));
            assert!(g.execute(prepared, approval).await.is_err());
            assert!(g.get("deployments", "demo", "api").await.is_ok());
        });
    }
    #[test]
    fn all_operation_capabilities_require_write_and_safe_namespace() {
        let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
        rt.block_on(async {
            let mut gateway = Gateway::connect(Connection {
                demo: true,
                writable: true,
                allow_exec: true,
                allow_attach: true,
                allow_port_forward: true,
                ..Default::default()
            })
            .await
            .unwrap_or_else(|e| panic!("{e}"));
            let service = tower::service_fn(|_: http::Request<kube::client::Body>| async {
                Ok::<_, std::convert::Infallible>(http::Response::new(http_body_util::Full::new(
                    bytes::Bytes::new(),
                )))
            });
            gateway.client = Some(Client::new(service, "test"));
            for capability in 0..3 {
                assert!(gateway.operation_api("test", capability).is_ok());
                for namespace in ["", "*", "kube-system", "kube-public", "kube-node-lease"] {
                    assert!(gateway.operation_api(namespace, capability).is_err());
                }
                gateway.policy.writable = false;
                assert!(gateway.operation_api("test", capability).is_err());
                gateway.policy.writable = true;
                gateway.operation_capabilities[capability] = false;
                assert!(gateway.operation_api("test", capability).is_err());
                gateway.operation_capabilities[capability] = true;
            }
        });
    }

    #[test]
    fn image_edit_uses_narrow_patch_preconditions_and_rejects_stale_server_state() {
        let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
        rt.block_on(async {
            let service = tower::service_fn(|request: http::Request<kube::client::Body>| async move {
                use http_body_util::BodyExt;
                let object = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"api","namespace":"test","uid":"old-uid","resourceVersion":"7"},"spec":{"replicas":1,"template":{"spec":{"containers":[{"name":"api","image":"nginx:1","env":[{"name":"TOKEN","value":"private-canary"}]}]}}}});
                let patching = request.method() == http::Method::PATCH;
                let dry = request.uri().query().is_some_and(|q| q.contains("dryRun=All"));
                let mut response = object;
                let mut status = http::StatusCode::OK;
                if patching {
                    assert_eq!(request.headers()["content-type"], "application/strategic-merge-patch+json");
                    let body = request.into_body().collect().await.unwrap_or_else(|e| panic!("{e}")).to_bytes();
                    let patch: Value = serde_json::from_slice(&body).unwrap_or_else(|e| panic!("{e}"));
                    assert_eq!(patch["metadata"]["uid"], "old-uid");
                    assert_eq!(patch["metadata"]["resourceVersion"], "7");
                    assert!(!String::from_utf8_lossy(&body).contains("private-canary"));
                    assert_eq!(patch["spec"]["template"]["spec"]["containers"], json!([{"name":"api","image":"nginx:2"}]));
                    if dry { response["spec"]["template"]["spec"]["containers"][0]["image"] = json!("nginx:2"); }
                    else {
                        status = http::StatusCode::CONFLICT;
                        response = json!({"apiVersion":"v1","kind":"Status","status":"Failure","message":"stale UID/resourceVersion","reason":"Conflict","code":409});
                    }
                }
                Ok::<_, std::convert::Infallible>(http::Response::builder().status(status).body(http_body_util::Full::new(bytes::Bytes::from(response.to_string()))).unwrap_or_else(|e| panic!("{e}")))
            });
            let mut gateway = Gateway::connect(Connection { demo: true, writable: true, ..Default::default() }).await.unwrap_or_else(|e| panic!("{e}"));
            gateway.client = Some(Client::new(service, "test"));
            let prepared = gateway.prepare("deployments", "test", "api", Action::SetImage { container: "api".into(), image: "nginx:2".into() }).await.unwrap_or_else(|e| panic!("{e}"));
            assert!(prepared.summary.contains("- nginx:1\n+ nginx:2"));
            assert!(!prepared.summary.contains("private-canary"));
            assert!(gateway.perform(&prepared, false).await.is_err());
        });
    }

    #[test]
    fn atomic_container_edit_uses_workload_api_and_rejects_unreviewed_admission() -> Result<()> {
        runtime()?.block_on(async {
            use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
            for (kind, resource, group, path) in [
                ("Deployment", "deployments", "apps", "/spec/template/spec/containers"),
                ("StatefulSet", "statefulsets", "apps", "/spec/template/spec/containers"),
                ("DaemonSet", "daemonsets", "apps", "/spec/template/spec/containers"),
                ("CronJob", "cronjobs", "batch", "/spec/jobTemplate/spec/template/spec/containers"),
            ] {
                let containers = json!([
                    {"name":"api","image":"nginx:1","env":[{"name":"TOKEN","value":"private-canary"}],"resources":{"limits":{"example.com/gpu":"1"}}},
                    {"name":"sidecar","image":"example/sidecar:1"}
                ]);
                let spec = if kind == "CronJob" {
                    json!({"schedule":"0 2 * * *","jobTemplate":{"spec":{"template":{"spec":{"containers":containers}}}}})
                } else {
                    json!({"template":{"spec":{"containers":containers}}})
                };
                let original = json!({"apiVersion":format!("{group}/v1"),"kind":kind,"metadata":{"name":"api","namespace":"test","uid":"uid-1","resourceVersion":"7"},"spec":spec});
                let expected_uri = format!("/apis/{group}/v1/namespaces/test/{resource}/api");
                let poison = Arc::new(AtomicBool::new(false));
                let poison_response = poison.clone();
                let requests = Arc::new(AtomicUsize::new(0));
                let counted = requests.clone();
                let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
                    let mut response = original.clone();
                    let expected_uri = expected_uri.clone();
                    let poison_response = poison_response.clone();
                    counted.fetch_add(1, Ordering::SeqCst);
                    async move {
                        use http_body_util::BodyExt;
                        assert_eq!(request.uri().path(), expected_uri);
                        if request.method() == http::Method::PATCH {
                            assert_eq!(request.headers()["content-type"], "application/strategic-merge-patch+json");
                            assert!(request.uri().query().is_some_and(|query| query.contains("dryRun=All")));
                            let body = request.into_body().collect().await.unwrap_or_else(|error| panic!("{error}")).to_bytes();
                            let patch: Value = serde_json::from_slice(&body).unwrap_or_else(|error| panic!("{error}"));
                            assert_eq!(patch["metadata"], json!({"uid":"uid-1","resourceVersion":"7"}));
                            assert!(!String::from_utf8_lossy(&body).contains("private-canary"));
                            let patch_containers = patch.pointer(path).unwrap_or_else(|| panic!("missing reviewed container patch"));
                            assert_eq!(patch_containers, &json!([{"name":"api","image":"nginx:2","resources":{"requests":{"cpu":"100m","memory":"64Mi"},"limits":{"cpu":"500m","memory":"128Mi"}}}]));
                            let selected = response.pointer_mut(path).unwrap_or_else(|| panic!("missing mock containers"));
                            selected[0]["image"] = json!("nginx:2");
                            selected[0]["resources"]["requests"] = json!({"cpu":"0.1","memory":"64Mi"});
                            selected[0]["resources"]["limits"]["cpu"] = json!("500m");
                            selected[0]["resources"]["limits"]["memory"] = json!("128Mi");
                            if poison_response.load(Ordering::SeqCst) {
                                selected[1]["image"] = json!("unreviewed:2");
                            }
                        }
                        Ok::<_, std::convert::Infallible>(http::Response::new(http_body_util::Full::new(bytes::Bytes::from(response.to_string()))))
                    }
                });
                let mut gateway = Gateway::connect(Connection { demo:true, writable:true, ..Default::default() }).await?;
                gateway.client = Some(Client::new(service, "test"));
                let action = Action::SetContainer {
                    container:"api".into(),
                    edit:crate::edit::ContainerEdit {
                        image:"nginx:2".into(),
                        resources:Some(crate::edit::ResourceEdit { cpu_request:"100m".into(),memory_request:"64Mi".into(),cpu_limit:"500m".into(),memory_limit:"128Mi".into() }),
                    },
                };
                let reviewed = gateway.prepare(resource,"test","api",action.clone()).await?;
                assert!(reviewed.summary.contains("+ nginx:2"));
                assert!(!reviewed.summary.contains("private-canary"));
                assert_eq!(requests.load(Ordering::SeqCst),2);
                poison.store(true, Ordering::SeqCst);
                assert!(gateway.prepare(resource,"test","api",action).await.is_err());
                assert_eq!(requests.load(Ordering::SeqCst),4);
            }
            Ok(())
        })
    }

    #[test]
    fn repeated_pagination_token_stops_before_an_unbounded_loop() {
        let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
        rt.block_on(async {
            let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let count = requests.clone();
            let service = tower::service_fn(move |_request: http::Request<kube::client::Body>| {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                async {
                    let body = http_body_util::Full::new(bytes::Bytes::from_static(
                        br#"{"apiVersion":"v1","kind":"PodList","metadata":{"continue":"repeated"},"items":[]}"#,
                    ));
                    Ok::<_, std::convert::Infallible>(http::Response::new(body))
                }
            });
            let mut g = Gateway::connect(Connection { demo: true, ..Default::default() })
                .await.unwrap_or_else(|e| panic!("{e}"));
            g.client = Some(Client::new(service, "default"));
            assert!(g.list("pods", "default").await.is_err());
            assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 2);
        });
    }
    #[test]
    fn duplicate_kubeconfig_names_are_rejected_before_selection() {
        let path = std::env::temp_dir()
            .canonicalize()
            .unwrap_or_else(|e| panic!("test temporary root: {e}"))
            .join(format!("kborg-duplicate-{}.json", std::process::id()));
        let config = json!({"apiVersion":"v1","kind":"Config","contexts":[
            {"name":"ambiguous","context":{"cluster":"first"}},
            {"name":"ambiguous","context":{"cluster":"second"}}
        ]});
        std::fs::write(&path, config.to_string()).unwrap_or_else(|e| panic!("{e}"));
        assert!(
            read_config(&Connection {
                kubeconfig: Some(path.clone()),
                ..Default::default()
            })
            .is_err()
        );
        std::fs::remove_file(path).unwrap_or_else(|e| panic!("{e}"));
    }
}

#[cfg(test)]
mod yaml_gateway_tests {
    use super::*;
    #[test]
    fn yaml_patch_contains_only_reviewed_paths_and_atomic_uid_version_tests() -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
            let stale = Arc::new(AtomicBool::new(false));
            let poisoned = Arc::new(AtomicBool::new(false));
            let patches = Arc::new(AtomicUsize::new(0));
            let (stale_server, poison_server, counted) = (stale.clone(), poisoned.clone(), patches.clone());
            let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
                let stale = stale_server.load(Ordering::SeqCst);
                let poison = poison_server.load(Ordering::SeqCst);
                let counted = counted.clone();
                async move {
                    use http_body_util::BodyExt;
                    assert_eq!(request.uri().path(), "/apis/apps/v1/namespaces/test/deployments/api");
                    let mut response = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"api","namespace":"test","uid":"u1","resourceVersion":if stale {"2"} else {"1"}},"spec":{"replicas":1,"template":{"spec":{"containers":[{"name":"api","image":"nginx:1","env":[{"name":"TOKEN","value":"private-yaml-canary"}]}]}}}});
                    let mut status = http::StatusCode::OK;
                    if request.method() == http::Method::PATCH {
                        counted.fetch_add(1, Ordering::SeqCst);
                        let dry = request.uri().query().is_some_and(|q| q.contains("dryRun=All"));
                        assert!(request.uri().query().is_some_and(|q| q.contains("fieldValidation=Strict")));
                        assert_eq!(request.headers()["content-type"], "application/json-patch+json");
                        let bytes = request.into_body().collect().await.unwrap_or_else(|e| panic!("{e}")).to_bytes();
                        let patch: Value = serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{e}"));
                        assert_eq!(patch, json!([
                            {"op":"test","path":"/metadata/uid","value":"u1"},
                            {"op":"test","path":"/metadata/resourceVersion","value":"1"},
                            {"op":"replace","path":"/spec/replicas","value":2}
                        ]));
                        assert!(!String::from_utf8_lossy(&bytes).contains("private-yaml-canary"));
                        if dry {
                            response["spec"]["replicas"] = json!(2);
                            if poison { response["spec"]["template"]["spec"]["containers"][0]["image"] = json!("unreviewed:1"); }
                        } else {
                            status = http::StatusCode::CONFLICT;
                            response = json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Conflict","code":409});
                        }
                    }
                    Ok::<_,std::convert::Infallible>(http::Response::builder().status(status).body(http_body_util::Full::new(bytes::Bytes::from(response.to_string()))).unwrap_or_else(|e| panic!("{e}")))
                }
            });
            let mut gateway = Gateway::for_test_client(Client::new(service, "test"), "test");
            gateway.policy.writable = true;
            gateway.allow_yaml_edit = true;
            let draft = gateway.yaml_draft("deployments", "test", "api").await?;
            let edited = draft.text().replace("\"replicas\": 1", "\"replicas\": 2");
            let prepared = gateway.prepare_yaml(&draft, &edited).await?;
            assert!(!prepared.summary().contains("private-yaml-canary"));
            assert!(gateway.perform_yaml(&prepared, false).await.is_err());
            assert!(gateway.execute_yaml(prepared, "test/wrong").await.is_err());
            poisoned.store(true, Ordering::SeqCst);
            assert!(gateway.prepare_yaml(&draft, &edited).await.is_err());
            stale.store(true, Ordering::SeqCst);
            let previous_count = patches.load(Ordering::SeqCst);
            assert!(gateway.prepare_yaml(&draft, &edited).await.is_err());
            assert_eq!(patches.load(Ordering::SeqCst), previous_count);
            Ok(())
        })
    }
    #[test]
    fn yaml_demo_requires_separate_capability_and_expires_reviews() -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            let mut gateway = Gateway::connect(Connection {
                demo: true,
                writable: true,
                ..Default::default()
            })
            .await?;
            let draft = gateway.yaml_draft("deployments", "demo", "api").await?;
            let mut edited: Value =
                serde_saphyr::from_str(draft.text()).map_err(|_| "fixture YAML")?;
            edited["spec"]["replicas"] = json!(4);
            let text = serde_json::to_string(&edited).map_err(|_| "fixture JSON")?;
            assert!(gateway.prepare_yaml(&draft, &text).await.is_err());
            gateway.allow_yaml_edit = true;
            let mut expired = gateway.prepare_yaml(&draft, &text).await?;
            expired.created = std::time::Instant::now() - Duration::from_secs(61);
            assert!(gateway.execute_yaml(expired, "demo/api").await.is_err());
            let prepared = gateway.prepare_yaml(&draft, &text).await?;
            gateway.execute_yaml(prepared, "demo/api").await?;
            assert_eq!(
                gateway.get("deployments", "demo", "api").await?["spec"]["replicas"],
                4
            );
            assert!(gateway.prepare_yaml(&draft, &text).await.is_err());
            Ok(())
        })
    }
}
