use crate::{Result, safety};
use crossterm::style::Color;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
        Color::Rgb {
            r: self.0,
            g: self.1,
            b: self.2,
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
        "K8Borg Dark",
        [
            "#0B1220", "#F8FAFC", "#94A3B8", "#2DD4BF", "#4ADE80", "#FBBF24", "#FB7185",
        ],
    ),
    (
        "k8borg-light",
        "K8Borg Light",
        [
            "#F8FAFC", "#0B1220", "#475569", "#0F766E", "#166534", "#92400E", "#BE123C",
        ],
    ),
    (
        "k8borg-high-contrast",
        "K8Borg High Contrast",
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
            let accent = colors.get("accent").copied().unwrap_or(Rgb(255, 255, 255));
            colors.insert("focus".into(), accent);
            colors.insert("selection_background".into(), accent);
            colors.insert("selection_text".into(), legible(bg, accent));
            colors.insert("status_background".into(), bg);
            colors.insert(
                "status_text".into(),
                colors.get("warning").copied().unwrap_or(accent),
            );
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
    read().unwrap_or_else(|_| "catppuccin-mocha".into())
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
    let dir = safety::config_dir()?;
    safety::checked_path(&dir)?;
    std::fs::create_dir_all(&dir).map_err(|_| "cannot create config directory")?;
    let path = dir.join("appearance.json");
    safety::checked_path(&path)?;
    let temp = dir.join(format!("appearance-{}.tmp", std::process::id()));
    let bytes = serde_json::to_vec(preference).map_err(|_| "cannot serialize preference")?;
    safety::write_new(&temp, &bytes)?;
    // Windows rename cannot replace; preference is non-sensitive and fallback-safe on interruption.
    if path.exists() {
        std::fs::remove_file(&path).map_err(|_| "cannot replace preference")?;
    }
    std::fs::rename(temp, path).map_err(|_| "cannot commit preference".into())
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
        use_theme("catppuccin-mocha")?;
    }
    std::fs::remove_file(path).map_err(|_| "cannot remove custom theme".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn all_builtins_meet_fixed_palette_targets() {
        assert_eq!(builtins().len(), 18);
        for t in builtins() {
            assert!(t.validate().is_ok(), "{}", t.id);
        }
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
