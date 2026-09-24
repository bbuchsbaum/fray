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
    // Review of 58daa7a: spellings that must overlap.
    assert!(paths_overlap("./src/x", "src/x"));
    assert!(paths_overlap("src", "src/x"));
    assert!(paths_overlap("src//x", "src/x"));
    assert!(paths_overlap("src/*.rs", "src/store.rs"));
    assert!(paths_overlap("Src/X", "src/x"));
    assert!(paths_overlap("docs/", "docs/my file.md"));
    // Unparseable input is never reported clear.
    assert!(paths_overlap("../outside", "src/x"));
    // Review of 64f05e6: Unicode names compare normally; they do not
    // overlap everything.
    assert!(!paths_overlap("src/\u{e9} q.rs", "docs/x"));
    assert!(paths_overlap("docs/", "docs/caf\u{e9}.md"));
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
    for bad in [
        "/etc/passwd",
        "../outside",
        "a/../b",
        "tab\there",
        "",
        " ",
        "src/ /x",
    ] {
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
    // Review: paths typed relative to a subdirectory, or absolute, resolve to
    // the repository; quoted names and both sides of a rename are observed.
    fs::create_dir_all(repo.join("src")).unwrap();
    fs::write(peer.join("my file.md"), "x\n").unwrap();
    git(&peer, &["mv", "main.rs", "renamed.rs"]);
    let sub = |args: &[&str]| {
        let out = Command::new(env!("CARGO_BIN_EXE_fray"))
            .current_dir(repo.join("src"))
            .args(["--home", home.to_str().unwrap(), "--as", "me", "--json"])
            .args(args)
            .output()
            .unwrap();
        serde_json::from_slice::<Value>(&out.stdout).unwrap_or(Value::Null)
    };
    let from_sub = sub(&["preflight", "../store.rs"]);
    let absolute = sub(&["preflight", repo.join("my file.md").to_str().unwrap()]);
    let renamed_from = sub(&["preflight", "../main.rs"]);
    let outside = sub(&["preflight", "/etc/hosts"]);
    // Review of 64f05e6: staged names are read NUL-separated, so a Unicode
    // name is not quoted into a false clear.
    fray("peer", &["lane", "take", "docs/", "--purpose", "docs"]);
    fs::create_dir_all(repo.join("docs")).unwrap();
    fs::write(repo.join("docs/caf\u{e9}.md"), "x\n").unwrap();
    git(&repo, &["add", "docs"]);
    let staged = fray("me", &["preflight", "--staged"]);
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
    assert_eq!(from_sub["clear"], false, "{from_sub}");
    assert_eq!(from_sub["paths"], json!(["store.rs"]));
    assert_eq!(absolute["clear"], false, "{absolute}");
    assert_eq!(absolute["observed"][0]["paths"], json!(["my file.md"]));
    assert!(
        renamed_from["observed"][0]["paths"]
            .as_array()
            .unwrap()
            .contains(&json!("main.rs")),
        "{renamed_from}"
    );
    assert_eq!(outside["error"]["code"], "invalid", "{outside}");
    assert_eq!(staged["clear"], false, "{staged}");
    assert_eq!(staged["paths"], json!(["docs/caf\u{e9}.md"]), "{staged}");
}

#[test]
fn review_a_handed_over_queued_lane_stays_queued_and_queues_are_fifo() {
    let mut s = board();
    run(&mut s, "extra", "join", json!({}), NOW).unwrap();
    let held = take(&mut s, "claude", &["src/x"], false).unwrap();
    let queued = take(&mut s, "codex", &["src/x"], true).unwrap();
    // BLOCK: handing over a queued lane must not make it held.
    let handed = run(
        &mut s,
        "codex",
        "lane_release",
        json!({"id": queued["lane"]["id"], "to": "deepseek"}),
        NOW,
    )
    .unwrap();
    let lanes = run(&mut s, "claude", "lanes", json!({}), NOW).unwrap()["lanes"].clone();
    let moved = lanes
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == handed["handed_to_lane"])
        .unwrap()
        .clone();
    assert_eq!(moved["state"], "queued");
    // FIFO: a later take overlapping the queued lane waits behind it.
    let later = take(&mut s, "extra", &["src/"], true).unwrap();
    assert_eq!(later["lane"]["state"], "queued");
    run(
        &mut s,
        "claude",
        "lane_release",
        json!({"id": held["lane"]["id"]}),
        NOW,
    )
    .unwrap();
    let lanes = run(&mut s, "claude", "lanes", json!({}), NOW).unwrap()["lanes"].clone();
    let state = |id: &Value| {
        lanes
            .as_array()
            .unwrap()
            .iter()
            .find(|l| &l["id"] == id)
            .unwrap()["state"]
            .clone()
    };
    assert_eq!(state(&handed["handed_to_lane"]), "held");
    assert_eq!(state(&later["lane"]["id"]), "queued");
}

#[test]
fn review_handover_needs_a_joined_recipient_and_stale_holders_are_told() {
    let mut s = board();
    run(
        &mut s,
        "claude",
        "send",
        json!({"to": "ghost", "body": "hi", "pending": true}),
        NOW,
    )
    .unwrap();
    let held = take(&mut s, "claude", &["docs/"], false).unwrap();
    assert_eq!(
        run(
            &mut s,
            "claude",
            "lane_release",
            json!({"id": held["lane"]["id"], "to": "ghost"}),
            NOW
        )
        .unwrap_err(),
        "unknown_agent"
    );
    let later = NOW + IDENTITY_TTL_MS + 1;
    run(&mut s, "codex", "heartbeat", json!({}), later).unwrap();
    run(
        &mut s,
        "codex",
        "lane_release",
        json!({"id": held["lane"]["id"]}),
        later,
    )
    .unwrap();
    let inbox = run(&mut s, "claude", "inbox", json!({}), later).unwrap();
    assert!(inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["card"]["title"] == "Your stale lane was released"));
}

#[test]
fn review_reading_keeps_an_agent_present() {
    let mut s = board();
    let held = take(&mut s, "claude", &["src/"], false).unwrap();
    // Claude only reads (inbox + the CLI's presentation) for longer than the
    // identity TTL: it must not become stale.
    run(
        &mut s,
        "codex",
        "send",
        json!({"to": "claude", "body": "ping"}),
        NOW,
    )
    .unwrap();
    let mut at = NOW;
    for _ in 0..6 {
        at += IDENTITY_TTL_MS / 4;
        let page = run(&mut s, "claude", "inbox", json!({}), at).unwrap();
        let receipts: Vec<Value> = page["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["receipt"].clone())
            .collect();
        if !receipts.is_empty() {
            run(
                &mut s,
                "claude",
                "present",
                json!({"source": "inbox", "receipts": receipts}),
                at,
            )
            .unwrap();
        }
    }
    let lanes = run(&mut s, "codex", "lanes", json!({}), at).unwrap()["lanes"].clone();
    let lane = lanes
        .as_array()
        .unwrap()
        .iter()
        .find(|l| l["id"] == held["lane"]["id"])
        .unwrap()
        .clone();
    assert_eq!(lane["stale"], false);
}

#[test]
fn review_queued_lanes_do_not_deadlock_the_holder_they_wait_on() {
    let mut s = board();
    take(&mut s, "claude", &["src/x"], false).unwrap();
    // codex waits on claude's src/x.
    let queued = take(&mut s, "codex", &["src/"], true).unwrap();
    assert_eq!(queued["lane"]["state"], "queued");
    // claude can still take more under src/: codex is waiting for claude.
    let more = take(&mut s, "claude", &["src/y"], false).unwrap();
    assert_eq!(more["lane"]["state"], "held");
    // A third agent still waits behind codex's queued lane.
    assert_eq!(
        take(&mut s, "deepseek", &["src/z"], false).unwrap_err(),
        "lane_busy"
    );
}

#[test]
fn review_handover_keeps_queue_position_and_stale_takeover_cannot_jump() {
    let mut s = board();
    run(&mut s, "extra", "join", json!({}), NOW).unwrap();
    let held = take(&mut s, "claude", &["src/x"], false).unwrap();
    let first = take(&mut s, "codex", &["src/x"], true).unwrap();
    let second = take(&mut s, "extra", &["src/x"], true).unwrap();
    // codex hands its queued lane to deepseek: same lane, same turn.
    let handed = run(
        &mut s,
        "codex",
        "lane_release",
        json!({"id": first["lane"]["id"], "to": "deepseek"}),
        NOW,
    )
    .unwrap();
    assert_eq!(handed["handed_to_lane"], first["lane"]["id"]);
    // Once claude is stale, a third party may release but not hand over.
    let later = NOW + IDENTITY_TTL_MS + 1;
    for who in ["deepseek", "extra"] {
        run(&mut s, who, "heartbeat", json!({}), later).unwrap();
    }
    assert_eq!(
        run(
            &mut s,
            "extra",
            "lane_release",
            json!({"id": held["lane"]["id"], "to": "extra"}),
            later
        )
        .unwrap_err(),
        "lane_not_yours"
    );
    let freed = run(
        &mut s,
        "extra",
        "lane_release",
        json!({"id": held["lane"]["id"]}),
        later,
    )
    .unwrap();
    assert_eq!(freed["promoted"], json!([first["lane"]["id"]]));
    let lanes = run(&mut s, "extra", "lanes", json!({}), later).unwrap()["lanes"].clone();
    let state = |id: &Value| {
        lanes
            .as_array()
            .unwrap()
            .iter()
            .find(|l| &l["id"] == id)
            .unwrap()["state"]
            .clone()
    };
    assert_eq!(state(&first["lane"]["id"]), "held");
    assert_eq!(state(&second["lane"]["id"]), "queued");
}

#[test]
fn review_a_whole_repository_lane_is_warned() {
    let mut s = board();
    let whole = take(&mut s, "claude", &["."], false).unwrap();
    assert!(whole["warning"].is_string(), "{whole}");
    let narrow = take(&mut s, "codex", &["docs/"], true).unwrap();
    assert!(narrow["warning"].is_null());
}

#[test]
fn review3_handover_never_creates_overlapping_holds() {
    let mut s = board();
    take(&mut s, "claude", &["y/a"], false).unwrap();
    let wide = take(&mut s, "claude", &["y"], false).unwrap();
    assert_eq!(
        run(
            &mut s,
            "claude",
            "lane_release",
            json!({"id": wide["lane"]["id"], "to": "codex"}),
            NOW
        )
        .unwrap_err(),
        "lane_busy"
    );
    // Handing over to yourself is a mistake, not a release.
    assert_eq!(
        run(
            &mut s,
            "claude",
            "lane_release",
            json!({"id": wide["lane"]["id"], "to": "claude"}),
            NOW
        )
        .unwrap_err(),
        "invalid"
    );
    // Nothing changed: claude still holds both.
    let lanes = run(&mut s, "codex", "lanes", json!({}), NOW).unwrap()["lanes"].clone();
    assert!(lanes
        .as_array()
        .unwrap()
        .iter()
        .all(|l| l["agent"] == "claude" && l["state"] == "held"));
}

#[test]
fn review3_a_holder_cannot_retake_what_a_queuer_waits_for() {
    let mut s = board();
    take(&mut s, "claude", &["x/child"], false).unwrap();
    let whole = take(&mut s, "claude", &["x/other"], false).unwrap();
    assert_eq!(
        take(&mut s, "codex", &["x"], true).unwrap()["lane"]["state"],
        "queued"
    );
    run(
        &mut s,
        "claude",
        "lane_release",
        json!({"id": whole["lane"]["id"]}),
        NOW,
    )
    .unwrap();
    // Work strictly inside is fine; retaking the whole of x is not.
    assert_eq!(
        take(&mut s, "claude", &["x/more"], false).unwrap()["lane"]["state"],
        "held"
    );
    assert_eq!(
        take(&mut s, "claude", &["x"], false).unwrap_err(),
        "lane_busy"
    );
    assert_eq!(
        take(&mut s, "claude", &["."], false).unwrap_err(),
        "lane_busy"
    );
}

#[test]
fn review3_unicode_names_compare_conservatively() {
    // NFC and NFD spellings of the same directory overlap.
    assert!(paths_overlap("caf\u{e9}/", "cafe\u{301}/x.md"));
    assert!(paths_overlap("docs/caf\u{e9}.md", "docs/cafe\u{301}.md"));
    // An ASCII sibling is still distinct.
    assert!(!paths_overlap("docs/caf\u{e9}.md", "src/x.rs"));
    let mut s = board();
    for bad in ["src/a\u{200b}b", "src/\u{202e}x", "src/x\u{fe0f}"] {
        assert_eq!(
            take(&mut s, "claude", &[bad], false).unwrap_err(),
            "invalid",
            "{bad:?}"
        );
    }
}
