//! Opt-in acceptance against an explicitly selected local cluster; fixtures are uniquely owned.
use futures_util::FutureExt;
use k8s_openapi::api::{apps::v1::Deployment, core::v1::Namespace};
use kborg::{
    Result,
    cli::runtime,
    gateway::{Connection, Gateway},
};
use kube::{
    Api, Client, Config,
    api::{DeleteParams, Patch, PatchParams, PostParams, Preconditions},
    config::{KubeConfigOptions, Kubeconfig},
};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[test]
#[ignore = "requires KBORG_YAML_TEST_KUBECONFIG and explicit Docker Desktop/minikube context"]
fn reviewed_yaml_dry_run_apply_redaction_and_stale_rejection() -> Result<()> {
    runtime()?.block_on(async {
        let path = PathBuf::from(std::env::var("KBORG_YAML_TEST_KUBECONFIG").map_err(|_| "explicit test kubeconfig missing")?);
        let context = std::env::var("KBORG_YAML_TEST_CONTEXT").map_err(|_| "explicit local test context missing")?;
        if !["docker-desktop", "kborg-v1"].contains(&context.as_str()) { return Err("YAML acceptance is limited to the authorized local contexts".into()); }
        let raw = Kubeconfig::read_from(&path).map_err(|_| "cannot read fixture kubeconfig")?;
        let config = Config::from_custom_kubeconfig(raw, &KubeConfigOptions { context:Some(context.clone()), ..Default::default() }).await.map_err(|_| "fixture configuration failed")?;
        let client = Client::try_from(config).map_err(|_| "fixture client failed")?;
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).map_err(|_| "clock")?.as_millis();
        let namespace = format!("kborg-yaml-{}-{stamp}", std::process::id());
        let namespaces: Api<Namespace> = Api::all(client.clone());
        let fixture: Namespace = serde_json::from_value(json!({"apiVersion":"v1","kind":"Namespace","metadata":{"name":namespace,"labels":{"kborg.dev/test":"yaml-v1"}}})).map_err(|_| "namespace fixture")?;
        let created = namespaces.create(&PostParams::default(), &fixture).await.map_err(|_| "namespace create failed")?;
        let uid = created.metadata.uid.ok_or("namespace UID missing")?;
        let result = std::panic::AssertUnwindSafe(async {
            let deployments: Api<Deployment> = Api::namespaced(client, &namespace);
            let deployment: Deployment = serde_json::from_value(json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"yaml-sample","namespace":namespace,"annotations":{"example.test/keep":"original"}},"spec":{"replicas":0,"selector":{"matchLabels":{"app":"yaml-sample"}},"template":{"metadata":{"labels":{"app":"yaml-sample"}},"spec":{"automountServiceAccountToken":false,"containers":[{"name":"sample","image":"nginx:1.27","env":[{"name":"PRIVATE","value":"yaml-existing-hidden-canary"}]}]}}}})).map_err(|_| "deployment fixture")?;
            deployments.create(&PostParams::default(), &deployment).await.map_err(|_| "deployment fixture create failed")?;
            tokio::time::sleep(Duration::from_secs(2)).await;
            let connection = Connection { kubeconfig: Some(path), context:Some(context.clone()), namespace:namespace.clone(), writable:true, allow_yaml_edit:true, ..Default::default() };
            let mut gateway = Gateway::connect(connection.clone()).await?;
            let readonly = Gateway::connect(Connection { writable:false, ..connection.clone() }).await?;
            let no_capability = Gateway::connect(Connection { allow_yaml_edit:false, ..connection }).await?;
            let draft = gateway.yaml_draft("deployments", &namespace, "yaml-sample").await?;
            assert!(!draft.text().contains("yaml-existing-hidden-canary"));
            let mut edited: Value = serde_saphyr::from_str(draft.text()).map_err(|_| "template decode failed")?;
            edited["spec"]["minReadySeconds"] = json!(5);
            let text = serde_json::to_string_pretty(&edited).map_err(|_| "template encode failed")?;
            assert!(readonly.prepare_yaml(&draft, &text).await.is_err());
            assert!(no_capability.prepare_yaml(&draft, &text).await.is_err());
            let prepared = gateway.prepare_yaml(&draft, &text).await?;
            assert!(!prepared.summary().contains("yaml-existing-hidden-canary"));
            assert_eq!(deployments.get("yaml-sample").await.map_err(|_| "dry-run verification")?.spec.and_then(|s|s.min_ready_seconds).unwrap_or(0), 0);
            gateway.execute_yaml(prepared, &format!("{namespace}/yaml-sample")).await?;
            let applied = deployments.get("yaml-sample").await.map_err(|_| "apply verification")?;
            assert_eq!(applied.metadata.annotations.as_ref().and_then(|a|a.get("example.test/keep")).map(String::as_str), Some("original"));
            let spec = applied.spec.ok_or("spec missing")?;
            assert_eq!(spec.min_ready_seconds, Some(5));
            assert_eq!(spec.template.spec.ok_or("pod spec missing")?.containers[0].env.as_ref().and_then(|env|env.first()).and_then(|env|env.value.as_deref()), Some("yaml-existing-hidden-canary"));
            tokio::time::sleep(Duration::from_secs(1)).await;
            let stale = gateway.yaml_draft("deployments", &namespace, "yaml-sample").await?;
            let mut edited: Value = serde_saphyr::from_str(stale.text()).map_err(|_| "template decode failed")?;
            edited["spec"]["minReadySeconds"] = json!(6);
            let text = serde_json::to_string(&edited).map_err(|_| "template encoding")?;
            let prepared = gateway.prepare_yaml(&stale, &text).await?;
            deployments.patch("yaml-sample", &PatchParams::default(), &Patch::Merge(json!({"metadata":{"annotations":{"example.test/concurrent":"changed"}}}))).await.map_err(|_| "concurrent fixture change")?;
            assert!(gateway.execute_yaml(prepared, &format!("{namespace}/yaml-sample")).await.is_err());
            assert!(gateway.prepare_yaml(&stale, &text).await.is_err());
            assert_eq!(deployments.get("yaml-sample").await.map_err(|_| "stale verification")?.spec.and_then(|s|s.min_ready_seconds), Some(5));
            println!("YAML acceptance {context}: dry-run, applied spec, hidden-field preservation, readonly/capability gates and concurrent-edit rejection passed");
            Ok::<_,String>(())
        }).catch_unwind().await.unwrap_or_else(|_| Err("YAML live assertion failed; fixture cleanup attempted".into()));
        let cleanup = namespaces.delete(&namespace, &DeleteParams { preconditions:Some(Preconditions { uid:Some(uid), resource_version:None }), ..Default::default() }).await.map_err(|_| "owned YAML namespace cleanup failed");
        cleanup?;
        result
    })
}
