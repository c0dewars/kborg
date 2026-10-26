mod apply;
mod batch;
mod catalog;
mod demo;
#[path = "discovery.rs"]
mod discovery;
mod errors;
mod kubeconfig;
mod log_streams;
mod operations;
mod reads;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod yaml_tests;
use crate::{
    Result,
    policy::{Action, Approval, Policy, Target},
    safety,
};
pub use batch::{
    BatchReport, BatchRequest, BatchTarget, MAX_BATCH, PreparedBatch, batch_confirmation,
    batch_supported,
};
use catalog::*;
pub use catalog::{Kind, kinds};
use demo::*;
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api, Client, Config,
    api::{
        ApiResource, DeleteParams, DynamicObject, ListParams, Patch, PatchParams, Preconditions,
    },
    config::{KubeConfigOptions, Kubeconfig},
};
#[cfg(any(test, fuzzing))]
pub(crate) use kubeconfig::parse_config_text;
use kubeconfig::*;
pub use kubeconfig::{audit, contexts, plugin_identity, review_auth_helper};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const MAX_OBJECTS: usize = 10_000;
const MAX_BODY: usize = 64 * 1024 * 1024;
/// Objects per list page; MAX_LIST_PAGES pages cover MAX_OBJECTS.
const LIST_PAGE: u32 = 500;
const MAX_LIST_PAGES: usize = MAX_OBJECTS.div_ceil(LIST_PAGE as usize);
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
    /// Opt-in: reviewed single-file copy to and from Pod containers.
    pub allow_file_copy: bool,
    /// Opt-in: reviewed privileged node shell Pods.
    pub allow_node_shell: bool,
    /// Opt-in: reviewed ephemeral debug containers.
    pub allow_debug: bool,
    pub allow_node_maintenance: bool,
    pub allow_yaml_edit: bool,
    /// Opt-in: reviewed server-side apply of local manifest files.
    pub allow_apply: bool,
    pub allow_force_kill: bool,
    pub allow_secret_reveal: bool,
    pub allow_auth_helper: bool,
    pub auth_helper_policy: Option<PathBuf>,
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
    operation_capabilities: [bool; 6],
    allow_node_maintenance: bool,
    allow_yaml_edit: bool,
    allow_apply: bool,
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
                return Err("Invalid namespace: it must be a DNS label (lowercase letters, digits, -). Next: pass a valid --namespace or pick one with :ns".into());
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
                allow_apply: options.allow_apply,
                allow_force_kill: options.allow_force_kill,
                allow_secret_reveal: options.allow_secret_reveal,
                operation_capabilities: [
                    options.allow_exec,
                    options.allow_attach,
                    options.allow_port_forward,
                    options.allow_file_copy,
                    options.allow_node_shell,
                    options.allow_debug,
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
            .ok_or("No context selected: kubeconfig has no current-context. Next: pass --context NAME or run `kubectl config use-context NAME`")?;
        let context = raw
            .contexts
            .iter()
            .find(|c| c.name == name)
            .and_then(|c| c.context.as_ref())
            .ok_or("Context not found in kubeconfig. Next: list contexts with `kubectl config get-contexts` and pass --context NAME")?
            .clone();
        let cluster = raw
            .clusters
            .iter()
            .find(|c| c.name == context.cluster)
            .and_then(|c| c.cluster.as_ref())
            .ok_or("The context's cluster entry is missing from kubeconfig. Next: fix the cluster name in that context or re-add the cluster (for example with your cloud CLI)")?
            .clone();
        let user = context
            .user
            .as_ref()
            .and_then(|name| raw.auth_infos.iter().find(|u| &u.name == name))
            .and_then(|u| u.auth_info.as_ref())
            .cloned();
        // Like kubectl, the kubeconfig is trusted as written: exec helpers,
        // auth providers, impersonation, proxies and insecure-skip-tls-verify
        // all work. `--allow-auth-helper --auth-helper-policy FILE` keeps the
        // stricter reviewed mode, which refuses all of them.
        let strict = options.allow_auth_helper;
        if strict && cluster.insecure_skip_tls_verify == Some(true) {
            return Err("Reviewed helper mode refuses insecure-skip-tls-verify. Next: remove it and set certificate-authority-data, or drop --allow-auth-helper".into());
        }
        if strict && cluster.proxy_url.is_some() {
            return Err("Reviewed helper mode refuses proxy-url. Next: remove proxy-url for this cluster, or drop --allow-auth-helper".into());
        }
        if let Some(u) = &user {
            if strict
                && (u.auth_provider.is_some()
                    || u.impersonate.is_some()
                    || u.impersonate_groups.is_some()
                    || u.impersonate_uid.is_some()
                    || u.impersonate_user_extra.is_some())
            {
                return Err("Reviewed helper mode refuses auth providers and impersonation. Next: drop --allow-auth-helper to use this kubeconfig as kubectl does".into());
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
        let auth_gate = if strict
            && let Some(user) = &user
            && let Some(exec) = &user.exec
        {
            validate_helper_user(user)?;
            let policy = options
                .auth_helper_policy
                .clone()
                .ok_or("--allow-auth-helper needs a reviewed policy. Next: add --auth-helper-policy FILE (written by `kborg auth-helper review`)")?;
            let user_name = context.user.as_deref().ok_or("The context's user entry is missing from kubeconfig. Next: fix the user name in that context or re-add the user")?;
            let binding = crate::auth::Binding::new(&name, user_name, &cluster, exec)?;
            let credentials =
                tokio::task::spawn_blocking(move || crate::auth::acquire(&policy, &binding))
                    .await
                    .map_err(|_| "Authentication helper worker failed; credentials suppressed. Next: retry; if it repeats, run the helper's own login and review it again")??;
            let (auth, gate) = credentials.into_auth()?;
            let selected_user = raw
                .auth_infos
                .iter_mut()
                .find(|user| user.name == user_name)
                .ok_or("The context's user entry is missing from kubeconfig. Next: fix the user name in that context or re-add the user")?;
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
        .map_err(|_| "Cannot load cluster credentials or CA from kubeconfig (details suppressed). Next: check that the certificate-authority, client-certificate and token files for this context exist and are readable")?;
        if config.cluster_url.scheme_str() != Some("https")
            || config.cluster_url.authority().is_none()
            || config
                .cluster_url
                .authority()
                .is_some_and(|a| a.as_str().contains('@'))
            || config.cluster_url.query().is_some()
        {
            return Err("The cluster server URL must be HTTPS without userinfo or query credentials. Next: set server to https://HOST:PORT in kubeconfig".into());
        }
        config.accept_invalid_certs = !strict && cluster.insecure_skip_tls_verify == Some(true);
        config.disable_compression = true;
        config.default_retry = false;
        if strict {
            config.proxy_url = None;
        }
        config.connect_timeout = Some(Duration::from_secs(5));
        // Normal reads remain bounded by BodyBudgetLayer. Quiet log/TTY
        // sessions use their own explicit lifetime rather than an idle cutoff.
        config.read_timeout = None;
        config.write_timeout = Some(Duration::from_secs(10));
        // Helm install/upgrade hands static credentials to the helm binary,
        // so it needs a token or certificate context. Other contexts still
        // connect; only Helm lifecycle actions explain why they can't run.
        let lifecycle_connection = if options.allow_helm_write {
            crate::helm_lifecycle::StaticConnection::from_config(&mut config, auth_gate.clone())
                .ok()
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
            .map_err(|_| "Cannot initialize the verified TLS client. Next: check that the CA and client certificate data in kubeconfig are valid PEM")?;
        let client = builder
            .with_layer(&crate::transport::BodyBudgetLayer::default())
            .with_layer(&auth_gate)
            .build();
        let serial = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "System clock error. Next: fix the system clock and restart KBorg")?
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
            allow_apply: options.allow_apply,
            allow_force_kill: options.allow_force_kill,
            allow_secret_reveal: options.allow_secret_reveal,
            operation_capabilities: [
                options.allow_exec,
                options.allow_attach,
                options.allow_port_forward,
                options.allow_file_copy,
                options.allow_node_shell,
                options.allow_debug,
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
            return Err("Helm release storage access requires --allow-helm. Next: restart with --allow-helm".into());
        }
        crate::helm::Reader::new(
            self.client
                .clone()
                .ok_or("Offline Helm data uses the demo reader. Next: open :helm in demo mode or connect to a cluster")?,
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
                .ok_or("Helm lifecycle needs a real selected cluster. Next: connect with --context NAME")?,
            self.lifecycle_connection
                .clone()
                .ok_or("Helm install/upgrade/rollback needs a context with a static token or client certificate; this one uses an exec helper, auth provider, impersonation or proxy. Next: run helm directly for this cluster, or switch to a token context with :ctx")?,
            self.session.clone(),
        ))
    }
    pub(crate) fn helm_lifecycle_session(&self) -> Result<&str> {
        if !self.policy.writable || !self.allow_helm_write || !self.allow_helm {
            return Err("Helm lifecycle requires --write --allow-helm --allow-helm-write. Next: restart with those flags".into());
        }
        Ok(&self.session)
    }
    pub(crate) fn helm_extension_access(&self, hooks: bool, crds: bool) -> Result<()> {
        self.helm_lifecycle_session()?;
        if hooks && !self.allow_helm_hooks {
            return Err("Helm hooks require explicit --allow-helm-hooks. Next: restart with --allow-helm-hooks".into());
        }
        if crds && !self.allow_helm_crds {
            return Err("Helm CRD actions require explicit --allow-helm-crds. Next: restart with --allow-helm-crds".into());
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
            operation_capabilities: [false; 6],
            allow_node_maintenance: false,
            allow_yaml_edit: false,
            allow_apply: false,
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
