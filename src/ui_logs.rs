//! Bounded log-view interaction and projection, separate from network workers.
use crate::{
    logs::{self, ReadOptions},
    safety,
};
use crossterm::event::KeyCode;
use std::cell::Cell;

const FILTER_BYTES: usize = 512;
const MARK: &str = "[---------------- KBorg log mark ----------------]";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogAction {
    None,
    Close,
    Reload(ReadOptions),
    AllContainers,
    Clear,
    Mark,
}

enum Filter {
    All,
    Regex(regex::Regex, bool),
    Fuzzy(String),
}
impl Filter {
    fn parse(text: &str) -> std::result::Result<Self, String> {
        let text = text.trim();
        if text.is_empty() {
            return Ok(Self::All);
        }
        if text == "-l" || text.starts_with("-l ") {
            return Err(
                "Label selectors apply to resource views; use a log regex or -f fuzzy filter."
                    .into(),
            );
        }
        if let Some(pattern) = text.strip_prefix("-f ") {
            return Ok(Self::Fuzzy(pattern.to_lowercase()));
        }
        let (pattern, inverse) = text
            .strip_prefix('!')
            .map(|s| (s.trim_start(), true))
            .unwrap_or((text, false));
        regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .size_limit(64 * 1024)
            .dfa_size_limit(128 * 1024)
            .build()
            .map(|pattern| Self::Regex(pattern, inverse))
            .map_err(|_| "Invalid or excessive regex; edit the filter or use -f fuzzy text.".into())
    }
    fn matches(&self, text: &str) -> bool {
        match self {
            Self::All => true,
            Self::Regex(pattern, inverse) => pattern.is_match(text) != *inverse,
            Self::Fuzzy(pattern) => {
                let haystack = text.to_lowercase();
                let mut chars = haystack.chars();
                pattern
                    .chars()
                    .all(|needle| chars.by_ref().any(|ch| ch == needle))
            }
        }
    }
}

pub struct LogView {
    pub range: ReadOptions,
    pub following: bool,
    pub previous: bool,
    pub fullscreen: bool,
    pub show_timestamps: bool,
    pub wrap: bool,
    pub autoscroll: bool,
    pub column_lock: bool,
    pub horizontal: usize,
    text: String,
    // A paused reader gets a stable bounded window. Collection keeps replacing
    // `text`, but rolling eviction cannot silently move the frozen viewport.
    paused: Option<String>,
    offset: usize,
    filter: String,
    draft: Option<String>,
    compiled: Filter,
    notice: Option<String>,
    all_containers: bool,
    show_help: bool,
    // Only viewport geometry is cached during immutable rendering, to make a
    // later PageUp/PageDown correspond to the viewport actually shown.
    viewport: Cell<(usize, usize)>,
}
impl LogView {
    pub fn new(text: String, range: ReadOptions) -> Self {
        Self {
            range,
            following: true,
            previous: false,
            fullscreen: false,
            show_timestamps: false,
            wrap: false,
            autoscroll: !range.head,
            column_lock: false,
            horizontal: 0,
            text: bounded_text(&text),
            paused: None,
            offset: 0,
            filter: String::new(),
            draft: None,
            compiled: Filter::All,
            notice: None,
            all_containers: false,
            show_help: false,
            viewport: Cell::new((80, 20)),
        }
    }
    pub fn update(&mut self, text: String) {
        self.text = bounded_text(&text);
        if self.autoscroll && !self.column_lock {
            self.horizontal = 0;
        }
    }
    pub fn enable_all_containers(&mut self, enabled: bool) {
        self.all_containers = enabled;
    }
    pub fn is_filtering(&self) -> bool {
        self.draft.is_some()
    }
    pub fn title(&self) -> String {
        format!(
            "{} | {} | {} | scroll:{} | time:{} wrap:{} col:{}{}",
            if self.previous {
                "Previous logs"
            } else {
                "Logs"
            },
            self.range.label(),
            if self.previous || !self.following || self.range.head {
                "snapshot"
            } else {
                "follow"
            },
            if self.autoscroll { "AUTO" } else { "PAUSED" },
            if self.show_timestamps { "on" } else { "off" },
            if self.wrap { "on" } else { "off" },
            self.horizontal,
            if self.filter.is_empty() {
                String::new()
            } else {
                format!(" | /{}", safety::safe_text(&self.filter, 80))
            },
        )
    }
    pub fn help(&self) -> &'static str {
        if self.draft.is_some() {
            "Enter apply | Esc cancel | regex, !inverse, -f fuzzy"
        } else {
            "/ or Enter filter | s scroll | t time | w wrap | f fullscreen | 0-6 range | ? help | Esc back"
        }
    }
    fn source(&self) -> &str {
        self.paused.as_deref().unwrap_or(&self.text)
    }
    fn selected_lines(&self) -> Vec<&str> {
        self.source()
            .lines()
            .filter_map(|line| {
                let shown = if self.show_timestamps {
                    line
                } else {
                    logs::without_timestamp(line)
                };
                // Keep local retention and stream-control notices visible even when
                // a filter would hide them. Ordinary application records are matched.
                let control = line.starts_with("[Rolling buffer:")
                    || line.starts_with("[Log stream ")
                    || line.starts_with("[Stopped:")
                    || line == MARK;
                (control || self.compiled.matches(shown)).then_some(shown)
            })
            .collect()
    }
    fn row_count(&self, width: usize) -> usize {
        self.selected_lines()
            .iter()
            .map(|line| {
                if self.wrap {
                    line.chars().count().max(1).div_ceil(width.max(1))
                } else {
                    1
                }
            })
            .sum()
    }
    fn pause(&mut self) {
        if self.autoscroll {
            let (width, height) = self.viewport.get();
            self.offset = self.row_count(width).saturating_sub(height);
            self.paused = Some(self.text.clone());
            self.autoscroll = false;
        }
    }
    fn resume(&mut self) {
        self.autoscroll = true;
        self.paused = None;
        self.offset = 0;
        if !self.column_lock {
            self.horizontal = 0;
        }
    }
    pub fn key(&mut self, code: KeyCode) -> LogAction {
        if let Some(mut draft) = self.draft.take() {
            match code {
                KeyCode::Esc => {
                    self.notice = None;
                    return LogAction::None;
                }
                KeyCode::Enter => match Filter::parse(&draft) {
                    Ok(compiled) => {
                        self.compiled = compiled;
                        self.filter = draft;
                        self.offset = 0;
                        self.notice = None;
                        return LogAction::None;
                    }
                    Err(message) => self.notice = Some(message),
                },
                KeyCode::Backspace => {
                    draft.pop();
                    self.notice = None;
                }
                KeyCode::Char(ch)
                    if !ch.is_control() && draft.len() + ch.len_utf8() <= FILTER_BYTES =>
                {
                    draft.push(ch);
                    self.notice = None;
                }
                _ => {}
            }
            self.draft = Some(draft);
            return LogAction::None;
        }
        self.notice = None;
        match code {
            KeyCode::Esc if !self.filter.is_empty() => {
                self.filter.clear();
                self.compiled = Filter::All;
                self.offset = 0;
            }
            KeyCode::Esc | KeyCode::Char('q') => return LogAction::Close,
            KeyCode::Enter | KeyCode::Char('/') => self.draft = Some(self.filter.clone()),
            KeyCode::Char('?') => self.show_help = !self.show_help,
            KeyCode::Char('s') => {
                if self.autoscroll {
                    self.pause()
                } else {
                    self.resume()
                }
            }
            KeyCode::End | KeyCode::Char('G') => self.resume(),
            KeyCode::Home | KeyCode::Char('g') => {
                self.pause();
                self.offset = 0;
            }
            KeyCode::Up | KeyCode::Char('k') | KeyCode::PageUp => {
                self.pause();
                let step = if code == KeyCode::PageUp {
                    self.viewport.get().1
                } else {
                    1
                };
                self.offset = self.offset.saturating_sub(step);
            }
            KeyCode::Down | KeyCode::Char('j') | KeyCode::PageDown => {
                self.pause();
                let (width, height) = self.viewport.get();
                let step = if code == KeyCode::PageDown { height } else { 1 };
                self.offset = self
                    .offset
                    .saturating_add(step)
                    .min(self.row_count(width).saturating_sub(height));
            }
            KeyCode::Left | KeyCode::Char('h') => {
                self.horizontal = self.horizontal.saturating_sub(4)
            }
            KeyCode::Right | KeyCode::Char('l') => {
                self.horizontal = self.horizontal.saturating_add(4).min(logs::MAX_LINE_BYTES)
            }
            KeyCode::Char('t') => {
                self.show_timestamps = !self.show_timestamps;
                self.offset = 0;
            }
            KeyCode::Char('w') => {
                self.wrap = !self.wrap;
                self.horizontal = 0;
                self.offset = 0;
            }
            KeyCode::Char('f') => self.fullscreen = !self.fullscreen,
            KeyCode::Char('L') => self.column_lock = !self.column_lock,
            KeyCode::Char('C') => {
                self.text.clear();
                self.paused = (!self.autoscroll).then(String::new);
                self.offset = 0;
                return LogAction::Clear;
            }
            KeyCode::Char('m') => {
                let text = format!("{}\n{MARK}\n", self.text.trim_end_matches('\n'));
                self.text = bounded_text(&text);
                self.notice = Some("Marked the newest retained log position.".into());
                return LogAction::Mark;
            }
            KeyCode::Char('a') if self.all_containers => return LogAction::AllContainers,
            KeyCode::Char('a') => {
                self.notice = Some("All-container logs are unavailable for this target.".into())
            }
            KeyCode::Char(ch) => {
                if let Some(range) = ReadOptions::for_key(ch) {
                    self.range = range;
                    self.text.clear();
                    self.paused = None;
                    self.offset = 0;
                    self.resume();
                    if range.head {
                        // Head is a finite snapshot anchored at its first row.
                        // Do not freeze an empty buffer while its request runs.
                        self.autoscroll = false;
                    }
                    return LogAction::Reload(range);
                }
            }
            _ => {}
        }
        LogAction::None
    }
    pub fn render(&self, width: usize, height: usize) -> Vec<String> {
        let width = width.min(2048);
        let height = height.min(200);
        if width == 0 || height == 0 {
            return Vec::new();
        }
        let mut output = Vec::new();
        if self.show_help {
            for line in [
                "0 tail | 1 head snapshot | 2 1m | 3 5m | 4 15m | 5 30m | 6 1h",
                "s autoscroll | w wrap | t timestamps | f fullscreen | L column lock",
                "C clear | m mark | a all containers (when available)",
                "/ or Enter filter | !pattern inverse | -f text fuzzy | Esc reset/back",
                "Arrows/hjkl scroll | PageUp/PageDown | g/Home first | G/End latest",
            ] {
                for chunk in line.as_bytes().chunks(width) {
                    output.push(String::from_utf8_lossy(chunk).into_owned());
                    if output.len() == height {
                        return output;
                    }
                }
            }
        }
        if let Some(notice) = &self.notice {
            output.push(ascii(notice).chars().take(width).collect());
            if output.len() == height {
                return output;
            }
        }
        if let Some(draft) = &self.draft {
            let draft = ascii(draft);
            let tail: String = draft
                .chars()
                .rev()
                .take(width.saturating_sub(1))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            output.push(format!("/{tail}"));
            if output.len() == height {
                return output;
            }
        }
        let visible = height - output.len();
        self.viewport.set((width, visible));
        let count = self.row_count(width);
        let start = if self.autoscroll {
            count.saturating_sub(visible)
        } else {
            self.offset.min(count.saturating_sub(visible))
        };
        if count == 0 {
            output.push("No retained log lines match.".chars().take(width).collect());
            return output;
        }
        let mut index = 0usize;
        for line in self.selected_lines() {
            let line = ascii(line);
            if self.wrap {
                if line.is_empty() {
                    if index >= start {
                        output.push(String::new());
                    }
                    index += 1;
                } else {
                    for chunk in line.as_bytes().chunks(width) {
                        if index >= start {
                            output.push(String::from_utf8_lossy(chunk).into_owned());
                        }
                        index += 1;
                        if output.len() == height {
                            return output;
                        }
                    }
                }
            } else {
                if index >= start {
                    output.push(line.chars().skip(self.horizontal).take(width).collect());
                }
                index += 1;
            }
            if output.len() == height {
                return output;
            }
        }
        output
    }
}

fn ascii(text: &str) -> String {
    text.chars()
        .map(|ch| if ch.is_ascii() { ch } else { '?' })
        .collect()
}
fn bounded_text(text: &str) -> String {
    let mut start = text.len().saturating_sub(logs::MAX_LOG_BYTES);
    while !text.is_char_boundary(start) {
        start += 1;
    }
    safety::safe_lines(&text[start..], logs::MAX_LOG_BYTES)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn view() -> LogView {
        LogView::new(
            (0..40)
                .map(|index| format!("2026-10-22T12:00:{index:02}Z line-{index:02}\n"))
                .collect(),
            ReadOptions::default(),
        )
    }
    fn type_filter(view: &mut LogView, text: &str) {
        view.key(KeyCode::Char('/'));
        for ch in text.chars() {
            view.key(KeyCode::Char(ch));
        }
        view.key(KeyCode::Enter);
    }
    #[test]
    fn pause_freezes_view_while_new_logs_collect_and_end_resumes() {
        let mut view = view();
        assert_eq!(view.render(40, 3), ["line-37", "line-38", "line-39"]);
        view.key(KeyCode::PageUp);
        let paused = view.render(40, 3);
        view.update("2026-10-22T12:01:00Z newest".into());
        assert_eq!(view.render(40, 3), paused);
        assert!(view.title().contains("PAUSED"));
        view.key(KeyCode::End);
        assert_eq!(view.render(40, 3), ["newest"]);
        assert!(view.autoscroll);
    }
    #[test]
    fn head_starts_at_the_first_retained_row_and_tail_resumes_the_latest() {
        let mut view = view();
        let text = view.text.clone();
        view.key(KeyCode::Char('1'));
        view.update(text.clone());
        assert_eq!(view.render(40, 3), ["line-00", "line-01", "line-02"]);
        assert!(!view.autoscroll);
        view.key(KeyCode::Char('0'));
        view.update(text.clone());
        assert_eq!(view.render(40, 3), ["line-37", "line-38", "line-39"]);
        let from_options = LogView::new(text, ReadOptions::for_key('1').unwrap_or_default());
        assert_eq!(
            from_options.render(40, 3),
            ["line-00", "line-01", "line-02"]
        );
    }
    #[test]
    fn filters_are_bounded_compiled_on_enter_and_cancel_restores_prior_filter() {
        let mut view = view();
        type_filter(&mut view, "line-(01|02)$");
        assert_eq!(view.render(40, 8), ["line-01", "line-02"]);
        view.key(KeyCode::Char('/'));
        view.key(KeyCode::Backspace);
        view.key(KeyCode::Esc);
        assert_eq!(view.render(40, 8), ["line-01", "line-02"]);
        view.key(KeyCode::Esc);
        type_filter(&mut view, "!line-3");
        assert!(
            !view
                .render(40, 40)
                .iter()
                .any(|line| line.starts_with("line-3"))
        );
        view.key(KeyCode::Esc);
        type_filter(&mut view, "[");
        assert!(view.is_filtering());
        assert!(view.render(120, 2)[0].contains("Invalid"));
        view.key(KeyCode::Esc);
        type_filter(&mut view, "-f ln09");
        assert_eq!(view.render(40, 8), ["line-09"]);
        view.key(KeyCode::Esc);
        view.key(KeyCode::Char('/'));
        for _ in 0..1000 {
            view.key(KeyCode::Char('é'));
        }
        assert_eq!(view.draft.as_ref().map(String::len), Some(FILTER_BYTES));
        assert!(view.render(5, 2).iter().all(|line| line.len() <= 5));
    }
    #[test]
    fn timestamp_wrap_horizontal_lock_and_ranges_preserve_their_own_semantics() {
        let mut view = LogView::new(
            "2026-10-22T12:00:00Z abcdefghijkl".into(),
            ReadOptions::default(),
        );
        assert_eq!(view.render(6, 4), ["abcdef"]);
        view.key(KeyCode::Right);
        assert_eq!(view.render(6, 4), ["efghij"]);
        view.key(KeyCode::Char('L'));
        view.update("2026-10-22T12:00:00Z abcdefghijkl".into());
        assert_eq!(view.horizontal, 4);
        view.key(KeyCode::Char('L'));
        view.update("2026-10-22T12:00:00Z abcdefghijkl".into());
        assert_eq!(view.horizontal, 0);
        view.key(KeyCode::Char('w'));
        assert_eq!(view.render(6, 4), ["abcdef", "ghijkl"]);
        view.key(KeyCode::Char('t'));
        assert!(view.render(40, 4)[0].starts_with("2026-10-22"));
        assert_eq!(
            view.key(KeyCode::Char('1')),
            LogAction::Reload(ReadOptions::for_key('1').unwrap_or_default())
        );
        assert!(view.range.head);
        assert_eq!(view.key(KeyCode::Char('C')), LogAction::Clear);
        assert_eq!(view.key(KeyCode::Char('m')), LogAction::Mark);
        assert_eq!(view.key(KeyCode::Char('a')), LogAction::None);
        view.enable_all_containers(true);
        assert_eq!(view.key(KeyCode::Char('a')), LogAction::AllContainers);
    }
    #[test]
    fn render_bounds_controls_and_empty_narrow_views() {
        let mut view = LogView::new(
            format!("{}\x1b[31m\u{202e}", "é".repeat(logs::MAX_LOG_BYTES)),
            ReadOptions::default(),
        );
        assert!(view.text.len() <= logs::MAX_LOG_BYTES);
        assert!(!view.text.contains('\x1b'));
        assert!(!view.text.contains('\u{202e}'));
        view.key(KeyCode::Char('w'));
        view.key(KeyCode::Char('?'));
        for (width, height) in [(0, 0), (1, 1), (1, 200), (48, 12), (120, 32)] {
            let rendered = view.render(width, height);
            assert!(rendered.len() <= height);
            assert!(rendered.iter().all(|line| line.len() <= width));
            assert!(rendered.iter().all(|line| line.is_ascii()));
        }
    }
}
