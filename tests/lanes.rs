//! Phase 2: declared lanes, presence status and preflight.
use fray::{
    model::{random_key, Request},
    store::{paths_overlap, Store, IDENTITY_TTL_MS},
};
use serde_json::{json, Value};
use std::{fs, path::Path, process::Command};

const NOW: i64 = 1_800_000_000_000;

fn run(s: &mut Store, actor: &str, op: &str, args: Value, at: i64) -> Result<Value, String> {
    s.execute_at(&Request::new(op, actor, args), at)
        .map_err(|e| e.code)
}

fn board() -> Store {
    let mut s = Store::memory().unwrap();
    for who in ["claude", "codex", "deepseek"] {
        run(&mut s, who, "join", json!({}), NOW).unwrap();
    }
    s
}

fn take(s: &mut Store, who: &str, paths: &[&str], queue: bool) -> Result<Value, String> {
    run(
        s,
        who,
        "lane_take",
        json!({"paths": paths, "purpose": "work", "queue": queue}),
        NOW,
    )
}

#[test]
fn overlap_semantics() {
    assert!(paths_overlap("src/store.rs", "src/store.rs"));
    assert!(!paths_overlap("src/store.rs", "src/main.rs"));
    assert!(paths_overlap("src/", "src/store.rs"));
    assert!(paths_overlap("src/**", "src/a/b.rs"));
    assert!(paths_overlap("src/", "src/a/"));
    assert!(!paths_overlap("src/", "srcx/a.rs"));
    assert!(!paths_overlap("docs/", "src/store.rs"));
}

#[test]
fn a_held_lane_refuses_overlap_and_queueing_orders_notification() {
    let mut s = board();
    let held = take(&mut s, "claude", &["src/store.rs", "tests/"], false).unwrap();
    assert_eq!(held["lane"]["state"], "held");
    // Overlap is refused with the holder named; disjoint paths are fine.
    assert_eq!(
        take(&mut s, "codex", &["src/store.rs"], false).unwrap_err(),
        "lane_busy"
    );
    assert_eq!(
        take(&mut s, "codex", &["tests/new.rs"], false).unwrap_err(),
        "lane_busy"
    );
    assert_eq!(
        take(&mut s, "codex", &["src/server.rs"], false).unwrap()["lane"]["state"],
        "held"
    );
    // Queue behind it; release promotes and tells the queued agent.
    let queued = take(&mut s, "deepseek", &["src/store.rs"], true).unwrap();
    assert_eq!(queued["lane"]["state"], "queued");
    let released = run(
        &mut s,
        "claude",
        "lane_release",
        json!({"id": held["lane"]["id"]}),
        NOW,
    )
    .unwrap();
    assert_eq!(released["promoted"], json!([queued["lane"]["id"]]));
    let inbox = run(&mut s, "deepseek", "inbox", json!({}), NOW).unwrap();
    assert!(inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["card"]["title"] == "Your queued lane is free"));
    let lanes = run(&mut s, "claude", "lanes", json!({}), NOW).unwrap()["lanes"].clone();
    assert!(lanes
        .as_array()
        .unwrap()
        .iter()
        .all(|l| l["state"] == "held"));
}

#[test]
fn handoff_and_stale_release() {
    let mut s = board();
    let held = take(&mut s, "claude", &["docs/"], false).unwrap();
    // Only the holder releases while present.
    assert_eq!(
        run(
            &mut s,
            "codex",
            "lane_release",
            json!({"id": held["lane"]["id"]}),
            NOW
        )
        .unwrap_err(),
        "lane_not_yours"
    );
    // Handoff moves the lane and notifies the receiver.
    let handed = run(
        &mut s,
        "claude",
        "lane_release",
        json!({"id": held["lane"]["id"], "to": "codex"}),
        NOW,
    )
    .unwrap();
    assert_eq!(handed["reason"], "handed to codex");
    let roster = run(&mut s, "deepseek", "agents", json!({}), NOW).unwrap();
    let codex = roster["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "codex")
        .unwrap()
        .clone();
    assert_eq!(codex["lanes"][0]["paths"], json!(["docs/"]));
    // Once the holder is gone, anyone may release its lane, visibly.
    let later = NOW + IDENTITY_TTL_MS + 1;
    run(&mut s, "deepseek", "heartbeat", json!({}), later).unwrap();
    let lanes = run(&mut s, "deepseek", "lanes", json!({}), later).unwrap()["lanes"].clone();
    assert_eq!(lanes[0]["stale"], true);
    let freed = run(
        &mut s,
        "deepseek",
        "lane_release",
        json!({"id": lanes[0]["id"]}),
        later,
    )
    .unwrap();
    assert!(freed["reason"].as_str().unwrap().starts_with("stale"));
}

#[test]
fn status_is_one_line_per_agent_updated_in_place() {
    let mut s = board();
    run(
        &mut s,
        "claude",
        "set_status",
        json!({"text": "reviewing #58"}),
        NOW,
    )
    .unwrap();
    run(
        &mut s,
        "claude",
        "set_status",
        json!({"text": "free for review"}),
        NOW,
    )
    .unwrap();
    let roster = run(&mut s, "codex", "agents", json!({}), NOW).unwrap();
    let claude = roster["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "claude")
        .unwrap()
        .clone();
    assert_eq!(claude["status"]["text"], "free for review");
    run(&mut s, "claude", "set_status", json!({"text": ""}), NOW).unwrap();
    let roster = run(&mut s, "codex", "agents", json!({}), NOW).unwrap();
    assert!(roster["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "claude")
        .unwrap()["status"]
        .is_null());
}

#[test]
fn bad_lane_paths_are_refused() {
    let mut s = board();
    for bad in ["/etc/passwd", "../outside", "a/../b", "has space", ""] {
        assert_eq!(
            take(&mut s, "claude", &[bad], false).unwrap_err(),
            "invalid",
            "{bad:?}"
        );
    }
}

fn git(dir: &Path, args: &[&str]) {
    let ok = Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(
        ok.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
}

#[test]
fn preflight_sees_declared_lanes_and_real_edits_in_other_worktrees() {
    let root = Path::new("/tmp").join(format!("fray-pf-{}", &random_key().unwrap()[..8]));
    let repo = root.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(
        &repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
    fs::write(repo.join("store.rs"), "one\n").unwrap();
    fs::write(repo.join("main.rs"), "one\n").unwrap();
    git(&repo, &["add", "."]);
    git(
        &repo,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-q",
            "-m",
            "files",
        ],
    );
    let peer = root.join("peer");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            peer.to_str().unwrap(),
            "-b",
            "peer",
        ],
    );
    // The peer edits store.rs without declaring anything.
    fs::write(peer.join("store.rs"), "two\n").unwrap();
    let home = root.join("h");
    let fray = |actor: &str, args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_fray"))
            .current_dir(&repo)
            .args(["--home", home.to_str().unwrap(), "--as", actor, "--json"])
            .args(args)
            .output()
            .unwrap();
        serde_json::from_slice::<Value>(&out.stdout).unwrap_or(Value::Null)
    };
    fray("me", &["start"]);
    fray("me", &["join"]);
    fray("peer", &["join"]);
    fray(
        "peer",
        &["lane", "take", "main.rs", "--purpose", "refactor"],
    );
    let report = fray("me", &["preflight", "store.rs", "main.rs"]);
    fray("me", &["stop"]);
    let _ = fs::remove_dir_all(&root);
    assert_eq!(report["clear"], false, "{report}");
    assert_eq!(report["declared"][0]["agent"], "peer", "{report}");
    assert_eq!(report["declared"][0]["paths"], json!(["main.rs"]));
    assert_eq!(
        report["observed"][0]["paths"],
        json!(["store.rs"]),
        "{report}"
    );
    assert_eq!(report["observed"][0]["branch"], "peer");
}
