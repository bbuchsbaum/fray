//! Open items after 7234f53: queue patience, waiting vs. presence, and the
//! nearer host when Claude and Codex are nested.
use fray::{
    model::{random_key, Request},
    store::{Store, LANE_PATIENCE_MS, WAIT_HOLD_MS},
};
use serde_json::{json, Value};
use std::{path::Path, process::Command};

const NOW: i64 = 1_800_000_000_000;

fn run(s: &mut Store, actor: &str, op: &str, args: Value, at: i64) -> Result<Value, String> {
    s.execute_at(&Request::new(op, actor, args), at)
        .map_err(|e| e.code)
}

fn take(s: &mut Store, who: &str, paths: &[&str], queue: bool, at: i64) -> Result<Value, String> {
    run(
        s,
        who,
        "lane_take",
        json!({"paths": paths, "purpose": "work", "queue": queue}),
        at,
    )
}

fn board() -> Store {
    let mut s = Store::memory().unwrap();
    for who in ["claude", "codex", "deepseek"] {
        run(&mut s, who, "join", json!({}), NOW).unwrap();
    }
    s
}

#[test]
fn a_queue_waits_for_the_holder_only_so_long() {
    let mut s = board();
    take(&mut s, "claude", &["x/child"], false, NOW).unwrap();
    take(&mut s, "codex", &["x"], true, NOW).unwrap();
    // Inside the queued region, soon after: allowed, and codex is told.
    let inside = take(&mut s, "claude", &["x/more"], false, NOW + 1).unwrap();
    assert_eq!(inside["lane"]["state"], "held");
    let told = run(&mut s, "codex", "inbox", json!({}), NOW + 2).unwrap();
    assert!(
        serde_json::to_string(&told)
            .unwrap()
            .contains("Work continues inside your queued lane"),
        "{told}"
    );
    // After the patience window, new takes wait behind the queue.
    let late = NOW + LANE_PATIENCE_MS + 1;
    for who in ["claude", "codex"] {
        run(&mut s, who, "heartbeat", json!({}), late).unwrap();
    }
    assert_eq!(
        take(&mut s, "claude", &["x/again"], false, late).unwrap_err(),
        "lane_busy"
    );
    assert_eq!(
        take(&mut s, "claude", &["x/again"], true, late).unwrap()["lane"]["state"],
        "queued"
    );
}

#[test]
fn waiting_is_reachable_but_holds_lanes_only_for_a_while() {
    let mut s = board();
    take(&mut s, "claude", &["docs/"], false, NOW).unwrap();
    let stale = |s: &mut Store, at: i64| {
        run(s, "codex", "heartbeat", json!({}), at).unwrap();
        run(s, "codex", "lanes", json!({}), at).unwrap()["lanes"][0]["stale"].clone()
    };
    // Two hours of waiting after real activity: still holding.
    let two_hours = NOW + 2 * 60 * 60_000;
    s.touch("claude", None, two_hours).unwrap();
    assert_eq!(stale(&mut s, two_hours), false);
    // Past the hold window, still waiting: the lane is stale...
    let later = NOW + WAIT_HOLD_MS + 60_000;
    s.touch("claude", None, later).unwrap();
    assert_eq!(stale(&mut s, later), true);
    // ...but a message still reaches it without an absence notice.
    let sent = run(
        &mut s,
        "codex",
        "send",
        json!({"to":"claude","body":"hi"}),
        later,
    )
    .unwrap();
    assert!(sent["notice"].is_null(), "{sent}");
    // A wait that stopped refreshing is not waiting.
    let gone = later + 10 * 60_000;
    let sent = run(
        &mut s,
        "codex",
        "send",
        json!({"to":"claude","body":"still there?"}),
        gone,
    )
    .unwrap();
    assert!(sent["notice"].is_string(), "{sent}");
}

#[test]
fn codex_inside_claude_binds_the_codex_session() {
    let root = Path::new("/tmp").join(format!("fray-nest-{}", &random_key().unwrap()[..8]));
    std::fs::create_dir_all(&root).unwrap();
    let home = root.join("h");
    // A process named codex stands in for the Codex host.
    let codex = root.join("codex");
    std::os::unix::fs::symlink("/bin/sh", &codex).unwrap();
    let fray = env!("CARGO_BIN_EXE_fray");
    let base = |cmd: &mut Command| {
        cmd.env_remove("FRAY_SESSION")
            .env_remove("FRAY_AGENT")
            .env("CLAUDE_CODE_SESSION_ID", "outer")
            .env("CODEX_THREAD_ID", "inner");
    };
    let mut start = Command::new(fray);
    base(&mut start);
    start
        .args(["--home", home.to_str().unwrap(), "start"])
        .output()
        .unwrap();
    let mut join = Command::new(&codex);
    base(&mut join);
    let out = join
        .args([
            "-c",
            &format!(
                "{fray} --home {} --as nested --json join >/dev/null && {fray} --home {} --as nested --json agents",
                home.display(),
                home.display()
            ),
        ])
        .output()
        .unwrap();
    let mut stop = Command::new(fray);
    base(&mut stop);
    stop.args(["--home", home.to_str().unwrap(), "stop"])
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(&root);
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("codex:inner"), "{text}");
    assert!(!text.contains("claude:outer"), "{text}");
}
