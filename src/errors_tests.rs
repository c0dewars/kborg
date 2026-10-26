use super::*;
use serde_json::json;

const SCOPE: Scope = Scope {
    context: "prod-east",
    server: "https://10.0.0.1:6443/base?token=QUERY_CANARY",
    namespace: "default",
};

fn api(code: u16, reason: &str, message: &str, details: serde_json::Value) -> kube::Error {
    let status: kube::core::Status = serde_json::from_value(json!({
        "apiVersion": "v1", "kind": "Status", "status": "Failure",
        "code": code, "reason": reason, "message": message, "details": details,
    }))
    .unwrap_or_else(|e| panic!("{e}"));
    kube::Error::Api(Box::new(status))
}
fn net(kind: std::io::ErrorKind, text: &str) -> kube::Error {
    kube::Error::Service(Box::new(std::io::Error::new(kind, text.to_owned())))
}
fn null() -> serde_json::Value {
    serde_json::Value::Null
}

/// The classification table: each Kubernetes or transport failure maps to
/// one cause, names why it failed, and carries a concrete next step.
#[test]
fn classification_table_names_cause_and_next_step() {
    use std::io::ErrorKind::*;
    let rows: Vec<(kube::Error, &[&str], &[&str])> = vec![
        (
            api(
                403,
                "Forbidden",
                r#"pods is forbidden: User "alice@example.com" cannot list resource "pods" in API group "" in the namespace "team-a""#,
                null(),
            ),
            &[
                "authorization denied",
                "list on pods in namespace team-a",
                "`kubectl auth can-i list pods -n team-a`",
                "switch from context prod-east (:ctx or --context)",
            ],
            &["alice"],
        ),
        (
            api(
                403,
                "Forbidden",
                r#"deployments.apps "web" is forbidden: User "bob" cannot update resource "deployments/scale" in API group "apps" in the namespace "prod""#,
                null(),
            ),
            &[
                "update on deployments/scale (API group apps) in namespace prod",
                "`kubectl auth can-i update deployments.apps --subresource=scale -n prod`",
            ],
            &["bob"],
        ),
        (
            api(
                403,
                "Forbidden",
                r#"nodes is forbidden: User "x" cannot list resource "nodes" in API group "" at the cluster scope"#,
                null(),
            ),
            &[
                "list on nodes at cluster scope",
                "`kubectl auth can-i list nodes`",
            ],
            &[" -n "],
        ),
        (
            api(403, "Forbidden", "PRIVATE_CANARY", null()),
            &[
                "authorization denied by RBAC or a policy",
                "`kubectl auth can-i --list -n default`",
            ],
            &["PRIVATE_CANARY"],
        ),
        (
            api(401, "Unauthorized", "Unauthorized", null()),
            &[
                "authorization denied",
                "credentials",
                "rerun your existing cluster login CLI",
                "orbit-use",
                "without copied tokens",
            ],
            &[],
        ),
        (
            api(
                404,
                "NotFound",
                "pods \"web\" not found",
                json!({"kind":"pods","name":"web"}),
            ),
            &["pods web was not found", "refresh the view"],
            &[],
        ),
        (
            api(
                409,
                "Conflict",
                "Operation cannot be fulfilled: the object has been modified",
                null(),
            ),
            &[
                "changed since it was reviewed",
                "reopen the object and review again",
            ],
            &[],
        ),
        (
            api(409, "AlreadyExists", "pods \"web\" already exists", null()),
            &["already exists", "choose another name"],
            &[],
        ),
        (
            api(
                400,
                "BadRequest",
                "admission webhook \"validate.kyverno.svc-fail\" denied the request: image tag latest is not allowed; token=abc123SECRET \u{1b}[31mred",
                null(),
            ),
            &[
                "admission control denied the request: admission webhook \"validate.kyverno.svc-fail\" denied the request: image tag latest is not allowed",
                "[withheld]",
                "satisfy that policy",
            ],
            &["abc123SECRET", "\u{1b}"],
        ),
        (
            api(
                422,
                "Invalid",
                "deployments.apps \"x\" is forbidden: ValidatingAdmissionPolicy 'replicas-limit' with binding 'b' denied request: replicas must be at most 5",
                null(),
            ),
            &[
                "admission control denied",
                "replicas-limit",
                "replicas must be at most 5",
            ],
            &[],
        ),
        (
            api(
                403,
                "Forbidden",
                "pods \"x\" is forbidden: violates PodSecurity \"restricted:latest\": privileged (container \"c\" must not set securityContext.privileged=true)",
                null(),
            ),
            &[
                "Pod Security admission denied",
                "restricted:latest",
                "securityContext",
            ],
            &[],
        ),
        (
            api(
                403,
                "Forbidden",
                "pods \"x\" is forbidden: exceeded quota: compute, requested: cpu=2, used: cpu=9, limited: cpu=10",
                null(),
            ),
            &[
                "ResourceQuota is exhausted: exceeded quota: compute",
                "`kubectl describe quota -n default`",
            ],
            &[],
        ),
        (
            api(
                422,
                "Invalid",
                "Deployment.apps \"x\" is invalid",
                json!({"causes":[{"field":"spec.replicas","message":"Invalid value: \"SECRET_VALUE\": must be greater than or equal to 0"}]}),
            ),
            &[
                "rejected the object as invalid: spec.replicas",
                "must be greater than or equal to 0",
                "fix the named fields",
            ],
            &["SECRET_VALUE"],
        ),
        (
            api(410, "Expired", "too old resource version", null()),
            &["expired", "relists"],
            &[],
        ),
        (
            api(429, "TooManyRequests", "slow down", null()),
            &["rate limiting", "wait a few seconds"],
            &[],
        ),
        (
            api(503, "ServiceUnavailable", "x", null()),
            &["10.0.0.1:6443 is unavailable (HTTP 503)", "APIService"],
            &[],
        ),
        (
            api(504, "Timeout", "x", null()),
            &["10.0.0.1:6443 did not answer in time", "VPN/proxy"],
            &[],
        ),
        (
            api(418, "Teapot", "SERVER_TEXT_CANARY", null()),
            &["HTTP 418 Teapot", "server message withheld"],
            &["SERVER_TEXT_CANARY"],
        ),
        (
            net(
                ConnectionRefused,
                "tcp connect error: Connection refused (os error 61) to https://10.0.0.1:6443?token=abc",
            ),
            &["10.0.0.1:6443 refused the connection", "VPN/proxy", ":ctx"],
            &["token=abc"],
        ),
        (
            net(TimedOut, "operation timed out"),
            &["did not answer in time", "check VPN/proxy"],
            &[],
        ),
        (
            net(Other, "invalid peer certificate: UnknownIssuer"),
            &[
                "TLS verification of 10.0.0.1:6443 failed",
                "certificate-authority",
                "kubeconfig",
            ],
            &[],
        ),
        (
            net(Other, "failed to lookup address information"),
            &["cannot resolve", "VPN/proxy and DNS"],
            &[],
        ),
        (
            net(Other, "length limit exceeded"),
            &["could not be read safely", "narrow the namespace"],
            &[],
        ),
        (
            net(Other, "connection reset by peer WEIRD_CANARY"),
            &["connection to 10.0.0.1:6443 failed", "VPN/proxy"],
            &["WEIRD_CANARY"],
        ),
    ];
    for (error, must, never) in rows {
        let text = explain("Kubernetes request", &error, &SCOPE);
        assert!(text.starts_with("Kubernetes request failed"), "{text}");
        assert!(text.contains(NEXT), "missing next step: {text}");
        for needle in must {
            assert!(text.contains(needle), "{needle:?} missing from {text}");
        }
        for needle in never.iter().chain(&["QUERY_CANARY", "/base"]) {
            assert!(!text.contains(needle), "{needle:?} leaked into {text}");
        }
    }
}

/// Exhaustive: adding a Cause variant forces a new arm here, and the test
/// then fails until a sample proves its text has a next step.
fn variant(cause: &Cause) -> usize {
    match cause {
        Cause::Forbidden(Some(_)) => 0,
        Cause::Forbidden(None) => 1,
        Cause::Unauthorized => 2,
        Cause::NotFound(_) => 3,
        Cause::Conflict => 4,
        Cause::AlreadyExists => 5,
        Cause::Admission(_) => 6,
        Cause::PodSecurity(_) => 7,
        Cause::Quota(_) => 8,
        Cause::Invalid(_) => 9,
        Cause::Expired => 10,
        Cause::Throttled => 11,
        Cause::Unavailable(_) => 12,
        Cause::Timeout => 13,
        Cause::Refused => 14,
        Cause::Dns => 15,
        Cause::Tls => 16,
        Cause::Credentials => 17,
        Cause::Network => 18,
        Cause::Upgrade(0) => 19,
        Cause::Upgrade(_) => 20,
        Cause::BadResponse => 21,
        Cause::Rejected(..) => 22,
    }
}
#[test]
fn every_cause_renders_why_and_next_step() {
    let rbac = rbac(r#"cannot get resource "pods" in API group "" in the namespace "a""#);
    assert!(rbac.is_some());
    let samples = vec![
        Cause::Forbidden(rbac),
        Cause::Forbidden(None),
        Cause::Unauthorized,
        Cause::NotFound(None),
        Cause::Conflict,
        Cause::AlreadyExists,
        Cause::Admission("x".into()),
        Cause::PodSecurity("x".into()),
        Cause::Quota("x".into()),
        Cause::Invalid("x".into()),
        Cause::Expired,
        Cause::Throttled,
        Cause::Unavailable(503),
        Cause::Timeout,
        Cause::Refused,
        Cause::Dns,
        Cause::Tls,
        Cause::Credentials,
        Cause::Network,
        Cause::Upgrade(0),
        Cause::Upgrade(403),
        Cause::BadResponse,
        Cause::Rejected(418, String::new()),
    ];
    let mut seen: Vec<usize> = samples.iter().map(variant).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen, (0..=22).collect::<Vec<_>>());
    for cause in &samples {
        for scope in [SCOPE, Scope::default()] {
            let text = render("Step", cause, &scope);
            let (why, next) = text
                .split_once(NEXT)
                .unwrap_or_else(|| panic!("no next step: {text}"));
            assert!(why.len() > "Step failed: ".len() + 10, "{text}");
            assert!(next.trim().len() > 10, "{text}");
        }
    }
}

#[test]
fn cluster_text_is_sanitized_redacted_and_bounded() {
    let jwt = "eyJhbGciOiJSUzI1NiIsImtpZCI6IjEifQ.eyJzdWIiOiJ4In0.c2lnbmF0dXJlMTIz";
    let text = cluster_text(&format!(
        "denied: Bearer abc.def password: hunter2 {jwt} keep-require-run-as-non-root-policy \u{202e}x"
    ));
    assert!(!text.contains("hunter2") && !text.contains("abc.def") && !text.contains("eyJ"));
    assert!(text.contains("keep-require-run-as-non-root-policy"));
    assert!(!text.contains('\u{202e}'));
    let long = cluster_text(&"word ".repeat(200));
    assert!(long.chars().count() <= CLUSTER_TEXT + 3 && long.ends_with("..."));
    assert_eq!(host("https://user:pw@h:1"), "the API server");
    assert_eq!(
        host("https://api.example.com:443/x?y"),
        "api.example.com:443"
    );
    assert_eq!(host(""), "the API server");
}

#[test]
fn fit_keeps_the_next_step_on_one_row() {
    let text = render("Watch of pods", &Cause::Forbidden(None), &SCOPE);
    for width in [60usize, 80, 120] {
        let fitted = fit(&text, width);
        assert!(fitted.starts_with("Watch of pods"), "{fitted}");
        assert!(fitted.contains("Next: run `kubectl auth can-i --list -n default`"));
        if text.chars().count() > width {
            assert!(fitted.contains("... Next:"));
        }
    }
    assert_eq!(fit("short", 10), "short");
    assert_eq!(fit(&"x".repeat(20), 10), "x".repeat(20));
}

#[test]
fn rbac_parser_rejects_tokens_outside_kubernetes_grammar() {
    assert!(rbac(r#"cannot list resource "pods; rm -rf" in API group """#).is_none());
    let parsed = rbac(r#"cannot get resource "secrets" in API group "" in the namespace "Bad_NS""#);
    assert!(parsed.is_some_and(|r| r.namespace.is_none()));
}

fn production(path: &str) -> String {
    let full = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    let text = std::fs::read_to_string(&full).unwrap_or_else(|e| panic!("{path}: {e}"));
    match text.find("mod tests {") {
        Some(at) => text[..at].to_owned(),
        None => text,
    }
}

/// String literals passed to the error constructors this codebase uses.
fn error_literals(source: &str) -> Vec<String> {
    let mut found = Vec::new();
    for prefix in ["Err(", ".ok_or(", ".ok_or_else(||", ".map_err(|_|"] {
        for (at, _) in source.match_indices(prefix) {
            let mut rest = source[at + prefix.len()..].trim_start();
            if let Some(after) = rest.strip_prefix("format!(") {
                rest = after.trim_start();
            }
            let Some(body) = rest.strip_prefix('"') else {
                continue;
            };
            let mut literal = String::new();
            let mut escaped = false;
            for c in body.chars() {
                match (escaped, c) {
                    (false, '\\') => escaped = true,
                    (false, '"') => break,
                    _ => {
                        escaped = false;
                        literal.push(c);
                    }
                }
            }
            found.push(literal);
        }
    }
    found
}

/// Modules whose errors reach users directly (status line, CLI stderr) at
/// connection time. Every literal error there must carry a next step; add a
/// module here once its strings are converted.
const USER_FACING: &[&str] = &[
    "src/errors.rs",
    "src/gateway.rs",
    "src/gateway/errors.rs",
    "src/gateway/kubeconfig.rs",
];

#[test]
fn lint_user_facing_error_literals_have_a_next_step() {
    let mut checked = 0;
    let mut missing = Vec::new();
    for path in USER_FACING {
        for literal in error_literals(&production(path)) {
            checked += 1;
            if !literal.contains(NEXT) {
                missing.push(format!("{path}: {literal}"));
            }
        }
    }
    assert!(checked >= 30, "lint scanned too little ({checked})");
    assert!(
        missing.is_empty(),
        "errors without `{NEXT}`:\n{}",
        missing.join("\n")
    );
}

fn sources(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// Kubernetes failures must go through this module: hand-written texts that
/// print a status code and drop the reason (the old pattern) are rejected.
#[test]
fn lint_no_ad_hoc_kubernetes_error_text() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    sources(&root, &mut files);
    assert!(files.len() > 50);
    let forbidden = [
        "server details suppressed",
        "server message suppressed",
        "HTTP {}",
        "Kubernetes API {}",
        "(Kubernetes API",
    ];
    let mut offenders = Vec::new();
    for file in files {
        let name = file.to_string_lossy().replace('\\', "/");
        if name.ends_with("/errors.rs") && name.contains("/src/errors.rs")
            || name.ends_with("tests.rs")
        {
            continue;
        }
        let text = std::fs::read_to_string(&file).unwrap_or_default();
        for pattern in forbidden {
            if text.contains(pattern) {
                offenders.push(format!("{name}: {pattern}"));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "use crate::errors instead:\n{}",
        offenders.join("\n")
    );
}
