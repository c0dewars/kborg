//! Reviewed Helm 4 lifecycle. Namespace-scoped deterministic packaged charts only.
//! Helm remains a trusted OS-user process, not a sandbox or atomic API transaction.
use crate::{
    Result,
    gateway::Gateway,
    helm::{self, Release, Storage},
    safety,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use flate2::read::MultiGzDecoder;
use kube::{
    Api, Client, Config,
    api::{ApiResource, DynamicObject},
};
use ring::digest::{Context, SHA256};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const MAX_CHART: u64 = 8 * 1024 * 1024;
const MAX_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_REVIEW: usize = 512 * 1024;
const MAX_RESOURCES: usize = 128;
const REVIEW_LIFETIME: Duration = Duration::from_secs(900);
#[derive(Clone, Debug)]
pub enum Request {
    InstallCrds {
        chart: PathBuf,
    },
    UpgradeCrds {
        chart: PathBuf,
    },
    DeleteCrds {
        chart: PathBuf,
    },
    Test,
    Install {
        chart: PathBuf,
        values: Option<PathBuf>,
    },
    Upgrade {
        chart: PathBuf,
        values: Option<PathBuf>,
    },
    Rollback {
        revision: u32,
    },
    Uninstall,
}
impl Request {
    fn name(&self) -> &'static str {
        match self {
            Self::InstallCrds { .. } => "crds",
            Self::UpgradeCrds { .. } => "crds-upgrade",
            Self::DeleteCrds { .. } => "crds-delete",
            Self::Test => "test",
            Self::Install { .. } => "install",
            Self::Upgrade { .. } => "upgrade",
            Self::Rollback { .. } => "rollback",
            Self::Uninstall => "uninstall",
        }
    }
}
#[derive(Clone, Debug)]
pub struct Options {
    pub executable: PathBuf,
    pub namespace: String,
    pub release: String,
    pub storage: Storage,
    pub seconds: u16,
    pub allow_hooks: bool,
    pub allow_crds: bool,
    pub registry: crate::helm_oci::Options,
    pub keyring: Option<PathBuf>,
}
#[derive(Debug, Serialize)]
pub struct Outcome {
    pub success: bool,
    pub summary: String,
    pub uncertain: bool,
}
/// Deliberately no Debug: contains only embedded, already validated credentials.
#[derive(Clone)]
pub(crate) struct StaticConnection {
    config: Arc<Value>,
    gate: crate::auth::AuthGate,
}
impl StaticConnection {
    pub(crate) fn from_config(config: &mut Config, gate: crate::auth::AuthGate) -> Result<Self> {
        let auth = &mut config.auth_info;
        if auth.exec.is_some()
            || auth.auth_provider.is_some()
            || auth.impersonate.is_some()
            || auth.impersonate_groups.is_some()
            || auth.impersonate_uid.is_some()
            || auth.impersonate_user_extra.is_some()
            || !auth.other.is_empty()
            || config.proxy_url.is_some()
            || config.accept_invalid_certs
            || !config.headers.is_empty()
        {
            return Err("Helm lifecycle needs a static verified connection without helpers/providers/impersonation/extra headers".into());
        }
        // Acquired helper replies were already replaced by the validated static
        // credentials. File inputs become identical immutable native/Helm data.
        if auth.token.is_none()
            && let Some(path) = auth.token_file.take()
        {
            let bytes = safety::read_small(Path::new(&path), 1024 * 1024)?;
            let token = String::from_utf8(bytes)
                .map_err(|_| "invalid static token file; details suppressed")?;
            auth.token = Some(token.trim().to_owned().into());
        }
        if auth.client_certificate_data.is_none()
            && let Some(path) = auth.client_certificate.take()
        {
            auth.client_certificate_data =
                Some(STANDARD.encode(safety::read_small(Path::new(&path), 1024 * 1024)?));
        }
        if auth.client_key_data.is_none()
            && let Some(path) = auth.client_key.take()
        {
            auth.client_key_data = Some(
                STANDARD
                    .encode(safety::read_small(Path::new(&path), 1024 * 1024)?)
                    .into(),
            );
        }
        auth.token_file = None;
        auth.client_certificate = None;
        auth.client_key = None;
        if config.root_cert_file.is_some() {
            return Err("Helm lifecycle requires embedded verified CA data; external rotating CA file unsupported".into());
        }
        let roots = config
            .root_cert
            .as_ref()
            .filter(|roots| !roots.is_empty())
            .ok_or("Helm lifecycle requires an explicit verified CA bundle")?;
        let mut pem = String::new();
        for root in roots {
            pem.push_str("-----BEGIN CERTIFICATE-----\n");
            let encoded = STANDARD.encode(root);
            for line in encoded.as_bytes().chunks(64) {
                pem.push_str(std::str::from_utf8(line).map_err(|_| "invalid CA encoding")?);
                pem.push('\n');
            }
            pem.push_str("-----END CERTIFICATE-----\n");
        }
        let serialized = serde_json::to_value(&config.auth_info)
            .map_err(|_| "cannot project static credentials")?;
        let mut user = serde_json::Map::new();
        for key in [
            "username",
            "password",
            "token",
            "client-certificate-data",
            "client-key-data",
        ] {
            if let Some(value) = serialized.get(key) {
                user.insert(key.into(), value.clone());
            }
        }
        if !user.contains_key("token")
            && let Some(token) = gate.static_bearer()?
        {
            user.insert("token".into(), json!(token));
        }
        if user.is_empty() {
            return Err("Helm lifecycle requires static token or certificate credentials".into());
        }
        let mut cluster = json!({"server":config.cluster_url.to_string(),"certificate-authority-data":STANDARD.encode(pem)});
        if let Some(name) = &config.tls_server_name {
            cluster["tls-server-name"] = json!(name);
        }
        Ok(Self {
            config: Arc::new(
                json!({"apiVersion":"v1","kind":"Config","current-context":"reviewed","clusters":[{"name":"reviewed","cluster":cluster}],"users":[{"name":"reviewed","user":user}],"contexts":[{"name":"reviewed","context":{"cluster":"reviewed","user":"reviewed"}}]}),
            ),
            gate,
        })
    }
    fn write(&self, path: &Path) -> Result<()> {
        self.gate.check_live()?;
        safety::write_new(
            path,
            &serde_json::to_vec(self.config.as_ref())
                .map_err(|_| "cannot encode private static connection")?,
        )?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o400))
                .map_err(|_| "cannot seal private static credential file")?;
        }
        Ok(())
    }
}
struct Workspace(PathBuf);
impl Workspace {
    fn new() -> Result<Self> {
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "private temporary directory unavailable")?;
        safety::checked_path(&root)?;
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| "invalid clock")?
            .as_nanos();
        let path = root.join(format!(
            "kborg-helm-{}-{stamp}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        builder
            .create(&path)
            .map_err(|_| "cannot create private Helm workspace")?;
        for name in ["home", "cache", "config", "data", "plugins"] {
            safety::ensure_private_dir(&path.join(name))?;
        }
        Ok(Self(path))
    }
}
impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[derive(Clone)]
struct Tool {
    path: PathBuf,
    hash: String,
    metadata: fs::Metadata,
}
impl Tool {
    fn new(path: &Path) -> Result<Self> {
        if !cfg!(unix) {
            return Err("Helm lifecycle supervision is supported on Unix only".into());
        }
        if !path.is_absolute()
            || path
                .to_str()
                .is_none_or(|p| p.len() > 4096 || safety::safe_text(p, 4096) != p)
        {
            return Err("Helm needs an absolute trusted native executable path".into());
        }
        safety::checked_path(path)?;
        let metadata =
            fs::symlink_metadata(path).map_err(|_| "cannot inspect trusted Helm executable")?;
        if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 256 * 1024 * 1024 {
            return Err("Helm must be a regular native executable of at most 256 MiB".into());
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0
                || metadata.permissions().mode() & 0o6022 != 0
            {
                return Err(
                    "Helm cannot be setuid/setgid or group/world writable and must be executable"
                        .into(),
                );
            }
        }
        let mut file = fs::File::open(path).map_err(|_| "cannot open Helm executable")?;
        let mut prefix = [0u8; 4];
        file.read_exact(&mut prefix)
            .map_err(|_| "invalid Helm executable")?;
        if prefix != *b"\x7fELF"
            && !matches!(
                prefix,
                [0xcf, 0xfa, 0xed, 0xfe]
                    | [0xfe, 0xed, 0xfa, 0xcf]
                    | [0xca, 0xfe, 0xba, 0xbe]
                    | [0xbe, 0xba, 0xfe, 0xca]
            )
        {
            return Err(
                "Helm requires a native Mach-O/ELF executable; scripts are forbidden".into(),
            );
        }
        let hash = file_hash(path, 256 * 1024 * 1024)?;
        Ok(Self {
            path: path.into(),
            hash,
            metadata,
        })
    }
    fn check(&self) -> Result<()> {
        let current = Self::new(&self.path)?;
        if current.hash != self.hash || !same_metadata(&current.metadata, &self.metadata) {
            return Err("trusted Helm executable changed; review again".into());
        }
        Ok(())
    }
}
fn same_metadata(a: &fs::Metadata, b: &fs::Metadata) -> bool {
    let same = a.len() == b.len() && a.modified().ok() == b.modified().ok();
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        same && a.dev() == b.dev() && a.ino() == b.ino() && a.mode() == b.mode()
    }
    #[cfg(not(unix))]
    {
        same
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
fn hash(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&SHA256, bytes).as_ref())
}
fn file_hash(path: &Path, limit: u64) -> Result<String> {
    if cfg!(all(
        unix,
        not(any(
            target_os = "macos",
            all(
                target_os = "linux",
                any(target_arch = "x86_64", target_arch = "aarch64")
            )
        ))
    )) {
        return Err(
            "nonblocking reviewed file validation is verified only on macOS and Linux x86-64/ARM64"
                .into(),
        );
    }
    safety::checked_path(path)?;
    let before = fs::symlink_metadata(path).map_err(|_| "cannot inspect reviewed file")?;
    if !before.is_file() || before.len() > limit {
        return Err("reviewed input must remain a bounded regular file".into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    // Stable platform O_NONBLOCK values, also used by the already locked libc.
    // A FIFO swapped in after metadata inspection must not block the reactor.
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0004);
    }
    #[cfg(target_os = "linux")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(0x0800);
    }
    let mut file = options
        .open(path)
        .map_err(|_| "cannot read reviewed file")?;
    let opened = file
        .metadata()
        .map_err(|_| "cannot inspect opened reviewed file")?;
    if !opened.is_file() || !same_metadata(&before, &opened) {
        return Err("reviewed input changed while opening; prepare again".into());
    }
    let mut digest = Context::new(&SHA256);
    let mut total = 0u64;
    let mut buf = [0; 16384];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|_| "cannot hash reviewed file")?;
        if n == 0 {
            break;
        }
        total += n as u64;
        if total > limit {
            return Err("reviewed file exceeds its bound".into());
        }
        digest.update(&buf[..n]);
    }
    Ok(hex(digest.finish().as_ref()))
}
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct Identity {
    api_version: String,
    kind: String,
    namespace: String,
    name: String,
}
#[derive(Clone)]
struct Resource {
    identity: Identity,
    object: Value,
    api: ApiResource,
}
#[derive(Clone, PartialEq, Eq)]
struct Seal {
    identity: Identity,
    uid: Option<String>,
    version: Option<String>,
}
struct SignatureReview {
    inputs: crate::helm_provenance::Prepared,
    fingerprint: String,
}
pub struct Prepared {
    options: Options,
    request: Request,
    tool: Option<Tool>,
    workspace: Option<Workspace>,
    connection: Option<StaticConnection>,
    session: String,
    selected: Option<Release>,
    rollback_target: Option<Release>,
    history: Vec<(u32, String, String, String)>,
    resources: Vec<Resource>,
    seals: Vec<Seal>,
    canonical: String,
    chart_hash: Option<String>,
    chart_source: Option<String>,
    values_hash: Option<String>,
    summary: String,
    created: Instant,
    demo: bool,
    hooks: crate::helm_extensions::HookPlan,
    hook_seals: Vec<Seal>,
    crds: crate::helm_extensions::CrdPlan,
    crd_seals: Vec<Option<crate::helm_crds::Binding>>,
    bindings: Vec<crate::helm_crds::Binding>,
    impact: Vec<crate::helm_crds::Inventory>,
    review_context: String,
    test_runs: Vec<helm::TestRun>,
    signature: Option<SignatureReview>,
}
#[derive(Clone, Debug)]
pub struct ReviewOverview {
    pub operation: String,
    pub context: String,
    pub endpoint: String,
    pub target: String,
    pub source: Option<String>,
    pub resources: usize,
    pub hooks: usize,
    pub crds: usize,
    pub destructive: bool,
}
impl Prepared {
    pub fn overview(&self) -> ReviewOverview {
        ReviewOverview {
            operation: self.request.name().into(),
            context: safety::safe_text(&self.review_context, 1024),
            endpoint: self
                .connection
                .as_ref()
                .and_then(|connection| {
                    connection.config["clusters"][0]["cluster"]["server"].as_str()
                })
                .map(|server| safety::safe_text(server, 1024))
                .unwrap_or_else(|| "demo".into()),
            target: format!("{}/{}", self.options.namespace, self.options.release),
            source: self.chart_source.clone().or_else(|| match &self.request {
                Request::Install { chart, .. }
                | Request::Upgrade { chart, .. }
                | Request::InstallCrds { chart }
                | Request::UpgradeCrds { chart }
                | Request::DeleteCrds { chart } => {
                    Some(safety::safe_text(&chart.to_string_lossy(), 4096))
                }
                _ => None,
            }),
            resources: self.resources.len()
                + if matches!(self.request, Request::DeleteCrds { .. }) {
                    self.impact
                        .iter()
                        .map(|inventory| inventory.instances.len())
                        .sum::<usize>()
                } else {
                    0
                },
            hooks: self
                .hooks
                .active()
                .iter()
                .map(|hook| &hook.identity)
                .collect::<BTreeSet<_>>()
                .len(),
            crds: self.crds.definitions.len(),
            destructive: matches!(
                self.request,
                Request::Uninstall | Request::DeleteCrds { .. } | Request::UpgradeCrds { .. }
            ),
        }
    }
    pub fn is_demo(&self) -> bool {
        self.demo
    }
    pub fn summary(&self) -> &str {
        &self.summary
    }
    pub fn confirmation(&self) -> String {
        format!(
            "{}{}/{}",
            if matches!(self.request, Request::DeleteCrds { .. }) {
                "DELETE CRDS "
            } else {
                ""
            },
            self.options.namespace,
            self.options.release
        )
    }
    pub fn selected(&self) -> Option<&Release> {
        self.selected.as_ref()
    }
    pub fn rollback_target(&self) -> Option<&Release> {
        self.rollback_target.as_ref()
    }
}

fn unverified_archive_notice(request: &Request, keyring_supplied: bool) -> &'static str {
    if matches!(
        request,
        Request::Test | Request::Rollback { .. } | Request::Uninstall
    ) {
        if keyring_supplied {
            "Stored release operation: no chart archive is acquired and archive provenance is not reverified. The supplied --helm-keyring is not applied to this stored release operation.\n"
        } else {
            "Stored release operation: no chart archive is acquired and archive provenance is not reverified.\n"
        }
    } else {
        "No OpenPGP signature verified: no explicit --helm-keyring supplied; content digest is integrity, not publisher identity.\n"
    }
}

fn schema_request(request: &Request) -> bool {
    matches!(
        request,
        Request::InstallCrds { .. } | Request::UpgradeCrds { .. } | Request::DeleteCrds { .. }
    )
}
fn namespace_allowed(namespace: &str) -> Result<()> {
    if !safety::valid_id(namespace)
        || namespace == "*"
        || ["kube-system", "kube-public", "kube-node-lease"].contains(&namespace)
    {
        return Err("Helm lifecycle requires one non-system namespace".into());
    }
    Ok(())
}
fn snapshot(path: &Path, destination: &Path, limit: u64) -> Result<String> {
    let bytes = safety::read_small(path, limit)?;
    safety::write_new(destination, &bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(destination, fs::Permissions::from_mode(0o400))
            .map_err(|_| "cannot seal reviewed input")?;
    }
    Ok(hash(&bytes))
}
#[cfg(test)]
fn scan_chart(bytes: &[u8]) -> Result<()> {
    inspect_chart(bytes, false).map(|_| ())
}
fn inspect_chart(bytes: &[u8], allow_crds: bool) -> Result<crate::helm_extensions::CrdPlan> {
    let mut budget = 0usize;
    let mut crds = Vec::new();
    scan_archive(bytes, 0, &mut budget, &mut crds)?;
    crate::helm_extensions::crds(&crds.join("\n---\n"), allow_crds)
}
fn scan_archive(
    bytes: &[u8],
    depth: usize,
    budget: &mut usize,
    crds: &mut Vec<String>,
) -> Result<()> {
    if depth > 4 {
        return Err("bundled chart nesting exceeds four levels".into());
    }
    let mut decoder = MultiGzDecoder::new(bytes);
    let mut tar = Vec::new();
    decoder
        .by_ref()
        .take(32 * 1024 * 1024 + 1)
        .read_to_end(&mut tar)
        .map_err(|_| "chart is not a valid gzip archive")?;
    *budget = budget.saturating_add(tar.len());
    if *budget > 32 * 1024 * 1024 {
        return Err("expanded chart exceeds 32 MiB aggregate".into());
    }
    let mut offset = 0usize;
    if tar.len() % 512 != 0 {
        return Err("chart tar archive has a partial block".into());
    }
    let mut files = BTreeSet::new();
    let mut chart = false;
    let mut count = 0;
    while offset + 512 <= tar.len() {
        let header = &tar[offset..offset + 512];
        if header.iter().all(|b| *b == 0) {
            if tar[offset..].iter().any(|b| *b != 0) {
                return Err("chart contains trailing archive data".into());
            }
            break;
        }
        count += 1;
        if count > 4096 {
            return Err("chart archive exceeds 4096 entries".into());
        }
        let field = |range: std::ops::Range<usize>| -> Result<String> {
            let bytes = &header[range];
            let end = bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len());
            std::str::from_utf8(&bytes[..end])
                .map(str::to_owned)
                .map_err(|_| "chart archive path/number is not UTF-8".into())
        };
        let name = field(0..100)?;
        let prefix = field(345..500)?;
        let name = if prefix.is_empty() {
            name
        } else {
            format!("{prefix}/{name}")
        };
        if name.is_empty()
            || name.starts_with('/')
            || name.contains('\\')
            || name.split('/').any(|part| part == ".." || part == ".")
            || safety::safe_text(&name, 4096) != name
            || !files.insert(name.clone())
        {
            return Err("chart has an unsafe or duplicate archive path".into());
        }
        let size = usize::from_str_radix(field(124..136)?.trim(), 8)
            .map_err(|_| "unsupported chart archive size")?;
        let checksum = usize::from_str_radix(field(148..156)?.trim(), 8)
            .map_err(|_| "invalid chart archive checksum")?;
        let actual: usize = header
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if (148..156).contains(&i) {
                    32
                } else {
                    usize::from(*b)
                }
            })
            .sum();
        if checksum != actual {
            return Err("chart archive checksum mismatch".into());
        }
        let start = offset + 512;
        let end = start
            .checked_add(size)
            .filter(|end| *end <= tar.len())
            .ok_or("truncated chart archive")?;
        if !matches!(header[156], 0 | b'0' | b'5') || header[156] == b'5' && size != 0 {
            return Err(
                "chart links, extended headers and special archive entries are forbidden".into(),
            );
        }
        if header[156] != b'5'
            && name.split('/').any(|part| part == "crds")
            && [".yaml", ".yml", ".json"]
                .iter()
                .any(|extension| name.ends_with(extension))
        {
            let text =
                std::str::from_utf8(&tar[start..end]).map_err(|_| "chart CRDs must be UTF-8")?;
            if crds
                .iter()
                .map(String::len)
                .sum::<usize>()
                .saturating_add(text.len())
                > crate::helm_extensions::MAX_EXTENSION_BYTES
            {
                return Err("chart CRDs exceed4 MiB".into());
            }
            crds.push(text.into());
        }
        if name.ends_with("/Chart.yaml") {
            chart = true;
        }
        if name.contains("/charts/")
            && (name.ends_with(".tgz") || tar[start..end].starts_with(&[0x1f, 0x8b]))
        {
            scan_archive(&tar[start..end], depth + 1, budget, crds)?;
        } else if name.contains("/templates/") {
            let text = std::str::from_utf8(&tar[start..end])
                .map_err(|_| "chart templates must be UTF-8")?;
            deterministic_templates(text)?;
        }
        offset = start
            .checked_add(size.div_ceil(512) * 512)
            .ok_or("chart archive size overflow")?;
    }
    if !chart || offset > tar.len() {
        return Err("chart archive is missing Chart.yaml or is truncated".into());
    }
    Ok(())
}
fn deterministic_templates(text: &str) -> Result<()> {
    let forbidden=regex::Regex::new(r"\b(keys|values|now|ago|date|date_in_zone|date_modify|must_date_modify|env|expandenv|dateInZone|htmlDate|htmlDateInZone|toDate|mustToDate|dateModify|mustDateModify|durationRound|bcrypt|mustEncryptAES|randAlphaNum|randAlpha|randAscii|randNumeric|randBytes|randInt|shuffle|uuidv4|genCA|genCAWithKey|genSelfSignedCert|genSelfSignedCertWithKey|genSignedCert|genSignedCertWithKey|genPrivateKey|encryptAES|htpasswd|lookup|tpl|getHostByName|unixEpoch)\b").map_err(|_|"template checker unavailable")?;
    deterministic_printf(text)?;
    // Whole-template scan cannot be terminated by quoted delimiters or comments.
    if forbidden.is_match(text) {
        return Err("chart uses time/random/lookup/dynamic tpl or generated credentials; lifecycle requires deterministic templates".into());
    }
    Ok(())
}
// Go's normal string formatting is useful for chart names. Dynamic formats
// and pointer formatting can generate different manifests across Helm processes.
fn deterministic_printf(text: &str) -> Result<()> {
    let calls = regex::Regex::new(r"\bprintf\b").map_err(|_| "template checker unavailable")?;
    let pointer =
        regex::Regex::new(r"%[^a-zA-Z%]*p").map_err(|_| "template checker unavailable")?;
    for call in calls.find_iter(text) {
        let rest = text[call.end()..].trim_start();
        let mut chars = rest.chars();
        let quote = chars.next().ok_or("printf format missing")?;
        if !matches!(quote, '"' | '`') {
            return Err("deterministic chart review requires a literal printf format".into());
        }
        let mut format = String::new();
        let mut ended = false;
        while let Some(c) = chars.next() {
            if c == quote {
                ended = true;
                break;
            }
            if quote == '"' && c == '\\' {
                let escaped = chars.next().ok_or("incomplete printf string escape")?;
                format.push(match escaped{'n'=>'\n','r'=>'\r','t'=>'\t','\\'=>'\\','"'=>'"',_=>return Err("deterministic printf format cannot contain encoded hex/octal/Unicode escapes".into())});
            } else {
                format.push(c);
            }
        }
        if !ended || pointer.is_match(&format) {
            return Err(
                "pointer/unterminated printf formats are not deterministic across Helm processes"
                    .into(),
            );
        }
    }
    Ok(())
}
#[cfg(test)]
fn parse_resources(manifest: &str, namespace: &str) -> Result<Vec<Resource>> {
    parse_resources_with(manifest, namespace, &[])
}
fn parse_resources_with(
    manifest: &str,
    namespace: &str,
    bindings: &[crate::helm_crds::Binding],
) -> Result<Vec<Resource>> {
    if manifest.len() > 4 * 1024 * 1024 {
        return Err("rendered manifest exceeds 4 MiB".into());
    }
    let options = serde_saphyr::options! {budget:serde_saphyr::budget!{max_events:200_000,max_aliases:0,max_anchors:0,max_depth:64,max_inclusion_depth:0,max_documents:MAX_RESOURCES,max_nodes:100_000,max_total_scalar_bytes:4*1024*1024,max_total_comment_bytes:1024*1024,max_merge_keys:0},merge_keys:serde_saphyr::MergeKeyPolicy::Error,duplicate_keys:serde_saphyr::DuplicateKeyPolicy::Error,with_snippet:false};
    let docs:Vec<Value>=serde_saphyr::from_multiple_with_options(manifest,options).map_err(|_|"rendered manifest is malformed, uses YAML indirection or exceeds review limits; contents suppressed")?;
    let mut resources = BTreeMap::new();
    for mut object in docs {
        if object.is_null() {
            continue;
        }
        if !object.is_object()
            || !object["metadata"].is_object()
            || ["labels", "annotations"].iter().any(|key| {
                !object["metadata"][key].is_null() && !object["metadata"][key].is_object()
            })
        {
            return Err("rendered object has malformed metadata".into());
        }
        let api = object["apiVersion"]
            .as_str()
            .ok_or("manifest has no apiVersion")?
            .to_owned();
        let kind = object["kind"]
            .as_str()
            .ok_or("manifest has no kind")?
            .to_owned();
        let name = object["metadata"]["name"]
            .as_str()
            .filter(|name| valid_object_name(name))
            .ok_or(
                "every resource needs an explicit valid metadata.name; generateName is unsupported",
            )?
            .to_owned();
        let resource_api = resource_api(&api, &kind).or_else(|_| bindings.iter().find_map(|binding| binding.definition.resource_api(&api, &kind)).ok_or_else(|| "custom resource requires --allow-helm-crds, an Established sealed Namespaced CRD, and a fresh review".to_owned()))?;
        let ns = object["metadata"]["namespace"]
            .as_str()
            .filter(|ns| !ns.is_empty())
            .unwrap_or(namespace);
        if ns != namespace {
            return Err("chart attempts a different namespace; blocked".into());
        }
        object["metadata"]["namespace"] = json!(namespace);
        check_gitops(&object)?;
        if object["metadata"]["annotations"]
            .get("helm.sh/hook")
            .is_some()
        {
            return Err(
                "Helm hooks are outside the reviewed resource set and are forbidden".into(),
            );
        }
        let identity = Identity {
            api_version: api,
            kind,
            namespace: namespace.into(),
            name,
        };
        if resources
            .insert(
                identity.clone(),
                Resource {
                    identity,
                    object,
                    api: resource_api,
                },
            )
            .is_some()
        {
            return Err("duplicate rendered resource identity".into());
        }
    }
    if resources.len() > MAX_RESOURCES {
        return Err("chart exceeds 128 affected resources".into());
    }
    Ok(resources.into_values().collect())
}
fn valid_object_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 253 && name.split('.').all(safety::valid_id)
}
fn resource_api(api: &str, kind: &str) -> Result<ApiResource> {
    let plural=match(api,kind){("v1","Secret")=>"secrets",("v1","ConfigMap")=>"configmaps",("v1","Service")=>"services",("v1","ServiceAccount")=>"serviceaccounts",("v1","PersistentVolumeClaim")=>"persistentvolumeclaims",("v1","Pod")=>"pods",("apps/v1","Deployment")=>"deployments",("apps/v1","StatefulSet")=>"statefulsets",("apps/v1","DaemonSet")=>"daemonsets",("batch/v1","Job")=>"jobs",("batch/v1","CronJob")=>"cronjobs",("networking.k8s.io/v1","Ingress")=>"ingresses",("networking.k8s.io/v1","NetworkPolicy")=>"networkpolicies",("autoscaling/v2","HorizontalPodAutoscaler")=>"horizontalpodautoscalers",("policy/v1","PodDisruptionBudget")=>"poddisruptionbudgets",("rbac.authorization.k8s.io/v1","Role")=>"roles",("rbac.authorization.k8s.io/v1","RoleBinding")=>"rolebindings",_=>return Err("chart contains an unsupported or cluster-scoped resource; lifecycle allows ordinary namespaced v1/apps/batch/networking/autoscaling/policy/RBAC resources only".into())};
    let (group, version) = api.split_once('/').unwrap_or(("", api));
    Ok(ApiResource {
        group: group.into(),
        version: version.into(),
        api_version: api.into(),
        kind: kind.into(),
        plural: plural.into(),
    })
}
fn check_gitops(object: &Value) -> Result<()> {
    for field in ["labels", "annotations"] {
        let Some(map) = object["metadata"][field].as_object() else {
            continue;
        };
        let marker = map.keys().any(|key| {
            [
                "argocd.argoproj.io/",
                "kustomize.toolkit.fluxcd.io/",
                "helm.toolkit.fluxcd.io/",
                "fluxcd.io/",
            ]
            .iter()
            .any(|prefix| key.starts_with(prefix))
        });
        let manager = map
            .get("app.kubernetes.io/managed-by")
            .and_then(Value::as_str)
            .is_some_and(|value| {
                ["argocd", "flux", "fluxcd"].contains(&value.to_ascii_lowercase().as_str())
            });
        if marker || manager {
            return Err(
                "GitOps-managed resource: change its source instead of Helm lifecycle".into(),
            );
        }
    }
    Ok(())
}
fn canonical(resources: &[Resource]) -> Result<String> {
    serde_json::to_string(
        &resources
            .iter()
            .map(|r| (&r.identity, &r.object))
            .collect::<Vec<_>>(),
    )
    .map_err(|_| "cannot seal reviewed resources".into())
}
fn history_seal(snapshot: &helm::Snapshot) -> Result<Vec<(u32, String, String, String)>> {
    if snapshot.truncated || !snapshot.issues.is_empty() {
        return Err("Helm history is incomplete; lifecycle preflight blocked".into());
    }
    let mut seal = snapshot
        .releases
        .iter()
        .map(|r| {
            (
                r.revision,
                r.uid.clone(),
                r.resource_version.clone(),
                r.status.clone(),
            )
        })
        .collect::<Vec<_>>();
    seal.sort();
    Ok(seal)
}
fn ownership(object: &Value, options: &Options) -> Result<()> {
    check_gitops(object)?;
    let annotations = &object["metadata"]["annotations"];
    if annotations["meta.helm.sh/release-name"] != options.release
        || annotations["meta.helm.sh/release-namespace"] != options.namespace
        || object["metadata"]["labels"]["app.kubernetes.io/managed-by"] != "Helm"
    {
        return Err(
            "existing resource is not owned by this exact Helm release; adoption is forbidden"
                .into(),
        );
    }
    Ok(())
}
async fn resource_seals(
    client: Client,
    resources: &[Resource],
    options: &Options,
    retain_objects: bool,
) -> Result<(Vec<Seal>, Vec<Value>)> {
    let mut seals = Vec::new();
    let mut existing = Vec::new();
    let mut current_bytes = 0usize;
    for resource in resources {
        let api: Api<DynamicObject> =
            Api::namespaced_with(client.clone(), &options.namespace, &resource.api);
        let object = tokio::time::timeout(
            Duration::from_secs(10),
            api.get_opt(&resource.identity.name),
        )
        .await
        .map_err(|_| "resource preflight timed out")?
        .map_err(|_| "resource preflight denied or failed; server details suppressed")?;
        if let Some(object) = object {
            let value =
                serde_json::to_value(&object).map_err(|_| "cannot inspect current resource")?;
            current_bytes = current_bytes
                .checked_add(
                    serde_json::to_vec(&value)
                        .map_err(|_| "cannot measure current resource")?
                        .len(),
                )
                .ok_or("current resource size overflow")?;
            if current_bytes > 8 * 1024 * 1024 {
                return Err("current affected resources exceed8 MiB aggregate; split the release before lifecycle review".into());
            }
            if value["apiVersion"] != resource.identity.api_version
                || value["kind"] != resource.identity.kind
                || value["metadata"]["namespace"] != resource.identity.namespace
                || value["metadata"]["name"] != resource.identity.name
            {
                return Err("resource preflight returned a different identity/scope; no lifecycle write sent".into());
            }
            ownership(&value, options)?;
            let uid = object
                .metadata
                .uid
                .filter(|uid| !uid.is_empty())
                .ok_or("resource lacks UID")?;
            let version = object
                .metadata
                .resource_version
                .filter(|rv| !rv.is_empty())
                .ok_or("resource lacks resourceVersion")?;
            seals.push(Seal {
                identity: resource.identity.clone(),
                uid: Some(uid),
                version: Some(version),
            });
            if retain_objects {
                existing.push(value);
            }
        } else {
            seals.push(Seal {
                identity: resource.identity.clone(),
                uid: None,
                version: None,
            });
        }
    }
    Ok((seals, existing))
}

#[derive(Clone)]
struct ProcessContext {
    tool: Tool,
    root: PathBuf,
    connection: StaticConnection,
    storage: Storage,
}
struct Capture {
    stdout: Vec<u8>,
}
struct CancelGuard(Arc<AtomicBool>, Arc<AtomicBool>);
impl Drop for CancelGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
        // Dropped async prepare/validation futures must finish bounded group
        // cleanup before their private workspace is removed.
        let until = Instant::now() + Duration::from_secs(2);
        while !self.1.load(Ordering::Acquire) && Instant::now() < until {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}
struct Completed(Arc<AtomicBool>);
impl Drop for Completed {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
async fn command(
    context: &ProcessContext,
    args: Vec<String>,
    seconds: u16,
    cancel: Option<tokio::sync::oneshot::Receiver<()>>,
) -> Result<Capture> {
    let mut cancel = cancel;
    command_with_cancel(context, args, seconds, &mut cancel).await
}
async fn command_with_cancel(
    context: &ProcessContext,
    args: Vec<String>,
    seconds: u16,
    cancel: &mut Option<tokio::sync::oneshot::Receiver<()>>,
) -> Result<Capture> {
    let flag = Arc::new(AtomicBool::new(false));
    let complete = Arc::new(AtomicBool::new(false));
    let _guard = CancelGuard(flag.clone(), complete.clone());
    let background_flag = flag.clone();
    let context = context.clone();
    let mut worker = tokio::task::spawn_blocking(move || {
        let _complete = Completed(complete);
        supervise(
            &context,
            &args,
            Duration::from_secs(u64::from(seconds)),
            background_flag,
        )
    });
    if let Some(cancel) = cancel.as_mut() {
        tokio::select! {result=&mut worker=>result.map_err(|_|"Helm supervision worker failed; outcome unknown".to_owned())?,_=cancel=>{flag.store(true,Ordering::Release);worker.await.map_err(|_|"Helm cancellation worker failed; outcome unknown".to_owned())?}}
    } else {
        worker
            .await
            .map_err(|_| "Helm supervision worker failed; outcome unknown".to_owned())?
    }
}
#[cfg(not(unix))]
fn supervise(_: &ProcessContext, _: &[String], _: Duration, _: Arc<AtomicBool>) -> Result<Capture> {
    Err("Helm lifecycle process cleanup is unsupported on this platform".into())
}
#[cfg(unix)]
fn supervise(
    context: &ProcessContext,
    args: &[String],
    budget: Duration,
    cancel: Arc<AtomicBool>,
) -> Result<Capture> {
    use std::{
        os::unix::process::CommandExt,
        process::{Command, Stdio},
        sync::mpsc,
        thread,
    };
    if cancel.load(Ordering::Acquire) {
        return Err("Helm operation canceled before spawn".into());
    }
    context.tool.check()?;
    context.connection.gate.check_live()?;
    let killer = Path::new("/bin/kill")
        .canonicalize()
        .map_err(|_| "OS process-group cleanup unavailable")?;
    let _ = Tool::new(&killer)?;
    let mut cmd = Command::new(&context.tool.path);
    cmd.args(args)
        .env_clear()
        .current_dir(&context.root)
        .env("HOME", context.root.join("home"))
        .env("TMPDIR", &context.root)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .env("HELM_CACHE_HOME", context.root.join("cache"))
        .env("HELM_CONFIG_HOME", context.root.join("config"))
        .env("HELM_DATA_HOME", context.root.join("data"))
        .env("HELM_PLUGINS", context.root.join("plugins"))
        .env(
            "HELM_DRIVER",
            match context.storage {
                Storage::Secret => "secret",
                Storage::ConfigMap => "configmap",
            },
        )
        .env("KUBECONFIG", context.root.join("connection.json"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    if cancel.load(Ordering::Acquire) {
        return Err("Helm operation canceled before spawn".into());
    }
    let child = cmd
        .spawn()
        .map_err(|_| "cannot start trusted Helm process")?;
    struct Process {
        child: std::process::Child,
        killer: PathBuf,
        cleaned: bool,
    }
    impl Process {
        fn cleanup(&mut self) -> Result<()> {
            if self.cleaned {
                return Ok(());
            }
            let until = Instant::now() + Duration::from_secs(1);
            let mut failure = None;
            match Command::new(&self.killer)
                .env_clear()
                .current_dir("/")
                .args(["-KILL", "--", &format!("-{}", self.child.id())])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(mut signal) => loop {
                    match signal.try_wait() {
                        Ok(Some(_)) => break,
                        Ok(None) if Instant::now() < until => {
                            thread::sleep(Duration::from_millis(5))
                        }
                        _ => {
                            let _ = signal.kill();
                            let _ = signal.try_wait();
                            failure = Some("Helm process-group signal cleanup failed");
                            break;
                        }
                    }
                },
                Err(_) => failure = Some("cannot signal Helm process group"),
            }
            // Direct-child termination/reaping is required even if signalling
            // the group failed. Reserve its own finite cleanup deadline.
            let _ = self.child.kill();
            let reap_until = Instant::now() + Duration::from_secs(1);
            loop {
                match self.child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < reap_until => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    _ => {
                        failure = Some("cannot verify Helm process reaping");
                        break;
                    }
                }
            }
            self.cleaned = true;
            failure.map_or(Ok(()), |message| Err(message.into()))
        }
    }
    impl Drop for Process {
        fn drop(&mut self) {
            if self.cleanup().is_err() {
                let _ = self.child.kill();
                let _ = self.child.try_wait();
            }
        }
    }
    let mut process = Process {
        child,
        killer,
        cleaned: false,
    };
    enum Event {
        Bytes(bool, Vec<u8>),
        Done,
        Failed,
    }
    fn reader(
        mut stream: impl Read + Send + 'static,
        err: bool,
        tx: mpsc::SyncSender<Event>,
    ) -> thread::JoinHandle<()> {
        thread::spawn(move || {
            let mut buf = [0; 8192];
            loop {
                match stream.read(&mut buf) {
                    Ok(0) => {
                        let _ = tx.send(Event::Done);
                        break;
                    }
                    Ok(n) => {
                        if tx.send(Event::Bytes(err, buf[..n].to_vec())).is_err() {
                            break;
                        }
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                    Err(_) => {
                        let _ = tx.send(Event::Failed);
                        break;
                    }
                }
            }
        })
    }
    let (tx, rx) = mpsc::sync_channel(16);
    let out = reader(
        process
            .child
            .stdout
            .take()
            .ok_or("Helm stdout unavailable")?,
        false,
        tx.clone(),
    );
    let err = reader(
        process
            .child
            .stderr
            .take()
            .ok_or("Helm stderr unavailable")?,
        true,
        tx,
    );
    let start = Instant::now();
    let mut stdout = Vec::new();
    let mut stderr_capture = Vec::new();
    let mut consumed = 0usize;
    let mut done = 0;
    let mut status = None;
    let mut failure = None;
    while done < 2 || status.is_none() {
        if cancel.load(Ordering::Acquire) {
            failure = Some("Helm operation canceled; execution may be partial");
            break;
        }
        if start.elapsed() >= budget {
            failure = Some("Helm operation deadline exceeded; execution may be partial");
            break;
        }
        if context.connection.gate.check_live().is_err() {
            failure = Some(
                "validated credentials expired or were revoked; Helm stopped; execution may be partial",
            );
            break;
        }
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(Event::Bytes(stderr, bytes)) => {
                consumed += bytes.len();
                if consumed > MAX_OUTPUT {
                    failure = Some("Helm output exceeded 8 MiB; execution may be partial");
                    break;
                }
                if !stderr {
                    stdout.extend(bytes);
                } else {
                    stderr_capture.extend(bytes);
                }
            }
            Ok(Event::Done) => done += 1,
            Ok(Event::Failed) => {
                failure = Some("Helm output capture failed; execution may be partial");
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) if done < 2 => {
                failure = Some("Helm output stopped unexpectedly");
                break;
            }
            Err(_) => thread::sleep(Duration::from_millis(5)),
        }
        if status.is_none() {
            status = process
                .child
                .try_wait()
                .map_err(|_| "cannot inspect Helm process")?;
            if status.is_some() {
                process.cleanup()?;
            }
        }
    }
    let cleanup = process.cleanup();
    drop(rx);
    let until = Instant::now() + Duration::from_millis(500);
    while !(out.is_finished() && err.is_finished()) && Instant::now() < until {
        thread::sleep(Duration::from_millis(5));
    }
    if !out.is_finished() || !err.is_finished() {
        return Err("Helm capture cleanup could not be verified; outcome unknown".into());
    }
    let _ = out.join();
    let _ = err.join();
    cleanup?;
    if let Some(error) = failure {
        return Err(error.into());
    }
    if !status.is_some_and(|status| status.success()) {
        return Err(format!(
            "Helm reported {}; raw diagnostics suppressed; reconcile release/resources before retry",
            classify_helm_failure(&stderr_capture)
        ));
    }
    Ok(Capture { stdout })
}
fn classify_helm_failure(bytes: &[u8]) -> &'static str {
    let text = String::from_utf8_lossy(bytes).to_ascii_lowercase();
    if text.contains("forbidden") || text.contains("unauthorized") {
        "an authorization refusal (check the selected identity's RBAC)"
    } else if text.contains("conflict") {
        "a resource ownership or apply conflict"
    } else if text.contains("timed out") || text.contains("deadline exceeded") {
        "a readiness or API timeout"
    } else if text.contains("already exists") || text.contains("re-use a name") {
        "an existing-resource or release-name conflict"
    } else if text.contains("dependenc") {
        "missing or invalid bundled chart dependencies"
    } else if text.contains("template") || text.contains("yaml parse") {
        "a chart template/values validation failure"
    } else if text.contains("connection refused") || text.contains("x509") {
        "an API connection or TLS failure"
    } else {
        "an operation failure"
    }
}
fn api_failure(error: kube::Error, stage: &str) -> String {
    let detail = match error {
        kube::Error::Api(status) => match status.code {
            401 | 403 => "authorization denied; check RBAC",
            404 => "resource disappeared",
            409 => "ownership/resourceVersion/apply conflict",
            422 => "admission/schema validation refused",
            429 => "API throttled the request",
            500..=599 => "API server unavailable",
            _ => "API rejected the request",
        },
        _ => "transport/response failed",
    };
    format!("{stage}: {detail}; server details suppressed")
}
fn common(options: &Options, root: &Path) -> Vec<String> {
    vec![
        "--kubeconfig".into(),
        root.join("connection.json").to_string_lossy().into(),
        "--kube-context".into(),
        "reviewed".into(),
        "--namespace".into(),
        options.namespace.clone(),
        "--color=never".into(),
    ]
}
fn lifecycle_args(options: &Options, request: &Request, root: &Path, dry: bool) -> Vec<String> {
    let mut args = vec![request.name().into(), options.release.clone()];
    match request {
        Request::Install { .. } | Request::Upgrade { .. } => {
            args.push(root.join("chart.tgz").to_string_lossy().into());
            args.extend([
                "--output=json".into(),
                "--hide-notes".into(),
                "--skip-crds".into(),
            ]);
            if matches!(request, Request::Upgrade { .. }) {
                args.push("--reset-then-reuse-values".into());
            }
            if root.join("values.yaml").exists() {
                args.extend([
                    "--values".into(),
                    root.join("values.yaml").to_string_lossy().into(),
                ]);
            }
        }
        Request::InstallCrds { .. } | Request::UpgradeCrds { .. } | Request::DeleteCrds { .. } => {
            return vec![];
        }
        Request::Test => {}
        Request::Rollback { revision } => args.push(revision.to_string()),
        Request::Uninstall => args.extend(["--cascade=foreground".into()]),
    }
    if !options.allow_hooks && !matches!(request, Request::Test) {
        args.push("--no-hooks".into());
    }
    args.push(format!("--timeout={}s", options.seconds));
    if dry {
        args.push(
            if matches!(request, Request::Uninstall) {
                "--dry-run"
            } else {
                "--dry-run=server"
            }
            .into(),
        );
    } else if !matches!(request, Request::Test) {
        args.push("--wait=watcher".into());
    }
    args.extend(common(options, root));
    args
}
async fn get_manifest(
    context: &ProcessContext,
    options: &Options,
    revision: u32,
) -> Result<String> {
    let mut args = vec![
        "get".into(),
        "manifest".into(),
        options.release.clone(),
        "--revision".into(),
        revision.to_string(),
    ];
    args.extend(common(options, &context.root));
    let capture = command(context, args, 30, None).await?;
    String::from_utf8(capture.stdout).map_err(|_| "stored Helm manifest is not UTF-8".into())
}
fn operation(request: &Request) -> crate::helm_extensions::Operation {
    use crate::helm_extensions::Operation;
    match request {
        Request::Install { .. } | Request::InstallCrds { .. } => Operation::Install,
        Request::Upgrade { .. } | Request::UpgradeCrds { .. } => Operation::Upgrade,
        Request::DeleteCrds { .. } => Operation::Uninstall,
        Request::Test => Operation::Test,
        Request::Rollback { .. } => Operation::Rollback,
        Request::Uninstall => Operation::Uninstall,
    }
}
async fn stored_hooks(
    gateway: &Gateway,
    options: &Options,
    selected: &Release,
    request: &Request,
) -> Result<crate::helm_extensions::HookPlan> {
    if selected.name != options.release
        || selected.namespace != options.namespace
        || selected.storage != options.storage
    {
        return Err("stored hook selection escaped the reviewed release identity".into());
    }
    let raw = helm::lifecycle_hooks(gateway, selected).await?;
    crate::helm_extensions::hooks(
        &raw,
        &options.namespace,
        &options.release,
        operation(request),
        options.allow_hooks,
    )
}
async fn parse_resolved(
    manifest: &str,
    options: &Options,
    client: &Client,
    bindings: &mut Vec<crate::helm_crds::Binding>,
) -> Result<Vec<Resource>> {
    for object in crate::helm_extensions::documents(manifest, MAX_RESOURCES)? {
        let api = object["apiVersion"]
            .as_str()
            .ok_or("manifest apiVersion missing")?;
        let kind = object["kind"].as_str().ok_or("manifest kind missing")?;
        if resource_api(api, kind).is_ok()
            || bindings
                .iter()
                .any(|binding| binding.definition.resource_api(api, kind).is_some())
        {
            continue;
        }
        if !options.allow_crds {
            return Err("custom resources require --allow-helm-crds and an already Established reviewed Namespaced CRD; run separate CRD bootstrap first".into());
        }
        if bindings.len() >= crate::helm_extensions::MAX_CRDS {
            return Err("chart exceeds16 custom-resource API bindings".into());
        }
        bindings.push(crate::helm_crds::resolve(client, api, kind).await?);
    }
    parse_resources_with(manifest, &options.namespace, bindings)
}
async fn dry_plan(
    gateway: &Gateway,
    context: &ProcessContext,
    options: &Options,
    request: &Request,
    client: &Client,
    bindings: &mut Vec<crate::helm_crds::Binding>,
    selected_hooks: Option<&Release>,
) -> Result<(Vec<Resource>, crate::helm_extensions::HookPlan)> {
    if matches!(request, Request::Test) {
        let hooks = stored_hooks(
            gateway,
            options,
            selected_hooks.ok_or("test release selection missing")?,
            request,
        )
        .await?;
        if hooks.active().is_empty() {
            return Err("selected release has no stored test/test-success hooks".into());
        }
        return Ok((vec![], hooks));
    }
    let capture = command(
        context,
        lifecycle_args(options, request, &context.root, true),
        30,
        None,
    )
    .await?;
    match request {
        Request::Install { .. } | Request::Upgrade { .. } => {
            let release: Value = serde_json::from_slice(&capture.stdout).map_err(
                |_| "Helm dry-run did not return bounded release JSON; diagnostics suppressed",
            )?;
            let hooks = crate::helm_extensions::hooks(
                &release["hooks"],
                &options.namespace,
                &options.release,
                operation(request),
                options.allow_hooks,
            )?;
            let resources = parse_resolved(
                release["manifest"]
                    .as_str()
                    .ok_or("Helm dry-run has no rendered manifest")?,
                options,
                client,
                bindings,
            )
            .await?;
            Ok((resources, hooks))
        }
        Request::Rollback { revision } => {
            let selected = selected_hooks
                .filter(|selected| selected.revision == *revision)
                .ok_or("rollback hook revision selection missing or changed")?;
            Ok((
                parse_resolved(
                    &get_manifest(context, options, *revision).await?,
                    options,
                    client,
                    bindings,
                )
                .await?,
                stored_hooks(gateway, options, selected, request).await?,
            ))
        }
        Request::Uninstall => Ok((
            vec![],
            stored_hooks(
                gateway,
                options,
                selected_hooks.ok_or("current hook revision selection missing")?,
                request,
            )
            .await?,
        )),
        Request::InstallCrds { .. }
        | Request::UpgradeCrds { .. }
        | Request::DeleteCrds { .. }
        | Request::Test => Err("native schema/test plan does not render release resources".into()),
    }
}
fn canonical_plan(
    resources: &[Resource],
    hooks: &crate::helm_extensions::HookPlan,
) -> Result<String> {
    serde_json::to_string(&(canonical(resources)?, &hooks.canonical))
        .map_err(|_| "cannot seal full Helm plan".into())
}
async fn hook_preflight(
    client: &Client,
    plan: &crate::helm_extensions::HookPlan,
    prior: &crate::helm_extensions::HookPlan,
    options: &Options,
    dry_run: bool,
) -> Result<(Vec<Seal>, Vec<Value>)> {
    use kube::api::{DeleteParams, Patch, PatchParams, PostParams, Preconditions};
    let active = plan
        .active()
        .into_iter()
        .map(|hook| (hook.identity.clone(), hook))
        .collect::<BTreeMap<_, _>>();
    let mut seals = Vec::new();
    let mut objects = Vec::new();
    let mut bytes = 0usize;
    for hook in active.into_values() {
        let api: Api<DynamicObject> = Api::namespaced_with(
            client.clone(),
            &options.namespace,
            &crate::helm_extensions::native_api(&hook.identity.api_version, &hook.identity.kind)?,
        );
        let current = api
            .get_opt(&hook.identity.name)
            .await
            .map_err(|error| api_failure(error, "hook identity preflight"))?;
        let identity = Identity {
            api_version: hook.identity.api_version.clone(),
            kind: hook.identity.kind.clone(),
            namespace: hook.identity.namespace.clone(),
            name: hook.identity.name.clone(),
        };
        let (uid, version) = if let Some(current) = current {
            let object =
                serde_json::to_value(&current).map_err(|_| "cannot inspect hook resource")?;
            bytes = bytes
                .checked_add(
                    serde_json::to_vec(&object)
                        .map_err(|_| "cannot measure hook")?
                        .len(),
                )
                .filter(|bytes| *bytes <= 8 * 1024 * 1024)
                .ok_or("existing hooks exceed8 MiB")?;
            ownership(&object, options)?;
            if !hook.deletes_before_creation()
                || !prior
                    .all
                    .iter()
                    .any(|old| old.identity == hook.identity && old.matches_existing(&object))
            {
                return Err("existing hook name does not match a stored authorized hook template with before-hook-creation policy; foreign adoption/deletion refused".into());
            }
            let uid = current
                .metadata
                .uid
                .filter(|uid| !uid.is_empty())
                .ok_or("hook UID missing")?;
            let version = current
                .metadata
                .resource_version
                .filter(|version| !version.is_empty())
                .ok_or("hook resourceVersion missing")?;
            if dry_run {
                api.delete(
                    &hook.identity.name,
                    &DeleteParams {
                        dry_run: true,
                        preconditions: Some(Preconditions {
                            uid: Some(uid.clone()),
                            resource_version: Some(version.clone()),
                        }),
                        ..Default::default()
                    },
                )
                .await
                .map_err(|error| api_failure(error, "hook delete dry-run"))?;
                api.patch(&hook.identity.name,&PatchParams::apply("helm").dry_run(),&Patch::Apply(&hook.object)).await.map_err(|error|api_failure(error,"hook replacement admission dry-run (changed immutable fields need a new hook name)"))?;
            }
            objects.push(object);
            (Some(uid), Some(version))
        } else {
            if dry_run {
                // Authorization is evaluated even for an absent named object.
                // This cannot simulate admission of a future completed Job.
                if hook.deletes_before_creation()
                    || hook.deletes_on_success()
                    || hook.deletes_on_failure()
                {
                    match api
                        .delete(
                            &hook.identity.name,
                            &DeleteParams {
                                dry_run: true,
                                ..Default::default()
                            },
                        )
                        .await
                    {
                        Ok(_) => {}
                        Err(kube::Error::Api(status)) if status.code == 404 => {}
                        Err(error) => {
                            return Err(api_failure(
                                error,
                                "future hook delete authorization dry-run",
                            ));
                        }
                    }
                }
                let object: DynamicObject = serde_json::from_value(hook.object.clone())
                    .map_err(|_| "invalid hook object")?;
                api.create(
                    &PostParams {
                        dry_run: true,
                        ..Default::default()
                    },
                    &object,
                )
                .await
                .map_err(|error| api_failure(error, "hook create dry-run"))?;
            }
            (None, None)
        };
        seals.push(Seal {
            identity,
            uid,
            version,
        });
    }
    Ok((seals, objects))
}
async fn admission(
    client: Client,
    desired: &[Resource],
    affected: &[Resource],
    seals: &[Seal],
    options: &Options,
    uninstall: bool,
) -> Result<()> {
    use kube::api::{DeleteParams, Patch, PatchParams, PostParams, Preconditions};
    let desired_ids = desired.iter().map(|r| &r.identity).collect::<BTreeSet<_>>();
    for resource in affected {
        let api: Api<DynamicObject> =
            Api::namespaced_with(client.clone(), &options.namespace, &resource.api);
        let seal = seals
            .iter()
            .find(|seal| seal.identity == resource.identity)
            .ok_or("resource identity seal missing")?;
        if uninstall || !desired_ids.contains(&resource.identity) {
            if let (Some(uid), Some(version)) = (&seal.uid, &seal.version) {
                if resource.object["metadata"]["annotations"]["helm.sh/resource-policy"] == "keep" {
                    continue;
                }
                tokio::time::timeout(
                    Duration::from_secs(10),
                    api.delete(
                        &resource.identity.name,
                        &DeleteParams {
                            dry_run: true,
                            preconditions: Some(Preconditions {
                                uid: Some(uid.clone()),
                                resource_version: Some(version.clone()),
                            }),
                            ..Default::default()
                        },
                    ),
                )
                .await
                .map_err(|_| "server delete dry-run timed out")?
                .map_err(|error| api_failure(error, "server delete dry-run"))?;
            }
            continue;
        }
        let mut object = resource.object.clone();
        object["metadata"]["labels"]["app.kubernetes.io/managed-by"] = json!("Helm");
        object["metadata"]["annotations"]["meta.helm.sh/release-name"] = json!(options.release);
        object["metadata"]["annotations"]["meta.helm.sh/release-namespace"] =
            json!(options.namespace);
        if seal.uid.is_some() {
            tokio::time::timeout(
                Duration::from_secs(10),
                api.patch(
                    &resource.identity.name,
                    &PatchParams::apply("helm").dry_run(),
                    &Patch::Apply(&object),
                ),
            )
            .await
            .map_err(|_| "server apply dry-run timed out")?
            .map_err(|error| api_failure(error, "server apply dry-run"))?;
        } else {
            let object: DynamicObject =
                serde_json::from_value(object).map_err(|_| "cannot validate desired object")?;
            tokio::time::timeout(
                Duration::from_secs(10),
                api.create(
                    &PostParams {
                        dry_run: true,
                        ..Default::default()
                    },
                    &object,
                ),
            )
            .await
            .map_err(|_| "server create dry-run timed out")?
            .map_err(|error| api_failure(error, "server create dry-run"))?;
        }
    }
    Ok(())
}

pub async fn prepare(gateway: &Gateway, options: Options, request: Request) -> Result<Prepared> {
    tokio::time::timeout(
        Duration::from_secs(120),
        prepare_inner(gateway, options, request),
    )
    .await
    .map_err(|_| "Helm preparation exceeded 120 seconds; no lifecycle action sent".to_owned())?
}
async fn prepare_inner(gateway: &Gateway, options: Options, request: Request) -> Result<Prepared> {
    namespace_allowed(&options.namespace)?;
    if !helm::valid_release_name(&options.release) || !(1..=900).contains(&options.seconds) {
        return Err("Helm needs a valid release name and timeout 1..900 seconds".into());
    }
    if matches!(request, Request::Rollback { revision: 0 }) {
        return Err("rollback needs an explicit positive revision".into());
    }
    gateway.helm_extension_access(options.allow_hooks, options.allow_crds)?;
    if schema_request(&request) && !options.allow_crds {
        return Err("CRD bootstrap requires --allow-helm-crds".into());
    }
    if matches!(request, Request::Test) && !options.allow_hooks {
        return Err("Helm tests require --allow-helm-hooks".into());
    }
    let session = gateway.helm_lifecycle_session()?.to_owned();
    if gateway.is_demo() {
        let selected = if matches!(
            request,
            Request::Install { .. }
                | Request::InstallCrds { .. }
                | Request::UpgradeCrds { .. }
                | Request::DeleteCrds { .. }
        ) {
            None
        } else {
            Some(helm::select_release(
                &helm::list_with_storage(gateway, &options.namespace, Some(options.storage))
                    .await?,
                &options.release,
                Some(options.storage),
            )?)
        };
        let rollback_target = if let Request::Rollback { revision } = &request {
            let current = selected.as_ref().ok_or("demo release missing")?;
            Some(
                helm::history(gateway, current)
                    .await?
                    .releases
                    .into_iter()
                    .find(|r| r.revision == *revision)
                    .ok_or("demo rollback target missing")?,
            )
        } else {
            None
        };
        let summary = format!(
            "DEMO / SIMULATED HELM {}\nContext: demo\nRelease: {}/{} | {} storage\nNo tool, chart, credentials or cluster requests are used.\nSIMULATED plan only; no real operation will occur.\nConfirmation label: {}/{}\nType {}{}/{} to simulate.\n",
            request.name(),
            options.namespace,
            options.release,
            options.storage.name(),
            options.namespace,
            options.release,
            if matches!(request, Request::DeleteCrds { .. }) {
                "DELETE CRDS "
            } else {
                ""
            },
            options.namespace,
            options.release
        );
        return Ok(Prepared {
            options,
            request,
            tool: None,
            workspace: None,
            connection: None,
            session,
            selected,
            rollback_target,
            history: vec![],
            resources: vec![],
            seals: vec![],
            canonical: String::new(),
            chart_hash: None,
            chart_source: None,
            values_hash: None,
            summary,
            created: Instant::now(),
            demo: true,
            hooks: Default::default(),
            hook_seals: vec![],
            crds: Default::default(),
            crd_seals: vec![],
            bindings: vec![],
            impact: vec![],
            review_context: gateway.context.clone(),
            test_runs: vec![],
            signature: None,
        });
    }
    let (client, connection, _) = gateway.helm_lifecycle_access()?;
    let tool = Tool::new(&options.executable)?;
    let workspace = Workspace::new()?;
    connection.write(&workspace.0.join("connection.json"))?;
    let context = ProcessContext {
        tool: tool.clone(),
        root: workspace.0.clone(),
        connection: connection.clone(),
        storage: options.storage,
    };
    let version = command(
        &context,
        vec!["version".into(), "--template={{.Version}}".into()],
        10,
        None,
    )
    .await?;
    let version =
        std::str::from_utf8(&version.stdout).map_err(|_| "invalid Helm version output")?;
    if version.trim().split('+').next() != Some("v4.3.1") {
        return Err(
            "Helm lifecycle currently requires a trusted Helm4.3.1 executable; other versions require deterministic-function review"
                .into(),
        );
    }
    let mut chart_hash = None;
    let mut chart_source = None;
    let mut values_hash = None;
    let mut signature = None;
    let input = match &request {
        Request::Install { chart, values } | Request::Upgrade { chart, values } => {
            Some((chart, values.as_ref()))
        }
        Request::InstallCrds { chart }
        | Request::UpgradeCrds { chart }
        | Request::DeleteCrds { chart } => Some((chart, None)),
        _ => None,
    };
    let mut crds = crate::helm_extensions::CrdPlan::default();
    if let Some((chart, values)) = input {
        let source = chart.to_str().ok_or("chart source must be UTF-8")?;
        let mut signed_inputs = None;
        if source.starts_with("oci://") {
            let reference = crate::helm_oci::Reference::parse(source)?;
            let acquired = crate::helm_oci::acquire(
                &reference,
                &options.registry,
                &workspace.0.join("chart.tgz"),
            )
            .await?;
            chart_source = Some(format!(
                "OCI source: {}\nResolved immutable reference: {}\nManifest digest: {}\nChart layer digest: {}\nConfig digest: {}\nDownloaded and sealed before review; no registry access during execution.",
                acquired.reference,
                acquired.resolved_reference,
                acquired.manifest_digest,
                acquired.chart_digest,
                acquired.config_digest
            ));
            if options.keyring.is_some() {
                signed_inputs=Some((acquired.chart_filename,acquired.provenance_path.ok_or("signature verification requested but OCI artifact has no Helm provenance layer")?));
            }
            chart_hash = Some(file_hash(&workspace.0.join("chart.tgz"), MAX_CHART)?);
        } else {
            if chart.extension().is_none_or(|extension| extension != "tgz") {
                return Err(
                    "Helm lifecycle requires a local .tgz or oci:// chart with explicit tag or sha256 digest".into(),
                );
            }
            chart_hash = Some(snapshot(chart, &workspace.0.join("chart.tgz"), MAX_CHART)?);
            if options.keyring.is_some() {
                signed_inputs = Some((
                    chart
                        .file_name()
                        .and_then(|name| name.to_str())
                        .ok_or("signed chart basename must be UTF-8")?
                        .to_owned(),
                    crate::helm_provenance::local_provenance_path(chart),
                ));
            }
        }
        if let (Some(keyring), Some((filename, provenance))) = (&options.keyring, signed_inputs) {
            let inputs = crate::helm_provenance::prepare(
                &workspace.0.join("chart.tgz"),
                &filename,
                &provenance,
                keyring,
                &workspace.0.join("signature"),
            )?;
            let verified = command(&context, inputs.argv(), 15, None).await?;
            let fingerprint = crate::helm_provenance::verified_fingerprint(&verified.stdout)?;
            signature = Some(SignatureReview {
                inputs,
                fingerprint,
            });
        }
        crds = inspect_chart(
            &safety::read_small(&workspace.0.join("chart.tgz"), MAX_CHART)?,
            options.allow_crds,
        )?;
        if let Some(values) = values {
            values_hash = Some(snapshot(
                values,
                &workspace.0.join("values.yaml"),
                1024 * 1024,
            )?);
        }
    }
    if schema_request(&request) {
        return prepare_crds(
            gateway,
            options,
            request,
            tool,
            workspace,
            connection,
            session,
            chart_hash,
            chart_source,
            signature,
            crds,
        )
        .await;
    }
    let crd_seals = crd_preflight(&client, &crds, false).await?;
    let mut bindings = Vec::new();
    let inventory = helm::list(gateway, &options.namespace).await?;
    if inventory.truncated || !inventory.issues.is_empty() {
        return Err("Helm lifecycle requires a complete Secret and ConfigMap inventory in the selected namespace; check storage read permissions".into());
    }
    let matches = inventory
        .releases
        .iter()
        .filter(|r| r.name == options.release)
        .collect::<Vec<_>>();
    if matches.iter().any(|r| r.storage != options.storage) {
        return Err(
            "release name exists in another Helm storage driver; lifecycle ownership is ambiguous"
                .into(),
        );
    }
    let selected = matches.first().map(|r| (*r).clone());
    if matches!(request, Request::Install { .. }) && selected.is_some() {
        return Err("release already exists; choose upgrade explicitly".into());
    }
    if !matches!(request, Request::Install { .. }) && selected.is_none() {
        return Err("release not found in the selected namespace/storage".into());
    }
    if !matches!(request, Request::Uninstall | Request::Test)
        && selected
            .as_ref()
            .is_some_and(|release| release.revision.checked_add(1).is_none())
    {
        return Err(
            "Helm release revision counter exhausted; lifecycle can only uninstall this release"
                .into(),
        );
    }
    let mut rollback_target = None;
    let history = if let Some(selected) = &selected {
        let snapshot = helm::history(gateway, selected).await?;
        if let Request::Rollback { revision } = &request {
            rollback_target = snapshot
                .releases
                .iter()
                .find(|r| r.revision == *revision)
                .cloned();
        }
        if matches!(request,Request::Rollback{revision}if !snapshot.releases.iter().any(|r|r.revision==revision))
        {
            return Err("requested rollback revision is missing".into());
        }
        if selected.status.starts_with("pending-") || selected.status == "uninstalling" {
            return Err(
                "release has a pending operation; reconcile it before lifecycle changes".into(),
            );
        }
        history_seal(&snapshot)?
    } else {
        vec![]
    };
    let test_runs = if matches!(request, Request::Test) {
        helm::lifecycle_test_results(gateway, selected.as_ref().ok_or("test release missing")?)
            .await?
    } else {
        vec![]
    };
    let old = if matches!(request, Request::Test) {
        vec![]
    } else if let Some(selected) = &selected {
        parse_resolved(
            &get_manifest(&context, &options, selected.revision).await?,
            &options,
            &client,
            &mut bindings,
        )
        .await?
    } else {
        vec![]
    };
    let prior_hooks = if let Some(selected) = &selected {
        stored_hooks(gateway, &options, selected, &request).await?
    } else {
        crate::helm_extensions::HookPlan::default()
    };
    let selected_hooks = if matches!(request, Request::Rollback { .. }) {
        rollback_target.as_ref()
    } else {
        selected.as_ref()
    };
    let (desired, hooks) = dry_plan(
        gateway,
        &context,
        &options,
        &request,
        &client,
        &mut bindings,
        selected_hooks,
    )
    .await?;
    let canonical = canonical_plan(&desired, &hooks)?;
    let mut affected = BTreeMap::new();
    for r in old.into_iter().chain(desired.iter().cloned()) {
        affected.insert(r.identity.clone(), r);
    }
    let resources = affected.into_values().collect::<Vec<_>>();
    if resources.len() > MAX_RESOURCES {
        return Err("operation affects more than 128 resources".into());
    }
    let active_hooks = hooks.active();
    if resources.len() + active_hooks.len() > MAX_RESOURCES
        || active_hooks.iter().any(|hook| {
            resources.iter().any(|resource| {
                resource.identity.api_version == hook.identity.api_version
                    && resource.identity.kind == hook.identity.kind
                    && resource.identity.name == hook.identity.name
            })
        })
    {
        return Err(
            "hook and ordinary resource identities collide or exceed128 affected resources".into(),
        );
    }
    let (hook_seals, existing_hooks) =
        hook_preflight(&client, &hooks, &prior_hooks, &options, true).await?;
    let (seals, existing) = resource_seals(client.clone(), &resources, &options, true).await?;
    admission(
        client.clone(),
        &desired,
        &resources,
        &seals,
        &options,
        matches!(request, Request::Uninstall),
    )
    .await?;
    let mut summary = format!(
        "REVIEW HELM {} / TRUSTED EXTERNAL HELM 4\nContext: {}\nVerified endpoint: {}\nRelease: {}/{} | {} storage\nCurrent release: {}\nExecutable: {}\nExecutable SHA-256: {}\nChart SHA-256: {}\nValues SHA-256: {}\nBudget: {} seconds; 8 MiB process output captured privately; no raw diagnostics shown.\nHooks require separate explicit capability and complete event/create/delete review. CRDs are retained create-only bootstrap objects; custom resources require sealed Established Namespaced CRDs. Cluster/system/cross-namespace/GitOps targets and foreign resource adoption forbidden.\nNo registry access during execution; no plugins, post-renderer, repository or dependency fetch. Upgrade resets chart defaults then reuses stored overrides plus the explicit values file.\nNative Helm server-connected simulation and per-resource API dry-run passed. These are not a transaction or readiness guarantee.\nHelm renders deterministic packaged input again; exact canonical plan, history and live UID/resourceVersion are rechecked before execution.\nHelm's own API requests do not carry the reviewed UID preconditions: a concurrent replacement remains a race.\nNo automatic rollback/retry. Cancellation, timeout, client failure and partial writes require reconciliation; remote changes may already have occurred.\nTrusted Helm retains your OS-user filesystem/network permissions and receives the private static credential; this is not an OS sandbox.\nKnown-field redaction can hide changed sensitive values; hashes bind exact chart/values and the complete private manifest. Custom values can still be confidential.\n\nAFFECTED RESOURCE IDENTITIES\n",
        request.name(),
        safety::safe_text(&gateway.context, 253),
        safety::safe_text(&gateway.server, 1024),
        options.namespace,
        options.release,
        options.storage.name(),
        selected
            .as_ref()
            .map(|r| format!(
                "revision {}; storage UID {}; RV {}",
                r.revision, r.uid, r.resource_version
            ))
            .unwrap_or_else(|| "absent (new release)".into()),
        options.executable.display(),
        tool.hash,
        chart_hash.as_deref().unwrap_or("stored release manifest"),
        values_hash.as_deref().unwrap_or("none / stored overrides"),
        options.seconds
    );
    for resource in &resources {
        summary.push_str(&format!(
            "{} {} {}/{}{}\n",
            resource.identity.api_version,
            resource.identity.kind,
            resource.identity.namespace,
            resource.identity.name,
            if resource.object["metadata"]["annotations"]["helm.sh/resource-policy"] == "keep" {
                " [Helm keep policy: retained on deletion]"
            } else {
                ""
            }
        ));
    }
    if let Some(signature) = &signature {
        summary.push_str(&signature.inputs.summary());
        summary.push_str(&format!(
            "Verified OpenPGP signer fingerprint: {}\n",
            signature.fingerprint
        ));
    } else {
        summary.push_str(unverified_archive_notice(
            &request,
            options.keyring.is_some(),
        ));
    }
    if let Some(source) = &chart_source {
        summary.push_str(&format!("\n{source}\n"));
    }
    summary.push_str(&format!("\nExact argv (private static connection path contains no credentials in argv): {:?}\nSource request: {}\n",lifecycle_args(&options,&request,&workspace.0,false),safety::safe_text(&format!("{request:?}"),16384)));
    if let Some(target) = &rollback_target {
        summary.push_str(&format!(
            "Rollback target revision {} | storage UID {} | RV {}\n",
            target.revision, target.uid, target.resource_version
        ));
    }
    summary.push_str("\nHelm storage writes are also authorized for this selected release: ");
    if matches!(request, Request::Uninstall) {
        summary.push_str("delete all captured revision records.\n");
    } else if matches!(request, Request::Test) {
        summary.push_str("update stored hook LastRun/test-suite results on the captured current revision; NO new release revision and NO automatic logs. Failed tests remain failed even though the release revision is unchanged.\n");
    } else {
        let next = match selected.as_ref() {
            Some(release) => release
                .revision
                .checked_add(1)
                .ok_or("Helm revision counter exhausted")?,
            None => 1,
        };
        summary.push_str(&format!(
            "create sh.helm.release.v1.{}.v{} and update current revision status when present.\n",
            options.release, next
        ));
    }
    if matches!(request, Request::Test) {
        summary=summary.replace("Native Helm server-connected simulation and per-resource API dry-run passed.","Test has no Helm dry-run: exact stored test/test-success hooks and their create/delete API admission were reviewed; no ordinary release resources are written.");
    }
    for (revision, uid, rv, status) in &history {
        summary.push_str(&format!(
            "Stored revision {} UID {} RV {} status {}\n",
            revision, uid, rv, status
        ));
    }
    summary.push_str("Controllers and Kubernetes garbage collection may create/update/delete dependent Pods, ReplicaSets, storage or other children; these are not individually UID-preconditioned by Helm.\n");
    summary.push_str(&hooks.review());
    summary
        .push_str("\nHOOK OBJECTS (known fields redacted; commands may have external effects)\n");
    for hook in &hooks.all {
        summary.push_str(&review_object(hook.object.clone())?);
        summary.push_str("\n---\n");
    }
    for (definition, seal) in crds.definitions.iter().zip(&crd_seals) {
        if let Some(binding) = seal {
            summary.push_str(&format!(
                "\nPACKAGED CRD RETAINED / SKIPPED BY HELM: {} UID {} RV {}; no update/delete\n",
                definition.name, binding.uid, binding.resource_version
            ));
            summary.push_str(&review_object(binding.definition.object.clone())?);
        }
    }
    for binding in &bindings {
        summary.push_str(&format!("\nSEALED CUSTOM API: {} UID {} RV {}; Namespaced, Established; CRD itself is never upgraded/deleted\n",binding.definition.name,binding.uid,binding.resource_version));
        if !crds
            .definitions
            .iter()
            .any(|definition| definition.name == binding.definition.name)
        {
            summary.push_str(&review_object(binding.definition.object.clone())?);
        }
    }
    summary.push_str("\nCURRENT HOOK OBJECTS (known fields redacted)\n");
    for object in existing_hooks {
        summary.push_str(&review_object(object)?);
        summary.push_str("\n---\n");
    }
    summary.push_str("\nCURRENT OBJECTS (known fields redacted)\n");
    for object in existing {
        summary.push_str(&review_object(object)?);
        summary.push_str("\n---\n");
    }
    summary.push_str("\nDESIRED RENDERED OBJECTS (known fields redacted; absent means delete)\n");
    for resource in &desired {
        summary.push_str(&review_object(resource.object.clone())?);
        summary.push_str("\n---\n");
    }
    summary.push_str(&format!(
        "\nExact private plan SHA-256: {}\nReview expires in 15 minutes. Type {}/{} to execute.\n",
        hash(canonical.as_bytes()),
        options.namespace,
        options.release
    ));
    if summary.len() > MAX_REVIEW {
        return Err(
            "complete redacted Helm review exceeds 512 KiB; split the chart before executing"
                .into(),
        );
    }
    Ok(Prepared {
        options,
        request,
        tool: Some(tool),
        workspace: Some(workspace),
        connection: Some(connection),
        session,
        selected,
        rollback_target,
        history,
        resources,
        seals,
        canonical,
        chart_hash,
        chart_source,
        values_hash,
        summary,
        created: Instant::now(),
        demo: false,
        hooks,
        hook_seals,
        crds,
        crd_seals,
        bindings,
        impact: vec![],
        review_context: gateway.context.clone(),
        test_runs,
        signature,
    })
}
fn crd_api() -> ApiResource {
    ApiResource {
        group: "apiextensions.k8s.io".into(),
        version: "v1".into(),
        api_version: "apiextensions.k8s.io/v1".into(),
        kind: "CustomResourceDefinition".into(),
        plural: "customresourcedefinitions".into(),
    }
}
async fn crd_preflight(
    client: &Client,
    plan: &crate::helm_extensions::CrdPlan,
    create_missing: bool,
) -> Result<Vec<Option<crate::helm_crds::Binding>>> {
    use kube::api::PostParams;
    let api: Api<DynamicObject> = Api::all_with(client.clone(), &crd_api());
    let mut seals = Vec::new();
    for definition in &plan.definitions {
        let current = api.get_opt(&definition.name).await.map_err(|error| {
            api_failure(error, "exact CRD read (needs get permission on this CRD)")
        })?;
        if let Some(current) = current {
            let object = serde_json::to_value(current).map_err(|_| "invalid live CRD")?;
            let binding = crate::helm_crds::validate_live(object.clone())?;
            if !definition.matches_existing(&object) {
                return Err("existing CRD differs from the packaged definition; create-only policy never upgrades or adopts a different schema".into());
            }
            seals.push(Some(binding));
        } else {
            if !create_missing {
                return Err("packaged CRD is absent; run a separately reviewed helm crds RELEASE CHART bootstrap, wait for Established, then prepare this release again".into());
            }
            let object: DynamicObject = serde_json::from_value(definition.object.clone())
                .map_err(|_| "invalid packaged CRD")?;
            let admitted = api
                .create(
                    &PostParams {
                        dry_run: true,
                        ..Default::default()
                    },
                    &object,
                )
                .await
                .map_err(|error| {
                    api_failure(
                        error,
                        "CRD create dry-run (needs explicit cluster-scoped create permission)",
                    )
                })?;
            let mut admitted =
                serde_json::to_value(admitted).map_err(|_| "invalid admitted CRD")?;
            if admitted["metadata"]["finalizers"]
                == json!(["customresourcecleanup.apiextensions.k8s.io"])
            {
                admitted["metadata"]
                    .as_object_mut()
                    .ok_or("invalid admitted metadata")?
                    .remove("finalizers");
            }
            if !definition.matches_existing(&admitted) {
                return Err(
                    "CRD admission changed reviewed schema fields; no bootstrap submitted".into(),
                );
            }
            let _ = crate::helm_extensions::crd_definition(admitted)?;
            seals.push(None);
        }
    }
    Ok(seals)
}
fn same_crd_seals(
    left: &[Option<crate::helm_crds::Binding>],
    right: &[Option<crate::helm_crds::Binding>],
) -> bool {
    left.len() == right.len()
        && left.iter().zip(right).all(|(a, b)| match (a, b) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                a.uid == b.uid
                    && a.resource_version == b.resource_version
                    && a.definition.name == b.definition.name
            }
            _ => false,
        })
}
#[allow(clippy::too_many_arguments)]
async fn prepare_crds(
    gateway: &Gateway,
    options: Options,
    request: Request,
    tool: Tool,
    workspace: Workspace,
    connection: StaticConnection,
    session: String,
    chart_hash: Option<String>,
    chart_source: Option<String>,
    signature: Option<SignatureReview>,
    crds: crate::helm_extensions::CrdPlan,
) -> Result<Prepared> {
    if crds.definitions.is_empty() {
        return Err("packaged chart has no supported crds/ definitions to bootstrap".into());
    }
    let (client, _, _) = gateway.helm_lifecycle_access()?;
    let (crd_seals, impact) = schema_preflight(&client, &crds, &request, &options, true).await?;
    let mut summary = format!(
        "REVIEW CLUSTER CRD {}\nContext: {}\nVerified endpoint: {}\nConfirmation label: {}/{} (NOT a namespace restriction)\nChart SHA-256: {}\nCRDs and this review affect ALL namespaces, including instances outside the confirmation namespace.\nNo Helm release revision is created or deleted by these native API operations. Exact schema UID/resourceVersion and complete instance membership are rechecked before writes.\nKnown-sensitive fields are redacted; the exact private chart/specification is hash-bound. Custom schema/instance data may still be confidential.\nNo automatic migration, rollback or retry. Cancellation or failure can leave partial changes. Budget {}seconds; review expires in15minutes.\n",
        request.name(),
        safety::safe_text(&gateway.context, 253),
        safety::safe_text(&gateway.server, 1024),
        options.namespace,
        options.release,
        chart_hash.as_deref().unwrap_or("missing"),
        options.seconds
    );
    match request {
        Request::InstallCrds{..}=>summary.push_str("CREATE-ONLY: create missing definitions, retain exactly matching Established schemas. CRDs/data persist after ordinary release uninstall; approve the release separately.\n"),
        Request::UpgradeCrds{..}=>summary.push_str("SCHEMA UPDATE: only proven compatible validation relaxations or optional fields absent in captured data; preserve group/names/scope, served/storage versions, defaults, subresources and existing metadata. No conversion webhook or storage migration. Cluster-wide instances are read and sealed, not written by upgrade.\n"),
        Request::DeleteCrds{..}=>summary.push_str("DESTRUCTIVE: permanently delete every listed owned custom instance across ALL namespaces, then delete these cluster API definitions. All instances need exact Helm release ownership and no GitOps/ownerReferences/finalizers; foreign instances refuse deletion. Instance and schema deletes carry exact UID/resourceVersion preconditions. A complete empty re-list is required before deleting the CRD. This is NOT atomic: concurrent creation after the final list can be deleted by Kubernetes CRD cleanup; stop writers/controllers before approval. Data loss is irreversible; backups are your responsibility.\n"),
        _=>return Err("invalid schema workflow".into()),
    }
    if let Some(signature) = &signature {
        summary.push_str(&signature.inputs.summary());
        summary.push_str(&format!(
            "Verified OpenPGP signer fingerprint: {}\n",
            signature.fingerprint
        ));
    } else {
        summary
            .push_str("No OpenPGP signature verified; content digest is not publisher identity.\n");
    }
    if let Some(source) = &chart_source {
        summary.push_str(source);
        summary.push('\n');
    }
    for (index, (definition, seal)) in crds.definitions.iter().zip(&crd_seals).enumerate() {
        summary.push_str(&format!(
            "\nCRD {} {}\nDESIRED PACKAGED SCHEMA\n",
            definition.name,
            if seal.is_some() {
                "EXISTING"
            } else {
                "MISSING"
            }
        ));
        summary.push_str(&review_object(definition.object.clone())?);
        if let Some(binding) = seal {
            summary.push_str(&format!(
                "\nCURRENT SCHEMA UID {} RV {}\n",
                binding.uid, binding.resource_version
            ));
            summary.push_str(&review_object(binding.definition.object.clone())?);
        }
        if let Some(inventory) = impact.get(index) {
            summary.push_str(&format!(
                "\nCOMPLETE ALL-NAMESPACE IMPACT: {} instances\n",
                inventory.instances.len()
            ));
            for instance in &inventory.instances {
                summary.push_str(&format!(
                    "{} {}/{} UID {} RV {}\n",
                    inventory.api.api_version,
                    instance.namespace,
                    instance.name,
                    instance.uid,
                    instance.resource_version
                ));
                summary.push_str(&review_object(instance.object.clone())?);
            }
        }
    }
    let confirmation = format!(
        "{}{}/{}",
        if matches!(request, Request::DeleteCrds { .. }) {
            "DELETE CRDS "
        } else {
            ""
        },
        options.namespace,
        options.release
    );
    summary.push_str(&format!(
        "\nExact CRD plan SHA-256: {}\nType {} to execute this cluster-scoped plan.\n",
        hash(crds.canonical.as_bytes()),
        confirmation
    ));
    if summary.len() > MAX_REVIEW {
        return Err(
            "complete schema/instance impact review exceeds512KiB; split the schema operation"
                .into(),
        );
    }
    let canonical = crds.canonical.clone();
    Ok(Prepared {
        options,
        request,
        tool: Some(tool),
        workspace: Some(workspace),
        connection: Some(connection),
        session,
        selected: None,
        rollback_target: None,
        history: vec![],
        resources: vec![],
        seals: vec![],
        canonical,
        chart_hash,
        chart_source,
        values_hash: None,
        summary,
        created: Instant::now(),
        demo: false,
        hooks: Default::default(),
        hook_seals: vec![],
        crds,
        crd_seals,
        bindings: vec![],
        impact,
        review_context: gateway.context.clone(),
        test_runs: vec![],
        signature,
    })
}
async fn execute_crds(gateway: &Gateway, prepared: &Prepared) -> Result<()> {
    if !matches!(prepared.request, Request::InstallCrds { .. }) {
        return execute_schema_mutation(gateway, prepared).await;
    }
    use kube::api::PostParams;
    gateway.helm_extension_access(false, true)?;
    let (client, _, _) = gateway.helm_lifecycle_access()?;
    let api: Api<DynamicObject> = Api::all_with(client, &crd_api());
    for (definition, seal) in prepared.crds.definitions.iter().zip(&prepared.crd_seals) {
        if seal.is_some() {
            continue;
        }
        // Never convert an unexpected AlreadyExists into adoption or update.
        let object: DynamicObject = serde_json::from_value(definition.object.clone())
            .map_err(|_| "invalid reviewed CRD")?;
        let created = api
            .create(&PostParams::default(), &object)
            .await
            .map_err(|error| {
                api_failure(
                    error,
                    "CRD create; previous definitions may already exist; reconcile before retry",
                )
            })?;
        let created_value = serde_json::to_value(&created)
            .map_err(|_| "cannot inspect created CRD; outcome unknown")?;
        if !definition.matches_existing(&created_value) {
            return Err("created CRD differs from its reviewed schema; stop and reconcile; no further definitions created".into());
        }
        let uid = created
            .metadata
            .uid
            .ok_or("created CRD omitted UID; outcome unknown")?;
        loop {
            let current = api
                .get(&definition.name)
                .await
                .map_err(|error| api_failure(error, "CRD establishment"))?;
            if current.metadata.uid.as_deref() != Some(&uid) {
                return Err(
                    "CRD was replaced during bootstrap; no further definitions created".into(),
                );
            }
            let object = serde_json::to_value(current).map_err(|_| "invalid CRD response")?;
            if !definition.matches_existing(&object) {
                return Err("CRD schema changed during establishment; stop and reconcile".into());
            }
            if crate::helm_extensions::CrdDefinition::established(&object) {
                let _ = crate::helm_crds::validate_live(object)?;
                break;
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    Ok(())
}
fn review_object(mut object: Value) -> Result<String> {
    if let Some(map) = object.as_object_mut() {
        map.remove("status");
    }
    if let Some(metadata) = object.get_mut("metadata").and_then(Value::as_object_mut) {
        metadata.remove("managedFields");
    }
    helm::redact_values(&mut object, 0, &mut 0)?;
    safety::redact(&mut object);
    crate::inspect::yaml(&object)
}
fn start_audit(prepared: &Prepared, context: &str) -> Result<PathBuf> {
    let dir = safety::config_dir()?.join("audit");
    safety::checked_path(&dir)?;
    if dir.exists()
        && fs::read_dir(&dir)
            .map_err(|_| "cannot inspect audit spool")?
            .take(1001)
            .count()
            >= 1000
    {
        return Err("audit spool full; archive before new writes".into());
    }
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "invalid audit clock")?
        .as_nanos();
    let path = dir.join(format!("{stamp}-helm-{}.jsonl", std::process::id()));
    let bootstrap = schema_request(&prepared.request);
    let mut value = json!({"schema_version":1,"stage":"intent","context":context,"namespace":if bootstrap {"[cluster]"} else {&prepared.options.namespace},"resource":if bootstrap {"customresourcedefinitions"} else {"helm-releases"},"name":prepared.options.release,"uid":prepared.selected.as_ref().map(|r|r.uid.as_str()).unwrap_or("absent"),"action":format!("helm-{}",prepared.request.name()),"plan_sha256":hash(prepared.canonical.as_bytes()),"timestamp_unix_ns":stamp.to_string()});
    if bootstrap {
        value["crd_targets"] = json!(prepared.crds.definitions.iter().zip(&prepared.crd_seals).map(|(definition, seal)| json!({"name":definition.name,"action":match prepared.request {Request::UpgradeCrds{..}=>"update",Request::DeleteCrds{..}=>"delete",_=>if seal.is_some(){"retain"}else{"create"}},"uid":seal.as_ref().map(|binding|binding.uid.as_str())})).collect::<Vec<_>>());
    }
    if bootstrap {
        value["custom_instance_targets"]=json!(prepared.impact.iter().flat_map(|inventory|inventory.instances.iter().map(|instance|json!({"schema":inventory.schema,"namespace":instance.namespace,"name":instance.name,"uid":instance.uid,"resource_version":instance.resource_version,"action":if matches!(prepared.request,Request::DeleteCrds{..}){"delete"}else{"inspect-only"}}))).collect::<Vec<_>>());
    }
    safety::write_new(&path, format!("{value}\n").as_bytes())?;
    Ok(path)
}
fn finish_audit(path: &Path, status: &str) -> Result<()> {
    safety::checked_path(path)?;
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(path)
        .map_err(|_| "Helm may have executed; cannot record audit outcome")?;
    writeln!(file, "{}", json!({"stage":"outcome","status":status}))
        .and_then(|_| file.sync_all())
        .map_err(|_| "Helm may have executed; cannot sync audit outcome".into())
}
async fn check_current_release(gateway: &Gateway, prepared: &Prepared) -> Result<()> {
    let inventory = helm::list(gateway, &prepared.options.namespace).await?;
    if inventory.truncated || !inventory.issues.is_empty() {
        return Err("fresh Helm inventory incomplete; no write sent".into());
    }
    let matches = inventory
        .releases
        .iter()
        .filter(|r| r.name == prepared.options.release)
        .collect::<Vec<_>>();
    match &prepared.selected {
        None if !matches.is_empty() => {
            return Err("release appeared after review; no write sent".into());
        }
        Some(old) => {
            let current = matches
                .iter()
                .find(|r| r.storage == prepared.options.storage)
                .ok_or("release disappeared after review; no write sent")?;
            if matches.len() != 1
                || current.uid != old.uid
                || current.resource_version != old.resource_version
                || current.revision != old.revision
                || history_seal(&helm::history(gateway, current).await?)? != prepared.history
            {
                return Err("release UID/revision/history changed; no write sent".into());
            }
        }
        _ => {}
    }
    Ok(())
}
async fn validate_final(gateway: &Gateway, prepared: &Prepared) -> Result<ProcessContext> {
    gateway.helm_extension_access(prepared.options.allow_hooks, prepared.options.allow_crds)?;
    let (client, connection, _) = gateway.helm_lifecycle_access()?;
    connection.gate.check_live()?;
    let context = ProcessContext {
        tool: prepared.tool.clone().ok_or("trusted Helm review missing")?,
        root: prepared
            .workspace
            .as_ref()
            .ok_or("private Helm workspace missing")?
            .0
            .clone(),
        connection: prepared
            .connection
            .clone()
            .ok_or("private Helm credential missing")?,
        storage: prepared.options.storage,
    };
    context.tool.check()?;
    let input = match &prepared.request {
        Request::Install { chart, values } | Request::Upgrade { chart, values } => {
            Some((chart, values.as_ref()))
        }
        Request::InstallCrds { chart }
        | Request::UpgradeCrds { chart }
        | Request::DeleteCrds { chart } => Some((chart, None)),
        _ => None,
    };
    if let Some((chart, values)) = input {
        if prepared.chart_source.is_none()
            && prepared.chart_hash.as_deref() != Some(file_hash(chart, MAX_CHART)?.as_str())
        {
            return Err("source chart changed after review; no write sent".into());
        }
        if let Some(values) = values
            && prepared.values_hash.as_deref() != Some(file_hash(values, 1024 * 1024)?.as_str())
        {
            return Err("source values changed after review; no write sent".into());
        }
    }
    for (name, expected, limit) in [
        ("chart.tgz", &prepared.chart_hash, MAX_CHART),
        ("values.yaml", &prepared.values_hash, 1024 * 1024),
    ] {
        if let Some(expected) = expected
            && file_hash(&context.root.join(name), limit)? != *expected
        {
            return Err("private reviewed input changed; prepare again".into());
        }
    }
    if let Some(signature) = &prepared.signature {
        signature.inputs.check()?;
        let verified = command(&context, signature.inputs.argv(), 15, None).await?;
        if crate::helm_provenance::verified_fingerprint(&verified.stdout)? != signature.fingerprint
        {
            return Err("verified chart signer changed after review; no write sent".into());
        }
    }
    if schema_request(&prepared.request) {
        let observed = inspect_chart(
            &safety::read_small(&context.root.join("chart.tgz"), MAX_CHART)?,
            true,
        )?;
        if observed.canonical != prepared.canonical {
            return Err("packaged schema plan changed after review".into());
        }
        let (seals, impact) = schema_preflight(
            &client,
            &prepared.crds,
            &prepared.request,
            &prepared.options,
            true,
        )
        .await?;
        if !same_crd_seals(&seals, &prepared.crd_seals)
            || impact.len() != prepared.impact.len()
            || !impact
                .iter()
                .zip(&prepared.impact)
                .all(|(a, b)| a.same_membership(b))
        {
            return Err(
                "CRD or complete instance membership changed after review; prepare again".into(),
            );
        }
        let bindings = seals.into_iter().flatten().collect::<Vec<_>>();
        crate::helm_crds::recheck(&client, &bindings).await?;
        if prepared.created.elapsed() > REVIEW_LIFETIME {
            return Err("schema review expired during validation".into());
        }
        return Ok(context);
    }
    let current_crds = crd_preflight(
        &client,
        &prepared.crds,
        matches!(prepared.request, Request::InstallCrds { .. }),
    )
    .await?;
    if !same_crd_seals(&current_crds, &prepared.crd_seals) {
        return Err(
            "packaged CRD UID/resourceVersion or absence changed after review; prepare again"
                .into(),
        );
    }
    if matches!(prepared.request, Request::InstallCrds { .. }) {
        let observed = inspect_chart(
            &safety::read_small(&context.root.join("chart.tgz"), MAX_CHART)?,
            true,
        )?;
        if observed.canonical != prepared.canonical || prepared.created.elapsed() > REVIEW_LIFETIME
        {
            return Err("CRD plan changed or review expired".into());
        }
        return Ok(context);
    }
    crate::helm_crds::recheck(&client, &prepared.bindings).await?;
    check_current_release(gateway, prepared).await?;
    let mut bindings = prepared.bindings.clone();
    let (desired, hooks) = dry_plan(
        gateway,
        &context,
        &prepared.options,
        &prepared.request,
        &client,
        &mut bindings,
        if matches!(prepared.request, Request::Rollback { .. }) {
            prepared.rollback_target.as_ref()
        } else {
            prepared.selected.as_ref()
        },
    )
    .await?;
    if bindings.len() != prepared.bindings.len()
        || hooks.canonical != prepared.hooks.canonical
        || canonical_plan(&desired, &hooks)? != prepared.canonical
    {
        return Err("Helm rendered a different plan after approval; no write sent; inspect dynamic templates or cluster changes".into());
    }
    let (seals, _) = resource_seals(
        client.clone(),
        &prepared.resources,
        &prepared.options,
        false,
    )
    .await?;
    if seals != prepared.seals {
        return Err("affected resource UID/resourceVersion changed; no write sent".into());
    }
    admission(
        client.clone(),
        &desired,
        &prepared.resources,
        &seals,
        &prepared.options,
        matches!(prepared.request, Request::Uninstall),
    )
    .await?;
    let prior_hooks = if let Some(release) = &prepared.selected {
        stored_hooks(gateway, &prepared.options, release, &prepared.request).await?
    } else {
        Default::default()
    };
    let (hook_seals, _) =
        hook_preflight(&client, &hooks, &prior_hooks, &prepared.options, true).await?;
    if hook_seals != prepared.hook_seals {
        return Err("hook UID/resourceVersion changed after review; no action submitted".into());
    }
    let (fresh, _) = resource_seals(
        client.clone(),
        &prepared.resources,
        &prepared.options,
        false,
    )
    .await?;
    let (fresh_hooks, _) =
        hook_preflight(&client, &hooks, &prior_hooks, &prepared.options, false).await?;
    if fresh_hooks != prepared.hook_seals {
        return Err("hook changed during final validation; no action submitted".into());
    }
    crate::helm_crds::recheck(&client, &prepared.bindings).await?;
    let final_crds = crd_preflight(&client, &prepared.crds, false).await?;
    if !same_crd_seals(&final_crds, &prepared.crd_seals) {
        return Err("packaged CRD changed during final validation; no action submitted".into());
    }

    if fresh != prepared.seals || prepared.created.elapsed() > REVIEW_LIFETIME {
        return Err(
            "resource changed or review expired during final validation; no write sent".into(),
        );
    }
    check_current_release(gateway, prepared).await?;
    Ok(context)
}
async fn canceled(cancel: &mut Option<tokio::sync::oneshot::Receiver<()>>) {
    if let Some(cancel) = cancel {
        let _ = cancel.await;
    } else {
        std::future::pending::<()>().await;
    }
}
async fn verify_uninstalled(gateway: &Gateway, prepared: &Prepared) -> Result<()> {
    let (client, _, _) = gateway.helm_lifecycle_access()?;
    let mut pending = prepared
        .resources
        .iter()
        .filter(|resource| {
            resource.object["metadata"]["annotations"]["helm.sh/resource-policy"] != "keep"
        })
        .collect::<Vec<_>>();
    while !pending.is_empty() {
        let mut retained = Vec::new();
        for resource in pending {
            let api: Api<DynamicObject> =
                Api::namespaced_with(client.clone(), &prepared.options.namespace, &resource.api);
            if let Some(current) = api
                .get_opt(&resource.identity.name)
                .await
                .map_err(|error| api_failure(error, "post-uninstall verification"))?
            {
                let expected = prepared
                    .seals
                    .iter()
                    .find(|seal| seal.identity == resource.identity)
                    .and_then(|seal| seal.uid.as_deref());
                if current.metadata.uid.as_deref() != expected {
                    return Err("resource name was replaced during uninstall; replacement was not part of this review; inspect it explicitly".into());
                }
                retained.push(resource);
            }
        }
        pending = retained;
        if !pending.is_empty() {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }
    Ok(())
}
async fn observed_release(gateway: &Gateway, options: &Options) -> String {
    match tokio::time::timeout(
        Duration::from_secs(10),
        helm::list_with_storage(gateway, &options.namespace, Some(options.storage)),
    )
    .await
    {
        Ok(Ok(snapshot)) if !snapshot.truncated && snapshot.issues.is_empty() => snapshot
            .releases
            .iter()
            .find(|r| r.name == options.release)
            .map(|r| {
                format!(
                    "Observed release revision {} status {} (storage UID {}).",
                    r.revision, r.status, r.uid
                )
            })
            .unwrap_or_else(|| {
                "Observed release absent from complete selected storage inventory.".into()
            }),
        _ => "Current release status could not be verified.".into(),
    }
}
pub async fn run(
    gateway: &Gateway,
    prepared: Prepared,
    typed: &str,
    cancel: Option<tokio::sync::oneshot::Receiver<()>>,
) -> Result<Outcome> {
    if typed != prepared.confirmation() {
        return Err("confirmation must exactly match namespace/release".into());
    }
    if gateway.helm_lifecycle_session()? != prepared.session
        || prepared.created.elapsed() > REVIEW_LIFETIME
    {
        return Err("Helm session changed or review expired; prepare again".into());
    }
    if prepared.demo {
        return Ok(Outcome {
            success: true,
            uncertain: false,
            summary: format!(
                "DEMO / SIMULATED Helm {} {}/{}; no cluster or tool changes",
                prepared.request.name(),
                prepared.options.namespace,
                prepared.options.release
            ),
        });
    }
    let mut cancel = cancel;
    let context = tokio::select! {
        result=tokio::time::timeout(Duration::from_secs(120),validate_final(gateway,&prepared))=>result.map_err(|_|"Helm final validation exceeded 120 seconds; no lifecycle write sent".to_owned())??,
        _=canceled(&mut cancel)=>return Err("Helm canceled during final validation; no lifecycle write sent".into()),
    };
    let audit = start_audit(&prepared, &gateway.context)?;
    let operation_started = Instant::now();
    if schema_request(&prepared.request) {
        let result = tokio::select! {result=tokio::time::timeout(Duration::from_secs(u64::from(prepared.options.seconds)),execute_crds(gateway,&prepared))=>result.map_err(|_|"CRD bootstrap deadline exceeded; definitions may already exist".to_owned()).and_then(|result|result),_=canceled(&mut cancel)=>Err("CRD bootstrap canceled; definitions may already exist".into())};
        let success = result.is_ok();
        finish_audit(
            &audit,
            if success {
                "succeeded"
            } else {
                "failed-or-unknown"
            },
        )?;
        return Ok(Outcome {
            success,
            uncertain: !success,
            summary: match result {
                Ok(()) => format!(
                    "CRD {} completed for {} reviewed definitions. No Helm release revision changed; inspect schemas and reviewed instance identities before further operations.",
                    prepared.request.name(),
                    prepared.crds.definitions.len()
                ),
                Err(error) => format!(
                    "CRD operation outcome UNKNOWN: {}. Earlier reviewed instances/schemas may already have changed or been deleted. Inspect exact reviewed names and audit pending; do not retry automatically.",
                    safety::safe_text(&error, 1024)
                ),
            },
        });
    }
    let mut result = command_with_cancel(
        &context,
        lifecycle_args(&prepared.options, &prepared.request, &context.root, false),
        prepared.options.seconds,
        &mut cancel,
    )
    .await;
    if result.is_ok() && matches!(prepared.request, Request::Uninstall) {
        let remaining = Duration::from_secs(u64::from(prepared.options.seconds))
            .saturating_sub(operation_started.elapsed());
        let deletion = tokio::select! {
            state=tokio::time::timeout(remaining,verify_uninstalled(gateway,&prepared))=>state.map_err(|_|"Helm returned success but reviewed resource deletion is still pending; operation budget expired".to_owned()).and_then(|state|state),
            _=canceled(&mut cancel)=>Err("Helm returned success but deletion verification was canceled; inspect terminating resources".into()),
        };
        if let Err(error) = deletion {
            result = Err(error);
        }
    }
    if matches!(prepared.request, Request::Test) {
        let verified = tokio::time::timeout(
            Duration::from_secs(15),
            test_outcome(gateway, &prepared, result.is_ok()),
        )
        .await
        .map_err(|_| "test-result verification timed out".to_owned())
        .and_then(|value| value);
        let normal_exit = result.is_ok()
            || result
                .as_ref()
                .err()
                .is_some_and(|error| error.starts_with("Helm reported"));
        return match verified {
            Ok((success, summary)) if normal_exit => {
                finish_audit(&audit, if success { "succeeded" } else { "failed" })?;
                Ok(Outcome {
                    success,
                    uncertain: false,
                    summary,
                })
            }
            other => {
                finish_audit(&audit, "failed-or-unknown")?;
                let detail = match other {
                    Err(error) => error,
                    Ok((_, summary)) => summary,
                };
                Ok(Outcome {
                    success: false,
                    uncertain: true,
                    summary: format!(
                        "Helm test outcome UNKNOWN: {}. Test resources/results may already have changed. Inspect exact release/test identities; no automatic retry or logs.",
                        safety::safe_text(&detail, 1024)
                    ),
                })
            }
        };
    }
    let observed = observed_release(gateway, &prepared.options).await;
    match result {
        Ok(_) => {
            finish_audit(&audit, "succeeded")?;
            Ok(Outcome {
                success: true,
                uncertain: false,
                summary: format!(
                    "Helm {} completed for {}/{} in {} storage; Helm reported completion. {} Inspect :helm/history before further actions.",
                    prepared.request.name(),
                    prepared.options.namespace,
                    prepared.options.release,
                    prepared.options.storage.name(),
                    observed
                ),
            })
        }
        Err(error) => {
            finish_audit(&audit, "failed-or-unknown")?;
            Ok(Outcome {
                success: false,
                uncertain: true,
                summary: format!(
                    "Helm {} outcome UNKNOWN for {}/{}: {}. {} Inspect :helm/history, resources and audit pending before any retry; no automatic retry or rollback was attempted.",
                    prepared.request.name(),
                    prepared.options.namespace,
                    prepared.options.release,
                    safety::safe_text(&error, 1024),
                    observed
                ),
            })
        }
    }
}

/// Schema mutations deliberately use the live object's metadata and only the
/// reviewed spec. This never adopts chart labels or discards existing labels.
fn schema_update_object(
    binding: &crate::helm_crds::Binding,
    desired: &crate::helm_extensions::CrdDefinition,
) -> Result<DynamicObject> {
    let mut object = binding.definition.object.clone();
    object["spec"] = desired.object["spec"].clone();
    object["metadata"] = binding.live_metadata.clone();
    if let Some(metadata) = object["metadata"].as_object_mut() {
        metadata.remove("managedFields");
    }
    object["metadata"]["uid"] = json!(binding.uid);
    object["metadata"]["resourceVersion"] = json!(binding.resource_version);
    serde_json::from_value(object).map_err(|_| "invalid reviewed schema update".into())
}
fn deletion_instance(instance: &crate::helm_crds::Instance, options: &Options) -> Result<()> {
    namespace_allowed(&instance.namespace)?;
    ownership(&instance.object, options)?;
    for key in ["ownerReferences", "finalizers"] {
        if instance.object["metadata"]
            .get(key)
            .is_some_and(|value| value.as_array().is_none_or(|values| !values.is_empty()))
        {
            return Err("schema deletion refuses instances with ownerReferences or finalizers; reconcile independently".into());
        }
    }
    if !instance.object["metadata"]["deletionTimestamp"].is_null() {
        return Err(
            "custom instance is already terminating; reconcile before schema deletion".into(),
        );
    }
    Ok(())
}
fn delete_params(uid: &str, version: &str, dry_run: bool) -> kube::api::DeleteParams {
    kube::api::DeleteParams {
        dry_run,
        preconditions: Some(kube::api::Preconditions {
            uid: Some(uid.into()),
            resource_version: Some(version.into()),
        }),
        propagation_policy: Some(kube::api::PropagationPolicy::Foreground),
        ..Default::default()
    }
}
async fn schema_preflight(
    client: &Client,
    plan: &crate::helm_extensions::CrdPlan,
    request: &Request,
    options: &Options,
    dry: bool,
) -> Result<(
    Vec<Option<crate::helm_crds::Binding>>,
    Vec<crate::helm_crds::Inventory>,
)> {
    if matches!(request, Request::InstallCrds { .. }) {
        return Ok((crd_preflight(client, plan, true).await?, vec![]));
    }
    let api: Api<DynamicObject> = Api::all_with(client.clone(), &crd_api());
    let mut bindings = Vec::new();
    let mut impact = Vec::new();
    let mut count = 0;
    let mut bytes = 0;
    for desired in &plan.definitions {
        let binding = crate::helm_crds::read(client, &desired.name).await?;
        // Explicit foreign Helm ownership is never overridden, even for schemas
        // originally installed by a different release's standalone bootstrap.
        let metadata = &binding.definition.object["metadata"];
        if metadata["annotations"]
            .get("meta.helm.sh/release-name")
            .is_some()
            || metadata["annotations"]
                .get("meta.helm.sh/release-namespace")
                .is_some()
            || metadata["labels"]
                .get("app.kubernetes.io/managed-by")
                .is_some()
        {
            ownership(&binding.definition.object, options)?;
        }
        let inventory =
            crate::helm_crds::inventory(client, &binding, &mut count, &mut bytes).await?;
        match request {
            Request::UpgradeCrds { .. } => {
                let instances = inventory
                    .instances
                    .iter()
                    .map(|instance| instance.object.clone())
                    .collect::<Vec<_>>();
                crate::helm_extensions::compatible_upgrade(
                    &binding.definition,
                    desired,
                    &instances,
                )?;
                if dry {
                    let admitted = api
                        .replace(
                            &desired.name,
                            &kube::api::PostParams {
                                dry_run: true,
                                ..Default::default()
                            },
                            &schema_update_object(&binding, desired)?,
                        )
                        .await
                        .map_err(|error| api_failure(error, "CRD schema update dry-run"))?;
                    let admitted = serde_json::to_value(admitted)
                        .map_err(|_| "invalid admitted schema update")?;
                    if !desired.matches_existing(&admitted)
                        || !schema_metadata_matches(&binding, &admitted)
                    {
                        return Err("admission changed the reviewed updated schema/identity".into());
                    }
                }
            }
            Request::DeleteCrds { .. } => {
                if !desired.matches_existing(&binding.definition.object) {
                    return Err("delete chart does not exactly match the live schema; supply the current packaged definition".into());
                }
                for instance in &inventory.instances {
                    deletion_instance(instance, options)?;
                    if dry {
                        let instances: Api<DynamicObject> = Api::namespaced_with(
                            client.clone(),
                            &instance.namespace,
                            &inventory.api,
                        );
                        instances
                            .delete(
                                &instance.name,
                                &delete_params(&instance.uid, &instance.resource_version, true),
                            )
                            .await
                            .map_err(|error| {
                                api_failure(error, "reviewed custom instance deletion dry-run")
                            })?;
                    }
                }
                if dry {
                    api.delete(
                        &desired.name,
                        &delete_params(&binding.uid, &binding.resource_version, true),
                    )
                    .await
                    .map_err(|error| api_failure(error, "cluster CRD deletion dry-run"))?;
                }
            }
            _ => return Err("invalid schema mutation workflow".into()),
        }
        bindings.push(Some(binding));
        impact.push(inventory);
    }
    Ok((bindings, impact))
}
async fn execute_schema_mutation(gateway: &Gateway, prepared: &Prepared) -> Result<()> {
    gateway.helm_extension_access(false, true)?;
    let (client, _, _) = gateway.helm_lifecycle_access()?;
    let api: Api<DynamicObject> = Api::all_with(client.clone(), &crd_api());
    let mut total = 0;
    let mut bytes = 0;
    // Check all inventories immediately before the first write. This is still
    // not a transaction; every later instance write also carries preconditions.
    for (expected, binding) in prepared
        .impact
        .iter()
        .zip(prepared.crd_seals.iter().flatten())
    {
        let current = crate::helm_crds::inventory(&client, binding, &mut total, &mut bytes).await?;
        if !expected.same_membership(&current) {
            return Err("cluster-wide instance membership changed before schema writes; no schema mutation started".into());
        }
    }
    for (index, (desired, binding)) in prepared
        .crds
        .definitions
        .iter()
        .zip(&prepared.crd_seals)
        .enumerate()
    {
        let binding = binding
            .as_ref()
            .ok_or("schema mutation lacks existing UID/RV")?;
        crate::helm_crds::recheck(&client, std::slice::from_ref(binding)).await?;
        if matches!(prepared.request, Request::UpgradeCrds { .. }) {
            let result = api
                .replace(
                    &desired.name,
                    &kube::api::PostParams::default(),
                    &schema_update_object(binding, desired)?,
                )
                .await
                .map_err(|error| {
                    api_failure(
                        error,
                        "reviewed CRD schema update; earlier updates may already exist",
                    )
                })?;
            let value = serde_json::to_value(result).map_err(|_| "invalid updated CRD response")?;
            if !schema_metadata_matches(binding, &value) || !desired.matches_existing(&value) {
                return Err(
                    "updated schema differs from exact review; outcome unknown; stop and reconcile"
                        .into(),
                );
            }
            let _ = crate::helm_crds::validate_live(value)?;
        } else {
            let inventory = prepared
                .impact
                .get(index)
                .ok_or("schema deletion impact missing")?;
            for instance in &inventory.instances {
                let instances: Api<DynamicObject> =
                    Api::namespaced_with(client.clone(), &instance.namespace, &inventory.api);
                instances
                    .delete(
                        &instance.name,
                        &delete_params(&instance.uid, &instance.resource_version, false),
                    )
                    .await
                    .map_err(|error| {
                        api_failure(
                            error,
                            "reviewed instance deletion; earlier instances may already be deleted",
                        )
                    })?;
            }
            loop {
                let mut count = 0;
                let mut bytes = 0;
                let current =
                    crate::helm_crds::inventory(&client, binding, &mut count, &mut bytes).await?;
                if current.instances.is_empty() {
                    break;
                }
                for instance in &current.instances {
                    if !inventory.instances.iter().any(|old| {
                        old.namespace == instance.namespace
                            && old.name == instance.name
                            && old.uid == instance.uid
                    }) {
                        return Err("new/replaced custom instance appeared during deletion; CRD retained; earlier instances may already be deleted".into());
                    }
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            crate::helm_crds::recheck(&client, std::slice::from_ref(binding)).await?;
            api.delete(
                &desired.name,
                &delete_params(&binding.uid, &binding.resource_version, false),
            )
            .await
            .map_err(|error| {
                api_failure(
                    error,
                    "reviewed CRD deletion; instance deletions may already be complete",
                )
            })?;
            loop {
                let remaining = api
                    .get_opt(&desired.name)
                    .await
                    .map_err(|error| api_failure(error, "CRD deletion completion"))?;
                let Some(remaining) = remaining else {
                    break;
                };
                if remaining.metadata.uid.as_deref() != Some(binding.uid.as_str()) {
                    return Err(
                        "CRD was replaced after deletion; replacement was not reviewed".into(),
                    );
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
    Ok(())
}

async fn test_outcome(
    gateway: &Gateway,
    prepared: &Prepared,
    command_succeeded: bool,
) -> Result<(bool, String)> {
    let expected = prepared
        .selected
        .as_ref()
        .ok_or("test release selection missing")?;
    let inventory = helm::list(gateway, &prepared.options.namespace).await?;
    if inventory.truncated || !inventory.issues.is_empty() {
        return Err("test result inventory incomplete".into());
    }
    let releases = inventory
        .releases
        .iter()
        .filter(|release| release.name == prepared.options.release)
        .collect::<Vec<_>>();
    let current = releases
        .first()
        .filter(|_| releases.len() == 1)
        .ok_or("test release disappeared or storage became ambiguous")?;
    if current.uid != expected.uid
        || current.revision != expected.revision
        || current.storage != expected.storage
        || current.status != expected.status
    {
        return Err("release UID/revision/status changed during test; outcome cannot be attributed to the captured release".into());
    }
    let history = history_seal(&helm::history(gateway, current).await?)?;
    if history.len() != prepared.history.len()
        || history
            .iter()
            .zip(&prepared.history)
            .any(|(after, before)| {
                after.0 != before.0
                    || after.1 != before.1
                    || after.3 != before.3
                    || (after.0 != expected.revision && after.2 != before.2)
            })
    {
        return Err("release history changed outside the reviewed test-result update".into());
    }
    let hooks = stored_hooks(gateway, &prepared.options, current, &prepared.request).await?;
    if hooks.canonical != prepared.hooks.canonical {
        return Err("stored hook plan changed during test; cannot attribute result".into());
    }
    let results = helm::lifecycle_test_results(gateway, current).await?;
    let (succeeded, failed) = fresh_test_counts(&prepared.hooks, &prepared.test_runs, &results)?;
    if failed > 0 {
        return Ok((
            false,
            format!(
                "Helm tests FAILED: {failed} newly failed stored results, {succeeded} newly succeeded. Release remains revision {} (same storage UID); no new revision was created. No test logs were read.",
                current.revision
            ),
        ));
    }
    if !command_succeeded {
        return Err("Helm process did not complete successfully; no newly failed stored test result proves completion".into());
    }
    if succeeded != prepared.hooks.for_event("test").len() {
        return Err("not every reviewed test has a fresh succeeded stored result".into());
    }
    Ok((
        true,
        format!(
            "Helm tests PASSED: {succeeded} freshly succeeded stored results. Release remains revision {} (same storage UID); only hook execution results were updated. No test logs were read.",
            current.revision
        ),
    ))
}
fn fresh_test_counts(
    plan: &crate::helm_extensions::HookPlan,
    before: &[helm::TestRun],
    after: &[helm::TestRun],
) -> Result<(usize, usize)> {
    let mut succeeded = 0;
    let mut failed = 0;
    for hook in plan.for_event("test") {
        let current = after
            .iter()
            .find(|run| run.name == hook.identity.name && run.kind == hook.identity.kind)
            .ok_or("stored result missing reviewed test identity")?;
        let previous = before
            .iter()
            .find(|run| run.name == current.name && run.kind == current.kind);
        let times = current
            .started
            .parse::<k8s_openapi::jiff::Timestamp>()
            .ok()
            .zip(
                current
                    .completed
                    .parse::<k8s_openapi::jiff::Timestamp>()
                    .ok(),
            );
        let fresh = times.is_some_and(|(started, completed)| completed >= started)
            && !current.started.starts_with("0001-")
            && previous.is_none_or(|old| old.started != current.started);
        if fresh {
            match current.phase.as_str() {
                "Succeeded" => succeeded += 1,
                "Failed" => failed += 1,
                _ => {}
            }
        }
    }
    Ok((succeeded, failed))
}

fn schema_metadata_matches(binding: &crate::helm_crds::Binding, object: &Value) -> bool {
    object["metadata"]["uid"] == binding.uid
        && [
            "name",
            "labels",
            "annotations",
            "finalizers",
            "ownerReferences",
        ]
        .iter()
        .all(|key| {
            let before = &binding.live_metadata[*key];
            let after = &object["metadata"][*key];
            before == after || (before.is_null() && after.as_array().is_some_and(Vec::is_empty))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    fn archive(files: Vec<(&str, Vec<u8>, u8)>) -> Result<Vec<u8>> {
        let mut tar = Vec::new();
        for (name, data, kind) in files {
            let mut header = [0u8; 512];
            if name.len() > 99 {
                return Err("fixture path too long".into());
            }
            header[..name.len()].copy_from_slice(name.as_bytes());
            header[100..108].copy_from_slice(b"0000600\0");
            header[108..116].copy_from_slice(b"0000000\0");
            header[116..124].copy_from_slice(b"0000000\0");
            header[124..136].copy_from_slice(format!("{:011o}\0", data.len()).as_bytes());
            header[136..148].copy_from_slice(b"00000000000\0");
            header[148..156].fill(b' ');
            header[156] = kind;
            header[257..263].copy_from_slice(b"ustar\0");
            header[263..265].copy_from_slice(b"00");
            let sum: usize = header.iter().map(|b| usize::from(*b)).sum();
            header[148..156].copy_from_slice(format!("{sum:06o}\0 ").as_bytes());
            tar.extend(header);
            tar.extend(&data);
            tar.resize(tar.len().div_ceil(512) * 512, 0);
        }
        tar.resize(tar.len() + 1024, 0);
        let mut gzip = GzEncoder::new(Vec::new(), Compression::fast());
        gzip.write_all(&tar).map_err(|_| "fixture gzip failed")?;
        gzip.finish()
            .map_err(|_| "fixture gzip finish failed".into())
    }
    #[test]
    fn packaged_chart_scan_rejects_dynamic_delimiters_nested_helpers_crds_and_archive_attacks()
    -> Result<()> {
        assert!(deterministic_templates(r#"{{ date "20060102150405" "invalid" }}"#).is_err());
        for name in [
            "bcrypt",
            "ago",
            "durationRound",
            "date_in_zone",
            "date_modify",
            "must_date_modify",
            "env",
            "expandenv",
        ] {
            assert!(deterministic_templates(&format!("{{{{ {name} 0 }}}}")).is_err());
        }
        assert!(
            deterministic_templates(r#"{{ printf "%s-%s" .Release.Name .Chart.Name }}"#).is_ok()
        );
        for text in [
            r#"{{ printf "%p" .Values }}"#,
            r#"{{ printf (printf "%%%s" "p") .Values }}"#,
            r#"{{ printf $format .Values }}"#,
            r#"{{ keys .Values }}"#,
            r#"{{ values .Values }}"#,
        ] {
            assert!(deterministic_templates(text).is_err(), "{text}");
        }
        let metadata = (
            "fixture/Chart.yaml",
            b"apiVersion: v2\nname: fixture\nversion: 1.0.0\n".to_vec(),
            b'0',
        );
        let deterministic =
            b"apiVersion: v1\nkind: ConfigMap\nmetadata:\n name: {{ .Release.Name }}\n".to_vec();
        assert!(
            scan_chart(&archive(vec![
                metadata.clone(),
                ("fixture/templates/cm.yaml", deterministic, b'0')
            ])?)
            .is_ok()
        );
        for template in [
            "{{ randAlphaNum 12 }}",
            "{{ print \"}}\" (randAlphaNum 12) }}",
            "{{/* lookup */}}",
            "{{ tpl .Values.expression . }}",
            "{{ include \"helper\" . }}\n{{ define \"helper\" }}{{ now }}{{ end }}",
        ] {
            let bytes = archive(vec![
                metadata.clone(),
                (
                    "fixture/templates/_helpers.tpl",
                    template.as_bytes().to_vec(),
                    b'0',
                ),
            ])?;
            assert!(scan_chart(&bytes).is_err(), "{template}");
        }
        let nested = archive(vec![
            ("child/Chart.yaml", b"name: child\n".to_vec(), b'0'),
            (
                "child/templates/_helpers.tpl",
                b"{{ genPrivateKey \"rsa\" }}".to_vec(),
                b'0',
            ),
        ])?;
        assert!(
            scan_chart(&archive(vec![
                metadata.clone(),
                ("fixture/charts/child.tgz", nested, b'0')
            ])?)
            .is_err()
        );
        for (name, kind) in [
            ("../escape", b'0'),
            ("fixture/link", b'2'),
            ("fixture/pax", b'x'),
            ("fixture/crds/type.yaml", b'0'),
        ] {
            assert!(
                scan_chart(&archive(vec![
                    metadata.clone(),
                    (name, b"x".to_vec(), kind)
                ])?)
                .is_err()
            );
        }
        let mut corrupt = archive(vec![metadata])?;
        let end = corrupt.len() - 3;
        corrupt[end] ^= 1;
        assert!(scan_chart(&corrupt).is_err());
        Ok(())
    }
    #[test]
    fn resource_plan_has_explicit_scope_identities_and_redacted_secrets() -> Result<()> {
        let manifest = "apiVersion: v1\nkind: Secret\nmetadata:\n  name: db\nstringData:\n  password: PRIVATE_CANARY\n---\napiVersion: apps/v1\nkind: Deployment\nmetadata:\n  name: app\nspec:\n  template:\n    spec:\n      containers:\n      - name: app\n        image: busybox:1\n        env:\n        - name: API_KEY\n          value: PRIVATE_CANARY\n";
        let resources = parse_resources(manifest, "test")?;
        let mut custom = json!({"apiVersion":"example.test/v1","kind":"Widget","metadata":{"name":"app","namespace":"test"},"spec":{"apiKey":"KEY_CANARY","api_key":"KEY_CANARY","passphrase":"PHRASE_CANARY","privateKey":"PRIVATE_CANARY","tls":{"key":"TLS_CANARY","certificate":"public certificate"},"unusual":"-----BEGIN EC PRIVATE KEY-----\nPEM_CANARY\n-----END EC PRIVATE KEY-----","image":"example:v1","replicas":2}});
        let shown = review_object(custom.clone())?;
        assert!(!shown.contains("CANARY"));
        assert!(
            shown.contains("example:v1")
                && shown.contains("public certificate")
                && shown.contains("\"replicas\": 2")
        );
        helm::redact_values(&mut custom, 0, &mut 0)?;
        safety::redact(&mut custom);
        assert_eq!(shown, review_object(custom)?);
        assert_eq!(resources.len(), 2);
        assert!(resources.iter().all(|r| r.identity.namespace == "test"));
        let rendered = resources
            .iter()
            .map(|r| review_object(r.object.clone()))
            .collect::<Result<Vec<_>>>()?
            .join("\n");
        assert!(!rendered.contains("PRIVATE_CANARY"));
        assert!(rendered.contains("busybox:1"));
        for invalid in [
            "apiVersion: v1\nkind: Namespace\nmetadata:\n  name: outside",
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n  name: cm\n  namespace: elsewhere",
            "apiVersion: v1\nkind: ConfigMap\nmetadata: broken",
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n name: cm\n annotations:\n  helm.sh/hook: pre-install",
            "apiVersion: v1\nkind: ConfigMap\nmetadata:\n name: cm\n labels:\n  argocd.argoproj.io/instance: source",
            "apiVersion: v1\nkind: List\nitems: []",
        ] {
            assert!(parse_resources(invalid, "test").is_err(), "{invalid}");
        }
        assert!(
            parse_resources(
                &(manifest.to_owned()
                    + "---\napiVersion: v1\nkind: Secret\nmetadata:\n name: db\n"),
                "test"
            )
            .is_err()
        );
        Ok(())
    }
    fn options() -> Options {
        Options {
            executable: PathBuf::from("/demo/helm"),
            namespace: "demo".into(),
            release: "demo-api".into(),
            storage: Storage::Secret,
            seconds: 60,
            allow_hooks: false,
            allow_crds: false,
            registry: Default::default(),
            keyring: None,
        }
    }
    #[test]
    fn demo_lifecycle_requires_individual_capabilities_confirmation_and_current_session()
    -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            let readonly = Gateway::connect(crate::gateway::Connection {
                demo: true,
                ..Default::default()
            })
            .await?;
            assert!(
                prepare(&readonly, options(), Request::Uninstall)
                    .await
                    .is_err()
            );
            let gateway = Gateway::connect(crate::gateway::Connection {
                demo: true,
                writable: true,
                allow_helm_write: true,
                ..Default::default()
            })
            .await?;
            let prepared = prepare(&gateway, options(), Request::Rollback { revision: 1 }).await?;
            assert_eq!(prepared.selected().map(|r| r.revision), Some(2));
            assert_eq!(prepared.rollback_target().map(|r| r.revision), Some(1));
            assert!(prepared.summary().contains("SIMULATED"));
            assert!(run(&gateway, prepared, "wrong", None).await.is_err());
            let mut prepared = prepare(&gateway, options(), Request::Uninstall).await?;
            prepared.created = Instant::now() - REVIEW_LIFETIME - Duration::from_secs(1);
            assert!(
                run(&gateway, prepared, "demo/demo-api", None)
                    .await
                    .is_err()
            );
            let prepared = prepare(&gateway, options(), Request::Uninstall).await?;
            let outcome = run(&gateway, prepared, "demo/demo-api", None).await?;
            assert!(outcome.success && !outcome.uncertain && outcome.summary.contains("SIMULATED"));
            let mut invalid = options();
            invalid.namespace = "kube-system".into();
            assert!(
                prepare(&gateway, invalid, Request::Uninstall)
                    .await
                    .is_err()
            );
            Ok(())
        })
    }
    fn static_connection() -> Result<StaticConnection> {
        let mut config = Config::new(
            "https://example.test:6443"
                .parse()
                .map_err(|_| "fixture URL")?,
        );
        config.root_cert = Some(vec![vec![1, 2, 3]]);
        config.auth_info.token = Some("TEST_PRIVATE_TOKEN".into());
        StaticConnection::from_config(&mut config, crate::auth::AuthGate::default())
    }
    #[test]
    fn private_static_projection_drops_original_paths_and_external_auth_and_is_cleaned()
    -> Result<()> {
        let workspace = Workspace::new()?;
        let root = workspace.0.clone();
        let source = root.join("source-token");
        safety::write_new(&source, b"FILE_TOKEN_CANARY\n")?;
        let mut config = Config::new("https://example.test".parse().map_err(|_| "fixture URL")?);
        config.root_cert = Some(vec![vec![1, 2, 3]]);
        config.auth_info.token_file = Some(source.to_string_lossy().into());
        config.tls_server_name = Some("api.example.test".into());
        let connection =
            StaticConnection::from_config(&mut config, crate::auth::AuthGate::default())?;
        assert!(config.auth_info.token_file.is_none());
        let path = root.join("connection.json");
        connection.write(&path)?;
        let data: Value = serde_json::from_slice(&safety::read_small(&path, 1024 * 1024)?)
            .map_err(|_| "fixture JSON")?;
        let user = &data["users"][0]["user"];
        assert_eq!(user["token"], "FILE_TOKEN_CANARY");
        assert!(
            user.get("tokenFile").is_none()
                && user.get("exec").is_none()
                && user.get("auth-provider").is_none()
        );
        assert_eq!(
            data["clusters"][0]["cluster"]["tls-server-name"],
            "api.example.test"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path)
                    .map_err(|_| "private file missing")?
                    .permissions()
                    .mode()
                    & 0o777,
                0o400
            );
        }
        drop(workspace);
        assert!(!root.exists());
        Ok(())
    }
    #[test]
    fn argv_never_fetches_dependencies_runs_hooks_or_discloses_credentials() -> Result<()> {
        let options = options();
        for request in [
            Request::Install {
                chart: PathBuf::from("/original/chart.tgz"),
                values: Some(PathBuf::from("/original/values.yaml")),
            },
            Request::Upgrade {
                chart: PathBuf::from("/original/chart.tgz"),
                values: None,
            },
            Request::Rollback { revision: 1 },
            Request::Uninstall,
        ] {
            let args = lifecycle_args(&options, &request, Path::new("/private/review"), false);
            assert!(args.iter().any(|arg| arg == "--no-hooks"));
            assert!(args.iter().any(|arg| arg == "--wait=watcher"));
            assert!(!args.iter().any(|arg| arg.contains("token")
                || arg.contains("dependency-update")
                || arg.contains("post-renderer")
                || arg.contains("/original")));
            let dry = lifecycle_args(&options, &request, Path::new("/private/review"), true);
            assert!(dry.iter().any(|arg| arg.starts_with("--dry-run")));
        }
        Ok(())
    }
    fn mock_client(
        handler: impl Fn(http::Method, String, Value) -> (u16, Value) + Send + Sync + 'static,
    ) -> Client {
        use http_body_util::{BodyExt, Full};
        let handler = Arc::new(handler);
        let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
            let handler = handler.clone();
            async move {
                let method = request.method().clone();
                let uri = request.uri().to_string();
                let body = request
                    .into_body()
                    .collect()
                    .await
                    .map(|body| body.to_bytes())
                    .unwrap_or_default();
                let value = serde_json::from_slice(&body).unwrap_or(Value::Null);
                let (status, value) = handler(method, uri, value);
                Ok::<_, std::convert::Infallible>(
                    http::Response::builder()
                        .status(status)
                        .header("content-type", "application/json")
                        .body(Full::new(bytes::Bytes::from(value.to_string())))
                        .unwrap_or_else(|_| http::Response::new(Full::new(bytes::Bytes::new()))),
                )
            }
        });
        Client::new(service, "test")
    }
    fn owned(name: &str, uid: &str) -> Value {
        json!({"apiVersion":"v1","kind":"ConfigMap","metadata":{"name":name,"namespace":"demo","uid":uid,"resourceVersion":"7","labels":{"app.kubernetes.io/managed-by":"Helm"},"annotations":{"meta.helm.sh/release-name":"demo-api","meta.helm.sh/release-namespace":"demo"}}})
    }
    #[test]
    fn native_resource_preflight_enforces_total_bytes_and_captures_replacement() -> Result<()> {
        crate::cli::runtime()?.block_on(async{
            let counter=Arc::new(AtomicU64::new(0));let count=counter.clone();
            let client=mock_client(move|method,_,_|{assert_eq!(method,http::Method::GET);let uid=if count.fetch_add(1,Ordering::SeqCst)==0{"first"}else{"replacement"};(200,owned("cm",uid))});
            let resources=parse_resources("apiVersion: v1\nkind: ConfigMap\nmetadata:\n name: cm\n","demo")?;
            let(first,objects)=resource_seals(client.clone(),&resources,&options(),true).await?;assert_eq!(objects.len(),1);
            let(second,objects)=resource_seals(client,&resources,&options(),false).await?;assert!(objects.is_empty());assert!(first!=second);
            let client=mock_client(move|_,uri,_|{let mut object=owned(uri.rsplit('/').next().unwrap_or("cm"),"uid");object["data"]=json!({"private":"X".repeat(3*1024*1024)});(200,object)});
            let resources=parse_resources("apiVersion: v1\nkind: ConfigMap\nmetadata:\n name: a\n---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n name: b\n---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n name: c\n","demo")?;
            assert!(resource_seals(client,&resources,&options(),true).await.is_err_and(|error|error.contains("aggregate")));Ok(())
        })
    }
    fn reviewed_hook() -> Result<crate::helm_extensions::HookPlan> {
        let mut object = owned("check", "unused");
        object["metadata"]
            .as_object_mut()
            .ok_or("fixture metadata")?
            .remove("uid");
        object["metadata"]
            .as_object_mut()
            .ok_or("fixture metadata")?
            .remove("resourceVersion");
        object["metadata"]["annotations"]["helm.sh/hook"] = json!("pre-install,pre-upgrade");
        object["data"] = json!({"result":"reviewed"});
        crate::helm_extensions::hooks_from_manifests(
            &object.to_string(),
            "demo",
            "demo-api",
            crate::helm_extensions::Operation::Install,
            true,
        )
    }
    #[test]
    fn hook_preflight_rejects_foreign_collision_requires_delete_rights_and_seals_uid() -> Result<()>
    {
        crate::cli::runtime()?.block_on(async {
            let plan=reviewed_hook()?;
            let mut existing=plan.all[0].object.clone();existing["metadata"]["uid"]=json!("captured");existing["metadata"]["resourceVersion"]=json!("7");
            let object=existing.clone();let client=mock_client(move|method,_,_|{assert_eq!(method,http::Method::GET);(200,object.clone())});
            assert!(hook_preflight(&client,&plan,&Default::default(),&options(),true).await.is_err_and(|error|error.contains("foreign adoption")));
            let object=existing.clone();let client=mock_client(move|method,_,body|{if method==http::Method::GET{(200,object.clone())}else{if method==http::Method::DELETE{assert_eq!(body["preconditions"]["uid"],"captured");assert_eq!(body["dryRun"],json!(["All"]));}(200,body)}});
            let (seals,_)=hook_preflight(&client,&plan,&plan,&options(),true).await?;assert_eq!(seals[0].uid.as_deref(),Some("captured"));
            let client=mock_client(|method,_,_|if method==http::Method::GET{(404,json!({"apiVersion":"v1","kind":"Status","code":404,"reason":"NotFound"}))}else{assert_eq!(method,http::Method::DELETE);(403,json!({"apiVersion":"v1","kind":"Status","code":403,"reason":"Forbidden","message":"PRIVATE_DELETE_CANARY"}))});
            assert!(hook_preflight(&client,&plan,&Default::default(),&options(),true).await.is_err_and(|error|error.contains("authorization")&&!error.contains("CANARY")));
            existing["data"]["result"]=json!("foreign mutation");let client=mock_client(move|_,_,_|(200,existing.clone()));
            assert!(hook_preflight(&client,&plan,&plan,&options(),true).await.is_err());
            Ok(())
        })
    }
    fn test_crd() -> Value {
        json!({"apiVersion":"apiextensions.k8s.io/v1","kind":"CustomResourceDefinition","metadata":{"name":"widgets.example.com"},"spec":{"group":"example.com","scope":"Namespaced","names":{"plural":"widgets","kind":"Widget"},"versions":[{"name":"v1","served":true,"storage":true,"schema":{"openAPIV3Schema":{"type":"object","properties":{"spec":{"type":"object","properties":{"value":{"type":"string"}}}}}}}]}})
    }
    #[test]
    fn crd_bootstrap_dry_runs_create_retains_existing_and_rejects_admission_drift() -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            let plan=crate::helm_extensions::crds(&test_crd().to_string(),true)?;
            for mutate in [false,true] {
                let client=mock_client(move|method,uri,mut body|{if method==http::Method::GET{(404,json!({"apiVersion":"v1","kind":"Status","code":404,"reason":"NotFound"}))}else{assert_eq!(method,http::Method::POST);assert!(uri.contains("dryRun=All"));if mutate{body["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["properties"]["value"]["type"]=json!("integer");}(201,body)}});
                let result=crd_preflight(&client,&plan,true).await;
                if mutate{assert!(result.is_err_and(|error|error.contains("changed reviewed schema")));}else{assert!(result?.iter().all(Option::is_none));}
            }
            let mut existing=test_crd();existing["metadata"]["uid"]=json!("uid-crd");existing["metadata"]["resourceVersion"]=json!("7");existing["metadata"]["finalizers"]=json!(["customresourcecleanup.apiextensions.k8s.io"]);existing["status"]["conditions"]=json!([{"type":"Established","status":"True"}]);
            let client=mock_client(move|method,_,_|{assert_eq!(method,http::Method::GET);(200,existing.clone())});
            let first=crd_preflight(&client,&plan,true).await?;let second=crd_preflight(&client,&plan,false).await?;assert!(same_crd_seals(&first,&second));assert_eq!(first[0].as_ref().map(|binding|binding.uid.as_str()),Some("uid-crd"));
            Ok(())
        })
    }
    #[test]
    fn crd_execution_creates_only_missing_and_rejects_live_admission_drift() -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            let demo = Gateway::connect(crate::gateway::Connection {
                demo: true,
                writable: true,
                allow_helm_write: true,
                allow_helm_crds: true,
                ..Default::default()
            })
            .await?;
            let mut config = options();
            config.allow_crds = true;
            for mutate in [false, true] {
                let mut prepared = prepare(
                    &demo,
                    config.clone(),
                    Request::InstallCrds {
                        chart: PathBuf::from("/demo/crds.tgz"),
                    },
                )
                .await?;
                prepared.crds = crate::helm_extensions::crds(&test_crd().to_string(), true)?;
                prepared.crd_seals = vec![None];
                let mut live = test_crd();
                live["metadata"]["uid"] = json!("created");
                live["metadata"]["resourceVersion"] = json!("1");
                live["status"]["conditions"] = json!([{"type":"Established","status":"True"}]);
                let client = mock_client(move |method, uri, body| {
                    assert!(!uri.contains("dryRun"));
                    assert!(method == http::Method::POST || method == http::Method::GET);
                    if method == http::Method::POST {
                        assert_eq!(body["metadata"]["name"], "widgets.example.com");
                    }
                    let mut result = live.clone();
                    if mutate {
                        result["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]
                            ["spec"]["properties"]["value"]["type"] = json!("integer");
                    }
                    (200, result)
                });
                let gateway = Gateway::for_helm_extension_test_client(
                    client,
                    "demo",
                    static_connection()?,
                    false,
                    true,
                );
                let result = execute_crds(&gateway, &prepared).await;
                if mutate {
                    assert!(result.is_err_and(|error| error.contains("reviewed schema")));
                } else {
                    result?;
                }
            }
            Ok(())
        })
    }
    #[test]
    fn server_admission_is_dry_run_for_create_update_delete_and_suppresses_error_body() -> Result<()>
    {
        crate::cli::runtime()?.block_on(async{
            let observed=Arc::new(std::sync::Mutex::new(Vec::new()));let recorded=observed.clone();
            let client=mock_client(move|method,uri,body|{if method!=http::Method::DELETE {assert!(uri.contains("dryRun=All"));}
                if method==http::Method::DELETE{assert_eq!(body["dryRun"],json!(["All"]));assert_eq!(body["preconditions"]["uid"],"old");assert_eq!(body["preconditions"]["resourceVersion"],"7");}recorded.lock().unwrap_or_else(|p|p.into_inner()).push(method.clone());if method==http::Method::DELETE{(200,json!({"apiVersion":"v1","kind":"Status","status":"Success","code":200}))}else{(200,body)}});
            let resources=parse_resources("apiVersion: v1\nkind: ConfigMap\nmetadata:\n name: create\n---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n name: update\n---\napiVersion: v1\nkind: ConfigMap\nmetadata:\n name: delete\n","demo")?;
            let desired=resources.iter().filter(|r|r.identity.name!="delete").cloned().collect::<Vec<_>>();let seals=resources.iter().map(|r|Seal{identity:r.identity.clone(),uid:(r.identity.name!="create").then(||"old".into()),version:(r.identity.name!="create").then(||"7".into())}).collect::<Vec<_>>();
            admission(client,&desired,&resources,&seals,&options(),false).await?;{let verbs=observed.lock().map_err(|_|"fixture lock")?;assert!(verbs.contains(&http::Method::POST)&&verbs.contains(&http::Method::PATCH)&&verbs.contains(&http::Method::DELETE));}
            let client=mock_client(|_,_,_|(403,json!({"apiVersion":"v1","kind":"Status","reason":"Forbidden","message":"PRIVATE_SERVER_CANARY","code":403})));
            let error=admission(client,&desired,&resources,&seals,&options(),false).await.err().ok_or("denied admission accepted")?;assert!(error.contains("authorization")&&!error.contains("CANARY"));Ok(())
        })
    }
    #[test]
    fn uninstall_waits_for_exact_uid_disappearance_and_keeps_stored_keep_resources() -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            let demo = Gateway::connect(crate::gateway::Connection {
                demo: true,
                writable: true,
                allow_helm_write: true,
                ..Default::default()
            })
            .await?;
            let mut prepared = prepare(&demo, options(), Request::Uninstall).await?;
            prepared.resources = parse_resources(
                "apiVersion: v1\nkind: ConfigMap\nmetadata:\n name: cm\n",
                "demo",
            )?;
            prepared.seals = vec![Seal {
                identity: prepared.resources[0].identity.clone(),
                uid: Some("old".into()),
                version: Some("7".into()),
            }];
            let calls = Arc::new(AtomicU64::new(0));
            let called = calls.clone();
            let client = mock_client(move |_, _, _| {
                if called.fetch_add(1, Ordering::SeqCst) == 0 {
                    (200, owned("cm", "old"))
                } else {
                    (
                        404,
                        json!({"apiVersion":"v1","kind":"Status","reason":"NotFound","code":404}),
                    )
                }
            });
            let gateway =
                Gateway::for_helm_lifecycle_test_client(client, "demo", static_connection()?);
            verify_uninstalled(&gateway, &prepared).await?;
            assert_eq!(calls.load(Ordering::SeqCst), 2);
            let client = mock_client(|_, _, _| (200, owned("cm", "replacement")));
            let gateway =
                Gateway::for_helm_lifecycle_test_client(client, "demo", static_connection()?);
            assert!(
                verify_uninstalled(&gateway, &prepared)
                    .await
                    .is_err_and(|error| error.contains("replaced"))
            );
            prepared.resources[0].object["metadata"]["annotations"]["helm.sh/resource-policy"] =
                json!("keep");
            let client =
                mock_client(|_, _, _| panic!("stored keep resource must not be waited on"));
            let gateway =
                Gateway::for_helm_lifecycle_test_client(client, "demo", static_connection()?);
            verify_uninstalled(&gateway, &prepared).await?;
            Ok(())
        })
    }
    #[cfg(unix)]
    #[test]
    fn replaced_source_fifo_is_rejected_without_blocking_validation() -> Result<()> {
        let workspace = Workspace::new()?;
        let source = workspace.0.join("reviewed-chart.tgz");
        safety::write_new(&source, b"reviewed chart bytes")?;
        let original = file_hash(&source, MAX_CHART)?;
        fs::remove_file(&source).map_err(|_| "fixture replacement failed")?;
        let status = std::process::Command::new("/usr/bin/mkfifo")
            .env_clear()
            .arg(&source)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_err(|_| "FIFO fixture unavailable")?;
        if !status.success() {
            return Err("FIFO fixture creation failed".into());
        }
        let (tx, rx) = std::sync::mpsc::channel();
        let path = source.clone();
        let thread = std::thread::spawn(move || {
            let _ = tx.send(file_hash(&path, MAX_CHART));
        });
        let result = rx
            .recv_timeout(Duration::from_secs(1))
            .map_err(|_| "hash validation blocked on a FIFO replacement")?;
        assert!(result.is_err_and(|error| error.contains("regular file")));
        thread.join().map_err(|_| "FIFO fixture thread failed")?;
        fs::remove_file(&source).map_err(|_| "FIFO fixture cleanup failed")?;
        safety::write_new(&source, b"replacement bytes")?;
        assert_ne!(file_hash(&source, MAX_CHART)?, original);
        Ok(())
    }
    #[cfg(unix)]
    fn process_context() -> Result<(Workspace, ProcessContext)> {
        let root = Workspace::new()?;
        let connection = static_connection()?;
        connection.write(&root.0.join("connection.json"))?;
        let tool = Tool::new(&std::env::current_exe().map_err(|_| "test executable missing")?)?;
        let context = ProcessContext {
            tool,
            root: root.0.clone(),
            connection,
            storage: Storage::Secret,
        };
        Ok((root, context))
    }
    #[cfg(unix)]
    fn fixture_args(name: &str) -> Vec<String> {
        vec![
            "--exact".into(),
            format!("helm_lifecycle::tests::{name}"),
            "--ignored".into(),
            "--nocapture".into(),
        ]
    }
    #[cfg(unix)]
    #[test]
    fn native_process_has_private_environment_and_suppresses_failure_diagnostics() -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            let (_root, context) = process_context()?;
            let output = command(
                &context,
                fixture_args("process_fixture_environment"),
                5,
                None,
            )
            .await?;
            let text = String::from_utf8(output.stdout).map_err(|_| "fixture output")?;
            assert!(text.contains("private_static_connection=true"));
            assert!(!text.contains("TEST_PRIVATE_TOKEN"));
            let error = command(&context, fixture_args("process_fixture_failure"), 5, None)
                .await
                .err()
                .ok_or("failure fixture unexpectedly succeeded")?;
            assert!(!error.contains("PRIVATE_DIAGNOSTIC_CANARY"));
            Ok(())
        })
    }
    #[cfg(unix)]
    #[test]
    fn native_cancellation_and_output_budget_stop_processes_and_drop_workspace() -> Result<()> {
        crate::cli::runtime()?.block_on(async {
            let (root, context) = process_context()?;
            let path = root.0.clone();
            let (tx, rx) = tokio::sync::oneshot::channel();
            let cancel = async {
                tokio::time::sleep(Duration::from_millis(100)).await;
                let _ = tx.send(());
            };
            let start = Instant::now();
            let (result, ()) = tokio::join!(
                command(
                    &context,
                    fixture_args("process_fixture_sleep"),
                    30,
                    Some(rx)
                ),
                cancel
            );
            assert!(result.is_err_and(|error| error.contains("canceled")));
            assert!(start.elapsed() < Duration::from_secs(3));
            let error = command(&context, fixture_args("process_fixture_flood"), 5, None)
                .await
                .err()
                .ok_or("flood unexpectedly succeeded")?;
            assert!(error.contains("output exceeded"));
            drop(root);
            assert!(!path.exists());
            Ok(())
        })
    }
    #[cfg(unix)]
    #[test]
    #[ignore = "subprocess boundary fixture"]
    fn process_fixture_environment() -> Result<()> {
        assert!(std::env::var_os("GH_TOKEN").is_none());
        assert!(std::env::var_os("HELM_KUBEAPISERVER").is_none());
        assert!(std::env::var_os("AWS_SHARED_CREDENTIALS_FILE").is_none());
        assert!(std::env::var_os("PATH").is_none());
        let root = PathBuf::from(std::env::var_os("TMPDIR").ok_or("private TMPDIR missing")?);
        assert_eq!(std::env::current_dir().map_err(|_| "cwd missing")?, root);
        let path = PathBuf::from(std::env::var_os("KUBECONFIG").ok_or("static path missing")?);
        assert!(path.starts_with(&root));
        let bytes = safety::read_small(&path, 1024 * 1024)?;
        assert!(
            std::str::from_utf8(&bytes)
                .map_err(|_| "config encoding")?
                .contains("TEST_PRIVATE_TOKEN")
        );
        println!("private_static_connection=true");
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    #[ignore = "subprocess boundary fixture"]
    fn process_fixture_failure() {
        eprintln!("PRIVATE_DIAGNOSTIC_CANARY");
        std::process::exit(7);
    }
    #[cfg(unix)]
    #[test]
    #[ignore = "subprocess boundary fixture"]
    fn process_fixture_sleep() {
        std::thread::sleep(Duration::from_secs(30));
    }
    #[cfg(unix)]
    #[test]
    #[ignore = "subprocess boundary fixture"]
    fn process_fixture_flood() {
        let mut out = std::io::stdout().lock();
        let buffer = [b'x'; 16384];
        for _ in 0..600 {
            if out.write_all(&buffer).is_err() {
                break;
            }
        }
    }
    fn live_schema() -> Value {
        let mut object = test_crd();
        object["metadata"]["uid"] = json!("schema-uid");
        object["metadata"]["resourceVersion"] = json!("7");
        object["metadata"]["labels"]["kborg.dev/run-id"] = json!("owned-fixture");
        object["metadata"]["finalizers"] = json!(["customresourcecleanup.apiextensions.k8s.io"]);
        object["status"]["conditions"] = json!([{"type":"Established","status":"True"}]);
        object
    }
    fn custom_instance(namespace: &str, name: &str) -> Value {
        let mut object = owned(name, &format!("uid-{name}"));
        object["apiVersion"] = json!("example.com/v1");
        object["kind"] = json!("Widget");
        object["metadata"]["namespace"] = json!(namespace);
        object["spec"] = json!({"value":"existing"});
        object
    }
    #[test]
    fn schema_update_preserves_live_metadata_and_rejects_admission_metadata_drift() -> Result<()> {
        let binding = crate::helm_crds::validate_live(live_schema())?;
        let mut next = test_crd();
        next["metadata"]["labels"]["do-not-adopt"] = json!("chart");
        next["spec"]["versions"][0]["schema"]["openAPIV3Schema"]["properties"]["spec"]["properties"]
            ["optional"] = json!({"type":"string"});
        let desired = crate::helm_extensions::crd_definition(next)?;
        let update = serde_json::to_value(schema_update_object(&binding, &desired)?)
            .map_err(|_| "fixture update")?;
        assert_eq!(update["metadata"]["uid"], "schema-uid");
        assert_eq!(update["metadata"]["resourceVersion"], "7");
        assert_eq!(
            update["metadata"]["labels"]["kborg.dev/run-id"],
            "owned-fixture"
        );
        assert!(update["metadata"]["labels"]["do-not-adopt"].is_null());
        assert_eq!(
            update["metadata"]["finalizers"],
            binding.live_metadata["finalizers"]
        );
        assert!(schema_metadata_matches(&binding, &update));
        let mut changed = update;
        changed["metadata"]["labels"]["admission"] = json!("new");
        assert!(!schema_metadata_matches(&binding, &changed));
        Ok(())
    }
    #[test]
    fn schema_mutation_preflight_uses_complete_cluster_scope_and_sealed_dry_runs() -> Result<()> {
        crate::cli::runtime()?.block_on(async{
            let schema=live_schema();let first=custom_instance("demo","a");let second=custom_instance("other-owned","b");
            let client=mock_client(move|method,uri,body|{
                if uri.contains("?limit=") {assert_eq!(uri,"/apis/example.com/v1/widgets?limit=1001");return (200,json!({"apiVersion":"example.com/v1","kind":"WidgetList","metadata":{},"items":[first,second]}));}
                if method==http::Method::GET {return(200,schema.clone());}
                assert_eq!(method,http::Method::DELETE);assert_eq!(body["dryRun"],json!(["All"]));assert!(body["preconditions"]["uid"].as_str().is_some());assert!(body["preconditions"]["resourceVersion"].as_str().is_some());
                (200,json!({"apiVersion":"v1","kind":"Status","status":"Success"}))
            });
            let plan=crate::helm_extensions::crds(&test_crd().to_string(),true)?;
            let (seals,impact)=schema_preflight(&client,&plan,&Request::DeleteCrds{chart:"/unused.tgz".into()},&options(),true).await?;assert_eq!(seals.len(),1);assert_eq!(impact[0].instances.len(),2);assert_eq!(impact[0].instances[1].namespace,"other-owned");Ok(())
        })
    }
    #[test]
    fn destructive_instance_policy_rejects_foreign_system_gitops_and_finalizers() -> Result<()> {
        let object = custom_instance("other-owned", "a");
        let instance = crate::helm_crds::Instance {
            namespace: "other-owned".into(),
            name: "a".into(),
            uid: "uid-a".into(),
            resource_version: "1".into(),
            object,
        };
        assert!(deletion_instance(&instance, &options()).is_ok());
        for variant in 0..5 {
            let mut changed = instance.clone();
            match variant {
                0 => {
                    changed.object["metadata"]["annotations"]["meta.helm.sh/release-name"] =
                        json!("foreign")
                }
                1 => changed.object["metadata"]["finalizers"] = json!(["example/finalizer"]),
                2 => {
                    changed.object["metadata"]["labels"]["argocd.argoproj.io/instance"] =
                        json!("managed")
                }
                3 => changed.namespace = "kube-system".into(),
                _ => changed.object["metadata"]["ownerReferences"] = json!([{"uid":"controller"}]),
            };
            assert!(deletion_instance(&changed, &options()).is_err());
        }
        Ok(())
    }
    #[test]
    fn test_event_has_no_automatic_logs_and_stored_results_must_be_fresh() -> Result<()> {
        let mut object = owned("test", "unused");
        object["metadata"]["annotations"]["helm.sh/hook"] = json!("test-success");
        let plan = crate::helm_extensions::hooks_from_manifests(
            &object.to_string(),
            "demo",
            "demo-api",
            crate::helm_extensions::Operation::Test,
            true,
        )?;
        assert_eq!(plan.events(), &["test"]);
        assert_eq!(plan.active().len(), 1);
        let args = lifecycle_args(
            &options(),
            &Request::Test,
            Path::new("/private/review"),
            false,
        );
        assert_eq!(&args[..2], &["test", "demo-api"]);
        assert!(!args.iter().any(|arg| arg == "--logs"
            || arg.starts_with("--wait")
            || arg.starts_with("--dry-run")
            || arg == "--no-hooks"));
        let old = helm::TestRun {
            name: "test".into(),
            kind: "ConfigMap".into(),
            started: "2026-10-22T08:00:00Z".into(),
            completed: "2026-10-22T08:00:01Z".into(),
            phase: "Failed".into(),
        };
        assert_eq!(
            fresh_test_counts(
                &plan,
                std::slice::from_ref(&old),
                std::slice::from_ref(&old)
            )?,
            (0, 0)
        );
        let mut new = old.clone();
        new.started = "2026-10-22T09:00:00Z".into();
        new.completed = "2026-10-22T09:00:01Z".into();
        assert_eq!(
            fresh_test_counts(
                &plan,
                std::slice::from_ref(&old),
                std::slice::from_ref(&new)
            )?,
            (0, 1)
        );
        new.phase = "Succeeded".into();
        assert_eq!(
            fresh_test_counts(
                &plan,
                std::slice::from_ref(&old),
                std::slice::from_ref(&new)
            )?,
            (1, 0)
        );
        new.completed = "bad timestamp".into();
        assert_eq!(fresh_test_counts(&plan, &[old], &[new])?, (0, 0));
        Ok(())
    }
    #[test]
    fn native_schema_delete_rechecks_membership_and_deletes_instances_before_schema() -> Result<()>
    {
        crate::cli::runtime()?.block_on(async {
            let demo=Gateway::connect(crate::gateway::Connection{demo:true,writable:true,allow_helm_write:true,allow_helm_crds:true,..Default::default()}).await?;
            let mut config=options();config.allow_crds=true;
            for mode in 0..3 {
                let mut prepared=prepare(&demo,config.clone(),Request::DeleteCrds{chart:"/demo/schema.tgz".into()}).await?;
                assert_eq!(prepared.confirmation(),"DELETE CRDS demo/demo-api");
                let schema=live_schema();let binding=crate::helm_crds::validate_live(schema.clone())?;
                let api=binding.definition.resource_api("example.com/v1","Widget").ok_or("fixture API")?;
                let object=custom_instance("other-owned","a");
                let instance=crate::helm_crds::Instance{namespace:"other-owned".into(),name:"a".into(),uid:"uid-a".into(),resource_version:"7".into(),object:object.clone()};
                prepared.crds=crate::helm_extensions::crds(&test_crd().to_string(),true)?;prepared.crd_seals=vec![Some(binding)];prepared.impact=vec![crate::helm_crds::Inventory{schema:"widgets.example.com".into(),api,instances:vec![instance]}];
                let writes=Arc::new(AtomicU64::new(0));let observed=writes.clone();let instance_deleted=Arc::new(AtomicBool::new(false));let gone=instance_deleted.clone();let schema_deleted=Arc::new(AtomicBool::new(false));let schema_gone=schema_deleted.clone();
                let client=mock_client(move|method,uri,body| {
                    if method==http::Method::GET && uri.contains("?limit=") {let mut items=vec![];if !gone.load(Ordering::SeqCst){let mut current=object.clone();if mode==1{current["metadata"]["uid"]=json!("replacement");}items.push(current);}return(200,json!({"apiVersion":"example.com/v1","kind":"WidgetList","metadata":{},"items":items}));}
                    if method==http::Method::GET {if schema_gone.load(Ordering::SeqCst){return(404,json!({"apiVersion":"v1","kind":"Status","code":404,"reason":"NotFound"}));}return(200,schema.clone());}
                    assert_eq!(method,http::Method::DELETE);assert!(body["dryRun"].is_null());assert_eq!(body["preconditions"]["resourceVersion"],"7");observed.fetch_add(1,Ordering::SeqCst);
                    if uri.contains("/namespaces/other-owned/") {assert_eq!(body["preconditions"]["uid"],"uid-a");if mode==2{return(409,json!({"apiVersion":"v1","kind":"Status","code":409,"reason":"Conflict","message":"PRIVATE_CANARY"}));}gone.store(true,Ordering::SeqCst);}
                    else {assert!(gone.load(Ordering::SeqCst));assert_eq!(body["preconditions"]["uid"],"schema-uid");schema_gone.store(true,Ordering::SeqCst);}
                    (200,json!({"apiVersion":"v1","kind":"Status","status":"Success"}))
                });
                let gateway=Gateway::for_helm_extension_test_client(client,"demo",static_connection()?,false,true);
                let result=execute_schema_mutation(&gateway,&prepared).await;
                match mode {0=>{result?;assert!(schema_deleted.load(Ordering::SeqCst));assert_eq!(writes.load(Ordering::SeqCst),2);},1=>{assert!(result.is_err_and(|error|error.contains("membership")));assert_eq!(writes.load(Ordering::SeqCst),0);},_=>{assert!(result.is_err_and(|error|error.contains("conflict")&&!error.contains("CANARY")));assert!(!schema_deleted.load(Ordering::SeqCst));}}
            }
            Ok(())
        })
    }
    #[test]
    fn stored_release_review_never_implies_supplied_keyring_enforcement() {
        let mut config = options();
        config.keyring = Some(PathBuf::from("/explicit/trusted-public.gpg"));
        for request in [
            Request::Test,
            Request::Rollback { revision: 1 },
            Request::Uninstall,
        ] {
            let notice = unverified_archive_notice(&request, config.keyring.is_some());
            assert!(notice.contains("no chart archive is acquired"));
            assert!(notice.contains("archive provenance is not reverified"));
            assert!(notice.contains("supplied --helm-keyring is not applied"));
            assert!(!notice.contains("no explicit --helm-keyring supplied"));
        }
        let unsigned = unverified_archive_notice(
            &Request::Install {
                chart: PathBuf::from("/local/chart.tgz"),
                values: None,
            },
            false,
        );
        assert_eq!(
            unsigned,
            "No OpenPGP signature verified: no explicit --helm-keyring supplied; content digest is integrity, not publisher identity.\n"
        );
    }
}
