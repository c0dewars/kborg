//! Bounded reads: watches, lists, gets, discovery and opt-in Secret inspection.
use super::*;

impl Gateway {
    pub fn watch(&self, key: &str, ns: &str) -> Result<crate::watch::WatchFeed> {
        self.watch_selector(key, ns, "")
    }
    pub fn watch_selector(
        &self,
        key: &str,
        ns: &str,
        selector: &str,
    ) -> Result<crate::watch::WatchFeed> {
        self.watch_fields(key, ns, selector, "", crate::watch::Projection::Full)
    }
    /// A metadata projection is honored only for kinds whose built-in columns
    /// need nothing else; other kinds always get full objects.
    pub fn watch_fields(
        &self,
        key: &str,
        ns: &str,
        selector: &str,
        fields: &str,
        projection: crate::watch::Projection,
    ) -> Result<crate::watch::WatchFeed> {
        crate::selectors::validate(selector)?;
        let scope = crate::selectors::field_scope(fields)?;
        let kind = self.find(key)?.clone();
        if let Some(scope) = scope
            && (kind.kind != scope.kind() || !kind.group.is_empty())
        {
            return Err(format!(
                "this field selector requires core {}s",
                scope.kind()
            ));
        }
        let projection = if crate::watch::metadata_kind(&kind) {
            projection
        } else {
            crate::watch::Projection::Full
        };
        Ok(crate::watch::WatchFeed::with_filters(
            self.api(&kind, ns)?,
            kind,
            selector.into(),
            fields.into(),
        )
        .with_projection(projection)
        .with_origin(&self.context, &self.server))
    }
    pub(crate) fn metrics_api(
        &self,
        scope: crate::metrics::MetricsScope,
        ns: &str,
    ) -> Result<Api<DynamicObject>> {
        let kind = Kind {
            plural: if matches!(scope, crate::metrics::MetricsScope::Pods) {
                "pods"
            } else {
                "nodes"
            }
            .into(),
            kind: if matches!(scope, crate::metrics::MetricsScope::Pods) {
                "PodMetrics"
            } else {
                "NodeMetrics"
            }
            .into(),
            group: "metrics.k8s.io".into(),
            version: "v1beta1".into(),
            namespaced: matches!(scope, crate::metrics::MetricsScope::Pods),
            short_names: Vec::new(),
        };
        self.api(&kind, ns)
    }
    pub(crate) fn find(&self, key: &str) -> Result<&Kind> {
        if let Some(kind) = self.kinds.iter().find(|k| {
            k.key() == key
                || (if k.group.is_empty() {
                    format!("{}/{}", k.version, k.plural)
                } else {
                    format!("{}/{}/{}", k.group, k.version, k.plural)
                }) == key
        }) {
            return Ok(kind);
        }
        let mut matches = self.kinds.iter().filter(|k| {
            k.plural == key
                || k.kind.eq_ignore_ascii_case(key)
                || k.short_names.iter().any(|name| name == key)
        });
        let first = matches.next().ok_or_else(|| {
            if crate::secrets::is_secret_alias(key) {
                crate::secrets::GENERIC_REFUSAL
            } else {
                "resource kind not available; Secrets are never exposed"
            }
        })?;
        if matches.next().is_some() {
            return Err("resource alias is ambiguous; use resource.group".into());
        }
        Ok(first)
    }
    pub(super) fn api(&self, k: &Kind, ns: &str) -> Result<Api<DynamicObject>> {
        if ns != "*" && !safety::valid_id(ns) {
            return Err("invalid namespace".into());
        }
        let client = self.client.clone().ok_or("offline backend")?;
        if k.plural == "secrets" || k.kind == "Secret" {
            return Err(crate::secrets::GENERIC_REFUSAL.into());
        }
        Ok(if k.namespaced && ns != "*" {
            Api::namespaced_with(client, ns, &k.ar())
        } else {
            Api::all_with(client, &k.ar())
        })
    }
    pub(super) fn secret_api(
        &self,
        ns: &str,
    ) -> Result<Option<Api<k8s_openapi::api::core::v1::Secret>>> {
        if !self.allow_secret_reveal {
            return Err("Secret inspection requires --allow-secret-reveal".into());
        }
        if ns == "*" || !safety::valid_id(ns) {
            return Err("Secret inspection requires one explicit namespace".into());
        }
        if self.is_demo() {
            return Ok(None);
        }
        let client = self.client.clone().ok_or("offline backend")?;
        Ok(Some(Api::namespaced(client, ns)))
    }
    /// Secret names, types and key sizes for one namespace. Values are dropped
    /// before this returns.
    pub async fn secret_index(&self, ns: &str) -> Result<crate::secrets::SecretIndex> {
        let Some(api) = self.secret_api(ns)? else {
            let data = crate::secrets::demo_data();
            return Ok(crate::secrets::SecretIndex {
                namespace: ns.into(),
                items: vec![crate::secrets::entry(
                    "api-credentials",
                    "demo-secret-uid",
                    Some("Opaque"),
                    Some(&data),
                )],
                truncated: false,
            });
        };
        let params = ListParams::default().limit(crate::secrets::MAX_SECRETS as u32);
        let list = tokio::time::timeout(OPERATION_BUDGET, api.list(&params))
            .await
            .map_err(|_| "Secret list timed out")?
            .map_err(|e| self.api_error(e))?;
        let truncated = list
            .metadata
            .continue_
            .as_deref()
            .is_some_and(|c| !c.is_empty());
        let mut list = list;
        let items = list
            .items
            .iter_mut()
            .map(|secret| {
                let entry = crate::secrets::entry_sizes(
                    secret.metadata.name.as_deref().unwrap_or(""),
                    secret.metadata.uid.as_deref().unwrap_or(""),
                    secret.type_.as_deref(),
                    secret
                        .data
                        .iter()
                        .flatten()
                        .map(|(key, value)| (key.as_str(), value.0.len())),
                );
                crate::secrets::wipe_secret(secret);
                entry
            })
            .collect();
        Ok(crate::secrets::SecretIndex {
            namespace: ns.into(),
            items,
            truncated,
        })
    }
    /// Fetches one key of one Secret, refusing a replacement with another UID.
    pub async fn secret_value(
        &self,
        ns: &str,
        name: &str,
        uid: &str,
        key: &str,
    ) -> Result<crate::secrets::Revealed> {
        crate::secrets::validate_request(ns, name, key)?;
        let Some(api) = self.secret_api(ns)? else {
            if name != "api-credentials" || uid != "demo-secret-uid" {
                return Err("Secret not found".into());
            }
            return crate::secrets::demo_data()
                .get(key)
                .map(|bytes| crate::secrets::reveal(bytes))
                .ok_or_else(|| "Secret key not found".into());
        };
        let mut secret = self.fetch_secret(&api, name, uid).await?;
        let value = secret
            .data
            .as_ref()
            .and_then(|data| data.get(key))
            .map(|bytes| crate::secrets::reveal(&bytes.0))
            .ok_or_else(|| "Secret key not found".into());
        crate::secrets::wipe_secret(&mut secret);
        value
    }
    /// Reads one Secret by name, refusing a replacement with another UID; the
    /// payload is wiped if the UID does not match.
    async fn fetch_secret(
        &self,
        api: &Api<k8s_openapi::api::core::v1::Secret>,
        name: &str,
        uid: &str,
    ) -> Result<k8s_openapi::api::core::v1::Secret> {
        let mut secret = tokio::time::timeout(OPERATION_BUDGET, api.get(name))
            .await
            .map_err(|_| "Secret read timed out")?
            .map_err(|e| self.api_error(e))?;
        if secret.metadata.uid.as_deref() != Some(uid) {
            crate::secrets::wipe_secret(&mut secret);
            return Err("Secret was replaced; reopen :secrets".into());
        }
        Ok(secret)
    }
    /// The masked YAML of one Secret: every value is replaced by its size and
    /// the payload is wiped before this returns.
    pub async fn secret_yaml(
        &self,
        ns: &str,
        name: &str,
        uid: &str,
    ) -> Result<crate::secrets::MaskedSecret> {
        crate::secrets::validate_target(ns, name)?;
        let Some(api) = self.secret_api(ns)? else {
            if name != "api-credentials" || uid != "demo-secret-uid" {
                return Err("Secret not found".into());
            }
            let mut secret = crate::secrets::demo_secret(ns);
            let masked = crate::secrets::masked(&secret);
            crate::secrets::wipe_secret(&mut secret);
            return Ok(masked);
        };
        let mut secret = self.fetch_secret(&api, name, uid).await?;
        let masked = crate::secrets::masked(&secret);
        crate::secrets::wipe_secret(&mut secret);
        Ok(masked)
    }
    pub async fn discover(&mut self) -> Result<Vec<Kind>> {
        if let Some(client) = &self.client {
            let kinds = discovery::collect(client)
                .await
                .map_err(|error| self.discovery_error(error))?;
            self.kinds = kinds;
        }
        Ok(self.kinds.clone())
    }
    pub async fn list(&self, key: &str, ns: &str) -> Result<Value> {
        self.list_selector(key, ns, "").await
    }
    pub async fn list_selector(&self, key: &str, ns: &str, selector: &str) -> Result<Value> {
        self.list_fields(key, ns, selector, "").await
    }
    pub async fn list_fields(
        &self,
        key: &str,
        ns: &str,
        selector: &str,
        fields: &str,
    ) -> Result<Value> {
        crate::selectors::validate(selector)?;
        let scope = crate::selectors::field_scope(fields)?;
        let kind = self.find(key)?;
        if let Some(scope) = scope
            && (kind.kind != scope.kind() || !kind.group.is_empty())
        {
            return Err(format!(
                "this field selector requires core {}s",
                scope.kind()
            ));
        }
        if self.is_demo() {
            let mut items: Vec<Value> = self
                .demo
                .iter()
                .filter(|v| {
                    v["kind"] == kind.kind
                        && (ns == "*" || v["metadata"]["namespace"] == ns || !kind.namespaced)
                        && crate::selectors::matches(selector, &v["metadata"]["labels"])
                        && scope.is_none_or(|scope| scope.matches(v))
                })
                .cloned()
                .collect();
            items.iter_mut().for_each(safety::redact);
            return Ok(json!({"items":items,"truncated":false,"source":"demo"}));
        }
        let api = self.api(kind, ns)?;
        let mut items = Vec::new();
        let mut continuation = None;
        let mut total_bytes = 0;
        let deadline = tokio::time::Instant::now() + OPERATION_BUDGET;
        let mut pages = 0;
        let mut seen_tokens = std::collections::HashSet::new();
        loop {
            let params = ListParams {
                limit: Some(LIST_PAGE),
                label_selector: (!selector.is_empty()).then(|| selector.to_owned()),
                field_selector: (!fields.is_empty()).then(|| fields.to_owned()),
                continue_token: continuation,
                ..Default::default()
            };
            let list = tokio::time::timeout_at(deadline, api.list(&params))
                .await
                .map_err(|_| "list budget exceeded; narrow namespace scope")?
                .map_err(|e| self.api_error(e))?;
            pages += 1;
            for item in list.items {
                let mut value = serde_json::to_value(item).map_err(|_| "cannot decode object")?;
                value["kind"] = json!(kind.kind);
                value["apiVersion"] = json!(kind.ar().api_version);
                safety::redact(&mut value);
                total_bytes += value.to_string().len();
                if items.len() >= MAX_OBJECTS || total_bytes > MAX_BODY {
                    return Ok(json!({"items":items,"truncated":true,"source":"cluster"}));
                }
                items.push(value);
            }
            continuation = list.metadata.continue_.filter(|s| !s.is_empty());
            if continuation.is_none() {
                break;
            }
            if pages >= MAX_LIST_PAGES {
                return Ok(json!({"items":items,"truncated":true,"source":"cluster"}));
            }
            if let Some(token) = &continuation
                && !seen_tokens.insert(token.clone())
            {
                return Err("server repeated a pagination token; listing stopped".into());
            }
            tokio::time::timeout_at(deadline, tokio::time::sleep(Duration::from_millis(50)))
                .await
                .map_err(|_| "list budget exceeded; narrow namespace scope")?;
        }
        Ok(json!({"items":items,"truncated":false,"source":"cluster"}))
    }
    pub(super) async fn raw_get(&self, key: &str, ns: &str, name: &str) -> Result<Value> {
        if name.is_empty()
            || name.len() > 253
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-' || b == b'.')
        {
            return Err("invalid object name".into());
        }
        let kind = self.find(key)?;
        if self.is_demo() {
            if kind.namespaced && ns == "*" {
                return Err("select a concrete namespace".into());
            }
            return self
                .demo
                .iter()
                .find(|v| {
                    v["kind"] == kind.kind
                        && v["metadata"]["name"] == name
                        && (!kind.namespaced || v["metadata"]["namespace"] == ns)
                })
                .cloned()
                .ok_or_else(|| "demo object not found".into());
        }
        if kind.namespaced && ns == "*" {
            return Err("select a concrete namespace".into());
        }
        let mut value = serde_json::to_value(
            self.api(kind, ns)?
                .get(name)
                .await
                .map_err(|e| self.api_error(e))?,
        )
        .map_err(|_| "cannot decode object")?;
        value["kind"] = json!(kind.kind);
        value["apiVersion"] = json!(kind.ar().api_version);
        Ok(value)
    }
    pub async fn get(&self, key: &str, ns: &str, name: &str) -> Result<Value> {
        let mut value = self.raw_get(key, ns, name).await?;
        safety::redact(&mut value);
        Ok(value)
    }
}
