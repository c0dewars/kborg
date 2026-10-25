//! Native read-only inspection pager. No external editor or subprocess is launched.
use crate::{Result, handoff::Format, safety};
use crossterm::{
    cursor,
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute, queue, style,
    terminal::{
        self, ClearType, DisableLineWrap, EnableLineWrap, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use serde::Serialize;
use serde_json::Value;
use std::{
    io::{self, IsTerminal, Write},
    time::{Duration, Instant},
};

const MAX_VIEW: usize = 8 * 1024 * 1024;

struct CappedText(Vec<u8>);
impl Write for CappedText {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > MAX_VIEW.saturating_sub(self.0.len()) {
            return Err(io::Error::other("inspection exceeds8 MiB"));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn views(object: &Value) -> Result<(Vec<String>, Vec<String>)> {
    if object["kind"]
        .as_str()
        .is_some_and(|kind| kind.eq_ignore_ascii_case("secret"))
    {
        return Err("Secrets cannot be inspected in the pager".into());
    }
    // This renderer first preflights nesting/node/input bounds and independently
    // redacts known fields. Both formats are projections of that same object.
    let yaml = crate::inspect::yaml(object)?;
    let mut projected = object.clone();
    safety::redact(&mut projected);
    let mut encoded = CappedText(Vec::new());
    projected
        .serialize(&mut serde_json::Serializer::pretty(&mut encoded))
        .map_err(|_| "JSON inspection exceeds8 MiB or cannot be encoded")?;
    let json = String::from_utf8(encoded.0).map_err(|_| "invalid inspection encoding")?;
    let mut safe = String::with_capacity(json.len());
    for c in json.chars() {
        if c != '\n' && safety::safe_text(&c.to_string(), 1).is_empty() {
            safe.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            safe.push(c);
        }
        if safe.len() > MAX_VIEW {
            return Err("sanitized JSON inspection exceeds8 MiB".into());
        }
    }
    Ok((
        safe.lines().map(str::to_owned).collect(),
        yaml.lines().map(str::to_owned).collect(),
    ))
}

struct Mode {
    original_raw: bool,
}
impl Mode {
    fn enter() -> Result<Self> {
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err("interactive inspection requires an input/output terminal".into());
        }
        if std::env::var("TERM").is_ok_and(|term| term == "dumb") {
            return Err("TERM=dumb cannot run the pager; use get --json or --yaml".into());
        }
        let original_raw =
            terminal::is_raw_mode_enabled().map_err(|_| "cannot inspect terminal mode")?;
        if !original_raw {
            terminal::enable_raw_mode().map_err(|_| "cannot enable inspection terminal mode")?;
        }
        let guard = Self { original_raw };
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            DisableLineWrap,
            cursor::Hide
        )
        .map_err(|_| "cannot enter inspection screen")?;
        Ok(guard)
    }
}
impl Drop for Mode {
    fn drop(&mut self) {
        let _ = execute!(
            io::stdout(),
            LeaveAlternateScreen,
            EnableLineWrap,
            cursor::Show,
            style::ResetColor
        );
        if !self.original_raw {
            let _ = terminal::disable_raw_mode();
        }
    }
}

struct Page {
    json: Vec<String>,
    yaml: Vec<String>,
    format: Format,
    offset: usize,
    horizontal: usize,
    query: String,
    editing: Option<String>,
    message: String,
}
impl Page {
    fn lines(&self) -> &[String] {
        match self.format {
            Format::Json => &self.json,
            Format::Yaml => &self.yaml,
        }
    }
    fn search(&mut self) {
        if self.query.is_empty() {
            self.message.clear();
            return;
        }
        let query = self.query.to_lowercase();
        let lines = self.lines();
        let found = (1..=lines.len())
            .map(|step| (self.offset + step) % lines.len())
            .find(|index| lines[*index].to_lowercase().contains(&query));
        if let Some(index) = found {
            self.offset = index;
            self.horizontal = 0;
            self.message = format!("match at line{}", index + 1);
        } else {
            self.message = "no matching line".into();
        }
    }
    fn toggle(&mut self) {
        self.format = match self.format {
            Format::Json => Format::Yaml,
            Format::Yaml => Format::Json,
        };
        self.offset = 0;
        self.horizontal = 0;
        self.message.clear();
    }
    fn navigate(&mut self, code: KeyCode, rows: usize) {
        match code {
            KeyCode::Down | KeyCode::Char('j') => self.offset = self.offset.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => self.offset = self.offset.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => {
                self.offset = self.offset.saturating_add(rows)
            }
            KeyCode::PageUp => self.offset = self.offset.saturating_sub(rows),
            KeyCode::Home => {
                self.offset = 0;
                self.horizontal = 0;
            }
            KeyCode::End => self.offset = self.lines().len().saturating_sub(rows),
            KeyCode::Left => self.horizontal = self.horizontal.saturating_sub(20),
            KeyCode::Right => self.horizontal = self.horizontal.saturating_add(20).min(MAX_VIEW),
            KeyCode::Char('v') => self.toggle(),
            KeyCode::Char('n') => self.search(),
            KeyCode::Char('/') => self.editing = Some(self.query.clone()),
            _ => {}
        }
        self.offset = self.offset.min(self.lines().len().saturating_sub(1));
    }
}

/// Cells `horizontal..horizontal+width` of already-sanitized text. The pager
/// has always drawn real glyphs; widths are grapheme-aware so wide and
/// combining characters neither overflow the line nor split.
fn clipped(text: &str, width: usize, horizontal: usize) -> String {
    crate::text::with_mode(true, || crate::text::window(text, horizontal, width))
}
fn colors(
    out: &mut impl Write,
    theme: Option<&crate::themes::Theme>,
    selected: bool,
) -> Result<()> {
    if let Some(theme) = theme.filter(|theme| !theme.terminal) {
        let (foreground, background) = if selected {
            ("selection_text", "selection_background")
        } else {
            ("text", "background")
        };
        queue!(
            out,
            crate::themes::ForegroundColor(theme.get(foreground).color()),
            crate::themes::BackgroundColor(theme.get(background).color())
        )
        .map_err(|_| "inspection output failed")?;
    } else {
        queue!(out, style::ResetColor).map_err(|_| "inspection output failed")?;
    }
    Ok(())
}
fn draw(
    page: &Page,
    title: &str,
    width: u16,
    height: u16,
    remaining: u64,
    theme: Option<&crate::themes::Theme>,
) -> Result<()> {
    let mut stdout = io::stdout().lock();
    colors(&mut stdout, theme, false)?;
    queue!(stdout, terminal::Clear(ClearType::All)).map_err(|_| "inspection output failed")?;
    let header = format!(
        "KBorg READ ONLY |{}|{}| line{}/{} |{}s",
        match page.format {
            Format::Json => "JSON",
            Format::Yaml => "YAML",
        },
        title,
        page.offset + 1,
        page.lines().len(),
        remaining
    );
    if let Some(theme) = theme.filter(|theme| !theme.terminal) {
        queue!(
            stdout,
            crate::themes::ForegroundColor(theme.get("accent").color())
        )
        .map_err(|_| "inspection output failed")?;
    }
    queue!(
        stdout,
        cursor::MoveTo(0, 0),
        style::Print(clipped(&header, usize::from(width), 0))
    )
    .map_err(|_| "inspection output failed")?;
    colors(&mut stdout, theme, false)?;
    let rows = usize::from(height.saturating_sub(3));
    for (row, line) in page.lines().iter().skip(page.offset).take(rows).enumerate() {
        let selected =
            !page.query.is_empty() && line.to_lowercase().contains(&page.query.to_lowercase());
        colors(&mut stdout, theme, selected)?;
        let number = format!("{:>6} ", page.offset + row + 1);
        let number_width = number.len().min(usize::from(width));
        queue!(
            stdout,
            cursor::MoveTo(0, row as u16 + 1),
            style::Print(clipped(&number, number_width, 0)),
            style::Print(clipped(
                line,
                usize::from(width).saturating_sub(number_width),
                page.horizontal
            ))
        )
        .map_err(|_| "inspection output failed")?;
    }
    colors(&mut stdout, theme, false)?;
    let status = page.editing.as_ref().map_or_else(
        || {
            if page.message.is_empty() {
                "Known-field redaction; CRDs may retain confidential text.".into()
            } else {
                page.message.clone()
            }
        },
        |query| format!("/{query}"),
    );
    if height >= 2 {
        queue!(
            stdout,
            cursor::MoveTo(0, height - 2),
            style::Print(clipped(&status, usize::from(width), 0))
        )
        .map_err(|_| "inspection output failed")?;
    }
    if height >= 1 {
        queue!(stdout,cursor::MoveTo(0,height-1),style::Print(clipped("Up/Down PgUp/Dn Home/End | Left/Right pan | / find n next | v JSON/YAML | q/Esc exit",usize::from(width),0))).map_err(|_|"inspection output failed")?;
    }
    stdout
        .flush()
        .map_err(|_| "inspection output failed".into())
}

pub fn run(object: &Value, format: Format, seconds: u16) -> Result<()> {
    run_with_theme(object, format, seconds, None)
}

/// Optional session palette; passing None keeps the viewer entirely monochrome.
pub fn run_with_theme(
    object: &Value,
    format: Format,
    seconds: u16,
    theme: Option<&crate::themes::Theme>,
) -> Result<()> {
    crate::session::validate_lifetime(seconds)?;
    let (json, yaml) = views(object)?;
    let title = format!(
        "{} {}/{}",
        object["kind"].as_str().unwrap_or("Object"),
        object["metadata"]["namespace"].as_str().unwrap_or("-"),
        object["metadata"]["name"].as_str().unwrap_or("-")
    );
    let title = safety::safe_text(&title, 512);
    let mut page = Page {
        json,
        yaml,
        format,
        offset: 0,
        horizontal: 0,
        query: String::new(),
        editing: None,
        message: String::new(),
    };
    let _mode = Mode::enter()?;
    let deadline = Instant::now() + Duration::from_secs(u64::from(seconds));
    let mut dirty = true;
    let mut size = (0, 0);
    let mut last_remaining = 0;
    loop {
        if Instant::now() >= deadline {
            return Ok(());
        }
        let (width, height) =
            terminal::size().map_err(|_| "cannot read inspection terminal size")?;
        // Bound one render independently of exaggerated terminal dimensions.
        let current = (width.clamp(1, 500), height.clamp(1, 150));
        let remaining = deadline.saturating_duration_since(Instant::now()).as_secs();
        if dirty || current != size || remaining != last_remaining {
            draw(&page, &title, current.0, current.1, remaining, theme)?;
            size = current;
            last_remaining = remaining;
            dirty = false;
        }
        if !event::poll(Duration::from_millis(100)).map_err(|_| "inspection input failed")? {
            continue;
        }
        let Event::Key(key) = event::read().map_err(|_| "inspection input failed")? else {
            dirty = true;
            continue;
        };
        if key.kind == KeyEventKind::Release {
            continue;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c' | ']'))
        {
            return Ok(());
        }
        if let Some(query) = page.editing.as_mut() {
            match key.code {
                KeyCode::Esc => page.editing = None,
                KeyCode::Enter => {
                    page.query = page.editing.take().unwrap_or_default();
                    page.search();
                }
                KeyCode::Backspace => {
                    query.pop();
                }
                KeyCode::Char(c)
                    if query.len() + c.len_utf8() <= 512
                        && !safety::safe_text(&c.to_string(), 1).is_empty() =>
                {
                    query.push(c)
                }
                _ => {}
            }
        } else {
            if matches!(key.code, KeyCode::Char('q') | KeyCode::Esc) {
                return Ok(());
            }
            page.navigate(key.code, usize::from(current.1.saturating_sub(3)).max(1));
        }
        dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn both_views_redact_and_escape_terminal_controls_without_raw_secret_paths() -> Result<()> {
        let object = serde_json::json!({"kind":"Pod","metadata":{"name":"sample","annotations":{"private":"ANNOTATION_CANARY"}},"spec":{"env":[{"value":"PASSWORD_CANARY"}]},"\u{202e}key":"hello\u{1b}]52;c;payload\u{7}"});
        let (json, yaml) = views(&object)?;
        for lines in [json, yaml] {
            let text = lines.join("\n");
            assert!(!text.contains("CANARY"));
            assert!(!text.contains('\u{1b}'));
            assert!(!text.contains('\u{202e}'));
        }
        assert!(views(&serde_json::json!({"kind":"Secret"})).is_err());
        assert!(views(&serde_json::json!({"large":"x".repeat(MAX_VIEW+1)})).is_err());
        Ok(())
    }
    #[test]
    fn navigation_search_and_format_toggle_are_bounded() {
        let mut page = Page {
            json: vec!["alpha".into(), "target".into(), "last".into()],
            yaml: vec!["yaml".into()],
            format: Format::Json,
            offset: 0,
            horizontal: 0,
            query: "TARGET".into(),
            editing: None,
            message: String::new(),
        };
        page.search();
        assert_eq!(page.offset, 1);
        page.navigate(KeyCode::End, 2);
        assert_eq!(page.offset, 1);
        page.navigate(KeyCode::PageDown, 99);
        assert_eq!(page.offset, 2);
        page.navigate(KeyCode::Home, 2);
        assert_eq!(page.offset, 0);
        page.toggle();
        assert!(matches!(page.format, Format::Yaml));
        assert_eq!(page.offset, 0);
        page.search();
        assert_eq!(page.message, "no matching line");
        assert_eq!(clipped("abcde", 2, 1), "bc");
    }
}
