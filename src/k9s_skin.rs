//! Import a k9s skin (`skins/NAME.yaml`) as a KBorg custom theme.
//!
//! k9s colors many parts of its screen separately; a KBorg theme has a few
//! roles. The importer reads, for each role, the k9s colors with the same
//! meaning, in order, and takes the first fixed one. A color the skin leaves
//! to the terminal (`default`) or does not set falls back to KBorg Dark. Every
//! foreground then keeps its hue but is moved just far enough toward white or
//! black to read at 4.5:1, as KBorg's built-in themes are. The result is an
//! ordinary custom theme file, checked like any other, and never replaces one.
use crate::themes::{self, Rgb};
use crate::{Result, safety};
use serde_json::Value;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAX_SKIN: u64 = 256 * 1024;

/// KBorg roles and the k9s colors that mean the same, first match wins.
const ROLES: [(&str, &[&str]); 7] = [
    ("background", &["views.table.bgColor", "body.bgColor"]),
    ("text", &["views.table.fgColor", "body.fgColor"]),
    (
        "muted",
        &[
            "views.table.header.fgColor",
            "frame.menu.fgColor",
            "info.fgColor",
        ],
    ),
    (
        "accent",
        &[
            "frame.border.focusColor",
            "frame.title.highlightColor",
            "body.logoColor",
        ],
    ),
    (
        "success",
        &[
            "frame.status.addColor",
            "frame.status.completedColor",
            "frame.status.newColor",
        ],
    ),
    (
        "warning",
        &[
            "frame.status.modifyColor",
            "frame.status.highlightColor",
            "frame.status.pendingColor",
        ],
    ),
    (
        "danger",
        &["frame.status.errorColor", "frame.status.killColor"],
    ),
];

/// KBorg Dark, for roles a skin leaves to the terminal.
const FALLBACK: [&str; 7] = [
    "#0B1220", "#F8FAFC", "#94A3B8", "#2DD4BF", "#4ADE80", "#FBBF24", "#FB7185",
];

/// The W3C color names k9s (tcell) accepts besides `#RRGGBB`.
const NAMED: &[(&str, u32)] = &[
    ("aliceblue", 0xF0F8FF),
    ("antiquewhite", 0xFAEBD7),
    ("aqua", 0x00FFFF),
    ("aquamarine", 0x7FFFD4),
    ("azure", 0xF0FFFF),
    ("beige", 0xF5F5DC),
    ("bisque", 0xFFE4C4),
    ("black", 0x000000),
    ("blanchedalmond", 0xFFEBCD),
    ("blue", 0x0000FF),
    ("blueviolet", 0x8A2BE2),
    ("brown", 0xA52A2A),
    ("burlywood", 0xDEB887),
    ("cadetblue", 0x5F9EA0),
    ("chartreuse", 0x7FFF00),
    ("chocolate", 0xD2691E),
    ("coral", 0xFF7F50),
    ("cornflowerblue", 0x6495ED),
    ("cornsilk", 0xFFF8DC),
    ("crimson", 0xDC143C),
    ("cyan", 0x00FFFF),
    ("darkblue", 0x00008B),
    ("darkcyan", 0x008B8B),
    ("darkgoldenrod", 0xB8860B),
    ("darkgray", 0xA9A9A9),
    ("darkgreen", 0x006400),
    ("darkgrey", 0xA9A9A9),
    ("darkkhaki", 0xBDB76B),
    ("darkmagenta", 0x8B008B),
    ("darkolivegreen", 0x556B2F),
    ("darkorange", 0xFF8C00),
    ("darkorchid", 0x9932CC),
    ("darkred", 0x8B0000),
    ("darksalmon", 0xE9967A),
    ("darkseagreen", 0x8FBC8F),
    ("darkslateblue", 0x483D8B),
    ("darkslategray", 0x2F4F4F),
    ("darkslategrey", 0x2F4F4F),
    ("darkturquoise", 0x00CED1),
    ("darkviolet", 0x9400D3),
    ("deeppink", 0xFF1493),
    ("deepskyblue", 0x00BFFF),
    ("dimgray", 0x696969),
    ("dimgrey", 0x696969),
    ("dodgerblue", 0x1E90FF),
    ("firebrick", 0xB22222),
    ("floralwhite", 0xFFFAF0),
    ("forestgreen", 0x228B22),
    ("fuchsia", 0xFF00FF),
    ("gainsboro", 0xDCDCDC),
    ("ghostwhite", 0xF8F8FF),
    ("gold", 0xFFD700),
    ("goldenrod", 0xDAA520),
    ("gray", 0x808080),
    ("grey", 0x808080),
    ("green", 0x008000),
    ("greenyellow", 0xADFF2F),
    ("honeydew", 0xF0FFF0),
    ("hotpink", 0xFF69B4),
    ("indianred", 0xCD5C5C),
    ("indigo", 0x4B0082),
    ("ivory", 0xFFFFF0),
    ("khaki", 0xF0E68C),
    ("lavender", 0xE6E6FA),
    ("lavenderblush", 0xFFF0F5),
    ("lawngreen", 0x7CFC00),
    ("lemonchiffon", 0xFFFACD),
    ("lightblue", 0xADD8E6),
    ("lightcoral", 0xF08080),
    ("lightcyan", 0xE0FFFF),
    ("lightgoldenrodyellow", 0xFAFAD2),
    ("lightgray", 0xD3D3D3),
    ("lightgreen", 0x90EE90),
    ("lightgrey", 0xD3D3D3),
    ("lightpink", 0xFFB6C1),
    ("lightsalmon", 0xFFA07A),
    ("lightseagreen", 0x20B2AA),
    ("lightskyblue", 0x87CEFA),
    ("lightslategray", 0x778899),
    ("lightslategrey", 0x778899),
    ("lightsteelblue", 0xB0C4DE),
    ("lightyellow", 0xFFFFE0),
    ("lime", 0x00FF00),
    ("limegreen", 0x32CD32),
    ("linen", 0xFAF0E6),
    ("magenta", 0xFF00FF),
    ("maroon", 0x800000),
    ("mediumaquamarine", 0x66CDAA),
    ("mediumblue", 0x0000CD),
    ("mediumorchid", 0xBA55D3),
    ("mediumpurple", 0x9370DB),
    ("mediumseagreen", 0x3CB371),
    ("mediumslateblue", 0x7B68EE),
    ("mediumspringgreen", 0x00FA9A),
    ("mediumturquoise", 0x48D1CC),
    ("mediumvioletred", 0xC71585),
    ("midnightblue", 0x191970),
    ("mintcream", 0xF5FFFA),
    ("mistyrose", 0xFFE4E1),
    ("moccasin", 0xFFE4B5),
    ("navajowhite", 0xFFDEAD),
    ("navy", 0x000080),
    ("oldlace", 0xFDF5E6),
    ("olive", 0x808000),
    ("olivedrab", 0x6B8E23),
    ("orange", 0xFFA500),
    ("orangered", 0xFF4500),
    ("orchid", 0xDA70D6),
    ("palegoldenrod", 0xEEE8AA),
    ("palegreen", 0x98FB98),
    ("paleturquoise", 0xAFEEEE),
    ("palevioletred", 0xDB7093),
    ("papayawhip", 0xFFEFD5),
    ("peachpuff", 0xFFDAB9),
    ("peru", 0xCD853F),
    ("pink", 0xFFC0CB),
    ("plum", 0xDDA0DD),
    ("powderblue", 0xB0E0E6),
    ("purple", 0x800080),
    ("rebeccapurple", 0x663399),
    ("red", 0xFF0000),
    ("rosybrown", 0xBC8F8F),
    ("royalblue", 0x4169E1),
    ("saddlebrown", 0x8B4513),
    ("salmon", 0xFA8072),
    ("sandybrown", 0xF4A460),
    ("seagreen", 0x2E8B57),
    ("seashell", 0xFFF5EE),
    ("sienna", 0xA0522D),
    ("silver", 0xC0C0C0),
    ("skyblue", 0x87CEEB),
    ("slateblue", 0x6A5ACD),
    ("slategray", 0x708090),
    ("slategrey", 0x708090),
    ("snow", 0xFFFAFA),
    ("springgreen", 0x00FF7F),
    ("steelblue", 0x4682B4),
    ("tan", 0xD2B48C),
    ("teal", 0x008080),
    ("thistle", 0xD8BFD8),
    ("tomato", 0xFF6347),
    ("turquoise", 0x40E0D0),
    ("violet", 0xEE82EE),
    ("wheat", 0xF5DEB3),
    ("white", 0xFFFFFF),
    ("whitesmoke", 0xF5F5F5),
    ("yellow", 0xFFFF00),
    ("yellowgreen", 0x9ACD32),
];

/// One imported role: its color, where it came from, and the skin's own
/// color when KBorg had to adjust it for contrast.
pub struct Mapped {
    pub role: &'static str,
    pub color: Rgb,
    pub source: String,
    pub adjusted_from: Option<Rgb>,
}
pub struct Converted {
    pub theme: themes::ThemeFile,
    pub mapped: Vec<Mapped>,
}
pub struct Imported {
    pub id: String,
    pub path: PathBuf,
    pub mapped: Vec<Mapped>,
}

/// A k9s color: `Some` for a fixed color, `None` for `default` or empty.
fn color(text: &str) -> Result<Option<Rgb>> {
    let text = text.trim().to_ascii_lowercase();
    if text.is_empty() || text == "default" || text == "-" {
        return Ok(None);
    }
    if text.starts_with('#') {
        return Rgb::parse(&text).map(Some);
    }
    NAMED
        .iter()
        .find(|(name, _)| *name == text)
        .map(|(_, rgb)| Some(Rgb((rgb >> 16) as u8, (rgb >> 8) as u8, *rgb as u8)))
        .ok_or_else(|| {
            format!(
                "unknown k9s color {}; use #RRGGBB or a W3C color name",
                safety::safe_text(&text, 40)
            )
        })
}

/// A dotted path into the skin, matching key names without regard to case
/// (k9s skins spell `highlightColor` both ways).
fn lookup<'a>(root: &'a Value, path: &str) -> Option<&'a str> {
    path.split('.')
        .try_fold(root, |node, key| {
            node.as_object()?
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(key))
                .map(|(_, value)| value)
        })?
        .as_str()
}

fn parse(text: &str) -> Result<Value> {
    // Skins share colors through anchors and aliases; the budget bounds
    // their expansion and the document's size.
    let options = serde_saphyr::options! {
        budget: serde_saphyr::budget! {
            max_reader_input_bytes: Some(MAX_SKIN as usize), max_events: 50_000, max_aliases: 2_000,
            max_anchors: 500, max_depth: 16, max_inclusion_depth: 0, max_documents: 1,
            max_nodes: 20_000, max_total_scalar_bytes: MAX_SKIN as usize, max_merge_keys: 0,
        },
        duplicate_keys: serde_saphyr::DuplicateKeyPolicy::Error,
        merge_keys: serde_saphyr::MergeKeyPolicy::Error,
        with_snippet: false,
    };
    serde_saphyr::from_str_with_options(text, options).map_err(|error| {
        format!(
            "invalid k9s skin{}; expected a k9s: mapping of colors",
            safety::yaml_location(&error)
        )
    })
}

/// Convert skin text into a theme file named `id`; nothing is written.
pub fn convert(text: &str, id: &str, name: &str) -> Result<Converted> {
    let document = parse(text)?;
    let root = document
        .get("k9s")
        .filter(|node| node.is_object())
        .ok_or("not a k9s skin: no top-level k9s: mapping")?;
    let mut palette = [Rgb(0, 0, 0); 7];
    let mut sources = Vec::with_capacity(7);
    for (index, (role, paths)) in ROLES.iter().enumerate() {
        let mut found = None;
        for path in *paths {
            if let Some(text) = lookup(root, path)
                && let Some(rgb) = color(text).map_err(|error| format!("k9s.{path}: {error}"))?
            {
                found = Some((rgb, format!("k9s.{path}")));
                break;
            }
        }
        let (rgb, source) = found.unwrap_or_else(|| {
            (
                Rgb::parse(FALLBACK[index]).unwrap_or(Rgb(0, 0, 0)),
                "not set or terminal default: KBorg Dark".to_owned(),
            )
        });
        palette[index] = rgb;
        sources.push((*role, rgb, source));
    }
    let mut colors = themes::palette_colors(palette, true);
    // The cursor row, when the skin gives it a fixed background.
    let cursor = |key: &str| {
        lookup(root, &format!("views.table.{key}"))
            .map(color)
            .transpose()
            .map(Option::flatten)
    };
    let mut extra = Vec::new();
    if let Some(selection) = cursor("cursorBgColor")? {
        let wanted = cursor("cursorFgColor")?.unwrap_or(colors["text"]);
        let text = themes::readable_on(wanted, selection);
        colors.insert("selection_background".into(), selection);
        colors.insert("selection_text".into(), text);
        extra.push(Mapped {
            role: "selection_background",
            color: selection,
            source: "k9s.views.table.cursorBgColor".into(),
            adjusted_from: None,
        });
        extra.push(Mapped {
            role: "selection_text",
            color: text,
            source: "k9s.views.table.cursorFgColor".into(),
            adjusted_from: (text != wanted).then_some(wanted),
        });
    }
    let mut mapped: Vec<Mapped> = sources
        .into_iter()
        .map(|(role, wanted, source)| {
            let color = colors[role];
            Mapped {
                role,
                color,
                source,
                adjusted_from: (color != wanted).then_some(wanted),
            }
        })
        .collect();
    mapped.extend(extra);
    let theme = themes::ThemeFile {
        schema_version: 1,
        id: id.into(),
        name: name.into(),
        base: "k8borg-dark".into(),
        colors: colors
            .into_iter()
            .map(|(role, rgb)| (role, rgb.hex()))
            .collect::<BTreeMap<_, _>>(),
    };
    theme.resolve()?;
    Ok(Converted { theme, mapped })
}

/// The theme id for a skin file: `k9s-` and its name, lowercased, with
/// anything else than letters, digits, `-` and `_` turned into `-`.
pub fn default_id(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("skin")
        .to_ascii_lowercase();
    let clean: String = stem
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .take(58)
        .collect();
    format!("k9s-{}", clean.trim_matches('-'))
}

/// Read a skin file and save it as a new custom theme.
pub fn import(path: &Path, id: Option<&str>) -> Result<Imported> {
    let bytes = safety::read_small(path, MAX_SKIN)?;
    let text = std::str::from_utf8(&bytes).map_err(|_| "k9s skin is not UTF-8 text")?;
    let id = id.map_or_else(|| default_id(path), str::to_owned);
    if !safety::valid_id(&id) {
        return Err("theme id must be 1..63 letters, digits, - or _; pass --id ID".into());
    }
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("skin");
    let name = format!("{} (k9s skin)", safety::safe_text(stem, 60));
    let Converted { theme, mapped } = convert(text, &id, &name)?;
    let path = themes::save_new(&theme).map_err(|error| {
        if error.contains("exist") {
            format!("a theme named {id} already exists; pass --id NEW-ID or remove it first")
        } else {
            error
        }
    })?;
    Ok(Imported { id, path, mapped })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shaped like the published Catppuccin Mocha k9s skin: anchors for the
    /// palette, aliases for the parts.
    const MOCHA: &str = r##"
foreground: &foreground "#cdd6f4"
background: &background "#1e1e2e"
current_line: &current_line "#f9e2af"
selection: &selection "#313244"
comment: &comment "#6c7086"
cyan: &cyan "#89dceb"
green: &green "#a6e3a1"
orange: &orange "#fab387"
magenta: &magenta "#cba6f7"
blue: &blue "#89b4fa"
red: &red "#f38ba8"
k9s:
  body:
    fgColor: *foreground
    bgColor: *background
    logoColor: *blue
  frame:
    border:
      fgColor: *selection
      focusColor: *blue
    menu:
      fgColor: *foreground
    status:
      newColor: *cyan
      modifyColor: *current_line
      addColor: *green
      errorColor: *red
      highlightcolor: *orange
      killColor: *comment
      completedColor: *comment
  views:
    table:
      fgColor: *foreground
      bgColor: default
      cursorFgColor: *background
      cursorBgColor: *blue
      header:
        fgColor: *comment
"##;

    fn role(converted: &Converted, role: &str) -> String {
        converted.theme.colors[role].clone()
    }

    #[test]
    fn a_catppuccin_skin_becomes_a_legible_theme() -> Result<()> {
        let converted = convert(MOCHA, "k9s-mocha", "mocha (k9s skin)")?;
        // `default` table background falls through to body.bgColor.
        assert_eq!(role(&converted, "background"), "#1E1E2E");
        assert_eq!(role(&converted, "text"), "#CDD6F4");
        assert_eq!(role(&converted, "accent"), "#89B4FA");
        assert_eq!(role(&converted, "success"), "#A6E3A1");
        assert_eq!(role(&converted, "warning"), "#F9E2AF");
        assert_eq!(role(&converted, "danger"), "#F38BA8");
        assert_eq!(role(&converted, "selection_background"), "#89B4FA");
        assert_eq!(role(&converted, "selection_text"), "#1E1E2E");
        // The header gray is too faint for text: kept in hue, made legible,
        // and reported.
        let muted = converted
            .mapped
            .iter()
            .find(|m| m.role == "muted")
            .ok_or("muted")?;
        assert_eq!(muted.source, "k9s.views.table.header.fgColor");
        assert_eq!(muted.adjusted_from, Some(Rgb(0x6c, 0x70, 0x86)));
        assert_ne!(muted.color, Rgb(255, 255, 255), "lightened, not replaced");
        assert!(muted.color.2 > muted.color.0, "keeps its blue-gray hue");
        let theme = converted.theme.resolve()?;
        theme.validate()?;
        Ok(())
    }

    #[test]
    fn named_and_missing_colors_fall_back_sensibly() -> Result<()> {
        let skin = "k9s:\n  body:\n    fgColor: dodgerblue\n    bgColor: black\n  frame:\n    status:\n      errorColor: orangered\n";
        let converted = convert(skin, "k9s-named", "named")?;
        assert_eq!(role(&converted, "background"), "#000000");
        assert_eq!(role(&converted, "danger"), "#FF4500");
        let accent = converted
            .mapped
            .iter()
            .find(|m| m.role == "accent")
            .ok_or("accent")?;
        assert!(accent.source.contains("KBorg Dark"), "{}", accent.source);
        Ok(())
    }

    #[test]
    fn bad_skins_are_refused_with_a_reason() {
        let refused = |text: &str| convert(text, "k9s-x", "x").err().unwrap_or_default();
        assert!(refused("skin:\n  body: {}\n").contains("no top-level k9s"));
        assert!(refused("k9s:\n  body:\n    fgColor: notacolor\n").contains("k9s.body.fgColor"));
        assert!(
            refused("k9s:\n  body:\n    fgColor: red\n    fgColor: blue\n")
                .contains("invalid k9s skin")
        );
        // An alias bomb stops at the budget instead of expanding.
        let mut bomb = String::from("a: &a [x, x, x, x, x, x, x, x, x, x]\n");
        for level in 1..12 {
            let previous = (b'a' + level - 1) as char;
            let name = (b'a' + level) as char;
            bomb.push_str(&format!(
                "{name}: &{name} [*{previous}, *{previous}, *{previous}, *{previous}, *{previous}, *{previous}, *{previous}, *{previous}, *{previous}, *{previous}]\n"
            ));
        }
        bomb.push_str("k9s: {}\n");
        assert!(refused(&bomb).contains("invalid k9s skin"));
        // A built-in id cannot be taken.
        assert!(
            convert("k9s:\n  body:\n    bgColor: black\n", "dracula", "x")
                .err()
                .is_some_and(|e| e.contains("reserved"))
        );
    }

    #[test]
    fn skin_file_names_become_theme_ids() {
        assert_eq!(
            default_id(Path::new("/x/catppuccin-mocha.yaml")),
            "k9s-catppuccin-mocha"
        );
        assert_eq!(default_id(Path::new("My Skin.v2.yml")), "k9s-my-skin-v2");
        assert!(safety::valid_id(&default_id(Path::new(&"a".repeat(200)))));
    }
}
