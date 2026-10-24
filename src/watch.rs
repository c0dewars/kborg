//! Bounded LIST/WATCH cache. A feed belongs to exactly one caller-owned scope epoch.
use crate::{gateway::Kind, safety};
use futures_util::{Stream, StreamExt};
use kube::{
    Api,
    api::{DynamicObject, ListParams, WatchEvent, WatchParams},
};
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet},
    pin::Pin,
    time::Duration,
};
use tokio::time::{Instant, timeout_at};

static FEED_SERIAL: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Live cache bounds. Objects are redacted (managedFields, data, annotations…)
/// before they are measured, so the cap reflects what is actually held.
const MAX_OBJECTS: usize = 5000;
const MAX_BYTES: usize = 32 * 1024 * 1024;
/// Objects per initial-list page; ten pages cover MAX_OBJECTS.
const PAGE_SIZE: u32 = 500;
const LIST_BUDGET: Duration = Duration::from_secs(15);
const WATCH_BUDGET: Duration = Duration::from_secs(12);
type Events = Pin<Box<dyn Stream<Item = Result<WatchEvent<DynamicObject>, kube::Error>> + Send>>;

#[derive(Clone, Debug)]
pub struct WatchSnapshot {
    pub objects: Vec<Value>,
    pub resource_version: String,
    pub stale: bool,
    pub message: String,
    pub terminal: bool,
    pub truncated: bool,
}

#[derive(Default)]
struct Cache {
    objects: BTreeMap<(String, String), (String, Value, usize)>,
    bytes: usize,
}
impl Cache {
    fn apply(
        &mut self,
        object: DynamicObject,
        deleted: bool,
        kind: &Kind,
    ) -> Result<String, &'static str> {
        let m = &object.metadata;
        let bounded = |v: &str| !v.is_empty() && v.len() <= 253 && safety::safe_text(v, 253) == v;
        let name = m
            .name
            .as_deref()
            .filter(|v| bounded(v))
            .ok_or("watch object has invalid name")?;
        let uid = m
            .uid
            .as_deref()
            .filter(|v| bounded(v))
            .ok_or("watch object has invalid UID")?;
        let rv = m
            .resource_version
            .as_deref()
            .filter(|v| bounded(v))
            .ok_or("watch object has invalid resourceVersion")?
            .to_owned();
        let namespace = m.namespace.clone().unwrap_or_default();
        if kind.namespaced && !safety::valid_id(&namespace) {
            return Err("watch object has invalid namespace");
        }
        let key = (namespace, name.to_owned());
        if deleted {
            // A delayed delete for a previous incarnation must not remove its replacement.
            if self.objects.get(&key).is_some_and(|old| old.0 == uid)
                && let Some(old) = self.objects.remove(&key)
            {
                self.bytes -= old.2;
            }
            return Ok(rv);
        }
        if object.types.as_ref().is_some_and(|t| t.kind == "Secret") {
            return Err("Secret watch objects are forbidden");
        }
        let uid = uid.to_owned();
        let old_bytes = self.objects.get(&key).map_or(0, |old| old.2);
        if !self.objects.contains_key(&key) && self.objects.len() >= MAX_OBJECTS {
            return Err("watch cache limit exceeded; narrow namespace scope");
        }
        let mut value = serde_json::to_value(object).map_err(|_| "invalid watch object")?;
        value["kind"] = json!(kind.kind);
        value["apiVersion"] = json!(if kind.group.is_empty() {
            kind.version.clone()
        } else {
            format!("{}/{}", kind.group, kind.version)
        });
        // Redact first: the cache bound then measures what is actually held.
        safety::redact(&mut value);
        let bytes = serde_json::to_vec(&value)
            .map_err(|_| "invalid watch object")?
            .len();
        if bytes > MAX_BYTES || self.bytes - old_bytes > MAX_BYTES - bytes {
            return Err("watch cache limit exceeded; narrow namespace scope");
        }
        self.bytes = self.bytes - old_bytes + bytes;
        self.objects.insert(key, (uid, value, bytes));
        Ok(rv)
    }
}

pub struct WatchFeed {
    api: Api<DynamicObject>,
    kind: Kind,
    selector: String,
    fields: String,
    cache: Cache,
    rv: String,
    initialized: bool,
    stream: Option<Events>,
    stream_started: Instant,
    stream_deadline: Instant,
    retry_at: Instant,
    failures: u32,
    retry_jitter: u64,
    terminal: Option<String>,
}
impl WatchFeed {
    #[cfg(test)]
    pub(crate) fn new(api: Api<DynamicObject>, kind: Kind) -> Self {
        Self::with_filters(api, kind, String::new(), String::new())
    }
    pub(crate) fn with_filters(
        api: Api<DynamicObject>,
        kind: Kind,
        selector: String,
        fields: String,
    ) -> Self {
        let now = Instant::now();
        let terminal = (kind.kind == "Secret" || kind.plural == "secrets")
            .then(|| "Secret watches are forbidden".to_owned());
        Self {
            api,
            kind,
            selector,
            fields,
            cache: Cache::default(),
            rv: String::new(),
            initialized: false,
            stream: None,
            stream_started: now,
            stream_deadline: now,
            retry_at: now,
            failures: 0,
            retry_jitter: FEED_SERIAL.fetch_add(1, std::sync::atomic::Ordering::Relaxed) % 251,
            terminal,
        }
    }
    fn snapshot(&self, stale: bool, message: &str) -> WatchSnapshot {
        WatchSnapshot {
            objects: self.cache.objects.values().map(|v| v.1.clone()).collect(),
            resource_version: self.rv.clone(),
            stale,
            message: message.into(),
            terminal: self.terminal.is_some(),
            truncated: self
                .terminal
                .as_ref()
                .is_some_and(|m| m.contains("limit exceeded")),
        }
    }
    fn fail_closed(&mut self, message: &str) -> WatchSnapshot {
        self.cache = Cache::default();
        self.rv.clear();
        self.stream = None;
        self.terminal = Some(message.into());
        self.snapshot(true, message)
    }
    fn retry(&mut self, message: &str, relist: bool) -> WatchSnapshot {
        self.stream = None;
        if relist {
            self.cache = Cache::default();
            self.rv.clear();
            self.initialized = false;
        }
        self.failures = self.failures.saturating_add(1);
        let delay = (250u64 * (1u64 << self.failures.min(5)) + self.retry_jitter).min(8000);
        self.retry_at = Instant::now() + Duration::from_millis(delay);
        self.snapshot(true, message)
    }
    fn api_failure(&mut self, error: kube::Error) -> WatchSnapshot {
        match error {
            kube::Error::Api(status) if status.code == 401 || status.code == 403 => {
                self.fail_closed("watch authorization denied; cache cleared")
            }
            kube::Error::Api(status) if status.code == 405 || status.code == 501 => {
                self.fail_closed("watch unsupported; use polling")
            }
            kube::Error::Api(status) if status.code == 404 => {
                self.fail_closed("watch resource unavailable; refresh resource catalog")
            }
            kube::Error::Api(status) if status.code == 410 => {
                self.retry("watch expired; rebuilding cache", true)
            }
            kube::Error::Api(status) if (400..500).contains(&status.code) && status.code != 429 => {
                self.fail_closed("watch request rejected; cache cleared")
            }
            kube::Error::SerdeError(_) => self.fail_closed("invalid watch response; cache cleared"),
            kube::Error::ReadEvents(_) => self.retry(
                "watch response interrupted or exceeded body limit; rebuilding cache",
                true,
            ),
            _ => self.retry("watch disconnected; reconnecting with backoff", false),
        }
    }
    fn validate_scope(&self, object: &DynamicObject) -> Result<(), &'static str> {
        if let Some(namespace) = self.api.namespace()
            && object.metadata.namespace.as_deref() != Some(namespace)
        {
            return Err("watch response escaped namespace scope");
        }
        if let Some(types) = &object.types
            && types.kind != self.kind.kind
        {
            return Err("watch response has unexpected kind");
        }
        Ok(())
    }
    async fn relist(&mut self) -> Result<(), String> {
        let deadline = Instant::now() + LIST_BUDGET;
        let mut cache = Cache::default();
        let mut rv = None;
        let mut continuation = None;
        let mut seen = HashSet::new();
        for _ in 0..10 {
            let params = ListParams {
                label_selector: (!self.selector.is_empty()).then(|| self.selector.clone()),
                field_selector: (!self.fields.is_empty()).then(|| self.fields.clone()),
                limit: Some(PAGE_SIZE),
                continue_token: continuation.take(),
                ..Default::default()
            };
            let list = timeout_at(deadline, self.api.list(&params))
                .await
                .map_err(|_| "watch initial list budget exceeded".to_string())?
                .map_err(|e| match e {
                    kube::Error::Api(s) if s.code == 401 || s.code == 403 => {
                        "watch authorization denied".to_owned()
                    }
                    kube::Error::Api(s) if s.code == 404 => {
                        "watch resource unavailable; refresh resource catalog".to_owned()
                    }
                    kube::Error::Api(s) if s.code == 405 || s.code == 501 => {
                        "watch unsupported; use polling".to_owned()
                    }
                    _ => "watch initial list failed".to_owned(),
                })?;
            let page_rv = list
                .metadata
                .resource_version
                .filter(|v| !v.is_empty() && v.len() <= 253 && safety::safe_text(v, 253) == *v)
                .ok_or("list has invalid resourceVersion")?;
            if rv.as_ref().is_some_and(|old| old != &page_rv) {
                return Err("list resourceVersion changed between pages".into());
            }
            rv = Some(page_rv);
            for object in list.items {
                self.validate_scope(&object)?;
                cache.apply(object, false, &self.kind)?;
            }
            continuation = list.metadata.continue_.filter(|v| !v.is_empty());
            if let Some(token) = &continuation {
                if token.len() > 16384 || !seen.insert(token.clone()) {
                    return Err("invalid repeated list continuation".into());
                }
            } else {
                self.cache = cache;
                self.rv = rv.unwrap_or_default();
                self.initialized = true;
                self.failures = 0;
                return Ok(());
            }
        }
        Err("watch list page limit exceeded; narrow namespace scope".into())
    }
    /// Pull one coalesced snapshot. Dropping this future cancels pending I/O; dropping
    /// the feed cancels its stream. Scope changes must create a fresh feed.
    pub async fn next(&mut self) -> WatchSnapshot {
        if let Some(message) = self.terminal.clone() {
            return self.snapshot(true, &message);
        }
        tokio::time::sleep_until(self.retry_at).await;
        if !self.initialized {
            return match self.relist().await {
                Ok(()) => self.snapshot(false, "watch cache initialized"),
                Err(message)
                    if message == "watch initial list failed"
                        || message == "watch initial list budget exceeded" =>
                {
                    self.retry(&message, true)
                }
                Err(message) => self.fail_closed(&message),
            };
        }
        let mut recovering = false;
        if self.stream.is_none() {
            let params = WatchParams {
                label_selector: (!self.selector.is_empty()).then(|| self.selector.clone()),
                field_selector: (!self.fields.is_empty()).then(|| self.fields.clone()),
                timeout: Some(8),
                bookmarks: true,
                ..Default::default()
            };
            let started = Instant::now();
            recovering = self.failures > 0;
            match timeout_at(
                Instant::now() + Duration::from_secs(10),
                self.api.watch(&params, &self.rv),
            )
            .await
            {
                Ok(Ok(stream)) => {
                    self.stream = Some(Box::pin(stream));
                    self.stream_started = started;
                    self.stream_deadline = Instant::now() + WATCH_BUDGET;
                }
                Ok(Err(error)) => return self.api_failure(error),
                Err(_) => return self.retry("watch connection timed out; reconnecting", false),
            }
        }
        // Inspect queued control/error events before declaring a resume healthy.
        let mut batch_deadline = if recovering {
            self.stream_deadline
                .min(Instant::now() + Duration::from_millis(100))
        } else {
            self.stream_deadline
        };
        for index in 0..100 {
            let Some(stream) = self.stream.as_mut() else {
                break;
            };
            let event = match timeout_at(batch_deadline, stream.next()).await {
                Ok(Some(Ok(event))) => event,
                Ok(Some(Err(error))) => return self.api_failure(error),
                Ok(None) if self.stream_started.elapsed() >= Duration::from_secs(7) => {
                    // The server normally closes the requested eight-second watch.
                    // This is renewal, not a transport failure or stale LIST.
                    self.stream = None;
                    self.failures = 0;
                    self.retry_at = Instant::now();
                    return self.snapshot(
                        false,
                        "watch interval completed; renewing at retained resourceVersion",
                    );
                }
                Ok(None) if index == 0 => {
                    // An empty successful resourceVersion watch is also a valid
                    // unchanged interval. Pace renewal to prevent empty-200 loops.
                    self.stream = None;
                    self.failures = 0;
                    self.retry_at = Instant::now() + Duration::from_millis(500 + self.retry_jitter);
                    return self.snapshot(
                        false,
                        "empty watch completed; resourceVersion cache retained",
                    );
                }
                Ok(None) => return self.retry("watch stream ended early; reconnecting", false),
                Err(_) if index == 0 && recovering => {
                    self.failures = 0;
                    return self.snapshot(false, "watch reconnected at retained resourceVersion");
                }
                Err(_) if index > 0 && batch_deadline < self.stream_deadline => break,
                Err(_) => return self.retry("watch time budget ended; reconnecting", false),
            };
            match event {
                WatchEvent::Added(object) | WatchEvent::Modified(object) => {
                    if let Err(message) = self.validate_scope(&object) {
                        return self.fail_closed(message);
                    }
                    match self.cache.apply(object, false, &self.kind) {
                        Ok(rv) => self.rv = rv,
                        Err(message) => return self.fail_closed(message),
                    }
                }
                WatchEvent::Deleted(object) => {
                    if let Err(message) = self.validate_scope(&object) {
                        return self.fail_closed(message);
                    }
                    match self.cache.apply(object, true, &self.kind) {
                        Ok(rv) => self.rv = rv,
                        Err(message) => return self.fail_closed(message),
                    }
                }
                WatchEvent::Bookmark(bookmark) => {
                    let rv = bookmark.metadata.resource_version;
                    if rv.is_empty() || rv.len() > 253 || safety::safe_text(&rv, 253) != rv {
                        return self.fail_closed("invalid watch bookmark; cache cleared");
                    }
                    self.rv = rv;
                }
                WatchEvent::Error(status) => return self.api_failure(kube::Error::Api(status)),
            }
            self.failures = 0;
            if index == 0 {
                batch_deadline = self
                    .stream_deadline
                    .min(Instant::now() + Duration::from_millis(100));
            }
        }
        self.snapshot(false, "watch connected")
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn quiet_watch_renewal_and_successful_resume_restore_live_without_event() {
        runtime().block_on(async {
            let (mut feed, requests) = feed(vec![(200, list("1")), (200, String::new())]);
            assert!(!feed.next().await.stale);
            feed.failures = 3;
            let resumed = feed.next().await;
            assert!(!resumed.stale);
            assert_eq!(resumed.resource_version, "1");
            assert_eq!(feed.failures, 0);
            assert_eq!(requests.lock().unwrap_or_else(|e| panic!("{e}")).len(), 2);
            feed.stream = Some(Box::pin(futures_util::stream::empty()));
            feed.retry_at = Instant::now();
            feed.stream_started = Instant::now() - Duration::from_secs(8);
            let renewed = feed.next().await;
            assert!(!renewed.stale);
            assert!(!renewed.terminal);
            assert_eq!(renewed.resource_version, "1");
            assert!(renewed.message.contains("renewing"));
            assert_eq!(feed.failures, 0);
            assert!(feed.stream.is_none());
            assert!(feed.retry_at <= Instant::now());
        });
    }
    use super::*;
    use kube::{Client, api::ApiResource};
    use std::sync::{Arc, Mutex};
    use tower::Layer;
    fn kind() -> Kind {
        Kind {
            plural: "pods".into(),
            kind: "Pod".into(),
            group: String::new(),
            version: "v1".into(),
            namespaced: true,
            short_names: vec!["po".into()],
        }
    }
    fn object(name: &str, uid: &str, rv: &str) -> DynamicObject {
        serde_json::from_value(json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":name,"namespace":"default","uid":uid,"resourceVersion":rv}})).unwrap_or_else(|e| panic!("{e}"))
    }
    fn list(rv: &str) -> String {
        json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":rv},"items":[]})
            .to_string()
    }
    fn event(event: &str, name: &str, uid: &str, rv: &str) -> String {
        format!("{}\n", json!({"type":event,"object":object(name, uid, rv)}))
    }
    fn feed(replies: Vec<(u16, String)>) -> (WatchFeed, Arc<Mutex<Vec<String>>>) {
        let replies = Arc::new(Mutex::new(std::collections::VecDeque::from(replies)));
        let requests = Arc::new(Mutex::new(Vec::new()));
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
                .unwrap_or_else(|| panic!("unexpected request"));
            async move {
                let body = http_body_util::Limited::new(
                    http_body_util::Full::new(bytes::Bytes::from(text)),
                    MAX_BYTES,
                );
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(status)
                        .body(body)
                        .unwrap_or_else(|e| panic!("{e}")),
                )
            }
        });
        let api = Api::namespaced_with(
            Client::new(
                crate::transport::BodyBudgetLayer::default().layer(service),
                "default",
            ),
            "default",
            &ApiResource::erase::<k8s_openapi::api::core::v1::Pod>(&()),
        );
        (WatchFeed::new(api, kind()), requests)
    }
    fn runtime() -> tokio::runtime::Runtime {
        crate::cli::runtime().unwrap_or_else(|e| panic!("{e}"))
    }
    #[test]
    fn selector_is_preserved_on_initial_list_watch_and_relist() {
        runtime().block_on(async {
            let (mut feed, requests) = feed(vec![
                (200, list("1")),
                (410, String::new()),
                (200, list("2")),
            ]);
            feed.selector = "app=web,tier!=batch".into();
            feed.fields = "spec.nodeName=worker".into();
            assert!(!feed.next().await.stale);
            assert!(feed.next().await.stale);
            assert!(!feed.next().await.stale);
            let paths = requests.lock().unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(paths.len(), 3);
            assert!(
                paths
                    .iter()
                    .all(|path| path.contains("labelSelector=app%3Dweb%2Ctier%21%3Dbatch")),
                "{paths:?}"
            );
            assert!(
                paths
                    .iter()
                    .all(|path| path.contains("fieldSelector=spec.nodeName%3Dworker")),
                "{paths:?}"
            );
        });
    }

    #[test]
    fn http_watch_denials_clear_cached_objects_even_with_empty_or_malformed_bodies() {
        runtime().block_on(async {
            for code in [401,403] {
                for body in [String::new(), "{malformed CANARY".into(), json!({"kind":"Status","apiVersion":"v1","code":200,"reason":"CANARY"}).to_string()] {
                    let initial = json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"1"},"items":[object("a","a","1")]}).to_string();
                    let (mut feed,requests) = feed(vec![(200,initial),(code,body)]);
                    assert_eq!(feed.next().await.objects.len(),1);
                    let denied = feed.next().await;
                    assert!(denied.terminal && denied.stale);
                    assert!(denied.objects.is_empty());
                    assert!(denied.resource_version.is_empty());
                    assert!(!denied.message.contains("CANARY"));
                    assert!(feed.next().await.terminal);
                    assert_eq!(requests.lock().unwrap_or_else(|error|panic!("{error}")).len(),2);
                }
            }
        });
    }

    #[test]
    fn http_watch_expiry_relists_and_server_failure_retries_with_retained_version() {
        runtime().block_on(async {
            let initial = || json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"1"},"items":[object("a","a","1")]}).to_string();
            let (mut expired,requests) = feed(vec![(200,initial()),(410,String::new()),(200,list("9"))]);
            assert_eq!(expired.next().await.objects.len(),1);
            let stale = expired.next().await;
            assert!(stale.stale && !stale.terminal);
            assert!(stale.objects.is_empty() && stale.resource_version.is_empty());
            expired.retry_at = Instant::now();
            let rebuilt = expired.next().await;
            assert!(!rebuilt.stale);
            assert_eq!(rebuilt.resource_version,"9");
            assert_eq!(requests.lock().unwrap_or_else(|error|panic!("{error}")).len(),3);

            let (mut retrying,requests) = feed(vec![(200,initial()),(503,"non-JSON CANARY".into()),(200,String::new())]);
            assert_eq!(retrying.next().await.objects.len(),1);
            let stale = retrying.next().await;
            assert!(stale.stale && !stale.terminal);
            assert_eq!(stale.objects.len(),1);
            assert_eq!(stale.resource_version,"1");
            assert!(retrying.initialized && retrying.retry_at > Instant::now());
            assert!(!stale.message.contains("CANARY"));
            retrying.retry_at = Instant::now();
            retrying.next().await;
            let paths = requests.lock().unwrap_or_else(|error|panic!("{error}"));
            assert_eq!(paths.len(),3);
            assert!(paths[2].contains("resourceVersion=1"));
        });
    }
    #[test]
    fn list_watch_delete_uid_replacement_and_bookmark_reconnect() {
        runtime().block_on(async {
            let stream = event("ADDED", "a", "old", "2") + &event("MODIFIED", "a", "new", "3")
                + &event("DELETED", "a", "old", "4") + &event("ADDED", "b", "b", "5")
                + &event("DELETED", "b", "b", "6")
                + "{\"type\":\"BOOKMARK\",\"object\":{\"apiVersion\":\"v1\",\"kind\":\"Pod\",\"metadata\":{\"resourceVersion\":\"7\"}}}\n";
            let (mut feed, requests) = feed(vec![(200,list("1")), (200,stream), (200,String::new())]);
            assert!(!feed.next().await.stale);
            let snapshot = feed.next().await;
            assert_eq!(snapshot.objects.len(), 1);
            assert_eq!(snapshot.objects[0]["metadata"]["uid"], "new");
            assert_eq!(snapshot.resource_version, "7");
            assert!(snapshot.stale);
            assert!(!snapshot.terminal);
            feed.next().await;
            let requests = requests.lock().unwrap_or_else(|e| panic!("{e}"));
            assert!(requests[1].contains("resourceVersion=1"));
            assert!(requests[1].contains("timeoutSeconds=8"));
            assert!(requests[2].contains("resourceVersion=7"));
        });
    }
    #[test]
    fn expired_watch_rebuilds_and_authorization_revocation_clears_and_stops() {
        runtime().block_on(async {
            let expired = json!({"type":"ERROR","object":{"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Expired","code":410}}).to_string()+"\n";
            let forbidden = json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Forbidden","code":403}).to_string();
            let (mut feed, requests) = feed(vec![(200,list("1")), (200,event("ADDED","a","a","2")), (200,expired), (200,list("9")), (403,forbidden)]);
            feed.next().await;
            assert_eq!(feed.next().await.objects.len(),1);
            let expired = feed.next().await;
            assert!(expired.objects.is_empty()); assert!(expired.stale); assert!(!expired.terminal);
            assert_eq!(feed.next().await.resource_version,"9");
            let denied = feed.next().await;
            assert!(denied.objects.is_empty()); assert!(denied.terminal);
            assert!(feed.next().await.terminal);
            assert_eq!(requests.lock().unwrap_or_else(|e| panic!("{e}")).len(),5);
        });
    }
    #[test]
    fn malformed_response_fails_closed_and_oversize_response_rebuilds() {
        runtime().block_on(async {
            let (mut invalid, _) = feed(vec![(200, list("1")), (200, "invalid\n".into())]);
            invalid.next().await;
            assert!(invalid.next().await.terminal);
            let (mut huge, _) = feed(vec![(200, list("1")), (200, "x".repeat(MAX_BYTES + 1))]);
            huge.next().await;
            let failure = huge.next().await;
            assert!(failure.stale);
            assert!(failure.objects.is_empty());
        });
    }
    #[test]
    fn cache_limits_fail_before_growth_and_preserve_old_incarnation() {
        let mut cache = Cache::default();
        for i in 0..MAX_OBJECTS {
            assert!(
                cache
                    .apply(
                        object(&format!("pod-{i}"), &format!("uid-{i}"), "1"),
                        false,
                        &kind()
                    )
                    .is_ok()
            );
        }
        assert!(
            cache
                .apply(object("overflow", "overflow", "2"), false, &kind())
                .is_err()
        );
        assert_eq!(cache.objects.len(), MAX_OBJECTS);
        let mut cache = Cache::default();
        let mut huge = object("huge", "huge", "2");
        // Redaction bounds each line to 4096 characters, so exceed the cache
        // with many large fields rather than one giant string.
        let fields = (0..MAX_BYTES / 4_000 + 64)
            .map(|i| (format!("k{i}"), json!("x".repeat(4_000))))
            .collect::<serde_json::Map<_, _>>();
        huge.data = json!({ "spec": fields });
        assert!(cache.apply(huge, false, &kind()).is_err());
        assert_eq!(cache.bytes, 0);
        let mut invalid = object("a", "a", "3");
        invalid.metadata.uid = None;
        assert!(cache.apply(invalid, false, &kind()).is_err());
    }
    #[test]
    fn pull_feed_coalesces_at_most_one_hundred_events_without_a_queue() {
        runtime().block_on(async {
            let events: String = (2..1002)
                .map(|rv| event("MODIFIED", "a", "a", &rv.to_string()))
                .collect();
            let (mut feed, requests) = feed(vec![(200, list("1")), (200, events)]);
            feed.next().await;
            let snapshot = feed.next().await;
            assert!(!snapshot.stale);
            assert_eq!(snapshot.resource_version, "101");
            assert_eq!(snapshot.objects.len(), 1);
            assert_eq!(requests.lock().unwrap_or_else(|e| panic!("{e}")).len(), 2);
            assert_eq!(feed.next().await.resource_version, "201");
        });
    }
    #[test]
    fn direct_secret_constructor_never_fetches_and_cancel_keeps_scope_state() {
        runtime().block_on(async {
            let (source, requests) = feed(vec![]);
            let mut secret_kind = kind();
            secret_kind.kind = "Secret".into();
            secret_kind.plural = "secrets".into();
            let mut secret = WatchFeed::new(source.api, secret_kind);
            assert!(secret.next().await.terminal);
            assert!(requests.lock().unwrap_or_else(|e| panic!("{e}")).is_empty());
            let (mut pending, _) = feed(vec![]);
            pending.initialized = true;
            pending.rv = "1".into();
            pending.stream_deadline = Instant::now() + WATCH_BUDGET;
            pending.stream = Some(Box::pin(futures_util::stream::pending()));
            assert!(
                tokio::time::timeout(Duration::from_millis(20), pending.next())
                    .await
                    .is_err()
            );
            assert_eq!(pending.rv, "1");
            assert!(pending.stream.is_some());
            assert!(pending.cache.objects.is_empty());
        });
    }
    #[test]
    fn inconsistent_paginated_list_never_publishes_partial_cache() {
        runtime().block_on(async {
            let first = json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"1","continue":"next"},"items":[object("a","a","1")]}).to_string();
            let (mut feed, _) = feed(vec![(200,first),(200,list("2"))]);
            let snapshot=feed.next().await;
            assert!(snapshot.terminal); assert!(snapshot.objects.is_empty()); assert!(snapshot.resource_version.is_empty());
        });
    }
    #[test]
    fn namespace_escape_and_incomplete_list_are_terminal() {
        runtime().block_on(async {
            let mut escaped = object("a","a","2"); escaped.metadata.namespace=Some("other".into());
            let event = json!({"type":"ADDED","object":escaped}).to_string()+"\n";
            let (mut scope, _) = feed(vec![(200,list("1")), (200,event)]);
            scope.next().await;
            assert!(scope.next().await.terminal);
            let items: Vec<_> = (0..=MAX_OBJECTS).map(|i| object(&format!("a-{i}"),&format!("a-{i}"),"1")).collect();
            let listing=json!({"apiVersion":"v1","kind":"PodList","metadata":{"resourceVersion":"1"},"items":items}).to_string();
            let (mut bounded,_) = feed(vec![(200,listing)]);
            let snapshot=bounded.next().await;
            assert!(snapshot.terminal); assert!(snapshot.truncated); assert!(snapshot.objects.is_empty());
            assert!(bounded.next().await.truncated);
        });
    }
    #[test]
    fn unsupported_or_removed_watch_stops_and_never_refetches() {
        runtime().block_on(async {
            for code in [404, 405, 501] {
                let status = json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Unsupported","code":code}).to_string();
                let (mut feed, requests) = feed(vec![(200,list("1")), (200,event("ADDED","a","a","2")), (code,status)]);
                feed.next().await;
                assert_eq!(feed.next().await.objects.len(), 1);
                let stopped = feed.next().await;
                assert!(stopped.terminal);
                assert!(stopped.stale);
                assert!(stopped.objects.is_empty());
                assert!(stopped.resource_version.is_empty());
                if code == 404 { assert!(stopped.message.contains("unavailable")); }
                else { assert_eq!(stopped.message,"watch unsupported; use polling"); }
                assert!(feed.next().await.terminal);
                assert_eq!(requests.lock().unwrap_or_else(|e| panic!("{e}")).len(), 3);
            }
        });
    }
}
