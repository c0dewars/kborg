use kborg::{
    cli::runtime,
    gateway::{Connection, Gateway},
    metrics::{self, MetricsScope},
    watch::{WatchFeed, WatchSnapshot},
};
use std::{path::PathBuf, process::Command, time::Duration};

fn options(identity: &str) -> Connection {
    let path = std::env::var(format!(
        "KBORG_WATCH_{}_CONFIG",
        identity.to_ascii_uppercase()
    ))
    .unwrap_or_else(|_| panic!("run scripts/Test-WatchMetrics.ps1"));
    Connection {
        kubeconfig: Some(PathBuf::from(path)),
        context: Some("kborg-watch-test".into()),
        ..Default::default()
    }
}
fn namespace() -> String {
    let ns = std::env::var("KBORG_WATCH_NAMESPACE")
        .unwrap_or_else(|_| panic!("missing owned fixture namespace"));
    let run =
        std::env::var("KBORG_WATCH_RUN_ID").unwrap_or_else(|_| panic!("missing run identity"));
    assert_eq!(ns, format!("kborg-watch-{run}"));
    assert_eq!(run.len(), 12);
    assert!(run.bytes().all(|b| b.is_ascii_hexdigit()));
    ns
}
fn kube(args: &[&str]) {
    let executable = std::env::var("KBORG_WATCH_KUBECTL")
        .unwrap_or_else(|_| panic!("missing trusted kubectl path"));
    assert!(PathBuf::from(&executable).is_absolute());
    let output = Command::new(executable)
        .args(["--context", "docker-desktop", "--namespace", &namespace()])
        .args(args)
        .output()
        .unwrap_or_else(|_| panic!("fixture command failed"));
    assert!(
        output.status.success(),
        "fixture command rejected (details suppressed)"
    );
}
async fn matching(
    feed: &mut WatchFeed,
    predicate: impl Fn(&WatchSnapshot) -> bool,
) -> WatchSnapshot {
    tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            let snapshot = feed.next().await;
            assert!(!snapshot.terminal, "{}", snapshot.message);
            assert!(
                !serde_json::to_string(&snapshot.objects)
                    .unwrap_or_default()
                    .contains("KBORG_WATCH_PAYLOAD_CANARY")
            );
            if !snapshot.stale && predicate(&snapshot) {
                return snapshot;
            }
        }
    })
    .await
    .unwrap_or_else(|_| panic!("watch update did not arrive within 20 seconds"))
}
fn has(snapshot: &WatchSnapshot, name: &str) -> bool {
    snapshot
        .objects
        .iter()
        .any(|v| v["metadata"]["name"] == name)
}

#[test]
#[ignore = "requires own scoped Docker Desktop fixtures; scripts/Test-WatchMetrics.ps1"]
fn desktop_watch_lifecycle_and_authorization() {
    let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
    rt.block_on(async {
        let reader = Gateway::connect(options("reader"))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let ns = namespace();
        assert_eq!(reader.namespace, ns);
        assert!(reader.watch("secrets", &ns).is_err());
        let mut feed = reader
            .watch("configmaps", &ns)
            .unwrap_or_else(|e| panic!("{e}"));
        let initial = matching(&mut feed, |s| has(s, "initial")).await;
        assert!(!initial.resource_version.is_empty());
        kube(&[
            "create",
            "configmap",
            "live",
            "--from-literal=sample=KBORG_WATCH_PAYLOAD_CANARY",
        ]);
        matching(&mut feed, |s| has(s, "live")).await;
        kube(&[
            "label",
            "configmap",
            "live",
            "revision=updated",
            "--overwrite",
        ]);
        matching(&mut feed, |s| {
            s.objects.iter().any(|v| {
                v["metadata"]["name"] == "live" && v["metadata"]["labels"]["revision"] == "updated"
            })
        })
        .await;
        kube(&["delete", "configmap", "live", "--wait=true"]);
        matching(&mut feed, |s| !has(s, "live")).await;
        drop(feed); // Dropping the pull-driven stream cancels its network body.
        let mut replacement = reader
            .watch("configmaps", &ns)
            .unwrap_or_else(|e| panic!("{e}"));
        matching(&mut replacement, |s| has(s, "initial") && !has(s, "live")).await;
        let denied = Gateway::connect(options("denied"))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let mut forbidden = denied
            .watch("configmaps", &ns)
            .unwrap_or_else(|e| panic!("{e}"));
        let result = forbidden.next().await;
        assert!(result.terminal && result.stale && result.objects.is_empty());
        assert!(!result.message.contains("system:serviceaccount"));
        let repeated = forbidden.next().await;
        assert!(repeated.terminal && repeated.objects.is_empty());
        // Existing cached identity loses its scoped RoleBinding before its next watch request.
        kube(&["delete", "rolebinding", "reader", "--wait=true"]);
        let revoked = tokio::time::timeout(Duration::from_secs(20), replacement.next())
            .await
            .unwrap_or_else(|_| panic!("revocation was not observed on the next request"));
        assert!(revoked.terminal && revoked.stale && revoked.objects.is_empty());
        assert!(revoked.resource_version.is_empty());
        assert!(!revoked.message.contains("system:serviceaccount"));
        let stopped = replacement.next().await;
        assert!(stopped.terminal && stopped.stale && stopped.objects.is_empty());
    });
}

#[test]
#[ignore = "requires own scoped Docker Desktop fixtures; scripts/Test-WatchMetrics.ps1"]
fn desktop_optional_metrics_api_unavailable_is_safe() {
    let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
    rt.block_on(async {
        let reader = Gateway::connect(options("reader"))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        for scope in [MetricsScope::Pods, MetricsScope::Nodes] {
            let result = metrics::collect(&reader, scope, &namespace()).await;
            let error = result
                .err()
                .unwrap_or_else(|| panic!("expected absent or forbidden optional Metrics API"));
            assert!(error.contains("unavailable") || error.contains("forbidden"));
            assert!(!error.contains("KBORG_WATCH_PAYLOAD_CANARY"));
            assert!(!error.contains("system:serviceaccount"));
            assert!(!error.contains("Bearer"));
        }
        assert!(MetricsScope::parse("secrets").is_err());
    });
}

#[test]
fn independent_metrics_quantity_contract() {
    assert_eq!(
        metrics::normalize_cpu_quantity("1000000n"),
        Ok((1_000_000, false))
    );
    assert_eq!(
        metrics::normalize_cpu_quantity("0.001"),
        Ok((1_000_000, false))
    );
    assert_eq!(metrics::normalize_cpu_quantity("1u"), Ok((1000, false)));
    assert_eq!(
        metrics::normalize_memory_quantity("1Mi"),
        Ok((1_048_576, false))
    );
    assert_eq!(
        metrics::normalize_memory_quantity("1M"),
        Ok((1_000_000, false))
    );
    assert_eq!(metrics::normalize_memory_quantity("0.1"), Ok((1, true)));
    for raw in [
        "",
        "NaN",
        "Inf",
        "-1",
        "1..0",
        "1MiB",
        "1e999",
        "1\u{1b}[2J",
        "KBORG_METRICS_PAYLOAD_CANARY",
        "18446744073709551616",
    ] {
        for result in [
            metrics::normalize_cpu_quantity(raw),
            metrics::normalize_memory_quantity(raw),
        ] {
            let error = result
                .err()
                .unwrap_or_else(|| panic!("invalid quantity accepted"));
            assert!(!error.contains(raw) || raw.is_empty());
            assert!(!error.chars().any(char::is_control));
        }
    }
    for raw in ["1Ki", "1Mi", "1Gi", "1Ti", "1Pi", "1Ei"] {
        assert!(metrics::normalize_cpu_quantity(raw).is_err());
    }
    assert!(metrics::normalize_cpu_quantity(&"1".repeat(129)).is_err());
    assert!(metrics::normalize_memory_quantity(&"1".repeat(129)).is_err());
}
