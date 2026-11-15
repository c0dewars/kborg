//! Readable YAML 1.2 inspection using JSON quoting for every string and mapping key.
use crate::{Result, safety};
use serde_json::Value;
const MAX_OUTPUT: usize = 8 * 1024 * 1024;
const MAX_DEPTH: usize = 64;

pub fn yaml(value: &Value) -> Result<String> {
    // Work only with redacted data, even when called independently of Gateway::get.
    document(value, safety::redact, Blocks::None)
}

/// YAML for the screen: [`safety::redact_shown`], with multi-line text (a
/// ConfigMap file) as a literal block instead of one escaped line.
pub fn yaml_shown(value: &Value) -> Result<String> {
    document(value, safety::redact_shown, Blocks::Text)
}

/// YAML for the editor. Its text is escaped to ASCII afterwards, and an
/// escape means nothing inside a literal block, so only ASCII text gets one.
pub fn yaml_editable(value: &Value) -> Result<String> {
    document(value, safety::redact_shown, Blocks::Ascii)
}

#[derive(Clone, Copy, PartialEq)]
enum Blocks {
    None,
    Text,
    Ascii,
}

fn document(value: &Value, redact: fn(&mut Value), blocks: Blocks) -> Result<String> {
    preflight(value, 0, &mut 0, &mut 0)?;
    let mut value = value.clone();
    redact(&mut value);
    let mut out = String::new();
    render(&value, 0, blocks, &mut out)?;
    Ok(out)
}

/// The lines of a literal block and whether the text ends with a line break,
/// or `None` when only a quoted scalar reproduces the text exactly.
fn block(text: &str, blocks: Blocks) -> Option<(Vec<&str>, bool)> {
    if blocks == Blocks::None || !text.contains('\n') || text.ends_with("\n\n") {
        return None;
    }
    let (body, newline) = match text.strip_suffix('\n') {
        Some(body) => (body, true),
        None => (text, false),
    };
    let lines: Vec<&str> = body.split('\n').collect();
    // The first line sets the block's indentation, so it cannot be blank or
    // start with a space.
    let first = lines.first()?;
    if first.is_empty() || first.starts_with(' ') {
        return None;
    }
    let plain = |line: &&str| {
        safety::safe_text(line, usize::MAX) == **line && (blocks == Blocks::Text || line.is_ascii())
    };
    lines.iter().all(plain).then_some((lines, newline))
}
fn preflight(value: &Value, depth: usize, bytes: &mut usize, nodes: &mut usize) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err("YAML inspection nesting exceeds 64 levels".into());
    }
    *nodes += 1;
    if *nodes > 100_000 {
        return Err("YAML inspection exceeds 100000 values".into());
    }
    *bytes = bytes.saturating_add(match value {
        Value::String(s) => s.len(),
        _ => 8,
    });
    if *bytes > MAX_OUTPUT {
        return Err("YAML inspection input exceeds 8 MiB".into());
    }
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                *bytes = bytes.saturating_add(key.len());
                preflight(child, depth + 1, bytes, nodes)?;
            }
        }
        Value::Array(items) => {
            for child in items {
                preflight(child, depth + 1, bytes, nodes)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn write(out: &mut String, text: &str) -> Result<()> {
    if out.len().saturating_add(text.len()) > MAX_OUTPUT {
        return Err("YAML inspection exceeds 8 MiB".into());
    }
    out.push_str(text);
    Ok(())
}
fn scalar(value: &Value) -> Result<String> {
    // JSON string escaping is a YAML 1.2 double-quoted scalar subset. Escape terminal
    // controls/bidi explicitly so inspection cannot execute terminal instructions.
    Ok(serde_json::to_string(value).map_err(|_| "cannot encode YAML scalar")?.chars().fold(String::new(), |mut text,c| {
        if c.is_control() || matches!(c, '\u{061c}'|'\u{200b}'..='\u{200f}'|'\u{202a}'..='\u{202e}'|'\u{2060}'..='\u{206f}'|'\u{feff}') {
            text.push_str(&format!("\\u{:04x}",c as u32));
        } else { text.push(c); } text
    }))
}
fn render(value: &Value, depth: usize, blocks: Blocks, out: &mut String) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err("YAML inspection nesting exceeds 64 levels".into());
    }
    let pad = "  ".repeat(depth);
    match value {
        Value::Object(map) if !map.is_empty() => {
            for (key, value) in map {
                write(out, &pad)?;
                write(out, &scalar(&Value::String(key.clone()))?)?;
                write(out, ":")?;
                entry(value, depth, blocks, out)?;
            }
        }
        Value::Array(items) if !items.is_empty() => {
            for value in items {
                write(out, &pad)?;
                write(out, "-")?;
                entry(value, depth, blocks, out)?;
            }
        }
        _ => {
            write(out, &pad)?;
            write(out, &scalar(value)?)?;
            write(out, "\n")?;
        }
    }
    Ok(())
}
fn entry(value: &Value, depth: usize, blocks: Blocks, out: &mut String) -> Result<()> {
    if matches!(value, Value::Object(m) if !m.is_empty())
        || matches!(value, Value::Array(a) if !a.is_empty())
    {
        write(out, "\n")?;
        return render(value, depth + 1, blocks, out);
    }
    if let Value::String(text) = value
        && let Some((lines, newline)) = block(text, blocks)
    {
        write(out, if newline { " |\n" } else { " |-\n" })?;
        let pad = "  ".repeat(depth + 1);
        for line in lines {
            if !line.is_empty() {
                write(out, &pad)?;
                write(out, line)?;
            }
            write(out, "\n")?;
        }
        return Ok(());
    }
    write(out, " ")?;
    write(out, &scalar(value)?)?;
    write(out, "\n")
}
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn quotes_ambiguous_strings_and_redacts_sensitive_values() -> Result<()> {
        let text = yaml(
            &json!({"a":"yes","b":"null","data":{"password":"sensitive"},"empty":[],"nested":[{"a":true}]}),
        )?;
        assert!(text.contains("\"a\": \"yes\""));
        assert!(!text.contains("sensitive"));
        assert!(text.contains("\"empty\": []"));
        assert!(text.contains("  -\n    \"a\": true"));
        Ok(())
    }
    #[test]
    fn hostile_strings_never_contain_terminal_controls() -> Result<()> {
        let text = yaml(&json!({"field":"\x1b]52;c;secret\x07\u{202e}\n"}))?;
        assert!(!text.contains('\x1b'));
        assert!(!text.contains('\u{202e}'));
        assert!(text.contains("field"));
        Ok(())
    }
    #[test]
    fn deep_values_are_rejected() {
        let mut value = Value::Null;
        for _ in 0..66 {
            value = json!({"nested":value});
        }
        assert!(yaml(&value).is_err());
    }
}
