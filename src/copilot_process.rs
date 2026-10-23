//! Bounded pipe I/O and process-group cleanup for the trusted Copilot runtime.
use crate::Result;
use std::{
    io::{Read, Write},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::{Duration, Instant},
};
pub(super) struct Capture {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
#[cfg(not(unix))]
pub(super) fn invoke(
    _: Command,
    _: Vec<u8>,
    _: Duration,
    _: Arc<AtomicBool>,
    _: usize,
) -> Result<Capture> {
    Err("Copilot process-group supervision is unsupported on this platform".into())
}
#[cfg(unix)]
pub(super) fn invoke(
    mut command: Command,
    input: Vec<u8>,
    budget: Duration,
    stop: Arc<AtomicBool>,
    limit: usize,
) -> Result<Capture> {
    use std::os::unix::process::CommandExt;
    if stop.load(Ordering::Acquire) {
        return Err("Copilot operation canceled before spawn".into());
    }
    let child = command
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .spawn()
        .map_err(|_| "cannot launch supervised Copilot process")?;
    struct Process {
        child: std::process::Child,
        cleaned: bool,
    }
    impl Process {
        fn cleanup(&mut self) -> Result<()> {
            if self.cleaned {
                return Ok(());
            }
            let mut error = None;
            match Command::new("/bin/kill")
                .args(["-KILL", "--", &format!("-{}", self.child.id())])
                .env_clear()
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(mut signal) => {
                    let deadline = Instant::now() + Duration::from_secs(1);
                    loop {
                        match signal.try_wait() {
                            Ok(Some(_)) => break,
                            Ok(None) if Instant::now() < deadline => {
                                thread::sleep(Duration::from_millis(5))
                            }
                            _ => {
                                let _ = signal.kill();
                                let _ = signal.try_wait();
                                error = Some("Copilot process-group signal deadline exceeded");
                                break;
                            }
                        }
                    }
                }
                Err(_) => error = Some("Copilot process-group signal failed"),
            }
            let _ = self.child.kill();
            let deadline = Instant::now() + Duration::from_secs(1);
            loop {
                match self.child.try_wait() {
                    Ok(Some(_)) => break,
                    Ok(None) if Instant::now() < deadline => {
                        thread::sleep(Duration::from_millis(5))
                    }
                    _ => {
                        error = Some("cannot confirm Copilot process reaping");
                        break;
                    }
                }
            }
            self.cleaned = true;
            error.map(|error| Err(error.into())).unwrap_or(Ok(()))
        }
    }
    impl Drop for Process {
        fn drop(&mut self) {
            let _ = self.cleanup();
        }
    }
    let mut process = Process {
        child,
        cleaned: false,
    };
    let mut stdin = process
        .child
        .stdin
        .take()
        .ok_or("Copilot stdin unavailable")?;
    let stdout = process
        .child
        .stdout
        .take()
        .ok_or("Copilot stdout unavailable")?;
    let stderr = process
        .child
        .stderr
        .take()
        .ok_or("Copilot stderr unavailable")?;
    enum Event {
        Bytes(bool, Vec<u8>),
        Input(bool),
        Eof(bool),
        Error,
    }
    let (tx, rx) = mpsc::sync_channel(8);
    let input_tx = tx.clone();
    let writer = thread::spawn(move || {
        let ok = stdin.write_all(&input).is_ok();
        drop(stdin);
        let _ = input_tx.send(Event::Input(ok));
    });
    let read = |mut reader: Box<dyn Read + Send>, stdout: bool, tx: mpsc::SyncSender<Event>| {
        thread::spawn(move || {
            let mut buffer = [0u8; 8192];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) => {
                        let _ = tx.send(Event::Eof(stdout));
                        break;
                    }
                    Ok(count) => {
                        if tx
                            .send(Event::Bytes(stdout, buffer[..count].to_vec()))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => {
                        let _ = tx.send(Event::Error);
                        break;
                    }
                }
            }
        })
    };
    let out = read(Box::new(stdout), true, tx.clone());
    let err = read(Box::new(stderr), false, tx.clone());
    drop(tx);
    let deadline = Instant::now() + budget;
    let mut capture = Capture {
        success: false,
        exit_code: None,
        signal: None,
        stdout: vec![],
        stderr: vec![],
    };
    let mut exited = None;
    let mut eof = [false; 2];
    let mut sent = false;
    let result = (|| -> Result<()> {
        loop {
            if stop.load(Ordering::Acquire) {
                return Err("Copilot operation canceled; a reviewed request may already have reached provider".into());
            }
            if Instant::now() >= deadline {
                return Err("Copilot operation exceeded its bounded deadline".into());
            }
            match rx.recv_timeout(Duration::from_millis(10)) {
                Ok(Event::Bytes(stdout, bytes)) => {
                    let target = if stdout {
                        &mut capture.stdout
                    } else {
                        &mut capture.stderr
                    };
                    if target.len().saturating_add(bytes.len()) > limit {
                        return Err("Copilot process output exceeded its capture limit; diagnostics suppressed".into());
                    }
                    target.extend(bytes);
                }
                Ok(Event::Input(ok)) => {
                    if !ok {
                        return Err("Copilot stopped reading the approved request".into());
                    }
                    sent = true;
                }
                Ok(Event::Eof(stdout)) => eof[usize::from(stdout)] = true,
                Ok(Event::Error) => return Err("Copilot pipe read failed".into()),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    if sent && eof.iter().all(|done| *done) {
                        // Pipe EOF may precede waitpid observing process exit.
                        thread::sleep(Duration::from_millis(2));
                    } else {
                        return Err("Copilot pipes closed without a complete bounded result".into());
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            if exited.is_none() {
                exited = process
                    .child
                    .try_wait()
                    .map_err(|_| "cannot inspect Copilot process")?;
            }
            if let Some(status) = exited {
                capture.success = status.success();
                capture.exit_code = status.code();
                use std::os::unix::process::ExitStatusExt;
                capture.signal = status.signal();
                process.cleanup()?;
                if sent && eof.iter().all(|done| *done) {
                    break;
                }
            }
        }
        Ok(())
    })();
    let cleanup = process.cleanup();
    drop(rx);
    let until = Instant::now() + Duration::from_secs(1);
    while !(writer.is_finished() && out.is_finished() && err.is_finished())
        && Instant::now() < until
    {
        thread::sleep(Duration::from_millis(5));
    }
    if !(writer.is_finished() && out.is_finished() && err.is_finished()) {
        return Err("Copilot pipes did not close after bounded process-group cleanup".into());
    }
    let _ = writer.join();
    let _ = out.join();
    let _ = err.join();
    cleanup?;
    result?;
    Ok(capture)
}
#[cfg(all(test, unix))]
mod tests {
    use super::*;
    #[test]
    fn stalled_stdin_and_descendant_pipes_are_bounded() -> Result<()> {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30 & wait"]).env_clear();
        let start = Instant::now();
        let result = invoke(
            command,
            vec![b'x'; 1024 * 1024],
            Duration::from_millis(100),
            Arc::new(AtomicBool::new(false)),
            4096,
        );
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(4));
        Ok(())
    }
    #[test]
    fn early_parent_exit_does_not_wait_for_descendant_output_pipe() -> Result<()> {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30 & exit 0"]).env_clear();
        let start = Instant::now();
        let result = invoke(
            command,
            vec![],
            Duration::from_secs(2),
            Arc::new(AtomicBool::new(false)),
            4096,
        )?;
        assert!(result.success);
        assert!(start.elapsed() < Duration::from_secs(3));
        Ok(())
    }
}
#[cfg(all(test, unix))]
mod cancel_tests {
    use super::*;
    #[test]
    fn cancellation_interrupts_stalled_input_without_waiting_for_deadline() -> Result<()> {
        let stop = Arc::new(AtomicBool::new(false));
        let trigger = stop.clone();
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "sleep 30 & wait"]).env_clear();
        let cancel = thread::spawn(move || {
            thread::sleep(Duration::from_millis(30));
            trigger.store(true, Ordering::Release);
        });
        let start = Instant::now();
        let result = invoke(
            command,
            vec![b'x'; 1024 * 1024],
            Duration::from_secs(10),
            stop,
            4096,
        );
        let _ = cancel.join();
        assert!(result.is_err());
        assert!(start.elapsed() < Duration::from_secs(3));
        Ok(())
    }
}
