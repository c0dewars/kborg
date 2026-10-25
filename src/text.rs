//! Terminal cell width for display text.
//!
//! Two modes. **Unicode** draws real glyphs: text is split into grapheme
//! clusters (a simplified UAX #29 that covers combining marks, emoji ZWJ
//! sequences, modifiers, variation selectors and flag pairs) and each cluster
//! takes 0, 1 or 2 cells (East Asian wide and emoji take 2). **ASCII** is the
//! conservative fallback for non-UTF-8 terminals or `text: ascii` in ui.yaml:
//! every non-ASCII character is drawn as `?`, one cell each.
//!
//! Input must already have passed `safety::safe_text` (no controls, bidi or
//! format characters). Truncation and padding never split a cluster, and a
//! cluster that would cross the right edge is replaced by spaces, so columns
//! stay aligned. Approval screens do not use this path for their data: they
//! keep the reversible ASCII escaping of `safety::review_display`.
use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

static UNICODE: AtomicBool = AtomicBool::new(false);
thread_local! {
    static OVERRIDE: Cell<Option<bool>> = const { Cell::new(None) };
}

/// Combining marks, ZWJ and variation selectors after one base character are
/// kept up to this many characters per cluster; the rest are dropped so a
/// "Zalgo" name cannot smear over neighbouring rows or inflate work.
const MAX_CLUSTER_CHARS: usize = 16;

/// Select the process-wide mode (startup and `:ui reload`).
pub fn set_unicode(enabled: bool) {
    UNICODE.store(enabled, Ordering::Relaxed);
}

/// True when real glyphs are drawn.
pub fn unicode() -> bool {
    OVERRIDE
        .with(Cell::get)
        .unwrap_or_else(|| UNICODE.load(Ordering::Relaxed))
}

/// Run `f` with a mode for the current thread only (tests and isolated
/// renderers), restoring the previous mode afterwards.
pub fn with_mode<R>(unicode: bool, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<bool>);
    impl Drop for Restore {
        fn drop(&mut self) {
            OVERRIDE.with(|cell| cell.set(self.0));
        }
    }
    let _restore = Restore(OVERRIDE.with(|cell| cell.replace(Some(unicode))));
    f()
}

/// Extended_Pictographic, approximated by the blocks that hold emoji.
pub fn pictographic(c: char) -> bool {
    matches!(c,
        '\u{00a9}' | '\u{00ae}' | '\u{203c}' | '\u{2049}' | '\u{2122}' | '\u{2139}' |
        '\u{2194}'..='\u{21aa}' | '\u{231a}'..='\u{23ff}' | '\u{24c2}' |
        '\u{25aa}'..='\u{25fe}' | '\u{2600}'..='\u{27bf}' | '\u{2934}' | '\u{2935}' |
        '\u{2b05}'..='\u{2b55}' | '\u{3030}' | '\u{303d}' | '\u{3297}' | '\u{3299}' |
        '\u{1f000}'..='\u{1f0ff}' | '\u{1f10d}'..='\u{1f1ad}' |
        '\u{1f200}'..='\u{1faff}' | '\u{1fc00}'..='\u{1fffd}')
}
fn regional(c: char) -> bool {
    ('\u{1f1e6}'..='\u{1f1ff}').contains(&c)
}
fn modifier(c: char) -> bool {
    ('\u{1f3fb}'..='\u{1f3ff}').contains(&c)
}
/// Characters that attach to the preceding one: combining and enclosing
/// marks, variation selectors, ZWJ and Hangul vowel/trailing jamo.
fn extend(c: char) -> bool {
    !c.is_control() && c.width() == Some(0)
}

/// Grapheme clusters of `text`, as byte slices.
pub fn graphemes(text: &str) -> Graphemes<'_> {
    Graphemes { text }
}
pub struct Graphemes<'a> {
    text: &'a str,
}
impl<'a> Iterator for Graphemes<'a> {
    type Item = &'a str;
    fn next(&mut self) -> Option<&'a str> {
        let mut chars = self.text.char_indices();
        let (_, first) = chars.next()?;
        let mut end = first.len_utf8();
        let mut previous = first;
        let emoji = pictographic(first);
        let mut flag = regional(first);
        for (index, c) in chars {
            let joins = if std::mem::take(&mut flag) && regional(c) {
                true
            } else if previous == '\u{200d}' {
                emoji && pictographic(c)
            } else {
                extend(c) || (emoji && modifier(c))
            };
            if !joins {
                break;
            }
            previous = c;
            end = index + c.len_utf8();
        }
        let (cluster, rest) = self.text.split_at(end);
        self.text = rest;
        Some(cluster)
    }
}

/// Cells one cluster occupies in Unicode mode (0, 1 or 2).
pub fn cluster_width(cluster: &str) -> usize {
    let mut chars = cluster.chars();
    match chars.next() {
        None => 0,
        Some(first) if first.is_control() || extend(first) => 0,
        Some(first) if regional(first) => 2,
        Some(first) => {
            let base = first.width().unwrap_or(0);
            if chars.as_str().is_empty() {
                base
            } else {
                cluster.width().clamp(base.max(1), 2)
            }
        }
    }
}

/// Display cells of already-sanitized `text` in the current mode.
pub fn width(text: &str) -> usize {
    if unicode() {
        graphemes(text).map(cluster_width).sum()
    } else {
        text.chars().filter(|c| *c != '\u{200d}').count()
    }
}

/// Maps already-sanitized text to the glyphs that will be painted: `?` per
/// non-ASCII character in ASCII mode; in Unicode mode orphan zero-width
/// clusters are dropped and over-long clusters capped.
fn clusters(text: &str) -> impl Iterator<Item = (String, usize)> + '_ {
    let unicode = unicode();
    let ascii = (!unicode).then(|| {
        text.chars()
            .filter(|c| *c != '\u{200d}')
            .map(|c| (if c.is_ascii() { c } else { '?' }).to_string())
    });
    let wide = unicode.then(|| {
        graphemes(text).filter_map(|cluster| {
            let cells = cluster_width(cluster);
            (cells > 0).then(|| (cluster.chars().take(MAX_CLUSTER_CHARS).collect(), cells))
        })
    });
    ascii
        .into_iter()
        .flatten()
        .map(|glyph| (glyph, 1))
        .chain(wide.into_iter().flatten())
}

/// Fit sanitized text into `width` cells. Longer text ends in `~`; a wide
/// cluster that does not fit is never split.
pub fn fit(text: &str, width: usize) -> String {
    let mut output = String::new();
    let mut used = 0usize;
    // Width of `output` before each pushed glyph, to back up for the marker.
    let mut marks: Vec<(usize, usize)> = Vec::new();
    for (glyph, cells) in clusters(text) {
        if used + cells > width {
            if width == 0 {
                return String::new();
            }
            // Drop glyphs until one cell is free for the `~` marker.
            while used + 1 > width {
                let Some((length, before)) = marks.pop() else {
                    break;
                };
                output.truncate(length);
                used = before;
            }
            output.push('~');
            return output;
        }
        marks.push((output.len(), used));
        output.push_str(&glyph);
        used += cells;
    }
    output
}

/// `fit` and then pad with spaces to exactly `width` cells.
pub fn pad(text: &str, width: usize) -> String {
    let mut output = fit(text, width);
    let used = self::width(&output);
    output.extend(std::iter::repeat_n(' ', width.saturating_sub(used)));
    output
}

/// Characters of untrusted input worth sanitizing for `width` cells; emoji
/// sequences use several characters per two cells.
fn budget(width: usize) -> usize {
    width.saturating_mul(8).saturating_add(16)
}
/// Untrusted text sanitized by `safety::safe_text` and fitted to `width`.
pub fn shown(text: &str, width: usize) -> String {
    fit(&crate::safety::safe_text(text, budget(width)), width)
}
/// Untrusted text sanitized and padded to exactly `width` cells: a table
/// column. Use instead of `format!("{:<width$}")`, which counts characters.
pub fn column(text: &str, width: usize) -> String {
    pad(&crate::safety::safe_text(text, budget(width)), width)
}

/// Cells `skip..skip+width` of sanitized text, without a truncation marker
/// (horizontal panning). A wide cluster cut by either edge becomes spaces.
pub fn window(text: &str, skip: usize, width: usize) -> String {
    let mut output = String::new();
    let mut at = 0usize;
    let mut used = 0usize;
    for (glyph, cells) in clusters(text) {
        if used >= width {
            break;
        }
        let start = at;
        at += cells;
        if at <= skip {
            continue;
        }
        if start < skip || used + cells > width {
            let blank = (at - start.max(skip)).min(width - used);
            output.extend(std::iter::repeat_n(' ', blank));
            used += blank;
            continue;
        }
        output.push_str(&glyph);
        used += cells;
    }
    output
}

/// Split sanitized text into rows of at most `width` cells, never inside a
/// cluster. ASCII mode chunks characters exactly as before.
pub fn wrap(line: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut rows = Vec::new();
    let mut current = String::new();
    let mut used = 0usize;
    for (glyph, cells) in clusters(line) {
        if used + cells > width && !current.is_empty() {
            rows.push(std::mem::take(&mut current));
            used = 0;
        }
        current.push_str(&glyph);
        used += cells;
    }
    if !current.is_empty() || rows.is_empty() {
        rows.push(current);
    }
    rows
}

/// Number of rows `wrap` produces, without allocating them.
pub fn rows(line: &str, width: usize) -> usize {
    let width = width.max(1);
    let (mut rows, mut used) = (1usize, 0usize);
    for (_, cells) in clusters(line) {
        if used + cells > width && used > 0 {
            rows += 1;
            used = 0;
        }
        used += cells;
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unicode<R>(f: impl FnOnce() -> R) -> R {
        with_mode(true, f)
    }

    #[test]
    fn clusters_and_widths_follow_terminal_cells() {
        let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}\u{200d}\u{1f466}";
        let flag = "\u{1f1ef}\u{1f1f5}";
        let accent = "e\u{301}";
        let thumbs = "\u{1f44d}\u{1f3fd}";
        let heart = "\u{2764}\u{fe0f}";
        for (text, clusters, cells) in [
            ("abc", 3, 3),
            ("東京", 2, 4),
            (family, 1, 2),
            (flag, 1, 2),
            (accent, 1, 1),
            (thumbs, 1, 2),
            (heart, 1, 2),
            ("שלום", 4, 4),
            ("مرحبا", 5, 5),
            ("\u{1f1ef}\u{1f1f5}\u{1f1fa}", 2, 4),
        ] {
            assert_eq!(graphemes(text).count(), clusters, "{text:?}");
            assert_eq!(unicode(|| width(text)), cells, "{text:?}");
        }
        assert_eq!(graphemes(family).next(), Some(family));
        assert_eq!(
            unicode(|| width("\u{301}x")),
            1,
            "orphan mark takes no cell"
        );
        assert_eq!(unicode(|| fit("\u{301}x", 4)), "x");
    }

    #[test]
    fn fit_and_pad_never_split_clusters() {
        unicode(|| {
            assert_eq!(fit("東京都", 6), "東京都");
            assert_eq!(fit("東京都", 5), "東京~");
            assert_eq!(fit("東京都", 4), "東~");
            assert_eq!(fit("a東京", 3), "a~");
            assert_eq!(pad("a東京", 3), "a~ ");
            assert_eq!(width(&pad("東京都x", 5)), 5);
            assert_eq!(fit("cafe\u{301}s", 4), "caf~");
            assert_eq!(fit("cafe\u{301}", 4), "cafe\u{301}");
            let family = "\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}";
            assert_eq!(fit(&format!("{family}{family}"), 3), format!("{family}~"));
            assert_eq!(fit("x", 0), "");
            for text in [
                "日本語のポッド名",
                "e\u{301}e\u{301}e\u{301}e\u{301}",
                "🇯🇵🇺🇸🇩🇪",
            ] {
                for cells in 0..12 {
                    assert!(width(&fit(text, cells)) <= cells);
                    assert_eq!(width(&pad(text, cells)), cells);
                }
            }
        });
    }

    #[test]
    fn window_and_wrap_blank_cut_wide_glyphs() {
        unicode(|| {
            assert_eq!(window("a東京b", 2, 3), " 京");
            assert_eq!(window("a東京b", 1, 2), "東");
            assert_eq!(window("a東京b", 0, 2), "a ");
            assert_eq!(wrap("東京都", 3), ["東", "京", "都"]);
            assert_eq!(wrap("ab東", 3), ["ab", "東"]);
            assert_eq!(wrap("", 3), [""]);
            for (text, cells) in [("東京都", 3), ("ab東", 3), ("", 3), ("abcdefg", 2)] {
                assert_eq!(rows(text, cells), wrap(text, cells).len());
            }
        });
    }

    #[test]
    fn ascii_mode_keeps_question_marks() {
        with_mode(false, || {
            assert_eq!(fit("東京", 8), "??");
            assert_eq!(fit("abcdef", 4), "abc~");
            assert_eq!(pad("é", 3), "?  ");
            let family = "\u{1f468}\u{200d}\u{1f469}";
            assert_eq!(fit(family, 8), "??");
            assert_eq!(wrap("abcde", 2), ["ab", "cd", "e"]);
        });
    }

    #[test]
    fn zalgo_clusters_are_capped() {
        let zalgo = format!("a{}", "\u{301}".repeat(200));
        let shown = unicode(|| fit(&zalgo, 4));
        assert_eq!(shown.chars().count(), MAX_CLUSTER_CHARS);
    }
}
