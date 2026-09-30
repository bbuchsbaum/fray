//! A newer `fray drive` against a daemon built before controller detail
//! (Mote bd-01M3S3KRC8RBJ3X5H4RCPV4ZCC): the run must start its child, warn
//! once about the orphan check it loses, and finish normally.
//!
//! Needs an old daemon binary: set FRAY_OLD_DAEMON to e.g. a release build of
//! d3b1480. Without it the test reports that it was skipped.
use fray::model::random_key;
use std::{
    fs,
    path::PathBuf,
    process::{Command, Stdio},
};

#[test]
fn drive_runs_against_a_daemon_that_predates_controller_detail() {
    let Some(old) = std::env::var_os("FRAY_OLD_DAEMON").map(PathBuf::from) else {
        eprintln!("FRAY_OLD_DAEMON not set; skipping the old-daemon check");
        return;
    };
    let home = PathBuf::from("/tmp").join(format!("fray-dc-{}", &random_key().unwrap()[..8]));
    fs::create_dir_all(&home).unwrap();
    let fray = |bin: &PathBuf, args: &[&str]| {
        Command::new(bin)
            .env_remove("FRAY_AGENT")
            .env("FRAY_SESSION", "test:w")
            .arg("--home")
            .arg(&home)
            .args(["--as", "w"])
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    };
    let new = PathBuf::from(env!("CARGO_BIN_EXE_fray"));
    assert!(fray(&old, &["start"]).status.success());
    fray(&new, &["join"]);
    let marker = home.join("child-ran");
    let out = fray(
        &new,
        &[
            "drive",
            "--bootstrap",
            "--max-turns",
            "1",
            "--idle-timeout",
            "5",
            "--",
            "sh",
            "-c",
            &format!("cat >/dev/null; touch {}", marker.display()),
        ],
    );
    fray(&old, &["stop"]);
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let _ = fs::remove_dir_all(&home);
    assert!(!stderr.contains("unknown field: detail"), "{stderr}");
    assert!(stderr.contains("predates controller detail"), "{stderr}");
    assert!(stderr.contains("\"exit_reason\":\"max_turns\""), "{stderr}");
    assert!(
        marker.exists() || stderr.contains("\"verified\":true"),
        "{stderr}"
    );
}
