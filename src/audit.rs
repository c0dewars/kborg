//! Crash recovery reports are observations, never authorization to retry an operation.
use crate::{Result, safety};
use serde::Serialize;
use serde_json::Value;
#[derive(Serialize)]
pub struct Pending {
    pub id: String,
    pub context: String,
    pub namespace: String,
    pub resource: String,
    pub name: String,
    pub uid: String,
    pub operation: String,
    pub state: String,
}
fn parse(id: String, bytes: &[u8]) -> Result<Option<Pending>> {
    let text = std::str::from_utf8(bytes).map_err(|_| "invalid audit encoding")?;
    let mut lines = text.lines();
    let intent: Value = serde_json::from_str(lines.next().ok_or("empty audit record")?)
        .map_err(|_| "invalid audit intent")?;
    if intent["stage"] != "intent" || intent["schema_version"] != 1 {
        return Err("unsupported audit record".into());
    }
    let mut state = "UNKNOWN: no durable outcome; inspect API server state before any retry";
    for line in lines {
        let Ok(outcome) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if outcome["stage"] == "outcome" && outcome["status"] == "succeeded" {
            return Ok(None);
        }
        if outcome["stage"] == "outcome" {
            state = "FAILED OR UNKNOWN: reconcile server state; do not automatically retry";
        }
    }
    let field = |name: &str| safety::safe_text(intent[name].as_str().unwrap_or("unknown"), 253);
    // Historical actions may include Debug output. Only retain known operation labels.
    let action = intent["action"].as_str().unwrap_or("");
    let operation = if action.starts_with("Scale") || action == "scale" {
        "scale"
    } else if action == "Restart" || action == "restart" {
        "restart"
    } else if action == "Delete" || action == "delete" {
        "delete"
    } else if action == "set-image" {
        "set-image"
    } else if action == "set-resources" {
        "set-resources"
    } else {
        "operation"
    };
    Ok(Some(Pending {
        id,
        context: field("context"),
        namespace: field("namespace"),
        resource: field("resource"),
        name: field("name"),
        uid: field("uid"),
        operation: operation.into(),
        state: state.into(),
    }))
}
pub fn pending() -> Result<Vec<Pending>> {
    let directory = safety::config_dir()?.join("audit");
    safety::checked_path(&directory)?;
    if !directory.exists() {
        return Ok(vec![]);
    }
    let mut paths = Vec::new();
    for entry in std::fs::read_dir(directory).map_err(|_| "cannot inspect audit spool")? {
        let entry = entry.map_err(|_| "cannot read audit entry")?;
        if paths.len() == 1000 {
            return Err(
                "audit spool exceeds 1000 entries; archive with an approved retention process"
                    .into(),
            );
        }
        paths.push(entry.path());
    }
    paths.sort();
    let mut records = vec![];
    for path in paths {
        let id = path
            .file_name()
            .and_then(|x| x.to_str())
            .ok_or("invalid audit filename")?;
        if !id.ends_with(".jsonl")
            || !id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.'))
        {
            return Err("unexpected audit spool entry".into());
        }
        if let Some(record) = parse(id.into(), &safety::read_small(&path, 16 * 1024)?)? {
            records.push(record);
        }
    }
    Ok(records)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn crash_partial_outcome_is_unknown_not_success() {
        let record=b"{\"schema_version\":1,\"stage\":\"intent\",\"action\":\"set-image\"}\n{\"stage\":\"out";
        assert!(
            parse("1.jsonl".into(), record)
                .unwrap_or_else(|e| panic!("{e}"))
                .is_some()
        );
    }
    #[test]
    fn durable_success_is_not_pending() {
        let record=b"{\"schema_version\":1,\"stage\":\"intent\"}\n{\"stage\":\"outcome\",\"status\":\"succeeded\"}\n";
        assert!(
            parse("1.jsonl".into(), record)
                .unwrap_or_else(|e| panic!("{e}"))
                .is_none()
        );
    }
}
