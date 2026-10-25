//! Image vulnerability scanning through an explicitly trusted, user-installed
//! external scanner (Trivy or Grype). KBorg contains no scanner and no
//! vulnerability database code: it collects image references from cluster
//! objects, validates them strictly, runs the scanner under the same
//! process-group supervision as tool views and summarizes its JSON.
//!
//! This is supervision, not an OS sandbox. The scanner runs with the user's
//! filesystem and network permissions, reads/writes its own cache under HOME,
//! may download its vulnerability database and pulls image layers from
//! registries. Nothing is ever written to the cluster.
use crate::{Result, gateway::Gateway, safety};
use serde::Serialize;
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    time::{Duration, Instant},
};

/// Typed approval; an empty line (Enter) is accepted by callers as this word.
pub const CONFIRMATION: &str = "scan";
/// Distinct images scanned per run; the rest are reported as omitted.
pub const MAX_IMAGES: usize = 50;
/// The first image may include a vulnerability database download.
pub const FIRST_IMAGE_BUDGET: Duration = Duration::from_secs(300);
pub const IMAGE_BUDGET: Duration = Duration::from_secs(120);
pub const TOTAL_BUDGET: Duration = Duration::from_secs(20 * 60);
/// Combined stdout/stderr per scanner process.
pub const MAX_OUTPUT: usize = 32 * 1024 * 1024;
/// Findings listed per image, most severe first.
pub const TOP_FINDINGS: usize = 10;
const MAX_IMAGE_LEN: usize = 512;
const MAX_REJECTED: usize = 20;
const MAX_EXECUTABLE: u64 = 1024 * 1024 * 1024;
const VERSION_BUDGET: Duration = Duration::from_secs(15);
const REVIEW_LIFETIME: Duration = Duration::from_secs(300);
/// Environment variables passed through to the scanner when set. Everything
/// else, including Kubernetes and cloud credentials, is cleared.
const PASSTHROUGH: &[&str] = &[
    "HOME",
    "PATH",
    "USER",
    "XDG_CACHE_HOME",
    "DOCKER_HOST",
    "DOCKER_CONFIG",
    "DOCKER_CERT_PATH",
    "DOCKER_TLS_VERIFY",
    "HTTPS_PROXY",
    "HTTP_PROXY",
    "NO_PROXY",
    "https_proxy",
    "http_proxy",
    "no_proxy",
    "SSL_CERT_FILE",
    "SSL_CERT_DIR",
    "TRIVY_CACHE_DIR",
    "GRYPE_DB_CACHE_DIR",
];

/// Validates an image reference read from a cluster object (untrusted).
/// Plain references only: `[registry[:port]/]path[:tag][@digest]` with the
/// characters `A-Za-z0-9./:@_-`, starting with a letter or digit so it can
/// never be read as an option, and without scanner source schemes.
pub fn validate_image(image: &str) -> Result<()> {
    let bad = || Err::<(), String>("image reference is not a plain registry reference".into());
    if image.is_empty() || image.len() > MAX_IMAGE_LEN {
        return bad();
    }
    let bytes = image.as_bytes();
    if !bytes[0].is_ascii_alphanumeric()
        || !bytes[bytes.len() - 1].is_ascii_alphanumeric()
        || image
            .bytes()
            .any(|b| !(b.is_ascii_alphanumeric() || b"./:@_-".contains(&b)))
        || image.contains("..")
        || image.contains("//")
        || image.matches('@').count() > 1
    {
        return bad();
    }
    let (name, digest) = image.split_once('@').unwrap_or((image, ""));
    if !digest.is_empty() {
        let Some((algorithm, hex)) = digest.split_once(':') else {
            return bad();
        };
        if algorithm.is_empty()
            || hex.len() < 32
            || !hex.bytes().all(|b| b.is_ascii_hexdigit())
            || !algorithm.bytes().all(|b| b.is_ascii_alphanumeric())
        {
            return bad();
        }
    }
    // A colon before the first slash must be a registry port; otherwise only
    // the last path component may carry a `:tag`. This also rejects scheme
    // forms such as `dir:/etc` or `file:/x`.
    if let Some((first, rest)) = name.split_once('/') {
        if let Some((host, port)) = first.split_once(':')
            && (host.is_empty() || port.is_empty() || !port.bytes().all(|b| b.is_ascii_digit()))
        {
            return bad();
        }
        let last = rest.rsplit('/').next().unwrap_or(rest);
        if rest[..rest.len() - last.len()].contains(':') || last.matches(':').count() > 1 {
            return bad();
        }
    } else if name.matches(':').count() > 1 {
        return bad();
    }
    if name.ends_with(':') || name.split('/').any(str::is_empty) {
        return bad();
    }
    Ok(())
}

/// The Pod template (or Pod spec) of an object that runs containers.
pub fn pod_spec(object: &Value) -> Option<&Value> {
    let spec = match object["kind"].as_str()? {
        "Pod" => &object["spec"],
        "CronJob" => &object["spec"]["jobTemplate"]["spec"]["template"]["spec"],
        _ => &object["spec"]["template"]["spec"],
    };
    spec.is_object().then_some(spec)
}

/// Distinct, validated image references to scan, bounded by `MAX_IMAGES`.
#[derive(Debug, Default, Serialize)]
pub struct Images {
    pub images: Vec<String>,
    /// Sanitized, clipped references that failed validation (not scanned).
    pub rejected: Vec<String>,
    /// Valid distinct images beyond the per-run limit (not scanned).
    pub omitted: usize,
    /// Partial-read notes (truncated or unavailable lists).
    pub notes: Vec<String>,
}

pub fn collect<'a>(objects: impl IntoIterator<Item = &'a Value>, limit: usize) -> Images {
    let mut valid = BTreeSet::new();
    let mut rejected = BTreeSet::new();
    for object in objects {
        let Some(spec) = pod_spec(object) else {
            continue;
        };
        for field in ["initContainers", "containers", "ephemeralContainers"] {
            for container in spec[field].as_array().into_iter().flatten() {
                let Some(image) = container["image"].as_str() else {
                    continue;
                };
                if validate_image(image).is_ok() {
                    valid.insert(image.to_owned());
                } else if rejected.len() < MAX_REJECTED {
                    rejected.insert(format!("{:?}", safety::safe_text(image, 80)));
                }
            }
        }
    }
    let omitted = valid.len().saturating_sub(limit);
    Images {
        images: valid.into_iter().take(limit).collect(),
        rejected: rejected.into_iter().collect(),
        omitted,
        notes: Vec::new(),
    }
}

/// What to scan: one Pod/workload, or every distinct image in a namespace.
#[derive(Clone, Debug)]
pub enum Target {
    Object {
        resource: String,
        namespace: String,
        name: String,
    },
    Namespace(String),
}

const NAMESPACE_RESOURCES: &[&str] = &[
    "pods",
    "deployments",
    "statefulsets",
    "daemonsets",
    "replicasets",
    "jobs",
    "cronjobs",
];

/// Reads the target's objects (read-only) and returns a scope label and images.
pub async fn gather(gateway: &Gateway, target: &Target) -> Result<(String, Images)> {
    match target {
        Target::Object {
            resource,
            namespace,
            name,
        } => {
            let object = gateway.get(resource, namespace, name).await?;
            if pod_spec(&object).is_none() {
                return Err("only Pods and workloads (Deployment, StatefulSet, DaemonSet, ReplicaSet, Job, CronJob) run container images; use :scan ns for a namespace".into());
            }
            let kind = safety::safe_text(object["kind"].as_str().unwrap_or("Object"), 63);
            let images = collect(std::iter::once(&object), MAX_IMAGES);
            Ok((
                format!(
                    "{kind} {}/{}",
                    safety::safe_text(namespace, 63),
                    safety::safe_text(name, 253)
                ),
                images,
            ))
        }
        Target::Namespace(namespace) => {
            let reads = crate::insights::collect(gateway, namespace, NAMESPACE_RESOURCES).await;
            let mut notes = Vec::new();
            let mut objects = Vec::new();
            for read in reads {
                if let Some(error) = read.error {
                    notes.push(format!("{}: unavailable ({error})", read.resource));
                    continue;
                }
                if read.incomplete {
                    notes.push(format!("{}: truncated to the first objects", read.resource));
                }
                objects.extend(read.items);
            }
            let mut images = collect(&objects, MAX_IMAGES);
            images.notes = notes;
            let scope = if namespace == "*" {
                "all namespaces".to_owned()
            } else {
                format!("namespace {}", safety::safe_text(namespace, 63))
            };
            Ok((scope, images))
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum ScannerKind {
    Trivy,
    Grype,
}
impl ScannerKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Trivy => "Trivy",
            Self::Grype => "Grype",
        }
    }
}

/// A detected, identity-pinned scanner executable.
pub struct Scanner {
    path: PathBuf,
    kind: ScannerKind,
    version: String,
    metadata: fs::Metadata,
}
impl Scanner {
    pub fn kind(&self) -> ScannerKind {
        self.kind
    }
    pub fn label(&self) -> String {
        format!("{} {}", self.kind.label(), self.version)
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    fn check(&self) -> Result<()> {
        safety::checked_path(&self.path)?;
        let now = crate::handoff::trusted_metadata(&self.path, MAX_EXECUTABLE)?;
        if !crate::handoff::same_metadata(&self.metadata, &now) {
            return Err("scanner executable changed after review; prepare the scan again".into());
        }
        Ok(())
    }
    /// The exact argv for one image; the reference is a single element after `--`.
    pub fn argv(&self, image: &str) -> Vec<String> {
        let mut argv: Vec<String> = match self.kind {
            ScannerKind::Trivy => ["image", "--quiet", "--format", "json", "--scanners", "vuln"]
                .map(String::from)
                .to_vec(),
            ScannerKind::Grype => ["-q", "-o", "json"].map(String::from).to_vec(),
        };
        argv.push("--".into());
        // Grype otherwise interprets `scheme:` prefixes (dir:, file:, ...);
        // the explicit registry source makes the reference a plain image.
        argv.push(match self.kind {
            ScannerKind::Trivy => image.to_owned(),
            ScannerKind::Grype => format!("registry:{image}"),
        });
        argv
    }
}

fn checked_executable(path: &Path) -> Result<fs::Metadata> {
    if !cfg!(unix) {
        return Err(
            "image scanning requires verified process-group cleanup; unsupported on this platform"
                .into(),
        );
    }
    if !path.is_absolute() {
        return Err(
            "--scanner must name an absolute trusted executable; PATH lookup is disabled".into(),
        );
    }
    let display = path
        .to_str()
        .ok_or("scanner path must be UTF-8 for exact review")?;
    if display.len() > 4096 || safety::safe_text(display, 4096) != display {
        return Err("scanner path exceeds limits or contains terminal controls".into());
    }
    safety::checked_path(path).map_err(|_| {
        "scanner path must not contain symlinks; pass the resolved path (for example from realpath)"
            .to_owned()
    })?;
    let metadata = crate::handoff::trusted_metadata(path, MAX_EXECUTABLE)?;
    let mut head = [0u8; 2];
    let mut file = fs::File::open(path).map_err(|_| "cannot open scanner executable")?;
    file.read_exact(&mut head)
        .map_err(|_| "cannot read scanner executable")?;
    if &head == b"#!" {
        return Err(
            "--scanner must be a native executable; interpreter scripts are excluded".into(),
        );
    }
    Ok(metadata)
}

/// Identifies the scanner from its `--version` output.
pub fn identify(output: &str) -> Option<(ScannerKind, String)> {
    let clean = |v: &str| safety::safe_text(v.trim().trim_start_matches('v'), 32);
    let version_ok = |v: &str| {
        !v.is_empty()
            && v.as_bytes()[0].is_ascii_digit()
            && v.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-+".contains(&b))
    };
    let first = output.lines().next()?.trim();
    if let Some(version) = first.strip_prefix("grype ") {
        let version = clean(version);
        return version_ok(&version).then_some((ScannerKind::Grype, version));
    }
    if output
        .lines()
        .any(|line| line.trim() == "Application: grype")
    {
        let version = output
            .lines()
            .find_map(|line| line.trim().strip_prefix("Version:"))
            .map(clean)?;
        return version_ok(&version).then_some((ScannerKind::Grype, version));
    }
    if let Some(version) = first.strip_prefix("Version:") {
        let version = clean(version);
        return version_ok(&version).then_some((ScannerKind::Trivy, version));
    }
    None
}

#[cfg(unix)]
fn base_command(path: &Path, directory: &Path) -> std::process::Command {
    let mut command = std::process::Command::new(path);
    command
        .env_clear()
        .current_dir(directory)
        .env("TMPDIR", directory)
        .env("LANG", "C")
        .env("LC_ALL", "C");
    for name in PASSTHROUGH {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    command
        .env("TRIVY_SKIP_VERSION_CHECK", "true")
        .env("GRYPE_CHECK_FOR_APP_UPDATE", "false");
    command
}

/// Validates the executable and runs `<path> --version` (bounded, supervised).
pub fn detect(path: &Path) -> Result<Scanner> {
    let metadata = checked_executable(path)?;
    #[cfg(unix)]
    {
        let directory = crate::handoff::PrivateDirectory::create()?;
        let mut command = base_command(path, directory.path());
        command.arg("--version");
        let captured = crate::handoff::capture(command, None, VERSION_BUDGET, 64 * 1024, None)
            .map_err(|error| format!("scanner --version failed: {error}"))?;
        if !captured.status.success() {
            return Err("scanner --version exited with an error".into());
        }
        let (kind, version) = identify(&String::from_utf8_lossy(&captured.stdout))
            .ok_or("--scanner is neither Trivy nor Grype (unrecognized --version output)")?;
        let scanner = Scanner {
            path: path.into(),
            kind,
            version,
            metadata,
        };
        scanner.check()?;
        Ok(scanner)
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Err(
            "image scanning requires verified process-group cleanup; unsupported on this platform"
                .into(),
        )
    }
}

/// A reviewed scan: the detected scanner and the exact images.
pub struct Plan {
    scanner: Scanner,
    scope: String,
    images: Images,
    created: Instant,
}

pub fn prepare(scanner: &Path, scope: String, images: Images) -> Result<Plan> {
    if images.images.is_empty() {
        let mut message = format!("No scannable image references found for {scope}.");
        if !images.rejected.is_empty() {
            message.push_str(&format!(
                " Rejected references: {}.",
                images.rejected.join(", ")
            ));
        }
        return Err(message);
    }
    Ok(Plan {
        scanner: detect(scanner)?,
        scope,
        images,
        created: Instant::now(),
    })
}

impl Plan {
    pub fn images(&self) -> &[String] {
        &self.images.images
    }
    pub fn summary(&self) -> String {
        let mut out = format!(
            "IMAGE SCAN / read-only, runs locally\n\nScanner: {}\nExecutable: {}\nScope: {}\n\nNETWORK: {} may download or update its vulnerability database and pulls image layers from registries.\nIt runs with your OS user's permissions and uses your HOME (scanner cache, registry credentials) and proxy settings; Kubernetes credentials are not passed.\nLimits: {}s first image, {}s each after, {} minutes total, {} MiB output per image.\nNothing in the cluster is changed.\n\n",
            self.scanner.label(),
            safety::safe_text(&self.scanner.path.to_string_lossy(), 4096),
            self.scope,
            self.scanner.kind.label(),
            FIRST_IMAGE_BUDGET.as_secs(),
            IMAGE_BUDGET.as_secs(),
            TOTAL_BUDGET.as_secs() / 60,
            MAX_OUTPUT / (1024 * 1024)
        );
        if self.images.omitted > 0 {
            out.push_str(&format!(
                "Not scanned: {} more distinct images beyond the {MAX_IMAGES}-image limit\n",
                self.images.omitted
            ));
        }
        if !self.images.rejected.is_empty() {
            out.push_str(&format!(
                "Not scanned (invalid references): {}\n",
                self.images.rejected.join(", ")
            ));
        }
        for note in &self.images.notes {
            out.push_str(&format!("Note: {note}\n"));
        }
        out.push_str(&format!("Images to scan ({}):\n", self.images.images.len()));
        for image in &self.images.images {
            out.push_str(&format!("  {image}\n"));
        }
        out
    }
}

/// Shared cancellation and progress for a running scan.
#[derive(Default)]
pub struct Control {
    cancel: AtomicBool,
    started: AtomicUsize,
}
impl Control {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }
    pub fn canceled(&self) -> bool {
        self.cancel.load(Ordering::Acquire)
    }
    /// Number of images whose scan has started.
    pub fn started(&self) -> usize {
        self.started.load(Ordering::Acquire)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Severity {
    Critical,
    High,
    Medium,
    Low,
    Unknown,
}
impl Severity {
    /// Grype's "Negligible" counts as Low.
    pub fn parse(text: &str) -> Self {
        match text.to_ascii_lowercase().as_str() {
            "critical" => Self::Critical,
            "high" => Self::High,
            "medium" | "moderate" => Self::Medium,
            "low" | "negligible" => Self::Low,
            _ => Self::Unknown,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Self::Critical => "CRITICAL",
            Self::High => "HIGH",
            Self::Medium => "MEDIUM",
            Self::Low => "LOW",
            Self::Unknown => "UNKNOWN",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub critical: usize,
    pub high: usize,
    pub medium: usize,
    pub low: usize,
    pub unknown: usize,
}
impl Counts {
    fn add(&mut self, severity: Severity) {
        match severity {
            Severity::Critical => self.critical += 1,
            Severity::High => self.high += 1,
            Severity::Medium => self.medium += 1,
            Severity::Low => self.low += 1,
            Severity::Unknown => self.unknown += 1,
        }
    }
    pub fn total(&self) -> usize {
        self.critical + self.high + self.medium + self.low + self.unknown
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub id: String,
    pub package: String,
    pub installed: String,
    pub fixed: Option<String>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub counts: Counts,
    pub total: usize,
    /// The most severe findings, at most `TOP_FINDINGS`.
    pub findings: Vec<Finding>,
}

fn text(value: &Value, limit: usize) -> String {
    safety::safe_text(value.as_str().unwrap_or(""), limit)
}

fn summarize(raw: Vec<Finding>) -> Summary {
    // Deduplicate (id, package, installed) reported for several targets.
    let mut unique = BTreeMap::new();
    for finding in raw {
        unique
            .entry((
                finding.id.clone(),
                finding.package.clone(),
                finding.installed.clone(),
            ))
            .or_insert(finding);
    }
    let mut findings: Vec<Finding> = unique.into_values().collect();
    let mut counts = Counts::default();
    for finding in &findings {
        counts.add(finding.severity);
    }
    findings.sort_by(|a, b| (a.severity, &a.id, &a.package).cmp(&(b.severity, &b.id, &b.package)));
    findings.truncate(TOP_FINDINGS);
    Summary {
        total: counts.total(),
        counts,
        findings,
    }
}

/// `trivy image --format json` output.
pub fn parse_trivy(bytes: &[u8]) -> Result<Summary> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "scanner output is not valid Trivy JSON")?;
    if !value.is_object()
        || (value.get("SchemaVersion").is_none() && value.get("Results").is_none())
    {
        return Err("scanner output is not a Trivy JSON report".into());
    }
    let mut raw = Vec::new();
    for result in value["Results"].as_array().into_iter().flatten() {
        for vulnerability in result["Vulnerabilities"].as_array().into_iter().flatten() {
            let fixed = text(&vulnerability["FixedVersion"], 128);
            raw.push(Finding {
                severity: Severity::parse(vulnerability["Severity"].as_str().unwrap_or("")),
                id: text(&vulnerability["VulnerabilityID"], 64),
                package: text(&vulnerability["PkgName"], 128),
                installed: text(&vulnerability["InstalledVersion"], 128),
                fixed: (!fixed.is_empty()).then_some(fixed),
            });
        }
    }
    Ok(summarize(raw))
}

/// `grype -o json` output.
pub fn parse_grype(bytes: &[u8]) -> Result<Summary> {
    let value: Value =
        serde_json::from_slice(bytes).map_err(|_| "scanner output is not valid Grype JSON")?;
    let matches = value["matches"]
        .as_array()
        .ok_or("scanner output is not a Grype JSON report")?;
    let mut raw = Vec::new();
    for entry in matches {
        let vulnerability = &entry["vulnerability"];
        let fixed = vulnerability["fix"]["versions"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .take(4)
            .collect::<Vec<_>>()
            .join(", ");
        let fixed = safety::safe_text(&fixed, 128);
        raw.push(Finding {
            severity: Severity::parse(vulnerability["severity"].as_str().unwrap_or("")),
            id: text(&vulnerability["id"], 64),
            package: text(&entry["artifact"]["name"], 128),
            installed: text(&entry["artifact"]["version"], 128),
            fixed: (!fixed.is_empty()).then_some(fixed),
        });
    }
    Ok(summarize(raw))
}

#[derive(Debug, Serialize)]
pub struct ImageResult {
    pub image: String,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<Summary>,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub scanner: String,
    pub executable: String,
    pub scope: String,
    pub images: Vec<ImageResult>,
    pub rejected: Vec<String>,
    pub omitted: usize,
    pub notes: Vec<String>,
    pub canceled: bool,
}

fn failure_detail(stderr: &[u8]) -> String {
    let text = String::from_utf8_lossy(stderr);
    let joined = text
        .lines()
        .map(|line| safety::safe_text(line.replace('\t', " ").trim(), 512))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    let chars: Vec<char> = joined.chars().collect();
    chars[chars.len().saturating_sub(300)..].iter().collect()
}

/// Runs the reviewed plan once. Blocking: call from a worker thread.
pub fn run(plan: Plan, confirmation: &str, allowed: bool, control: &Control) -> Result<Report> {
    if !allowed {
        return Err("image scanning requires --scanner ABS_PATH and --allow-scan".into());
    }
    if confirmation != CONFIRMATION {
        return Err("scan canceled; type scan or press Enter to approve".into());
    }
    if plan.created.elapsed() > REVIEW_LIFETIME {
        return Err("scan review expired; prepare the scan again".into());
    }
    plan.scanner.check()?;
    #[cfg(unix)]
    {
        let directory = crate::handoff::PrivateDirectory::create()?;
        let started = Instant::now();
        let mut results = Vec::with_capacity(plan.images.images.len());
        for (index, image) in plan.images.images.iter().enumerate() {
            let skipped = |reason: &str| ImageResult {
                image: image.clone(),
                status: "skipped",
                error: Some(reason.into()),
                summary: None,
            };
            if control.canceled() {
                results.push(skipped("canceled"));
                continue;
            }
            let remaining = TOTAL_BUDGET.saturating_sub(started.elapsed());
            if remaining.is_zero() {
                results.push(skipped("overall time budget exhausted"));
                continue;
            }
            // Revalidate per image (defense in depth); images were validated
            // at collection time and the argument is a single argv element.
            validate_image(image)?;
            plan.scanner.check()?;
            control.started.store(index + 1, Ordering::Release);
            let budget = if index == 0 {
                FIRST_IMAGE_BUDGET
            } else {
                IMAGE_BUDGET
            }
            .min(remaining);
            let mut command = base_command(&plan.scanner.path, directory.path());
            command.args(plan.scanner.argv(image));
            let outcome =
                crate::handoff::capture(command, None, budget, MAX_OUTPUT, Some(&control.cancel));
            results.push(match outcome {
                Ok(captured) if captured.status.success() => {
                    let parsed = match plan.scanner.kind {
                        ScannerKind::Trivy => parse_trivy(&captured.stdout),
                        ScannerKind::Grype => parse_grype(&captured.stdout),
                    };
                    match parsed {
                        Ok(summary) => ImageResult {
                            image: image.clone(),
                            status: "ok",
                            error: None,
                            summary: Some(summary),
                        },
                        Err(error) => ImageResult {
                            image: image.clone(),
                            status: "failed",
                            error: Some(error),
                            summary: None,
                        },
                    }
                }
                Ok(captured) => ImageResult {
                    image: image.clone(),
                    status: "failed",
                    error: Some(format!(
                        "scanner {}: {}",
                        captured
                            .status
                            .code()
                            .map_or_else(|| "terminated by signal".into(), |c| format!("exit {c}")),
                        failure_detail(&captured.stderr)
                    )),
                    summary: None,
                },
                Err(error) => ImageResult {
                    image: image.clone(),
                    status: if control.canceled() {
                        "skipped"
                    } else {
                        "failed"
                    },
                    error: Some(error),
                    summary: None,
                },
            });
        }
        Ok(Report {
            scanner: plan.scanner.label(),
            executable: safety::safe_text(&plan.scanner.path.to_string_lossy(), 4096),
            scope: plan.scope,
            images: results,
            rejected: plan.images.rejected,
            omitted: plan.images.omitted,
            notes: plan.images.notes,
            canceled: control.canceled(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = control;
        Err(
            "image scanning requires verified process-group cleanup; unsupported on this platform"
                .into(),
        )
    }
}

fn clip(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        text.to_owned()
    } else {
        let mut out: String = text.chars().take(width.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Human-readable table plus the top findings for each image.
pub fn report_text(report: &Report) -> String {
    let mut out = format!(
        "IMAGE SCAN  {}\nScanner: {} ({})\nRead-only; nothing in the cluster was changed.{}\n\n{:<48} {:>5} {:>5} {:>5} {:>5} {:>5}  STATUS\n",
        report.scope,
        report.scanner,
        report.executable,
        if report.canceled {
            "  CANCELED: remaining images were skipped."
        } else {
            ""
        },
        "IMAGE",
        "CRIT",
        "HIGH",
        "MED",
        "LOW",
        "UNK"
    );
    for result in &report.images {
        let counts = result
            .summary
            .as_ref()
            .map(|s| s.counts)
            .unwrap_or_default();
        let cell = |n: usize| {
            if result.summary.is_some() {
                n.to_string()
            } else {
                "-".into()
            }
        };
        out.push_str(&format!(
            "{:<48} {:>5} {:>5} {:>5} {:>5} {:>5}  {}\n",
            clip(&result.image, 48),
            cell(counts.critical),
            cell(counts.high),
            cell(counts.medium),
            cell(counts.low),
            cell(counts.unknown),
            match &result.error {
                Some(error) => format!("{}: {error}", result.status),
                None => result.status.to_owned(),
            }
        ));
    }
    if report.omitted > 0 {
        out.push_str(&format!(
            "\n{} more distinct images were not scanned ({MAX_IMAGES}-image limit).\n",
            report.omitted
        ));
    }
    if !report.rejected.is_empty() {
        out.push_str(&format!(
            "Invalid references not scanned: {}\n",
            report.rejected.join(", ")
        ));
    }
    for note in &report.notes {
        out.push_str(&format!("Note: {note}\n"));
    }
    for result in &report.images {
        let Some(summary) = &result.summary else {
            continue;
        };
        if summary.findings.is_empty() {
            continue;
        }
        out.push_str(&format!(
            "\n{}  (top {} of {})\n",
            result.image,
            summary.findings.len(),
            summary.total
        ));
        for finding in &summary.findings {
            out.push_str(&format!(
                "  {:<8} {:<20} {} {} -> {}\n",
                finding.severity.label(),
                finding.id,
                finding.package,
                finding.installed,
                finding.fixed.as_deref().unwrap_or("no fix")
            ));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn image_references_are_validated_strictly() {
        for good in [
            "nginx",
            "nginx:1.25",
            "library/nginx:1.25-alpine",
            "ghcr.io/org/app:v1.2.3",
            "registry.example.com:5000/team/app_x:2024.01",
            "localhost:5000/app",
            "registry:2",
            "dir:etc",
            "busybox@sha256:3fbc632167424a6d997e74f52b878d7cc478225cffac6bc977eedfe51c7f4e79",
            "quay.io/a/b:tag@sha256:3fbc632167424a6d997e74f52b878d7cc478225cffac6bc977eedfe51c7f4e79",
        ] {
            assert!(validate_image(good).is_ok(), "{good}");
        }
        for bad in [
            "",
            "-o",
            "--help",
            "-",
            "nginx latest",
            " nginx",
            "nginx\n",
            "nginx\u{1b}[31m",
            "nginx\0",
            "nginx;rm",
            "$(id)",
            "nginx:",
            "nginx/",
            ":tag",
            "/abs/path",
            "dir:/etc",
            "file:/etc/passwd",
            "docker-archive:/tmp/x.tar",
            "a//b",
            "a/../b",
            "host:port/app",
            "a/b:c/d",
            "a:b:c",
            "img@sha256:short",
            "img@@sha256:3fbc632167424a6d997e74f52b878d7cc478225cffac6bc977eedfe51c7f4e79",
            "ngïnx",
        ] {
            assert!(validate_image(bad).is_err(), "{bad:?}");
        }
        assert!(validate_image(&"a".repeat(MAX_IMAGE_LEN + 1)).is_err());
        assert!(validate_image(&"a".repeat(MAX_IMAGE_LEN)).is_ok());
    }

    #[test]
    fn images_are_collected_from_pods_workloads_and_cronjobs_bounded() {
        let pod = json!({"kind":"Pod","spec":{
            "initContainers":[{"name":"init","image":"busybox:1.36"}],
            "containers":[{"name":"app","image":"nginx:1.25"},{"name":"bad","image":"-o"}],
            "ephemeralContainers":[{"name":"debug","image":"busybox:1.36"}]}});
        let deployment = json!({"kind":"Deployment","spec":{"template":{"spec":{
            "containers":[{"name":"api","image":"ghcr.io/org/api:v2"}]}}}});
        let cronjob = json!({"kind":"CronJob","spec":{"jobTemplate":{"spec":{"template":{"spec":{
            "containers":[{"name":"job","image":"alpine:3.19"},{"name":"x","image":"bad image\u{1b}"}]}}}}}});
        let service =
            json!({"kind":"Service","spec":{"template":{"containers":[{"image":"nope"}]}}});
        let images = collect([&pod, &deployment, &cronjob, &service], MAX_IMAGES);
        assert_eq!(
            images.images,
            [
                "alpine:3.19",
                "busybox:1.36",
                "ghcr.io/org/api:v2",
                "nginx:1.25"
            ]
        );
        assert_eq!(images.rejected.len(), 2);
        assert!(images.rejected.iter().all(|r| !r.contains('\u{1b}')));
        assert_eq!(images.omitted, 0);
        assert!(pod_spec(&service).is_none());

        let many: Vec<Value> = (0..70)
            .map(|i| json!({"kind":"Pod","spec":{"containers":[{"image":format!("app{i:02}:1")}]}}))
            .collect();
        let bounded = collect(&many, MAX_IMAGES);
        assert_eq!(bounded.images.len(), MAX_IMAGES);
        assert_eq!(bounded.omitted, 20);
        let rejected: Vec<Value> = (0..40)
            .map(|i| json!({"kind":"Pod","spec":{"containers":[{"image":format!("-bad{i}")}]}}))
            .collect();
        assert_eq!(collect(&rejected, MAX_IMAGES).rejected.len(), MAX_REJECTED);
    }

    const TRIVY: &str = r#"{
      "SchemaVersion": 2,
      "ArtifactName": "alpine:3.19",
      "ArtifactType": "container_image",
      "Results": [
        {"Target": "alpine:3.19 (alpine 3.19.0)", "Class": "os-pkgs", "Type": "alpine",
         "Vulnerabilities": [
          {"VulnerabilityID": "CVE-2023-6129", "PkgName": "libcrypto3", "InstalledVersion": "3.1.4-r2",
           "FixedVersion": "3.1.4-r3", "Severity": "MEDIUM", "Title": "openssl: POLY1305 MAC"},
          {"VulnerabilityID": "CVE-2024-0727", "PkgName": "libssl3", "InstalledVersion": "3.1.4-r2",
           "FixedVersion": "3.1.4-r5", "Severity": "LOW"},
          {"VulnerabilityID": "CVE-2023-5363", "PkgName": "libcrypto3", "InstalledVersion": "3.1.4-r2",
           "FixedVersion": "3.1.4-r1", "Severity": "HIGH"},
          {"VulnerabilityID": "CVE-2023-5363", "PkgName": "libcrypto3", "InstalledVersion": "3.1.4-r2",
           "FixedVersion": "3.1.4-r1", "Severity": "HIGH"}
         ]},
        {"Target": "app/go.sum", "Class": "lang-pkgs", "Type": "gomod",
         "Vulnerabilities": [
          {"VulnerabilityID": "CVE-2024-24790", "PkgName": "stdlib\u001b[31m", "InstalledVersion": "1.21.0",
           "FixedVersion": "", "Severity": "CRITICAL"},
          {"VulnerabilityID": "GHSA-xxxx-yyyy", "PkgName": "golang.org/x/net", "InstalledVersion": "0.17.0",
           "Severity": "UNKNOWN"}
         ]},
        {"Target": "clean", "Class": "lang-pkgs", "Vulnerabilities": null}
      ]
    }"#;

    const GRYPE: &str = r#"{
      "matches": [
        {"vulnerability": {"id": "CVE-2023-5678", "severity": "Medium",
                           "fix": {"versions": ["3.1.4-r1"], "state": "fixed"}},
         "artifact": {"name": "libcrypto3", "version": "3.1.4-r0", "type": "apk"}},
        {"vulnerability": {"id": "CVE-2024-2511", "severity": "Negligible",
                           "fix": {"versions": [], "state": "not-fixed"}},
         "artifact": {"name": "libssl3", "version": "3.1.4-r0", "type": "apk"}},
        {"vulnerability": {"id": "GHSA-abcd", "severity": "Critical",
                           "fix": {"versions": ["1.2.3", "2.0.1"], "state": "fixed"}},
         "artifact": {"name": "lodash", "version": "4.17.10", "type": "npm"}},
        {"vulnerability": {"id": "CVE-2022-0001", "severity": "High", "fix": {"versions": ["9"]}},
         "artifact": {"name": "pkg\u202e", "version": "1", "type": "deb"}}
      ],
      "source": {"type": "image", "target": {"userInput": "alpine:3.19"}},
      "descriptor": {"name": "grype", "version": "0.74.0"}
    }"#;

    #[test]
    fn trivy_json_is_summarized_and_sanitized() -> Result<()> {
        let summary = parse_trivy(TRIVY.as_bytes())?;
        assert_eq!(
            summary.counts,
            Counts {
                critical: 1,
                high: 1,
                medium: 1,
                low: 1,
                unknown: 1
            }
        );
        assert_eq!(summary.total, 5);
        let first = &summary.findings[0];
        assert_eq!(first.severity, Severity::Critical);
        assert_eq!(first.id, "CVE-2024-24790");
        assert_eq!(first.package, "stdlib[31m");
        assert_eq!(first.fixed, None);
        assert_eq!(summary.findings[1].id, "CVE-2023-5363");
        assert_eq!(summary.findings[1].fixed.as_deref(), Some("3.1.4-r1"));
        assert_eq!(summary.findings[4].severity, Severity::Unknown);
        assert!(
            parse_trivy(br#"{"Results": null, "SchemaVersion": 2}"#)?
                .findings
                .is_empty()
        );
        assert!(parse_trivy(b"not json").is_err());
        assert!(parse_trivy(br#"{"matches": []}"#).is_err());
        Ok(())
    }

    #[test]
    fn grype_json_is_summarized_and_sanitized() -> Result<()> {
        let summary = parse_grype(GRYPE.as_bytes())?;
        assert_eq!(
            summary.counts,
            Counts {
                critical: 1,
                high: 1,
                medium: 1,
                low: 1,
                unknown: 0
            }
        );
        let ids: Vec<_> = summary.findings.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "GHSA-abcd",
                "CVE-2022-0001",
                "CVE-2023-5678",
                "CVE-2024-2511"
            ]
        );
        assert_eq!(summary.findings[0].fixed.as_deref(), Some("1.2.3, 2.0.1"));
        assert_eq!(summary.findings[1].package, "pkg");
        assert_eq!(summary.findings[3].fixed, None);
        assert!(parse_grype(br#"{"SchemaVersion": 2}"#).is_err());
        assert!(parse_grype(br#"{"matches": []}"#)?.findings.is_empty());
        Ok(())
    }

    #[test]
    fn severities_order_most_severe_first_and_findings_are_bounded() -> Result<()> {
        let mut all = vec![
            Severity::Unknown,
            Severity::Low,
            Severity::Critical,
            Severity::Medium,
            Severity::High,
        ];
        all.sort();
        assert_eq!(
            all,
            [
                Severity::Critical,
                Severity::High,
                Severity::Medium,
                Severity::Low,
                Severity::Unknown
            ]
        );
        assert_eq!(Severity::parse("Negligible"), Severity::Low);
        assert_eq!(Severity::parse("weird"), Severity::Unknown);
        let matches: Vec<Value> = (0..30)
            .map(|i| json!({"vulnerability":{"id":format!("CVE-{i:04}"),"severity":if i == 29 {"Critical"} else {"Low"}},"artifact":{"name":"p","version":"1"}}))
            .collect();
        let bytes = serde_json::to_vec(&json!({ "matches": matches }))
            .map_err(|_| "fixture encoding failed")?;
        let summary = parse_grype(&bytes)?;
        assert_eq!(summary.total, 30);
        assert_eq!(summary.findings.len(), TOP_FINDINGS);
        assert_eq!(summary.findings[0].id, "CVE-0029");
        Ok(())
    }

    #[test]
    fn scanner_identity_comes_from_version_output() {
        assert_eq!(
            identify("Version: 0.50.1\nVulnerability DB:\n  Version: 2\n"),
            Some((ScannerKind::Trivy, "0.50.1".into()))
        );
        assert_eq!(
            identify("grype 0.74.0\n"),
            Some((ScannerKind::Grype, "0.74.0".into()))
        );
        assert_eq!(
            identify("Application: grype\nVersion: 0.80.1\nBuildDate: x\n"),
            Some((ScannerKind::Grype, "0.80.1".into()))
        );
        assert_eq!(identify("kubectl v1.30"), None);
        assert_eq!(identify("Version: $(id)"), None);
        assert_eq!(identify(""), None);
    }

    #[test]
    fn scanner_argv_keeps_the_image_a_single_element_after_the_separator() {
        let metadata = match fs::metadata("/") {
            Ok(metadata) => metadata,
            Err(_) => return,
        };
        let mut scanner = Scanner {
            path: "/bin/trivy".into(),
            kind: ScannerKind::Trivy,
            version: "0.50.1".into(),
            metadata,
        };
        let argv = scanner.argv("nginx:1.25");
        assert_eq!(argv[argv.len() - 2], "--");
        assert_eq!(argv[argv.len() - 1], "nginx:1.25");
        assert!(argv.contains(&"vuln".to_owned()));
        scanner.kind = ScannerKind::Grype;
        assert_eq!(
            scanner.argv("registry:2").last().map(String::as_str),
            Some("registry:registry:2")
        );
    }

    #[test]
    fn scanner_paths_must_be_absolute_native_executables() {
        assert!(detect(Path::new("trivy")).is_err());
        assert!(detect(Path::new("/nonexistent/kborg/trivy")).is_err());
        #[cfg(unix)]
        {
            // A real native executable that is not a scanner is refused after
            // its bounded --version run.
            if let Ok(echo) = Path::new("/bin/echo").canonicalize() {
                assert!(detect(&echo).is_err());
            }
        }
    }

    #[test]
    fn report_text_lists_counts_failures_and_top_findings() -> Result<()> {
        let report = Report {
            scanner: "Trivy 0.50.1".into(),
            executable: "/opt/trivy".into(),
            scope: "Pod default/web".into(),
            images: vec![
                ImageResult {
                    image: "alpine:3.19".into(),
                    status: "ok",
                    error: None,
                    summary: Some(parse_trivy(TRIVY.as_bytes())?),
                },
                ImageResult {
                    image: "private.example.com/app:1".into(),
                    status: "failed",
                    error: Some("scanner exit 1: unauthorized".into()),
                    summary: None,
                },
            ],
            rejected: vec!["\"-o\"".into()],
            omitted: 3,
            notes: vec![],
            canceled: false,
        };
        let text = report_text(&report);
        assert!(text.contains("CVE-2024-24790"));
        assert!(text.contains("failed: scanner exit 1: unauthorized"));
        assert!(text.contains("3 more distinct images"));
        assert!(text.contains("libcrypto3 3.1.4-r2 -> 3.1.4-r1"));
        let json = serde_json::to_value(&report).map_err(|_| "encoding failed")?;
        assert_eq!(json["images"][0]["summary"]["counts"]["critical"], 1);
        assert_eq!(
            json["images"][0]["summary"]["findings"][0]["severity"],
            "CRITICAL"
        );
        Ok(())
    }
}
