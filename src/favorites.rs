//! Recently used namespaces (per context) and contexts, k9s-style favorites
//! on digit keys. Stored as small private JSON; names only, no credentials.
use crate::{Result, safety};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_RECENT: usize = 9;
const MAX_CONTEXTS: usize = 64;
const FILE: &str = "recent.json";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Recent {
    #[serde(default)]
    pub contexts: Vec<String>,
    #[serde(default)]
    pub namespaces: BTreeMap<String, Vec<String>>,
}

fn bump(list: &mut Vec<String>, value: &str, limit: usize) -> bool {
    if list.first().is_some_and(|first| first == value) {
        return false;
    }
    list.retain(|existing| existing != value);
    list.insert(0, value.to_owned());
    list.truncate(limit);
    true
}

impl Recent {
    /// Record a visit; returns whether anything changed.
    pub fn visit(&mut self, context: &str, namespace: &str) -> bool {
        if context.is_empty() || safety::safe_text(context, 253) != context {
            return false;
        }
        let mut changed = bump(&mut self.contexts, context, MAX_CONTEXTS);
        if namespace != "*" && safety::valid_id(namespace) {
            let list = self.namespaces.entry(context.to_owned()).or_default();
            changed |= bump(list, namespace, MAX_RECENT);
        }
        if self.namespaces.len() > MAX_CONTEXTS {
            let keep: Vec<_> = self.contexts.clone();
            self.namespaces.retain(|context, _| keep.contains(context));
        }
        changed
    }
    pub fn namespaces(&self, context: &str) -> &[String] {
        self.namespaces.get(context).map_or(&[], Vec::as_slice)
    }
    /// `contexts` reordered so recently used ones come first.
    pub fn order_contexts(&self, mut contexts: Vec<String>) -> Vec<String> {
        contexts.sort_by_key(|name| {
            self.contexts
                .iter()
                .position(|recent| recent == name)
                .unwrap_or(usize::MAX)
        });
        contexts
    }
}

pub fn load() -> Recent {
    let Ok(path) = safety::config_dir().map(|dir| dir.join(FILE)) else {
        return Recent::default();
    };
    if !path.exists() {
        return Recent::default();
    }
    safety::read_small(&path, 64 * 1024)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

pub fn save(recent: &Recent) -> Result<()> {
    let path = safety::config_dir()?.join(FILE);
    let bytes = serde_json::to_vec_pretty(recent).map_err(|_| "cannot encode recent scopes")?;
    safety::atomic_write(&path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn recent_scopes_are_most_recent_first_bounded_and_per_context() {
        let mut recent = Recent::default();
        for ns in ["a", "b", "c", "a"] {
            recent.visit("prod", ns);
        }
        assert_eq!(recent.namespaces("prod"), ["a", "c", "b"]);
        assert!(!recent.visit("prod", "a"), "repeat visit changes nothing");
        assert!(!recent.visit("prod", "*") || recent.namespaces("prod").len() == 3);
        for i in 0..20 {
            recent.visit("dev", &format!("ns-{i}"));
        }
        assert_eq!(recent.namespaces("dev").len(), MAX_RECENT);
        assert_eq!(recent.namespaces("dev")[0], "ns-19");
        assert_eq!(
            recent.order_contexts(vec!["prod".into(), "other".into(), "dev".into()]),
            vec!["dev", "prod", "other"]
        );
        assert!(!recent.visit("bad\u{1b}", "x"));
    }
}
