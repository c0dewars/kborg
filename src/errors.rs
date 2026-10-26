//! Actionable failure text. Every Kubernetes and transport failure a user can
//! see is classified here once, then rendered as "WHAT failed: WHY. Next:
//! STEP". Cluster-provided text (admission, quota, validation messages) is
//! sanitized, redacted and bounded; raw transport text is never echoed.
use crate::safety;
use regex::Regex;
use std::sync::LazyLock;

/// Every rendered failure carries this marker before its next step. Tests
/// enforce it for the classifier and for the user-facing modules.
pub const NEXT: &str = "Next:";
const CLUSTER_TEXT: usize = 160;

/// Where the failure happened; used to name the server host and context.
#[derive(Clone, Copy, Default)]
pub struct Scope<'a> {
    pub context: &'a str,
    pub server: &'a str,
    pub namespace: &'a str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rbac {
    pub verb: String,
    pub resource: String,
    pub group: String,
    pub subresource: String,
    /// None means cluster scope.
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cause {
    Forbidden(Option<Rbac>),
    Unauthorized,
    NotFound(Option<String>),
    Conflict,
    AlreadyExists,
    Admission(String),
    PodSecurity(String),
    Quota(String),
    Invalid(String),
    Expired,
    Throttled,
    Unavailable(u16),
    Timeout,
    Refused,
    Dns,
    Tls,
    Credentials,
    Network,
    Upgrade(u16),
    BadResponse,
    Rejected(u16, String),
}

/// Classifies any kube client failure without keeping raw transport text.
pub fn classify(error: &kube::Error) -> Cause {
    match error {
        kube::Error::Api(status) => classify_status(
            status.code,
            &status.reason,
            &status.message,
            status.details.as_ref(),
        ),
        kube::Error::UpgradeConnection(kube::client::UpgradeConnectionError::ProtocolSwitch(
            code,
        )) => match code.as_u16() {
            401 => Cause::Unauthorized,
            403 => Cause::Forbidden(None),
            404 => Cause::NotFound(None),
            code => Cause::Upgrade(code),
        },
        kube::Error::UpgradeConnection(_) => Cause::Upgrade(0),
        kube::Error::SerdeError(_) => Cause::BadResponse,
        kube::Error::Auth(_) => Cause::Credentials,
        kube::Error::RustlsTls(_) => Cause::Tls,
        other => transport(other),
    }
}

/// Classifies a transport error by walking its source chain. Only the
/// resulting class is kept; the chain text can carry endpoints or tokens.
pub fn transport(error: &(dyn std::error::Error + 'static)) -> Cause {
    let mut chain = String::new();
    let mut current = Some(error);
    while let Some(error) = current {
        chain.push_str(&error.to_string().to_ascii_lowercase());
        chain.push(' ');
        current = error.source();
    }
    let has = |needles: &[&str]| needles.iter().any(|n| chain.contains(n));
    if has(&["connection refused"]) {
        Cause::Refused
    } else if has(&[
        "dns",
        "failed to lookup",
        "nodename nor servname",
        "name or service",
    ]) {
        Cause::Dns
    } else if has(&["certificate", "unknownissuer", "tls", "handshake"]) {
        Cause::Tls
    } else if has(&["length limit", "invalid json", "expected value"]) {
        Cause::BadResponse
    } else if has(&["timed out", "deadline", "elapsed", "budget exceeded"]) {
        Cause::Timeout
    } else if has(&["unauthorized", "credential", "exec"]) {
        Cause::Credentials
    } else {
        Cause::Network
    }
}

/// Classifies a Kubernetes Status object (code, reason, message, details).
pub fn classify_status(
    code: u16,
    reason: &str,
    message: &str,
    details: Option<&kube::core::response::StatusDetails>,
) -> Cause {
    let lower = message.to_ascii_lowercase();
    let from = |marker: &str| {
        lower
            .find(marker)
            .and_then(|at| message.get(at..))
            .map(cluster_text)
            .unwrap_or_default()
    };
    if lower.contains("admission webhook") {
        return Cause::Admission(from("admission webhook"));
    }
    if lower.contains("validatingadmissionpolicy") {
        return Cause::Admission(from("validatingadmissionpolicy"));
    }
    if lower.contains("violates podsecurity") {
        return Cause::PodSecurity(from("violates podsecurity"));
    }
    if lower.contains("exceeded quota") {
        return Cause::Quota(from("exceeded quota"));
    }
    if lower.contains("denied the request") || lower.contains("denied request") {
        return Cause::Admission(cluster_text(message));
    }
    match code {
        401 => Cause::Unauthorized,
        403 => Cause::Forbidden(rbac(message)),
        404 => Cause::NotFound(details.and_then(object_name)),
        409 if reason == "AlreadyExists" => Cause::AlreadyExists,
        409 => Cause::Conflict,
        410 => Cause::Expired,
        400 | 422 if reason == "Invalid" || code == 422 => {
            Cause::Invalid(invalid(message, details))
        }
        429 => Cause::Throttled,
        408 | 504 => Cause::Timeout,
        500..=599 => Cause::Unavailable(code),
        _ => Cause::Rejected(code, safety::safe_text(reason, 40)),
    }
}

static RBAC: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r#"cannot ([a-z]+) resource "([a-z0-9.\-]+)(?:/([a-z0-9\-]+))?"(?: in API group "([a-z0-9.\-]*)")?(?: in the namespace "([a-z0-9\-]+)")?"#,
    )
    .ok()
});

/// Parses the standard RBAC denial message, keeping only validated tokens.
pub fn rbac(message: &str) -> Option<Rbac> {
    let found = RBAC.as_ref()?.captures(message)?;
    let text = |i: usize| {
        found
            .get(i)
            .map(|m| m.as_str().to_owned())
            .unwrap_or_default()
    };
    let namespace = found
        .get(5)
        .map(|m| m.as_str().to_owned())
        .filter(|ns| safety::valid_id(ns));
    let rbac = Rbac {
        verb: text(1),
        resource: text(2),
        subresource: text(3),
        group: text(4),
        namespace,
    };
    (rbac.verb.len() <= 32 && rbac.resource.len() <= 253 && rbac.group.len() <= 253).then_some(rbac)
}

fn object_name(details: &kube::core::response::StatusDetails) -> Option<String> {
    let ok = |s: &str| {
        !s.is_empty()
            && s.len() <= 253
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b':'))
    };
    match (ok(&details.kind), ok(&details.name)) {
        (true, true) => Some(format!("{} {}", details.kind, details.name)),
        (true, false) => Some(details.kind.clone()),
        (false, true) => Some(details.name.clone()),
        _ => None,
    }
}

fn invalid(message: &str, details: Option<&kube::core::response::StatusDetails>) -> String {
    let causes: Vec<String> = details
        .map(|d| d.causes.as_slice())
        .unwrap_or_default()
        .iter()
        .take(2)
        .map(|c| {
            if c.field.is_empty() {
                c.message.clone()
            } else {
                format!("{}: {}", c.field, c.message)
            }
        })
        .collect();
    if causes.is_empty() {
        let lower = message.to_ascii_lowercase();
        let tail = lower
            .find("is invalid: ")
            .and_then(|at| message.get(at + 12..))
            .unwrap_or(message);
        cluster_text(tail)
    } else {
        cluster_text(&causes.join("; "))
    }
}

static SECRETISH: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(
        r#"(?i)bearer\s+\S+|(?:token|password|passwd|secret|api[-_]?key|authorization)\s*[=:]\s*\S+|invalid value: "[^"]*""#,
    )
    .ok()
});
static OPAQUE: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"[A-Za-z0-9+/_\-]{24,}={0,2}(?:\.[A-Za-z0-9+/_\-=]+)*").ok());

/// Cluster text shown to users: control and bidi characters removed,
/// credential-like values and long opaque strings withheld, bounded length.
pub fn cluster_text(input: &str) -> String {
    let clean = safety::safe_text(input, 2048);
    let (Some(secretish), Some(opaque)) = (SECRETISH.as_ref(), OPAQUE.as_ref()) else {
        return "[cluster message withheld]".into();
    };
    let keyed = secretish.replace_all(&clean, "[withheld]");
    // Names like "require-run-as-non-root" stay; mixed-case strings with
    // digits (tokens, keys, JWTs) are withheld.
    let redacted = opaque.replace_all(&keyed, |found: &regex::Captures| {
        let text = &found[0];
        let mixed = text.bytes().any(|b| b.is_ascii_uppercase())
            && text.bytes().any(|b| b.is_ascii_lowercase())
            && text.bytes().any(|b| b.is_ascii_digit());
        if mixed {
            "[withheld]".to_owned()
        } else {
            text.to_owned()
        }
    });
    let trimmed = redacted.trim().trim_end_matches('.');
    if trimmed.chars().count() > CLUSTER_TEXT {
        let cut: String = trimmed.chars().take(CLUSTER_TEXT).collect();
        format!("{cut}...")
    } else {
        trimmed.to_owned()
    }
}

/// The API server host from a kubeconfig server URL, without path or query.
pub fn host(server: &str) -> String {
    server
        .parse::<http::Uri>()
        .ok()
        .and_then(|uri| uri.authority().map(|a| a.as_str().to_owned()))
        .filter(|a| !a.contains('@'))
        .map(|a| safety::safe_text(&a, 120))
        .unwrap_or_else(|| "the API server".into())
}

/// Renders a classified failure as "WHAT failed: WHY. Next: STEP".
pub fn render(what: &str, cause: &Cause, scope: &Scope) -> String {
    render_code(what, None, cause, scope)
}

/// Like `render`, with the HTTP status code when one is known.
pub fn render_code(what: &str, code: Option<u16>, cause: &Cause, scope: &Scope) -> String {
    let (why, next) = explain_cause(cause, scope);
    match code {
        Some(code) => format!("{what} failed (HTTP {code}): {why}. {NEXT} {next}."),
        None => format!("{what} failed: {why}. {NEXT} {next}."),
    }
}

/// Classify and render in one step; API errors also show their HTTP code.
pub fn explain(what: &str, error: &kube::Error, scope: &Scope) -> String {
    let code = match error {
        kube::Error::Api(status) => Some(status.code),
        _ => None,
    };
    render_code(what, code, &classify(error), scope)
}

/// A raw HTTP status from a hand-built request (no Status body is read).
pub fn http_status(what: &str, code: u16, scope: &Scope) -> String {
    render_code(
        what,
        Some(code),
        &classify_status(code, "", "", None),
        scope,
    )
}

/// Fits an actionable message into one row of `width` characters. The next
/// step is kept whole and the cause is shortened, so a one-line status bar
/// never cuts off what to do.
pub fn fit(text: &str, width: usize) -> String {
    if text.chars().count() <= width {
        return text.into();
    }
    let Some(at) = text.find(NEXT) else {
        return text.into();
    };
    let (head, next) = text.split_at(at);
    let keep = width
        .saturating_sub(next.chars().count() + 4)
        .max(24.min(width / 3));
    let head: String = head.chars().take(keep).collect();
    format!("{}... {next}", head.trim_end())
}

/// A local failure that is not a Kubernetes error but still needs a step.
pub fn local(what: &str, why: &str, next: &str) -> String {
    format!("{what}: {why}. {NEXT} {next}.")
}

fn switch_context(scope: &Scope) -> String {
    if scope.context.is_empty() {
        "switch context (:ctx or --context)".into()
    } else {
        format!(
            "switch from context {} (:ctx or --context)",
            safety::safe_text(scope.context, 80)
        )
    }
}

fn can_i(rbac: &Rbac) -> String {
    let resource = if rbac.group.is_empty() {
        rbac.resource.clone()
    } else {
        format!("{}.{}", rbac.resource, rbac.group)
    };
    let mut command = format!("kubectl auth can-i {} {resource}", rbac.verb);
    if !rbac.subresource.is_empty() {
        command.push_str(&format!(" --subresource={}", rbac.subresource));
    }
    if let Some(ns) = &rbac.namespace {
        command.push_str(&format!(" -n {ns}"));
    }
    command
}

fn explain_cause(cause: &Cause, scope: &Scope) -> (String, String) {
    let host = host(scope.server);
    let ns = if safety::valid_id(scope.namespace) {
        format!(" -n {}", scope.namespace)
    } else {
        String::new()
    };
    match cause {
        Cause::Forbidden(Some(rbac)) => {
            let resource = if rbac.subresource.is_empty() {
                rbac.resource.clone()
            } else {
                format!("{}/{}", rbac.resource, rbac.subresource)
            };
            let group = if rbac.group.is_empty() {
                String::new()
            } else {
                format!(" (API group {})", rbac.group)
            };
            let place = match &rbac.namespace {
                Some(ns) => format!("in namespace {ns}"),
                None => "at cluster scope".into(),
            };
            (
                format!(
                    "authorization denied: RBAC does not allow {} on {resource}{group} {place}",
                    rbac.verb
                ),
                format!(
                    "run `{}` to confirm, ask an admin for that permission, or {}",
                    can_i(rbac),
                    switch_context(scope)
                ),
            )
        }
        Cause::Forbidden(None) => (
            "authorization denied by RBAC or a policy".into(),
            format!(
                "run `kubectl auth can-i --list{ns}` to see what this identity may do, or {}",
                switch_context(scope)
            ),
        ),
        Cause::Unauthorized => (
            "authorization denied: the API server rejected the credentials (expired or invalid)"
                .into(),
            "rerun your existing cluster login CLI (for example orbit-use) in this terminal, then reconnect; KBorg reuses that kubeconfig without copied tokens".into(),
        ),
        Cause::NotFound(name) => (
            match name {
                Some(name) => format!("{} was not found (deleted, renamed or in another namespace)", safety::safe_text(name, 160)),
                None => "the object or API was not found (deleted, renamed or not served)".into(),
            },
            "refresh the view and check the namespace (:ns) and context".into(),
        ),
        Cause::Conflict => (
            "conflict: the object changed since it was reviewed; nothing was overwritten".into(),
            "reopen the object and review again".into(),
        ),
        Cause::AlreadyExists => (
            "an object with that name already exists".into(),
            "choose another name or open the existing object".into(),
        ),
        Cause::Admission(text) => (
            format!("admission control denied the request: {text}"),
            "change the object to satisfy that policy, or ask the policy owner for an exception".into(),
        ),
        Cause::PodSecurity(text) => (
            format!("Pod Security admission denied the request: {text}"),
            "adjust the securityContext to the namespace's pod-security level, or use a namespace that permits it".into(),
        ),
        Cause::Quota(text) => (
            format!("a ResourceQuota is exhausted: {text}"),
            format!(
                "free capacity or lower the requests, or ask an admin to raise the quota (`kubectl describe quota{ns}`)"
            ),
        ),
        Cause::Invalid(text) => (
            format!("the API server rejected the object as invalid: {text}"),
            "fix the named fields and try again".into(),
        ),
        Cause::Expired => (
            "the list or watch position expired (HTTP 410)".into(),
            "refresh; KBorg relists automatically".into(),
        ),
        Cause::Throttled => (
            "the API server is rate limiting requests (HTTP 429)".into(),
            "wait a few seconds and retry, or refresh less often".into(),
        ),
        Cause::Unavailable(code) => (
            format!("the API server or an aggregated API at {host} is unavailable (HTTP {code})"),
            "retry shortly; if it persists, check control-plane and APIService health".into(),
        ),
        Cause::Timeout => (
            format!("{host} did not answer in time"),
            "check VPN/proxy and network access to that host, then retry; refresh to confirm whether any write was applied".into(),
        ),
        Cause::Refused => (
            format!("{host} refused the connection (connection refused)"),
            format!(
                "check that the cluster is running and VPN/proxy is up, or {}",
                switch_context(scope)
            ),
        ),
        Cause::Dns => (
            format!("cannot resolve the API server host {host}"),
            "check VPN/proxy and DNS, and the cluster server URL in kubeconfig".into(),
        ),
        Cause::Tls => (
            format!(
                "TLS verification of {host} failed: the certificate-authority in kubeconfig does not match the server certificate"
            ),
            "refresh this cluster with your existing cluster CLI or ask your administrator for its trusted CA; do not disable TLS verification as a workaround".into(),
        ),
        Cause::Credentials => (
            "authentication with the API server failed: the credential helper or client certificate did not produce valid credentials".into(),
            "rerun your existing cluster login CLI (for example orbit-use) in this terminal, ensure its credential helper is on PATH, then reconnect; no token copying is needed".into(),
        ),
        Cause::Network => (
            format!("the connection to {host} failed or was interrupted"),
            "check VPN/proxy and network, then retry; refresh to confirm whether any write was applied".into(),
        ),
        Cause::Upgrade(0) => (
            "streaming protocol negotiation with the API server failed".into(),
            "check that proxies between you and the cluster allow WebSockets, then retry".into(),
        ),
        Cause::Upgrade(code) => (
            format!("the API server refused the streaming connection (HTTP {code})"),
            format!(
                "check RBAC for the subresource (`kubectl auth can-i create pods/exec{ns}`; also pods/attach, pods/portforward) and that proxies allow WebSockets"
            ),
        ),
        Cause::BadResponse => (
            "the response could not be read safely (malformed or over the size limit)".into(),
            "retry; narrow the namespace or selector if the list is large".into(),
        ),
        Cause::Rejected(code, reason) => (
            format!(
                "the API server rejected the request (HTTP {code}{}{}); server message withheld",
                if reason.is_empty() { "" } else { " " },
                reason
            ),
            "check the object, namespace and permissions, then retry".into(),
        ),
    }
}

#[cfg(test)]
#[path = "errors_tests.rs"]
mod tests;
