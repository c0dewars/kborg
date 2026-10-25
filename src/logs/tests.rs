//! Unit tests for the log follow, merge and membership modules.
use super::dynamic::membership_changes;
use super::rolling::LINE_NOTICE;
use super::*;
use crate::{Result, safety};
use futures_util::io::{AsyncBufRead, AsyncReadExt, Cursor};
use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use std::{collections::VecDeque, pin::Pin, sync::Arc, task::Poll, time::Duration};
use tokio::time::Instant;

fn workload_membership(containers: usize) -> Result<LogMembership> {
    LogMembership::new(
        "default",
        "selector app=web; fixed membership".into(),
        ["one", "two"]
            .into_iter()
            .map(|name| LogPod {
                name: name.into(),
                uid: format!("uid-{name}"),
                containers: (0..containers).map(|index| format!("c{index}")).collect(),
            })
            .collect(),
        Vec::new(),
        "connection",
    )
}
#[test]
fn workload_membership_is_complete_connection_bound_and_caps_pods_and_sources() -> Result<()> {
    let membership = workload_membership(2)?;
    assert_eq!(membership.pods().len(), 2);
    assert_eq!(membership.source_count(), 4);
    assert!(membership.verify_connection("connection").is_ok());
    assert!(membership.verify_connection("other").is_err());
    let json = serde_json::to_string(&membership).map_err(|_| "membership JSON")?;
    assert!(!json.contains("connection"));
    assert!(json.contains("uid-one"));
    let pod = LogPod {
        name: "one".into(),
        uid: "uid".into(),
        containers: vec!["main".into()],
    };
    assert!(
        LogMembership::new(
            "*",
            "scope".into(),
            vec![pod.clone()],
            Vec::new(),
            "connection"
        )
        .is_err()
    );
    assert!(
        LogMembership::new(
            "default",
            "scope".into(),
            vec![pod.clone(), pod],
            Vec::new(),
            "connection"
        )
        .is_err()
    );
    for (count, containers, valid) in [(8, 4, true), (9, 1, false), (8, 5, false), (1, 17, false)] {
        let pods = (0..count)
            .map(|n| LogPod {
                name: format!("pod-{n}"),
                uid: format!("uid-{n}"),
                containers: (0..containers).map(|n| format!("c{n}")).collect(),
            })
            .collect();
        assert_eq!(
            LogMembership::new("default", "scope".into(), pods, Vec::new(), "connection").is_ok(),
            valid
        );
    }
    let mut bytes = MEMBERSHIP_BYTES - 2;
    assert!(membership_bytes(&mut bytes, &"data").is_err());
    assert!(bytes <= MEMBERSHIP_BYTES);
    Ok(())
}
#[test]
fn dynamic_changes_are_exact_uid_departures_and_joins() -> Result<()> {
    let current = workload_membership(1)?;
    let next = LogMembership::new(
        "default",
        "selector app=web; fixed membership".into(),
        vec![
            LogPod {
                name: "two".into(),
                uid: "uid-two".into(),
                containers: vec!["c0".into()],
            },
            LogPod {
                name: "one".into(),
                uid: "uid-replacement".into(),
                containers: vec!["c0".into()],
            },
        ],
        Vec::new(),
        "connection",
    )?;
    let (left, joined) = membership_changes(&current, &next);
    assert_eq!(
        left.iter().map(|pod| pod.uid.as_str()).collect::<Vec<_>>(),
        ["uid-one"]
    );
    assert_eq!(
        joined
            .iter()
            .map(|pod| pod.uid.as_str())
            .collect::<Vec<_>>(),
        ["uid-replacement"],
        "a same-name replacement is a departure plus a join"
    );
    let dynamic = current.into_dynamic(PodLogSelection::Selector("app=web".into()));
    assert!(dynamic.is_dynamic());
    assert!(dynamic.description().contains("dynamic membership"));
    let json = serde_json::to_string(&dynamic).map_err(|_| "membership JSON")?;
    assert!(!json.contains("app=web\"") && !json.contains("Selector"));
    Ok(())
}
#[test]
fn dynamic_merge_closes_departed_sources_admits_joins_and_stays_open() -> Result<()> {
    runtime()?.block_on(async {
        let membership =
            workload_membership(1)?.into_dynamic(PodLogSelection::Selector("app=web".into()));
        let source = |text: &str| Ok(LogFeed::new(Cursor::new(text.as_bytes().to_vec())));
        let mut feed = MergedLogFeed::from_membership_sources(
            &membership,
            vec![
                ("one".into(), "c0".into(), source("one-record\n")),
                ("two".into(), "c0".into(), source("two-record\n")),
            ],
            Instant::now() + Duration::from_secs(10),
            Vec::new(),
        )?;
        feed.keep_open = true;
        assert!(
            feed.next()
                .await?
                .is_some_and(|line| line.contains("dynamic membership opted in"))
        );
        for _ in 0..2 {
            feed.next().await?;
        }
        assert_eq!(feed.remove_pod("default", "one"), 1);
        assert_eq!(feed.remove_pod("default", "one"), 0);
        feed.add_sources(vec![("default/three:c0".into(), source("three-record\n"))])?;
        assert!(
            feed.add_sources(vec![("default/three:c0".into(), source("again\n"))])
                .is_err(),
            "duplicate sources are refused"
        );
        let mut seen = Vec::new();
        while let Ok(Ok(Some(line))) =
            tokio::time::timeout(Duration::from_millis(200), feed.next()).await
        {
            seen.push(line);
        }
        assert!(
            seen.iter()
                .any(|line| line.contains("[default/two:c0] two-record"))
        );
        assert!(
            seen.iter()
                .any(|line| line.contains("[default/three:c0] three-record"))
        );
        assert!(!seen.iter().any(|line| line.contains("one-record")));
        assert!(
            !feed.ended,
            "dynamic sessions wait for joins after sources end"
        );
        let many = (0..MAX_WORKLOAD_LOG_SOURCES)
            .map(|index| (format!("default/many:c{index}"), source("x\n")))
            .collect();
        assert!(
            feed.add_sources(many).is_err(),
            "the 32-source bound is global"
        );
        Ok(())
    })
}
#[test]
fn multi_pod_streams_are_attributed_fair_and_have_one_global_tail() -> Result<()> {
    runtime()?.block_on(async {
        let membership = workload_membership(1)?;
        let sources = vec![
            (
                "one".into(),
                "c0".into(),
                Ok(LogFeed::new(Cursor::new(
                    b"2026-10-22T12:00:01Z one-first\n2026-10-22T12:00:03Z one-last\n".to_vec(),
                ))),
            ),
            (
                "two".into(),
                "c0".into(),
                Ok(LogFeed::new(Cursor::new(
                    b"2026-10-22T12:00:02Z two-first\n2026-10-22T12:00:04Z two-last\n".to_vec(),
                ))),
            ),
        ];
        let mut feed = MergedLogFeed::from_membership_sources(
            &membership,
            sources,
            Instant::now() + Duration::from_secs(10),
            Vec::new(),
        )?;
        for expected in ["fixed membership", "uid-one", "uid-two"] {
            assert!(
                feed.next()
                    .await?
                    .is_some_and(|line| line.contains(expected))
            );
        }
        assert!(
            feed.next()
                .await?
                .is_some_and(|line| line.contains("[default/one:c0] one-first"))
        );
        assert!(
            feed.next()
                .await?
                .is_some_and(|line| line.contains("[default/two:c0] two-first"))
        );
        let text = feed
            .snapshot(ReadOptions {
                tail_lines: 2,
                ..Default::default()
            })
            .await?;
        let data = text
            .lines()
            .filter(|line| line.starts_with("2026-"))
            .collect::<Vec<_>>();
        assert_eq!(data.len(), 2);
        assert!(data[0].ends_with("one-last"));
        assert!(data[1].ends_with("two-last"));
        assert!(text.len() <= MAX_LOG_BYTES);
        Ok(())
    })
}
#[test]
fn multi_pod_partial_source_errors_survive_without_hiding_other_pod_data() -> Result<()> {
    runtime()?.block_on(async {
        let membership = workload_membership(1)?;
        let sources = vec![
            (
                "one".into(),
                "c0".into(),
                Err("previous logs unavailable (Kubernetes API 400)".into()),
            ),
            (
                "two".into(),
                "c0".into(),
                Ok(LogFeed::new(Cursor::new(
                    b"2026-10-22T12:00:00Z survivor\n".to_vec(),
                ))),
            ),
        ];
        let text = MergedLogFeed::from_membership_sources(
            &membership,
            sources,
            Instant::now() + Duration::from_secs(10),
            Vec::new(),
        )?
        .snapshot(ReadOptions::default())
        .await?;
        assert!(text.contains("[Log stream default/one:c0: previous logs unavailable"));
        assert!(text.contains("[default/two:c0] survivor"));
        assert!(
            MergedLogFeed::from_membership_sources(
                &membership,
                Vec::new(),
                Instant::now(),
                Vec::new()
            )
            .is_err()
        );
        Ok(())
    })
}
#[test]
fn multi_pod_global_flood_guard_and_drop_cancel_all_sources() -> Result<()> {
    runtime()?.block_on(async {
        let membership = workload_membership(1)?;
        let data = "line\n".repeat(3000);
        let sources = membership
            .pods()
            .iter()
            .map(|pod| {
                (
                    pod.name.clone(),
                    "c0".into(),
                    Ok(LogFeed::new(Cursor::new(data.as_bytes().to_vec()))),
                )
            })
            .collect();
        let mut feed = MergedLogFeed::from_membership_sources(
            &membership,
            sources,
            Instant::now() + Duration::from_secs(10),
            Vec::new(),
        )?;
        loop {
            match feed.next().await {
                Ok(Some(_)) => {}
                Ok(None) => return Err("multi-Pod flood guard missing".into()),
                Err(error) => {
                    assert!(error.contains("shared 5000 lines"));
                    break;
                }
            }
        }
        assert_eq!(feed.next().await?, None);
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sources = membership
            .pods()
            .iter()
            .map(|pod| {
                (
                    pod.name.clone(),
                    "c0".into(),
                    LogFeed::with_options(
                        futures_util::io::BufReader::new(QuietDrop(dropped.clone())),
                        FollowOptions::default(),
                    ),
                )
            })
            .collect();
        let mut feed = MergedLogFeed::from_membership_sources(
            &membership,
            sources,
            Instant::now() + Duration::from_secs(10),
            Vec::new(),
        )?;
        for _ in 0..3 {
            feed.next().await?;
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(5), feed.next())
                .await
                .is_err()
        );
        drop(feed);
        assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 2);
        Ok(())
    })
}
#[test]
fn quiet_multi_pod_follow_verifies_pod_and_workload_uids_without_stream_activity() -> Result<()> {
    runtime()?.block_on(async {
        for parent in [false,true] {
            let membership=workload_membership(1)?;
            let (api,requests)=resume_api(vec![(200,serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"namespace":"default","name":"one","uid":"replacement"}}).to_string())]);
            let guard=if parent {IdentityGuard::Parent{api:Api::namespaced_with(api.into_client(),"default",&kube::api::ApiResource::erase::<k8s_openapi::api::apps::v1::Deployment>(&())),name:"one".into(),uid:"uid-one".into()}}else{IdentityGuard::Pod{api,name:"one".into(),uid:"uid-one".into()}};
            let dropped=Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let sources=membership.pods().iter().map(|pod|(pod.name.clone(),"c0".into(),LogFeed::with_options(futures_util::io::BufReader::new(QuietDrop(dropped.clone())),FollowOptions::default()))).collect();
            let mut feed=MergedLogFeed::from_membership_sources(&membership,sources,Instant::now()+Duration::from_secs(10),vec![guard])?;
            if !parent {for _ in 0..3 {feed.next().await?;}}
            feed.next_identity_check=Some(Instant::now()+if parent{Duration::ZERO}else{Duration::from_millis(5)});
            let error=feed.next().await.err().ok_or("quiet membership replacement was ignored")?;
            assert!(error.contains("changed")||error.contains("replaced"));assert!(error.contains("all fixed-membership"));
            assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst),2);
            assert_eq!(requests.lock().map_err(|_|"requests")?.len(),1);
            assert_eq!(feed.next().await?,None);
        }
        Ok(())
    })
}
#[test]
fn workload_guard_deletion_is_fatal_and_transient_verification_is_bounded() -> Result<()> {
    runtime()?.block_on(async {
        for code in [404,503] {
            let membership=workload_membership(1)?;
            let status=serde_json::json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":code,"reason":"Unavailable","message":"CANARY private server error"}).to_string();
            let (api,requests)=resume_api(vec![(code,status.clone()),(code,status.clone()),(code,status)]);
            let guard=IdentityGuard::Pod{api,name:"one".into(),uid:"uid-one".into()};
            let dropped=Arc::new(std::sync::atomic::AtomicUsize::new(0));
            let sources=membership.pods().iter().map(|pod|(pod.name.clone(),"c0".into(),LogFeed::with_options(futures_util::io::BufReader::new(QuietDrop(dropped.clone())),FollowOptions::default()))).collect();
            let mut feed=MergedLogFeed::from_membership_sources(&membership,sources,Instant::now()+Duration::from_secs(30),vec![guard])?;
            if code==503 {
                for _ in 0..2 {feed.next_identity_check=Some(Instant::now());let notice=feed.next().await?.ok_or("transient guard ended early")?;assert!(notice.contains("temporarily unavailable")&&!notice.contains("CANARY"));assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst),0);}
            }
            feed.next_identity_check=Some(Instant::now());
            let error=feed.next().await.err().ok_or("guard failure did not stop")?;
            assert!(!error.contains("CANARY"));assert!(error.contains("404")||error.contains("three times"));
            assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst),2);
            assert_eq!(requests.lock().map_err(|_|"requests")?.len(),if code==503{3}else{1});
        }
        Ok(())
    })
}
#[test]
fn multi_pod_open_checks_every_uid_after_open_before_delivering_any_source() -> Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    runtime()?.block_on(async {
        for replace in [false,true] {
            let membership=workload_membership(1)?;
            let opens=Arc::new(AtomicUsize::new(0));let calls=opens.clone();
            let requests:Requests=Default::default();let recorded=requests.clone();
            let service=tower::service_fn(move|request:http::Request<kube::client::Body>|{
                let path=request.uri().path();recorded.lock().unwrap_or_else(|e|panic!("{e}")).push(request.uri().to_string());
                let text=if path.ends_with("/log") {
                    calls.fetch_add(1,Ordering::SeqCst);
                    format!("2026-10-22T12:00:00Z {}\n",if path.contains("/one/"){ "source-one" }else{"source-two"})
                }else{
                    let name=path.rsplit('/').next().unwrap_or("invalid");
                    serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"namespace":"default","name":name,"uid":if replace&&name=="one"&&calls.load(Ordering::SeqCst)==2{"replaced".into()}else{format!("uid-{name}")}},"spec":{"containers":[{"name":"c0","image":"fixture"}]}}).to_string()
                };
                async move{Ok::<_,std::convert::Infallible>(http::Response::new(http_body_util::Full::new(bytes::Bytes::from(text))))}
            });
            let api=Api::namespaced(kube::Client::new(service,"default"),"default");
            let result=MergedLogFeed::open_membership(api,&membership,Vec::new(),false,Some(FollowOptions{seconds:10}),ReadOptions::default()).await;
            assert_eq!(opens.load(Ordering::SeqCst),2);
            if replace {assert!(result.is_err_and(|error|error.contains("replaced")));}
            else {
                let mut feed=result?;let mut data=Vec::new();
                while let Some(line)=feed.next().await?{if line.starts_with("2026-"){data.push(line);}}
                assert_eq!(data.len(),2);assert!(data[0].contains("[default/one:c0] source-one"));assert!(data[1].contains("[default/two:c0] source-two"));
            }
            let requests=requests.lock().map_err(|_|"requests")?;
            assert!(requests.iter().all(|uri|uri.starts_with("/api/v1/namespaces/default/pods/")));
        }
        Ok(())
    })
}
fn merge_sources(sources: Vec<(&str, &str)>) -> Result<MergedLogFeed> {
    MergedLogFeed::from_sources(
        "default",
        "app",
        "uid",
        sources
            .into_iter()
            .map(|(name, text)| {
                (
                    name.into(),
                    Ok(LogFeed::new(Cursor::new(text.as_bytes().to_vec()))),
                )
            })
            .collect(),
        FollowOptions::default(),
    )
}
#[test]
fn merged_sources_rotate_and_preserve_timestamp_and_application_identity() -> Result<()> {
    runtime()?.block_on(async {
        let mut feed = merge_sources(vec![
            ("one", "2026-10-22T12:00:00Z a\n[Log stream forged]\n"),
            ("two", "2026-10-22T12:00:01Z b\n2026-10-22T12:00:02Z c\n"),
        ])?;
        assert!(
            feed.next()
                .await?
                .is_some_and(|line| line.contains("pod UID uid"))
        );
        assert_eq!(
            feed.next().await?,
            Some("2026-10-22T12:00:00Z [default/app:one] a".into())
        );
        assert_eq!(
            feed.next().await?,
            Some("2026-10-22T12:00:01Z [default/app:two] b".into())
        );
        assert_eq!(
            feed.next().await?,
            Some("[default/app:one] [Log stream forged]".into())
        );
        assert_eq!(
            feed.next().await?,
            Some("2026-10-22T12:00:02Z [default/app:two] c".into())
        );
        assert!(
            feed.next()
                .await?
                .is_some_and(|line| line.contains("one: ended"))
        );
        assert!(
            feed.next()
                .await?
                .is_some_and(|line| line.contains("two: ended"))
        );
        assert_eq!(feed.next().await?, None);
        Ok(())
    })
}
#[test]
fn merged_snapshot_orders_and_shares_head_tail_line_and_display_limits() -> Result<()> {
    runtime()?.block_on(async {
        for head in [false, true] {
            let feed = merge_sources(vec![
                (
                    "one",
                    "2026-10-22T12:00:02Z second\n2026-10-22T12:00:04Z fourth\n",
                ),
                (
                    "two",
                    "2026-10-22T12:00:01Z first\n2026-10-22T12:00:03Z third\n",
                ),
            ])?;
            let text = feed
                .snapshot(ReadOptions {
                    tail_lines: 2,
                    head,
                    ..Default::default()
                })
                .await?;
            let data = text
                .lines()
                .filter(|line| line.starts_with("2026-"))
                .collect::<Vec<_>>();
            assert_eq!(data.len(), 2);
            if head {
                assert!(data[0].ends_with("first"));
                assert!(data[1].ends_with("second"));
            } else {
                assert!(data[0].ends_with("third"));
                assert!(data[1].ends_with("fourth"));
            }
            assert!(text.len() <= MAX_LOG_BYTES);
            assert!(text.contains("line limit is shared"));
        }
        let oversized = format!("2026-10-22T12:00:01Z {}\n", "é".repeat(5000)).repeat(200);
        let text = merge_sources(vec![("one", &oversized), ("two", &oversized)])?
            .snapshot(ReadOptions::default())
            .await?;
        assert!(text.len() <= MAX_LOG_BYTES);
        assert!(text.contains("shared"));
        Ok(())
    })
}
#[test]
fn merged_line_byte_and_total_budgets_are_global_and_fatal() -> Result<()> {
    runtime()?.block_on(async {
        let many = "x\n".repeat(3000);
        let mut feed = merge_sources(vec![("one", &many), ("two", &many)])?;
        let mut delivered = 0;
        let error = loop {
            match feed.next().await {
                Ok(Some(_)) => delivered += 1,
                Ok(None) => return Err("shared line guard failed".into()),
                Err(error) => break error,
            }
        };
        assert!(error.contains("shared 5000 lines"));
        assert!(delivered <= MAX_RATE_LINES + 1);
        assert_eq!(feed.next().await?, None);
        let bytes = format!("{}\n", "x".repeat(3999)).repeat(160);
        let mut feed = merge_sources(vec![("one", &bytes), ("two", &bytes)])?;
        let error = loop {
            match feed.next().await {
                Ok(Some(_)) => {}
                Ok(None) => return Err("shared byte rate guard failed".into()),
                Err(error) => break error,
            }
        };
        assert!(error.contains("shared 1 MiB/second"));
        assert_eq!(feed.next().await?, None);
        let mut feed = merge_sources(vec![("one", "abc\n"), ("two", "def\n")])?;
        feed.budget.lock().map_err(|_| "test budget")?.total_bytes =
            crate::transport::LOG_STREAM_BYTES - 2;
        assert!(feed.next().await?.is_some());
        assert!(
            feed.next()
                .await
                .is_err_and(|error| error.contains("shared 64 MiB"))
        );
        assert_eq!(feed.next().await?, None);
        Ok(())
    })
}
struct QuietDrop(Arc<std::sync::atomic::AtomicUsize>);
impl Drop for QuietDrop {
    fn drop(&mut self) {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
}
impl futures_util::io::AsyncRead for QuietDrop {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &mut [u8],
    ) -> Poll<std::io::Result<usize>> {
        Poll::Pending
    }
}
fn quiet_merge(dropped: Arc<std::sync::atomic::AtomicUsize>) -> Result<MergedLogFeed> {
    MergedLogFeed::from_sources(
        "default",
        "app",
        "uid",
        ["one", "two"]
            .into_iter()
            .map(|name| {
                (
                    name.into(),
                    LogFeed::with_options(
                        futures_util::io::BufReader::new(QuietDrop(dropped.clone())),
                        FollowOptions::default(),
                    ),
                )
            })
            .collect(),
        FollowOptions::default(),
    )
}
#[test]
fn merged_cancel_and_shared_quiet_deadline_drop_every_pending_reader() -> Result<()> {
    runtime()?.block_on(async {
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut feed = quiet_merge(dropped.clone())?;
        assert!(feed.next().await?.is_some());
        assert!(
            tokio::time::timeout(Duration::from_millis(5), feed.next())
                .await
                .is_err()
        );
        drop(feed);
        assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 2);
        let mut feed = quiet_merge(dropped.clone())?;
        feed.deadline = Instant::now() + Duration::from_millis(5);
        assert!(feed.next().await?.is_some());
        assert!(
            feed.next()
                .await
                .is_err_and(|error| error.contains("deadline"))
        );
        assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 4);
        assert_eq!(feed.next().await?, None);
        Ok(())
    })
}
#[test]
fn merged_reconnect_replacement_stops_and_drops_every_source() -> Result<()> {
    runtime()?.block_on(async {
        let (api, _) = resume_api(vec![(200, metadata("replacement"))]);
        let dropped = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let first = broken_feed(api, "2026-10-22T12:00:00Z first\n")?;
        let quiet = LogFeed::with_options(
            futures_util::io::BufReader::new(QuietDrop(dropped.clone())),
            FollowOptions::default(),
        )?;
        let mut feed = MergedLogFeed::from_sources(
            "default",
            "app",
            "uid",
            vec![("one".into(), Ok(first)), ("two".into(), Ok(quiet))],
            FollowOptions::default(),
        )?;
        assert!(feed.next().await?.is_some());
        assert!(
            feed.next()
                .await?
                .is_some_and(|line| line.ends_with(" first"))
        );
        assert!(
            feed.next()
                .await?
                .is_some_and(|line| line.contains("interrupted"))
        );
        assert!(feed.next().await.is_err_and(
            |error| error.contains("replaced") && error.contains("all merged streams stopped")
        ));
        assert_eq!(dropped.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert_eq!(feed.next().await?, None);
        Ok(())
    })
}
fn merge_pod(uid: &str, names: &[&str]) -> String {
    serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"namespace":"default","name":"app","uid":uid},"spec":{"containers":names.iter().map(|name| serde_json::json!({"name":name,"image":"fixture"})).collect::<Vec<_>>(),"initContainers":[{"name":"init","image":"fixture"}],"ephemeralContainers":[{"name":"debug","image":"fixture"}]} }).to_string()
}
#[test]
fn merged_scope_source_count_and_uid_are_validated_before_reading_logs() -> Result<()> {
    let pod: Pod =
        serde_json::from_str(&merge_pod("uid", &["one", "two"])).map_err(|_| "test pod")?;
    assert_eq!(
        ordinary_container_names(&pod, "default", "app", "uid")?,
        ["one", "two"]
    );
    assert!(ordinary_container_names(&pod, "other", "app", "uid").is_err());
    assert!(ordinary_container_names(&pod, "default", "app", "recreated").is_err());
    let too_many = (0..=MAX_MERGED_CONTAINERS)
        .map(|n| format!("c{n}"))
        .collect::<Vec<_>>();
    let pod: Pod = serde_json::from_str(&merge_pod(
        "uid",
        &too_many.iter().map(String::as_str).collect::<Vec<_>>(),
    ))
    .map_err(|_| "test pod")?;
    assert!(ordinary_container_names(&pod, "default", "app", "uid").is_err());
    runtime()?.block_on(async {
        let (api, requests) = resume_api(vec![(200, merge_pod("recreated", &["one", "two"]))]);
        assert!(
            MergedLogFeed::open(
                api,
                "app",
                "uid",
                false,
                Some(FollowOptions::default()),
                ReadOptions::default()
            )
            .await
            .is_err()
        );
        assert_eq!(requests.lock().map_err(|_| "requests")?.len(), 1);
        Ok(())
    })
}
#[test]
fn merged_previous_failure_is_source_specific_and_post_open_replacement_is_rejected() -> Result<()>
{
    runtime()?.block_on(async {
        let (api, requests) = resume_api(vec![(200, merge_pod("uid", &["one", "two"])), (400, serde_json::json!({"apiVersion":"v1","kind":"Status","status":"Failure","message":"CANARY should not leak","reason":"BadRequest","code":400}).to_string()), (200, "2026-10-22T12:00:00Z surviving\n".into()), (200, metadata("uid"))]);
        let feed = MergedLogFeed::open(api, "app", "uid", true, None, ReadOptions::default()).await?;
        let text = feed.snapshot(ReadOptions::default()).await?;
        assert!(text.contains("one: Previous logs (other container streams continue) failed (HTTP 400)"));
        assert!(text.contains(crate::errors::NEXT));
        assert!(text.contains("[default/app:two] surviving"));
        assert!(!text.contains("CANARY"));
        {
            let requests = requests.lock().map_err(|_| "requests")?;
            let log_requests = requests.iter().filter(|request| request.contains("/log?")).collect::<Vec<_>>();
            assert_eq!(log_requests.len(), 2);
            assert!(log_requests.iter().all(|uri| uri.contains("previous=true") && uri.contains("timestamps=true")));
        }
        let (api, _) = resume_api(vec![(200, merge_pod("uid", &["one", "two"])), (200, "secret first\n".into()), (200, "secret second\n".into()), (200, metadata("replacement"))]);
        assert!(MergedLogFeed::open(api, "app", "uid", false, Some(FollowOptions::default()), ReadOptions::default()).await.is_err_and(|error| error.contains("replaced")));
        Ok(())
    })
}
type Requests = std::sync::Arc<std::sync::Mutex<Vec<String>>>;
#[test]
fn merged_open_concurrency_is_four_and_never_fetches_init_or_ephemeral_logs() -> Result<()> {
    use std::sync::atomic::{AtomicUsize, Ordering};
    runtime()?.block_on(async {
        let active = Arc::new(AtomicUsize::new(0));
        let maximum = Arc::new(AtomicUsize::new(0));
        let requests: Requests = Default::default();
        let (active_service, maximum_service, requests_service) =
            (active.clone(), maximum.clone(), requests.clone());
        let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
            let (active, maximum, requests) = (
                active_service.clone(),
                maximum_service.clone(),
                requests_service.clone(),
            );
            async move {
                let uri = request.uri().to_string();
                requests
                    .lock()
                    .unwrap_or_else(|e| panic!("{e}"))
                    .push(uri.clone());
                let text = if request.uri().path().ends_with("/log") {
                    let current = active.fetch_add(1, Ordering::SeqCst) + 1;
                    maximum.fetch_max(current, Ordering::SeqCst);
                    tokio::time::sleep(Duration::from_millis(5)).await;
                    active.fetch_sub(1, Ordering::SeqCst);
                    "2026-10-22T12:00:00Z fixture\n".into()
                } else {
                    merge_pod("uid", &["one", "two", "three", "four", "five", "six"])
                };
                Ok::<_, std::convert::Infallible>(http::Response::new(http_body_util::Full::new(
                    bytes::Bytes::from(text),
                )))
            }
        });
        let api = Api::namespaced(kube::Client::new(service, "default"), "default");
        let feed =
            MergedLogFeed::open(api, "app", "uid", false, None, ReadOptions::default()).await?;
        let text = feed.snapshot(ReadOptions::default()).await?;
        assert_eq!(maximum.load(Ordering::SeqCst), MERGED_OPEN_CONCURRENCY);
        assert_eq!(active.load(Ordering::SeqCst), 0);
        let requests = requests.lock().map_err(|_| "requests")?;
        let log_requests = requests
            .iter()
            .filter(|uri| uri.contains("/log?"))
            .collect::<Vec<_>>();
        assert_eq!(log_requests.len(), 6);
        assert!(
            !log_requests
                .iter()
                .any(|uri| uri.contains("container=init") || uri.contains("container=debug"))
        );
        for name in ["one", "two", "three", "four", "five", "six"] {
            assert!(text.contains(&format!("[default/app:{name}] fixture")));
        }
        Ok(())
    })
}
fn resume_api(replies: Vec<(u16, String)>) -> (Api<Pod>, Requests) {
    let replies = std::sync::Arc::new(std::sync::Mutex::new(VecDeque::from(replies)));
    let requests: Requests = Default::default();
    let recorded = requests.clone();
    let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
        recorded
            .lock()
            .unwrap_or_else(|e| panic!("{e}"))
            .push(request.uri().to_string());
        let (status, text) = replies
            .lock()
            .unwrap_or_else(|e| panic!("{e}"))
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected log request"));
        async move {
            Ok::<_, std::convert::Infallible>(
                http::Response::builder()
                    .status(status)
                    .body(http_body_util::Full::new(bytes::Bytes::from(text)))
                    .unwrap_or_else(|e| panic!("{e}")),
            )
        }
    });
    (
        Api::namespaced(kube::Client::new(service, "default"), "default"),
        requests,
    )
}
fn metadata(uid: &str) -> String {
    serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"namespace":"default","name":"app","uid":uid}}).to_string()
}
struct Broken;
impl futures_util::io::AsyncRead for Broken {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
        _: &mut [u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        std::task::Poll::Ready(Err(std::io::Error::other(
            "CANARY transport detail must not be displayed",
        )))
    }
}
fn broken_feed(api: Api<Pod>, prefix: &str) -> Result<LogFeed> {
    let reader =
        futures_util::io::BufReader::new(Cursor::new(prefix.as_bytes().to_vec()).chain(Broken));
    LogFeed::with_options(reader, FollowOptions::default())?.with_resume(
        api,
        "app",
        "uid",
        Some("main".into()),
        ReadOptions {
            since_seconds: Some(300),
            ..Default::default()
        },
    )
}
#[test]
fn transport_resume_floors_since_time_checks_uid_and_labels_overlap_and_gaps() -> Result<()> {
    runtime()?.block_on(async {
        let (api, requests) = resume_api(vec![
            (200, metadata("uid")),
            (
                200,
                "2026-10-22T12:00:56.9Z repeated\n2026-10-22T12:00:57Z next\n".into(),
            ),
            (200, metadata("uid")),
        ]);
        let mut feed = broken_feed(api, "2026-10-22T12:00:56.9Z first\n")?;
        let deadline = feed.deadline;
        assert!(
            feed.next()
                .await?
                .is_some_and(|line| line.ends_with(" first"))
        );
        assert!(
            feed.next()
                .await?
                .is_some_and(|line| line.contains("interrupted"))
        );
        let notice = feed.next().await?.ok_or("missing reconnect notice")?;
        assert!(notice.contains("overlap may repeat"));
        assert!(notice.contains("may leave gaps"));
        assert_eq!(feed.deadline, deadline);
        assert!(
            feed.next()
                .await?
                .is_some_and(|line| line.ends_with(" repeated"))
        );
        assert!(
            feed.next()
                .await?
                .is_some_and(|line| line.ends_with(" next"))
        );
        assert_eq!(feed.next().await?, None); // A clean container EOF is final.
        let requests = requests.lock().unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(requests.len(), 3);
        assert!(requests[1].contains("sinceTime=2026-10-22T12%3A00%3A56Z"));
        assert!(!requests[1].contains("tailLines"));
        assert!(!requests[1].contains("sinceSeconds"));
        assert!(requests[1].contains("container=main"));
        Ok(())
    })
}
#[test]
fn reconnect_stops_on_replacement_denial_missing_timestamp_and_original_budgets() -> Result<()> {
    runtime()?.block_on(async {
        for reply in [(200, metadata("replacement")), (403, serde_json::json!({"apiVersion":"v1","kind":"Status","status":"Failure","code":403,"reason":"Forbidden","message":"CANARY"}).to_string())] {
            let (api, requests) = resume_api(vec![reply]);
            let mut feed = broken_feed(api, "2026-10-22T12:00:56Z first\n")?;
            feed.next().await?;
            assert!(feed.next().await?.is_some_and(|line| line.contains("interrupted")));
            let error = feed.next().await.err().ok_or("expected stopped reconnect")?;
            assert!(!error.contains("CANARY"));
            assert!(error.contains("replaced") || error.contains("403"));
            assert_eq!(feed.next().await?, None);
            assert_eq!(requests.lock().unwrap_or_else(|e| panic!("{e}")).len(), 1);
        }
        for budget in [0, 1, 2, 3] {
            let (api, requests) = resume_api(vec![]);
            let mut feed = broken_feed(api, if budget == 0 {"not timestamped\n"} else {"2026-10-22T12:00:56Z first\n"})?;
            feed.next().await?;
            match budget {
                1 => feed.total_bytes = crate::transport::LOG_STREAM_BYTES,
                2 => feed.deadline = Instant::now() + Duration::from_millis(10),
                3 => feed.reconnects = MAX_RECONNECTS,
                _ => {},
            }
            let result = feed.next().await;
            let error = if matches!(result, Ok(Some(_))) {
                feed.next().await.err().ok_or("expected deferred local log stop")?
            } else {
                result.err().ok_or("expected local log stop")?
            };
            assert!(!error.contains("CANARY"));
            assert_eq!(feed.next().await?, None);
            assert!(requests.lock().unwrap_or_else(|e| panic!("{e}")).is_empty());
        }
        Ok(())
    })
}
#[test]
fn head_and_tail_preserve_the_requested_end_and_snapshot_bounds() -> Result<()> {
    let input = (0..10)
        .map(|index| format!("line-{index}\n"))
        .collect::<String>();
    let mut options = ReadOptions {
        tail_lines: 3,
        ..Default::default()
    };
    assert_eq!(snapshot_text(&input, options), "line-7\nline-8\nline-9\n");
    options.head = true;
    assert_eq!(snapshot_text(&input, options), "line-0\nline-1\nline-2\n");
    let huge = format!("first\n{}\nlast\n", "é".repeat(MAX_LOG_BYTES));
    let text = snapshot_text(&huge, options);
    assert!(text.starts_with("first\n"));
    assert!(text.contains(LINE_NOTICE));
    assert!(text.contains("may be incomplete"));
    assert!(text.len() <= MAX_LOG_BYTES);
    let mut rolling = RollingLog::default();
    rolling.push(&"é".repeat(MAX_LINE_BYTES));
    assert!(rolling.text().len() <= MAX_LINE_BYTES + 1);
    assert!(rolling.text().contains(LINE_NOTICE));
    runtime()?.block_on(async {
        let mut feed = LogFeed::with_read_options(
            Cursor::new(input.into_bytes()),
            FollowOptions::default(),
            options,
        )?;
        assert_eq!(feed.next().await?, Some("line-0".into()));
        assert_eq!(feed.next().await?, Some("line-1".into()));
        assert_eq!(feed.next().await?, Some("line-2".into()));
        assert_eq!(feed.next().await?, None);
        Ok(())
    })
}
#[test]
fn range_shortcuts_produce_distinct_bounded_server_requests() -> Result<()> {
    for (key, since) in [
        ('0', None),
        ('2', Some(60)),
        ('3', Some(300)),
        ('4', Some(900)),
        ('5', Some(1800)),
        ('6', Some(3600)),
    ] {
        let options = ReadOptions::for_key(key).ok_or("missing range shortcut")?;
        let params = options.params(Some("app".into()), false, true)?;
        assert_eq!(params.since_seconds, since);
        assert_eq!(params.tail_lines, Some(200));
        assert!(params.follow);
        assert!(params.timestamps);
        assert_eq!(params.limit_bytes, None);
    }
    let head = ReadOptions::for_key('1').ok_or("missing head shortcut")?;
    let params = head.params(None, false, true)?;
    assert!(!params.follow);
    assert_eq!(params.tail_lines, None);
    assert_eq!(params.limit_bytes, Some(MAX_LOG_BYTES as i64));
    assert!(params.since_seconds.is_none());
    assert!(ReadOptions::for_key('7').is_none());
    assert!(
        ReadOptions {
            tail_lines: 0,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        ReadOptions {
            tail_lines: 2001,
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        ReadOptions {
            since_seconds: Some(0),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        ReadOptions {
            since_seconds: Some(86_401),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(
        ReadOptions {
            head: true,
            since_seconds: Some(60),
            ..Default::default()
        }
        .validate()
        .is_err()
    );
    assert!(ReadOptions::default().params(None, true, true).is_err());
    assert!(ReadOptions::default().params(None, true, false)?.previous);
    Ok(())
}
#[test]
fn timestamps_toggle_locally_without_hiding_application_text() {
    assert_eq!(
        without_timestamp("2026-10-22T12:03:04.123456789Z payload"),
        "payload"
    );
    assert_eq!(
        without_timestamp("2026-10-22T13:03:04+01:00 payload"),
        "payload"
    );
    assert_eq!(
        without_timestamp("2026-02-30T12:00:00Z payload"),
        "2026-02-30T12:00:00Z payload"
    );
    assert_eq!(
        without_timestamp("app 2026-10-22T12:00:00Z"),
        "app 2026-10-22T12:00:00Z"
    );
    assert_eq!(
        without_timestamp("2026-10-22T12:00:00Z"),
        "2026-10-22T12:00:00Z"
    );
    let mut buffer = RollingLog::default();
    buffer.push("2026-10-22T12:03:04Z INFO served");
    buffer.push("2026-10-22T12:03:05Z ERROR failed\x1b[31m");
    let filtered = buffer.filtered_text(false, |line| line.contains("ERROR"));
    assert_eq!(filtered, "ERROR failed[31m\n");
    assert_eq!(
        buffer.filtered_text(false, |line| line.contains("2026-10-22")),
        ""
    );
    assert_eq!(buffer.line_count(), 2);
    assert!(buffer.text().contains("2026-10-22T12:03:04Z"));
    buffer.clear();
    assert!(buffer.text().is_empty());
    assert_eq!(buffer.dropped(), 0);
}
#[test]
fn truncated_notice_survives_the_rolling_ui_pipeline() -> Result<()> {
    runtime()?.block_on(async {
        let mut feed = LogFeed::new(Cursor::new(format!("{}\n", "x".repeat(5000)).into_bytes()));
        let line = feed.next().await?.ok_or("missing long line")?;
        assert!(line.chars().count() <= MAX_LINE_BYTES);
        let mut buffer = RollingLog::default();
        buffer.push(&safety::safe_text(&line, MAX_LINE_BYTES));
        assert!(buffer.text().contains("[line truncated at 4096 bytes]"));
        Ok(())
    })
}
fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .map_err(|e| e.to_string())
}
#[test]
fn invalid_utf8_and_expired_session_are_safe() -> Result<()> {
    runtime()?.block_on(async {
        let mut feed = LogFeed::new(Cursor::new(vec![0xc3, b'\n', 0xff]));
        assert_eq!(feed.next().await?, Some("�".into()));
        assert_eq!(feed.next().await?, Some("�".into()));
        let mut expired = LogFeed::new(Cursor::new(b"never displayed".to_vec()));
        expired.deadline = Instant::now();
        assert!(expired.next().await.is_err());
        assert_eq!(expired.next().await?, None);
        Ok(())
    })
}
#[test]
fn sanitizes_controls_and_preserves_utf8_across_chunks() -> Result<()> {
    runtime()?.block_on(async {
        let mut bytes = vec![b'a'; 4095];
        bytes.extend_from_slice("é\n".as_bytes());
        let mut too_long = LogFeed::new(Cursor::new(bytes));
        let truncated = too_long.next().await?.ok_or("missing truncated line")?;
        assert!(truncated.ends_with("[line truncated at 4096 bytes]"));
        assert!(truncated.len() < 13_000);
        let mut feed = LogFeed::new(Cursor::new(
            "ok\x1b[31m\r\nlasté\u{202e}".as_bytes().to_vec(),
        ));
        assert_eq!(feed.next().await?, Some("ok[31m".into()));
        assert_eq!(feed.next().await?, Some("lasté".into()));
        assert_eq!(feed.next().await?, None);
        Ok(())
    })
}
#[test]
fn follows_beyond_old_total_limit_and_stops_floods() -> Result<()> {
    runtime()?.block_on(async {
        let line = format!("{}\n", "x".repeat(1000));
        let mut feed = LogFeed::new(Cursor::new(line.repeat(300).into_bytes()));
        let mut count = 0;
        while feed.next().await?.is_some() {
            count += 1;
        }
        assert_eq!(count, 300); // More than 256 KiB; fixed memory is independent of total traffic.
        let mut flood = LogFeed::new(Cursor::new(vec![b'\n'; MAX_RATE_LINES + 1]));
        for _ in 0..MAX_RATE_LINES {
            assert!(flood.next().await?.is_some());
        }
        assert!(flood.next().await.is_err());
        assert_eq!(flood.next().await?, None);
        let mut bytes = LogFeed::new(Cursor::new(vec![b'x'; MAX_RATE_BYTES + 1]));
        assert!(bytes.next().await.is_err());
        Ok(())
    })
}
#[test]
fn rolling_cache_evicts_old_data_and_keeps_utf8_and_sanitization() {
    let mut buffer = RollingLog::default();
    for i in 0..3000 {
        buffer.push(&format!("line {i}: {}\x1b]52;secret\x07", "é".repeat(1000)));
    }
    let text = buffer.text();
    assert!(text.len() <= MAX_LOG_BYTES);
    assert!(buffer.line_count() <= MAX_LOG_LINES);
    assert!(buffer.dropped() > 0);
    assert!(text.contains("line 2999:"));
    assert!(!text.contains("line 0:"));
    assert!(!text.contains('\x1b'));
    assert!(text.contains("Rolling buffer"));
}
#[test]
fn validates_lifetime_and_quiet_stream_deadline() -> Result<()> {
    assert!(FollowOptions { seconds: 0 }.validate().is_err());
    assert!(FollowOptions { seconds: 86_401 }.validate().is_err());
    assert_eq!(FollowOptions::default().seconds, 3600);
    assert!(FollowOptions { seconds: 86_400 }.validate().is_ok());
    runtime()?.block_on(async {
        struct Quiet;
        impl futures_util::io::AsyncRead for Quiet {
            fn poll_read(
                self: Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
                _: &mut [u8],
            ) -> std::task::Poll<std::io::Result<usize>> {
                std::task::Poll::Pending
            }
        }
        impl AsyncBufRead for Quiet {
            fn poll_fill_buf(
                self: Pin<&mut Self>,
                _: &mut std::task::Context<'_>,
            ) -> std::task::Poll<std::io::Result<&[u8]>> {
                std::task::Poll::Pending
            }
            fn consume(self: Pin<&mut Self>, _: usize) {}
        }
        let mut feed = LogFeed::with_options(Quiet, FollowOptions { seconds: 1 })?;
        let started = Instant::now();
        assert!(feed.next().await.is_err());
        assert!(started.elapsed() >= Duration::from_millis(900));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(feed.next().await?, None);
        Ok(())
    })
}
#[test]
fn maximum_line_and_empty_lines_are_supported() -> Result<()> {
    runtime()?.block_on(async {
        let mut data = vec![b'x'; MAX_LINE_BYTES];
        data.extend_from_slice(b"\n\nend");
        let mut feed = LogFeed::new(Cursor::new(data));
        assert_eq!(feed.next().await?.map(|x| x.len()), Some(MAX_LINE_BYTES));
        assert_eq!(feed.next().await?, Some(String::new()));
        assert_eq!(feed.next().await?, Some("end".into()));
        Ok(())
    })
}
