//! Native Kubernetes sessions with bounded, filtered terminal output.
//! This is a line-oriented shell, not an unrestricted terminal emulator.
use crate::{Result, safety};
use crossterm::{
    event::{
        self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyCode, KeyEvent, KeyEventKind,
        KeyModifiers,
    },
    execute, style, terminal,
};
use futures_util::future::{Either, select};
use kube::api::{AttachParams, AttachedProcess, TerminalSize};
use std::{
    io::{self, IsTerminal, Write},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};

pub const MAX_OUTPUT: usize = 16 * 1024 * 1024;
pub const MAX_INPUT: usize = 1024 * 1024;
pub const MAX_INPUT_CHUNK: usize = 4096;
const WRITE_BUDGET: Duration = Duration::from_millis(250);

pub fn validate_lifetime(seconds: u16) -> Result<()> {
    if !(1..=3600).contains(&seconds) {
        return Err("interactive session lifetime must be 1..3600 seconds".into());
    }
    Ok(())
}
pub fn review(seconds: u16) -> String {
    format!(
        "Interactive stdin + remote TTY; {seconds}-second maximum lifetime.\nCtrl-] disconnects locally; Ctrl-C is sent to the remote process.\nLine-oriented shell input/backspace only; remote full-screen applications and cursor controls are unsupported.\nOutput is filtered: OSC/clipboard, hyperlinks, terminal queries, cursor/private controls and bidi are removed.\nLimits:16 MiB received output,1 MiB typed input,4 KiB per input chunk.\nDisconnect does not guarantee remote process termination; commands may have changed the workload."
    )
}
pub fn parameters(container: &str) -> AttachParams {
    AttachParams {
        container: Some(container.into()),
        stdin: true,
        stdout: true,
        stderr: false,
        tty: true,
        max_stdin_buf_size: Some(8192),
        max_stdout_buf_size: Some(8192),
        max_stderr_buf_size: Some(8192),
    }
}

#[derive(Debug)]
pub enum Input {
    Bytes(Vec<u8>),
    Resize { width: u16, height: u16 },
    CloseStdin,
    Cancel,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum End {
    Completed,
    Canceled,
    Expired,
    OutputLimit,
    InputLimit,
    Disconnected,
}
#[derive(Debug)]
pub struct Outcome {
    pub end: End,
    pub input_bytes: usize,
    pub output_bytes: usize,
}
impl Outcome {
    pub fn remote_outcome_unknown(&self) -> bool {
        self.end != End::Completed
    }
    pub fn summary(&self) -> String {
        let reason = match self.end {
            End::Completed => "remote command completed successfully",
            End::Canceled => "local Ctrl-] or cancellation",
            End::Expired => "configured lifetime expired",
            End::OutputLimit => "16 MiB received-output limit reached",
            End::InputLimit => "typed-input limit reached",
            End::Disconnected => "remote connection closed without a success status",
        };
        format!(
            "Interactive session ended: {reason}. {} bytes sent,{} bytes received.{}",
            self.input_bytes,
            self.output_bytes,
            if self.remote_outcome_unknown() {
                " Remote process termination is not guaranteed; inspect the workload before retrying."
            } else {
                ""
            }
        )
    }
}

/// An incremental allowlist. CSI SGR colors/attributes are accepted; every other
/// escape sequence is discarded with fixed parser storage, including long OSC/DCS.
pub struct OutputFilter {
    state: FilterState,
    csi: Vec<u8>,
    utf8: [u8; 4],
    utf8_len: usize,
    utf8_target: usize,
}
#[derive(Clone, Copy)]
enum FilterState {
    Ground,
    Escape,
    Csi,
    DiscardCsi,
    String { escape: bool },
    Charset,
}
impl Default for OutputFilter {
    fn default() -> Self {
        Self {
            state: FilterState::Ground,
            csi: Vec::with_capacity(64),
            utf8: [0; 4],
            utf8_len: 0,
            utf8_target: 0,
        }
    }
}
impl OutputFilter {
    pub fn push(&mut self, bytes: &[u8]) -> String {
        let mut out = String::with_capacity(bytes.len());
        for byte in bytes {
            match self.state {
                FilterState::Ground => self.ground(*byte, &mut out),
                FilterState::Escape => {
                    self.state = match *byte {
                        b'[' => {
                            self.csi.clear();
                            FilterState::Csi
                        }
                        b']' | b'P' | b'X' | b'^' | b'_' => FilterState::String { escape: false },
                        b'(' | b')' | b'*' | b'+' | b'-' | b'.' | b'/' => FilterState::Charset,
                        0x1b => FilterState::Escape,
                        _ => FilterState::Ground,
                    };
                }
                FilterState::Csi => {
                    if (0x40..=0x7e).contains(byte) {
                        if *byte == b'm' && approved_sgr(&self.csi) {
                            out.push_str("\x1b[");
                            for value in &self.csi {
                                out.push(char::from(*value));
                            }
                            out.push('m');
                        }
                        self.state = FilterState::Ground;
                        self.csi.clear();
                    } else if self.csi.len() < 64 && (0x20..=0x3f).contains(byte) {
                        self.csi.push(*byte);
                    } else {
                        self.csi.clear();
                        self.state = FilterState::DiscardCsi;
                    }
                }
                FilterState::DiscardCsi => {
                    if (0x40..=0x7e).contains(byte) {
                        self.state = FilterState::Ground;
                    }
                }
                FilterState::String { escape } => {
                    if *byte == 7 || (escape && *byte == b'\\') {
                        self.state = FilterState::Ground;
                    } else {
                        self.state = FilterState::String {
                            escape: *byte == 0x1b,
                        };
                    }
                }
                FilterState::Charset => self.state = FilterState::Ground,
            }
        }
        out
    }
    fn ground(&mut self, byte: u8, out: &mut String) {
        if self.utf8_len > 0 {
            if (0x80..=0xbf).contains(&byte) {
                self.utf8[self.utf8_len] = byte;
                self.utf8_len += 1;
                if self.utf8_len == self.utf8_target {
                    if let Ok(text) = std::str::from_utf8(&self.utf8[..self.utf8_len]) {
                        out.push_str(&safety::safe_text(text, 1));
                    } else {
                        out.push('\u{fffd}');
                    }
                    self.utf8_len = 0;
                }
                return;
            }
            self.utf8_len = 0;
            out.push('\u{fffd}');
        }
        match byte {
            0x1b => self.state = FilterState::Escape,
            b'\r' | b'\n' | b'\t' | 8 => out.push(char::from(byte)),
            0x20..=0x7e => out.push(char::from(byte)),
            0xc2..=0xdf => {
                self.utf8[0] = byte;
                self.utf8_len = 1;
                self.utf8_target = 2;
            }
            0xe0..=0xef => {
                self.utf8[0] = byte;
                self.utf8_len = 1;
                self.utf8_target = 3;
            }
            0xf0..=0xf4 => {
                self.utf8[0] = byte;
                self.utf8_len = 1;
                self.utf8_target = 4;
            }
            0x80..=0xff => out.push('\u{fffd}'),
            _ => {}
        }
    }
}

fn approved_sgr(bytes: &[u8]) -> bool {
    if bytes.is_empty() {
        return true;
    }
    if !bytes.iter().all(|b| b.is_ascii_digit() || *b == b';') {
        return false;
    }
    let Ok(text) = std::str::from_utf8(bytes) else {
        return false;
    };
    let values = text
        .split(';')
        .map(|value| {
            if value.is_empty() {
                Some(0)
            } else {
                value.parse::<u16>().ok()
            }
        })
        .collect::<Option<Vec<_>>>();
    let Some(values) = values else {
        return false;
    };
    if values.len() > 16 {
        return false;
    }
    let mut index = 0;
    while index < values.len() {
        match values[index] {
            0 | 1 | 3 | 4 | 22 | 23 | 24 | 30..=37 | 39 | 40..=47 | 49 | 90..=97 | 100..=107 => {
                index += 1
            }
            38 | 48 => match values.get(index + 1) {
                Some(5) if values.get(index + 2).is_some_and(|v| *v <= 255) => index += 3,
                Some(2)
                    if values
                        .get(index + 2..index + 5)
                        .is_some_and(|rgb| rgb.iter().all(|v| *v <= 255)) =>
                {
                    index += 5
                }
                _ => return false,
            },
            _ => return false,
        }
    }
    true
}

/// Drives a reviewed native stream without owning a terminal. Cancellation takes
/// priority over output, and the output callback must remain bounded/nonblocking.
pub async fn pump(
    mut process: AttachedProcess,
    seconds: u16,
    mut inputs: mpsc::Receiver<Input>,
    mut emit: impl FnMut(&str) -> Result<()>,
) -> Result<Outcome> {
    validate_lifetime(seconds)?;
    let mut stdin = process.stdin();
    let mut stdout = process.stdout().ok_or("interactive stdout unavailable")?;
    let mut stderr = process.stderr();
    let status = process
        .take_status()
        .ok_or("interactive status unavailable")?;
    let mut status = Box::pin(status);
    let mut resize = process.terminal_size();
    let deadline = tokio::time::sleep(Duration::from_secs(u64::from(seconds)));
    tokio::pin!(deadline);
    let mut output = [0u8; 8192];
    let mut errors = [0u8; 8192];
    let mut out_filter = OutputFilter::default();
    let mut err_filter = OutputFilter::default();
    let mut stdout_closed = false;
    let mut stderr_closed = stderr.is_none();
    let mut status_received = false;
    let mut remote_status: Option<k8s_openapi::apimachinery::pkg::apis::meta::v1::Status> = None;
    let mut inputs_closed = false;
    let mut outcome = Outcome {
        end: End::Disconnected,
        input_bytes: 0,
        output_bytes: 0,
    };
    loop {
        if stdout_closed && stderr_closed && status_received {
            if let Some(status) = remote_status {
                if status.status.as_deref() != Some("Success") {
                    return Err("remote interactive command failed; server details withheld".into());
                }
                outcome.end = End::Completed;
            }
            break;
        }
        tokio::select! {
            biased;
            _ = &mut deadline => { outcome.end = End::Expired; break; },
            input = inputs.recv(), if !inputs_closed => match input {
                Some(Input::Cancel) => { outcome.end = End::Canceled; break; },
                Some(Input::CloseStdin) => { stdin = None; },
                Some(Input::Resize {width,height}) => {
                    if width == 0 || height == 0 || width >4096 || height >4096 { return Err("invalid terminal resize dimensions".into()); }
                    if let Some(sender) = resize.as_mut() {
                        sender.try_send(TerminalSize {width,height}).map_err(|_| "remote terminal resize queue unavailable or full")?;
                    } else { return Err("remote session has no TTY resize stream".into()); }
                },
                Some(Input::Bytes(bytes)) => {
                    if bytes.len() >MAX_INPUT_CHUNK || outcome.input_bytes.saturating_add(bytes.len()) >MAX_INPUT { outcome.end=End::InputLimit; break; }
                    if let Some(writer) = stdin.as_mut() {
                        tokio::time::timeout(WRITE_BUDGET, writer.write_all(&bytes)).await.map_err(|_| "remote stdin blocked; session disconnected, remote outcome unknown")?.map_err(|_| "remote stdin failed; remote outcome unknown")?;
                        outcome.input_bytes += bytes.len();
                    } else { return Err("remote stdin is closed".into()); }
                },
                None => { inputs_closed = true; },
            },
            status = &mut status, if !status_received => { remote_status=status; status_received=true; },
            read = stdout.read(&mut output), if !stdout_closed => {
                let count = read.map_err(|_| "remote stdout failed; remote outcome unknown")?;
                if count ==0 { stdout_closed=true; }
                else {
                    outcome.output_bytes = outcome.output_bytes.saturating_add(count);
                    if outcome.output_bytes >MAX_OUTPUT { outcome.end=End::OutputLimit; break; }
                    emit(&out_filter.push(&output[..count]))?;
                }
            },
            read = async { match stderr.as_mut() { Some(reader)=>reader.read(&mut errors).await, None=>std::future::pending().await } }, if !stderr_closed => {
                let count = read.map_err(|_| "remote stderr failed; remote outcome unknown")?;
                if count ==0 { stderr_closed=true; }
                else {
                    outcome.output_bytes = outcome.output_bytes.saturating_add(count);
                    if outcome.output_bytes >MAX_OUTPUT { outcome.end=End::OutputLimit; break; }
                    emit(&err_filter.push(&errors[..count]))?;
                }
            },
        }
    }
    process.abort();
    Ok(outcome)
}

pub fn ensure_terminal() -> Result<()> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        return Err("interactive sessions require a real input/output terminal".into());
    }
    if std::env::var("TERM").is_ok_and(|term| term == "dumb") {
        return Err("TERM=dumb cannot run an interactive session".into());
    }
    Ok(())
}
struct TerminalMode {
    original_raw: bool,
}
impl TerminalMode {
    fn enter() -> Result<Self> {
        ensure_terminal()?;
        let original_raw =
            terminal::is_raw_mode_enabled().map_err(|_| "cannot inspect terminal mode")?;
        if !original_raw {
            terminal::enable_raw_mode().map_err(|_| "cannot enable session terminal mode")?;
        }
        let mode = Self { original_raw };
        execute!(io::stdout(), EnableBracketedPaste)
            .map_err(|_| "cannot enable bounded session paste")?;
        Ok(mode)
    }
}
impl Drop for TerminalMode {
    fn drop(&mut self) {
        let _ = execute!(io::stdout(), DisableBracketedPaste, style::ResetColor);
        if !self.original_raw {
            let _ = terminal::disable_raw_mode();
        }
    }
}

fn key_input(key: KeyEvent) -> Option<Input> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char(']')) {
        return Some(Input::Cancel);
    }
    if key.modifiers.contains(KeyModifiers::ALT) {
        return None;
    }
    let bytes = match key.code {
        KeyCode::Char(c) if key.modifiers.contains(KeyModifiers::CONTROL) && c.is_ascii() => {
            vec![(c as u8) & 0x1f]
        }
        KeyCode::Char(c) => {
            let mut buffer = [0; 4];
            c.encode_utf8(&mut buffer).as_bytes().to_vec()
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Esc => vec![0x1b],
        _ => return None,
    };
    Some(Input::Bytes(bytes))
}

pub async fn terminal(process: AttachedProcess, seconds: u16) -> Result<Outcome> {
    validate_lifetime(seconds)?;
    let _mode = TerminalMode::enter()?;
    let (sender, inputs) = mpsc::channel(32);
    if let Ok((width, height)) = terminal::size() {
        let _ = sender.try_send(Input::Resize { width, height });
    }
    let mut stdout = io::stdout();
    let stream = pump(process, seconds, inputs, |text| {
        if text.is_empty() {
            return Ok(());
        }
        stdout
            .write_all(text.as_bytes())
            .and_then(|()| stdout.flush())
            .map_err(|_| "local terminal output failed".into())
    });
    let keyboard = async {
        loop {
            if event::poll(Duration::ZERO).map_err(|_| "session input polling failed")? {
                let input = match event::read().map_err(|_| "session input failed")? {
                    Event::Key(key) => key_input(key),
                    Event::Resize(width, height) => Some(Input::Resize { width, height }),
                    Event::Paste(text) if text.len() <= MAX_INPUT_CHUNK => {
                        if text.chars().any(|c| {
                            !matches!(c, '\r' | '\n' | '\t')
                                && safety::safe_text(&c.to_string(), 1).is_empty()
                        }) {
                            return Err(
                                "paste contains hidden terminal controls; session disconnected"
                                    .into(),
                            );
                        }
                        Some(Input::Bytes(text.into_bytes()))
                    }
                    Event::Paste(_) => {
                        return Err("paste exceeds4 KiB; session disconnected".into());
                    }
                    _ => None,
                };
                if let Some(input) = input {
                    let canceled = matches!(input, Input::Cancel);
                    sender
                        .send(input)
                        .await
                        .map_err(|_| "session input closed")?;
                    if canceled {
                        return std::future::pending::<Result<()>>().await;
                    }
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    match select(Box::pin(stream), Box::pin(keyboard)).await {
        Either::Left((outcome, _)) => outcome,
        Either::Right((error, _)) => {
            error?;
            Err("session input ended unexpectedly".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn removes_terminal_side_effects_and_keeps_only_safe_sgr_and_text() {
        let mut filter = OutputFilter::default();
        let text = filter.push(b"hello\x1b]52;c;CLIPBOARD_CANARY\x07\x1b]8;;https://evil\x1b\\link\x1b]8;;\x1b\\\x1b[2J\x1b[?1049h\x1b[6n\x1b[8mhidden\x1b[31mred\x1b[0m\r\n\x08\t");
        assert!(!text.contains("CLIPBOARD_CANARY") && !text.contains("https://evil"));
        assert!(
            !text.contains("2J")
                && !text.contains("1049")
                && !text.contains("6n")
                && !text.contains("[8m")
        );
        assert!(text.contains("\x1b[31mred\x1b[0m"));
        assert!(text.ends_with("\r\n\x08\t"));
        for invalid in ["8", "5", "38;2;1;2", "38;5;256", "?31", "99999"] {
            assert!(!approved_sgr(invalid.as_bytes()));
        }
        for valid in ["", "0", "1;31", "38;5;255", "48;2;12;34;56"] {
            assert!(approved_sgr(valid.as_bytes()));
        }
    }
    #[test]
    fn split_unicode_escape_sequences_and_floods_keep_fixed_parser_storage() {
        let mut filter = OutputFilter::default();
        assert_eq!(filter.push(&[0xe2, 0x82]), "");
        assert_eq!(filter.push(&[0xac]), "€");
        assert_eq!(filter.push("\u{202e}\u{009b}\u{200b}".as_bytes()), "");
        assert_eq!(filter.push(b"\x1b]52;c;"), "");
        for _ in 0..1024 {
            assert!(filter.push(&[b'x'; 4096]).is_empty());
        }
        assert!(filter.push(b"\x1b").is_empty());
        assert_eq!(filter.push(b"\\safe"), "safe");
        assert!(filter.push(b"\x1b[").is_empty());
        for _ in 0..1024 {
            let _ = filter.push(&[b'1'; 4096]);
            assert!(filter.csi.len() <= 64);
        }
        assert_eq!(filter.push(b"msafe"), "safe");
    }
    #[test]
    fn lifetime_and_keyboard_controls_are_explicit() {
        assert!(validate_lifetime(0).is_err() && validate_lifetime(3601).is_err());
        assert!(validate_lifetime(1).is_ok() && validate_lifetime(3600).is_ok());
        assert!(matches!(
            key_input(KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL)),
            Some(Input::Cancel)
        ));
        assert!(
            matches!(key_input(KeyEvent::new(KeyCode::Char('c'),KeyModifiers::CONTROL)),Some(Input::Bytes(bytes)) if bytes == [3])
        );
        assert!(key_input(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE)).is_none());
    }
}
