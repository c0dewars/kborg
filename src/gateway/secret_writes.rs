//! Reviewed Secret changes: set or remove one key, or delete the Secret.
//! Values never reach a review, an audit record or an error.
use super::*;
use crate::secret_edit::{Change, Prepared, Shape, Usage};
use k8s_openapi::api::core::v1::Secret;
use kube::api::PostParams;

/// The sample Secret of demo mode: listed, never written.
const DEMO_SECRET: (&str, &str) = ("api-credentials", "demo-secret-uid");

impl Gateway {
    fn secret_edit_access(&self, namespace: &str) -> Result<()> {
        if !self.policy.writable {
            return Err(
                "READ ONLY: changes are off in this session (--read-only or operator config)"
                    .into(),
            );
        }
        if !self.allow_secret_edit {
            return Err(
                "Secret changes are off in this session (operator config allow_secret_edit: false)"
                    .into(),
            );
        }
        self.policy.scope(namespace)
    }
    /// `u` on a Secret: the Pods of the namespace that use it, with their
    /// owners, and how many Pods were read. The Pods are matched unmasked
    /// here and dropped; only names and how they use it are returned.
    pub async fn secret_users(
        &self,
        namespace: &str,
        name: &str,
    ) -> Result<(Vec<crate::navigation_views::UsedRow>, usize, bool)> {
        crate::secrets::validate_target(namespace, name)?;
        let (pods, truncated) = match self.client.clone() {
            None => (
                self.demo
                    .iter()
                    .filter(|v| v["kind"] == "Pod" && v["metadata"]["namespace"] == namespace)
                    .cloned()
                    .collect::<Vec<_>>(),
                false,
            ),
            Some(client) => {
                let api: Api<Pod> = Api::namespaced(client, namespace);
                let params = ListParams::default().limit(crate::secret_edit::MAX_PODS as u32);
                let list = tokio::time::timeout(OPERATION_BUDGET, api.list(&params))
                    .await
                    .map_err(|_| "the Pod list timed out")?
                    .map_err(|e| self.api_error(e))?;
                let truncated = list
                    .metadata
                    .continue_
                    .as_deref()
                    .is_some_and(|token| !token.is_empty());
                let pods = list
                    .items
                    .into_iter()
                    .filter_map(|pod| serde_json::to_value(pod).ok())
                    .collect();
                (pods, truncated)
            }
        };
        let rows = crate::navigation_views::used_rows("Secret", name, &pods);
        Ok((rows, pods.len(), truncated))
    }
    /// The Pods of the namespace that use the Secret. A failure to list Pods
    /// does not block the change; the review says the users are unknown.
    async fn secret_usage(&self, namespace: &str, name: &str) -> Usage {
        let unknown = |reason: String| Usage {
            users: Vec::new(),
            pods: 0,
            truncated: false,
            unknown: Some(reason),
        };
        let Some(client) = self.client.clone() else {
            return unknown("demo mode has no Pods to read".into());
        };
        let api: Api<Pod> = Api::namespaced(client, namespace);
        let params = ListParams::default().limit(crate::secret_edit::MAX_PODS as u32);
        match tokio::time::timeout(OPERATION_BUDGET, api.list(&params)).await {
            Ok(Ok(list)) => Usage {
                users: crate::secret_edit::users(name, &list.items),
                pods: list.items.len(),
                truncated: list
                    .metadata
                    .continue_
                    .as_deref()
                    .is_some_and(|token| !token.is_empty()),
                unknown: None,
            },
            Ok(Err(error)) => unknown(self.api_error(error)),
            Err(_) => unknown("the Pod list timed out".into()),
        }
    }
    /// The shape of a Secret that is about to be created; an error if the
    /// name is taken. The existing Secret, if any, is wiped unread.
    async fn absent_secret(&self, api: &Api<Secret>, namespace: &str, name: &str) -> Result<Shape> {
        let found = tokio::time::timeout(OPERATION_BUDGET, api.get_opt(name))
            .await
            .map_err(|_| "reading the Secret name timed out; nothing was written")?
            .map_err(|e| self.api_error(e))?;
        match found {
            Some(mut secret) => {
                crate::secrets::wipe_secret(&mut secret);
                Err(exists(namespace, name))
            }
            None => Ok(Shape::absent()),
        }
    }
    /// Reads the Secret, keeps only its shape and wipes the payload.
    async fn secret_shape(&self, api: &Api<Secret>, name: &str, uid: &str) -> Result<Shape> {
        let mut secret = self.fetch_secret(api, name, uid).await?;
        let shape = Shape::of(&secret);
        crate::secrets::wipe_secret(&mut secret);
        Ok(shape)
    }
    fn demo_shape(namespace: &str, name: &str, uid: &str) -> Result<Shape> {
        if (name, uid) != DEMO_SECRET {
            return Err("Secret not found".into());
        }
        let mut secret = crate::secrets::demo_secret(namespace);
        let mut shape = Shape::of(&secret);
        crate::secrets::wipe_secret(&mut secret);
        shape.version = "1".into();
        Ok(shape)
    }
    /// Build the review of one Secret change and check it with a server dry run.
    pub async fn prepare_secret(
        &self,
        namespace: &str,
        name: &str,
        uid: &str,
        change: Change,
    ) -> Result<Prepared> {
        self.secret_edit_access(namespace)?;
        crate::secrets::validate_target(namespace, name)?;
        let api = self.secret_api(namespace)?;
        let creating = matches!(change, Change::Create { .. });
        if creating && !uid.is_empty() {
            return Err("a new Secret has no UID yet".into());
        }
        let shape = match &api {
            Some(api) if creating => self.absent_secret(api, namespace, name).await?,
            Some(api) => self.secret_shape(api, name, uid).await?,
            None if creating && name == DEMO_SECRET.0 => {
                return Err(exists(namespace, name));
            }
            None if creating => Shape::absent(),
            None => Self::demo_shape(namespace, name, uid)?,
        };
        crate::secret_edit::check(&shape, &change)?;
        let usage = self.secret_usage(namespace, name).await;
        let summary = crate::secret_edit::review(
            &self.context,
            &self.server,
            namespace,
            name,
            uid,
            &shape,
            &change,
            &usage,
            api.is_none(),
        );
        let prepared = Prepared::new(namespace, name, uid, &shape, change, &self.session, summary)?;
        if let Some(api) = &api {
            tokio::time::timeout(OPERATION_BUDGET, self.perform_secret(api, &prepared, true))
                .await
                .map_err(|_| "Secret dry-run timed out; nothing was written")??;
        }
        Ok(prepared)
    }
    /// Send the reviewed change, as a dry run or for real. A response that
    /// carries the Secret is wiped before this returns.
    async fn perform_secret(
        &self,
        api: &Api<Secret>,
        prepared: &Prepared,
        dry_run: bool,
    ) -> Result<()> {
        if let Some(mut secret) = crate::secret_edit::new_secret(prepared) {
            let params = PostParams {
                dry_run,
                field_manager: Some("kborg".into()),
            };
            let created = api.create(&params, &secret).await;
            crate::secrets::wipe_secret(&mut secret);
            let mut created = created.map_err(|e| self.api_error(e))?;
            let stored = match &prepared.change {
                Change::Create { key, value, .. } => created.data.as_ref().is_some_and(|data| {
                    data.len() == 1 && data.get(key).is_some_and(|b| b.0 == value.bytes())
                }),
                _ => false,
            };
            crate::secrets::wipe_secret(&mut created);
            if !stored {
                return Err(if dry_run {
                    "admission would change the new Secret; nothing was written".into()
                } else {
                    "the server stored something other than the reviewed Secret; inspect it in :secrets".into()
                });
            }
            return Ok(());
        }
        let Some(patch) = crate::secret_edit::patch(prepared) else {
            let params = DeleteParams {
                dry_run,
                preconditions: Some(Preconditions {
                    uid: Some(prepared.uid.clone()),
                    resource_version: Some(prepared.version.clone()),
                }),
                ..DeleteParams::background()
            };
            let deleted = api
                .delete(&prepared.name, &params)
                .await
                .map_err(|e| self.api_error(e))?;
            // A delete answers with the Secret itself while it is finalizing.
            if let Some(mut secret) = deleted.left() {
                crate::secrets::wipe_secret(&mut secret);
            }
            return Ok(());
        };
        let url = format!(
            "{}/{}?fieldValidation=Strict{}",
            api.resource_url(),
            prepared.name,
            if dry_run { "&dryRun=All" } else { "" }
        );
        let request = http::Request::builder()
            .method("PATCH")
            .uri(url)
            .header("Content-Type", "application/json-patch+json")
            .body(serde_json::to_vec(&patch).map_err(|_| "cannot encode the Secret patch")?)
            .map_err(|_| "cannot construct the Secret patch request")?;
        let mut response = self
            .client
            .as_ref()
            .ok_or("offline backend")?
            .request::<Secret>(request)
            .await
            .map_err(|e| self.api_error(e))?;
        // The server must hold exactly what was reviewed for this key.
        let stored = match &prepared.change {
            Change::Set { key, value, .. } => response
                .data
                .as_ref()
                .and_then(|data| data.get(key))
                .is_some_and(|bytes| bytes.0 == value.bytes()),
            Change::Remove { key } => !response
                .data
                .as_ref()
                .is_some_and(|data| data.contains_key(key)),
            Change::Delete => true,
            // Sent by the create branch above, never as a patch.
            Change::Create { .. } => false,
        };
        crate::secrets::wipe_secret(&mut response);
        if !stored {
            return Err(if dry_run {
                "admission would change the reviewed Secret change; nothing was written".into()
            } else {
                "the server stored something other than the reviewed change; inspect the Secret"
                    .into()
            });
        }
        Ok(())
    }
    /// Apply a reviewed Secret change after the typed or one-key confirmation.
    pub async fn execute_secret(&self, prepared: Prepared, typed: &str) -> Result<String> {
        self.secret_edit_access(&prepared.namespace)?;
        if typed != prepared.confirmation() {
            return Err("confirmation must exactly match namespace/name".into());
        }
        let event =
            crate::audit_sink::Event::new(prepared.change.operation(), &self.context, &self.server)
                .target(
                    &prepared.namespace,
                    "secrets",
                    &prepared.name,
                    &prepared.uid,
                )
                .review(prepared.summary());
        let audit = crate::audit_sink::intent(event)?;
        let result = self.execute_secret_approved(prepared).await;
        audit.finish(&result)?;
        result
    }
    async fn execute_secret_approved(&self, prepared: Prepared) -> Result<String> {
        if prepared.session != self.session || prepared.created.elapsed() > Duration::from_secs(60)
        {
            return Err("Secret review expired or session changed; review again".into());
        }
        let creating = matches!(prepared.change, Change::Create { .. });
        let Some(api) = self.secret_api(&prepared.namespace)? else {
            if !creating {
                Self::demo_shape(&prepared.namespace, &prepared.name, &prepared.uid)?;
            }
            return Ok("Demo: the change was validated; no cluster contacted and the sample Secret stays as it is.".into());
        };
        // Anything that changed the Secret since the review cancels the write.
        // A create needs no check: it fails if the name was taken meanwhile.
        if !creating {
            let current = self
                .secret_shape(&api, &prepared.name, &prepared.uid)
                .await?;
            if current.version != prepared.version {
                return Err("the Secret changed since the review; review again".into());
            }
        }
        let target = Target {
            session: self.session.clone(),
            resource: "secrets".into(),
            namespace: prepared.namespace.clone(),
            name: prepared.name.clone(),
            uid: prepared.uid.clone(),
            version: prepared.version.clone(),
        };
        let audit =
            operations::start_operation_audit(&target, prepared.change.operation(), &self.context)?;
        let result = match tokio::time::timeout(
            OPERATION_BUDGET,
            self.perform_secret(&api, &prepared, false),
        )
        .await
        {
            Ok(result) => result,
            Err(_) => Err(
                "Secret outcome UNKNOWN after timeout; inspect it in :secrets before retrying"
                    .into(),
            ),
        };
        operations::finish_audit(
            &audit,
            if result.is_ok() {
                "succeeded"
            } else {
                "failed-or-unknown"
            },
        )?;
        result?;
        Ok(prepared.outcome())
    }
}

fn exists(namespace: &str, name: &str) -> String {
    format!(
        "a Secret {namespace}/{name} already exists. Next: select it in :secrets and press a to add a key"
    )
}
