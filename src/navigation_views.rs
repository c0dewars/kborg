//! Native read-only counterparts for k9s :can and :dir navigation.
use crate::{Result, gateway::Gateway, safety};
use futures_util::{StreamExt, stream};
use serde_json::Value;
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

const MAX_MANIFEST: usize = 256 * 1024;
const MAX_OUTPUT: usize = 512 * 1024;
const RBAC_GROUP: &str = "rbac.authorization.k8s.io";

struct Subject {
    kind: &'static str,
    name: String,
    namespace: Option<String>,
}
impl Subject {
    fn parse(input: &str, namespace: &str) -> Result<Self> {
        let (kind, name) = input.split_once(':').ok_or("use can u:NAME, g:NAME or s:NAME (s:NAMESPACE:NAME for a service account in another namespace)")?;
        if name.is_empty()
            || name.len() > 253
            || name.chars().any(char::is_whitespace)
            || safety::safe_text(name, 253) != name
        {
            return Err("RBAC subject must be 1..253 visible characters without whitespace".into());
        }
        let (kind, name, namespace) = match kind {
            "u" => ("User", name, None),
            "g" => ("Group", name, None),
            "s" => {
                let (ns, name) = name.split_once(':').unwrap_or((namespace, name));
                if !safety::valid_id(ns) || !dns_name(name) {
                    return Err(
                        "service account needs a concrete namespace and a lowercase DNS name"
                            .into(),
                    );
                }
                ("ServiceAccount", name, Some(ns.to_owned()))
            }
            _ => return Err("RBAC subject prefix must be u:, g: or s:".into()),
        };
        Ok(Self {
            kind,
            name: name.to_owned(),
            namespace,
        })
    }
    fn matches(&self, subject: &Value, binding_namespace: &str) -> bool {
        if subject["kind"].as_str() != Some(self.kind)
            || subject["name"].as_str() != Some(&self.name)
        {
            return false;
        }
        let group = subject["apiGroup"].as_str().unwrap_or("");
        if self.kind == "ServiceAccount" {
            group.is_empty()
                && subject["namespace"].as_str().unwrap_or(binding_namespace)
                    == self.namespace.as_deref().unwrap_or("")
        } else {
            (group.is_empty() || group == RBAC_GROUP)
                && subject["namespace"].as_str().unwrap_or("").is_empty()
        }
    }
}

fn dns_name(name: &str) -> bool {
    !name.is_empty() && name.len() <= 253 && name.split('.').all(safety::valid_id)
}

struct RbacRead {
    resource: &'static str,
    items: Vec<Value>,
    incomplete: bool,
    error: Option<String>,
}

/// Declared Role/ClusterRole binding evidence, rather than effective authorization.
pub async fn can(gateway: &Gateway, namespace: &str, subject: &str) -> Result<String> {
    if namespace != "*" && !safety::valid_id(namespace) {
        return Err("invalid RBAC namespace scope".into());
    }
    let subject = Subject::parse(subject, namespace)?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let resources = [
        "roles.rbac.authorization.k8s.io",
        "rolebindings.rbac.authorization.k8s.io",
        "clusterroles.rbac.authorization.k8s.io",
        "clusterrolebindings.rbac.authorization.k8s.io",
    ];
    let reads: Vec<_> = stream::iter(resources.into_iter().map(|resource| async move {
        match tokio::time::timeout_at(deadline, gateway.list(resource, namespace)).await {
            Ok(Ok(mut list)) => RbacRead {
                resource,
                items: list["items"]
                    .as_array_mut()
                    .map(std::mem::take)
                    .unwrap_or_default(),
                incomplete: list["truncated"] == true,
                error: None,
            },
            result => RbacRead {
                resource,
                items: Vec::new(),
                incomplete: true,
                error: Some(match result {
                    Ok(Err(error)) => safety::safe_text(&error, 160),
                    _ => "RBAC snapshot deadline exceeded".into(),
                }),
            },
        }
    }))
    .buffered(4)
    .collect()
    .await;
    Ok(render_can(&subject, namespace, gateway.is_demo(), &reads))
}

fn field(value: &Value, key: &str) -> String {
    safety::safe_text(value[key].as_str().unwrap_or("?"), 253)
}
fn rule_values(rule: &Value, key: &str) -> String {
    let Some(values) = rule[key].as_array() else {
        return "-".into();
    };
    let mut text = values
        .iter()
        .take(32)
        .filter_map(Value::as_str)
        .map(|value| {
            if value.is_empty() && key == "apiGroups" {
                "core".into()
            } else {
                let mut shown = safety::safe_text(value, 128);
                if shown != value {
                    shown.push_str("[TRUNCATED/SANITIZED]");
                }
                if shown.is_empty() {
                    shown.push_str("\"\"");
                }
                shown
            }
        })
        .collect::<Vec<String>>()
        .join(",");
    if values.len() > 32 {
        text.push_str(",[TRUNCATED]");
    }
    if text.is_empty() {
        text.push('-');
    }
    text
}

fn render_can(subject: &Subject, scope: &str, demo: bool, reads: &[RbacRead]) -> String {
    let mut out = format!(
        "CAN | declared RBAC binding evidence{}\nSubject: {} {}{} | namespace scope: {scope}\n\nThis is not an authorization decision. User group memberships, external authorizers,\nimpersonation, admission and missing or truncated reads can change effective access.\nOnly explicit matching subject bindings are shown; group membership is not inferred.\n\n",
        if demo { " | DEMO" } else { "" },
        subject.kind,
        subject
            .namespace
            .as_ref()
            .map(|ns| format!("{ns}/"))
            .unwrap_or_default(),
        subject.name
    );
    let mut roles = BTreeMap::new();
    for read in reads {
        if let Some(error) = &read.error {
            out.push_str(&format!("INCOMPLETE {}: {error}\n", read.resource));
        } else if read.incomplete {
            out.push_str(&format!(
                "TRUNCATED {}: binding evidence may be missing\n",
                read.resource
            ));
        }
        for role in &read.items {
            if matches!(role["kind"].as_str(), Some("Role" | "ClusterRole")) {
                let key = (
                    field(role, "kind"),
                    role["metadata"]["namespace"]
                        .as_str()
                        .unwrap_or("")
                        .to_owned(),
                    field(&role["metadata"], "name"),
                );
                roles.insert(key, role);
            }
        }
    }
    let mut matched = 0;
    for read in reads {
        for binding in &read.items {
            let kind = binding["kind"].as_str().unwrap_or("");
            if !matches!(kind, "RoleBinding" | "ClusterRoleBinding") {
                continue;
            }
            let namespace = binding["metadata"]["namespace"].as_str().unwrap_or("");
            if !binding["subjects"].as_array().is_some_and(|subjects| {
                subjects.iter().any(|item| subject.matches(item, namespace))
            }) {
                continue;
            }
            matched += 1;
            if matched > 256 || out.len() > MAX_OUTPUT - 16 * 1024 {
                out.push_str("TRUNCATED binding evidence: narrow namespace or subject scope.\n");
                return out;
            }
            let role_kind = binding["roleRef"]["kind"].as_str().unwrap_or("");
            let role_name = field(&binding["roleRef"], "name");
            let role_ns = if role_kind == "Role" { namespace } else { "" };
            out.push_str(&format!(
                "\n{kind} {}{} -> {}/{role_name} [scope: {}]\n",
                if namespace.is_empty() {
                    String::new()
                } else {
                    format!("{}/", safety::safe_text(namespace, 63))
                },
                field(&binding["metadata"], "name"),
                safety::safe_text(role_kind, 63),
                if namespace.is_empty() {
                    "cluster / all namespaces".to_owned()
                } else {
                    safety::safe_text(namespace, 63)
                }
            ));
            if binding["roleRef"]["apiGroup"].as_str() != Some(RBAC_GROUP)
                || !matches!(role_kind, "Role" | "ClusterRole")
                || (kind == "ClusterRoleBinding" && role_kind != "ClusterRole")
            {
                out.push_str("  INVALID role reference; no rules inferred.\n");
                continue;
            }
            let Some(role) = roles.get(&(role_kind.to_owned(), role_ns.to_owned(), role_name))
            else {
                out.push_str("  UNRESOLVED role reference; missing, forbidden or outside available snapshot.\n");
                continue;
            };
            let Some(rules) = role["rules"].as_array() else {
                out.push_str("  No declared rules in available role snapshot.\n");
                continue;
            };
            for rule in rules.iter().take(64) {
                if out.len() > MAX_OUTPUT - 16 * 1024 {
                    out.push_str("TRUNCATED rules: narrow namespace or subject scope.\n");
                    return out;
                }
                let rendered = format!(
                    "  verbs: {} | groups: {} | resources: {}\n  resourceNames: {} | nonResourceURLs: {}\n",
                    rule_values(rule, "verbs"),
                    rule_values(rule, "apiGroups"),
                    rule_values(rule, "resources"),
                    rule_values(rule, "resourceNames"),
                    rule_values(rule, "nonResourceURLs")
                );
                if out.len() + rendered.len() + 128 > MAX_OUTPUT {
                    out.push_str("TRUNCATED rules: output limit reached.\n");
                    return out;
                }
                out.push_str(&rendered);
            }
            if rules.len() > 64 {
                out.push_str("  TRUNCATED role rules (64 shown).\n");
            }
        }
    }
    if matched == 0 {
        out.push_str("\nNo explicit matching subject bindings in the available snapshot. This does not prove denial.\n");
    }
    out
}

pub struct DirectoryEntry {
    pub name: String,
    pub path: PathBuf,
    pub directory: bool,
}
pub struct Directory {
    pub path: PathBuf,
    pub entries: Vec<DirectoryEntry>,
    pub truncated: bool,
}

fn local_path(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|_| "working directory unavailable")?
            .join(path)
    };
    let text = path.to_str().ok_or("local path must be UTF-8")?;
    if text.len() > 4096 || safety::safe_text(text, 4096) != text {
        return Err("local path contains controls or exceeds 4096 bytes".into());
    }
    safety::checked_path(&path)?;
    Ok(path)
}

fn manifest(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "yaml" | "yml" | "json"
            )
        })
}

fn validate_manifest(document: &mut Value, depth: usize) -> Result<()> {
    if depth > 32 {
        return Err("manifest List nesting exceeds 32 levels".into());
    }
    let api_version = document["apiVersion"]
        .as_str()
        .ok_or("manifest requires apiVersion and kind")?;
    let kind = document["kind"]
        .as_str()
        .ok_or("manifest requires apiVersion and kind")?;
    if api_version.is_empty()
        || api_version.len() > 320
        || !api_version
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".-/".contains(&byte))
        || kind.is_empty()
        || kind.len() > 128
        || !kind.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        return Err("manifest apiVersion or kind is invalid".into());
    }
    if kind.eq_ignore_ascii_case("Secret") {
        *document = serde_json::json!({"apiVersion":"v1","kind":"OmittedSecret","notice":"Secret document omitted"});
    } else if kind.ends_with("List") {
        let items = document
            .get_mut("items")
            .and_then(Value::as_array_mut)
            .ok_or("manifest List requires an items array")?;
        for item in items {
            validate_manifest(item, depth + 1)?;
        }
    }
    Ok(())
}

/// List only this directory. No recursive reads, script execution, apply or deletion.
pub fn directory(path: &Path) -> Result<Directory> {
    let path = local_path(path)?;
    let metadata = fs::symlink_metadata(&path).map_err(|_| "cannot inspect chosen directory")?;
    if !metadata.is_dir() {
        return Err("chosen path is not a directory".into());
    }
    let mut entries = Vec::new();
    let mut truncated = false;
    for (index, entry) in fs::read_dir(&path)
        .map_err(|_| "cannot list chosen directory")?
        .enumerate()
    {
        if index >= 4096 || entries.len() >= 256 {
            truncated = true;
            break;
        }
        let entry = entry.map_err(|_| "cannot read directory entry")?;
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if safety::safe_text(&name, 255) != name {
            continue;
        }
        let entry_path = entry.path();
        if safety::checked_path(&entry_path).is_err() {
            continue;
        }
        let metadata =
            fs::symlink_metadata(&entry_path).map_err(|_| "cannot inspect directory entry")?;
        if metadata.is_dir() || (metadata.is_file() && manifest(&entry_path)) {
            entries.push(DirectoryEntry {
                name,
                path: entry_path,
                directory: metadata.is_dir(),
            });
        }
    }
    entries.sort_by(|a, b| (!a.directory, &a.name).cmp(&(!b.directory, &b.name)));
    Ok(Directory {
        path,
        entries,
        truncated,
    })
}

/// Parse a bounded local manifest stream; Secret documents become `OmittedSecret`.
pub fn manifest_documents(path: &Path) -> Result<(PathBuf, Vec<Value>)> {
    let path = local_path(path)?;
    if !manifest(&path) {
        return Err("choose a .yaml, .yml or .json manifest".into());
    }
    let bytes = safety::read_small(&path, MAX_MANIFEST as u64)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "manifest is not UTF-8")?;
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_reader_input_bytes: Some(MAX_MANIFEST), max_events: 100_000,
            max_aliases: 64, max_anchors: 32, max_depth: 64, max_inclusion_depth: 0,
            max_documents: 32, max_nodes: 30_000, max_total_scalar_bytes: MAX_MANIFEST,
            max_merge_keys: 0,
        },
        alias_limits: serde_saphyr::alias_limits! { max_replay_stack_depth: 16, max_alias_expansions_per_anchor: 8 },
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys: serde_saphyr::MergeKeyPolicy::Error,
        with_snippet: false,
    };
    let mut documents: Vec<Value> = serde_saphyr::from_multiple_with_options(text,options)
        .map_err(|_| "manifest is malformed or exceeds document, nesting, alias or scalar limits; source details withheld")?;
    documents.retain(|document| !document.is_null());
    for document in &mut documents {
        validate_manifest(document, 0)?;
    }
    Ok((path, documents))
}

/// Parse a bounded local manifest stream and redact every document before display.
pub fn preview_manifest(path: &Path) -> Result<String> {
    let (path, mut documents) = manifest_documents(path)?;
    let mut out = format!(
        "LOCAL MANIFEST | {}\nRedacted preview. `a` reviews a server-side apply (--write --allow-apply).\n",
        path.display()
    );
    for document in &mut documents {
        safety::redact(document);
        let rendered = crate::inspect::yaml(document)?;
        if out.len() + rendered.len() + 8 > MAX_OUTPUT {
            return Err("redacted manifest preview exceeds 512 KiB".into());
        }
        out.push_str("\n---\n");
        out.push_str(&rendered);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn read(resource: &'static str, items: Vec<Value>) -> RbacRead {
        RbacRead {
            resource,
            items,
            incomplete: false,
            error: None,
        }
    }
    #[test]
    fn rbac_evidence_binds_service_account_namespace_and_does_not_infer_groups() -> Result<()> {
        let role = |ns: &str, resource: &str| json!({"kind":"Role","metadata":{"name":"viewer","namespace":ns},"rules":[{"verbs":["get"],"apiGroups":[""],"resources":[resource]}]});
        let binding = |ns: &str, subject: Value| json!({"kind":"RoleBinding","metadata":{"name":"viewer","namespace":ns},"subjects":[subject],"roleRef":{"apiGroup":RBAC_GROUP,"kind":"Role","name":"viewer"}});
        let reads = vec![
            read(
                "roles",
                vec![role("alpha", "pods"), role("beta", "deployments")],
            ),
            read(
                "rolebindings",
                vec![
                    binding(
                        "alpha",
                        json!({"kind":"ServiceAccount","name":"reader","namespace":"alpha"}),
                    ),
                    binding(
                        "beta",
                        json!({"kind":"ServiceAccount","name":"reader","namespace":"beta"}),
                    ),
                    binding(
                        "alpha",
                        json!({"kind":"Group","name":"system:authenticated","apiGroup":RBAC_GROUP}),
                    ),
                ],
            ),
        ];
        let subject = Subject::parse("s:beta:reader", "*")?;
        let output = render_can(&subject, "*", false, &reads);
        assert!(output.contains("RoleBinding beta/viewer"));
        assert!(output.contains("resources: deployments"));
        assert!(!output.contains("RoleBinding alpha/viewer"));
        assert!(!output.contains("resources: pods"));
        let user = Subject::parse("u:alice", "alpha")?;
        let output = render_can(&user, "alpha", false, &reads);
        assert!(output.contains("No explicit matching subject bindings"));
        assert!(!output.contains("resources: pods"));
        assert!(Subject::parse("s:reader", "*").is_err());
        assert!(Subject::parse("u:alice\u{001b}", "alpha").is_err());
        Ok(())
    }

    #[test]
    fn rbac_output_is_bounded_even_for_unicode_rule_values() -> Result<()> {
        let values = vec!["😀".repeat(200); 32];
        let rule = json!({"verbs":values,"apiGroups":values,"resources":values,"resourceNames":values,"nonResourceURLs":values});
        let reads = vec![
            read(
                "roles",
                vec![
                    json!({"kind":"Role","metadata":{"name":"viewer","namespace":"alpha"},"rules":vec![rule;64]}),
                ],
            ),
            read(
                "rolebindings",
                vec![
                    json!({"kind":"RoleBinding","metadata":{"name":"viewer","namespace":"alpha"},"subjects":[{"kind":"User","name":"alice","apiGroup":RBAC_GROUP}],"roleRef":{"apiGroup":RBAC_GROUP,"kind":"Role","name":"viewer"}}),
                ],
            ),
        ];
        let output = render_can(&Subject::parse("u:alice", "alpha")?, "alpha", false, &reads);
        assert!(output.len() <= MAX_OUTPUT);
        assert!(output.contains("TRUNCATED"));
        Ok(())
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Result<Self> {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let base = std::env::temp_dir()
                .canonicalize()
                .map_err(|_| "test temp directory unavailable")?;
            let path = base.join(format!(
                "kborg-navigation-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).map_err(|_| "cannot create navigation fixture")?;
            Ok(Self(path))
        }
        fn write(&self, name: &str, text: &str) -> Result<PathBuf> {
            let path = self.0.join(name);
            fs::write(&path, text).map_err(|_| "cannot write navigation fixture")?;
            Ok(path)
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn local_previews_redact_all_documents_and_bound_parser_work() -> Result<()> {
        let fixture = Fixture::new()?;
        let path = fixture.write("objects.yaml","apiVersion: v1\nkind: ConfigMap\ndata:\n  password: LOCAL_CANARY\n---\napiVersion: v1\nkind: Secret\ndata:\n  value: SECRET_CANARY\n---\napiVersion: apps/v1\nkind: Deployment\nspec:\n  template:\n    spec:\n      containers:\n        - name: app\n          image: nginx:1\n          env:\n            - name: TOKEN\n              value: ENV_CANARY\n")?;
        let shown = preview_manifest(&path)?;
        for canary in ["LOCAL_CANARY", "SECRET_CANARY", "ENV_CANARY"] {
            assert!(!shown.contains(canary));
        }
        assert!(shown.contains("Secret document omitted"));
        assert!(shown.contains("nginx:1"));
        let list = fixture.write("list.json",r#"{"apiVersion":"v1","kind":"List","items":[{"apiVersion":"v1","kind":"Secret","opaque":"NESTED_CANARY"}]}"#)?;
        let list = preview_manifest(&list)?;
        assert!(list.contains("Secret document omitted"));
        assert!(!list.contains("NESTED_CANARY"));
        for (name, text) in [
            ("credentials.json",r#"{"refresh_token":"LOCAL_CREDENTIAL_CANARY"}"#.into()),
            ("invalid-list.json",r#"{"apiVersion":"v1","kind":"List","items":[{"refresh_token":"LOCAL_CREDENTIAL_CANARY"}]}"#.into()),
            ("duplicate.yaml", "kind: Pod\nkind: Secret\n".to_owned()),
            (
                "deep.yaml",
                format!("{}0{}", "[".repeat(65), "]".repeat(65)),
            ),
            ("many.yaml", "---\nkind: Pod\n".repeat(33)),
            ("large.yaml", "a".repeat(MAX_MANIFEST + 1)),
            (
                "aliases.yaml",
                format!("a: &a [x]\nb: [{}]", vec!["*a"; 100].join(",")),
            ),
            ("include.yaml", "!include /private/credential-canary".into()),
        ] {
            let path = fixture.write(name, &text)?;
            assert!(preview_manifest(&path).is_err(), "{name} accepted");
        }
        Ok(())
    }

    #[test]
    fn directory_lists_only_one_level_and_never_follows_symlinks() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write("a.yaml", "kind: Pod\n")?;
        fixture.write("ignored.txt", "private-canary")?;
        fs::create_dir(fixture.0.join("nested")).map_err(|_| "cannot create child fixture")?;
        fixture.write("nested/nested.json", "{}")?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(fixture.0.join("a.yaml"), fixture.0.join("link.yaml"))
            .map_err(|_| "cannot create symlink fixture")?;
        let shown = directory(&fixture.0)?;
        assert_eq!(
            shown
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            vec!["nested", "a.yaml"]
        );
        assert!(!shown.truncated);
        #[cfg(unix)]
        assert!(preview_manifest(&fixture.0.join("link.yaml")).is_err());
        Ok(())
    }
}

/// One RBAC subject named by RoleBindings/ClusterRoleBindings (k9s `:users`,
/// `:groups`). Declared bindings only; group membership is never inferred.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubjectRow {
    pub kind: String,
    pub name: String,
    /// Service account namespace; empty for users and groups.
    pub namespace: String,
    pub bindings: usize,
    /// "ClusterRole/NAME" or "Role/NAME", sorted, at most four.
    pub roles: Vec<String>,
    pub role_count: usize,
}
impl SubjectRow {
    /// The `:can` argument for this subject.
    pub fn can_argument(&self) -> String {
        match self.kind.as_str() {
            "User" => format!("u:{}", self.name),
            "Group" => format!("g:{}", self.name),
            _ => format!("s:{}:{}", self.namespace, self.name),
        }
    }
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SubjectIndex {
    pub rows: Vec<SubjectRow>,
    pub incomplete: bool,
    pub errors: Vec<String>,
}
pub const MAX_SUBJECTS: usize = 2000;

/// Aggregates subjects across binding objects; `kind` keeps one subject kind.
pub fn subject_rows(bindings: &[Value], kind: Option<&str>) -> Vec<SubjectRow> {
    type Key = (String, String, String);
    let mut found: std::collections::BTreeMap<Key, (usize, std::collections::BTreeSet<String>)> =
        Default::default();
    for binding in bindings {
        let role = format!(
            "{}/{}",
            field(&binding["roleRef"], "kind"),
            field(&binding["roleRef"], "name")
        );
        let binding_namespace = binding["metadata"]["namespace"].as_str().unwrap_or("");
        for subject in binding["subjects"].as_array().into_iter().flatten() {
            let subject_kind = subject["kind"].as_str().unwrap_or("");
            if !matches!(subject_kind, "User" | "Group" | "ServiceAccount")
                || kind.is_some_and(|kind| kind != subject_kind)
            {
                continue;
            }
            let name = subject["name"].as_str().unwrap_or("");
            if name.is_empty() || name.len() > 253 || safety::safe_text(name, 253) != name {
                continue;
            }
            let namespace = if subject_kind == "ServiceAccount" {
                subject["namespace"].as_str().unwrap_or(binding_namespace)
            } else {
                ""
            };
            let entry = found
                .entry((subject_kind.into(), name.into(), namespace.into()))
                .or_default();
            entry.0 += 1;
            entry.1.insert(role.clone());
        }
    }
    found
        .into_iter()
        .take(MAX_SUBJECTS)
        .map(|((kind, name, namespace), (bindings, roles))| SubjectRow {
            role_count: roles.len(),
            roles: roles.into_iter().take(4).collect(),
            kind,
            name,
            namespace,
            bindings,
        })
        .collect()
}

pub async fn subjects(
    gateway: &Gateway,
    namespace: &str,
    kind: Option<&str>,
) -> Result<SubjectIndex> {
    if namespace != "*" && !safety::valid_id(namespace) {
        return Err("invalid RBAC namespace scope".into());
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let mut index = SubjectIndex::default();
    let mut bindings = Vec::new();
    for (resource, scope) in [
        ("rolebindings.rbac.authorization.k8s.io", namespace),
        ("clusterrolebindings.rbac.authorization.k8s.io", "*"),
    ] {
        match tokio::time::timeout_at(deadline, gateway.list(resource, scope)).await {
            Ok(Ok(mut list)) => {
                index.incomplete |= list["truncated"] == true;
                bindings.extend(
                    list["items"]
                        .as_array_mut()
                        .map(std::mem::take)
                        .unwrap_or_default(),
                );
            }
            Ok(Err(error)) => {
                index.incomplete = true;
                index
                    .errors
                    .push(format!("{resource}: {}", safety::safe_text(&error, 160)));
            }
            Err(_) => {
                index.incomplete = true;
                index.errors.push(format!("{resource}: deadline exceeded"));
            }
        }
    }
    index.rows = subject_rows(&bindings, kind);
    Ok(index)
}

#[cfg(test)]
mod subject_tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn subjects_aggregate_bindings_roles_and_service_account_namespaces() {
        let bindings = vec![
            json!({"metadata":{"namespace":"shop"},"roleRef":{"kind":"Role","name":"reader"},"subjects":[{"kind":"User","name":"alice"},{"kind":"ServiceAccount","name":"api"}]}),
            json!({"metadata":{},"roleRef":{"kind":"ClusterRole","name":"view"},"subjects":[{"kind":"User","name":"alice"},{"kind":"Group","name":"devs"},{"kind":"ServiceAccount","name":"api","namespace":"ops"}]}),
            json!({"metadata":{},"roleRef":{"kind":"ClusterRole","name":"x"},"subjects":[{"kind":"User","name":"bad\u{1b}[2J"},{"kind":"Other","name":"o"}]}),
        ];
        let rows = subject_rows(&bindings, None);
        let alice = rows.iter().find(|r| r.name == "alice").cloned();
        assert_eq!(
            alice.as_ref().map(|r| (r.bindings, r.role_count)),
            Some((2, 2))
        );
        assert_eq!(
            alice.map(|r| r.roles),
            Some(vec![
                "ClusterRole/view".to_owned(),
                "Role/reader".to_owned()
            ])
        );
        let accounts = rows
            .iter()
            .filter(|r| r.kind == "ServiceAccount")
            .map(|r| r.can_argument())
            .collect::<Vec<_>>();
        assert_eq!(accounts, ["s:ops:api", "s:shop:api"]);
        assert_eq!(rows.len(), 4);
        assert_eq!(subject_rows(&bindings, Some("Group")).len(), 1);
    }
}
