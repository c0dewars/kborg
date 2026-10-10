//! Explicit, bounded, independent read-only context snapshots.
use crate::{
    Result,
    gateway::{Connection, Gateway},
    safety,
};
use futures_util::{StreamExt, stream};
use serde::Serialize;
use std::time::Duration;

#[derive(Serialize)]
pub struct ContextSummary {
    pub demo: bool,
    pub context: String,
    pub namespace: String,
    pub pods: Option<usize>,
    pub unhealthy: Option<usize>,
    pub truncated: bool,
    pub error: Option<String>,
}

pub fn validate(names: &[String]) -> Result<()> {
    if names.is_empty() || names.len() > 100 {
        return Err("overview requires 1..100 explicit contexts".into());
    }
    let mut seen = std::collections::HashSet::new();
    for name in names {
        if name.is_empty()
            || name.len() > 253
            || safety::safe_text(name, 253) != *name
            || !seen.insert(name)
        {
            return Err("overview context names must be unique bounded plain text".into());
        }
    }
    Ok(())
}

pub async fn collect(options: &Connection, names: &[String]) -> Result<Vec<ContextSummary>> {
    validate(names)?;
    // Each connection owns its identity and transport. No credentials, object caches,
    // or writes are shared; release each object list before retaining the summary.
    let mut pending = stream::iter(names.iter().cloned().map(|name| {
        let mut connection = options.clone();
        connection.context = Some(name.clone());
        connection.writable = false;
        connection.logs = false;
        connection.allow_exec = false;
        connection.allow_attach = false;
        connection.allow_port_forward = false;
        async move {
            let result = tokio::time::timeout(Duration::from_secs(20), async {
                let gateway = Gateway::connect(connection).await?;
                let list = gateway.list("pods", &gateway.namespace).await?;
                let items = list["items"].as_array().ok_or("invalid pod list")?;
                let unhealthy = items.iter().filter(|pod| pod_unhealthy(pod)).count();
                Ok::<_, String>((
                    gateway.namespace,
                    items.len(),
                    unhealthy,
                    list["truncated"] == true,
                ))
            })
            .await;
            match result {
                Ok(Ok((namespace, pods, unhealthy, truncated))) => ContextSummary {
                    demo: options.demo,
                    context: name,
                    namespace,
                    pods: Some(pods),
                    unhealthy: Some(unhealthy),
                    truncated,
                    error: None,
                },
                other => ContextSummary {
                    demo: options.demo,
                    context: name,
                    namespace: String::new(),
                    pods: None,
                    unhealthy: None,
                    truncated: false,
                    error: Some(match other {
                        Ok(Err(e)) => safety::safe_text(&e, 200),
                        _ => "context timed out".into(),
                    }),
                },
            }
        }
    }))
    .buffer_unordered(4);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut rows = Vec::new();
    while let Ok(Some(row)) = tokio::time::timeout_at(deadline, pending.next()).await {
        rows.push(row);
    }
    drop(pending);
    for name in names {
        if !rows.iter().any(|row| &row.context == name) {
            rows.push(ContextSummary {
                demo: options.demo,
                context: name.clone(),
                namespace: String::new(),
                pods: None,
                unhealthy: None,
                truncated: false,
                error: Some(
                    "overview 30-second global budget exceeded; narrow the selected contexts"
                        .into(),
                ),
            });
        }
    }
    rows.sort_by(|a, b| a.context.cmp(&b.context));
    Ok(rows)
}

fn pod_unhealthy(pod: &serde_json::Value) -> bool {
    if pod["metadata"]["deletionTimestamp"].is_string() {
        return true;
    }
    if pod["status"]["phase"] == "Succeeded" {
        return false;
    }
    crate::gateway::summary(pod) != "Running"
        || pod["status"]["containerStatuses"]
            .as_array()
            .is_none_or(|statuses| {
                statuses.is_empty() || statuses.iter().any(|status| status["ready"] != true)
            })
}

pub fn text(rows: &[ContextSummary]) -> String {
    let mut output = String::from(
        "READ ONLY — independent context snapshots (maximum 4 simultaneous requests)\nCONTEXT\tNAMESPACE\tPODS\tUNHEALTHY\tSTATE\n",
    );
    if rows.iter().any(|row| row.demo) {
        output.insert_str(0, "DEMO: synthetic samples; no cluster contacted\n");
    }
    for row in rows {
        output.push_str(&format!(
            "{}\t{}\t{}\t{}\t{}\n",
            row.context,
            row.namespace,
            row.pods.map_or("-".into(), |v| v.to_string()),
            row.unhealthy.map_or("-".into(), |v| v.to_string()),
            row.error.as_deref().unwrap_or(if row.truncated {
                "TRUNCATED"
            } else {
                "snapshot"
            })
        ));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unhealthy_includes_readiness_gates_terminating_and_unknown_status() {
        use serde_json::json;
        assert!(pod_unhealthy(
            &json!({"kind":"Pod","status":{"phase":"Running"}})
        ));
        assert!(pod_unhealthy(
            &json!({"kind":"Pod","status":{"phase":"Running","containerStatuses":[{"ready":true}],"conditions":[{"type":"Ready","status":"False"}]}})
        ));
        assert!(pod_unhealthy(
            &json!({"kind":"Pod","metadata":{"deletionTimestamp":"now"},"status":{"phase":"Succeeded"}})
        ));
        assert!(!pod_unhealthy(
            &json!({"kind":"Pod","status":{"phase":"Succeeded"}})
        ));
        assert!(!pod_unhealthy(
            &json!({"kind":"Pod","status":{"phase":"Running","containerStatuses":[{"ready":true}]}})
        ));
    }
    #[test]
    fn rejects_ambiguous_and_unbounded_scope() {
        assert!(validate(&[]).is_err());
        assert!(validate(&vec!["a".into(); 101]).is_err());
        assert!(validate(&["a".into(), "a".into()]).is_err());
        assert!(validate(&["bad\u{1b}".into()]).is_err());
    }
    #[test]
    fn snapshots_are_read_only_and_errors_are_not_zeroes() {
        crate::cli::runtime()
            .unwrap_or_else(|e| panic!("{e}"))
            .block_on(async {
                let rows = collect(
                    &Connection {
                        demo: true,
                        writable: true,
                        ..Default::default()
                    },
                    &["demo".into()],
                )
                .await
                .unwrap_or_else(|e| panic!("{e}"));
                assert!(rows[0].demo);
                assert!(text(&rows).starts_with("DEMO:"));
                assert_eq!(rows[0].pods, Some(2));
                assert_eq!(rows[0].unhealthy, Some(1));
            });
    }
}
