//! Native Kubernetes sessions.
//!
//! In a real terminal a session is a full TTY passthrough, like
//! `kubectl exec -it`: raw bytes flow both ways unfiltered, so vim, top and
//! less work. The user approved an exact review (sessions are off with
//! `--read-only` or an operator config without `allow_exec`/attach/node shell/debug). The local escape key (Ctrl-] by
//! default) disconnects, and [`RESET`] restores terminal modes the remote
//! program may have changed. [`pump`] drives the same stream for controlled
//! (non-terminal) callers and filters output to text and safe colors.
use crate::{Result, safety};
#[cfg(any(not(unix), test))]
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal;
use futures_util::future::{Either, select};
use kube::api::{AttachParams, AttachedProcess, TerminalSize};
use std::{
    io::{self, IsTerminal, Write},
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::mpsc,
};

pub const MAX_OUTPUT: usize = 16 * 1024 * 1024;
pub const MAX_INPUT: usize = 1024 * 1024;
pub const MAX_INPUT_CHUNK: usize = 4096;
/// Typed-input budget for full-TTY sessions. Larger than [`MAX_INPUT`] because
/// remote apps that enable mouse tracking receive a report for every move.
pub const MAX_TTY_INPUT: usize = 8 * 1024 * 1024;
const WRITE_BUDGET: Duration = Duration::from_millis(250);
/// Ctrl-]: the default local escape that ends a session, as in telnet/kubectl.
pub const DEFAULT_ESCAPE: u8 = 0x1d;
static ESCAPE: AtomicU8 = AtomicU8::new(DEFAULT_ESCAPE);

/// Written to the local terminal when a full-TTY session ends (and from the
/// panic hook) so a remote program cannot leave the terminal unusable. Each
/// part is a standard reset that terminals without the feature ignore.
pub const RESET: &str = concat!(
    "\x18\x1b\\",                                   // abort an unfinished escape, OSC or DCS
    "\x1b[?2026l",                                  // end synchronized update
    "\x1b7\x1b[r\x1b8",                             // full scroll region, keep the cursor
    "\x1b[?1000l\x1b[?1001l\x1b[?1002l\x1b[?1003l", // mouse tracking off
    "\x1b[?1005l\x1b[?1006l\x1b[?1015l\x1b[?1016l", // mouse encodings off
    "\x1b[?1004l\x1b[?1007l",                       // focus events, alternate scroll off
    "\x1b[?2004l",                                  // bracketed paste off
    "\x1b[?1l\x1b>",                                // normal cursor keys, numeric keypad
    "\x1b[>4m\x1b[<99u",                            // modifyOtherKeys, kitty keyboard off
    "\x1b[?5l\x1b[?6l\x1b[?7h\x1b[4l",              // normal video, origin off, wrap, replace
    "\x1b[?1049l",                                  // leave the alternate screen
    "\x1b(B\x1b)B\x0f",                             // ASCII character sets
    "\x1b]104\x1b\\\x1b]110\x1b\\\x1b]111\x1b\\\x1b]112\x1b\\", // default palette and colors
    "\x1b[0m\x1b[0 q\x1b[?25h", // plain text, default cursor shape, visible cursor
    "\x1b[23;0t",               // restore the window title saved at session start
);
/// Saves the window title so [`RESET`] can restore it.
const TITLE_PUSH: &str = "\x1b[22;0t";

/// Parses `ctrl-]`, `ctrl-q`, `^]` and similar. Keys the shell or remote apps
/// need (Ctrl-C, Ctrl-H/I/J/M, Ctrl-[ which is Esc) cannot be the escape.
pub fn parse_escape(text: &str) -> Result<u8> {
    let lower = text.trim().to_ascii_lowercase();
    let key = lower
        .strip_prefix("ctrl-")
        .or_else(|| lower.strip_prefix("ctrl+"))
        .or_else(|| lower.strip_prefix('^'))
        .ok_or("session escape must look like ctrl-] or ctrl-q")?;
    let [key] = key.as_bytes() else {
        return Err("session escape must be Ctrl plus one key".into());
    };
    let byte = match key {
        b'a'..=b'z' => key - b'a' + 1,
        b'\\' | b']' | b'^' | b'_' => key & 0x1f,
        _ => return Err("session escape must be Ctrl plus a letter or one of \\ ] ^ _".into()),
    };
    if matches!(byte, 3 | 8 | 9 | 10 | 13) {
        return Err(
            "Ctrl-C, Ctrl-H, Ctrl-I, Ctrl-J and Ctrl-M cannot be the session escape".into(),
        );
    }
    Ok(byte)
}
/// Sets the escape for this process (from `--session-escape`).
pub fn set_escape(byte: u8) {
    ESCAPE.store(byte, Ordering::Relaxed);
}
pub fn escape_byte() -> u8 {
    ESCAPE.load(Ordering::Relaxed)
}
/// `Ctrl-]` style label for hints and reviews.
pub fn escape_label() -> String {
    label(escape_byte())
}
fn label(byte: u8) -> String {
    match byte {
        1..=26 => format!("Ctrl-{}", char::from(b'A' + byte - 1)),
        _ => format!("Ctrl-{}", char::from(byte | 0x40)),
    }
}

pub fn validate_lifetime(seconds: u16) -> Result<()> {
    if !(1..=3600).contains(&seconds) {
        return Err("interactive session lifetime must be 1..3600 seconds".into());
    }
    Ok(())
}
pub fn review(seconds: u16) -> String {
    let escape = escape_label();
    format!(
        "Full interactive terminal (remote TTY, like kubectl exec -it); {seconds}-second maximum lifetime.\nThe remote program controls your terminal: keys, pastes, mouse and escape sequences pass through unfiltered both ways, so vim, top and less work (it can also set the title or clipboard).\n{escape} disconnects locally; Ctrl-C and every other key go to the remote process. Window resizes are forwarded.\nOn return KBorg resets terminal modes (alternate screen, mouse, paste, keypad, cursor, colors) and redraws.\nLimits: 16 MiB received output, 8 MiB typed input; reaching one ends the session and says which.\nDisconnect does not guarantee remote process termination; commands may have changed the workload."
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

/// Input limits for one stream: the filtered controlled mode keeps the strict
/// 1 MiB budget; a full TTY allows more typing/mouse input and slower writes.
#[derive(Clone, Copy)]
struct Limits {
    input: usize,
    write_budget: Duration,
}
const FILTERED: Limits = Limits {
    input: MAX_INPUT,
    write_budget: WRITE_BUDGET,
};
const FULL_TTY: Limits = Limits {
    input: MAX_TTY_INPUT,
    write_budget: Duration::from_secs(5),
};

/// Drives a reviewed native stream without owning a terminal. Cancellation takes
/// priority over output, and the output callback must remain bounded/nonblocking.
/// Output is filtered to text and safe colors (this is not the full-TTY path).
pub async fn pump(
    process: AttachedProcess,
    seconds: u16,
    inputs: mpsc::Receiver<Input>,
    mut emit: impl FnMut(&str) -> Result<()>,
) -> Result<Outcome> {
    let mut filters = [OutputFilter::default(), OutputFilter::default()];
    pump_bytes(process, seconds, inputs, FILTERED, |stream, bytes| {
        emit(&filters[stream].push(bytes))
    })
    .await
}

/// The shared stream loop. `emit` receives raw remote bytes with the stream
/// index (0 stdout, 1 stderr); callers decide whether to filter them.
async fn pump_bytes(
    mut process: AttachedProcess,
    seconds: u16,
    mut inputs: mpsc::Receiver<Input>,
    limits: Limits,
    mut emit: impl FnMut(usize, &[u8]) -> Result<()>,
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
                        // A full queue (fast window dragging) waits briefly; a
                        // later resize carries the final size if this one drops.
                        let ready = tokio::time::timeout(WRITE_BUDGET, std::future::poll_fn(|cx| sender.poll_ready(cx))).await;
                        match ready {
                            Ok(Ok(())) => sender.try_send(TerminalSize {width,height}).map_err(|_| "remote terminal resize queue unavailable")?,
                            Ok(Err(_)) => return Err("remote terminal resize queue unavailable".into()),
                            Err(_) => {}
                        }
                    } else { return Err("remote session has no TTY resize stream".into()); }
                },
                Some(Input::Bytes(bytes)) => {
                    if bytes.len() >MAX_INPUT_CHUNK || outcome.input_bytes.saturating_add(bytes.len()) >limits.input { outcome.end=End::InputLimit; break; }
                    if let Some(writer) = stdin.as_mut() {
                        tokio::time::timeout(limits.write_budget, writer.write_all(&bytes)).await.map_err(|_| "remote stdin blocked; session disconnected, remote outcome unknown")?.map_err(|_| "remote stdin failed; remote outcome unknown")?;
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
                    emit(0, &output[..count])?;
                }
            },
            read = async { match stderr.as_mut() { Some(reader)=>reader.read(&mut errors).await, None=>std::future::pending().await } }, if !stderr_closed => {
                let count = read.map_err(|_| "remote stderr failed; remote outcome unknown")?;
                if count ==0 { stderr_closed=true; }
                else {
                    outcome.output_bytes = outcome.output_bytes.saturating_add(count);
                    if outcome.output_bytes >MAX_OUTPUT { outcome.end=End::OutputLimit; break; }
                    emit(1, &errors[..count])?;
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

/// The one-line notice printed before the remote program takes the terminal.
fn banner(target: &str, escape: u8) -> String {
    format!(
        "KBorg session: {}. {} returns to KBorg.\r\n",
        safety::safe_text(target, 200),
        label(escape)
    )
}

/// Raw mode for a full-TTY session. Drop writes [`RESET`] and restores the
/// previous mode on every exit path: return, error, disconnect, panic unwind.
struct TerminalMode {
    original_raw: bool,
}
impl TerminalMode {
    fn enter(banner: &str) -> Result<Self> {
        ensure_terminal()?;
        let original_raw =
            terminal::is_raw_mode_enabled().map_err(|_| "cannot inspect terminal mode")?;
        if !original_raw {
            terminal::enable_raw_mode().map_err(|_| "cannot enable session terminal mode")?;
        }
        let mode = Self { original_raw };
        let mut out = io::stdout().lock();
        out.write_all(TITLE_PUSH.as_bytes())
            .and_then(|()| out.write_all(banner.as_bytes()))
            .and_then(|()| out.flush())
            .map_err(|_| "cannot write to the session terminal")?;
        Ok(mode)
    }
}
impl Drop for TerminalMode {
    fn drop(&mut self) {
        let mut out = io::stdout().lock();
        let _ = out
            .write_all(RESET.as_bytes())
            .and_then(|()| out.write_all(b"\r\n"))
            .and_then(|()| out.flush());
        drop(out);
        if !self.original_raw {
            let _ = terminal::disable_raw_mode();
        }
    }
}

/// Finds the local escape key in raw terminal input: the plain control byte,
/// or the kitty (`CSI 93;5u`) and xterm modifyOtherKeys (`CSI 27;5;93~`)
/// encodings a remote program may switch on, so it cannot trap the user.
/// The last bytes are kept so a sequence split across reads is still found.
pub struct EscapeDetector {
    byte: u8,
    patterns: [Vec<u8>; 3],
    tail: Vec<u8>,
}
impl EscapeDetector {
    pub fn new(byte: u8) -> Self {
        let code = match byte {
            1..=26 => b'a' + byte - 1,
            _ => byte | 0x40,
        };
        Self {
            byte,
            patterns: [
                format!("\x1b[{code};5u"),
                format!("\x1b[{code};5:"),
                format!("\x1b[27;5;{code}~"),
            ]
            .map(String::into_bytes),
            tail: Vec::with_capacity(32),
        }
    }
    /// Returns how many leading bytes of `chunk` to forward and whether the
    /// escape was pressed; bytes from the escape on are never forwarded.
    pub fn scan(&mut self, chunk: &[u8]) -> (usize, bool) {
        let mut combined = std::mem::take(&mut self.tail);
        let carried = combined.len();
        combined.extend_from_slice(chunk);
        let raw = chunk
            .iter()
            .position(|byte| *byte == self.byte)
            .map(|index| index + carried);
        let sequence = self
            .patterns
            .iter()
            .filter_map(|pattern| {
                combined
                    .windows(pattern.len())
                    .position(|window| window == pattern.as_slice())
            })
            .min();
        let found = match (raw, sequence) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        if let Some(start) = found {
            return (start.saturating_sub(carried), true);
        }
        let keep = combined.len().saturating_sub(15);
        self.tail = combined.split_off(keep);
        (chunk.len(), false)
    }
}

#[cfg(unix)]
enum Raw {
    Bytes(Vec<u8>),
    Escape,
    Closed,
}

/// Reads raw stdin on its own thread. It polls with a short timeout and checks
/// `stop` before every read, so no keystroke meant for KBorg is consumed after
/// the session ends; Drop stops and joins it.
#[cfg(unix)]
struct StdinReader {
    stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
#[cfg(unix)]
impl StdinReader {
    fn spawn(escape: u8, sender: mpsc::Sender<Raw>) -> Result<Self> {
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("kborg-session-stdin".into())
            .spawn(move || read_stdin(escape, &flag, &sender))
            .map_err(|_| "cannot start session input")?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}
#[cfg(unix)]
impl Drop for StdinReader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
#[cfg(unix)]
fn read_stdin(escape: u8, stop: &std::sync::atomic::AtomicBool, sender: &mpsc::Sender<Raw>) {
    use rustix::{
        event::{PollFd, PollFlags, Timespec, poll},
        io::Errno,
    };
    use std::os::fd::AsFd;
    let stdin = io::stdin();
    let fd = stdin.as_fd();
    let mut detector = EscapeDetector::new(escape);
    let mut buffer = [0u8; MAX_INPUT_CHUNK];
    let timeout = Timespec {
        tv_sec: 0,
        tv_nsec: 50_000_000,
    };
    let end = loop {
        if stop.load(Ordering::Relaxed) {
            return;
        }
        let mut fds = [PollFd::from_borrowed_fd(fd, PollFlags::IN)];
        match poll(&mut fds, Some(&timeout)) {
            Ok(0) | Err(Errno::INTR) => continue,
            Ok(_) => {}
            Err(_) => break Raw::Closed,
        }
        let events = fds[0].revents();
        if !events.contains(PollFlags::IN) {
            if events.intersects(PollFlags::HUP | PollFlags::ERR | PollFlags::NVAL) {
                break Raw::Closed;
            }
            continue;
        }
        if stop.load(Ordering::Relaxed) {
            return;
        }
        match rustix::io::read(fd, &mut buffer[..]) {
            Ok(0) => break Raw::Closed,
            Ok(count) => {
                let (forward, escaped) = detector.scan(&buffer[..count]);
                if forward > 0
                    && sender
                        .blocking_send(Raw::Bytes(buffer[..forward].to_vec()))
                        .is_err()
                {
                    return;
                }
                if escaped {
                    break Raw::Escape;
                }
            }
            Err(Errno::INTR | Errno::AGAIN) => continue,
            Err(_) => break Raw::Closed,
        }
    };
    let _ = sender.blocking_send(end);
}

/// Unix: forwards every stdin byte unchanged (escape sequences, pastes, mouse
/// reports) and SIGWINCH resizes until the escape key or stdin closes.
#[cfg(unix)]
async fn forward_input(sender: mpsc::Sender<Input>, escape: u8) -> Result<()> {
    use tokio::signal::unix::{SignalKind, signal};
    let (raw_sender, mut raw) = mpsc::channel(64);
    let _reader = StdinReader::spawn(escape, raw_sender)?;
    let mut resized =
        signal(SignalKind::window_change()).map_err(|_| "cannot watch terminal resizes")?;
    let mut size = terminal::size().ok();
    loop {
        tokio::select! {
            item = raw.recv() => {
                let last = match item {
                    Some(Raw::Bytes(bytes)) => {
                        for chunk in bytes.chunks(MAX_INPUT_CHUNK) {
                            sender
                                .send(Input::Bytes(chunk.to_vec()))
                                .await
                                .map_err(|_| "session input closed")?;
                        }
                        continue;
                    }
                    Some(Raw::Escape) => Input::Cancel,
                    Some(Raw::Closed) | None => Input::CloseStdin,
                };
                sender.send(last).await.map_err(|_| "session input closed")?;
                return std::future::pending().await;
            }
            _ = resized.recv() => {
                let current = terminal::size().ok();
                if current != size
                    && let Some((width, height)) = current
                {
                    size = current;
                    sender
                        .send(Input::Resize { width, height })
                        .await
                        .map_err(|_| "session input closed")?;
                }
            }
        }
    }
}

/// Other platforms: translate key events to the bytes a VT terminal sends.
#[cfg(not(unix))]
async fn forward_input(sender: mpsc::Sender<Input>, escape: u8) -> Result<()> {
    use crossterm::event::{self, Event};
    loop {
        if event::poll(Duration::ZERO).map_err(|_| "session input polling failed")? {
            let inputs = match event::read().map_err(|_| "session input failed")? {
                Event::Key(key) => key_input(key, escape).into_iter().collect(),
                Event::Resize(width, height) => vec![Input::Resize { width, height }],
                Event::Paste(text) => text
                    .as_bytes()
                    .chunks(MAX_INPUT_CHUNK)
                    .map(|chunk| Input::Bytes(chunk.to_vec()))
                    .collect(),
                _ => Vec::new(),
            };
            for input in inputs {
                let canceled = matches!(input, Input::Cancel);
                sender
                    .send(input)
                    .await
                    .map_err(|_| "session input closed")?;
                if canceled {
                    return std::future::pending().await;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[cfg(any(not(unix), test))]
fn key_input(key: KeyEvent, escape: u8) -> Option<Input> {
    if key.kind == KeyEventKind::Release {
        return None;
    }
    let control = key.modifiers.contains(KeyModifiers::CONTROL);
    if control
        && let KeyCode::Char(c) = key.code
        && c.is_ascii()
        && (c as u8) & 0x1f == escape
    {
        return Some(Input::Cancel);
    }
    let sequence = |text: &str| text.as_bytes().to_vec();
    let mut bytes = match key.code {
        KeyCode::Char(c) if control && c.is_ascii() => vec![(c as u8) & 0x1f],
        KeyCode::Char(c) => {
            let mut buffer = [0; 4];
            c.encode_utf8(&mut buffer).as_bytes().to_vec()
        }
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => sequence("\x1b[Z"),
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => sequence("\x1b[A"),
        KeyCode::Down => sequence("\x1b[B"),
        KeyCode::Right => sequence("\x1b[C"),
        KeyCode::Left => sequence("\x1b[D"),
        KeyCode::Home => sequence("\x1b[H"),
        KeyCode::End => sequence("\x1b[F"),
        KeyCode::PageUp => sequence("\x1b[5~"),
        KeyCode::PageDown => sequence("\x1b[6~"),
        KeyCode::Insert => sequence("\x1b[2~"),
        KeyCode::Delete => sequence("\x1b[3~"),
        KeyCode::F(n @ 1..=4) => vec![0x1b, b'O', b'P' + n - 1],
        KeyCode::F(n @ 5..=12) => {
            let code = [15, 17, 18, 19, 20, 21, 23, 24][usize::from(n - 5)];
            sequence(&format!("\x1b[{code}~"))
        }
        _ => return None,
    };
    if key.modifiers.contains(KeyModifiers::ALT) {
        bytes.insert(0, 0x1b);
    }
    Some(Input::Bytes(bytes))
}

/// Runs a reviewed session as a full TTY passthrough in the real terminal,
/// like `kubectl exec -it`: remote output is written unfiltered, every input
/// byte and resize is forwarded, the escape key disconnects, and [`RESET`]
/// runs on every exit path. `target` names the reviewed Pod/container.
pub async fn terminal(process: AttachedProcess, seconds: u16, target: &str) -> Result<Outcome> {
    validate_lifetime(seconds)?;
    let escape = escape_byte();
    let _mode = TerminalMode::enter(&banner(target, escape))?;
    let (sender, inputs) = mpsc::channel(64);
    if let Ok((width, height)) = terminal::size() {
        let _ = sender.try_send(Input::Resize { width, height });
    }
    let mut stdout = io::stdout();
    let stream = pump_bytes(process, seconds, inputs, FULL_TTY, |_, bytes| {
        stdout
            .write_all(bytes)
            .and_then(|()| stdout.flush())
            .map_err(|_| "local terminal output failed".into())
    });
    let keyboard = forward_input(sender, escape);
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
            key_input(
                KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL),
                DEFAULT_ESCAPE
            ),
            Some(Input::Cancel)
        ));
        let bytes = |code, modifiers| match key_input(KeyEvent::new(code, modifiers), 0x1d) {
            Some(Input::Bytes(bytes)) => bytes,
            _ => Vec::new(),
        };
        assert_eq!(bytes(KeyCode::Char('c'), KeyModifiers::CONTROL), [3]);
        // Full-screen apps need cursor, paging and function keys as VT bytes.
        assert_eq!(bytes(KeyCode::Left, KeyModifiers::NONE), b"\x1b[D");
        assert_eq!(bytes(KeyCode::PageDown, KeyModifiers::NONE), b"\x1b[6~");
        assert_eq!(bytes(KeyCode::F(1), KeyModifiers::NONE), b"\x1bOP");
        assert_eq!(bytes(KeyCode::F(12), KeyModifiers::NONE), b"\x1b[24~");
        assert_eq!(bytes(KeyCode::Char('x'), KeyModifiers::ALT), b"\x1bx");
        // A remapped escape frees Ctrl-] for the remote program.
        assert!(matches!(
            key_input(
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::CONTROL),
                0x11
            ),
            Some(Input::Cancel)
        ));
    }
    #[test]
    fn session_escape_is_configurable_but_never_a_key_the_shell_needs() -> Result<()> {
        assert_eq!(parse_escape("ctrl-]")?, 0x1d);
        assert_eq!(parse_escape("Ctrl+Q")?, 0x11);
        assert_eq!(parse_escape("^\\")?, 0x1c);
        for bad in [
            "ctrl-c", "ctrl-m", "ctrl-[", "ctrl-i", "ctrl-1", "q", "ctrl-ab", "",
        ] {
            assert!(parse_escape(bad).is_err(), "{bad}");
        }
        assert_eq!(label(0x1d), "Ctrl-]");
        assert_eq!(label(0x11), "Ctrl-Q");
        assert!(banner("ns/pod\x1b]52;c;x\x07 (app)", 0x1d).contains("Ctrl-] returns to KBorg"));
        assert!(!banner("ns/pod\x1b]52;c;x\x07", 0x1d).contains('\x1b'));
        Ok(())
    }
    #[test]
    fn escape_is_found_raw_or_in_enhanced_keyboard_encodings_even_when_split() {
        let mut detector = EscapeDetector::new(DEFAULT_ESCAPE);
        // Esc, arrows, mouse reports and UTF-8 pass through untouched.
        for chunk in [&b"\x1b"[..], b"\x1b[A\x1b[<0;10;5M", "é:wq\r".as_bytes()] {
            assert_eq!(detector.scan(chunk), (chunk.len(), false));
        }
        assert_eq!(EscapeDetector::new(0x1d).scan(b"ab\x1dcd"), (2, true));
        assert_eq!(EscapeDetector::new(0x1d).scan(b"x\x1b[93;5u"), (1, true));
        assert_eq!(EscapeDetector::new(0x1d).scan(b"\x1b[93;5:1u"), (0, true));
        assert_eq!(EscapeDetector::new(0x1d).scan(b"\x1b[27;5;93~"), (0, true));
        assert_eq!(EscapeDetector::new(0x11).scan(b"\x1b[113;5u"), (0, true));
        let mut split = EscapeDetector::new(0x1d);
        assert_eq!(split.scan(b"ls\x1b[93"), (6, false));
        assert_eq!(split.scan(b";5u more"), (0, true));
        // Ctrl-] with another escape configured is an ordinary remote key.
        assert_eq!(EscapeDetector::new(0x11).scan(b"\x1d"), (1, false));
    }
    #[test]
    fn reset_undoes_every_mode_a_remote_program_can_leave_behind() {
        assert!(
            RESET.starts_with("\x18\x1b\\"),
            "abort unfinished sequences first"
        );
        for mode in [
            "\x1b[?1049l", // alternate screen
            "\x1b[?2004l", // bracketed paste
            "\x1b[?1000l", // mouse tracking
            "\x1b[?1002l",
            "\x1b[?1003l",
            "\x1b[?1006l",
            "\x1b[?1004l", // focus events
            "\x1b[?1l",    // application cursor keys
            "\x1b>",       // application keypad
            "\x1b[<99u",   // kitty keyboard
            "\x1b[r",      // scroll region
            "\x1b[?7h",    // autowrap
            "\x1b(B",      // line drawing charset
            "\x1b[0m",     // SGR
            "\x1b[?25h",   // cursor visibility
        ] {
            assert!(RESET.contains(mode), "{mode:?}");
        }
        let position = |part: &str| RESET.find(part).unwrap_or(usize::MAX);
        assert!(position("\x1b[?1049l") < position("\x1b[?25h"));
        assert!(RESET.ends_with("\x1b[23;0t") && TITLE_PUSH == "\x1b[22;0t");
        // Every part is a control sequence: nothing visible is printed.
        let visible = OutputFilter::default().push(RESET.as_bytes());
        assert_eq!(visible.replace("\x1b[0m", ""), "");
    }
    #[test]
    fn full_tty_keeps_session_limits_but_allows_interactive_input() {
        assert_eq!(FILTERED.input, MAX_INPUT);
        assert!(FULL_TTY.input > FILTERED.input && FULL_TTY.write_budget > WRITE_BUDGET);
        let review = review(60);
        assert!(
            review.contains("pass through unfiltered") && review.contains("Ctrl-] disconnects")
        );
        assert!(review.contains("16 MiB received output") && review.contains("ends the session"));
    }
}
