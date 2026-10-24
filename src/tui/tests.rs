//! Terminal UI behaviour tests.
use super::*;

type Harness = (
    App,
    mpsc::SyncSender<(u64, Reply)>,
    tokio::sync::mpsc::Receiver<(u64, Task)>,
);
fn harness() -> Harness {
    let (tx, commands) = tokio::sync::mpsc::channel(4);
    let (replies, rx) = mpsc::sync_channel(4);
    let watch = std::sync::Arc::new(WatchSlot::default());
    watch.invalidate(12);
    let forward = std::sync::Arc::new(ForwardSlot::default());
    (
        App {
            options: Options {
                connection: Connection::default(),
                theme: None,
                color: false,
                mouse: false,
                json: false,
                splash: false,
                poll: false,
                yaml: false,
                follow: false,
                previous: false,
                all_containers: false,
                dynamic_membership: false,
                dashboard: Vec::new(),
                dashboard_last: false,
                external_editor: false,
                helm_storage: None,
                helm_tool: None,
                helm_values: None,
                helm_registry_ca: None,
                helm_registry_auth: None,
                helm_keyring: None,
                helm_sigstore_policy: None,
                copilot_policy: None,
                helm_timeout_seconds: 300,
                allow_tools: false,
                allow_extensions: false,
                log_seconds: 3600,
                interactive: false,
                session_seconds: 900,
                metrics_resources: false,
                selector: String::new(),
                log_read: crate::logs::ReadOptions::default(),
                command: vec![],
            },
            theme: themes::builtins().remove(0),
            context: "test".into(),
            server: "test".into(),
            namespace: "test".into(),
            kinds: crate::gateway::kinds(),
            kind_index: 0,
            items: vec![],
            selected: 0,
            filter: String::new(),
            selector: String::new(),
            field_selector: String::new(),
            sort: Sort::Name,
            history: vec![],
            history_index: None,
            history_draft: String::new(),
            completion: None,
            command_cursor: 0,
            back: vec![],
            forward: vec![],
            marks: Default::default(),
            saved: vec![],
            aliases: Default::default(),
            hotkeys: Default::default(),
            known_namespaces: vec![],
            restoring_uid: None,
            subviews: vec![],
            log_target: None,
            helm_generation: 0,
            helm_cancel: None,
            advice_cancel: None,
            helm_return: None,
            helm_elapsed: 0,
            last_forward: forward.text(),
            forward_selected: 0,
            desired_resource: None,
            status: "waiting".into(),
            stale: false,
            truncated: false,
            modal: Modal::None,
            epoch: 12,
            busy: false,
            command_epoch: None,
            connecting_epoch: None,
            watching: false,
            watch_blocked: false,
            bounded_poll: false,
            pending_discovery: false,
            sticky_status: false,
            namespace_override: false,
            write_inflight: false,
            connected: true,
            refresh: false,
            last_refresh: Instant::now(),
            metrics_history: crate::metrics::MetricsHistory::default(),
            last_metrics: Instant::now(),
            last_table_metrics: None,
            table_metrics: Default::default(),
            items_generation: 0,
            visible_cache: Default::default(),
            metrics_resources: false,
            overview: None,
            selection_ui: Default::default(),
            pending_session: None,
            pending_editor: false,
            worker: Worker {
                tx,
                rx,
                watch,
                logs: std::sync::Arc::new(LogSlot::default()),
                forward,
            },
        },
        replies,
        commands,
    )
}
#[test]
fn copilot_closed_queue_and_late_previews_do_not_leave_loading_views() -> Result<()> {
    let (mut app, replies, commands) = harness();
    app.items = vec![
        serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"test","namespace":"test","uid":"test-uid"}}),
    ];
    drop(commands);
    app.command("copilot")?;
    assert!(!app.busy);
    assert!(
        matches!(app.modal, Modal::AdviceDocument(ref title,..) if title.contains("NOT STARTED"))
    );
    app.modal = Modal::AdviceLoading;
    app.key(KeyCode::Esc)?;
    replies
        .send((
            app.epoch,
            Reply::AdvicePrepared(Ok((None, "late preview".into()))),
        ))
        .map_err(|_| "reply closed")?;
    app.receive();
    assert!(matches!(app.modal, Modal::None));
    app.modal = Modal::AdviceLoading;
    replies
        .send((
            app.epoch - 1,
            Reply::AdvicePrepared(Ok((None, "stale preview".into()))),
        ))
        .map_err(|_| "reply closed")?;
    app.receive();
    assert!(matches!(app.modal, Modal::AdviceLoading));
    Ok(())
}
#[test]
fn copilot_cancellation_waits_for_cleanup_before_unlocking_navigation() -> Result<()> {
    let (mut app, replies, _commands) = harness();
    let (cancel, mut receiver) = tokio::sync::oneshot::channel();
    app.advice_cancel = Some(cancel);
    app.modal = Modal::AdviceRunning;
    app.busy = true;
    app.write_inflight = true;
    app.command_epoch = Some(app.epoch);
    app.key_event(KeyCode::Char('c'), KeyModifiers::CONTROL)?;
    assert!(receiver.try_recv().is_ok());
    assert!(app.busy && app.write_inflight && matches!(app.modal, Modal::AdviceRunning));
    app.key(KeyCode::Esc)?;
    assert!(matches!(app.modal, Modal::AdviceRunning));
    replies
        .send((
            app.epoch,
            Reply::AdviceOutcome(Err("Canceled after cleanup".into())),
        ))
        .map_err(|_| "reply closed")?;
    app.receive();
    assert!(!app.busy && !app.write_inflight && app.advice_cancel.is_none());
    assert!(matches!(app.modal,Modal::AdviceDocument(ref title,..) if title.contains("STOPPED")));
    Ok(())
}
fn demo_helm_prepared() -> Result<Box<crate::helm_lifecycle::Prepared>> {
    crate::cli::runtime()?.block_on(async {
        let gateway = Gateway::connect(Connection {
            demo: true,
            writable: true,
            allow_helm: true,
            allow_helm_write: true,
            ..Default::default()
        })
        .await?;
        crate::helm_lifecycle::prepare(
            &gateway,
            crate::helm_lifecycle::Options {
                executable: "/demo/helm".into(),
                namespace: "demo".into(),
                release: "demo-new".into(),
                storage: crate::helm::Storage::Secret,
                seconds: 300,
                allow_hooks: false,
                allow_crds: false,
                registry: Default::default(),
                keyring: None,
                sigstore_policy: None,
            },
            crate::helm_lifecycle::Request::Install {
                chart: "/demo/chart.tgz".into(),
                values: None,
            },
        )
        .await
        .map(Box::new)
    })
}
#[test]
fn chart_crd_command_requires_explicit_flag_and_propagates_registry_policy() -> Result<()> {
    let (mut app, _, mut commands) = harness();
    app.options.connection.demo = true;
    app.options.connection.writable = true;
    app.options.connection.allow_helm = true;
    app.options.connection.allow_helm_write = true;
    app.command("helm-crds")?;
    assert!(matches!(app.modal, Modal::None));
    assert!(app.status.contains("--allow-helm-crds"));
    app.options.connection.allow_helm_crds = true;
    app.options.connection.allow_helm_hooks = true;
    app.options.helm_registry_ca = Some("/private/registry-ca.pem".into());
    app.options.helm_registry_auth = Some("/private/registry-auth.json".into());
    app.command("helm-crds")?;
    let Modal::HelmForm(form) = &mut app.modal else {
        return Err("missing chart CRD form".into());
    };
    form.fields[1].1 = "my.api".into();
    let draft = form.draft("/demo/helm".into(), 300)?;
    assert!(draft.options.allow_hooks && draft.options.allow_crds);
    assert_eq!(draft.options.registry.ca_file, app.options.helm_registry_ca);
    assert_eq!(
        draft.options.registry.auth_file,
        app.options.helm_registry_auth
    );
    assert!(matches!(
        draft.request,
        crate::helm_lifecycle::Request::InstallCrds { .. }
    ));
    assert!(commands.try_recv().is_err());
    Ok(())
}
#[test]
fn helm_install_inherits_visible_release_table_scope() -> Result<()> {
    let (mut app, _, mut commands) = harness();
    app.options.connection.demo = true;
    app.options.connection.writable = true;
    app.options.connection.allow_helm = true;
    app.options.connection.allow_helm_write = true;
    app.namespace = "default".into();
    app.options.helm_storage = Some(crate::helm::Storage::Secret);
    app.modal = Modal::Helm(Box::new(helm_ui::Table::new(
        crate::helm::Snapshot {
            releases: vec![],
            truncated: false,
            issues: vec![],
            source: "test".into(),
        },
        None,
        "staging".into(),
        Some(crate::helm::Storage::ConfigMap),
    )));
    app.key(KeyCode::Char('i'))?;
    let Modal::HelmForm(mut form) = std::mem::replace(&mut app.modal, Modal::None) else {
        return Err("missing install form".into());
    };
    assert_eq!(form.fields[0].1, "staging");
    form.fields[1].1 = "new-release".into();
    let draft = form.draft("/demo/helm".into(), 300)?;
    assert_eq!(draft.options.namespace, "staging");
    assert_eq!(draft.options.storage, crate::helm::Storage::ConfigMap);
    app.subviews.clear();
    app.begin_helm_form(helm_ui::Intent::Install);
    assert!(matches!(app.modal,Modal::HelmForm(ref form) if form.fields[0].1=="default"));
    assert!(commands.try_recv().is_err());
    Ok(())
}
#[test]
fn schema_commands_require_capabilities_and_keep_visible_helm_scope() -> Result<()> {
    let (mut app, _, mut commands) = harness();
    app.options.connection.demo = true;
    app.options.connection.writable = true;
    app.options.connection.allow_helm = true;
    app.options.connection.allow_helm_write = true;
    app.command("helm-crds-delete")?;
    assert!(matches!(app.modal, Modal::None));
    assert!(app.status.contains("--allow-helm-crds"));
    app.options.connection.allow_helm_crds = true;
    app.options.helm_keyring = Some("/private/trusted.gpg".into());
    app.subviews.push(Modal::Helm(Box::new(helm_ui::Table::new(
        crate::helm::Snapshot {
            releases: vec![],
            truncated: false,
            issues: vec![],
            source: "test".into(),
        },
        None,
        "staging".into(),
        Some(crate::helm::Storage::ConfigMap),
    ))));
    app.command("helm-crds-upgrade")?;
    let Modal::HelmForm(form) = &mut app.modal else {
        return Err("schema upgrade form missing".into());
    };
    assert_eq!(form.fields[0].1, "staging");
    assert!(matches!(form.intent, helm_ui::Intent::UpgradeCrds));
    form.fields[1].1 = "schema-owner".into();
    let draft = form.draft("/demo/helm".into(), 300)?;
    assert_eq!(draft.options.storage, crate::helm::Storage::ConfigMap);
    assert_eq!(draft.options.keyring, app.options.helm_keyring);
    assert!(matches!(
        draft.request,
        crate::helm_lifecycle::Request::UpgradeCrds { .. }
    ));
    assert!(
        commands.try_recv().is_err(),
        "forms never enqueue writes before review"
    );
    Ok(())
}
#[test]
fn helm_status_tracks_current_draft_review_and_execution() -> Result<()> {
    let (mut app, replies, mut commands) = harness();
    app.options.connection.demo = true;
    app.options.connection.writable = true;
    app.options.connection.allow_helm = true;
    app.options.connection.allow_helm_write = true;
    app.status = "HELM OPERATION SUCCEEDED".into();
    app.sticky_status = true;
    app.begin_helm_form(helm_ui::Intent::Install);
    assert_eq!(app.status, "Helm draft; no execution requested.");
    assert!(!app.sticky_status);
    let Modal::HelmForm(form) = &mut app.modal else {
        return Err("missing Helm draft".into());
    };
    form.fields[0].1 = "demo".into();
    form.fields[1].1 = "demo-new".into();
    app.key_with_approval_size(KeyCode::Enter, || Ok((80, 24)))?;
    assert_eq!(app.status, "Preparing Helm review; no execution requested.");
    assert!(matches!(
        commands.try_recv(),
        Ok((_, Task::HelmPrepare(..)))
    ));
    replies
        .send((
            app.epoch,
            Reply::HelmPrepared(app.helm_generation, Ok(demo_helm_prepared()?)),
        ))
        .map_err(|_| "reply closed")?;
    app.receive();
    assert_eq!(app.status, "Helm review ready; no execution requested.");
    let Modal::HelmReview(review) = &mut app.modal else {
        return Err("missing Helm review".into());
    };
    review.typed = review.prepared.confirmation();
    review.mark_drawn(78, 500);
    app.key_with_approval_size(KeyCode::Enter, || Ok((80, 24)))?;
    assert!(app.write_inflight);
    assert_eq!(
        app.status,
        "Helm execution in progress; Ctrl-C requests cancellation."
    );
    assert!(!screen(&app, 80, 24).join("\n").contains("SUCCEEDED"));
    Ok(())
}
#[test]
fn helm_lifecycle_gates_cancel_review_stale_scope_and_unknown_outcome() -> Result<()> {
    let (mut app, replies, mut commands) = harness();
    app.begin_helm_form(helm_ui::Intent::Install);
    assert!(matches!(app.modal, Modal::None));
    assert!(app.status.contains("--allow-helm-write"));
    assert!(commands.try_recv().is_err());
    app.options.connection.demo = true;
    app.options.connection.writable = true;
    app.options.connection.allow_helm = true;
    app.options.connection.allow_helm_write = true;
    app.begin_helm_form(helm_ui::Intent::Install);
    assert!(matches!(app.modal, Modal::HelmForm(_)));
    app.key(KeyCode::Esc)?;
    assert!(commands.try_recv().is_err());
    app.modal = Modal::HelmReview(Box::new(helm_ui::Review::new(
        demo_helm_prepared()?,
        app.epoch,
    )));
    assert!(screen(&app, 28, 10).join("\n").contains("unread"));
    app.key_with_approval_size(KeyCode::Enter, || Ok((80, 24)))?;
    assert!(commands.try_recv().is_err());
    if let Modal::HelmReview(review) = &mut app.modal {
        review.typed = review.prepared.confirmation();
        review.mark_drawn(78, 500);
        review.epoch = app.epoch - 1;
    }
    app.key_with_approval_size(KeyCode::Enter, || Ok((80, 24)))?;
    assert!(commands.try_recv().is_err());
    if let Modal::HelmReview(review) = &mut app.modal {
        review.epoch = app.epoch;
    }
    app.helm_return = Some(("demo".into(), crate::helm::Storage::Secret));
    app.key_with_approval_size(KeyCode::Enter, || Ok((80, 24)))?;
    assert!(app.write_inflight && matches!(app.modal, Modal::HelmRunning(..)));
    let (generation, mut cancel) = match commands.try_recv() {
        Ok((_, Task::HelmExecute(_, typed, generation, cancel))) => {
            assert_eq!(typed, "demo/demo-new");
            (generation, cancel)
        }
        _ => return Err("missing reviewed Helm execution".into()),
    };
    app.key_event(KeyCode::Char('c'), KeyModifiers::CONTROL)?;
    assert!(cancel.try_recv().is_ok());
    assert!(app.write_inflight);
    replies
        .send((
            app.epoch,
            Reply::HelmOutcome(
                generation,
                Ok(crate::helm_lifecycle::Outcome {
                    success: false,
                    summary: "UNKNOWN: verify release state before retrying".into(),
                    uncertain: true,
                }),
            ),
        ))
        .map_err(|_| "reply closed")?;
    app.receive();
    assert!(!app.write_inflight && !app.busy);
    assert!(matches!(app.modal,Modal::HelmOutcome(ref title,..) if title.contains("UNKNOWN")));
    assert!(screen(&app, 80, 24).join("\n").contains("UNKNOWN"));
    app.key(KeyCode::Esc)?;
    assert!(
        matches!(commands.try_recv(),Ok((_,Task::Helm(helm_ui::Request::List(namespace,Some(crate::helm::Storage::Secret)),_))) if namespace=="demo")
    );
    Ok(())
}
#[test]
fn unavailable_helm_queue_never_leaves_a_running_operation_modal() -> Result<()> {
    let (mut app, _, commands) = harness();
    drop(commands);
    let mut review = helm_ui::Review::new(demo_helm_prepared()?, app.epoch);
    review.typed = review.prepared.confirmation();
    review.mark_drawn(78, 500);
    app.modal = Modal::HelmReview(Box::new(review));
    app.key_with_approval_size(KeyCode::Enter, || Ok((80, 24)))?;
    assert!(!app.write_inflight && !app.busy);
    assert!(matches!(app.modal,Modal::HelmOutcome(ref title,..) if title.contains("NOT STARTED")));
    Ok(())
}
#[test]
fn approval_enter_requires_known_painted_dimensions_before_export_or_dispatch() -> Result<()> {
    struct Directory(std::path::PathBuf);
    impl Drop for Directory {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| "test clock unavailable")?
        .as_nanos();
    let path = std::env::temp_dir()
        .canonicalize()
        .map_err(|_| "test temporary root unavailable")?
        .join(format!("kborg-export-size-{}-{stamp}", std::process::id()));
    std::fs::create_dir(&path).map_err(|_| "test directory creation failed")?;
    let directory = Directory(path);
    safety::ensure_private_dir(&directory.0)?;
    let (mut app, _, mut commands) = harness();
    for (width, height) in [(20, 100), (27, 100), (120, 9)] {
        let draft = crate::exports::fixture_at(&directory.0.join("exports"))?;
        let destination = draft.path().to_owned();
        app.modal = Modal::Export(draft);
        let painted = screen(&app, width, height).join("\n");
        assert!(painted.contains("needs 28x10"));
        assert!(!painted.contains("File (escaped)"));
        app.key_with_approval_size(KeyCode::Enter, || Ok((width, height)))?;
        assert!(!destination.exists());
        assert!(matches!(app.modal, Modal::Export(_)));
        assert!(commands.try_recv().is_err());
    }
    let draft = crate::exports::fixture_at(&directory.0.join("exports"))?;
    let destination = draft.path().to_owned();
    app.modal = Modal::Export(draft);
    assert!(
        app.key_with_approval_size(KeyCode::Enter, || Err(io::Error::other("fixture")))
            .is_err()
    );
    assert!(!destination.exists());
    assert!(!directory.0.join("exports").exists());
    let draft = crate::exports::fixture_at(&directory.0.join("exports"))?;
    let destination = draft.path().to_owned();
    app.modal = Modal::Export(draft);
    assert!(screen(&app, 120, 34).join("\n").contains("File (escaped)"));
    app.key_with_approval_size(KeyCode::Enter, || Ok((120, 34)))?;
    assert_eq!(
        std::fs::read_to_string(destination).map_err(|_| "test export unavailable")?,
        "kind: Pod\n"
    );
    let mut review = helm_ui::Review::new(demo_helm_prepared()?, app.epoch);
    review.typed = review.prepared.confirmation();
    review.mark_drawn(78, 500);
    app.modal = Modal::HelmReview(Box::new(review));
    assert!(
        app.key_with_approval_size(KeyCode::Enter, || Err(io::Error::other("fixture")))
            .is_err()
    );
    assert!(!app.write_inflight);
    assert!(commands.try_recv().is_err());
    Ok(())
}
#[test]
fn queued_helm_execution_rejects_stale_epoch_before_running() -> Result<()> {
    let worker = worker();
    worker.watch.invalidate(99);
    let (_, cancel) = tokio::sync::oneshot::channel();
    worker
        .tx
        .blocking_send((
            12,
            Task::HelmExecute(demo_helm_prepared()?, "demo/demo-new".into(), 5, cancel),
        ))
        .map_err(|_| "queue closed")?;
    let (_, reply) = worker
        .rx
        .recv_timeout(Duration::from_secs(2))
        .map_err(|_| "worker timeout")?;
    assert!(matches!(reply,Reply::HelmOutcome(5,Err(error)) if error.contains("stale")));
    Ok(())
}
#[test]
fn group_log_title_keeps_range_visible_before_long_workload_identity() -> Result<()> {
    let (mut app, _, _) = harness();
    let namespace = format!("kborg-{}", "a".repeat(56));
    let membership = crate::logs::LogMembership::new(
        &namespace,
        format!(
            "workload deployments.apps / ui-lifecycle-worker UID {}; fixed membership",
            "1".repeat(36)
        ),
        ["one", "two"]
            .into_iter()
            .map(|name| crate::logs::LogPod {
                name: name.into(),
                uid: format!("uid-{name}"),
                containers: vec!["worker".into()],
            })
            .collect(),
        vec![],
        "test-connection",
    )?;
    app.log_target = Some(LogTarget {
        namespace,
        pod: "ui-lifecycle-worker".into(),
        container: None,
        uid: "workload-uid".into(),
        previous: false,
        follow: true,
        all: true,
        dynamic: false,
        membership: Some(membership),
    });
    let view = crate::ui_logs::LogView::new(
        "[Log stream merge fixed membership]\nrecord".into(),
        crate::logs::ReadOptions::for_key('3').ok_or("missing range")?,
    );
    app.modal = Modal::Follow(Box::new(view));
    for width in [48, 80, 140] {
        let frame = screen(&app, width, 12);
        let title = &frame[3];
        for expected in ["FIXED 2 Pods", "2 containers", "last 300s"] {
            assert!(
                title.contains(expected),
                "{width}: missing {expected}: {title}"
            );
        }
        assert!(!title.contains("111111111111111111111111111111111111"));
    }
    Ok(())
}
#[test]
fn logs_live_is_an_explicit_dynamic_opt_in_and_rejects_head_snapshots() {
    let (mut app, _replies, mut commands) = harness();
    app.options.connection.logs = true;
    app.namespace = "default".into();
    assert!(app.command("logs app=web").is_ok());
    match commands.try_recv() {
        Ok((_, Task::CaptureLogs(target, _, _))) => assert!(!target.dynamic),
        _ => panic!("fixed capture missing"),
    }
    app.busy = false;
    app.command_epoch = None;
    assert!(app.command("logs-live app=web").is_ok());
    match commands.try_recv() {
        Ok((_, Task::CaptureLogs(target, crate::logs::PodLogSelection::Selector(selector), _))) => {
            assert!(target.dynamic && target.follow && !target.previous);
            assert_eq!(selector, "app=web");
        }
        _ => panic!("dynamic capture missing"),
    }
    app.busy = false;
    app.command_epoch = None;
    app.options.log_read.head = true;
    assert!(app.command("logs-live app=web").is_ok());
    assert!(
        commands.try_recv().is_err(),
        "head snapshots cannot be dynamic"
    );
    assert!(app.status.contains("not head"));
}
#[test]
fn workload_logs_capture_once_and_range_reload_retains_exact_pod_membership() -> Result<()> {
    let (object, membership) = crate::cli::runtime()?.block_on(async {
        let gateway = Gateway::connect(Connection {
            demo: true,
            logs: true,
            ..Default::default()
        })
        .await?;
        let object = gateway.get("deployments.apps", "demo", "api").await?;
        let uid = object["metadata"]["uid"]
            .as_str()
            .ok_or("demo workload UID missing")?
            .to_owned();
        let membership = gateway
            .capture_log_membership(
                "demo",
                crate::logs::PodLogSelection::Workload {
                    resource: "deployments.apps".into(),
                    name: "api".into(),
                    uid,
                },
            )
            .await?;
        Ok::<_, String>((object, membership))
    })?;
    let (mut app, replies, mut commands) = harness();
    app.options.connection.logs = true;
    app.kind_index = app.resolve_resource("deployments.apps")?;
    app.items = vec![object];
    app.request_logs(false, true);
    let (target, generation) = match commands.try_recv() {
        Ok((
            _,
            Task::CaptureLogs(
                target,
                crate::logs::PodLogSelection::Workload { resource, .. },
                generation,
            ),
        )) => {
            assert_eq!(resource, "deployments.apps");
            (target, generation)
        }
        _ => return Err("missing fixed membership capture".into()),
    };
    app.items[0]["metadata"]["uid"] = "replacement".into();
    let mut target = target;
    target.membership = Some(membership.clone());
    replies
        .send((app.epoch, Reply::Membership(target, generation)))
        .map_err(|_| "reply closed")?;
    app.receive();
    let initial = match commands.try_recv() {
        Ok((_, Task::Follow(target, _, _, _))) => target.membership.ok_or("membership lost")?,
        _ => return Err("missing captured follow".into()),
    };
    assert_eq!(initial.pods()[0].uid, membership.pods()[0].uid);
    app.busy = false;
    app.key(KeyCode::Char('3'))?;
    match commands.try_recv() {
        Ok((_, Task::Follow(target, _, _, read))) => {
            assert_eq!(
                read,
                crate::logs::ReadOptions::for_key('3').ok_or("range missing")?
            );
            assert_eq!(
                target.membership.ok_or("range lost membership")?.pods()[0].uid,
                initial.pods()[0].uid
            );
        }
        _ => return Err("range recaptured membership unexpectedly".into()),
    };
    assert!(screen(&app, 120, 24).join("\n").contains("FIXED"));
    Ok(())
}
#[test]
fn helm_reads_require_capability_and_late_results_do_not_reopen_views() -> Result<()> {
    let (mut app, replies, mut commands) = harness();
    app.command("helm")?;
    assert!(commands.try_recv().is_err());
    assert!(app.status.contains("--allow-helm"));
    app.options.connection.allow_helm = true;
    app.command("helm")?;
    let generation = match commands.try_recv() {
        Ok((_, Task::Helm(helm_ui::Request::List(ns, None), generation))) => {
            assert_eq!(ns, "test");
            generation
        }
        _ => return Err("missing Helm read".into()),
    };
    assert!(matches!(app.modal, Modal::HelmLoading(_)));
    app.key(KeyCode::Esc)?;
    replies
        .send((
            app.epoch,
            Reply::Helm(
                generation,
                Ok(helm_ui::Response::Document(
                    "old".into(),
                    "late secret read".into(),
                )),
            ),
        ))
        .map_err(|_| "reply closed")?;
    app.receive();
    assert!(matches!(app.modal, Modal::None));
    assert!(!screen(&app, 80, 24).join("\n").contains("late secret read"));
    app.command("history my.api")?;
    assert!(
        matches!(commands.try_recv(),Ok((_,Task::Helm(helm_ui::Request::NamedHistory(_,name,None),_))) if name=="my.api")
    );
    Ok(())
}
#[test]
fn pod_log_defaults_and_source_toggle_preserve_captured_identity() -> Result<()> {
    let (mut app, _replies, mut commands) = harness();
    app.options.connection.logs = true;
    let mut pod = object("api");
    pod["apiVersion"] = "v1".into();
    pod["kind"] = "Pod".into();
    pod["spec"] = serde_json::json!({"containers":[{"name":"app"},{"name":"sidecar"}],"initContainers":[{"name":"init"}]});
    app.items = vec![pod.clone()];
    app.subviews
        .push(Modal::Follow(Box::new(crate::ui_logs::LogView::new(
            "older session".into(),
            Default::default(),
        ))));
    app.request_logs(false, true);
    assert!(
        !app.subviews
            .iter()
            .any(|modal| matches!(modal, Modal::Follow(_)))
    );
    let uid = match commands.try_recv() {
        Ok((_, Task::Follow(target, _, _, _))) => {
            assert!(target.all);
            assert_eq!(target.container.as_deref(), Some("app"));
            target.uid
        }
        _ => return Err("missing merged follow".into()),
    };
    app.busy = false;
    app.items[0]["metadata"]["uid"] = "replacement".into();
    app.key(KeyCode::Char('a'))?;
    match commands.try_recv() {
        Ok((_, Task::Follow(target, _, _, _))) => {
            assert!(!target.all);
            assert_eq!(target.uid, uid);
            assert_eq!(target.container.as_deref(), Some("app"));
        }
        _ => return Err("missing single follow".into()),
    };
    app.busy = false;
    app.key(KeyCode::Esc)?;
    pod["metadata"]["annotations"] =
        serde_json::json!({"kubectl.kubernetes.io/default-container":"sidecar"});
    app.items = vec![pod];
    app.request_logs(false, true);
    assert!(
        matches!(commands.try_recv(),Ok((_,Task::Follow(target,_,_,_))) if !target.all && target.container.as_deref()==Some("sidecar"))
    );
    assert_eq!(
        pod_log_choices(&app.items[0]),
        vec!["*", "app", "sidecar", "init"]
    );
    Ok(())
}
fn object(name: &str) -> Value {
    serde_json::json!({"kind":"Deployment","metadata":{"name":name,"namespace":"test","uid":format!("uid-{name}")}})
}
fn screen(app: &App, width: u16, height: u16) -> Vec<String> {
    let mut output = Vec::new();
    assert!(render(app, width, height, &mut output).is_ok());
    let mut cells = vec![vec![' '; usize::from(width)]; usize::from(height)];
    let mut painted = vec![vec![false; usize::from(width)]; usize::from(height)];
    let (mut x, mut y, mut index) = (0usize, 0usize, 0usize);
    while index < output.len() {
        if output[index] == 27 {
            assert_eq!(output[index + 1], b'[');
            let start = index + 2;
            index = start;
            while index < output.len() && !output[index].is_ascii_alphabetic() {
                index += 1;
            }
            let parameters = std::str::from_utf8(&output[start..index]).unwrap_or_default();
            match output[index] {
                b'H' => {
                    let mut parameters = parameters
                        .split(';')
                        .map(|value| value.parse::<usize>().unwrap_or(1));
                    y = parameters.next().unwrap_or(1).saturating_sub(1);
                    x = parameters.next().unwrap_or(1).saturating_sub(1);
                }
                b'J' => cells.iter_mut().for_each(|row| row.fill(' ')),
                b'm' => {}
                b'h' | b'l' if parameters == "?25" => {}
                other => panic!("unexpected terminal command: {other}"),
            }
        } else {
            let Some(character) = std::str::from_utf8(&output[index..])
                .ok()
                .and_then(|text| text.chars().next())
            else {
                panic!("expected a valid terminal UTF-8 character");
            };
            assert!(
                character.is_ascii_graphic()
                    || character == ' '
                    || matches!(character, '╭' | '╮' | '╰' | '╯' | '─' | '│')
            );
            assert!(
                x < usize::from(width) && y < usize::from(height),
                "paint outside {width}x{height}: {x},{y}"
            );
            cells[y][x] = character;
            painted[y][x] = true;
            x += 1;
            index += character.len_utf8() - 1;
        }
        index += 1;
    }
    assert!(
        painted.iter().flatten().all(|painted| *painted),
        "frame left stale cells behind"
    );
    cells
        .into_iter()
        .map(|row| row.into_iter().collect())
        .collect()
}
#[test]
fn destination_only_resource_command_waits_for_destination_discovery() {
    let (mut app, replies, mut commands) = harness();
    app.options.connection.demo = true;
    assert!(app.command("widget team-cmd app=api @demo").is_ok());
    assert_eq!(app.desired_resource.as_deref(), Some("widget"));
    assert_eq!(
        (app.namespace.as_str(), app.selector.as_str()),
        ("team-cmd", "app=api")
    );
    assert!(matches!(commands.try_recv(), Ok((_, Task::Connect(_)))));
    app.options.connection.demo = false;
    assert!(
        replies
            .send((
                app.epoch,
                Reply::Connected(
                    "demo".into(),
                    "server".into(),
                    "wrong-default".into(),
                    crate::gateway::kinds()
                )
            ))
            .is_ok()
    );
    app.receive();
    assert_eq!(app.kind(), "widget");
    assert!(app.watch_blocked);
    assert!(matches!(commands.try_recv(), Ok((_, Task::Discover))));
    let mut widget = app.kinds[0].clone();
    widget.plural = "widgets".into();
    widget.kind = "Widget".into();
    widget.group = "example.test".into();
    let mut catalog = crate::gateway::kinds();
    catalog.push(widget);
    assert!(replies.send((app.epoch, Reply::Catalog(catalog))).is_ok());
    app.receive();
    assert_eq!(app.kind(), "widgets.example.test");
    assert_eq!(app.namespace, "team-cmd");
    assert_eq!(app.selector, "app=api");
    assert!(!app.watch_blocked);
    app.connected = false;
    app.options.connection.context = Some("failed-context".into());
    let mut prior = app.view_state();
    prior.context = "demo".into();
    app.back.push(prior);
    app.go_back();
    match commands.try_recv() {
        Ok((_, Task::Connect(connection))) => {
            assert_eq!(connection.context.as_deref(), Some("demo"))
        }
        _ => panic!("failed connection history did not reconnect destination"),
    }
}
#[test]
fn custom_kind_collisions_use_inspection_instead_of_builtin_drilldowns() {
    for (plural, kind) in [
        ("nodes", "Node"),
        ("pods", "Pod"),
        ("namespaces", "Namespace"),
        ("deployments", "Deployment"),
    ] {
        let (mut app, _replies, mut commands) = harness();
        let mut custom = app.kinds[0].clone();
        custom.plural = plural.into();
        custom.kind = kind.into();
        custom.group = "example.test".into();
        app.kinds.push(custom.clone());
        app.kind_index = app.kinds.len() - 1;
        app.items = vec![
            serde_json::json!({"apiVersion":"example.test/v1","kind":kind,"metadata":{"name":"custom-a","namespace":"test","uid":"custom-uid"},"spec":{"selector":{"matchLabels":{"app":"api"}},"nodeName":"worker-a"}}),
        ];
        assert!(app.key(KeyCode::Enter).is_ok());
        match commands.try_recv() {
            Ok((_, Task::Inspect(resource, _, _))) => assert_eq!(resource, custom.key()),
            _ => panic!("custom kind entered a built-in subview"),
        }
        assert_eq!(app.kind(), custom.key());
        assert!(app.field_selector.is_empty());
    }
}
#[test]
fn history_restores_destination_crd_catalog_and_keeps_unavailable_entries() {
    let (mut app, replies, mut commands) = harness();
    let mut widget = app.kinds[0].clone();
    widget.plural = "widgets".into();
    widget.kind = "Widget".into();
    widget.group = "a.test".into();
    let mut gadget = widget.clone();
    gadget.plural = "gadgets".into();
    gadget.kind = "Gadget".into();
    gadget.group = "b.test".into();
    app.context = "B".into();
    app.namespace = "team-b".into();
    app.kinds.push(gadget.clone());
    app.kind_index = app.kinds.len() - 1;
    app.filter = "gadget".into();
    app.selector = "app=b".into();
    app.items = vec![
        serde_json::json!({"kind":"Gadget","metadata":{"name":"gadget-b","namespace":"team-b","uid":"b-selected"}}),
    ];
    app.back.push(ViewState {
        resource: widget.key(),
        namespace: "team-a".into(),
        context: "A".into(),
        filter: "widget".into(),
        selector: "app=a".into(),
        field_selector: String::new(),
        selected_uid: Some("a-selected".into()),
    });
    fn destination(
        app: &mut App,
        replies: &mpsc::SyncSender<(u64, Reply)>,
        commands: &mut tokio::sync::mpsc::Receiver<(u64, Task)>,
        context: &str,
        kind: Kind,
    ) {
        match commands.try_recv() {
            Ok((_, Task::Connect(connection))) => {
                assert_eq!(connection.context.as_deref(), Some(context))
            }
            _ => panic!("history did not connect to destination"),
        }
        assert!(
            replies
                .send((
                    app.epoch,
                    Reply::Connected(
                        context.into(),
                        "server".into(),
                        "wrong-default".into(),
                        crate::gateway::kinds()
                    )
                ))
                .is_ok()
        );
        app.receive();
        assert_eq!(app.kind(), kind.key());
        assert!(app.desired_resource.is_some());
        assert!(matches!(commands.try_recv(), Ok((_, Task::Discover))));
        let mut catalog = crate::gateway::kinds();
        catalog.push(kind.clone());
        assert!(replies.send((app.epoch, Reply::Catalog(catalog))).is_ok());
        app.receive();
        assert_eq!(app.kind(), kind.key());
        assert!(app.desired_resource.is_none());
    }
    app.go_back();
    assert_eq!(app.kind(), widget.key());
    assert_eq!((app.back.len(), app.forward.len()), (0, 1));
    destination(&mut app, &replies, &mut commands, "A", widget);
    assert_eq!(
        (
            app.namespace.as_str(),
            app.filter.as_str(),
            app.selector.as_str()
        ),
        ("team-a", "widget", "app=a")
    );
    app.replace_items(vec![serde_json::json!({"kind":"Widget","metadata":{"name":"widget-a","namespace":"team-a","uid":"a-selected"}})]);
    assert_eq!(app.selected_uid().as_deref(), Some("a-selected"));
    app.go_forward();
    assert_eq!(app.kind(), gadget.key());
    destination(&mut app, &replies, &mut commands, "B", gadget);
    assert_eq!(
        (
            app.namespace.as_str(),
            app.filter.as_str(),
            app.selector.as_str()
        ),
        ("team-b", "gadget", "app=b")
    );
    let mut unavailable = app.view_state();
    unavailable.resource = "missing.b.test".into();
    app.back.push(unavailable.clone());
    app.forward.push(unavailable);
    let stacks = (app.back.len(), app.forward.len());
    let resource = app.kind();
    app.go_back();
    assert_eq!((app.back.len(), app.forward.len()), stacks);
    assert_eq!(app.kind(), resource);
    app.go_forward();
    assert_eq!((app.back.len(), app.forward.len()), stacks);
    assert_eq!(app.kind(), resource);
}
fn forward_target(local_port: u16) -> crate::ops::ForwardTarget {
    crate::ops::ForwardTarget {
        namespace: "test".into(),
        pod: "server".into(),
        local_port,
        remote_port: 80,
        seconds: 300,
    }
}
#[test]
fn queued_forward_cannot_recreate_canceled_or_different_scope_session() {
    let slot = ForwardSlot::default();
    let watch = WatchSlot::default();
    watch.invalidate(12);
    let generation = slot.generation();
    slot.cancel();
    assert!(
        slot.begin(generation, 12, &watch, forward_target(8080))
            .is_err()
    );
    let generation = slot.generation();
    watch.invalidate(13);
    assert!(
        slot.begin(generation, 12, &watch, forward_target(8080))
            .is_err()
    );
    let (id, mut receiver) = slot
        .begin(generation, 13, &watch, forward_target(8080))
        .unwrap_or_else(|e| panic!("{e}"));
    slot.cancel();
    assert!(matches!(
        receiver.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
    assert!(slot.listening(id, "late bind").is_err());
    assert!(
        slot.begin(generation, 13, &watch, forward_target(8081))
            .is_err()
    );
    assert_eq!(
        slot.active_count(),
        1,
        "stopping remains active until cleanup completes"
    );
    slot.finish(id, Ok("closed".into()));
    assert_eq!(slot.active_count(), 0);
}
#[test]
fn forwards_stop_independently_and_reserve_ports_until_cleanup() {
    let slot = ForwardSlot::default();
    let watch = WatchSlot::default();
    let generation = slot.generation();
    let (a, mut a_cancel) = slot
        .begin(generation, 0, &watch, forward_target(8080))
        .unwrap_or_else(|e| panic!("{e}"));
    let (b, mut b_cancel) = slot
        .begin(generation, 0, &watch, forward_target(8081))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(slot.listening(a, "A bound").is_ok());
    assert!(slot.listening(b, "B bound").is_ok());
    assert_eq!(slot.active_count(), 2);
    assert!(slot.cancel_selected(0));
    assert!(matches!(
        a_cancel.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
    assert!(matches!(
        b_cancel.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert!(
        slot.begin(generation, 0, &watch, forward_target(8080))
            .is_err()
    );
    slot.finish(a, Ok("A closed".into()));
    let (c, _c_cancel) = slot
        .begin(generation, 0, &watch, forward_target(8080))
        .unwrap_or_else(|e| panic!("{e}"));
    assert_ne!(a, c);
    assert_eq!(slot.active_count(), 2);
    assert!(slot.lines(1, 120, 30).join("\n").contains("B bound"));
    slot.cancel();
    assert!(matches!(
        b_cancel.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
}
#[test]
fn concurrent_forward_reservations_cannot_claim_the_same_listener() {
    let slot = std::sync::Arc::new(ForwardSlot::default());
    let watch = std::sync::Arc::new(WatchSlot::default());
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
    let attempts = (0..8)
        .map(|_| {
            let (slot, watch, barrier) = (slot.clone(), watch.clone(), barrier.clone());
            std::thread::spawn(move || {
                barrier.wait();
                slot.begin(0, 0, &watch, forward_target(8080)).is_ok()
            })
        })
        .collect::<Vec<_>>();
    let winners = attempts
        .into_iter()
        .map(|attempt| attempt.join().unwrap_or(false))
        .filter(|won| *won)
        .count();
    assert_eq!(winners, 1);
    assert_eq!(slot.active_count(), 1);
}
#[test]
fn forwards_have_bounded_history_and_stale_ids_cannot_overwrite_new_sessions() {
    let slot = ForwardSlot::default();
    let watch = WatchSlot::default();
    let generation = slot.generation();
    let mut handles = Vec::new();
    for port in 8080..8088 {
        handles.push(
            slot.begin(generation, 0, &watch, forward_target(port))
                .unwrap_or_else(|e| panic!("{e}")),
        );
    }
    assert_eq!(slot.active_count(), MAX_FORWARDS);
    assert!(
        slot.begin(generation, 0, &watch, forward_target(8090))
            .is_err()
    );
    let old = handles[0].0;
    slot.finish(old, Ok("old ended".into()));
    let (new, _cancel) = slot
        .begin(generation, 0, &watch, forward_target(8090))
        .unwrap_or_else(|e| panic!("{e}"));
    slot.finish(old, Err("stale failure".into()));
    assert!(slot.listening(old, "stale bind").is_err());
    assert_eq!(slot.len(), MAX_FORWARDS);
    assert_eq!(slot.active_count(), MAX_FORWARDS);
    assert!(slot.listening(new, "new bound").is_ok());
    assert!(!slot.text().contains("stale"));
}
#[test]
fn forward_view_selects_and_stops_only_highlighted_session() {
    let (mut app, _replies, _commands) = harness();
    let generation = app.worker.forward.generation();
    let watch = &app.worker.watch;
    let epoch = watch.epoch.load(std::sync::atomic::Ordering::SeqCst);
    let (_, mut a) = app
        .worker
        .forward
        .begin(generation, epoch, watch, forward_target(8080))
        .unwrap_or_else(|e| panic!("{e}"));
    let (_, mut b) = app
        .worker
        .forward
        .begin(generation, epoch, watch, forward_target(8081))
        .unwrap_or_else(|e| panic!("{e}"));
    assert!(app.command("pf").is_ok());
    assert!(app.key(KeyCode::Char('j')).is_ok());
    assert_eq!(app.forward_selected, 1);
    assert!(app.key(KeyCode::Char('s')).is_ok());
    assert!(matches!(
        a.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty)
    ));
    assert!(matches!(
        b.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
    app.changed();
    assert!(matches!(
        a.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Closed)
    ));
}
#[test]
fn hotkeys_run_commands_only_from_the_main_view() {
    use crate::hotkeys::{Hotkey, Shortcut};
    let (mut app, _replies, _commands) = harness();
    app.hotkeys = vec![
        Hotkey {
            shortcut: Shortcut::Shift(1),
            description: "Deployments".into(),
            command: "deploy".into(),
        },
        Hotkey {
            shortcut: Shortcut::F(5),
            description: "Nodes".into(),
            command: "no".into(),
        },
    ];
    assert!(
        app.key_event(KeyCode::Char('!'), KeyModifiers::NONE)
            .is_ok()
    );
    assert_eq!(app.kind(), "deployments.apps");
    assert!(
        app.key_event(KeyCode::Char(':'), KeyModifiers::NONE)
            .is_ok()
    );
    assert!(
        app.key_event(KeyCode::Char('!'), KeyModifiers::NONE)
            .is_ok()
    );
    assert!(app.key_event(KeyCode::F(5), KeyModifiers::NONE).is_ok());
    assert_eq!(
        app.kind(),
        "deployments.apps",
        "typing in the prompt never fires hotkeys"
    );
    assert!(matches!(&app.modal, Modal::Input("Command", text) if text == "!"));
    app.modal = Modal::None;
    assert!(app.key_event(KeyCode::F(5), KeyModifiers::NONE).is_ok());
    assert_eq!(app.kind(), "nodes");
    app.aliases();
    assert!(
        matches!(&app.modal, Modal::Picker(_, lines, _) if lines.iter().any(|line| line.contains("hotkey F5") && line.contains(":no")))
    );
}
#[test]
fn familiar_commands_complete_recall_and_keep_scoped_back_history() {
    let (mut app, _replies, _commands) = harness();
    app.aliases
        .insert("apis".into(), "v1/pods test /api|web".into());
    assert!(app.command("apis").is_ok());
    assert_eq!(
        (app.kind(), app.namespace.as_str(), app.filter.as_str()),
        ("pods".into(), "test", "api|web")
    );
    assert!(app.command("nodes").is_ok());
    assert!(app.key(KeyCode::Char('[')).is_ok());
    assert_eq!(app.filter, "api|web");
    assert!(app.key(KeyCode::Char(']')).is_ok());
    assert_eq!(app.kind(), "nodes");
    app.modal = Modal::None;
    assert!(app.key(KeyCode::Char(':')).is_ok());
    for c in "dep".chars() {
        assert!(app.key(KeyCode::Char(c)).is_ok());
    }
    assert!(screen(&app, 48, 12).join("\n").contains(":deploy"));
    assert!(app.key(KeyCode::Tab).is_ok());
    assert!(app.key(KeyCode::Enter).is_ok());
    assert_eq!(app.kind(), "deployments.apps");
    assert_eq!(app.history.last().map(String::as_str), Some("deploy"));
    assert!(app.key(KeyCode::Char(':')).is_ok());
    assert!(app.key(KeyCode::Char('é')).is_ok());
    assert!(app.key(KeyCode::Up).is_ok());
    assert!(matches!(&app.modal,Modal::Input("Command",text) if text=="deploy"));
    assert!(app.key(KeyCode::Down).is_ok());
    assert!(matches!(&app.modal,Modal::Input("Command",text) if text=="é"));
    assert!(app.key(KeyCode::Left).is_ok());
    assert!(app.key(KeyCode::Delete).is_ok());
    assert!(matches!(&app.modal,Modal::Input("Command",text) if text.is_empty()));
}
#[test]
fn explicit_core_gvr_is_unambiguous_and_discovered_aliases_are_usable() {
    let (mut app, _replies, _commands) = harness();
    let mut custom = app.kinds[0].clone();
    custom.group = "example.test".into();
    custom.short_names = vec!["custompo".into()];
    app.kinds.push(custom);
    assert_eq!(app.resolve_resource("pods"), Ok(0));
    assert_eq!(app.resolve_resource("po"), Ok(0));
    assert_eq!(app.resolve_resource("v1/pods"), Ok(0));
    assert_eq!(app.resolve_resource("custompo"), Ok(app.kinds.len() - 1));
    assert!(app.command("secret").is_ok());
    assert!(app.status.contains("excluded"));
    app.items = vec![object("api"), object("web"), object("worker")];
    app.filter = "api|web".into();
    assert_eq!(app.visible().len(), 2);
    app.filter = "!api|web".into();
    assert_eq!(
        app.visible().iter().map(|v| name(v)).collect::<Vec<_>>(),
        vec!["worker"]
    );
    app.filter = "-f wkr".into();
    assert_eq!(app.visible().len(), 1);
    app.filter = "[".into();
    assert!(app.visible().is_empty());
}
#[test]
fn drilldown_uses_node_fields_and_workload_labels_and_escape_restores_scope() {
    let (mut app, _replies, mut commands) = harness();
    app.command("nodes").unwrap_or(false);
    app.items = vec![
        serde_json::json!({"apiVersion":"v1","kind":"Node","metadata":{"name":"worker-a","uid":"node-uid"}}),
    ];
    assert!(app.key(KeyCode::Enter).is_ok());
    assert_eq!(
        (
            app.kind(),
            app.namespace.as_str(),
            app.field_selector.as_str()
        ),
        ("pods".into(), "*", "spec.nodeName=worker-a")
    );
    assert!(app.key(KeyCode::Esc).is_ok());
    assert_eq!(app.kind(), "nodes");
    assert!(app.field_selector.is_empty());
    app.command("deploy").unwrap_or(false);
    app.items = vec![
        serde_json::json!({"apiVersion":"apps/v1","kind":"Deployment","metadata":{"name":"api","namespace":"team","uid":"dep"},"spec":{"selector":{"matchLabels":{"app":"api"}}}}),
    ];
    assert!(app.key(KeyCode::Enter).is_ok());
    assert_eq!(
        (app.kind(), app.namespace.as_str(), app.selector.as_str()),
        ("pods".into(), "team", "app=api")
    );
    app.items = vec![
        serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"api-a","namespace":"team","uid":"pod"}}),
    ];
    assert!(app.key(KeyCode::Enter).is_ok());
    assert!(matches!(commands.try_recv(), Ok((_, Task::Drill(_, _, _)))));
}
#[test]
fn late_log_snapshot_is_rejected_and_static_marks_keep_original_lines() {
    let (mut app, replies, _commands) = harness();
    let old = app.worker.logs.cancel();
    let current = app.worker.logs.cancel();
    let mut view = crate::ui_logs::LogView::new(
        "current snapshot".into(),
        crate::logs::ReadOptions::default(),
    );
    view.following = false;
    app.modal = Modal::Follow(Box::new(view));
    assert!(
        replies
            .send((app.epoch, Reply::Logs(old, "obsolete".into())))
            .is_ok()
    );
    app.receive();
    assert!(!screen(&app, 80, 24).join("\n").contains("obsolete"));
    assert!(
        replies
            .send((app.epoch, Reply::Logs(current, "current snapshot".into())))
            .is_ok()
    );
    app.receive();
    assert!(app.key(KeyCode::Char('m')).is_ok());
    app.receive();
    let frame = screen(&app, 80, 24).join("\n");
    assert!(frame.contains("current snapshot") && frame.contains("log mark"));
    app.busy = true;
    assert!(app.key(KeyCode::Char('2')).is_ok());
    assert!(screen(&app, 80, 24).join("\n").contains("current snapshot"));
}
#[test]
fn responsive_frames_preserve_identity_mode_and_bounds() {
    let (mut app, _replies, _commands) = harness();
    app.items = vec![
        serde_json::json!({"kind":"Pod","metadata":{"name":"api","namespace":"test","uid":"pod1","creationTimestamp":"2026-10-10T07:00:00Z"},"spec":{"containers":[{"name":"app"}]},"status":{"phase":"Running","containerStatuses":[{"ready":true,"restartCount":4}]}}),
    ];
    for width in [28, 40, 60, 80, 88, 120] {
        for height in [10, 12, 24] {
            {
                let frame = screen(&app, width, height);
                assert!(frame[..2].join(" ").contains("PRODUCTION READ ONLY"));
                assert!(frame[1].contains("CTX test | NS test"));
                assert!(frame[3].starts_with(['+', '╭']));
            }
        }
    }
    let frame = screen(&app, 120, 24).join("\n");
    for heading in ["NAMESPACE", "NAME", "READY", "STATUS", "RSTS", "AGE"] {
        assert!(frame.contains(heading));
    }
    assert!(frame.contains("1/1") && frame.contains("Running"));
    app.options.connection.writable = true;
    assert!(screen(&app, 28, 10)[0].contains("PRODUCTION WRITE"));
    app.options.connection.demo = true;
    assert!(screen(&app, 28, 10)[0].contains("DEMO SAMPLE DATA"));
}
#[test]
fn resize_reflows_details_and_confirmation_cannot_approve_hidden_review() {
    let (mut app, _replies, _commands) = harness();
    let text = "ABCDEFGHIJ".repeat(7);
    app.modal = Modal::Detail(text.clone(), 0);
    let frame = screen(&app, 28, 12);
    let displayed: String = frame
        .iter()
        .skip(4)
        .take(4)
        .map(|line| {
            line.chars()
                .skip(1)
                .take(26)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect();
    assert!(displayed.starts_with(&text));
    assert!(confirmation_fits("Short review", "test/api", 80, 24));
    assert!(!confirmation_fits(
        &"long review ".repeat(30),
        "test/api",
        28,
        12
    ));
    assert!(!confirmation_fits("Review", "test/api", 20, 8));
    app.modal = Modal::Metrics("CPU: |.-#|\nMEM: |--#|".into(), 0);
    assert!(screen(&app, 60, 12).join("\n").contains("CPU: |.-#|"));
    app.modal = Modal::Containers(
        LogTarget {
            namespace: "test".into(),
            pod: "api".into(),
            container: None,
            uid: "uid".into(),
            previous: false,
            follow: false,
            all: false,
            dynamic: false,
            membership: None,
        },
        (0..20).map(|index| format!("container-{index}")).collect(),
        19,
    );
    assert!(screen(&app, 40, 12).join("\n").contains("> container-19"));
}
#[test]
fn commands_navigate_aliases_scopes_and_sort_without_issuing_mutations() {
    let (mut app, _replies, mut commands) = harness();
    let epoch = app.epoch;
    assert_eq!(app.command(":deploy"), Ok(false));
    assert_eq!(app.kind(), "deployments.apps");
    assert_eq!(app.epoch, epoch + 1);
    assert_eq!(app.command("ns other"), Ok(false));
    assert_eq!(app.kind(), "pods");
    assert_eq!(app.namespace, "other");
    assert_eq!(app.command("all"), Ok(false));
    assert_eq!(app.namespace, "*");
    assert_eq!(app.command("sort restarts"), Ok(false));
    assert_eq!(app.sort, Sort::Restarts);
    assert_eq!(app.command("pods; rm -rf /"), Ok(false));
    assert_eq!(app.kind(), "pods");
    assert!(commands.try_recv().is_err());
    assert_eq!(app.command("q"), Ok(true));
}
#[test]
fn readiness_restarts_age_and_sort_use_real_fields_and_keep_uid() {
    let (mut app, _replies, _commands) = harness();
    let mut pod = object("z");
    pod["kind"] = "Pod".into();
    pod["spec"] = serde_json::json!({"containers":[{"name":"app"},{"name":"sidecar"}]});
    pod["status"] = serde_json::json!({"containerStatuses":[{"ready":true,"restartCount":10},{"ready":false,"restartCount":2}],"initContainerStatuses":[{"restartCount":1}]});
    pod["metadata"]["creationTimestamp"] = "2026-10-09T07:00:00Z".into();
    let now = "2026-10-10T07:00:00Z"
        .parse::<k8s_openapi::jiff::Timestamp>()
        .map(|timestamp| timestamp.as_second())
        .unwrap_or(0);
    assert_eq!(readiness(&pod), "1/2");
    assert_eq!(restarts(&pod), 13);
    assert_eq!(age(&pod, now), "1d");
    assert_eq!(age(&pod, 0), "future");
    assert_eq!(age(&object("missing-time"), now), "-");
    app.items = vec![pod, object("a")];
    app.selected = 1;
    assert_eq!(app.selected_uid().as_deref(), Some("uid-z"));
    app.set_sort(Sort::Restarts);
    assert_eq!(app.selected, 0);
    assert_eq!(app.selected_uid().as_deref(), Some("uid-z"));
    assert_eq!(display("long-name", 5), "long~");
    assert_eq!(display("\u{1b}[31mname", 20), "[31mname");
}
#[test]
fn table_metrics_sort_exact_usage_and_missing_rows_last_preserving_uid() -> Result<()> {
    let (mut app, replies, mut commands) = harness();
    let pod = |name: &str, uid: &str| serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":"test","uid":uid},"spec":{"containers":[{"name":"app"}]}});
    app.items = vec![
        pod("big", "big-uid"),
        pod("small", "small-uid"),
        pod("unknown", "unknown-uid"),
    ];
    let row = |name: &str, uid: &str, cpu: u64, memory: u64| crate::metrics::MetricsRow {
        name: name.into(),
        namespace: Some("test".into()),
        container: Some("app".into()),
        timestamp: "2026-10-22T12:00:30Z".into(),
        window: "30s".into(),
        cpu_raw: format!("{cpu}n"),
        memory_raw: memory.to_string(),
        cpu_nanocores: cpu,
        cpu_millicores: crate::metrics::format_millicores(cpu),
        memory_bytes: memory,
        rounded_up: false,
        identity_uid: Some(uid.into()),
        allocations: None,
    };
    let snapshot = crate::metrics::MetricsSnapshot {
        api_version: "metrics.k8s.io/v1beta1",
        scope: crate::metrics::MetricsScope::Pods,
        namespace: Some("test".into()),
        source: "demo (test)",
        items: vec![
            row("big", "big-uid", 100_000_001, 2_097_152),
            row("small", "small-uid", 9_000_001, 67_108_864),
        ],
        truncated: false,
        allocation_status: None,
    };
    app.selected = 1;
    replies
        .send((
            app.epoch,
            Reply::Metrics(
                crate::metrics::MetricsScope::Pods,
                "test".into(),
                Ok(snapshot),
            ),
        ))
        .map_err(|_| "reply closed")?;
    app.receive();
    app.last_table_metrics = Some(Instant::now());
    app.key(KeyCode::Char('C'))?;
    assert_eq!(
        app.visible()
            .iter()
            .map(|object| name(object))
            .collect::<Vec<_>>(),
        vec!["big", "small", "unknown"]
    );
    assert_eq!(app.selected_uid().as_deref(), Some("small-uid"));
    app.key(KeyCode::Char('M'))?;
    assert_eq!(
        app.visible()
            .iter()
            .map(|object| name(object))
            .collect::<Vec<_>>(),
        vec!["small", "big", "unknown"]
    );
    let frame = screen(&app, 120, 24).join("\n");
    assert!(frame.contains("CPU(m)"));
    assert!(frame.contains("MEM"));
    assert!(frame.contains("9        "));
    assert!(frame.contains("64Mi"));
    let usage = table_metrics::Usage {
        cpu_nanocores: 72_574_100,
        memory_bytes: 1_268_680 * 1024,
        timestamp: "now".into(),
        window: "30s".into(),
        identity_uid: "small-uid".into(),
        identity_verified: false,
    };
    let compact = table_line_with_metrics(
        &[(Column::Cpu, 9), (Column::Memory, 9)],
        Some(&app.items[1]),
        0,
        Some(&usage),
    );
    assert_eq!(
        compact, "72.57~    1.21Gi~  ",
        "each tilde must be the UID marker, with units intact"
    );
    assert!(commands.try_recv().is_err());
    // Expiry reorders missing values by name but preserves the selected UID.
    let before = app.selected_uid();
    assert!(app.expire_table_metrics(Instant::now() + Duration::from_secs(46)));
    assert_eq!(app.selected_uid(), before);
    app.items[1]["metadata"]["uid"] = "new-uid".into();
    assert!(
        app.table_metrics
            .usage(&app.items[1], Instant::now())
            .is_none()
    );
    app.changed();
    assert!(app.table_metrics.label(Instant::now()).contains("loading"));
    Ok(())
}
#[test]
fn closed_metrics_view_does_not_reopen_on_late_reply() {
    let (mut app, replies, _commands) = harness();
    app.request_metrics();
    assert!(matches!(app.modal, Modal::Metrics(..)));
    assert!(app.key(KeyCode::Esc).is_ok());
    assert!(
        replies
            .send((
                12,
                Reply::Metrics(
                    crate::metrics::MetricsScope::Pods,
                    "test".into(),
                    Err("unavailable".into())
                )
            ))
            .is_ok()
    );
    app.receive();
    assert!(matches!(app.modal, Modal::None));
    assert!(!app.stale);
}
#[test]
fn theme_creation_captures_preview_palette_and_cancel_restores_previous_theme() {
    let (mut app, _replies, _commands) = harness();
    let previous = app.theme.clone();
    let palette = themes::builtins();
    app.theme = palette[1].clone();
    let preview = app.theme.clone();
    app.modal = Modal::Themes(palette, 1, previous.clone());
    assert!(app.key(KeyCode::Char('N')).is_ok());
    match &app.modal {
        Modal::NewTheme(id, source, old) => {
            assert!(id.is_empty());
            assert_eq!(source.id, preview.id);
            assert_eq!(old.id, previous.id);
        }
        _ => panic!("theme creation did not capture palette"),
    }
    assert!(app.key(KeyCode::Char('a')).is_ok());
    assert!(app.key(KeyCode::Char('-')).is_ok());
    assert!(app.key(KeyCode::Esc).is_ok());
    assert_eq!(app.theme.id, previous.id);
    assert!(matches!(app.modal, Modal::None));
}
#[test]
fn live_filter_keeps_table_visible_and_cancel_restores_selected_uid() {
    let (mut app, _replies, _commands) = harness();
    app.items = vec![object("alpha"), object("beta")];
    app.selected = 1;
    assert!(app.key(KeyCode::Char('/')).is_ok());
    assert!(app.key(KeyCode::Char('a')).is_ok());
    assert!(app.key(KeyCode::Char('l')).is_ok());
    assert_eq!(app.visible().len(), 1);
    let frame = screen(&app, 60, 12).join("\n");
    assert!(frame.contains("alpha") && frame.contains("Filter > al_"));
    app.replace_items(vec![object("aardvark"), object("alpha"), object("beta")]);
    assert!(app.key(KeyCode::Esc).is_ok());
    assert_eq!(app.filter, "");
    assert_eq!(app.selected_uid().as_deref(), Some("uid-beta"));
}
#[test]
fn long_context_does_not_hide_namespace_identity_and_actions_explain_capabilities() {
    let (mut app, _replies, _commands) = harness();
    app.context = "very-long-production-context-identifier".repeat(4);
    app.namespace = "payments-production-namespace".into();
    for width in [28, 40, 80, 120] {
        let frame = screen(&app, width, 12);
        assert!(frame[1].contains("CTX ") && frame[1].contains(" | NS "));
        assert!(frame[1].contains('~'));
    }
    app.items = vec![object("selected")];
    let actions = app.action_menu();
    assert!(actions[2].contains("[OFF]") && actions[2].contains("--allow-logs"));
    assert!(actions[7].contains("READ ONLY"));
    app.options.connection.writable = true;
    assert!(app.action_menu()[7].contains("Deployments"));
    app.kind_index = 1;
    assert!(!app.action_menu()[7].contains("[OFF]"));
}
#[test]
fn resource_table_uses_every_column_and_mouse_never_changes_resource() {
    let (mut app, _replies, _commands) = harness();
    app.items = vec![object("a"), object("b")];
    for (w, h) in [(28, 10), (48, 12), (120, 32)] {
        let layout = Layout::new(w, h);
        assert_eq!(layout.table.x, 0);
        assert_eq!(layout.table.width, w);
        let frame = screen(&app, w, h).join("\n");
        assert!(!frame.contains("RESOURCES [Tab]") && !frame.contains("WORKLOADS"));
    }
    app.mouse(
        MouseEvent {
            kind: MouseEventKind::Down(crossterm::event::MouseButton::Left),
            column: 3,
            row: 6,
            modifiers: KeyModifiers::NONE,
        },
        120,
        32,
    );
    assert_eq!(app.selected, 1);
    assert_eq!(app.kind(), "pods");
    app.selector = "app=api".into();
    let frame = screen(&app, 80, 24).join("\n");
    assert!(frame.contains("FILTERED") && frame.contains("LABEL:app=api"));
    app.selector.clear();
    app.field_selector = "spec.nodeName=worker-a".into();
    let frame = screen(&app, 120, 24).join("\n");
    assert!(frame.contains("FILTERED") && frame.contains("FIELD:spec.nodeName=worker-a"));
    assert!(screen(&app, 28, 10).join("\n").contains("FILTERED"));
}
#[test]
fn full_width_frame_shows_health_and_centered_actions_without_clear() {
    let (mut app, _replies, _commands) = harness();
    app.items = vec![
        serde_json::json!({"kind":"Pod","metadata":{"name":"api","namespace":"test","uid":"pod1"},"spec":{"nodeName":"worker-a","containers":[{"name":"app","image":"example/api:v2"}]},"status":{"phase":"Running","containerStatuses":[{"name":"app","ready":true,"restartCount":0}]}}),
    ];
    let frame = screen(&app, 120, 32).join("\n");
    for label in ["1 healthy", "0 need attention", "NAMESPACE", "api"] {
        assert!(frame.contains(label));
    }
    let mut output = Vec::new();
    assert!(render(&app, 120, 32, &mut output).is_ok());
    assert!(!output.windows(4).any(|window| window == b"\x1b[2J"));
    app.modal = Modal::Picker("Actions", app.action_menu(), 0);
    let rect = modal_rect(&app.modal, 120, 32);
    assert!(rect.x > 0 && rect.y > 3 && rect.width < 120);
    let frame = screen(&app, 120, 32).join("\n");
    assert!(frame.contains("[OFF]") && frame.contains("Interactive pod shell"));
}
#[test]
fn human_overview_uses_redacted_object_fields_and_json_remains_accessible() {
    let (mut app, replies, mut commands) = harness();
    let mut pod = serde_json::json!({"kind":"Pod","metadata":{"name":"api","namespace":"test","uid":"pod1","resourceVersion":"9"},"spec":{"nodeName":"worker-a","containers":[{"name":"app","image":"example/api:v2"}],"password":"canary-secret"},"status":{"phase":"Running","conditions":[{"type":"Ready","status":"True"}],"containerStatuses":[{"name":"app","ready":true,"restartCount":3}]}});
    safety::redact(&mut pod);
    let text = overview_text(&pod, "current-context");
    for label in [
        "IDENTITY",
        "CONTAINERS",
        "SCHEDULING",
        "worker-a",
        "example/api:v2",
        "Ready",
    ] {
        assert!(text.contains(label));
    }
    assert!(!text.contains("canary-secret"));
    app.items = vec![pod.clone()];
    assert!(app.key(KeyCode::Char('d')).is_ok());
    assert!(matches!(commands.try_recv(), Ok((_, Task::Inspect(..)))));
    assert!(replies.send((12, Reply::Object(Box::new(pod)))).is_ok());
    app.receive();
    assert!(matches!(app.modal, Modal::Overview(..)));
    assert!(app.key(KeyCode::Char('J')).is_ok());
    assert!(matches!(app.modal, Modal::Detail(..)));
}
#[test]
fn apply_requires_its_capability_and_queues_a_dry_run_review() {
    let (mut app, _replies, mut commands) = harness();
    assert!(app.command("apply deploy.yaml").is_ok());
    assert!(app.status.contains("--allow-apply"));
    assert!(commands.try_recv().is_err());
    app.options.connection.writable = true;
    app.options.connection.allow_apply = true;
    app.namespace = "shop".into();
    assert!(app.command("apply deploy.yaml").is_ok());
    match commands.try_recv() {
        Ok((_, Task::ApplyPrepare(path, namespace))) => {
            assert_eq!((path, namespace.as_str()), ("deploy.yaml".into(), "shop"))
        }
        _ => panic!(":apply did not request a dry-run review"),
    }
    app.busy = false;
    assert!(app.command("apply").is_ok());
    assert!(app.status.starts_with("Use :apply"));
}
#[test]
fn file_copy_commands_review_the_selected_pod_default_container() {
    let (mut app, _replies, mut commands) = harness();
    app.items = vec![
        serde_json::json!({"kind":"Pod","metadata":{"name":"api","namespace":"shop","uid":"api-uid","annotations":{"kubectl.kubernetes.io/default-container":"app"}},"spec":{"containers":[{"name":"sidecar"},{"name":"app"}]}}),
    ];
    assert!(app.command("cp-from /etc/hosts").is_ok());
    assert!(app.status.contains("--allow-file-copy"));
    assert!(commands.try_recv().is_err());
    app.options.connection.writable = true;
    app.options.connection.allow_file_copy = true;
    let dir = std::env::temp_dir();
    assert!(
        app.command(&format!("cp-from /etc/hosts {}", dir.display()))
            .is_ok()
    );
    match commands.try_recv() {
        Ok((
            _,
            Task::PrepareSession(
                namespace,
                pod,
                uid,
                crate::ops::Request::CopyFrom {
                    container,
                    remote,
                    local,
                },
            ),
        )) => assert_eq!(
            (
                namespace.as_str(),
                pod.as_str(),
                uid.as_deref(),
                container.as_str(),
                remote.as_str(),
                local
            ),
            (
                "shop",
                "api",
                Some("api-uid"),
                "app",
                "/etc/hosts",
                dir.clone()
            )
        ),
        _ => panic!("cp-from did not request a review"),
    }
    app.busy = false;
    assert!(app.command("cp-to /definitely/missing/file /tmp/x").is_ok());
    assert!(app.status.contains("cannot upload"));
    assert!(commands.try_recv().is_err());
    assert!(app.command("cp-to onlyone").is_ok());
    assert!(app.status.starts_with("Use :cp-from"));
}
#[test]
fn pod_shell_picker_preserves_captured_identity_and_capabilities() {
    let (mut app, _replies, mut commands) = harness();
    app.items = vec![
        serde_json::json!({"kind":"Pod","metadata":{"name":"first","namespace":"test","uid":"first-uid"},"spec":{"containers":[{"name":"app"}]}}),
    ];
    assert!(app.key(KeyCode::Char('E')).is_ok());
    assert!(matches!(app.modal, Modal::None));
    app.options.connection.writable = true;
    assert!(app.key(KeyCode::Char('E')).is_ok());
    assert!(matches!(app.modal, Modal::None));
    app.options.connection.allow_exec = true;
    assert!(app.key(KeyCode::Char('E')).is_ok());
    app.items = vec![object("replacement")];
    assert!(app.key(KeyCode::Enter).is_ok());
    match commands.try_recv() {
        Ok((
            _,
            Task::PrepareSession(
                namespace,
                pod,
                uid,
                crate::ops::Request::InteractiveExec {
                    container, seconds, ..
                },
            ),
        )) => {
            assert_eq!(
                (
                    namespace.as_str(),
                    pod.as_str(),
                    container.as_str(),
                    uid.as_deref(),
                    seconds
                ),
                ("test", "first", "app", Some("first-uid"), 900)
            );
        }
        _ => panic!("shell picker changed target"),
    }
}
#[test]
fn log_follow_defaults_to_newest_window_and_end_restores_tail() {
    let (mut app, _replies, _commands) = harness();
    let text = (0..100)
        .map(|index| format!("line-{index}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.modal = Modal::Follow(Box::new(crate::ui_logs::LogView::new(
        text,
        crate::logs::ReadOptions::default(),
    )));
    assert!(screen(&app, 60, 12).join("\n").contains("line-99"));
    assert!(app.key(KeyCode::Home).is_ok());
    assert!(screen(&app, 60, 12).join("\n").contains("line-0"));
    assert!(app.key(KeyCode::End).is_ok());
    assert!(matches!(app.modal,Modal::Follow(view) if view.autoscroll));
}
#[test]
fn live_context_overview_updates_and_close_rejects_late_feed() -> Result<()> {
    let rt = runtime()?;
    let create = || {
        rt.block_on(async {
            let feed = crate::overview::OverviewFeed::new(
                &Connection {
                    demo: true,
                    ..Default::default()
                },
                &["demo".into()],
            )?;
            tokio::time::sleep(Duration::from_millis(20)).await;
            Ok::<_, String>(feed)
        })
    };
    let (mut app, replies, _commands) = harness();
    app.modal = Modal::LiveOverview(dashboard_ui::Dashboard::new(&["demo".into()], true));
    assert!(replies.send((12, Reply::Overview(create()?))).is_ok());
    app.receive();
    assert!(app.overview.is_some());
    let frame = screen(&app, 80, 24).join("\n");
    assert!(frame.contains("demo"));
    assert!(frame.contains("2 Pods"));
    assert!(frame.contains("NEEDS ATTENTION"));
    assert!(frame.contains("WARNINGS / LAST 15m"));
    assert!(!frame.contains(":pods"));
    app.key(KeyCode::Esc)?;
    assert!(app.overview.is_none());
    assert!(replies.send((12, Reply::Overview(create()?))).is_ok());
    app.receive();
    assert!(app.overview.is_none() && matches!(app.modal, Modal::None));
    Ok(())
}
#[test]
fn overview_input_accepts_long_names_and_four_full_utf8_names() {
    let (mut app, _replies, mut commands) = harness();
    let names = vec!["a".repeat(150), "b".repeat(160)];
    app.modal = Modal::Input(OVERVIEW_CONTEXTS_LABEL, String::new());
    for c in names.join(",").chars() {
        assert!(app.key(KeyCode::Char(c)).is_ok());
    }
    assert!(app.key(KeyCode::Enter).is_ok());
    match commands.try_recv() {
        Ok((_, Task::Overview(_, entered))) => assert_eq!(entered, names),
        _ => panic!("long overview names were not accepted"),
    }
    app.busy = false;
    app.command_epoch = None;
    let names: Vec<_> = ['a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i']
        .iter()
        .map(|prefix| format!("{prefix}{}", "é".repeat(126)))
        .collect();
    let text = names.join(",");
    assert!(names.iter().all(|name| name.len() == 253));
    assert_eq!(text.len(), OVERVIEW_INPUT_BYTES);
    app.modal = Modal::Input(OVERVIEW_CONTEXTS_LABEL, String::new());
    for c in text.chars() {
        assert!(app.key(KeyCode::Char(c)).is_ok());
    }
    assert!(app.key(KeyCode::Char('x')).is_ok());
    match &app.modal {
        Modal::Input(_, entered) => assert_eq!(entered, &text),
        _ => panic!("input modal changed"),
    }
    assert!(app.key(KeyCode::Enter).is_ok());
    match commands.try_recv() {
        Ok((_, Task::Overview(_, entered))) => assert_eq!(entered, names),
        _ => panic!("four maximum-length overview names were not accepted"),
    }
    app.busy = false;
    app.modal = Modal::Input(OVERVIEW_CONTEXTS_LABEL, "a,b,c,d,e,f,g,h,i,j".into());
    assert!(app.key(KeyCode::Enter).is_ok());
    assert!(commands.try_recv().is_err());
    assert!(app.status.contains("up to 9"));
}
#[test]
fn dashboard_focus_help_and_drill_preserve_explicit_scope() {
    let (mut app, _replies, mut commands) = harness();
    app.namespace = "selected-space".into();
    app.namespace_override = false;
    assert!(app.key(KeyCode::Char('D')).is_ok());
    match commands.try_recv() {
        Ok((_, Task::Overview(connection, names))) => {
            assert_eq!(connection.namespace, "selected-space");
            assert_eq!(names, vec![app.context.clone()]);
        }
        _ => panic!("dashboard did not open independent reads"),
    }
    app.busy = false;
    app.command_epoch = None;
    app.modal = Modal::LiveOverview(dashboard_ui::Dashboard::new(
        &["local".into(), "other cluster".into()],
        false,
    ));
    assert!(app.key(KeyCode::Char('2')).is_ok());
    assert!(app.key(KeyCode::Char('?')).is_ok());
    assert!(
        matches!(&app.modal, Modal::LiveOverview(dashboard) if dashboard.help && dashboard.selected == 1)
    );
    assert!(screen(&app, 100, 28).join("\n").contains("DASHBOARD KEYS"));
    assert!(app.key(KeyCode::Esc).is_ok());
    assert!(
        matches!(&app.modal, Modal::LiveOverview(dashboard) if !dashboard.help && dashboard.selected == 1)
    );
    app.filter = "old-filter".into();
    app.selector = "app=old".into();
    assert!(app.key(KeyCode::Enter).is_ok());
    match commands.try_recv() {
        Ok((_, Task::Connect(connection))) => {
            assert_eq!(connection.context.as_deref(), Some("other cluster"));
        }
        _ => panic!("focused context was not opened"),
    }
    assert_eq!(app.namespace, "selected-space");
    assert!(app.namespace_override);
    assert!(app.filter.is_empty() && app.selector.is_empty());
    assert_eq!(app.desired_resource.as_deref(), Some("v1/pods"));
    assert!(app.overview.is_none());
}
#[test]
fn dashboard_log_pane_follows_exact_warning_pod_and_closes_first() -> Result<()> {
    let (mut app, replies, mut commands) = harness();
    app.options.connection.logs = true;
    app.options.connection.writable = true;
    app.options.connection.allow_force_kill = true;
    let mut row = crate::overview::ContextSummary::connecting("other cluster".into(), false);
    row.state = crate::dashboard_health::EvidenceState::Live;
    row.health = Some(crate::dashboard_health::HealthSummary {
        ready: 1,
        unready: 1,
        pending: 0,
        failed: 0,
        succeeded: 0,
        terminating: 0,
        unknown: 0,
        restarts: 3,
        oom_killed: 0,
        restart_alerts: Vec::new(),
        restart_alert_count: 0,
        warnings: crate::dashboard_health::WarningSummary {
            state: crate::dashboard_health::EvidenceState::Live,
            count: Some(1),
            recent: vec![crate::dashboard_health::WarningEvidence {
                namespace: "payments".into(),
                pod: "api-7d9f".into(),
                uid: "0f0b21f0-af23-4c1e-9a77-3546a8e1e804".into(),
                reason: "BackOff".into(),
                occurrences: 4,
                observed_unix_seconds: 1,
            }],
            message: None,
        },
    });
    let mut dashboard = dashboard_ui::Dashboard::new(&["other cluster".into()], false);
    dashboard.update(vec![row]);
    app.modal = Modal::LiveOverview(dashboard);
    app.key(KeyCode::Char('L'))?;
    assert!(commands.try_recv().is_err());
    app.key(KeyCode::Char('w'))?;
    app.key(KeyCode::Char('L'))?;
    let generation = match commands.try_recv() {
        Ok((_, Task::DashboardLogs(connection, ns, pod, uid, generation, _, _))) => {
            assert_eq!(connection.context.as_deref(), Some("other cluster"));
            assert!(!connection.writable && !connection.allow_force_kill);
            assert_eq!(
                (ns.as_str(), pod.as_str(), uid.as_str()),
                (
                    "payments",
                    "api-7d9f",
                    "0f0b21f0-af23-4c1e-9a77-3546a8e1e804"
                )
            );
            generation
        }
        _ => return Err("dashboard log task not sent".into()),
    };
    replies
        .send((app.epoch, Reply::Following))
        .map_err(|_| "reply closed")?;
    app.receive();
    assert!(app.worker.logs.append(generation, "api-7d9f/app: boom"));
    app.receive();
    let text = screen(&app, 120, 40).join("\n");
    assert!(text.contains("LOGS other cluster / payments/api-7d9f") && text.contains("boom"));
    app.key(KeyCode::Esc)?;
    assert!(
        matches!(&app.modal, Modal::LiveOverview(d) if d.logs.is_none() && d.warning.is_some())
    );
    assert!(!app.worker.logs.append(generation, "late"));
    Ok(())
}
#[test]
fn dashboard_tail_lists_every_cluster_read_only_and_merges_selected_workloads() -> Result<()> {
    let (mut app, replies, mut commands) = harness();
    app.options.connection.logs = true;
    app.options.connection.writable = true;
    app.namespace = "shop".into();
    let names = vec!["east".to_owned(), "west".to_owned()];
    app.modal = Modal::LiveOverview(dashboard_ui::Dashboard::new(&names, false));
    app.key(KeyCode::Char('t'))?;
    match commands.try_recv() {
        Ok((_, Task::DashboardWorkloads(contexts))) => {
            assert_eq!(contexts.len(), 2);
            for (context, connection) in &contexts {
                assert_eq!(connection.context.as_deref(), Some(context.as_str()));
                assert!(!connection.writable && !connection.allow_exec);
                assert_eq!(connection.namespace, "shop");
            }
        }
        _ => return Err("workload listing not requested".into()),
    }
    let workload = |context: &str, name: &str| crate::workload_tail::WorkloadRef {
        context: context.into(),
        resource: "deployments".into(),
        namespace: "shop".into(),
        name: name.into(),
        uid: format!("uid-{context}-{name}"),
    };
    replies
        .send((
            app.epoch,
            Reply::DashboardWorkloads(vec![
                ("east".into(), Ok(vec![workload("east", "api")])),
                ("west".into(), Ok(vec![workload("west", "api")])),
            ]),
        ))
        .map_err(|_| "reply closed")?;
    app.receive();
    assert!(
        screen(&app, 120, 40)
            .join("\n")
            .contains("west/shop/deploy/api")
    );
    app.key(KeyCode::Char(' '))?;
    app.key(KeyCode::Down)?;
    app.key(KeyCode::Char(' '))?;
    app.key(KeyCode::Enter)?;
    let generation = match commands.try_recv() {
        Ok((_, Task::DashboardTail(sources, generation, _, _))) => {
            assert_eq!(
                sources
                    .iter()
                    .map(|s| s.workload.context.as_str())
                    .collect::<Vec<_>>(),
                ["east", "west"]
            );
            assert!(sources.iter().all(|s| !s.connection.writable
                && s.connection.context.as_deref() == Some(s.workload.context.as_str())));
            generation
        }
        _ => return Err("tail not started".into()),
    };
    replies
        .send((app.epoch, Reply::Following))
        .map_err(|_| "reply closed")?;
    app.receive();
    assert!(app.worker.logs.append(
        generation,
        "2026-10-24T10:00:00Z [east] [shop/api-1:app] ERROR boom"
    ));
    assert!(app.worker.logs.append(
        generation,
        "2026-10-24T10:00:01Z [west] [shop/api-2:app] INFO ok"
    ));
    app.receive();
    let text = screen(&app, 120, 40).join("\n");
    assert!(text.contains("TAIL 2 workloads across 2 clusters") && text.contains("[west]"));
    app.key(KeyCode::Char('e'))?;
    let text = screen(&app, 120, 40).join("\n");
    assert!(text.contains("ERROR boom") && !text.contains("INFO ok"));
    app.key(KeyCode::Char('t'))?;
    assert!(matches!(&app.modal, Modal::LiveOverview(d) if d.logs.is_none()));
    assert!(!app.worker.logs.append(generation, "late"));
    Ok(())
}
/// Every foreground the UI paints, on every surface it paints it on:
/// plain background, the 3% header surface, 6% raised rows and the 2%/5%
/// dashboard card tints. Status and hint text must stay at WCAG AA 4.5:1;
/// selection text is checked on its own background.
#[test]
fn every_builtin_theme_keeps_rendered_text_legible_on_every_surface() {
    let mut failures = Vec::new();
    for theme in themes::all().unwrap_or_default() {
        if theme.terminal {
            continue;
        }
        let background = theme.get("background");
        let text = theme.get("text");
        for (surface, tint) in [
            ("background", 0),
            ("card", 2),
            ("header", 3),
            ("focused card", 5),
            ("raised", 6),
        ] {
            let base = blend(background, text, tint);
            for role in ["text", "muted", "accent", "success", "warning", "danger"] {
                let ratio = themes::contrast(theme.get(role), base);
                if ratio < 4.5 {
                    failures.push(format!("{} {role} on {surface}: {ratio:.2}", theme.id));
                }
            }
        }
        let ratio = themes::contrast(
            theme.get("selection_text"),
            theme.get("selection_background"),
        );
        if ratio < 4.5 {
            failures.push(format!("{} selection: {ratio:.2}", theme.id));
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
#[test]
fn oversized_scope_falls_back_to_bounded_polling_instead_of_an_empty_table() -> Result<()> {
    let (mut app, replies, _commands) = harness();
    app.connected = true;
    app.worker.watch.publish(
        app.epoch,
        crate::watch::WatchSnapshot {
            objects: Vec::new(),
            resource_version: String::new(),
            stale: true,
            message: "watch cache limit exceeded; narrow namespace scope".into(),
            terminal: true,
            truncated: true,
        },
    );
    app.receive();
    assert!(app.bounded_poll && !app.watch_blocked && app.refresh);
    let items = (0..1000)
        .map(|i| object(&format!("cfg-{i:04}")))
        .collect::<Vec<_>>();
    replies
        .send((
            app.epoch,
            Reply::Items(serde_json::json!({"items": items, "truncated": true})),
        ))
        .map_err(|_| "reply closed")?;
    app.receive();
    assert_eq!(app.items.len(), 1000);
    assert!(app.truncated && app.status.contains("Showing the first 1000 objects"));
    app.changed();
    assert!(!app.bounded_poll);
    Ok(())
}
#[test]
fn visible_order_is_memoized_and_invalidated_by_filter_sort_and_versions() {
    let (mut app, _replies, _commands) = harness();
    let pod = |name: &str, restarts: u64, rv: &str| serde_json::json!({"kind":"Pod","metadata":{"name":name,"namespace":"default","uid":format!("uid-{name}"),"resourceVersion":rv},"status":{"phase":"Running","containerStatuses":[{"restartCount":restarts}]}});
    app.items = vec![pod("b", 1, "1"), pod("a", 5, "1"), pod("c", 3, "1")];
    let names = |app: &App| {
        app.visible()
            .iter()
            .map(|o| name(o).to_owned())
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&app), ["a", "b", "c"]);
    assert!(std::rc::Rc::ptr_eq(
        &app.visible_order(),
        &app.visible_order()
    ));
    app.sort = Sort::Restarts;
    assert_eq!(names(&app), ["a", "c", "b"]);
    app.items[2] = pod("c", 9, "2");
    app.items_generation += 1;
    assert_eq!(names(&app), ["c", "a", "b"]);
    app.filter = "default [ac] ".into();
    assert_eq!(names(&app), ["c", "a"]);
    app.filter = "(".into();
    assert!(names(&app).is_empty());
}
#[test]
fn table_hints_show_only_actions_that_work_for_this_kind_and_session() -> Result<()> {
    let (mut app, _replies, _commands) = harness();
    let keys = |app: &App| {
        table_hints(app)
            .into_iter()
            .map(|(key, _)| key)
            .collect::<Vec<_>>()
    };
    app.desired_resource = Some("pods".into());
    let read_only = keys(&app);
    assert!(read_only.contains(&"?") && read_only.contains(&"Enter"));
    for hidden in ["l", "s", "Ctrl-D", "Ctrl-K", "e"] {
        assert!(!read_only.contains(&hidden), "{hidden}");
    }
    app.options.connection.logs = true;
    app.options.connection.writable = true;
    app.options.connection.allow_exec = true;
    app.options.connection.allow_force_kill = true;
    let pods = keys(&app);
    for shown in ["l", "s", "Ctrl-D", "Ctrl-K"] {
        assert!(pods.contains(&shown), "{shown}");
    }
    app.desired_resource = Some("deployments.apps".into());
    let deployments = table_hints(&app);
    assert!(deployments.contains(&("s", "scale")) && deployments.contains(&("r", "restart")));
    assert!(!deployments.iter().any(|(key, _)| *key == "Ctrl-K"));
    app.desired_resource = Some("nodes".into());
    assert!(!keys(&app).contains(&"c"));
    app.options.connection.allow_node_maintenance = true;
    assert!(table_hints(&app).contains(&("c", "cordon")));
    app.modal = Modal::None;
    let bottom = screen(&app, 120, 24).pop().unwrap_or_default();
    assert!(bottom.contains("? all keys") && bottom.contains("c cordon"));
    Ok(())
}
#[test]
fn secrets_are_opt_in_hidden_by_default_and_reveal_one_key() -> Result<()> {
    let (mut app, replies, mut commands) = harness();
    app.namespace = "demo".into();
    app.command("secrets")?;
    assert!(app.status.contains("--allow-secret-reveal"));
    assert!(commands.try_recv().is_err());
    app.options.connection.allow_secret_reveal = true;
    app.command("secrets")?;
    assert!(matches!(commands.try_recv(), Ok((_, Task::Secrets(ns))) if ns == "demo"));
    let data = crate::secrets::demo_data();
    replies
        .send((
            app.epoch,
            Reply::Secrets(crate::secrets::SecretIndex {
                namespace: "demo".into(),
                items: vec![crate::secrets::entry("api", "u1", None, Some(&data))],
                truncated: false,
            }),
        ))
        .map_err(|_| "reply closed")?;
    app.receive();
    app.key(KeyCode::Enter)?;
    app.key(KeyCode::Char('x'))?;
    assert!(matches!(
        commands.try_recv(),
        Ok((_, Task::SecretValue(ns, name, uid, key)))
            if (ns.as_str(), name.as_str(), uid.as_str(), key.as_str()) == ("demo", "api", "u1", "blob.bin")
    ));
    assert!(
        !screen(&app, 120, 30)
            .join("\n")
            .contains("not-a-real-password")
    );
    replies
        .send((
            app.epoch,
            Reply::SecretValue(
                "u1".into(),
                "blob.bin".into(),
                Ok(crate::secrets::reveal(&data["blob.bin"])),
            ),
        ))
        .map_err(|_| "reply closed")?;
    app.receive();
    assert!(screen(&app, 120, 30).join("\n").contains("binary, 4 bytes"));
    app.key(KeyCode::Esc)?;
    app.key(KeyCode::Esc)?;
    assert!(matches!(app.modal, Modal::None));
    Ok(())
}
#[test]
fn dashboard_warning_opens_exact_pod_events_and_esc_clears_selection_first() {
    let (mut app, _replies, mut commands) = harness();
    app.namespace = "*".into();
    let mut row = crate::overview::ContextSummary::connecting("other cluster".into(), false);
    row.state = crate::dashboard_health::EvidenceState::Live;
    row.namespace = "*".into();
    row.health = Some(crate::dashboard_health::HealthSummary {
        ready: 1,
        unready: 1,
        pending: 0,
        failed: 0,
        succeeded: 0,
        terminating: 0,
        unknown: 0,
        restarts: 3,
        oom_killed: 0,
        restart_alerts: Vec::new(),
        restart_alert_count: 0,
        warnings: crate::dashboard_health::WarningSummary {
            state: crate::dashboard_health::EvidenceState::Live,
            count: Some(1),
            recent: vec![crate::dashboard_health::WarningEvidence {
                namespace: "payments".into(),
                pod: "api-7d9f".into(),
                uid: "0f0b21f0-af23-4c1e-9a77-3546a8e1e804".into(),
                reason: "BackOff".into(),
                occurrences: 4,
                observed_unix_seconds: 1,
            }],
            message: None,
        },
    });
    let mut dashboard = dashboard_ui::Dashboard::new(&["other cluster".into()], false);
    dashboard.update(vec![row]);
    app.modal = Modal::LiveOverview(dashboard);
    assert!(app.key(KeyCode::Char('w')).is_ok());
    assert!(
        screen(&app, 120, 30)
            .join("\n")
            .contains("> BackOff / payments/api-7d9f")
    );
    assert!(app.key(KeyCode::Esc).is_ok());
    assert!(matches!(&app.modal, Modal::LiveOverview(dashboard) if dashboard.warning.is_none()));
    assert!(app.key(KeyCode::Char('w')).is_ok());
    assert!(app.key(KeyCode::Enter).is_ok());
    match commands.try_recv() {
        Ok((_, Task::Connect(connection))) => {
            assert_eq!(connection.context.as_deref(), Some("other cluster"));
        }
        _ => panic!("warning context was not opened"),
    }
    assert_eq!(app.namespace, "payments");
    assert_eq!(app.desired_resource.as_deref(), Some("v1/events"));
    assert_eq!(
        app.field_selector,
        crate::selectors::pod_events_field("api-7d9f", "0f0b21f0-af23-4c1e-9a77-3546a8e1e804")
    );
    assert!(app.overview.is_none());
}
#[test]
fn overview_input_enforces_byte_boundary_and_keeps_active_tail_visible() {
    let (mut app, _replies, _commands) = harness();
    app.modal = Modal::Input(
        OVERVIEW_CONTEXTS_LABEL,
        "x".repeat(OVERVIEW_INPUT_BYTES - 1),
    );
    assert!(app.key(KeyCode::Char('é')).is_ok()); // Two bytes cannot cross the remaining one-byte budget.
    match &app.modal {
        Modal::Input(_, text) => assert_eq!(text.len(), OVERVIEW_INPUT_BYTES - 1),
        _ => panic!("input modal changed"),
    }
    assert!(app.key(KeyCode::Char('x')).is_ok());
    assert!(app.key(KeyCode::Backspace).is_ok());
    assert!(app.key(KeyCode::Backspace).is_ok());
    assert!(app.key(KeyCode::Char('é')).is_ok());
    match &app.modal {
        Modal::Input(_, text) => {
            assert_eq!(text.len(), OVERVIEW_INPUT_BYTES);
            assert!(text.ends_with('é'));
        }
        _ => panic!("input modal changed"),
    }
    let text = format!("{},ACTIVE-TAIL", "a".repeat(155));
    app.modal = Modal::Input(OVERVIEW_CONTEXTS_LABEL, text.clone());
    for (width, height) in [(28, 10), (48, 12), (80, 24), (120, 32)] {
        let rect = modal_rect(&app.modal, width, height);
        let lines = input_lines(
            OVERVIEW_CONTEXTS_LABEL,
            &text,
            usize::from(rect.width - 2),
            usize::from(rect.height - 2),
        );
        assert!(lines.join("").ends_with("ACTIVE-TAIL_"));
        assert!(screen(&app, width, height).join("\n").contains("TAIL_"));
    }
    assert_eq!(
        input_lines("Command", "abcdefgh", 10, 1),
        vec!["_".to_owned()]
    );
    app.modal = Modal::Input("Namespace", "x".repeat(99));
    assert!(app.key(KeyCode::Char('é')).is_ok());
    match &app.modal {
        Modal::Input(_, text) => assert_eq!(text.len(), 99),
        _ => panic!("namespace modal changed"),
    }
    assert_eq!(input_limit("Command"), nav::COMMAND_BYTES);
    assert_eq!(input_limit("Namespace"), 100);
}
#[test]
fn metrics_extra_resource_reads_require_explicit_uppercase_key() {
    let (mut app, _replies, mut commands) = harness();
    assert!(app.key(KeyCode::Char('m')).is_ok());
    assert!(matches!(
        commands.try_recv(),
        Ok((_, Task::Metrics(_, _, false)))
    ));
    app.busy = false;
    app.command_epoch = None;
    app.modal = Modal::None;
    assert!(app.command("metrics resources").is_ok());
    assert!(matches!(
        commands.try_recv(),
        Ok((_, Task::Metrics(_, _, true)))
    ));
    assert!(
        screen(&app, 80, 24)
            .join("\n")
            .contains("extra reads enabled")
    );
}
#[test]
fn resource_budget_form_captures_uid_and_validates_before_preparation() {
    let (mut app, _replies, mut commands) = harness();
    app.kind_index = 1;
    app.items = vec![object("first")];
    assert!(app.key(KeyCode::Char('V')).is_ok());
    assert!(matches!(app.modal, Modal::None));
    app.options.connection.writable = true;
    assert!(app.key(KeyCode::Char('V')).is_ok());
    app.items = vec![object("replacement")];
    for character in "app 100m 128Mi 500m 512Mi".chars() {
        assert!(app.key(KeyCode::Char(character)).is_ok());
    }
    assert!(app.key(KeyCode::Enter).is_ok());
    match commands.try_recv() {
        Ok((
            _,
            Task::Prepare(
                _,
                namespace,
                name,
                uid,
                Action::SetResources {
                    container,
                    resources,
                },
            ),
        )) => {
            assert_eq!(
                (namespace.as_str(), name.as_str(), uid.as_deref()),
                ("test", "first", Some("uid-first"))
            );
            assert_eq!(container, "app");
            assert_eq!(
                (
                    resources.cpu_request.as_str(),
                    resources.memory_request.as_str(),
                    resources.cpu_limit.as_str(),
                    resources.memory_limit.as_str()
                ),
                ("100m", "128Mi", "500m", "512Mi")
            );
        }
        _ => panic!("resource budget form changed target"),
    }
    app.busy = false;
    app.modal = Modal::Resources(
        "deployments.apps".into(),
        "test".into(),
        "first".into(),
        Some("uid-first".into()),
        "app 2 1Gi 1 512Mi".into(),
    );
    assert!(app.key(KeyCode::Enter).is_ok());
    assert!(matches!(app.modal, Modal::Resources(..)));
    assert!(commands.try_recv().is_err());
}
#[test]
fn log_mailbox_is_bounded_and_cancel_discards_obsolete_producer() {
    let slot = LogSlot::default();
    let generation = slot.cancel();
    let line = "x".repeat(4096);
    for _ in 0..1000 {
        if !slot.append(generation, &line) {
            break;
        }
    }
    let text = slot.take().unwrap_or_default();
    assert!(text.len() <= crate::logs::MAX_LOG_BYTES);
    assert!(text.contains("older lines discarded"));
    assert!(slot.append(generation, "newest retained line"));
    assert!(
        slot.take()
            .unwrap_or_default()
            .ends_with("newest retained line\n")
    );
    slot.cancel();
    assert!(!slot.append(generation, "obsolete private log"));
    assert!(slot.take().is_none());
}
#[test]
fn escape_cancels_log_modal_and_scope_change_rejects_old_logs() {
    let (mut app, _replies, _commands) = harness();
    let generation = app.worker.logs.cancel();
    app.modal = Modal::Follow(Box::new(crate::ui_logs::LogView::new(
        "live".into(),
        crate::logs::ReadOptions::default(),
    )));
    assert!(app.key(KeyCode::Esc).is_ok());
    assert!(matches!(app.modal, Modal::None));
    assert!(!app.worker.logs.append(generation, "old"));
    let generation = app.worker.logs.cancel();
    app.changed();
    assert!(!app.worker.logs.append(generation, "different scope"));
}
#[test]
fn image_form_captures_target_uid_and_readonly_never_opens_it() {
    let (mut app, _replies, mut commands) = harness();
    app.kind_index = 1;
    app.items = vec![object("first")];
    assert!(app.command("image").is_ok());
    assert!(matches!(app.modal, Modal::None));
    app.options.connection.writable = true;
    assert!(app.command("image").is_ok());
    app.items = vec![object("replacement")];
    for c in "app".chars() {
        assert!(app.key(KeyCode::Char(c)).is_ok());
    }
    assert!(app.key(KeyCode::Enter).is_ok());
    for c in "nginx:1.30".chars() {
        assert!(app.key(KeyCode::Char(c)).is_ok());
    }
    assert!(app.key(KeyCode::Enter).is_ok());
    match commands.try_recv() {
        Ok((_, Task::Prepare(_, ns, name, uid, Action::SetImage { container, image }))) => {
            assert_eq!(
                (ns.as_str(), name.as_str(), uid.as_deref()),
                ("test", "first", Some("uid-first"))
            );
            assert_eq!((container.as_str(), image.as_str()), ("app", "nginx:1.30"));
        }
        _ => panic!("image form changed selected target"),
    }
}
fn snapshot(name: &str) -> crate::watch::WatchSnapshot {
    crate::watch::WatchSnapshot {
        objects: vec![object(name)],
        resource_version: "rv".into(),
        stale: false,
        message: "LIVE".into(),
        terminal: false,
        truncated: false,
    }
}
#[test]
fn mutation_and_session_reviews_never_approve_lossy_unicode_or_overflow() -> Result<()> {
    let summary = "context: 東京\\u{e9}\nDirect argv: [\"echo\",\"é\"]";
    let lines = confirmation_lines(summary, "test/api", 79);
    let displayed = lines.iter().map(|line| &line[1..]).collect::<String>();
    assert!(displayed.is_ascii());
    assert!(displayed.contains("\\u{6771}\\u{4eac}\\\\u{e9}"));
    assert!(displayed.contains("\\u{e9}"));
    assert!(!confirmation_fits(summary, "test/api", 28, 10));
    assert!(confirmation_fits(summary, "test/api", 120, 40));
    assert!(!confirmation_fits(
        &"é".repeat(200_000),
        "test/api",
        65535,
        65535
    ));
    // Real data cell rendering cannot silently replace the escaped characters.
    assert_eq!(display(&displayed, displayed.len()), displayed);
    Ok(())
}

#[test]
fn confirmation_wrap_preserves_complete_target_and_effect() {
    let summary = format!(
        "target: {}/sample\n{}",
        "a".repeat(63),
        "effect ".repeat(20)
    );
    let lines = confirmation_lines(&summary, "typed-target", 80);
    assert!(lines.iter().all(|line| line.chars().count() <= 79));
    let displayed: String = lines.iter().map(|line| &line[1..]).collect();
    assert!(displayed.contains(&summary.replace('\n', "")));
    assert!(displayed.contains("CONFIRM > typed-target_"));
}
#[test]
fn write_outcome_survives_epoch_and_watch_updates_until_acknowledged() {
    let (mut app, replies, _commands) = harness();
    app.busy = true;
    app.command_epoch = Some(11);
    app.write_inflight = true;
    assert!(
        replies
            .send((
                11,
                Reply::Failure("Mutation outcome UNKNOWN; reconcile before retrying".into())
            ))
            .is_ok()
    );
    assert!(app.receive());
    assert!(app.status.contains("outcome UNKNOWN"));
    assert!(app.stale && app.sticky_status);
    assert!(!app.busy && !app.write_inflight);
    app.worker.watch.publish(12, snapshot("fresh"));
    app.receive();
    assert!(app.status.contains("outcome UNKNOWN"));
    assert!(app.key(KeyCode::Char('?')).is_ok());
    assert!(!app.sticky_status);
}
#[test]
fn namespace_changed_during_connect_is_not_overwritten_and_old_session_is_rejected() {
    let (mut app, replies, _commands) = harness();
    app.connected = false;
    app.busy = true;
    app.command_epoch = Some(12);
    app.connecting_epoch = Some(12);
    app.modal = Modal::Input("Namespace", "different".into());
    assert!(app.key(KeyCode::Enter).is_ok());
    assert_eq!(app.epoch, 13);
    assert!(
        replies
            .send((
                11,
                Reply::Connected(
                    "obsolete".into(),
                    "wrong".into(),
                    "wrong".into(),
                    crate::gateway::kinds()
                )
            ))
            .is_ok()
    );
    app.receive();
    assert_eq!(app.context, "test");
    assert!(app.busy);
    assert!(
        replies
            .send((
                12,
                Reply::Connected(
                    "current".into(),
                    "server".into(),
                    "kubeconfig-default".into(),
                    crate::gateway::kinds()
                )
            ))
            .is_ok()
    );
    app.receive();
    assert_eq!(app.context, "current");
    assert_eq!(app.namespace, "different");
    // Rows come first: the watch is requested, then discovery follows it.
    assert!(app.connected && !app.busy && app.refresh && app.pending_discovery);
}
#[test]
fn obsolete_watch_is_dropped_and_revocation_clears_the_view() {
    let (mut app, _replies, _commands) = harness();
    app.items = vec![object("old")];
    app.changed();
    app.worker.watch.publish(12, snapshot("obsolete"));
    assert!(!app.receive());
    assert!(app.items.is_empty());
    app.worker.watch.publish(13, snapshot("current"));
    assert!(app.receive());
    assert_eq!(app.items[0]["metadata"]["name"], "current");
    let mut denied = snapshot("discard");
    denied.objects.clear();
    denied.stale = true;
    denied.terminal = true;
    app.worker.watch.publish(13, denied);
    app.receive();
    assert!(app.items.is_empty() && app.stale && app.watch_blocked && !app.watching);
}
#[test]
fn producer_burst_coalesces_without_waiting_for_ui_consumption() {
    let mailbox = std::sync::Arc::new(WatchSlot::default());
    mailbox.invalidate(7);
    let producer = mailbox.clone();
    let task = std::thread::spawn(move || {
        for n in 0..1000 {
            producer.publish(7, snapshot(&n.to_string()));
        }
    });
    assert!(task.join().is_ok());
    let last = mailbox
        .take()
        .unwrap_or_else(|| panic!("missing latest snapshot"));
    assert_eq!(last.0, 7);
    assert_eq!(last.1.objects[0]["metadata"]["name"], "999");
    assert!(mailbox.take().is_none());
}
#[test]
fn watch_insertion_preserves_identity_and_scale_form_captures_name_and_uid() {
    let (mut app, _replies, mut commands) = harness();
    app.options.connection.writable = true;
    app.kind_index = 1;
    app.items = vec![object("b")];
    assert!(app.command("scale").is_ok());
    app.replace_items(vec![object("a"), object("b")]);
    assert_eq!(app.selected, 1);
    // Even if the original row disappears, the form must not prepare a different row.
    app.replace_items(vec![object("a")]);
    assert!(app.key(KeyCode::Char('2')).is_ok());
    assert!(app.key(KeyCode::Enter).is_ok());
    match commands.try_recv() {
        Ok((_, Task::Prepare(_, ns, name, uid, Action::Scale(2)))) => {
            assert_eq!(ns, "test");
            assert_eq!(name, "b");
            assert_eq!(uid.as_deref(), Some("uid-b"));
        }
        _ => panic!("scale form changed its selected target"),
    }
}
#[test]
fn background_replies_never_unlock_an_in_flight_write() -> Result<()> {
    let (mut app, replies, mut commands) = harness();
    app.busy = true;
    app.write_inflight = true;
    app.command_epoch = Some(app.epoch);
    // A metrics refresh may still be queued behind the write...
    app.send(Task::Metrics(
        crate::metrics::MetricsScope::Pods,
        "test".into(),
        false,
    ));
    assert!(matches!(commands.try_recv(), Ok((_, Task::Metrics(..)))));
    // ...but its reply must not end the write's busy state.
    replies
        .send((
            app.epoch,
            Reply::Metrics(
                crate::metrics::MetricsScope::Pods,
                "test".into(),
                Err("Metrics API unavailable".into()),
            ),
        ))
        .map_err(|_| "reply closed")?;
    app.receive();
    assert!(app.busy && app.write_inflight);
    Ok(())
}
#[test]
fn missing_metrics_does_not_invalidate_resource_data() {
    let (mut app, replies, _commands) = harness();
    app.last_table_metrics = Instant::now().checked_sub(Duration::from_secs(30));
    app.items = vec![object("a")];
    // Metrics refreshes are background work: they never hold the busy gate.
    app.modal = Modal::Metrics("loading".into(), 0);
    assert!(
        replies
            .send((
                12,
                Reply::Metrics(
                    crate::metrics::MetricsScope::Pods,
                    "test".into(),
                    Err("Metrics API unavailable".into())
                )
            ))
            .is_ok()
    );
    app.receive();
    assert!(
        app.last_table_metrics
            .is_some_and(|last| last.elapsed() < Duration::from_secs(1)),
        "retry quiet period must begin after the failed response, not request start"
    );
    assert!(!app.busy);
    assert_eq!(app.items.len(), 1);
    assert!(!app.stale);
    assert!(matches!(app.modal, Modal::Metrics(..)));
}
#[test]
fn metrics_wrapping_preserves_long_identity_usage_and_measurement_time() {
    let text = format!(
        "test/{} container 125.5 12345678 2026-10-09T22:00:00Z 15s",
        "a".repeat(63)
    );
    let wrapped = wrap_text(&text, 48);
    assert!(wrapped.lines().all(|line| line.chars().count() <= 48));
    assert_eq!(wrapped.replace('\n', ""), text);
}
#[test]
fn reauthentication_keeps_context_namespace_and_resource_identity() {
    let (mut app, replies, mut commands) = harness();
    app.kinds.swap(0, 10);
    app.kind_index = 0;
    app.watch_blocked = true;
    assert_eq!(app.kind(), "configmaps");
    assert!(app.key(KeyCode::Char('r')).is_ok());
    let epoch = app.epoch;
    match commands.try_recv() {
        Ok((e, Task::Connect(options))) => {
            assert_eq!(e, epoch);
            assert_eq!(options.context.as_deref(), Some("test"));
        }
        _ => panic!("reauthentication did not pin the current context"),
    }
    assert!(
        replies
            .send((
                epoch,
                Reply::Connected(
                    "test".into(),
                    "server".into(),
                    "default".into(),
                    crate::gateway::kinds()
                )
            ))
            .is_ok()
    );
    app.receive();
    assert_eq!(app.namespace, "test");
    assert_eq!(app.kind(), "configmaps");
    assert!(app.refresh && !app.watch_blocked);
}
#[test]
fn saved_selection_rejects_mode_mismatch_without_changing_scope() -> Result<()> {
    let (mut app, _, _) = harness();
    let before = (app.epoch, app.namespace.clone(), app.context.clone());
    let saved = crate::selections::Selection {
        name: "Offline dashboard".into(),
        demo: !app.options.connection.demo,
        target: crate::selections::Target::Dashboard {
            contexts: vec!["demo".into()],
            namespace: "demo".into(),
        },
    };
    assert!(app.open_selection(saved).is_err());
    assert_eq!(
        before,
        (app.epoch, app.namespace.clone(), app.context.clone())
    );
    Ok(())
}

#[test]
fn saved_scope_must_exist_in_destination_catalog_before_refresh() {
    let (mut app, _, _) = harness();
    app.selection_ui.restoring = Some(crate::selections::Selection {
        name: "Unavailable custom kind".into(),
        demo: true,
        target: crate::selections::Target::Resource {
            context: "demo".into(),
            namespace: "test".into(),
            kind: "widgets.example.test".into(),
            label_selector: String::new(),
            workload: None,
        },
    });
    app.finish_saved_selection_restore();
    assert!(!app.refresh && app.watch_blocked);
    assert!(app.status.contains("unavailable"));
}

#[test]
fn saved_workload_waits_for_fresh_list_and_rejects_replacement_uid() {
    let (mut app, _, _) = harness();
    app.selection_ui.focus = Some(crate::selections::Workload {
        namespace: "test".into(),
        name: "api".into(),
        uid: "old-uid".into(),
    });
    app.items =
        vec![serde_json::json!({"metadata":{"namespace":"test","name":"api","uid":"new-uid"}})];
    app.stale = true;
    assert!(!app.finish_saved_workload_focus());
    assert!(app.selection_ui.focus.is_some());
    app.stale = false;
    assert!(app.finish_saved_workload_focus());
    assert!(
        matches!(app.modal, Modal::Detail(ref text, _) if text.contains("SAVED WORKLOAD UNAVAILABLE"))
    );
    assert!(!app.write_inflight);
}

#[test]
fn scope_change_clears_pending_saved_workload_identity() {
    let (mut app, _, _) = harness();
    app.selection_ui.focus = Some(crate::selections::Workload {
        namespace: "test".into(),
        name: "api".into(),
        uid: "old-uid".into(),
    });
    app.changed();
    assert!(app.selection_ui.focus.is_none());
}

#[test]
fn saved_picker_keeps_its_actions_when_watch_status_changes() {
    let (mut app, _, _) = harness();
    app.modal = Modal::Picker(
        selections_ui::PICKER_LABEL,
        vec!["Local workspace | Dashboard | 1 context".into()],
        0,
    );
    app.status = "Watch updated: 12 resources".into();
    for (width, height) in [(120, 34), (48, 16)] {
        let mut frame = Vec::new();
        render(&app, width, height, &mut frame).unwrap_or_else(|error| panic!("{error}"));
        let frame = String::from_utf8(frame).unwrap_or_else(|error| panic!("{error}"));
        assert!(frame.contains("Enter open | d remove | Esc back"));
        assert!(frame.contains("Watch updated"));
    }
}
