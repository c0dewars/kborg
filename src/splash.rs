//! Startup animation: a block-letter KBORG logo that decrypts out of glyph
//! noise, catches a diagonal light beam, types a tagline and fades out.
//! Frames are a pure function of time, so the effect is deterministic and
//! testable; the terminal renderer lives in `tui`.
use crate::themes::Rgb;

/// Total animation length in seconds.
pub const DURATION: f32 = 2.2;
const DECRYPT_END: f32 = 0.95;
const BEAM_START: f32 = 0.85;
const BEAM_END: f32 = 1.55;
const TYPE_START: f32 = 1.2;
const FADE_START: f32 = 1.85;

pub const TAGLINE: &str = "every cluster at a glance - every change reviewed";

const ORANGE: (Rgb, Rgb) = (Rgb(0xFF, 0xA4, 0x1B), Rgb(0xFF, 0x5A, 0x1F));
const GREEN: (Rgb, Rgb) = (Rgb(0x3B, 0xE0, 0x8A), Rgb(0x0F, 0xA9, 0x68));

/// ANSI Shadow style letters, six rows each.
const GLYPHS: &[(char, [&str; 6])] = &[
    (
        'K',
        [
            "██╗  ██╗",
            "██║ ██╔╝",
            "█████╔╝ ",
            "██╔═██╗ ",
            "██║  ██╗",
            "╚═╝  ╚═╝",
        ],
    ),
    (
        'B',
        [
            "██████╗ ",
            "██╔══██╗",
            "██████╔╝",
            "██╔══██╗",
            "██████╔╝",
            "╚═════╝ ",
        ],
    ),
    (
        'O',
        [
            " ██████╗ ",
            "██╔═══██╗",
            "██║   ██║",
            "██║   ██║",
            "╚██████╔╝",
            " ╚═════╝ ",
        ],
    ),
    (
        'R',
        [
            "██████╗ ",
            "██╔══██╗",
            "██████╔╝",
            "██╔══██╗",
            "██║  ██║",
            "╚═╝  ╚═╝",
        ],
    ),
    (
        'G',
        [
            " ██████╗ ",
            "██╔════╝ ",
            "██║  ███╗",
            "██║   ██║",
            "╚██████╔╝",
            " ╚═════╝ ",
        ],
    ),
];

const NOISE_UNICODE: &[char] = &[
    '░', '▒', '▓', '█', '▖', '▗', '▘', '▝', '▚', '▞', '╳', '╱', '╲', '+', '*', '#', '%', '&', '<',
    '>', '/', '\\', '0', '1',
];
const NOISE_ASCII: &[char] = &[
    '#', '%', '&', '*', '+', '=', '<', '>', '/', '\\', '0', '1', 'x', '@', '$',
];

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Cell {
    pub x: u16,
    pub y: u16,
    pub ch: char,
    pub rgb: Rgb,
}

struct Target {
    x: u16,
    y: u16,
    ch: char,
    color: Rgb,
    /// Seconds after start when this cell resolves out of the noise.
    resolve: f32,
    seed: u32,
}

pub struct Splash {
    targets: Vec<Target>,
    background: Rgb,
    text: Rgb,
    unicode: bool,
    pub width: u16,
    pub height: u16,
    tagline_x: u16,
    tagline_y: u16,
}

fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let channel = |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
    Rgb(channel(a.0, b.0), channel(a.1, b.1), channel(a.2, b.2))
}

fn hash(mut value: u32) -> u32 {
    value ^= value >> 16;
    value = value.wrapping_mul(0x7feb_352d);
    value ^= value >> 15;
    value = value.wrapping_mul(0x846c_a68b);
    value ^ (value >> 16)
}

fn unit(seed: u32) -> f32 {
    (hash(seed) % 10_000) as f32 / 10_000.0
}

fn ease_out(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    1.0 - (1.0 - t).powi(3)
}

/// The logo's ASCII form for terminals without UTF-8.
fn ascii(ch: char) -> char {
    match ch {
        '█' => '#',
        '╗' | '╔' | '╝' | '╚' | '═' | '║' => ' ',
        other => other,
    }
}

impl Splash {
    /// Lays the logo out centred in a terminal of `columns` x `rows`.
    /// Returns None when the terminal is too small for the effect.
    pub fn new(columns: u16, rows: u16, background: Rgb, text: Rgb, unicode: bool) -> Option<Self> {
        let letters = ['K', 'B', 'O', 'R', 'G'];
        let glyphs: Vec<&[&str; 6]> = letters
            .iter()
            .filter_map(|letter| GLYPHS.iter().find(|(c, _)| c == letter).map(|(_, g)| g))
            .collect();
        let logo_width: u16 = glyphs
            .iter()
            .map(|g| g[0].chars().count() as u16)
            .sum::<u16>()
            + 4;
        let width = logo_width.max(TAGLINE.len() as u16);
        let height = 6 + 2 + 1;
        if columns < width + 4 || rows < height + 4 {
            return None;
        }
        let left = (columns - logo_width) / 2;
        let top = (rows - height) / 2;
        let mut targets = Vec::new();
        let mut x0 = left;
        for (index, glyph) in glyphs.iter().enumerate() {
            let glyph_width = glyph[0].chars().count() as u16;
            for (row, line) in glyph.iter().enumerate() {
                for (column, ch) in line.chars().enumerate() {
                    if ch == ' ' {
                        continue;
                    }
                    let x = x0 + column as u16;
                    let y = top + row as u16;
                    let vertical = row as f32 / 5.0;
                    let color = match index {
                        0 => lerp(ORANGE.0, ORANGE.1, vertical),
                        1 => lerp(GREEN.0, GREEN.1, vertical),
                        _ => lerp(text, lerp(text, background, 0.35), vertical * 0.6),
                    };
                    let seed = u32::from(x) * 7919 + u32::from(y) * 104_729;
                    // Resolve left to right with jitter, all by DECRYPT_END.
                    let progress = f32::from(x - left) / f32::from(logo_width.max(1));
                    let resolve = 0.15 + progress * 0.55 + unit(seed) * 0.25;
                    targets.push(Target {
                        x,
                        y,
                        ch: if unicode { ch } else { ascii(ch) },
                        color,
                        resolve: resolve.min(DECRYPT_END),
                        seed,
                    });
                }
            }
            x0 += glyph_width + 1;
        }
        Some(Self {
            targets,
            background,
            text,
            unicode,
            width: columns,
            height: rows,
            tagline_x: (columns.saturating_sub(TAGLINE.len() as u16)) / 2,
            tagline_y: top + 8,
        })
    }

    /// Every visible cell at `t` seconds; empty after DURATION.
    pub fn frame(&self, t: f32) -> Vec<Cell> {
        if t >= DURATION {
            return Vec::new();
        }
        let fade = if t > FADE_START {
            ease_out((t - FADE_START) / (DURATION - FADE_START))
        } else {
            0.0
        };
        let noise = if self.unicode {
            NOISE_UNICODE
        } else {
            NOISE_ASCII
        };
        let tick = (t * 30.0) as u32;
        let mut cells = Vec::with_capacity(self.targets.len() + TAGLINE.len());
        for target in &self.targets {
            // Cells fade into view as glyph noise before they resolve.
            let appear = target.resolve - 0.35;
            if t < appear {
                continue;
            }
            let (ch, mut rgb) = if t < target.resolve {
                let pick = hash(target.seed ^ tick.wrapping_mul(2_654_435_761));
                let glow = ease_out((t - appear) / 0.35) * 0.55;
                (
                    noise[(pick as usize) % noise.len()],
                    lerp(self.background, lerp(target.color, self.text, 0.3), glow),
                )
            } else {
                // A short white flash as the true glyph lands.
                let since = t - target.resolve;
                let flash = 1.0 - ease_out(since / 0.18);
                (
                    target.ch,
                    lerp(target.color, Rgb(255, 255, 255), flash * 0.85),
                )
            };
            // Diagonal light beam sweeping left to right.
            if (BEAM_START..BEAM_END).contains(&t) {
                let span = f32::from(self.width) + 12.0;
                let beam = (t - BEAM_START) / (BEAM_END - BEAM_START) * span - 6.0;
                let distance = (f32::from(target.x) - beam - f32::from(target.y) * 0.6).abs();
                if distance < 4.0 {
                    rgb = lerp(rgb, Rgb(255, 255, 255), (1.0 - distance / 4.0) * 0.7);
                }
            }
            cells.push(Cell {
                x: target.x,
                y: target.y,
                ch,
                rgb: lerp(rgb, self.background, fade),
            });
        }
        // Tagline types itself out with a block cursor.
        if t >= TYPE_START {
            let typed = (((t - TYPE_START) * 70.0) as usize).min(TAGLINE.len());
            let muted = lerp(self.text, self.background, 0.4);
            for (index, ch) in TAGLINE.chars().take(typed).enumerate() {
                cells.push(Cell {
                    x: self.tagline_x + index as u16,
                    y: self.tagline_y,
                    ch,
                    rgb: lerp(muted, self.background, fade),
                });
            }
            if typed < TAGLINE.len() || ((t * 4.0) as u32).is_multiple_of(2) {
                cells.push(Cell {
                    x: self.tagline_x + typed as u16,
                    y: self.tagline_y,
                    ch: if self.unicode { '▌' } else { '_' },
                    rgb: lerp(ORANGE.0, self.background, fade),
                });
            }
        }
        cells
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const BG: Rgb = Rgb(0x10, 0x0F, 0x0F);
    const TEXT: Rgb = Rgb(0xCE, 0xCD, 0xC3);

    #[test]
    fn too_small_terminals_skip_and_layout_stays_inside_the_screen() {
        assert!(Splash::new(40, 12, BG, TEXT, true).is_none());
        let splash = Splash::new(120, 40, BG, TEXT, true).unwrap_or_else(|| panic!("fits"));
        for t in [0.3, 0.9, 1.2, 1.6, 2.0] {
            for cell in splash.frame(t) {
                assert!(cell.x < 120 && cell.y < 40, "{cell:?}");
            }
        }
    }
    #[test]
    fn logo_decrypts_into_exact_glyphs_with_brand_colors_then_fades_out() {
        let splash = Splash::new(120, 40, BG, TEXT, true).unwrap_or_else(|| panic!("fits"));
        // The decrypt starts at once: the first frame is glyph noise only.
        assert!(
            splash
                .frame(0.0)
                .iter()
                .all(|c| NOISE_UNICODE.contains(&c.ch))
        );
        // Mid-decrypt there is noise as well as resolved glyphs.
        let mid = splash.frame(0.5);
        assert!(
            mid.iter()
                .any(|c| NOISE_UNICODE.contains(&c.ch) && c.ch != '█')
        );
        // After decrypt and before the beam, every logo cell is its final glyph.
        let settled = splash.frame(0.97);
        let logo = settled
            .iter()
            .filter(|c| c.y < splash.tagline_y)
            .collect::<Vec<_>>();
        assert!(logo.iter().all(|c| "█╗╔╝╚═║".contains(c.ch)));
        // K is orange-red, B is green.
        let first = logo.iter().min_by_key(|c| (c.x, c.y)).map(|c| c.rgb);
        assert!(first.is_some_and(|rgb| rgb.0 > 200 && rgb.2 < 80));
        // Fully faded at the end, gone afterwards.
        let last = splash.frame(DURATION - 0.001);
        assert!(
            last.iter()
                .all(|c| (i16::from(c.rgb.0) - i16::from(BG.0)).abs() < 8)
        );
        assert!(splash.frame(DURATION).is_empty());
    }
    #[test]
    fn ascii_terminals_get_an_ascii_logo_and_tagline_types_in_order() {
        let splash = Splash::new(120, 40, BG, TEXT, false).unwrap_or_else(|| panic!("fits"));
        assert!(splash.frame(1.0).iter().all(|c| c.ch.is_ascii()));
        let typed = splash
            .frame(1.5)
            .into_iter()
            .filter(|c| c.y == splash.tagline_y && c.ch != '_')
            .map(|c| c.ch)
            .collect::<String>();
        assert!(!typed.is_empty() && TAGLINE.starts_with(&typed));
    }
}
