//! `kborg describe RESOURCE NAME`: the TUI's describe text for one object,
//! plus what a person or an assistant needs to debug it in one read: each
//! container's last termination and the object's recent events. The object
//! comes from the CLI's strict projection (`Gateway::get`), so it shows what
//! `kborg get` shows: credential-like fields and ConfigMap data stay masked.
//! The TUI's describe view (`d`) appends the same two sections.
use crate::safety;
use serde_json::Value;

/// The recent events of `object`, asked for by its exact kind, name and UID
/// with a server-side field selector, so a busy namespace is not listed.
/// Cluster-scoped objects (Nodes, PersistentVolumes) record theirs in `default`.
pub async fn fetch_events(
    gateway: &crate::gateway::Gateway,
    object: &Value,
) -> std::result::Result<Vec<Value>, String> {
    let meta = &object["metadata"];
    let (Some(kind), Some(name), Some(uid)) = (
        object["kind"].as_str(),
        meta["name"].as_str(),
        meta["uid"].as_str(),
    ) else {
        return Err("the object has no kind, name or UID to match events by".into());
    };
    let namespace = meta["namespace"].as_str().unwrap_or("default");
    let fields = crate::selectors::object_events_field(kind, name, uid);
    // A UID that cannot go into a field selector (the demo cluster's are not
    // real UUIDs) falls back to the namespace's events, matched below.
    let fields = if crate::selectors::field_scope(&fields).is_ok() {
        fields
    } else {
        String::new()
    };
    let list = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        gateway.list_fields("events", namespace, "", &fields),
    )
    .await
    .map_err(|_| "the event list timed out".to_owned())??;
    Ok(events_for(
        object,
        list["items"].as_array().map_or(&[][..], Vec::as_slice),
    ))
}

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

/// The events about this object: kind and name, the namespace when it has
/// one, and the UID when the event records one, so the events of a deleted
/// object of the same name never show under its replacement.
pub fn events_for(object: &Value, events: &[Value]) -> Vec<Value> {
    let kind = &object["kind"];
    let name = &object["metadata"]["name"];
    let namespace = object["metadata"]["namespace"].as_str();
    let uid = object["metadata"]["uid"].as_str();
    let mut found: Vec<Value> = events
        .iter()
        .filter(|event| {
            let target = &event["involvedObject"];
            &target["kind"] == kind
                && &target["name"] == name
                && namespace.is_none_or(|ns| target["namespace"].as_str() == Some(ns))
                && target["uid"]
                    .as_str()
                    .is_none_or(|event_uid| Some(event_uid) == uid)
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

/// The full describe text: overview, last terminations, events (`None`
/// while they are still being read).
pub fn render(overview: &str, object: &Value, events: Option<Result<&[Value], &str>>) -> String {
    let mut out = overview.trim_end().to_owned();
    let terminations = terminations(object);
    if !terminations.is_empty() {
        out.push_str("\n\nLAST TERMINATION\n");
        out.push_str(&terminations.join("\n"));
    }
    let Some(events) = events else {
        return out;
    };
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
        // An event recording another UID belongs to an earlier object of this name.
        let mut replaced = event("web-1", "shop", "2026-11-15T10:08:00Z", "OldPod");
        replaced["involvedObject"]["uid"] = serde_json::json!("old-uid");
        let mut current = event("web-1", "shop", "2026-11-15T10:09:00Z", "SameUid");
        current["involvedObject"]["uid"] = serde_json::json!("uid-1");
        let mut pod = pod;
        pod["metadata"]["uid"] = serde_json::json!("uid-1");
        let uid_events = events_for(&pod, &[replaced, current]);
        let uid_reasons: Vec<&str> = uid_events
            .iter()
            .filter_map(|e| e["reason"].as_str())
            .collect();
        assert_eq!(uid_reasons, ["SameUid"]);
        let found = events_for(&pod, &events);
        let reasons: Vec<&str> = found.iter().filter_map(|e| e["reason"].as_str()).collect();
        assert_eq!(reasons, ["Pulled", "BackOff"]);
        let shown = render("POD / shop / web-1", &pod, Some(Ok(&found)));
        assert!(
            shown.contains("LAST TERMINATION\napp  OOMKilled (exit 137) at 2026-11-15T10:00:00Z"),
            "{shown}"
        );
        assert!(
            shown.contains("EVENTS (newest first)\n2026-11-15T10:07:00Z  Warning  Pulled  x3  m"),
            "{shown}"
        );
        assert!(
            render("x", &pod, Some(Ok(&[])))
                .ends_with("none recorded (events expire after about an hour)")
        );
        assert!(render("x", &pod, Some(Err("forbidden"))).ends_with("unavailable: forbidden"));
        // Before the events arrive, only the overview and terminations show.
        let early = render("x", &pod, None);
        assert!(early.contains("LAST TERMINATION") && !early.contains("EVENTS"));
    }
}
