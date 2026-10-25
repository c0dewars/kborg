//! Pod and parent identity verification for identity-bound log sessions.
use super::{CONNECT_BUDGET, MAX_MERGED_CONTAINERS, MERGED_OPEN_CONCURRENCY};
use crate::{Result, safety};
use futures_util::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::{Api, api::DynamicObject};
use tokio::time::Instant;

#[derive(Clone)]
pub(crate) enum IdentityGuard {
    Pod {
        api: Api<Pod>,
        name: String,
        uid: String,
    },
    Parent {
        api: Api<DynamicObject>,
        name: String,
        uid: String,
    },
}
impl IdentityGuard {
    async fn verify(&self, deadline: Instant) -> std::result::Result<(), ReopenFailure> {
        match self {
            Self::Pod { api, name, uid } => {
                check_uid(api, name, uid, deadline)
                    .await
                    .map_err(|error| match error {
                        ReopenFailure::Stop(error) => ReopenFailure::Stop(format!(
                            "captured Pod {}/{} UID {}: {error}",
                            api.namespace().unwrap_or("?"),
                            name,
                            uid
                        )),
                        ReopenFailure::Retry => ReopenFailure::Retry,
                    })
            }
            Self::Parent { api, name, uid } => {
                let value = tokio::time::timeout_at(deadline, api.get_metadata(name))
                    .await
                    .map_err(|_| ReopenFailure::Retry)?
                    .map_err(reopen_failure)?;
                if value.metadata.name.as_deref() != Some(name)
                    || value.metadata.namespace.as_deref() != api.namespace()
                    || value.metadata.uid.as_deref() != Some(uid)
                {
                    return Err(ReopenFailure::Stop(format!(
                        "captured workload or Job {}/{} UID {} identity changed; capture log membership again",
                        api.namespace().unwrap_or("?"),
                        name,
                        uid
                    )));
                }
                Ok(())
            }
        }
    }
}
pub(super) async fn verify_guards(
    guards: &[IdentityGuard],
    deadline: Instant,
) -> std::result::Result<(), ReopenFailure> {
    let mut checks = futures_util::stream::iter(guards.to_vec())
        .map(|guard| async move { guard.verify(deadline).await })
        .buffer_unordered(MERGED_OPEN_CONCURRENCY);
    while let Some(result) = checks.next().await {
        result?;
    }
    Ok(())
}

pub(super) enum ReopenFailure {
    Retry,
    Stop(String),
}
pub(super) fn reopen_failure(error: kube::Error) -> ReopenFailure {
    match error {
        kube::Error::Api(response) if matches!(response.code, 408 | 429 | 500..=599) => {
            ReopenFailure::Retry
        }
        error @ kube::Error::Api(_) => ReopenFailure::Stop(crate::errors::explain(
            "Log reconnect (stopped)",
            &error,
            &crate::errors::Scope::default(),
        )),
        kube::Error::HyperError(_) | kube::Error::Service(_) | kube::Error::ReadEvents(_) => {
            ReopenFailure::Retry
        }
        _ => ReopenFailure::Stop(
            "log reconnect received an invalid response; stopped without displaying server details. Next: reopen the log view"
                .into(),
        ),
    }
}
pub(super) async fn check_uid(
    api: &Api<Pod>,
    name: &str,
    uid: &str,
    deadline: Instant,
) -> std::result::Result<(), ReopenFailure> {
    let pod = tokio::time::timeout_at(deadline, api.get_metadata(name))
        .await
        .map_err(|_| ReopenFailure::Retry)?
        .map_err(reopen_failure)?;
    if pod.metadata.name.as_deref() != Some(name)
        || pod.metadata.namespace.as_deref() != api.namespace()
        || pod.metadata.uid.as_deref() != Some(uid)
    {
        return Err(ReopenFailure::Stop("log target was replaced or returned outside the selected scope; reopen the selected pod".into()));
    }
    Ok(())
}
/// Called only by the explicitly identity-bound resumable workflow. A plain log
/// read never adds a new pod-spec permission requirement.
pub(crate) async fn verify_uid(api: &Api<Pod>, name: &str, uid: &str) -> Result<()> {
    if uid.is_empty() || uid.len() > 253 || safety::safe_text(uid, 253) != uid {
        return Err("invalid selected pod identity for log reconnect".into());
    }
    match check_uid(api, name, uid, Instant::now() + CONNECT_BUDGET).await {
        Ok(()) => Ok(()),
        Err(ReopenFailure::Stop(message)) => Err(message),
        Err(ReopenFailure::Retry) => {
            Err("log target identity verification failed; retry after checking connectivity".into())
        }
    }
}

/// Validate the complete Pod identity before deriving ordinary container names.
/// A same-name replacement must never silently join an existing log session.
pub(crate) fn ordinary_container_names(
    pod: &Pod,
    ns: &str,
    name: &str,
    uid: &str,
) -> Result<Vec<String>> {
    validate_merge_target(ns, name, uid)?;
    if pod.metadata.name.as_deref() != Some(name)
        || pod.metadata.namespace.as_deref() != Some(ns)
        || pod.metadata.uid.as_deref() != Some(uid)
    {
        return Err("merged log target was replaced or returned outside the selected scope; reopen the selected pod".into());
    }
    let containers = &pod
        .spec
        .as_ref()
        .ok_or("merged log target has no pod spec")?
        .containers;
    if containers.is_empty() || containers.len() > MAX_MERGED_CONTAINERS {
        return Err(format!(
            "merged logs require 1..{MAX_MERGED_CONTAINERS} ordinary containers; select one container instead"
        ));
    }
    let mut names = Vec::with_capacity(containers.len());
    for container in containers {
        if !safety::valid_id(&container.name) || names.contains(&container.name) {
            return Err(
                "merged log pod contains invalid or duplicate ordinary container names".into(),
            );
        }
        names.push(container.name.clone());
    }
    Ok(names)
}
pub(crate) fn validate_merge_target(ns: &str, name: &str, uid: &str) -> Result<()> {
    if !safety::valid_id(ns)
        || name.is_empty()
        || name.len() > 253
        || !name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'-' | b'.'))
        || uid.is_empty()
        || uid.len() > 253
        || safety::safe_text(uid, 253) != uid
    {
        return Err("invalid identity-bound merged log target".into());
    }
    Ok(())
}
