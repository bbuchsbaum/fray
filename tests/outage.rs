//! A missing daemon must never look like a successful mutation.
use fray::model::random_key;
use std::{fs, os::unix::net::UnixListener, process::Command};

#[test]
fn stopped_and_stale_sockets_return_nonzero_explicit_cli_errors() {
    let dir = std::env::temp_dir().join(format!("fray-outage-{}", random_key().unwrap()));
    fs::create_dir(&dir).unwrap();
    for stale in [false, true] {
        if stale {
            let listener = UnixListener::bind(dir.join("bus.sock")).unwrap();
            drop(listener);
        }
        for args in [
            vec!["ping"],
            vec!["status", "Reviewing"],
            vec!["send", "peer", "Review requested"],
            vec!["ack", "1", "--through", "1"],
            vec!["brief"],
            vec!["agents"],
        ] {
            let out = Command::new(env!("CARGO_BIN_EXE_fray"))
                .args(["--home", dir.to_str().unwrap(), "--as", "fixture", "--json"])
                .args(&args)
                .env_remove("FRAY_SESSION")
                .env_remove("FRAY_AGENT")
                .output()
                .unwrap();
            assert!(
                !out.status.success(),
                "stale={stale} {args:?}: successful outage"
            );
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(
                text.contains("unavailable"),
                "stale={stale} {args:?}: {text}"
            );
            assert!(!text.trim().is_empty());
        }
    }
    fs::remove_dir_all(dir).unwrap();
}
