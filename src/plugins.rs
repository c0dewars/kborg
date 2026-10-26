//! k9s-style plugin semantics for registered extensions: `$VARIABLE`
//! expansion inside single argv elements (never through a shell), scope
//! matching, and shortcut bindings layered over the keymap.
//!
//! Variable values come from cluster data and are untrusted. Each value is
//! checked before it lands in argv: control characters are refused, a value
//! that starts with `-` is refused when it would begin an argv element (an
//! option position) before a literal `--`, and when the registered executable
//! is a shell every value is limited to a small safe character set.
use crate::{
    Result,
    keymap::{Key, Keymap, Scope},
    safety,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// Variables KBorg can supply. `$COL-<TITLE>` adds visible column values.
pub const VARIABLES: &[&str] = &[
    "NAMESPACE",
    "NAME",
    "CONTEXT",
    "CLUSTER",
    "USER",
    "GROUPS",
    "KUBECONFIG",
    "CONTAINER",
    "POD",
    "FILTER",
    "RESOURCE_NAME",
    "RESOURCE_GROUP",
    "RESOURCE_VERSION",
];
const SHELLS: &[&str] = &[
    "sh", "bash", "zsh", "dash", "ksh", "ash", "fish", "csh", "tcsh",
];
const MAX_ARGUMENT: usize = 4096;
pub const MAX_DESCRIPTION: usize = 120;

/// k9s plugin fields kept with a schema 2 registration.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub description: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub short_cut: Option<String>,
    #[serde(default)]
    pub scopes: Vec<String>,
    #[serde(default)]
    pub background: bool,
    #[serde(default)]
    pub confirm: bool,
    #[serde(default)]
    pub dangerous: bool,
    #[serde(default)]
    pub overwrite_output: bool,
}

impl Meta {
    pub fn validate(&self) -> Result<()> {
        if self.description.len() > MAX_DESCRIPTION
            || safety::safe_text(&self.description, MAX_DESCRIPTION) != self.description
        {
            return Err("plugin description must be single-line text up to 120 bytes".into());
        }
        if let Some(short_cut) = &self.short_cut
            && (short_cut.len() > 16 || Key::parse(short_cut).is_none())
        {
            return Err("plugin shortCut is not a key KBorg can parse".into());
        }
        if self.scopes.len() > 16
            || self.scopes.iter().any(|scope| {
                scope.is_empty()
                    || scope.len() > 63
                    || !scope
                        .bytes()
                        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"-.".contains(&b))
            })
        {
            return Err(
                "plugin scopes must be at most 16 lowercase resource names (or all)".into(),
            );
        }
        Ok(())
    }
}

/// Values for one invocation, keyed by variable name (`NAME`, `COL-IMAGE`).
#[derive(Clone, Debug, Default)]
pub struct Variables(BTreeMap<String, String>);

impl Variables {
    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        self.0.insert(name.into(), value.into());
    }
    /// `$COL-<TITLE>`: uppercase, spaces become `_` (`${COL-NOMINATED_NODE}`).
    pub fn column(&mut self, title: &str, value: impl Into<String>) {
        let title = title.trim().to_ascii_uppercase().replace(' ', "_");
        if valid_column(&title) {
            self.0.insert(format!("COL-{title}"), value.into());
        }
    }
    pub fn get(&self, name: &str) -> Option<&str> {
        self.0.get(name).map(String::as_str)
    }
    /// Values every view knows about a selected object of `resource`
    /// (`pods`, `deployments.apps`): namespace, name and resource identity.
    pub fn for_object(object: &serde_json::Value, resource: &str, context: &str) -> Self {
        let mut values = Self::default();
        let metadata = &object["metadata"];
        let name = metadata["name"].as_str().unwrap_or_default();
        values.set(
            "NAMESPACE",
            metadata["namespace"].as_str().unwrap_or_default(),
        );
        values.set("NAME", name);
        values.set("CONTEXT", context);
        let (plural, group) = resource.split_once('.').unwrap_or((resource, ""));
        values.set("RESOURCE_NAME", plural);
        values.set("RESOURCE_GROUP", group);
        let version = object["apiVersion"].as_str().unwrap_or_default();
        values.set(
            "RESOURCE_VERSION",
            version.rsplit('/').next().unwrap_or_default(),
        );
        if plural == "pods" {
            values.set("POD", name);
        }
        // KBorg never impersonates groups; k9s passes an empty value then too.
        values.set("GROUPS", "");
        values
    }
    /// `$CLUSTER`, `$USER` and `$KUBECONFIG` from kubeconfig names, read only
    /// when the plugin references one of them.
    pub fn add_identity(
        &mut self,
        connection: &crate::gateway::Connection,
        context: Option<&str>,
        needed: &BTreeSet<String>,
    ) -> Result<()> {
        if !["CLUSTER", "USER", "KUBECONFIG"]
            .iter()
            .any(|name| needed.contains(*name))
        {
            return Ok(());
        }
        let (name, cluster, user, kubeconfig) =
            crate::gateway::plugin_identity(connection, context)?;
        self.set("CLUSTER", cluster);
        self.set("USER", user);
        self.set("KUBECONFIG", kubeconfig);
        if self.get("CONTEXT").is_none_or(str::is_empty) {
            self.set("CONTEXT", name);
        }
        Ok(())
    }
}

fn valid_column(title: &str) -> bool {
    !title.is_empty()
        && title.len() <= 63
        && title
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b"_-%/".contains(&b))
}
fn known(name: &str) -> bool {
    VARIABLES.contains(&name) || name.strip_prefix("COL-").is_some_and(valid_column)
}

#[derive(Debug, PartialEq, Eq)]
enum Piece {
    Text(String),
    Variable(String),
}

/// Splits one argv element into literal text and variable references.
/// `$NAME` and `${NAME}` are variables; `$$` is a literal `$`; anything
/// else after `$` (including `$(` and lowercase names) stays literal.
fn pieces(argument: &str) -> Result<Vec<Piece>> {
    let mut out = Vec::new();
    let mut text = String::new();
    let mut chars = argument.char_indices().peekable();
    while let Some((_, c)) = chars.next() {
        if c != '$' {
            text.push(c);
            continue;
        }
        match chars.peek().map(|(_, next)| *next) {
            Some('$') => {
                chars.next();
                text.push('$');
            }
            Some('{') => {
                let rest: String = chars.clone().skip(1).map(|(_, c)| c).collect();
                let Some(end) = rest.find('}') else {
                    text.push('$');
                    continue;
                };
                let name = &rest[..end];
                let shaped = name
                    .chars()
                    .next()
                    .is_some_and(|first| first.is_ascii_uppercase());
                if !shaped {
                    text.push('$');
                    continue;
                }
                if !known(name) {
                    return Err(unknown(name));
                }
                // Consume `{`, the name and `}`.
                for _ in 0..name.len() + 2 {
                    chars.next();
                }
                out.push(Piece::Text(std::mem::take(&mut text)));
                out.push(Piece::Variable(name.into()));
            }
            Some(first) if first.is_ascii_uppercase() => {
                let mut name = String::new();
                while let Some((_, next)) = chars.peek() {
                    let column = name.starts_with("COL-");
                    if next.is_ascii_uppercase()
                        || next.is_ascii_digit()
                        || *next == '_'
                        || (*next == '-' && (name == "COL" || column))
                    {
                        name.push(*next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                let mut trailing = String::new();
                while name.len() > 4 && name.starts_with("COL-") && name.ends_with('-') {
                    name.pop();
                    trailing.push('-');
                }
                if !known(&name) {
                    return Err(unknown(&name));
                }
                out.push(Piece::Text(std::mem::take(&mut text)));
                out.push(Piece::Variable(name));
                text.push_str(&trailing);
            }
            _ => text.push('$'),
        }
    }
    out.push(Piece::Text(text));
    out.retain(|piece| !matches!(piece, Piece::Text(text) if text.is_empty()));
    Ok(out)
}

fn unknown(name: &str) -> String {
    format!(
        "unknown plugin variable ${}; KBorg expands only {} and $COL-<COLUMN>, and never reads environment variables",
        safety::safe_text(name, 63),
        VARIABLES
            .iter()
            .map(|name| format!("${name}"))
            .collect::<Vec<_>>()
            .join(" ")
    )
}

/// Variables an argv references; unknown `$UPPERCASE` names are errors.
pub fn referenced(arguments: &[String]) -> Result<BTreeSet<String>> {
    let mut names = BTreeSet::new();
    for argument in arguments {
        for piece in pieces(argument)? {
            if let Piece::Variable(name) = piece {
                names.insert(name);
            }
        }
    }
    Ok(names)
}

pub fn is_shell(executable: &Path) -> bool {
    executable
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| SHELLS.contains(&name))
}

/// Expands every argv element on its own. A variable never splits, globs or
/// joins elements: each element stays exactly one argument.
pub fn expand(executable: &Path, arguments: &[String], values: &Variables) -> Result<Vec<String>> {
    let shell = is_shell(executable);
    let mut options_ended = false;
    let mut out = Vec::with_capacity(arguments.len());
    for argument in arguments {
        let mut expanded = String::new();
        for piece in pieces(argument)? {
            match piece {
                Piece::Text(text) => expanded.push_str(&text),
                Piece::Variable(name) => {
                    let value = values.get(&name).ok_or_else(|| {
                        format!("plugin needs ${name}, which this view does not provide")
                    })?;
                    check(&name, value, expanded.is_empty() && !options_ended, shell)?;
                    expanded.push_str(value);
                }
            }
        }
        if expanded.len() > MAX_ARGUMENT {
            return Err("an expanded plugin argument exceeds 4096 bytes".into());
        }
        if argument == "--" {
            options_ended = true;
        }
        out.push(expanded);
    }
    Ok(out)
}

fn check(name: &str, value: &str, option_position: bool, shell: bool) -> Result<()> {
    if value.len() > MAX_ARGUMENT
        || value.chars().any(char::is_control)
        || safety::safe_text(value, MAX_ARGUMENT) != value
    {
        return Err(format!(
            "refused: the value of ${name} contains control or formatting characters"
        ));
    }
    if value.starts_with('-') && (option_position || shell) {
        return Err(format!(
            "refused: the value of ${name} starts with '-' and would be read as an option"
        ));
    }
    if shell
        && !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._:/@=+,-".contains(&b))
    {
        return Err(format!(
            "refused: the value of ${name} has characters a shell would interpret; shell plugins accept only letters, digits and ._:/@=+,-"
        ));
    }
    Ok(())
}

/// Why an invocation needs the stronger `RUN DANGEROUS` confirmation, if it
/// does. This adds friction for likely writes; it never makes a tool safe.
pub fn danger(meta: &Meta, executable: &Path, argv: &[String]) -> Option<&'static str> {
    if meta.dangerous {
        return Some("the plugin is marked dangerous");
    }
    if is_shell(executable) {
        return Some("the plugin runs a shell script");
    }
    let tool = executable.file_name()?.to_str()?;
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    let kubectl = [
        "apply",
        "create",
        "delete",
        "edit",
        "patch",
        "replace",
        "scale",
        "set",
        "label",
        "annotate",
        "taint",
        "cordon",
        "uncordon",
        "drain",
        "exec",
        "attach",
        "debug",
        "cp",
        "port-forward",
        "run",
        "expose",
        "autoscale",
    ];
    let helm = [
        "install",
        "upgrade",
        "uninstall",
        "delete",
        "rollback",
        "test",
        "push",
    ];
    let writes = match tool {
        "kubectl" | "oc" => {
            words.iter().any(|word| kubectl.contains(word))
                || words.windows(2).any(|pair| {
                    pair[0] == "rollout"
                        && ["restart", "undo", "pause", "resume"].contains(&pair[1])
                })
        }
        "helm" => words.iter().any(|word| helm.contains(word)),
        _ => false,
    };
    writes.then_some("the arguments contain a kubectl/helm verb that changes the cluster")
}

/// Whether a plugin applies to a view. `names` holds the view's resource
/// names (plural, group-qualified, kind and short names) or `containers` /
/// `helm`. No scopes, or `all`, matches every view.
pub fn applies(scopes: &[String], names: &[String]) -> bool {
    scopes.is_empty()
        || scopes.iter().any(|scope| {
            scope == "all"
                || names.iter().any(|name| {
                    name == scope
                        || name
                            .strip_suffix('s')
                            .is_some_and(|singular| singular == scope)
                })
        })
}

/// A plugin shortcut bound in one view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub id: String,
    pub key: Key,
    pub scope: Scope,
}

/// Registered plugins and their key bindings for this session.
#[derive(Clone, Debug, Default)]
pub struct Catalog {
    pub entries: Vec<(String, Meta)>,
    pub bindings: Vec<Binding>,
    /// Shortcuts that could not be bound, with the reason.
    pub notices: Vec<String>,
    pub error: Option<String>,
    /// Registry directory override (tests); `None` uses the config folder.
    pub directory: Option<std::path::PathBuf>,
}

fn views(meta: &Meta) -> Vec<Scope> {
    let all = meta.scopes.is_empty() || meta.scopes.iter().any(|scope| scope == "all");
    let mut out = Vec::new();
    if all
        || meta
            .scopes
            .iter()
            .any(|scope| scope != "containers" && scope != "helm")
    {
        out.push(Scope::Table);
    }
    if all || meta.scopes.iter().any(|scope| scope == "containers") {
        out.push(Scope::Containers);
    }
    if all || meta.scopes.iter().any(|scope| scope == "helm") {
        out.push(Scope::Helm);
    }
    out
}

/// The registered action that already uses `key` in `scope`, if any.
fn taken(keymap: &Keymap, scope: Scope, key: Key) -> Option<&'static str> {
    crate::keymap::ACTIONS
        .iter()
        .find(|action| scope.layers().contains(&action.scope) && keymap.matches(action.name, key))
        .map(|action| action.name)
}

impl Catalog {
    /// Binds each plugin `shortCut` in its views when the key is free there:
    /// reserved keys, keys of any registered action in that view (after
    /// keymap.yaml), hotkeys.yaml keys and earlier plugins win.
    pub fn new(
        entries: Vec<(String, Meta)>,
        keymap: &Keymap,
        hotkeys: &crate::hotkeys::Hotkeys,
    ) -> Self {
        let mut bindings: Vec<Binding> = Vec::new();
        let mut notices = Vec::new();
        for (id, meta) in &entries {
            let Some(text) = &meta.short_cut else {
                continue;
            };
            let Some(key) = Key::parse(text) else {
                notices.push(format!("{id}: shortCut {text} is not a key KBorg can bind"));
                continue;
            };
            if key.reserved() {
                notices.push(format!("{id}: {key} is reserved and never rebound"));
                continue;
            }
            for scope in views(meta) {
                let (code, modifiers) = key.event();
                let conflict = taken(keymap, scope, key)
                    .map(str::to_owned)
                    .or_else(|| {
                        (scope == Scope::Table)
                            .then(|| crate::hotkeys::find(hotkeys, code, modifiers))
                            .flatten()
                            .map(|hotkey| format!("hotkey :{}", hotkey.command))
                    })
                    .or_else(|| {
                        bindings
                            .iter()
                            .find(|binding| binding.scope == scope && binding.key == key)
                            .map(|binding| format!("plugin {}", binding.id))
                    });
                match conflict {
                    Some(owner) => notices.push(format!(
                        "{id}: {key} is taken by {owner} in the {} view; use :ext {id}",
                        scope.name()
                    )),
                    None => bindings.push(Binding {
                        id: id.clone(),
                        key,
                        scope,
                    }),
                }
            }
        }
        Self {
            entries,
            bindings,
            notices,
            error: None,
            directory: None,
        }
    }
    /// Registered plugins from the private registry (no executable is read).
    pub fn load(keymap: &Keymap, hotkeys: &crate::hotkeys::Hotkeys) -> Self {
        match crate::extensions::list() {
            Ok(entries) => Self::new(
                entries
                    .into_iter()
                    .filter_map(|entry| entry.plugin.map(|meta| (entry.id, meta)))
                    .collect(),
                keymap,
                hotkeys,
            ),
            Err(error) => Self {
                error: Some(error),
                ..Self::default()
            },
        }
    }
    pub fn meta(&self, id: &str) -> Option<&Meta> {
        self.entries
            .iter()
            .find(|(entry, _)| entry == id)
            .map(|(_, meta)| meta)
    }
    pub fn key_for(&self, id: &str, scope: Scope) -> Option<Key> {
        self.bindings
            .iter()
            .find(|binding| binding.id == id && binding.scope == scope)
            .map(|binding| binding.key)
    }
    pub fn find(&self, scope: Scope, key: Key) -> Option<&str> {
        self.bindings
            .iter()
            .find(|binding| binding.scope == scope && binding.key == key)
            .map(|binding| binding.id.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values() -> Variables {
        let mut values = Variables::default();
        values.set("NAMESPACE", "shop");
        values.set("NAME", "api-7d9");
        values.set("CONTEXT", "docker-desktop");
        values.set("FILTER", "");
        values.column("IMAGE", "nginx:1.27");
        values.column("nominated node", "n1");
        values
    }
    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn variables_expand_inside_one_argv_element_without_splitting() -> Result<()> {
        let argv = expand(
            Path::new("/usr/bin/stern"),
            &args(&[
                "--tail",
                "50",
                "$FILTER",
                "-n",
                "$NAMESPACE",
                "--context=${CONTEXT}",
                "$NAMESPACE/$NAME",
                "${COL-IMAGE}",
                "$COL-NOMINATED_NODE-x",
                "cost: $$5 $(id) ${lower} $1",
            ]),
            &values(),
        )?;
        assert_eq!(
            argv,
            args(&[
                "--tail",
                "50",
                "",
                "-n",
                "shop",
                "--context=docker-desktop",
                "shop/api-7d9",
                "nginx:1.27",
                "n1-x",
                "cost: $5 $(id) ${lower} $1",
            ])
        );
        let mut spaced = values();
        spaced.set("NAME", "two words; rm -rf ~");
        assert_eq!(
            expand(Path::new("/usr/bin/echo"), &args(&["$NAME"]), &spaced)?,
            args(&["two words; rm -rf ~"]),
            "one element, no shell, no splitting"
        );
        Ok(())
    }

    #[test]
    fn untrusted_values_cannot_inject_options_controls_or_shell_syntax() -> Result<()> {
        let tool = Path::new("/usr/local/bin/kubectl");
        let mut hostile = values();
        hostile.set("NAME", "--exec=/bin/sh");
        assert!(expand(tool, &args(&["logs", "$NAME"]), &hostile).is_err());
        assert!(expand(tool, &args(&["logs", "${NAME}"]), &hostile).is_err());
        // Embedded after a literal prefix it cannot become an option.
        assert_eq!(
            expand(tool, &args(&["--selector=app=$NAME"]), &hostile)?,
            args(&["--selector=app=--exec=/bin/sh"])
        );
        // After a literal `--`, positional values may start with '-'.
        assert_eq!(
            expand(tool, &args(&["get", "--", "$NAME"]), &hostile)?,
            args(&["get", "--", "--exec=/bin/sh"])
        );
        for bad in ["a\nb", "a\rb", "a\u{0}b", "a\u{1b}[2Jb", "a\u{202e}b"] {
            let mut values = values();
            values.set("NAME", bad);
            assert!(
                expand(tool, &args(&["x=$NAME"]), &values).is_err(),
                "{bad:?}"
            );
        }
        // Shells only take a safe character set, never a leading '-'.
        let shell = Path::new("/bin/bash");
        let mut values = values();
        values.set("NAME", "a$(touch x)");
        assert!(expand(shell, &args(&["-c", "kubectl logs $NAME"]), &values).is_err());
        values.set("NAME", "x;id");
        assert!(expand(shell, &args(&["-c", "kubectl logs $NAME"]), &values).is_err());
        values.set("NAME", "api-1.prod");
        assert_eq!(
            expand(shell, &args(&["-c", "kubectl logs $NAME"]), &values)?,
            args(&["-c", "kubectl logs api-1.prod"])
        );
        // Missing and unknown variables are explicit errors.
        assert!(expand(tool, &args(&["$CONTAINER"]), &values).is_err());
        assert!(referenced(&args(&["$HOME"])).is_err());
        assert!(referenced(&args(&["${SECRET_TOKEN}"])).is_err());
        assert_eq!(
            referenced(&args(&["$NAME", "${COL-IMAGE}", "$(x)"]))?,
            ["COL-IMAGE", "NAME"].map(String::from).into()
        );
        Ok(())
    }

    #[test]
    fn danger_scope_and_bindings_follow_keymap_rules() -> Result<()> {
        let meta = Meta::default();
        let kubectl = Path::new("/usr/local/bin/kubectl");
        assert!(danger(&meta, kubectl, &args(&["logs", "x"])).is_none());
        assert!(danger(&meta, kubectl, &args(&["delete", "pod", "x"])).is_some());
        assert!(danger(&meta, kubectl, &args(&["rollout", "restart", "x"])).is_some());
        assert!(danger(&meta, Path::new("/bin/sh"), &[]).is_some());
        let marked = Meta {
            dangerous: true,
            ..Meta::default()
        };
        assert!(danger(&marked, Path::new("/usr/bin/dive"), &[]).is_some());

        let pods = args(&["pods", "v1/pods", "pod", "po"]);
        assert!(applies(&args(&["pods"]), &pods));
        assert!(applies(&args(&["po"]), &pods));
        assert!(applies(&args(&["pod"]), &pods));
        assert!(applies(&args(&["all"]), &pods));
        assert!(applies(&[], &pods));
        assert!(!applies(&args(&["deployments"]), &pods));

        let plugin = |key: &str, scopes: &[&str]| Meta {
            short_cut: Some(key.into()),
            scopes: args(scopes),
            ..Meta::default()
        };
        let catalog = Catalog::new(
            vec![
                ("stern".into(), plugin("Ctrl-L", &["pods"])),
                ("dive".into(), plugin("d", &["containers"])),
                ("values".into(), plugin("v", &["helm"])),
                ("again".into(), plugin("Ctrl-L", &["po"])),
                ("enter".into(), plugin("Enter", &["pods"])),
                ("free".into(), plugin("Shift-X", &["containers"])),
            ],
            &Keymap::default(),
            &Vec::new(),
        );
        assert_eq!(catalog.find(Scope::Table, Key::Ctrl('l')), Some("stern"));
        assert_eq!(
            catalog.find(Scope::Containers, Key::Char('X')),
            Some("free")
        );
        assert!(catalog.find(Scope::Containers, Key::Char('d')).is_none());
        assert!(catalog.find(Scope::Helm, Key::Char('v')).is_none());
        let notices = catalog.notices.join("\n");
        assert!(notices.contains("dive: d is taken by containers.describe"));
        assert!(notices.contains("values: v is taken by helm.values"));
        assert!(notices.contains("again: Ctrl-L is taken by plugin stern"));
        assert!(notices.contains("enter: Enter is reserved"));
        // A keymap.yaml remap that frees `d` lets the plugin bind it.
        let keymap = crate::keymap::parse("keys:\n  containers.describe: D\n")?;
        let catalog = Catalog::new(
            vec![("dive".into(), plugin("d", &["containers"]))],
            &keymap,
            &Vec::new(),
        );
        assert_eq!(
            catalog.find(Scope::Containers, Key::Char('d')),
            Some("dive")
        );
        Ok(())
    }
}
