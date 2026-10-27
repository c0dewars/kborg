//! Per-context settings, like k9s `clusters/CONTEXT/config.yaml`: a data-only
//! `contexts/<context>.yaml` in the KBorg config folder with a default
//! namespace and view, a theme, a header badge, and restrictions. The
//! restrictions (`read_only`, `strict_confirmations`) only ever tighten what
//! the command line and the operator `--config` allow. A file that exists but
//! cannot be trusted or parsed makes its context read-only until it is fixed.
use crate::{Result, safety, themes};
use serde::Deserialize;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

pub const DIR_NAME: &str = "contexts";
const MAX_BYTES: usize = 4 * 1024;
/// Longest encoded file stem before it is shortened with a digest.
const MAX_STEM: usize = 120;
pub const MAX_LABEL: usize = 16;

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ContextConfig {
    pub version: u32,
    /// Default namespace for this context (`all` or `*` for every namespace).
    pub namespace: Option<String>,
    /// Default resource on entering the context, for example `deployments`.
    pub view: Option<String>,
    /// Theme id used while this context is selected.
    pub theme: Option<String>,
    /// Browse only: changes are off for this context.
    pub read_only: bool,
    /// Every change in this context needs its typed confirmation phrase.
    pub strict_confirmations: bool,
    /// Header badge text, for example `PROD`.
    pub label: Option<String>,
    /// Badge color: a palette role (`danger`, `warning`, ...) or `#RRGGBB`.
    pub color: Option<String>,
}
impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            version: 1,
            namespace: None,
            view: None,
            theme: None,
            read_only: false,
            strict_confirmations: false,
            label: None,
            color: None,
        }
    }
}

/// A resolved header badge.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Badge {
    pub label: String,
    pub color: String,
}
impl Badge {
    /// Background color for the badge in `theme`.
    pub fn background(&self, theme: &themes::Theme) -> themes::Rgb {
        themes::Rgb::parse(&self.color).unwrap_or_else(|_| theme.get(&self.color))
    }
    /// Black or white, whichever reads better on `background`.
    pub fn foreground(background: themes::Rgb) -> themes::Rgb {
        let white = themes::Rgb(255, 255, 255);
        let black = themes::Rgb(0, 0, 0);
        if themes::contrast(white, background) >= themes::contrast(black, background) {
            white
        } else {
            black
        }
    }
}

impl ContextConfig {
    pub fn badge(&self) -> Option<Badge> {
        self.label.as_ref().map(|label| Badge {
            label: label.clone(),
            color: self.color.clone().unwrap_or_else(|| "accent".into()),
        })
    }
    /// The namespace as KBorg stores it (`*` for all namespaces).
    pub fn namespace(&self) -> Option<String> {
        self.namespace
            .as_ref()
            .map(|ns| if ns == "all" { "*".into() } else { ns.clone() })
    }
    fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err("version must be 1".into());
        }
        if let Some(ns) = &self.namespace
            && ns != "all"
            && ns != "*"
            && !safety::valid_id(ns)
        {
            return Err("namespace must be a namespace name, all or *".into());
        }
        if let Some(view) = &self.view
            && (view.is_empty()
                || view.len() > 253
                || view
                    .split('/')
                    .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with('.'))
                || !view
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'/')))
        {
            return Err(
                "view must be a resource such as deployments, dp or apps/v1/deployments".into(),
            );
        }
        if let Some(theme) = &self.theme
            && !safety::valid_id(theme)
        {
            return Err("theme must be a theme id such as nord or flexoki-dark".into());
        }
        if let Some(label) = &self.label
            && (label.trim().is_empty()
                || label.trim() != label
                || label.len() > MAX_LABEL
                || !label.bytes().all(|b| b.is_ascii_graphic() || b == b' '))
        {
            return Err(format!(
                "label must be 1..{MAX_LABEL} printable ASCII characters, such as PROD"
            ));
        }
        if let Some(color) = &self.color {
            if self.label.is_none() {
                return Err("color needs a label to color".into());
            }
            if !crate::ui_config::ROLES.contains(&color.as_str())
                && themes::Rgb::parse(color).is_err()
            {
                return Err(format!(
                    "color must be #RRGGBB or a palette role: {}",
                    crate::ui_config::ROLES.join(", ")
                ));
            }
        }
        Ok(())
    }
    /// One line for `kborg contexts` and notices; empty for an empty file.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(badge) = self.badge() {
            parts.push(format!("badge [{}] {}", badge.label, badge.color));
        }
        if self.read_only {
            parts.push("read-only".into());
        }
        if self.strict_confirmations {
            parts.push("strict confirmations".into());
        }
        if let Some(theme) = &self.theme {
            parts.push(format!("theme {theme}"));
        }
        if let Some(ns) = &self.namespace {
            parts.push(format!("namespace {ns}"));
        }
        if let Some(view) = &self.view {
            parts.push(format!("view {view}"));
        }
        parts.join(", ")
    }
}

pub fn parse(text: &str) -> Result<ContextConfig> {
    if text.len() > MAX_BYTES {
        return Err("context settings exceed 4 KiB".into());
    }
    if text
        .lines()
        .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
    {
        return Ok(ContextConfig::default());
    }
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_reader_input_bytes: Some(MAX_BYTES), max_events: 200, max_aliases: 0, max_anchors: 0,
            max_depth: 2, max_inclusion_depth: 0, max_documents: 1, max_nodes: 64,
            max_total_scalar_bytes: MAX_BYTES, max_merge_keys: 0,
        },
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys: serde_saphyr::MergeKeyPolicy::Error,
        with_snippet: false,
    };
    let config: ContextConfig = serde_saphyr::from_str_with_options(text, options).map_err(|_| {
        "invalid context settings; unknown keys or wrong value types are rejected (keys: version, namespace, view, theme, read_only, strict_confirmations, label, color)"
    })?;
    config.validate()?;
    Ok(config)
}

/// Safe file name for a context. Letters, digits, `-` and `_` stay; every
/// other byte (slashes, dots, colons, Unicode, ...) becomes `%XX`, so no name
/// can leave the folder or hide as a dot file. Windows device names are
/// escaped and very long names keep a prefix plus a digest of the full name.
pub fn file_name(context: &str) -> Result<String> {
    if context.is_empty() || context.len() > 253 {
        return Err("context name must be 1..253 bytes".into());
    }
    let mut stem = String::with_capacity(context.len());
    for byte in context.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_' {
            stem.push(char::from(byte));
        } else {
            let _ = write!(stem, "%{byte:02X}");
        }
    }
    let upper = stem.to_ascii_uppercase();
    let device = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((upper.starts_with("COM") || upper.starts_with("LPT"))
            && upper.len() == 4
            && upper.as_bytes()[3].is_ascii_digit());
    if device {
        stem = format!("%{:02X}{}", stem.as_bytes()[0], &stem[1..]);
    }
    if stem.len() > MAX_STEM {
        let digest = ring::digest::digest(&ring::digest::SHA256, context.as_bytes());
        let mut cut = MAX_STEM - 26;
        // Never split a %XX escape.
        let bytes = stem.as_bytes();
        if bytes[cut - 1] == b'%' {
            cut -= 1;
        } else if bytes[cut - 2] == b'%' {
            cut -= 2;
        }
        let mut short = stem[..cut].to_owned();
        short.push('~');
        for byte in &digest.as_ref()[..12] {
            let _ = write!(short, "{byte:02x}");
        }
        stem = short;
    }
    Ok(format!("{stem}.yaml"))
}

pub fn directory() -> Result<PathBuf> {
    Ok(safety::config_dir()?.join(DIR_NAME))
}

pub fn path_in(directory: &Path, context: &str) -> Result<PathBuf> {
    Ok(directory.join(file_name(context)?))
}

/// Refuse files or folders others could rewrite to drop a restriction.
fn check_private(metadata: &std::fs::Metadata, what: &str) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.permissions().mode() & 0o022 != 0 {
            return Err(format!(
                "{what} must not be writable by group or others (chmod go-w)"
            ));
        }
        if metadata.is_file() && metadata.nlink() != 1 {
            return Err(format!("{what} must not be a hard link"));
        }
    }
    #[cfg(not(unix))]
    let _ = (metadata, what);
    Ok(())
}

/// The settings for `context`, `None` when it has no file. An error means the
/// file exists but is unsafe or invalid; callers treat that context as
/// read-only.
pub fn load_in(directory: &Path, context: &str) -> Result<Option<ContextConfig>> {
    let path = path_in(directory, context)?;
    safety::checked_path(&path)?;
    let dir = match std::fs::symlink_metadata(directory) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cannot inspect the contexts folder".into()),
    };
    if !dir.is_dir() {
        return Err("contexts must be a folder".into());
    }
    check_private(&dir, "the contexts folder")?;
    let meta = match std::fs::symlink_metadata(&path) {
        Ok(meta) => meta,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err("cannot inspect the context settings file".into()),
    };
    if !meta.is_file() {
        return Err("context settings must be a regular file".into());
    }
    check_private(&meta, "the context settings file")?;
    let bytes = safety::read_small(&path, MAX_BYTES as u64)?;
    parse(std::str::from_utf8(&bytes).map_err(|_| "context settings must be UTF-8")?).map(Some)
}

/// The installed settings for `context` (see `load_in`).
pub fn load(context: &str) -> Result<Option<ContextConfig>> {
    load_in(&directory()?, context)
}

/// The commented starting file for a new context.
pub fn template(context: &str) -> String {
    format!(
        "# KBorg settings for context {}\n\
         # Data only; unknown keys are rejected. Applied on every switch to this\n\
         # context. read_only and strict_confirmations only tighten what KBorg\n\
         # allows; nothing here can turn a capability on. docs/themes.md has more.\n\
         version: 1\n\
         # namespace: default          # default namespace (all = every namespace)\n\
         # view: deployments           # resource shown when entering the context\n\
         # theme: flexoki-dark         # theme while this context is selected\n\
         # read_only: true             # browse only: no changes in this context\n\
         # strict_confirmations: true  # typed confirmation phrase for every change\n\
         # label: PROD                 # header badge\n\
         # color: danger               # badge color: palette role or #RRGGBB\n",
        safety::safe_text(context, 253)
    )
}

/// Current text of the file for `context`, or the template when none exists.
pub fn text_in(directory: &Path, context: &str) -> Result<String> {
    let path = path_in(directory, context)?;
    safety::checked_path(&path)?;
    if !path.exists() {
        return Ok(template(context));
    }
    let bytes = safety::read_small(&path, MAX_BYTES as u64)?;
    String::from_utf8(bytes).map_err(|_| "context settings must be UTF-8".into())
}

/// Validate `text` and store it for `context` (private folder, 0600 file,
/// atomic replace). Invalid text is refused and the file is left unchanged.
pub fn save_in(directory: &Path, context: &str, text: &str) -> Result<ContextConfig> {
    let config = parse(text)?;
    safety::ensure_private_dir(directory)?;
    let path = path_in(directory, context)?;
    safety::atomic_write(&path, text.as_bytes())?;
    Ok(config)
}

/// What a context's file does to a one-shot CLI command: read-only and strict
/// confirmations only. A refused file makes the command read-only. Returns a
/// warning to print when the file was refused.
pub fn restrict_in(
    directory: &Path,
    context: &str,
    writable: &mut bool,
    strict: &mut bool,
) -> Option<String> {
    match load_in(directory, context) {
        Ok(Some(config)) => {
            *writable &= !config.read_only;
            *strict |= config.strict_confirmations;
            None
        }
        Ok(None) => None,
        Err(error) => {
            *writable = false;
            Some(format!(
                "contexts/{} refused ({error}); running read-only. Next: fix it with :ctx-config or your editor.",
                file_name(context)
                    .map(|name| safety::safe_text(&name, 80))
                    .unwrap_or_default()
            ))
        }
    }
}

/// `kborg contexts` text: the name, plus the settings summary when the
/// context has a file.
pub fn describe_in(directory: &Path, context: &str) -> String {
    let name = safety::safe_text(context, 100);
    match load_in(directory, context) {
        Ok(Some(config)) => {
            let summary = config.summary();
            format!(
                "{name}  [config: {}]",
                if summary.is_empty() {
                    "defaults".into()
                } else {
                    summary
                }
            )
        }
        Ok(None) => name,
        Err(error) => format!(
            "{name}  [config refused, read-only: {}]",
            safety::safe_text(&error, 120)
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp(name: &str) -> Result<PathBuf> {
        let root = std::env::temp_dir()
            .canonicalize()
            .map_err(|_| "temp")?
            .join(format!("kborg-ctx-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        Ok(root)
    }

    #[test]
    fn parses_every_field_and_rejects_unknown_or_bad_values() -> Result<()> {
        let config = parse(
            "version: 1\nnamespace: payments\nview: deployments\ntheme: nord\nread_only: true\nstrict_confirmations: true\nlabel: PROD\ncolor: danger\n",
        )?;
        assert_eq!(config.namespace.as_deref(), Some("payments"));
        assert_eq!(config.view.as_deref(), Some("deployments"));
        assert_eq!(config.theme.as_deref(), Some("nord"));
        assert!(config.read_only && config.strict_confirmations);
        assert_eq!(
            config.badge(),
            Some(Badge {
                label: "PROD".into(),
                color: "danger".into()
            })
        );
        assert_eq!(parse("# nothing yet\n\n")?, ContextConfig::default());
        assert_eq!(parse("namespace: all")?.namespace().as_deref(), Some("*"));
        assert_eq!(
            parse("label: STAGE\ncolor: '#FFAA00'")?
                .badge()
                .map(|b| b.color),
            Some("#FFAA00".into())
        );
        assert_eq!(
            parse("label: dev")?.badge().map(|b| b.color),
            Some("accent".into())
        );
        for bad in [
            "command: sh",
            "version: 2",
            "read_only: yes please",
            "read_only: true\nread_only: false",
            "namespace: Bad_NS",
            "view: ../secrets",
            "view: 'pods; rm'",
            "theme: ../../etc",
            "label: \"\\e[2JPROD\"",
            "label: WAY-TOO-LONG-FOR-A-BADGE",
            "label: ' PROD'",
            "color: danger",
            "label: P\ncolor: bright-red",
            "namespace: [a, b]",
            "a: &x 1\nb: *x",
        ] {
            assert!(parse(bad).is_err(), "{bad:?} must be rejected");
        }
        assert!(parse(&format!("# {}\n", "x".repeat(MAX_BYTES))).is_err());
        Ok(())
    }

    #[test]
    fn hostile_context_names_encode_to_one_safe_file_name() -> Result<()> {
        let cases = [
            ("docker-desktop", "docker-desktop.yaml"),
            ("prod_eu", "prod_eu.yaml"),
            (
                "arn:aws:eks:us-east-1:1:cluster/prod",
                "arn%3Aaws%3Aeks%3Aus-east-1%3A1%3Acluster%2Fprod.yaml",
            ),
            ("../../etc/passwd", "%2E%2E%2F%2E%2E%2Fetc%2Fpasswd.yaml"),
            ("..", "%2E%2E.yaml"),
            (".hidden", "%2Ehidden.yaml"),
            ("a\\b", "a%5Cb.yaml"),
            ("100%", "100%25.yaml"),
            ("本番", "%E6%9C%AC%E7%95%AA.yaml"),
            ("con", "%63on.yaml"),
            ("LPT1", "%4CPT1.yaml"),
            ("a\u{0}b", "a%00b.yaml"),
        ];
        for (context, expected) in cases {
            let name = file_name(context)?;
            assert_eq!(name, expected, "{context:?}");
            assert!(!name.contains('/') && !name.contains('\\') && !name.starts_with('.'));
            let path = Path::new("/kborg/contexts").join(&name);
            assert_eq!(path.parent(), Some(Path::new("/kborg/contexts")));
        }
        assert!(file_name("").is_err());
        assert!(file_name(&"x".repeat(254)).is_err());
        let long = format!("arn:aws:eks:{}", "/".repeat(200));
        let short = file_name(&long)?;
        assert!(
            short.len() <= MAX_STEM + 5 && short.contains('~'),
            "{short}"
        );
        assert_ne!(short, file_name(&format!("{long}x"))?);
        let wide = "本".repeat(80);
        assert!(file_name(&wide)?.len() <= MAX_STEM + 5);
        Ok(())
    }

    #[test]
    fn load_save_and_fail_closed_on_untrusted_files() -> Result<()> {
        let dir = temp("load")?;
        assert_eq!(load_in(&dir, "prod/eu")?, None);
        assert!(text_in(&dir, "prod/eu")?.starts_with("# KBorg settings for context prod/eu"));
        assert!(parse(&template("x"))? == ContextConfig::default());
        assert!(save_in(&dir, "prod/eu", "label: [x]").is_err());
        assert!(!path_in(&dir, "prod/eu")?.exists());
        save_in(
            &dir,
            "prod/eu",
            "read_only: true\nlabel: PROD\ncolor: danger\n",
        )?;
        let loaded = load_in(&dir, "prod/eu")?.ok_or("saved settings")?;
        assert!(loaded.read_only);
        assert_eq!(load_in(&dir, "prod")?, None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let path = path_in(&dir, "prod/eu")?;
            let mode = |p: &Path| {
                std::fs::metadata(p)
                    .map(|m| m.permissions().mode() & 0o777)
                    .unwrap_or(0)
            };
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(&dir), 0o700);
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o666))
                .map_err(|_| "chmod")?;
            assert!(load_in(&dir, "prod/eu").is_err());
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644))
                .map_err(|_| "chmod")?;
            assert!(load_in(&dir, "prod/eu")?.is_some());
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o777))
                .map_err(|_| "chmod")?;
            assert!(load_in(&dir, "prod/eu").is_err());
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))
                .map_err(|_| "chmod")?;
            let target = dir.join("elsewhere.yaml");
            std::fs::write(&target, "label: X\n").map_err(|_| "write")?;
            std::os::unix::fs::symlink(&target, path_in(&dir, "linked")?).map_err(|_| "symlink")?;
            assert!(load_in(&dir, "linked").is_err());
            std::fs::hard_link(&target, path_in(&dir, "hard")?).map_err(|_| "link")?;
            assert!(load_in(&dir, "hard").is_err());
        }
        std::fs::write(path_in(&dir, "broken")?, "read_only: maybe\n").map_err(|_| "write")?;
        assert!(load_in(&dir, "broken").is_err());
        // One-shot CLI commands: tighten only, fail closed.
        let (mut writable, mut strict) = (true, false);
        assert!(restrict_in(&dir, "prod/eu", &mut writable, &mut strict).is_none());
        assert!(!writable && !strict);
        let (mut writable, mut strict) = (true, false);
        assert!(restrict_in(&dir, "broken", &mut writable, &mut strict).is_some());
        assert!(!writable);
        save_in(&dir, "dev", "strict_confirmations: true\n")?;
        let (mut writable, mut strict) = (false, false);
        assert!(restrict_in(&dir, "dev", &mut writable, &mut strict).is_none());
        assert!(!writable && strict, "a context file never turns writes on");
        assert_eq!(describe_in(&dir, "none"), "none");
        assert_eq!(
            describe_in(&dir, "prod/eu"),
            "prod/eu  [config: badge [PROD] danger, read-only]"
        );
        assert!(describe_in(&dir, "broken").contains("refused, read-only"));
        let _ = std::fs::remove_dir_all(&dir);
        Ok(())
    }
}
