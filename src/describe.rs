//! `kborg describe RESOURCE NAME`: the TUI's describe text for one object,
//! plus what a person or an assistant needs to debug it in one read: each
//! container's last termination and the object's recent events. The object
//! comes from the CLI's strict projection (`Gateway::get`), so it shows what
//! `kborg get` shows: credential-like fields and ConfigMap data stay masked.
use crate::safety;
use serde_json::Value;

/// Events listed under an object, newest first.
pub const MAX_EVENTS: usize = 15;

fn text(value: &Value) -> String {
    match value {
        Value::Null => "-".into(),
        Value::String(text) => safety::safe_text(text, 300),
        other => safety::safe_text(&other.to_string(), 300),
    }
}

/// When an event last happened, for sorting and display.
fn when(event: &Value) -> &str {
    ["lastTimestamp", "eventTime", "firstTimestamp"]
        .iter()
        .find_map(|field| event[field].as_str())
        .or_else(|| event["metadata"]["creationTimestamp"].as_str())
        .unwrap_or("")
}

/// The events about this object (kind and name, and namespace when it has one).
pub fn events_for(object: &Value, events: &[Value]) -> Vec<Value> {
    let kind = &object["kind"];
    let name = &object["metadata"]["name"];
    let namespace = object["metadata"]["namespace"].as_str();
    let mut found: Vec<Value> = events
        .iter()
        .filter(|event| {
            let target = &event["involvedObject"];
            &target["kind"] == kind
                && &target["name"] == name
                && namespace.is_none_or(|ns| target["namespace"].as_str() == Some(ns))
        })
        .cloned()
        .collect();
    found.sort_by(|a, b| when(b).cmp(when(a)));
    found.truncate(MAX_EVENTS);
    found
}

/// Last terminations of the object's containers (Pods only).
fn terminations(object: &Value) -> Vec<String> {
    let mut lines = Vec::new();
    for field in ["initContainerStatuses", "containerStatuses"] {
        for status in object["status"][field].as_array().into_iter().flatten() {
            let last = &status["lastState"]["terminated"];
            if last.is_null() {
                continue;
            }
            lines.push(format!(
                "{}  {} (exit {}) at {}",
                text(&status["name"]),
                text(&last["reason"]),
                text(&last["exitCode"]),
                text(&last["finishedAt"])
            ));
        }
    }
    lines
}

/// The full describe text: overview, last terminations, events.
pub fn render(overview: &str, object: &Value, events: Result<&[Value], &str>) -> String {
    let mut out = overview.trim_end().to_owned();
    let terminations = terminations(object);
    if !terminations.is_empty() {
        out.push_str("\n\nLAST TERMINATION\n");
        out.push_str(&terminations.join("\n"));
    }
    out.push_str("\n\nEVENTS (newest first)\n");
    match events {
        Err(error) => out.push_str(&format!("unavailable: {}", safety::safe_text(error, 300))),
        Ok([]) => out.push_str("none recorded (events expire after about an hour)"),
        Ok(events) => {
            for event in events {
                out.push_str(&format!(
                    "{}  {}  {}  x{}  {}\n",
                    text(&Value::String(when(event).to_owned())),
                    text(&event["type"]),
                    text(&event["reason"]),
                    event["count"].as_u64().unwrap_or(1),
                    text(&event["message"])
                ));
            }
            out.truncate(out.trim_end().len());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn events_are_the_objects_own_newest_first_and_terminations_are_listed() {
        let pod = json!({"kind":"Pod","metadata":{"name":"web-1","namespace":"shop"},
            "status":{"containerStatuses":[{"name":"app","lastState":{"terminated":{"reason":"OOMKilled","exitCode":137,"finishedAt":"2026-11-15T10:00:00Z"}}}]}});
        let event = |name: &str, ns: &str, at: &str, reason: &str| {
            json!({"involvedObject":{"kind":"Pod","name":name,"namespace":ns},"type":"Warning",
                "reason":reason,"count":3,"message":"m","lastTimestamp":at})
        };
        let events = [
            event("web-1", "shop", "2026-11-15T10:00:00Z", "BackOff"),
            event("web-1", "other", "2026-11-15T10:05:00Z", "Elsewhere"),
            event("web-2", "shop", "2026-11-15T10:06:00Z", "NotThisPod"),
            event("web-1", "shop", "2026-11-15T10:07:00Z", "Pulled"),
        ];
        let found = events_for(&pod, &events);
        let reasons: Vec<&str> = found.iter().filter_map(|e| e["reason"].as_str()).collect();
        assert_eq!(reasons, ["Pulled", "BackOff"]);
        let shown = render("POD / shop / web-1", &pod, Ok(&found));
        assert!(
            shown.contains("LAST TERMINATION\napp  OOMKilled (exit 137) at 2026-11-15T10:00:00Z"),
            "{shown}"
        );
        assert!(
            shown.contains("EVENTS (newest first)\n2026-11-15T10:07:00Z  Warning  Pulled  x3  m"),
            "{shown}"
        );
        assert!(
            render("x", &pod, Ok(&[]))
                .ends_with("none recorded (events expire after about an hour)")
        );
        assert!(render("x", &pod, Err("forbidden")).ends_with("unavailable: forbidden"));
    }
}
