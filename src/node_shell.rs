//! Reviewed node shell: a short-lived privileged Pod pinned to one Node,
//! entered with `nsenter` into the host's namespaces, deleted afterwards.
//!
//! The review shows the complete Pod before anything is created. The Pod
//! carries `activeDeadlineSeconds`, so Kubernetes ends it even if KBorg is
//! killed before its own cleanup runs.
use crate::{Result, gateway::Gateway, safety};
use k8s_openapi::api::core::v1::Pod;
use kube::api::{DeleteParams, PostParams};
use serde_json::{Value, json};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// busybox 1.37.0 (multi-arch index); it ships `nsenter` and `sleep`.
pub const DEFAULT_IMAGE: &str =
    "busybox:1.37.0@sha256:bdf57e528e45e4433820e045b29b4597825a1c9e38353532d90a01445013f82e";
pub const CONTAINER: &str = "shell";
pub const COMPONENT_LABEL: &str = "node-shell";
/// Extra lifetime beyond the session so the shell is never cut short by its own Pod.
const GRACE_SECONDS: u64 = 120;
const START_BUDGET: Duration = Duration::from_secs(90);

/// Enter every host namespace of PID 1 and start an interactive shell there.
pub fn argv() -> Vec<String> {
    [
        "nsenter", "-t", "1", "-m", "-u", "-i", "-n", "-p", "--", "/bin/sh", "-i",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

pub fn validate_image(image: &str) -> Result<()> {
    if image.is_empty()
        || image.len() > 512
        || image
            .bytes()
            .any(|b| !(b.is_ascii_alphanumeric() || b"./:@-_".contains(&b)))
    {
        return Err("node shell image must be a plain image reference".into());
    }
    Ok(())
}

fn pod_name(node: &str) -> Result<String> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock error")?
        .as_millis()
        % 1_000_000;
    let prefix: String = node.chars().take(36).collect();
    Ok(format!(
        "kborg-shell-{}-{stamp}",
        prefix.trim_end_matches(['-', '.'])
    )
    .replace('.', "-"))
}

pub fn manifest(node: &str, namespace: &str, name: &str, image: &str, seconds: u16) -> Value {
    let lifetime = u64::from(seconds) + GRACE_SECONDS;
    json!({
        "apiVersion": "v1",
        "kind": "Pod",
        "metadata": {
            "name": name,
            "namespace": namespace,
            "labels": {
                "app.kubernetes.io/managed-by": "kborg",
                "app.kubernetes.io/component": COMPONENT_LABEL
            }
        },
        "spec": {
            "nodeName": node,
            "hostPID": true,
            "hostNetwork": true,
            "hostIPC": true,
            "restartPolicy": "Never",
            "terminationGracePeriodSeconds": 0,
            "activeDeadlineSeconds": lifetime,
            "automountServiceAccountToken": false,
            "tolerations": [{"operator": "Exists"}],
            "containers": [{
                "name": CONTAINER,
                "image": image,
                "imagePullPolicy": "IfNotPresent",
                "command": ["sleep", lifetime.to_string()],
                "securityContext": {"privileged": true},
                "resources": {
                    "requests": {"cpu": "10m", "memory": "16Mi"},
                    "limits": {"memory": "64Mi"}
                }
            }]
        }
    })
}

pub struct Prepared {
    node: String,
    namespace: String,
    name: String,
    pod: Pod,
    seconds: u16,
    session: String,
    created: Instant,
    summary: String,
}
impl Prepared {
    pub fn summary(&self) -> &str {
        &self.summary
    }
    pub fn confirmation(&self) -> String {
        format!("shell {}", self.node)
    }
}

/// Validate the Node, dry-run the privileged Pod and build the review.
pub async fn prepare(
    gateway: &Gateway,
    node: &str,
    namespace: &str,
    image: &str,
    seconds: u16,
) -> Result<Prepared> {
    crate::session::validate_lifetime(seconds)?;
    validate_image(image)?;
    if node.is_empty()
        || node.len() > 253
        || !node
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
    {
        return Err("invalid Node name".into());
    }
    let api = gateway.operation_api(namespace, crate::ops::NODE_SHELL_CAPABILITY)?;
    let live = tokio::time::timeout(Duration::from_secs(15), gateway.get("nodes", "*", node))
        .await
        .map_err(|_| "reading the Node timed out")??;
    let ready = live["status"]["conditions"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|c| c["type"] == "Ready" && c["status"] == "True");
    let name = pod_name(node)?;
    let value = manifest(node, namespace, &name, image, seconds);
    let pod: Pod =
        serde_json::from_value(value.clone()).map_err(|_| "cannot build node shell Pod")?;
    tokio::time::timeout(
        Duration::from_secs(15),
        api.create(
            &PostParams {
                dry_run: true,
                field_manager: Some("kborg".into()),
            },
            &pod,
        ),
    )
    .await
    .map_err(|_| "node shell dry run timed out")?
    .map_err(|error| match error {
        kube::Error::Api(status) if status.code == 403 || status.code == 422 => format!(
            "the API server refused the privileged Pod in {namespace} (HTTP {}): RBAC, Pod Security or a policy forbids it. Choose a namespace that permits privileged Pods.",
            status.code
        ),
        _ => "node shell dry run failed; nothing was created".into(),
    })?;
    let summary = format!(
        "REVIEW NODE SHELL / root on the Node\nContext: {}\nServer: {}\nNode: {node}{}\nPod: {namespace}/{name}\nImage: {image}\nSession: up to {seconds} seconds; the Pod ends itself after {} seconds\n\nThis creates a privileged Pod with the host's PID, network and IPC namespaces and runs:\n  {}\nAnything typed runs as root on the Node and can change or break it, and every workload on it.\nThe Pod is deleted when the session ends; if KBorg is killed first, Kubernetes ends it at its deadline.\n\nPod sent to the API server (dry run passed):\n{}\nType shell {node} to create the Pod and connect. Review expires after 60 seconds.",
        gateway.context,
        gateway.server,
        if ready { "" } else { " (NOT Ready)" },
        u64::from(seconds) + GRACE_SECONDS,
        argv().join(" "),
        crate::inspect::yaml(&value)?,
    );
    Ok(Prepared {
        node: node.into(),
        namespace: namespace.into(),
        name,
        pod,
        seconds,
        session: gateway.operation_session().into(),
        created: Instant::now(),
        summary: safety::review_text(&summary, 64 * 1024)?,
    })
}

/// Create the reviewed Pod, wait until it runs and return the session review
/// for it. The caller runs that session; `ops::run` deletes the Pod after.
pub async fn start(
    gateway: &Gateway,
    prepared: Prepared,
    typed: &str,
) -> Result<crate::ops::ReviewedOperation> {
    if typed != prepared.confirmation()
        || prepared.created.elapsed() > Duration::from_secs(60)
        || prepared.session != gateway.operation_session()
    {
        return Err("node shell approval expired or the confirmation differs".into());
    }
    let api = gateway.operation_api(&prepared.namespace, crate::ops::NODE_SHELL_CAPABILITY)?;
    tokio::time::timeout(
        Duration::from_secs(15),
        api.create(
            &PostParams {
                dry_run: false,
                field_manager: Some("kborg".into()),
            },
            &prepared.pod,
        ),
    )
    .await
    .map_err(|_| "node shell Pod creation timed out; check for a kborg-shell Pod")?
    .map_err(|_| "node shell Pod creation failed; nothing is running")?;
    let started = Instant::now();
    let running = loop {
        let phase = api
            .get(&prepared.name)
            .await
            .ok()
            .and_then(|pod| pod.status.and_then(|status| status.phase));
        match phase.as_deref() {
            Some("Running") => break Ok(()),
            Some("Failed" | "Succeeded") => {
                break Err("the node shell Pod stopped before it was ready");
            }
            _ if started.elapsed() > START_BUDGET => {
                break Err(
                    "the node shell Pod did not start within 90 seconds (image pull or scheduling)",
                );
            }
            _ => tokio::time::sleep(Duration::from_secs(1)).await,
        }
    };
    let reviewed = match running {
        Ok(()) => {
            crate::ops::prepare(
                gateway,
                &prepared.namespace,
                &prepared.name,
                crate::ops::Request::NodeShell {
                    node: prepared.node.clone(),
                    seconds: prepared.seconds,
                },
            )
            .await
        }
        Err(error) => Err(error.into()),
    };
    if reviewed.is_err() {
        let _ = api
            .delete(&prepared.name, &DeleteParams::default().grace_period(0))
            .await;
    }
    reviewed
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn pod_is_pinned_privileged_bounded_and_self_expiring() {
        let pod = manifest(
            "worker-1",
            "ops",
            "kborg-shell-worker-1-1",
            DEFAULT_IMAGE,
            300,
        );
        assert_eq!(pod["spec"]["nodeName"], "worker-1");
        assert_eq!(pod["spec"]["activeDeadlineSeconds"], 420);
        assert_eq!(pod["spec"]["automountServiceAccountToken"], false);
        assert_eq!(
            pod["spec"]["containers"][0]["securityContext"]["privileged"],
            true
        );
        assert_eq!(
            pod["metadata"]["labels"]["app.kubernetes.io/component"],
            COMPONENT_LABEL
        );
        assert!(serde_json::from_value::<Pod>(pod).is_ok());
        let name = pod_name("ip-10-0-0-1.ec2.internal").unwrap_or_default();
        assert!(name.len() <= 63 && safety::valid_id(&name), "{name}");
        assert!(validate_image(DEFAULT_IMAGE).is_ok());
        assert!(validate_image("busybox; rm -rf /").is_err());
        assert_eq!(argv()[0], "nsenter");
    }
}
