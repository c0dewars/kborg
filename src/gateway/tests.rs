//! Gateway behaviour tests.
use super::*;
use crate::cli::runtime;
type LogRequests = std::sync::Arc<std::sync::Mutex<Vec<String>>>;
fn log_capture_gateway(replies: Vec<(u16, Value)>) -> (Gateway, LogRequests) {
    let replies = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
        replies,
    )));
    let requests: LogRequests = Default::default();
    let recorded = requests.clone();
    let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
        recorded
            .lock()
            .unwrap_or_else(|e| panic!("{e}"))
            .push(request.uri().to_string());
        let (status, value) = replies
            .lock()
            .unwrap_or_else(|e| panic!("{e}"))
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected workload log request"));
        async move {
            Ok::<_, std::convert::Infallible>(
                http::Response::builder()
                    .status(status)
                    .body(http_body_util::Full::new(bytes::Bytes::from(
                        value.to_string(),
                    )))
                    .unwrap_or_else(|e| panic!("{e}")),
            )
        }
    });
    let mut gateway = Gateway::for_test_client(Client::new(service, "default"), "default");
    gateway.logs = true;
    (gateway, requests)
}
fn log_capture_pod(name: &str, uid: &str) -> Value {
    json!({"apiVersion":"v1","kind":"Pod","metadata":{"namespace":"default","name":name,"uid":uid,"labels":{"app":"web","team":"prod"}},"spec":{"containers":[{"name":"main","image":"fixture"},{"name":"sidecar","image":"fixture"}],"initContainers":[{"name":"init","image":"fixture"}]}})
}
#[test]
fn workload_logs_capture_real_selector_complete_membership_and_connection_identity() -> Result<()> {
    runtime()?.block_on(async {
        let owner=json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"namespace":"default","name":"different-than-label","uid":"deployment-uid"},"spec":{"selector":{"matchLabels":{"app":"web"},"matchExpressions":[{"key":"team","operator":"In","values":["prod"]}]}}});
        let pods=json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"10"},"items":[log_capture_pod("one","uid-one"),log_capture_pod("two","uid-two")]});
        let (mut gateway,requests)=log_capture_gateway(vec![(200,owner),(200,pods)]);
        let membership=gateway.capture_log_membership("default",crate::logs::PodLogSelection::Workload{resource:"deployments.apps".into(),name:"different-than-label".into(),uid:"deployment-uid".into()}).await?;
        assert_eq!(membership.pods().len(),2);assert_eq!(membership.source_count(),4);
        assert!(membership.pods().iter().all(|pod|pod.containers==["main","sidecar"]));
        {let requests=requests.lock().map_err(|_|"requests")?;assert_eq!(requests.len(),2);assert!(requests[1].contains("labelSelector="));assert!(requests[1].contains("web")&&requests[1].contains("team")&&requests[1].contains("prod"));assert!(!requests[1].contains("different-than-label"));}
        gateway.session="reconnected".into();
        assert!(gateway.membership_logs(&membership,false,Default::default()).await.is_err_and(|error|error.contains("another connection")));
        assert_eq!(requests.lock().map_err(|_|"requests")?.len(),2);
        Ok(())
    })
}
#[test]
fn workload_logs_reject_partial_foreign_namespace_selector_escapes_and_stale_workload() -> Result<()>
{
    runtime()?.block_on(async {
        for variant in 0..4 {
            let mut pod=log_capture_pod("one","uid-one");
            if variant==0{pod["metadata"]["namespace"]=json!("foreign");}
            if variant==1{pod["metadata"]["labels"]["app"]=json!("other");}
            let mut list=json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"10"},"items":[pod]});
            if variant==2{list["metadata"]["continue"]=json!("more");}
            let response=if variant==3{(403,json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":403,"reason":"Forbidden","message":"CANARY private diagnostic"}))}else{(200,list)};
            let (gateway,requests)=log_capture_gateway(vec![response]);
            let error=gateway.capture_log_membership("default",crate::logs::PodLogSelection::Selector("app=web".into())).await.err().ok_or("invalid or partial log scope accepted")?;
            assert!(!error.contains("CANARY"));assert_eq!(requests.lock().map_err(|_|"requests")?.len(),1);
        }
        let owner=json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"namespace":"default","name":"web","uid":"replaced"},"spec":{"selector":{"matchLabels":{"app":"web"}}}});
        let (gateway,requests)=log_capture_gateway(vec![(200,owner)]);
        assert!(gateway.capture_log_membership("default",crate::logs::PodLogSelection::Workload{resource:"deployments.apps".into(),name:"web".into(),uid:"captured".into()}).await.is_err());
        assert_eq!(requests.lock().map_err(|_|"requests")?.len(),1);
        let (mut gateway,requests)=log_capture_gateway(Vec::new());gateway.logs=false;
        assert!(gateway.capture_log_membership("default",crate::logs::PodLogSelection::Selector("app=web".into())).await.is_err_and(|error|error.contains("--allow-logs")));
        gateway.logs=true;
        assert!(gateway.capture_log_membership("*",crate::logs::PodLogSelection::Selector("app=web".into())).await.is_err());
        assert!(gateway.capture_log_membership("default",crate::logs::PodLogSelection::Selector(String::new())).await.is_err());
        gateway.kinds.push(Kind{plural:"deployments".into(),kind:"Deployment".into(),group:"example.io".into(),version:"v1".into(),namespaced:true,short_names:Vec::new()});
        assert!(gateway.capture_log_membership("default",crate::logs::PodLogSelection::Workload{resource:"deployments.example.io".into(),name:"web".into(),uid:"uid".into()}).await.is_err());
        assert!(requests.lock().map_err(|_|"requests")?.is_empty());
        Ok(())
    })
}
#[test]
fn cronjob_logs_follow_controller_uid_chain_and_exclude_unrelated_matching_pods() -> Result<()> {
    runtime()?.block_on(async {
        let owner=json!({"apiVersion":"batch/v1","kind":"CronJob","metadata":{"namespace":"default","name":"nightly","uid":"cron-uid"},"spec":{}});
        let job=|name:&str,uid:&str,cron:&str|json!({"apiVersion":"batch/v1","kind":"Job","metadata":{"namespace":"default","name":name,"uid":uid,"ownerReferences":[{"apiVersion":"batch/v1","kind":"CronJob","name":"nightly","uid":cron,"controller":true}]},"spec":{"selector":{"matchLabels":{"app":"web"}},"template":{"spec":{"containers":[{"name":"main","image":"fixture"}],"restartPolicy":"Never"}}}});
        let jobs=json!({"apiVersion":"batch/v1","kind":"JobList","metadata":{"resourceVersion":"10"},"items":[job("owned","job-uid","cron-uid"),job("other","other-job","different-cron")]});
        let mut related=log_capture_pod("owned-pod","owned-pod-uid");related["metadata"]["ownerReferences"]=json!([{"apiVersion":"batch/v1","kind":"Job","name":"owned","uid":"job-uid","controller":true}]);
        let mut unrelated=log_capture_pod("unrelated","other-pod-uid");unrelated["metadata"]["ownerReferences"]=json!([{"apiVersion":"batch/v1","kind":"Job","name":"other","uid":"other-job","controller":true}]);
        let pods=json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"11"},"items":[related,unrelated]});
        let (gateway,requests)=log_capture_gateway(vec![(200,owner),(200,jobs),(200,pods)]);
        let membership=gateway.capture_log_membership("default",crate::logs::PodLogSelection::Workload{resource:"cronjobs.batch".into(),name:"nightly".into(),uid:"cron-uid".into()}).await?;
        assert_eq!(membership.pods().len(),1);assert_eq!(membership.pods()[0].name,"owned-pod");assert_eq!(membership.parents.len(),2);
        assert_eq!(membership.parents[1].uid,"job-uid");assert_eq!(requests.lock().map_err(|_|"requests")?.len(),3);
        Ok(())
    })
}
#[test]
fn merged_logs_gate_before_pod_reads_and_reject_unbound_identity() -> Result<()> {
    runtime()?.block_on(async {
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let recorded = calls.clone();
        let service = tower::service_fn(move |_: http::Request<kube::client::Body>| {
            recorded.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async {
                Ok::<_, std::convert::Infallible>(http::Response::new(http_body_util::Full::new(
                    bytes::Bytes::new(),
                )))
            }
        });
        let mut gateway = Gateway::for_test_client(Client::new(service, "default"), "default");
        assert!(
            gateway
                .merged_logs("default", "app", "uid", false, Default::default())
                .await
                .is_err_and(|error| error.contains("--allow-logs"))
        );
        assert!(
            gateway
                .follow_merged_logs(
                    "default",
                    "app",
                    "uid",
                    Default::default(),
                    Default::default()
                )
                .await
                .is_err_and(|error| error.contains("--allow-logs"))
        );
        gateway.logs = true;
        assert!(
            gateway
                .merged_logs("default", "app", "", false, Default::default())
                .await
                .is_err()
        );
        assert!(
            gateway
                .follow_merged_logs("*", "app", "uid", Default::default(), Default::default())
                .await
                .is_err()
        );
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 0);
        Ok(())
    })
}
#[test]
fn log_ranges_reach_the_api_and_unbound_reads_keep_log_only_permissions() -> Result<()> {
    runtime()?.block_on(async {
        let requests = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = requests.clone();
        let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
            recorded
                .lock()
                .unwrap_or_else(|e| panic!("{e}"))
                .push(request.uri().to_string());
            async move {
                Ok::<_, std::convert::Infallible>(http::Response::new(http_body_util::Full::new(
                    bytes::Bytes::from_static(b"first\nsecond\nthird\nfourth\n"),
                )))
            }
        });
        let mut gateway = Gateway::for_test_client(Client::new(service, "default"), "default");
        assert!(gateway.logs("default", "app", None, false).await.is_err());
        assert!(requests.lock().unwrap_or_else(|e| panic!("{e}")).is_empty());
        gateway.logs = true;
        let range = crate::logs::ReadOptions {
            tail_lines: 2,
            since_seconds: Some(60),
            head: false,
        };
        assert_eq!(
            gateway
                .logs_with_options("default", "app", Some("main".into()), false, range)
                .await?,
            "third\nfourth\n"
        );
        let head = crate::logs::ReadOptions {
            tail_lines: 2,
            head: true,
            ..Default::default()
        };
        let mut feed = gateway
            .follow_logs_with_read_options("default", "app", None, false, Default::default(), head)
            .await?;
        assert_eq!(feed.next().await?, Some("first".into()));
        assert_eq!(feed.next().await?, Some("second".into()));
        assert_eq!(feed.next().await?, None);
        let requests = requests.lock().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(requests.len(), 2); // No additional Pod GET for plain logs.
        assert!(requests[0].contains("tailLines=2"));
        assert!(requests[0].contains("sinceSeconds=60"));
        assert!(requests[0].contains("container=main"));
        assert!(requests[0].contains("timestamps=true"));
        assert!(requests[1].contains("limitBytes=262144"));
        assert!(!requests[1].contains("tailLines"));
        assert!(!requests[1].contains("follow=true"));
        Ok(())
    })
}
#[test]
fn offline_resource_edit_is_sealed_and_preserves_container_fields() -> Result<()> {
    runtime()?.block_on(async {
        let mut gateway = Gateway::connect(Connection {
            demo: true,
            writable: true,
            ..Default::default()
        })
        .await?;
        let action = Action::SetResources {
            container: "app".into(),
            resources: crate::edit::ResourceEdit {
                cpu_request: "100m".into(),
                memory_request: "64Mi".into(),
                cpu_limit: "500m".into(),
                memory_limit: "128Mi".into(),
            },
        };
        let prepared = gateway
            .prepare("deployments", "demo", "api", action)
            .await?;
        assert!(prepared.summary.contains("+ requests.cpu: 100m"));
        let approval =
            Approval::confirm(prepared.target.clone(), prepared.action.clone(), "demo/api")?;
        gateway.execute(prepared, approval).await?;
        let object = gateway.get("deployments", "demo", "api").await?;
        assert_eq!(
            object["spec"]["template"]["spec"]["containers"][0]["image"],
            "example/demo:v1"
        );
        assert_eq!(
            object["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]["memory"],
            "128Mi"
        );
        Ok(())
    })
}
#[test]
fn summaries_report_init_readiness_deletion_and_node_health() {
    let pod =
        json!({"kind":"Pod","status":{"phase":"Running","containerStatuses":[{"ready":false}]}});
    assert_eq!(summary(&pod), "NotReady");
    assert_eq!(
        summary(
            &json!({"kind":"Pod","status":{"phase":"Running","containerStatuses":[{"ready":true}],"conditions":[{"type":"Ready","status":"False"}]}})
        ),
        "NotReady"
    );
    assert_eq!(
        summary(&json!({"kind":"Pod","status":{"phase":"Succeeded"}})),
        "Completed"
    );
    let mut init = pod.clone();
    init["status"]["initContainerStatuses"] =
        json!([{"name":"init","state":{"waiting":{"reason":"ImagePullBackOff"}}}]);
    assert_eq!(summary(&init), "Init:ImagePullBackOff");
    init["metadata"]["deletionTimestamp"] = json!("2026-10-10T08:00:00Z");
    assert_eq!(summary(&init), "Terminating");
    let node = json!({"kind":"Node","spec":{"unschedulable":true},"status":{"conditions":[{"type":"Ready","status":"False"}]}});
    assert_eq!(summary(&node), "NotReady,SchedulingDisabled");
    let hostile = json!({"kind":"Event","reason":"bad\u{001b}[2J\u{202e}"});
    assert_eq!(summary(&hostile), "bad[2J");
}
#[test]
fn demo_covers_every_browsable_kind_with_correct_scope_and_version() -> Result<()> {
    runtime()?.block_on(async {
        let gateway = Gateway::connect(Connection {
            demo: true,
            ..Default::default()
        })
        .await?;
        for kind in kinds() {
            let list = gateway.list(&kind.key(), "demo").await?;
            let items = list["items"].as_array().ok_or("demo list missing")?;
            assert!(!items.is_empty(), "demo missing {}", kind.kind);
            for item in items {
                assert_eq!(item["apiVersion"], kind.ar().api_version);
                assert_eq!(item["metadata"]["namespace"].is_string(), kind.namespaced);
            }
        }
        Ok(())
    })
}
#[test]
fn offline_image_edit_is_reviewed_and_updates_the_sample() {
    runtime().unwrap_or_else(|e| panic!("{e}")).block_on(async {
        let mut gateway = Gateway::connect(Connection {
            demo: true,
            writable: true,
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        let reviewed = gateway
            .prepare(
                "deployments",
                "demo",
                "api",
                Action::SetImage {
                    container: "app".into(),
                    image: "example/demo:v2".into(),
                },
            )
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let approval =
            Approval::confirm(reviewed.target.clone(), reviewed.action.clone(), "demo/api")
                .unwrap_or_else(|e| panic!("{e}"));
        gateway
            .execute(reviewed, approval)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let object = gateway
            .get("deployments", "demo", "api")
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            object["spec"]["template"]["spec"]["containers"][0]["image"],
            "example/demo:v2"
        );
    });
}
#[test]
fn offline_details_and_logs_respect_exact_namespace_and_target() {
    runtime().unwrap_or_else(|e| panic!("{e}")).block_on(async {
        let gateway = Gateway::connect(Connection {
            demo: true,
            logs: true,
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(gateway.get("pods", "demo", "api-7d4f9").await.is_ok());
        assert!(gateway.get("pods", "other", "api-7d4f9").await.is_err());
        assert!(gateway.get("pods", "*", "api-7d4f9").await.is_err());
        assert!(gateway.logs("demo", "api-7d4f9", None, false).await.is_ok());
        assert!(
            gateway
                .logs("other", "api-7d4f9", None, false)
                .await
                .is_err()
        );
        assert!(gateway.logs("demo", "missing", None, false).await.is_err());
        let scoped = Gateway::connect(Connection {
            demo: true,
            namespace: "other".into(),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(scoped.namespace, "other");
        assert!(
            scoped
                .get("pods", &scoped.namespace, "api-7d4f9")
                .await
                .is_err()
        );
        assert!(
            scoped
                .list("pods", &scoped.namespace)
                .await
                .unwrap_or_else(|e| panic!("{e}"))["items"]
                .as_array()
                .is_some_and(Vec::is_empty)
        );
    });
}
#[test]
fn malicious_auth_and_insecure_tls_are_rejected_before_network() {
    let dir = std::env::temp_dir()
        .canonicalize()
        .unwrap_or_else(|e| panic!("test temporary root: {e}"))
        .join(format!("kborg-security-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
    for (i, user, cluster) in [
        (
            0,
            json!({"exec":{"command":"evil","apiVersion":"client.authentication.k8s.io/v1"}}),
            json!({"server":"https://127.0.0.1:1"}),
        ),
        (
            1,
            json!({"token":"FAKE"}),
            json!({"server":"https://127.0.0.1:1","insecure-skip-tls-verify":true}),
        ),
        (
            2,
            json!({"token":"FAKE","as":"administrator"}),
            json!({"server":"https://127.0.0.1:1"}),
        ),
        (
            3,
            json!({"token":"FAKE"}),
            json!({"server":"https://127.0.0.1:1/?access_token=CANARY"}),
        ),
    ] {
        let path = dir.join(format!("{i}.json"));
        let config = json!({"apiVersion":"v1","kind":"Config","current-context":"test","clusters":[{"name":"test","cluster":cluster}],"users":[{"name":"test","user":user}],"contexts":[{"name":"test","context":{"cluster":"test","user":"test","namespace":"default"}}]});
        std::fs::write(&path, config.to_string()).unwrap_or_else(|e| panic!("{e}"));
        let result = rt.block_on(Gateway::connect(Connection {
            kubeconfig: Some(path.clone()),
            ..Default::default()
        }));
        assert!(result.is_err());
        std::fs::remove_file(path).unwrap_or_else(|e| panic!("{e}"));
    }
    std::fs::remove_dir(dir).unwrap_or_else(|e| panic!("{e}"));
}
#[test]
fn oversized_api_body_fails_before_json_decode() {
    let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
    rt.block_on(async {
        let service = tower::service_fn(|_request: http::Request<kube::client::Body>| async {
            let body = http_body_util::Full::new(bytes::Bytes::from(vec![b' '; MAX_BODY + 1]));
            Ok::<_, std::convert::Infallible>(http::Response::new(http_body_util::Limited::new(
                body, MAX_BODY,
            )))
        });
        let client = Client::new(service, "default");
        let request = http::Request::get("/api/v1/pods")
            .body(vec![])
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(client.request::<Value>(request).await.is_err());
    });
}
#[test]
fn demo_never_reads_missing_credentials_and_readonly_fails_closed() {
    let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
    rt.block_on(async {
        let g = Gateway::connect(Connection {
            demo: true,
            kubeconfig: Some(PathBuf::from("missing-file")),
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(g.list("pods", "demo").await.is_ok());
        assert!(g.list("secrets", "demo").await.is_err());
        assert!(
            g.prepare("deployments", "demo", "api", Action::Restart)
                .await
                .is_err()
        );
    });
}
#[test]
fn kubeconfig_files_merge_first_wins_and_skip_missing_entries() -> Result<()> {
    let root = std::env::temp_dir()
        .canonicalize()
        .map_err(|_| "temp")?
        .join(format!("kborg-kubeconfig-merge-{}", std::process::id()));
    safety::ensure_private_dir(&root)?;
    let file = |name: &str, current: &str, server: &str, extra: &str| -> Result<PathBuf> {
        let path = root.join(name);
        let text = format!(
            "apiVersion: v1\nkind: Config\ncurrent-context: {current}\nclusters:\n- name: shared\n  cluster:\n    server: {server}\n    certificate-authority: ca.pem\n- name: {extra}\n  cluster:\n    server: https://{extra}.example\ncontexts:\n- name: shared\n  context:\n    cluster: shared\n    user: u\n- name: {extra}\n  context:\n    cluster: {extra}\n    user: u\nusers:\n- name: u\n  user:\n    token: t\n"
        );
        let _ = std::fs::remove_file(&path);
        safety::write_new(&path, text.as_bytes())?;
        Ok(path)
    };
    let first = file("a.yaml", "shared", "https://first.example", "east")?;
    let second = file("b.yaml", "west", "https://second.example", "west")?;
    let merged = merge_config_files(vec![first, root.join("missing.yaml"), second])?;
    assert_eq!(merged.current_context.as_deref(), Some("shared"));
    let names = merged
        .contexts
        .iter()
        .map(|c| c.name.as_str())
        .collect::<Vec<_>>();
    assert_eq!(names, ["shared", "east", "west"]);
    let shared = merged
        .clusters
        .iter()
        .find(|c| c.name == "shared")
        .and_then(|c| c.cluster.as_ref())
        .ok_or("shared cluster")?;
    assert_eq!(shared.server.as_deref(), Some("https://first.example"));
    assert_eq!(
        shared.certificate_authority.as_deref(),
        Some(root.join("ca.pem").to_string_lossy().as_ref())
    );
    assert!(merge_config_files(vec![root.join("missing.yaml"), root.join("gone.yaml")]).is_err());
    assert!(merge_config_files(vec![root.join("only-missing.yaml")]).is_err());
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
#[test]
fn transport_failures_name_a_cause_without_echoing_raw_text() {
    let refused = std::io::Error::new(
        std::io::ErrorKind::ConnectionRefused,
        "tcp connect error: Connection refused (os error 61) to https://10.0.0.1:6443?token=abc",
    );
    let hint = transport_hint(&refused);
    assert!(hint.contains("connection refused") && !hint.contains("token"));
    let timeout = std::io::Error::new(std::io::ErrorKind::TimedOut, "operation timed out");
    assert!(transport_hint(&timeout).contains("did not answer in time"));
    let other = std::io::Error::other("weird");
    assert_eq!(
        transport_hint(&other),
        "Kubernetes transport or response failure"
    );
}
#[test]
fn force_kill_is_pod_only_opt_in_and_uses_zero_grace() {
    let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
    rt.block_on(async {
        let mut g = Gateway::connect(Connection {
            demo: true,
            writable: true,
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        let name = "api-7d4f9".to_owned();
        assert!(g.get("pods", "demo", &name).await.is_ok());
        assert!(
            g.prepare("pods", "demo", &name, Action::Kill)
                .await
                .is_err()
        );
        g.allow_force_kill = true;
        assert!(
            g.prepare("deployments", "demo", "api", Action::Kill)
                .await
                .is_err()
        );
        let prepared = g
            .prepare("pods", "demo", &name, Action::Kill)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(prepared.summary.contains("FORCE KILL"));
        assert_eq!(
            Gateway::delete_params(&prepared, false).grace_period_seconds,
            Some(0)
        );
        let approval = Approval::confirm(
            prepared.target.clone(),
            prepared.action.clone(),
            &format!("demo/{name}"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        g.execute(prepared, approval)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(g.get("pods", "demo", &name).await.is_err());
    });
}
#[test]
fn prepared_review_cannot_be_changed_after_dry_run() {
    let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
    rt.block_on(async {
        let mut g = Gateway::connect(Connection {
            demo: true,
            writable: true,
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        let mut prepared = g
            .prepare("deployments", "demo", "api", Action::Scale(2))
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        prepared.action = Action::Delete;
        let approval =
            Approval::confirm(prepared.target.clone(), prepared.action.clone(), "demo/api")
                .unwrap_or_else(|e| panic!("{e}"));
        assert!(g.execute(prepared, approval).await.is_err());
        assert!(g.get("deployments", "demo", "api").await.is_ok());
    });
}
#[test]
fn all_operation_capabilities_require_write_and_safe_namespace() {
    let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
    rt.block_on(async {
        let mut gateway = Gateway::connect(Connection {
            demo: true,
            writable: true,
            allow_exec: true,
            allow_attach: true,
            allow_port_forward: true,
            ..Default::default()
        })
        .await
        .unwrap_or_else(|e| panic!("{e}"));
        let service = tower::service_fn(|_: http::Request<kube::client::Body>| async {
            Ok::<_, std::convert::Infallible>(http::Response::new(http_body_util::Full::new(
                bytes::Bytes::new(),
            )))
        });
        gateway.client = Some(Client::new(service, "test"));
        for capability in 0..3 {
            assert!(gateway.operation_api("test", capability).is_ok());
            for namespace in ["", "*", "kube-system", "kube-public", "kube-node-lease"] {
                assert!(gateway.operation_api(namespace, capability).is_err());
            }
            gateway.policy.writable = false;
            assert!(gateway.operation_api("test", capability).is_err());
            gateway.policy.writable = true;
            gateway.operation_capabilities[capability] = false;
            assert!(gateway.operation_api("test", capability).is_err());
            gateway.operation_capabilities[capability] = true;
        }
    });
}

#[test]
fn image_edit_uses_narrow_patch_preconditions_and_rejects_stale_server_state() {
    let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
    rt.block_on(async {
        let service = tower::service_fn(|request: http::Request<kube::client::Body>| async move {
            use http_body_util::BodyExt;
            let object = json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"api","namespace":"test","uid":"old-uid","resourceVersion":"7"},"spec":{"replicas":1,"template":{"spec":{"containers":[{"name":"api","image":"nginx:1","env":[{"name":"TOKEN","value":"private-canary"}]}]}}}});
            let patching = request.method() == http::Method::PATCH;
            let dry = request.uri().query().is_some_and(|q| q.contains("dryRun=All"));
            let mut response = object;
            let mut status = http::StatusCode::OK;
            if patching {
                assert_eq!(request.headers()["content-type"], "application/strategic-merge-patch+json");
                let body = request.into_body().collect().await.unwrap_or_else(|e| panic!("{e}")).to_bytes();
                let patch: Value = serde_json::from_slice(&body).unwrap_or_else(|e| panic!("{e}"));
                assert_eq!(patch["metadata"]["uid"], "old-uid");
                assert_eq!(patch["metadata"]["resourceVersion"], "7");
                assert!(!String::from_utf8_lossy(&body).contains("private-canary"));
                assert_eq!(patch["spec"]["template"]["spec"]["containers"], json!([{"name":"api","image":"nginx:2"}]));
                if dry { response["spec"]["template"]["spec"]["containers"][0]["image"] = json!("nginx:2"); }
                else {
                    status = http::StatusCode::CONFLICT;
                    response = json!({"apiVersion":"v1","kind":"Status","status":"Failure","message":"stale UID/resourceVersion","reason":"Conflict","code":409});
                }
            }
            Ok::<_, std::convert::Infallible>(http::Response::builder().status(status).body(http_body_util::Full::new(bytes::Bytes::from(response.to_string()))).unwrap_or_else(|e| panic!("{e}")))
        });
        let mut gateway = Gateway::connect(Connection { demo: true, writable: true, ..Default::default() }).await.unwrap_or_else(|e| panic!("{e}"));
        gateway.client = Some(Client::new(service, "test"));
        let prepared = gateway.prepare("deployments", "test", "api", Action::SetImage { container: "api".into(), image: "nginx:2".into() }).await.unwrap_or_else(|e| panic!("{e}"));
        assert!(prepared.summary.contains("- nginx:1\n+ nginx:2"));
        assert!(!prepared.summary.contains("private-canary"));
        assert!(gateway.perform(&prepared, false).await.is_err());
    });
}

#[test]
fn atomic_container_edit_uses_workload_api_and_rejects_unreviewed_admission() -> Result<()> {
    runtime()?.block_on(async {
        use std::sync::{Arc, atomic::{AtomicBool, AtomicUsize, Ordering}};
        for (kind, resource, group, path) in [
            ("Deployment", "deployments", "apps", "/spec/template/spec/containers"),
            ("StatefulSet", "statefulsets", "apps", "/spec/template/spec/containers"),
            ("DaemonSet", "daemonsets", "apps", "/spec/template/spec/containers"),
            ("CronJob", "cronjobs", "batch", "/spec/jobTemplate/spec/template/spec/containers"),
        ] {
            let containers = json!([
                {"name":"api","image":"nginx:1","env":[{"name":"TOKEN","value":"private-canary"}],"resources":{"limits":{"example.com/gpu":"1"}}},
                {"name":"sidecar","image":"example/sidecar:1"}
            ]);
            let spec = if kind == "CronJob" {
                json!({"schedule":"0 2 * * *","jobTemplate":{"spec":{"template":{"spec":{"containers":containers}}}}})
            } else {
                json!({"template":{"spec":{"containers":containers}}})
            };
            let original = json!({"apiVersion":format!("{group}/v1"),"kind":kind,"metadata":{"name":"api","namespace":"test","uid":"uid-1","resourceVersion":"7"},"spec":spec});
            let expected_uri = format!("/apis/{group}/v1/namespaces/test/{resource}/api");
            let poison = Arc::new(AtomicBool::new(false));
            let poison_response = poison.clone();
            let requests = Arc::new(AtomicUsize::new(0));
            let counted = requests.clone();
            let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
                let mut response = original.clone();
                let expected_uri = expected_uri.clone();
                let poison_response = poison_response.clone();
                counted.fetch_add(1, Ordering::SeqCst);
                async move {
                    use http_body_util::BodyExt;
                    assert_eq!(request.uri().path(), expected_uri);
                    if request.method() == http::Method::PATCH {
                        assert_eq!(request.headers()["content-type"], "application/strategic-merge-patch+json");
                        assert!(request.uri().query().is_some_and(|query| query.contains("dryRun=All")));
                        let body = request.into_body().collect().await.unwrap_or_else(|error| panic!("{error}")).to_bytes();
                        let patch: Value = serde_json::from_slice(&body).unwrap_or_else(|error| panic!("{error}"));
                        assert_eq!(patch["metadata"], json!({"uid":"uid-1","resourceVersion":"7"}));
                        assert!(!String::from_utf8_lossy(&body).contains("private-canary"));
                        let patch_containers = patch.pointer(path).unwrap_or_else(|| panic!("missing reviewed container patch"));
                        assert_eq!(patch_containers, &json!([{"name":"api","image":"nginx:2","resources":{"requests":{"cpu":"100m","memory":"64Mi"},"limits":{"cpu":"500m","memory":"128Mi"}}}]));
                        let selected = response.pointer_mut(path).unwrap_or_else(|| panic!("missing mock containers"));
                        selected[0]["image"] = json!("nginx:2");
                        selected[0]["resources"]["requests"] = json!({"cpu":"0.1","memory":"64Mi"});
                        selected[0]["resources"]["limits"]["cpu"] = json!("500m");
                        selected[0]["resources"]["limits"]["memory"] = json!("128Mi");
                        if poison_response.load(Ordering::SeqCst) {
                            selected[1]["image"] = json!("unreviewed:2");
                        }
                    }
                    Ok::<_, std::convert::Infallible>(http::Response::new(http_body_util::Full::new(bytes::Bytes::from(response.to_string()))))
                }
            });
            let mut gateway = Gateway::connect(Connection { demo:true, writable:true, ..Default::default() }).await?;
            gateway.client = Some(Client::new(service, "test"));
            let action = Action::SetContainer {
                container:"api".into(),
                edit:crate::edit::ContainerEdit {
                    image:"nginx:2".into(),
                    resources:Some(crate::edit::ResourceEdit { cpu_request:"100m".into(),memory_request:"64Mi".into(),cpu_limit:"500m".into(),memory_limit:"128Mi".into() }),
                },
            };
            let reviewed = gateway.prepare(resource,"test","api",action.clone()).await?;
            assert!(reviewed.summary.contains("+ nginx:2"));
            assert!(!reviewed.summary.contains("private-canary"));
            assert_eq!(requests.load(Ordering::SeqCst),2);
            poison.store(true, Ordering::SeqCst);
            assert!(gateway.prepare(resource,"test","api",action).await.is_err());
            assert_eq!(requests.load(Ordering::SeqCst),4);
        }
        Ok(())
    })
}

#[test]
fn repeated_pagination_token_stops_before_an_unbounded_loop() {
    let rt = runtime().unwrap_or_else(|e| panic!("{e}"));
    rt.block_on(async {
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = requests.clone();
        let service = tower::service_fn(move |_request: http::Request<kube::client::Body>| {
            count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            async {
                let body = http_body_util::Full::new(bytes::Bytes::from_static(
                    br#"{"apiVersion":"v1","kind":"PodList","metadata":{"continue":"repeated"},"items":[]}"#,
                ));
                Ok::<_, std::convert::Infallible>(http::Response::new(body))
            }
        });
        let mut g = Gateway::connect(Connection { demo: true, ..Default::default() })
            .await.unwrap_or_else(|e| panic!("{e}"));
        g.client = Some(Client::new(service, "default"));
        assert!(g.list("pods", "default").await.is_err());
        assert_eq!(requests.load(std::sync::atomic::Ordering::SeqCst), 2);
    });
}
#[test]
fn duplicate_kubeconfig_names_are_rejected_before_selection() {
    let path = std::env::temp_dir()
        .canonicalize()
        .unwrap_or_else(|e| panic!("test temporary root: {e}"))
        .join(format!("kborg-duplicate-{}.json", std::process::id()));
    let config = json!({"apiVersion":"v1","kind":"Config","contexts":[
        {"name":"ambiguous","context":{"cluster":"first"}},
        {"name":"ambiguous","context":{"cluster":"second"}}
    ]});
    std::fs::write(&path, config.to_string()).unwrap_or_else(|e| panic!("{e}"));
    assert!(
        read_config(&Connection {
            kubeconfig: Some(path.clone()),
            ..Default::default()
        })
        .is_err()
    );
    std::fs::remove_file(path).unwrap_or_else(|e| panic!("{e}"));
}
