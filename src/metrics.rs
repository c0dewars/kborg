//! Read-only, bounded access to the standard Kubernetes Metrics API.
use crate::{Result, gateway::Gateway, safety};
use kube::api::ListParams;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, HashSet, VecDeque},
    time::{Duration, Instant},
};

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
    /// Present only for the explicit additional resource-read workflow.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allocation_status: Option<String>,
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
    /// Retained only for detecting replacement of a named object in local history.
    /// This does not change the versioned machine-output projection.
    #[serde(skip)]
    pub identity_uid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allocations: Option<crate::allocations::ResourceComparison>,
}
impl MetricsSnapshot {
    pub fn text(&self) -> String {
        let mut output = format!(
            "Metrics API {} | {} | measurements include timestamp and window\nNAMESPACE/NAME  CONTAINER  CPU(m)  MEMORY(bytes; exact size)  TIMESTAMP  WINDOW\n",
            self.api_version, self.source
        );
        if let Some(status) = &self.allocation_status {
            output.push_str(&format!("Resource comparisons: {status}\n"));
        }
        for row in &self.items {
            let target = row
                .namespace
                .as_ref()
                .map(|ns| format!("{ns}/{}", row.name))
                .unwrap_or_else(|| row.name.clone());
            output.push_str(&format!(
                "{}  {}  {}  {} ({})  {}  {}{}\n",
                target,
                row.container.as_deref().unwrap_or("-"),
                row.cpu_millicores,
                row.memory_bytes,
                format_memory(row.memory_bytes),
                row.timestamp,
                row.window,
                if row.rounded_up {
                    " (rounded up to base unit)"
                } else {
                    ""
                }
            ));
            if let Some(allocations) = &row.allocations {
                output.push_str(&allocations.text());
            }
        }
        if self.items.is_empty() {
            output.push_str("No metrics samples returned; missing samples do not mean zero usage.\nNew containers may need a collection window. Inspect pod/node status and events while waiting.\n");
        }
        if self.truncated {
            output.push_str(
                "Metrics truncated at the bounded result limit; narrow namespace scope.\n",
            );
        }
        output
    }
}

/// Session-local history bounds. No samples are persisted or collected in the background.
pub const HISTORY_MAX_IDENTITIES: usize = 64;
pub const HISTORY_MAX_SAMPLES: usize = 60;
pub const HISTORY_RETENTION: Duration = Duration::from_secs(10 * 60);
const HISTORY_CHART_WIDTH: usize = 36;

#[derive(Debug, PartialEq, Eq)]
struct HistorySelection {
    context: String,
    scope: MetricsScope,
    namespace: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct MetricsIdentity {
    namespace: Option<String>,
    name: String,
    container: Option<String>,
}
impl MetricsIdentity {
    fn from_row(row: &MetricsRow) -> Self {
        Self {
            namespace: row.namespace.clone(),
            name: row.name.clone(),
            container: row.container.clone(),
        }
    }
    fn label(&self) -> String {
        let target = self
            .namespace
            .as_ref()
            .map(|ns| format!("{ns}/{}", self.name))
            .unwrap_or_else(|| self.name.clone());
        match &self.container {
            Some(container) => format!("{target} [{container}]"),
            None => target,
        }
    }
}

#[derive(Clone, Debug)]
struct HistoryUsage {
    cpu_nanocores: u64,
    memory_bytes: u64,
}
#[derive(Debug)]
struct HistoryPoint {
    observed_at: Instant,
    usage: Option<HistoryUsage>,
}
#[derive(Debug)]
struct HistorySeries {
    uid: Option<String>,
    latest_measurement: Option<(i64, String)>,
    last_seen: Instant,
    points: VecDeque<HistoryPoint>,
}
impl HistorySeries {
    fn push(&mut self, now: Instant, usage: Option<HistoryUsage>) {
        // Do not allow a delayed result to reorder a series, or an error to
        // manufacture more than one gap at the same observation instant.
        if let Some(last) = self.points.back()
            && (now < last.observed_at || (now == last.observed_at && usage.is_none()))
        {
            return;
        }
        if self.points.len() == HISTORY_MAX_SAMPLES {
            self.points.pop_front();
        }
        self.points.push_back(HistoryPoint {
            observed_at: now,
            usage,
        });
    }
    fn prune(&mut self, now: Instant) {
        while self.points.front().is_some_and(|point| {
            now.saturating_duration_since(point.observed_at) > HISTORY_RETENTION
        }) {
            self.points.pop_front();
        }
    }
}

/// A bounded, read-only history of actual Metrics API measurements.
///
/// Identity is namespace/name/container. A supplied object UID detects name
/// reuse; if the API omits UID, no replacement guarantee is implied. Duplicate
/// measurement timestamps do not become new samples. Errors, missing rows and
/// backward timestamps produce gaps, never zeros or carried-forward values.
#[derive(Debug, Default)]
pub struct MetricsHistory {
    selection: Option<HistorySelection>,
    source: Option<&'static str>,
    series: BTreeMap<MetricsIdentity, HistorySeries>,
    limited: bool,
}
impl MetricsHistory {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    fn select(&mut self, context: &str, scope: MetricsScope, namespace: &str) -> bool {
        if context.is_empty()
            || context.len() > 4096
            || safety::safe_text(context, 4096) != context
            || validate_namespace(namespace).is_err()
        {
            self.clear();
            return false;
        }
        let selection = HistorySelection {
            context: context.into(),
            scope,
            namespace: (scope == MetricsScope::Pods).then(|| namespace.into()),
        };
        if self.selection.as_ref() != Some(&selection) {
            self.clear();
            self.selection = Some(selection);
        }
        true
    }

    pub fn record(&mut self, context: &str, snapshot: &MetricsSnapshot, now: Instant) {
        let namespace = snapshot.namespace.as_deref().unwrap_or("*");
        if !self.select(context, snapshot.scope, namespace) {
            return;
        }
        if self.source.is_some_and(|source| source != snapshot.source) {
            self.series.clear();
        }
        self.source = Some(snapshot.source);
        let incoming: BTreeMap<_, _> = snapshot
            .items
            .iter()
            .take(MAX_ROWS)
            .filter(|row| valid_history_identity(row, snapshot.scope, namespace))
            .map(|row| (MetricsIdentity::from_row(row), row))
            .collect();
        self.limited = snapshot.truncated || incoming.len() > HISTORY_MAX_IDENTITIES;
        for (identity, series) in &mut self.series {
            series.prune(now);
            if !incoming.contains_key(identity) {
                series.push(now, None);
            }
        }
        // Prefer identities already being tracked. If the scope is larger than
        // the cap, a reordered API response cannot churn the retained charts.
        for (identity, row) in &incoming {
            if !self.series.contains_key(identity) {
                continue;
            }
            self.record_row(identity, row, now);
        }
        for (identity, row) in &incoming {
            if self.series.contains_key(identity) {
                continue;
            }
            if self.series.len() >= HISTORY_MAX_IDENTITIES {
                let evict = self
                    .series
                    .iter()
                    .filter(|(key, _)| !incoming.contains_key(*key))
                    .min_by_key(|(_, series)| series.last_seen)
                    .map(|(key, _)| key.clone());
                match evict {
                    Some(key) => {
                        self.series.remove(&key);
                    }
                    None => {
                        self.limited = true;
                        continue;
                    }
                }
            }
            self.series.insert(
                identity.clone(),
                HistorySeries {
                    uid: row.identity_uid.clone(),
                    latest_measurement: None,
                    last_seen: now,
                    points: VecDeque::new(),
                },
            );
            self.record_row(identity, row, now);
        }
        self.series.retain(|_, series| {
            now.saturating_duration_since(series.last_seen) <= HISTORY_RETENTION
        });
    }

    fn record_row(&mut self, identity: &MetricsIdentity, row: &MetricsRow, now: Instant) {
        let Some(series) = self.series.get_mut(identity) else {
            return;
        };
        if now < series.last_seen {
            return;
        }
        // A change to a known UID, or upgrading an unknown identity to a known
        // one, starts a fresh series rather than mixing object generations.
        if row.identity_uid.is_some() && row.identity_uid != series.uid {
            series.points.clear();
            series.latest_measurement = None;
            series.uid = row.identity_uid.clone();
        }
        series.last_seen = now;
        let Some(measurement) = timestamp_key(&row.timestamp) else {
            series.push(now, None);
            return;
        };
        if let Some(previous) = &series.latest_measurement {
            if &measurement == previous {
                return;
            }
            if &measurement < previous {
                series.push(now, None);
                return;
            }
        }
        series.latest_measurement = Some(measurement);
        series.push(
            now,
            Some(HistoryUsage {
                cpu_nanocores: row.cpu_nanocores,
                memory_bytes: row.memory_bytes,
            }),
        );
    }

    pub fn record_error(
        &mut self,
        context: &str,
        scope: MetricsScope,
        namespace: &str,
        now: Instant,
    ) {
        if !self.select(context, scope, namespace) {
            return;
        }
        for series in self.series.values_mut() {
            series.prune(now);
            series.push(now, None);
        }
        self.series.retain(|_, series| {
            now.saturating_duration_since(series.last_seen) <= HISTORY_RETENTION
        });
    }

    pub fn text(&self, now: Instant) -> String {
        let mut output = format!(
            "\nSESSION HISTORY | last 10 min | at most {HISTORY_MAX_IDENTITIES} identities x {HISTORY_MAX_SAMPLES} slots\nRefresh with m; g charts the selected Pod or Node. Graphs run oldest -> newest; '-' = missing/error.\nEach chart scales from 0 to its observed maximum, not a usage percentage.\nRepeated measurement timestamps are not new samples.\n"
        );
        let mut rendered = 0;
        for (identity, series) in &self.series {
            let points: Vec<_> = series
                .points
                .iter()
                .filter(|point| {
                    now.saturating_duration_since(point.observed_at) <= HISTORY_RETENTION
                })
                .collect();
            let values: Vec<_> = points
                .iter()
                .filter_map(|point| point.usage.as_ref())
                .collect();
            let Some(latest) = values.last() else {
                continue;
            };
            rendered += 1;
            output.push_str(&format!("\n{}\n", identity.label()));
            let gap = points.last().is_some_and(|point| point.usage.is_none());
            for (label, cpu) in [("CPU", true), ("MEM", false)] {
                let quantity = |usage: &HistoryUsage| {
                    if cpu {
                        usage.cpu_nanocores
                    } else {
                        usage.memory_bytes
                    }
                };
                let min = values
                    .iter()
                    .map(|usage| quantity(usage))
                    .min()
                    .unwrap_or(0);
                let max = values
                    .iter()
                    .map(|usage| quantity(usage))
                    .max()
                    .unwrap_or(0);
                let format = |value| {
                    if cpu {
                        format!("{}m", format_millicores(value))
                    } else {
                        format_memory(value)
                    }
                };
                let chart = history_chart(&points, cpu, max);
                output.push_str(&format!(
                    " {label} |{chart:<width$}| {} {}\n     min {} / max {}\n",
                    if gap { "gap; last" } else { "latest" },
                    format(quantity(latest)),
                    format(min),
                    format(max),
                    width = HISTORY_CHART_WIDTH
                ));
            }
        }
        if rendered == 0 {
            output.push_str("No history measurements in the retention window.\n");
        }
        if self.limited {
            output.push_str(
                "History is bounded; at most 64 resource/container identities are tracked.\n",
            );
        }
        output
    }
}

/// One aggregated observation of an object for chart mode.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ChartSample {
    /// How long before `now` it was observed.
    pub age: Duration,
    /// Summed CPU nanocores and memory bytes, or `None` for a gap (error,
    /// missing row, or a sample missing some of the object's containers).
    pub usage: Option<(u64, u64)>,
}

impl MetricsHistory {
    /// The bounded history of one Pod (all its containers summed) or Node,
    /// oldest first. Partial observations are gaps, never partial sums.
    pub fn object_samples(
        &self,
        namespace: Option<&str>,
        name: &str,
        now: Instant,
    ) -> Vec<ChartSample> {
        let members: Vec<&HistorySeries> = self
            .series
            .iter()
            .filter(|(identity, _)| {
                identity.name == name && identity.namespace.as_deref() == namespace
            })
            .map(|(_, series)| series)
            .collect();
        let mut instants: BTreeMap<Instant, (u64, u64, usize, bool)> = BTreeMap::new();
        for series in &members {
            for point in &series.points {
                if now.saturating_duration_since(point.observed_at) > HISTORY_RETENTION
                    || point.observed_at > now
                {
                    continue;
                }
                let entry = instants
                    .entry(point.observed_at)
                    .or_insert((0, 0, 0, false));
                match &point.usage {
                    Some(usage) => {
                        entry.0 = entry.0.saturating_add(usage.cpu_nanocores);
                        entry.1 = entry.1.saturating_add(usage.memory_bytes);
                        entry.2 += 1;
                    }
                    None => entry.3 = true,
                }
            }
        }
        instants
            .into_iter()
            .take(HISTORY_MAX_SAMPLES * 2)
            .map(|(at, (cpu, memory, count, gap))| ChartSample {
                age: now.saturating_duration_since(at),
                usage: (!gap && count == members.len()).then_some((cpu, memory)),
            })
            .collect()
    }
}

fn history_chart(points: &[&HistoryPoint], cpu: bool, maximum: u64) -> String {
    const LEVELS: &[u8] = b".:=+*#@";
    points
        .iter()
        .skip(points.len().saturating_sub(HISTORY_CHART_WIDTH))
        .map(|point| match &point.usage {
            None => '-',
            Some(usage) => {
                let value = if cpu {
                    usage.cpu_nanocores
                } else {
                    usage.memory_bytes
                };
                let level = if maximum == 0 {
                    0
                } else {
                    (u128::from(value) * (LEVELS.len() - 1) as u128 / u128::from(maximum)) as usize
                };
                char::from(LEVELS[level.min(LEVELS.len() - 1)])
            }
        })
        .collect()
}

fn valid_history_identity(row: &MetricsRow, scope: MetricsScope, namespace: &str) -> bool {
    !row.name.is_empty()
        && row.name.len() <= 253
        && row
            .name
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-' || c == b'.')
        && row
            .identity_uid
            .as_deref()
            .is_none_or(|uid| uid.len() <= 128 && safety::safe_text(uid, 128) == uid)
        && match scope {
            MetricsScope::Pods => {
                row.namespace
                    .as_deref()
                    .is_some_and(|ns| safety::valid_id(ns) && (namespace == "*" || namespace == ns))
                    && row.container.as_deref().is_some_and(safety::valid_id)
            }
            MetricsScope::Nodes => row.namespace.is_none() && row.container.is_none(),
        }
}

pub(crate) fn format_millicores(nanocores: u64) -> String {
    let fractional = nanocores % 1_000_000;
    if fractional == 0 {
        (nanocores / 1_000_000).to_string()
    } else {
        format!("{}.{fractional:06}", nanocores / 1_000_000)
            .trim_end_matches('0')
            .to_string()
    }
}

/// Human-readable memory using an exact integral binary unit, without float rounding.
pub(crate) fn format_memory(bytes: u64) -> String {
    if bytes != 0 {
        for (unit, shift) in [
            ("Ei", 60),
            ("Pi", 50),
            ("Ti", 40),
            ("Gi", 30),
            ("Mi", 20),
            ("Ki", 10),
        ] {
            let divisor = 1u64 << shift;
            if bytes.is_multiple_of(divisor) {
                return format!("{}{unit}", bytes / divisor);
            }
        }
    }
    format!("{bytes}B")
}

pub async fn collect(gateway: &Gateway, scope: MetricsScope, ns: &str) -> Result<MetricsSnapshot> {
    validate_namespace(ns)?;
    if gateway.is_demo() {
        // Each demo refresh is the next step of a fixed sequence, so chart
        // mode has a live-looking history; the first call equals `demo`.
        static STEP: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let step = STEP.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        return demo_step(scope, ns, step);
    }
    let api = gateway.metrics_api(scope, ns)?;
    collect_api(api, scope, ns).await
}

/// Explicit opt-in: usage first, then one separately bounded current resource
/// read. A denied/failed denominator read retains the valid usage snapshot.
pub async fn collect_with_allocations(
    gateway: &Gateway,
    scope: MetricsScope,
    ns: &str,
) -> Result<MetricsSnapshot> {
    let mut snapshot = collect(gateway, scope, ns).await?;
    let resource = if scope == MetricsScope::Pods {
        "pods"
    } else {
        "nodes"
    };
    let objects = tokio::time::timeout(BUDGET, gateway.list(resource, ns)).await;
    match objects {
        Ok(Ok(objects)) => {
            if let Err(error) = crate::allocations::enrich(&mut snapshot, &objects) {
                snapshot.allocation_status = Some(format!("Resource denominator snapshot unavailable: {}. Usage retained.", safety::safe_text(&error, 256)));
            }
        }
        Ok(Err(error)) => snapshot.allocation_status = Some(format!("Additional resource read failed: {}. Usage retained; no denominators inferred.", safety::safe_text(&error, 256))),
        Err(_) => snapshot.allocation_status = Some("Additional resource read exceeded 15-second budget. Usage retained; no denominators inferred.".into()),
    }
    Ok(snapshot)
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
        allocation_status: None,
    }
}
fn metrics_error(error: kube::Error) -> String {
    match &error {
        kube::Error::Api(response) if matches!(response.code, 401 | 403 | 404 | 429 | 503) => {
            status_error(response.code)
        }
        _ => crate::errors::explain("Metrics API read", &error, &crate::errors::Scope::default()),
    }
}
fn status_error(code: u16) -> String {
    match code {
        404 => "Metrics API unavailable: metrics.k8s.io/v1beta1 is not served. Next: ask an administrator to install a working metrics provider (none is installed automatically); Pod/node status and events remain available without metrics.".into(),
        403 => "Metrics API read forbidden: core pod/node read permission alone does not grant metrics access. Next: check with `kubectl auth can-i list pods.metrics.k8s.io` and request list on pods.metrics.k8s.io in the namespace (nodes.metrics.k8s.io for node metrics).".into(),
        401 => "Metrics API authentication failed: the credentials were rejected. Next: refresh your login and reconnect before retrying.".into(),
        429 => "Metrics API is rate limited (HTTP 429); no zero-usage samples were recorded. Next: wait a few seconds before refreshing.".into(),
        503 => "Metrics API unavailable: its registered provider is not ready (HTTP 503); no zero-usage samples were recorded. Next: check provider/APIService health (`kubectl get apiservice v1beta1.metrics.k8s.io`) and wait for a collection window.".into(),
        _ => crate::errors::render(
            "Metrics API read",
            &crate::errors::Cause::Rejected(code, String::new()),
            &crate::errors::Scope::default(),
        ),
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
    let identity_uid = match meta.get("uid") {
        None | Some(Value::Null) => None,
        Some(Value::String(uid)) if uid.is_empty() => None,
        Some(_) => Some(bounded_text(meta, "uid", 128)?),
    };
    if !valid_timestamp(&timestamp) || !valid_window(&window) {
        return Err("malformed metrics timestamp or window".into());
    }
    let make_row = |usage: &Value, container: Option<String>| -> Result<MetricsRow> {
        let cpu_raw = bounded_text(usage, "cpu", 128)?;
        let memory_raw = bounded_text(usage, "memory", 128)?;
        let (cpu_nanocores, cpu_rounded) = normalize_cpu_quantity(&cpu_raw)?;
        let (memory_bytes, memory_rounded) = normalize_memory_quantity(&memory_raw)?;
        let cpu_millicores = format_millicores(cpu_nanocores);
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
            identity_uid: identity_uid.clone(),
            allocations: None,
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

/// Comparable RFC3339 time, including explicit offsets and arbitrary bounded
/// fractional precision. Only the display quantities use base-unit rounding.
pub(crate) fn timestamp_key(text: &str) -> Option<(i64, String)> {
    if text.len() > 64 || !valid_timestamp(text) {
        return None;
    }
    let number = |start, end| text.get(start..end)?.parse::<i64>().ok();
    let year = number(0, 4)?;
    let month = usize::try_from(number(5, 7)?).ok()?;
    let day = number(8, 10)?;
    let before_month = [0i64, 0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334];
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    // Absolute Gregorian days since year zero; only differences/order matter.
    let days = 365 * year + (year + 3) / 4 - (year + 99) / 100
        + (year + 399) / 400
        + before_month[month]
        + i64::from(month > 2 && leap)
        + day
        - 1;
    let mut seconds =
        days * 86_400 + number(11, 13)? * 3600 + number(14, 16)? * 60 + number(17, 19)?;
    let mut zone = 19;
    let fraction = if text.as_bytes().get(zone) == Some(&b'.') {
        zone += 1;
        let start = zone;
        while text.as_bytes().get(zone).is_some_and(u8::is_ascii_digit) {
            zone += 1;
        }
        text[start..zone].trim_end_matches('0').to_string()
    } else {
        String::new()
    };
    if text.get(zone..) != Some("Z") {
        let offset = number(zone + 1, zone + 3)? * 3600 + number(zone + 4, zone + 6)? * 60;
        seconds += if text.as_bytes()[zone] == b'+' {
            -offset
        } else {
            offset
        };
    }
    Some((seconds, fraction))
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
    demo_step(scope, ns, 0)
}

/// Step `n` of the deterministic demo sequence: a new measurement timestamp
/// every step and usage following a fixed 16-step wave. Step 0 is `demo`.
pub fn demo_step(scope: MetricsScope, ns: &str, step: u64) -> Result<MetricsSnapshot> {
    validate_namespace(ns)?;
    const WAVE: [u64; 16] = [
        100, 112, 125, 140, 160, 150, 130, 118, 105, 96, 90, 98, 110, 135, 170, 120,
    ];
    let phase = WAVE[(step % 16) as usize];
    let timestamp = k8s_openapi::jiff::Timestamp::from_second(
        1_791_615_630_i64 + i64::try_from(step % (1 << 32)).unwrap_or(0) * 15,
    )
    .map_err(|_| "demo timestamp out of range")?
    .to_string();
    let scale = |base: u64| base * phase / 100;
    let (cpu, memory) = match scope {
        MetricsScope::Pods => (format!("{}m", scale(125)), format!("{}Mi", scale(64))),
        MetricsScope::Nodes => (format!("{}m", scale(750)), format!("{}Mi", scale(1024))),
    };
    let (cpu, memory) = if step == 0 {
        match scope {
            MetricsScope::Pods => ("125m".to_owned(), "64Mi".to_owned()),
            MetricsScope::Nodes => ("750m".to_owned(), "1Gi".to_owned()),
        }
    } else {
        (cpu, memory)
    };
    let mut snapshot = empty(scope, ns, "demo (deterministic sample)");
    let value = match scope {
        MetricsScope::Pods => {
            json!({"metadata":{"name":"api-7d4f9","namespace":"demo","uid":"demo-Pod-api-7d4f9"},"timestamp":timestamp,"window":"30s","containers":[{"name":"app","usage":{"cpu":cpu,"memory":memory}}]})
        }
        MetricsScope::Nodes => {
            json!({"metadata":{"name":"demo-node","uid":"demo-Node-demo-node"},"timestamp":timestamp,"window":"30s","usage":{"cpu":cpu,"memory":memory}})
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
    fn history_snapshot(
        scope: MetricsScope,
        namespace: &str,
        name: &str,
        timestamp: &str,
        cpu: &str,
        memory: &str,
        uid: Option<&str>,
    ) -> MetricsSnapshot {
        let mut value = sample(scope, name);
        value["timestamp"] = json!(timestamp);
        if scope == MetricsScope::Pods {
            value["metadata"]["namespace"] = json!(namespace);
            value["containers"][0]["usage"] = json!({"cpu":cpu,"memory":memory});
        } else {
            value["usage"] = json!({"cpu":cpu,"memory":memory});
        }
        if let Some(uid) = uid {
            value["metadata"]["uid"] = json!(uid);
        }
        let mut snapshot = empty(scope, namespace, "cluster");
        snapshot.items = decode_rows(&value, scope, namespace).unwrap_or_else(|e| panic!("{e}"));
        snapshot
    }
    #[test]
    fn history_bounds_identities_samples_and_observation_retention() {
        let started = Instant::now();
        let mut history = MetricsHistory::default();
        let mut snapshot = empty(MetricsScope::Nodes, "*", "cluster");
        for i in 0..HISTORY_MAX_IDENTITIES + 10 {
            snapshot.items.extend(
                history_snapshot(
                    MetricsScope::Nodes,
                    "*",
                    &format!("node-{i:03}"),
                    "2026-01-01T00:00:00Z",
                    "1m",
                    "1Mi",
                    None,
                )
                .items,
            );
        }
        history.record("cluster-a", &snapshot, started);
        assert_eq!(history.series.len(), HISTORY_MAX_IDENTITIES);
        assert!(history.limited);
        let identities: Vec<_> = history.series.keys().cloned().collect();
        snapshot.items.reverse();
        for i in 1..90 {
            for row in &mut snapshot.items {
                row.timestamp = format!("2026-01-01T00:{:02}:{:02}Z", i / 60, i % 60);
            }
            history.record("cluster-a", &snapshot, started + Duration::from_secs(i));
        }
        assert_eq!(
            history.series.keys().cloned().collect::<Vec<_>>(),
            identities
        );
        assert!(
            history
                .series
                .values()
                .all(|s| s.points.len() == HISTORY_MAX_SAMPLES)
        );
        let expired = started + HISTORY_RETENTION + Duration::from_secs(90);
        assert!(history.text(expired).contains("No history measurements"));
        history.record_error("cluster-a", MetricsScope::Nodes, "*", expired);
        assert!(history.series.is_empty());
    }
    #[test]
    fn history_resets_context_namespace_scope_source_and_object_generation() {
        let now = Instant::now();
        let mut history = MetricsHistory::default();
        let first = history_snapshot(
            MetricsScope::Pods,
            "demo",
            "api",
            "2026-01-01T00:00:00Z",
            "1m",
            "1Mi",
            Some("uid-1"),
        );
        history.record("cluster-a", &first, now);
        let replaced = history_snapshot(
            MetricsScope::Pods,
            "demo",
            "api",
            "2026-01-01T00:00:01Z",
            "2m",
            "2Mi",
            Some("uid-2"),
        );
        history.record("cluster-a", &replaced, now + Duration::from_secs(1));
        assert_eq!(
            history.series.values().next().map(|s| s.points.len()),
            Some(1)
        );
        assert!(
            !history
                .text(now + Duration::from_secs(1))
                .contains("min 1m")
        );
        history.record("cluster-b", &first, now + Duration::from_secs(2));
        assert_eq!(
            history.series.values().next().map(|s| s.points.len()),
            Some(1)
        );
        let other = history_snapshot(
            MetricsScope::Pods,
            "other",
            "api",
            "2026-01-01T00:00:03Z",
            "3m",
            "3Mi",
            None,
        );
        history.record("cluster-b", &other, now + Duration::from_secs(3));
        assert_eq!(history.series.len(), 1);
        assert!(
            history
                .text(now + Duration::from_secs(3))
                .contains("other/api")
        );
        let mut node = history_snapshot(
            MetricsScope::Nodes,
            "*",
            "node",
            "2026-01-01T00:00:04Z",
            "4m",
            "4Mi",
            None,
        );
        history.record("cluster-b", &node, now + Duration::from_secs(4));
        assert_eq!(history.series.len(), 1);
        assert!(
            !history
                .text(now + Duration::from_secs(4))
                .contains("other/api")
        );
        node.source = "demo (deterministic sample)";
        node.items[0].timestamp = "2026-01-01T00:00:05Z".into();
        history.record("cluster-b", &node, now + Duration::from_secs(5));
        assert_eq!(
            history.series.values().next().map(|s| s.points.len()),
            Some(1)
        );
        // A namespace selection has no bearing on cluster-wide node metrics.
        history.record_error(
            "cluster-b",
            MetricsScope::Nodes,
            "other",
            now + Duration::from_secs(6),
        );
        assert_eq!(history.series.len(), 1);
        history.clear();
        assert!(history.series.is_empty());
    }
    #[test]
    fn history_errors_missing_rows_and_backward_measurements_are_gaps() {
        let now = Instant::now();
        let mut history = MetricsHistory::default();
        let first = history_snapshot(
            MetricsScope::Pods,
            "demo",
            "api",
            "2026-01-01T00:00:01Z",
            "1.000001m",
            "67108865",
            None,
        );
        history.record("cluster-a", &first, now);
        history.record_error(
            "cluster-a",
            MetricsScope::Pods,
            "demo",
            now + Duration::from_secs(1),
        );
        // Recovery still serving the old measurement does not silently fill a gap.
        history.record("cluster-a", &first, now + Duration::from_secs(2));
        let text = history.text(now + Duration::from_secs(2));
        assert!(text.contains("@-"));
        assert!(text.contains("gap; last 1.000001m"));
        assert!(text.contains("gap; last 67108865B"));
        assert!(text.contains("min 1.000001m / max 1.000001m"));
        assert_eq!(
            history.series.values().next().map(|s| s.points.len()),
            Some(2)
        );
        let missing = empty(MetricsScope::Pods, "demo", "cluster");
        history.record("cluster-a", &missing, now + Duration::from_secs(3));
        let older = history_snapshot(
            MetricsScope::Pods,
            "demo",
            "api",
            "2026-01-01T00:00:00Z",
            "999m",
            "999Mi",
            None,
        );
        history.record("cluster-a", &older, now + Duration::from_secs(4));
        assert!(history.text(now + Duration::from_secs(4)).contains("@---"));
        assert!(!history.text(now + Duration::from_secs(4)).contains("999"));
        let recovered = history_snapshot(
            MetricsScope::Pods,
            "demo",
            "api",
            "2026-01-01T00:00:02Z",
            "2m",
            "2Mi",
            None,
        );
        history.record("cluster-a", &recovered, now + Duration::from_secs(5));
        assert!(
            history
                .text(now + Duration::from_secs(5))
                .contains("latest 2m")
        );
        assert!(
            history
                .text(now + Duration::from_secs(5))
                .contains("latest 2Mi")
        );
    }
    #[test]
    fn history_timestamps_are_exact_and_duplicates_do_not_refresh_sample_age() {
        assert_eq!(
            timestamp_key("2026-01-01T00:00:00.10Z"),
            timestamp_key("2026-01-01T01:00:00.1+01:00")
        );
        assert!(
            timestamp_key("2026-01-01T00:00:00.10000000000000000001Z")
                > timestamp_key("2026-01-01T00:00:00.1Z")
        );
        let now = Instant::now();
        let mut history = MetricsHistory::default();
        let first = history_snapshot(
            MetricsScope::Nodes,
            "*",
            "node",
            "2026-01-01T00:00:00Z",
            "0",
            "0",
            None,
        );
        history.record("cluster-a", &first, now);
        history.record("cluster-a", &first, now + Duration::from_secs(30));
        assert_eq!(
            history.series.values().next().map(|s| s.points.len()),
            Some(1)
        );
        assert!(history.text(now).contains("|."));
        assert!(!history.text(now).contains("gap; last"));
        let after_retention = now + HISTORY_RETENTION + Duration::from_secs(1);
        history.record("cluster-a", &first, after_retention);
        assert!(
            history
                .text(after_retention)
                .contains("No history measurements")
        );
        // A local result received before the most recent observation is ignored.
        history.record("cluster-a", &first, now);
        assert!(
            history
                .text(after_retention)
                .contains("No history measurements")
        );
    }
    #[test]
    fn history_evicts_absent_identities_and_keeps_display_and_json_projection_safe() {
        let now = Instant::now();
        let mut history = MetricsHistory::default();
        let mut many = empty(MetricsScope::Nodes, "*", "cluster");
        for i in 0..HISTORY_MAX_IDENTITIES {
            many.items.extend(
                history_snapshot(
                    MetricsScope::Nodes,
                    "*",
                    &format!("node-{i}"),
                    "2026-01-01T00:00:00Z",
                    "1m",
                    "1Mi",
                    None,
                )
                .items,
            );
        }
        history.record("cluster-a", &many, now);
        let next = history_snapshot(
            MetricsScope::Nodes,
            "*",
            "replacement",
            "2026-01-01T00:00:01Z",
            "18446744073.709551615",
            "18446744073709551615",
            Some("CANARY-UID"),
        );
        history.record("cluster-a", &next, now + Duration::from_secs(1));
        assert_eq!(history.series.len(), HISTORY_MAX_IDENTITIES);
        assert!(
            history
                .text(now + Duration::from_secs(1))
                .contains("replacement")
        );
        assert!(
            history
                .text(now + Duration::from_secs(1))
                .contains("18446744073709.551615m")
        );
        let machine = serde_json::to_string(&next).unwrap_or_else(|e| panic!("{e}"));
        assert!(!machine.contains("identity_uid"));
        assert!(!machine.contains("CANARY-UID"));
        let mut unsafe_snapshot = next;
        unsafe_snapshot.items[0].name = "node\x1b[2J".into();
        history.clear();
        history.record("cluster-a", &unsafe_snapshot, now);
        assert!(history.series.is_empty());
        assert!(!history.text(now).contains('\x1b'));
        assert_eq!(format_memory(67_108_864), "64Mi");
        assert_eq!(format_memory(1_610_612_736), "1536Mi");
        assert_eq!(format_memory(1), "1B");
    }
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
    fn demo_steps_advance_and_object_samples_sum_containers_with_gaps() -> Result<()> {
        assert_eq!(
            serde_json::to_string(&demo_step(MetricsScope::Pods, "demo", 0)?).ok(),
            serde_json::to_string(&demo(MetricsScope::Pods, "demo")?).ok()
        );
        assert_eq!(
            demo(MetricsScope::Pods, "demo")?.items[0].timestamp,
            "2026-10-10T07:00:30Z"
        );
        let now = Instant::now();
        let mut history = MetricsHistory::default();
        for step in 0..4u64 {
            let mut snapshot = demo_step(MetricsScope::Pods, "demo", step)?;
            let mut sidecar = demo_step(MetricsScope::Pods, "demo", step)?.items.remove(0);
            sidecar.container = Some("sidecar".into());
            sidecar.cpu_nanocores = 1_000_000;
            if step == 2 {
                // A sample missing one container is a gap, never a partial sum.
                snapshot.items[0].timestamp = "2026-10-10T07:00:00Z".into();
            }
            snapshot.items.push(sidecar);
            history.record("ctx", &snapshot, now + Duration::from_secs(step * 5));
        }
        history.record_error(
            "ctx",
            MetricsScope::Pods,
            "demo",
            now + Duration::from_secs(20),
        );
        let samples =
            history.object_samples(Some("demo"), "api-7d4f9", now + Duration::from_secs(20));
        let cpu: Vec<_> = samples.iter().map(|s| s.usage.map(|u| u.0)).collect();
        assert_eq!(
            cpu,
            vec![
                Some(126_000_000),
                Some(141_000_000),
                None,
                Some(176_000_000),
                None
            ]
        );
        assert_eq!(samples[0].age, Duration::from_secs(20));
        assert!(history.object_samples(None, "api-7d4f9", now).is_empty());
        Ok(())
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
    fn optional_resource_reads_are_explicit_and_forbidden_or_invalid_denominators_retain_usage() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap_or_else(|error| panic!("{error}"));
        runtime.block_on(async {
            let usage = page(MetricsScope::Pods, vec![sample(MetricsScope::Pods, "sample")], "", "1");
            let resource = json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":[{"metadata":{"name":"sample","namespace":"demo","uid":"resource-uid","creationTimestamp":"2025-12-31T23:59:00Z"},"spec":{"containers":[{"name":"api","resources":{"requests":{"cpu":"125m","memory":"1Mi"},"limits":{"cpu":"1","memory":"2Mi"}}}]}}]});
            for response in [
                (200, resource.clone()),
                (403, json!({"apiVersion":"v1","kind":"Status","status":"Failure","reason":"Forbidden","message":"CANARY credential detail","code":403})),
                (200, json!({"apiVersion":"v1","kind":"PodList","metadata":{},"items":[{"metadata":{"name":"sample","namespace":"other"}}]})),
            ] {
                let (api, requests) = mock_api(MetricsScope::Pods, "demo", vec![(200, usage.clone()), response]);
                let gateway = Gateway::for_test_client(api.into_client(), "demo");
                let snapshot = collect_with_allocations(&gateway, MetricsScope::Pods, "demo").await.unwrap_or_else(|error| panic!("{error}"));
                assert_eq!(snapshot.items.len(), 1);
                assert_eq!(snapshot.items[0].cpu_nanocores, 1_000_000);
                assert_eq!(snapshot.items[0].memory_bytes, 1_048_576);
                assert!(snapshot.allocation_status.is_some());
                assert!(!snapshot.text().contains("CANARY"));
                let paths = requests.lock().unwrap_or_else(|error| panic!("{error}"));
                assert_eq!(paths.len(), 2);
                assert!(paths[0].starts_with("/apis/metrics.k8s.io/v1beta1/namespaces/demo/pods"));
                assert!(paths[1].starts_with("/api/v1/namespaces/demo/pods"));
            }
            let (api, requests) = mock_api(MetricsScope::Pods, "demo", vec![(200, usage)]);
            let gateway = Gateway::for_test_client(api.into_client(), "demo");
            let snapshot = collect(&gateway, MetricsScope::Pods, "demo").await.unwrap_or_else(|error| panic!("{error}"));
            assert!(snapshot.allocation_status.is_none() && snapshot.items[0].allocations.is_none());
            assert_eq!(requests.lock().unwrap_or_else(|error| panic!("{error}")).len(), 1);
        });
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
