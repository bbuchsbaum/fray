use fray::{model::Request, store::Store};
use serde_json::{json, Value};
fn call(store: &mut Store, actor: &str, op: &str, args: Value) -> Value {
    store
        .execute_at(&Request::new(op, actor, args), 1_800_000_000_000)
        .unwrap()
}
#[test]
fn unread_pages_can_be_read_without_acknowledging_the_first_page() {
    let mut store = Store::memory().unwrap();
    for actor in ["writer", "reader"] {
        call(&mut store, actor, "join", json!({}));
    }
    let first = call(
        &mut store,
        "writer",
        "send",
        json!({"to":"reader","ask":true,"body":"Initial question"}),
    );
    let id = first["card"]["id"].clone();
    for body in ["context one", "context two", "context three"] {
        call(
            &mut store,
            "writer",
            "annotate",
            json!({"id":id,"body":body}),
        );
    }
    let page1 = call(
        &mut store,
        "reader",
        "show",
        json!({"id":id,"unread":true,"limit":2}),
    );
    let after = page1["next_after"].as_i64().unwrap();
    let page2 = call(
        &mut store,
        "reader",
        "show",
        json!({"id":id,"unread":true,"limit":2,"after":after}),
    );
    assert!(
        page2["unread"][0]["seq"].as_i64().unwrap() > after,
        "continuation repeated the first page: {page2}"
    );
    assert_eq!(page2["ack_seq"], 0, "reading must not acknowledge page one");
}

fn setup(store: &mut Store) -> i64 {
    for actor in ["writer", "reader"] {
        call(store, actor, "join", json!({}));
    }
    call(
        store,
        "writer",
        "send",
        json!({"to":"reader","body":"Review this"}),
    )["card"]["id"]
        .as_i64()
        .unwrap()
}

fn presented(store: &mut Store) -> (Value, Value) {
    let receipt = call(store, "reader", "inbox", json!({}))["items"][0]["receipt"].clone();
    let batch = call(
        store,
        "reader",
        "present",
        json!({"source":"inbox","receipts":[receipt]}),
    )["batch"]["id"]
        .clone();
    (batch, receipt)
}

#[test]
fn interleaved_reader_batches_keep_exact_versions_across_store_restart() {
    let dir = std::env::temp_dir().join(format!(
        "fray-batch-review-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("board.db");
    let mut store = Store::open(&path, false).unwrap();
    let id = setup(&mut store);
    let (early, early_receipt) = presented(&mut store);
    call(
        &mut store,
        "writer",
        "annotate",
        json!({"id":id,"body":"Later evidence"}),
    );
    let (late, late_receipt) = presented(&mut store);
    assert_ne!(early, late);
    drop(store);
    let mut restarted = Store::open(&path, false).unwrap();
    assert_eq!(
        call(&mut restarted, "reader", "batch", json!({"batch":early}))["items"][0]["receipt"],
        early_receipt
    );
    let acked = call(&mut restarted, "reader", "ack", json!({"batch":early}));
    assert_eq!(
        acked["acknowledged"][0]["ack_seq"],
        early_receipt["through_seq"]
    );
    assert_eq!(acked["acknowledged"][0]["still_pending"], true);
    let later = call(&mut restarted, "reader", "batch", json!({"batch":late}));
    assert_eq!(later["items"][0]["receipt"], late_receipt);
    assert_eq!(later["items"][0]["handled"], false);
    call(&mut restarted, "reader", "ack", json!({"batch":late}));
    assert_eq!(
        call(&mut restarted, "reader", "inbox", json!({}))["total"],
        0
    );
    drop(restarted);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn token_from_another_store_cannot_ack_same_actor_and_card_number() {
    let mut origin = Store::memory().unwrap();
    let mut other = Store::memory().unwrap();
    assert_eq!(setup(&mut origin), setup(&mut other));
    let (batch, _) = presented(&mut origin);
    let error = other
        .execute_at(
            &Request::new("ack", "reader", json!({"batch":batch})),
            1_800_000_000_000,
        )
        .unwrap_err();
    assert_eq!(error.code, "batch_unknown");
    assert_eq!(
        call(&mut other, "reader", "inbox", json!({}))["items"][0]["ack_seq"],
        0
    );
}

#[test]
fn compact_conversation_preserves_changed_summary_and_reply_references() {
    let mut store = Store::memory().unwrap();
    let id = setup(&mut store);
    call(
        &mut store,
        "writer",
        "patch",
        json!({"id":id,"expect":1,"summary":"Revised contract"}),
    );
    call(
        &mut store,
        "reader",
        "annotate",
        json!({"id":id,"body":"Counterexample\nwith detail","kind":"evidence","refs":["mote:review-42"]}),
    );
    let compact = call(
        &mut store,
        "writer",
        "show",
        json!({"id":id,"history":true,"compact":true}),
    );
    assert_eq!(
        compact["history"][1]["changed"]["summary"],
        "Revised contract"
    );
    assert_eq!(compact["history"][2]["body"], "Counterexample\nwith detail");
    assert_eq!(compact["history"][2]["refs"], json!(["mote:review-42"]));
}
