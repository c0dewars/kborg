//! Terminal mode guard: raw mode, alternate screen and restoration on drop.
use super::*;

pub(super) struct Terminal;
impl Terminal {
    pub(super) fn enter(mouse: bool) -> Result<Self> {
        if std::env::var("TERM").is_ok_and(|term| term == "dumb") {
            return Err(
                "TERM=dumb cannot run the interactive UI; use CLI JSON/YAML commands".into(),
            );
        }
        if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
            return Err("interactive mode needs a terminal; use list/get --json or --help".into());
        }
        terminal::enable_raw_mode().map_err(|_| "cannot enable raw mode")?;
        let guard = Self;
        if execute!(
            io::stdout(),
            EnterAlternateScreen,
            EnableBracketedPaste,
            DisableLineWrap,
            cursor::Hide
        )
        .is_err()
        {
            let _ = terminal::disable_raw_mode();
            return Err("cannot enter alternate screen".into());
        }
        if mouse {
            execute!(io::stdout(), EnableMouseCapture)
                .map_err(|_| "cannot enable mouse capture")?;
        }
        std::panic::set_hook(Box::new(|_| {
            let _ = terminal::disable_raw_mode();
            // A panic during a full-TTY session must also undo remote modes.
            let _ = io::stdout().write_all(crate::session::RESET.as_bytes());
            let _ = execute!(
                io::stdout(),
                EndSynchronizedUpdate,
                LeaveAlternateScreen,
                DisableBracketedPaste,
                EnableLineWrap,
                DisableMouseCapture,
                cursor::Show,
                style::ResetColor
            );
            eprintln!("KBorg encountered an internal error; no object contents recorded.");
        }));
        Ok(guard)
    }
}
impl Drop for Terminal {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = execute!(
            io::stdout(),
            EndSynchronizedUpdate,
            LeaveAlternateScreen,
            DisableBracketedPaste,
            EnableLineWrap,
            DisableMouseCapture,
            cursor::Show,
            style::ResetColor
        );
    }
}
