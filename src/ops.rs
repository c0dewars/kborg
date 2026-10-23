//! Explicitly approved, bounded native Kubernetes subresource operations.
use crate::{Result, gateway::Gateway, safety};
use futures_util::{
    StreamExt,
    future::{Either, select},
    stream::FuturesUnordered,
};
use kube::api::{AttachParams, Portforwarder};
use serde_json::json;
use std::{
    path::PathBuf,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncRead, AsyncReadExt};

pub enum Request {
    Exec {
        container: String,
        argv: Vec<String>,
    },
    Attach {
        container: String,
    },
    InteractiveExec {
        container: String,
        argv: Vec<String>,
        seconds: u16,
    },
    InteractiveAttach {
        container: String,
        seconds: u16,
    },
    PortForward {
        local_port: u16,
        remote_port: u16,
        seconds: u16,
    },
}
impl Request {
    fn capability(&self) -> usize {
        match self {
            Self::Exec { .. } | Self::InteractiveExec { .. } => 0,
            Self::Attach { .. } | Self::InteractiveAttach { .. } => 1,
            Self::PortForward { .. } => 2,
        }
    }
    fn name(&self) -> &'static str {
        match self {
            Self::Exec { .. } => "exec",
            Self::Attach { .. } => "attach",
            Self::InteractiveExec { .. } => "interactive-exec",
            Self::InteractiveAttach { .. } => "interactive-attach",
            Self::PortForward { .. } => "port-forward",
        }
    }
    fn validate(&self) -> Result<()> {
        match self {
            Self::Exec { container, argv }
            | Self::InteractiveExec {
                container, argv, ..
            } => {
                crate::edit::validate(container, "placeholder:1")?;
                if argv.is_empty()
                    || argv.len() > 64
                    || argv.iter().map(String::len).sum::<usize>() > 2048
                    || argv.iter().any(|arg| safety::safe_text(arg, 2048) != *arg)
                    || argv[0].is_empty()
                    || serde_json::to_string(argv)
                        .map_err(|_| "cannot validate arguments")?
                        .len()
                        > 3000
                {
                    return Err("exec requires 1..64 direct arguments, at most 2048 bytes, without control characters".into());
                }
            }
            Self::Attach { container } => crate::edit::validate(container, "placeholder:1")?,
            Self::InteractiveAttach { container, .. } => {
                crate::edit::validate(container, "placeholder:1")?
            }
            Self::PortForward {
                local_port,
                remote_port,
                seconds,
            } => {
                if *local_port < 1024 || *remote_port == 0 || !(1..=300).contains(seconds) {
                    return Err("forwarding requires local port 1024..65535, remote port 1..65535 and duration 1..300 seconds".into());
                }
            }
        }
        if let Self::InteractiveExec { seconds, .. } | Self::InteractiveAttach { seconds, .. } =
            self
        {
            crate::session::validate_lifetime(*seconds)?;
        }
        Ok(())
    }
}
/// Non-secret routing metadata for a reviewed, loopback-only listener.
#[derive(Clone, Debug)]
pub struct ForwardTarget {
    pub namespace: String,
    pub pod: String,
    pub local_port: u16,
    pub remote_port: u16,
    pub seconds: u16,
}
pub struct ReviewedOperation {
    request: Request,
    namespace: String,
    pod: String,
    uid: String,
    version: String,
    session: String,
    created: Instant,
    summary: String,
}
impl ReviewedOperation {
    pub fn summary(&self) -> &str {
        &self.summary
    }
    pub fn uid(&self) -> &str {
        &self.uid
    }
    pub fn confirmation(&self) -> String {
        format!("{}/{}", self.namespace, self.pod)
    }
    pub fn is_port_forward(&self) -> bool {
        matches!(self.request, Request::PortForward { .. })
    }
    pub fn forward_target(&self) -> Option<ForwardTarget> {
        let Request::PortForward {
            local_port,
            remote_port,
            seconds,
        } = self.request
        else {
            return None;
        };
        Some(ForwardTarget {
            namespace: self.namespace.clone(),
            pod: self.pod.clone(),
            local_port,
            remote_port,
            seconds,
        })
    }
}

pub async fn prepare(
    gateway: &Gateway,
    namespace: &str,
    pod: &str,
    request: Request,
) -> Result<ReviewedOperation> {
    request.validate()?;
    if !safety::valid_id(pod) {
        return Err("invalid pod name".into());
    }
    let api = gateway.operation_api(namespace, request.capability())?;
    let object = api
        .get(pod)
        .await
        .map_err(|_| "cannot read operation target")?;
    let uid = object.metadata.uid.ok_or("target has no UID")?;
    let version = object
        .metadata
        .resource_version
        .ok_or("target has no resourceVersion")?;
    let details = match &request {
        Request::Exec { container, argv } => {
            verify_container(object.spec.as_ref(), container)?;
            format!(
                "container: {container}\nDirect argv: {}\nNo stdin or TTY. 30-second deadline. Disconnect does not guarantee remote process termination.",
                serde_json::to_string(argv).map_err(|_| "cannot review arguments")?
            )
        }
        Request::Attach { container } => {
            verify_container(object.spec.as_ref(), container)?;
            format!(
                "container: {container}\nRead-only stdout/stderr attachment; no stdin or TTY. 30-second deadline."
            )
        }
        Request::InteractiveExec {
            container,
            argv,
            seconds,
        } => {
            verify_container(object.spec.as_ref(), container)?;
            format!(
                "container: {container}\nDirect argv: {}\n{}",
                serde_json::to_string(argv).map_err(|_| "cannot review arguments")?,
                crate::session::review(*seconds)
            )
        }
        Request::InteractiveAttach { container, seconds } => {
            verify_container(object.spec.as_ref(), container)?;
            if !object.spec.as_ref().is_some_and(|spec| {
                spec.containers
                    .iter()
                    .any(|c| c.name == *container && c.stdin == Some(true) && c.tty == Some(true))
            }) {
                return Err("interactive attach requires a container originally configured with stdin:true and tty:true".into());
            }
            format!(
                "container: {container}\nAttach to the existing container process; entered commands affect that process.\n{}",
                crate::session::review(*seconds)
            )
        }
        Request::PortForward {
            local_port,
            remote_port,
            seconds,
        } => format!(
            "127.0.0.1:{local_port} -> pod:{remote_port}; {seconds} seconds; at most 4 simultaneous clients. Other local users can access the listener."
        ),
    };
    let summary = safety::review_text(
        &format!(
            "{}\nserver: {}\ncontext: {}\ntarget: {namespace}/{pod}\nUID: {uid}\nresourceVersion: {version}\n{details}\nKubernetes subresources have no UID precondition; a narrow name-recreation race remains.\nType {namespace}/{pod} to approve within 60 seconds.",
            request.name(),
            gateway.server,
            gateway.context
        ),
        16384,
    )?;
    Ok(ReviewedOperation {
        request,
        namespace: namespace.into(),
        pod: pod.into(),
        uid,
        version,
        session: gateway.operation_session().into(),
        created: Instant::now(),
        summary,
    })
}
fn verify_container(spec: Option<&k8s_openapi::api::core::v1::PodSpec>, name: &str) -> Result<()> {
    if spec.is_none_or(|spec| spec.containers.iter().filter(|c| c.name == name).count() != 1) {
        return Err("container must identify exactly one ordinary container".into());
    }
    Ok(())
}

async fn verify_review(
    gateway: &Gateway,
    reviewed: &ReviewedOperation,
    typed: &str,
) -> Result<kube::Api<k8s_openapi::api::core::v1::Pod>> {
    if reviewed.created.elapsed() > Duration::from_secs(60)
        || reviewed.session != gateway.operation_session()
        || typed != format!("{}/{}", reviewed.namespace, reviewed.pod)
    {
        return Err("operation approval expired or target confirmation changed".into());
    }
    let api = gateway.operation_api(&reviewed.namespace, reviewed.request.capability())?;
    let current = tokio::time::timeout(Duration::from_secs(15), api.get(&reviewed.pod))
        .await
        .map_err(|_| "operation target recheck timed out")?
        .map_err(|_| "cannot recheck operation target")?;
    if current.metadata.uid.as_deref() != Some(&reviewed.uid)
        || current.metadata.resource_version.as_deref() != Some(&reviewed.version)
    {
        return Err("target changed; review again".into());
    }
    Ok(api)
}

async fn connect_interactive(
    api: &kube::Api<k8s_openapi::api::core::v1::Pod>,
    reviewed: &ReviewedOperation,
) -> Result<kube::api::AttachedProcess> {
    let connection = async {
        match &reviewed.request {
            Request::InteractiveExec {
                container, argv, ..
            } => {
                api.exec(&reviewed.pod, argv, &crate::session::parameters(container))
                    .await
            }
            Request::InteractiveAttach { container, .. } => {
                api.attach(&reviewed.pod, &crate::session::parameters(container))
                    .await
            }
            _ => return Err("review does not authorize an interactive session".into()),
        }
        .map_err(connection_error)
    };
    tokio::time::timeout(Duration::from_secs(15), connection)
        .await
        .map_err(|_| "interactive connection timed out; remote outcome unknown")?
}

/// Controlled-input session entrypoint for UI integrations and owned live tests.
/// Reuses the exact same one-use target review, capabilities and audit as terminal sessions.
pub async fn run_interactive(
    gateway: &Gateway,
    reviewed: ReviewedOperation,
    typed: &str,
    inputs: tokio::sync::mpsc::Receiver<crate::session::Input>,
    emit: impl FnMut(&str) -> Result<()>,
) -> Result<crate::session::Outcome> {
    let seconds = match &reviewed.request {
        Request::InteractiveExec { seconds, .. } | Request::InteractiveAttach { seconds, .. } => {
            *seconds
        }
        _ => return Err("review does not authorize an interactive session".into()),
    };
    let api = verify_review(gateway, &reviewed, typed).await?;
    let audit = start_audit(gateway, &reviewed)?;
    let result = match connect_interactive(&api, &reviewed).await {
        Ok(process) => crate::session::pump(process, seconds, inputs, emit).await,
        Err(error) => Err(error),
    };
    finish_audit(
        &audit,
        match &result {
            Ok(outcome) if outcome.remote_outcome_unknown() => {
                "session-ended-remote-outcome-unknown"
            }
            Ok(_) => "succeeded",
            Err(_) => "failed-or-unknown",
        },
    )?;
    result
}

pub async fn run(gateway: &Gateway, reviewed: ReviewedOperation, typed: &str) -> Result<String> {
    let api = verify_review(gateway, &reviewed, typed).await?;
    if matches!(
        &reviewed.request,
        Request::InteractiveExec { .. } | Request::InteractiveAttach { .. }
    ) {
        crate::session::ensure_terminal()?;
    }
    let audit = start_audit(gateway, &reviewed)?;
    let mut session_unknown = false;
    let result = match &reviewed.request {
        Request::Exec { container, argv } => {
            let params = AttachParams {
                container: Some(container.clone()),
                ..Default::default()
            };
            match api.exec(&reviewed.pod, argv, &params).await {
                Ok(process) => capture(process).await,
                Err(error) => Err(connection_error(error)),
            }
        }
        Request::Attach { container } => {
            let params = AttachParams {
                container: Some(container.clone()),
                ..Default::default()
            };
            match api.attach(&reviewed.pod, &params).await {
                Ok(process) => capture(process).await,
                Err(error) => Err(connection_error(error)),
            }
        }
        Request::InteractiveExec { seconds, .. } | Request::InteractiveAttach { seconds, .. } => {
            match connect_interactive(&api, &reviewed).await {
                Ok(process) => match crate::session::terminal(process, *seconds).await {
                    Ok(outcome) => {
                        session_unknown = outcome.remote_outcome_unknown();
                        Ok(outcome.summary())
                    }
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            }
        }
        Request::PortForward {
            local_port,
            remote_port,
            seconds,
        } => {
            forward(
                api,
                &reviewed.pod,
                *local_port,
                *remote_port,
                *seconds,
                &reviewed.uid,
                ForwardControl {
                    cancel: None,
                    on_bound: |_: &str| Ok(()),
                },
            )
            .await
        }
    };
    finish_audit(
        &audit,
        if result.is_ok() && session_unknown {
            "session-ended-remote-outcome-unknown"
        } else if result.is_ok() {
            "succeeded"
        } else {
            "failed-or-unknown"
        },
    )?;
    result
}

/// Run the reviewed forwarding session with an explicit UI-owned cancellation handle.
/// The caller must keep this future polled; the callback runs after loopback bind.
pub async fn run_forward(
    gateway: &Gateway,
    reviewed: ReviewedOperation,
    typed: &str,
    mut cancel: tokio::sync::oneshot::Receiver<()>,
    on_bound: impl FnMut(&str) -> Result<()>,
) -> Result<String> {
    let Request::PortForward {
        local_port,
        remote_port,
        seconds,
    } = &reviewed.request
    else {
        return Err("review does not authorize port forwarding".into());
    };
    let api = tokio::select! {
        biased;
        _ = &mut cancel => return Ok("Forwarding canceled before listener creation".into()),
        result = tokio::time::timeout(Duration::from_secs(20), verify_review(gateway, &reviewed, typed)) => {
            result.map_err(|_| "Forward target verification timed out")??
        }
    };
    let audit = start_audit(gateway, &reviewed)?;
    let result = forward(
        api,
        &reviewed.pod,
        *local_port,
        *remote_port,
        *seconds,
        &reviewed.uid,
        ForwardControl {
            cancel: Some(cancel),
            on_bound,
        },
    )
    .await;
    finish_audit(
        &audit,
        if result.is_ok() {
            "succeeded"
        } else {
            "failed-or-unknown"
        },
    )?;
    result
}
fn connection_error(error: kube::Error) -> String {
    match error {
        kube::Error::Api(status) => format!(
            "remote connection failed (HTTP {}); outcome may be unknown",
            status.code
        ),
        kube::Error::UpgradeConnection(kube::client::UpgradeConnectionError::ProtocolSwitch(
            status,
        )) => format!(
            "websocket upgrade refused (HTTP {}); outcome may be unknown",
            status.as_u16()
        ),
        kube::Error::UpgradeConnection(_) => {
            "websocket protocol negotiation failed; outcome may be unknown".into()
        }
        _ => "remote transport failed; outcome may be unknown".into(),
    }
}

async fn read_bounded(reader: impl AsyncRead + Unpin) -> Result<String> {
    let mut data = Vec::new();
    reader
        .take(262145)
        .read_to_end(&mut data)
        .await
        .map_err(|_| "remote output read failed")?;
    if data.len() > 262144 {
        return Err(
            "remote output limit exceeded; connection closed, process outcome unknown".into(),
        );
    }
    Ok(safety::safe_lines(&String::from_utf8_lossy(&data), 262144))
}
async fn capture(mut process: kube::api::AttachedProcess) -> Result<String> {
    let stdout = process.stdout().ok_or("stdout stream unavailable")?;
    let stderr = process.stderr().ok_or("stderr stream unavailable")?;
    let status = process.take_status().ok_or("process status unavailable")?;
    let result = tokio::time::timeout(Duration::from_secs(30), async {
        let (out, err) =
            futures_util::future::try_join(read_bounded(stdout), read_bounded(stderr)).await?;
        let status = status.await.ok_or("remote status unknown")?;
        if status.status.as_deref() != Some("Success") {
            return Err("remote process failed; server details withheld".into());
        }
        Ok(format!("stdout:\n{out}\nstderr:\n{err}"))
    })
    .await
    .map_err(|_| "remote operation timed out; process outcome unknown")?;
    process.abort();
    result
}
struct ForwardGuard(Portforwarder);
impl Drop for ForwardGuard {
    fn drop(&mut self) {
        self.0.abort();
    }
}
struct ForwardControl<F> {
    cancel: Option<tokio::sync::oneshot::Receiver<()>>,
    on_bound: F,
}
async fn forward(
    api: kube::Api<k8s_openapi::api::core::v1::Pod>,
    pod: &str,
    local: u16,
    remote: u16,
    seconds: u16,
    uid: &str,
    control: ForwardControl<impl FnMut(&str) -> Result<()>>,
) -> Result<String> {
    let ForwardControl {
        mut cancel,
        mut on_bound,
    } = control;
    if let Some(receiver) = cancel.as_mut()
        && !matches!(
            receiver.try_recv(),
            Err(tokio::sync::oneshot::error::TryRecvError::Empty)
        )
    {
        return Ok("Forwarding canceled before listener creation".into());
    }
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, local))
        .await
        .map_err(|_| "cannot bind loopback forwarding port")?;
    on_bound(&format!("Listening on 127.0.0.1:{local} -> {pod}:{remote}"))?;
    let session = async {
        let mut clients = FuturesUnordered::new();
        loop {
            if clients.len() == 4 {
                if let Some(result) = clients.next().await {
                    result?;
                }
                continue;
            }
            let accepted = if clients.is_empty() {
                listener.accept().await
            } else {
                match select(Box::pin(listener.accept()), Box::pin(clients.next())).await {
                    Either::Left((accepted, _)) => accepted,
                    Either::Right((completed, _)) => {
                        if let Some(result) = completed {
                            result?;
                        }
                        continue;
                    }
                }
            };
            let (mut socket, _) = accepted.map_err(|_| "loopback accept failed")?;
            let api = api.clone();
            clients.push(async move {
                let current = api
                    .get(pod)
                    .await
                    .map_err(|_| "cannot recheck forwarding target")?;
                if current.metadata.uid.as_deref() != Some(uid) {
                    return Err("forwarding target was replaced; session closed".into());
                }
                let mut guard = ForwardGuard(
                    api.portforward(pod, &[remote])
                        .await
                        .map_err(|_| "remote forwarding connection failed")?,
                );
                let mut stream = guard
                    .0
                    .take_stream(remote)
                    .ok_or("forward stream unavailable")?;
                // A browser refresh or reset closes only this client. Target UID,
                // authorization and upgrade failures above still stop the session.
                let _ = tokio::io::copy_bidirectional(&mut socket, &mut stream).await;
                Ok::<(), String>(())
            });
        }
        #[allow(unreachable_code)]
        Ok::<(), String>(())
    };
    let stopped = async {
        if let Some(cancel) = cancel {
            let _ = cancel.await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    let result = tokio::select! {
        biased;
        _ = stopped => return Ok("Forwarding canceled; listener and active connections closed".into()),
        result = tokio::time::timeout(Duration::from_secs(u64::from(seconds)), session) => result,
    };
    match result {
        Err(_) => Ok("Forwarding session ended; listener and active connections closed".into()),
        Ok(Err(error)) => Err(error),
        Ok(Ok(())) => Ok("Forwarding session ended".into()),
    }
}
fn start_audit(gateway: &Gateway, op: &ReviewedOperation) -> Result<PathBuf> {
    let dir = safety::config_dir()?.join("audit");
    safety::checked_path(&dir)?;
    if dir.exists()
        && std::fs::read_dir(&dir)
            .map_err(|_| "cannot inspect audit spool")?
            .take(1001)
            .count()
            >= 1000
    {
        return Err("audit spool full".into());
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock error")?
        .as_nanos();
    let path = dir.join(format!("ops-{stamp}-{}.jsonl", std::process::id()));
    let record = json!({"schema_version":1,"stage":"intent","context":gateway.context,"namespace":op.namespace,"name":op.pod,"resource":"pods","uid":op.uid,"resourceVersion":op.version,"action":op.request.name()});
    safety::write_new(&path, format!("{record}\n").as_bytes())?;
    Ok(path)
}
fn finish_audit(path: &std::path::Path, status: &str) -> Result<()> {
    use std::io::Write;
    safety::checked_path(path)?;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|_| "operation ended; audit outcome storage failed")?;
    writeln!(file, "{}", json!({"stage":"outcome","status":status}))
        .and_then(|_| file.sync_all())
        .map_err(|_| "operation ended; audit outcome storage failed".into())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn forwarding_can_cancel_after_bind_and_releases_listener() -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            let reserved = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .map_err(|_| "cannot reserve test loopback port")?;
            let port = reserved
                .local_addr()
                .map_err(|_| "cannot inspect test port")?
                .port();
            drop(reserved);
            let service = tower::service_fn(|_: http::Request<kube::client::Body>| async {
                Ok::<_, std::convert::Infallible>(http::Response::new(http_body_util::Full::new(
                    bytes::Bytes::new(),
                )))
            });
            let api = kube::Api::namespaced(kube::Client::new(service, "test"), "test");
            let (cancel, stopped) = tokio::sync::oneshot::channel();
            let mut cancel = Some(cancel);
            let result = forward(
                api,
                "pod",
                port,
                8080,
                300,
                "uid",
                ForwardControl {
                    cancel: Some(stopped),
                    on_bound: |message: &str| {
                        assert!(message.contains(&format!("127.0.0.1:{port}")));
                        cancel
                            .take()
                            .ok_or("test cancellation already consumed")?
                            .send(())
                            .map_err(|_| "test cancellation receiver closed".to_owned())
                    },
                },
            )
            .await?;
            assert!(result.contains("canceled"));
            let rebound = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port))
                .map_err(|_| "forward listener was not released")?;
            drop(rebound);
            Ok(())
        })
    }
    #[test]
    fn canceled_forward_does_not_bind_or_publish_a_listener() -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            let reserved = std::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
                .map_err(|_| "cannot reserve test port")?;
            let port = reserved
                .local_addr()
                .map_err(|_| "cannot inspect test port")?
                .port();
            let service = tower::service_fn(|_: http::Request<kube::client::Body>| async {
                Ok::<_, std::convert::Infallible>(http::Response::new(http_body_util::Full::new(
                    bytes::Bytes::new(),
                )))
            });
            let api = kube::Api::namespaced(kube::Client::new(service, "test"), "test");
            let (cancel, stopped) = tokio::sync::oneshot::channel();
            drop(cancel);
            let result = forward(
                api,
                "pod",
                port,
                8080,
                300,
                "uid",
                ForwardControl {
                    cancel: Some(stopped),
                    on_bound: |_: &str| panic!("canceled forward published listener"),
                },
            )
            .await?;
            assert!(result.contains("canceled before listener creation"));
            drop(reserved);
            Ok(())
        })
    }
    #[test]
    fn restrictive_request_validation() {
        assert!(
            Request::Exec {
                container: "api".into(),
                argv: vec!["echo".into(), "x\nsh".into()]
            }
            .validate()
            .is_err()
        );
        assert!(
            Request::Exec {
                container: "api".into(),
                argv: vec!["echo".into(), "hello world".into()]
            }
            .validate()
            .is_ok()
        );
        for (local_port, remote_port, seconds) in [(80, 80, 1), (8080, 0, 1), (8080, 80, 301)] {
            assert!(
                Request::PortForward {
                    local_port,
                    remote_port,
                    seconds
                }
                .validate()
                .is_err()
            );
        }
    }
    #[test]
    fn rejects_invisible_arguments_and_truncated_reviews() {
        for hidden in ["a\u{202e}b", "a\u{200b}b", "a\u{feff}b"] {
            assert!(
                Request::Exec {
                    container: "api".into(),
                    argv: vec!["echo".into(), hidden.into()]
                }
                .validate()
                .is_err()
            );
        }
        assert!(
            Request::Exec {
                container: "api".into(),
                argv: vec!["echo".into(), "x".repeat(2048)]
            }
            .validate()
            .is_err()
        );
    }
    #[test]
    fn output_bounds_and_terminal_sanitizing() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap_or_else(|e| panic!("{e}"));
        rt.block_on(async {
            assert!(
                !read_bounded(&b"hello\x1b]52;clipboard\x07"[..])
                    .await
                    .unwrap_or_else(|e| panic!("{e}"))
                    .contains('\x1b')
            );
            assert!(read_bounded(vec![b'x'; 262145].as_slice()).await.is_err());
        });
    }
}
