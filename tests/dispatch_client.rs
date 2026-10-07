//! Always-run transport court uses a synthetic public Mote CLI.
#[test]
fn public_readback_and_exact_confirmation_recovery() {
    let output = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/dispatch_client_integration.py"
        ))
        .arg(env!("CARGO_BIN_EXE_fray"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["passed"], 5);
}

#[test]
fn invalid_adoption_ttl_refuses_during_cli_parsing() {
    for ttl in ["0", "86401"] {
        let out = std::process::Command::new(env!("CARGO_BIN_EXE_fray"))
            .args([
                "--home",
                "/tmp/fray-invalid-ttl-unused",
                "--as",
                "alice",
                "--key",
                "invalid-ttl",
                "handoff",
                "1",
                "--to",
                "bob",
                "--state",
                "Partial",
                "--next",
                "Finish",
                "--reservation-ttl",
                ttl,
            ])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(2));
        assert!(String::from_utf8_lossy(&out.stderr).contains("reservation-ttl"));
    }
}
