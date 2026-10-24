//! YAML edit gateway tests.
use super::*;
#[test]
fn yaml_patch_contains_only_reviewed_paths_and_atomic_uid_version_tests() -> Result<()> {
    crate::cli::runtime()?.block_on(async {
        use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
        let stale = Arc::new(AtomicBool::new(false));
        let poisoned = Arc::new(AtomicBool::new(false));
        let patches = Arc::new(AtomicUsize::new(0));
        let (stale_server, poison_server, counted) = (stale.clone(), poisoned.clone(), patches.clone());
        let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
            let stale = stale_server.load(Ordering::SeqCst);
            let poison = poison_server.load(Ordering::SeqCst);
            let counted = counted.clone();
            async move {
                use http_body_util::BodyExt;
                assert_eq!(request.uri().path(), "/apis/apps/v1/namespaces/test/deployments/api");
                let mut response = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"api","namespace":"test","uid":"u1","resourceVersion":if stale {"2"} else {"1"}},"spec":{"replicas":1,"template":{"spec":{"containers":[{"name":"api","image":"nginx:1","env":[{"name":"TOKEN","value":"private-yaml-canary"}]}]}}}});
                let mut status = http::StatusCode::OK;
                if request.method() == http::Method::PATCH {
                    counted.fetch_add(1, Ordering::SeqCst);
                    let dry = request.uri().query().is_some_and(|q| q.contains("dryRun=All"));
                    assert!(request.uri().query().is_some_and(|q| q.contains("fieldValidation=Strict")));
                    assert_eq!(request.headers()["content-type"], "application/json-patch+json");
                    let bytes = request.into_body().collect().await.unwrap_or_else(|e| panic!("{e}")).to_bytes();
                    let patch: Value = serde_json::from_slice(&bytes).unwrap_or_else(|e| panic!("{e}"));
                    assert_eq!(patch, json!([
                        {"op":"test","path":"/metadata/uid","value":"u1"},
                        {"op":"test","path":"/metadata/resourceVersion","value":"1"},
                        {"op":"replace","path":"/spec/replicas","value":2}
                    ]));
                    assert!(!String::from_utf8_lossy(&bytes).contains("private-yaml-canary"));
                    if dry {
                        response["spec"]["replicas"] = json!(2);
                        if poison { response["spec"]["template"]["spec"]["containers"][0]["image"] = json!("unreviewed:1"); }
                    } else {
                        status = http::StatusCode::CONFLICT;
                        response = json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Conflict","code":409});
                    }
                }
                Ok::<_,std::convert::Infallible>(http::Response::builder().status(status).body(http_body_util::Full::new(bytes::Bytes::from(response.to_string()))).unwrap_or_else(|e| panic!("{e}")))
            }
        });
        let mut gateway = Gateway::for_test_client(Client::new(service, "test"), "test");
        gateway.policy.writable = true;
        gateway.allow_yaml_edit = true;
        let draft = gateway.yaml_draft("deployments", "test", "api").await?;
        let edited = draft.text().replace("\"replicas\": 1", "\"replicas\": 2");
        let prepared = gateway.prepare_yaml(&draft, &edited).await?;
        assert!(!prepared.summary().contains("private-yaml-canary"));
        assert!(gateway.perform_yaml(&prepared, false).await.is_err());
        assert!(gateway.execute_yaml(prepared, "test/wrong").await.is_err());
        poisoned.store(true, Ordering::SeqCst);
        assert!(gateway.prepare_yaml(&draft, &edited).await.is_err());
        stale.store(true, Ordering::SeqCst);
        let previous_count = patches.load(Ordering::SeqCst);
        assert!(gateway.prepare_yaml(&draft, &edited).await.is_err());
        assert_eq!(patches.load(Ordering::SeqCst), previous_count);
        Ok(())
    })
}
#[test]
fn yaml_demo_requires_separate_capability_and_expires_reviews() -> Result<()> {
    crate::cli::runtime()?.block_on(async {
        let mut gateway = Gateway::connect(Connection {
            demo: true,
            writable: true,
            ..Default::default()
        })
        .await?;
        let draft = gateway.yaml_draft("deployments", "demo", "api").await?;
        let mut edited: Value = serde_saphyr::from_str(draft.text()).map_err(|_| "fixture YAML")?;
        edited["spec"]["replicas"] = json!(4);
        let text = serde_json::to_string(&edited).map_err(|_| "fixture JSON")?;
        assert!(gateway.prepare_yaml(&draft, &text).await.is_err());
        gateway.allow_yaml_edit = true;
        let mut expired = gateway.prepare_yaml(&draft, &text).await?;
        expired.created = std::time::Instant::now() - Duration::from_secs(61);
        assert!(gateway.execute_yaml(expired, "demo/api").await.is_err());
        let prepared = gateway.prepare_yaml(&draft, &text).await?;
        gateway.execute_yaml(prepared, "demo/api").await?;
        assert_eq!(
            gateway.get("deployments", "demo", "api").await?["spec"]["replicas"],
            4
        );
        assert!(gateway.prepare_yaml(&draft, &text).await.is_err());
        Ok(())
    })
}
