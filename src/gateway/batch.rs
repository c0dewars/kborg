//! Reviewed bulk actions on marked objects: every target is prepared and
//! dry-run individually through [`Gateway::prepare`], shown in one combined
//! review, then executed one at a time through [`Gateway::execute`], stopping
//! at the first failure.
use super::*;
use std::time::Instant;

/// Upper bound on objects in one reviewed batch.
pub const MAX_BATCH: usize = 50;
const REVIEW_WINDOW: Duration = Duration::from_secs(60);
/// Budget for the combined review text (50 targets with identity lines).
const BATCH_REVIEW_CHARS: usize = 48 * 1024;

/// One marked object, identified by its UID at the time it was marked.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BatchTarget {
    pub namespace: String,
    pub name: String,
    pub uid: String,
}
impl BatchTarget {
    fn label(&self) -> String {
        format!("{}/{}", self.namespace, self.name)
    }
}

/// What the TUI asks for: one action on marked objects of one resource kind
/// in the context the marks were made in.
pub struct BatchRequest {
    pub context: String,
    /// The resource key of the current table (for example `deployments.apps`).
    pub resource: String,
    pub action: Action,
    pub targets: Vec<BatchTarget>,
}

/// Every target prepared and dry-run, sealed with one combined review.
pub struct PreparedBatch {
    context: String,
    session: String,
    resource: String,
    action: Action,
    items: Vec<Prepared>,
    summary: String,
    created: Instant,
}
impl PreparedBatch {
    pub fn summary(&self) -> &str {
        &self.summary
    }
    pub fn confirmation(&self) -> String {
        batch_confirmation(&self.action, self.items.len())
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Outcome of an executed batch: succeeded objects in order, the first
/// failure (if any) and the objects never attempted after it.
#[derive(Debug)]
pub struct BatchReport {
    pub verb: &'static str,
    pub succeeded: Vec<BatchTarget>,
    pub failed: Option<(BatchTarget, String)>,
    pub not_attempted: Vec<BatchTarget>,
}
impl BatchReport {
    pub fn complete(&self) -> bool {
        self.failed.is_none() && self.not_attempted.is_empty()
    }
    pub fn text(&self) -> String {
        let total =
            self.succeeded.len() + usize::from(self.failed.is_some()) + self.not_attempted.len();
        let list = |targets: &[BatchTarget]| {
            if targets.is_empty() {
                "  (none)".to_owned()
            } else {
                targets
                    .iter()
                    .map(|target| format!("  {}  UID {}", target.label(), target.uid))
                    .collect::<Vec<_>>()
                    .join("\n")
            }
        };
        let mut text = if self.complete() {
            format!(
                "BATCH COMPLETE: {} {total} object{}\n\nSucceeded ({}):\n{}",
                self.verb,
                plural(total),
                self.succeeded.len(),
                list(&self.succeeded)
            )
        } else {
            format!(
                "BATCH STOPPED after {} of {total}: {}\n\nSucceeded ({}):\n{}",
                self.succeeded.len(),
                self.verb,
                self.succeeded.len(),
                list(&self.succeeded)
            )
        };
        if let Some((target, error)) = &self.failed {
            text.push_str(&format!(
                "\n\nFailed (1):\n  {}  UID {}\n  {error}",
                target.label(),
                target.uid
            ));
        }
        if !self.complete() {
            text.push_str(&format!(
                "\n\nNot attempted ({}):\n{}",
                self.not_attempted.len(),
                list(&self.not_attempted)
            ));
        }
        text.push_str(
            "\n\nAccepted writes are not rolled back and nothing is retried automatically.\nInspect current cluster state before preparing another batch.",
        );
        text
    }
}

fn plural(count: usize) -> &'static str {
    if count == 1 { "" } else { "s" }
}

/// The exact text the user types to approve a batch: verb plus exact count,
/// for example `delete 7 objects`.
pub fn batch_confirmation(action: &Action, count: usize) -> String {
    format!(
        "{} {count} object{}",
        action.operation_name(),
        plural(count)
    )
}

/// Whether `action` may run as a batch on the table resource `resource`.
pub fn batch_supported(resource: &str, action: &Action) -> bool {
    match action {
        Action::Delete => matches!(
            resource,
            "pods"
                | "deployments.apps"
                | "statefulsets.apps"
                | "daemonsets.apps"
                | "services"
                | "jobs.batch"
                | "cronjobs.batch"
        ),
        Action::Restart => matches!(
            resource,
            "deployments.apps" | "statefulsets.apps" | "daemonsets.apps"
        ),
        Action::Suspend(_) => resource == "cronjobs.batch",
        _ => false,
    }
}

/// Local bounds checked before any server call: size, action family,
/// resource kind and duplicate or empty identities.
pub fn check_batch(request: &BatchRequest) -> Result<()> {
    if request.targets.is_empty() {
        return Err("no marked objects; Space marks rows".into());
    }
    if request.targets.len() > MAX_BATCH {
        return Err(format!(
            "{} objects marked; one batch covers at most {MAX_BATCH}. Unmark some rows and try again.",
            request.targets.len()
        ));
    }
    if !batch_supported(&request.resource, &request.action) {
        return Err(format!(
            "bulk {} is not available for {}; batches cover delete, workload restart and CronJob suspend/resume",
            request.action.operation_name(),
            request.resource
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    for target in &request.targets {
        if target.uid.is_empty() || !seen.insert(target.uid.as_str()) {
            return Err(format!(
                "marked object {} has a missing or duplicate UID; clear marks and mark again",
                target.label()
            ));
        }
    }
    Ok(())
}

impl Gateway {
    /// Prepare (policy check plus server dry run) every marked target. Any
    /// refusal refuses the whole batch before a review is shown.
    pub async fn prepare_batch(&self, request: BatchRequest) -> Result<PreparedBatch> {
        if request.context != self.context {
            return Err(format!(
                "marks were made in context {} but the connection is {}; a batch never mixes contexts",
                request.context, self.context
            ));
        }
        check_batch(&request)?;
        let created = Instant::now();
        let mut items = Vec::with_capacity(request.targets.len());
        for target in &request.targets {
            let prepared = self
                .prepare(
                    &request.resource,
                    &target.namespace,
                    &target.name,
                    request.action.clone(),
                )
                .await
                .map_err(|error| {
                    format!(
                        "Batch refused at {}; nothing was changed.\n{error}",
                        target.label()
                    )
                })?;
            if prepared.target.uid != target.uid {
                return Err(format!(
                    "Batch refused; nothing was changed.\n{} was replaced since it was marked (UID changed).",
                    target.label()
                ));
            }
            items.push(prepared);
        }
        if created.elapsed() > REVIEW_WINDOW {
            return Err("dry runs took longer than the review window; nothing was changed".into());
        }
        let summary = self.batch_summary(&request, &items)?;
        Ok(PreparedBatch {
            context: self.context.clone(),
            session: self.session.clone(),
            resource: request.resource,
            action: request.action,
            items,
            summary,
            created,
        })
    }
    fn batch_summary(&self, request: &BatchRequest, items: &[Prepared]) -> Result<String> {
        let confirmation = batch_confirmation(&request.action, items.len());
        let effect = match &request.action {
            Action::Delete => {
                "delete each object; background cascading; server default grace period; no force"
            }
            Action::Restart => {
                "rollout restart each workload: set the Pod template annotation kborg.dev/restartedAt; a rolling replacement starts"
            }
            Action::Suspend(true) => {
                "SUSPEND each CronJob: no new Jobs are scheduled; Jobs already running continue"
            }
            Action::Suspend(false) => {
                "RESUME each CronJob: scheduling restarts; missed runs within startingDeadlineSeconds may start at once"
            }
            _ => return Err("this action cannot run as a batch".into()),
        };
        let targets = items
            .iter()
            .enumerate()
            .map(|(index, prepared)| {
                format!(
                    "{:>3}. {} {}/{}\n     UID {}  resourceVersion {}",
                    index + 1,
                    prepared.kind.kind,
                    prepared.target.namespace,
                    prepared.target.name,
                    prepared.target.uid,
                    prepared.target.version
                )
            })
            .collect::<Vec<_>>()
            .join("\n");
        let summary = format!(
            "BATCH OF {count} OBJECTS / one approval covers every target below\n{mode}\nserver: {server}\ncontext: {context}\nresource: {resource}\naction: {verb} (each target, one at a time, in this order)\neffect: {effect}\n{checked}\n\nTARGETS ({count})\n{targets}\n\nBefore each write the target's UID and resourceVersion are checked again.\nThe batch STOPS at the first failure; earlier writes are not rolled back.\nOne audit record is written per object.\nType {confirmation} to execute. Approval expires 60 seconds after the first dry run.",
            count = items.len(),
            mode = if self.is_demo() {
                "DEMO"
            } else {
                "PRODUCTION / WRITE ENABLED"
            },
            server = self.server,
            context = self.context,
            resource = request.resource,
            verb = request.action.operation_name(),
            checked = if self.is_demo() {
                "Every target passed policy checks; demo validation only, no cluster contacted."
            } else {
                "Every target passed policy checks and a server dry run."
            },
        );
        safety::review_text(&summary, BATCH_REVIEW_CHARS)
    }
    /// Execute a reviewed batch in order. Errors returned here mean nothing
    /// was attempted; once writing starts the report says what happened.
    pub async fn execute_batch(
        &mut self,
        batch: PreparedBatch,
        typed: &str,
    ) -> Result<BatchReport> {
        if batch.session != self.session || batch.context != self.context {
            return Err(
                "session or context changed; nothing was changed. Prepare and review again.".into(),
            );
        }
        if batch.created.elapsed() > REVIEW_WINDOW {
            return Err(
                "batch review expired; nothing was changed. Prepare and review again.".into(),
            );
        }
        if typed != batch.confirmation() {
            return Err(format!(
                "confirmation must be exactly '{}'; nothing was changed",
                batch.confirmation()
            ));
        }
        let mut report = BatchReport {
            verb: batch.action.operation_name(),
            succeeded: vec![],
            failed: None,
            not_attempted: vec![],
        };
        let resource = batch.resource;
        let label = |prepared: &Prepared| BatchTarget {
            namespace: prepared.target.namespace.clone(),
            name: prepared.target.name.clone(),
            uid: prepared.target.uid.clone(),
        };
        let mut items = batch.items.into_iter();
        while let Some(prepared) = items.next() {
            let target = label(&prepared);
            match self.execute_batch_item(&resource, prepared).await {
                Ok(_) => report.succeeded.push(target),
                Err(error) => {
                    report.failed = Some((target, error));
                    report.not_attempted = items.map(|prepared| label(&prepared)).collect();
                    break;
                }
            }
        }
        Ok(report)
    }
    async fn execute_batch_item(&mut self, resource: &str, prepared: Prepared) -> Result<String> {
        let target = &prepared.target;
        let current = tokio::time::timeout(
            OPERATION_BUDGET,
            self.raw_get(resource, &target.namespace, &target.name),
        )
        .await
        .map_err(|_| "identity recheck timed out; not written".to_owned())?
        .map_err(|error| format!("identity recheck failed; not written: {error}"))?;
        if current["metadata"]["uid"].as_str() != Some(target.uid.as_str())
            || current["metadata"]["resourceVersion"].as_str() != Some(target.version.as_str())
        {
            return Err(
                "object changed since review (UID or resourceVersion differs); not written".into(),
            );
        }
        let approval = Approval::confirm(
            target.clone(),
            prepared.action.clone(),
            &format!("{}/{}", target.namespace, target.name),
        )?;
        self.execute(prepared, approval).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::runtime;

    async fn demo() -> Result<Gateway> {
        let mut gateway = Gateway::connect(Connection {
            demo: true,
            writable: true,
            ..Default::default()
        })
        .await?;
        let template = gateway
            .demo
            .iter()
            .find(|object| object["kind"] == "Deployment")
            .cloned()
            .ok_or("demo Deployment missing")?;
        for (namespace, name) in [("demo", "web"), ("demo", "jobs"), ("kube-system", "dns")] {
            let mut object = template.clone();
            object["metadata"]["name"] = json!(name);
            object["metadata"]["namespace"] = json!(namespace);
            object["metadata"]["uid"] = json!(format!("uid-{namespace}-{name}"));
            gateway.demo.push(object);
        }
        Ok(gateway)
    }
    fn target(gateway: &Gateway, namespace: &str, name: &str) -> BatchTarget {
        let uid = gateway
            .demo
            .iter()
            .find(|object| {
                object["kind"] == "Deployment"
                    && object["metadata"]["name"] == name
                    && object["metadata"]["namespace"] == namespace
            })
            .and_then(|object| object["metadata"]["uid"].as_str())
            .unwrap_or("")
            .to_owned();
        BatchTarget {
            namespace: namespace.into(),
            name: name.into(),
            uid,
        }
    }
    fn request(gateway: &Gateway, action: Action, names: &[(&str, &str)]) -> BatchRequest {
        BatchRequest {
            context: gateway.context.clone(),
            resource: "deployments.apps".into(),
            action,
            targets: names
                .iter()
                .map(|(namespace, name)| target(gateway, namespace, name))
                .collect(),
        }
    }

    #[test]
    fn confirmation_names_the_verb_and_exact_count() {
        assert_eq!(batch_confirmation(&Action::Delete, 7), "delete 7 objects");
        assert_eq!(batch_confirmation(&Action::Restart, 1), "restart 1 object");
        assert_eq!(
            batch_confirmation(&Action::Suspend(true), 2),
            "suspend 2 objects"
        );
        assert_eq!(
            batch_confirmation(&Action::Suspend(false), 3),
            "resume 3 objects"
        );
    }

    #[test]
    fn batch_is_bounded_to_fifty_supported_targets_of_one_kind() {
        let targets = |count: usize| {
            (0..count)
                .map(|i| BatchTarget {
                    namespace: "demo".into(),
                    name: format!("app-{i}"),
                    uid: format!("uid-{i}"),
                })
                .collect::<Vec<_>>()
        };
        let request = |resource: &str, action: Action, count: usize| BatchRequest {
            context: "test".into(),
            resource: resource.into(),
            action,
            targets: targets(count),
        };
        assert!(check_batch(&request("deployments.apps", Action::Delete, MAX_BATCH)).is_ok());
        let error = check_batch(&request("deployments.apps", Action::Delete, MAX_BATCH + 1))
            .err()
            .unwrap_or_default();
        assert!(error.contains("at most 50"), "{error}");
        assert!(check_batch(&request("deployments.apps", Action::Delete, 0)).is_err());
        assert!(check_batch(&request("pods", Action::Restart, 2)).is_err());
        assert!(check_batch(&request("deployments.apps", Action::Suspend(true), 2)).is_err());
        assert!(check_batch(&request("cronjobs.batch", Action::Suspend(false), 2)).is_ok());
        assert!(check_batch(&request("configmaps", Action::Delete, 2)).is_err());
        assert!(check_batch(&request("deployments.apps", Action::Scale(2), 2)).is_err());
        assert!(check_batch(&request("deployments.apps", Action::Kill, 2)).is_err());
        let mut duplicate = request("deployments.apps", Action::Delete, 2);
        duplicate.targets[1].uid = duplicate.targets[0].uid.clone();
        assert!(check_batch(&duplicate).is_err());
    }

    #[test]
    fn batch_refuses_other_contexts_and_protected_namespace_targets() -> Result<()> {
        runtime()?.block_on(async {
            let gateway = demo().await?;
            let mut other = request(&gateway, Action::Delete, &[("demo", "api")]);
            other.context = "elsewhere".into();
            let error = gateway.prepare_batch(other).await.err().unwrap_or_default();
            assert!(error.contains("never mixes contexts"), "{error}");
            let protected = request(
                &gateway,
                Action::Delete,
                &[("demo", "api"), ("kube-system", "dns")],
            );
            let error = gateway
                .prepare_batch(protected)
                .await
                .err()
                .unwrap_or_default();
            assert!(
                error.contains("kube-system/dns") && error.contains("system-namespace"),
                "{error}"
            );
            let mut replaced = request(&gateway, Action::Delete, &[("demo", "api")]);
            replaced.targets[0].uid = "old-uid".into();
            assert!(gateway.prepare_batch(replaced).await.is_err());
            let mut wrong_kind = request(&gateway, Action::Delete, &[("demo", "api")]);
            wrong_kind.resource = "services".into();
            assert!(gateway.prepare_batch(wrong_kind).await.is_err());
            Ok(())
        })
    }

    #[test]
    fn combined_review_lists_every_target_and_requires_the_count() -> Result<()> {
        runtime()?.block_on(async {
            let mut gateway = demo().await?;
            let batch = gateway
                .prepare_batch(request(
                    &gateway,
                    Action::Restart,
                    &[("demo", "api"), ("demo", "web")],
                ))
                .await?;
            assert_eq!(batch.confirmation(), "restart 2 objects");
            let summary = batch.summary();
            assert!(summary.starts_with("BATCH OF 2 OBJECTS"));
            for needle in [
                "Deployment demo/api",
                "Deployment demo/web",
                "UID uid-demo-web",
                "resourceVersion 1",
                "STOPS at the first failure",
                "Type restart 2 objects",
            ] {
                assert!(summary.contains(needle), "{needle}: {summary}");
            }
            let error = gateway
                .execute_batch(batch, "demo/api")
                .await
                .err()
                .unwrap_or_default();
            assert!(error.contains("nothing was changed"), "{error}");
            let mut expired = gateway
                .prepare_batch(request(&gateway, Action::Delete, &[("demo", "api")]))
                .await?;
            expired.created = Instant::now() - Duration::from_secs(61);
            assert!(
                gateway
                    .execute_batch(expired, "delete 1 object")
                    .await
                    .is_err()
            );
            assert!(gateway.get("deployments", "demo", "api").await.is_ok());
            Ok(())
        })
    }

    #[test]
    fn execution_stops_at_first_failure_and_reports_every_target() -> Result<()> {
        runtime()?.block_on(async {
            let mut gateway = demo().await?;
            let batch = gateway
                .prepare_batch(request(
                    &gateway,
                    Action::Delete,
                    &[("demo", "api"), ("demo", "web"), ("demo", "jobs")],
                ))
                .await?;
            // Someone else changes the second target after the review.
            for object in &mut gateway.demo {
                if object["metadata"]["uid"] == "uid-demo-web" {
                    object["metadata"]["resourceVersion"] = json!("2");
                }
            }
            let report = gateway.execute_batch(batch, "delete 3 objects").await?;
            assert!(!report.complete());
            assert_eq!(report.succeeded.len(), 1);
            assert_eq!(report.succeeded[0].name, "api");
            let (failed, error) = report.failed.clone().ok_or("missing failure")?;
            assert_eq!(failed.name, "web");
            assert!(error.contains("resourceVersion"), "{error}");
            assert_eq!(report.not_attempted.len(), 1);
            assert_eq!(report.not_attempted[0].name, "jobs");
            let text = report.text();
            assert!(text.starts_with("BATCH STOPPED after 1 of 3"), "{text}");
            assert!(text.contains("Not attempted (1)") && text.contains("demo/jobs"));
            assert!(gateway.get("deployments", "demo", "api").await.is_err());
            assert!(gateway.get("deployments", "demo", "web").await.is_ok());
            assert!(gateway.get("deployments", "demo", "jobs").await.is_ok());

            let batch = gateway
                .prepare_batch(request(
                    &gateway,
                    Action::Delete,
                    &[("demo", "web"), ("demo", "jobs")],
                ))
                .await?;
            let report = gateway.execute_batch(batch, "delete 2 objects").await?;
            assert!(report.complete() && report.text().starts_with("BATCH COMPLETE"));
            assert!(gateway.get("deployments", "demo", "jobs").await.is_err());
            Ok(())
        })
    }
}
