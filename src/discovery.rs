//! Bounded preferred-version discovery; commit a catalog only after complete success.
use super::Kind;
use crate::Result;
use http_body_util::BodyExt;
use kube::Client;
use serde_json::Value;
use std::{collections::HashSet, time::Duration};

const MAX_DOCUMENT: usize = 1024 * 1024;
const MAX_TOTAL: usize = 16 * 1024 * 1024;
const MAX_GROUPS: usize = 100;
const MAX_KINDS: usize = 200;
const MAX_REQUESTS: usize = MAX_GROUPS + 2;
const BUDGET: Duration = Duration::from_secs(15);

struct Budget {
    bytes: usize,
    requests: usize,
    deadline: tokio::time::Instant,
}
impl Budget {
    fn new() -> Self {
        Self {
            bytes: 0,
            requests: 0,
            deadline: tokio::time::Instant::now() + BUDGET,
        }
    }
    async fn document(&mut self, client: &Client, path: &str) -> Result<Value> {
        if tokio::time::Instant::now() >= self.deadline {
            return Err("discovery deadline exceeded; previous catalog retained".into());
        }
        if self.requests >= MAX_REQUESTS {
            return Err("discovery request limit exceeded; previous catalog retained".into());
        }
        self.requests += 1;
        let request = http::Request::get(path)
            .header(http::header::ACCEPT, "application/json")
            .body(kube::client::Body::empty())
            .map_err(|_| "invalid discovery request")?;
        let response = tokio::time::timeout_at(self.deadline, client.send(request))
            .await
            .map_err(|_| "discovery deadline exceeded; previous catalog retained")?
            .map_err(|_| "discovery transport failed; previous catalog retained")?;
        if !response.status().is_success() {
            return Err(format!(
                "discovery HTTP {}; previous catalog retained (refresh incomplete)",
                response.status().as_u16()
            ));
        }
        let remaining = MAX_TOTAL.saturating_sub(self.bytes);
        let body = http_body_util::Limited::new(response.into_body(), MAX_DOCUMENT.min(remaining));
        let bytes = tokio::time::timeout_at(self.deadline, body.collect()).await
            .map_err(|_| "discovery deadline exceeded; previous catalog retained")?
            .map_err(|_| "discovery document/aggregate byte limit or body failure; previous catalog retained")?.to_bytes();
        self.bytes += bytes.len();
        serde_json::from_slice(&bytes)
            .map_err(|_| "malformed discovery document; previous catalog retained".into())
    }
}

fn segment(value: &str, dotted: bool) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 63
                && part
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && part
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && part
                    .bytes()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-')
        })
        && (dotted || !value.contains('.'))
}
fn text<'a>(object: &'a Value, field: &str) -> Result<&'a str> {
    object[field]
        .as_str()
        .ok_or_else(|| "malformed discovery metadata; previous catalog retained".into())
}
fn preferred_paths(document: &Value) -> Result<Vec<(String, String)>> {
    let groups = document["groups"]
        .as_array()
        .ok_or("malformed discovery group list")?;
    if groups.len() > MAX_GROUPS {
        return Err("discovery group limit exceeded; previous catalog retained".into());
    }
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    for group in groups {
        let name = text(group, "name")?;
        let version = text(&group["preferredVersion"], "version")?;
        if !segment(name, true)
            || !segment(version, false)
            || text(&group["preferredVersion"], "groupVersion")? != format!("{name}/{version}")
            || !seen.insert(name.to_owned())
        {
            return Err(
                "invalid or duplicate discovery group/version; previous catalog retained".into(),
            );
        }
        paths.push((name.to_owned(), version.to_owned()));
    }
    Ok(paths)
}
fn append_resources(
    document: &Value,
    group: &str,
    version: &str,
    found: &mut Vec<Kind>,
    seen: &mut HashSet<String>,
) -> Result<()> {
    let expected = if group.is_empty() {
        version.to_owned()
    } else {
        format!("{group}/{version}")
    };
    if text(document, "groupVersion")? != expected {
        return Err("discovery group/version mismatch; previous catalog retained".into());
    }
    let resources = document["resources"]
        .as_array()
        .ok_or("malformed discovery resource list")?;
    if resources.len() > 1000 {
        return Err("discovery resource document limit exceeded; previous catalog retained".into());
    }
    for resource in resources {
        let name = text(resource, "name")?;
        let kind = text(resource, "kind")?;
        // Connect subresources and Secrets never enter the browsable catalog.
        if name.contains('/') || name == "secrets" || kind == "Secret" {
            continue;
        }
        if !segment(name, false)
            || kind.is_empty()
            || kind.len() > 253
            || !kind.bytes().all(|c| c.is_ascii_alphanumeric())
        {
            return Err("invalid discovery resource identity; previous catalog retained".into());
        }
        let namespaced = resource["namespaced"]
            .as_bool()
            .ok_or("malformed discovery resource scope")?;
        let verbs = resource["verbs"]
            .as_array()
            .ok_or("malformed discovery resource verbs")?;
        if !verbs.iter().any(|v| v.as_str() == Some("list")) {
            continue;
        }
        let item = Kind {
            plural: name.into(),
            kind: kind.into(),
            group: group.into(),
            version: version.into(),
            namespaced,
        };
        if !seen.insert(item.key()) {
            return Err("duplicate discovery resource; previous catalog retained".into());
        }
        if found.len() >= MAX_KINDS {
            return Err("discovery kind limit exceeded; previous catalog retained".into());
        }
        found.push(item);
    }
    Ok(())
}
pub(super) async fn collect(client: &Client) -> Result<Vec<Kind>> {
    let mut budget = Budget::new();
    let mut found = Vec::new();
    let mut seen = HashSet::new();
    let core = budget.document(client, "/api/v1").await?;
    append_resources(&core, "", "v1", &mut found, &mut seen)?;
    drop(core);
    let groups = budget.document(client, "/apis").await?;
    let paths = preferred_paths(&groups)?;
    drop(groups);
    for (group, version) in paths {
        let document = budget
            .document(client, &format!("/apis/{group}/{version}"))
            .await?;
        append_resources(&document, &group, &version, &mut found, &mut seen)?;
    }
    found.sort_by_key(Kind::key);
    Ok(found)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    fn resource(name: &str) -> Value {
        json!({"name":name,"kind":"Thing","namespaced":true,"verbs":["get","list"]})
    }
    fn group(name: &str) -> Value {
        json!({"name":name,"preferredVersion":{"version":"v1","groupVersion":format!("{name}/v1")}})
    }
    fn client(
        pages: Vec<(u16, Vec<u8>)>,
    ) -> (Client, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let queue = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
            pages,
        )));
        let paths = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = paths.clone();
        let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
            recorded
                .lock()
                .unwrap_or_else(|e| panic!("{e}"))
                .push(request.uri().to_string());
            let (status, data) = queue
                .lock()
                .unwrap_or_else(|e| panic!("{e}"))
                .pop_front()
                .unwrap_or_else(|| panic!("extra discovery request"));
            async move {
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(status)
                        .body(http_body_util::Full::new(bytes::Bytes::from(data)))
                        .unwrap_or_else(|e| panic!("{e}")),
                )
            }
        });
        (Client::new(service, "default"), paths)
    }
    fn page(value: Value) -> (u16, Vec<u8>) {
        (200, value.to_string().into_bytes())
    }
    #[test]
    fn preferred_path_validation_blocks_injection_duplicates_and_group_overflow() {
        for invalid in [
            "../evil", "evil/x", "x?y", "x%2fy", "x#y", "x..y", "UPPER", "-x", "x-",
        ] {
            assert!(preferred_paths(&json!({"groups":[group(invalid)]})).is_err());
        }
        assert!(preferred_paths(&json!({"groups":[group("apps"),group("apps")]})).is_err());
        assert!(preferred_paths(&json!({"groups":(0..101).map(|n| group(&format!("g{n}.example"))).collect::<Vec<_>>()})).is_err());
    }
    #[test]
    fn mock_discovery_success_has_fixed_paths_and_excludes_secrets() {
        crate::cli::runtime().unwrap_or_else(|e| panic!("{e}")).block_on(async {
            let (client, paths) = client(vec![page(json!({"groupVersion":"v1","resources":[resource("pods"),resource("secrets"),resource("pods/log")]})),
                page(json!({"groups":[group("example.com")]})),page(json!({"groupVersion":"example.com/v1","resources":[resource("things")]}))]);
            let catalog = collect(&client).await.unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(catalog.len(),2);
            assert_eq!(*paths.lock().unwrap_or_else(|e| panic!("{e}")), vec!["/api/v1","/apis","/apis/example.com/v1"]);
        });
    }
    #[test]
    fn mock_forbidden_malformed_oversize_and_deadline_stop() {
        crate::cli::runtime()
            .unwrap_or_else(|e| panic!("{e}"))
            .block_on(async {
                for response in [
                    (403, b"SENSITIVE".to_vec()),
                    (200, b"malformed SENSITIVE".to_vec()),
                    (200, vec![b' '; MAX_DOCUMENT + 1]),
                ] {
                    let (client, paths) = client(vec![response]);
                    let error = collect(&client)
                        .await
                        .err()
                        .unwrap_or_else(|| panic!("accepted bad response"));
                    assert!(!error.contains("SENSITIVE"));
                    assert_eq!(paths.lock().unwrap_or_else(|e| panic!("{e}")).len(), 1);
                }
                let (client, _) = client(vec![page(json!({"x":1}))]);
                let mut budget = Budget::new();
                budget.deadline = tokio::time::Instant::now() - Duration::from_secs(1);
                assert!(budget.document(&client, "/api/v1").await.is_err());
            });
    }
    #[test]
    fn mock_aggregate_and_request_budgets_stop_before_extra_work() {
        crate::cli::runtime()
            .unwrap_or_else(|e| panic!("{e}"))
            .block_on(async {
                let (client, paths) = client(vec![page(json!({"x":"abc"}))]);
                let mut budget = Budget::new();
                budget.bytes = MAX_TOTAL - 1;
                assert!(budget.document(&client, "/api/v1").await.is_err());
                budget.requests = MAX_REQUESTS;
                assert!(budget.document(&client, "/api/v1").await.is_err());
                assert_eq!(paths.lock().unwrap_or_else(|e| panic!("{e}")).len(), 1);
            });
    }
    #[test]
    fn resource_limits_and_mismatch_never_return_truncated_success() {
        let mut found = Vec::new();
        let mut seen = HashSet::new();
        assert!(
            append_resources(
                &json!({"groupVersion":"evil/v1","resources":[]}),
                "",
                "v1",
                &mut found,
                &mut seen
            )
            .is_err()
        );
        assert!(append_resources(&json!({"groupVersion":"v1","resources":(0..201).map(|n| resource(&format!("r{n}"))).collect::<Vec<_>>()}),"","v1",&mut found,&mut seen).is_err());
        assert_eq!(found.len(), 200);
    }
}
