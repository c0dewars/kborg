use kborg::{
    cli::runtime,
    gateway::{Connection, Gateway},
    ops::{self, Request},
};
use std::{
    path::PathBuf,
    time::{Duration, Instant},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Owned restricted BusyBox pod: no service-account mount, writable root or
/// elevated capabilities. Tests use the same sealed reviews as the product.
async fn interactive_sessions(
    gateway: &Gateway,
    pods: &kube::Api<k8s_openapi::api::core::v1::Pod>,
    namespace: &str,
) {
    use kborg::session::{End, Input};
    let server = pods.get("server").await.unwrap_or_else(|e| panic!("{e}"));
    let image = server
        .spec
        .and_then(|spec| spec.containers.into_iter().find(|c| c.name == "server"))
        .and_then(|container| container.image)
        .unwrap_or_else(|| panic!("fixture image unavailable"));
    let fixture: k8s_openapi::api::core::v1::Pod = serde_json::from_value(serde_json::json!({
        "apiVersion":"v1","kind":"Pod","metadata":{"name":"shell-session","namespace":namespace},
        "spec":{"automountServiceAccountToken":false,"securityContext":{"runAsNonRoot":true,"runAsUser":1000,"seccompProfile":{"type":"RuntimeDefault"}},
            "containers":[{"name":"shell","image":image,"command":["/bin/sh","-i"],"stdin":true,"tty":true,
                "env":[{"name":"TERM","value":"dumb"}],
                "securityContext":{"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]}},
                "resources":{"requests":{"cpu":"10m","memory":"8Mi"},"limits":{"cpu":"100m","memory":"32Mi"}}}]}
    })).unwrap_or_else(|e|panic!("{e}"));
    let fixture = pods
        .create(&kube::api::PostParams::default(), &fixture)
        .await
        .unwrap_or_else(|e| panic!("session fixture: {e}"));
    let uid = fixture
        .metadata
        .uid
        .unwrap_or_else(|| panic!("fixture UID unavailable"));
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let ready = pods
                .get("shell-session")
                .await
                .unwrap_or_else(|e| panic!("{e}"))
                .status
                .and_then(|s| s.conditions)
                .is_some_and(|cs| cs.iter().any(|c| c.type_ == "Ready" && c.status == "True"));
            if ready {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap_or_else(|e| panic!("session readiness: {e}"));
    let typed = format!("{namespace}/shell-session");
    let request = || Request::InteractiveExec {
        container: "shell".into(),
        argv: vec!["/bin/sh".into(), "-i".into()],
        seconds: 20,
    };
    let reviewed = ops::prepare(gateway, namespace, "shell-session", request())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    assert_eq!(reviewed.uid(), uid);
    assert_eq!(reviewed.confirmation(), typed);
    let (sender, inputs) = tokio::sync::mpsc::channel(8);
    let (acknowledge, acknowledged) = tokio::sync::oneshot::channel();
    let mut acknowledge = Some(acknowledge);
    let keyboard = async {
        sender
            .send(Input::Bytes(
                b"printf READY_FOR_INPUT; stty -echo\r".to_vec(),
            ))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        // Actual remote echo acknowledges the native connection. Remain idle
        // beyond the old10s socket timeout before sending more stdin/resize.
        acknowledged
            .await
            .unwrap_or_else(|e| panic!("remote stdin was not acknowledged: {e}"));
        tokio::time::sleep(Duration::from_secs(11)).await;
        sender
            .send(Input::Resize {
                width: 80,
                height: 24,
            })
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        tokio::time::sleep(Duration::from_millis(200)).await;
        sender.send(Input::Bytes(b"printf 'SESSION_STDIN_OK\\n'; printf '\\033]52;c;CLIPBOARD_CANARY\\007'; stty size; exit\r".to_vec())).await.unwrap_or_else(|e|panic!("{e}"));
    };
    let mut output = String::new();
    let (outcome, ()) = futures_util::future::join(
        ops::run_interactive(gateway, reviewed, &typed, inputs, |text| {
            output.push_str(text);
            if output.contains("READY_FOR_INPUT")
                && let Some(sender) = acknowledge.take()
            {
                let _ = sender.send(());
            }
            Ok(())
        }),
        keyboard,
    )
    .await;
    let outcome = outcome.unwrap_or_else(|e| panic!("stdin/resize: {e}"));
    assert_eq!(outcome.end, End::Completed, "{output}");
    assert!(output.contains("SESSION_STDIN_OK"), "{output}");
    assert!(output.contains("24 80"), "resize not reflected: {output}");
    assert!(
        !output.contains("CLIPBOARD_CANARY"),
        "OSC payload leaked: {output}"
    );

    let reviewed = ops::prepare(
        gateway,
        namespace,
        "shell-session",
        Request::InteractiveAttach {
            container: "shell".into(),
            seconds: 5,
        },
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    let (sender, inputs) = tokio::sync::mpsc::channel(8);
    let keyboard = async {
        sender
            .send(Input::Bytes(b"printf 'SESSION_ATTACH_OK\\n'\r".to_vec()))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        tokio::time::sleep(Duration::from_millis(500)).await;
        sender
            .send(Input::Cancel)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
    };
    let mut output = String::new();
    let (outcome, ()) = futures_util::future::join(
        ops::run_interactive(gateway, reviewed, &typed, inputs, |text| {
            output.push_str(text);
            Ok(())
        }),
        keyboard,
    )
    .await;
    assert_eq!(
        outcome.unwrap_or_else(|e| panic!("attach: {e}")).end,
        End::Canceled
    );
    assert!(output.contains("SESSION_ATTACH_OK"), "{output}");

    let reviewed = ops::prepare(
        gateway,
        namespace,
        "shell-session",
        Request::InteractiveExec {
            container: "shell".into(),
            argv: vec!["sleep".into(), "10".into()],
            seconds: 1,
        },
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    let (sender, inputs) = tokio::sync::mpsc::channel(1);
    let started = Instant::now();
    let outcome = ops::run_interactive(gateway, reviewed, &typed, inputs, |_| Ok(()))
        .await
        .unwrap_or_else(|e| panic!("expiry: {e}"));
    drop(sender);
    assert_eq!(outcome.end, End::Expired);
    assert!(started.elapsed() < Duration::from_secs(3));
    assert!(outcome.remote_outcome_unknown());

    let reviewed = ops::prepare(
        gateway,
        namespace,
        "shell-session",
        Request::InteractiveExec {
            container: "shell".into(),
            // Large binary blocks avoid a line-generating loop throttled by the
            // restricted fixture's100m CPU budget. NUL is filtered from display
            // but still counts toward the cumulative raw-output boundary.
            argv: vec![
                "dd".into(),
                "if=/dev/zero".into(),
                "bs=65536".into(),
                "count=272".into(),
            ],
            seconds: 30,
        },
    )
    .await
    .unwrap_or_else(|e| panic!("{e}"));
    let (sender, inputs) = tokio::sync::mpsc::channel(1);
    let outcome = ops::run_interactive(gateway, reviewed, &typed, inputs, |_| Ok(()))
        .await
        .unwrap_or_else(|e| panic!("output bound: {e}"));
    drop(sender);
    assert_eq!(outcome.end, End::OutputLimit, "{outcome:?}");
    assert!(outcome.output_bytes > kborg::session::MAX_OUTPUT);
    assert!(outcome.output_bytes <= kborg::session::MAX_OUTPUT + 8192);

    let reviewed = ops::prepare(gateway, namespace, "shell-session", request())
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let (sender, inputs) = tokio::sync::mpsc::channel(1);
    sender
        .send(Input::Bytes(vec![
            b'x';
            kborg::session::MAX_INPUT_CHUNK + 1
        ]))
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    let outcome = ops::run_interactive(gateway, reviewed, &typed, inputs, |_| Ok(()))
        .await
        .unwrap_or_else(|e| panic!("input bound: {e}"));
    assert_eq!(outcome.end, End::InputLimit);
    assert_eq!(outcome.input_bytes, 0);
    pods.delete(
        "shell-session",
        &kube::api::DeleteParams {
            grace_period_seconds: Some(0),
            preconditions: Some(kube::api::Preconditions {
                uid: Some(uid),
                resource_version: None,
            }),
            ..Default::default()
        },
    )
    .await
    .unwrap_or_else(|e| panic!("session fixture cleanup: {e}"));
}
fn options(identity: &str) -> Connection {
    Connection {
        kubeconfig: Some(PathBuf::from(
            std::env::var(format!(
                "KBORG_OPS_{}_CONFIG",
                identity.to_ascii_uppercase()
            ))
            .unwrap_or_else(|_| panic!("run Test-NativeOperations.ps1")),
        )),
        writable: true,
        allow_exec: true,
        allow_attach: true,
        allow_port_forward: true,
        ..Default::default()
    }
}
fn exec() -> Request {
    Request::Exec {
        container: "server".into(),
        argv: vec!["printf".into(), "KBORG_NATIVE_OK\\033[31m".into()],
    }
}
#[test]
#[ignore = "owned local fixture only; scripts/Test-NativeOperations.ps1"]
fn isolated_native_operations() {
    runtime().unwrap_or_else(|e| panic!("{e}")).block_on(async {
        let gateway = Gateway::connect(options("operator"))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let ns = gateway.namespace.clone();
        assert!(ns.starts_with("kborg-ops-"));
        let typed = format!("{ns}/server");
        let mut readonly = options("operator");
        readonly.writable = false;
        let readonly = Gateway::connect(readonly)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            ops::prepare(&readonly, &ns, "server", exec())
                .await
                .is_err()
        );
        let mut disabled = options("operator");
        disabled.allow_exec = false;
        let disabled = Gateway::connect(disabled)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            ops::prepare(&disabled, &ns, "server", exec())
                .await
                .is_err()
        );
        assert!(
            ops::prepare(&gateway, "kube-system", "server", exec())
                .await
                .is_err()
        );
        let wrong = ops::prepare(&gateway, &ns, "server", exec())
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(ops::run(&gateway, wrong, "wrong/target").await.is_err());
        let reviewed = ops::prepare(&gateway, &ns, "server", exec())
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let output = ops::run(&gateway, reviewed, &typed)
            .await
            .unwrap_or_else(|e| panic!("exec: {e}"));
        assert!(output.contains("KBORG_NATIVE_OK"));
        assert!(!output.contains('\x1b'));
        let denied = Gateway::connect(options("reader"))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let reviewed = ops::prepare(&denied, &ns, "server", exec())
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(ops::run(&denied, reviewed, &typed).await.is_err());
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
        let port = socket.local_addr().unwrap_or_else(|e| panic!("{e}")).port();
        drop(socket);
        let reviewed = ops::prepare(
            &gateway,
            &ns,
            "server",
            Request::PortForward {
                local_port: port,
                remote_port: 8080,
                seconds: 3,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        let client = async {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let mut stream = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            stream
                .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            let mut response = vec![];
            tokio::time::timeout(
                Duration::from_secs(2),
                stream.take(4096).read_to_end(&mut response),
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_else(|e| panic!("{e}"));
            assert!(String::from_utf8_lossy(&response).contains("KBORG_FORWARD_OK"));
        };
        let (result, _) =
            futures_util::future::join(ops::run(&gateway, reviewed, &typed), client).await;
        result.unwrap_or_else(|e| panic!("forward: {e}"));
        assert!(
            tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .is_err()
        );
        // Occupy all four slots with incomplete HTTP requests. A fifth connection may
        // enter the OS accept backlog but must receive no remote data until a slot opens.
        let socket = std::net::TcpListener::bind("127.0.0.1:0").unwrap_or_else(|e| panic!("{e}"));
        let port = socket.local_addr().unwrap_or_else(|e| panic!("{e}")).port();
        drop(socket);
        let reviewed = ops::prepare(
            &gateway,
            &ns,
            "server",
            Request::PortForward {
                local_port: port,
                remote_port: 8080,
                seconds: 5,
            },
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        let clients = async {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let mut held = Vec::new();
            for _ in 0..4 {
                let mut socket =
                    tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                        .await
                        .unwrap_or_else(|e| panic!("{e}"));
                socket
                    .write_all(b"GET / HTTP/1.0\r\n")
                    .await
                    .unwrap_or_else(|e| panic!("{e}"));
                held.push(socket);
            }
            tokio::time::sleep(Duration::from_millis(300)).await;
            let mut fifth = tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            fifth
                .write_all(b"GET / HTTP/1.0\r\nHost: localhost\r\n\r\n")
                .await
                .unwrap_or_else(|e| panic!("{e}"));
            let mut first = [0; 1];
            assert!(
                tokio::time::timeout(Duration::from_millis(300), fifth.read(&mut first))
                    .await
                    .is_err(),
                "fifth client bypassed four-client limit"
            );
            drop(held.pop()); // Abrupt client disconnect frees one slot.
            let mut response = Vec::new();
            tokio::time::timeout(
                Duration::from_secs(2),
                fifth.take(4096).read_to_end(&mut response),
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_else(|e| panic!("{e}"));
            assert!(String::from_utf8_lossy(&response).contains("KBORG_FORWARD_OK"));
            // Leave other streams active until session expiry; they must close.
            for mut socket in held {
                let mut byte = [0; 1];
                let result = tokio::time::timeout(Duration::from_secs(6), socket.read(&mut byte))
                    .await
                    .unwrap_or_else(|e| panic!("{e}"));
                assert!(matches!(result, Ok(0) | Err(_)));
            }
        };
        let (result, _) =
            futures_util::future::join(ops::run(&gateway, reviewed, &typed), clients).await;
        result.unwrap_or_else(|e| panic!("saturation session: {e}"));
        let reviewed = ops::prepare(
            &gateway,
            &ns,
            "server",
            Request::Attach {
                container: "server".into(),
            },
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        let started = Instant::now();
        assert!(ops::run(&gateway, reviewed, &typed).await.is_err());
        assert!(started.elapsed() < Duration::from_secs(40));
        let reviewed = ops::prepare(&gateway, &ns, "server", exec())
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let path = PathBuf::from(
            std::env::var("KBORG_OPS_FIXTURE_CONFIG").unwrap_or_else(|e| panic!("{e}")),
        );
        let config = kube::config::Kubeconfig::read_from(path).unwrap_or_else(|e| panic!("{e}"));
        let config = kube::Config::from_custom_kubeconfig(
            config,
            &kube::config::KubeConfigOptions::default(),
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        let client = kube::Client::try_from(config).unwrap_or_else(|e| panic!("{e}"));
        let pods: kube::Api<k8s_openapi::api::core::v1::Pod> = kube::Api::namespaced(client, &ns);
        interactive_sessions(&gateway, &pods, &ns).await;
        let mut original = pods.get("server").await.unwrap_or_else(|e| panic!("{e}"));
        let old_uid = original.metadata.uid.clone();
        pods.delete(
            "server",
            &kube::api::DeleteParams {
                grace_period_seconds: Some(0),
                ..Default::default()
            },
        )
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        tokio::time::timeout(Duration::from_secs(30), async {
            while pods
                .get_opt("server")
                .await
                .unwrap_or_else(|e| panic!("{e}"))
                .is_some()
            {
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        original.metadata = kube::api::ObjectMeta {
            name: Some("server".into()),
            namespace: Some(ns.clone()),
            ..Default::default()
        };
        original.status = None;
        let replacement = pods
            .create(&kube::api::PostParams::default(), &original)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_ne!(replacement.metadata.uid, old_uid);
        let error = ops::run(&gateway, reviewed, &typed)
            .await
            .err()
            .unwrap_or_else(|| panic!("recreated target was accepted"));
        assert!(error.contains("target changed"));
        tokio::time::timeout(Duration::from_secs(60), async {
            loop {
                let pod = pods.get("server").await.unwrap_or_else(|e| panic!("{e}"));
                if pod
                    .status
                    .and_then(|s| s.conditions)
                    .is_some_and(|cs| cs.iter().any(|c| c.type_ == "Ready" && c.status == "True"))
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));
    });
}
