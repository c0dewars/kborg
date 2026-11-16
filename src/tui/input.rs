//! Terminal input for the main loop, with one repair over crossterm.
//!
//! crossterm reports a lone Esc key when a read ends right after the ESC
//! byte of an escape sequence, and the rest of that sequence (`[<65;30;15M`
//! for a wheel notch) then arrives as typed characters. A fast mouse wheel
//! produces such splits: the digits chose namespaces, `[` went back a view,
//! and the Esc closed whatever was open. Here an Esc followed within a few
//! milliseconds by `[` or `O` and the rest of an escape sequence is put back
//! together: an SGR mouse report becomes the mouse event it was, anything
//! else is dropped. A real Esc press is delivered at most `GAP` late, or
//! `MOUSE_GAP` late while the mouse is in use (over SSH or tmux the halves
//! of one sequence can be tens of milliseconds apart).
use crate::Result;
use crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use std::{
    collections::VecDeque,
    time::{Duration, Instant},
};

/// How long the bytes of one escape sequence may be apart.
const GAP: Duration = Duration::from_millis(15);
/// The same while mouse events arrive: an Esc then is far more likely the
/// start of a split report than a key press.
const MOUSE_GAP: Duration = Duration::from_millis(100);
/// How long after a mouse event the mouse counts as in use.
const MOUSE_RECENT: Duration = Duration::from_secs(1);
/// Longest sequence body read back (an SGR mouse report is at most ~16).
const MAX_SEQUENCE: usize = 32;

/// Where events come from: the terminal, or a script in tests.
pub(super) trait Source {
    fn poll(&mut self, timeout: Duration) -> Result<bool>;
    fn read(&mut self) -> Result<Event>;
}

/// crossterm's global event reader.
pub(super) struct Terminal;
impl Source for Terminal {
    fn poll(&mut self, timeout: Duration) -> Result<bool> {
        crossterm::event::poll(timeout).map_err(|_| "input failed".into())
    }
    fn read(&mut self) -> Result<Event> {
        crossterm::event::read().map_err(|_| "input failed".into())
    }
}

pub(super) struct Input<S> {
    source: S,
    /// Events read while looking past an Esc, delivered next.
    pending: VecDeque<Event>,
    /// When the last mouse event came.
    last_mouse: Option<Instant>,
}

fn char_of(event: &Event) -> Option<char> {
    match event {
        Event::Key(KeyEvent {
            code: KeyCode::Char(c),
            kind: KeyEventKind::Press,
            modifiers,
            ..
        }) if !modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
            && c.is_ascii_graphic() =>
        {
            Some(*c)
        }
        _ => None,
    }
}

fn is_lone_escape(event: &Event) -> bool {
    matches!(
        event,
        Event::Key(KeyEvent {
            code: KeyCode::Esc,
            kind: KeyEventKind::Press,
            modifiers: KeyModifiers::NONE,
            ..
        })
    )
}

/// The mouse event an SGR report body (`<B;X;YM` or `...m`) describes.
fn sgr_mouse(body: &str) -> Option<MouseEvent> {
    let rest = body.strip_prefix('<')?;
    let (fields, end) = rest.split_at(rest.len().checked_sub(1)?);
    let release = match end {
        "M" => false,
        "m" => true,
        _ => return None,
    };
    let mut numbers = fields.split(';').map(|n| n.parse::<u16>().ok());
    let (code, x, y) = (numbers.next()??, numbers.next()??, numbers.next()??);
    if numbers.next().is_some() || x == 0 || y == 0 {
        return None;
    }
    let mut modifiers = KeyModifiers::NONE;
    if code & 4 != 0 {
        modifiers |= KeyModifiers::SHIFT;
    }
    if code & 8 != 0 {
        modifiers |= KeyModifiers::ALT;
    }
    if code & 16 != 0 {
        modifiers |= KeyModifiers::CONTROL;
    }
    let button = match code & 3 {
        0 => Some(MouseButton::Left),
        1 => Some(MouseButton::Middle),
        2 => Some(MouseButton::Right),
        _ => None,
    };
    let kind = if code & 64 != 0 {
        match code & 3 {
            0 => MouseEventKind::ScrollUp,
            1 => MouseEventKind::ScrollDown,
            2 => MouseEventKind::ScrollLeft,
            _ => MouseEventKind::ScrollRight,
        }
    } else if code & 32 != 0 {
        button.map_or(MouseEventKind::Moved, MouseEventKind::Drag)
    } else {
        match (button, release) {
            (Some(button), false) => MouseEventKind::Down(button),
            (Some(button), true) => MouseEventKind::Up(button),
            (None, _) => MouseEventKind::Up(MouseButton::Left),
        }
    };
    Some(MouseEvent {
        kind,
        column: x - 1,
        row: y - 1,
        modifiers,
    })
}

impl<S: Source> Input<S> {
    pub(super) fn new(source: S) -> Self {
        Self {
            source,
            pending: VecDeque::new(),
            last_mouse: None,
        }
    }
    pub(super) fn poll(&mut self, timeout: Duration) -> Result<bool> {
        Ok(!self.pending.is_empty() || self.source.poll(timeout)?)
    }
    /// The next event; `None` when the input was a split escape sequence
    /// that is not a mouse report, which is dropped.
    pub(super) fn read(&mut self) -> Result<Option<Event>> {
        let event = self.read_repaired()?;
        if matches!(event, Some(Event::Mouse(_))) {
            self.last_mouse = Some(Instant::now());
        }
        Ok(event)
    }
    fn read_repaired(&mut self) -> Result<Option<Event>> {
        if let Some(event) = self.pending.pop_front() {
            return Ok(Some(event));
        }
        let gap = if self
            .last_mouse
            .is_some_and(|at| at.elapsed() < MOUSE_RECENT)
        {
            MOUSE_GAP
        } else {
            GAP
        };
        let event = self.source.read()?;
        if !is_lone_escape(&event) || !self.source.poll(gap)? {
            return Ok(Some(event));
        }
        let next = self.source.read()?;
        let introducer = char_of(&next).filter(|c| matches!(c, '[' | 'O'));
        let Some(introducer) = introducer else {
            // A real Esc, then whatever came next.
            self.pending.push_back(next);
            return Ok(Some(event));
        };
        // The rest of the sequence: CSI parameters end at a final byte
        // (`@`..`~`); SS3 is one character.
        let mut body = String::new();
        while body.len() < MAX_SEQUENCE && self.source.poll(gap)? {
            let more = self.source.read()?;
            let Some(c) = char_of(&more) else {
                self.pending.push_back(more);
                break;
            };
            body.push(c);
            if introducer == 'O' || ('@'..='~').contains(&c) {
                break;
            }
        }
        Ok(if introducer == '[' {
            sgr_mouse(&body).map(Event::Mouse)
        } else {
            None
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Events as crossterm reports them; `poll` says whether any is left.
    struct Script(VecDeque<Event>);
    impl Source for Script {
        fn poll(&mut self, _: Duration) -> Result<bool> {
            Ok(!self.0.is_empty())
        }
        fn read(&mut self) -> Result<Event> {
            self.0.pop_front().ok_or_else(|| "script ended".into())
        }
    }
    fn key(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }
    /// A lone Esc and then each byte as a typed character: a split read.
    fn leaked(sequence: &str) -> Vec<Event> {
        let mut events = vec![key(KeyCode::Esc)];
        events.extend(sequence.chars().map(|c| key(KeyCode::Char(c))));
        events
    }
    fn all(events: Vec<Event>) -> Result<Vec<Option<Event>>> {
        let mut input = Input::new(Script(events.into()));
        let mut out = Vec::new();
        while input.poll(Duration::ZERO)? {
            out.push(input.read()?);
        }
        Ok(out)
    }

    #[test]
    fn a_split_wheel_report_is_the_wheel_not_typed_keys() -> Result<()> {
        let out = all(leaked("[<65;30;15M"))?;
        assert_eq!(out.len(), 1, "{out:?}");
        let Some(Event::Mouse(mouse)) = &out[0] else {
            return Err(format!("{out:?}"));
        };
        assert_eq!(mouse.kind, MouseEventKind::ScrollDown);
        assert_eq!((mouse.column, mouse.row), (29, 14));
        // Ctrl with the wheel, a click and its release, a drag.
        let ctrl = all(leaked("[<80;3;4M"))?;
        assert!(
            matches!(&ctrl[0], Some(Event::Mouse(m)) if m.kind == MouseEventKind::ScrollUp && m.modifiers == KeyModifiers::CONTROL)
        );
        let click = all(leaked("[<0;10;5M"))?;
        assert!(
            matches!(&click[0], Some(Event::Mouse(m)) if m.kind == MouseEventKind::Down(MouseButton::Left))
        );
        let up = all(leaked("[<0;10;5m"))?;
        assert!(
            matches!(&up[0], Some(Event::Mouse(m)) if m.kind == MouseEventKind::Up(MouseButton::Left))
        );
        let drag = all(leaked("[<32;10;5M"))?;
        assert!(
            matches!(&drag[0], Some(Event::Mouse(m)) if matches!(m.kind, MouseEventKind::Drag(MouseButton::Left)))
        );
        Ok(())
    }

    #[test]
    fn other_split_sequences_are_dropped_and_real_keys_pass() -> Result<()> {
        // An arrow in either form, a function key: no stray letters or digits.
        for sequence in ["[A", "OB", "[15~", "[1;5C"] {
            assert_eq!(all(leaked(sequence))?, vec![None], "{sequence}");
        }
        // A real Esc alone, and Esc then a key that starts no sequence.
        assert_eq!(all(vec![key(KeyCode::Esc)])?, vec![Some(key(KeyCode::Esc))]);
        assert_eq!(
            all(vec![key(KeyCode::Esc), key(KeyCode::Char('1'))])?,
            vec![Some(key(KeyCode::Esc)), Some(key(KeyCode::Char('1')))]
        );
        // Ordinary typing, '[' included, is untouched.
        assert_eq!(
            all(vec![key(KeyCode::Char('[')), key(KeyCode::Char('1'))])?,
            vec![Some(key(KeyCode::Char('['))), Some(key(KeyCode::Char('1')))]
        );
        // A sequence followed by a real key keeps that key.
        let mut events = leaked("[<65;3;4M");
        events.push(key(KeyCode::Char('j')));
        let out = all(events)?;
        assert_eq!(out.len(), 2);
        assert_eq!(out[1], Some(key(KeyCode::Char('j'))));
        // Garbage after `[<` never becomes a mouse event.
        assert_eq!(all(leaked("[<65;0;4M"))?, vec![None]);
        assert_eq!(all(leaked("[<65;3M"))?, vec![None], "a missing field");
        assert_eq!(all(leaked("[<99999;3;4M"))?, vec![None], "out of range");
        Ok(())
    }
}
