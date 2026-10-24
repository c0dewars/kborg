//! Workload log sessions and opt-in dynamic Pod membership.
use super::{
    CONNECT_BUDGET, FollowOptions, LogFeed, LogMembership, LogPod, MEMBERSHIP_CHECK_INTERVAL,
    MergedLogFeed, ReadOptions,
    identity::{ReopenFailure, check_uid},
    log_pod,
    merged::open_failure,
    rolling::bounded_line,
};
use crate::{Result, safety};
use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use std::{collections::VecDeque, time::Duration};
use tokio::time::Instant;

/// A followed workload log session with fixed or explicitly dynamic membership.
pub enum WorkloadLogFeed {
    Merged(MergedLogFeed),
    Dynamic(Box<DynamicLogFeed>),
}
impl WorkloadLogFeed {
    pub async fn next(&mut self) -> Result<Option<String>> {
        match self {
            Self::Merged(feed) => feed.next().await,
            Self::Dynamic(feed) => feed.next().await,
        }
    }
}

/// Membership refresh cadence for explicitly opted-in dynamic sessions.
pub const MEMBERSHIP_REFRESH_INTERVAL: Duration = Duration::from_secs(10);
const MEMBERSHIP_REFRESH_FAILURES: u8 = 3;

/// Pods that left (by exact UID) and Pods that joined between two captures.
/// A same-name replacement appears in both lists, never as a continuation.
pub(crate) fn membership_changes<'a>(
    current: &'a LogMembership,
    next: &'a LogMembership,
) -> (Vec<&'a LogPod>, Vec<&'a LogPod>) {
    let left = current
        .pods
        .iter()
        .filter(|pod| !next.pods.iter().any(|other| other.uid == pod.uid))
        .collect();
    let joined = next
        .pods
        .iter()
        .filter(|pod| !current.pods.iter().any(|other| other.uid == pod.uid))
        .collect();
    (left, joined)
}

fn membership_notice(namespace: &str, pod: &LogPod, change: &str) -> String {
    bounded_line(&format!(
        "[Log membership: {namespace}/{} UID {} {change}]",
        pod.name, pod.uid
    ))
}

/// Opt-in workload tail. It periodically recaptures the same selection with the
/// complete-inventory rules of a fixed capture, announces every departure and
/// join, and opens joined Pods only after verifying their exact UID. Refresh
/// failures retain current sources and are reported; repeated transport or
/// permission failures stop the session rather than hiding stale membership.
pub struct DynamicLogFeed {
    gateway: crate::gateway::Gateway,
    api: Option<Api<Pod>>,
    membership: LogMembership,
    feed: MergedLogFeed,
    read: ReadOptions,
    options: FollowOptions,
    next_refresh: Instant,
    notices: VecDeque<String>,
    failures: u8,
    last_limit: Option<String>,
    /// Joined Pods whose streams could not all open yet (for example, still
    /// starting). They are retried each refresh and announced only once.
    waiting: std::collections::BTreeSet<String>,
}
impl DynamicLogFeed {
    pub(crate) fn new(
        gateway: crate::gateway::Gateway,
        api: Option<Api<Pod>>,
        membership: LogMembership,
        mut feed: MergedLogFeed,
        read: ReadOptions,
        options: FollowOptions,
    ) -> Result<Self> {
        if !membership.is_dynamic() {
            return Err("dynamic log feed requires an explicitly dynamic membership".into());
        }
        if read.head {
            return Err(
                "dynamic membership follows new records; choose a tail or time range, not head"
                    .into(),
            );
        }
        feed.keep_open = true;
        Ok(Self {
            gateway,
            api,
            membership,
            feed,
            read,
            options,
            next_refresh: Instant::now() + MEMBERSHIP_REFRESH_INTERVAL,
            notices: VecDeque::new(),
            failures: 0,
            last_limit: None,
            waiting: std::collections::BTreeSet::new(),
        })
    }
    pub async fn next(&mut self) -> Result<Option<String>> {
        loop {
            if let Some(notice) = self.notices.pop_front() {
                return Ok(Some(notice));
            }
            if Instant::now() >= self.feed.deadline() {
                return Err("dynamic log session deadline reached; all streams stopped; start a new session to continue".into());
            }
            if Instant::now() >= self.next_refresh {
                self.refresh().await?;
                continue;
            }
            // The merged read is cancel-safe: pending source futures stay owned
            // by the feed, so a refresh tick never drops buffered records.
            if let Ok(result) = tokio::time::timeout_at(self.next_refresh, self.feed.next()).await {
                return result;
            }
        }
    }
    async fn refresh(&mut self) -> Result<()> {
        self.next_refresh = Instant::now() + MEMBERSHIP_REFRESH_INTERVAL;
        let Some(selection) = self.membership.dynamic.clone() else {
            return Err("dynamic log membership selection unavailable".into());
        };
        let deadline = self
            .feed
            .deadline()
            .min(Instant::now() + MEMBERSHIP_CHECK_INTERVAL + CONNECT_BUDGET);
        let captured = tokio::time::timeout_at(
            deadline,
            self.gateway
                .capture_log_membership(self.membership.namespace(), selection.clone()),
        )
        .await
        .unwrap_or_else(|_| Err("membership refresh timed out".into()));
        let next = match captured {
            Ok(next) => next.into_dynamic(selection),
            Err(error) => {
                let error = safety::safe_text(&error, 400);
                // A scale-up past the bounded inventory is a visible, stable
                // condition; transport and permission failures count as strikes.
                if error.contains("exceeds") || error.contains("limit") {
                    if self.last_limit.as_deref() != Some(error.as_str()) {
                        self.notices.push_back(bounded_line(&format!(
                            "[Log membership refresh refused: {error}; current sources retained; no Pods admitted]"
                        )));
                        self.last_limit = Some(error);
                    }
                    return Ok(());
                }
                self.failures += 1;
                if self.failures >= MEMBERSHIP_REFRESH_FAILURES {
                    return Err(format!(
                        "dynamic log membership could not be refreshed three times ({error}); all streams stopped; capture again"
                    ));
                }
                self.notices.push_back(bounded_line(&format!(
                    "[Log membership refresh failed: {error}; current sources retained; no Pods admitted]"
                )));
                return Ok(());
            }
        };
        self.failures = 0;
        self.last_limit = None;
        let namespace = self.membership.namespace().to_owned();
        let (left, joined) = membership_changes(&self.membership, &next);
        for pod in left {
            let closed = self.feed.remove_pod(&namespace, &pod.name);
            self.notices.push_back(membership_notice(
                &namespace,
                pod,
                &format!("left; {closed} streams closed"),
            ));
        }
        let mut admitted = Vec::new();
        for pod in joined {
            let opened = match self.api.clone() {
                Some(api) => {
                    open_joined_pod(
                        api,
                        &namespace,
                        pod,
                        self.read,
                        self.options,
                        self.feed.deadline().min(Instant::now() + CONNECT_BUDGET),
                    )
                    .await
                }
                None => Err("joined Pods cannot be opened without a live connection".into()),
            };
            match opened {
                Ok(sources) => match self.feed.add_sources(sources) {
                    Ok(()) => {
                        self.notices.push_back(membership_notice(
                            &namespace,
                            pod,
                            &format!("joined; {} ordinary containers", pod.containers.len()),
                        ));
                        self.waiting.remove(&pod.uid);
                        admitted.push(pod.uid.clone());
                    }
                    Err(error) => self.notices.push_back(membership_notice(
                        &namespace,
                        pod,
                        &format!("not admitted: {error}"),
                    )),
                },
                Err(error) => {
                    if self.waiting.insert(pod.uid.clone()) {
                        self.notices.push_back(membership_notice(
                            &namespace,
                            pod,
                            &format!(
                                "waiting to join: {}; retried every refresh",
                                safety::safe_text(&error, 300)
                            ),
                        ));
                    }
                }
            }
        }
        // Only admitted Pods become membership; a refused join is retried next refresh.
        self.waiting
            .retain(|uid| next.pods.iter().any(|pod| &pod.uid == uid));
        let current = std::mem::replace(&mut self.membership, next);
        self.membership.pods.retain(|pod| {
            current.pods.iter().any(|old| old.uid == pod.uid) || admitted.contains(&pod.uid)
        });
        Ok(())
    }
}

async fn open_joined_pod(
    api: Api<Pod>,
    namespace: &str,
    member: &LogPod,
    read: ReadOptions,
    options: FollowOptions,
    deadline: Instant,
) -> Result<Vec<(String, Result<LogFeed>)>> {
    let pod = tokio::time::timeout_at(deadline, api.get(&member.name))
        .await
        .map_err(|_| "joined Pod verification timed out")?
        .map_err(|error| open_failure(error, false))?;
    let current = log_pod(&pod, namespace)?;
    if current.uid != member.uid || current.containers != member.containers {
        return Err("joined Pod changed before its streams opened".into());
    }
    let mut sources = Vec::new();
    for container in &member.containers {
        let identity = format!("{namespace}/{}:{container}", member.name);
        let result = async {
            let params = read.params(Some(container.clone()), false, true)?;
            let reader = tokio::time::timeout_at(deadline, api.log_stream(&member.name, &params))
                .await
                .map_err(|_| "log source connection timed out".to_owned())?
                .map_err(|error| open_failure(error, false))?;
            LogFeed::with_read_options(reader, options, read)?.with_resume(
                api.clone(),
                &member.name,
                &member.uid,
                Some(container.clone()),
                read,
            )
        }
        .await;
        // All-or-nothing: a Pod whose containers are still starting joins on
        // a later refresh instead of streaming with a silent permanent gap.
        sources.push((identity, Ok(result?)));
    }
    // The pod/log endpoint has no UID precondition; recheck after opening.
    match check_uid(&api, &member.name, &member.uid, deadline).await {
        Ok(()) => Ok(sources),
        Err(ReopenFailure::Stop(error)) => Err(error),
        Err(ReopenFailure::Retry) => {
            Err("joined Pod identity could not be verified after opening".into())
        }
    }
}
