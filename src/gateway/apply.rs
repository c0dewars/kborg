//! Reviewed server-side apply of local manifests: dry run, review, guarded write.
use super::*;
use crate::manifest_apply::{self as apply, Planned, PreparedApply};
use operations::{finish_audit, start_operation_audit};

impl Gateway {
    fn apply_access(&self) -> Result<()> {
        if !self.policy.writable || !self.allow_apply {
            return Err("Apply requires --write --allow-apply".into());
        }
        if self.is_demo() {
            return Err("Apply needs a live cluster; demo mode has no API server".into());
        }
        Ok(())
    }

    async fn live_object(&self, kind: &Kind, ns: &str, name: &str) -> Result<Option<Value>> {
        let object = tokio::time::timeout(OPERATION_BUDGET, self.api(kind, ns)?.get_opt(name))
            .await
            .map_err(|_| "reading the live object timed out")?
            .map_err(|e| self.api_error(e))?;
        object
            .map(|object| serde_json::to_value(object).map_err(|_| "cannot decode object".into()))
            .transpose()
    }

    async fn server_apply(&self, planned: &Planned, dry_run: bool) -> Result<Value> {
        let api = self.api(&planned.kind, &planned.namespace)?;
        let url = format!(
            "{}/{}?fieldManager={}&fieldValidation=Strict{}",
            api.resource_url(),
            planned.name,
            apply::FIELD_MANAGER,
            if dry_run { "&dryRun=All" } else { "" }
        );
        let request = http::Request::builder()
            .method("PATCH")
            .uri(url)
            .header("Content-Type", "application/apply-patch+yaml")
            .body(serde_json::to_vec(&planned.body).map_err(|_| "cannot encode apply body")?)
            .map_err(|_| "cannot construct apply request")?;
        let client = self.client.as_ref().ok_or("offline backend")?;
        tokio::time::timeout(OPERATION_BUDGET, client.request::<Value>(request))
            .await
            .map_err(|_| "apply request timed out".to_owned())?
            .map_err(|error| match &error {
                kube::Error::Api(status) if status.code == 409 => format!(
                    "{}: conflict with fields owned by another manager or a concurrent change; KBorg never forces ownership. Next: reopen the manifest and review the apply again, or coordinate with the other field manager",
                    planned.label()
                ),
                _ => format!("{}: {}", planned.label(), self.api_error(error)),
            })
    }

    /// Dry-run every object in a local manifest and build the review.
    pub async fn prepare_apply(
        &self,
        path: &std::path::Path,
        namespace: &str,
    ) -> Result<PreparedApply> {
        self.apply_access()?;
        let (path, documents) = crate::navigation_views::manifest_documents(path)?;
        let documents = apply::flatten(documents)?;
        let mut objects = Vec::new();
        let mut review = Vec::new();
        let mut unchanged = true;
        for document in &documents {
            let kind = apply::kind_for(&self.kinds, document)?.clone();
            let (ns, name, mut body) = apply::body(document, &kind, namespace)?;
            if objects.iter().any(|other: &Planned| {
                other.kind.key() == kind.key() && other.namespace == ns && other.name == name
            }) {
                return Err(format!("{} {ns}/{name} appears twice", kind.kind));
            }
            let live = self.live_object(&kind, &ns, &name).await?;
            if let Some(live) = &live {
                crate::edit::check_gitops(live)?;
                body["metadata"]["resourceVersion"] = live["metadata"]["resourceVersion"].clone();
            }
            let identity = live.as_ref().map(|live| {
                (
                    live["metadata"]["uid"].as_str().unwrap_or("").to_owned(),
                    live["metadata"]["resourceVersion"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                )
            });
            let planned = Planned {
                kind,
                namespace: ns,
                name,
                body,
                live: identity,
            };
            let result = self.server_apply(&planned, true).await?;
            // A create lists what the file sets; server defaults would bury it.
            let changes = match &live {
                Some(live) => apply::diff(Some(live), &result),
                None => apply::diff(None, &planned.body),
            };
            unchanged &= planned.live.is_some() && changes.is_empty();
            review.push(format!(
                "{} {}{}",
                if planned.live.is_some() {
                    "UPDATE"
                } else {
                    "CREATE"
                },
                planned.label(),
                if changes.is_empty() {
                    "  (no changes)"
                } else {
                    ""
                }
            ));
            if let Some(note) = apply::privilege_note(&planned.kind) {
                review.push(note.into());
            }
            review.extend(changes);
            objects.push(planned);
        }
        if unchanged {
            return Err(format!(
                "All {} object(s) already match the manifest; nothing to apply.",
                objects.len()
            ));
        }
        let file = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        let summary = format!(
            "REVIEW SERVER-SIDE APPLY\nContext: {}\nServer: {}\nFile: {}\nObjects: {}\nField manager: {} (conflicts are reported, never forced)\n\n{}\n\nCREATE lists the fields in the file; the server adds its defaults. Every object passed a server dry run. Updates are locked to the reviewed resourceVersion; if any object changed, appeared or disappeared, nothing is written.\nObjects are applied in file order; a later failure leaves earlier objects applied.\nType apply {} to apply. Review expires after 60 seconds.",
            self.context,
            self.server,
            path.display(),
            objects.len(),
            apply::FIELD_MANAGER,
            review.join("\n"),
            safety::safe_text(&file, 128)
        );
        PreparedApply::new(self.session.clone(), &path, objects, summary)
    }

    pub async fn execute_apply(&self, prepared: PreparedApply, typed: &str) -> Result<String> {
        self.apply_access()?;
        if prepared.session != self.session || prepared.created.elapsed() > Duration::from_secs(60)
        {
            return Err("Apply review expired or session changed; review again".into());
        }
        if typed != prepared.confirmation() {
            return Err(format!("type {} exactly to apply", prepared.confirmation()));
        }
        for planned in &prepared.objects {
            let live = self
                .live_object(&planned.kind, &planned.namespace, &planned.name)
                .await?;
            let now = live.as_ref().map(|live| {
                (
                    live["metadata"]["uid"].as_str().unwrap_or("").to_owned(),
                    live["metadata"]["resourceVersion"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                )
            });
            if now != planned.live {
                return Err(format!(
                    "{} changed since review; nothing was applied. Review again.",
                    planned.label()
                ));
            }
        }
        let mut applied = Vec::new();
        for planned in &prepared.objects {
            let (uid, version) = planned.live.clone().unwrap_or_default();
            let target = Target {
                session: self.session.clone(),
                resource: planned.kind.key(),
                namespace: planned.namespace.clone(),
                name: planned.name.clone(),
                uid,
                version,
            };
            let audit = start_operation_audit(&target, "server-side-apply", &self.context)?;
            let result = self.server_apply(planned, false).await;
            finish_audit(
                &audit,
                if result.is_ok() {
                    "succeeded"
                } else {
                    "failed-or-unknown"
                },
            )?;
            if let Err(error) = result {
                let remaining = prepared.objects.len() - applied.len() - 1;
                return Err(format!(
                    "{error}\nApplied before the failure: {}\nNot applied: {remaining} later object(s). Inspect the cluster before retrying.",
                    if applied.is_empty() {
                        "none".into()
                    } else {
                        applied.join(", ")
                    }
                ));
            }
            applied.push(planned.label());
        }
        Ok(format!(
            "Applied {} object(s): {}. Inspect rollouts and current state; no automatic retry.",
            applied.len(),
            applied.join(", ")
        ))
    }
}
