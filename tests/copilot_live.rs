//! Explicit opt-in acceptance using only this fixed public synthetic object.
//! The selected account is operator-authorized; no cluster API is contacted.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[tokio::test]
#[ignore = "explicit authorized account/network test; synthetic public bundle only"]
async fn synthetic_reviewed_copilot_advice() -> kborg::Result<()> {
    use serde_json::json;
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let options = kborg::advice::load_options(&root.join(".local/copilot-policy.json"))?;
    let object = json!({"apiVersion":"v1","kind":"Pod","metadata":{"name":"SYNTHETIC_PRIVATE_NAME_CANARY"},"spec":{"containers":[{"name":"unused","env":[{"name":"PASSWORD","value":"SYNTHETIC_SECRET_CANARY"}]}]},"status":{"phase":"Running","conditions":[{"type":"Ready","status":"False","reason":"ContainersNotReady","message":"PRIVATE_MESSAGE_CANARY"}],"containerStatuses":[{"ready":false,"restartCount":3,"state":{"waiting":{"reason":"CrashLoopBackOff","message":"PRIVATE_LOG_CANARY"}}}]}});
    let prepared = kborg::advice::prepare(&object, options).await?;
    let preview = prepared.preview()?;
    for canary in [
        "SYNTHETIC_PRIVATE_NAME_CANARY",
        "SYNTHETIC_SECRET_CANARY",
        "PRIVATE_MESSAGE_CANARY",
        "PRIVATE_LOG_CANARY",
    ] {
        assert!(!preview.contains(canary));
    }
    // This exact test invocation is the user's authorization for this fixed
    // synthetic transfer; production paths require interactive painted review.
    let typed = prepared.confirmation();
    let result = kborg::advice::run(prepared, &typed, None).await?;
    assert!(!result.answer.trim().is_empty());
    // The answer must reflect the payload: an earlier runtime passed -p and
    // the CLI ignored stdin, so Copilot answered without seeing the bundle.
    assert!(
        result.answer.to_ascii_lowercase().contains("crashloop"),
        "answer does not reflect the reviewed bundle"
    );
    assert!(result.network.approved_tunnels > 0);
    let sources: [(&str, &[u8]); 12] = [
        ("src/copilot.rs", include_bytes!("../src/copilot.rs")),
        ("src/advice.rs", include_bytes!("../src/advice.rs")),
        (
            "src/copilot_runtime.rs",
            include_bytes!("../src/copilot_runtime.rs"),
        ),
        (
            "src/copilot_proxy.rs",
            include_bytes!("../src/copilot_proxy.rs"),
        ),
        (
            "src/copilot_process.rs",
            include_bytes!("../src/copilot_process.rs"),
        ),
        ("src/ui_advice.rs", include_bytes!("../src/ui_advice.rs")),
        ("src/tui.rs", include_bytes!("../src/tui.rs")),
        ("src/cli.rs", include_bytes!("../src/cli.rs")),
        ("src/safety.rs", include_bytes!("../src/safety.rs")),
        ("Cargo.lock", include_bytes!("../Cargo.lock")),
        ("Cargo.toml", include_bytes!("../Cargo.toml")),
        ("tests/copilot_live.rs", include_bytes!("copilot_live.rs")),
    ];
    let hashes = sources
        .into_iter()
        .map(|(name, bytes)| {
            (
                name,
                ring::digest::digest(&ring::digest::SHA256, bytes)
                    .as_ref()
                    .iter()
                    .map(|byte| format!("{byte:02x}"))
                    .collect::<String>(),
            )
        })
        .collect::<std::collections::BTreeMap<_, _>>();
    for (name, expected) in &hashes {
        let bytes = std::fs::read(root.join(name))
            .map_err(|_| "cannot verify live evidence source stability")?;
        let actual = ring::digest::digest(&ring::digest::SHA256, &bytes)
            .as_ref()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(&actual, expected, "source changed during live test: {name}");
    }
    let evidence = json!({"passed":true,"date":"2026-10-23","runner":"cargo test --test copilot_live -- --ignored","tested_source_sha256":hashes,"fresh_os_denial_canaries_passed":true,"request_completed":true,"process_group_and_broker_cleanup_confirmed":true,"private_workspace_removal_confirmed":true,"gh_sha256":"81904d0aaa9fb3d9e35e13e7b1fe917451df0430eaee63144c69426d5d2832f1","synthetic_public_prompt_only":true,"cluster_api_contacted":false,"private_canaries_absent_from_bundle":true,"token_or_answer_retained_in_evidence":false,"credential_host":result.host,"credential_account":result.account,"native_version":"1.0.91","native_sha256":"87f04922933c139cf4af7cb6a80b96161428618a6275fea4b8dfe7e7a69c9518","model":result.model,"network":result.network,"notice":result.notice});
    let destination = root.join("docs/evidence/trust-copilot-live.json");
    std::fs::write(
        destination,
        serde_json::to_vec_pretty(&evidence).map_err(|_| "evidence encoding")?,
    )
    .map_err(|_| "cannot save synthetic evidence")?;
    println!(
        "Synthetic Copilot answer received through contained broker; no credential contents printed."
    );
    Ok(())
}
