//! Fair, budget-shared merge of several container and Pod log sources.
use super::{
    CONNECT_BUDGET, FollowOptions, IdentityGuard, LogFeed, LogMembership, MAX_LOG_BYTES,
    MAX_MERGED_CONTAINERS, MAX_WORKLOAD_LOG_SOURCES, MEMBERSHIP_CHECK_BUDGET,
    MEMBERSHIP_CHECK_INTERVAL, MERGED_OPEN_CONCURRENCY, ReadOptions,
    budget::{MergeBudget, SharedMergeBudget, charge_merge},
    identity::{ReopenFailure, check_uid, verify_guards},
    log_pod, membership_bytes, ordinary_container_names,
    rolling::bounded_line,
    validate_merge_target,
};
use crate::{Result, safety};
use futures_util::StreamExt;
use k8s_openapi::api::core::v1::Pod;
use kube::Api;
use std::{
    collections::VecDeque,
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    task::Poll,
    time::Duration,
};
use tokio::time::Instant;

type PendingSource = Pin<Box<dyn Future<Output = (LogFeed, Result<Option<String>>)> + Send>>;
struct MergedSource {
    identity: String,
    pending: Option<PendingSource>,
    notice: Option<String>,
}
fn pending_source(mut feed: LogFeed) -> PendingSource {
    Box::pin(async move {
        let result = feed.next().await;
        (feed, result)
    })
}
fn source_notice(identity: &str, message: &str) -> String {
    bounded_line(&format!("[Log stream {identity}: {message}]"))
}
fn source_line(identity: &str, line: &str, control: bool) -> String {
    // Leave the timestamp first: local timestamp toggling and reconnect semantics
    // remain meaningful. Application text can never impersonate a merge notice.
    if control {
        return source_notice(identity, line.trim_start_matches('[').trim_end_matches(']'));
    }
    if let Some((timestamp, message)) = line.split_once(' ')
        && crate::metrics::timestamp_key(timestamp).is_some()
    {
        return bounded_line(&format!("{timestamp} [{identity}] {message}"));
    }
    bounded_line(&format!("[{identity}] {line}"))
}
pub(super) fn open_failure(error: kube::Error, previous: bool) -> String {
    match error {
        kube::Error::Api(response) => format!(
            "{} unavailable (Kubernetes API {}); other container streams continue",
            if previous { "previous logs" } else { "logs" },
            response.code
        ),
        _ => "log connection failed; other container streams continue (server details suppressed)"
            .into(),
    }
}

/// One bounded read future per source; no spawned tasks, queues or detached work.
/// Ready sources rotate fairly. Dropping the feed cancels every open body/read.
pub struct MergedLogFeed {
    sources: Vec<MergedSource>,
    cursor: usize,
    pub(super) deadline: Instant,
    pub(super) budget: SharedMergeBudget,
    header: VecDeque<String>,
    pub(super) ended: bool,
    snapshot_mode: bool,
    guards: Arc<Vec<IdentityGuard>>,
    pub(super) next_identity_check: Option<Instant>,
    identity_failures: u8,
    notice_reserve: usize,
    /// Dynamic sessions wait for joins instead of ending when no source is active.
    pub(super) keep_open: bool,
}
impl MergedLogFeed {
    /// Close every source of one departed Pod. Dropping a pending read cancels it.
    pub(crate) fn remove_pod(&mut self, namespace: &str, pod: &str) -> usize {
        let prefix = format!("{namespace}/{pod}:");
        let before = self.sources.len();
        self.sources
            .retain(|source| !source.identity.starts_with(&prefix));
        self.cursor = 0;
        before - self.sources.len()
    }
    /// Admit sources for one newly joined Pod under the same session deadline,
    /// shared budget and parent guards. The 32-source bound stays global.
    pub(crate) fn add_sources(&mut self, sources: Vec<(String, Result<LogFeed>)>) -> Result<()> {
        if self.ended {
            return Err("log session already ended".into());
        }
        if self.sources.len() + sources.len() > MAX_WORKLOAD_LOG_SOURCES {
            return Err("joined Pod would exceed the shared 32-source limit".into());
        }
        for (identity, result) in sources {
            if self
                .sources
                .iter()
                .any(|source| source.identity == identity)
            {
                return Err("joined Pod duplicates an active log source".into());
            }
            let (pending, notice) = match result {
                Ok(mut feed) => {
                    feed.deadline = self.deadline;
                    feed.merge_budget = Some(self.budget.clone());
                    feed.isolate_identity = true;
                    if let Some(resume) = &mut feed.resume {
                        resume.guards = self.guards.clone();
                    }
                    (Some(pending_source(feed)), None)
                }
                Err(error) => (
                    None,
                    Some(source_notice(&identity, &safety::safe_text(&error, 512))),
                ),
            };
            self.sources.push(MergedSource {
                identity,
                pending,
                notice,
            });
        }
        Ok(())
    }
    pub(crate) fn deadline(&self) -> Instant {
        self.deadline
    }
    pub(crate) async fn open_membership(
        api: Api<Pod>,
        membership: &LogMembership,
        mut guards: Vec<IdentityGuard>,
        previous: bool,
        follow: Option<FollowOptions>,
        read: ReadOptions,
    ) -> Result<Self> {
        if api.namespace() != Some(membership.namespace()) {
            return Err("log membership API escaped its captured namespace".into());
        }
        let read = read.validate()?;
        if previous && follow.is_some() {
            return Err("previous container logs support snapshots only; omit follow".into());
        }
        let options = follow
            .unwrap_or(FollowOptions {
                seconds: CONNECT_BUDGET.as_secs() as u32,
            })
            .validate()?;
        let deadline = Instant::now() + Duration::from_secs(u64::from(options.seconds));
        let connect_deadline = deadline.min(Instant::now() + CONNECT_BUDGET);
        let mut bytes = 0;
        let mut captures=futures_util::stream::iter(membership.pods.iter().map(|member| {
            let api=api.clone(); async move {
                let pod=tokio::time::timeout_at(connect_deadline,api.get(&member.name)).await.map_err(|_|"captured log Pod verification timed out")?.map_err(|error|open_failure(error,false))?;
                let current=log_pod(&pod,membership.namespace())?;
                let mut actual=current.containers.clone(); actual.sort();
                let mut expected=member.containers.clone(); expected.sort();
                if current.name!=member.name||current.uid!=member.uid||actual!=expected {return Err("captured log Pod or ordinary-container membership changed; capture sources again".into());}
                Ok::<Pod,String>(pod)
            }
        })).buffer_unordered(MERGED_OPEN_CONCURRENCY);
        while let Some(pod) = captures.next().await {
            membership_bytes(&mut bytes, &pod?)?;
        }
        // Dynamic sessions verify every captured Pod while opening, but keep only
        // the parent workload as a session-wide guard: a departing Pod is a
        // reported membership change, not a reason to stop its peers.
        let dynamic = membership.is_dynamic();
        let session_guards = dynamic.then(|| guards.clone());
        for member in &membership.pods {
            guards.push(IdentityGuard::Pod {
                api: api.clone(),
                name: member.name.clone(),
                uid: member.uid.clone(),
            });
        }
        match verify_guards(&guards, connect_deadline).await {
            Ok(()) => {}
            Err(ReopenFailure::Retry) => {
                return Err(
                    "captured workload log identities could not be verified; retry explicitly"
                        .into(),
                );
            }
            Err(ReopenFailure::Stop(error)) => return Err(error),
        }
        let requests = membership
            .pods
            .iter()
            .flat_map(|pod| pod.containers.iter().map(move |container| (pod, container)));
        let mut opens =
            futures_util::stream::iter(requests.enumerate().map(|(index, (pod, container))| {
                let api = api.clone();
                async move {
                    let result = async {
                        let params =
                            read.params(Some(container.clone()), previous, follow.is_some())?;
                        let reader = tokio::time::timeout_at(
                            connect_deadline,
                            api.log_stream(&pod.name, &params),
                        )
                        .await
                        .map_err(|_| "log source connection timed out".to_owned())?
                        .map_err(|error| open_failure(error, previous))?;
                        let mut feed = LogFeed::with_read_options(reader, options, read)?;
                        feed.deadline = deadline;
                        feed.isolate_identity = dynamic;
                        if follow.is_some() {
                            feed = feed.with_resume(
                                api,
                                &pod.name,
                                &pod.uid,
                                Some(container.clone()),
                                read,
                            )?;
                        }
                        Ok(feed)
                    }
                    .await;
                    (index, pod.name.clone(), container.clone(), result)
                }
            }))
            .buffer_unordered(MERGED_OPEN_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        // Prevent replacement while any of the source requests were opening.
        match verify_guards(&guards,connect_deadline).await {
            Ok(())=>{}, Err(ReopenFailure::Retry)=>return Err("captured workload log identities could not be verified after opening; retry explicitly".into()), Err(ReopenFailure::Stop(error))=>return Err(error),
        }
        opens.sort_by_key(|(index, _, _, _)| *index);
        if let Some(parents) = session_guards {
            guards = parents;
        }
        if follow.is_none() || read.head {
            guards.clear();
        }
        Self::from_membership_sources(
            membership,
            opens
                .into_iter()
                .map(|(_, pod, container, result)| (pod, container, result))
                .collect(),
            deadline,
            guards,
        )
    }
    pub(crate) fn from_membership_sources(
        membership: &LogMembership,
        sources: Vec<(String, String, Result<LogFeed>)>,
        deadline: Instant,
        guards: Vec<IdentityGuard>,
    ) -> Result<Self> {
        if sources.len() != membership.source_count() {
            return Err("log streams do not cover the complete captured membership".into());
        }
        let mut seen = std::collections::BTreeSet::new();
        for (pod, container, _) in &sources {
            if !membership
                .pods
                .iter()
                .any(|member| member.name == *pod && member.containers.contains(container))
                || !seen.insert((pod, container))
            {
                return Err(
                    "log stream escaped or duplicated the captured Pod/container membership".into(),
                );
            }
        }
        let mut headers = VecDeque::new();
        let admission = if membership.is_dynamic() {
            "dynamic membership opted in; every join and departure is announced, replacements are a departure plus a join"
        } else {
            "fixed membership, no new Pods or replacements admitted"
        };
        headers.push_back(bounded_line(&format!("[Log stream merge: {}; {} Pods / {} ordinary containers; {admission}; delivery arrival order, timestamps preserved; init/ephemeral excluded]",membership.description,membership.pods.len(),sources.len())));
        for pod in &membership.pods {
            headers.push_back(source_notice(
                &format!("{}/{}", membership.namespace, pod.name),
                &format!(
                    "captured UID {}; {} ordinary containers",
                    pod.uid,
                    pod.containers.len()
                ),
            ));
        }
        let sources = sources
            .into_iter()
            .map(|(pod, container, result)| {
                (
                    format!("{}/{pod}:{container}", membership.namespace),
                    result,
                )
            })
            .collect();
        let mut feed = Self::build_sources(sources, headers, deadline, guards)?;
        feed.notice_reserve = 64 * 1024;
        Ok(feed)
    }
    pub(crate) async fn open(
        api: Api<Pod>,
        name: &str,
        uid: &str,
        previous: bool,
        follow: Option<FollowOptions>,
        read_options: ReadOptions,
    ) -> Result<Self> {
        let ns = api
            .namespace()
            .ok_or("merged logs require a concrete namespace")?
            .to_owned();
        validate_merge_target(&ns, name, uid)?;
        let read_options = read_options.validate()?;
        if previous && follow.is_some() {
            return Err("previous container logs support snapshots only; omit follow".into());
        }
        let options = follow
            .unwrap_or(FollowOptions {
                seconds: CONNECT_BUDGET.as_secs() as u32,
            })
            .validate()?;
        let started = Instant::now();
        let deadline = started + Duration::from_secs(u64::from(options.seconds));
        let connect_deadline = deadline.min(started + CONNECT_BUDGET);
        let pod = tokio::time::timeout_at(connect_deadline, api.get(name))
            .await
            .map_err(|_| "merged log pod lookup timed out")?
            .map_err(|error| match error {
                kube::Error::Api(response) => format!("cannot read merged-log Pod specification (Kubernetes API {}); all-container logs require pod get permission", response.code),
                _ => "merged-log Pod lookup failed; check connectivity and pod get permission (server details suppressed)".into(),
            })?;
        let names = ordinary_container_names(&pod, &ns, name, uid)?;
        let mut opened =
            futures_util::stream::iter(names.into_iter().enumerate().map(|(index, container)| {
                let api = api.clone();
                async move {
                    let result = async {
                        let params = read_options.params(
                            Some(container.clone()),
                            previous,
                            follow.is_some(),
                        )?;
                        let reader = tokio::time::timeout_at(
                            connect_deadline,
                            api.log_stream(name, &params),
                        )
                        .await
                        .map_err(|_| {
                            "log connection timed out; other container streams continue".to_owned()
                        })?
                        .map_err(|error| open_failure(error, previous))?;
                        let mut feed = LogFeed::with_read_options(reader, options, read_options)?;
                        feed.deadline = deadline;
                        if follow.is_some() {
                            feed = feed.with_resume(
                                api,
                                name,
                                uid,
                                Some(container.clone()),
                                read_options,
                            )?;
                        }
                        Ok(feed)
                    }
                    .await;
                    (index, container, result)
                }
            }))
            .buffer_unordered(MERGED_OPEN_CONCURRENCY)
            .collect::<Vec<_>>()
            .await;
        // No source body is polled before this post-open identity check.
        match check_uid(&api, name, uid, connect_deadline).await {
            Ok(()) => {},
            Err(ReopenFailure::Stop(error)) => return Err(error),
            Err(ReopenFailure::Retry) => return Err("merged log target identity verification failed; reopen after checking connectivity".into()),
        }
        opened.sort_by_key(|(index, _, _)| *index);
        Self::from_sources_at(
            &ns,
            name,
            uid,
            opened
                .into_iter()
                .map(|(_, name, result)| (name, result))
                .collect(),
            deadline,
        )
    }
    pub(crate) fn from_sources(
        ns: &str,
        name: &str,
        uid: &str,
        sources: Vec<(String, Result<LogFeed>)>,
        options: FollowOptions,
    ) -> Result<Self> {
        let options = options.validate()?;
        Self::from_sources_at(
            ns,
            name,
            uid,
            sources,
            Instant::now() + Duration::from_secs(u64::from(options.seconds)),
        )
    }
    fn from_sources_at(
        ns: &str,
        name: &str,
        uid: &str,
        sources: Vec<(String, Result<LogFeed>)>,
        deadline: Instant,
    ) -> Result<Self> {
        validate_merge_target(ns, name, uid)?;
        if sources.is_empty() || sources.len() > MAX_MERGED_CONTAINERS {
            return Err(
                "merged log source count exceeded its bounded ordinary-container scope".into(),
            );
        }
        let mut names = Vec::with_capacity(sources.len());
        let mut identified = Vec::with_capacity(sources.len());
        for (container, result) in sources {
            if !safety::valid_id(&container) || names.contains(&container) {
                return Err("invalid or duplicate merged log source".into());
            }
            names.push(container.clone());
            let identity = format!("{ns}/{name}:{container}");
            identified.push((identity, result));
        }
        Self::build_sources(
            identified,
            VecDeque::from([format!(
                "[Log stream merge: {} ordinary containers; pod UID {uid}; delivery in arrival order, source timestamps preserved; init/ephemeral excluded]",
                names.len()
            )]),
            deadline,
            Vec::new(),
        )
    }
    fn build_sources(
        sources: Vec<(String, Result<LogFeed>)>,
        header: VecDeque<String>,
        deadline: Instant,
        guards: Vec<IdentityGuard>,
    ) -> Result<Self> {
        if sources.is_empty() || sources.len() > MAX_WORKLOAD_LOG_SOURCES {
            return Err("merged logs exceed the shared 32-source limit".into());
        }
        let budget: SharedMergeBudget = Arc::new(Mutex::new(MergeBudget::default()));
        let guards = Arc::new(guards);
        let mut merged = Vec::with_capacity(sources.len());
        for (identity, result) in sources {
            let (pending, notice) = match result {
                Ok(mut feed) => {
                    feed.deadline = deadline;
                    feed.merge_budget = Some(budget.clone());
                    if let Some(resume) = &mut feed.resume {
                        resume.guards = guards.clone();
                    }
                    (Some(pending_source(feed)), None)
                }
                Err(error) => (
                    None,
                    Some(source_notice(&identity, &safety::safe_text(&error, 512))),
                ),
            };
            merged.push(MergedSource {
                identity,
                pending,
                notice,
            });
        }
        Ok(Self {
            sources: merged,
            cursor: 0,
            deadline,
            budget,
            header,
            ended: false,
            snapshot_mode: false,
            next_identity_check: (!guards.is_empty())
                .then(|| Instant::now() + MEMBERSHIP_CHECK_INTERVAL),
            guards,
            identity_failures: 0,
            notice_reserve: 16 * 1024,
            keep_open: false,
        })
    }
    pub async fn next(&mut self) -> Result<Option<String>> {
        loop {
            if self.ended {
                return Ok(None);
            }
            let shared_error = self
                .budget
                .lock()
                .map_err(|_| "merged log budget unavailable")?
                .stopped
                .clone();
            if let Some(error) = shared_error {
                self.stop();
                return Err(error);
            }
            if Instant::now() >= self.deadline {
                self.stop();
                return Err("merged log session deadline reached; all streams stopped; start a new session to continue".into());
            }
            if self
                .next_identity_check
                .is_some_and(|next| Instant::now() >= next)
            {
                let deadline = self.deadline.min(Instant::now() + MEMBERSHIP_CHECK_BUDGET);
                let result = verify_guards(&self.guards, deadline).await;
                if Instant::now() >= self.deadline {
                    self.stop();
                    return Err("merged log session deadline reached during identity verification; all streams stopped".into());
                }
                self.next_identity_check = Some(Instant::now() + MEMBERSHIP_CHECK_INTERVAL);
                match result {
                    Ok(()) => self.identity_failures = 0,
                    Err(ReopenFailure::Stop(error)) => {
                        self.stop();
                        return Err(format!(
                            "{error}; all {} log streams stopped",
                            if self.keep_open {
                                "dynamic-membership"
                            } else {
                                "fixed-membership"
                            }
                        ));
                    }
                    Err(ReopenFailure::Retry) => {
                        self.identity_failures += 1;
                        if self.identity_failures >= 3 {
                            self.stop();
                            return Err("fixed log membership could not be verified three times; all streams stopped; capture sources again".into());
                        }
                        return Ok(Some("[Log stream membership verification temporarily unavailable; captured sources retained; no new Pods admitted]".into()));
                    }
                }
            }
            if let Some(header) = self.header.pop_front() {
                return Ok(Some(header));
            }
            let count = self.sources.len();
            let keep_open = self.keep_open;
            for distance in 0..count {
                let index = (self.cursor + distance) % count;
                if let Some(notice) = self.sources[index].notice.take() {
                    self.cursor = (index + 1) % count;
                    return Ok(Some(notice));
                }
            }
            let ready = std::future::poll_fn(|cx| {
                let mut active = false;
                for distance in 0..count {
                    let index = (self.cursor + distance) % count;
                    if let Some(pending) = self.sources[index].pending.as_mut() {
                        active = true;
                        if let Poll::Ready(result) = pending.as_mut().poll(cx) {
                            return Poll::Ready(Some((index, result)));
                        }
                    }
                }
                if active || keep_open {
                    Poll::Pending
                } else {
                    Poll::Ready(None)
                }
            });
            let wait_deadline = self
                .next_identity_check
                .map_or(self.deadline, |check| check.min(self.deadline));
            let ready = match tokio::time::timeout_at(wait_deadline, ready).await {
                Ok(ready) => ready,
                Err(_) => {
                    if Instant::now() < self.deadline {
                        continue;
                    }
                    self.stop();
                    return Err("merged log session deadline reached; all streams stopped; start a new session to continue".into());
                }
            };
            let Some((index, (feed, result))) = ready else {
                self.stop();
                return Ok(None);
            };
            self.sources[index].pending = None;
            self.cursor = (index + 1) % count;
            // A shared budget violation is fatal to every source, not a recoverable
            // per-container failure. No remaining buffered data is delivered.
            let shared_error = self
                .budget
                .lock()
                .map_err(|_| "merged log budget unavailable")?
                .stopped
                .clone();
            if let Some(error) = shared_error {
                self.stop();
                return Err(error);
            }
            return match result {
                Ok(Some(line)) => {
                    let output =
                        source_line(&self.sources[index].identity, &line, feed.last_control);
                    if let Err(error) =
                        charge_merge(&self.budget, output.len().saturating_sub(line.len()), 0)
                    {
                        self.stop();
                        return Err(error);
                    }
                    self.sources[index].pending = Some(pending_source(feed));
                    Ok(Some(output))
                }
                Ok(None) => Ok(Some(source_notice(
                    &self.sources[index].identity,
                    if self.snapshot_mode && feed.total_bytes >= MAX_LOG_BYTES {
                        "ended at the requested server byte limit; the final record may be incomplete; other container streams continue"
                    } else {
                        "ended; other container streams continue"
                    },
                ))),
                Err(error) => Ok(Some(source_notice(
                    &self.sources[index].identity,
                    &safety::safe_text(&error, 512),
                ))),
            };
        }
    }
    fn stop(&mut self) {
        self.ended = true;
        self.sources.clear();
    }
    /// A bounded timestamp-ordered projection. Selection is global across the
    /// sources, rather than multiplying the requested head/tail by their count.
    pub(crate) async fn snapshot(mut self, options: ReadOptions) -> Result<String> {
        let options = options.validate()?;
        self.snapshot_mode = true;
        let notice_bytes = self.notice_reserve;
        let mut notices = String::new();
        let mut lines = Vec::new();
        let mut bytes = 0usize;
        let mut ordinal = 0usize;
        let mut discarded = false;
        loop {
            let line = match self.next().await {
                Ok(Some(line)) => line,
                Ok(None) => break,
                Err(error) => {
                    notices.push_str(&format!("[Stopped: {}]\n", safety::safe_text(&error, 512)));
                    break;
                }
            };
            if line.starts_with("[Log stream ") {
                if notices.len() + line.len() < notice_bytes - 1024 {
                    notices.push_str(&line);
                    notices.push('\n');
                }
                continue;
            }
            let key = line
                .split_once(' ')
                .and_then(|(time, _)| crate::metrics::timestamp_key(time));
            let entry = (key, ordinal, line);
            ordinal += 1;
            let index = lines.partition_point(|old: &(Option<(i64, String)>, usize, String)| {
                (old.0.as_ref(), old.1) <= (entry.0.as_ref(), entry.1)
            });
            bytes += entry.2.len() + 1;
            lines.insert(index, entry);
            while lines.len() > options.tail_lines as usize || bytes > MAX_LOG_BYTES - notice_bytes
            {
                let removed = if options.head {
                    lines.pop().ok_or("empty merged snapshot")?
                } else {
                    lines.remove(0)
                };
                bytes -= removed.2.len() + 1;
                discarded = true;
            }
        }
        notices.push_str("[Log stream merge snapshot: timestamped records ordered by Kubernetes timestamp; untimestamped records precede them in arrival order; requested line limit is shared across containers]\n");
        if discarded {
            notices.push_str("[Log stream merge snapshot: records omitted by the shared head/tail or display byte limit]\n");
        }
        for (_, _, line) in lines {
            notices.push_str(&line);
            notices.push('\n');
        }
        Ok(notices)
    }
}
