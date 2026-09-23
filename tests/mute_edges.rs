//! #19: mute edge cases found in review of b749c2a (probes p7, p3c, p3d).
use fray::{model::Request, store::Store};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000_000;

fn call(s: &mut Store, actor: &str, op: &str, args: Value) -> Value {
    s.execute_at(&Request::new(op, actor, args), NOW).unwrap()
}

fn board() -> Store {
    let mut s = Store::memory().unwrap();
    for actor in ["author", "reviewer", "other"] {
        call(&mut s, actor, "join", json!({"topics": []}));
    }
    s
}

fn total(s: &mut Store, who: &str) -> Value {
    call(s, who, "inbox", json!({"selection": "all"}))["total"].clone()
}

#[test]
fn fu_a_a_mute_placed_before_assignment_never_hides_the_final_outcome() {
    let mut s = board();
    let note = call(
        &mut s,
        "author",
        "send",
        json!({"to": "reviewer", "body": "thread"}),
    );
    let id = note["card"]["id"].clone();
    call(&mut s, "other", "mute", json!({"id": id}));
    call(
        &mut s,
        "author",
        "patch",
        json!({"id": id, "expect": 1, "kind": "question", "assignee": "other"}),
    );
    assert_eq!(total(&mut s, "other"), 1);
    let withdrawn = call(
        &mut s,
        "author",
        "patch",
        json!({"id": id, "expect": 2, "status": "withdrawn"}),
    );
    // The withdrawal is the final outcome of a request addressed to "other":
    // it must be delivered, not hidden by the earlier mute.
    let page = call(&mut s, "other", "inbox", json!({"selection": "all"}));
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["through_seq"], withdrawn["event_seq"]);
    call(&mut s, "other", "leave", json!({}));
    assert_eq!(
        call(&mut s, "other", "join", json!({}))["attention"]["total"],
        1
    );
}

#[test]
fn fu_b_unmute_never_invents_a_receipt_for_a_card_never_routed() {
    // p3c: a topic-matching agent joins after the card was resolved.
    let mut s = board();
    let card = call(
        &mut s,
        "author",
        "post",
        json!({"kind": "note", "title": "T", "summary": "s", "topic": "parser"}),
    );
    let id = card["card"]["id"].clone();
    call(
        &mut s,
        "author",
        "patch",
        json!({"id": id, "expect": 1, "status": "resolved"}),
    );
    call(&mut s, "late", "join", json!({"topics": ["parser"]}));
    assert_eq!(total(&mut s, "late"), 0);
    call(&mut s, "late", "mute", json!({"id": id}));
    call(&mut s, "late", "unmute", json!({"id": id}));
    assert_eq!(total(&mut s, "late"), 0);
    // p3d: a note patched into an unassigned question was never fanned out.
    let card = call(
        &mut s,
        "author",
        "post",
        json!({"kind": "note", "title": "T", "summary": "s", "topic": "x"}),
    );
    let id = card["card"]["id"].clone();
    call(
        &mut s,
        "author",
        "patch",
        json!({"id": id, "expect": 1, "kind": "question"}),
    );
    // Active and relevant (an unassigned question), so a rejoin would seed it;
    // unmute agrees with rejoin rather than inventing or withholding.
    call(&mut s, "other", "mute", json!({"id": id}));
    call(&mut s, "other", "unmute", json!({"id": id}));
    let after_unmute = total(&mut s, "other");
    call(&mut s, "other", "leave", json!({}));
    call(&mut s, "other", "join", json!({"topics": []}));
    assert_eq!(total(&mut s, "other"), after_unmute);
}

#[test]
fn review_fu1_a_request_assigned_to_you_cannot_be_muted_even_when_closed() {
    let mut s = board();
    let asked = call(
        &mut s,
        "author",
        "send",
        json!({"to": "reviewer", "body": "Question?", "ask": true}),
    );
    let id = asked["card"]["id"].clone();
    call(
        &mut s,
        "author",
        "patch",
        json!({"id": id, "expect": asked["card"]["rev"], "status": "withdrawn"}),
    );
    let err = s
        .execute_at(&Request::new("mute", "reviewer", json!({"id": id})), NOW)
        .unwrap_err();
    assert_eq!(err.code, "cannot_mute_assigned_request");
}

#[test]
fn review_fu2_unmute_agrees_with_what_a_join_would_seed() {
    let mut s = board();
    let card = call(
        &mut s,
        "author",
        "post",
        json!({"kind": "note", "title": "T", "summary": "s", "topic": "parser"}),
    );
    let id = card["card"]["id"].clone();
    call(&mut s, "late", "join", json!({"topics": []}));
    call(&mut s, "late", "mute", json!({"id": id}));
    call(&mut s, "late", "join", json!({"topics": ["parser"]}));
    assert_eq!(total(&mut s, "late"), 0);
    call(&mut s, "late", "unmute", json!({"id": id}));
    assert_eq!(total(&mut s, "late"), 1);
}

#[test]
fn unmute_still_restores_updates_missed_on_a_routed_card() {
    // The legitimate catch-up must survive the FU-B narrowing.
    let mut s = board();
    let asked = call(
        &mut s,
        "author",
        "send",
        json!({"to": "reviewer", "body": "first"}),
    );
    let id = asked["card"]["id"].clone();
    let page = call(&mut s, "reviewer", "inbox", json!({}));
    call(
        &mut s,
        "reviewer",
        "ack",
        json!({"receipts": [page["items"][0]["receipt"].clone()]}),
    );
    call(&mut s, "reviewer", "mute", json!({"id": id}));
    let missed = call(
        &mut s,
        "author",
        "annotate",
        json!({"id": id, "kind": "note", "body": "while muted"}),
    );
    assert_eq!(total(&mut s, "reviewer"), 0);
    call(&mut s, "reviewer", "unmute", json!({"id": id}));
    let page = call(&mut s, "reviewer", "inbox", json!({}));
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["through_seq"], missed["event_seq"]);
}
