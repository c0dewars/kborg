//! Exercise real process output sinks against a closed downstream Unix socket.
#[cfg(unix)]
#[test]
fn closed_downstream_exits_cleanly_for_every_read_output_family() -> std::io::Result<()> {
    use std::{
        os::{fd::OwnedFd, unix::net::UnixStream},
        process::{Command, Stdio},
        time::{SystemTime, UNIX_EPOCH},
    };
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let settings = std::env::temp_dir()
        .canonicalize()?
        .join(format!("kborg-output-{}-{stamp}", std::process::id()));
    let commands: &[&[&str]] = &[
        &["--help"],
        &["--version"],
        &["completions", "bash"],
        &["theme", "list"],
        &["--demo", "list", "pods"],
        &["--demo", "list", "pods", "--json"],
        &["--demo", "get", "pods", "api-7d4f9"],
        &["--demo", "get", "pods", "api-7d4f9", "--yaml"],
        &["--demo", "metrics", "pods"],
        &["--demo", "metrics", "nodes", "--json"],
        &["--demo", "overview", "demo"],
        &["--demo", "overview", "demo", "--json"],
        &["--demo", "resources"],
        &["--demo", "contexts"],
        &["--demo", "doctor"],
        &["--demo", "pulses"],
        &["--demo", "xray", "pods"],
        &["--demo", "workloads"],
        &["--demo", "can", "u:demo"],
        &["dir", "."],
        &["--demo", "--allow-logs", "logs", "api-7d4f9"],
    ];
    for argv in commands {
        let (sender, receiver) = UnixStream::pair()?;
        drop(receiver);
        let result = Command::new(env!("CARGO_BIN_EXE_kborg"))
            .args(*argv)
            .env("KBORG_CONFIG_DIR", &settings)
            .stdout(Stdio::from(OwnedFd::from(sender)))
            .output()?;
        assert!(
            result.status.success(),
            "closed output failed for {argv:?}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            result.stderr.is_empty(),
            "unexpected diagnostic for {argv:?}"
        );
    }
    if settings.exists() {
        std::fs::remove_dir_all(settings)?;
    }
    Ok(())
}
