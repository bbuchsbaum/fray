use fray::{
    model::Request,
    store::{InboxSelection, Store},
};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000_000;

fn call(store: &mut Store, actor: &str, op: &str, args: Value) -> Value {
    store
        .execute_at(&Request::new(op, actor, args), NOW)
        .unwrap()
}

fn fails(store: &mut Store, actor: &str, op: &str, args: Value) -> String {
    store
        .execute_at(&Request::new(op, actor, args), NOW)
        .unwrap_err()
        .code
}

fn pair() -> Store {
    let mut store = Store::memory().unwrap();
    for actor in ["codex", "claude", "deepseek"] {
        call(&mut store, actor, "join", json!({"topics": []}));
    }
    store
}

fn ask(store: &mut Store, body: &str) -> i64 {
    call(
        store,
        "codex",
        "send",
        json!({"to": "claude", "body": body, "ask": true}),
    )["card"]["id"]
        .as_i64()
        .unwrap()
}

fn pending(store: &mut Store, actor: &str) -> i64 {
    call(store, actor, "inbox", json!({}))["total"]
        .as_i64()
        .unwrap()
}

/// What `fray inbox` does: read, then record exactly the receipts it showed.
fn present_inbox(store: &mut Store, actor: &str) -> (String, Vec<Value>) {
    let page = call(store, actor, "inbox", json!({}));
    let receipts: Vec<Value> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["receipt"].clone())
        .collect();
    let batch = call(
        store,
        actor,
        "present",
        json!({"source": "inbox", "receipts": receipts}),
    );
    assert_eq!(batch["acknowledged"], false);
    (batch["batch"]["id"].as_str().unwrap().to_owned(), receipts)
}

#[test]
fn presenting_never_acknowledges() {
    let mut s = pair();
    ask(&mut s, "Review the stream?");
    let (batch, _) = present_inbox(&mut s, "claude");
    assert_eq!(pending(&mut s, "claude"), 1);
    let fetched = call(&mut s, "claude", "batch", json!({"batch": batch}));
    assert_eq!(fetched["items"][0]["handled"], false);
    assert_eq!(fetched["read_is_not_ack"], true);
    assert_eq!(pending(&mut s, "claude"), 1);
}

#[test]
fn batch_ack_covers_exactly_what_was_shown() {
    let mut s = pair();
    let id = ask(&mut s, "Review the stream?");
    let (batch, receipts) = present_inbox(&mut s, "claude");
    let shown = receipts[0]["through_seq"].as_i64().unwrap();
    // A newer reply lands after the read but before the ack.
    call(
        &mut s,
        "codex",
        "annotate",
        json!({"id": id, "kind": "note", "body": "Also check reconnects"}),
    );
    let acked = call(&mut s, "claude", "ack", json!({"batch": batch}));
    assert_eq!(acked["acknowledged"][0]["ack_seq"], shown);
    assert_eq!(acked["acknowledged"][0]["still_pending"], true);
    assert_eq!(pending(&mut s, "claude"), 1);
    let fetched = call(&mut s, "claude", "batch", json!({"batch": batch}));
    assert_eq!(fetched["items"][0]["handled"], true);
    assert_eq!(fetched["items"][0]["newer_pending"], true);
    // Repeating the ack is harmless and never advances past the shown version.
    let again = call(&mut s, "claude", "ack", json!({"batch": batch}));
    assert_eq!(again["acknowledged"][0]["ack_seq"], shown);
}

#[test]
fn batch_ack_subset_by_ids() {
    let mut s = pair();
    let first = ask(&mut s, "First?");
    let second = ask(&mut s, "Second?");
    let (batch, _) = present_inbox(&mut s, "claude");
    call(
        &mut s,
        "claude",
        "ack",
        json!({"batch": batch, "ids": [second]}),
    );
    let left = call(&mut s, "claude", "inbox", json!({}));
    assert_eq!(left["total"], 1);
    assert_eq!(left["items"][0]["card"]["id"], first);
}

#[test]
fn foreign_unknown_and_mismatched_batches_acknowledge_nothing() {
    let mut s = pair();
    ask(&mut s, "Review the stream?");
    let (batch, _) = present_inbox(&mut s, "claude");
    call(
        &mut s,
        "codex",
        "send",
        json!({"to": "deepseek", "body": "hi"}),
    );
    assert_eq!(
        fails(&mut s, "deepseek", "ack", json!({"batch": batch})),
        "batch_foreign"
    );
    assert_eq!(
        fails(&mut s, "deepseek", "batch", json!({"batch": batch})),
        "batch_foreign"
    );
    assert_eq!(
        fails(&mut s, "claude", "ack", json!({"batch": "0".repeat(32)})),
        "batch_unknown"
    );
    assert_eq!(
        fails(
            &mut s,
            "claude",
            "ack",
            json!({"batch": batch, "ids": [999]})
        ),
        "batch_mismatch"
    );
    assert_eq!(pending(&mut s, "claude"), 1);
    assert_eq!(pending(&mut s, "deepseek"), 1);
}

#[test]
fn present_rejects_receipts_that_were_not_delivered() {
    let mut s = pair();
    let id = ask(&mut s, "Review the stream?");
    let (_, receipts) = present_inbox(&mut s, "claude");
    let mut ahead = receipts[0].clone();
    ahead["through_seq"] = json!(ahead["through_seq"].as_i64().unwrap() + 100);
    assert_eq!(
        fails(
            &mut s,
            "claude",
            "present",
            json!({"source": "inbox", "receipts": [ahead]})
        ),
        "invalid"
    );
    assert_eq!(
        fails(
            &mut s,
            "deepseek",
            "present",
            json!({"source": "inbox", "receipts": receipts})
        ),
        "receipt_mismatch"
    );
    let twice = vec![receipts[0].clone(), receipts[0].clone()];
    assert_eq!(
        fails(
            &mut s,
            "claude",
            "present",
            json!({"source": "inbox", "receipts": twice})
        ),
        "invalid"
    );
    assert_eq!(
        fails(
            &mut s,
            "claude",
            "present",
            json!({"source": "doctor", "receipts": receipts})
        ),
        "invalid"
    );
    let _ = id;
}

#[test]
fn old_batches_expire_by_retention() {
    let mut s = pair();
    ask(&mut s, "Review the stream?");
    let (first, _) = present_inbox(&mut s, "claude");
    for _ in 0..32 {
        present_inbox(&mut s, "claude");
    }
    assert_eq!(
        fails(&mut s, "claude", "ack", json!({"batch": first})),
        "batch_unknown"
    );
    assert_eq!(pending(&mut s, "claude"), 1);
}

#[test]
fn unread_thread_receipt_covers_only_the_shown_page() {
    let mut s = pair();
    let id = ask(&mut s, "Review the stream?");
    for n in 0..4 {
        call(
            &mut s,
            "codex",
            "annotate",
            json!({"id": id, "kind": "note", "body": format!("detail {n}")}),
        );
    }
    let page = call(
        &mut s,
        "claude",
        "show",
        json!({"id": id, "unread": true, "limit": 2}),
    );
    assert_eq!(page["unread"].as_array().unwrap().len(), 2);
    assert_eq!(page["more"], true);
    let shown_last = page["unread"][1]["seq"].as_i64().unwrap();
    assert_eq!(page["receipt"]["through_seq"], shown_last);
    assert!(page["pending_seq"].as_i64().unwrap() > shown_last);
    // A new reply while reading does not extend what the first page covers.
    call(
        &mut s,
        "codex",
        "annotate",
        json!({"id": id, "kind": "note", "body": "late"}),
    );
    call(
        &mut s,
        "claude",
        "ack",
        json!({"receipts": [page["receipt"].clone()]}),
    );
    // Page two continues exactly after the acknowledged prefix.
    let next = call(
        &mut s,
        "claude",
        "show",
        json!({"id": id, "unread": true, "limit": 100}),
    );
    assert_eq!(next["ack_seq"], shown_last);
    assert!(next["unread"][0]["seq"].as_i64().unwrap() > shown_last);
    assert_eq!(next["more"], false);
    let bodies: Vec<&str> = next["unread"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["body"].as_str())
        .collect();
    assert!(bodies.contains(&"late"), "{bodies:?}");
    call(
        &mut s,
        "claude",
        "ack",
        json!({"receipts": [next["receipt"].clone()]}),
    );
    assert_eq!(pending(&mut s, "claude"), 0);
    let done = call(&mut s, "claude", "show", json!({"id": id, "unread": true}));
    assert_eq!(done["unread"].as_array().unwrap().len(), 0);
    assert!(done["receipt"].is_null());
}

#[test]
fn thread_lists_open_objections_until_resolved() {
    let mut s = pair();
    let id = ask(&mut s, "Review the stream?");
    let objection = call(
        &mut s,
        "claude",
        "annotate",
        json!({"id": id, "kind": "objection", "body": "Budget band kills the stream"}),
    )["follow_up"]
        .clone();
    let shown = call(&mut s, "codex", "show", json!({"id": id}));
    assert_eq!(shown["follow_ups"][0]["id"], objection["id"]);
    assert_eq!(shown["follow_ups"][0]["assignee"], "codex");
    call(
        &mut s,
        "codex",
        "patch",
        json!({"id": objection["id"], "expect": objection["rev"], "status": "resolved"}),
    );
    let shown = call(&mut s, "codex", "show", json!({"id": id}));
    assert_eq!(shown["follow_ups"].as_array().unwrap().len(), 0);
}

#[test]
fn directly_built_kind_filters_cannot_inject_sql() {
    let mut s = pair();
    ask(&mut s, "Review the stream?");
    let hostile = InboxSelection {
        mode: "all",
        addressed_to_me: false,
        unresolved: false,
        kinds: vec!["x') OR 1=1 --".into()],
        min_priority: None,
        card_ids: Vec::new(),
    };
    let page = s.filtered_attention("claude", 0, 10, hostile, NOW).unwrap();
    assert_eq!(page["total"], 0);
    let honest = InboxSelection {
        mode: "all",
        addressed_to_me: false,
        unresolved: false,
        kinds: vec!["question".into()],
        min_priority: None,
        card_ids: Vec::new(),
    };
    assert_eq!(
        s.filtered_attention("claude", 0, 10, honest, NOW).unwrap()["total"],
        1
    );
}

#[test]
fn continuation_receipt_requires_the_skipped_prefix_to_be_presented() {
    let mut s = pair();
    let id = ask(&mut s, "Review the stream?");
    for n in 0..4 {
        call(
            &mut s,
            "codex",
            "annotate",
            json!({"id": id, "kind": "note", "body": format!("detail {n}")}),
        );
    }
    let page1 = call(
        &mut s,
        "claude",
        "show",
        json!({"id": id, "unread": true, "limit": 2}),
    );
    let after = page1["next_after"].as_i64().unwrap();
    // Jumping ahead without presenting page one: content, but no receipt,
    // because ACK is cumulative and would cover events never shown.
    let skipped = call(
        &mut s,
        "claude",
        "show",
        json!({"id": id, "unread": true, "limit": 2, "after": after}),
    );
    assert!(skipped["unread"][0]["seq"].as_i64().unwrap() > after);
    assert!(skipped["receipt"].is_null());
    assert!(skipped["receipt_withheld"].is_string());
    assert_eq!(skipped["ack_seq"], 0);
    // Once page one was presented (as `fray thread --unread` does), page two
    // carries a receipt, and reading still acknowledges nothing.
    call(
        &mut s,
        "claude",
        "present",
        json!({"source": "thread", "receipts": [page1["receipt"].clone()]}),
    );
    let page2 = call(
        &mut s,
        "claude",
        "show",
        json!({"id": id, "unread": true, "limit": 2, "after": after}),
    );
    assert!(page2["receipt"]["through_seq"].as_i64().unwrap() > after);
    assert_eq!(page2["ack_seq"], 0);
    assert_eq!(pending(&mut s, "claude"), 1);
}

#[test]
fn compact_history_keeps_every_state_change_and_unread_takes_precedence() {
    let mut s = pair();
    let posted = call(
        &mut s,
        "codex",
        "post",
        json!({"title": "Plan", "summary": "v1", "kind": "task", "assignee": "claude"}),
    );
    let id = posted["card"]["id"].clone();
    let patched = call(
        &mut s,
        "codex",
        "patch",
        json!({"id": id, "expect": 1, "summary": "v2 only the summary changed"}),
    );
    call(
        &mut s,
        "codex",
        "patch",
        json!({"id": id, "expect": patched["card"]["rev"], "topic": "parser", "kind": "decision"}),
    );
    let compact = call(
        &mut s,
        "claude",
        "show",
        json!({"id": id, "history": true, "compact": true}),
    );
    let history = compact["history"].as_array().unwrap();
    assert_eq!(history[0]["body"], "v1");
    assert_eq!(
        history[1]["changed"],
        json!({"summary": "v2 only the summary changed"})
    );
    assert_eq!(
        history[2]["changed"],
        json!({"topic": "parser", "kind": "decision"})
    );
    assert!(history.iter().all(|e| e.get("payload").is_none()));
    // Asking for both: unread wins and is itself compact.
    let both = call(
        &mut s,
        "claude",
        "show",
        json!({"id": id, "history": true, "compact": true, "unread": true}),
    );
    assert!(both.get("history").is_none());
    assert!(both["unread"][0].get("payload").is_none());
}

#[test]
fn presenting_does_not_count_as_presence() {
    let mut s = pair();
    ask(&mut s, "Review the stream?");
    let page = call(&mut s, "claude", "inbox", json!({}));
    let later = NOW + 600_000;
    s.execute_at(
        &Request::new(
            "present",
            "claude",
            json!({"source": "attention", "receipts": [page["items"][0]["receipt"].clone()]}),
        ),
        later,
    )
    .unwrap();
    let roster = s
        .execute_at(&Request::new("agents", "codex", json!({})), later)
        .unwrap();
    let claude = roster["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "claude")
        .unwrap()
        .clone();
    assert_eq!(claude["recently_seen"], false, "{claude}");
}

#[test]
fn open_follow_ups_are_never_silently_truncated() {
    let mut s = pair();
    let id = ask(&mut s, "Review the stream?");
    for n in 0..51 {
        call(
            &mut s,
            "claude",
            "annotate",
            json!({"id": id, "kind": "question", "body": format!("question {n}")}),
        );
    }
    let shown = call(&mut s, "codex", "show", json!({"id": id}));
    assert_eq!(shown["follow_ups"].as_array().unwrap().len(), 50);
    assert_eq!(shown["follow_ups_more"], true);
    let next = shown["follow_ups_next"].as_str().unwrap();
    assert!(next.contains(&format!("parent:{id}")) && next.contains("--offset 50"));
    // The continuation really yields the omitted one.
    let rest = call(
        &mut s,
        "codex",
        "query",
        json!({"tag": format!("parent:{id}"), "sort": "id", "offset": 50}),
    );
    assert_eq!(rest["items"].as_array().unwrap().len(), 1);
    assert_eq!(rest["items"][0]["title"], "Question on #1: question 50");
    // Exactly 50 open: no marker.
    let first = shown["follow_ups"][0]["id"].clone();
    call(
        &mut s,
        "codex",
        "patch",
        json!({"id": first, "expect": 1, "status": "resolved"}),
    );
    let shown = call(&mut s, "codex", "show", json!({"id": id}));
    assert_eq!(shown["follow_ups"].as_array().unwrap().len(), 50);
    assert_eq!(shown["follow_ups_more"], false);
    assert!(shown.get("follow_ups_next").is_none());
}

#[test]
fn unread_skips_the_readers_own_messages() {
    let mut s = pair();
    let id = ask(&mut s, "Review the stream?");
    let note = |s: &mut Store, who: &str, body: &str| {
        call(
            s,
            who,
            "annotate",
            json!({"id": id, "kind": "note", "body": body}),
        );
    };
    // Interleaved conversation: claude's own replies sit between codex's.
    note(&mut s, "claude", "mine 1");
    note(&mut s, "codex", "theirs 1");
    note(&mut s, "claude", "mine 2");
    note(&mut s, "codex", "theirs 2");
    note(&mut s, "codex", "theirs 3");
    let bodies = |page: &Value| -> Vec<String> {
        page["unread"]
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["body"].as_str().unwrap_or("").to_owned())
            .collect()
    };
    let page = call(&mut s, "claude", "show", json!({"id": id, "unread": true}));
    assert_eq!(
        bodies(&page),
        ["Review the stream?", "theirs 1", "theirs 2", "theirs 3"]
    );
    assert_eq!(page["own_skipped"], 2);
    // The limit counts only what is shown, so a page is never eaten by own messages.
    let first = call(
        &mut s,
        "claude",
        "show",
        json!({"id": id, "unread": true, "limit": 2}),
    );
    assert_eq!(bodies(&first), ["Review the stream?", "theirs 1"]);
    assert_eq!(first["more"], true);
    // The cumulative receipt still covers the skipped own messages: after
    // acknowledging the full page, nothing lingers as unread.
    call(
        &mut s,
        "claude",
        "ack",
        json!({"receipts": [page["receipt"].clone()]}),
    );
    assert_eq!(pending(&mut s, "claude"), 0);
    let after = call(&mut s, "claude", "show", json!({"id": id, "unread": true}));
    assert_eq!(after["unread"].as_array().unwrap().len(), 0);
    assert_eq!(after["own_skipped"], 0);
}
