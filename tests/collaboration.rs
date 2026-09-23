use fray::{model::Request, store::Store};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000_000;

fn call(store: &mut Store, actor: &str, op: &str, args: Value) -> Value {
    store
        .execute_at(&Request::new(op, actor, args), NOW)
        .unwrap()
}

fn team() -> Store {
    let mut store = Store::memory().unwrap();
    for (actor, topics) in [
        ("codex", json!(["parser"])),
        ("claude", json!(["tests"])),
        ("deepseek", json!(["*"])),
    ] {
        call(&mut store, actor, "join", json!({"topics": topics}));
    }
    // An empty topic list must not defeat the steward's project-wide role.
    for actor in ["manager", "review-manager"] {
        call(
            &mut store,
            actor,
            "join",
            json!({"role": "steward", "topics": []}),
        );
    }
    store
}

fn note(store: &mut Store) -> Value {
    call(
        store,
        "codex",
        "post",
        json!({
            "title": "Parser contract", "summary": "Preserve empty input", "topic": "parser"
        }),
    )
}

#[test]
fn off_topic_contributor_receives_later_replies() {
    let mut store = team();
    note(&mut store);
    assert_eq!(call(&mut store, "claude", "inbox", json!({}))["total"], 0);
    call(
        &mut store,
        "claude",
        "annotate",
        json!({"id": 1, "body": "Does empty input return zero?"}),
    );
    assert_eq!(call(&mut store, "claude", "inbox", json!({}))["total"], 0);
    let answer = call(
        &mut store,
        "codex",
        "annotate",
        json!({"id": 1, "body": "Yes, see the parser test", "kind": "answer"}),
    );
    let inbox = call(&mut store, "claude", "inbox", json!({}));
    assert_eq!(inbox["total"], 1);
    assert_eq!(inbox["items"][0]["through_seq"], answer["event_seq"]);
    assert_eq!(
        inbox["items"][0]["annotations"][0]["excerpt"],
        "Yes, see the parser test"
    );
}

#[test]
fn contributing_does_not_acknowledge_unseen_updates() {
    let mut store = team();
    let first = note(&mut store);
    call(
        &mut store,
        "codex",
        "patch",
        json!({"id": 1, "expect": 1, "summary": "Changed contract"}),
    );
    call(
        &mut store,
        "deepseek",
        "annotate",
        json!({"id": 1, "body": "Independent evidence"}),
    );
    call(
        &mut store,
        "deepseek",
        "ack",
        json!({"id": 1, "through": first["event_seq"]}),
    );
    let inbox = call(&mut store, "deepseek", "inbox", json!({}));
    assert_eq!(inbox["total"], 1);
    assert_eq!(inbox["items"][0]["through_seq"], 2);
}

#[test]
fn rejoin_recovers_an_off_topic_conversation() {
    let mut store = team();
    note(&mut store);
    call(
        &mut store,
        "claude",
        "annotate",
        json!({"id": 1, "body": "Reviewing this"}),
    );
    call(&mut store, "claude", "leave", json!({}));
    let answer = call(
        &mut store,
        "codex",
        "annotate",
        json!({"id": 1, "body": "Ready for your review"}),
    );
    let joined = call(&mut store, "claude", "join", json!({}));
    assert_eq!(joined["attention"]["total"], 1);
    assert_eq!(
        joined["attention"]["items"][0]["through_seq"],
        answer["event_seq"]
    );
}

#[test]
fn departing_participant_retains_the_final_outcome() {
    let mut store = team();
    note(&mut store);
    call(
        &mut store,
        "claude",
        "annotate",
        json!({"id": 1, "body": "Reviewing"}),
    );
    call(&mut store, "claude", "leave", json!({}));
    let closed = call(
        &mut store,
        "codex",
        "patch",
        json!({"id": 1, "expect": 1, "status": "resolved", "summary": "Review accepted"}),
    );
    let joined = call(&mut store, "claude", "join", json!({}));
    assert_eq!(joined["attention"]["total"], 1);
    assert_eq!(
        joined["attention"]["items"][0]["through_seq"],
        closed["event_seq"]
    );
    assert_eq!(
        joined["attention"]["items"][0]["card"]["status"],
        "resolved"
    );
    // A wholly new agent still gets current context, not completed history.
    assert_eq!(
        call(&mut store, "newcomer", "join", json!({}))["attention"]["total"],
        0
    );
}

#[test]
fn directed_messages_seed_only_after_explicit_rejoin() {
    let mut store = team();
    call(&mut store, "claude", "leave", json!({}));
    call(
        &mut store,
        "codex",
        "send",
        json!({"to": "claude", "body": "Review when you return"}),
    );
    let receipts = call(
        &mut store,
        "codex",
        "show",
        json!({"id": 1, "receipts": true}),
    );
    assert!(receipts["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["agent"] != "claude"));
    assert_eq!(
        call(&mut store, "claude", "join", json!({}))["attention"]["total"],
        1
    );
}

#[test]
fn rejoin_does_not_echo_ones_own_publication() {
    let mut store = team();
    note(&mut store);
    assert_eq!(
        call(&mut store, "codex", "join", json!({}))["attention"]["total"],
        0
    );
}

#[test]
fn newcomer_discovers_existing_unassigned_work_across_scopes() {
    let mut store = team();
    call(
        &mut store,
        "codex",
        "post",
        json!({"kind": "task", "topic": "parser", "title": "Review", "summary": "Check the parser contract"}),
    );
    let joined = call(&mut store, "newcomer", "join", json!({"topics": ["docs"]}));
    assert_eq!(joined["attention"]["total"], 1);
    assert_eq!(joined["available"]["total"], 1);
}

#[test]
fn directed_question_reaches_peer_and_both_managers_without_broadcast_noise() {
    let mut store = team();
    let question = call(
        &mut store,
        "codex",
        "send",
        json!({
            "to": "claude", "body": "Can you review the parser?", "ask": true,
            "refs": ["mote:parser-42"], "priority": 1
        }),
    );
    assert_eq!(question["card"]["kind"], "question");
    assert_eq!(question["card"]["assignee"], "claude");
    assert_eq!(question["card"]["tags"], json!(["mote:parser-42"]));
    for actor in ["claude", "manager", "review-manager"] {
        assert_eq!(call(&mut store, actor, "inbox", json!({}))["total"], 1);
    }
    for actor in ["codex", "deepseek"] {
        assert_eq!(call(&mut store, actor, "inbox", json!({}))["total"], 0);
    }
    assert_eq!(
        call(&mut store, "deepseek", "join", json!({}))["attention"]["total"],
        0
    );
    assert_eq!(
        call(
            &mut store,
            "late-manager",
            "join",
            json!({"role": "steward", "topics": []})
        )["attention"]["total"],
        1
    );
    // Routing is public: an uninvolved peer can still inspect and search it.
    assert_eq!(
        call(
            &mut store,
            "deepseek",
            "query",
            json!({"tag": "mote:parser-42"})
        )["total"],
        1
    );
    let reply = call(
        &mut store,
        "claude",
        "annotate",
        json!({"id": 1, "body": "Found a missing empty-input test", "kind": "answer"}),
    );
    assert_eq!(
        call(&mut store, "codex", "inbox", json!({}))["items"][0]["through_seq"],
        reply["event_seq"]
    );
    assert_eq!(reply["card"]["status"], "open");
}

#[test]
fn send_retry_is_atomic_and_unknown_recipient_leaves_no_state() {
    let mut store = team();
    let bad = Request::new(
        "send",
        "codex",
        json!({"to": "typo", "body": "Review this"}),
    );
    assert!(store.execute_at(&bad, NOW).is_err());
    assert_eq!(store.highwater().unwrap(), 0);
    let mut request = Request::new(
        "send",
        "codex",
        json!({"to": "claude", "body": "Review this"}),
    );
    request.key = Some("review-request".into());
    let first = store.execute_at(&request, NOW).unwrap();
    assert_eq!(store.execute_at(&request, NOW + 1).unwrap(), first);
    assert_eq!(store.highwater().unwrap(), 1);
    request.args["to"] = json!("deepseek");
    assert_eq!(
        store.execute_at(&request, NOW).unwrap_err().code,
        "idempotency_conflict"
    );
}

#[test]
fn longest_identity_and_unicode_message_fit_the_contract() {
    let mut store = team();
    let actor = "x".repeat(80);
    call(&mut store, &actor, "join", json!({}));
    let sent = call(
        &mut store,
        "codex",
        "send",
        json!({"to": actor, "body": "🧠".repeat(500)}),
    );
    assert!(sent["card"]["title"].as_str().unwrap().len() <= 160);
    assert_eq!(sent["card"]["summary"].as_str().unwrap().len(), 2000);
}

#[test]
fn annotation_preview_reports_clipped_text() {
    let mut store = team();
    note(&mut store);
    call(
        &mut store,
        "codex",
        "annotate",
        json!({"id": 1, "body": "x".repeat(201)}),
    );
    let inbox = call(&mut store, "deepseek", "inbox", json!({}));
    assert_eq!(
        inbox["items"][0]["annotations"][0]["excerpt_truncated"],
        true
    );
}

#[test]
fn reply_references_are_additive_atomic_searchable_and_retry_safe() {
    let mut store = team();
    let sent = call(
        &mut store,
        "codex",
        "send",
        json!({"to":"claude","body":"Review","refs":["mote:original"]}),
    );
    let id = sent["card"]["id"].as_i64().unwrap();
    let mut request = Request::new(
        "annotate",
        "claude",
        json!({"id":id,"body":"Missing case","kind":"objection","refs":["mote:new","mote:new"]}),
    );
    request.key = Some("reply-refs".into());
    let reply = store.execute_at(&request, NOW).unwrap();
    assert_eq!(reply["card"]["tags"], json!(["mote:new", "mote:original"]));
    assert_eq!(reply["card"]["rev"], 2);
    assert_eq!(
        reply["follow_up"]["tags"],
        json!(["mote:new", format!("parent:{id}")])
    );
    assert_eq!(
        call(&mut store, "deepseek", "query", json!({"tag":"mote:new"}))["total"],
        2
    );
    let water = store.highwater().unwrap();
    assert_eq!(store.execute_at(&request, NOW + 1).unwrap(), reply);
    assert_eq!(store.highwater().unwrap(), water);
    let history = call(&mut store, "codex", "show", json!({"id":id,"history":true}));
    assert_eq!(
        history["history"][1]["payload"]["detail"]["refs"],
        json!(["mote:new"])
    );
    let plain = call(
        &mut store,
        "claude",
        "annotate",
        json!({"id":id,"body":"Acknowledged"}),
    );
    assert_eq!(plain["card"]["tags"], reply["card"]["tags"]);
    assert_eq!(plain["card"]["rev"], 2);
    let before = store.highwater().unwrap();
    for refs in [
        json!(["invalid ref"]),
        json!(42),
        json!((0..16).map(|i| format!("mote:{i}")).collect::<Vec<_>>()),
    ] {
        let bad = Request::new(
            "annotate",
            "claude",
            json!({"id":id,"body":"Must roll back","kind":"objection","refs":refs}),
        );
        assert!(store.execute_at(&bad, NOW).is_err());
        assert_eq!(store.highwater().unwrap(), before);
        assert_eq!(
            call(&mut store, "codex", "show", json!({"id":id}))["card"],
            plain["card"]
        );
    }
}

#[test]
fn long_message_preserves_exact_body_with_bounded_head_and_atomic_retry() {
    let mut store = team();
    let body = format!("{} uniqueending   ", "🧠".repeat(1996));
    assert_eq!(body.len(), 8000);
    let mut request = Request::new(
        "send",
        "codex",
        json!({"to":"claude","body":body,"ask":true}),
    );
    request.key = Some("long-message".into());
    let sent = store.execute_at(&request, NOW).unwrap();
    assert_eq!(store.execute_at(&request, NOW).unwrap(), sent);
    assert!(sent["card"]["summary"].as_str().unwrap().len() <= 2000);
    assert_eq!(sent["card"]["kind"], "question");
    let history = call(
        &mut store,
        "claude",
        "show",
        json!({"id":sent["card"]["id"],"history":true}),
    );
    assert_eq!(history["history"].as_array().unwrap().len(), 1);
    assert_eq!(history["history"][0]["payload"]["detail"]["body"], body);
    let inbox = call(&mut store, "claude", "inbox", json!({}));
    assert_eq!(inbox["total"], 1);
    // Addressed to claude, so the inbox carries the exact full message (from
    // its creation event), while the stored card head stays bounded.
    assert_eq!(inbox["items"][0]["card"]["summary"], body);
    assert_eq!(inbox["items"][0]["card"]["summary_truncated"], false);
    assert_eq!(inbox["items"][0]["full_text"], true);
    let head = call(
        &mut store,
        "claude",
        "show",
        json!({"id":sent["card"]["id"]}),
    );
    let head = head["card"]["summary"].as_str().unwrap();
    assert!(head.len() <= 2000 && head.contains("--bodies"), "{head}");
    assert_eq!(inbox["items"][0]["through_seq"], sent["event_seq"]);
    assert_eq!(
        call(
            &mut store,
            "claude",
            "search_history",
            json!({"q":"uniqueending"})
        )["items"]
            .as_array()
            .unwrap()
            .len(),
        1
    );

    // Byte limits, including multibyte input, fail before creating any head/event.
    let cursor = store.highwater().unwrap();
    for body in ["x".repeat(8001), "🧠".repeat(2001)] {
        let error = store
            .execute_at(
                &Request::new("send", "codex", json!({"to":"claude","body":body})),
                NOW,
            )
            .unwrap_err();
        assert_eq!(error.code, "invalid");
        assert_eq!(store.highwater().unwrap(), cursor);
    }
}

#[test]
fn inbox_filters_preserve_unselected_receipts_and_paginate_filtered_rows() {
    let mut store = team();
    note(&mut store); // Broad steward traffic.
    call(
        &mut store,
        "codex",
        "send",
        json!({"to":"claude","body":"Unrelated question","ask":true}),
    );
    for status in [
        "open",
        "active",
        "blocked",
        "resolved",
        "superseded",
        "withdrawn",
    ] {
        let sent = call(
            &mut store,
            "codex",
            "send",
            json!({"to":"manager","body":status,"ask":true}),
        );
        if status != "open" {
            call(
                &mut store,
                "codex",
                "patch",
                json!({"id":sent["card"]["id"],"expect":1,"status":status}),
            );
        }
    }
    // Outgoing replies are involved, but not currently addressed to the steward.
    let outgoing = call(
        &mut store,
        "manager",
        "send",
        json!({"to":"claude","body":"Review"}),
    );
    call(
        &mut store,
        "claude",
        "annotate",
        json!({"id":outgoing["card"]["id"],"body":"Reviewed"}),
    );
    assert_eq!(call(&mut store, "manager", "inbox", json!({}))["total"], 9);
    assert_eq!(
        call(
            &mut store,
            "manager",
            "inbox",
            json!({"addressed_to_me":true})
        )["total"],
        6
    );
    let first = call(
        &mut store,
        "manager",
        "inbox",
        json!({"addressed_to_me":true,"unresolved":true,"selection":"involved","limit":2}),
    );
    assert_eq!(first["total"], 3);
    assert_eq!(first["more"], true);
    let last = call(
        &mut store,
        "manager",
        "inbox",
        json!({"addressed_to_me":true,"unresolved":true,"after":first["items"][1]["through_seq"]}),
    );
    assert_eq!(last["items"].as_array().unwrap().len(), 1);
    assert_eq!(last["items"][0]["card"]["status"], "blocked");
    assert_eq!(call(&mut store, "manager", "inbox", json!({}))["total"], 9);
    let receipts: Vec<_> = first["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["receipt"].clone())
        .collect();
    call(&mut store, "manager", "ack", json!({"receipts":receipts}));
    assert_eq!(call(&mut store, "manager", "inbox", json!({}))["total"], 7);
    for field in ["addressed_to_me", "unresolved"] {
        let mut args = json!({});
        args[field] = json!("yes");
        assert_eq!(
            store
                .execute_at(&Request::new("inbox", "manager", args), NOW)
                .unwrap_err()
                .code,
            "invalid"
        );
    }
}
