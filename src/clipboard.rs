//! Explicit log selection to the desktop clipboard, or to the terminal over SSH.
use crate::{Result, safety};
use base64::Engine;
use std::io::{self, IsTerminal, Write};
#[cfg(any(target_os = "macos", target_os = "linux", windows))]
use std::{
    process::{Command, Stdio},
    time::{Duration, Instant},
};

// Fits within common terminal clipboard sequence limits even after base64.
const MAX_COPY_BYTES: usize = 48 * 1024;

pub(crate) enum ClipboardTarget {
    System,
    Terminal,
}

pub(crate) fn copy_log_text(text: &str) -> Result<ClipboardTarget> {
    let text = clipboard_text(text)?;
    // An SSH session's desktop, if there is one, is on the other machine.
    if ["SSH_CONNECTION", "SSH_CLIENT", "SSH_TTY"]
        .iter()
        .all(|name| !env_set(name))
        && copy_system(&text)
    {
        return Ok(ClipboardTarget::System);
    }
    let stdout = io::stdout();
    if !stdout.is_terminal() {
        return Err("No system clipboard available and output is not a terminal.".into());
    }
    write_terminal(&mut stdout.lock(), &text)
        .map_err(|_| "Could not send the selection to the terminal clipboard.")?;
    Ok(ClipboardTarget::Terminal)
}

fn clipboard_text(text: &str) -> Result<String> {
    if text.len() > MAX_COPY_BYTES {
        return Err("Selection exceeds 48 KiB. Select less text or use the log export.".into());
    }
    let text = text
        .split('\n')
        .map(|line| safety::safe_text(line, MAX_COPY_BYTES))
        .collect::<Vec<_>>()
        .join("\n");
    if text.is_empty() {
        return Err("Select log text with the mouse first.".into());
    }
    Ok(text)
}

fn env_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// Send already sanitized, bounded text as one terminal clipboard request.
pub(crate) fn write_terminal(out: &mut impl Write, text: &str) -> io::Result<()> {
    let encoded = base64::engine::general_purpose::STANDARD.encode(text);
    write!(out, "\x1b]52;c;{encoded}\x07")?;
    out.flush()
}

#[cfg(target_os = "macos")]
fn copy_system(text: &str) -> bool {
    // pbcopy can advertise rich formats based on the input header. A log
    // selection should always be pasted as literal plain text.
    if text.starts_with("{\\rtf") || text.starts_with("%!PS") {
        return false;
    }
    let mut command = Command::new("/usr/bin/pbcopy");
    command.env("LC_ALL", "en_US.UTF-8");
    run_helper(command, text.as_bytes(), helper_deadline())
}

#[cfg(target_os = "linux")]
fn copy_system(text: &str) -> bool {
    let deadline = helper_deadline();
    if env_set("WAYLAND_DISPLAY") {
        let mut command = Command::new("wl-copy");
        command.args(["--type", "text/plain"]);
        if run_helper(command, text.as_bytes(), deadline) {
            return true;
        }
    }
    if env_set("DISPLAY") {
        let mut xclip = Command::new("xclip");
        xclip.args(["-in", "-selection", "clipboard"]);
        let mut xsel = Command::new("xsel");
        xsel.args(["--input", "--clipboard"]);
        return run_helper(xclip, text.as_bytes(), deadline)
            || run_helper(xsel, text.as_bytes(), deadline);
    }
    false
}

#[cfg(windows)]
fn copy_system(text: &str) -> bool {
    let Some(root) = std::env::var_os("SystemRoot").map(std::path::PathBuf::from) else {
        return false;
    };
    if !root.is_absolute() {
        return false;
    }
    let powershell = root.join("System32").join("WindowsPowerShell").join("v1.0");
    let mut command = Command::new(powershell.join("powershell.exe"));
    // Use the built-in clipboard cmdlet with a fixed command. Reading the
    // pipe directly keeps Unicode and multiline text out of the script and
    // avoids the legacy console code page used by clip.exe.
    command.env("PSModulePath", powershell.join("Modules")).args([
        "-NoLogo",
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        "$ErrorActionPreference='Stop'; \
         $reader=[IO.StreamReader]::new([Console]::OpenStandardInput(), [Text.Encoding]::UTF8, $false); \
         Set-Clipboard -Value ($reader.ReadToEnd())",
    ]);
    run_helper(
        command,
        text.as_bytes(),
        Instant::now() + Duration::from_secs(2),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux", windows)))]
fn copy_system(_: &str) -> bool {
    false
}

#[cfg(any(target_os = "macos", target_os = "linux"))]
fn helper_deadline() -> Instant {
    Instant::now() + Duration::from_secs(1)
}

#[cfg(any(target_os = "macos", target_os = "linux", windows))]
fn run_helper(mut command: Command, text: &[u8], deadline: Instant) -> bool {
    if Instant::now() >= deadline {
        return false;
    }
    // Pass log contents through stdin alone. A failed helper must not echo
    // the selection to the UI or block it while waiting on a desktop session.
    let Ok(mut child) = command
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    let (finished, input_result) = std::sync::mpsc::sync_channel(1);
    let input = child.stdin.take();
    let bytes = text.to_vec();
    let writer = std::thread::Builder::new().spawn(move || {
        let written = input.is_some_and(|mut input| input.write_all(&bytes).is_ok());
        let _ = finished.send(written);
    });
    if writer.is_ok() {
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return status.success()
                        && input_result
                            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
                            == Ok(true);
                }
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(5));
                }
                _ => break,
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_clipboard_has_only_one_escape_and_preserves_selected_newlines() -> Result<()> {
        let text = clipboard_text("api 🛠️\n\x1b]52;c;injected\x07a\u{202e}b\0\n\n")?;
        assert_eq!(text, "api 🛠️\n]52;c;injectedab\n\n");
        let mut output = Vec::new();
        write_terminal(&mut output, &text).map_err(|error| error.to_string())?;
        let payload = output
            .strip_prefix(b"\x1b]52;c;")
            .and_then(|output| output.strip_suffix(b"\x07"))
            .ok_or("Malformed OSC 52 frame")?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|error| error.to_string())?;
        assert_eq!(decoded, text.as_bytes());
        Ok(())
    }

    #[test]
    fn rejects_excessive_selection_instead_of_copying_a_partial_log() {
        assert!(clipboard_text(&"é".repeat(MAX_COPY_BYTES / 2)).is_ok());
        assert!(clipboard_text(&"é".repeat(MAX_COPY_BYTES / 2 + 1)).is_err());
        assert!(clipboard_text("\x1b\x07").is_err());
    }
}
