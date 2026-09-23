//! Owner-authority channel, Tiers 1 and 3 (docs/design/owner-authority.md).
use fray::{
    model::Request,
    store::{Store, OWNER},
};
use serde_json::{json, Value};
use std::process::{Command, Stdio};

const NOW: i64 = 1_800_000_000_000;

fn run(s: &mut Store, actor: &str, op: &str, args: Value) -> Result<Value, String> {
    s.execute_at(&Request::new(op, actor, args), NOW)
        .map_err(|e| e.code)
}

fn board() -> Store {
    let mut s = Store::memory().unwrap();
    for who in ["claude", "codex"] {
        run(&mut s, who, "join", json!({})).unwrap();
    }
    s
}

#[test]
fn agents_cannot_act_as_the_owner_and_the_owner_only_uses_owner_ops() {
    let mut s = board();
    // Ordinary operations under the owner name are refused, including join.
    for (op, args) in [
        ("join", json!({})),
        ("post", json!({"title": "t", "summary": "s"})),
        ("send", json!({"to": "claude", "body": "do X"})),
    ] {
        assert_eq!(
            run(&mut s, OWNER, op, args).unwrap_err(),
            "reserved_owner",
            "{op}"
        );
    }
    // Owner operations are refused for anyone else.
    assert_eq!(
        run(
            &mut s,
            "claude",
            "owner_decide",
            json!({"title": "t", "summary": "s"})
        )
        .unwrap_err(),
        "reserved_owner"
    );
    assert_eq!(
        run(
            &mut s,
            "claude",
            "owner_answer",
            json!({"id": 1, "verdict": "approve", "body": ""})
        )
        .unwrap_err(),
        "reserved_owner"
    );
}

#[test]
fn an_owner_decision_is_pinned_for_everyone_and_marked_with_authority() {
    let mut s = board();
    let decided = run(
        &mut s,
        OWNER,
        "owner_decide",
        json!({"title": "Charter", "summary": "Agents act within docs/CHARTER.md."}),
    )
    .unwrap();
    assert_eq!(decided["card"]["author"], OWNER);
    assert_eq!(decided["card"]["pinned"], true);
    assert_eq!(decided["card"]["kind"], "decision");
    for who in ["claude", "codex"] {
        let page = run(&mut s, who, "inbox", json!({})).unwrap();
        let card = &page["items"][0]["card"];
        assert_eq!(card["id"], decided["card"]["id"], "{who}");
        assert_eq!(card["authority"], "owner (unsigned)");
    }
    // An agent's own decision card carries no authority marker.
    let own = run(
        &mut s,
        "claude",
        "post",
        json!({"kind": "decision", "title": "Mine", "summary": "s", "topic": "*"}),
    )
    .unwrap();
    let page = run(&mut s, "codex", "inbox", json!({})).unwrap();
    let mine = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["card"]["id"] == own["card"]["id"])
        .unwrap();
    assert!(mine["card"].get("authority").is_none());
}

#[test]
fn ask_owner_queues_and_the_answer_reaches_the_asker() {
    let mut s = board();
    // What `fray ask-owner` sends.
    let asked = run(
        &mut s,
        "claude",
        "send",
        json!({"to": OWNER, "body": "Restart the shared daemon?", "ask": true, "pending": true, "refs": ["ask-owner"]}),
    )
    .unwrap();
    let id = asked["card"]["id"].clone();
    // The owner's queue.
    let queue = run(
        &mut s,
        OWNER,
        "query",
        json!({"assignee": OWNER, "kind": "question"}),
    )
    .unwrap();
    assert_eq!(queue["items"][0]["id"], id);
    // Clear claude's own view, then the owner approves.
    let answered = run(
        &mut s,
        OWNER,
        "owner_answer",
        json!({"id": id, "verdict": "approve", "body": "Go ahead after announcing it."}),
    )
    .unwrap();
    assert_eq!(answered["verdict"], "approve");
    assert_eq!(answered["card"]["status"], "resolved");
    let page = run(&mut s, "claude", "inbox", json!({})).unwrap();
    let item = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["card"]["id"] == id)
        .unwrap()
        .clone();
    let text = item["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .find_map(|a| a["excerpt"].as_str().filter(|t| t.starts_with("APPROVED")))
        .unwrap()
        .to_owned();
    assert!(text.contains("Go ahead after announcing it."), "{text}");
    // Answered requests leave the queue.
    let queue = run(
        &mut s,
        OWNER,
        "query",
        json!({"assignee": OWNER, "kind": "question"}),
    )
    .unwrap();
    assert_eq!(queue["items"].as_array().unwrap().len(), 0);
}

#[test]
fn owner_commands_refuse_a_non_interactive_shell() {
    // Agent tool shells are not terminals, so `fray owner` cannot be driven
    // through ordinary tool calls. No daemon is needed for this refusal.
    let out = Command::new(env!("CARGO_BIN_EXE_fray"))
        .args([
            "--home",
            "/nonexistent-fray-home",
            "owner",
            "decide",
            "t",
            "--summary",
            "s",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("interactive terminal"), "{stderr}");
}
