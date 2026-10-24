//! Pod, merged and workload-membership log reads and follows.
use super::*;

impl Gateway {
    /// Container names from the pod spec, including init and ephemeral containers.
    pub async fn containers(&self, ns: &str, name: &str) -> Result<Vec<String>> {
        let pod = self.get("pods", ns, name).await?;
        let mut names = Vec::new();
        for field in ["containers", "initContainers", "ephemeralContainers"] {
            if let Some(containers) = pod["spec"][field].as_array() {
                for container in containers {
                    if let Some(name) = container["name"].as_str().filter(|n| safety::valid_id(n)) {
                        if !names.iter().any(|n| n == name) {
                            names.push(name.to_owned());
                        }
                        if names.len() > 1000 {
                            return Err("container listing exceeded limit".into());
                        }
                    }
                }
            }
        }
        if self.is_demo() && names.is_empty() {
            names.push("app".into());
        }
        Ok(names)
    }
    /// Explicit opt-in is checked here immediately before opening the log request.
    pub async fn follow_logs(
        &self,
        ns: &str,
        name: &str,
        container: Option<String>,
        previous: bool,
    ) -> Result<crate::logs::LogFeed> {
        self.follow_logs_with_options(
            ns,
            name,
            container,
            previous,
            crate::logs::FollowOptions::default(),
        )
        .await
    }
    pub async fn follow_logs_with_options(
        &self,
        ns: &str,
        name: &str,
        container: Option<String>,
        previous: bool,
        options: crate::logs::FollowOptions,
    ) -> Result<crate::logs::LogFeed> {
        self.follow_logs_with_read_options(
            ns,
            name,
            container,
            previous,
            options,
            crate::logs::ReadOptions::default(),
        )
        .await
    }
    pub async fn follow_logs_with_read_options(
        &self,
        ns: &str,
        name: &str,
        container: Option<String>,
        previous: bool,
        options: crate::logs::FollowOptions,
        read_options: crate::logs::ReadOptions,
    ) -> Result<crate::logs::LogFeed> {
        let options = options.validate()?;
        let read_options = read_options.validate()?;
        if !self.logs {
            return Err("logs may contain sensitive data; opt in with --allow-logs".into());
        }
        if !safety::valid_id(ns)
            || name.is_empty()
            || name.len() > 253
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
            || container.as_deref().is_some_and(|c| !safety::valid_id(c))
        {
            return Err("invalid log target or container".into());
        }
        if previous {
            return Err("previous container logs support snapshots only; omit --follow".into());
        }
        if self.is_demo() {
            let text = self
                .logs_with_options(ns, name, container, false, read_options)
                .await?;
            return crate::logs::LogFeed::with_options(
                futures_util::io::Cursor::new(text.into_bytes()),
                options,
            );
        }
        let pods: Api<Pod> = Api::namespaced(self.client.clone().ok_or("no client")?, ns);
        let stream = tokio::time::timeout(
            OPERATION_BUDGET,
            pods.log_stream(name, &read_options.params(container, false, true)?),
        )
        .await
        .map_err(|_| "log connection timeout")?
        .map_err(api_error)?;
        crate::logs::LogFeed::with_read_options(stream, options, read_options)
    }
    /// Explicitly bind reconnection to a pod selected by UID. Extra pod metadata
    /// reads occur only in this workflow; ordinary log-only RBAC remains usable.
    pub async fn follow_logs_resumable(
        &self,
        ns: &str,
        name: &str,
        container: Option<String>,
        uid: &str,
        options: crate::logs::FollowOptions,
        read_options: crate::logs::ReadOptions,
    ) -> Result<crate::logs::LogFeed> {
        let feed = self
            .follow_logs_with_read_options(
                ns,
                name,
                container.clone(),
                false,
                options,
                read_options,
            )
            .await?;
        if self.is_demo() || read_options.head {
            return Ok(feed);
        }
        let api = Api::namespaced(self.client.clone().ok_or("no client")?, ns);
        // Validate after opening, before any body data is delivered to callers.
        crate::logs::verify_uid(&api, name, uid).await?;
        feed.with_resume(api, name, uid, container, read_options)
    }
    pub async fn logs(
        &self,
        ns: &str,
        name: &str,
        container: Option<String>,
        previous: bool,
    ) -> Result<String> {
        self.logs_with_options(
            ns,
            name,
            container,
            previous,
            crate::logs::ReadOptions::default(),
        )
        .await
    }
    pub async fn logs_with_options(
        &self,
        ns: &str,
        name: &str,
        container: Option<String>,
        previous: bool,
        options: crate::logs::ReadOptions,
    ) -> Result<String> {
        let options = options.validate()?;
        if !safety::valid_id(ns)
            || name.is_empty()
            || name.len() > 253
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        {
            return Err("invalid log target".into());
        }
        if container.as_deref().is_some_and(|c| !safety::valid_id(c)) {
            return Err("invalid log container".into());
        }
        if !self.logs {
            return Err("logs may contain sensitive data; opt in with --allow-logs".into());
        }
        if self.is_demo() {
            if !self.demo.iter().any(|v| {
                v["kind"] == "Pod"
                    && v["metadata"]["namespace"] == ns
                    && v["metadata"]["name"] == name
            }) {
                return Err("demo pod not found in selected namespace".into());
            }
            return Ok(crate::logs::snapshot_text(
                "2026-10-09T20:00:00Z INFO KBorg demo started\n2026-10-09T20:00:01Z WARN connection retry\n2026-10-09T20:00:02Z INFO request served",
                options,
            ));
        }
        let pods: Api<Pod> = Api::namespaced(self.client.clone().ok_or("no client")?, ns);
        let output = pods
            .logs(name, &options.params(container, previous, false)?)
            .await
            .map_err(api_error)?;
        Ok(crate::logs::snapshot_text(&output, options))
    }
    /// Explicit Pod-wide read: ordinary containers only, one captured Pod UID,
    /// bounded source count and a shared session/display budget.
    pub async fn merged_logs(
        &self,
        ns: &str,
        name: &str,
        uid: &str,
        previous: bool,
        read_options: crate::logs::ReadOptions,
    ) -> Result<String> {
        let read_options = read_options.validate()?;
        if !self.logs {
            return Err("logs may contain sensitive data; opt in with --allow-logs".into());
        }
        crate::logs::validate_merge_target(ns, name, uid)?;
        let feed = if self.is_demo() {
            self.demo_merged_logs(
                (ns, name, uid),
                previous,
                crate::logs::FollowOptions { seconds: 15 },
                read_options,
            )
            .await?
        } else {
            let api = Api::namespaced(self.client.clone().ok_or("no client")?, ns);
            crate::logs::MergedLogFeed::open(api, name, uid, previous, None, read_options).await?
        };
        feed.snapshot(read_options).await
    }
    pub async fn follow_merged_logs(
        &self,
        ns: &str,
        name: &str,
        uid: &str,
        options: crate::logs::FollowOptions,
        read_options: crate::logs::ReadOptions,
    ) -> Result<crate::logs::MergedLogFeed> {
        let options = options.validate()?;
        let read_options = read_options.validate()?;
        if !self.logs {
            return Err("logs may contain sensitive data; opt in with --allow-logs".into());
        }
        crate::logs::validate_merge_target(ns, name, uid)?;
        if self.is_demo() {
            return self
                .demo_merged_logs((ns, name, uid), false, options, read_options)
                .await;
        }
        let api = Api::namespaced(self.client.clone().ok_or("no client")?, ns);
        crate::logs::MergedLogFeed::open(api, name, uid, false, Some(options), read_options).await
    }
    pub(super) async fn demo_merged_logs(
        &self,
        target: (&str, &str, &str),
        previous: bool,
        options: crate::logs::FollowOptions,
        read_options: crate::logs::ReadOptions,
    ) -> Result<crate::logs::MergedLogFeed> {
        let (ns, name, uid) = target;
        let pod: Pod = serde_json::from_value(self.raw_get("pods", ns, name).await?)
            .map_err(|_| "invalid demo Pod for merged logs")?;
        let names = crate::logs::ordinary_container_names(&pod, ns, name, uid)?;
        let mut sources = Vec::with_capacity(names.len());
        for container in names {
            let text = self
                .logs_with_options(ns, name, Some(container.clone()), previous, read_options)
                .await?;
            let feed = crate::logs::LogFeed::with_read_options(
                futures_util::io::Cursor::new(text.into_bytes()),
                options,
                read_options,
            );
            sources.push((container, feed));
        }
        crate::logs::MergedLogFeed::from_sources(ns, name, uid, sources, options)
    }
    /// Capture one complete, connection-bound Pod membership snapshot. A reload
    /// reuses these sources; discovering new Pods is an explicit new capture.
    pub async fn capture_log_membership(
        &self,
        namespace: &str,
        selection: crate::logs::PodLogSelection,
    ) -> Result<crate::logs::LogMembership> {
        use crate::logs::{LogMembership, LogParent, PodLogSelection};
        if !self.logs {
            return Err("logs may contain sensitive data; opt in with --allow-logs".into());
        }
        if !safety::valid_id(namespace) {
            return Err("workload logs require one concrete namespace".into());
        }
        let deadline = tokio::time::Instant::now() + OPERATION_BUDGET;
        let mut bytes = 0;
        let mut parents = Vec::new();
        let (description, selectors) = match selection {
            PodLogSelection::Selector(selector) => {
                crate::selectors::validate(&selector)?;
                if selector.trim().is_empty() {
                    return Err("workload logs require an explicit nonempty Pod selector".into());
                }
                (
                    format!("selector {selector}; fixed membership"),
                    vec![(selector, None)],
                )
            }
            PodLogSelection::Workload {
                resource,
                name,
                uid,
            } => {
                crate::logs::validate_merge_target(namespace, &name, &uid)?;
                let kind = self.find(&resource)?.clone();
                if !matches!(
                    (
                        kind.group.as_str(),
                        kind.version.as_str(),
                        kind.kind.as_str()
                    ),
                    (
                        "apps",
                        "v1",
                        "Deployment" | "StatefulSet" | "DaemonSet" | "ReplicaSet"
                    ) | ("batch", "v1", "Job" | "CronJob")
                        | ("", "v1", "Service" | "ReplicationController")
                ) {
                    return Err("workload logs support native Deployments, StatefulSets, DaemonSets, ReplicaSets, Jobs, CronJobs, Services and ReplicationControllers".into());
                }
                let object =
                    tokio::time::timeout_at(deadline, self.raw_get(&kind.key(), namespace, &name))
                        .await
                        .map_err(|_| "workload log membership lookup timed out")??;
                crate::logs::membership_bytes(&mut bytes, &object)?;
                if object["metadata"]["namespace"] != namespace
                    || object["metadata"]["name"] != name
                    || object["metadata"]["uid"] != uid
                {
                    return Err(
                        "selected workload identity changed or escaped namespace; select it again"
                            .into(),
                    );
                }
                parents.push(LogParent {
                    resource: kind.key(),
                    name: name.clone(),
                    uid: uid.clone(),
                });
                let selectors = if kind.kind == "CronJob" {
                    let jobs = if self.is_demo() {
                        self.demo
                            .iter()
                            .filter(|job| {
                                job["apiVersion"] == "batch/v1"
                                    && job["kind"] == "Job"
                                    && job["metadata"]["namespace"] == namespace
                            })
                            .cloned()
                            .collect::<Vec<_>>()
                    } else {
                        let api: Api<k8s_openapi::api::batch::v1::Job> =
                            Api::namespaced(self.client.clone().ok_or("no client")?, namespace);
                        let list = tokio::time::timeout_at(
                            deadline,
                            api.list(&ListParams {
                                limit: Some((crate::logs::MAX_LOG_JOBS + 1) as u32),
                                ..Default::default()
                            }),
                        )
                        .await
                        .map_err(|_| "CronJob child inventory timed out")?
                        .map_err(api_error)?;
                        if list
                            .metadata
                            .resource_version
                            .as_deref()
                            .is_none_or(|version| {
                                version.is_empty()
                                    || version.len() > 253
                                    || safety::safe_text(version, 253) != version
                            })
                        {
                            return Err("CronJob child inventory omitted a valid resourceVersion; complete membership cannot be established".into());
                        }
                        if list
                            .metadata
                            .continue_
                            .as_ref()
                            .is_some_and(|token| !token.is_empty())
                            || list.items.len() > crate::logs::MAX_LOG_JOBS
                        {
                            return Err("CronJob logs require a complete child Job inventory of at most 64 Jobs; use an explicit Pod selector".into());
                        }
                        list.items
                            .into_iter()
                            .map(|job| {
                                serde_json::to_value(job)
                                    .map_err(|_| "invalid CronJob child Job".to_owned())
                            })
                            .collect::<Result<Vec<_>>>()?
                    };
                    let mut selectors = Vec::new();
                    for mut job in jobs {
                        crate::logs::membership_bytes(&mut bytes, &job)?;
                        if job["metadata"]["namespace"] != namespace {
                            return Err("CronJob child inventory escaped namespace".into());
                        }
                        if !crate::logs::owned_by(&job, "batch/v1", "CronJob", &uid) {
                            continue;
                        }
                        job["apiVersion"] = json!("batch/v1");
                        job["kind"] = json!("Job");
                        let name = job["metadata"]["name"]
                            .as_str()
                            .ok_or("child Job name missing")?
                            .to_owned();
                        let uid = job["metadata"]["uid"]
                            .as_str()
                            .ok_or("child Job UID missing")?
                            .to_owned();
                        crate::logs::validate_merge_target(namespace, &name, &uid)?;
                        selectors.push((
                            crate::insights::pod_selector(&job)?,
                            Some(LogParent {
                                resource: "jobs.batch".into(),
                                name,
                                uid,
                            }),
                        ));
                    }
                    selectors
                } else {
                    vec![(crate::insights::pod_selector(&object)?, None)]
                };
                (
                    format!("{}/{name}; UID {uid}; fixed membership", kind.key()),
                    selectors,
                )
            }
        };
        let mut members = Vec::new();
        for (selector, owner) in selectors {
            let selected = if self.is_demo() {
                let mut selected = Vec::new();
                for value in self.demo.iter().filter(|pod| {
                    pod["apiVersion"] == "v1"
                        && pod["kind"] == "Pod"
                        && pod["metadata"]["namespace"] == namespace
                        && crate::selectors::matches(&selector, &pod["metadata"]["labels"])
                }) {
                    crate::logs::membership_bytes(&mut bytes, value)?;
                    if owner.as_ref().is_some_and(|owner| {
                        !crate::logs::owned_by(value, "batch/v1", "Job", &owner.uid)
                    }) {
                        continue;
                    }
                    let pod: Pod = serde_json::from_value(value.clone())
                        .map_err(|_| "invalid demo Pod membership")?;
                    selected.push(crate::logs::log_pod(&pod, namespace)?);
                }
                selected
            } else {
                let api = Api::namespaced(self.client.clone().ok_or("no client")?, namespace);
                crate::logs::selected_pods(
                    &api,
                    &selector,
                    owner.as_ref().map(|owner| owner.uid.as_str()),
                    deadline,
                    &mut bytes,
                )
                .await?
            };
            if !selected.is_empty()
                && let Some(owner) = owner
            {
                parents.push(owner);
            }
            members.extend(selected);
            if members.len() > crate::logs::MAX_LOG_PODS {
                return Err("workload log membership exceeds 8 Pods; narrow the selector".into());
            }
        }
        LogMembership::new(namespace, description, members, parents, &self.session)
    }
    pub(super) fn log_membership_guards(
        &self,
        membership: &crate::logs::LogMembership,
    ) -> Result<Vec<crate::logs::IdentityGuard>> {
        membership.verify_connection(&self.session)?;
        let mut guards = Vec::new();
        for parent in &membership.parents {
            guards.push(crate::logs::IdentityGuard::Parent {
                api: self.api(self.find(&parent.resource)?, membership.namespace())?,
                name: parent.name.clone(),
                uid: parent.uid.clone(),
            });
        }
        Ok(guards)
    }
    pub async fn membership_logs(
        &self,
        membership: &crate::logs::LogMembership,
        previous: bool,
        read: crate::logs::ReadOptions,
    ) -> Result<String> {
        let feed = self
            .open_membership_logs(membership, previous, None, read)
            .await?;
        feed.snapshot(read).await
    }
    pub async fn follow_membership_logs(
        &self,
        membership: &crate::logs::LogMembership,
        follow: crate::logs::FollowOptions,
        read: crate::logs::ReadOptions,
    ) -> Result<crate::logs::MergedLogFeed> {
        self.open_membership_logs(membership, false, Some(follow), read)
            .await
    }
    /// Explicit opt-in: announced joins/departures on top of the same bounds.
    pub async fn follow_dynamic_logs(
        &self,
        membership: &crate::logs::LogMembership,
        follow: crate::logs::FollowOptions,
        read: crate::logs::ReadOptions,
    ) -> Result<crate::logs::DynamicLogFeed> {
        if !membership.is_dynamic() {
            return Err("dynamic workload logs require an explicit dynamic capture".into());
        }
        let read = read.validate()?;
        let follow = follow.validate()?;
        if read.head {
            return Err(
                "dynamic membership follows new records; choose a tail or time range, not head"
                    .into(),
            );
        }
        let feed = self
            .open_membership_logs(membership, false, Some(follow), read)
            .await?;
        let api = if self.is_demo() {
            None
        } else {
            Some(Api::namespaced(
                self.client.clone().ok_or("no client")?,
                membership.namespace(),
            ))
        };
        crate::logs::DynamicLogFeed::new(self.clone(), api, membership.clone(), feed, read, follow)
    }
    pub(super) async fn open_membership_logs(
        &self,
        membership: &crate::logs::LogMembership,
        previous: bool,
        follow: Option<crate::logs::FollowOptions>,
        read: crate::logs::ReadOptions,
    ) -> Result<crate::logs::MergedLogFeed> {
        if !self.logs {
            return Err("logs may contain sensitive data; opt in with --allow-logs".into());
        }
        membership.verify_connection(&self.session)?;
        if self.is_demo() {
            let read = read.validate()?;
            let options = follow
                .unwrap_or(crate::logs::FollowOptions { seconds: 15 })
                .validate()?;
            if previous && follow.is_some() {
                return Err("previous container logs support snapshots only; omit follow".into());
            }
            let mut sources = Vec::new();
            for member in membership.pods() {
                let pod: Pod = serde_json::from_value(
                    self.raw_get("v1/pods", membership.namespace(), &member.name)
                        .await?,
                )
                .map_err(|_| "invalid demo Pod membership")?;
                let current = crate::logs::log_pod(&pod, membership.namespace())?;
                if current.uid != member.uid || current.containers != member.containers {
                    return Err(
                        "captured demo Pod membership changed; capture sources again".into(),
                    );
                }
                for container in &member.containers {
                    let text = self
                        .logs_with_options(
                            membership.namespace(),
                            &member.name,
                            Some(container.clone()),
                            previous,
                            read,
                        )
                        .await?;
                    sources.push((
                        member.name.clone(),
                        container.clone(),
                        crate::logs::LogFeed::with_read_options(
                            futures_util::io::Cursor::new(text.into_bytes()),
                            options,
                            read,
                        ),
                    ));
                }
            }
            return crate::logs::MergedLogFeed::from_membership_sources(
                membership,
                sources,
                tokio::time::Instant::now() + Duration::from_secs(u64::from(options.seconds)),
                Vec::new(),
            );
        }
        let api = Api::namespaced(
            self.client.clone().ok_or("no client")?,
            membership.namespace(),
        );
        crate::logs::MergedLogFeed::open_membership(
            api,
            membership,
            self.log_membership_guards(membership)?,
            previous,
            follow,
            read,
        )
        .await
    }
}
