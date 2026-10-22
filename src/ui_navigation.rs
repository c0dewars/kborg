//! Command-line navigation. This grammar follows k9s, without invoking a shell.
use crate::Result;

pub const COMMAND_BYTES: usize = 2048;
pub fn valid_resource(resource: &str) -> bool {
    !resource.is_empty()
        && resource.len() <= 512
        && resource.split('/').count() <= 3
        && resource.split('/').all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b".-".contains(&b))
                && part
                    .as_bytes()
                    .first()
                    .is_some_and(u8::is_ascii_alphanumeric)
                && part
                    .as_bytes()
                    .last()
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ResourceCommand {
    pub resource: String,
    pub namespace: Option<String>,
    pub context: Option<String>,
    pub filter: String,
    pub selector: String,
}

/// Single quotes group a Kubernetes set selector; they never enable expansion.
pub fn words(line: &str) -> Result<Vec<String>> {
    if line.len() > COMMAND_BYTES || line.chars().any(char::is_control) {
        return Err("Command exceeds 2048 bytes or contains control characters.".into());
    }
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for ch in line.trim().trim_start_matches(':').chars() {
        match ch {
            '\'' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if quoted {
        return Err("Unclosed selector quote; use :pod 'app in (api,web)'".into());
    }
    if !word.is_empty() {
        words.push(word);
    }
    Ok(words)
}

pub fn resource(words: &[String]) -> Result<ResourceCommand> {
    let Some(first) = words.first() else {
        return Err("Enter a resource alias.".into());
    };
    let mut command = ResourceCommand {
        resource: first.to_ascii_lowercase(),
        ..Default::default()
    };
    let mut args = words.iter().skip(1);
    while let Some(arg) = args.next() {
        if let Some(context) = arg.strip_prefix('@') {
            if context.is_empty() || command.context.replace(context.into()).is_some() {
                return Err("Use one @context argument.".into());
            }
        } else if let Some(filter) = arg.strip_prefix('/') {
            if !command.filter.is_empty() {
                return Err("Use one /filter argument.".into());
            }
            command.filter = filter.into();
        } else if arg == "-f" {
            command.filter = format!("-f {}", args.next().ok_or("Use -f PATTERN")?);
        } else if arg == "-n" {
            let ns = args.next().ok_or("Use -n NAMESPACE")?;
            if command.namespace.replace(ns.clone()).is_some() {
                return Err("Use one namespace argument.".into());
            }
        } else if arg.contains('=')
            || arg.contains(" in ")
            || arg.contains(" notin ")
            || arg.starts_with('!')
        {
            if !command.selector.is_empty() {
                return Err("Combine label requirements with commas.".into());
            }
            command.selector = arg.clone();
        } else if command.namespace.replace(arg.clone()).is_some() {
            return Err("Use :RESOURCE [NAMESPACE] [/filter | LABELS] [@CONTEXT].".into());
        }
    }
    Ok(command)
}

pub const SPECIAL: &[&str] = &[
    "actions",
    "discover",
    "workload",
    "workloads",
    "wk",
    "pulse",
    "hz",
    "xrays",
    "screendumps",
    "pf",
    "portforward",
    "portforwards",
    "container",
    "containers",
    "benchmarks",
    "benchmark",
    "be",
    "helm",
    "history",
    "helm-install",
    "helm-crds",
    "logs",
    "scans",
    "scan",
    "pop",
    "popeye",
    "users",
    "groups",
    "rbacs",
    "policies",
    "ctx",
    "context",
    "contexts",
    "ns",
    "namespace",
    "namespaces",
    "alias",
    "aliases",
    "a",
    "pulses",
    "pu",
    "xray",
    "xr",
    "x",
    "screendump",
    "sd",
    "help",
    "h",
    "?",
    "q",
    "q!",
    "qa",
    "Q",
    "quit",
    "exit",
    "can",
    "dir",
    "dirs",
    "d",
    "ls",
    "cow",
    "sort",
    "all",
    "overview",
    "themes",
    "metrics",
    "scale",
    "image",
    "restart",
    "delete",
];

pub fn fuzzy_matches(pattern: &str, haystack: &str) -> bool {
    let haystack = haystack.to_lowercase();
    let mut chars = haystack.chars();
    pattern
        .to_lowercase()
        .chars()
        .all(|needle| chars.by_ref().any(|c| c == needle))
}

pub enum Filter {
    All,
    Regex(regex::Regex, bool),
    Fuzzy(String),
}
impl Filter {
    pub fn parse(text: &str) -> Result<Self> {
        let text = text.trim();
        if text.is_empty() || text.starts_with("-l ") || text == "-l" {
            return Ok(Self::All);
        }
        if let Some(pattern) = text.strip_prefix("-f ") {
            return Ok(Self::Fuzzy(pattern.to_owned()));
        }
        let (pattern, inverse) = text
            .strip_prefix('!')
            .map(|s| (s.trim_start(), true))
            .unwrap_or((text, false));
        regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .size_limit(64 * 1024)
            .dfa_size_limit(128 * 1024)
            .build()
            .map(|regex| Self::Regex(regex, inverse))
            .map_err(|_| "Invalid or excessive regex. Use /-f TEXT for fuzzy search.".into())
    }
    pub fn matches(&self, text: &str) -> bool {
        match self {
            Self::All => true,
            Self::Regex(regex, inverse) => regex.is_match(text) != *inverse,
            Self::Fuzzy(pattern) => fuzzy_matches(pattern, text),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_grammar_keeps_context_case_and_selector_values() {
        let parsed = resource(
            &words(":po team-a 'app in (API,web),tier!=batch' @Prod_EU").unwrap_or_default(),
        )
        .unwrap_or_default();
        assert_eq!(parsed.resource, "po");
        assert_eq!(parsed.namespace.as_deref(), Some("team-a"));
        assert_eq!(parsed.context.as_deref(), Some("Prod_EU"));
        assert_eq!(parsed.selector, "app in (API,web),tier!=batch");
        assert_eq!(
            resource(&words("pod /api|web @prod").unwrap_or_default())
                .unwrap_or_default()
                .filter,
            "api|web"
        );
        assert!(words("pod 'unclosed").is_err());
        assert!(resource(&words("pod one two").unwrap_or_default()).is_err());
        assert!(words(&"x".repeat(COMMAND_BYTES + 1)).is_err());
        assert!(words("pod\nquit").is_err());
    }
    #[test]
    fn fuzzy_search_is_case_insensitive_subsequence() {
        assert!(fuzzy_matches("aPW", "API-production-Worker"));
        assert!(!fuzzy_matches("wpa", "API-production-Worker"));
    }
}
