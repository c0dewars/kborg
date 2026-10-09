use crate::{
    Result,
    policy::{Action, Approval, Policy, Target},
    safety,
};
use k8s_openapi::api::core::v1::Pod;
use kube::{
    Api, Client, Config,
    api::{
        ApiResource, DeleteParams, DynamicObject, ListParams, LogParams, Patch, PatchParams,
        Preconditions,
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Kind {
    pub plural: String,
    pub kind: String,
    pub group: String,
    pub version: String,
    pub namespaced: bool,
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
    })
    .collect()
}

fn config_path(options: &Connection) -> Result<PathBuf> {
    if let Some(path) = &options.kubeconfig {
        return Ok(path.clone());
    }
    if let Some(env) = std::env::var_os("KUBECONFIG") {
        let paths: Vec<_> = std::env::split_paths(&env).collect();
        if paths.len() != 1 {
            return Err("multiple KUBECONFIG files are not supported yet; pass --kubeconfig with one trusted file".into());
        }
        return paths
            .into_iter()
            .next()
            .ok_or_else(|| "empty KUBECONFIG".into());
    }
    let home = std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .ok_or("home directory unavailable")?;
    Ok(PathBuf::from(home).join(".kube/config"))
}
fn read_config(options: &Connection) -> Result<Kubeconfig> {
    let path = config_path(options)?;
    let bytes = safety::read_small(&path, 1024 * 1024)?;
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

pub struct Gateway {
    client: Option<Client>,
    pub context: String,
    pub server: String,
    pub namespace: String,
    session: String,
    policy: Policy,
    logs: bool,
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
                demo: demo_objects(),
                kinds: kinds(),
            });
        }
        let raw = read_config(&options)?;
        let name = options
            .context
            .or_else(|| raw.current_context.clone())
            .ok_or("select a context with --context")?;
        let context = raw
            .contexts
            .iter()
            .find(|c| c.name == name)
            .and_then(|c| c.context.as_ref())
            .ok_or("context not found")?;
        let cluster = raw
            .clusters
            .iter()
            .find(|c| c.name == context.cluster)
            .and_then(|c| c.cluster.as_ref())
            .ok_or("cluster not found")?;
        let user = context
            .user
            .as_ref()
            .and_then(|name| raw.auth_infos.iter().find(|u| &u.name == name))
            .and_then(|u| u.auth_info.as_ref());
        if cluster.insecure_skip_tls_verify == Some(true) {
            return Err("insecure TLS is forbidden".into());
        }
        if cluster.proxy_url.is_some() {
            return Err(
                "proxy configurations need a reviewed adapter; unsupported in this iteration"
                    .into(),
            );
        }
        if let Some(u) = user {
            if u.exec.is_some() || u.auth_provider.is_some() {
                return Err("authentication helpers/providers are disabled until trust and process isolation are implemented; use a short-lived token or client certificate kubeconfig".into());
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
        config.read_timeout = Some(Duration::from_secs(10));
        config.write_timeout = Some(Duration::from_secs(10));
        let server = config.cluster_url.to_string();
        let namespace = if options.namespace.is_empty() {
            config.default_namespace.clone()
        } else {
            options.namespace
        };
        let builder = kube::client::ClientBuilder::try_from(config)
            .map_err(|_| "cannot initialize verified TLS client")?;
        let layer = tower::util::MapResponseLayer::new(
            |response: http::Response<Box<kube::client::DynBody>>| {
                response.map(|body| http_body_util::Limited::new(body, MAX_BODY))
            },
        );
        let client = builder.with_layer(&layer).build();
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
            demo: vec![],
            kinds: kinds(),
        })
    }
    pub fn is_demo(&self) -> bool {
        self.client.is_none()
    }
    pub fn watch(&self, key: &str, ns: &str) -> Result<crate::watch::WatchFeed> {
        let kind = self.find(key)?.clone();
        Ok(crate::watch::WatchFeed::new(self.api(&kind, ns)?, kind))
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
        };
        self.api(&kind, ns)
    }
    fn find(&self, key: &str) -> Result<&Kind> {
        self.kinds
            .iter()
            .find(|k| k.key() == key || k.plural == key)
            .ok_or_else(|| "resource kind not available; Secrets are never exposed".into())
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
    pub async fn discover(&mut self) -> Result<Vec<Kind>> {
        if let Some(client) = &self.client {
            let discovery =
                tokio::time::timeout(OPERATION_BUDGET, kube::Discovery::new(client.clone()).run())
                    .await
                    .map_err(|_| "discovery budget exceeded; narrow the API surface")?
                    .map_err(api_error)?;
            let mut found = Vec::new();
            for group in discovery.groups().take(100) {
                for (ar, caps) in group.recommended_resources() {
                    if !ar.plural.contains('/')
                        && ar.plural != "secrets"
                        && ar.kind != "Secret"
                        && caps.operations.iter().any(|s| s == "list")
                        && found.len() < 200
                    {
                        found.push(Kind {
                            plural: ar.plural,
                            kind: ar.kind,
                            group: ar.group,
                            version: ar.version,
                            namespaced: caps.scope == kube::discovery::Scope::Namespaced,
                        });
                    }
                }
            }
            found.sort_by_key(Kind::key);
            self.kinds = found;
        }
        Ok(self.kinds.clone())
    }
    pub async fn list(&self, key: &str, ns: &str) -> Result<Value> {
        let kind = self.find(key)?;
        if self.is_demo() {
            let items: Vec<Value> = self
                .demo
                .iter()
                .filter(|v| {
                    v["kind"] == kind.kind
                        && (ns == "*" || v["metadata"]["namespace"] == ns || !kind.namespaced)
                })
                .cloned()
                .collect();
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
                    return Ok(json!({"items":items,"truncated":true}));
                }
                items.push(value);
            }
            continuation = list.metadata.continue_.filter(|s| !s.is_empty());
            if continuation.is_none() {
                break;
            }
            if pages >= MAX_LIST_PAGES {
                return Ok(json!({"items":items,"truncated":true}));
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
        Ok(json!({"items":items,"truncated":false}))
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
    pub async fn logs(
        &self,
        ns: &str,
        name: &str,
        container: Option<String>,
        previous: bool,
    ) -> Result<String> {
        if !safety::valid_id(ns)
            || name.is_empty()
            || name.len() > 253
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        {
            return Err("invalid log target".into());
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
            return Ok("2026-10-09T20:00:00Z INFO K8Borg demo started\n2026-10-09T20:00:01Z WARN connection retry\n2026-10-09T20:00:02Z INFO request served".into());
        }
        let pods: Api<Pod> = Api::namespaced(self.client.clone().ok_or("no client")?, ns);
        let output = pods
            .logs(
                name,
                &LogParams {
                    container,
                    previous,
                    tail_lines: Some(200),
                    limit_bytes: Some(256 * 1024),
                    timestamps: true,
                    ..Default::default()
                },
            )
            .await
            .map_err(api_error)?;
        Ok(safety::safe_lines(&output, 256 * 1024))
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
        let object = self.raw_get(key, ns, name).await?;
        let kind = self.find(key)?;
        if !matches!(
            (kind.plural.as_str(), kind.group.as_str()),
            ("pods", "") | ("deployments", "apps") | ("services", "") | ("jobs", "batch")
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
                json!({"metadata":{"uid":target.uid,"resourceVersion":target.version},"spec":{"template":{"metadata":{"annotations":{"k8borg.dev/restartedAt":timestamp}}}}})
            }
            Action::Delete => Value::Null,
        };
        let effect = match &action {
            Action::Scale(n) => format!("scale replicas {} -> {n}", object["spec"]["replicas"]),
            Action::Restart => format!(
                "set pod-template annotation k8borg.dev/restartedAt={timestamp}; triggers rollout"
            ),
            Action::Delete => {
                "delete object; background cascading; server default grace period; no force".into()
            }
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
                Action::Scale(n) if response["spec"]["replicas"] != *n => {
                    return Err(
                        "admission changed the requested replica count; operation blocked".into(),
                    );
                }
                Action::Restart => {
                    let mut expected = object["spec"].clone();
                    expected["template"]["metadata"]["annotations"]["k8borg.dev/restartedAt"]=prepared.patch["spec"]["template"]["metadata"]["annotations"]["k8borg.dev/restartedAt"].clone();
                    if response["spec"] != expected {
                        return Err("admission changed additional deployment fields during dry-run; operation blocked for review".into());
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
            ..DeleteParams::background()
        }
    }
    async fn perform(&self, prepared: &Prepared, dry_run: bool) -> Result<Value> {
        let api = self.api(&prepared.kind, &prepared.target.namespace)?;
        match &prepared.action {
            Action::Delete => {
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
                Action::Delete => self
                    .demo
                    .retain(|v| v["metadata"]["uid"] != prepared.target.uid),
                Action::Scale(n) => {
                    for v in &mut self.demo {
                        if v["metadata"]["uid"] == prepared.target.uid {
                            v["spec"]["replicas"] = json!(n);
                        }
                    }
                }
                Action::Restart => {}
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
    let record = json!({"schema_version":1,"stage":"intent","context":context,"namespace":prepared.target.namespace,"resource":prepared.target.resource,"name":prepared.target.name,"uid":prepared.target.uid,"resourceVersion":prepared.target.version,"action":format!("{:?}",prepared.action),"timestamp_unix_ns":stamp.to_string()});
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
    let object = |kind: &str, name: &str, status: Value, spec: Value| json!({"apiVersion":if kind=="Deployment"{"apps/v1"}else{"v1"},"kind":kind,"metadata":{"name":name,"namespace":"demo","uid":format!("demo-{name}"),"resourceVersion":"1"},"spec":spec,"status":status});
    vec![
        object(
            "Pod",
            "api-7d4f9",
            json!({"phase":"Running","containerStatuses":[{"ready":true,"restartCount":0}]}),
            json!({"nodeName":"demo-node"}),
        ),
        object(
            "Pod",
            "worker-6a1bc",
            json!({"phase":"Running","containerStatuses":[{"ready":false,"restartCount":7,"state":{"waiting":{"reason":"CrashLoopBackOff"}}}]}),
            json!({"nodeName":"demo-node"}),
        ),
        object(
            "Deployment",
            "api",
            json!({"readyReplicas":2,"availableReplicas":2}),
            json!({"replicas":2}),
        ),
        object(
            "Service",
            "api",
            json!({}),
            json!({"type":"ClusterIP","clusterIP":"10.96.10.4","ports":[{"port":8080}]}),
        ),
        object("Event", "worker-backoff", json!({}), json!({})),
        object("Namespace", "demo", json!({"phase":"Active"}), json!({})),
    ]
}

pub fn summary(object: &Value) -> String {
    match object["kind"].as_str().unwrap_or("") {
        "Pod" => {
            let state = object["status"]["containerStatuses"]
                .as_array()
                .and_then(|a| {
                    a.iter()
                        .find_map(|v| v["state"]["waiting"]["reason"].as_str())
                });
            state
                .unwrap_or(object["status"]["phase"].as_str().unwrap_or("Unknown"))
                .into()
        }
        "Deployment" | "StatefulSet" => format!(
            "{}/{} ready",
            object["status"]["readyReplicas"].as_i64().unwrap_or(0),
            object["spec"]["replicas"].as_i64().unwrap_or(0)
        ),
        "Service" => object["spec"]["type"].as_str().unwrap_or("").into(),
        "Event" => object["reason"].as_str().unwrap_or("BackOff").into(),
        _ => object["status"]["phase"].as_str().unwrap_or("-").into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::runtime;
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
        let dir = std::env::temp_dir().join(format!("k8borg-security-{}", std::process::id()));
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
        let path =
            std::env::temp_dir().join(format!("k8borg-duplicate-{}.json", std::process::id()));
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
