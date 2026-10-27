//! Reviewed ephemeral debug containers, KBorg's `kubectl debug -it`.
//!
//! The review shows the exact ephemeral container after a server dry run.
//! On approval KBorg adds it, waits until it runs and opens a shell in it.
//! Kubernetes never removes ephemeral containers from a Pod: the container
//! sleeps for the session lifetime and then exits, but stays in the spec.
use crate::{Result, gateway::Gateway, safety};
use k8s_openapi::api::core::v1::Pod;
use kube::api::{Patch, PatchParams};
use serde_json::{Value, json};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub const NAME_PREFIX: &str = "kborg-debug-";
const GRACE_SECONDS: u64 = 60;
const START_BUDGET: Duration = Duration::from_secs(90);

pub fn argv() -> Vec<String> {
    vec!["sh".into(), "-i".into()]
}

pub fn container(name: &str, image: &str, target: Option<&str>, seconds: u16) -> Value {
    let mut container = json!({
        "name": name,
        "image": image,
        "imagePullPolicy": "IfNotPresent",
        "command": ["sleep", (u64::from(seconds) + GRACE_SECONDS).to_string()],
        "stdin": true,
        "tty": true
    });
    if let Some(target) = target {
        container["targetContainerName"] = json!(target);
    }
    container
}

pub struct Prepared {
    namespace: String,
    pod: String,
    uid: String,
    name: String,
    patch: Value,
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
        format!("debug {}/{}", self.namespace, self.pod)
    }
    pub fn uid(&self) -> &str {
        &self.uid
    }
}

pub async fn prepare(
    gateway: &Gateway,
    namespace: &str,
    pod: &str,
    target: Option<&str>,
    image: &str,
    seconds: u16,
) -> Result<Prepared> {
    crate::session::validate_lifetime(seconds)?;
    crate::node_shell::validate_image(image)?;
    let api = gateway.operation_api(namespace, crate::ops::DEBUG_CAPABILITY)?;
    let live = tokio::time::timeout(Duration::from_secs(15), api.get(pod))
        .await
        .map_err(|_| "reading the Pod timed out")?
        .map_err(|_| "cannot read the Pod")?;
    let uid = live.metadata.uid.clone().ok_or("Pod has no UID")?;
    let spec = live.spec.as_ref().ok_or("Pod has no spec")?;
    if let Some(target) = target
        && !spec.containers.iter().any(|c| c.name == target)
    {
        return Err(format!(
            "{} is not a container of this Pod",
            safety::safe_text(target, 63)
        ));
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock error")?
        .as_secs()
        % 100_000;
    let name = format!("{NAME_PREFIX}{stamp}");
    let added = container(&name, image, target, seconds);
    let patch = json!({"spec":{"ephemeralContainers":[added.clone()]}});
    tokio::time::timeout(
        Duration::from_secs(15),
        api.patch_ephemeral_containers(
            pod,
            &PatchParams {
                dry_run: true,
                ..Default::default()
            },
            &Patch::Strategic(&patch),
        ),
    )
    .await
    .map_err(|_| "debug container dry run timed out")?
    .map_err(|error| {
        crate::errors::explain(
            "Debug container dry run (nothing was changed; needs patch on pods/ephemeralcontainers)",
            &error,
            &gateway.scope(),
        )
    })?;
    let summary = format!(
        "REVIEW DEBUG CONTAINER\nContext: {}\nServer: {}\nPod: {namespace}/{pod}\nUID: {uid}\n{}Image: {image}\nSession: up to {seconds} seconds\n\nAdds this ephemeral container (dry run passed) and opens `sh -i` in it:\n{}\nIt is not privileged and gets no host access. With a target container it shares that container's process namespace when the runtime supports it.\nKubernetes cannot remove ephemeral containers: it exits after its sleep but stays listed in the Pod until the Pod is replaced.\nType debug {namespace}/{pod} to add it and connect. Review expires after 60 seconds.",
        gateway.context,
        gateway.server,
        target.map_or_else(String::new, |t| format!("Target container: {t}\n")),
        crate::inspect::yaml(&added)?,
    );
    Ok(Prepared {
        namespace: namespace.into(),
        pod: pod.into(),
        uid,
        name,
        patch,
        seconds,
        session: gateway.operation_session().into(),
        created: Instant::now(),
        summary: safety::review_text(&summary, 32 * 1024)?,
    })
}

/// Add the reviewed container, wait until it runs and return the session review.
pub async fn start(
    gateway: &Gateway,
    prepared: Prepared,
    typed: &str,
) -> Result<crate::ops::ReviewedOperation> {
    if typed != prepared.confirmation() {
        return Err("debug approval expired or the confirmation differs".into());
    }
    let audit = crate::audit_sink::intent(audit_event(gateway, &prepared))?;
    let result = start_approved(gateway, prepared, typed).await;
    audit.finish(&result)?;
    result
}
/// Audit-sink event for adding the reviewed ephemeral debug container.
pub fn audit_event(gateway: &Gateway, prepared: &Prepared) -> crate::audit_sink::Event {
    crate::audit_sink::Event::new("debug-container-add", &gateway.context, &gateway.server)
        .target(&prepared.namespace, "pods", &prepared.pod, &prepared.uid)
        .review(&prepared.summary)
}
async fn start_approved(
    gateway: &Gateway,
    prepared: Prepared,
    typed: &str,
) -> Result<crate::ops::ReviewedOperation> {
    if typed != prepared.confirmation()
        || prepared.created.elapsed() > Duration::from_secs(60)
        || prepared.session != gateway.operation_session()
    {
        return Err("debug approval expired or the confirmation differs".into());
    }
    let api: kube::Api<Pod> =
        gateway.operation_api(&prepared.namespace, crate::ops::DEBUG_CAPABILITY)?;
    let current = api
        .get(&prepared.pod)
        .await
        .map_err(|_| "cannot recheck the Pod")?;
    if current.metadata.uid.as_deref() != Some(prepared.uid.as_str()) {
        return Err("the Pod was replaced since review; nothing was added".into());
    }
    tokio::time::timeout(
        Duration::from_secs(15),
        api.patch_ephemeral_containers(
            &prepared.pod,
            &PatchParams::default(),
            &Patch::Strategic(&prepared.patch),
        ),
    )
    .await
    .map_err(|_| "adding the debug container timed out; check the Pod")?
    .map_err(|_| "adding the debug container failed; nothing is running")?;
    let started = Instant::now();
    loop {
        let pod = api.get(&prepared.pod).await.ok();
        let state = pod
            .as_ref()
            .and_then(|pod| pod.status.as_ref())
            .and_then(|status| status.ephemeral_container_statuses.as_ref())
            .and_then(|statuses| statuses.iter().find(|s| s.name == prepared.name))
            .and_then(|status| status.state.clone());
        if state.as_ref().is_some_and(|s| s.running.is_some()) {
            break;
        }
        if state.as_ref().is_some_and(|s| s.terminated.is_some()) {
            return Err(
                "the debug container exited before it was ready (does the image have sh?)".into(),
            );
        }
        if started.elapsed() > START_BUDGET {
            return Err("the debug container did not start within 90 seconds (image pull?)".into());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    crate::ops::prepare(
        gateway,
        &prepared.namespace,
        &prepared.pod,
        crate::ops::Request::Debug {
            container: prepared.name.clone(),
            seconds: prepared.seconds,
        },
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn debug_container_is_interactive_bounded_and_unprivileged() {
        let c = container(
            "kborg-debug-1",
            crate::node_shell::DEFAULT_IMAGE,
            Some("app"),
            300,
        );
        assert_eq!(c["targetContainerName"], "app");
        assert_eq!(c["command"][1], "360");
        assert_eq!(
            (c["stdin"].clone(), c["tty"].clone()),
            (json!(true), json!(true))
        );
        assert!(c.get("securityContext").is_none());
        assert!(
            serde_json::from_value::<k8s_openapi::api::core::v1::EphemeralContainer>(c).is_ok()
        );
    }
}
