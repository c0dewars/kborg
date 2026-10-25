//! Reviewed writes: preparation, dry run, execution, YAML edits and local audit records.
use super::*;

impl Gateway {
    pub(crate) fn operation_api(&self, namespace: &str, capability: usize) -> Result<Api<Pod>> {
        if !self.policy.writable {
            return Err("READ ONLY: operations require --write".into());
        }
        if !self
            .operation_capabilities
            .get(capability)
            .copied()
            .unwrap_or(false)
        {
            return Err("operation requires its explicit capability flag".into());
        }
        if !safety::valid_id(namespace)
            || ["kube-system", "kube-public", "kube-node-lease"].contains(&namespace)
        {
            return Err("operation namespace is prohibited".into());
        }
        let client = self
            .client
            .clone()
            .ok_or("remote operations are unavailable in demo mode")?;
        Ok(Api::namespaced(client, namespace))
    }
    pub(crate) fn node_maintenance_access(&self) -> Result<Client> {
        if !self.policy.writable || !self.allow_node_maintenance {
            return Err("node maintenance requires --write --allow-node-maintenance".into());
        }
        self.client
            .clone()
            .ok_or_else(|| "node maintenance requires a real selected cluster".into())
    }
    #[cfg(test)]
    pub(crate) fn for_node_maintenance_test_client(client: Client) -> Self {
        let mut gateway = Self::for_test_client(client, "default");
        gateway.policy.writable = true;
        gateway.allow_node_maintenance = true;
        gateway
    }
    pub(crate) fn operation_session(&self) -> &str {
        &self.session
    }
    /// A safe editing projection backed by an unredacted, private baseline.
    pub async fn yaml_draft(
        &self,
        key: &str,
        ns: &str,
        name: &str,
    ) -> Result<crate::manifest_edit::Draft> {
        let kind = self.find(key)?.clone();
        if !kind.namespaced || kind.kind.eq_ignore_ascii_case("Secret") {
            return Err("YAML editing supports namespaced non-Secret spec resources".into());
        }
        let original = self.raw_get(key, ns, name).await?;
        crate::manifest_edit::Draft::new(original, kind, self.session.clone())
    }
    pub(super) fn yaml_edit_access(&self) -> Result<()> {
        if !self.policy.writable || !self.allow_yaml_edit {
            return Err("YAML edits require --write --allow-yaml-edit".into());
        }
        Ok(())
    }
    pub async fn prepare_yaml(
        &self,
        draft: &crate::manifest_edit::Draft,
        edited: &str,
    ) -> Result<crate::manifest_edit::PreparedYaml> {
        self.yaml_edit_access()?;
        if draft.session != self.session {
            return Err("session changed; reopen the YAML editor".into());
        }
        let current = self
            .raw_get(&draft.resource(), draft.namespace(), draft.name())
            .await?;
        draft.unchanged(&current)?;
        let (patch, expected_spec, diff) = draft.changes(edited)?;
        let summary = format!(
            "REVIEW YAML SPEC EDIT\nContext: {}\nServer: {}\nResource: {} {}/{}\nUID: {}\nResourceVersion: {}\n\n{}\n\nOnly the listed spec paths change. Hidden values, metadata and status are preserved.\nSpec edits may start workloads or change networking; server RBAC and admission still apply.\n{}\nType {}/{} to apply. Review expires after 60 seconds.",
            self.context,
            self.server,
            draft.resource(),
            draft.namespace(),
            draft.name(),
            draft.original["metadata"]["uid"].as_str().unwrap_or(""),
            draft.original["metadata"]["resourceVersion"]
                .as_str()
                .unwrap_or(""),
            diff,
            if self.is_demo() {
                "Demo validation only; no cluster contacted."
            } else {
                "Server dry-run passed. Admission may behave differently on the real write."
            },
            draft.namespace(),
            draft.name()
        );
        let prepared =
            crate::manifest_edit::PreparedYaml::new(draft.clone(), patch, expected_spec, summary)?;
        if !self.is_demo() {
            let response =
                tokio::time::timeout(OPERATION_BUDGET, self.perform_yaml(&prepared, true))
                    .await
                    .map_err(|_| "YAML dry-run timed out")??;
            crate::manifest_edit::check_admission(
                &response,
                &prepared.expected_spec,
                &draft.original,
            )?;
        }
        Ok(prepared)
    }
    pub(super) async fn perform_yaml(
        &self,
        prepared: &crate::manifest_edit::PreparedYaml,
        dry_run: bool,
    ) -> Result<Value> {
        let draft = &prepared.draft;
        let api = self.api(&draft.kind, draft.namespace())?;
        let url = format!(
            "{}/{}?fieldValidation=Strict{}",
            api.resource_url(),
            draft.name(),
            if dry_run { "&dryRun=All" } else { "" }
        );
        let request = http::Request::builder()
            .method("PATCH")
            .uri(url)
            .header("Content-Type", "application/json-patch+json")
            .body(
                serde_json::to_vec(&prepared.patch)
                    .map_err(|_| "cannot encode reviewed YAML patch")?,
            )
            .map_err(|_| "cannot construct YAML patch request")?;
        self.client
            .as_ref()
            .ok_or("offline backend")?
            .request::<Value>(request)
            .await
            .map_err(|e| self.api_error(e))
    }
    pub async fn execute_yaml(
        &mut self,
        prepared: crate::manifest_edit::PreparedYaml,
        typed: &str,
    ) -> Result<String> {
        self.yaml_edit_access()?;
        if prepared.draft.session != self.session
            || prepared.created.elapsed() > Duration::from_secs(60)
        {
            return Err("YAML review expired or session changed; prepare and review again".into());
        }
        if typed != prepared.confirmation() {
            return Err("confirmation must exactly match namespace/name".into());
        }
        let draft = &prepared.draft;
        let current = self
            .raw_get(&draft.resource(), draft.namespace(), draft.name())
            .await?;
        draft.unchanged(&current)?;
        if self.is_demo() {
            for object in &mut self.demo {
                if object["metadata"]["uid"] == draft.original["metadata"]["uid"] {
                    object["spec"] = prepared.expected_spec.clone();
                    object["metadata"]["resourceVersion"] = json!(format!(
                        "{}y",
                        draft.original["metadata"]["resourceVersion"]
                            .as_str()
                            .unwrap_or("")
                    ));
                }
            }
            return Ok("Demo YAML edit applied; no cluster contacted".into());
        }
        let response = tokio::time::timeout(OPERATION_BUDGET, self.perform_yaml(&prepared, true))
            .await
            .map_err(|_| "YAML revalidation timed out; no write sent")??;
        crate::manifest_edit::check_admission(&response, &prepared.expected_spec, &draft.original)?;
        if prepared.created.elapsed() > Duration::from_secs(60) {
            return Err("YAML review expired during revalidation; review again".into());
        }
        let target = Target {
            session: self.session.clone(),
            resource: draft.resource(),
            namespace: draft.namespace().into(),
            name: draft.name().into(),
            uid: draft.original["metadata"]["uid"]
                .as_str()
                .unwrap_or("")
                .into(),
            version: draft.original["metadata"]["resourceVersion"]
                .as_str()
                .unwrap_or("")
                .into(),
        };
        let audit = start_operation_audit(&target, "yaml-edit", &self.context)?;
        let result =
            match tokio::time::timeout(OPERATION_BUDGET, self.perform_yaml(&prepared, false)).await
            {
                Ok(result) => result,
                Err(_) => Err(
                    "YAML outcome UNKNOWN after timeout; inspect server state before retrying"
                        .into(),
                ),
            };
        let result = result.and_then(|response| {
            crate::manifest_edit::check_applied(&response, &prepared.expected_spec, &draft.original)
        });
        finish_audit(
            &audit,
            if result.is_ok() {
                "succeeded"
            } else {
                "failed-or-unknown"
            },
        )?;
        result?;
        Ok("YAML edit accepted by API server; inspect current state and rollout. No automatic retry.".into())
    }
    pub async fn prepare(
        &self,
        key: &str,
        ns: &str,
        name: &str,
        action: Action,
    ) -> Result<Prepared> {
        if !self.policy.writable {
            return Err("READ ONLY: mutations are disabled; opt in with --write".into());
        }
        if action == Action::Kill && !self.allow_force_kill {
            return Err("Force kill requires --write --allow-force-kill".into());
        }
        let object = self.raw_get(key, ns, name).await?;
        let kind = self.find(key)?;
        if !matches!(
            (kind.plural.as_str(), kind.group.as_str()),
            ("pods", "")
                | ("deployments" | "statefulsets" | "daemonsets", "apps")
                | ("services", "")
                | ("jobs" | "cronjobs", "batch")
        ) {
            return Err("mutations on this API group/resource are disabled".into());
        }
        let target = Target {
            session: self.session.clone(),
            resource: kind.plural.clone(),
            namespace: ns.into(),
            name: name.into(),
            uid: object["metadata"]["uid"].as_str().unwrap_or("").into(),
            version: object["metadata"]["resourceVersion"]
                .as_str()
                .unwrap_or("")
                .into(),
        };
        self.policy.check(&target, &action)?;
        for identity in [&target.uid, &target.version] {
            if identity.len() > 256 || safety::safe_text(identity, 256) != *identity {
                return Err("operation identity exceeds its bounded plain-text limit".into());
            }
        }
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "system clock error")?
            .as_secs()
            .to_string();
        let patch = match &action {
            Action::Scale(replicas) => {
                json!({"metadata":{"uid":target.uid,"resourceVersion":target.version},"spec":{"replicas":replicas}})
            }
            Action::Restart => {
                crate::edit::expected_restart(&object, &timestamp)?;
                json!({"metadata":{"uid":target.uid,"resourceVersion":target.version},"spec":{"template":{"metadata":{"annotations":{"kborg.dev/restartedAt":timestamp}}}}})
            }
            Action::SetImage { container, image } => {
                crate::edit::expected_spec(&object, container, image)?;
                json!({"metadata":{"uid":target.uid,"resourceVersion":target.version},"spec":crate::edit::container_patch_spec(&object, container, Some(image), None)?})
            }
            Action::SetResources {
                container,
                resources,
            } => {
                crate::edit::expected_resources(&object, container, resources)?;
                json!({"metadata":{"uid":target.uid,"resourceVersion":target.version},"spec":crate::edit::container_patch_spec(&object, container, None, Some(resources))?})
            }
            Action::SetContainer { container, edit } => {
                crate::edit::expected_container(&object, container, edit)?;
                json!({"metadata":{"uid":target.uid,"resourceVersion":target.version},"spec":crate::edit::container_patch_spec(&object, container, Some(&edit.image), edit.resources.as_ref())?})
            }
            Action::Delete | Action::Kill => Value::Null,
            Action::Suspend(suspend) => {
                if object["spec"]["suspend"].as_bool().unwrap_or(false) == *suspend {
                    return Err(format!(
                        "CronJob is already {}",
                        if *suspend { "suspended" } else { "active" }
                    ));
                }
                json!({"metadata":{"uid":target.uid,"resourceVersion":target.version},"spec":{"suspend":suspend}})
            }
            Action::Trigger => crate::workload_actions::job_from_cronjob(&object, &timestamp)?,
            Action::Rollback(wanted) => {
                let (current, chosen) = self
                    .rollback_revision(&kind.plural, ns, &object, *wanted)
                    .await?;
                json!({"metadata":{"uid":target.uid,"resourceVersion":target.version},
                    "spec":{"template":{"$patch":"replace"}},
                    "kborgRevision":{"from":current,"to":chosen.number,"template":chosen.template}})
            }
        };
        let effect = match &action {
            Action::Scale(n) => format!("scale replicas {} -> {n}", object["spec"]["replicas"]),
            Action::Restart => crate::edit::expected_restart(&object, &timestamp)?.1,
            Action::SetImage { container, image } => {
                crate::edit::expected_spec(&object, container, image)?.1
            }
            Action::SetResources {
                container,
                resources,
            } => crate::edit::expected_resources(&object, container, resources)?.1,
            Action::SetContainer { container, edit } => {
                crate::edit::expected_container(&object, container, edit)?.1
            }
            Action::Delete => {
                "delete object; background cascading; server default grace period; no force".into()
            }
            Action::Kill => "FORCE KILL Pod: gracePeriodSeconds=0; the API object is removed at once without waiting for container shutdown; the kubelet may still be stopping processes and a controller may create a replacement".into(),
            Action::Suspend(true) => "SUSPEND CronJob: no new Jobs are scheduled; Jobs already running continue".into(),
            Action::Suspend(false) => "RESUME CronJob: scheduling restarts; missed runs within startingDeadlineSeconds may start at once".into(),
            Action::Trigger => format!(
                "TRIGGER CronJob now: create Job {}/{} from its jobTemplate (like kubectl create job --from=cronjob/{name}); it starts immediately and is owned by the CronJob",
                ns,
                patch["metadata"]["name"].as_str().unwrap_or("?")
            ),
            Action::Rollback(_) => {
                let revision = &patch["kborgRevision"];
                format!(
                    "ROLL BACK {} to revision {} (current {}): the Pod template is replaced and a rollout starts\nimages now:  {}\nimages after: {}",
                    kind.kind,
                    revision["to"],
                    revision["from"],
                    crate::workload_actions::images(&object["spec"]["template"]).join(", "),
                    crate::workload_actions::images(&revision["template"]).join(", ")
                )
            }
        };
        let summary = format!(
            "{}\nserver: {}\ncontext: {}\ntarget: {}/{} ({})\nUID: {}\nresourceVersion: {}\n{}\nType {}/{} to execute. Approval expires after 60 seconds.",
            if self.is_demo() {
                "DEMO"
            } else {
                "PRODUCTION / WRITE ENABLED"
            },
            self.server,
            self.context,
            ns,
            name,
            key,
            target.uid,
            target.version,
            effect,
            ns,
            name
        );
        let summary = safety::review_text(&summary, 16_384)?;
        let prepared = Prepared {
            reviewed_target: target.clone(),
            reviewed_action: action.clone(),
            reviewed_summary: summary.clone(),
            target,
            action,
            summary,
            patch,
            kind: kind.clone(),
            created: std::time::Instant::now(),
        };
        if !self.is_demo() {
            let response = self.perform(&prepared, true).await?;
            match &prepared.action {
                Action::SetImage { container, image } => {
                    let (expected, _) = crate::edit::expected_spec(&object, container, image)?;
                    crate::edit::check_admission(&response, &expected, &object)?;
                }
                Action::SetResources {
                    container,
                    resources,
                } => {
                    let (expected, _) =
                        crate::edit::expected_resources(&object, container, resources)?;
                    crate::edit::check_resource_admission(
                        &response, &expected, &object, container,
                    )?;
                }
                Action::SetContainer { container, edit } => {
                    let (expected, _) = crate::edit::expected_container(&object, container, edit)?;
                    if edit.resources.is_some() {
                        crate::edit::check_resource_admission(
                            &response, &expected, &object, container,
                        )?;
                    } else {
                        crate::edit::check_admission(&response, &expected, &object)?;
                    }
                }
                Action::Scale(n) if response["spec"]["replicas"] != *n => {
                    return Err(
                        "admission changed the requested replica count; operation blocked".into(),
                    );
                }
                Action::Rollback(_) => {
                    if crate::workload_actions::images(&response["spec"]["template"])
                        != crate::workload_actions::images(
                            &prepared.patch["kborgRevision"]["template"],
                        )
                    {
                        return Err("admission changed the restored images during dry-run; operation blocked".into());
                    }
                }
                Action::Restart => {
                    crate::edit::check_metadata(&response, &object)?;
                    let (expected, _) = crate::edit::expected_restart(&object, &timestamp)?;
                    if response["spec"] != expected {
                        return Err("admission changed additional workload fields during dry-run; operation blocked for review".into());
                    }
                }
                _ => {}
            }
        }
        Ok(prepared)
    }
    /// Current revision number and the revision to restore, from owned
    /// ReplicaSets (Deployments) or ControllerRevisions (StatefulSets, DaemonSets).
    async fn rollback_revision(
        &self,
        plural: &str,
        ns: &str,
        object: &Value,
        wanted: Option<u64>,
    ) -> Result<(u64, crate::workload_actions::Revision)> {
        let client = self
            .client
            .clone()
            .ok_or("rollback needs a live cluster; demo mode keeps no revision history")?;
        let uid = object["metadata"]["uid"].as_str().unwrap_or("");
        let params = ListParams::default().limit(500);
        let history = if plural == "deployments" {
            let api: Api<k8s_openapi::api::apps::v1::ReplicaSet> = Api::namespaced(client, ns);
            let items = tokio::time::timeout(OPERATION_BUDGET, api.list(&params))
                .await
                .map_err(|_| "reading revision history timed out")?
                .map_err(|e| self.api_error(e))?
                .items
                .into_iter()
                .filter_map(|item| serde_json::to_value(item).ok())
                .collect::<Vec<_>>();
            crate::workload_actions::deployment_revisions(uid, &items)
        } else {
            let api: Api<k8s_openapi::api::apps::v1::ControllerRevision> =
                Api::namespaced(client, ns);
            let items = tokio::time::timeout(OPERATION_BUDGET, api.list(&params))
                .await
                .map_err(|_| "reading revision history timed out")?
                .map_err(|e| self.api_error(e))?
                .items
                .into_iter()
                .filter_map(|item| serde_json::to_value(item).ok())
                .collect::<Vec<_>>();
            crate::workload_actions::controller_revisions(uid, &items)
        };
        let current = history.first().map_or(0, |r| r.number);
        let chosen = crate::workload_actions::choose(&history, wanted)?.clone();
        Ok((current, chosen))
    }
    pub(super) fn delete_params(prepared: &Prepared, dry_run: bool) -> DeleteParams {
        DeleteParams {
            dry_run,
            preconditions: Some(Preconditions {
                uid: Some(prepared.target.uid.clone()),
                resource_version: Some(prepared.target.version.clone()),
            }),
            grace_period_seconds: (prepared.action == Action::Kill).then_some(0),
            ..DeleteParams::background()
        }
    }
    pub(super) async fn perform(&self, prepared: &Prepared, dry_run: bool) -> Result<Value> {
        let api = self.api(&prepared.kind, &prepared.target.namespace)?;
        match &prepared.action {
            Action::Delete | Action::Kill => {
                api.delete(
                    &prepared.target.name,
                    &Self::delete_params(prepared, dry_run),
                )
                .await
                .map_err(|e| self.api_error(e))?;
                Ok(Value::Null)
            }
            Action::Scale(_) => {
                let response = api
                    .patch_scale(
                        &prepared.target.name,
                        &PatchParams {
                            dry_run,
                            ..Default::default()
                        },
                        &Patch::Merge(&prepared.patch),
                    )
                    .await
                    .map_err(|e| self.api_error(e))?;
                serde_json::to_value(response).map_err(|_| "cannot decode scale response".into())
            }
            Action::SetImage { .. } | Action::SetResources { .. } | Action::SetContainer { .. } => {
                let response = api
                    .patch(
                        &prepared.target.name,
                        &PatchParams {
                            dry_run,
                            ..Default::default()
                        },
                        &Patch::Strategic(&prepared.patch),
                    )
                    .await
                    .map_err(|e| self.api_error(e))?;
                serde_json::to_value(response).map_err(|_| "cannot decode workload response".into())
            }
            Action::Restart | Action::Suspend(_) => {
                let response = api
                    .patch(
                        &prepared.target.name,
                        &PatchParams {
                            dry_run,
                            ..Default::default()
                        },
                        &Patch::Merge(&prepared.patch),
                    )
                    .await
                    .map_err(|e| self.api_error(e))?;
                serde_json::to_value(response).map_err(|_| "cannot decode workload response".into())
            }
            Action::Rollback(_) => {
                // Strategic merge with $patch: replace swaps the whole template,
                // as kubectl rollout undo does; uid/resourceVersion lock it.
                let mut template = prepared.patch["kborgRevision"]["template"].clone();
                template["$patch"] = json!("replace");
                let body =
                    json!({"metadata":prepared.patch["metadata"],"spec":{"template":template}});
                let response = api
                    .patch(
                        &prepared.target.name,
                        &PatchParams {
                            dry_run,
                            ..Default::default()
                        },
                        &Patch::Strategic(&body),
                    )
                    .await
                    .map_err(|e| self.api_error(e))?;
                serde_json::to_value(response).map_err(|_| "cannot decode workload response".into())
            }
            Action::Trigger => {
                let client = self.client.clone().ok_or("offline backend")?;
                let jobs: Api<k8s_openapi::api::batch::v1::Job> =
                    Api::namespaced(client, &prepared.target.namespace);
                let job: k8s_openapi::api::batch::v1::Job =
                    serde_json::from_value(prepared.patch.clone())
                        .map_err(|_| "cannot build the Job")?;
                let created = jobs
                    .create(
                        &kube::api::PostParams {
                            dry_run,
                            field_manager: Some("kborg".into()),
                        },
                        &job,
                    )
                    .await
                    .map_err(|e| self.api_error(e))?;
                serde_json::to_value(created).map_err(|_| "cannot decode Job response".into())
            }
        }
    }
    pub async fn execute(&mut self, prepared: Prepared, mut approval: Approval) -> Result<String> {
        if prepared.target != prepared.reviewed_target
            || prepared.action != prepared.reviewed_action
            || prepared.summary != prepared.reviewed_summary
        {
            return Err("prepared review changed; prepare and review again".into());
        }
        self.policy.check(&prepared.target, &prepared.action)?;
        if prepared.created.elapsed() > Duration::from_secs(60) {
            return Err("review expired; prepare and review again".into());
        }
        if prepared.target.session != self.session {
            return Err("session changed; review again".into());
        }
        approval.consume(&prepared.target, &prepared.action)?;
        if self.is_demo() {
            match prepared.action {
                Action::Delete | Action::Kill => self
                    .demo
                    .retain(|v| v["metadata"]["uid"] != prepared.target.uid),
                Action::Scale(n) => {
                    for v in &mut self.demo {
                        if v["metadata"]["uid"] == prepared.target.uid {
                            v["spec"]["replicas"] = json!(n);
                        }
                    }
                }
                Action::SetImage { container, image } => {
                    for object in &mut self.demo {
                        if object["metadata"]["uid"] == prepared.target.uid {
                            object["spec"] =
                                crate::edit::expected_spec(object, &container, &image)?.0;
                        }
                    }
                }
                Action::SetResources {
                    container,
                    resources,
                } => {
                    for v in &mut self.demo {
                        if v["metadata"]["uid"] == prepared.target.uid {
                            v["spec"] =
                                crate::edit::expected_resources(v, &container, &resources)?.0;
                            v["metadata"]["resourceVersion"] =
                                json!(format!("{}r", prepared.target.version));
                        }
                    }
                }
                Action::SetContainer { container, edit } => {
                    for object in &mut self.demo {
                        if object["metadata"]["uid"] == prepared.target.uid {
                            object["spec"] =
                                crate::edit::expected_container(object, &container, &edit)?.0;
                            object["metadata"]["resourceVersion"] =
                                json!(format!("{}e", prepared.target.version));
                        }
                    }
                }
                Action::Suspend(suspend) => {
                    for object in &mut self.demo {
                        if object["metadata"]["uid"] == prepared.target.uid {
                            object["spec"]["suspend"] = json!(suspend);
                        }
                    }
                }
                Action::Trigger => {
                    let mut job = prepared.patch.clone();
                    job["metadata"]["uid"] = json!(format!(
                        "demo-{}",
                        job["metadata"]["name"].as_str().unwrap_or("job")
                    ));
                    job["status"] = json!({"active":1});
                    self.demo.push(job);
                }
                Action::Rollback(_) => {}
                Action::Restart => {
                    for object in &mut self.demo {
                        if object["metadata"]["uid"] == prepared.target.uid {
                            let timestamp = prepared.patch["spec"]["template"]["metadata"]["annotations"]["kborg.dev/restartedAt"].as_str().ok_or("restart review timestamp missing")?;
                            object["spec"] = crate::edit::expected_restart(object, timestamp)?.0;
                            object["metadata"]["resourceVersion"] =
                                json!(format!("{}r", prepared.target.version));
                        }
                    }
                }
            }
            return Ok("Demo operation completed (no cluster contacted)".into());
        }
        let audit = start_audit(&prepared, &self.context)?;
        let outcome =
            tokio::time::timeout(Duration::from_secs(15), self.perform(&prepared, false)).await;
        let result = match outcome {
            Ok(value) => value,
            Err(_) => {
                Err("outcome UNKNOWN after timeout; reconcile server state before retrying".into())
            }
        };
        let status = if result.is_ok() {
            "succeeded"
        } else {
            "failed-or-unknown"
        };
        finish_audit(&audit, status)?;
        result?;
        Ok(
            "Operation accepted by API server; inspect rollout/current state. No automatic retry."
                .into(),
        )
    }
}

pub(super) fn start_audit(prepared: &Prepared, context: &str) -> Result<PathBuf> {
    start_operation_audit(&prepared.target, prepared.action.operation_name(), context)
}
pub(super) fn start_operation_audit(
    target: &Target,
    action: &str,
    context: &str,
) -> Result<PathBuf> {
    let dir = safety::config_dir()?.join("audit");
    safety::checked_path(&dir)?;
    if dir.exists()
        && std::fs::read_dir(&dir)
            .map_err(|_| "cannot inspect audit spool")?
            .take(1001)
            .count()
            >= 1000
    {
        return Err("audit spool full; archive it before new writes".into());
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "clock error")?
        .as_nanos();
    let path = dir.join(format!("{stamp}-{}.jsonl", std::process::id()));
    let record = json!({"schema_version":1,"stage":"intent","context":context,"namespace":target.namespace,"resource":target.resource,"name":target.name,"uid":target.uid,"resourceVersion":target.version,"action":action,"timestamp_unix_ns":stamp.to_string()});
    safety::write_new(&path, format!("{record}\n").as_bytes())?;
    Ok(path)
}
pub(super) fn finish_audit(path: &std::path::Path, status: &str) -> Result<()> {
    use std::io::Write;
    safety::checked_path(path)?;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|_| "operation may have executed; audit result storage failed")?;
    writeln!(file, "{}", json!({"stage":"outcome","status":status}))
        .and_then(|_| file.sync_all())
        .map_err(|_| "operation may have executed; audit result storage failed".into())
}
