//! Explicitly reviewed kubeconfig ExecCredential helpers. This supervises a
//! trusted native program; it does not sandbox the program's OS privileges.
use crate::{Result, safety};
use base64::{Engine, engine::general_purpose::STANDARD};
use k8s_openapi::jiff::Timestamp;
use kube::config::{AuthInfo, Cluster, ExecConfig, ExecInteractiveMode};
use ring::digest::{Context as Digest, SHA256};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
#[cfg(unix)]
use std::io::Read;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub const CONFIRMATION: &str = "TRUST AUTH HELPER";
const MAX_EXECUTABLE: u64 = 128 * 1024 * 1024;
const MAX_OUTPUT: usize = 256 * 1024;
const MAX_POLICY: u64 = 64 * 1024;
const PROCESS_BUDGET: Duration = Duration::from_secs(15);
const REVIEW_LIFETIME: Duration = Duration::from_secs(60);
const MAX_CREDENTIAL_LIFETIME: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Binding {
    context: String,
    user: String,
    server: String,
    tls_server_name: Option<String>,
    ca_sha256: String,
    exec_sha256: String,
    exec_info_sha256: String,
    #[serde(skip)]
    exec_info: String,
    #[serde(skip)]
    exec: ExecConfig,
}
impl Binding {
    pub(crate) fn new(
        context: &str,
        user: &str,
        cluster: &Cluster,
        exec: &ExecConfig,
    ) -> Result<Self> {
        validate_exec(exec)?;
        if [context, user].iter().any(|text| {
            text.is_empty() || text.len() > 253 || safety::safe_text(text, 253) != *text
        }) || cluster.tls_server_name.as_deref().is_some_and(|text| {
            text.is_empty()
                || text.len() > 253
                || !text.is_ascii()
                || safety::safe_text(text, 253) != text
        }) {
            return Err("helper context/user/TLS names must be bounded safe text".into());
        }
        let server = cluster
            .server
            .as_deref()
            .ok_or("cluster endpoint missing")?;
        let uri: http::Uri = server
            .parse()
            .map_err(|_| "invalid helper cluster endpoint")?;
        if uri.scheme_str() != Some("https")
            || uri.authority().is_none()
            || uri
                .authority()
                .is_some_and(|authority| authority.as_str().contains('@'))
            || uri.query().is_some()
            || server.len() > 4096
            || safety::safe_text(server, 4096) != server
            || cluster.insecure_skip_tls_verify == Some(true)
            || cluster.proxy_url.is_some()
        {
            return Err("auth helper trust requires a verified HTTPS endpoint without proxy or URL credentials".into());
        }
        let ca = match &cluster.certificate_authority_data {
            Some(data) => STANDARD
                .decode(data)
                .map_err(|_| "invalid helper cluster CA data")?,
            None => safety::read_small(
                Path::new(
                    cluster
                        .certificate_authority
                        .as_deref()
                        .ok_or("auth helper trust requires an explicit cluster CA")?,
                ),
                1024 * 1024,
            )?,
        };
        if ca.is_empty() || ca.len() > 1024 * 1024 {
            return Err("helper cluster CA is empty or exceeds its bound".into());
        }
        let mut info = json!({"apiVersion":exec.api_version,"kind":"ExecCredential","spec":{"interactive":false}});
        if exec.provide_cluster_info {
            let mut provided = json!({"server":server,"certificate-authority-data":STANDARD.encode(&ca),"insecure-skip-tls-verify":false});
            if let Some(name) = &cluster.tls_server_name {
                provided["tls-server-name"] = json!(name);
            }
            if let Some(extension) = cluster.extensions.as_ref().and_then(|extensions| {
                extensions
                    .iter()
                    .find(|extension| extension.name == "client.authentication.k8s.io/exec")
            }) {
                provided["config"] = extension.extension.clone();
            }
            info["spec"]["cluster"] = provided;
        }
        let exec_info =
            serde_json::to_string(&info).map_err(|_| "cannot prepare helper protocol input")?;
        if exec_info.len() > MAX_OUTPUT {
            return Err("helper protocol input exceeds 256 KiB".into());
        }
        let exec_value =
            serde_json::to_value(exec).map_err(|_| "cannot bind helper configuration")?;
        let exec_bytes =
            serde_json::to_vec(&exec_value).map_err(|_| "cannot bind helper configuration")?;
        Ok(Self {
            context: context.into(),
            user: user.into(),
            server: server.into(),
            tls_server_name: cluster.tls_server_name.clone(),
            ca_sha256: sha256(&ca),
            exec_sha256: sha256(&exec_bytes),
            exec_info_sha256: sha256(exec_info.as_bytes()),
            exec_info,
            exec: exec.clone(),
        })
    }
    fn matches(&self, other: &Self) -> bool {
        self.context == other.context
            && self.user == other.user
            && self.server == other.server
            && self.tls_server_name == other.tls_server_name
            && self.ca_sha256 == other.ca_sha256
            && self.exec_sha256 == other.exec_sha256
            && self.exec_info_sha256 == other.exec_info_sha256
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutableIdentity {
    sha256: String,
    length: u64,
    device: u64,
    inode: u64,
    mode: u32,
    modified_seconds: i64,
    modified_nanoseconds: i64,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema: u32,
    binding: Binding,
    executable: PathBuf,
    identity: ExecutableIdentity,
    #[serde(deserialize_with = "deserialize_environment")]
    environment: BTreeMap<String, String>,
}
fn deserialize_environment<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> std::result::Result<BTreeMap<String, String>, D::Error> {
    struct EnvironmentVisitor;
    impl<'de> serde::de::Visitor<'de> for EnvironmentVisitor {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a unique bounded reviewed environment")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> std::result::Result<Self::Value, A::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, String>()? {
                if result.len() >= 32 || result.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom(
                        "duplicate/oversized reviewed environment",
                    ));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(EnvironmentVisitor)
}
/// Immutable exact review. Approval saves a private policy, never runs a helper.
pub struct PreparedPolicy {
    manifest: Manifest,
    prepared_at: Instant,
}
impl PreparedPolicy {
    pub fn summary(&self) -> String {
        let m = &self.manifest;
        let argv = m.binding.exec.args.as_deref().unwrap_or(&[]);
        let environment = m
            .environment
            .iter()
            .map(|(key, value)| format!("{key}={value:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "REVIEW AUTHENTICATION HELPER\nContext: {}\nUser: {}\nEndpoint: {}\nCA SHA-256: {}\nTLS name: {}\nNative executable: {}\nExecutable SHA-256: {}\nConfigured command: {:?}\nExact arguments: {:?}\nApproved environment (all other inherited variables are removed):\n{}\nKUBERNETES_EXEC_INFO: version {}; interactive=false; cluster info={}\nLimits: 15 seconds, 256 KiB combined output; credentials require expiry within 24h.\nHelpers retain your OS user's filesystem/network permissions and may read provider configuration. This is process supervision, not an OS sandbox.\nHelper output and credentials are never displayed or saved. Credentials are acquired once per connection; expiry or API 401/403 blocks further requests until explicit reconnect.\nChanging executable, argv/environment, endpoint, CA, context or user invalidates this policy.\nApproval expires after 60 seconds. Type {CONFIRMATION} to save this trust policy.\n",
            m.binding.context,
            m.binding.user,
            m.binding.server,
            m.binding.ca_sha256,
            m.binding
                .tls_server_name
                .as_deref()
                .unwrap_or("endpoint hostname"),
            m.executable.display(),
            m.identity.sha256,
            m.binding.exec.command,
            argv,
            environment,
            m.binding.exec.api_version.as_deref().unwrap_or("-"),
            m.binding.exec.provide_cluster_info,
        )
    }
    pub fn save(self, path: &Path, confirmation: &str) -> Result<()> {
        if confirmation != CONFIRMATION || self.prepared_at.elapsed() > REVIEW_LIFETIME {
            return Err("auth helper review confirmation is invalid or expired".into());
        }
        revalidate(&self.manifest)?;
        let bytes = serde_json::to_vec_pretty(&self.manifest)
            .map_err(|_| "cannot encode helper trust policy")?;
        if bytes.len() as u64 > MAX_POLICY {
            return Err("helper trust policy exceeds 64 KiB".into());
        }
        safety::write_new(path, &bytes)
    }
}

pub(crate) fn prepare(
    binding: Binding,
    executable: Option<&Path>,
    explicit_env: &[(String, String)],
) -> Result<PreparedPolicy> {
    if !cfg!(unix) {
        return Err("reviewed auth helpers require verified Unix process-group cleanup; unsupported on this platform".into());
    }
    let path = executable
        .map(Path::to_owned)
        .or_else(|| binding.exec.command.as_deref().map(PathBuf::from))
        .ok_or("helper command missing")?;
    if !path.is_absolute() {
        return Err(
            "auth helper review needs an absolute native --executable; PATH lookup is disabled"
                .into(),
        );
    }
    let identity = executable_identity(&path)?;
    let mut environment = exec_environment(&binding.exec)?;
    for (key, value) in explicit_env {
        validate_env(key, value)?;
        if environment.insert(key.clone(), value.clone()).is_some() {
            return Err("auth helper environment contains duplicate/overridden names".into());
        }
    }
    if environment.len() > 32
        || environment
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            > 16 * 1024
    {
        return Err("auth helper environment exceeds its bound".into());
    }
    Ok(PreparedPolicy {
        manifest: Manifest {
            schema: 1,
            binding,
            executable: path,
            identity,
            environment,
        },
        prepared_at: Instant::now(),
    })
}

fn validate_exec(exec: &ExecConfig) -> Result<()> {
    if !matches!(
        exec.api_version.as_deref(),
        Some("client.authentication.k8s.io/v1" | "client.authentication.k8s.io/v1beta1")
    ) || exec.interactive_mode == Some(ExecInteractiveMode::Always)
        || (exec.api_version.as_deref() == Some("client.authentication.k8s.io/v1")
            && exec.interactive_mode.is_none())
        || !exec.other.is_empty()
    {
        return Err("helper requires a supported ExecCredential API version and noninteractive configuration".into());
    }
    let command = exec.command.as_deref().ok_or("helper command missing")?;
    if command.is_empty() || command.len() > 4096 || safety::safe_text(command, 4096) != command {
        return Err("invalid helper command text".into());
    }
    let args = exec.args.as_deref().unwrap_or(&[]);
    if args.len() > 64
        || args.iter().map(String::len).sum::<usize>() > 16 * 1024
        || args
            .iter()
            .any(|arg| arg.len() > 4096 || safety::safe_text(arg, 4096) != *arg)
    {
        return Err("helper arguments exceed bounds or contain controls".into());
    }
    exec_environment(exec)?;
    Ok(())
}
fn exec_environment(exec: &ExecConfig) -> Result<BTreeMap<String, String>> {
    let mut result = BTreeMap::new();
    for item in exec.env.as_deref().unwrap_or(&[]) {
        if item.len() != 2 {
            return Err("malformed helper environment entry".into());
        }
        let key = item.get("name").ok_or("helper environment name missing")?;
        let value = item
            .get("value")
            .ok_or("helper environment value missing")?;
        validate_env(key, value)?;
        if result.insert(key.clone(), value.clone()).is_some() {
            return Err("duplicate helper environment name".into());
        }
        if result.len() > 32 {
            return Err("helper environment exceeds its bound".into());
        }
    }
    Ok(result)
}
fn validate_env(key: &str, value: &str) -> Result<()> {
    let path = matches!(
        key,
        "HOME"
            | "USERPROFILE"
            | "XDG_CONFIG_HOME"
            | "AWS_CONFIG_FILE"
            | "AWS_SHARED_CREDENTIALS_FILE"
            | "GOOGLE_APPLICATION_CREDENTIALS"
            | "CLOUDSDK_CONFIG"
            | "AZURE_CONFIG_DIR"
    );
    let provider = matches!(
        key,
        "AWS_PROFILE"
            | "AWS_DEFAULT_PROFILE"
            | "AWS_REGION"
            | "AWS_DEFAULT_REGION"
            | "AWS_SDK_LOAD_CONFIG"
            | "AZURE_TENANT_ID"
            | "AZURE_CLIENT_ID"
            | "AZURE_AUTHORITY_HOST"
            | "CLOUDSDK_CORE_PROJECT"
    );
    if (!path && !provider)
        || value.is_empty()
        || value.len() > 4096
        || safety::safe_text(value, 4096) != value
    {
        return Err("helper environment allows only reviewed provider configuration paths/profile values; raw credential or loader variables are forbidden".into());
    }
    if path {
        let path = Path::new(value);
        if !path.is_absolute() || !path.exists() {
            return Err(
                "reviewed helper credential/configuration paths must be absolute and exist".into(),
            );
        }
        safety::checked_path(path)?;
    } else if value.len() > 253 || !value.is_ascii() {
        return Err("helper provider configuration values exceed their text bounds".into());
    }
    if key == "AZURE_AUTHORITY_HOST" {
        let endpoint: http::Uri = value
            .parse()
            .map_err(|_| "invalid Azure authority endpoint")?;
        if endpoint.scheme_str() != Some("https")
            || endpoint
                .authority()
                .is_none_or(|authority| authority.as_str().contains('@'))
            || endpoint.query().is_some()
        {
            return Err("Azure authority must be HTTPS without URL credentials".into());
        }
    }
    Ok(())
}

fn sha256(bytes: &[u8]) -> String {
    let mut digest = Digest::new(&SHA256);
    digest.update(bytes);
    hex_digest(digest.finish().as_ref())
}
fn hex_digest(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn executable_identity(path: &Path) -> Result<ExecutableIdentity> {
    safety::checked_path(path)?;
    if !path.is_absolute()
        || path
            .to_str()
            .is_none_or(|path| path.len() > 4096 || safety::safe_text(path, 4096) != path)
    {
        return Err("helper must have an absolute bounded executable path".into());
    }
    let filename = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if matches!(
        filename,
        "sh" | "bash"
            | "zsh"
            | "dash"
            | "fish"
            | "csh"
            | "tcsh"
            | "ksh"
            | "cmd"
            | "powershell"
            | "pwsh"
            | "node"
            | "perl"
            | "ruby"
            | "php"
    ) || filename.starts_with("python")
    {
        return Err("shells and generic interpreters are not auth helpers".into());
    }
    let before = fs::symlink_metadata(path).map_err(|_| "cannot inspect helper executable")?;
    if !before.is_file() || before.len() == 0 || before.len() > MAX_EXECUTABLE {
        return Err("helper must be a regular native executable of at most 128 MiB".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if before.mode() & 0o111 == 0 || before.mode() & 0o022 != 0 {
            return Err("helper must be executable and cannot be group/world writable".into());
        }
        let mut file = fs::File::open(path).map_err(|_| "cannot read helper executable")?;
        let mut digest = Digest::new(&SHA256);
        let mut length = 0u64;
        let mut buffer = [0u8; 16 * 1024];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|_| "cannot hash helper executable")?;
            if read == 0 {
                break;
            }
            if length == 0 && (read < 4 || !native_magic(&buffer[..4])) {
                return Err("helper must be a native executable; scripts/interpreters are not accepted as helpers".into());
            }
            length += read as u64;
            if length > MAX_EXECUTABLE {
                return Err("helper executable exceeds its bound".into());
            }
            digest.update(&buffer[..read]);
        }
        let after = fs::symlink_metadata(path).map_err(|_| "cannot recheck helper executable")?;
        if before.dev() != after.dev()
            || before.ino() != after.ino()
            || before.mode() != after.mode()
            || before.len() != length
            || before.len() != after.len()
            || before.mtime() != after.mtime()
            || before.mtime_nsec() != after.mtime_nsec()
        {
            return Err("helper executable changed while its identity was checked".into());
        }
        Ok(ExecutableIdentity {
            sha256: hex_digest(digest.finish().as_ref()),
            length,
            device: before.dev(),
            inode: before.ino(),
            mode: before.mode(),
            modified_seconds: before.mtime(),
            modified_nanoseconds: before.mtime_nsec(),
        })
    }
    #[cfg(not(unix))]
    {
        Err("auth helper executable identity is unavailable on this platform".into())
    }
}
#[cfg(unix)]
fn native_magic(magic: &[u8]) -> bool {
    matches!(
        magic,
        b"\x7fELF"
            | b"\xfe\xed\xfa\xce"
            | b"\xce\xfa\xed\xfe"
            | b"\xfe\xed\xfa\xcf"
            | b"\xcf\xfa\xed\xfe"
            | b"\xca\xfe\xba\xbe"
            | b"\xbe\xba\xfe\xca"
            | b"\xca\xfe\xba\xbf"
            | b"\xbf\xba\xfe\xca"
    )
}
fn revalidate(manifest: &Manifest) -> Result<()> {
    if manifest.schema != 1 || executable_identity(&manifest.executable)? != manifest.identity {
        return Err("auth helper executable identity changed; review a new trust policy".into());
    }
    for (key, value) in &manifest.environment {
        validate_env(key, value)?;
    }
    if manifest.environment.len() > 32
        || manifest
            .environment
            .iter()
            .map(|(key, value)| key.len() + value.len())
            .sum::<usize>()
            > 16 * 1024
    {
        return Err("helper environment exceeds its bound".into());
    }
    Ok(())
}

pub(crate) struct Credentials {
    until: Timestamp,
    material: Material,
}
enum Material {
    Token(String),
    Certificate(String, String),
}
impl Credentials {
    pub(crate) fn into_auth(self) -> Result<(AuthInfo, AuthGate)> {
        let mut user = AuthInfo::default();
        let token = match self.material {
            Material::Token(token) => Some(token),
            Material::Certificate(cert, key) => {
                user.client_certificate_data = Some(STANDARD.encode(cert));
                user.client_key_data = Some(STANDARD.encode(key).into());
                None
            }
        };
        Ok((user, AuthGate::new(self.until, token)?))
    }
}
pub(crate) fn acquire(policy: &Path, binding: &Binding) -> Result<Credentials> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata =
            fs::symlink_metadata(policy).map_err(|_| "cannot inspect auth helper policy")?;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("auth helper policy must be private (Unix mode 0600)".into());
        }
    }
    let bytes = safety::read_small(policy, MAX_POLICY)?;
    let manifest: Manifest = serde_json::from_slice(&bytes)
        .map_err(|_| "invalid helper trust policy (details suppressed)")?;
    if !manifest.binding.matches(binding) {
        return Err("auth helper trust binding changed; review this context/endpoint/CA/helper configuration again".into());
    }
    revalidate(&manifest)?;
    let configured = exec_environment(&binding.exec)?;
    if configured
        .iter()
        .any(|(key, value)| manifest.environment.get(key) != Some(value))
    {
        return Err("helper environment differs from approved policy".into());
    }
    let mut output = run_helper(&manifest, binding, PROCESS_BUDGET, MAX_OUTPUT)?;
    let credentials =
        decode_credentials(&output, binding.exec.api_version.as_deref().unwrap_or(""));
    output.fill(0);
    credentials
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CredentialOutput {
    api_version: String,
    kind: String,
    #[serde(default)]
    spec: Option<Value>,
    status: CredentialStatus,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CredentialStatus {
    expiration_timestamp: String,
    token: Option<String>,
    client_certificate_data: Option<String>,
    client_key_data: Option<String>,
}
fn decode_credentials(bytes: &[u8], expected_version: &str) -> Result<Credentials> {
    if bytes.len() > MAX_OUTPUT {
        return Err("helper credential output exceeds its bound".into());
    }
    let output: CredentialOutput = serde_json::from_slice(bytes)
        .map_err(|_| "invalid ExecCredential output; helper diagnostics suppressed")?;
    if output.api_version != expected_version
        || output.kind != "ExecCredential"
        || output.spec.as_ref().is_some_and(|spec| !spec.is_object())
    {
        return Err("helper returned an unexpected ExecCredential version or shape".into());
    }
    let until: Timestamp = output
        .status
        .expiration_timestamp
        .parse()
        .map_err(|_| "helper credential expiry is invalid")?;
    remaining_lifetime(until)?;
    let material = match (
        output.status.token,
        output.status.client_certificate_data,
        output.status.client_key_data,
    ) {
        (Some(token), None, None)
            if !token.is_empty()
                && token.len() <= 64 * 1024
                && token.bytes().all(|byte| byte.is_ascii_graphic()) =>
        {
            Material::Token(token)
        }
        (None, Some(cert), Some(key))
            if cert.len() <= 128 * 1024
                && key.len() <= 128 * 1024
                && cert.starts_with("-----BEGIN CERTIFICATE-----")
                && key.starts_with("-----BEGIN ")
                && pem_text(&cert)
                && pem_text(&key) =>
        {
            Material::Certificate(cert, key)
        }
        _ => {
            return Err(
                "helper must return one bounded token or a complete PEM certificate/key pair"
                    .into(),
            );
        }
    };
    Ok(Credentials { until, material })
}
fn pem_text(text: &str) -> bool {
    text.bytes()
        .all(|byte| byte.is_ascii_graphic() || matches!(byte, b' ' | b'\n' | b'\r' | b'\t'))
}
fn remaining_lifetime(until: Timestamp) -> Result<Duration> {
    let remaining = until.as_nanosecond() - Timestamp::now().as_nanosecond();
    let nanos = u64::try_from(remaining)
        .map_err(|_| "helper credential is expired or has an excessive lifetime")?;
    let duration = Duration::from_nanos(nanos);
    if duration.is_zero() || duration > MAX_CREDENTIAL_LIFETIME {
        return Err("helper credentials require future expiry within 24 hours".into());
    }
    Ok(duration)
}

#[derive(Clone, Default)]
pub(crate) struct AuthGate {
    state: Arc<Mutex<GateState>>,
    until: Option<Timestamp>,
    deadline: Option<Instant>,
}
#[derive(Default)]
struct GateState {
    blocked: bool,
    bearer: Option<http::HeaderValue>,
}
impl AuthGate {
    pub(crate) fn check_live(&self) -> Result<()> {
        self.authorize(&mut http::Request::new(()))
    }
    pub(crate) fn static_bearer(&self) -> Result<Option<String>> {
        let mut request = http::Request::new(());
        self.authorize(&mut request)?;
        request
            .headers()
            .get(http::header::AUTHORIZATION)
            .map(|header| {
                header
                    .to_str()
                    .ok()
                    .and_then(|value| value.strip_prefix("Bearer "))
                    .map(str::to_owned)
                    .ok_or_else(|| {
                        "validated bearer cannot be projected; credentials suppressed".to_owned()
                    })
            })
            .transpose()
    }
    fn new(until: Timestamp, token: Option<String>) -> Result<Self> {
        let duration = remaining_lifetime(until)?;
        let mut bearer = token
            .map(|token| {
                http::HeaderValue::from_str(&format!("Bearer {token}"))
                    .map_err(|_| "helper token cannot be used as an authorization header")
            })
            .transpose()?;
        if let Some(header) = &mut bearer {
            header.set_sensitive(true);
        }
        Ok(Self {
            state: Arc::new(Mutex::new(GateState {
                blocked: false,
                bearer,
            })),
            until: Some(until),
            deadline: Some(Instant::now() + duration),
        })
    }
    fn authorize<B>(&self, request: &mut http::Request<B>) -> Result<()> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "helper credential state is unavailable")?;
        if self.until.is_some_and(|until| Timestamp::now() >= until)
            || self
                .deadline
                .is_some_and(|deadline| Instant::now() >= deadline)
        {
            state.blocked = true;
            state.bearer = None;
        }
        if state.blocked {
            return Err(
                "auth helper credentials expired or were revoked; reconnect explicitly".into(),
            );
        }
        if let Some(bearer) = &state.bearer {
            request
                .headers_mut()
                .insert(http::header::AUTHORIZATION, bearer.clone());
        }
        Ok(())
    }
    fn response(&self, status: http::StatusCode) {
        if self.until.is_some()
            && matches!(status.as_u16(), 401 | 403)
            && let Ok(mut state) = self.state.lock()
        {
            state.blocked = true;
            state.bearer = None;
        }
    }
}
impl<S> tower::Layer<S> for AuthGate {
    type Service = AuthService<S>;
    fn layer(&self, inner: S) -> Self::Service {
        AuthService {
            inner,
            gate: self.clone(),
        }
    }
}
#[derive(Clone)]
pub(crate) struct AuthService<S> {
    inner: S,
    gate: AuthGate,
}
impl<S, B, R> tower::Service<http::Request<B>> for AuthService<S>
where
    S: tower::Service<http::Request<B>, Response = http::Response<R>>,
    S::Future: Send + 'static,
    S::Error: Into<tower::BoxError>,
{
    type Response = http::Response<R>;
    type Error = tower::BoxError;
    type Future = std::pin::Pin<
        Box<
            dyn std::future::Future<Output = std::result::Result<Self::Response, Self::Error>>
                + Send,
        >,
    >;
    fn poll_ready(
        &mut self,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::result::Result<(), Self::Error>> {
        self.inner.poll_ready(cx).map_err(Into::into)
    }
    fn call(&mut self, mut request: http::Request<B>) -> Self::Future {
        if self.gate.authorize(&mut request).is_err() {
            return Box::pin(async {
                Err(Box::new(kube::Error::Api(
                    kube::core::Status::failure(
                        "auth helper credentials expired or revoked; reconnect explicitly",
                        "Unauthorized",
                    )
                    .with_code(401)
                    .boxed(),
                )) as tower::BoxError)
            });
        }
        let future = self.inner.call(request);
        let gate = self.gate.clone();
        Box::pin(async move {
            let response = future.await.map_err(Into::into)?;
            gate.response(response.status());
            Ok(response)
        })
    }
}

#[cfg(unix)]
struct PrivateDirectory(PathBuf);
#[cfg(unix)]
impl PrivateDirectory {
    fn create() -> Result<Self> {
        use std::os::unix::fs::DirBuilderExt;
        use std::sync::atomic::{AtomicU64, Ordering};
        static SEQUENCE: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "helper temporary directory unavailable")?;
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|_| "system clock unavailable")?
            .as_nanos();
        let path = root.join(format!(
            "kborg-auth-{}-{stamp}-{}",
            std::process::id(),
            SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        safety::checked_path(&path)?;
        fs::DirBuilder::new()
            .mode(0o700)
            .create(&path)
            .map_err(|_| "cannot create private helper working directory")?;
        Ok(Self(path))
    }
}
#[cfg(unix)]
impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}
#[cfg(unix)]
enum Captured {
    Bytes(bool, Vec<u8>),
    Done,
    Failed,
}
#[cfg(unix)]
fn reader(
    mut input: impl Read + Send + 'static,
    stderr: bool,
    tx: std::sync::mpsc::SyncSender<Captured>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let mut buffer = [0u8; 8192];
        loop {
            match input.read(&mut buffer) {
                Ok(0) => {
                    let _ = tx.send(Captured::Done);
                    break;
                }
                Ok(read) => {
                    if tx
                        .send(Captured::Bytes(stderr, buffer[..read].to_vec()))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => {
                    let _ = tx.send(Captured::Failed);
                    break;
                }
            }
        }
    })
}
#[cfg(unix)]
struct Process {
    child: std::process::Child,
    killer: PathBuf,
    cleaned: bool,
}
#[cfg(unix)]
impl Process {
    fn cleanup(&mut self) -> Result<()> {
        use std::process::{Command, Stdio};
        if self.cleaned {
            return Ok(());
        }
        let until = Instant::now() + Duration::from_secs(1);
        let mut failure = None;
        // Fixed trusted OS signal utility, passed literal argv; never a shell.
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
                        std::thread::sleep(Duration::from_millis(5))
                    }
                    _ => {
                        let _ = signal.kill();
                        let _ = signal.try_wait();
                        failure = Some("auth helper process-group cleanup failed");
                        break;
                    }
                }
            },
            Err(_) => failure = Some("cannot signal auth helper process group"),
        }
        let _ = self.child.kill();
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(5)),
                _ => {
                    failure = Some("cannot reap auth helper process");
                    break;
                }
            }
        }
        self.cleaned = true;
        failure.map_or(Ok(()), |message| Err(message.into()))
    }
}
#[cfg(unix)]
impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

#[cfg(unix)]
fn run_helper(
    manifest: &Manifest,
    binding: &Binding,
    budget: Duration,
    limit: usize,
) -> Result<Vec<u8>> {
    use std::{
        os::unix::process::CommandExt,
        process::{Command, Stdio},
        sync::mpsc,
    };
    let killer = Path::new("/bin/kill")
        .canonicalize()
        .map_err(|_| "verified process-group cleanup unavailable")?;
    executable_identity(&killer)?;
    let directory = PrivateDirectory::create()?;
    let mut command = Command::new(&manifest.executable);
    command
        .args(binding.exec.args.as_deref().unwrap_or(&[]))
        .env_clear()
        .env("HOME", &directory.0)
        .env("USERPROFILE", &directory.0)
        .env("TMPDIR", &directory.0)
        .env("LANG", "C")
        .env("LC_ALL", "C")
        .envs(&manifest.environment)
        .env("KUBERNETES_EXEC_INFO", &binding.exec_info)
        .current_dir(&directory.0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0);
    revalidate(manifest)?;
    let child = command
        .spawn()
        .map_err(|_| "cannot start reviewed auth helper")?;
    let mut process = Process {
        child,
        killer,
        cleaned: false,
    };
    let (tx, rx) = mpsc::sync_channel(16);
    let stdout = reader(
        process
            .child
            .stdout
            .take()
            .ok_or("helper stdout unavailable")?,
        false,
        tx.clone(),
    );
    let stderr = reader(
        process
            .child
            .stderr
            .take()
            .ok_or("helper stderr unavailable")?,
        true,
        tx,
    );
    let started = Instant::now();
    let mut output = Vec::new();
    let mut total = 0usize;
    let mut done = 0usize;
    let mut status = None;
    let mut failure = None;
    while done < 2 || status.is_none() {
        if started.elapsed() >= budget {
            failure = Some("auth helper exceeded 15-second time budget");
            break;
        }
        match rx.recv_timeout(Duration::from_millis(10)) {
            Ok(Captured::Bytes(is_stderr, mut bytes)) => {
                total = total.saturating_add(bytes.len());
                if total > limit {
                    bytes.fill(0);
                    failure = Some("auth helper exceeded combined output limit");
                    break;
                }
                // Stderr is counted but never retained, printed or interpreted.
                if !is_stderr {
                    output.extend_from_slice(&bytes);
                }
                bytes.fill(0);
            }
            Ok(Captured::Done) => done += 1,
            Ok(Captured::Failed) => {
                failure = Some("cannot read auth helper output");
                break;
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                if done < 2 {
                    failure = Some("helper output capture stopped unexpectedly");
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
        if status.is_none() {
            status = process
                .child
                .try_wait()
                .map_err(|_| "cannot inspect helper process")?;
            if status.is_some() {
                process.cleanup()?;
            }
        }
    }
    let cleanup = process.cleanup();
    drop(rx);
    let drain_until = Instant::now() + Duration::from_millis(500);
    while !(stdout.is_finished() && stderr.is_finished()) && Instant::now() < drain_until {
        std::thread::sleep(Duration::from_millis(5));
    }
    let finished = stdout.is_finished() && stderr.is_finished();
    if stdout.is_finished() {
        let _ = stdout.join();
    }
    if stderr.is_finished() {
        let _ = stderr.join();
    }
    if cleanup.is_err()
        || !finished
        || failure.is_some()
        || !status.is_some_and(|status| status.success())
    {
        output.fill(0);
        cleanup?;
        if !finished {
            return Err(
                "helper retained pipe handles outside its supervised group; authentication stopped"
                    .into(),
            );
        }
        return Err(failure
            .unwrap_or("auth helper exited unsuccessfully; diagnostics suppressed")
            .into());
    }
    Ok(output)
}
#[cfg(not(unix))]
fn run_helper(_: &Manifest, _: &Binding, _: Duration, _: usize) -> Result<Vec<u8>> {
    Err("reviewed auth helpers are unsupported without verified process-group cleanup".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tower::Layer;

    fn expiry(seconds: i64) -> String {
        Timestamp::from_second(Timestamp::now().as_second() + seconds)
            .map(|timestamp| timestamp.to_string())
            .unwrap_or_default()
    }
    fn credential(version: &str, token: &str) -> Vec<u8> {
        serde_json::to_vec(&json!({"apiVersion":version,"kind":"ExecCredential","status":{"token":token,"expirationTimestamp":expiry(60)}})).unwrap_or_default()
    }
    fn binding(args: Vec<String>) -> Result<Binding> {
        let exec = ExecConfig {
            api_version: Some("client.authentication.k8s.io/v1".into()),
            command: Some("configured-provider".into()),
            args: Some(args),
            interactive_mode: Some(ExecInteractiveMode::Never),
            provide_cluster_info: true,
            ..Default::default()
        };
        let cluster = Cluster {
            server: Some("https://127.0.0.1:6443".into()),
            certificate_authority_data: Some(STANDARD.encode(b"TEST-CA")),
            ..Default::default()
        };
        Binding::new("test-context", "test-user", &cluster, &exec)
    }

    #[test]
    fn credential_versions_material_expiry_and_errors_are_strict_and_secret_free() -> Result<()> {
        for version in [
            "client.authentication.k8s.io/v1",
            "client.authentication.k8s.io/v1beta1",
        ] {
            let credentials = decode_credentials(&credential(version, "TEST-TOKEN"), version)?;
            let (user, gate) = credentials.into_auth()?;
            assert!(user.exec.is_none() && user.token.is_none());
            let mut request = http::Request::new(());
            gate.authorize(&mut request)?;
            let header = &request.headers()[http::header::AUTHORIZATION];
            assert_eq!(
                header.to_str().map_err(|_| "invalid test header")?,
                "Bearer TEST-TOKEN"
            );
            assert!(header.is_sensitive());
        }
        let valid = credential("client.authentication.k8s.io/v1", "TEST-TOKEN");
        let base: Value = serde_json::from_slice(&valid).map_err(|_| "test JSON failed")?;
        for change in [
            json!({"apiVersion":"bad"}),
            json!({"kind":"Bad"}),
            json!({"status":{"token":"CANARY-SECRET"}}),
            json!({"status":{"token":"CANARY-SECRET\u{1b}[31m","expirationTimestamp":expiry(60)}}),
            json!({"status":{"token":"CANARY-SECRET","expirationTimestamp":expiry(-1)}}),
            json!({"status":{"token":"CANARY-SECRET","expirationTimestamp":expiry(90_000)}}),
            json!({"status":{"token":"CANARY-SECRET","clientKeyData":"KEY","expirationTimestamp":expiry(60)}}),
        ] {
            let mut value = base.clone();
            for (key, replacement) in change.as_object().ok_or("test object missing")? {
                value[key] = replacement.clone();
            }
            let error = decode_credentials(
                &serde_json::to_vec(&value).map_err(|_| "test JSON failed")?,
                "client.authentication.k8s.io/v1",
            )
            .err()
            .ok_or("malformed credentials accepted")?;
            assert!(!error.contains("CANARY") && !error.chars().any(char::is_control));
        }
        let cert = json!({"apiVersion":"client.authentication.k8s.io/v1","kind":"ExecCredential","status":{"clientCertificateData":"-----BEGIN CERTIFICATE-----\nTEST\n-----END CERTIFICATE-----","clientKeyData":"-----BEGIN PRIVATE KEY-----\nTEST\n-----END PRIVATE KEY-----","expirationTimestamp":expiry(60)}});
        let decoded = decode_credentials(
            &serde_json::to_vec(&cert).map_err(|_| "test JSON failed")?,
            "client.authentication.k8s.io/v1",
        )?;
        let (user, _) = decoded.into_auth()?;
        assert!(user.client_certificate_data.is_some() && user.client_key_data.is_some());
        assert!(
            decode_credentials(
                &vec![b'x'; MAX_OUTPUT + 1],
                "client.authentication.k8s.io/v1"
            )
            .is_err()
        );
        assert!(decode_credentials(br#"{"apiVersion":"client.authentication.k8s.io/v1","kind":"ExecCredential","status":{"token":"ONE","token":"TWO","expirationTimestamp":"2030-01-01T00:00:00Z"}}"#, "client.authentication.k8s.io/v1").is_err());
        Ok(())
    }

    #[test]
    fn helper_configuration_rejects_interactive_controls_unpinned_tls_and_secret_environment()
    -> Result<()> {
        let valid = binding(vec!["literal; $(touch SHOULD_NOT_EXIST)".into()])?;
        for (key, value) in [
            ("AWS_SECRET_ACCESS_KEY", "CANARY"),
            ("AWS_SESSION_TOKEN", "CANARY"),
            ("KUBECONFIG", "/tmp/config"),
            ("PATH", "/bin"),
            ("LD_PRELOAD", "/tmp/module"),
            ("DYLD_INSERT_LIBRARIES", "/tmp/module"),
            ("HOME", "relative"),
            ("AWS_PROFILE", "bad\u{1b}"),
            ("AZURE_AUTHORITY_HOST", "https://user:CANARY@example.com"),
        ] {
            let error = validate_env(key, value)
                .err()
                .ok_or("unsafe helper environment accepted")?;
            assert!(!error.contains("CANARY"));
        }
        let mut exec = valid.exec.clone();
        exec.interactive_mode = Some(ExecInteractiveMode::Always);
        assert!(validate_exec(&exec).is_err());
        exec.interactive_mode = None;
        assert!(validate_exec(&exec).is_err());
        exec = valid.exec.clone();
        exec.args = Some(vec!["bad\u{1b}".into()]);
        assert!(validate_exec(&exec).is_err());
        let cluster = Cluster {
            server: Some("https://example.com".into()),
            ..Default::default()
        };
        assert!(Binding::new("context", "user", &cluster, &valid.exec).is_err());
        let insecure = Cluster {
            server: Some("http://example.com".into()),
            certificate_authority_data: Some(STANDARD.encode(b"TEST-CA")),
            ..Default::default()
        };
        assert!(Binding::new("context", "user", &insecure, &valid.exec).is_err());
        assert_eq!(
            sha256(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn reviewed_native_policy_binds_rotation_context_endpoint_ca_argv_and_environment() -> Result<()>
    {
        use std::os::unix::fs::PermissionsExt;
        let directory = PrivateDirectory::create()?;
        let native = Path::new("/bin/echo")
            .canonicalize()
            .map_err(|_| "native echo unavailable")?;
        let body = String::from_utf8(credential(
            "client.authentication.k8s.io/v1",
            "CANARY-TEST-TOKEN",
        ))
        .map_err(|_| "invalid test UTF-8")?;
        let binding = binding(vec![body])?;
        let policy = directory.0.join("policy.json");
        let prepared = prepare(
            binding.clone(),
            Some(&native),
            &[("AWS_PROFILE".into(), "test".into())],
        )?;
        assert!(prepared.summary().contains("SHA-256"));
        prepared.save(&policy, CONFIRMATION)?;
        let manifest = String::from_utf8(safety::read_small(&policy, MAX_POLICY)?)
            .map_err(|_| "invalid test UTF-8")?;
        assert!(!manifest.contains("CANARY-TEST-TOKEN"));
        assert!(acquire(&policy, &binding).is_ok());
        assert!(
            directory
                .0
                .read_dir()
                .map_err(|_| "test directory unreadable")?
                .count()
                == 1
        );
        for change in 0..5 {
            let mut changed = binding.clone();
            match change {
                0 => changed.context = "other".into(),
                1 => changed.server = "https://other:6443".into(),
                2 => changed.ca_sha256 = sha256(b"OTHER-CA"),
                3 => changed.exec_sha256 = sha256(b"CHANGED-ARGV"),
                _ => changed.tls_server_name = Some("other".into()),
            }
            assert!(acquire(&policy, &changed).is_err());
        }
        fs::set_permissions(&policy, fs::Permissions::from_mode(0o644))
            .map_err(|_| "cannot set test permissions")?;
        assert!(acquire(&policy, &binding).is_err());
        let copied = directory.0.join("copied-native");
        fs::copy(&native, &copied).map_err(|_| "cannot copy native fixture")?;
        fs::set_permissions(&copied, fs::Permissions::from_mode(0o700))
            .map_err(|_| "cannot set test permissions")?;
        let prepared = prepare(binding.clone(), Some(&copied), &[])?;
        let modified = fs::metadata(&copied)
            .and_then(|metadata| metadata.modified())
            .map_err(|_| "fixture metadata missing")?;
        use std::io::{Seek, Write};
        let previous_last = safety::read_small(&copied, MAX_EXECUTABLE)?
            .last()
            .copied()
            .ok_or("test fixture empty")?;
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(&copied)
            .map_err(|_| "fixture cannot be changed")?;
        file.seek(std::io::SeekFrom::End(-1))
            .and_then(|_| file.write_all(&[previous_last ^ 0xff]))
            .and_then(|_| file.set_times(fs::FileTimes::new().set_modified(modified)))
            .map_err(|_| "fixture cannot be changed")?;
        assert!(
            prepared
                .save(&directory.0.join("changed-policy.json"), CONFIRMATION)
                .is_err()
        );
        let duplicate = manifest.replace(
            "\"AWS_PROFILE\": \"test\"",
            "\"AWS_PROFILE\": \"test\", \"AWS_PROFILE\": \"different\"",
        );
        assert!(serde_json::from_str::<Manifest>(&duplicate).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn policy_confirmation_expiry_scripts_symlinks_and_implicit_path_are_rejected() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let directory = PrivateDirectory::create()?;
        let native = Path::new("/bin/echo")
            .canonicalize()
            .map_err(|_| "native echo unavailable")?;
        assert!(prepare(binding(vec![])?, None, &[]).is_err());
        assert!(prepare(binding(vec![])?, Some(Path::new("echo")), &[]).is_err());
        assert!(prepare(binding(vec![])?, Some(Path::new("/bin/sh")), &[]).is_err());
        let script = directory.0.join("script");
        fs::write(&script, b"#!/bin/sh\necho CANARY\n")
            .map_err(|_| "cannot create script fixture")?;
        fs::set_permissions(&script, fs::Permissions::from_mode(0o700))
            .map_err(|_| "cannot set test permissions")?;
        assert!(prepare(binding(vec![])?, Some(&script), &[]).is_err());
        let link = directory.0.join("link");
        symlink(&native, &link).map_err(|_| "cannot create symlink fixture")?;
        assert!(prepare(binding(vec![])?, Some(&link), &[]).is_err());
        let prepared = prepare(binding(vec![])?, Some(&native), &[])?;
        assert!(
            prepared
                .save(&directory.0.join("bad-confirmation"), "YES")
                .is_err()
        );
        let mut expired = prepare(binding(vec![])?, Some(&native), &[])?;
        expired.prepared_at = Instant::now() - REVIEW_LIFETIME - Duration::from_secs(1);
        assert!(
            expired
                .save(&directory.0.join("expired"), CONFIRMATION)
                .is_err()
        );
        assert_eq!(
            directory
                .0
                .read_dir()
                .map_err(|_| "test directory unreadable")?
                .count(),
            2
        );
        Ok(())
    }

    #[test]
    fn expiration_and_forbidden_transport_clear_bearer_and_prevent_future_sends() -> Result<()> {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let runtime = crate::cli::runtime()?;
        runtime.block_on(async {
            let until: Timestamp = expiry(60).parse().map_err(|_| "invalid test time")?;
            let gate = AuthGate::new(until, Some("CANARY-TEST-TOKEN".into()))?;
            let count = Arc::new(AtomicUsize::new(0));
            let calls = count.clone();
            let service = tower::service_fn(move |request: http::Request<kube::client::Body>| {
                calls.fetch_add(1, Ordering::SeqCst);
                assert!(request.headers()[http::header::AUTHORIZATION].is_sensitive());
                async {
                    Ok::<_, std::convert::Infallible>(http::Response::builder().status(403).body(http_body_util::Full::new(bytes::Bytes::from_static(br#"{"kind":"Status","apiVersion":"v1","status":"Failure","reason":"Forbidden","message":"test","code":403}"#))).unwrap_or_else(|_| panic!("test response invalid")))
                }
            });
            let client = kube::Client::new(gate.layer(service), "test");
            let response = client.request::<Value>(http::Request::new(vec![])).await;
            assert!(matches!(response, Err(kube::Error::Api(status)) if status.code == 403));
            let response = client.request::<Value>(http::Request::new(vec![])).await;
            assert!(matches!(response, Err(kube::Error::Api(status)) if status.code == 401 && !status.message.contains("CANARY")));
            assert_eq!(count.load(Ordering::SeqCst), 1);
            assert!(gate.state.lock().map_err(|_| "test gate unavailable")?.bearer.is_none());
            let mut expired = AuthGate::new(until, Some("TEST-TOKEN".into()))?;
            expired.deadline = Some(Instant::now() - Duration::from_secs(1));
            assert!(expired.authorize(&mut http::Request::new(())).is_err());
            assert!(expired.state.lock().map_err(|_| "test gate unavailable")?.bearer.is_none());
            // Every explicit reconnect receives a fresh independent session.
            let reconnected = AuthGate::new(until, Some("NEW-TOKEN".into()))?;
            assert!(reconnected.authorize(&mut http::Request::new(())).is_ok());
            Ok(())
        })
    }

    #[cfg(unix)]
    fn fixture(mode: &str) -> Result<PreparedPolicy> {
        let native = std::env::current_exe().map_err(|_| "native fixture unavailable")?;
        let binding = binding(
            [
                "--ignored",
                "--exact",
                "auth::tests::process_fixture",
                "--nocapture",
            ]
            .map(String::from)
            .to_vec(),
        )?;
        prepare(
            binding,
            Some(&native),
            &[("AWS_PROFILE".into(), mode.into())],
        )
    }
    #[cfg(unix)]
    #[test]
    fn supervised_helpers_use_minimal_environment_and_bound_flood_timeout_descendants() -> Result<()>
    {
        let copied = fixture("copy")?;
        let output = run_helper(
            &copied.manifest,
            &copied.manifest.binding,
            Duration::from_secs(2),
            MAX_OUTPUT,
        )?;
        assert!(String::from_utf8_lossy(&output).contains("minimal_environment=true"));
        let sleeping = fixture("sleep")?;
        let started = Instant::now();
        let error = run_helper(
            &sleeping.manifest,
            &sleeping.manifest.binding,
            Duration::from_millis(100),
            MAX_OUTPUT,
        )
        .err()
        .ok_or("sleeping helper accepted")?;
        assert!(!error.contains("CANARY"));
        assert!(started.elapsed() < Duration::from_secs(3));
        let flood = fixture("flood")?;
        let error = run_helper(
            &flood.manifest,
            &flood.manifest.binding,
            Duration::from_secs(2),
            8192,
        )
        .err()
        .ok_or("flood helper accepted")?;
        assert!(!error.contains("CANARY"));
        let descendant = fixture("descendant")?;
        let started = Instant::now();
        let output = run_helper(
            &descendant.manifest,
            &descendant.manifest.binding,
            Duration::from_secs(2),
            MAX_OUTPUT,
        )?;
        assert!(String::from_utf8_lossy(&output).contains("descendant_pid="));
        assert!(started.elapsed() < Duration::from_secs(3));
        Ok(())
    }
    #[cfg(unix)]
    #[test]
    #[ignore]
    fn process_fixture() -> Result<()> {
        use std::process::{Command, Stdio};
        match std::env::var("AWS_PROFILE").as_deref() {
            Ok("sleep") => std::thread::sleep(Duration::from_secs(30)),
            Ok("flood") => {
                use std::io::Write;
                let mut stderr = std::io::stderr().lock();
                for _ in 0..1024 {
                    if stderr.write_all(&[b'x'; 8192]).is_err() {
                        break;
                    }
                }
            }
            Ok("descendant") => {
                let executable =
                    std::env::current_exe().map_err(|_| "fixture executable unavailable")?;
                let child = Command::new(executable)
                    .args([
                        "--ignored",
                        "--exact",
                        "auth::tests::process_fixture",
                        "--nocapture",
                    ])
                    .env("AWS_PROFILE", "sleep")
                    .stdin(Stdio::null())
                    .stdout(Stdio::inherit())
                    .stderr(Stdio::inherit())
                    .spawn()
                    .map_err(|_| "cannot spawn descendant")?;
                println!("descendant_pid={}", child.id());
            }
            _ => {
                let minimal = std::env::vars_os().all(|(key, _)| {
                    matches!(
                        key.to_str(),
                        Some(
                            "HOME"
                                | "USERPROFILE"
                                | "TMPDIR"
                                | "LANG"
                                | "LC_ALL"
                                | "KUBERNETES_EXEC_INFO"
                                | "AWS_PROFILE"
                        )
                    ) || (cfg!(target_os = "macos") && key == "__CF_USER_TEXT_ENCODING")
                });
                let info: Value = serde_json::from_str(
                    &std::env::var("KUBERNETES_EXEC_INFO")
                        .map_err(|_| "helper protocol missing")?,
                )
                .map_err(|_| "helper protocol malformed")?;
                assert_eq!(info["spec"]["interactive"], false);
                assert_eq!(info["spec"]["cluster"]["server"], "https://127.0.0.1:6443");
                println!("minimal_environment={minimal}");
            }
        }
        Ok(())
    }
}
