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
pub(crate) fn node_field(selector: &str) -> Result<Option<&str>> {
    if selector.is_empty() {
        return Ok(None);
    }
    let name = selector
        .strip_prefix("spec.nodeName=")
        .ok_or("only spec.nodeName field selectors are supported")?;
    if name.is_empty()
        || name.len() > 253
        || !name.split('.').all(|part| {
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
    {
        return Err("invalid node field selector".into());
    }
    Ok(Some(name))
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
}
