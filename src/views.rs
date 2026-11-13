//! k9s-compatible data-only `views.yaml`: per-resource table columns and
//! default sort. Custom columns read a JSONPath-like field path; nothing is
//! evaluated as code.
use crate::{Result, safety};
use serde::Deserialize;
use serde_json::Value;
use std::collections::BTreeMap;

const MAX_VIEWS: usize = 128;
const MAX_COLUMNS: usize = 24;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    views: BTreeMap<String, Entry>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Entry {
    #[serde(default)]
    columns: Vec<String>,
    #[serde(default)]
    sort_column: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Segment {
    Field(String),
    Index(usize),
    All,
}

/// A predefined column whose value is worked out from the object (and, for
/// usage columns, the table's metrics sample) instead of read from one path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Computed {
    /// Node roles from the `node-role.kubernetes.io/*` labels.
    Role,
    /// Number of Node taints.
    Taints,
    /// Node address of type InternalIP / ExternalIP.
    InternalIp,
    ExternalIp,
    /// Node usage as a percentage of the Node's own allocatable.
    PercentCpu,
    PercentMemory,
    /// Node allocatable CPU in millicores / memory in Mi.
    CpuAllocatable,
    MemoryAllocatable,
    /// Pod usage as a percentage of its containers' summed requests / limits.
    PercentCpuRequest,
    PercentCpuLimit,
    PercentMemoryRequest,
    PercentMemoryLimit,
    /// Number of data keys in a ConfigMap or Secret.
    DataKeys,
}

/// Predefined columns usable by bare title in views.yaml, beyond the table's
/// own NAMESPACE, NAME, READY, STATUS, RSTS, AGE, NODE, IP, CPU, MEM, LABELS
/// and IMAGES: (title, value, width, numeric, what it shows).
enum Source {
    Path(&'static str),
    Computed(Computed),
}
const PREDEFINED: &[(&str, Source, u8, bool, &str)] = &[
    // Nodes (the k9s node columns).
    (
        "ROLE",
        Source::Computed(Computed::Role),
        14,
        false,
        "Node roles",
    ),
    (
        "ARCH",
        Source::Path(".status.nodeInfo.architecture"),
        6,
        false,
        "Node CPU architecture",
    ),
    (
        "TAINTS",
        Source::Computed(Computed::Taints),
        6,
        true,
        "Number of Node taints",
    ),
    (
        "VERSION",
        Source::Path(".status.nodeInfo.kubeletVersion"),
        15,
        false,
        "Kubelet version",
    ),
    (
        "OS-IMAGE",
        Source::Path(".status.nodeInfo.osImage"),
        26,
        false,
        "Node operating system",
    ),
    (
        "KERNEL",
        Source::Path(".status.nodeInfo.kernelVersion"),
        24,
        false,
        "Node kernel version",
    ),
    (
        "RUNTIME",
        Source::Path(".status.nodeInfo.containerRuntimeVersion"),
        22,
        false,
        "Container runtime",
    ),
    (
        "INTERNAL-IP",
        Source::Computed(Computed::InternalIp),
        15,
        false,
        "Node internal address",
    ),
    (
        "EXTERNAL-IP",
        Source::Computed(Computed::ExternalIp),
        15,
        false,
        "Node external address",
    ),
    (
        "PODS-MAX",
        Source::Path(".status.allocatable.pods"),
        8,
        true,
        "Pods the Node can run (allocatable)",
    ),
    (
        "%CPU",
        Source::Computed(Computed::PercentCpu),
        5,
        true,
        "Node CPU usage / allocatable",
    ),
    (
        "%MEM",
        Source::Computed(Computed::PercentMemory),
        5,
        true,
        "Node memory usage / allocatable",
    ),
    (
        "CPU/A",
        Source::Computed(Computed::CpuAllocatable),
        7,
        true,
        "Node allocatable CPU (m)",
    ),
    (
        "MEM/A",
        Source::Computed(Computed::MemoryAllocatable),
        8,
        true,
        "Node allocatable memory (Mi)",
    ),
    // Pods.
    (
        "%CPU/R",
        Source::Computed(Computed::PercentCpuRequest),
        6,
        true,
        "Pod CPU usage / requests",
    ),
    (
        "%CPU/L",
        Source::Computed(Computed::PercentCpuLimit),
        6,
        true,
        "Pod CPU usage / limits",
    ),
    (
        "%MEM/R",
        Source::Computed(Computed::PercentMemoryRequest),
        6,
        true,
        "Pod memory usage / requests",
    ),
    (
        "%MEM/L",
        Source::Computed(Computed::PercentMemoryLimit),
        6,
        true,
        "Pod memory usage / limits",
    ),
    (
        "QOS",
        Source::Path(".status.qosClass"),
        10,
        false,
        "Pod QoS class",
    ),
    (
        "SA",
        Source::Path(".spec.serviceAccountName"),
        20,
        false,
        "Pod service account",
    ),
    (
        "HOST-IP",
        Source::Path(".status.hostIP"),
        15,
        false,
        "IP of the Pod's Node",
    ),
    // Workloads.
    (
        "UP-TO-DATE",
        Source::Path(".status.updatedReplicas"),
        10,
        true,
        "Updated replicas",
    ),
    (
        "AVAILABLE",
        Source::Path(".status.availableReplicas"),
        9,
        true,
        "Available replicas",
    ),
    (
        "DESIRED",
        Source::Path(".status.desiredNumberScheduled"),
        7,
        true,
        "DaemonSet Pods wanted",
    ),
    (
        "CURRENT",
        Source::Path(".status.currentNumberScheduled"),
        7,
        true,
        "DaemonSet Pods scheduled",
    ),
    (
        "COMPLETIONS",
        Source::Path(".spec.completions"),
        11,
        true,
        "Job completions wanted",
    ),
    (
        "SUCCEEDED",
        Source::Path(".status.succeeded"),
        9,
        true,
        "Job Pods succeeded",
    ),
    (
        "SCHEDULE",
        Source::Path(".spec.schedule"),
        16,
        false,
        "CronJob schedule",
    ),
    (
        "SUSPEND",
        Source::Path(".spec.suspend"),
        7,
        false,
        "CronJob suspended",
    ),
    (
        "LAST-SCHEDULE",
        Source::Path(".status.lastScheduleTime"),
        13,
        false,
        "CronJob last run (add |T for an age)",
    ),
    // Services, storage and configuration.
    (
        "TYPE",
        Source::Path(".spec.type"),
        13,
        false,
        "Service type",
    ),
    (
        "CLUSTER-IP",
        Source::Path(".spec.clusterIP"),
        15,
        false,
        "Service cluster IP",
    ),
    (
        "PORTS",
        Source::Path(".spec.ports[*].port"),
        16,
        false,
        "Service ports",
    ),
    (
        "CAPACITY",
        Source::Path(".spec.resources.requests.storage"),
        9,
        false,
        "PersistentVolumeClaim size",
    ),
    (
        "VOLUME",
        Source::Path(".spec.volumeName"),
        24,
        false,
        "Bound PersistentVolume",
    ),
    (
        "STORAGECLASS",
        Source::Path(".spec.storageClassName"),
        16,
        false,
        "Storage class",
    ),
    (
        "DATA",
        Source::Computed(Computed::DataKeys),
        5,
        true,
        "Number of keys in a ConfigMap",
    ),
    (
        "CLASS",
        Source::Path(".spec.ingressClassName"),
        12,
        false,
        "Ingress class",
    ),
    (
        "HOSTS",
        Source::Path(".spec.rules[*].host"),
        30,
        false,
        "Ingress hosts",
    ),
];

/// Titles and descriptions of every predefined column, for `:columns`.
pub fn predefined() -> impl Iterator<Item = (&'static str, &'static str)> {
    PREDEFINED
        .iter()
        .map(|(title, _, _, _, about)| (*title, *about))
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ColumnSpec {
    pub title: String,
    /// `None` for a built-in or computed column named by `title`.
    pub path: Option<Vec<Segment>>,
    /// A predefined computed column.
    pub computed: Option<Computed>,
    /// Column width in cells; `None` sizes it from the title.
    pub width: Option<u8>,
    /// `|W`: shown only in wide mode (Ctrl-W).
    pub wide: bool,
    /// `|H`: hidden.
    pub hidden: bool,
    /// `|N`: sort numerically.
    pub numeric: bool,
    /// `|T`: the value is a timestamp shown as an age.
    pub time: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct View {
    pub columns: Vec<ColumnSpec>,
    /// Default sort: column title and descending flag.
    pub sort: Option<(String, bool)>,
}
pub type Views = BTreeMap<String, View>;

fn parse_path(text: &str) -> Result<Vec<Segment>> {
    let invalid = || format!("invalid column path {:?}", safety::safe_text(text, 120));
    let mut chars = text
        .strip_prefix('.')
        .ok_or_else(invalid)?
        .chars()
        .peekable();
    let mut segments = Vec::new();
    let mut field = String::new();
    while let Some(c) = chars.next() {
        match c {
            '\\' => field.push(chars.next().ok_or_else(invalid)?),
            '.' => {
                if !field.is_empty() {
                    segments.push(Segment::Field(std::mem::take(&mut field)));
                }
            }
            '[' => {
                if !field.is_empty() {
                    segments.push(Segment::Field(std::mem::take(&mut field)));
                }
                let mut inner = String::new();
                for c in chars.by_ref() {
                    if c == ']' {
                        break;
                    }
                    inner.push(c);
                }
                let inner = inner.trim();
                segments.push(if inner == "*" {
                    Segment::All
                } else if let Some(quoted) = inner
                    .strip_prefix('\'')
                    .and_then(|s| s.strip_suffix('\''))
                    .or_else(|| inner.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
                {
                    Segment::Field(quoted.into())
                } else {
                    Segment::Index(inner.parse().map_err(|_| invalid())?)
                });
            }
            c => field.push(c),
        }
    }
    if !field.is_empty() {
        segments.push(Segment::Field(field));
    }
    if segments.is_empty() || segments.len() > 16 {
        return Err(invalid());
    }
    Ok(segments)
}

/// True when `path` reads only what metadata-projected list rows keep
/// (kind, apiVersion and metadata name, namespace, uid, resourceVersion,
/// creation/deletion timestamps and labels).
pub fn metadata_path(path: &[Segment]) -> bool {
    let field = |index: usize| match path.get(index) {
        Some(Segment::Field(name)) => Some(name.as_str()),
        _ => None,
    };
    match field(0) {
        Some("kind" | "apiVersion") => path.len() == 1,
        Some("metadata") => matches!(
            field(1),
            Some(
                "name"
                    | "namespace"
                    | "uid"
                    | "resourceVersion"
                    | "creationTimestamp"
                    | "deletionTimestamp"
                    | "labels"
            )
        ),
        _ => false,
    }
}

/// Values at `path`; `[*]` fans out over arrays.
pub fn eval(path: &[Segment], value: &Value) -> Vec<String> {
    let mut current = vec![value];
    for segment in path {
        current = current
            .into_iter()
            .flat_map(|value| match segment {
                Segment::Field(name) => value.get(name).into_iter().collect::<Vec<_>>(),
                Segment::Index(index) => value.get(*index).into_iter().collect(),
                Segment::All => value
                    .as_array()
                    .map(|a| a.iter().collect())
                    .unwrap_or_default(),
            })
            .collect();
    }
    current
        .into_iter()
        .filter(|value| !value.is_null())
        .map(|value| match value {
            Value::String(text) => text.clone(),
            Value::Object(map) => map
                .iter()
                .map(|(k, v)| {
                    format!(
                        "{k}={}",
                        v.as_str().map_or_else(|| v.to_string(), str::to_owned)
                    )
                })
                .collect::<Vec<_>>()
                .join(","),
            other => other.to_string(),
        })
        .collect()
}

fn parse_column(text: &str) -> Result<ColumnSpec> {
    let (main, attributes) = text.split_once('|').unwrap_or((text, ""));
    let (title, path) = match main.split_once(':') {
        Some((title, path)) => (title.trim(), Some(parse_path(path.trim())?)),
        None => (main.trim(), None),
    };
    if title.is_empty()
        || title.len() > 32
        || !title
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_%()/".contains(&b))
    {
        return Err(format!(
            "column titles are 1..32 letters, digits or -_%()/: {:?}",
            safety::safe_text(title, 40)
        ));
    }
    let attributes = attributes.to_ascii_uppercase();
    let title = title.to_ascii_uppercase();
    // A bare title that names a predefined column takes its value from it.
    let known = PREDEFINED
        .iter()
        .find(|(name, ..)| path.is_none() && *name == title);
    let (path, computed, width, numeric) = match known {
        Some((_, Source::Path(text), width, numeric, _)) => {
            (Some(parse_path(text)?), None, Some(*width), *numeric)
        }
        Some((_, Source::Computed(computed), width, numeric, _)) => {
            (None, Some(*computed), Some(*width), *numeric)
        }
        None => (path, None, None, false),
    };
    Ok(ColumnSpec {
        title,
        path,
        computed,
        width,
        wide: attributes.contains('W'),
        hidden: attributes.contains('H'),
        numeric: numeric || attributes.contains('N'),
        time: attributes.contains('T'),
    })
}

fn parse_sort(text: &str) -> Result<(String, bool)> {
    let (title, order) = text.split_once(':').unwrap_or((text, "asc"));
    let descending = match order.trim().to_ascii_lowercase().as_str() {
        "asc" => false,
        "desc" => true,
        _ => return Err("sortColumn order must be asc or desc".into()),
    };
    Ok((title.trim().to_ascii_uppercase(), descending))
}

pub fn parse(text: &str) -> Result<Views> {
    if text.len() > 64 * 1024 {
        return Err("views.yaml exceeds 64 KiB".into());
    }
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_reader_input_bytes: Some(64*1024), max_events: 20_000, max_aliases: 0, max_anchors: 0,
            max_depth: 5, max_inclusion_depth: 0, max_documents: 1, max_nodes: 8192,
            max_total_scalar_bytes: 64*1024, max_merge_keys: 0,
        },
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys: serde_saphyr::MergeKeyPolicy::Error,
        with_snippet: false,
    };
    let file: File = serde_saphyr::from_str_with_options(text, options).map_err(|error| {
        format!(
            "invalid views.yaml{}; expected views: RESOURCE: {{columns: [...], sortColumn: NAME:asc}}",
            safety::yaml_location(&error)
        )
    })?;
    if file.views.len() > MAX_VIEWS {
        return Err(format!("views.yaml exceeds {MAX_VIEWS} views"));
    }
    let mut views = Views::new();
    for (resource, entry) in file.views {
        if entry.columns.len() > MAX_COLUMNS {
            return Err(format!("a view has more than {MAX_COLUMNS} columns"));
        }
        let view = View {
            columns: entry
                .columns
                .iter()
                .map(|column| parse_column(column))
                .collect::<Result<_>>()?,
            sort: entry.sort_column.as_deref().map(parse_sort).transpose()?,
        };
        views.insert(resource.trim().to_ascii_lowercase(), view);
    }
    Ok(views)
}

pub fn load() -> Result<Views> {
    load_from(&safety::config_dir()?.join("views.yaml"))
}

/// The columns KBorg shows without a views.yaml, in views.yaml's own format:
/// the k9s columns for the kinds listed. `kborg views init` writes this text
/// as a starting file. Pods keep the table's own width-aware default.
pub const BUILTIN: &str = "\
# KBorg table columns (k9s views.yaml format). Edit, save, and a running KBorg
# applies the file within a second. Delete a resource to get its default back.
#
# A column is a predefined name, or TITLE:.json.path for any field. Attributes
# follow a |: W shown only in wide mode (Ctrl-W), H hidden, N numeric sort,
# T timestamp shown as an age. Example: ZONE:.metadata.labels['topology.kubernetes.io/zone']|W
#
# Predefined names: NAMESPACE NAME READY STATUS RSTS AGE NODE IP CPU MEM LABELS IMAGES
#   Nodes: ROLE ARCH TAINTS VERSION OS-IMAGE KERNEL RUNTIME INTERNAL-IP EXTERNAL-IP
#          PODS-MAX %CPU %MEM CPU/A MEM/A
#   Pods: %CPU/R %CPU/L %MEM/R %MEM/L QOS SA HOST-IP
#   Workloads: UP-TO-DATE AVAILABLE DESIRED CURRENT COMPLETIONS SUCCEEDED SCHEDULE SUSPEND LAST-SCHEDULE
#   Other: TYPE CLUSTER-IP PORTS CAPACITY VOLUME STORAGECLASS DATA CLASS HOSTS
# :columns in KBorg lists them with what each shows.
views:
  v1/nodes:
    columns: [NAME, STATUS, ROLE, ARCH, TAINTS, VERSION, CPU, '%CPU', CPU/A, MEM, '%MEM', MEM/A, 'OS-IMAGE|W', 'KERNEL|W', 'INTERNAL-IP|W', 'EXTERNAL-IP|W', 'LABELS|W', AGE]
  apps/v1/deployments:
    columns: [NAMESPACE, NAME, READY, UP-TO-DATE, AVAILABLE, STATUS, 'IMAGES|W', 'LABELS|W', AGE]
  apps/v1/statefulsets:
    columns: [NAMESPACE, NAME, READY, STATUS, 'IMAGES|W', 'LABELS|W', AGE]
  apps/v1/daemonsets:
    columns: [NAMESPACE, NAME, DESIRED, CURRENT, READY, STATUS, 'IMAGES|W', 'LABELS|W', AGE]
  v1/services:
    columns: [NAMESPACE, NAME, TYPE, CLUSTER-IP, PORTS, 'LABELS|W', AGE]
  batch/v1/jobs:
    columns: [NAMESPACE, NAME, COMPLETIONS, SUCCEEDED, STATUS, 'IMAGES|W', AGE]
  batch/v1/cronjobs:
    columns: [NAMESPACE, NAME, SCHEDULE, SUSPEND, 'LAST-SCHEDULE|T', STATUS, AGE]
  v1/persistentvolumeclaims:
    columns: [NAMESPACE, NAME, STATUS, VOLUME, CAPACITY, STORAGECLASS, AGE]
  networking.k8s.io/v1/ingresses:
    columns: [NAMESPACE, NAME, CLASS, HOSTS, AGE]
";

/// The built-in views, parsed once.
pub fn builtin() -> &'static Views {
    static VIEWS: std::sync::OnceLock<Views> = std::sync::OnceLock::new();
    VIEWS.get_or_init(|| parse(BUILTIN).unwrap_or_default())
}

/// Write the built-in columns as a starting views.yaml; never replaces a file.
pub fn init() -> Result<std::path::PathBuf> {
    let path = safety::config_dir()?.join("views.yaml");
    safety::write_new(&path, BUILTIN.as_bytes())
        .map_err(|_| format!("{} already exists or cannot be created", path.display()))?;
    Ok(path)
}

fn quantity(value: &Value, parse: fn(&str) -> Result<(u64, bool)>) -> Option<u64> {
    parse(value.as_str()?).ok().map(|(amount, _)| amount)
}

/// Sum of a resource across a Pod's ordinary containers; `None` when any
/// container leaves it unset, as a percentage of a partial sum would mislead.
fn pod_total(object: &Value, bound: &str, resource: &str) -> Option<u64> {
    let parse = if resource == "cpu" {
        crate::metrics::normalize_cpu_quantity
    } else {
        crate::metrics::normalize_memory_quantity
    };
    let containers = object["spec"]["containers"].as_array()?;
    if containers.is_empty() {
        return None;
    }
    containers.iter().try_fold(0u64, |total, container| {
        total.checked_add(quantity(&container["resources"][bound][resource], parse)?)
    })
}

fn percent(used: Option<u64>, of: Option<u64>) -> String {
    match (used, of) {
        (Some(used), Some(of)) if of > 0 => ((u128::from(used) * 100) / u128::from(of)).to_string(),
        _ => "n/a".into(),
    }
}

/// The value of a computed column. `usage` is the object's metrics sample as
/// (CPU nanocores, memory bytes), when the table has one.
pub fn computed_value(computed: Computed, object: &Value, usage: Option<(u64, u64)>) -> String {
    let cpu = usage.map(|(cpu, _)| cpu);
    let memory = usage.map(|(_, memory)| memory);
    let allocatable = |resource: &str| {
        quantity(
            &object["status"]["allocatable"][resource],
            if resource == "cpu" {
                crate::metrics::normalize_cpu_quantity
            } else {
                crate::metrics::normalize_memory_quantity
            },
        )
    };
    let address = |kind: &str| {
        object["status"]["addresses"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|address| address["type"] == kind)
            .and_then(|address| address["address"].as_str())
            .unwrap_or("-")
            .to_owned()
    };
    match computed {
        Computed::Role => {
            let mut roles: Vec<&str> = object["metadata"]["labels"]
                .as_object()
                .into_iter()
                .flatten()
                .filter_map(|(key, value)| {
                    key.strip_prefix("node-role.kubernetes.io/").or_else(|| {
                        (key == "kubernetes.io/role")
                            .then(|| value.as_str())
                            .flatten()
                    })
                })
                .filter(|role| !role.is_empty())
                .collect();
            roles.sort_unstable();
            roles.dedup();
            if roles.is_empty() {
                "-".into()
            } else {
                roles.join(",")
            }
        }
        Computed::Taints => object["spec"]["taints"]
            .as_array()
            .map_or(0, Vec::len)
            .to_string(),
        Computed::InternalIp => address("InternalIP"),
        Computed::ExternalIp => address("ExternalIP"),
        Computed::PercentCpu => percent(cpu, allocatable("cpu")),
        Computed::PercentMemory => percent(memory, allocatable("memory")),
        Computed::CpuAllocatable => {
            allocatable("cpu").map_or_else(|| "-".into(), |nano| (nano / 1_000_000).to_string())
        }
        Computed::MemoryAllocatable => allocatable("memory")
            .map_or_else(|| "-".into(), |bytes| (bytes / (1024 * 1024)).to_string()),
        Computed::PercentCpuRequest => percent(cpu, pod_total(object, "requests", "cpu")),
        Computed::PercentCpuLimit => percent(cpu, pod_total(object, "limits", "cpu")),
        Computed::PercentMemoryRequest => percent(memory, pod_total(object, "requests", "memory")),
        Computed::PercentMemoryLimit => percent(memory, pod_total(object, "limits", "memory")),
        Computed::DataKeys => {
            let count = |field: &str| object[field].as_object().map_or(0, serde_json::Map::len);
            (count("data") + count("binaryData")).to_string()
        }
    }
}

/// Load from an explicit path (live reload watches the config folder).
pub fn load_from(path: &std::path::Path) -> Result<Views> {
    safety::checked_path(path)?;
    if !path.exists() {
        return Ok(Views::new());
    }
    let bytes = safety::read_small(path, 64 * 1024)?;
    parse(std::str::from_utf8(&bytes).map_err(|_| "views.yaml must be UTF-8")?)
}

/// The view for a resource under any of its k9s names: `v1/pods`,
/// `apps/v1/deployments`, `deployments.apps`, `deployments`.
pub fn find<'a>(views: &'a Views, group: &str, version: &str, plural: &str) -> Option<&'a View> {
    let qualified = if group.is_empty() {
        format!("{version}/{plural}")
    } else {
        format!("{group}/{version}/{plural}")
    };
    let dotted = if group.is_empty() {
        plural.to_owned()
    } else {
        format!("{plural}.{group}")
    };
    [qualified, dotted, plural.to_owned()]
        .iter()
        .find_map(|key| views.get(key))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn k9s_views_parse_columns_paths_attributes_and_sort() -> Result<()> {
        let views = parse(
            "views:\n  v1/pods:\n    sortColumn: AGE:desc\n    columns:\n      - NAMESPACE\n      - NAME\n      - STATUS\n      - IMAGE:.spec.containers[0].image\n      - APP:.metadata.labels['app.kubernetes.io/name']|W\n      - PORTS:.spec.containers[*].ports[*].containerPort|N\n      - AGE\n",
        )?;
        let view = find(&views, "", "v1", "pods").ok_or("view")?;
        assert_eq!(view.sort, Some(("AGE".into(), true)));
        assert_eq!(view.columns.len(), 7);
        assert!(view.columns[4].wide && view.columns[5].numeric);
        let pod = json!({"metadata":{"labels":{"app.kubernetes.io/name":"api"}},"spec":{"containers":[{"image":"api:1","ports":[{"containerPort":80},{"containerPort":9090}]},{"image":"side:2"}]}});
        assert_eq!(
            eval(view.columns[3].path.as_deref().ok_or("path")?, &pod),
            vec!["api:1"]
        );
        assert_eq!(
            eval(view.columns[4].path.as_deref().ok_or("path")?, &pod),
            vec!["api"]
        );
        assert_eq!(
            eval(view.columns[5].path.as_deref().ok_or("path")?, &pod),
            vec!["80", "9090"]
        );
        assert!(find(&views, "apps", "v1", "deployments").is_none());
        for bad in [
            "views:\n  pods:\n    columns: [\"BAD TITLE\"]\n",
            "views:\n  pods:\n    columns: [\"X:spec\"]\n",
            "views:\n  pods:\n    sortColumn: AGE:sideways\n",
            "views:\n  pods:\n    columns: [NAME]\n    script: rm\n",
        ] {
            assert!(parse(bad).is_err(), "{bad}");
        }
        Ok(())
    }
    #[test]
    fn metadata_paths_are_only_fields_metadata_rows_keep() -> Result<()> {
        for path in [
            ".metadata.name",
            ".metadata.labels.team",
            ".metadata.labels['app.kubernetes.io/name']",
            ".metadata.creationTimestamp",
            ".kind",
        ] {
            assert!(metadata_path(&parse_path(path)?), "{path}");
        }
        for path in [
            ".data.key",
            ".spec.containers[0].image",
            ".metadata.annotations.owner",
            ".metadata.ownerReferences[0].name",
            ".metadata",
            ".kind.extra",
        ] {
            assert!(!metadata_path(&parse_path(path)?), "{path}");
        }
        Ok(())
    }
    #[test]
    fn predefined_columns_resolve_by_name_and_compute_their_values() -> Result<()> {
        let views = parse(
            "views:\n  v1/nodes:\n    columns: [NAME, ROLE, VERSION, '%CPU', 'OS-IMAGE|W', UNKNOWN-NAME, ZONE:.metadata.labels.zone]\n",
        )?;
        let columns = &views["v1/nodes"].columns;
        assert_eq!(columns[1].computed, Some(Computed::Role));
        assert_eq!(columns[1].width, Some(14));
        assert!(columns[2].path.is_some() && columns[2].computed.is_none());
        assert!(
            columns[3].numeric,
            "predefined numeric columns sort as numbers"
        );
        assert!(columns[4].wide && columns[4].path.is_some());
        // An unknown bare name stays a (hidden) built-in lookup, as before.
        assert!(columns[5].path.is_none() && columns[5].computed.is_none());
        assert!(
            columns[6].width.is_none(),
            "custom paths size from their title"
        );

        let node = json!({"metadata":{"labels":{"node-role.kubernetes.io/worker":"","kubernetes.io/role":"agent"}},"spec":{"taints":[{"key":"x"}]},"status":{"allocatable":{"cpu":"2","memory":"4Gi"},"addresses":[{"type":"ExternalIP","address":"203.0.113.9"}]}});
        let usage = Some((500_000_000, 1024 * 1024 * 1024));
        assert_eq!(computed_value(Computed::Role, &node, None), "agent,worker");
        assert_eq!(computed_value(Computed::Taints, &node, None), "1");
        assert_eq!(
            computed_value(Computed::ExternalIp, &node, None),
            "203.0.113.9"
        );
        assert_eq!(computed_value(Computed::InternalIp, &node, None), "-");
        assert_eq!(computed_value(Computed::PercentCpu, &node, usage), "25");
        assert_eq!(computed_value(Computed::PercentMemory, &node, usage), "25");
        assert_eq!(computed_value(Computed::PercentCpu, &node, None), "n/a");
        assert_eq!(
            computed_value(Computed::CpuAllocatable, &node, None),
            "2000"
        );
        assert_eq!(
            computed_value(Computed::MemoryAllocatable, &node, None),
            "4096"
        );
        assert_eq!(computed_value(Computed::Role, &json!({}), None), "-");

        let pod = json!({"spec":{"containers":[
            {"resources":{"requests":{"cpu":"250m","memory":"256Mi"},"limits":{"cpu":"1"}}},
            {"resources":{"requests":{"cpu":"250m","memory":"256Mi"},"limits":{"cpu":"1"}}}]}});
        assert_eq!(
            computed_value(Computed::PercentCpuRequest, &pod, usage),
            "100"
        );
        assert_eq!(computed_value(Computed::PercentCpuLimit, &pod, usage), "25");
        assert_eq!(
            computed_value(Computed::PercentMemoryRequest, &pod, usage),
            "200"
        );
        // One container without a memory limit: no honest percentage.
        assert_eq!(
            computed_value(Computed::PercentMemoryLimit, &pod, usage),
            "n/a"
        );
        assert_eq!(
            computed_value(Computed::DataKeys, &json!({"data":{"a":"1","b":"2"}}), None),
            "2"
        );
        Ok(())
    }
    #[test]
    fn built_in_views_parse_and_name_only_known_columns() -> Result<()> {
        let views = parse(BUILTIN)?;
        assert_eq!(&views, builtin());
        let nodes = find(&views, "", "v1", "nodes").ok_or("nodes view")?;
        for title in [
            "ROLE",
            "ARCH",
            "TAINTS",
            "VERSION",
            "%CPU",
            "CPU/A",
            "%MEM",
            "MEM/A",
            "OS-IMAGE",
            "KERNEL",
            "INTERNAL-IP",
            "EXTERNAL-IP",
        ] {
            assert!(
                nodes.columns.iter().any(|spec| spec.title == title),
                "{title}"
            );
        }
        const TABLE: [&str; 12] = [
            "NAMESPACE",
            "NAME",
            "READY",
            "STATUS",
            "RSTS",
            "AGE",
            "NODE",
            "IP",
            "CPU",
            "MEM",
            "LABELS",
            "IMAGES",
        ];
        for (resource, view) in &views {
            for spec in &view.columns {
                assert!(
                    spec.path.is_some()
                        || spec.computed.is_some()
                        || TABLE.contains(&spec.title.as_str()),
                    "{resource}: {} is not a known column",
                    spec.title
                );
            }
        }
        // ConfigMaps keep their metadata-only rows: no built-in view reads their data.
        assert!(find(&views, "", "v1", "configmaps").is_none());
        assert!(predefined().count() >= 35);
        Ok(())
    }
}
