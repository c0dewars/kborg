//! Bounded, generation-bound table projections of explicit Metrics API snapshots.
use crate::metrics::{MetricsRow, MetricsScope, MetricsSnapshot};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    time::{Duration, Instant},
};

const RETENTION: Duration = Duration::from_secs(45);

/// Rounded table labels only. Exact base units remain in Usage and all sorting.
pub fn compact_cpu(nanocores: u64) -> String {
    if nanocores > 0 && nanocores < 10_000 {
        return "<0.01".into();
    }
    let value = u128::from(nanocores);
    let text = decimal((value * 100 + 500_000) / 1_000_000);
    if text.len() <= 8 {
        return text;
    }
    let mut exponent = (nanocores / 1_000_000).ilog10();
    let scale = 10u128.pow(exponent) * 1_000_000;
    let mut mantissa = (value * 100 + scale / 2) / scale;
    if mantissa >= 1000 {
        mantissa = 100;
        exponent += 1;
    }
    format!("{}e{exponent}", decimal(mantissa))
}

pub fn compact_memory(bytes: u64) -> String {
    const UNITS: &[&str] = &["B", "Ki", "Mi", "Gi", "Ti", "Pi", "Ei"];
    let value = u128::from(bytes);
    let mut index = 0usize;
    while index + 1 < UNITS.len() && value >= 1u128 << ((index + 1) * 10) {
        index += 1;
    }
    loop {
        let scale = 1u128 << (index * 10);
        let hundredths = if value / scale >= 1000 {
            ((value * 10 + scale / 2) / scale) * 10
        } else {
            (value * 100 + scale / 2) / scale
        };
        if hundredths >= 102_400 && index + 1 < UNITS.len() {
            index += 1;
            continue;
        }
        return format!("{}{}", decimal(hundredths), UNITS[index]);
    }
}

fn decimal(hundredths: u128) -> String {
    let whole = hundredths / 100;
    let fraction = hundredths % 100;
    if fraction == 0 {
        whole.to_string()
    } else if fraction.is_multiple_of(10) {
        format!("{whole}.{}", fraction / 10)
    } else {
        format!("{whole}.{fraction:02}")
    }
}
#[derive(Debug)]
pub struct Usage {
    pub cpu_nanocores: u64,
    pub memory_bytes: u64,
    pub timestamp: String,
    pub window: String,
    pub identity_uid: String,
    pub identity_verified: bool,
}
#[derive(Default)]
pub struct TableMetrics {
    rows: BTreeMap<String, Usage>,
    received: Option<Instant>,
    notice: String,
}
impl TableMetrics {
    pub fn clear(&mut self) {
        *self = Self::default();
    }
    pub fn fail(&mut self, error: &str) {
        self.rows.clear();
        self.received = None;
        self.notice = format!("unavailable: {}", crate::safety::safe_text(error, 120));
    }
    pub fn record(&mut self, snapshot: &MetricsSnapshot, objects: &[Value], now: Instant) {
        self.rows.clear();
        self.received = Some(now);
        let mut unverified = false;
        for object in objects.iter().take(1000) {
            let Some(uid) = object["metadata"]["uid"]
                .as_str()
                .filter(|uid| !uid.is_empty())
            else {
                continue;
            };
            let Some(name) = object["metadata"]["name"].as_str() else {
                continue;
            };
            let kind = match snapshot.scope {
                MetricsScope::Pods => "Pod",
                MetricsScope::Nodes => "Node",
            };
            if object["apiVersion"] != "v1" || object["kind"] != kind {
                continue;
            }
            let namespace = object["metadata"]["namespace"].as_str();
            let candidates = snapshot
                .items
                .iter()
                .filter(|row| row.name == name && row.namespace.as_deref() == namespace)
                .collect::<Vec<_>>();
            if let Some(usage) = aggregate(
                object,
                &candidates,
                snapshot.scope,
                snapshot.source.to_ascii_lowercase().starts_with("demo"),
            ) {
                unverified |= !usage.identity_verified;
                self.rows.insert(uid.to_owned(), usage);
            }
        }
        self.notice = format!(
            "{} measured{}{}",
            self.rows.len(),
            if unverified {
                " | ~ UID absent; creation-time checked"
            } else {
                " | UID verified"
            },
            if snapshot.truncated {
                " | INCOMPLETE"
            } else {
                ""
            }
        );
        if self.rows.is_empty() {
            self.notice = "no matching current samples | - means unknown".into();
        }
    }
    pub fn usage(&self, object: &Value, now: Instant) -> Option<&Usage> {
        if now
            .checked_duration_since(self.received?)
            .is_none_or(|age| age > RETENTION)
        {
            return None;
        }
        self.rows.get(object["metadata"]["uid"].as_str()?)
    }
    // Sorting changes only on an explicit UI transition, allowing selection to
    // remain attached to its UID when a snapshot expires between input ticks.
    pub fn sort_usage(&self, object: &Value) -> Option<&Usage> {
        self.rows.get(object["metadata"]["uid"].as_str()?)
    }
    pub fn expired(&self, now: Instant) -> bool {
        !self.rows.is_empty()
            && self.received.is_some_and(|received| {
                now.checked_duration_since(received)
                    .is_none_or(|age| age > RETENTION)
            })
    }
    pub fn expire(&mut self) {
        self.rows.clear();
    }
    pub fn label(&self, now: Instant) -> String {
        if self.received.is_some_and(|received| {
            now.checked_duration_since(received)
                .is_none_or(|age| age > RETENTION)
        }) {
            return "METRICS STALE | - means unknown | m details".into();
        }
        if self.notice.is_empty() {
            "METRICS loading | - means unknown".into()
        } else {
            format!("METRICS {} | m details", self.notice)
        }
    }
}
fn aggregate(
    object: &Value,
    rows: &[&MetricsRow],
    scope: MetricsScope,
    demo: bool,
) -> Option<Usage> {
    let uid = object["metadata"]["uid"].as_str()?;
    let expected = if scope == MetricsScope::Pods {
        object["spec"]["containers"]
            .as_array()?
            .iter()
            .filter_map(|container| container["name"].as_str())
            .collect::<BTreeSet<_>>()
    } else {
        BTreeSet::new()
    };
    if scope == MetricsScope::Pods && expected.is_empty() {
        return None;
    }
    let mut seen = BTreeSet::new();
    let mut cpu = 0u64;
    let mut memory = 0u64;
    let mut measurement: Option<(&str, &str)> = None;
    let mut verified = true;
    for row in rows {
        if scope == MetricsScope::Pods
            && !row
                .container
                .as_deref()
                .is_some_and(|container| expected.contains(container))
        {
            continue;
        }
        if !seen.insert(row.container.as_deref()) {
            return None;
        }
        if row
            .identity_uid
            .as_deref()
            .is_some_and(|identity| identity != uid)
        {
            return None;
        }
        let timestamp = row.timestamp.parse::<k8s_openapi::jiff::Timestamp>().ok()?;
        if !demo
            && timestamp.as_second()
                < k8s_openapi::jiff::Timestamp::now()
                    .as_second()
                    .saturating_sub(120)
        {
            return None;
        }
        if !demo
            && timestamp.as_second()
                > k8s_openapi::jiff::Timestamp::now()
                    .as_second()
                    .saturating_add(5)
        {
            return None;
        }
        if row.identity_uid.is_none() {
            verified = false;
            let created = object["metadata"]["creationTimestamp"]
                .as_str()?
                .parse::<k8s_openapi::jiff::Timestamp>()
                .ok()?;
            let window = window_nanoseconds(&row.window)?;
            if timestamp.as_nanosecond().checked_sub(i128::from(window))? < created.as_nanosecond()
            {
                return None;
            }
        }
        if measurement
            .is_some_and(|(timestamp, window)| timestamp != row.timestamp || window != row.window)
        {
            return None;
        }
        measurement = Some((&row.timestamp, &row.window));
        cpu = cpu.checked_add(row.cpu_nanocores)?;
        memory = memory.checked_add(row.memory_bytes)?;
    }
    if (scope == MetricsScope::Pods && seen.len() != expected.len())
        || (scope == MetricsScope::Nodes && seen.len() != 1)
    {
        return None;
    }
    let (timestamp, window) = measurement?;
    Some(Usage {
        cpu_nanocores: cpu,
        memory_bytes: memory,
        timestamp: timestamp.into(),
        window: window.into(),
        identity_uid: uid.into(),
        identity_verified: verified,
    })
}
/// Metrics API uses Go durations; fractions are converted exactly to nanoseconds.
fn window_nanoseconds(mut text: &str) -> Option<u64> {
    let mut total = 0u128;
    while !text.is_empty() {
        let end = text
            .bytes()
            .take_while(|b| b.is_ascii_digit() || *b == b'.')
            .count();
        if end == 0 {
            return None;
        }
        let number = &text[..end];
        text = &text[end..];
        let (suffix, base) = [
            ("ns", 1u128),
            ("us", 1000),
            ("µs", 1000),
            ("ms", 1_000_000),
            ("s", 1_000_000_000),
            ("m", 60_000_000_000),
            ("h", 3_600_000_000_000),
        ]
        .into_iter()
        .find(|(suffix, _)| text.starts_with(suffix))?;
        text = &text[suffix.len()..];
        let (whole, fraction) = number.split_once('.').unwrap_or((number, ""));
        if fraction.len() > 9
            || !whole.bytes().all(|b| b.is_ascii_digit())
            || !fraction.bytes().all(|b| b.is_ascii_digit())
        {
            return None;
        }
        let integer = if whole.is_empty() {
            0
        } else {
            whole.parse::<u128>().ok()?
        };
        let denominator = 10u128.checked_pow(fraction.len() as u32)?;
        let fractional = if fraction.is_empty() {
            0
        } else {
            fraction.parse::<u128>().ok()?.checked_mul(base)?
        };
        if fractional % denominator != 0 {
            return None;
        }
        total = total
            .checked_add(integer.checked_mul(base)?)?
            .checked_add(fractional / denominator)?;
    }
    u64::try_from(total).ok().filter(|value| *value > 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row(container: &str, cpu: u64) -> MetricsRow {
        MetricsRow {
            name: "api".into(),
            namespace: Some("test".into()),
            container: Some(container.into()),
            timestamp: "2026-10-22T12:00:30Z".into(),
            window: "30s".into(),
            cpu_raw: cpu.to_string(),
            memory_raw: "1Mi".into(),
            cpu_nanocores: cpu,
            cpu_millicores: crate::metrics::format_millicores(cpu),
            memory_bytes: 1_048_576,
            rounded_up: false,
            identity_uid: Some("uid".into()),
            allocations: None,
        }
    }
    fn object() -> Value {
        serde_json::json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"api","namespace":"test","uid":"uid","creationTimestamp":"2026-10-22T11:00:00Z"},"spec":{"containers":[{"name":"app"},{"name":"sidecar"}]}})
    }
    #[test]
    fn compact_table_quantities_keep_units_and_uid_marker_inside_columns() {
        assert_eq!(compact_cpu(72_574_100), "72.57");
        assert_eq!(compact_cpu(1), "<0.01");
        assert_eq!(compact_cpu(9_999), "<0.01");
        assert_eq!(compact_cpu(10_000), "0.01");
        assert_eq!(compact_cpu(0), "0");
        assert_eq!(compact_cpu(u64::MAX), "1.84e13");
        assert_eq!(compact_memory(1_268_680 * 1024), "1.21Gi");
        assert_eq!(compact_memory(67_108_864), "64Mi");
        assert_eq!(compact_memory(u64::MAX), "16Ei");
        for value in [0, 1, 1023, 1024, 1_048_575, 1_268_680 * 1024, u64::MAX] {
            for text in [compact_cpu(value), compact_memory(value)] {
                assert!(text.len() <= 8, "{text}");
                assert!(
                    format!("{text}~").len() <= 9,
                    "UID marker must never force terminal truncation"
                );
            }
        }
        assert!(!compact_memory(1_268_680 * 1024).contains('~'));
    }
    #[test]
    fn aggregates_exactly_and_rejects_partial_duplicate_overflow_and_replacement() {
        let a = row("app", 1_000_001);
        let b = row("sidecar", 2_000_002);
        let object = object();
        let usage = aggregate(&object, &[&a, &b], MetricsScope::Pods, true);
        assert_eq!(
            usage.as_ref().map(|usage| usage.cpu_nanocores),
            Some(3_000_003)
        );
        assert_eq!(
            usage.as_ref().map(|usage| usage.memory_bytes),
            Some(2_097_152)
        );
        assert!(aggregate(&object, &[&a], MetricsScope::Pods, true).is_none());
        assert!(aggregate(&object, &[&a, &a, &b], MetricsScope::Pods, true).is_none());
        let mut wrong = b;
        wrong.identity_uid = Some("replacement".into());
        assert!(aggregate(&object, &[&a, &wrong], MetricsScope::Pods, true).is_none());
        wrong.identity_uid = Some("uid".into());
        wrong.cpu_nanocores = u64::MAX;
        assert!(aggregate(&object, &[&a, &wrong], MetricsScope::Pods, true).is_none());
    }
    #[test]
    fn missing_uid_requires_entire_window_after_creation_and_cache_expires() {
        let mut object = object();
        object["spec"]["containers"] = serde_json::json!([{"name":"app"}]);
        let mut a = row("app", 1);
        a.identity_uid = None;
        assert_eq!(
            aggregate(&object, &[&a], MetricsScope::Pods, true)
                .map(|usage| usage.identity_verified),
            Some(false)
        );
        object["metadata"]["creationTimestamp"] = "2026-10-22T12:00:15Z".into();
        assert!(aggregate(&object, &[&a], MetricsScope::Pods, true).is_none());
        object["metadata"]["creationTimestamp"] = "2026-10-22T11:00:00Z".into();
        let now = Instant::now();
        let mut cache = TableMetrics::default();
        let snapshot = MetricsSnapshot {
            api_version: "metrics.k8s.io/v1beta1",
            scope: MetricsScope::Pods,
            namespace: Some("test".into()),
            source: "DEMO",
            items: vec![a],
            truncated: false,
            allocation_status: None,
        };
        cache.record(&snapshot, &[object.clone()], now);
        assert!(cache.usage(&object, now).is_some());
        assert!(
            cache
                .usage(&object, now + RETENTION + Duration::from_secs(1))
                .is_none()
        );
        object["metadata"]["uid"] = "replacement".into();
        assert!(cache.usage(&object, now).is_none());
        cache.fail("denied");
        assert!(cache.label(now).contains("unavailable"));
    }
    #[test]
    fn parses_go_duration_without_float_rounding() {
        assert_eq!(window_nanoseconds("1m30.5s"), Some(90_500_000_000));
        assert_eq!(window_nanoseconds("0.5ms"), Some(500_000));
        assert_eq!(window_nanoseconds("1.5ns"), None);
        assert_eq!(window_nanoseconds("0s"), None);
    }
}
