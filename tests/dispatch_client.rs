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
    assert_eq!(result["passed"], 4);
}
