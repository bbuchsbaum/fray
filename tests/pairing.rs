use fray::{model::Request, store::Store};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000_000;

fn call(store: &mut Store, actor: &str, op: &str, args: Value) -> Value {
    store
        .execute_at(
            &Request::new(op, actor, args).with_session(Some(format!("test:{actor}"))),
            NOW,
        )
        .unwrap()
}

fn pair() -> (Store, i64) {
    let mut store = Store::memory().unwrap();
    for actor in ["writer", "reader"] {
        call(&mut store, actor, "join", json!({"topics":[]}));
    }
    let id = call(
        &mut store,
        "writer",
        "send",
        json!({"to":"reader","body":"Review this"}),
    )["card"]["id"]
        .as_i64()
        .unwrap();
    (store, id)
}

fn present(store: &mut Store, source: &str) -> Value {
    let page = call(store, "reader", "inbox", json!({}));
    let receipts: Vec<_> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["receipt"].clone())
        .collect();
    call(
        store,
        "reader",
        "present",
        json!({"source":source,"receipts":receipts}),
    )["batch"]["id"]
        .clone()
}

#[test]
fn reply_ack_is_exact_card_scoped_and_covers_overlapping_watch_batch() {
    let (mut store, id) = pair();
    let other = call(
        &mut store,
        "writer",
        "send",
        json!({"to":"reader","body":"Separate request"}),
    )["card"]["id"]
        .clone();
    let watch = present(&mut store, "attention");
    let read = present(&mut store, "thread");
    let result = call(
        &mut store,
        "reader",
        "annotate",
        json!({"id":id,"body":"Verified","ack_batch":read}),
    );
    assert_eq!(result["acknowledged"].as_array().unwrap().len(), 1);
    assert_eq!(result["acknowledged"][0]["id"], id);
    assert!(result["reply_warning"].is_null());
    let fetched = call(&mut store, "reader", "batch", json!({"batch":watch}));
    assert_eq!(
        fetched["items"][0]["handled"], true,
        "one ack handles every overlapping receipt"
    );
    let remaining = call(&mut store, "reader", "inbox", json!({}));
    assert_eq!(remaining["total"], 1);
    assert_eq!(remaining["items"][0]["card"]["id"], other);
}

#[test]
fn crossing_reply_warns_even_after_background_exposure_and_keeps_newer_pending() {
    let (mut store, id) = pair();
    let read = present(&mut store, "thread");
    let later = call(
        &mut store,
        "writer",
        "annotate",
        json!({"id":id,"body":"Changed requirement"}),
    );
    present(&mut store, "attention");
    let result = call(
        &mut store,
        "reader",
        "annotate",
        json!({"id":id,"body":"Answer to old requirement","ack_batch":read}),
    );
    assert_eq!(result["unseen_updates"]["count"], 1);
    assert_eq!(result["unseen_updates"]["through_seq"], later["event_seq"]);
    assert!(result["reply_warning"].as_str().unwrap().contains("thread"));
    assert_eq!(result["acknowledged"][0]["still_pending"], true);
    assert_eq!(call(&mut store, "reader", "inbox", json!({}))["total"], 1);
    // Reading, without acknowledging, is sufficient to avoid the crossing warning.
    present(&mut store, "thread");
    let reply = call(
        &mut store,
        "reader",
        "annotate",
        json!({"id":id,"body":"Now addressing the change"}),
    );
    assert!(reply["reply_warning"].is_null());
    assert_eq!(call(&mut store, "reader", "inbox", json!({}))["total"], 1);
}

#[test]
fn invalid_reply_ack_rolls_back_reply_refs_and_linked_objection() {
    let (mut store, id) = pair();
    let other = call(
        &mut store,
        "writer",
        "send",
        json!({"to":"reader","body":"Other card"}),
    )["card"]["id"]
        .clone();
    let receipt = call(&mut store, "reader", "inbox", json!({"card_ids":[other]}))["items"][0]
        ["receipt"]
        .clone();
    let wrong_card = call(
        &mut store,
        "reader",
        "present",
        json!({"source":"thread","receipts":[receipt]}),
    )["batch"]["id"]
        .clone();
    let before = call(
        &mut store,
        "reader",
        "show",
        json!({"id":id,"history":true}),
    );
    for (batch, code) in [
        (wrong_card, "batch_mismatch"),
        (json!("0".repeat(32)), "batch_unknown"),
    ] {
        let error = store.execute_at(&Request::new("annotate", "reader", json!({"id":id,"body":"Concern","kind":"objection","refs":["mote:test"],"ack_batch":batch})), NOW).unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(
            call(
                &mut store,
                "reader",
                "show",
                json!({"id":id,"history":true})
            ),
            before
        );
    }
}

#[test]
fn failure_after_ack_validation_rolls_back_the_ack_too() {
    let (mut store, id) = pair();
    let batch = present(&mut store, "thread");
    let before = call(&mut store, "reader", "inbox", json!({}));
    // Reserved refs are rejected after the nested acknowledgement has executed.
    let error = store.execute_at(&Request::new("annotate", "reader", json!({
        "id":id,"body":"Concern","kind":"objection","refs":["authority:owner"],"ack_batch":batch
    })).with_session(Some("test:reader".into())), NOW).unwrap_err();
    assert_eq!(error.code, "reserved_owner");
    assert_eq!(call(&mut store, "reader", "inbox", json!({})), before);
    assert_eq!(
        call(&mut store, "reader", "batch", json!({"batch":batch}))["items"][0]["handled"],
        false
    );
}

#[test]
fn unbound_readers_do_not_share_an_implicit_read_session() {
    let (mut store, id) = pair();
    let receipt = call(&mut store, "reader", "inbox", json!({}))["items"][0]["receipt"].clone();
    for source in ["inbox", "thread"] {
        store
            .execute_at(
                &Request::new(
                    "present",
                    "reader",
                    json!({"source":source,"receipts":[receipt]}),
                ),
                NOW,
            )
            .unwrap();
    }
    let reply = store
        .execute_at(
            &Request::new(
                "annotate",
                "reader",
                json!({"id":id,"body":"Answer from an unbound caller"}),
            ),
            NOW,
        )
        .unwrap();
    assert_eq!(reply["unseen_updates"]["count"], 1);
    assert!(reply["reply_warning"].is_string());
}
