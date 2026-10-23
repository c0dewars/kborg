//! Helm OCI acquisition sealed to one digest, independent of Kubernetes credentials.
//! Downloads are bounded and verified before a private packaged chart is written.
//! Digest integrity is not signature/authorship verification.
use crate::{Result, safety};
use base64::{Engine, engine::general_purpose::STANDARD};
use http::{HeaderMap, HeaderValue, StatusCode, Uri};
use http_body_util::BodyExt;
use kube::{Client, Config, client::Body};
use ring::digest::{SHA256, digest};
use serde::Deserialize;
use serde_json::Value;
use std::{
    collections::BTreeMap,
    net::IpAddr,
    path::{Path, PathBuf},
    time::Duration,
};

const MANIFEST_TYPE: &str = "application/vnd.oci.image.manifest.v1+json";
const CONFIG_TYPE: &str = "application/vnd.cncf.helm.config.v1+json";
const CHART_TYPE: &str = "application/vnd.cncf.helm.chart.content.v1.tar+gzip";
const PROVENANCE_TYPE: &str = "application/vnd.cncf.helm.chart.provenance.v1.prov";
const MAX_MANIFEST: usize = 256 * 1024;
const MAX_CONFIG: usize = 256 * 1024;
const MAX_CHART: usize = 8 * 1024 * 1024;
const MAX_PROVENANCE: usize = 1024 * 1024;
const MAX_TOKEN: usize = 16 * 1024;
const IDLE: Duration = Duration::from_secs(10);
const ACQUISITION: Duration = Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reference {
    source: String,
    registry: String,
    repository: String,
    selector: String,
}
impl Reference {
    pub fn parse(source: &str) -> Result<Self> {
        let rest = source
            .strip_prefix("oci://")
            .ok_or("OCI chart reference must start with oci://")?;
        if source.len() > 1024 || !source.is_ascii() || source.bytes().any(|b| b.is_ascii_control())
        {
            return Err("OCI chart reference is too long or contains invalid characters".into());
        }
        let (registry, target) = rest
            .split_once('/')
            .ok_or("OCI chart reference needs a registry and repository/chart name")?;
        let (repository, selector) = if let Some((repository, digest)) = target.rsplit_once('@') {
            if !valid_digest(digest) {
                return Err("OCI digest must be @sha256:<64 lowercase hex>".into());
            }
            (repository, digest)
        } else {
            let (repository, tag) = target.rsplit_once(':').ok_or("OCI chart needs an explicit :TAG or @sha256 digest; implicit latest is not accepted")?;
            if !valid_tag(tag) {
                return Err("OCI tag must contain 1..128 ASCII letters, digits, underscores, dots or dashes; use underscore for Helm version build metadata".into());
            }
            (repository, tag)
        };
        if !valid_authority(registry) || !valid_repository(repository) {
            return Err("invalid OCI registry or repository; userinfo, queries, escapes and traversal are not accepted".into());
        }
        Ok(Self {
            source: source.into(),
            registry: registry.into(),
            repository: repository.into(),
            selector: selector.into(),
        })
    }
    pub fn as_str(&self) -> &str {
        &self.source
    }
    pub fn registry(&self) -> &str {
        &self.registry
    }
    pub fn repository(&self) -> &str {
        &self.repository
    }
    pub fn digest(&self) -> Option<&str> {
        valid_digest(&self.selector).then_some(self.selector.as_str())
    }
    pub fn tag(&self) -> Option<&str> {
        (!valid_digest(&self.selector)).then_some(self.selector.as_str())
    }
    pub fn selector(&self) -> &str {
        &self.selector
    }
    fn uri(&self, kind: &str, id: &str) -> Result<Uri> {
        format!(
            "https://{}/v2/{}/{kind}/{id}",
            self.registry, self.repository
        )
        .parse()
        .map_err(|_| "cannot build OCI request".into())
    }
}

fn valid_tag(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .as_bytes()
            .first()
            .is_some_and(|b| b.is_ascii_alphanumeric() || *b == b'_')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
}

fn valid_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}
fn valid_authority(value: &str) -> bool {
    if value.is_empty()
        || value.len() > 253
        || value
            .bytes()
            .any(|b| !b.is_ascii_lowercase() && !b.is_ascii_digit() && !b".-:[]".contains(&b))
    {
        return false;
    }
    let Ok(authority) = value.parse::<http::uri::Authority>() else {
        return false;
    };
    if authority.as_str() != value || authority.host().is_empty() {
        return false;
    }
    let suffix = value.strip_prefix(authority.host()).unwrap_or("invalid");
    if !suffix.is_empty()
        && suffix.strip_prefix(':').is_none_or(|port| {
            port.is_empty()
                || !port.bytes().all(|b| b.is_ascii_digit())
                || port.parse::<u16>().ok().is_none_or(|number| number == 0)
        })
    {
        return false;
    }
    let host = authority
        .host()
        .trim_start_matches('[')
        .trim_end_matches(']');
    if host.parse::<IpAddr>().is_ok() {
        return true;
    }
    // Reject legacy numeric IPv4 spellings which system resolvers may treat as
    // loopback despite failing Rust's canonical IpAddr parser.
    if host.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        || (host.starts_with("0x") && host[2..].bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return false;
    }
    !host.contains(':')
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                && !label.starts_with('-')
                && !label.ends_with('-')
        })
}
fn valid_repository(value: &str) -> bool {
    if value.is_empty() || value.len() > 255 {
        return false;
    }
    value.split('/').all(|part| {
        !part.is_empty()
            && part.len() <= 128
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
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b))
    })
}

#[derive(Clone, Debug, Default)]
pub struct Options {
    pub ca_file: Option<PathBuf>,
    pub auth_file: Option<PathBuf>,
}
#[derive(Clone, Debug, serde::Serialize)]
pub struct Acquired {
    pub reference: String,
    pub resolved_reference: String,
    pub manifest_digest: String,
    pub chart_digest: String,
    pub config_digest: String,
    pub chart_bytes: u64,
    pub chart_filename: String,
    pub provenance_path: Option<PathBuf>,
    /// Integrity only: the provenance signature is not verified by this module.
    pub provenance_digest: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuthFile {
    registry: String,
    username: Option<String>,
    password: Option<String>,
    token: Option<String>,
}
// Deliberately no Debug/Serialize: caller-supplied and exchanged credentials.
struct Credentials {
    direct: Option<HeaderValue>,
    basic: Option<HeaderValue>,
}
fn bearer(token: &str) -> Result<HeaderValue> {
    if token.is_empty()
        || token.len() > MAX_TOKEN
        || token.bytes().any(|b| !(0x21..=0x7e).contains(&b))
    {
        return Err("registry returned an invalid bearer token; details suppressed".into());
    }
    let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| "invalid registry bearer token")?;
    value.set_sensitive(true);
    Ok(value)
}
fn credentials(path: Option<&Path>, reference: &Reference) -> Result<Credentials> {
    let Some(path) = path else {
        return Ok(Credentials {
            direct: None,
            basic: None,
        });
    };
    safety::checked_path(path)?;
    let meta =
        std::fs::symlink_metadata(path).map_err(|_| "cannot inspect registry credential file")?;
    if !meta.is_file() || meta.len() > MAX_TOKEN as u64 {
        return Err("registry credentials need a bounded private regular file".into());
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            return Err(
                "registry credential file must have private 0600 or 0400 permissions".into(),
            );
        }
    }
    let auth: AuthFile = serde_json::from_slice(&safety::read_small(path, MAX_TOKEN as u64)?)
        .map_err(|_| "invalid registry credential JSON; expected registry plus username/password or token; details suppressed")?;
    if auth.registry != reference.registry {
        return Err("registry credentials are bound to a different host/port".into());
    }
    match (auth.username, auth.password, auth.token) {
        (None, None, Some(token)) => Ok(Credentials {
            direct: Some(bearer(&token)?),
            basic: None,
        }),
        (Some(username), Some(password), None)
            if !username.is_empty()
                && !password.is_empty()
                && !username.contains(':')
                && !username
                    .chars()
                    .chain(password.chars())
                    .any(char::is_control) =>
        {
            let mut basic = HeaderValue::from_str(&format!(
                "Basic {}",
                STANDARD.encode(format!("{username}:{password}"))
            ))
            .map_err(|_| "invalid registry basic credentials")?;
            basic.set_sensitive(true);
            Ok(Credentials {
                direct: Some(basic.clone()),
                basic: Some(basic),
            })
        }
        _ => Err(
            "registry credentials require exactly username/password or token; details suppressed"
                .into(),
        ),
    }
}
fn ca_bundle(path: Option<&Path>) -> Result<Option<Vec<Vec<u8>>>> {
    let Some(path) = path else {
        return Ok(None);
    };
    let bytes = safety::read_small(path, 32 * 1024)?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| "registry CA must be a public PEM certificate bundle")?;
    let mut rest = text.trim();
    let mut roots = Vec::new();
    while !rest.is_empty() && roots.len() < 8 {
        rest = rest
            .strip_prefix("-----BEGIN CERTIFICATE-----")
            .ok_or("registry CA must contain only public PEM certificates")?;
        let (body, after) = rest
            .split_once("-----END CERTIFICATE-----")
            .ok_or("incomplete registry CA certificate")?;
        let compact: String = body.chars().filter(|c| !c.is_ascii_whitespace()).collect();
        let der = STANDARD
            .decode(compact)
            .map_err(|_| "invalid registry CA certificate encoding")?;
        if der.is_empty() {
            return Err("empty registry CA certificate".into());
        }
        roots.push(der);
        rest = after.trim();
    }
    if roots.is_empty() || !rest.is_empty() {
        return Err(
            "registry CA bundle exceeds eight certificates or contains unsupported data".into(),
        );
    }
    Ok(Some(roots))
}

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}
struct Fetcher {
    registry: String,
    scope: Option<String>,
    roots: Option<Vec<Vec<u8>>>,
    credentials: Credentials,
    authorization: Option<HeaderValue>,
    #[cfg(test)]
    client: Option<Client>,
}
impl Fetcher {
    fn client(&self, uri: &Uri) -> Result<Client> {
        #[cfg(test)]
        if let Some(client) = &self.client {
            return Ok(client.clone());
        }
        let origin: Uri = format!(
            "https://{}",
            uri.authority().ok_or("OCI endpoint lacks authority")?
        )
        .parse()
        .map_err(|_| "invalid OCI endpoint")?;
        // Construct from nothing: never infer kubeconfig, service-account,
        // helper, cloud, Docker/Helm credential stores, proxy or environment.
        let mut config = Config::new(origin);
        config.default_retry = false;
        config.connect_timeout = Some(IDLE);
        config.read_timeout = Some(IDLE);
        config.write_timeout = Some(IDLE);
        config.disable_compression = true;
        if uri.authority().is_some_and(|a| a.as_str() == self.registry) {
            config.root_cert = self.roots.clone();
        }
        Client::try_from(config)
            .map_err(|_| "cannot construct verified registry TLS transport".into())
    }
    async fn send(
        &self,
        uri: &Uri,
        authorization: Option<&HeaderValue>,
        limit: usize,
        accept: &str,
    ) -> Result<Reply> {
        validate_uri(uri)?;
        let mut request = http::Request::get(uri.clone())
            .header(http::header::ACCEPT, accept)
            .header(http::header::ACCEPT_ENCODING, "identity")
            .header(http::header::USER_AGENT, "kborg-oci/0.3")
            .body(Body::empty())
            .map_err(|_| "cannot build registry request")?;
        if let Some(authorization) = authorization {
            request
                .headers_mut()
                .insert(http::header::AUTHORIZATION, authorization.clone());
        }
        let response = tokio::time::timeout(IDLE, self.client(uri)?.send(request))
            .await
            .map_err(|_| "registry response headers timed out")?
            .map_err(|_| "registry transport/TLS failed; endpoint and credentials suppressed")?;
        let (parts, mut body) = response.into_parts();
        // Do not consume error/redirect bodies: neither required nor trusted.
        if !parts.status.is_success() {
            return Ok(Reply {
                status: parts.status,
                headers: parts.headers,
                body: Vec::new(),
            });
        }
        if parts.status != StatusCode::OK {
            return Err("registry returned an unsupported partial/success response".into());
        }
        if parts
            .headers
            .get(http::header::CONTENT_ENCODING)
            .is_some_and(|v| v != "identity")
        {
            return Err(
                "registry content encoding is unsupported; expected exact OCI bytes".into(),
            );
        }
        if let Some(length) = parts.headers.get(http::header::CONTENT_LENGTH) {
            let length: usize = length
                .to_str()
                .ok()
                .and_then(|s| s.parse().ok())
                .ok_or("invalid registry content length")?;
            if length > limit {
                return Err("registry response exceeds its bounded download limit".into());
            }
        }
        let mut bytes = Vec::new();
        while let Some(frame) = tokio::time::timeout(IDLE, body.frame())
            .await
            .map_err(|_| "registry response body stalled")?
        {
            let frame = frame.map_err(|_| "registry response body failed; details suppressed")?;
            if let Ok(data) = frame.into_data() {
                if data.len() > limit.saturating_sub(bytes.len()) {
                    return Err("registry response exceeds its bounded download limit".into());
                }
                bytes.extend_from_slice(&data);
            }
        }
        Ok(Reply {
            status: parts.status,
            headers: parts.headers,
            body: bytes,
        })
    }
    async fn fetch(&mut self, initial: Uri, limit: usize, blob: bool) -> Result<Vec<u8>> {
        let mut uri = initial;
        let mut authenticated = false;
        let mut redirects = 0;
        let mut may_authorize = true;
        loop {
            let authorized =
                may_authorize && uri.authority().is_some_and(|a| a.as_str() == self.registry);
            let reply = self
                .send(
                    &uri,
                    authorized.then_some(self.authorization.as_ref()).flatten(),
                    limit,
                    if blob {
                        "application/octet-stream"
                    } else {
                        MANIFEST_TYPE
                    },
                )
                .await?;
            if reply.status == StatusCode::UNAUTHORIZED && authorized && !authenticated {
                let challenge = reply
                    .headers
                    .get(http::header::WWW_AUTHENTICATE)
                    .and_then(|v| v.to_str().ok())
                    .ok_or("registry denied access without a supported authentication challenge")?;
                self.authorization = Some(self.exchange(challenge).await?);
                authenticated = true;
                continue;
            }
            if reply.status.is_redirection() {
                if !matches!(reply.status.as_u16(), 301 | 302 | 303 | 307 | 308) || redirects >= 3 {
                    return Err(
                        "registry redirect limit exceeded or unsupported redirect status".into(),
                    );
                }
                let location = reply
                    .headers
                    .get(http::header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .ok_or("registry redirect lacks a valid location")?;
                let target = redirect(&uri, location)?;
                let cross = target.authority() != uri.authority();
                if cross && !blob {
                    return Err("registry manifest redirects may not change origin".into());
                }
                if cross {
                    may_authorize = false;
                }
                uri = target;
                redirects += 1;
                continue;
            }
            if !reply.status.is_success() {
                return Err(match reply.status.as_u16() {
                    401 | 403 => {
                        "registry denied access; check explicit registry credentials and pull scope"
                    }
                    404 => "digest-pinned OCI chart or blob not found",
                    429 => "registry rate limit reached; no automatic retry",
                    _ => "registry returned an unsuccessful response; remote body suppressed",
                }
                .into());
            }
            return Ok(reply.body);
        }
    }
    async fn exchange(&self, challenge: &str) -> Result<HeaderValue> {
        let fields = bearer_challenge(challenge)?;
        let realm = fields
            .get("realm")
            .ok_or("registry bearer challenge lacks realm")?;
        if realm.contains('#') {
            return Err("registry bearer realm must not contain a fragment".into());
        }
        let realm: Uri = realm
            .parse()
            .map_err(|_| "registry bearer realm is invalid")?;
        validate_uri(&realm)?;
        if realm.query().is_some() {
            return Err("registry bearer realm must not contain a query".into());
        }
        let origin: Uri = format!("https://{}", self.registry)
            .parse()
            .map_err(|_| "invalid registry origin")?;
        safe_destination(&origin, &realm)?;
        let same = realm
            .authority()
            .is_some_and(|a| a.as_str() == self.registry);
        if self.credentials.basic.is_some() && !same {
            return Err("private registry credentials may not be sent to a different token-service origin; supply a registry-scoped token instead".into());
        }
        // Always request exactly this repository's pull scope. The challenge's
        // push/catalog/arbitrary scope is never authoritative.
        let service = fields
            .get("service")
            .map(String::as_str)
            .unwrap_or(&self.registry);
        if service.len() > 253
            || !service
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-:_".contains(&b))
        {
            return Err("invalid registry token service".into());
        }
        let scope = fields
            .get("scope")
            .ok_or("registry bearer challenge lacks repository pull scope")?;
        let repository = scope
            .strip_prefix("repository:")
            .and_then(|s| s.rsplit_once(':'))
            .map(|(repo, _)| repo)
            .ok_or("unsupported registry bearer scope")?;
        // Reference repository is independently carried by the scoped request.
        // The caller fills this invariant in with the exact expected scope.
        if !valid_repository(repository) {
            return Err("invalid registry bearer repository scope".into());
        }
        let expected = self.expected_scope()?;
        if repository != expected {
            return Err("registry requested a token for a different repository".into());
        }
        let token_uri: Uri = format!(
            "{}?service={}&scope={}",
            realm,
            encode(service),
            encode(&format!("repository:{expected}:pull"))
        )
        .parse()
        .map_err(|_| "cannot build registry token request")?;
        let reply = self
            .send(
                &token_uri,
                same.then_some(self.credentials.basic.as_ref()).flatten(),
                MAX_TOKEN,
                "application/json",
            )
            .await?;
        if reply.status != StatusCode::OK {
            return Err("registry token service denied or redirected the request; credentials and body suppressed".into());
        }
        let value: Value = serde_json::from_slice(&reply.body)
            .map_err(|_| "invalid registry token response; body suppressed")?;
        let token = value.get("token").and_then(Value::as_str);
        let access = value.get("access_token").and_then(Value::as_str);
        if token.zip(access).is_some_and(|(a, b)| a != b) {
            return Err("registry token response contains conflicting tokens".into());
        }
        bearer(
            token
                .or(access)
                .ok_or("registry token response has no bearer token")?,
        )
    }
    fn expected_scope(&self) -> Result<&str> {
        // Set once by acquire, never taken from a challenge or redirect.
        self.scope
            .as_deref()
            .ok_or("registry request scope missing".into())
    }
}

fn encode(value: &str) -> String {
    let mut out = String::new();
    for b in value.bytes() {
        if b.is_ascii_alphanumeric() || b"-._~".contains(&b) {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
fn validate_uri(uri: &Uri) -> Result<()> {
    if uri.to_string().len() > 8192
        || uri.scheme_str() != Some("https")
        || uri.authority().is_none_or(|a| !valid_authority(a.as_str()))
        || uri.to_string().contains('#')
    {
        return Err("registry endpoints must use verified HTTPS without userinfo/fragments".into());
    }
    Ok(())
}
fn private_host(uri: &Uri) -> bool {
    let host = uri
        .host()
        .unwrap_or("")
        .trim_start_matches('[')
        .trim_end_matches(']');
    if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || host.ends_with(".home.arpa")
    {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            ip.is_private()
                || ip.is_loopback()
                || ip.is_link_local()
                || ip.is_unspecified()
                || ip.is_multicast()
                || ip.is_broadcast()
        }
        Ok(IpAddr::V6(ip)) => {
            ip.is_loopback()
                || ip.is_unspecified()
                || ip.is_unique_local()
                || ip.is_unicast_link_local()
                || ip.is_multicast()
                || ip.to_ipv4_mapped().is_some_and(|v4| {
                    v4.is_private() || v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()
                })
        }
        Err(_) => false,
    }
}
fn safe_destination(source: &Uri, target: &Uri) -> Result<()> {
    if source.authority() != target.authority() && !private_host(source) && private_host(target) {
        return Err(
            "public registry redirect/token realm may not target a private or loopback address"
                .into(),
        );
    }
    Ok(())
}
fn redirect(source: &Uri, location: &str) -> Result<Uri> {
    if location.len() > 8192 || location.contains('#') {
        return Err("invalid registry redirect location".into());
    }
    let target: Uri = if location.starts_with("//") {
        format!("https:{location}")
    } else if location.starts_with('/') {
        format!(
            "https://{}{location}",
            source.authority().ok_or("invalid registry origin")?
        )
    } else {
        location.into()
    }
    .parse()
    .map_err(|_| "invalid registry redirect location")?;
    validate_uri(&target)?;
    safe_destination(source, &target)?;
    Ok(target)
}
fn bearer_challenge(challenge: &str) -> Result<BTreeMap<String, String>> {
    if challenge.len() > 4096 {
        return Err("registry authentication challenge too large".into());
    }
    let (scheme, mut rest) = challenge
        .split_once(' ')
        .ok_or("unsupported registry authentication scheme")?;
    if !scheme.eq_ignore_ascii_case("Bearer") {
        return Err(
            "registry needs explicit basic/bearer credentials or a supported Bearer challenge"
                .into(),
        );
    }
    let mut result = BTreeMap::new();
    while !rest.trim().is_empty() {
        rest = rest.trim_start();
        let (key, after) = rest
            .split_once('=')
            .ok_or("invalid registry bearer challenge")?;
        if !matches!(key, "realm" | "service" | "scope") {
            return Err("unsupported registry bearer challenge parameter".into());
        }
        let quoted = after
            .strip_prefix('"')
            .ok_or("registry bearer parameters must be quoted")?;
        let (value, after) = quoted
            .split_once('"')
            .ok_or("invalid quoted registry bearer challenge")?;
        if value.bytes().any(|b| b < 0x20 || b == b'\\')
            || result.insert(key.into(), value.into()).is_some()
        {
            return Err("invalid or duplicate registry bearer challenge parameter".into());
        }
        rest = after.trim_start();
        if rest.is_empty() {
            break;
        }
        rest = rest
            .strip_prefix(',')
            .ok_or("invalid registry bearer challenge separator")?;
        if rest.trim().is_empty() {
            return Err("invalid trailing registry bearer parameter".into());
        }
    }
    Ok(result)
}

struct Descriptor {
    digest: String,
    size: usize,
}
fn descriptor(value: &Value, media: &str, limit: usize) -> Result<Descriptor> {
    if value["mediaType"] != media || value.get("urls").is_some() || value.get("data").is_some() {
        return Err(
            "unsupported OCI Helm descriptor media type or external/embedded content".into(),
        );
    }
    let digest = value["digest"]
        .as_str()
        .filter(|s| valid_digest(s))
        .ok_or("OCI descriptor requires a sha256 digest")?;
    let size = value["size"]
        .as_u64()
        .and_then(|s| usize::try_from(s).ok())
        .filter(|s| *s > 0 && *s <= limit)
        .ok_or("OCI descriptor exceeds its bounded content limit")?;
    Ok(Descriptor {
        digest: digest.into(),
        size,
    })
}
fn verify(bytes: &[u8], expected: &str, size: Option<usize>) -> Result<()> {
    let actual = sha256(bytes);
    if actual != expected || size.is_some_and(|size| bytes.len() != size) {
        return Err("OCI content digest/size mismatch; no chart retained".into());
    }
    Ok(())
}
fn sha256(bytes: &[u8]) -> String {
    format!(
        "sha256:{}",
        digest(&SHA256, bytes)
            .as_ref()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    )
}

/// Acquire one immutable OCI manifest and its Helm chart. Caller owns the
/// destination directory; cancellation leaves no partial chart behind. No
/// Kubernetes or Helm/Docker account configuration is read.
pub async fn acquire(
    reference: &Reference,
    options: &Options,
    destination: &Path,
) -> Result<Acquired> {
    let credentials = credentials(options.auth_file.as_deref(), reference)?;
    let authorization = credentials.direct.clone();
    let mut fetcher = Fetcher {
        registry: reference.registry.clone(),
        roots: ca_bundle(options.ca_file.as_deref())?,
        credentials,
        authorization,
        scope: Some(reference.repository.clone()),
        #[cfg(test)]
        client: None,
    };
    tokio::time::timeout(
        ACQUISITION,
        acquire_inner(reference, destination, &mut fetcher),
    )
    .await
    .map_err(|_| "OCI chart acquisition exceeded 60 seconds; no automatic retry".to_owned())?
}
async fn acquire_inner(
    reference: &Reference,
    destination: &Path,
    fetcher: &mut Fetcher,
) -> Result<Acquired> {
    let bytes = fetcher
        .fetch(
            reference.uri("manifests", reference.selector())?,
            MAX_MANIFEST,
            false,
        )
        .await?;
    let manifest_digest = sha256(&bytes);
    if let Some(pinned) = reference.digest() {
        verify(&bytes, pinned, None)?;
    }
    let manifest: Value = serde_json::from_slice(&bytes)
        .map_err(|_| "invalid OCI manifest JSON; remote content suppressed")?;
    if manifest["schemaVersion"] != 2
        || manifest
            .get("mediaType")
            .is_some_and(|v| v != MANIFEST_TYPE)
        || manifest.get("manifests").is_some()
    {
        return Err("OCI reference must select a single Helm image manifest, not an index".into());
    }
    let config = descriptor(&manifest["config"], CONFIG_TYPE, MAX_CONFIG)?;
    let layers = manifest["layers"]
        .as_array()
        .filter(|layers| !layers.is_empty() && layers.len() <= 2)
        .ok_or("OCI Helm manifest needs exactly one chart and optional provenance layer")?;
    let mut chart = None;
    let mut provenance = None;
    for layer in layers {
        match layer["mediaType"].as_str() {
            Some(CHART_TYPE) if chart.is_none() => {
                chart = Some(descriptor(layer, CHART_TYPE, MAX_CHART)?)
            }
            Some(PROVENANCE_TYPE) if provenance.is_none() => {
                provenance = Some(descriptor(layer, PROVENANCE_TYPE, MAX_PROVENANCE)?)
            }
            _ => {
                return Err("OCI manifest has duplicate or unsupported Helm content layers".into());
            }
        }
    }
    let chart = chart.ok_or("OCI manifest lacks a Helm chart layer")?;
    let config_bytes = fetcher
        .fetch(reference.uri("blobs", &config.digest)?, config.size, true)
        .await?;
    verify(&config_bytes, &config.digest, Some(config.size))?;
    let config_metadata: Value =
        serde_json::from_slice(&config_bytes).map_err(|_| "invalid OCI Helm config JSON")?;
    if !config_metadata.is_object() {
        return Err("OCI Helm config must be an object".into());
    }
    let chart_filename = archive_filename(&config_metadata)?;
    let chart_bytes = fetcher
        .fetch(reference.uri("blobs", &chart.digest)?, chart.size, true)
        .await?;
    verify(&chart_bytes, &chart.digest, Some(chart.size))?;
    if !chart_bytes.starts_with(&[0x1f, 0x8b]) {
        return Err("OCI Helm chart is not a gzip packaged chart".into());
    }
    let mut provenance_bytes = None;
    if let Some(provenance) = &provenance {
        let bytes = fetcher
            .fetch(
                reference.uri("blobs", &provenance.digest)?,
                provenance.size,
                true,
            )
            .await?;
        verify(&bytes, &provenance.digest, Some(provenance.size))?;
        provenance_bytes = Some(bytes);
    }
    // Final synchronous create/sync/permission sealing has no await point.
    // Only remove a partial file after our create_new actually succeeded.
    let provenance_path = if let Some(bytes) = provenance_bytes {
        let mut adjacent = destination.as_os_str().to_os_string();
        adjacent.push(".prov");
        let adjacent = PathBuf::from(adjacent);
        write_immutable(&adjacent, &bytes)?;
        Some(adjacent)
    } else {
        None
    };
    if let Err(error) = write_immutable(destination, &chart_bytes) {
        if let Some(path) = &provenance_path {
            let _ = std::fs::remove_file(path);
        }
        return Err(error);
    }
    Ok(Acquired {
        reference: reference.source.clone(),
        resolved_reference: format!(
            "oci://{}/{}@{manifest_digest}",
            reference.registry, reference.repository
        ),
        manifest_digest,
        chart_digest: chart.digest,
        config_digest: config.digest,
        chart_bytes: chart_bytes.len() as u64,
        chart_filename,
        provenance_path,
        provenance_digest: provenance.map(|p| p.digest),
    })
}
pub(crate) fn valid_archive_filename(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 240
        && name.ends_with(".tgz")
        && name
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphanumeric)
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-+".contains(&b))
}
fn archive_filename(metadata: &Value) -> Result<String> {
    let name = metadata["name"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .ok_or("OCI Helm config lacks a bounded chart name")?;
    let version = metadata["version"]
        .as_str()
        .filter(|s| !s.is_empty() && s.len() <= 128)
        .ok_or("OCI Helm config lacks a bounded chart version")?;
    let filename = format!("{name}-{version}.tgz");
    if !valid_archive_filename(&filename) {
        return Err(
            "OCI chart name/version cannot form a safe signature verification filename".into(),
        );
    }
    Ok(filename)
}
fn write_immutable(destination: &Path, bytes: &[u8]) -> Result<()> {
    safety::checked_path(destination)?;
    let parent = destination
        .parent()
        .ok_or("OCI chart destination needs a private directory")?;
    safety::ensure_private_dir(parent)?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o400);
    }
    let mut file = options
        .open(destination)
        .map_err(|_| "cannot create OCI chart destination; refusing overwrite")?;
    let write = (|| {
        use std::io::Write;
        file.write_all(bytes)
            .map_err(|_| "cannot write downloaded OCI chart")?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o400))
                .map_err(|_| "cannot seal downloaded OCI chart")?;
        }
        file.sync_all()
            .map_err(|_| "cannot sync downloaded OCI chart")?;
        Ok(())
    })();
    if let Err(error) = write {
        let _ = std::fs::remove_file(destination);
        return Err(error);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use http_body_util::Full;
    use serde_json::json;
    use std::sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    };
    type Response = http::Response<Full<Bytes>>;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Result<Self> {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let root = std::env::temp_dir()
                .canonicalize()
                .map_err(|_| "temporary root")?
                .join(format!(
                    "kborg-oci-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
            safety::ensure_private_dir(&root)?;
            Ok(Self(root))
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn sha(bytes: &[u8]) -> String {
        format!(
            "sha256:{}",
            digest(&SHA256, bytes)
                .as_ref()
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>()
        )
    }
    fn response(code: u16, bytes: impl Into<Vec<u8>>) -> Response {
        http::Response::builder()
            .status(code)
            .body(Full::new(Bytes::from(bytes.into())))
            .unwrap_or_else(|e| panic!("{e}"))
    }
    fn client(handler: impl Fn(http::Request<Body>) -> Response + Send + Sync + 'static) -> Client {
        let handler = Arc::new(handler);
        Client::new(
            tower::service_fn(move |request| {
                let response = handler(request);
                async move { Ok::<_, std::convert::Infallible>(response) }
            }),
            "unused",
        )
    }
    fn fetcher(client: Client) -> Fetcher {
        Fetcher {
            registry: "registry.test".into(),
            scope: Some("x/chart".into()),
            roots: None,
            credentials: Credentials {
                direct: None,
                basic: None,
            },
            authorization: None,
            client: Some(client),
        }
    }
    fn fixture(provenance: bool) -> Result<(Reference, BTreeMap<String, Vec<u8>>)> {
        let config = br#"{"name":"chart","version":"1.0.0"}"#.to_vec();
        let chart = b"\x1f\x8bFIXTURE_PACKAGED_CHART".to_vec();
        let prov = b"SIGNATURE_NOT_AUTHENTICATED".to_vec();
        let mut layers =
            vec![json!({"mediaType":CHART_TYPE,"digest":sha(&chart),"size":chart.len()})];
        if provenance {
            layers.push(json!({"mediaType":PROVENANCE_TYPE,"digest":sha(&prov),"size":prov.len()}));
        }
        let manifest = serde_json::to_vec(&json!({"schemaVersion":2,"mediaType":MANIFEST_TYPE,
            "config":{"mediaType":CONFIG_TYPE,"digest":sha(&config),"size":config.len()},"layers":layers})).map_err(|_| "manifest")?;
        let reference =
            Reference::parse(&format!("oci://registry.test/x/chart@{}", sha(&manifest)))?;
        let mut bodies = BTreeMap::from([
            (
                reference
                    .uri("manifests", reference.selector())?
                    .path()
                    .into(),
                manifest,
            ),
            (reference.uri("blobs", &sha(&config))?.path().into(), config),
            (reference.uri("blobs", &sha(&chart))?.path().into(), chart),
        ]);
        if provenance {
            bodies.insert(reference.uri("blobs", &sha(&prov))?.path().into(), prov);
        }
        Ok((reference, bodies))
    }
    fn runtime() -> Result<tokio::runtime::Runtime> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| "runtime".into())
    }
    #[test]
    fn reference_has_one_canonical_scope_and_rejects_ambiguous_inputs() -> Result<()> {
        let digest = "a".repeat(64);
        for host in ["ghcr.io", "127.0.0.1:5443", "localhost:5443", "[::1]:5443"] {
            let source = format!("oci://{host}/team/chart@sha256:{digest}");
            let reference = Reference::parse(&source)?;
            assert_eq!(reference.registry(), host);
            assert_eq!(reference.repository(), "team/chart");
            assert_eq!(reference.as_str(), source);
        }
        for value in [
            "https://ghcr.io/a/chart",
            "oci://ghcr.io/a/chart:latest",
            "oci://user:PASS@ghcr.io/a/chart",
            "oci://GHCR.io/a/chart",
            "oci://ghcr.io/a/../chart",
            "oci://ghcr.io/a/%63hart",
            "oci://ghcr.io/a/chart?token=SECRET",
            "oci://ghcr.io//chart",
            "oci://ghcr.io:0/a/chart",
            "oci://ghcr.io:65536/a/chart",
            "oci://127.1/a/chart",
            "oci://2130706433/a/chart",
            "oci://ghcr.io/a/chart:1.0.0",
            "oci://-bad/a/chart",
        ] {
            assert!(
                Reference::parse(&format!("{value}@sha256:{digest}")).is_err(),
                "{value}"
            );
        }
        assert!(
            Reference::parse(&format!("oci://ghcr.io/a/chart@sha256:{}", "A".repeat(64))).is_err()
        );
        Ok(())
    }
    #[test]
    fn explicit_tags_are_bounded_and_unambiguous() -> Result<()> {
        for tag in [
            "latest",
            "stable",
            "1.2.3",
            "1.2.3_build.4",
            "_candidate",
            "UPPER",
        ] {
            let source = format!("oci://registry.test/team/chart:{tag}");
            let reference = Reference::parse(&source)?;
            assert_eq!(reference.tag(), Some(tag));
            assert_eq!(reference.digest(), None);
            assert_eq!(reference.as_str(), source);
        }
        for bad in ["", ".hidden", "-bad", "a+b", "a/b", "a@b", "a?b", "a%3Ab"] {
            assert!(Reference::parse(&format!("oci://registry.test/team/chart:{bad}")).is_err());
        }
        assert!(Reference::parse("oci://registry.test/team/chart").is_err());
        assert!(
            Reference::parse(&format!(
                "oci://registry.test/team/chart:{}",
                "a".repeat(129)
            ))
            .is_err()
        );
        Ok(())
    }
    #[test]
    fn mutable_tag_is_requested_once_and_seals_a_digest_and_named_provenance() -> Result<()> {
        runtime()?.block_on(async {
            let root = Temp::new()?;
            let destination = root.0.join("chart.tgz");
            let (pinned, mut bodies) = fixture(true)?;
            let manifest_path = pinned
                .uri("manifests", pinned.selector())?
                .path()
                .to_owned();
            let manifest = bodies.remove(&manifest_path).ok_or("fixture manifest")?;
            let reference = Reference::parse("oci://registry.test/x/chart:stable")?;
            let tag_path = reference.uri("manifests", "stable")?.path().to_owned();
            let requests = Arc::new(AtomicUsize::new(0));
            let recorded = requests.clone();
            let mut transport = fetcher(client(move |request| {
                if request.uri().path() == tag_path {
                    let first = recorded.fetch_add(1, Ordering::Relaxed) == 0;
                    return response(
                        200,
                        if first {
                            manifest.clone()
                        } else {
                            b"TAG_MOVED_UNREVIEWED".to_vec()
                        },
                    );
                }
                response(
                    200,
                    bodies
                        .get(request.uri().path())
                        .cloned()
                        .unwrap_or_default(),
                )
            }));
            let acquired = acquire_inner(&reference, &destination, &mut transport).await?;
            assert_eq!(requests.load(Ordering::Relaxed), 1);
            assert_eq!(acquired.reference, reference.as_str());
            assert_eq!(acquired.resolved_reference, pinned.as_str());
            assert_eq!(acquired.manifest_digest, pinned.digest().ok_or("digest")?);
            assert_eq!(acquired.chart_filename, "chart-1.0.0.tgz");
            let provenance = acquired.provenance_path.ok_or("retained signature")?;
            assert_eq!(
                std::fs::read(&provenance).map_err(|_| "provenance")?,
                b"SIGNATURE_NOT_AUTHENTICATED"
            );
            verify(
                &safety::read_small(&provenance, MAX_PROVENANCE as u64)?,
                acquired.provenance_digest.as_deref().ok_or("digest")?,
                None,
            )?;
            Ok(())
        })
    }
    #[test]
    fn immutable_acquisition_verifies_all_descriptors_and_never_overwrites() -> Result<()> {
        runtime()?.block_on(async {
            let root = Temp::new()?;
            let destination = root.0.join("chart.tgz");
            let (reference, bodies) = fixture(true)?;
            let count = Arc::new(AtomicUsize::new(0));
            let recorded = count.clone();
            let mut fetcher = fetcher(client(move |request| {
                assert!(request.headers().get(http::header::AUTHORIZATION).is_none());
                assert_eq!(request.headers()[http::header::ACCEPT_ENCODING], "identity");
                assert_eq!(
                    request.uri().authority().map(|a| a.as_str()),
                    Some("registry.test")
                );
                recorded.fetch_add(1, Ordering::Relaxed);
                response(
                    200,
                    bodies
                        .get(request.uri().path())
                        .cloned()
                        .unwrap_or_default(),
                )
            }));
            let acquired = acquire_inner(&reference, &destination, &mut fetcher).await?;
            assert_eq!(count.load(Ordering::Relaxed), 4);
            assert_eq!(acquired.reference, reference.as_str());
            assert!(acquired.provenance_digest.is_some());
            verify(
                &safety::read_small(&destination, MAX_CHART as u64)?,
                &acquired.chart_digest,
                Some(acquired.chart_bytes as usize),
            )?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(&destination)
                        .map_err(|_| "stat")?
                        .permissions()
                        .mode()
                        & 0o777,
                    0o400
                );
            }
            let before = std::fs::read(&destination).map_err(|_| "read")?;
            assert!(
                acquire_inner(&reference, &destination, &mut fetcher)
                    .await
                    .is_err()
            );
            assert_eq!(std::fs::read(destination).map_err(|_| "read")?, before);
            Ok(())
        })
    }
    #[test]
    fn corrupt_manifest_chart_and_provenance_leave_no_chart_or_remote_diagnostics() -> Result<()> {
        runtime()?.block_on(async {
            for target in ["manifest", "chart", "provenance"] {
                let root = Temp::new()?;
                let destination = root.0.join("chart.tgz");
                let (reference, bodies) = fixture(true)?;
                let mut fetcher = fetcher(client(move |request| {
                    let mut bytes = bodies
                        .get(request.uri().path())
                        .cloned()
                        .unwrap_or_default();
                    if ((target == "manifest" && request.uri().path().contains("manifests"))
                        || (target == "chart" && bytes.starts_with(&[0x1f, 0x8b]))
                        || (target == "provenance" && bytes.starts_with(b"SIGNATURE")))
                        && let Some(last) = bytes.last_mut()
                    {
                        *last ^= 1;
                    }
                    response(200, bytes)
                }));
                let error = acquire_inner(&reference, &destination, &mut fetcher)
                    .await
                    .err()
                    .ok_or("tampered content accepted")?;
                assert!(error.contains("digest/size"));
                assert!(!destination.exists());
            }
            let mut denied = fetcher(client(|_| response(500, b"PRIVATE_REMOTE_CANARY")));
            let error = denied
                .fetch(
                    "https://registry.test/x".parse().map_err(|_| "uri")?,
                    10,
                    false,
                )
                .await
                .err()
                .ok_or("error accepted")?;
            assert!(!error.contains("PRIVATE_REMOTE_CANARY"));
            Ok(())
        })
    }
    #[test]
    fn redirect_policy_strips_cross_origin_credentials_and_rejects_unsafe_routes() -> Result<()> {
        runtime()?.block_on(async {
            let seen = Arc::new(AtomicUsize::new(0));
            let observed = seen.clone();
            let mut transport = fetcher(client(move |request| {
                if request.uri().host() == Some("registry.test") {
                    assert_eq!(
                        request.headers()[http::header::AUTHORIZATION],
                        "Bearer PRIVATE_TOKEN"
                    );
                    let mut reply = response(307, Vec::new());
                    reply.headers_mut().insert(
                        http::header::LOCATION,
                        HeaderValue::from_static("https://cdn.test/blob?signature=OPAQUE"),
                    );
                    reply
                } else {
                    assert!(request.headers().get(http::header::AUTHORIZATION).is_none());
                    observed.fetch_add(1, Ordering::Relaxed);
                    response(200, b"chart")
                }
            }));
            transport.authorization = Some(bearer("PRIVATE_TOKEN")?);
            assert_eq!(
                transport
                    .fetch(
                        "https://registry.test/blob".parse().map_err(|_| "uri")?,
                        5,
                        true
                    )
                    .await?,
                b"chart"
            );
            assert_eq!(seen.load(Ordering::Relaxed), 1);
            assert!(
                transport
                    .fetch(
                        "https://registry.test/manifest"
                            .parse()
                            .map_err(|_| "uri")?,
                        5,
                        false
                    )
                    .await
                    .is_err()
            );
            let public: Uri = "https://registry.test/blob".parse().map_err(|_| "uri")?;
            for target in [
                "http://cdn.test/blob",
                "https://user:SECRET@cdn.test/blob",
                "https://127.0.0.1:5000/blob",
                "https://[::1]/blob",
                "https://169.254.169.254/token",
                "https://localhost/token",
                "/blob#fragment",
            ] {
                assert!(redirect(&public, target).is_err(), "{target}");
            }
            assert_eq!(
                redirect(&public, "/second")?.to_string(),
                "https://registry.test/second"
            );
            let mut looping = fetcher(client(|_| {
                let mut reply = response(307, Vec::new());
                reply
                    .headers_mut()
                    .insert(http::header::LOCATION, HeaderValue::from_static("/again"));
                reply
            }));
            assert!(
                looping
                    .fetch(public, 5, true)
                    .await
                    .is_err_and(|e| e.contains("redirect limit"))
            );
            Ok(())
        })
    }
    #[test]
    fn anonymous_and_private_token_exchange_are_scope_bound_and_never_redirect_credentials()
    -> Result<()> {
        runtime()?.block_on(async {
            let token_calls = Arc::new(AtomicUsize::new(0)); let observed = token_calls.clone();
            let mut transport = fetcher(client(move |request| {
                if request.uri().path() == "/token" {
                    assert_eq!(request.uri().query(), Some("service=registry.test&scope=repository%3Ax%2Fchart%3Apull"));
                    assert!(request.headers().get(http::header::AUTHORIZATION).is_none());
                    observed.fetch_add(1, Ordering::Relaxed); response(200, br#"{"token":"EXCHANGED_TOKEN"}"#)
                } else if request.headers().get(http::header::AUTHORIZATION).is_none() {
                    let mut reply = response(401, Vec::new()); reply.headers_mut().insert(http::header::WWW_AUTHENTICATE,
                        HeaderValue::from_static("Bearer realm=\"https://registry.test/token\",service=\"registry.test\",scope=\"repository:x/chart:pull,push\"")); reply
                } else { assert_eq!(request.headers()[http::header::AUTHORIZATION], "Bearer EXCHANGED_TOKEN"); response(200, b"ok") }
            }));
            assert_eq!(transport.fetch("https://registry.test/manifest".parse().map_err(|_| "uri")?, 2, false).await?, b"ok");
            assert_eq!(token_calls.load(Ordering::Relaxed), 1);
            let mut private = fetcher(client(|request| { assert_eq!(request.headers()[http::header::AUTHORIZATION], "Basic PRIVATE"); let mut reply = response(307, b"SECRET_BODY"); reply.headers_mut().insert(http::header::LOCATION, HeaderValue::from_static("https://evil.test/token")); reply }));
            private.credentials.basic = Some(HeaderValue::from_static("Basic PRIVATE"));
            let error = private.exchange("Bearer realm=\"https://registry.test/token\",scope=\"repository:x/chart:pull\"").await.err().ok_or("redirect accepted")?;
            assert!(!error.contains("SECRET_BODY") && error.contains("redirected"));
            assert!(private.exchange("Bearer realm=\"https://other.test/token\",scope=\"repository:x/chart:pull\"").await.is_err());
            assert!(private.exchange("Bearer realm=\"https://registry.test/token\",scope=\"repository:other/chart:pull\"").await.is_err());
            Ok(())
        })
    }
    #[test]
    fn bounded_bodies_descriptor_urls_and_cancellation_cannot_leave_partial_files() -> Result<()> {
        runtime()?.block_on(async {
            let mut advertised = fetcher(client(|_| { let mut reply = response(200, b"small"); reply.headers_mut().insert(http::header::CONTENT_LENGTH, HeaderValue::from_static("999999999")); reply }));
            assert!(advertised.fetch("https://registry.test/x".parse().map_err(|_| "uri")?, 5, true).await.is_err());
            let mut chunked = fetcher(client(|_| response(200, b"sixsix")));
            assert!(chunked.fetch("https://registry.test/x".parse().map_err(|_| "uri")?, 5, true).await.is_err());
            assert!(descriptor(&json!({"mediaType":CHART_TYPE,"digest":sha(b"x"),"size":1,"urls":["https://external.test/file"]}), CHART_TYPE, MAX_CHART).is_err());
            assert!(descriptor(&json!({"mediaType":CHART_TYPE,"digest":sha(b"x"),"size":MAX_CHART+1}), CHART_TYPE, MAX_CHART).is_err());
            let root = Temp::new()?; let destination = root.0.join("chart.tgz"); let (reference, _) = fixture(false)?;
            let quiet = Client::new(tower::service_fn(|_: http::Request<Body>| async { std::future::pending::<std::result::Result<Response, std::convert::Infallible>>().await }), "unused");
            let mut transport = fetcher(quiet);
            assert!(tokio::time::timeout(Duration::from_millis(20), acquire_inner(&reference, &destination, &mut transport)).await.is_err());
            assert!(!destination.exists());
            Ok(())
        })
    }
    #[test]
    fn credential_and_ca_files_are_explicit_private_bounded_and_host_bound() -> Result<()> {
        let root = Temp::new()?;
        let (reference, _) = fixture(false)?;
        let auth = root.0.join("auth.json");
        safety::write_new(
            &auth,
            br#"{"registry":"registry.test","username":"reader","password":"PRIVATE_AUTH_CANARY"}"#,
        )?;
        let loaded = credentials(Some(&auth), &reference)?;
        assert!(
            loaded
                .direct
                .as_ref()
                .is_some_and(HeaderValue::is_sensitive)
        );
        assert!(loaded.basic.is_some());
        let other = Reference::parse(&reference.as_str().replace("registry.test", "other.test"))?;
        assert!(credentials(Some(&auth), &other).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&auth, std::fs::Permissions::from_mode(0o644))
                .map_err(|_| "permissions")?;
            assert!(credentials(Some(&auth), &reference).is_err());
        }
        let ca = root.0.join("ca.pem");
        safety::write_new(
            &ca,
            b"-----BEGIN PRIVATE KEY-----\nCANARY\n-----END PRIVATE KEY-----",
        )?;
        assert!(ca_bundle(Some(&ca)).is_err_and(|e| !e.contains("CANARY")));
        assert!(
            bearer_challenge(
                "Bearer realm=\"https://registry.test/token\",realm=\"https://evil.test/token\""
            )
            .is_err()
        );
        Ok(())
    }
}
