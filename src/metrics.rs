//! Read-only, bounded access to the standard Kubernetes Metrics API.
use crate::{Result, gateway::Gateway, safety};
use kube::api::ListParams;
use serde::Serialize;
use serde_json::{Value, json};
use std::{collections::HashSet, time::Duration};

const MAX_OBJECTS: usize = 1000;
const MAX_ROWS: usize = 1000;
const MAX_BYTES: usize = 8 * 1024 * 1024;
const MAX_PAGES: usize = 10;
const BUDGET: Duration = Duration::from_secs(15);

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum MetricsScope {
    Pods,
    Nodes,
}
impl MetricsScope {
    pub fn parse(value: &str) -> Result<Self> {
        match value {
            "pods" => Ok(Self::Pods),
            "nodes" => Ok(Self::Nodes),
            _ => Err("metrics scope must be pods or nodes".into()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct MetricsSnapshot {
    pub api_version: &'static str,
    pub scope: MetricsScope,
    pub namespace: Option<String>,
    pub source: &'static str,
    pub items: Vec<MetricsRow>,
    pub truncated: bool,
}
#[derive(Debug, Serialize)]
pub struct MetricsRow {
    pub name: String,
    pub namespace: Option<String>,
    pub container: Option<String>,
    pub timestamp: String,
    pub window: String,
    pub cpu_raw: String,
    pub memory_raw: String,
    pub cpu_nanocores: u64,
    /// Exact decimal representation, never a floating point approximation.
    pub cpu_millicores: String,
    pub memory_bytes: u64,
    /// Sub-base-unit quantities are rounded upward, as Kubernetes quantities are.
    pub rounded_up: bool,
}
impl MetricsSnapshot {
    pub fn text(&self) -> String {
        let mut output = format!(
            "Metrics API {} | {} | measurements include timestamp and window\nNAMESPACE/NAME  CONTAINER  CPU(m)  MEMORY(bytes)  TIMESTAMP  WINDOW\n",
            self.api_version, self.source
        );
        for row in &self.items {
            let target = row
                .namespace
                .as_ref()
                .map(|ns| format!("{ns}/{}", row.name))
                .unwrap_or_else(|| row.name.clone());
            output.push_str(&format!(
                "{}  {}  {}  {}  {}  {}{}\n",
                target,
                row.container.as_deref().unwrap_or("-"),
                row.cpu_millicores,
                row.memory_bytes,
                row.timestamp,
                row.window,
                if row.rounded_up {
                    " (rounded up to base unit)"
                } else {
                    ""
                }
            ));
        }
        if self.items.is_empty() {
            output.push_str("No metrics samples returned.\n");
        }
        if self.truncated {
            output.push_str(
                "Metrics truncated at the bounded result limit; narrow namespace scope.\n",
            );
        }
        output
    }
}

pub async fn collect(gateway: &Gateway, scope: MetricsScope, ns: &str) -> Result<MetricsSnapshot> {
    validate_namespace(ns)?;
    if gateway.is_demo() {
        return demo(scope, ns);
    }
    let api = gateway.metrics_api(scope, ns)?;
    collect_api(api, scope, ns).await
}

async fn collect_api(
    api: kube::Api<kube::api::DynamicObject>,
    scope: MetricsScope,
    ns: &str,
) -> Result<MetricsSnapshot> {
    validate_namespace(ns)?;
    let deadline = tokio::time::Instant::now() + BUDGET;
    let mut snapshot = empty(scope, ns, "cluster");
    let mut continuation = None;
    let mut seen = HashSet::new();
    let mut object_keys = HashSet::new();
    let mut resource_version = None;
    let mut objects = 0usize;
    let mut total_bytes = 0usize;
    for page in 0..MAX_PAGES {
        let params = ListParams {
            limit: Some(200),
            continue_token: continuation,
            ..Default::default()
        };
        let list = tokio::time::timeout_at(deadline, api.list(&params))
            .await
            .map_err(|_| "metrics read exceeded 15-second budget")?
            .map_err(metrics_error)?;
        if page == 0 {
            resource_version = Some(list.metadata.resource_version.clone());
        } else if resource_version.as_ref() != Some(&list.metadata.resource_version) {
            return Err("metrics pagination resource version changed".into());
        }
        // Count the entire decoded page, including metadata and continuation tokens.
        total_bytes = total_bytes
            .checked_add(
                serde_json::to_vec(&list)
                    .map_err(|_| "cannot decode metrics page")?
                    .len(),
            )
            .ok_or("metrics byte limit exceeded")?;
        if total_bytes > MAX_BYTES {
            return Err("metrics response exceeds 8 MiB aggregate limit".into());
        }
        for object in list.items {
            if objects >= MAX_OBJECTS {
                snapshot.truncated = true;
                return Ok(snapshot);
            }
            objects += 1;
            let value = serde_json::to_value(object).map_err(|_| "cannot decode metrics sample")?;
            let rows = decode_rows(&value, scope, ns)?;
            if let Some(row) = rows.first()
                && !object_keys.insert((row.namespace.clone(), row.name.clone()))
            {
                return Err("duplicate object in metrics pagination".into());
            }
            if rows.len() > MAX_ROWS - snapshot.items.len() {
                snapshot.truncated = true;
                return Ok(snapshot);
            }
            snapshot.items.extend(rows);
        }
        continuation = list.metadata.continue_.filter(|token| !token.is_empty());
        if continuation.is_none() {
            return Ok(snapshot);
        }
        if let Some(token) = &continuation
            && (token.len() > 8192 || !seen.insert(token.clone()))
        {
            return Err("invalid or repeated metrics pagination token".into());
        }
        if page + 1 == MAX_PAGES {
            snapshot.truncated = true;
        }
    }
    Ok(snapshot)
}
fn validate_namespace(ns: &str) -> Result<()> {
    if ns == "*" || safety::valid_id(ns) {
        Ok(())
    } else {
        Err("invalid metrics namespace".into())
    }
}
fn empty(scope: MetricsScope, ns: &str, source: &'static str) -> MetricsSnapshot {
    MetricsSnapshot {
        api_version: "metrics.k8s.io/v1beta1",
        scope,
        namespace: if scope == MetricsScope::Pods {
            Some(ns.into())
        } else {
            None
        },
        source,
        items: Vec::new(),
        truncated: false,
    }
}
fn metrics_error(error: kube::Error) -> String {
    match error {
        kube::Error::Api(response) => status_error(response.code),
        _ => "Metrics API read failed (transport or response error; details suppressed)".into(),
    }
}
fn status_error(code: u16) -> String {
    match code {
        404 => "Metrics API unavailable: metrics.k8s.io/v1beta1 is not served; no metrics server is installed automatically".into(),
        403 => "Metrics API read forbidden: RBAC does not permit listing the selected pod or node metrics".into(),
        401 => "Metrics API authentication failed".into(),
        _ => format!("Metrics API read failed (HTTP {code}; details suppressed)"),
    }
}
fn bounded_text(value: &Value, key: &str, max: usize) -> Result<String> {
    let text = value
        .get(key)
        .and_then(Value::as_str)
        .ok_or("malformed metrics sample: required text missing")?;
    if text.is_empty() || text.len() > max || safety::safe_text(text, max) != text {
        return Err("malformed metrics sample: invalid text".into());
    }
    Ok(text.into())
}
fn decode_rows(value: &Value, scope: MetricsScope, ns: &str) -> Result<Vec<MetricsRow>> {
    let meta = value.get("metadata").ok_or("malformed metrics metadata")?;
    let name = bounded_text(meta, "name", 253)?;
    if !name
        .bytes()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'.')
    {
        return Err("invalid metrics object name".into());
    }
    let namespace = if scope == MetricsScope::Pods {
        let actual = bounded_text(meta, "namespace", 63)?;
        if !safety::valid_id(&actual) || (ns != "*" && ns != actual) {
            return Err("metrics sample is outside selected namespace".into());
        }
        Some(actual)
    } else {
        None
    };
    let timestamp = bounded_text(value, "timestamp", 64)?;
    let window = bounded_text(value, "window", 64)?;
    if !valid_timestamp(&timestamp) || !valid_window(&window) {
        return Err("malformed metrics timestamp or window".into());
    }
    let make_row = |usage: &Value, container: Option<String>| -> Result<MetricsRow> {
        let cpu_raw = bounded_text(usage, "cpu", 128)?;
        let memory_raw = bounded_text(usage, "memory", 128)?;
        let (cpu_nanocores, cpu_rounded) = normalize_cpu_quantity(&cpu_raw)?;
        let (memory_bytes, memory_rounded) = normalize_memory_quantity(&memory_raw)?;
        let fractional = cpu_nanocores % 1_000_000;
        let cpu_millicores = if fractional == 0 {
            (cpu_nanocores / 1_000_000).to_string()
        } else {
            format!("{}.{:06}", cpu_nanocores / 1_000_000, fractional)
                .trim_end_matches('0')
                .to_string()
        };
        Ok(MetricsRow {
            name: name.clone(),
            namespace: namespace.clone(),
            container,
            timestamp: timestamp.clone(),
            window: window.clone(),
            cpu_raw,
            memory_raw,
            cpu_nanocores,
            cpu_millicores,
            memory_bytes,
            rounded_up: cpu_rounded || memory_rounded,
        })
    };
    match scope {
        MetricsScope::Nodes => Ok(vec![make_row(
            value.get("usage").ok_or("malformed node metrics usage")?,
            None,
        )?]),
        MetricsScope::Pods => {
            let containers = value
                .get("containers")
                .and_then(Value::as_array)
                .ok_or("malformed pod metrics containers")?;
            if containers.is_empty() || containers.len() > MAX_ROWS {
                return Err("malformed or oversized pod metrics containers".into());
            }
            let mut names = HashSet::new();
            containers
                .iter()
                .map(|container| {
                    let name = bounded_text(container, "name", 63)?;
                    if !safety::valid_id(&name) || !names.insert(name.clone()) {
                        return Err("invalid or duplicate metrics container name".into());
                    }
                    make_row(
                        container
                            .get("usage")
                            .ok_or("malformed container metrics usage")?,
                        Some(name),
                    )
                })
                .collect()
        }
    }
}

/// CPU nanocores and whether sub-nanocore precision required upward rounding.
pub fn normalize_cpu_quantity(raw: &str) -> Result<(u64, bool)> {
    if ["Ki", "Mi", "Gi", "Ti", "Pi", "Ei"]
        .iter()
        .any(|suffix| raw.ends_with(suffix))
    {
        return Err("invalid CPU metrics unit".into());
    }
    quantity(raw, 1_000_000_000)
}
/// Memory bytes and whether fractional-byte precision required upward rounding.
pub fn normalize_memory_quantity(raw: &str) -> Result<(u64, bool)> {
    quantity(raw, 1)
}

fn valid_timestamp(text: &str) -> bool {
    // RFC3339 date/time with optional fractional seconds and explicit zone.
    let bytes = text.as_bytes();
    if bytes.len() < 20 || !text.is_ascii() {
        return false;
    }
    for (index, expected) in [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':')] {
        if bytes[index] != expected {
            return false;
        }
    }
    let number = |start: usize, end: usize| -> Option<u32> {
        let part = text.get(start..end)?;
        if !part.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        part.parse().ok()
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        number(0, 4),
        number(5, 7),
        number(8, 10),
        number(11, 13),
        number(14, 16),
        number(17, 19),
    ) else {
        return false;
    };
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        _ => return false,
    };
    if day == 0 || day > days || hour > 23 || minute > 59 || second > 59 {
        return false;
    }
    let mut zone = 19;
    if bytes.get(zone) == Some(&b'.') {
        zone += 1;
        let start = zone;
        while bytes.get(zone).is_some_and(u8::is_ascii_digit) {
            zone += 1;
        }
        if zone == start {
            return false;
        }
    }
    if text.get(zone..) == Some("Z") {
        return true;
    }
    if bytes.len() != zone + 6 || !matches!(bytes[zone], b'+' | b'-') || bytes[zone + 3] != b':' {
        return false;
    }
    matches!((number(zone+1,zone+3),number(zone+4,zone+6)),(Some(h),Some(m)) if h<=23 && m<=59)
}
fn valid_window(text: &str) -> bool {
    // Go duration syntax used by Metrics API metav1.Duration; require a positive window.
    let mut rest = text;
    let mut total = 0u64;
    while !rest.is_empty() {
        let end = rest
            .bytes()
            .take_while(|b| b.is_ascii_digit() || *b == b'.')
            .count();
        if end == 0 {
            return false;
        }
        let number = &rest[..end];
        rest = &rest[end..];
        let unit = [
            ("ns", 1u128),
            ("us", 1000),
            ("µs", 1000),
            ("ms", 1_000_000),
            ("s", 1_000_000_000),
            ("m", 60_000_000_000),
            ("h", 3_600_000_000_000),
        ]
        .into_iter()
        .find(|(suffix, _)| rest.starts_with(suffix));
        let Some((suffix, base)) = unit else {
            return false;
        };
        let Ok((amount, rounded)) = quantity(number, base) else {
            return false;
        };
        if rounded {
            return false;
        }
        let Some(sum) = total.checked_add(amount) else {
            return false;
        };
        total = sum;
        rest = &rest[suffix.len()..];
    }
    total > 0
}

/// Parse Kubernetes decimal/binary/exponent quantities using checked integer arithmetic.
/// Returns the nonnegative base unit count and whether a fractional base unit was rounded up.
fn quantity(raw: &str, base: u128) -> Result<(u64, bool)> {
    let error = || "invalid, negative, or overflowing metrics quantity".to_string();
    if raw.is_empty() || raw.len() > 128 {
        return Err(error());
    }
    let input = raw.strip_prefix('+').unwrap_or(raw);
    let number_end = input
        .bytes()
        .take_while(|c| c.is_ascii_digit() || *c == b'.')
        .count();
    if number_end == 0 {
        return Err(error());
    }
    let (number, suffix) = input.split_at(number_end);
    let mut numerator = 0u128;
    let mut fraction_digits = 0u32;
    let mut decimal = false;
    let mut digits = 0;
    for byte in number.bytes() {
        if byte == b'.' {
            if decimal {
                return Err(error());
            }
            decimal = true;
            continue;
        }
        digits += 1;
        numerator = numerator
            .checked_mul(10)
            .and_then(|v| v.checked_add(u128::from(byte - b'0')))
            .ok_or_else(error)?;
        if decimal {
            fraction_digits += 1;
        }
    }
    if digits == 0 {
        return Err(error());
    }
    let (multiplier, exponent): (u128, i32) = match suffix {
        "" => (1, 0),
        "n" => (1, -9),
        "u" => (1, -6),
        "m" => (1, -3),
        "k" => (1, 3),
        "M" => (1, 6),
        "G" => (1, 9),
        "T" => (1, 12),
        "P" => (1, 15),
        "E" => (1, 18),
        "Ki" => (1 << 10, 0),
        "Mi" => (1 << 20, 0),
        "Gi" => (1 << 30, 0),
        "Ti" => (1 << 40, 0),
        "Pi" => (1 << 50, 0),
        "Ei" => (1 << 60, 0),
        value if value.starts_with('e') || value.starts_with('E') => {
            (1, value[1..].parse::<i32>().map_err(|_| error())?)
        }
        _ => return Err(error()),
    };
    let shift = exponent
        .checked_sub(i32::try_from(fraction_digits).map_err(|_| error())?)
        .ok_or_else(error)?;
    if shift.unsigned_abs() > 38 {
        return Err(error());
    }
    numerator = numerator
        .checked_mul(multiplier)
        .and_then(|v| v.checked_mul(base))
        .ok_or_else(error)?;
    let denominator = if shift < 0 {
        10u128.checked_pow(shift.unsigned_abs()).ok_or_else(error)?
    } else {
        numerator = numerator
            .checked_mul(10u128.checked_pow(shift as u32).ok_or_else(error)?)
            .ok_or_else(error)?;
        1
    };
    let rounded = !numerator.is_multiple_of(denominator);
    let units = (numerator / denominator)
        .checked_add(u128::from(rounded))
        .ok_or_else(error)?;
    Ok((u64::try_from(units).map_err(|_| error())?, rounded))
}

pub fn demo(scope: MetricsScope, ns: &str) -> Result<MetricsSnapshot> {
    validate_namespace(ns)?;
    let mut snapshot = empty(scope, ns, "demo (deterministic sample)");
    let value = match scope {
        MetricsScope::Pods => {
            json!({"metadata":{"name":"api-7d4f9","namespace":"demo"},"timestamp":"2026-01-01T00:00:00Z","window":"30s","containers":[{"name":"api","usage":{"cpu":"125m","memory":"64Mi"}}]})
        }
        MetricsScope::Nodes => {
            json!({"metadata":{"name":"demo-node"},"timestamp":"2026-01-01T00:00:00Z","window":"30s","usage":{"cpu":"750m","memory":"1Gi"}})
        }
    };
    if scope == MetricsScope::Nodes || ns == "*" || ns == "demo" {
        snapshot.items = decode_rows(&value, scope, ns)?;
    }
    Ok(snapshot)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn quantities_are_exact_and_checked() {
        for (raw, base, expected) in [
            ("125m", 1_000_000_000, 125_000_000),
            ("987654321n", 1_000_000_000, 987_654_321),
            (".5", 1_000_000_000, 500_000_000),
            ("64Mi", 1, 67_108_864),
            ("1.5Gi", 1, 1_610_612_736),
            ("2e3", 1, 2000),
            ("2E-3", 1_000_000_000, 2_000_000),
        ] {
            assert_eq!(quantity(raw, base), Ok((expected, false)));
        }
        assert_eq!(quantity("1m", 1), Ok((1, true)));
        for raw in [
            "-1",
            "NaN",
            "inf",
            "1..2",
            "1MiB",
            "1K",
            "1e999",
            "18446744073709551616",
            "1e",
            "",
        ] {
            assert!(quantity(raw, 1).is_err(), "{raw}");
        }
    }
    #[test]
    fn decode_rejects_malformed_shapes_and_out_of_scope_data() {
        assert!(decode_rows(&json!({}), MetricsScope::Pods, "demo").is_err());
        let mut value = json!({"metadata":{"name":"pod","namespace":"demo"},"timestamp":"2026-01-01T00:00:00Z","window":"30s","containers":[{"name":"api","usage":{"cpu":"1m","memory":"1Mi"}}]});
        assert!(decode_rows(&value, MetricsScope::Pods, "other").is_err());
        value["containers"][0]["usage"]["cpu"] = json!(42);
        assert!(decode_rows(&value, MetricsScope::Pods, "demo").is_err());
        value["containers"] = json!(vec![json!({}); MAX_ROWS + 1]);
        assert!(decode_rows(&value, MetricsScope::Pods, "demo").is_err());
    }
    #[test]
    fn distinguishes_unavailable_and_forbidden() {
        assert!(status_error(404).contains("unavailable"));
        assert!(status_error(403).contains("forbidden"));
        assert!(!status_error(403).contains("unavailable"));
    }
    #[test]
    fn validates_measurement_time_and_dimensions() {
        for value in ["2026-01-01T00:00:00Z", "2024-02-29T23:59:59.123+01:00"] {
            assert!(valid_timestamp(value));
        }
        for value in [
            "now",
            "2026-02-29T00:00:00Z",
            "2026-01-01T24:00:00Z",
            "2026-01-01T00:00:00",
        ] {
            assert!(!valid_timestamp(value));
        }
        for value in ["30s", "1m30s", "0.5s", "1ns"] {
            assert!(valid_window(value));
        }
        for value in ["", "0s", "-30s", "30", "NaNs", "0.1ns"] {
            assert!(!valid_window(value));
        }
        assert!(normalize_cpu_quantity("1Mi").is_err());
        assert_eq!(normalize_memory_quantity("1Mi"), Ok((1_048_576, false)));
    }
    #[test]
    fn demo_is_deterministic_and_scoped() {
        assert_eq!(
            serde_json::to_string(
                &demo(MetricsScope::Pods, "demo").unwrap_or_else(|e| panic!("{e}"))
            )
            .ok(),
            serde_json::to_string(
                &demo(MetricsScope::Pods, "demo").unwrap_or_else(|e| panic!("{e}"))
            )
            .ok()
        );
        assert!(
            demo(MetricsScope::Pods, "other")
                .map(|s| s.items.is_empty())
                .unwrap_or(false)
        );
        assert!(demo(MetricsScope::Nodes, "bad/namespace").is_err());
    }
    fn sample(scope: MetricsScope, name: &str) -> Value {
        match scope {
            MetricsScope::Pods => {
                json!({"metadata":{"name":name,"namespace":"demo","annotations":{"private":"CANARY"}},"timestamp":"2026-01-01T00:00:00Z","window":"30s","containers":[{"name":"api","usage":{"cpu":"1000000n","memory":"1Mi"}}],"spec":{"private":"CANARY"}})
            }
            MetricsScope::Nodes => {
                json!({"metadata":{"name":name},"timestamp":"2026-01-01T00:00:00Z","window":"30s","usage":{"cpu":"125m","memory":"2Gi"}})
            }
        }
    }
    fn page(scope: MetricsScope, items: Vec<Value>, token: &str, version: &str) -> Value {
        json!({"apiVersion":"metrics.k8s.io/v1beta1","kind":if scope==MetricsScope::Pods {"PodMetricsList"} else {"NodeMetricsList"},"metadata":{"continue":token,"resourceVersion":version},"items":items})
    }
    type Requests = std::sync::Arc<std::sync::Mutex<Vec<String>>>;
    fn mock_api(
        scope: MetricsScope,
        ns: &str,
        pages: Vec<(u16, Value)>,
    ) -> (kube::Api<kube::api::DynamicObject>, Requests) {
        let queue = std::sync::Arc::new(std::sync::Mutex::new(std::collections::VecDeque::from(
            pages,
        )));
        let requests: Requests = Default::default();
        let recorded = requests.clone();
        let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
            assert_eq!(request.method(), http::Method::GET);
            recorded
                .lock()
                .unwrap_or_else(|e| panic!("{e}"))
                .push(request.uri().to_string());
            let (status, value) = queue
                .lock()
                .unwrap_or_else(|e| panic!("{e}"))
                .pop_front()
                .unwrap_or_else(|| panic!("unexpected metrics request"));
            async move {
                let body = http_body_util::Full::new(bytes::Bytes::from(value.to_string()));
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(status)
                        .body(http_body_util::Limited::new(body, MAX_BYTES))
                        .unwrap_or_else(|e| panic!("{e}")),
                )
            }
        });
        let client = kube::Client::new(service, "default");
        let resource = kube::api::ApiResource {
            group: "metrics.k8s.io".into(),
            version: "v1beta1".into(),
            api_version: "metrics.k8s.io/v1beta1".into(),
            kind: if scope == MetricsScope::Pods {
                "PodMetrics"
            } else {
                "NodeMetrics"
            }
            .into(),
            plural: if scope == MetricsScope::Pods {
                "pods"
            } else {
                "nodes"
            }
            .into(),
        };
        let api = if scope == MetricsScope::Pods && ns != "*" {
            kube::Api::namespaced_with(client, ns, &resource)
        } else {
            kube::Api::all_with(client, &resource)
        };
        (api, requests)
    }
    fn run_collect(
        scope: MetricsScope,
        ns: &str,
        pages: Vec<(u16, Value)>,
    ) -> (Result<MetricsSnapshot>, Vec<String>) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap_or_else(|e| panic!("{e}"));
        let (result, requests) = runtime.block_on(async {
            let (api, requests) = mock_api(scope, ns, pages);
            (collect_api(api, scope, ns).await, requests)
        });
        let recorded = requests.lock().unwrap_or_else(|e| panic!("{e}")).clone();
        (result, recorded)
    }
    #[test]
    fn mock_interop_pods_nodes_and_projection() {
        for (scope, ns, path) in [
            (
                MetricsScope::Pods,
                "demo",
                "/apis/metrics.k8s.io/v1beta1/namespaces/demo/pods",
            ),
            (MetricsScope::Pods, "*", "/apis/metrics.k8s.io/v1beta1/pods"),
            (
                MetricsScope::Nodes,
                "demo",
                "/apis/metrics.k8s.io/v1beta1/nodes",
            ),
        ] {
            let (result, requests) = run_collect(
                scope,
                ns,
                vec![(200, page(scope, vec![sample(scope, "sample")], "", "1"))],
            );
            let snapshot = result.unwrap_or_else(|e| panic!("{e}"));
            assert!(requests[0].starts_with(path));
            assert!(requests[0].contains("limit=200"));
            assert_eq!(snapshot.items.len(), 1);
            if scope == MetricsScope::Pods {
                assert_eq!(snapshot.items[0].cpu_nanocores, 1_000_000);
                assert_eq!(snapshot.items[0].cpu_millicores, "1");
                assert_eq!(snapshot.items[0].memory_bytes, 1_048_576);
            } else {
                assert!(snapshot.namespace.is_none());
                assert_eq!(snapshot.items[0].memory_bytes, 2_147_483_648);
            }
            assert!(
                !serde_json::to_string(&snapshot)
                    .unwrap_or_default()
                    .contains("CANARY")
            );
        }
    }
    #[test]
    fn mock_interop_pagination_rejects_repeats_and_inconsistency() {
        let scope = MetricsScope::Pods;
        let (result, requests) = run_collect(
            scope,
            "demo",
            vec![
                (200, page(scope, vec![sample(scope, "one")], "next", "1")),
                (200, page(scope, vec![sample(scope, "two")], "", "1")),
            ],
        );
        assert_eq!(result.map(|s| s.items.len()), Ok(2));
        assert!(requests[1].contains("continue=next"));
        for pages in [
            vec![
                (200, page(scope, vec![], "repeat", "1")),
                (200, page(scope, vec![], "repeat", "1")),
            ],
            vec![
                (200, page(scope, vec![sample(scope, "same")], "next", "1")),
                (200, page(scope, vec![sample(scope, "same")], "", "1")),
            ],
            vec![
                (200, page(scope, vec![], "next", "1")),
                (200, page(scope, vec![], "", "2")),
            ],
        ] {
            assert!(run_collect(scope, "demo", pages).0.is_err());
        }
    }
    #[test]
    fn mock_interop_status_and_malformed_samples_suppress_payloads() {
        for (code, word) in [(404, "unavailable"), (403, "forbidden")] {
            let value = json!({"apiVersion":"v1","kind":"Status","status":"Failure","message":"CANARY credential detail","reason":"Forbidden","code":code});
            let error = run_collect(MetricsScope::Pods, "demo", vec![(code, value)])
                .0
                .err()
                .unwrap_or_default();
            assert!(error.contains(word));
            assert!(!error.contains("CANARY"));
        }
        let mut malformed = sample(MetricsScope::Pods, "sample");
        malformed["containers"][0]["usage"]["cpu"] = json!("CANARYNaN");
        let error = run_collect(
            MetricsScope::Pods,
            "demo",
            vec![(200, page(MetricsScope::Pods, vec![malformed], "", "1"))],
        )
        .0
        .err()
        .unwrap_or_default();
        assert!(!error.is_empty());
        assert!(!error.contains("CANARY"));
    }
    #[test]
    fn mock_interop_namespace_and_container_row_limits() {
        let (result, requests) = run_collect(MetricsScope::Pods, "../default", vec![]);
        assert!(result.is_err());
        assert!(requests.is_empty());
        let mut other = sample(MetricsScope::Pods, "pod");
        other["metadata"]["namespace"] = json!("other");
        assert!(
            run_collect(
                MetricsScope::Pods,
                "demo",
                vec![(200, page(MetricsScope::Pods, vec![other], "", "1"))]
            )
            .0
            .is_err()
        );
        let pods = (0..501)
            .map(|i| {
                let mut pod = sample(MetricsScope::Pods, &format!("pod-{i}"));
                let mut second = pod["containers"][0].clone();
                second["name"] = json!("sidecar");
                pod["containers"]
                    .as_array_mut()
                    .unwrap_or_else(|| panic!("fixture containers"))
                    .push(second);
                pod
            })
            .collect();
        let snapshot = run_collect(
            MetricsScope::Pods,
            "demo",
            vec![(200, page(MetricsScope::Pods, pods, "", "1"))],
        )
        .0
        .unwrap_or_else(|e| panic!("{e}"));
        assert!(snapshot.truncated);
        assert_eq!(snapshot.items.len(), MAX_ROWS);
    }
    #[test]
    fn mock_interop_object_page_and_byte_limits() {
        let scope = MetricsScope::Nodes;
        let objects = (0..MAX_OBJECTS + 1)
            .map(|i| sample(scope, &format!("node-{i}")))
            .collect();
        let snapshot = run_collect(scope, "demo", vec![(200, page(scope, objects, "", "1"))])
            .0
            .unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(snapshot.items.len(), MAX_OBJECTS);
        assert!(snapshot.truncated);
        let pages = (0..MAX_PAGES)
            .map(|i| (200, page(scope, vec![], &format!("token-{i}"), "1")))
            .collect();
        let (result, requests) = run_collect(scope, "demo", pages);
        assert!(result.map(|s| s.truncated).unwrap_or(false));
        assert_eq!(requests.len(), MAX_PAGES);
        let mut oversized = sample(scope, "node");
        oversized["metadata"]["annotations"] = json!({"payload":"x".repeat(MAX_BYTES)});
        assert!(
            run_collect(
                scope,
                "demo",
                vec![(200, page(scope, vec![oversized], "", "1"))]
            )
            .0
            .is_err()
        );
        // Each page is under the transport cap, but their combined serialized size exceeds it.
        let mut big = sample(scope, "node-one");
        big["metadata"]["annotations"] = json!({"payload":"x".repeat(MAX_BYTES/2+1024)});
        let mut second = big.clone();
        second["metadata"]["name"] = json!("node-two");
        let error = run_collect(
            scope,
            "demo",
            vec![
                (200, page(scope, vec![big], "next", "1")),
                (200, page(scope, vec![second], "", "1")),
            ],
        )
        .0
        .err()
        .unwrap_or_default();
        assert!(error.contains("aggregate limit"));
    }
}
