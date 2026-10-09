use k8borg::{
    cli::runtime,
    gateway::{Connection, Gateway},
    policy::{Action, Approval},
};
use std::{path::PathBuf, time::Duration};

fn options(identity: &str, writable: bool) -> Connection {
    let path = std::env::var(format!(
        "K8BORG_TEST_{}_CONFIG",
        identity.to_ascii_uppercase()
    ))
    .unwrap_or_else(|_| panic!("run scripts/Test-DockerDesktop.ps1 first"));
    Connection {
        kubeconfig: Some(PathBuf::from(path)),
        context: Some("k8borg-test".into()),
        writable,
        logs: true,
        ..Default::default()
    }
}
#[test]
#[ignore = "requires isolated Docker Desktop fixtures; run scripts/Test-DockerDesktop.ps1"]
fn isolated_cluster_acceptance() {
    let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
    rt.block_on(async {
        let reader = Gateway::connect(options("reader", false))
            .await
            .unwrap_or_else(|e| panic!("reader connect: {e}"));
        let ns = reader.namespace.clone();
        let pods = reader
            .list("pods", &ns)
            .await
            .unwrap_or_else(|e| panic!("reader pods: {e}"));
        let items = pods["items"]
            .as_array()
            .unwrap_or_else(|| panic!("missing items"));
        assert!(!items.is_empty());
        assert!(reader.list("secrets", &ns).await.is_err());
        assert!(
            reader
                .prepare("deployments", &ns, "sample", Action::Scale(2))
                .await
                .is_err()
        );
        let config = reader
            .get("configmaps", &ns, "canary")
            .await
            .unwrap_or_else(|e| panic!("configmap: {e}"));
        assert!(!config.to_string().contains("K8BORG_TEST_CANARY"));
        let pod = items[0]["metadata"]["name"]
            .as_str()
            .unwrap_or_else(|| panic!("missing pod name"));
        let logs = reader
            .logs(&ns, pod, None, false)
            .await
            .unwrap_or_else(|e| panic!("logs: {e}"));
        assert!(logs.contains("K8Borg fixture"));
        assert!(!logs.chars().any(|c| c == '\x1b' || c == '\r'));
        let denied = Gateway::connect(options("denied", false))
            .await
            .unwrap_or_else(|e| panic!("denied connect: {e}"));
        assert!(denied.list("pods", &ns).await.is_err());
        let reader_write = Gateway::connect(options("reader", true))
            .await
            .unwrap_or_else(|e| panic!("reader write connect: {e}"));
        assert!(
            reader_write
                .prepare("deployments", &ns, "sample", Action::Scale(2))
                .await
                .is_err()
        );
        let mut operator = Gateway::connect(options("operator", true))
            .await
            .unwrap_or_else(|e| panic!("operator connect: {e}"));
        let scale = operator
            .prepare("deployments", &ns, "sample", Action::Scale(2))
            .await
            .unwrap_or_else(|e| panic!("scale dry run: {e}"));
        let approval = Approval::confirm(
            scale.target.clone(),
            scale.action.clone(),
            &format!("{ns}/sample"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        operator
            .execute(scale, approval)
            .await
            .unwrap_or_else(|e| panic!("scale: {e}"));
        assert_eq!(
            operator
                .get("deployments", &ns, "sample")
                .await
                .unwrap_or_else(|e| panic!("{e}"))["spec"]["replicas"],
            2
        );
        tokio::time::sleep(Duration::from_secs(5)).await;
        let stale = operator
            .prepare("deployments", &ns, "sample", Action::Scale(1))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let restart = operator
            .prepare("deployments", &ns, "sample", Action::Restart)
            .await
            .unwrap_or_else(|e| panic!("restart prepare: {e}"));
        let approval = Approval::confirm(
            restart.target.clone(),
            restart.action.clone(),
            &format!("{ns}/sample"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        operator
            .execute(restart, approval)
            .await
            .unwrap_or_else(|e| panic!("restart: {e}"));
        let approval = Approval::confirm(
            stale.target.clone(),
            stale.action.clone(),
            &format!("{ns}/sample"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(
            operator.execute(stale, approval).await.is_err(),
            "stale resourceVersion must be rejected"
        );
        tokio::time::sleep(Duration::from_secs(5)).await;
        let reset = operator
            .prepare("deployments", &ns, "sample", Action::Scale(1))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let approval = Approval::confirm(
            reset.target.clone(),
            reset.action.clone(),
            &format!("{ns}/sample"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        operator
            .execute(reset, approval)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let delete = operator
            .prepare("services", &ns, "delete-me", Action::Delete)
            .await
            .unwrap_or_else(|e| panic!("delete prepare: {e}"));
        let approval = Approval::confirm(
            delete.target.clone(),
            delete.action.clone(),
            &format!("{ns}/delete-me"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        operator
            .execute(delete, approval)
            .await
            .unwrap_or_else(|e| panic!("delete: {e}"));
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert!(operator.get("services", &ns, "delete-me").await.is_err());
    });
}
