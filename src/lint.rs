//! `:lint`, a read-only misconfiguration check in the spirit of k9s's popeye.
//! It reads one bounded snapshot and reports findings; it changes nothing.
use crate::{Result, gateway::Gateway, insights, safety};
use serde::Serialize;
use serde_json::Value;
use std::collections::BTreeMap;

const RESOURCES: &[&str] = &[
    "pods",
    "deployments",
    "statefulsets",
    "daemonsets",
    "cronjobs",
    "services",
];
const MAX_FINDINGS: usize = 500;
const RESTART_WARNING: u64 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
    Info,
}
impl Severity {
    fn label(self) -> &'static str {
        match self {
            Self::Error => "ERROR",
            Self::Warning => "WARN ",
            Self::Info => "INFO ",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub severity: Severity,
    pub object: String,
    pub message: String,
}

fn name(object: &Value) -> String {
    format!(
        "{}/{}",
        safety::safe_text(object["metadata"]["namespace"].as_str().unwrap_or("-"), 63),
        safety::safe_text(object["metadata"]["name"].as_str().unwrap_or("?"), 253)
    )
}

fn push(out: &mut Vec<Finding>, severity: Severity, object: &str, message: impl Into<String>) {
    out.push(Finding {
        severity,
        object: object.into(),
        message: message.into(),
    });
}

/// Container checks for one Pod template; `serving` adds probe checks.
fn template(spec: &Value, object: &str, serving: bool, out: &mut Vec<Finding>) {
    for (field, flag) in [
        ("hostNetwork", "host network"),
        ("hostPID", "host PID"),
        ("hostIPC", "host IPC"),
    ] {
        if spec[field] == true {
            push(
                out,
                Severity::Warning,
                object,
                format!("uses the {flag} namespace"),
            );
        }
    }
    for container in spec["containers"].as_array().into_iter().flatten() {
        let label = format!(
            "{object} [{}]",
            safety::safe_text(container["name"].as_str().unwrap_or("?"), 63)
        );
        let image = container["image"].as_str().unwrap_or("");
        let tag = image.rsplit('/').next().unwrap_or(image);
        if !image.contains('@') && (!tag.contains(':') || tag.ends_with(":latest")) {
            push(
                out,
                Severity::Warning,
                &label,
                "image has no tag or uses :latest; pin a version or digest",
            );
        }
        let resources = &container["resources"];
        if resources["requests"]["cpu"].is_null() || resources["requests"]["memory"].is_null() {
            push(
                out,
                Severity::Warning,
                &label,
                "no CPU or memory request; scheduling and eviction are unpredictable",
            );
        }
        if resources["limits"]["memory"].is_null() {
            push(out, Severity::Info, &label, "no memory limit");
        }
        if container["securityContext"]["privileged"] == true {
            push(out, Severity::Error, &label, "runs privileged");
        } else if container["securityContext"]["allowPrivilegeEscalation"] == true {
            push(
                out,
                Severity::Warning,
                &label,
                "allows privilege escalation",
            );
        }
        if serving {
            if container["readinessProbe"].is_null() {
                push(
                    out,
                    Severity::Warning,
                    &label,
                    "no readiness probe; traffic may reach it before it is ready",
                );
            }
            if container["livenessProbe"].is_null() {
                push(out, Severity::Info, &label, "no liveness probe");
            }
        }
    }
}

fn selects(selector: &serde_json::Map<String, Value>, labels: &Value) -> bool {
    selector
        .iter()
        .all(|(key, value)| labels.get(key) == Some(value))
}

pub fn findings(reads: &BTreeMap<String, Vec<Value>>) -> Vec<Finding> {
    let mut out = Vec::new();
    let empty = Vec::new();
    let items = |resource: &str| reads.get(resource).unwrap_or(&empty);
    for (resource, serving) in [
        ("deployments", true),
        ("statefulsets", true),
        ("daemonsets", false),
    ] {
        for object in items(resource) {
            let label = format!("{} {}", &resource[..resource.len() - 1], name(object));
            template(
                &object["spec"]["template"]["spec"],
                &label,
                serving,
                &mut out,
            );
            if resource == "deployments" {
                let wanted = object["spec"]["replicas"].as_u64().unwrap_or(1);
                if wanted == 1 {
                    push(
                        &mut out,
                        Severity::Info,
                        &label,
                        "one replica; no redundancy during rollouts or node loss",
                    );
                }
                if crate::cluster_health::degraded("Deployment", object) {
                    push(
                        &mut out,
                        Severity::Error,
                        &label,
                        "wants replicas that are not available",
                    );
                }
            }
        }
    }
    for object in items("cronjobs") {
        let label = format!("cronjob {}", name(object));
        template(
            &object["spec"]["jobTemplate"]["spec"]["template"]["spec"],
            &label,
            false,
            &mut out,
        );
    }
    let pods = items("pods");
    for pod in pods {
        let label = format!("pod {}", name(pod));
        if pod["metadata"]["ownerReferences"]
            .as_array()
            .is_none_or(Vec::is_empty)
        {
            push(
                &mut out,
                Severity::Info,
                &label,
                "bare Pod without a controller; it is not recreated if deleted or evicted",
            );
            template(&pod["spec"], &label, false, &mut out);
        }
        if pod["status"]["phase"] == "Pending" {
            push(&mut out, Severity::Warning, &label, "Pending");
        }
        for status in pod["status"]["containerStatuses"]
            .as_array()
            .into_iter()
            .flatten()
        {
            let reason = status["state"]["waiting"]["reason"].as_str().unwrap_or("");
            if [
                "CrashLoopBackOff",
                "ImagePullBackOff",
                "ErrImagePull",
                "CreateContainerConfigError",
            ]
            .contains(&reason)
            {
                push(
                    &mut out,
                    Severity::Error,
                    &label,
                    format!(
                        "container {} is {reason}",
                        safety::safe_text(status["name"].as_str().unwrap_or("?"), 63)
                    ),
                );
            }
            let restarts = status["restartCount"].as_u64().unwrap_or(0);
            if restarts >= RESTART_WARNING {
                push(
                    &mut out,
                    Severity::Warning,
                    &label,
                    format!(
                        "container {} restarted {restarts} times",
                        safety::safe_text(status["name"].as_str().unwrap_or("?"), 63)
                    ),
                );
            }
        }
    }
    for service in items("services") {
        let Some(selector) = service["spec"]["selector"]
            .as_object()
            .filter(|s| !s.is_empty())
        else {
            continue;
        };
        let namespace = &service["metadata"]["namespace"];
        if !pods.iter().any(|pod| {
            &pod["metadata"]["namespace"] == namespace
                && selects(selector, &pod["metadata"]["labels"])
        }) {
            push(
                &mut out,
                Severity::Warning,
                &format!("service {}", name(service)),
                "selector matches no Pods; the Service has no endpoints",
            );
        }
    }
    out.sort_by(|a, b| (a.severity, &a.object).cmp(&(b.severity, &b.object)));
    out
}

pub async fn collect(gateway: &Gateway, namespace: &str) -> Result<(Vec<Finding>, Vec<String>)> {
    let mut reads = BTreeMap::new();
    let mut notes = Vec::new();
    for read in insights::collect(gateway, namespace, RESOURCES).await {
        if let Some(error) = read.error {
            notes.push(format!("{}: unavailable ({error})", read.resource));
            continue;
        }
        if read.incomplete {
            notes.push(format!("{}: truncated to the first objects", read.resource));
        }
        reads.insert(read.resource, read.items);
    }
    if reads.is_empty() {
        return Err("no resource could be read; nothing to lint".into());
    }
    Ok((findings(&reads), notes))
}

pub fn report(context: &str, namespace: &str, findings: &[Finding], notes: &[String]) -> String {
    let count = |severity| findings.iter().filter(|f| f.severity == severity).count();
    let mut out = format!(
        "LINT  {} / {}\nRead-only misconfiguration check; nothing is changed.\n\n{} errors   {} warnings   {} info\n",
        safety::safe_text(context, 253),
        if namespace == "*" {
            "all namespaces"
        } else {
            namespace
        },
        count(Severity::Error),
        count(Severity::Warning),
        count(Severity::Info)
    );
    for note in notes {
        out.push_str(&format!("Note: {note}\n"));
    }
    out.push('\n');
    if findings.is_empty() {
        out.push_str("No findings.\n");
    }
    for finding in findings.iter().take(MAX_FINDINGS) {
        out.push_str(&format!(
            "{}  {}  {}\n",
            finding.severity.label(),
            finding.object,
            finding.message
        ));
    }
    if findings.len() > MAX_FINDINGS {
        out.push_str(&format!(
            "… {} more findings; narrow the namespace\n",
            findings.len() - MAX_FINDINGS
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn lint_flags_common_misconfigurations() {
        let mut reads = BTreeMap::new();
        reads.insert("deployments".to_owned(), vec![json!({"metadata":{"namespace":"shop","name":"api"},"spec":{"replicas":1,"template":{"spec":{"containers":[{"name":"app","image":"nginx","securityContext":{"privileged":true}}]}}},"status":{"availableReplicas":1}})]);
        reads.insert("pods".to_owned(), vec![
            json!({"metadata":{"namespace":"shop","name":"api-1","labels":{"app":"api"},"ownerReferences":[{"kind":"ReplicaSet"}]},"status":{"phase":"Running","containerStatuses":[{"name":"app","restartCount":7,"state":{"waiting":{"reason":"CrashLoopBackOff"}}}]}}),
            json!({"metadata":{"namespace":"shop","name":"debug"},"spec":{"containers":[{"name":"x","image":"busybox:1.37.0","resources":{"requests":{"cpu":"1m","memory":"1Mi"},"limits":{"memory":"8Mi"}}}]},"status":{"phase":"Pending"}}),
        ]);
        reads.insert("services".to_owned(), vec![
            json!({"metadata":{"namespace":"shop","name":"api"},"spec":{"selector":{"app":"api"}}}),
            json!({"metadata":{"namespace":"shop","name":"ghost"},"spec":{"selector":{"app":"gone"}}}),
        ]);
        let found = findings(&reads);
        let has = |severity, object: &str, text: &str| {
            found.iter().any(|f| {
                f.severity == severity && f.object.starts_with(object) && f.message.contains(text)
            })
        };
        assert!(has(
            Severity::Error,
            "deployment shop/api [app]",
            "privileged"
        ));
        assert!(has(
            Severity::Warning,
            "deployment shop/api [app]",
            ":latest"
        ));
        assert!(has(
            Severity::Warning,
            "deployment shop/api [app]",
            "readiness"
        ));
        assert!(has(Severity::Info, "deployment shop/api", "one replica"));
        assert!(has(Severity::Error, "pod shop/api-1", "CrashLoopBackOff"));
        assert!(has(
            Severity::Warning,
            "pod shop/api-1",
            "restarted 7 times"
        ));
        assert!(has(Severity::Info, "pod shop/debug", "bare Pod"));
        assert!(has(Severity::Warning, "service shop/ghost", "no endpoints"));
        assert!(!has(Severity::Warning, "service shop/api", "no endpoints"));
        assert!(!has(Severity::Warning, "pod shop/debug [x]", "request"));
        assert_eq!(found[0].severity, Severity::Error);
        let text = report("ctx", "shop", &found, &[]);
        assert!(has(Severity::Error, "deployment shop/api", "not available"));
        assert!(text.contains("3 errors"), "{text}");
    }
}
