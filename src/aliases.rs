//! k9s-compatible data-only aliases.yaml. Expansion always returns to our parser.
use crate::{Result, safety};
use serde::Deserialize;
use std::collections::{BTreeMap, HashSet};

#[derive(Deserialize, Default)]
#[serde(deny_unknown_fields)]
struct File {
    aliases: BTreeMap<String, String>,
}
pub type Aliases = BTreeMap<String, String>;

fn parse(text: &str) -> Result<Aliases> {
    if text.len() > 64 * 1024 {
        return Err("aliases.yaml exceeds 64 KiB".into());
    }
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_reader_input_bytes: Some(64*1024), max_events: 5000, max_aliases: 0, max_anchors: 0,
            max_depth: 4, max_inclusion_depth: 0, max_documents: 1, max_nodes: 1024,
            max_total_scalar_bytes: 64*1024, max_merge_keys: 0,
        },
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys: serde_saphyr::MergeKeyPolicy::Error,
        with_snippet: false,
    };
    let file: File = serde_saphyr::from_str_with_options(text, options).map_err(
        |_| "invalid aliases.yaml; expected aliases: mapping without duplicates/anchors",
    )?;
    if file.aliases.len() > 256 {
        return Err("aliases.yaml exceeds 256 aliases".into());
    }
    for (name, command) in &file.aliases {
        if name.is_empty()
            || name.len() > 63
            || !name
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-_".contains(&b))
        {
            return Err("alias names must be 1..63 lowercase letters, digits, - or _".into());
        }
        if command.trim().is_empty()
            || command.len() > 2048
            || safety::safe_text(command, 2048) != *command
        {
            return Err("alias commands must be bounded single-line plain text".into());
        }
        // Exit and identity commands retain their documented semantics.
        if [
            "q",
            "qa",
            "quit",
            "exit",
            "h",
            "help",
            "ctx",
            "context",
            "contexts",
            "ns",
            "namespace",
            "namespaces",
            "alias",
            "aliases",
            "a",
        ]
        .contains(&name.as_str())
        {
            return Err("custom alias cannot replace a built-in navigation command".into());
        }
        expand(name, &file.aliases)?;
    }
    Ok(file.aliases)
}
pub fn load() -> Result<Aliases> {
    let path = safety::config_dir()?.join("aliases.yaml");
    safety::checked_path(&path)?;
    if !path.exists() {
        return Ok(Aliases::new());
    }
    let bytes = safety::read_small(&path, 64 * 1024)?;
    parse(std::str::from_utf8(&bytes).map_err(|_| "aliases.yaml must be UTF-8")?)
}
pub fn expand(command: &str, aliases: &Aliases) -> Result<String> {
    let mut current = command.trim().trim_start_matches(':').to_owned();
    let mut seen = HashSet::new();
    for _ in 0..16 {
        if current.len() > 2048 {
            return Err("expanded alias exceeds command limit".into());
        }
        let end = current.find(char::is_whitespace).unwrap_or(current.len());
        let name = &current[..end];
        let Some(replacement) = aliases.get(name) else {
            return Ok(current);
        };
        if !seen.insert(name.to_owned()) {
            return Err("alias cycle detected".into());
        }
        current = format!("{}{}", replacement.trim(), &current[end..]);
    }
    Err("alias expansion exceeds 16 steps".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn upstream_mapping_and_chains_preserve_quoted_selector() -> Result<()> {
        let aliases = parse("aliases:\n  pp: 'v1/pods'\n  apis: \"pp 'app in (api,web)'\"\n")?;
        assert_eq!(
            expand("apis @Prod", &aliases)?,
            "v1/pods 'app in (api,web)' @Prod"
        );
        for yaml in [
            "aliases:\n  x: x",
            "aliases:\n  x: y\n  y: x",
            "aliases:\n  x: pods\n  x: nodes",
            "aliases:\n  q: pods",
            "aliases:\n  x: &cmd pods\n  y: *cmd",
        ] {
            assert!(parse(yaml).is_err(), "{yaml}");
        }
        Ok(())
    }
}
