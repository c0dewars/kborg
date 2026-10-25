//! Data-only `ui.yaml`: borders, chrome rows, table style, dashboard cards and
//! per-role color overrides on top of the selected theme. Nothing is
//! evaluated; every value is bounded, unknown keys are rejected and a missing
//! file reproduces the built-in look exactly.
use crate::{Result, safety, themes};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;

const MAX_BYTES: usize = 16 * 1024;
const FILE_NAME: &str = "ui.yaml";

/// Palette roles a ui.yaml may override or name; the same roles themes define.
pub const ROLES: [&str; 12] = [
    "background",
    "text",
    "muted",
    "accent",
    "success",
    "warning",
    "danger",
    "focus",
    "selection_background",
    "selection_text",
    "status_background",
    "status_text",
];

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Borders {
    /// Rounded box glyphs in UTF-8 locales, ASCII otherwise (the built-in look).
    #[default]
    Auto,
    Rounded,
    Square,
    Ascii,
    None,
}
impl Borders {
    /// Corner and edge glyphs: top-left, top-right, bottom-left, bottom-right,
    /// horizontal, vertical. Only these trusted single-cell glyphs bypass the
    /// ASCII display policy for data.
    pub fn glyphs(self) -> [&'static str; 6] {
        let rounded = ["╭", "╮", "╰", "╯", "─", "│"];
        let ascii = ["+", "+", "+", "+", "-", "|"];
        match self {
            Self::Auto if utf8_locale() => rounded,
            Self::Auto | Self::Ascii => ascii,
            Self::Rounded => rounded,
            Self::Square => ["┌", "┐", "└", "┘", "─", "│"],
            Self::None => [" "; 6],
        }
    }
}
fn utf8_locale() -> bool {
    ["LC_ALL", "LC_CTYPE", "LANG"]
        .iter()
        .filter_map(|key| std::env::var(key).ok())
        .find(|value| !value.is_empty())
        .is_some_and(|value| {
            let value = value.to_ascii_lowercase();
            value.contains("utf-8") || value.contains("utf8")
        })
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TitleStyle {
    /// ` > TITLE ` on focused panels, `   TITLE ` otherwise (built-in).
    #[default]
    Marker,
    /// ` TITLE `
    Plain,
    /// `[ TITLE ]`
    Bracketed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Density {
    #[default]
    Normal,
    /// One blank line between table rows.
    Relaxed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgeFormat {
    /// Largest unit only: `14d`.
    #[default]
    Short,
    /// Two units: `14d3h`.
    Long,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Timestamps {
    #[default]
    Utc,
    Local,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Header {
    /// The top bar with the KBORG badge and view title.
    pub show: bool,
    /// Context / namespace / API server line. When false it is folded into the
    /// header bar, so context, namespace and mode always remain visible.
    pub identity: bool,
    /// Freshness, counts, health and sort summary bar.
    pub summary: bool,
    /// Fold the identity line into the header bar and drop the spare row above
    /// the key hints: two more table rows.
    pub compact: bool,
}
impl Default for Header {
    fn default() -> Self {
        Self {
            show: true,
            identity: true,
            summary: true,
            compact: false,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Hints {
    pub show: bool,
    pub key_role: String,
    pub text_role: String,
}
impl Default for Hints {
    fn default() -> Self {
        Self {
            show: true,
            key_role: "accent".into(),
            text_role: "muted".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Table {
    pub density: Density,
    /// One printable ASCII character, or "" for none.
    pub selection_marker: String,
    /// One printable ASCII character, or "" for none.
    pub mark_marker: String,
    pub zebra: bool,
    /// Exactly one printable ASCII character between columns.
    pub column_separator: String,
    pub age: AgeFormat,
    pub sort_ascending: String,
    pub sort_descending: String,
}
impl Default for Table {
    fn default() -> Self {
        Self {
            density: Density::Normal,
            selection_marker: ">".into(),
            mark_marker: "*".into(),
            zebra: false,
            column_separator: " ".into(),
            age: AgeFormat::Short,
            sort_ascending: "^".into(),
            sort_descending: "v".into(),
        }
    }
}
impl Table {
    pub fn separator(&self) -> char {
        self.column_separator.chars().next().unwrap_or(' ')
    }
    pub fn selection(&self) -> char {
        self.selection_marker.chars().next().unwrap_or(' ')
    }
    pub fn mark(&self) -> char {
        self.mark_marker.chars().next().unwrap_or(' ')
    }
    pub fn long_age(&self) -> bool {
        self.age == AgeFormat::Long
    }
    pub fn step(&self) -> u16 {
        match self.density {
            Density::Normal => 1,
            Density::Relaxed => 2,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Dashboard {
    /// 0 chooses 1..3 columns from the terminal width (built-in).
    pub columns: u8,
    /// 0 keeps the built-in width breakpoints.
    pub min_card_width: u16,
    /// Below this height the grid falls back to one focused card.
    pub min_card_height: u16,
    pub trend: bool,
    pub node_details: bool,
}
impl Default for Dashboard {
    fn default() -> Self {
        Self {
            columns: 0,
            min_card_width: 0,
            min_card_height: 9,
            trend: true,
            node_details: true,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct UiConfig {
    pub version: u32,
    /// A theme id; overrides the theme saved by the picker (not `--theme`).
    pub theme: Option<String>,
    /// Startup animation; unset keeps the `kborg startup on|off` preference.
    pub splash: Option<bool>,
    pub borders: Borders,
    pub panel_title: TitleStyle,
    pub header: Header,
    pub hints: Hints,
    pub table: Table,
    pub timestamps: Timestamps,
    /// Role → `#RRGGBB` overrides on top of the active theme.
    pub colors: BTreeMap<String, String>,
    pub dashboard: Dashboard,
}
impl Default for UiConfig {
    fn default() -> Self {
        Self {
            version: 1,
            theme: None,
            splash: None,
            borders: Borders::Auto,
            panel_title: TitleStyle::Marker,
            header: Header::default(),
            hints: Hints::default(),
            table: Table::default(),
            timestamps: Timestamps::Utc,
            colors: BTreeMap::new(),
            dashboard: Dashboard::default(),
        }
    }
}

/// Rows the resource table view gives to chrome, derived from `header`/`hints`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Chrome {
    pub header: Option<u16>,
    /// `None` when the identity line is folded into the header bar.
    pub identity: Option<u16>,
    pub summary: Option<u16>,
    /// Rows above the table.
    pub top: u16,
    /// Rows below the table: status line, spare row and hints row.
    pub bottom: u16,
}

impl UiConfig {
    pub fn chrome(&self) -> Chrome {
        let header = &self.header;
        let separate_identity = !header.show || (header.identity && !header.compact);
        let mut next = 0u16;
        let mut take = |shown: bool| {
            shown.then(|| {
                next += 1;
                next - 1
            })
        };
        let header_row = take(header.show);
        let identity = take(separate_identity);
        let summary = take(header.summary);
        Chrome {
            header: header_row,
            identity,
            summary,
            top: next,
            bottom: 1 + u16::from(!header.compact) + u16::from(self.hints.show),
        }
    }
    /// A panel's top-border title.
    pub fn panel_title(&self, title: &str, focused: bool) -> String {
        match self.panel_title {
            TitleStyle::Marker => format!(" {} {title} ", if focused { ">" } else { " " }),
            _ if title.is_empty() => String::new(),
            TitleStyle::Plain => format!(" {title} "),
            TitleStyle::Bracketed => format!("[ {title} ]"),
        }
    }
    /// A title painted over a border by the table and modal frames.
    pub fn inline_title(&self, title: &str) -> String {
        match self.panel_title {
            TitleStyle::Bracketed => format!("[ {title} ]"),
            _ => format!(" {title} "),
        }
    }
    pub fn local_time(&self) -> bool {
        self.timestamps == Timestamps::Local
    }
    /// The theme with this file's color overrides; the result must still pass
    /// the theme contrast checks. The host-palette theme ignores overrides.
    pub fn apply(&self, theme: &themes::Theme) -> Result<themes::Theme> {
        let mut theme = theme.clone();
        if self.colors.is_empty() || theme.terminal {
            return Ok(theme);
        }
        for (role, hex) in &self.colors {
            theme.colors.insert(role.clone(), themes::Rgb::parse(hex)?);
        }
        theme
            .validate()
            .map_err(|error| format!("ui.yaml colors on {}: {error}", theme.name))?;
        Ok(theme)
    }
    fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err("ui.yaml version must be 1".into());
        }
        if let Some(theme) = &self.theme
            && !safety::valid_id(theme)
        {
            return Err("ui.yaml theme must be a theme id such as nord or flexoki-dark".into());
        }
        if !self.header.show && !self.header.identity {
            return Err(
                "ui.yaml header: identity can only be hidden while show is true; context, namespace and mode stay visible".into(),
            );
        }
        for (name, role) in [
            ("hints.key_role", &self.hints.key_role),
            ("hints.text_role", &self.hints.text_role),
        ] {
            if !ROLES.contains(&role.as_str()) {
                return Err(format!(
                    "ui.yaml {name} must be one of {}",
                    ROLES.join(", ")
                ));
            }
        }
        let printable = |text: &str| text.bytes().all(|b| b.is_ascii_graphic() || b == b' ');
        let table = &self.table;
        for (name, value) in [
            ("selection_marker", &table.selection_marker),
            ("mark_marker", &table.mark_marker),
        ] {
            if value.len() > 1 || !printable(value) {
                return Err(format!(
                    "ui.yaml table.{name} must be one printable ASCII character or \"\""
                ));
            }
        }
        if table.column_separator.len() != 1 || !printable(&table.column_separator) {
            return Err(
                "ui.yaml table.column_separator must be exactly one printable ASCII character"
                    .into(),
            );
        }
        for (name, value) in [
            ("sort_ascending", &table.sort_ascending),
            ("sort_descending", &table.sort_descending),
        ] {
            if value.is_empty() || value.len() > 2 || !value.bytes().all(|b| b.is_ascii_graphic()) {
                return Err(format!(
                    "ui.yaml table.{name} must be 1 or 2 printable ASCII characters"
                ));
            }
        }
        if self.colors.len() > ROLES.len() {
            return Err("ui.yaml colors has too many entries".into());
        }
        for (role, hex) in &self.colors {
            if !ROLES.contains(&role.as_str()) {
                return Err(format!(
                    "ui.yaml colors: unknown role {:?}; roles are {}",
                    safety::safe_text(role, 40),
                    ROLES.join(", ")
                ));
            }
            themes::Rgb::parse(hex)
                .map_err(|_| format!("ui.yaml colors.{role} must be #RRGGBB"))?;
        }
        let dashboard = &self.dashboard;
        if dashboard.columns > 3 {
            return Err("ui.yaml dashboard.columns must be 0 (auto) or 1..3".into());
        }
        if dashboard.min_card_width > 200 {
            return Err("ui.yaml dashboard.min_card_width must be 0..200".into());
        }
        if !(5..=40).contains(&dashboard.min_card_height) {
            return Err("ui.yaml dashboard.min_card_height must be 5..40".into());
        }
        Ok(())
    }
}

pub fn parse(text: &str) -> Result<UiConfig> {
    if text.len() > MAX_BYTES {
        return Err("ui.yaml exceeds 16 KiB".into());
    }
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_reader_input_bytes: Some(MAX_BYTES), max_events: 2000, max_aliases: 0, max_anchors: 0,
            max_depth: 4, max_inclusion_depth: 0, max_documents: 1, max_nodes: 512,
            max_total_scalar_bytes: MAX_BYTES, max_merge_keys: 0,
        },
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys: serde_saphyr::MergeKeyPolicy::Error,
        with_snippet: false,
    };
    // An empty or comment-only file means defaults.
    if text
        .lines()
        .all(|line| line.trim().is_empty() || line.trim_start().starts_with('#'))
    {
        return Ok(UiConfig::default());
    }
    let config: UiConfig = serde_saphyr::from_str_with_options(text, options).map_err(|_| {
        "invalid ui.yaml; unknown keys or wrong value types are rejected (kborg ui init writes a documented example)"
    })?;
    config.validate()?;
    Ok(config)
}

fn path() -> Result<PathBuf> {
    Ok(safety::config_dir()?.join(FILE_NAME))
}

pub fn load() -> Result<UiConfig> {
    let path = path()?;
    safety::checked_path(&path)?;
    if !path.exists() {
        return Ok(UiConfig::default());
    }
    let bytes = safety::read_small(&path, MAX_BYTES as u64)?;
    parse(std::str::from_utf8(&bytes).map_err(|_| "ui.yaml must be UTF-8")?)
}

/// Write the commented example; never replaces an existing file.
pub fn init() -> Result<PathBuf> {
    let path = path()?;
    safety::write_new(&path, EXAMPLE.as_bytes())
        .map_err(|_| format!("{} already exists or cannot be created", path.display()))?;
    Ok(path)
}

/// Validate the installed file without starting the UI.
pub fn validate_installed() -> Result<PathBuf> {
    let path = path()?;
    let config = load()?;
    if let Some(theme) = &config.theme {
        let theme = themes::find(theme)?;
        config.apply(&theme)?;
    } else {
        config.apply(&themes::find(&themes::selected())?)?;
    }
    Ok(path)
}

/// Local time zone for `timestamps: local`: `TZ`, then /etc/localtime, else UTC.
pub fn local_zone() -> &'static k8s_openapi::jiff::tz::TimeZone {
    use k8s_openapi::jiff::tz::TimeZone;
    static ZONE: std::sync::OnceLock<TimeZone> = std::sync::OnceLock::new();
    ZONE.get_or_init(|| {
        let read = |path: &std::path::Path| -> Option<Vec<u8>> {
            use std::io::Read;
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .ok()?
                .take(512 * 1024)
                .read_to_end(&mut bytes)
                .ok()?;
            Some(bytes)
        };
        let named = std::env::var("TZ")
            .ok()
            .map(|tz| tz.trim_start_matches(':').to_owned());
        let file = match named.as_deref() {
            Some("" | "UTC" | "Etc/UTC") => return TimeZone::UTC,
            Some(name)
                if name.len() <= 64
                    && !name.contains("..")
                    && !name.starts_with('/')
                    && name
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"/_+-".contains(&b)) =>
            {
                read(&std::path::Path::new("/usr/share/zoneinfo").join(name))
            }
            _ => None,
        }
        .or_else(|| read(std::path::Path::new("/etc/localtime")));
        file.and_then(|bytes| TimeZone::tzif("localtime", &bytes).ok())
            .unwrap_or(TimeZone::UTC)
    })
}

/// Rewrite a leading RFC 3339 log timestamp in `zone`, keeping the rest.
pub fn localize_timestamp(line: &str, zone: &k8s_openapi::jiff::tz::TimeZone) -> Option<String> {
    let (stamp, rest) = line.split_once(' ')?;
    if stamp.len() > 64 {
        return None;
    }
    let time = stamp.parse::<k8s_openapi::jiff::Timestamp>().ok()?;
    let offset = zone.to_offset(time);
    Some(format!("{} {rest}", time.display_with_offset(offset)))
}

pub const EXAMPLE: &str = r##"# KBorg ui.yaml: layout and style, data only. Every value below is the
# built-in default; delete what you do not change. Unknown keys are rejected
# and an invalid file falls back to defaults with a notice. Reload while
# running with :ui reload. Documentation: docs/themes.md, "ui.yaml".
version: 1

# Theme id (kborg theme list). When set it wins over the picker's saved
# choice; --theme still wins over this. Unset keeps the saved choice.
# theme: flexoki-dark

# Startup animation. Unset keeps the `kborg startup on|off` preference.
# splash: true

# auto (rounded in UTF-8 locales, ASCII otherwise) | rounded | square | ascii | none
borders: auto
# marker (" > TITLE ") | plain (" TITLE ") | bracketed ("[ TITLE ]")
panel_title: marker

header:
  show: true        # top bar with the KBORG badge and view title
  identity: true    # CTX / NS / API line; false folds it into the top bar
  summary: true     # freshness, counts, health and sort bar
  compact: false    # fold identity into the top bar and drop the spare row

hints:
  show: true        # bottom key-hint row of the resource table
  key_role: accent  # palette role for shortcut keys
  text_role: muted  # palette role for their labels

table:
  density: normal           # normal | relaxed (blank line between rows)
  selection_marker: ">"     # one character, or "" for none
  mark_marker: "*"          # one character, or "" for none
  zebra: false              # alternate row background
  column_separator: " "     # exactly one character
  age: short                # short (14d) | long (14d3h)
  sort_ascending: "^"
  sort_descending: "v"

# Log timestamps (t in the log view): utc as sent by the API | local
timestamps: utc

# Per-role #RRGGBB overrides on top of the active theme. Roles: background,
# text, muted, accent, success, warning, danger, focus, selection_background,
# selection_text, status_background, status_text. Contrast is still checked.
colors: {}
#  accent: "#88C0D0"

dashboard:
  columns: 0            # 0 = automatic (1..3 by width), or 1..3
  min_card_width: 0     # 0 = built-in breakpoints; else columns that fit
  min_card_height: 9    # 5..40; below it one focused card is shown
  trend: true           # sparkline trend line on cards
  node_details: true    # busiest-Node detail lines under NODES
"##;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_empty_and_example_files_equal_the_built_in_look() -> Result<()> {
        assert_eq!(parse("")?, UiConfig::default());
        assert_eq!(parse("# only a comment\n")?, UiConfig::default());
        assert_eq!(parse(EXAMPLE)?, UiConfig::default());
        let chrome = UiConfig::default().chrome();
        assert_eq!(
            chrome,
            Chrome {
                header: Some(0),
                identity: Some(1),
                summary: Some(2),
                top: 3,
                bottom: 3
            }
        );
        assert_eq!(UiConfig::default().panel_title("Pods", true), " > Pods ");
        assert_eq!(UiConfig::default().inline_title("Pods"), " Pods ");
        Ok(())
    }

    #[test]
    fn full_file_parses_every_option() -> Result<()> {
        let config = parse(
            "version: 1\ntheme: nord\nsplash: false\nborders: ascii\npanel_title: bracketed\nheader:\n  show: true\n  identity: false\n  summary: false\n  compact: true\nhints:\n  show: false\n  key_role: warning\n  text_role: text\ntable:\n  density: relaxed\n  selection_marker: \"\"\n  mark_marker: \"+\"\n  zebra: true\n  column_separator: \"|\"\n  age: long\n  sort_ascending: \"<\"\n  sort_descending: \">\"\ntimestamps: local\ncolors:\n  accent: \"#88C0D0\"\ndashboard:\n  columns: 2\n  min_card_width: 40\n  min_card_height: 12\n  trend: false\n  node_details: false\n",
        )?;
        assert_eq!(config.theme.as_deref(), Some("nord"));
        assert_eq!(config.splash, Some(false));
        assert_eq!(config.borders, Borders::Ascii);
        assert_eq!(config.borders.glyphs()[0], "+");
        assert_eq!(config.panel_title("x", true), "[ x ]");
        assert_eq!(config.panel_title("", false), "");
        assert_eq!(config.table.selection(), ' ');
        assert_eq!(config.table.mark(), '+');
        assert_eq!(config.table.separator(), '|');
        assert_eq!(config.table.step(), 2);
        assert!(config.table.long_age() && config.local_time());
        assert_eq!(config.dashboard.columns, 2);
        assert_eq!(
            config.chrome(),
            Chrome {
                header: Some(0),
                identity: None,
                summary: None,
                top: 1,
                bottom: 1
            }
        );
        Ok(())
    }

    #[test]
    fn hidden_header_keeps_identity_on_top() -> Result<()> {
        let config = parse("header:\n  show: false\n")?;
        let chrome = config.chrome();
        assert_eq!(
            (chrome.header, chrome.identity, chrome.top),
            (None, Some(0), 2)
        );
        Ok(())
    }

    #[test]
    fn bad_files_are_rejected() {
        for bad in [
            "unknown: 1\n",
            "borders: dotted\n",
            "header:\n  shows: true\n",
            "header:\n  show: false\n  identity: false\n",
            "table:\n  column_separator: \"||\"\n",
            "table:\n  column_separator: \"\"\n",
            "table:\n  selection_marker: \"ab\"\n",
            "table:\n  selection_marker: \"\u{e9}\"\n",
            "table:\n  sort_ascending: \"\"\n",
            "table:\n  age: medium\n",
            "colors:\n  accent: blue\n",
            "colors:\n  link: \"#FFFFFF\"\n",
            "hints:\n  key_role: neon\n",
            "dashboard:\n  columns: 4\n",
            "dashboard:\n  min_card_height: 2\n",
            "theme: Not A Theme\n",
            "version: 2\n",
            "base: &a {show: true}\nheader: *a\n",
            "borders: ascii\nborders: none\n",
            "[1, 2]\n",
        ] {
            assert!(parse(bad).is_err(), "accepted {bad:?}");
        }
        assert!(parse(&"#".repeat(MAX_BYTES + 1)).is_err());
    }

    #[test]
    fn color_overrides_apply_on_the_theme_and_keep_contrast_checks() -> Result<()> {
        let base = themes::builtins()
            .into_iter()
            .find(|theme| theme.id == "nord")
            .ok_or("nord")?;
        let config = parse("colors:\n  accent: \"#FFD700\"\n")?;
        let themed = config.apply(&base)?;
        assert_eq!(themed.get("accent"), themes::Rgb(0xFF, 0xD7, 0x00));
        assert_eq!(themed.get("text"), base.get("text"));
        let unreadable = parse("colors:\n  text: \"#2E3440\"\n")?;
        assert!(unreadable.apply(&base).is_err());
        Ok(())
    }

    #[test]
    fn log_timestamps_are_rewritten_in_the_chosen_zone() {
        use k8s_openapi::jiff::tz::{Offset, TimeZone};
        let zone = TimeZone::fixed(Offset::constant(2));
        assert_eq!(
            localize_timestamp("2026-10-23T10:00:00Z INFO ok", &zone).as_deref(),
            Some("2026-10-23T12:00:00+02:00 INFO ok")
        );
        assert_eq!(localize_timestamp("plain line", &zone), None);
    }
}
