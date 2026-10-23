use kborg::{
    cli::runtime,
    gateway::{Connection, Gateway},
    policy::{Action, Approval},
};
use std::{path::PathBuf, time::Duration};

#[test]
#[ignore = "requires uniquely owned local fixtures; scripts/test-local.py"]
fn long_follow_crosses_old_byte_cap_and_a_quiet_network_interval() -> kborg::Result<()> {
    use k8s_openapi::api::core::v1::Pod;
    use kube::{
        Api,
        api::{DeleteParams, PostParams, Preconditions},
    };
    use serde_json::json;
    runtime()?.block_on(async {
        let reader=Gateway::connect(options("reader",false)).await?;
        let namespace=reader.namespace.clone();
        assert!(namespace.starts_with("kborg-tests-"));
        let path=PathBuf::from(std::env::var("KBORG_TEST_FIXTURE_CONFIG").map_err(|_|"missing owned fixture credential")?);
        let config=kube::config::Kubeconfig::read_from(path).map_err(|_|"cannot read scoped fixture config")?;
        let config=kube::Config::from_custom_kubeconfig(config,&Default::default()).await.map_err(|_|"invalid scoped fixture config")?;
        assert_eq!(config.default_namespace,namespace);
        let client=kube::Client::try_from(config).map_err(|_|"fixture client unavailable")?;
        let pods:Api<Pod>=Api::namespaced(client,&namespace);
        let listed=reader.list("pods",&namespace).await?;
        let image=listed["items"].as_array().and_then(|items|items.iter().find_map(|pod|pod["spec"]["containers"][0]["image"].as_str())).ok_or("fixture image missing")?.to_string();
        let digest=image.rsplit_once("@sha256:").ok_or("fixture image must be pinned")?.1;
        assert_eq!(digest.len(),64); assert!(digest.bytes().all(|b|b.is_ascii_hexdigit()));
        let pod:Pod=serde_json::from_value(json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"long-follow","namespace":namespace},"spec":{
            "automountServiceAccountToken":false,"terminationGracePeriodSeconds":1,
            "securityContext":{"runAsNonRoot":true,"runAsUser":1000,"seccompProfile":{"type":"RuntimeDefault"}},
            "containers":[{"name":"sample","image":image,"command":["sh","-c","printf 'START_LONG_LOG\\n'; dd if=/dev/zero bs=1000 count=300 2>/dev/null | tr '\\000' x; printf '\\n'; sleep 15; printf 'AFTER_QUIET\\n'; sleep 60"],
                "securityContext":{"allowPrivilegeEscalation":false,"readOnlyRootFilesystem":true,"capabilities":{"drop":["ALL"]}},
                "resources":{"requests":{"cpu":"10m","memory":"8Mi"},"limits":{"cpu":"100m","memory":"32Mi"}}}]
        }})).map_err(|_|"invalid log fixture")?;
        let created=pods.create(&PostParams::default(),&pod).await.map_err(|_|"cannot create owned log fixture")?;
        let uid=created.metadata.uid.ok_or("fixture UID missing")?;
        let result=async {
            tokio::time::timeout(Duration::from_secs(60),async {
                loop {
                    let pod=pods.get("long-follow").await.map_err(|_|"fixture read failed")?;
                    if pod.status.and_then(|status|status.conditions).is_some_and(|conditions|conditions.iter().any(|condition|condition.type_=="Ready" && condition.status=="True")) {break;}
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Ok::<_,String>(())
            }).await.map_err(|_|"log fixture readiness timed out")??;
            let mut feed=reader.follow_logs_with_options(&namespace,"long-follow",Some("sample".into()),false,kborg::logs::FollowOptions{seconds:45}).await?;
            let mut saw_large=false;
            let mut quiet_started=None;
            while let Some(line)=feed.next().await? {
                assert!(!line.contains('\x1b'));
                if line.contains("line truncated at 4096 bytes") {saw_large=true;quiet_started=Some(std::time::Instant::now());}
                if line.contains("AFTER_QUIET") {
                    assert!(saw_large,"follow did not cross the old 256 KiB cap");
                    assert!(quiet_started.is_some_and(|started|started.elapsed()>Duration::from_secs(10)),"quiet network interval was not demonstrated");
                    return Ok::<_,String>(());
                }
            }
            Err("live log stream ended before the quiet interval resumed".into())
        }.await;
        pods.delete("long-follow",&DeleteParams{preconditions:Some(Preconditions{uid:Some(uid),resource_version:None}),..Default::default()}).await.map_err(|_|"owned log fixture cleanup failed")?;
        result
    })
}

fn options(identity: &str, writable: bool) -> Connection {
    let path = std::env::var(format!(
        "KBORG_TEST_{}_CONFIG",
        identity.to_ascii_uppercase()
    ))
    .unwrap_or_else(|_| panic!("run scripts/Test-DockerDesktop.ps1 first"));
    Connection {
        kubeconfig: Some(PathBuf::from(path)),
        context: Some("kborg-test".into()),
        writable,
        logs: true,
        ..Default::default()
    }
}

async fn controlled_workload_edits(
    operator: &mut Gateway,
    namespace: &str,
    image: &str,
) -> kborg::Result<()> {
    use serde_json::json;
    for (resource, name, path) in [
        ("statefulsets", "edit-stateful", "/template/spec/containers"),
        ("daemonsets", "edit-daemon", "/template/spec/containers"),
        (
            "cronjobs",
            "edit-cron",
            "/jobTemplate/spec/template/spec/containers",
        ),
    ] {
        let before = operator.get(resource, namespace, name).await?;
        let mut expected = before["spec"].clone();
        let selected = expected
            .pointer_mut(path)
            .ok_or("live edit fixture has no containers")?;
        selected[0]["image"] = json!(image);
        selected[0]["resources"]["requests"]["cpu"] = json!("10m");
        selected[0]["resources"]["requests"]["memory"] = json!("8Mi");
        selected[0]["resources"]["limits"]["cpu"] = json!("200m");
        selected[0]["resources"]["limits"]["memory"] = json!("64Mi");
        let reviewed = operator
            .prepare(
                resource,
                namespace,
                name,
                Action::SetContainer {
                    container: "sample".into(),
                    edit: kborg::edit::ContainerEdit {
                        image: image.into(),
                        resources: Some(kborg::edit::ResourceEdit {
                            cpu_request: "0.01".into(),
                            memory_request: "8Mi".into(),
                            cpu_limit: "0.2".into(),
                            memory_limit: "64Mi".into(),
                        }),
                    },
                },
            )
            .await
            .map_err(|error| format!("{resource}/{name} edit preparation: {error}"))?;
        assert_eq!(
            reviewed.target.uid,
            before["metadata"]["uid"]
                .as_str()
                .ok_or("fixture UID missing")?
        );
        assert!(!reviewed.target.version.is_empty());
        assert!(reviewed.summary.contains(image));
        assert!(reviewed.summary.contains("+ limits.cpu: 0.2"));
        let unchanged = operator.get(resource, namespace, name).await?;
        assert_eq!(
            unchanged["spec"], before["spec"],
            "dry-run modified {resource}"
        );
        let reviewed_version = reviewed.target.version.clone();
        let approval = Approval::confirm(
            reviewed.target.clone(),
            reviewed.action.clone(),
            &format!("{namespace}/{name}"),
        )?;
        operator
            .execute(reviewed, approval)
            .await
            .map_err(|error| format!("{resource}/{name} edit execution: {error}"))?;
        let after = operator.get(resource, namespace, name).await?;
        assert_eq!(after["metadata"]["uid"], before["metadata"]["uid"]);
        assert_ne!(after["metadata"]["resourceVersion"], reviewed_version);
        assert_eq!(
            after["spec"], expected,
            "unreviewed fields changed in {resource}"
        );
        if resource == "statefulsets" {
            assert_eq!(after["spec"]["replicas"], 0);
        }
        if resource == "daemonsets" {
            assert_eq!(
                after["spec"]["template"]["spec"]["nodeSelector"]["kborg.io/isolated-edit-fixture"],
                namespace
            );
        }
        if resource == "cronjobs" {
            assert_eq!(after["spec"]["suspend"], true);
        }
    }
    Ok(())
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
        let selected = reader
            .list_selector("po", &ns, "app in (kborg-sample)")
            .await
            .unwrap_or_else(|error| panic!("live label selector: {error}"));
        let selected = selected["items"]
            .as_array()
            .unwrap_or_else(|| panic!("selector items missing"));
        assert!(!selected.is_empty());
        assert!(
            selected
                .iter()
                .all(|pod| pod["metadata"]["labels"]["app"] == "kborg-sample")
        );
        let no_match = reader
            .list_selector("po", &ns, "app=not-the-owned-fixture")
            .await
            .unwrap_or_else(|error| panic!("no-match label selector: {error}"));
        assert_eq!(no_match["items"], serde_json::json!([]));
        let mut no_match_watch = reader
            .watch_selector("po", &ns, "app=not-the-owned-fixture")
            .unwrap_or_else(|error| panic!("no-match watch setup: {error}"));
        let initial = no_match_watch.next().await;
        assert!(initial.objects.is_empty());
        assert!(!initial.stale && !initial.terminal);
        let watched = tokio::time::timeout(Duration::from_secs(15), no_match_watch.next())
            .await
            .unwrap_or_else(|error| panic!("no-match native watch: {error}"));
        assert!(watched.objects.is_empty());
        assert!(!watched.stale && !watched.terminal);
        drop(no_match_watch);
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
        assert!(!config.to_string().contains("KBORG_TEST_CANARY"));
        let yaml = kborg::inspect::yaml(&config).unwrap_or_else(|e| panic!("{e}"));
        assert!(!yaml.contains("KBORG_TEST_CANARY"));
        let pod = items
            .iter()
            .find(|pod| pod["metadata"]["labels"]["app"] == "kborg-sample")
            .unwrap_or_else(|| panic!("owned sample pod missing"))["metadata"]["name"]
            .as_str()
            .unwrap_or_else(|| panic!("missing pod name"));
        let logs = reader
            .logs(&ns, pod, None, false)
            .await
            .unwrap_or_else(|e| panic!("logs: {e}"));
        assert!(logs.contains("KBorg fixture"));
        assert!(!logs.chars().any(|c| c == '\x1b' || c == '\r'));
        let head = reader
            .logs_with_options(
                &ns,
                pod,
                Some("sample".into()),
                false,
                kborg::logs::ReadOptions {
                    tail_lines: 2,
                    since_seconds: None,
                    head: true,
                },
            )
            .await
            .unwrap_or_else(|error| panic!("head logs: {error}"));
        assert!(head.lines().count() <= 2);
        let first = head
            .lines()
            .next()
            .unwrap_or_else(|| panic!("head logs empty"));
        assert!(kborg::logs::without_timestamp(first).contains("KBorg fixture"));
        assert_ne!(
            kborg::logs::without_timestamp(first),
            first,
            "timestamp was not retained"
        );
        assert_eq!(
            reader
                .containers(&ns, pod)
                .await
                .unwrap_or_else(|e| panic!("{e}")),
            vec!["sample"]
        );
        let mut follow = reader
            .follow_logs(&ns, pod, Some("sample".into()), false)
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let line = tokio::time::timeout(Duration::from_secs(10), follow.next())
            .await
            .unwrap_or_else(|e| panic!("{e}"))
            .unwrap_or_else(|e| panic!("{e}"));
        assert!(line.is_some());
        drop(follow);
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
            .get("deployments", &ns, "sample")
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        let original = delete["spec"]["template"]["spec"]["containers"][0]["image"]
            .as_str()
            .unwrap_or_else(|| panic!("missing image"))
            .to_string();
        let replacement = if original.starts_with("busybox@") {
            format!("docker.io/library/{original}")
        } else {
            format!(
                "busybox@{}",
                original
                    .split_once('@')
                    .unwrap_or_else(|| panic!("image must be pinned"))
                    .1
            )
        };
        tokio::time::sleep(Duration::from_secs(5)).await;
        let edited = operator
            .prepare(
                "deployments",
                &ns,
                "sample",
                Action::SetImage {
                    container: "sample".into(),
                    image: replacement.clone(),
                },
            )
            .await
            .unwrap_or_else(|e| panic!("image dry-run: {e}"));
        assert!(edited.summary.contains(&replacement));
        let approval = Approval::confirm(
            edited.target.clone(),
            edited.action.clone(),
            &format!("{ns}/sample"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        operator
            .execute(edited, approval)
            .await
            .unwrap_or_else(|e| panic!("image execute: {e}"));
        let changed = operator
            .get("deployments", &ns, "sample")
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            changed["spec"]["template"]["spec"]["containers"][0]["image"],
            replacement
        );
        tokio::time::sleep(Duration::from_secs(5)).await;
        let budgets = operator
            .prepare(
                "deployments",
                &ns,
                "sample",
                Action::SetResources {
                    container: "sample".into(),
                    resources: kborg::edit::ResourceEdit {
                        cpu_request: "0.01".into(),
                        memory_request: "8Mi".into(),
                        cpu_limit: "0.2".into(),
                        memory_limit: "64Mi".into(),
                    },
                },
            )
            .await
            .unwrap_or_else(|e| panic!("resources dry-run: {e}"));
        assert!(budgets.summary.contains("+ limits.cpu: 0.2"));
        let approval = Approval::confirm(
            budgets.target.clone(),
            budgets.action.clone(),
            &format!("{ns}/sample"),
        )
        .unwrap_or_else(|e| panic!("{e}"));
        operator
            .execute(budgets, approval)
            .await
            .unwrap_or_else(|e| panic!("resources execute: {e}"));
        let updated = operator
            .get("deployments", &ns, "sample")
            .await
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(
            updated["spec"]["template"]["spec"]["containers"][0]["image"],
            replacement
        );
        assert_eq!(
            updated["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]["cpu"],
            "200m"
        );
        assert_eq!(
            updated["spec"]["template"]["spec"]["containers"][0]["resources"]["limits"]["memory"],
            "64Mi"
        );
        controlled_workload_edits(&mut operator, &ns, &replacement)
            .await
            .unwrap_or_else(|error| panic!("controlled workload edit acceptance: {error}"));
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
