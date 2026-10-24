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
    let Some(created) = timestamp(object) else {
        return "-".into();
    };
    if created > now {
        return "future".into();
    }
    let seconds = now.saturating_sub(created);
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
                "{:<22} {:<8} {}",
                clean_value(&condition["type"]),
                clean_value(&condition["status"]),
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
pub(super) const TUI_HELP: &str = "NAVIGATE LIKE K9S\n  :pod :pods :po           Singular, plural or discovered short name\n  :pod NAMESPACE          Resource in namespace; all means all namespaces\n  :pod /api|web           Resource plus regex filter\n  :pod app=api,tier=web    Server-side label selector\n  :pod @CONTEXT           Switch context; context names retain their case\n  :ctx / :ctx NAME        Context picker (Pod view) / direct switch (same view)\n  :ns / :ns NAME          Namespace picker / direct switch (Pod view)\n  Ctrl-A / :aliases       Resource alias picker; Enter navigates\n  :pulses / :pu           Read-only cluster health snapshot\n  :xray po|svc|dp [NS]    Ownership and service relationship snapshot\n  :screendump / :sd       Browse saved redacted resource snapshots\n  :can u:NAME             Read-only RBAC subject view\n  :dir PATH               Browse local YAML directory; no apply\n  :helm [NAMESPACE]       Native release metadata (--allow-helm)\n  :history RELEASE        Selected release revision history\n  :q :q! :qa :Q :quit     Quit (in-flight mutations must finish)\n\nCOMMAND PROMPT\n  : / Tab                 Open prompt; Tab completes; Shift-Tab reverses\n  Up / Down               Recall in-session command history\n  Left Right Home End     Edit at cursor; Ctrl-U clears\n  Enter / Esc             Run command / cancel without changing view\n  [ / ] / -               Previous / next / last view\n  Esc                     Close view or return to previous resource\n\nFILTER\n  /PATTERN                Case-insensitive regex over namespace/name/status\n  /!PATTERN               Inverse regex\n  /-f PATTERN             Fuzzy subsequence match\n  /-l LABEL_SELECTOR      Apply server-side label selector on Enter\n  Enter / Esc             Keep / restore previous filter and selected UID\n\nRESOURCE TABLE\n  j k / arrows            Move; PgUp PgDown Home End page\n  Enter                   Drill into containers, related Pods or namespace\n  d                       Describe selected object (redacted overview)\n  D                       Dashboard for current context and namespace\n  b / B                   Save current selection / open saved selections\n  y / v                   View redacted YAML; J views JSON\n  c / n                   Copy name / namespace via terminal OSC52 clipboard\n  w / 0                   Warp to selected namespace / all namespaces\n  N / A / S / R           Sort by name / age / status / readiness\n  C / M                   Sort Pods/Nodes by CPU / memory, highest first\n  CPU/MEM snapshots refresh every 15s while the table is visible; - means\n  unknown, never zero. ~ indicates Metrics API omitted UID; creation checked.\n  Table CPU/MEM labels are rounded; d or m shows exact measured quantities.\n  Space / Ctrl-Space      Mark / mark range (visual only; no bulk mutations)\n  Ctrl-Backslash          Clear marks\n  Ctrl-S / :export        Save visible snapshot / selected resource YAML\n  Ctrl-R / :discover      Refresh / API discovery; r restarts workloads\n  l / p                   Follow logs / previous logs (--allow-logs)\n  Pod s / a / f / F       Shell / attach / forwards / new forward\n  :cp-from PATH [LOCAL]   Reviewed copy of one Pod file (--allow-file-copy)\n  :apply FILE / :dir a    Dry-run, diff and reviewed server-side apply (--allow-apply)\n  :ext [NAME]             Run a registered read-only extension on the selection\n  Node s                  Reviewed node shell Pod (--allow-node-shell)\n  :cp-to LOCAL PATH       Reviewed copy to a new Pod file; never replaces\n  Workload s / r          Scale Deployment-StatefulSet / reviewed restart\n  e / :edit-container     YAML spec editor / focused container editor\n  Ctrl-D                  Reviewed deletion\n  Ctrl-K / :kill          Reviewed zero-grace Pod kill (--allow-force-kill)\n  o                       Show selected Pod's Node\n\nHELM AND LOGS\n  Helm Enter / r          Release revision history\n  History Enter / v       Explicit selected revision stored values\n  Helm i / u / t / c      Install / upgrade / test / separately install chart CRDs\n  History r / Ctrl-D      Rollback selected revision / uninstall current release\n  Helm lifecycle needs --write --allow-helm --allow-helm-write and --helm-tool.\n  Sources: local .tgz or explicit OCI :TAG / @sha256:DIGEST.
  --helm-keyring requires native Helm provenance verification during prepare.\n  Hooks/CRDs require --allow-helm-hooks / --allow-helm-crds; default OFF.\n  :helm-crds prepares create-only Namespaced CRDs, retained on uninstall.
  :helm-test runs stored tests; :helm-crds-upgrade reviews schema changes.
  :helm-crds-delete is explicit, checks live instances and needs stronger approval.\n  :helm-crds-migrate reviews storage rewrites; controller proof gates finalization.\n  :copilot previews selected diagnostics; explicit policy and transfer approval to send.\n  Every review line must be painted before exact namespace/release approval.\n  Helm d / m / y          Metadata / resource identities / metadata YAML\n  Helm Ctrl-R / /         Refresh / filter; Esc returns to previous view\n  Log a                   Toggle all ordinary containers / preferred container\n  Workload l / p          Fixed captured Pod membership / previous logs\n  :logs LABEL_SELECTOR    Explicit namespace-scoped multi-Pod log capture\n  Group reloads keep the same Pod UIDs; close and reopen to capture new Pods.\n  :logs-live [SELECTOR]   Opt in to announced Pod joins/departures (10s refresh)\n  Log 0..6                Tail / head / last 1m,5m,15m,30m,1h\n  Log s w t f             Autoscroll / wrap / timestamps / fullscreen\n  Log e / W / Ctrl-S      Error matches / warning matches / local export\n  Pod l honors the default-container annotation, otherwise merges multiple\n  ordinary containers. Each merged line carries a namespace/Pod/container.\n\nKBORG COMMANDS\n  :actions                Discover actions with capability reasons\n  :scale :image :restart  Explicit reviewed workload operations\n  :metrics [resources]    Metrics / opt into requests-limit comparisons\n  :overview               Dashboard of up to nine explicit contexts\n  Dashboard 1..4 / Enter   Focus a context / open its Pods\n  Dashboard c / s / B      Choose contexts / save / open saved selections\n  Dashboard ?             Contextual keymap and health evidence meaning\n  :themes                 Theme preview and custom palette creation\n\nLIMITS AND SAFETY\n  Context, namespace and mode remain visible; DEMO is sample data.\n  STALE and TRUNCATED indicate incomplete data. Regex uses bounded Rust regex\n  syntax; look-around and backreferences are unavailable.\n  :secrets lists key names only with --allow-secret-reveal; x reveals one\n  key for 30s. Values are never exported, copied or kept after Esc.\n  e edits existing namespaced non-Secret spec fields with --allow-yaml-edit.\n  Hidden values, metadata and status are preserved; no resource creation.\n  :edit-container retains focused image/resource forms.\n  Node c/u/r reviews cordon/uncordon/drain with --allow-node-maintenance.\n  Pod file transfer, node shell, benchmarks and scans remain unavailable.\n  Helm release reads need --allow-helm. Values are stored overrides with\n  known fields redacted; custom keys can remain confidential. Complete\n  review is mandatory for lifecycle changes. Raw storage, chart files and notes are excluded.\n  Visual marks never authorize bulk writes. Every mutation requires --write\n  plus one-use approval; shell/attach/forward need separate capabilities.";

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
}
impl Layout {
    pub(super) fn new(width: u16, height: u16) -> Self {
        Self {
            table: Rect {
                x: 0,
                y: 3,
                width,
                height: height.saturating_sub(6),
            },
        }
    }
    pub(super) fn table_rows(self) -> usize {
        usize::from(self.table.height.saturating_sub(3))
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
            Self::Cpu | Self::Memory => "-".into(),
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
pub(super) fn table_line(columns: &[(Column, usize)], object: Option<&Value>, now: i64) -> String {
    table_line_with_metrics(columns, object, now, None)
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
            format!("{:<width$}", display(&text, *width), width = *width)
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn display(text: &str, width: usize) -> String {
    let mut text: String = safety::safe_text(text, width.saturating_add(1))
        .chars()
        .map(|c| if c.is_ascii() { c } else { '?' })
        .collect();
    if text.len() > width {
        text.truncate(width);
        if width > 0 {
            text.pop();
            text.push('~');
        }
    }
    text
}
pub(super) fn identity_line(context: &str, namespace: &str, server: &str, width: usize) -> String {
    let server_width = if width >= 110 { 32 } else { 0 };
    let available = width.saturating_sub(10 + server_width);
    let namespace_width = (available / 3).clamp(6, 24).min(available);
    let context_width = available.saturating_sub(namespace_width);
    let mut line = format!(
        "CTX {} | NS {}",
        display(context, context_width),
        display(namespace, namespace_width)
    );
    if server_width > 0 {
        line.push_str(&format!(" | API {}", display(server, server_width - 7)));
    }
    line
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
