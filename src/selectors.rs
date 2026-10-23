//! Kubernetes label selectors: server-side filtering with matching demo semantics.
use crate::Result;
use serde_json::Value;

enum Requirement<'a> {
    Exists(&'a str, bool),
    Values(&'a str, Vec<&'a str>, bool),
}

fn label_name(s: &str, empty: bool) -> bool {
    (empty && s.is_empty())
        || (!s.is_empty()
            && s.len() <= 63
            && s.as_bytes().first().is_some_and(u8::is_ascii_alphanumeric)
            && s.as_bytes().last().is_some_and(u8::is_ascii_alphanumeric)
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b)))
}
fn key_valid(s: &str) -> bool {
    let (prefix, name) = s.rsplit_once('/').map_or((None, s), |(p, n)| (Some(p), n));
    label_name(name, false)
        && prefix.is_none_or(|p| {
            !p.is_empty()
                && p.len() <= 253
                && p.split('.').all(|part| {
                    !part.is_empty()
                        && part.len() <= 63
                        && part
                            .as_bytes()
                            .first()
                            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                        && part
                            .as_bytes()
                            .last()
                            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                        && part
                            .bytes()
                            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
                })
        })
}
fn parse(selector: &str) -> Result<Vec<Requirement<'_>>> {
    if selector.len() > 2048 || selector.chars().any(char::is_control) {
        return Err("label selector exceeds 2048 bytes or contains control characters".into());
    }
    if selector.trim().is_empty() {
        return Ok(Vec::new());
    }
    let invalid =
        || "invalid label selector; use app=web,tier!=batch,app in (api,web),key,!key".to_owned();
    let mut parts = Vec::new();
    let mut depth = 0;
    let mut start = 0;
    for (i, c) in selector.char_indices() {
        match c {
            '(' if depth == 0 => depth = 1,
            ')' if depth == 1 => depth = 0,
            '(' | ')' => return Err(invalid()),
            ',' if depth == 0 => {
                parts.push(&selector[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if depth != 0 {
        return Err(invalid());
    }
    parts.push(&selector[start..]);
    if parts.len() > 64 {
        return Err("label selector exceeds 64 requirements".into());
    }
    let mut out = Vec::new();
    for part in parts {
        let part = part.trim();
        let requirement = if let Some((left, values)) = part.split_once('(') {
            let values = values.strip_suffix(')').ok_or_else(invalid)?;
            let mut words = left.split_whitespace();
            let key = words.next().ok_or_else(invalid)?;
            let op = words.next().ok_or_else(invalid)?;
            if words.next().is_some() || !matches!(op, "in" | "notin") {
                return Err(invalid());
            }
            Requirement::Values(
                key,
                values.split(',').map(str::trim).collect(),
                op == "notin",
            )
        } else if let Some((key, value)) = part.split_once("!=") {
            Requirement::Values(key.trim(), vec![value.trim()], true)
        } else if let Some((key, value)) = part.split_once("==").or_else(|| part.split_once('=')) {
            Requirement::Values(key.trim(), vec![value.trim()], false)
        } else if let Some(key) = part.strip_prefix('!') {
            Requirement::Exists(key.trim(), false)
        } else {
            Requirement::Exists(part, true)
        };
        let key = match &requirement {
            Requirement::Exists(key, _) => key,
            Requirement::Values(key, values, _) => {
                if values.iter().any(|v| !label_name(v, true)) {
                    return Err(invalid());
                }
                key
            }
        };
        if !key_valid(key) {
            return Err(invalid());
        }
        out.push(requirement);
    }
    Ok(out)
}
pub fn validate(selector: &str) -> Result<()> {
    parse(selector).map(|_| ())
}
/// The only server-side field selectors KBorg sends: Pods on one Node, and the
/// Events recorded for one exact Pod identity. Anything else is rejected.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FieldScope<'a> {
    Node(&'a str),
    PodEvents { name: &'a str, uid: &'a str },
}

impl FieldScope<'_> {
    /// Core v1 kind the selector applies to.
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Node(_) => "Pod",
            Self::PodEvents { .. } => "Event",
        }
    }
    pub fn matches(&self, object: &Value) -> bool {
        match self {
            Self::Node(node) => object["spec"]["nodeName"].as_str() == Some(node),
            Self::PodEvents { name, uid } => {
                let involved = &object["involvedObject"];
                involved["kind"] == "Pod" && involved["name"] == *name && involved["uid"] == *uid
            }
        }
    }
}

const POD_EVENTS_PREFIX: &str = "involvedObject.kind=Pod,involvedObject.name=";

pub(crate) fn pod_events_field(name: &str, uid: &str) -> String {
    format!("{POD_EVENTS_PREFIX}{name},involvedObject.uid={uid}")
}

fn dns_subdomain(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 253
        && name.split('.').all(|part| {
            !part.is_empty()
                && part.len() <= 63
                && part
                    .as_bytes()
                    .first()
                    .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                && part
                    .as_bytes()
                    .last()
                    .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        })
}

fn object_uid(uid: &str) -> bool {
    (1..=64).contains(&uid.len())
        && uid
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f' | b'-'))
}

pub(crate) fn field_scope(selector: &str) -> Result<Option<FieldScope<'_>>> {
    if selector.is_empty() {
        return Ok(None);
    }
    if let Some(name) = selector.strip_prefix("spec.nodeName=") {
        if !dns_subdomain(name) {
            return Err("invalid node field selector".into());
        }
        return Ok(Some(FieldScope::Node(name)));
    }
    if let Some(rest) = selector.strip_prefix(POD_EVENTS_PREFIX) {
        let (name, uid) = rest
            .split_once(",involvedObject.uid=")
            .ok_or("invalid Pod event field selector")?;
        if !dns_subdomain(name) || !object_uid(uid) {
            return Err("invalid Pod event field selector".into());
        }
        return Ok(Some(FieldScope::PodEvents { name, uid }));
    }
    Err("only spec.nodeName and exact Pod event field selectors are supported".into())
}
pub fn matches(selector: &str, labels: &Value) -> bool {
    parse(selector).is_ok_and(|requirements| {
        requirements.iter().all(|r| match r {
            Requirement::Exists(key, expected) => labels.get(*key).is_some() == *expected,
            Requirement::Values(key, values, inverse) => {
                labels
                    .get(*key)
                    .and_then(Value::as_str)
                    .is_some_and(|v| values.contains(&v))
                    != *inverse
            }
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn kubernetes_set_and_absence_semantics() {
        let labels = serde_json::json!({"app":"web", "example.io/tier":"API", "empty":""});
        for selector in [
            "",
            "app",
            "!missing",
            "missing!=yes",
            "app==web",
            "app in (api,web)",
            "app notin (other),example.io/tier=API",
            "empty=",
        ] {
            assert!(matches(selector, &labels), "{selector}");
        }
        for selector in [
            "missing",
            "!app",
            "app!=web",
            "app in (other)",
            "app=api",
            "app in (web",
            "app>>web",
            "app=web,",
            "Bad.io/tier=API",
        ] {
            assert!(!matches(selector, &labels), "{selector}");
        }
        assert!(validate(&"a".repeat(2049)).is_err());
        assert!(validate("app=web\n").is_err());
    }
    #[test]
    fn field_scopes_are_exact_and_reject_injection() {
        assert_eq!(field_scope(""), Ok(None));
        assert_eq!(
            field_scope("spec.nodeName=worker-a"),
            Ok(Some(FieldScope::Node("worker-a")))
        );
        let uid = "0f0b21f0-af23-4c1e-9a77-3546a8e1e804";
        let events = pod_events_field("api-7d9f", uid);
        assert_eq!(
            field_scope(&events),
            Ok(Some(FieldScope::PodEvents {
                name: "api-7d9f",
                uid
            }))
        );
        let event =
            serde_json::json!({"involvedObject":{"kind":"Pod","name":"api-7d9f","uid":uid}});
        assert!(
            field_scope(&events)
                .ok()
                .flatten()
                .is_some_and(|s| s.matches(&event))
        );
        let replaced =
            serde_json::json!({"involvedObject":{"kind":"Pod","name":"api-7d9f","uid":"other"}});
        assert!(
            !field_scope(&events)
                .ok()
                .flatten()
                .is_some_and(|s| s.matches(&replaced))
        );
        for hostile in [
            "metadata.name=x",
            "spec.nodeName=Worker",
            "spec.nodeName=a,spec.nodeName=b",
            "involvedObject.kind=Pod,involvedObject.name=api",
            "involvedObject.kind=Pod,involvedObject.name=api,involvedObject.uid=",
            "involvedObject.kind=Pod,involvedObject.name=api,involvedObject.uid=ABC",
            "involvedObject.kind=Pod,involvedObject.name=api,involvedObject.uid=a,type=Normal",
            "involvedObject.kind=Pod,involvedObject.name=a,b,involvedObject.uid=abc",
        ] {
            assert!(field_scope(hostile).is_err(), "{hostile}");
        }
    }
}
