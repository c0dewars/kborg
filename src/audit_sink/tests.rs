use super::*;

fn event(name: &str) -> Event {
    Event::new(
        "scale",
        "prod",
        "https://userinfo-canary:pw@api.example.com:6443/path?x=1",
    )
    .target("shop", "deployments.apps", name, "uid-1")
    .review("REVIEW TEXT")
}
fn write(count: usize) -> Result<()> {
    for index in 0..count {
        let pending = intent(event(&format!("web-{index}")))?;
        pending.finish(&Ok::<(), String>(()))?;
    }
    Ok(())
}

#[test]
fn records_carry_identity_review_hash_and_outcome_only() -> Result<()> {
    let capture = Capture::new()?;
    let pending = intent(event("web"))?;
    pending.finish(&Err::<(), _>(
        "API said forbidden: token SECRET_TOKEN_CANARY".to_owned(),
    ))?;
    let records = capture.records();
    assert_eq!(records.len(), 2);
    let (intent, outcome) = (&records[0], &records[1]);
    assert_eq!(intent["stage"], "intent");
    assert_eq!(intent["schema"], "kborg.audit/1");
    assert_eq!(intent["kborg_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(intent["context"], "prod");
    assert_eq!(intent["server_host"], "api.example.com:6443");
    assert_eq!(intent["namespace"], "shop");
    assert_eq!(intent["kind"], "deployments.apps");
    assert_eq!(intent["name"], "web");
    assert_eq!(intent["uid"], "uid-1");
    assert_eq!(intent["action"], "scale");
    assert_eq!(intent["review_sha256"], sha256(b"REVIEW TEXT"));
    assert!(intent["os_user"].as_str().is_some_and(|u| !u.is_empty()));
    assert!(matches!(
        intent["confirmation"].as_str(),
        Some("strict" | "one-key")
    ));
    assert!(intent["ts"].as_str().is_some_and(|t| t.ends_with('Z')));
    assert_eq!(intent["prev"], GENESIS);
    assert!(intent.get("outcome").is_none());
    assert_eq!(outcome["stage"], "outcome");
    assert_eq!(outcome["id"], intent["id"]);
    assert_eq!(outcome["outcome"], "failed");
    assert_eq!(outcome["error_class"], "forbidden");
    let raw = capture.raw();
    // Distinctive markers: an OS user such as "runneradmin" is recorded.
    for canary in [
        "SECRET_TOKEN_CANARY",
        "REVIEW TEXT",
        "userinfo-canary",
        "pw@",
    ] {
        assert!(!raw.contains(canary), "{canary} leaked");
    }
    Ok(())
}

#[test]
fn outcomes_and_error_classes_are_fixed_words() {
    assert_eq!(
        outcome_of("outcome UNKNOWN after timeout"),
        Outcome::Unknown
    );
    assert_eq!(outcome_of("conflict"), Outcome::Failed);
    for (error, class) in [
        ("Operation timed out", "timeout"),
        ("object changed since review", "conflict"),
        ("Secret not found", "not-found"),
        ("review expired; prepare again", "review-rejected"),
        ("Copilot request canceled", "canceled"),
        ("anything else with a password=hunter2", "error"),
    ] {
        assert_eq!(error_class(error), class, "{error}");
    }
    assert_eq!(rfc3339(0), "1970-01-01T00:00:00.000Z");
    assert_eq!(
        rfc3339(1_709_210_096_789_000_000),
        "2024-02-29T12:34:56.789Z"
    );
    assert_eq!(server_host("https://10.0.0.1:443"), "10.0.0.1:443");
    assert_eq!(server_host("demo"), "demo");
}

#[test]
fn chain_verifies_and_detects_removed_and_edited_lines() -> Result<()> {
    let capture = Capture::new()?;
    write(3)?;
    let verified = verify(std::slice::from_ref(&capture.file))?;
    assert_eq!(verified.records, 6);
    assert_eq!(verified.first_prev, GENESIS);
    let lines: Vec<String> = capture.raw().lines().map(str::to_owned).collect();
    let tampered = capture.dir.join("tampered.jsonl");
    let save = |lines: &[String]| -> Result<()> {
        std::fs::write(&tampered, format!("{}\n", lines.join("\n"))).map_err(|_| "write".into())
    };
    // Removed line.
    let mut removed = lines.clone();
    removed.remove(2);
    save(&removed)?;
    let error = verify(std::slice::from_ref(&tampered))
        .err()
        .ok_or("removed line must fail")?;
    assert!(error.contains(":3: CHAIN BROKEN"), "{error}");
    // Edited line (still valid JSON).
    let mut edited = lines.clone();
    edited[1] = edited[1].replace("\"ok\"", "\"failed\"");
    assert_ne!(edited[1], lines[1]);
    save(&edited)?;
    let error = verify(std::slice::from_ref(&tampered))
        .err()
        .ok_or("edited line must fail")?;
    assert!(error.contains(":3: CHAIN BROKEN"), "{error}");
    // Removing the last line cannot be seen inside the file, but the next
    // record KBorg writes no longer chains to it... the chain continues from
    // what is there; reordering is caught.
    let mut swapped = lines.clone();
    swapped.swap(3, 4);
    save(&swapped)?;
    assert!(verify(std::slice::from_ref(&tampered)).is_err());
    // A truncated (partial) final record is reported.
    std::fs::write(&tampered, format!("{}\n{}", lines[0], &lines[1][..10])).map_err(|_| "write")?;
    assert!(
        verify(std::slice::from_ref(&tampered))
            .err()
            .is_some_and(|e| e.contains("partial record"))
    );
    Ok(())
}

#[test]
fn rotation_caps_size_keeps_bounded_files_and_chains_across_them() -> Result<()> {
    let capture = Capture::new()?;
    set_thread_settings(Some(Settings {
        file: Some(capture.file.clone()),
        max_bytes: 4096,
        ..Settings::default()
    }));
    write(60)?;
    let numbered = |n: u32| {
        let mut name = capture.file.as_os_str().to_owned();
        name.push(format!(".{n}"));
        PathBuf::from(name)
    };
    for n in 1..=KEEP {
        assert!(numbered(n).is_file(), "rotated file {n} missing");
    }
    assert!(!numbered(KEEP + 1).exists());
    let size = std::fs::metadata(&capture.file).map_err(|_| "meta")?.len();
    assert!(size <= 4096);
    for n in 1..=KEEP {
        assert!(std::fs::metadata(numbered(n)).map_err(|_| "meta")?.len() <= 4096);
    }
    let mut files: Vec<PathBuf> = (1..=KEEP).rev().map(numbered).collect();
    files.push(capture.file.clone());
    let verified = verify(&files)?;
    assert_ne!(verified.first_prev, GENESIS);
    // Files out of order break the chain.
    files.swap(0, 1);
    assert!(verify(&files).is_err());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&capture.file)
            .map_err(|_| "meta")?
            .permissions()
            .mode();
        assert_eq!(mode & 0o077, 0);
    }
    Ok(())
}

#[test]
fn required_audit_blocks_and_optional_audit_warns() -> Result<()> {
    let capture = Capture::new()?;
    let missing = capture.dir.join("no-such-dir").join("audit.jsonl");
    set_thread_settings(Some(Settings {
        file: Some(missing.clone()),
        required: true,
        ..Settings::default()
    }));
    let error = intent(event("web"))
        .err()
        .ok_or("required audit must block")?;
    assert!(error.starts_with("Change blocked"), "{error}");
    assert!(error.contains("nothing was sent") && error.contains("Next:"));
    set_thread_settings(Some(Settings {
        file: Some(missing),
        required: false,
        ..Settings::default()
    }));
    intent(event("web"))?.finish(&Ok::<(), String>(()))?;
    let warning = take_warning().ok_or("missing warning")?;
    assert!(warning.contains("Audit record not written") && warning.contains("Next:"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn audit_file_must_be_private_and_not_a_symlink() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let capture = Capture::new()?;
    std::fs::write(&capture.file, b"").map_err(|_| "write")?;
    std::fs::set_permissions(&capture.file, std::fs::Permissions::from_mode(0o644))
        .map_err(|_| "chmod")?;
    set_thread_settings(Some(Settings {
        file: Some(capture.file.clone()),
        required: true,
        ..Settings::default()
    }));
    assert!(
        intent(event("web"))
            .err()
            .is_some_and(|e| e.contains("chmod 600"))
    );
    let link = capture.dir.join("link.jsonl");
    std::os::unix::fs::symlink(&capture.file, &link).map_err(|_| "symlink")?;
    set_thread_settings(Some(Settings {
        file: Some(link),
        required: true,
        ..Settings::default()
    }));
    assert!(intent(event("web")).is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn syslog_receives_the_same_chained_records() -> Result<()> {
    use std::os::unix::net::UnixDatagram;
    let _capture = Capture::new()?;
    // A short path: macOS limits Unix socket paths to 104 bytes, and its
    // per-user temp folder alone uses most of that.
    let socket = std::path::Path::new("/tmp").join(format!(
        "kborg-syslog-{}-{}.sock",
        std::process::id(),
        unix_nanos()
    ));
    let receiver = UnixDatagram::bind(&socket).map_err(|_| "bind")?;
    struct Remove(std::path::PathBuf);
    impl Drop for Remove {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }
    let _cleanup = Remove(socket.clone());
    receiver
        .set_read_timeout(Some(std::time::Duration::from_secs(2)))
        .map_err(|_| "timeout")?;
    set_thread_settings(Some(Settings {
        syslog: true,
        syslog_socket: Some(socket.clone()),
        ..Settings::default()
    }));
    intent(event("web"))?.finish(&Err::<(), _>("timed out".to_owned()))?;
    let mut messages = Vec::new();
    for _ in 0..2 {
        let mut buffer = vec![0u8; 8192];
        let size = receiver.recv(&mut buffer).map_err(|_| "recv")?;
        messages.push(String::from_utf8_lossy(&buffer[..size]).into_owned());
    }
    assert!(messages[0].starts_with("<37>kborg["), "{}", messages[0]);
    assert!(messages[1].starts_with("<36>kborg["), "{}", messages[1]);
    let body = |message: &str| -> Result<String> {
        Ok(message.split_once("]: ").ok_or("syslog body")?.1.to_owned())
    };
    let first: Value = serde_json::from_str(&body(&messages[0])?).map_err(|_| "json")?;
    let second: Value = serde_json::from_str(&body(&messages[1])?).map_err(|_| "json")?;
    assert_eq!(second["prev"], sha256(body(&messages[0])?.as_bytes()));
    assert_eq!(first["stage"], "intent");
    assert_eq!(second["outcome"], "unknown");
    // A dead socket is an audit failure.
    drop(receiver);
    set_thread_settings(Some(Settings {
        syslog: true,
        syslog_socket: Some(socket),
        required: true,
        ..Settings::default()
    }));
    assert!(intent(event("web")).is_err());
    Ok(())
}

#[test]
fn settings_validation_explains_the_fix() {
    let relative = Settings {
        file: Some("audit.jsonl".into()),
        ..Settings::default()
    };
    assert!(relative.validate().is_err_and(|e| e.contains("Next:")));
    let required = Settings {
        required: true,
        ..Settings::default()
    };
    assert!(required.validate().is_err());
    let small = Settings {
        file: Some(PathBuf::from(crate::test_paths::abs(
            "/var/log/kborg.jsonl",
        ))),
        max_bytes: 10,
        ..Settings::default()
    };
    assert!(small.validate().is_err());
    let fine = Settings {
        file: Some(PathBuf::from(crate::test_paths::abs(
            "/var/log/kborg.jsonl",
        ))),
        required: true,
        ..Settings::default()
    };
    assert!(fine.validate().is_ok());
}

#[test]
fn no_sink_writes_nothing() -> Result<()> {
    set_thread_settings(Some(Settings::default()));
    intent(event("web"))?.finish(&Ok::<(), String>(()))?;
    set_thread_settings(None);
    Ok(())
}
