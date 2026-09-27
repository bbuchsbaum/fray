use fray::{model::Request, store::Store};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000_000;

fn run(store: &mut Store, actor: &str, op: &str, args: Value) -> Result<Value, fray::model::Error> {
    store.execute_at(&Request::new(op, actor, args), NOW)
}

fn board() -> Store {
    let mut s = Store::memory().unwrap();
    for who in ["claude", "codex-attention-0923"] {
        run(&mut s, who, "join", json!({})).unwrap();
    }
    s
}

#[test]
fn unknown_names_fail_with_suggestions() {
    let mut s = board();
    // A shortened name (the field case: "codex" for "codex-attention-0923").
    let err = run(
        &mut s,
        "claude",
        "send",
        json!({"to": "codex", "body": "hi"}),
    )
    .unwrap_err();
    assert_eq!(err.code, "unknown_agent");
    assert!(
        err.message.contains("Did you mean codex-attention-0923?"),
        "{}",
        err.message
    );
    assert!(
        err.message.contains("send --pending codex"),
        "{}",
        err.message
    );
    // A typo.
    let err = run(
        &mut s,
        "codex-attention-0923",
        "send",
        json!({"to": "cluade", "body": "hi"}),
    )
    .unwrap_err();
    assert!(
        err.message.contains("Did you mean claude?"),
        "{}",
        err.message
    );
    // Nothing close: no guess.
    let err = run(
        &mut s,
        "claude",
        "send",
        json!({"to": "zzzzzzzz", "body": "hi"}),
    )
    .unwrap_err();
    assert!(!err.message.contains("Did you mean"), "{}", err.message);
    // Nothing was created by the failures.
    let agents = run(&mut s, "claude", "agents", json!({})).unwrap();
    assert_eq!(agents["items"].as_array().unwrap().len(), 2);
}

#[test]
fn pending_mail_waits_for_the_agent_and_arrives_on_join() {
    let mut s = board();
    let sent = run(
        &mut s,
        "claude",
        "send",
        json!({"to": "reviewer-2", "body": "Please review #16 when you join.", "ask": true, "pending": true}),
    )
    .unwrap();
    let id = sent["card"]["id"].clone();
    assert_eq!(sent["card"]["assignee"], "reviewer-2");
    // Registered but not joined: shown as not enabled, and not yet delivered.
    let agents = run(&mut s, "claude", "agents", json!({})).unwrap();
    let pending = agents["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "reviewer-2")
        .unwrap()
        .clone();
    assert_eq!(pending["enabled"], false);
    assert_eq!(pending["pending"], true);
    // A second pending send to the same name reuses it.
    run(
        &mut s,
        "claude",
        "send",
        json!({"to": "reviewer-2", "body": "Also #17.", "pending": true}),
    )
    .unwrap();
    // On join, the mail is waiting.
    let joined = run(&mut s, "reviewer-2", "join", json!({})).unwrap();
    let ids: Vec<Value> = joined["attention"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["card"]["id"].clone())
        .collect();
    assert!(ids.contains(&id), "{ids:?}");
    assert_eq!(
        run(&mut s, "reviewer-2", "inbox", json!({})).unwrap()["total"],
        2
    );
}

#[test]
fn withdrawn_or_rerouted_pending_mail_does_not_leave_a_phantom_in_default_roster() {
    let mut s = board();
    let mut cards = Vec::new();
    for body in ["First", "Second"] {
        cards.push(
            run(
                &mut s,
                "claude",
                "send",
                json!({"to":"typo","body":body,"pending":true}),
            )
            .unwrap()["card"]["id"]
                .clone(),
        );
    }
    let visible = |s: &mut Store, args| {
        run(s, "claude", "agents", args).unwrap()["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["name"] == "typo")
    };
    run(
        &mut s,
        "claude",
        "patch",
        json!({"id":cards[0],"expect":1,"status":"withdrawn"}),
    )
    .unwrap();
    assert!(
        visible(&mut s, json!({})),
        "one open message still needs the recipient"
    );
    run(
        &mut s,
        "claude",
        "patch",
        json!({"id":cards[1],"expect":1,"assignee":"codex-attention-0923"}),
    )
    .unwrap();
    assert!(!visible(&mut s, json!({})));
    assert!(
        visible(&mut s, json!({"all":true})),
        "history is retained, never deleted"
    );
    let brief = run(&mut s, "claude", "brief", json!({})).unwrap();
    assert!(!brief["agents"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["name"] == "typo"));
    // A later genuine join still works and recovers the terminal directed message.
    run(&mut s, "typo", "join", json!({})).unwrap();
    assert!(visible(&mut s, json!({})));
    let inbox = run(&mut s, "typo", "inbox", json!({})).unwrap();
    assert!(inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["card"]["id"] == cards[0]));
}
