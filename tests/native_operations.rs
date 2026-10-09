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
