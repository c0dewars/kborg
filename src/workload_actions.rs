//! Pure helpers for CronJob trigger/suspend and workload rollback reviews.
use crate::{Result, safety};
use serde_json::{Value, json};

/// A Job built from a CronJob's `jobTemplate`, as `kubectl create job --from`
/// does: same template, owned by the CronJob, marked as a manual run.
pub fn job_from_cronjob(cronjob: &Value, stamp: &str) -> Result<Value> {
    let name = cronjob["metadata"]["name"]
        .as_str()
        .ok_or("CronJob has no name")?;
    let template = &cronjob["spec"]["jobTemplate"];
    if !template["spec"].is_object() {
        return Err("CronJob has no jobTemplate".into());
    }
    let suffix = format!("-manual-{stamp}");
    let prefix: String = name.chars().take(63 - suffix.len()).collect();
    let job_name = format!("{}{suffix}", prefix.trim_end_matches(['-', '.']));
    let mut annotations = template["metadata"]["annotations"]
        .as_object()
        .cloned()
        .unwrap_or_default();
    annotations.insert("cronjob.kubernetes.io/instantiate".into(), json!("manual"));
    Ok(json!({
        "apiVersion": "batch/v1",
        "kind": "Job",
        "metadata": {
            "name": job_name,
            "namespace": cronjob["metadata"]["namespace"],
            "labels": template["metadata"]["labels"].clone(),
            "annotations": annotations,
            "ownerReferences": [{
                "apiVersion": "batch/v1",
                "kind": "CronJob",
                "name": name,
                "uid": cronjob["metadata"]["uid"],
                "controller": true,
                "blockOwnerDeletion": true
            }]
        },
        "spec": template["spec"].clone()
    }))
}

/// One restorable revision of a workload's Pod template.
#[derive(Clone, Debug, PartialEq)]
pub struct Revision {
    pub number: u64,
    pub template: Value,
}

/// Container `name=image` pairs, for a readable rollback summary.
pub fn images(template: &Value) -> Vec<String> {
    template["spec"]["containers"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| {
            format!(
                "{}={}",
                safety::safe_text(c["name"].as_str().unwrap_or("?"), 63),
                safety::safe_text(c["image"].as_str().unwrap_or("?"), 300)
            )
        })
        .collect()
}

/// Deployment revisions from its ReplicaSets (annotation
/// `deployment.kubernetes.io/revision`), without the generated
/// `pod-template-hash` label, newest first.
pub fn deployment_revisions(deployment_uid: &str, replicasets: &[Value]) -> Vec<Revision> {
    let mut out: Vec<Revision> = replicasets
        .iter()
        .filter(|rs| {
            rs["metadata"]["ownerReferences"]
                .as_array()
                .is_some_and(|owners| owners.iter().any(|o| o["uid"] == deployment_uid))
        })
        .filter_map(|rs| {
            let number = rs["metadata"]["annotations"]["deployment.kubernetes.io/revision"]
                .as_str()?
                .parse()
                .ok()?;
            let mut template = rs["spec"]["template"].clone();
            if let Some(labels) = template["metadata"]["labels"].as_object_mut() {
                labels.remove("pod-template-hash");
            }
            Some(Revision { number, template })
        })
        .collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.number));
    out
}

/// StatefulSet/DaemonSet revisions from ControllerRevisions. Their `data` is
/// the strategic-merge patch that restores the template.
pub fn controller_revisions(owner_uid: &str, revisions: &[Value]) -> Vec<Revision> {
    let mut out: Vec<Revision> = revisions
        .iter()
        .filter(|r| {
            r["metadata"]["ownerReferences"]
                .as_array()
                .is_some_and(|owners| owners.iter().any(|o| o["uid"] == owner_uid))
        })
        .filter_map(|r| {
            Some(Revision {
                number: r["revision"].as_u64()?,
                template: r["data"]["spec"]["template"].clone(),
            })
        })
        .filter(|r| r.template.is_object())
        .collect();
    out.sort_by_key(|r| std::cmp::Reverse(r.number));
    out
}

/// The revision to restore: `wanted`, or the newest one older than current.
pub fn choose(revisions: &[Revision], wanted: Option<u64>) -> Result<&Revision> {
    let current = revisions
        .first()
        .ok_or("no revision history found for this workload")?;
    match wanted {
        Some(number) if number == current.number => {
            Err(format!("revision {number} is already the current revision"))
        }
        Some(number) => revisions
            .iter()
            .find(|r| r.number == number)
            .ok_or_else(|| {
                format!(
                    "revision {number} not found; history has {}",
                    list(revisions)
                )
            }),
        None => revisions
            .get(1)
            .ok_or_else(|| "only one revision exists; nothing to roll back to".into()),
    }
}

pub fn list(revisions: &[Revision]) -> String {
    revisions
        .iter()
        .map(|r| r.number.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Pods that need attention, for the errors-only view (k9s Ctrl-Z).
pub fn faulty(object: &Value) -> bool {
    match object["kind"].as_str().unwrap_or("") {
        "Pod" => {
            let phase = object["status"]["phase"].as_str().unwrap_or("");
            if phase == "Succeeded" {
                return false;
            }
            phase != "Running"
                || object["status"]["containerStatuses"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .any(|c| c["ready"] != true)
        }
        "Deployment" | "StatefulSet" => {
            crate::cluster_health::degraded(object["kind"].as_str().unwrap_or(""), object)
        }
        "Node" => !object["status"]["conditions"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|c| c["type"] == "Ready" && c["status"] == "True"),
        "Event" => object["type"] == "Warning",
        "Job" => object["status"]["failed"].as_u64().unwrap_or(0) > 0,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn trigger_builds_an_owned_manual_job_from_the_template() -> Result<()> {
        let cron = json!({"metadata":{"name":"nightly-report-generator-with-a-very-long-name-x","namespace":"ops","uid":"c1"},"spec":{"jobTemplate":{"metadata":{"labels":{"app":"report"}},"spec":{"template":{"spec":{"restartPolicy":"Never","containers":[{"name":"r","image":"report:1"}]}}}}}});
        let job = job_from_cronjob(&cron, "1729800000")?;
        let name = job["metadata"]["name"].as_str().unwrap_or("");
        assert!(
            name.ends_with("-manual-1729800000") && name.len() <= 63,
            "{name}"
        );
        assert_eq!(job["metadata"]["ownerReferences"][0]["uid"], "c1");
        assert_eq!(
            job["metadata"]["annotations"]["cronjob.kubernetes.io/instantiate"],
            "manual"
        );
        assert_eq!(
            job["spec"]["template"]["spec"]["containers"][0]["image"],
            "report:1"
        );
        assert!(job_from_cronjob(&json!({"metadata":{"name":"x"},"spec":{}}), "1").is_err());
        Ok(())
    }
    #[test]
    fn revisions_come_from_owned_history_and_default_to_previous() -> Result<()> {
        let rs = |rev: &str, owner: &str, image: &str| json!({"metadata":{"ownerReferences":[{"uid":owner}],"annotations":{"deployment.kubernetes.io/revision":rev}},"spec":{"template":{"metadata":{"labels":{"app":"web","pod-template-hash":"abc"}},"spec":{"containers":[{"name":"web","image":image}]}}}});
        let history = deployment_revisions(
            "d1",
            &[
                rs("1", "d1", "web:1"),
                rs("3", "d1", "web:3"),
                rs("2", "d1", "web:2"),
                rs("9", "other", "x"),
            ],
        );
        assert_eq!(list(&history), "3, 2, 1");
        assert!(
            history[0].template["metadata"]["labels"]
                .get("pod-template-hash")
                .is_none()
        );
        assert_eq!(images(&choose(&history, None)?.template), vec!["web=web:2"]);
        assert_eq!(choose(&history, Some(1))?.number, 1);
        assert!(choose(&history, Some(3)).is_err());
        assert!(choose(&history, Some(7)).is_err());
        assert!(choose(&history[..1], None).is_err());
        let cr = json!({"metadata":{"ownerReferences":[{"uid":"s1"}]},"revision":4,"data":{"spec":{"template":{"spec":{"containers":[{"name":"db","image":"db:4"}]}}}}});
        assert_eq!(controller_revisions("s1", &[cr])[0].number, 4);
        Ok(())
    }
    #[test]
    fn errors_only_keeps_unhealthy_objects() {
        assert!(faulty(&json!({"kind":"Pod","status":{"phase":"Pending"}})));
        assert!(faulty(
            &json!({"kind":"Pod","status":{"phase":"Running","containerStatuses":[{"ready":false}]}})
        ));
        assert!(!faulty(
            &json!({"kind":"Pod","status":{"phase":"Running","containerStatuses":[{"ready":true}]}})
        ));
        assert!(!faulty(
            &json!({"kind":"Pod","status":{"phase":"Succeeded"}})
        ));
        assert!(faulty(&json!({"kind":"Event","type":"Warning"})));
        assert!(faulty(
            &json!({"kind":"Node","status":{"conditions":[{"type":"Ready","status":"False"}]}})
        ));
    }
}
