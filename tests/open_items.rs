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
    s.touch("claude", None, true, two_hours).unwrap();
    assert_eq!(stale(&mut s, two_hours), false);
    // Past the hold window, still waiting: the lane is stale...
    let later = NOW + WAIT_HOLD_MS + 60_000;
    s.touch("claude", None, true, later).unwrap();
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

fn in_session(
    s: &mut Store,
    actor: &str,
    session: &str,
    op: &str,
    args: Value,
    at: i64,
) -> Result<Value, String> {
    s.execute_at(
        &Request::new(op, actor, args).with_session(Some(session.to_owned())),
        at,
    )
    .map_err(|e| e.code)
}

#[test]
fn review_a_session_bound_wait_cannot_renew_its_own_hold() {
    let mut s = Store::memory().unwrap();
    in_session(&mut s, "claude", "claude:s1", "join", json!({}), NOW).unwrap();
    run(&mut s, "codex", "join", json!({}), NOW).unwrap();
    in_session(
        &mut s,
        "claude",
        "claude:s1",
        "lane_take",
        json!({"paths":["docs/"],"purpose":"w"}),
        NOW,
    )
    .unwrap();
    // A wait from s1, refreshed every minute for 2 hours: still held, and a
    // new session is refused (s1 is alive and waiting).
    let mut t = NOW;
    while t < NOW + 2 * 60 * 60_000 {
        t += 60_000;
        s.touch("claude", Some("claude:s1"), true, t).unwrap();
    }
    run(&mut s, "codex", "heartbeat", json!({}), t).unwrap();
    assert_eq!(
        run(&mut s, "codex", "lanes", json!({}), t).unwrap()["lanes"][0]["stale"],
        false
    );
    assert_eq!(
        in_session(&mut s, "claude", "claude:s2", "join", json!({}), t).unwrap_err(),
        "identity_busy"
    );
    // Keep refreshing past the hold window: it does not renew itself.
    while t < NOW + WAIT_HOLD_MS + 5 * 60_000 {
        t += 60_000;
        s.touch("claude", Some("claude:s1"), true, t).unwrap();
    }
    run(&mut s, "codex", "heartbeat", json!({}), t).unwrap();
    assert_eq!(
        run(&mut s, "codex", "lanes", json!({}), t).unwrap()["lanes"][0]["stale"],
        true
    );
    in_session(&mut s, "claude", "claude:s2", "join", json!({}), t).unwrap();
}

#[test]
fn review_inner_takes_notify_once_and_busy_says_to_release() {
    let mut s = board();
    take(&mut s, "claude", &["x/child"], false, NOW).unwrap();
    take(&mut s, "codex", &["x"], true, NOW).unwrap();
    for (i, p) in ["x/a", "x/b", "x/c"].iter().enumerate() {
        take(&mut s, "claude", &[p], false, NOW + 1 + i as i64).unwrap();
    }
    let inbox = run(&mut s, "codex", "inbox", json!({}), NOW + 10).unwrap();
    let text = serde_json::to_string(&inbox).unwrap();
    assert_eq!(
        text.matches("Work continues inside your queued lane")
            .count(),
        1,
        "{text}"
    );
    let late = NOW + LANE_PATIENCE_MS + 1;
    for who in ["claude", "codex"] {
        run(&mut s, who, "heartbeat", json!({}), late).unwrap();
    }
    let err = s
        .execute_at(
            &Request::new(
                "lane_take",
                "claude",
                json!({"paths":["x/d"],"purpose":"w"}),
            ),
            late,
        )
        .unwrap_err();
    assert_eq!(err.code, "lane_busy");
    assert!(err.message.contains("release it"), "{}", err.message);
}
