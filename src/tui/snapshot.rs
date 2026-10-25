//! The visible screen as plain text, for `:ask`. The frame is rendered with
//! the normal renderer and replayed into a character grid, so the text is
//! exactly what the user sees (already redacted and sanitized for display).
use super::*;

/// Render `app` at `width`×`height` and return the visible text, one line
/// per row, with trailing spaces and the bottom key-hint row removed.
pub(super) fn screen_text(app: &App, width: u16, height: u16) -> Result<String> {
    let mut output = Vec::new();
    render(app, width, height, &mut output)?;
    let output = String::from_utf8_lossy(&output);
    let (w, h) = (usize::from(width), usize::from(height));
    let mut cells = vec![vec![' '; w]; h];
    let (mut x, mut y) = (0usize, 0usize);
    let mut chars = output.chars();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            // CSI: ESC [ parameters final-letter. Only cursor moves matter.
            if chars.next() != Some('[') {
                continue;
            }
            let mut parameters = String::new();
            let final_byte = loop {
                match chars.next() {
                    Some(c) if c.is_ascii_alphabetic() => break Some(c),
                    Some(c) => parameters.push(c),
                    None => break None,
                }
            };
            match final_byte {
                Some('H') => {
                    let mut values = parameters
                        .split(';')
                        .map(|value| value.parse::<usize>().unwrap_or(1));
                    y = values.next().unwrap_or(1).saturating_sub(1);
                    x = values.next().unwrap_or(1).saturating_sub(1);
                }
                Some('J') => cells.iter_mut().for_each(|row| row.fill(' ')),
                _ => {}
            }
            continue;
        }
        if y < h && x < w {
            // Box-drawing and chart glyphs become ASCII so the text stays plain.
            cells[y][x] = match character {
                c if c.is_ascii() => c,
                '│' | '┃' => '|',
                '─' | '━' | '┈' => '-',
                '╭' | '╮' | '╰' | '╯' | '┌' | '┐' | '└' | '┘' => '+',
                _ => '#',
            };
        }
        x += 1;
    }
    let mut lines: Vec<String> = cells
        .into_iter()
        .map(|row| row.into_iter().collect::<String>().trim_end().to_owned())
        .collect();
    // The last row is the key-hint row; it describes KBorg, not the cluster.
    lines.pop();
    while lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    Ok(lines.join("\n"))
}
