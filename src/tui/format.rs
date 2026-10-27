//! Object formatting, table columns and layout helpers.
use super::*;

pub(super) fn name(object: &Value) -> &str {
    object["metadata"]["name"].as_str().unwrap_or("-")
}
pub(super) fn pod_container_names(object: &Value) -> Vec<String> {
    ["containers", "initContainers", "ephemeralContainers"]
        .iter()
        .flat_map(|field| object["spec"][field].as_array().into_iter().flatten())
        .filter_map(|container| container["name"].as_str().map(str::to_owned))
        .take(128)
        .collect()
}
pub(super) fn supports_group_logs(resource: &str) -> bool {
    [
        "deployments.apps",
        "statefulsets.apps",
        "daemonsets.apps",
        "replicasets.apps",
        "jobs.batch",
        "cronjobs.batch",
        "services",
        "replicationcontrollers",
    ]
    .contains(&resource)
}
pub(super) fn ordinary_container_names(object: &Value) -> Vec<String> {
    object["spec"]["containers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|container| container["name"].as_str().map(str::to_owned))
        .take(128)
        .collect()
}
pub(super) fn pod_log_choices(object: &Value) -> Vec<String> {
    std::iter::once("*".to_owned())
        .chain(pod_container_names(object))
        .collect()
}
pub(super) fn namespace(object: &Value) -> &str {
    object["metadata"]["namespace"].as_str().unwrap_or("-")
}
pub(super) fn restarts(object: &Value) -> u64 {
    [
        "initContainerStatuses",
        "containerStatuses",
        "ephemeralContainerStatuses",
    ]
    .iter()
    .filter_map(|field| object["status"][field].as_array())
    .flatten()
    .filter_map(|status| status["restartCount"].as_u64())
    .fold(0, u64::saturating_add)
}
pub(super) fn readiness(object: &Value) -> String {
    match object["kind"].as_str().unwrap_or("") {
        "Pod" => {
            let ready = object["status"]["containerStatuses"]
                .as_array()
                .map(|items| items.iter().filter(|item| item["ready"] == true).count())
                .unwrap_or(0);
            let total = object["spec"]["containers"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            format!("{ready}/{total}")
        }
        "Deployment" | "StatefulSet" | "ReplicaSet" => format!(
            "{}/{}",
            object["status"]["readyReplicas"].as_u64().unwrap_or(0),
            object["spec"]["replicas"].as_u64().unwrap_or(1)
        ),
        "DaemonSet" => format!(
            "{}/{}",
            object["status"]["numberReady"].as_u64().unwrap_or(0),
            object["status"]["desiredNumberScheduled"]
                .as_u64()
                .unwrap_or(0)
        ),
        "Job" => format!(
            "{}/{}",
            object["status"]["succeeded"].as_u64().unwrap_or(0),
            object["spec"]["completions"].as_u64().unwrap_or(1)
        ),
        "Node" => object["status"]["conditions"]
            .as_array()
            .and_then(|conditions| {
                conditions
                    .iter()
                    .find(|condition| condition["type"] == "Ready")
                    .and_then(|condition| condition["status"].as_str())
            })
            .unwrap_or("Unknown")
            .into(),
        _ => "-".into(),
    }
}
pub(super) fn ready_sort_key(object: &Value) -> (u64, u64) {
    let readiness = readiness(object);
    if let Some((ready, total)) = readiness.split_once('/') {
        (ready.parse().unwrap_or(0), total.parse().unwrap_or(0))
    } else {
        (u64::from(readiness == "True"), 0)
    }
}
pub(super) fn timestamp(object: &Value) -> Option<i64> {
    object["metadata"]["creationTimestamp"]
        .as_str()?
        .parse::<k8s_openapi::jiff::Timestamp>()
        .ok()
        .map(|time| time.as_second())
}
pub(super) fn age(object: &Value, now: i64) -> String {
    age_styled(object, now, false)
}
/// `long` adds the next smaller unit: `14d3h`, `5h12m`, `3m4s`.
pub(super) fn age_styled(object: &Value, now: i64, long: bool) -> String {
    let Some(created) = timestamp(object) else {
        return "-".into();
    };
    if created > now {
        return "future".into();
    }
    let seconds = now.saturating_sub(created);
    if long {
        return if seconds < 60 {
            format!("{seconds}s")
        } else if seconds < 3600 {
            format!("{}m{}s", seconds / 60, seconds % 60)
        } else if seconds < 86400 {
            format!("{}h{}m", seconds / 3600, seconds % 3600 / 60)
        } else {
            format!("{}d{}h", seconds / 86400, seconds % 86400 / 3600)
        };
    }
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86400 {
        format!("{}h", seconds / 3600)
    } else {
        format!("{}d", seconds / 86400)
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Health {
    Healthy,
    Attention,
    Neutral,
}
pub(super) fn health(object: &Value) -> Health {
    let status = summary(object);
    if [
        "Failed",
        "Error",
        "Crash",
        "BackOff",
        "NotReady",
        "Pending",
        "Unknown",
        "Terminating",
    ]
    .iter()
    .any(|needle| status.contains(needle))
    {
        return Health::Attention;
    }
    match object["kind"].as_str().unwrap_or("") {
        "Pod" if object["status"]["phase"] == "Running" => {
            let statuses = object["status"]["containerStatuses"].as_array();
            let count = object["spec"]["containers"]
                .as_array()
                .map(Vec::len)
                .unwrap_or(0);
            if count > 0
                && statuses.is_some_and(|statuses| {
                    statuses.len() == count && statuses.iter().all(|item| item["ready"] == true)
                })
            {
                Health::Healthy
            } else {
                Health::Attention
            }
        }
        "Node" => {
            if readiness(object) == "True" {
                Health::Healthy
            } else {
                Health::Attention
            }
        }
        "Deployment" | "StatefulSet" | "ReplicaSet" | "DaemonSet" => {
            let ready = readiness(object);
            if let Some((ready, desired)) = ready.split_once('/') {
                if ready == desired {
                    Health::Healthy
                } else {
                    Health::Attention
                }
            } else {
                Health::Neutral
            }
        }
        _ if [
            "Succeeded",
            "Complete",
            "Running",
            "Active",
            "Bound",
            "Ready",
        ]
        .contains(&status.as_str()) =>
        {
            Health::Healthy
        }
        _ => Health::Neutral,
    }
}
pub(super) fn clean_value(value: &Value) -> String {
    value
        .as_str()
        .map(|value| safety::safe_text(value, 512))
        .unwrap_or_else(|| {
            if value.is_null() {
                "-".into()
            } else {
                safety::safe_text(&value.to_string(), 512)
            }
        })
}
pub(super) fn overview_text(object: &Value, context: &str) -> String {
    let now = k8s_openapi::jiff::Timestamp::now().as_second();
    let kind = object["kind"].as_str().unwrap_or("Resource");
    let mut lines = vec![
        format!(
            "{} / {} / {}",
            kind.to_ascii_uppercase(),
            namespace(object),
            name(object)
        ),
        format!(
            "STATE {}   READY {}   RESTARTS {}   AGE {}",
            summary(object),
            readiness(object),
            restarts(object),
            age(object, now)
        ),
        String::new(),
        "IDENTITY".into(),
        format!("Context   {}", safety::safe_text(context, 256)),
        format!("Namespace {}", namespace(object)),
        format!("Name      {}", name(object)),
        format!("UID       {}", clean_value(&object["metadata"]["uid"])),
        format!(
            "Version   {}",
            clean_value(&object["metadata"]["resourceVersion"])
        ),
    ];
    if kind == "Pod" {
        lines.extend([
            String::new(),
            "SCHEDULING & NETWORK".into(),
            format!("Node      {}", clean_value(&object["spec"]["nodeName"])),
            format!("Pod IP    {}", clean_value(&object["status"]["podIP"])),
            format!("QoS       {}", clean_value(&object["status"]["qosClass"])),
            format!(
                "Account   {}",
                clean_value(&object["spec"]["serviceAccountName"])
            ),
        ]);
    }
    let pod_spec = if kind == "Pod" {
        &object["spec"]
    } else {
        &object["spec"]["template"]["spec"]
    };
    let mut remaining = 64usize;
    for (field, status_field, label) in [
        ("initContainers", "initContainerStatuses", "INIT CONTAINERS"),
        ("containers", "containerStatuses", "CONTAINERS"),
        (
            "ephemeralContainers",
            "ephemeralContainerStatuses",
            "EPHEMERAL CONTAINERS",
        ),
    ] {
        if let Some(containers) = pod_spec[field].as_array() {
            lines.extend([String::new(), label.into()]);
            for container in containers.iter().take(remaining) {
                let container_name = container["name"].as_str().unwrap_or("-");
                let status = object["status"][status_field]
                    .as_array()
                    .and_then(|statuses| {
                        statuses
                            .iter()
                            .find(|status| status["name"] == container_name)
                    });
                lines.push(format!(
                    "{}  ready:{}  restarts:{}",
                    safety::safe_text(container_name, 256),
                    status
                        .map(|status| clean_value(&status["ready"]))
                        .unwrap_or_else(|| "-".into()),
                    status
                        .map(|status| clean_value(&status["restartCount"]))
                        .unwrap_or_else(|| "-".into())
                ));
                lines.push(format!("  image {}", clean_value(&container["image"])));
                if let Some(status) = status {
                    for state in ["waiting", "terminated"] {
                        if !status["state"][state].is_null() {
                            lines.push(format!(
                                "  {} {}",
                                state,
                                clean_value(&status["state"][state]["reason"])
                            ));
                        }
                    }
                }
            }
            remaining = remaining.saturating_sub(containers.len());
            if remaining == 0 {
                lines.push(
                    "Container overview capped at 64 entries; J shows bounded redacted JSON."
                        .into(),
                );
                break;
            }
        }
    }
    if let Some(conditions) = object["status"]["conditions"].as_array() {
        lines.extend([String::new(), "CONDITIONS".into()]);
        lines.extend(conditions.iter().take(32).map(|condition| {
            format!(
                "{} {} {}",
                padded(&clean_value(&condition["type"]), 22),
                padded(&clean_value(&condition["status"]), 8),
                clean_value(&condition["reason"])
            )
        }));
    }
    if kind == "Service" {
        lines.extend([
            String::new(),
            "NETWORK".into(),
            format!("Type      {}", clean_value(&object["spec"]["type"])),
            format!("ClusterIP {}", clean_value(&object["spec"]["clusterIP"])),
        ]);
        if let Some(ports) = object["spec"]["ports"].as_array() {
            lines.extend(ports.iter().take(32).map(|port| {
                format!(
                    "Port      {} / {} -> {}",
                    clean_value(&port["port"]),
                    clean_value(&port["protocol"]),
                    clean_value(&port["targetPort"])
                )
            }));
        }
    }
    if kind == "Event" {
        lines.extend([
            String::new(),
            "EVENT".into(),
            format!("Reason    {}", clean_value(&object["reason"])),
            format!("Message   {}", clean_value(&object["message"])),
        ]);
    }
    if let Some(labels) = object["metadata"]["labels"].as_object()
        && !labels.is_empty()
    {
        lines.extend([String::new(), "LABELS".into()]);
        lines.extend(labels.iter().take(32).map(|(key, value)| {
            format!("{} = {}", safety::safe_text(key, 256), clean_value(value))
        }));
    }
    lines.extend([String::new(), "J redacted JSON   y YAML   Esc close".into()]);
    safety::safe_lines(&lines.join("\n"), 64 * 1024)
}
pub(super) fn resource_label(kind: &Kind) -> String {
    match kind.kind.as_str() {
        "PersistentVolumeClaim" => "Volume claims".into(),
        "ConfigMap" => "Config maps".into(),
        "StatefulSet" => "Stateful sets".into(),
        "DaemonSet" => "Daemon sets".into(),
        "ReplicaSet" => "Replica sets".into(),
        "CronJob" => "Cron jobs".into(),
        "RoleBinding" => "Role bindings".into(),
        "StorageClass" => "Storage classes".into(),
        _ => {
            let mut name = kind.plural.clone();
            if let Some(first) = name.get_mut(..1) {
                first.make_ascii_uppercase();
            }
            name
        }
    }
}
/// Command reference shown under every generated key section in `?` help.
/// Keys are not listed here: they come from the keymap registry.
pub(super) const COMMAND_HELP: &str = "NAVIGATE LIKE K9S\n  :pod :pods :po           Singular, plural or discovered short name\n  :pod NAMESPACE          Resource in namespace; all means all namespaces\n  :pod /api|web           Resource plus regex filter\n  :pod app=api,tier=web    Server-side label selector\n  :pod @CONTEXT           Switch context; context names retain their case\n  :ctx / :ctx NAME        Context picker (Pod view) / direct switch (same view)\n  :ns / :ns NAME          Namespace picker / direct switch (Pod view)\n  :aliases                Resource alias and hotkey picker; Enter navigates\n  :pulses / :pu           Read-only cluster health snapshot\n  :xray po|svc|dp [NS]    Ownership and service relationship snapshot\n  :screendump / :sd       Browse saved redacted resource snapshots\n  :can u:NAME             Read-only RBAC subject view\n  :dir PATH               Browse local manifests; a reviews an apply\n  :helm [NAMESPACE]       Native release metadata\n  :history RELEASE        Selected release revision history\n  :keymap [TEXT] / :keys  Every action, its keys and view (filterable)\n  :q :q! :qa :Q :quit     Quit (in-flight mutations must finish)\n\nFILTER SYNTAX\n  /PATTERN                Case-insensitive regex over namespace/name/status\n  /!PATTERN               Inverse regex\n  /-f PATTERN             Fuzzy subsequence match\n  /-l LABEL_SELECTOR      Apply server-side label selector on Enter\n\nRESOURCE COMMANDS\n  :sort COLUMN [asc|desc] Sort by any header column; sort keys again reverse\n  :faults                 Only objects that need attention (same as the faults key)\n  :export                 Save the selected resource's redacted YAML\n  :discover               Refresh API discovery\n  :cp-from [-r] PATH [L]  Reviewed copy of a Pod file or directory\n  :cp-to [-r] LOCAL PATH  Reviewed copy to a new Pod file or directory; never replaces\n  :apply FILE / :dir a    Dry-run, diff and reviewed server-side apply\n  :ext [NAME]             Run a registered extension or k9s plugin on the selection\n  :debug [CONTAINER]      Reviewed ephemeral debug container and shell\n  :pf / :bench N C [PATH] Forward list / benchmark the selected loopback forward\n  :lint / :popeye         Read-only misconfiguration check for the namespace\n  :scan / :scan ns        Scan selected or namespace images (trivy/grype on PATH)\n  :rollback [REV]         Reviewed rollback of a Deployment/StatefulSet/DaemonSet\n  :trigger                Trigger a Job from the selected CronJob (reviewed)\n  :edit-container         Focused container image/resource editor\n  :kill                   Reviewed zero-grace Pod kill\n  :cordon :uncordon :drain Reviewed node maintenance\n  CPU/MEM snapshots refresh every 15s while the table is visible; - means\n  unknown, never zero. ~ indicates Metrics API omitted UID; creation checked.\n  Table CPU/MEM labels are rounded; describe or :metrics shows exact quantities.\n\nHELM AND LOGS\n  Helm Enter / r          Release revision history\n  History Enter / v       Explicit selected revision stored values\n  Helm i / u / t / c      Install / upgrade / test / separately install chart CRDs\n  History r / Ctrl-D      Rollback selected revision / uninstall current release\n  Helm d / m / y          Metadata / resource identities / metadata YAML\n  Helm Ctrl-R / /         Refresh / filter; Esc returns to previous view\n  Sources: local .tgz or explicit OCI :TAG / @sha256:DIGEST.\n  --helm-keyring requires native Helm provenance verification during prepare.\n  :helm-crds prepares create-only Namespaced CRDs, retained on uninstall.\n  :helm-test runs stored tests; :helm-crds-upgrade reviews schema changes.\n  :helm-crds-delete is explicit, checks live instances and needs stronger approval.\n  :helm-crds-migrate reviews storage rewrites; controller proof gates finalization.\n  :copilot advice on the selection; shows what is sent first.\n  :ask [QUESTION] asks Copilot about the screen behind the prompt; shows the exact text first.\n  :logs LABEL_SELECTOR    Explicit namespace-scoped multi-Pod log capture\n  Group reloads keep the same Pod UIDs; close and reopen to capture new Pods.\n  :logs-live [SELECTOR]   Opt in to announced Pod joins/departures (10s refresh)\n  Pod logs honor the default-container annotation, otherwise merge multiple\n  ordinary containers. Each merged line carries a namespace/Pod/container.\n\nKBORG COMMANDS\n  :actions                Discover actions with capability reasons\n  :scale :image :restart  Explicit reviewed workload operations\n  :metrics [resources]    Metrics / opt into requests-limit comparisons\n  :overview               Dashboard of up to nine explicit contexts\n  :themes                 Theme preview and custom palette creation\n\nCUSTOMIZE\n  keymap.yaml remaps any action above except fixed keys; hotkeys.yaml binds\n  Shift-0..9, Alt, F1..F12 or free Ctrl keys to commands; aliases.yaml adds\n  command names; views.yaml sets columns. All live in the KBorg config folder.\n\nLIMITS AND SAFETY\n  Context, namespace and mode remain visible; DEMO is sample data.\n  STALE and TRUNCATED indicate incomplete data. Regex uses bounded Rust regex\n  syntax; look-around and backreferences are unavailable.\n  :secrets lists key names; x reveals one\n  key for 30s; y/d show masked YAML (values as sizes). Values are never\n  exported, copied or kept after Esc.\n  The YAML editor edits existing namespaced non-Secret spec fields.\n  Hidden values, metadata and status are preserved.\n  Helm values are stored overrides with\n  known fields redacted; custom keys can remain confidential. Complete\n  review is mandatory for lifecycle changes. Raw storage, chart files and notes are excluded.\n  Marked rows change together only through one batch review: delete, restart\n  or CronJob suspend/resume, at most 50 objects of one kind, each dry-run and\n  policy-checked and approved with y/Enter; it stops at the first failure.\n  Every change shows a review first: y/Enter applies, n/Esc cancels.\n  --read-only browses without changes; an operator --config can switch features off.\n  Esc, Enter, Ctrl-C and review confirmations can never be remapped.";

#[derive(Clone, Copy, Debug)]
pub(super) struct Rect {
    pub(super) x: u16,
    pub(super) y: u16,
    pub(super) width: u16,
    pub(super) height: u16,
}
#[derive(Clone, Copy, Debug)]
pub(super) struct Layout {
    pub(super) table: Rect,
    /// Screen lines per table record (ui.yaml `table.density`).
    pub(super) step: u16,
}
impl Layout {
    #[cfg(test)]
    pub(super) fn new(width: u16, height: u16) -> Self {
        Self::for_ui(width, height, &crate::ui_config::UiConfig::default())
    }
    /// The resource table below and above the chrome rows ui.yaml keeps.
    pub(super) fn for_ui(width: u16, height: u16, ui: &crate::ui_config::UiConfig) -> Self {
        let chrome = ui.chrome();
        Self {
            table: Rect {
                x: 0,
                y: chrome.top,
                width,
                height: height.saturating_sub(chrome.top + chrome.bottom),
            },
            step: ui.table.step().max(1),
        }
    }
    pub(super) fn table_rows(self) -> usize {
        let lines = usize::from(self.table.height.saturating_sub(3));
        let step = usize::from(self.step.max(1));
        lines.div_ceil(step)
    }
}
#[derive(Clone, Copy)]
pub(super) enum Column {
    Namespace,
    Name,
    Ready,
    Status,
    Restarts,
    Age,
    Node,
    Ip,
    Cpu,
    Memory,
    Labels,
    Images,
    /// Index into the active view's custom columns.
    Custom(u8),
}
/// A built-in column by its k9s/KBorg title.
pub(super) fn builtin_column(title: &str) -> Option<Column> {
    Some(match title {
        "NAMESPACE" | "NS" => Column::Namespace,
        "NAME" => Column::Name,
        "READY" => Column::Ready,
        "STATUS" => Column::Status,
        "RSTS" | "RESTARTS" => Column::Restarts,
        "AGE" => Column::Age,
        "NODE" => Column::Node,
        "IP" => Column::Ip,
        "CPU" | "CPU(M)" => Column::Cpu,
        "MEM" | "MEMORY" => Column::Memory,
        "LABELS" => Column::Labels,
        "IMAGES" | "IMAGE" => Column::Images,
        _ => return None,
    })
}
fn default_width(column: Column) -> usize {
    match column {
        Column::Namespace => 16,
        Column::Name => 1,
        Column::Ready => 7,
        Column::Status => 20,
        Column::Restarts => 5,
        Column::Age => 6,
        Column::Node => 22,
        Column::Ip => 16,
        Column::Cpu | Column::Memory => 9,
        Column::Labels | Column::Images => 30,
        Column::Custom(_) => 16,
    }
}
/// Columns for a configured view: hidden and (outside wide mode) wide-only
/// columns are skipped, NAME takes the remaining width, and trailing columns
/// are dropped when the terminal is too narrow.
pub(super) fn view_columns(
    view: &crate::views::View,
    width: usize,
    wide: bool,
) -> Vec<(Column, usize)> {
    let mut columns: Vec<(Column, usize)> = view
        .columns
        .iter()
        .enumerate()
        .filter(|(_, spec)| !spec.hidden && (wide || !spec.wide))
        .filter_map(|(index, spec)| {
            let column = match &spec.path {
                Some(_) => Column::Custom(u8::try_from(index).ok()?),
                None => builtin_column(&spec.title)?,
            };
            let width = match column {
                Column::Custom(_) => (spec.title.len() + 2).clamp(20, 32),
                other => default_width(other),
            };
            Some((column, width))
        })
        .collect();
    if !columns
        .iter()
        .any(|(column, _)| matches!(column, Column::Name))
    {
        columns.insert(0, (Column::Name, 1));
    }
    let used = |columns: &[(Column, usize)]| {
        columns.iter().map(|(_, w)| w).sum::<usize>() + columns.len().saturating_sub(1)
    };
    while used(&columns) + 12 > width && columns.len() > 1 {
        let Some(last) = columns
            .iter()
            .rposition(|(column, _)| !matches!(column, Column::Name))
        else {
            break;
        };
        columns.remove(last);
    }
    let occupied = used(&columns);
    if let Some((_, name_width)) = columns
        .iter_mut()
        .find(|(column, _)| matches!(column, Column::Name))
    {
        *name_width += width.saturating_sub(occupied);
    }
    columns
}
fn labels(object: &Value) -> String {
    object["metadata"]["labels"]
        .as_object()
        .map(|labels| {
            labels
                .iter()
                .map(|(k, v)| format!("{k}={}", v.as_str().unwrap_or("")))
                .collect::<Vec<_>>()
                .join(",")
        })
        .unwrap_or_default()
}
fn images(object: &Value) -> String {
    let spec = if object["spec"]["template"].is_object() {
        &object["spec"]["template"]["spec"]
    } else if object["spec"]["jobTemplate"].is_object() {
        &object["spec"]["jobTemplate"]["spec"]["template"]["spec"]
    } else {
        &object["spec"]
    };
    spec["containers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["image"].as_str())
        .collect::<Vec<_>>()
        .join(",")
}
/// A custom column's value; `|T` columns show the age of a timestamp.
pub(super) fn custom_value(spec: &crate::views::ColumnSpec, object: &Value, now: i64) -> String {
    let values = spec
        .path
        .as_deref()
        .map(|path| crate::views::eval(path, object))
        .unwrap_or_default();
    if spec.time
        && let Some(first) = values.first()
    {
        return age(
            &serde_json::json!({"metadata":{"creationTimestamp":first}}),
            now,
        );
    }
    if values.is_empty() {
        "-".into()
    } else {
        values.join(",")
    }
}
impl Column {
    pub(super) fn title(self) -> &'static str {
        match self {
            Self::Namespace => "NAMESPACE",
            Self::Name => "NAME",
            Self::Ready => "READY",
            Self::Status => "STATUS",
            Self::Restarts => "RSTS",
            Self::Age => "AGE",
            Self::Node => "NODE",
            Self::Ip => "IP",
            Self::Cpu => "CPU(m)",
            Self::Memory => "MEM",
            Self::Labels => "LABELS",
            Self::Images => "IMAGES",
            Self::Custom(_) => "?",
        }
    }
    pub(super) fn value(self, object: &Value, now: i64) -> String {
        match self {
            Self::Namespace => namespace(object).into(),
            Self::Name => name(object).into(),
            Self::Ready => readiness(object),
            Self::Status => summary(object),
            Self::Restarts => {
                if object["kind"] == "Pod" {
                    restarts(object).to_string()
                } else {
                    "-".into()
                }
            }
            Self::Age => age(object, now),
            Self::Node => clean_value(&object["spec"]["nodeName"]),
            Self::Ip => clean_value(&object["status"]["podIP"]),
            Self::Cpu | Self::Memory | Self::Custom(_) => "-".into(),
            Self::Labels => labels(object),
            Self::Images => images(object),
        }
    }
}
pub(super) fn columns(width: usize, namespaced: bool) -> Vec<(Column, usize)> {
    // Name and health remain usable even when optional columns no longer fit.
    let mut columns = Vec::new();
    if namespaced && width >= 54 {
        columns.push((Column::Namespace, if width >= 94 { 16 } else { 12 }));
    }
    columns.push((Column::Name, 1));
    if width >= 46 {
        columns.push((Column::Ready, 7));
    }
    columns.push((
        Column::Status,
        if width >= 64 {
            20
        } else if width >= 46 {
            16
        } else {
            12
        },
    ));
    if width >= 70 {
        columns.push((Column::Restarts, 5));
    }
    if width >= 46 {
        columns.push((Column::Age, 6));
    }
    let occupied: usize =
        columns.iter().map(|(_, width)| width).sum::<usize>() + columns.len().saturating_sub(1);
    if let Some((_, name_width)) = columns
        .iter_mut()
        .find(|(column, _)| matches!(column, Column::Name))
    {
        *name_width += width.saturating_sub(occupied);
    }
    columns
}
/// Header with an ascending (^) or descending (v) marker on the sorted column.
/// Sort markers and the column separator come from ui.yaml `table`.
pub(super) fn header_line(
    columns: &[(Column, usize)],
    custom: &[crate::views::ColumnSpec],
    sorted: Option<(&str, bool)>,
    style: &crate::ui_config::Table,
) -> String {
    columns
        .iter()
        .map(|(column, width)| {
            let title = match column {
                Column::Custom(index) => custom
                    .get(usize::from(*index))
                    .map_or("?", |spec| spec.title.as_str()),
                other => other.title(),
            };
            let text = match sorted {
                Some((name, descending)) if column_matches(*column, title, name) => {
                    format!(
                        "{title}{}",
                        if descending {
                            &style.sort_descending
                        } else {
                            &style.sort_ascending
                        }
                    )
                }
                _ => title.to_owned(),
            };
            padded(&text, *width)
        })
        .collect::<Vec<_>>()
        .join(&style.column_separator)
}
fn column_matches(column: Column, title: &str, sorted: &str) -> bool {
    title.eq_ignore_ascii_case(sorted)
        || builtin_column(&sorted.to_ascii_uppercase()).is_some_and(|c| {
            std::mem::discriminant(&c) == std::mem::discriminant(&column)
                && !matches!(column, Column::Custom(_))
        })
}
/// Age format and the column separator come from ui.yaml `table`.
pub(super) fn table_line_custom(
    columns: &[(Column, usize)],
    object: &Value,
    now: i64,
    usage: Option<&table_metrics::Usage>,
    custom: &[crate::views::ColumnSpec],
    style: &crate::ui_config::Table,
) -> String {
    columns
        .iter()
        .map(|(column, width)| {
            let text = match column {
                Column::Custom(index) => custom
                    .get(usize::from(*index))
                    .map_or_else(|| "-".into(), |spec| custom_value(spec, object, now)),
                Column::Age => age_styled(object, now, style.long_age()),
                _ => {
                    return table_line_with_metrics(&[(*column, *width)], Some(object), now, usage);
                }
            };
            padded(&text, *width)
        })
        .collect::<Vec<_>>()
        .join(&style.column_separator)
}
pub(super) fn table_line_with_metrics(
    columns: &[(Column, usize)],
    object: Option<&Value>,
    now: i64,
    usage: Option<&table_metrics::Usage>,
) -> String {
    columns
        .iter()
        .map(|(column, width)| {
            let text = object
                .map(|object| match (column, usage) {
                    (Column::Cpu, Some(usage)) => format!(
                        "{}{}",
                        table_metrics::compact_cpu(usage.cpu_nanocores),
                        if usage.identity_verified { "" } else { "~" }
                    ),
                    (Column::Memory, Some(usage)) => format!(
                        "{}{}",
                        table_metrics::compact_memory(usage.memory_bytes),
                        if usage.identity_verified { "" } else { "~" }
                    ),
                    _ => column.value(object, now),
                })
                .unwrap_or_else(|| column.title().into());
            padded(&text, *width)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Untrusted text fitted to `width` terminal cells: sanitized, then real
/// glyphs (Unicode mode) or `?` per non-ASCII character (ASCII mode).
pub(super) fn display(text: &str, width: usize) -> String {
    crate::text::shown(text, width)
}
/// Cells `display` paints for `text` at unlimited width, for sizing a cell.
pub(super) fn text_cells(text: &str) -> u16 {
    u16::try_from(crate::text::width(&safety::safe_text(text, 4096))).unwrap_or(u16::MAX)
}
/// `display` padded with spaces to exactly `width` cells.
pub(super) fn padded(text: &str, width: usize) -> String {
    crate::text::column(text, width)
}
/// The CTX / NS / API identity as (text, theme role) spans: labels muted,
/// values in the text color and the namespace in the accent color, so the
/// eye finds the scope first. Joined, the spans fit `width` cells.
pub(super) fn identity_spans(
    context: &str,
    namespace: &str,
    server: &str,
    width: usize,
) -> Vec<(String, &'static str)> {
    const SEP: &str = "  |  ";
    let server_width = if width >= 110 { 36 } else { 0 };
    let available = width.saturating_sub(4 + SEP.len() + 3 + server_width);
    let namespace_width = (available / 3).clamp(6, 24).min(available);
    let context_width = available.saturating_sub(namespace_width);
    let mut spans = vec![
        ("CTX ".to_owned(), "muted"),
        (display(context, context_width), "text"),
        (SEP.to_owned(), "muted"),
        ("NS ".to_owned(), "muted"),
        (display(namespace, namespace_width), "accent"),
    ];
    if server_width > 0 {
        spans.push((SEP.to_owned(), "muted"));
        spans.push(("API ".to_owned(), "muted"));
        spans.push((display(server, server_width - SEP.len() - 4), "text"));
    }
    spans
}
/// Background of marked table rows: a tint of the theme's warning hue, so a
/// mark reads differently from the accent-based cursor row. Each theme gets
/// the strongest tint (up to 18%) that keeps text, status and mark colors at
/// 4.5:1; low-contrast palettes get a lighter tint.
pub(super) fn mark_background(theme: &themes::Theme) -> themes::Rgb {
    let tint = |percent| blend(theme.get("background"), theme.get("warning"), percent);
    (2..=18)
        .rev()
        .map(tint)
        .find(|mark| {
            ["text", "success", "warning"]
                .iter()
                .all(|role| themes::contrast(theme.get(role), *mark) >= 4.5)
        })
        .unwrap_or_else(|| tint(2))
}
pub(super) fn blend(a: themes::Rgb, b: themes::Rgb, percent: u16) -> themes::Rgb {
    let channel =
        |a: u8, b: u8| ((u16::from(a) * (100 - percent) + u16::from(b) * percent) / 100) as u8;
    themes::Rgb(channel(a.0, b.0), channel(a.1, b.1), channel(a.2, b.2))
}
pub(super) fn helm_heading(line: &str) -> bool {
    let text = line.trim_start();
    [
        "TARGET  ",
        "POLICY  ",
        "SOURCE  ",
        "TRUST  ",
        "01 ",
        "DETAILS",
        "Details",
        "OPERATION",
        "CONNECTION",
        "CHANGE PLAN",
        "APPROVAL",
        "FULL REVIEW",
    ]
    .iter()
    .any(|prefix| text.starts_with(prefix))
        || (text.chars().any(|c| c.is_ascii_alphabetic())
            && text.chars().all(|c| !c.is_ascii_lowercase()))
}
