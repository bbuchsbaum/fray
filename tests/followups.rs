use fray::{model::Request, store::Store};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000_000;

fn call(store: &mut Store, actor: &str, op: &str, args: Value) -> Value {
    store
        .execute_at(&Request::new(op, actor, args), NOW)
        .unwrap()
}

fn pair() -> Store {
    let mut store = Store::memory().unwrap();
    for actor in ["codex", "claude", "deepseek"] {
        call(&mut store, actor, "join", json!({"topics": []}));
    }
    store
}

fn raise(store: &mut Store, actor: &str, id: &Value, kind: &str, body: &str) -> Value {
    call(
        store,
        actor,
        "annotate",
        json!({"id": id, "kind": kind, "body": body}),
    )["follow_up"]
        .clone()
}

#[test]
fn assignee_objecting_to_a_request_reaches_its_author() {
    // codex asks claude for a review; claude's objection must go back to codex.
    let mut s = pair();
    let asked = call(
        &mut s,
        "codex",
        "send",
        json!({"to": "claude", "body": "Please review the stream", "ask": true}),
    );
    let id = asked["card"]["id"].clone();
    assert_eq!(asked["card"]["assignee"], "claude");
    let objection = raise(
        &mut s,
        "claude",
        &id,
        "objection",
        "Packet budget kills the stream",
    );
    assert_eq!(objection["assignee"], "codex");
    let question = raise(
        &mut s,
        "claude",
        &id,
        "question",
        "Is there a harness workaround?",
    );
    assert_eq!(question["assignee"], "codex");
}

#[test]
fn author_questioning_an_unassigned_card_does_not_assign_itself() {
    // Nobody else is responsible, so the author keeps it rather than guessing.
    let mut s = pair();
    let posted = call(
        &mut s,
        "claude",
        "post",
        json!({"title": "Lanes", "summary": "Who edits what", "kind": "question", "topic": "*"}),
    );
    let id = posted["card"]["id"].clone();
    let follow_up = raise(&mut s, "claude", &id, "question", "Can I take the block?");
    assert_eq!(follow_up["assignee"], "claude");
}

#[test]
fn author_questioning_an_assigned_card_reaches_the_assignee() {
    let mut s = pair();
    let posted = call(
        &mut s,
        "codex",
        "post",
        json!({"title": "Fix packets", "summary": "Budget band", "kind": "task", "assignee": "claude"}),
    );
    let id = posted["card"]["id"].clone();
    let follow_up = raise(&mut s, "codex", &id, "question", "ETA?");
    assert_eq!(follow_up["assignee"], "claude");
}

#[test]
fn third_party_objection_prefers_live_lease_owner() {
    let mut s = pair();
    let posted = call(
        &mut s,
        "codex",
        "post",
        json!({"title": "Fix packets", "summary": "Budget band", "kind": "task", "assignee": "claude"}),
    );
    let id = posted["card"]["id"].clone();
    call(&mut s, "claude", "claim", json!({"id": id}));
    let follow_up = raise(&mut s, "deepseek", &id, "objection", "Races with reconnect");
    assert_eq!(follow_up["assignee"], "claude");
}

#[test]
fn lease_owner_objecting_skips_itself() {
    let mut s = pair();
    let posted = call(
        &mut s,
        "codex",
        "post",
        json!({"title": "Fix packets", "summary": "Budget band", "kind": "task", "assignee": "claude"}),
    );
    let id = posted["card"]["id"].clone();
    call(&mut s, "claude", "claim", json!({"id": id}));
    let follow_up = raise(&mut s, "claude", &id, "objection", "Spec is contradictory");
    assert_eq!(follow_up["assignee"], "codex");
}

#[test]
fn follow_up_title_names_the_concern_not_the_parent() {
    let mut s = pair();
    let asked = call(
        &mut s,
        "codex",
        "send",
        json!({"to": "claude", "body": "Joined per user. I own the attention stream", "ask": true}),
    );
    let id = asked["card"]["id"].clone();
    let first = raise(
        &mut s,
        "claude",
        &id,
        "objection",
        "\n  Packet near budget kills the stream\nDetails follow on later lines.",
    );
    assert_eq!(
        first["title"],
        format!("Objection on #{}: Packet near budget kills the stream", id)
    );
    // A question on a question no longer nests "Question on #N:" prefixes.
    let nested = raise(&mut s, "codex", &first["id"], "question", "Which budget?");
    assert_eq!(
        nested["title"],
        format!("Question on #{}: Which budget?", first["id"])
    );
    let long = raise(&mut s, "claude", &id, "question", &"x".repeat(200));
    let title = long["title"].as_str().unwrap();
    assert!(
        title.ends_with('…') && title.chars().count() < 90,
        "{title}"
    );
}

#[test]
fn kind_filter_ignores_the_readers_own_annotations() {
    // bob's own evidence must not make the card match bob's evidence filter
    // once someone else's unrelated note makes the card pending again.
    let mut s = pair();
    let asked = call(
        &mut s,
        "codex",
        "send",
        json!({"to": "claude", "body": "Please check the stream"}),
    );
    let id = asked["card"]["id"].clone();
    let pending = call(&mut s, "claude", "inbox", json!({}));
    call(
        &mut s,
        "claude",
        "ack",
        json!({"receipts": [pending["items"][0]["receipt"]]}),
    );
    call(
        &mut s,
        "claude",
        "annotate",
        json!({"id": id, "kind": "evidence", "body": "Checked at 78d7a2e1"}),
    );
    call(
        &mut s,
        "codex",
        "annotate",
        json!({"id": id, "kind": "note", "body": "Thanks"}),
    );
    assert_eq!(call(&mut s, "claude", "inbox", json!({}))["total"], 1);
    assert_eq!(
        call(&mut s, "claude", "inbox", json!({"kinds": ["evidence"]}))["total"],
        0
    );
    // Evidence from someone else still matches.
    call(
        &mut s,
        "codex",
        "annotate",
        json!({"id": id, "kind": "evidence", "body": "Reproduced too"}),
    );
    assert_eq!(
        call(&mut s, "claude", "inbox", json!({"kinds": ["evidence"]}))["total"],
        1
    );
}
