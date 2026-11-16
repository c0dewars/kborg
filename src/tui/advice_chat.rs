//! Render the local Copilot conversation with its original screen context.
use super::{App, AskThread, FOLLOW_UP_LABEL, Modal, Rect, cell, panel, ui_color};
use crate::{Result, safety, text};
use crossterm::{
    event::KeyCode,
    queue,
    style::{Attribute, SetAttribute},
};
use std::{
    cell::{Ref, RefCell},
    io::Write,
};

#[derive(Default)]
pub(super) struct Scroll {
    anchor: Anchor,
    // Each completed turn installs a fresh Scroll. Inside a turn, only a
    // change in width or Unicode mode requires wrapping the text again.
    projection: RefCell<Projection>,
}

pub(super) fn view(app: &App) -> Option<(&Scroll, Option<&str>)> {
    match &app.modal {
        Modal::AdviceChat(scroll) => Some((scroll, None)),
        Modal::Input(FOLLOW_UP_LABEL, draft) => match app.subviews.last() {
            Some(Modal::AdviceChat(scroll)) => Some((scroll, Some(draft))),
            _ => None,
        },
        _ => None,
    }
}

#[derive(Default)]
struct Projection {
    key: Option<(usize, bool)>,
    transcript: Transcript,
}

#[derive(Default)]
enum Anchor {
    // Long answers begin at the top of the latest exchange.
    #[default]
    Latest,
    Row(usize),
    End,
    Match(usize),
}

struct Layout {
    panel: Rect,
    columns: u16,
    body_rows: usize,
}

impl Layout {
    fn new(width: u16, height: u16, top: u16) -> Self {
        let panel_width = width.min(110);
        let panel_height = height.saturating_sub(top + 2);
        Self {
            panel: Rect {
                x: (width - panel_width) / 2,
                y: top,
                width: panel_width,
                height: panel_height,
            },
            columns: panel_width.saturating_sub(4),
            // Border, fixed source row, transcript, border.
            body_rows: usize::from(panel_height.saturating_sub(3)),
        }
    }
}

impl Scroll {
    fn transcript(&self, thread: &AskThread, width: usize) -> Ref<'_, Transcript> {
        let key = Some((width, text::unicode()));
        if self.projection.borrow().key != key {
            *self.projection.borrow_mut() = Projection {
                key,
                transcript: Transcript::new(thread, width),
            };
        }
        Ref::map(self.projection.borrow(), |projection| {
            &projection.transcript
        })
    }

    fn position(&self, transcript: &Transcript, height: usize) -> usize {
        let last = transcript.lines.len().saturating_sub(height.max(1));
        match self.anchor {
            Anchor::Latest => transcript.latest.min(last),
            Anchor::Row(index) | Anchor::Match(index) => index.min(last),
            Anchor::End => last,
        }
    }

    fn search_position(&self, transcript: &Transcript, height: usize) -> usize {
        if let Anchor::Match(index) = self.anchor {
            index.min(transcript.lines.len().saturating_sub(1))
        } else {
            self.position(transcript, height)
        }
    }

    pub(super) fn jump_to_match(&mut self, index: usize) {
        self.anchor = Anchor::Match(index);
    }

    pub(super) fn key(
        &mut self,
        thread: &AskThread,
        code: KeyCode,
        width: u16,
        height: u16,
        top: u16,
    ) {
        let layout = Layout::new(width, height, top);
        let (current, last) = {
            let transcript = self.transcript(thread, usize::from(layout.columns));
            (
                self.position(&transcript, layout.body_rows),
                transcript
                    .lines
                    .len()
                    .saturating_sub(layout.body_rows.max(1)),
            )
        };
        let page = layout.body_rows.saturating_sub(1).max(1);
        let offset = match code {
            KeyCode::Down | KeyCode::Char('j') => current.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => current.saturating_sub(1),
            KeyCode::PageDown => current.saturating_add(page),
            KeyCode::PageUp => current.saturating_sub(page),
            KeyCode::Home => {
                self.anchor = Anchor::Row(0);
                return;
            }
            KeyCode::End => {
                self.anchor = Anchor::End;
                return;
            }
            _ => return,
        };
        self.anchor = Anchor::Row(offset.min(last));
    }
}

/// Search uses the same wrapped rows as the conversation pane.
pub(super) fn search_document(
    thread: &AskThread,
    scroll: &Scroll,
    width: u16,
    height: u16,
    top: u16,
) -> (String, usize) {
    let layout = Layout::new(width, height, top);
    let transcript = scroll.transcript(thread, usize::from(layout.columns));
    let offset = scroll.search_position(&transcript, layout.body_rows);
    let text = transcript
        .lines
        .iter()
        .map(|line| line.text.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    (text, offset)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tone {
    You,
    Question,
    Copilot,
    Paragraph,
    Heading,
    Code,
}

struct Line {
    text: String,
    tone: Tone,
}

#[derive(Default)]
struct Transcript {
    lines: Vec<Line>,
    latest: usize,
}

impl Transcript {
    fn new(thread: &AskThread, width: usize) -> Self {
        let mut transcript = Self {
            lines: Vec::new(),
            latest: 0,
        };
        for (question, answer) in &thread.turns {
            if !transcript.lines.is_empty() {
                transcript.line(String::new(), Tone::Paragraph);
            }
            transcript.latest = transcript.lines.len();
            transcript.line("You".to_owned(), Tone::You);
            transcript.prose(question, "", width, Tone::Question);
            transcript.line(String::new(), Tone::Paragraph);
            transcript.line("Copilot".to_owned(), Tone::Copilot);
            transcript.answer(answer, width);
        }
        transcript
    }

    fn line(&mut self, text: String, tone: Tone) {
        self.lines.push(Line { text, tone });
    }

    fn prose(&mut self, value: &str, prefix: &str, width: usize, tone: Tone) {
        let available = width.saturating_sub(text::width(prefix)).max(2);
        let indent = " ".repeat(text::width(prefix));
        let value = safety::safe_text(value, 4096);
        let mut words = value.split_whitespace();
        let Some(first) = words.next() else {
            self.line(prefix.to_owned(), tone);
            return;
        };
        let mut current = first.to_owned();
        let mut prefix = prefix;
        for word in words {
            if text::width(&current) + 1 + text::width(word) > available {
                for part in text::wrap(&current, available) {
                    self.line(format!("{prefix}{part}"), tone);
                    prefix = &indent;
                }
                current.clear();
            } else {
                current.push(' ');
            }
            current.push_str(word);
        }
        for part in text::wrap(&current, available) {
            self.line(format!("{prefix}{part}"), tone);
            prefix = &indent;
        }
    }

    fn answer(&mut self, answer: &str, width: usize) {
        let mut fenced = None;
        for raw in answer.lines() {
            let trimmed = raw.trim_start();
            let fence = fence(trimmed);
            if let Some((marker, length)) = fenced {
                if fence.is_some_and(|(end, count, rest)| {
                    end == marker && count >= length && rest.trim().is_empty()
                }) {
                    fenced = None;
                    continue;
                }
                let line = safety::safe_text(raw, 4096);
                for part in text::wrap(&line, width.saturating_sub(2).max(2)) {
                    self.line(format!("| {part}"), Tone::Code);
                }
                continue;
            }
            if let Some((marker, length, language)) = fence {
                fenced = Some((marker, length));
                let language = language.trim();
                if !language.is_empty()
                    && language.len() <= 20
                    && language
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'+'))
                {
                    self.line(format!("| {language}"), Tone::Code);
                }
                continue;
            }
            let hashes = trimmed.bytes().take_while(|&byte| byte == b'#').count();
            if (1..=6).contains(&hashes)
                && let Some(heading) = trimmed[hashes..].strip_prefix(' ')
            {
                self.prose(heading, "", width, Tone::Heading);
            } else if let Some(item) = trimmed
                .strip_prefix("- ")
                .or_else(|| trimmed.strip_prefix("* "))
                .or_else(|| trimmed.strip_prefix("+ "))
            {
                self.prose(item, "  - ", width, Tone::Paragraph);
            } else {
                let digits = trimmed.bytes().take_while(u8::is_ascii_digit).count();
                if (1..=9).contains(&digits)
                    && let Some(item) = trimmed[digits..]
                        .strip_prefix(". ")
                        .or_else(|| trimmed[digits..].strip_prefix(") "))
                {
                    let prefix = format!("  {} ", &trimmed[..digits + 1]);
                    self.prose(item, &prefix, width, Tone::Paragraph);
                } else {
                    self.prose(trimmed, "", width, Tone::Paragraph);
                }
            }
        }
    }
}

fn fence(line: &str) -> Option<(u8, usize, &str)> {
    let marker = *line.as_bytes().first()?;
    let count = line.bytes().take_while(|&byte| byte == marker).count();
    (matches!(marker, b'`' | b'~') && count >= 3).then(|| (marker, count, &line[count..]))
}

fn glyph_match(line: &str, start: usize, end: usize) -> (usize, usize) {
    let mut left = start;
    let mut offset = 0;
    for glyph in text::graphemes(line) {
        let next = offset + glyph.len();
        if (offset..next).contains(&start) {
            left = offset;
        }
        if next >= end {
            return (left, next);
        }
        offset = next;
    }
    (left, end)
}

pub(super) fn paint(
    app: &App,
    scroll: &Scroll,
    draft: Option<&str>,
    width: u16,
    height: u16,
    top: u16,
    out: &mut impl Write,
) -> Result<()> {
    let layout = Layout::new(width, height, top);
    let color = |role| ui_color(app, role);
    let background = color("background");
    let code_background = if app.options.color && !app.theme.terminal {
        super::blend(app.theme.get("background"), app.theme.get("text"), 8).color()
    } else {
        background
    };
    if layout.panel.width < width {
        for y in top..height - 2 {
            super::row(out, y, width, "", color("text"), background).map_err(|_| "draw failed")?;
        }
    }
    panel(
        out,
        layout.panel,
        "Copilot chat",
        true,
        color("border"),
        background,
        &app.ui,
    )
    .map_err(|_| "draw failed")?;
    let x = layout.panel.x + 2;
    let Some(thread) = &app.ask_thread else {
        cell(
            out,
            x,
            top + 1,
            layout.columns,
            "No conversation available.",
            color("muted"),
            background,
        )
        .map_err(|_| "draw failed")?;
        out.flush().map_err(|_| "draw failed")?;
        return Ok(());
    };
    let columns = usize::from(layout.columns);
    let source = text::shown_middle(&format!("Captured: {}", thread.source), columns);
    cell(
        out,
        x,
        top + 1,
        layout.columns,
        &source,
        color("muted"),
        background,
    )
    .map_err(|_| "draw failed")?;

    let transcript = scroll.transcript(thread, columns);
    let offset = scroll.position(&transcript, layout.body_rows);
    let query = app.doc_search.query();
    for (row, line) in transcript
        .lines
        .iter()
        .skip(offset)
        .take(layout.body_rows)
        .enumerate()
    {
        let (foreground, surface) = match line.tone {
            Tone::You => (color("accent"), code_background),
            Tone::Question => (color("text"), code_background),
            Tone::Copilot | Tone::Heading => (color("accent"), background),
            Tone::Code => (color("text"), code_background),
            Tone::Paragraph => (color("text"), background),
        };
        let bold = matches!(line.tone, Tone::You | Tone::Copilot | Tone::Heading);
        if bold {
            queue!(out, SetAttribute(Attribute::Bold)).map_err(|_| "draw failed")?;
        }
        let y = top + 2 + row as u16;
        cell(out, x, y, layout.columns, &line.text, foreground, surface)
            .map_err(|_| "draw failed")?;
        if bold {
            queue!(out, SetAttribute(Attribute::NormalIntensity)).map_err(|_| "draw failed")?;
        }
        for (start, end) in super::doc_search::matches(&line.text, query) {
            let (start, end) = glyph_match(&line.text, start, end);
            let match_x = x + text::width(&line.text[..start]) as u16;
            let found = &line.text[start..end];
            let reverse = !app.options.color || app.theme.terminal;
            if reverse {
                queue!(out, SetAttribute(Attribute::Reverse)).map_err(|_| "draw failed")?;
            }
            cell(
                out,
                match_x,
                y,
                text::width(found) as u16,
                found,
                background,
                color("warning"),
            )
            .map_err(|_| "draw failed")?;
            if reverse {
                queue!(out, SetAttribute(Attribute::NoReverse)).map_err(|_| "draw failed")?;
            }
        }
    }

    let search = app.doc_search.draft();
    let hints = if draft.is_some() {
        "Enter review | Esc cancel"
    } else if search.is_some() {
        "Enter find | Esc cancel"
    } else {
        "f follow-up | Esc back | / find | PgUp/PgDn page"
    };
    super::row(out, height - 2, width, hints, color("accent"), background)
        .map_err(|_| "draw failed")?;
    app.add_hint_clicks(height - 2, hints);
    if let Some(draft) = draft.or(search) {
        let prefix = if search.is_some() {
            "Search > "
        } else {
            "Follow-up > "
        };
        let visible = usize::from(width).saturating_sub(prefix.len() + 1);
        let draft = safety::safe_text(draft, crate::advice::MAX_QUESTION);
        let skip = text::width(&draft).saturating_sub(visible);
        super::row(
            out,
            height - 1,
            width,
            &format!("{prefix}{}_", text::window(&draft, skip, visible)),
            color("text"),
            code_background,
        )
        .map_err(|_| "draw failed")?;
    } else {
        let status = if let Some(notice) = &app.ask_notice {
            notice.clone()
        } else if !query.is_empty() {
            let rows: Vec<_> = transcript
                .lines
                .iter()
                .map(|line| line.text.as_str())
                .collect();
            app.doc_search
                .hint(&rows, scroll.search_position(&transcript, layout.body_rows))
                .unwrap_or_default()
        } else if transcript.lines.len() > layout.body_rows {
            format!(
                "Read-only advice | lines {}-{} of {}",
                offset + 1,
                (offset + layout.body_rows).min(transcript.lines.len()),
                transcript.lines.len(),
            )
        } else {
            "Read-only advice".to_owned()
        };
        super::row(out, height - 1, width, &status, color("muted"), background)
            .map_err(|_| "draw failed")?;
    }
    out.flush().map_err(|_| "draw failed".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thread(turns: &[(&str, &str)]) -> AskThread {
        AskThread {
            source: "Logs in demo".into(),
            screen: String::new(),
            keys: String::new(),
            turns: turns
                .iter()
                .map(|(question, answer)| ((*question).to_owned(), (*answer).to_owned()))
                .collect(),
            epoch: 0,
        }
    }

    #[test]
    fn chat_distinguishes_headings_lists_and_literal_code() {
        let thread = thread(&[(
            "What can I check?",
            "## Previous container\n* Check the exit code.\n1. Read previous logs.\n\x60\x60\x60sh\n  kubectl logs api  --previous\n# command comment\n\x60\x60\x60\nNext step.",
        )]);
        let transcript = Transcript::new(&thread, 80);
        let lines: Vec<_> = transcript
            .lines
            .iter()
            .map(|line| (line.text.as_str(), line.tone))
            .collect();
        assert!(lines.contains(&("You", Tone::You)));
        assert!(lines.contains(&("Copilot", Tone::Copilot)));
        assert!(lines.contains(&("Previous container", Tone::Heading)));
        assert!(lines.contains(&("  - Check the exit code.", Tone::Paragraph)));
        assert!(lines.contains(&("  1. Read previous logs.", Tone::Paragraph)));
        assert!(lines.contains(&("|   kubectl logs api  --previous", Tone::Code)));
        assert!(lines.contains(&("| # command comment", Tone::Code)));
        assert!(lines.contains(&("Next step.", Tone::Paragraph)));
    }

    #[test]
    fn chat_starts_at_latest_question_and_clamps_after_widening() {
        let answer = "Verify the previous container exit and its Kubernetes events. ".repeat(19);
        let thread = thread(&[
            ("First question", "Old answer"),
            ("Latest question", &answer),
        ]);
        let narrow = Layout::new(32, 16, 3);
        let tall = Layout::new(120, 22, 3);
        let transcript = Transcript::new(&thread, usize::from(narrow.columns));
        let mut scroll = Scroll::default();
        let offset = scroll.position(&transcript, narrow.body_rows);
        assert_eq!(transcript.lines[offset + 1].text, "Latest question");
        scroll.key(&thread, KeyCode::End, 32, 16, 3);
        let narrow_end = scroll.position(&transcript, narrow.body_rows);
        let transcript = Transcript::new(&thread, usize::from(tall.columns));
        let wide_end = scroll.position(&transcript, tall.body_rows);
        assert!(narrow_end > transcript.lines.len());
        assert_eq!(
            wide_end,
            transcript.lines.len().saturating_sub(tall.body_rows)
        );
        scroll.key(&thread, KeyCode::End, 120, 22, 3);
        let wrapped_again = Transcript::new(&thread, usize::from(narrow.columns));
        assert_eq!(
            scroll.position(&wrapped_again, narrow.body_rows),
            narrow_end
        );
        scroll.key(&thread, KeyCode::Home, 120, 22, 3);
        assert_eq!(scroll.position(&transcript, tall.body_rows), 0);
        assert_eq!(transcript.lines[1].text, "First question");
    }

    #[test]
    fn matching_part_of_a_unicode_glyph_highlights_the_whole_cluster() -> Result<()> {
        let line = "字 e\u{301}x";
        let mark = line.find('\u{301}').ok_or("missing combining mark")?;
        let (start, end) = glyph_match(line, mark, mark + '\u{301}'.len_utf8());
        assert_eq!(&line[start..end], "e\u{301}");
        Ok(())
    }
}
