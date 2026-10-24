use crate::{Result, safety};
use crossterm::{
    Command,
    style::{self, Color},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::{fmt, sync::OnceLock};

pub const DEFAULT_THEME: &str = "flexoki-dark";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ColorDepth {
    Basic16,
    Indexed256,
    TrueColor,
}

fn detected_depth(term: Option<&str>, colorterm: Option<&str>, windows: bool) -> ColorDepth {
    let term = term.unwrap_or_default().to_ascii_lowercase();
    let colorterm = colorterm.unwrap_or_default().to_ascii_lowercase();
    if matches!(colorterm.as_str(), "truecolor" | "24bit")
        || term.ends_with("-direct")
        || term.contains("truecolor")
        || term.contains("24bit")
        // Terminals that always support 24-bit color; COLORTERM is commonly
        // dropped over SSH, which would otherwise fall back to 16 colors.
        || matches!(
            term.as_str(),
            "xterm-ghostty" | "xterm-kitty" | "wezterm" | "alacritty" | "foot" | "contour"
        )
    {
        ColorDepth::TrueColor
    } else if term.contains("256color") {
        ColorDepth::Indexed256
    } else if term.is_empty() && windows {
        // Native Windows terminals commonly omit TERM. Preserve the existing
        // Crossterm native/modern-console behavior in that case.
        ColorDepth::TrueColor
    } else {
        ColorDepth::Basic16
    }
}

fn terminal_depth() -> ColorDepth {
    static DEPTH: OnceLock<ColorDepth> = OnceLock::new();
    *DEPTH.get_or_init(|| {
        detected_depth(
            std::env::var("TERM").ok().as_deref(),
            std::env::var("COLORTERM").ok().as_deref(),
            cfg!(windows),
        )
    })
}

// The fixed xterm cube/grayscale entries are independent of customized terminal
// ANSI slots 0..15. Basic-color approximations necessarily use the host palette.
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];
const BASIC: [Rgb; 16] = [
    Rgb(0, 0, 0),
    Rgb(128, 0, 0),
    Rgb(0, 128, 0),
    Rgb(128, 128, 0),
    Rgb(0, 0, 128),
    Rgb(128, 0, 128),
    Rgb(0, 128, 128),
    Rgb(192, 192, 192),
    Rgb(128, 128, 128),
    Rgb(255, 0, 0),
    Rgb(0, 255, 0),
    Rgb(255, 255, 0),
    Rgb(0, 0, 255),
    Rgb(255, 0, 255),
    Rgb(0, 255, 255),
    Rgb(255, 255, 255),
];

fn distance(a: Rgb, b: Rgb) -> u32 {
    let channel = |x: u8, y: u8| (i32::from(x) - i32::from(y)).pow(2) as u32;
    channel(a.0, b.0) + channel(a.1, b.1) + channel(a.2, b.2)
}

fn nearest_basic(rgb: Rgb) -> u8 {
    let mut index = 0;
    let mut closest = u32::MAX;
    for (candidate, color) in BASIC.iter().enumerate() {
        let score = distance(rgb, *color);
        if score < closest {
            closest = score;
            index = candidate as u8;
        }
    }
    index
}

fn nearest_indexed(rgb: Rgb) -> u8 {
    let channel = |value: u8| -> u8 {
        let mut index = 0;
        let mut closest = u32::MAX;
        for (candidate, level) in CUBE.iter().enumerate() {
            let score = (i32::from(value) - i32::from(*level)).pow(2) as u32;
            if score < closest {
                closest = score;
                index = candidate as u8;
            }
        }
        index
    };
    let (r, g, b) = (channel(rgb.0), channel(rgb.1), channel(rgb.2));
    let cube = Rgb(
        CUBE[usize::from(r)],
        CUBE[usize::from(g)],
        CUBE[usize::from(b)],
    );
    // The best grayscale entry is nearest to the mean of the three channels.
    let sum = i32::from(rgb.0) + i32::from(rgb.1) + i32::from(rgb.2);
    let gray_index = ((sum - 24 + 15) / 30).clamp(0, 23) as u8;
    let gray = 8 + gray_index * 10;
    if distance(rgb, Rgb(gray, gray, gray)) < distance(rgb, cube) {
        232 + gray_index
    } else {
        16 + 36 * r + 6 * g + b
    }
}

fn rgb_for_index(index: u8) -> Rgb {
    match index {
        0..=15 => BASIC[usize::from(index)],
        16..=231 => {
            let cube = index - 16;
            Rgb(
                CUBE[usize::from(cube / 36)],
                CUBE[usize::from((cube / 6) % 6)],
                CUBE[usize::from(cube % 6)],
            )
        }
        _ => {
            let gray = 8 + (index - 232) * 10;
            Rgb(gray, gray, gray)
        }
    }
}

fn basic_index(color: Color) -> Option<u8> {
    Some(match color {
        Color::Reset => return None,
        Color::Black => 0,
        Color::DarkRed => 1,
        Color::DarkGreen => 2,
        Color::DarkYellow => 3,
        Color::DarkBlue => 4,
        Color::DarkMagenta => 5,
        Color::DarkCyan => 6,
        Color::Grey => 7,
        Color::DarkGrey => 8,
        Color::Red => 9,
        Color::Green => 10,
        Color::Yellow => 11,
        Color::Blue => 12,
        Color::Magenta => 13,
        Color::Cyan => 14,
        Color::White => 15,
        Color::AnsiValue(index) if index < 16 => index,
        Color::AnsiValue(index) => nearest_basic(rgb_for_index(index)),
        Color::Rgb { r, g, b } => nearest_basic(Rgb(r, g, b)),
    })
}
#[cfg(any(windows, test))]
fn native_color(color: Color) -> Color {
    const NAMED: [Color; 16] = [
        Color::Black,
        Color::DarkRed,
        Color::DarkGreen,
        Color::DarkYellow,
        Color::DarkBlue,
        Color::DarkMagenta,
        Color::DarkCyan,
        Color::Grey,
        Color::DarkGrey,
        Color::Red,
        Color::Green,
        Color::Yellow,
        Color::Blue,
        Color::Magenta,
        Color::Cyan,
        Color::White,
    ];
    basic_index(color)
        .map(|index| NAMED[usize::from(index)])
        .unwrap_or(Color::Reset)
}

fn write_color(
    color: Color,
    background: bool,
    depth: ColorDepth,
    out: &mut impl fmt::Write,
) -> fmt::Result {
    if depth == ColorDepth::Basic16 {
        let code = match basic_index(color) {
            None => {
                if background {
                    49
                } else {
                    39
                }
            }
            Some(index) if index < 8 => 30 + u16::from(index) + if background { 10 } else { 0 },
            Some(index) => 90 + u16::from(index - 8) + if background { 10 } else { 0 },
        };
        write!(out, "\x1b[{code}m")
    } else {
        let color = match (depth, color) {
            (ColorDepth::Indexed256, Color::Rgb { r, g, b }) => {
                Color::AnsiValue(nearest_indexed(Rgb(r, g, b)))
            }
            _ => color,
        };
        if background {
            style::SetBackgroundColor(color).write_ansi(out)
        } else {
            style::SetForegroundColor(color).write_ansi(out)
        }
    }
}

/// Color command that emits basic SGR on terminals advertising only ANSI colors.
#[derive(Clone, Copy, Debug)]
pub struct ForegroundColor(pub Color);
impl Command for ForegroundColor {
    fn write_ansi(&self, out: &mut impl fmt::Write) -> fmt::Result {
        write_color(self.0, false, terminal_depth(), out)
    }
    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        style::SetForegroundColor(native_color(self.0)).execute_winapi()
    }
}

/// Background equivalent of `ForegroundColor`, preserving native Windows fallback.
#[derive(Clone, Copy, Debug)]
pub struct BackgroundColor(pub Color);
impl Command for BackgroundColor {
    fn write_ansi(&self, out: &mut impl fmt::Write) -> fmt::Result {
        write_color(self.0, true, terminal_depth(), out)
    }
    #[cfg(windows)]
    fn execute_winapi(&self) -> std::io::Result<()> {
        style::SetBackgroundColor(native_color(self.0)).execute_winapi()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rgb(pub u8, pub u8, pub u8);
impl Rgb {
    pub fn parse(s: &str) -> Result<Self> {
        if s.len() != 7 || !s.starts_with('#') || !s[1..].bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err("color must be #RRGGBB".into());
        }
        Ok(Self(
            u8::from_str_radix(&s[1..3], 16).map_err(|_| "invalid color")?,
            u8::from_str_radix(&s[3..5], 16).map_err(|_| "invalid color")?,
            u8::from_str_radix(&s[5..7], 16).map_err(|_| "invalid color")?,
        ))
    }
    pub fn hex(self) -> String {
        format!("#{:02X}{:02X}{:02X}", self.0, self.1, self.2)
    }
    pub fn color(self) -> Color {
        self.color_at_depth(terminal_depth())
    }
    fn color_at_depth(self, depth: ColorDepth) -> Color {
        match depth {
            ColorDepth::Basic16 => Color::AnsiValue(nearest_basic(self)),
            ColorDepth::Indexed256 => Color::AnsiValue(nearest_indexed(self)),
            ColorDepth::TrueColor => Color::Rgb {
                r: self.0,
                g: self.1,
                b: self.2,
            },
        }
    }
    fn luminance(self) -> f64 {
        let channel = |c: u8| {
            let v = f64::from(c) / 255.0;
            if v <= 0.04045 {
                v / 12.92
            } else {
                ((v + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(self.0) + 0.7152 * channel(self.1) + 0.0722 * channel(self.2)
    }
}
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let x = a.luminance();
    let y = b.luminance();
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}
fn legible(a: Rgb, b: Rgb) -> Rgb {
    if contrast(a, b) >= 4.5 {
        a
    } else if contrast(Rgb(255, 255, 255), b) > contrast(Rgb(0, 0, 0), b) {
        Rgb(255, 255, 255)
    } else {
        Rgb(0, 0, 0)
    }
}

fn mix(a: Rgb, b: Rgb, percent: u16) -> Rgb {
    let channel =
        |a: u8, b: u8| ((u16::from(a) * (100 - percent) + u16::from(b) * percent) / 100) as u8;
    Rgb(channel(a.0, b.0), channel(a.1, b.1), channel(a.2, b.2))
}

/// Strongest surface tint the UI paints (raised rows mix 6% text into the
/// background); cards, headers and selection sit between it and the background.
pub const MAX_SURFACE_TINT: u16 = 6;

/// Keeps a palette color's hue while moving it toward white (dark themes) or
/// black (light themes) just far enough to reach 4.5:1 on every surface from
/// the plain background to the most tinted one.
/// `text` is `None` when the color being adjusted is the text color itself,
/// whose own value defines the tinted surfaces.
fn legible_on_surfaces(color: Rgb, background: Rgb, text: Option<Rgb>) -> Rgb {
    let target = if background.luminance() < 0.5 {
        Rgb(255, 255, 255)
    } else {
        Rgb(0, 0, 0)
    };
    for step in 0..=100u16 {
        let candidate = mix(color, target, step);
        let tinted = mix(background, text.unwrap_or(candidate), MAX_SURFACE_TINT);
        if contrast(candidate, background) >= 4.5 && contrast(candidate, tinted) >= 4.5 {
            return candidate;
        }
    }
    legible(color, background)
}

#[derive(Clone)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub colors: BTreeMap<String, Rgb>,
    pub terminal: bool,
}
impl Theme {
    pub fn get(&self, role: &str) -> Rgb {
        self.colors.get(role).copied().unwrap_or(Rgb(255, 255, 255))
    }
    pub fn validate(&self) -> Result<()> {
        for (fg, bg) in [
            ("text", "background"),
            ("muted", "background"),
            ("accent", "background"),
            ("success", "background"),
            ("warning", "background"),
            ("danger", "background"),
            ("selection_text", "selection_background"),
            ("status_text", "status_background"),
        ] {
            if contrast(self.get(fg), self.get(bg)) < 4.5 {
                return Err(format!(
                    "{fg}/{bg} contrast is below 4.5:1; choose a lighter or darker foreground"
                ));
            }
        }
        if contrast(self.get("focus"), self.get("background")) < 3.0 {
            return Err("focus/background contrast must be at least 3:1".into());
        }
        Ok(())
    }
}

const PALETTES: &[(&str, &str, [&str; 7])] = &[
    (
        "k8borg-dark",
        "KBorg Dark",
        [
            "#0B1220", "#F8FAFC", "#94A3B8", "#2DD4BF", "#4ADE80", "#FBBF24", "#FB7185",
        ],
    ),
    (
        "k8borg-light",
        "KBorg Light",
        [
            "#F8FAFC", "#0B1220", "#475569", "#0F766E", "#166534", "#92400E", "#BE123C",
        ],
    ),
    (
        "k8borg-high-contrast",
        "KBorg High Contrast",
        [
            "#000000", "#FFFFFF", "#FFFFFF", "#00FFFF", "#00FF00", "#FFFF00", "#FF8080",
        ],
    ),
    (
        "catppuccin-latte",
        "Catppuccin Latte",
        [
            "#EFF1F5", "#4C4F69", "#6C6F85", "#1E66F5", "#40A02B", "#DF8E1D", "#D20F39",
        ],
    ),
    (
        "catppuccin-frappe",
        "Catppuccin Frappe",
        [
            "#303446", "#C6D0F5", "#A5ADCE", "#8CAAEE", "#A6D189", "#E5C890", "#E78284",
        ],
    ),
    (
        "catppuccin-macchiato",
        "Catppuccin Macchiato",
        [
            "#24273A", "#CAD3F5", "#A5ADCB", "#8AADF4", "#A6DA95", "#EED49F", "#ED8796",
        ],
    ),
    (
        "catppuccin-mocha",
        "Catppuccin Mocha",
        [
            "#1E1E2E", "#CDD6F4", "#A6ADC8", "#89B4FA", "#A6E3A1", "#F9E2AF", "#F38BA8",
        ],
    ),
    (
        "dracula",
        "Dracula",
        [
            "#282A36", "#F8F8F2", "#6272A4", "#BD93F9", "#50FA7B", "#F1FA8C", "#FF5555",
        ],
    ),
    (
        "nord",
        "Nord",
        [
            "#2E3440", "#ECEFF4", "#D8DEE9", "#88C0D0", "#A3BE8C", "#EBCB8B", "#BF616A",
        ],
    ),
    (
        "gruvbox-dark",
        "Gruvbox Dark",
        [
            "#282828", "#EBDBB2", "#A89984", "#83A598", "#B8BB26", "#FABD2F", "#FB4934",
        ],
    ),
    (
        "gruvbox-light",
        "Gruvbox Light",
        [
            "#FBF1C7", "#3C3836", "#7C6F64", "#076678", "#79740E", "#B57614", "#9D0006",
        ],
    ),
    (
        "flexoki-dark",
        "Flexoki Dark",
        [
            "#100F0F", "#CECDC3", "#878580", "#3AA99F", "#879A39", "#D0A215", "#E8705F",
        ],
    ),
    (
        "rose-pine",
        "Rose Pine",
        [
            "#191724", "#E0DEF4", "#908CAA", "#C4A7E7", "#9CCFD8", "#F6C177", "#EB6F92",
        ],
    ),
    (
        "tokyo-night-night",
        "Tokyo Night / Night",
        [
            "#1A1B26", "#C0CAF5", "#565F89", "#7AA2F7", "#9ECE6A", "#E0AF68", "#F7768E",
        ],
    ),
    (
        "tokyo-night-storm",
        "Tokyo Night / Storm",
        [
            "#24283B", "#C0CAF5", "#565F89", "#7AA2F7", "#9ECE6A", "#E0AF68", "#F7768E",
        ],
    ),
    (
        "tokyo-night-moon",
        "Tokyo Night / Moon",
        [
            "#222436", "#C8D3F5", "#636DA6", "#82AAFF", "#C3E88D", "#FFC777", "#FF757F",
        ],
    ),
    (
        "tokyo-night-day",
        "Tokyo Night / Day",
        [
            "#E1E2E7", "#3760BF", "#848CB5", "#2E7DE9", "#587539", "#8C6C3E", "#F52A65",
        ],
    ),
    (
        "solarized-dark",
        "Solarized Dark",
        [
            "#002B36", "#839496", "#586E75", "#268BD2", "#859900", "#B58900", "#DC322F",
        ],
    ),
    (
        "solarized-light",
        "Solarized Light",
        [
            "#FDF6E3", "#657B83", "#93A1A1", "#268BD2", "#859900", "#B58900", "#DC322F",
        ],
    ),
    (
        "terminal",
        "Terminal (host palette)",
        [
            "#000000", "#FFFFFF", "#FFFFFF", "#FFFFFF", "#FFFFFF", "#FFFFFF", "#FFFFFF",
        ],
    ),
];

pub fn builtins() -> Vec<Theme> {
    PALETTES
        .iter()
        .map(|(id, name, hex)| {
            let bg = Rgb::parse(hex[0]).unwrap_or(Rgb(0, 0, 0));
            let mut colors = BTreeMap::new();
            colors.insert("background".into(), bg);
            for (role, color) in ["text", "muted", "accent", "success", "warning", "danger"]
                .into_iter()
                .zip(hex[1..].iter())
            {
                colors.insert(
                    role.into(),
                    legible(Rgb::parse(color).unwrap_or(Rgb(255, 255, 255)), bg),
                );
            }
            // Text first: every tinted surface is derived from it.
            let palette = |role: &str| colors.get(role).copied().unwrap_or(Rgb(255, 255, 255));
            let text = legible_on_surfaces(palette("text"), bg, None);
            let adjusted = ["muted", "accent", "success", "warning", "danger"]
                .map(|role| (role, legible_on_surfaces(palette(role), bg, Some(text))));
            colors.insert("text".into(), text);
            for (role, color) in adjusted {
                colors.insert(role.into(), color);
            }
            let accent = colors.get("accent").copied().unwrap_or(Rgb(255, 255, 255));
            colors.insert("focus".into(), accent);
            colors.insert("selection_background".into(), accent);
            colors.insert("selection_text".into(), legible(bg, accent));
            colors.insert("status_background".into(), bg);
            colors.insert(
                "status_text".into(),
                colors.get("warning").copied().unwrap_or(accent),
            );
            // These reading-oriented palettes use quiet surfaces rather than a
            // saturated accent fill. Keep focus in the accent and ordinary
            // selected text in the original foreground. Their upstream faint
            // colors are intentionally not used for functional text below 4.5:1.
            let surfaces = match *id {
                "flexoki-dark" => Some(("#282726", "#1C1B1A")),
                "rose-pine" => Some(("#403D52", "#1F1D2E")),
                _ => None,
            };
            if let Some((selection, status)) = surfaces {
                colors.insert(
                    "selection_background".into(),
                    Rgb::parse(selection).unwrap_or(bg),
                );
                colors.insert("selection_text".into(), colors["text"]);
                colors.insert("status_background".into(), Rgb::parse(status).unwrap_or(bg));
                colors.insert("status_text".into(), colors["muted"]);
            }
            Theme {
                id: (*id).into(),
                name: (*name).into(),
                colors,
                terminal: *id == "terminal",
            }
        })
        .collect()
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ThemeFile {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub base: String,
    #[serde(deserialize_with = "unique_colors")]
    pub colors: BTreeMap<String, String>,
}
fn unique_colors<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<BTreeMap<String, String>, D::Error> {
    struct Visitor;
    impl<'de> serde::de::Visitor<'de> for Visitor {
        type Value = BTreeMap<String, String>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("unique color role entries")
        }
        fn visit_map<M: serde::de::MapAccess<'de>>(
            self,
            mut map: M,
        ) -> std::result::Result<Self::Value, M::Error> {
            let mut colors = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, String>()? {
                if colors.insert(key, value).is_some() {
                    return Err(serde::de::Error::custom("duplicate color role"));
                }
            }
            Ok(colors)
        }
    }
    d.deserialize_map(Visitor)
}
impl ThemeFile {
    pub fn resolve(&self) -> Result<Theme> {
        if self.schema_version != 1 {
            return Err("unsupported theme schema_version".into());
        }
        if !safety::valid_id(&self.id) || builtins().iter().any(|t| t.id == self.id) {
            return Err("invalid or reserved custom theme id".into());
        }
        if self.name.is_empty()
            || self.name.len() > 80
            || safety::safe_text(&self.name, 80) != self.name
        {
            return Err("theme name must be short plain text".into());
        }
        let mut theme = builtins()
            .into_iter()
            .find(|t| t.id == self.base && !t.terminal)
            .ok_or("base must be a fixed built-in theme")?;
        for (role, hex) in &self.colors {
            if !theme.colors.contains_key(role) {
                return Err(format!(
                    "unknown color role: {}",
                    safety::safe_text(role, 40)
                ));
            }
            theme.colors.insert(role.clone(), Rgb::parse(hex)?);
        }
        theme.id = self.id.clone();
        theme.name = self.name.clone();
        theme.validate()?;
        Ok(theme)
    }
}
pub fn load_file(path: &Path) -> Result<ThemeFile> {
    let bytes = safety::read_small(path, safety::MAX_FILE)?;
    let file: ThemeFile = serde_json::from_slice(&bytes).map_err(|e| {
        format!(
            "invalid theme JSON at line {}, column {}",
            e.line(),
            e.column()
        )
    })?;
    file.resolve()?;
    Ok(file)
}
fn themes_dir() -> Result<PathBuf> {
    Ok(safety::config_dir()?.join("themes"))
}
pub fn all() -> Result<Vec<Theme>> {
    let mut themes = builtins();
    let dir = themes_dir()?;
    safety::checked_path(&dir)?;
    if dir.exists() {
        let entries = std::fs::read_dir(dir).map_err(|_| "cannot read theme directory")?;
        for entry in entries.take(128).flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json")
                && let Ok(file) = load_file(&path)
                && path.file_stem().and_then(|s| s.to_str()) == Some(file.id.as_str())
            {
                themes.push(file.resolve()?);
            }
        }
    }
    Ok(themes)
}
pub fn find(id: &str) -> Result<Theme> {
    all()?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| format!("theme not found: {}", safety::safe_text(id, 63)))
}
#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Preference {
    theme: String,
    #[serde(default = "enabled")]
    startup_animation: bool,
    #[serde(default = "enabled")]
    onboarding_complete: bool,
}
fn enabled() -> bool {
    true
}
fn preference() -> Result<Preference> {
    let bytes = safety::read_small(&safety::config_dir()?.join("appearance.json"), 4096)?;
    serde_json::from_slice(&bytes).map_err(|_| "invalid appearance preference".into())
}
pub fn first_run() -> Result<bool> {
    let path = safety::config_dir()?.join("appearance.json");
    safety::checked_path(&path)?;
    if !path.exists() {
        return Ok(true);
    }
    Ok(!preference()?.onboarding_complete)
}
pub fn animation_enabled() -> bool {
    preference().map(|p| p.startup_animation).unwrap_or(true)
}
pub fn set_animation(enabled: bool) -> Result<()> {
    let theme = selected();
    save_preference(&Preference {
        theme,
        startup_animation: enabled,
        onboarding_complete: preference().map(|p| p.onboarding_complete).unwrap_or(false),
    })
}
pub fn selected() -> String {
    let read = || -> Result<String> {
        let p = preference()?;
        find(&p.theme)?;
        Ok(p.theme)
    };
    read().unwrap_or_else(|_| DEFAULT_THEME.into())
}
pub fn use_theme(id: &str) -> Result<()> {
    find(id)?;
    save_preference(&Preference {
        theme: id.into(),
        startup_animation: animation_enabled(),
        onboarding_complete: true,
    })
}
fn save_preference(preference: &Preference) -> Result<()> {
    let path = safety::config_dir()?.join("appearance.json");
    let bytes = serde_json::to_vec(preference).map_err(|_| "cannot serialize preference")?;
    safety::atomic_write(&path, &bytes)
}
pub fn install(path: &Path) -> Result<String> {
    let file = load_file(path)?;
    let bytes = serde_json::to_vec_pretty(&file).map_err(|_| "cannot serialize theme")?;
    safety::write_new(&themes_dir()?.join(format!("{}.json", file.id)), &bytes)?;
    Ok(file.id)
}
pub fn new_theme(id: &str, base: &str) -> Result<PathBuf> {
    let file = ThemeFile {
        schema_version: 1,
        id: id.into(),
        name: id.into(),
        base: base.into(),
        colors: BTreeMap::new(),
    };
    file.resolve()?;
    let path = themes_dir()?.join(format!("{id}.json"));
    safety::write_new(
        &path,
        &serde_json::to_vec_pretty(&file).map_err(|_| "cannot serialize theme")?,
    )?;
    Ok(path)
}
pub fn export(id: &str, path: &Path) -> Result<()> {
    let t = find(id)?;
    if t.terminal {
        return Err("host palette cannot be exported as fixed RGB".into());
    }
    let custom = if builtins().iter().any(|t| t.id == id) {
        format!("{id}-custom")
    } else {
        id.into()
    };
    let file = ThemeFile {
        schema_version: 1,
        id: custom,
        name: t.name,
        base: "k8borg-dark".into(),
        colors: t.colors.into_iter().map(|(k, v)| (k, v.hex())).collect(),
    };
    safety::write_new(
        path,
        &serde_json::to_vec_pretty(&file).map_err(|_| "cannot serialize theme")?,
    )
}
pub fn remove(id: &str) -> Result<()> {
    if !safety::valid_id(id) || builtins().iter().any(|t| t.id == id) {
        return Err("only custom themes can be removed".into());
    }
    let path = themes_dir()?.join(format!("{id}.json"));
    safety::checked_path(&path)?;
    if selected() == id {
        use_theme(DEFAULT_THEME)?;
    }
    std::fs::remove_file(path).map_err(|_| "cannot remove custom theme".into())
}

/// Replace an installed custom theme only after retaining its prior bytes.
/// An existing backup is never overwritten; the user must archive it explicitly.
pub fn replace(path: &Path) -> Result<String> {
    let file = load_file(path)?;
    replace_in_dir(&file, &themes_dir()?)
}
fn replace_in_dir(file: &ThemeFile, dir: &Path) -> Result<String> {
    file.resolve()?;
    let destination = dir.join(format!("{}.json", file.id));
    let previous = safety::read_small(&destination, safety::MAX_FILE)?;
    let backup = dir.join(format!("{}.json.bak", file.id));
    let bytes = serde_json::to_vec_pretty(file).map_err(|_| "cannot serialize theme")?;
    safety::write_new(&backup, &previous).map_err(
        |_| "cannot preserve theme backup; archive any existing .json.bak before replacing",
    )?;
    safety::atomic_write(&destination, &bytes)?;
    Ok(file.id.clone())
}
/// Create an independent resolved custom theme from the current palette.
pub fn create_from_current(id: &str) -> Result<PathBuf> {
    let current = find(&selected())?;
    create_from_theme(id, &current)
}
/// Clone the actual session/preview palette without changing saved preferences.
pub fn create_from_theme(id: &str, current: &Theme) -> Result<PathBuf> {
    let file = cloned_theme(id, current)?;
    let destination = themes_dir()?.join(format!("{id}.json"));
    let bytes = serde_json::to_vec_pretty(&file).map_err(|_| "cannot serialize theme")?;
    safety::write_new(&destination, &bytes)?;
    Ok(destination)
}
fn cloned_theme(id: &str, current: &Theme) -> Result<ThemeFile> {
    if current.terminal {
        return Err("host palette cannot be copied as fixed RGB".into());
    }
    let file = ThemeFile {
        schema_version: 1,
        id: id.into(),
        name: id.into(),
        base: "k8borg-dark".into(),
        colors: current
            .colors
            .iter()
            .map(|(key, value)| (key.clone(), value.hex()))
            .collect(),
    };
    file.resolve()?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_windows_fallback_uses_named_host_palette_colors() {
        use super::{Color, basic_index, native_color};
        for index in 0..16 {
            let color = native_color(Color::AnsiValue(index));
            assert_eq!(basic_index(color), Some(index));
            assert!(!matches!(color, Color::Rgb { .. } | Color::AnsiValue(_)));
        }
        assert_eq!(native_color(Color::Rgb { r: 255, g: 0, b: 0 }), Color::Red);
        assert_eq!(
            native_color(Color::Rgb { r: 128, g: 0, b: 0 }),
            Color::DarkRed
        );
        assert_eq!(native_color(Color::AnsiValue(196)), Color::Red);
        assert_eq!(native_color(Color::AnsiValue(244)), Color::DarkGrey);
        assert_eq!(native_color(Color::Reset), Color::Reset);
    }
    #[test]
    fn declared_terminal_depth_controls_color_encoding() {
        use super::{ColorDepth::*, detected_depth};
        for term in ["linux", "vt100", "xterm", "screen", "dumb"] {
            assert_eq!(detected_depth(Some(term), None, false), Basic16);
        }
        for term in ["xterm-ghostty", "xterm-kitty", "wezterm", "alacritty"] {
            assert_eq!(detected_depth(Some(term), None, false), TrueColor, "{term}");
        }
        for term in ["xterm-256color", "screen-256color", "rxvt-unicode-256color"] {
            assert_eq!(detected_depth(Some(term), None, false), Indexed256);
        }
        for term in ["xterm-direct", "screen-direct", "xterm-24bit"] {
            assert_eq!(detected_depth(Some(term), None, false), TrueColor);
        }
        for declaration in ["truecolor", "24bit", "TRUECOLOR"] {
            assert_eq!(
                detected_depth(Some("xterm-256color"), Some(declaration), false),
                TrueColor
            );
        }
        assert_eq!(detected_depth(None, None, false), Basic16);
        assert_eq!(detected_depth(None, None, true), TrueColor);
        assert_eq!(detected_depth(Some("linux"), None, true), Basic16);
    }

    #[test]
    fn indexed_palette_preserves_cube_and_grayscale_and_chooses_the_nearest_entry() {
        use super::*;
        for index in 16..=255 {
            assert_eq!(nearest_indexed(rgb_for_index(index)), index);
        }
        // Include colors between entries and unequal channels: selecting a cube
        // entry alone loses the much closer neutral grayscale candidates.
        for r in [0, 17, 64, 96, 128, 181, 217, 254, 255] {
            for g in [0, 17, 64, 96, 128, 181, 217, 254, 255] {
                for b in [0, 17, 64, 96, 128, 181, 217, 254, 255] {
                    let rgb = Rgb(r, g, b);
                    let chosen = nearest_indexed(rgb);
                    assert!(chosen >= 16);
                    let score = distance(rgb, rgb_for_index(chosen));
                    assert!(
                        (16..=255).all(|index| score <= distance(rgb, rgb_for_index(index))),
                        "{rgb:?}"
                    );
                }
            }
        }
        assert_eq!(nearest_indexed(Rgb(128, 128, 128)), 244);
    }

    #[test]
    fn basic_palette_uses_all_sixteen_colors_and_actual_basic_sgr() -> fmt::Result {
        for (index, rgb) in BASIC.iter().enumerate() {
            let color = rgb.color_at_depth(ColorDepth::Basic16);
            assert_eq!(color, Color::AnsiValue(index as u8));
            let mut foreground = String::new();
            let mut background = String::new();
            write_color(color, false, ColorDepth::Basic16, &mut foreground)?;
            write_color(color, true, ColorDepth::Basic16, &mut background)?;
            let expected = if index < 8 {
                30 + index
            } else {
                90 + index - 8
            };
            assert_eq!(foreground, format!("\x1b[{expected}m"));
            assert_eq!(background, format!("\x1b[{}m", expected + 10));
            assert!(!foreground.contains("38;5;") && !background.contains("48;5;"));
        }
        for index in 0..=255 {
            let mut foreground = String::new();
            write_color(
                Color::AnsiValue(index),
                false,
                ColorDepth::Basic16,
                &mut foreground,
            )?;
            assert!(!foreground.contains(';'));
            assert!(nearest_basic(rgb_for_index(index)) < 16);
        }
        let mut reset = String::new();
        write_color(Color::Reset, false, ColorDepth::Basic16, &mut reset)?;
        write_color(Color::Reset, true, ColorDepth::Basic16, &mut reset)?;
        assert_eq!(reset, "\x1b[39m\x1b[49m");
        let rgb = Rgb(37, 98, 211);
        assert_eq!(
            rgb.color_at_depth(ColorDepth::TrueColor),
            Color::Rgb {
                r: 37,
                g: 98,
                b: 211
            }
        );
        Ok(())
    }

    #[test]
    fn cloning_uses_the_session_palette_and_validates_name_and_fixed_colors() -> crate::Result<()> {
        let preview = super::find("catppuccin-macchiato")?;
        let copy = super::cloned_theme("my-session-theme", &preview)?.resolve()?;
        assert_eq!(copy.colors, preview.colors);
        assert!(super::cloned_theme("../invalid", &preview).is_err());
        assert!(super::cloned_theme("catppuccin-mocha", &preview).is_err());
        assert!(super::cloned_theme("host-copy", &super::find("terminal")?).is_err());
        Ok(())
    }
    #[test]
    fn replacement_retains_original_backup_and_refuses_backup_overwrite_or_builtin() {
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_else(|e| panic!("{e}"))
            .as_nanos();
        let dir = std::env::temp_dir()
            .canonicalize()
            .unwrap_or_else(|e| panic!("test temporary root: {e}"))
            .join(format!("kborg-theme-{}-{stamp}", std::process::id()));
        let mut file = ThemeFile {
            schema_version: 1,
            id: "my-test".into(),
            name: "Original".into(),
            base: "catppuccin-mocha".into(),
            colors: BTreeMap::new(),
        };
        let original = serde_json::to_vec(&file).unwrap_or_else(|e| panic!("{e}"));
        let target = dir.join("my-test.json");
        safety::write_new(&target, &original).unwrap_or_else(|e| panic!("{e}"));
        file.name = "Updated".into();
        assert!(replace_in_dir(&file, &dir).is_ok());
        assert_eq!(
            safety::read_small(&dir.join("my-test.json.bak"), safety::MAX_FILE)
                .unwrap_or_else(|e| panic!("{e}")),
            original
        );
        assert_eq!(
            load_file(&target).unwrap_or_else(|e| panic!("{e}")).name,
            "Updated"
        );
        file.name = "Not Applied".into();
        assert!(replace_in_dir(&file, &dir).is_err());
        assert_eq!(
            load_file(&target).unwrap_or_else(|e| panic!("{e}")).name,
            "Updated"
        );
        file.id = "catppuccin-mocha".into();
        assert!(replace_in_dir(&file, &dir).is_err());
        std::fs::remove_dir_all(&dir).unwrap_or_else(|e| panic!("{e}"));
    }

    use super::*;
    #[test]
    fn all_builtins_meet_fixed_palette_targets() {
        assert_eq!(builtins().len(), 20);
        for t in builtins() {
            assert!(t.validate().is_ok(), "{}", t.id);
        }
    }
    #[test]
    fn reading_palettes_keep_their_ink_colors_and_quiet_legible_surfaces() -> crate::Result<()> {
        for (id, background, text, accent, danger) in [
            ("flexoki-dark", "#100F0F", "#CECDC3", "#3AA99F", "#E8705F"),
            ("rose-pine", "#191724", "#E0DEF4", "#C4A7E7", "#EB6F92"),
        ] {
            let theme = builtins()
                .into_iter()
                .find(|theme| theme.id == id)
                .ok_or("missing reading palette")?;
            theme.validate()?;
            assert_eq!(theme.get("background"), Rgb::parse(background)?);
            assert_eq!(theme.get("text"), Rgb::parse(text)?);
            assert_eq!(theme.get("accent"), Rgb::parse(accent)?);
            assert_eq!(theme.get("danger"), Rgb::parse(danger)?);
            assert_eq!(theme.get("focus"), theme.get("accent"));
            assert_eq!(theme.get("selection_text"), theme.get("text"));
            assert_eq!(theme.get("status_text"), theme.get("muted"));
            for surface in ["selection_background", "status_background"] {
                assert_ne!(theme.get(surface), theme.get("background"));
                assert!(theme.get(surface).luminance() < theme.get("accent").luminance());
            }
            // The two sources use colors that are too faint for actionable
            // terminal text. Preserve palette hues instead of bleaching them
            // through the black/white fallback in legible().
            assert!(
                theme
                    .colors
                    .values()
                    .all(|color| { *color != Rgb(255, 255, 255) && *color != Rgb(0, 0, 0) })
            );
            assert_eq!(
                cloned_theme("reading-copy", &theme)?.resolve()?.colors,
                theme.colors
            );
        }
        Ok(())
    }
    #[test]
    fn rejects_duplicate_keys_and_hooks() {
        for json in [
            r##"{"schema_version":1,"id":"x","name":"X","base":"k8borg-dark","colors":{"accent":"#FFFFFF","accent":"#000000"}}"##,
            r#"{"schema_version":1,"id":"x","name":"X","base":"k8borg-dark","colors":{},"script":"evil"}"#,
        ] {
            assert!(serde_json::from_str::<ThemeFile>(json).is_err());
        }
    }
    #[test]
    fn rejects_low_contrast_and_custom_inheritance() {
        let mut f = ThemeFile {
            schema_version: 1,
            id: "custom".into(),
            name: "Custom".into(),
            base: "k8borg-dark".into(),
            colors: BTreeMap::new(),
        };
        f.colors.insert("text".into(), "#0B1220".into());
        assert!(f.resolve().is_err());
        f.colors.clear();
        f.base = "custom-other".into();
        assert!(f.resolve().is_err());
    }
}
