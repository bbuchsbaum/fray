use fray::notification::{notices, NOTICE_BYTES};
use serde_json::json;

#[test]
fn small_notifications_retain_every_exact_receipt_with_unicode_and_escaping() {
    let items:Vec<_>=(1..=12).map(|id| json!({"card":{"id":id,"title":"🧠\\\"\n".repeat(150),"priority":1},"receipt":{"store_id":"a".repeat(32),"agent":"x".repeat(80),"id":id,"through_seq":i64::MAX}})).collect();
    let packet = json!({"items":items,"more":true});
    let notifications = notices(&packet).unwrap();
    assert_eq!(notifications.len(), 12);
    for (index, notice) in notifications.iter().enumerate() {
        assert!(serde_json::to_vec(notice).unwrap().len() < NOTICE_BYTES);
        assert_eq!(notice["receipt"], packet["items"][index]["receipt"]);
        assert_eq!(notice["remaining_in_packet"], 11 - index);
        assert_eq!(notice["more"], true);
        assert_eq!(notice["read_is_not_ack"], true);
    }
}

#[test]
fn malformed_receipts_fail_visibly_instead_of_emitting_bad_ack_instructions() {
    assert!(notices(
        &json!({"items":[{"receipt":{"id":1,"through_seq":2,"agent":"valid","store_id":"wrong"}}]})
    )
    .is_err());
}

#[test]
fn activation_metadata_is_declared_validated_and_separate_from_selection() {
    use fray::notification::take_activation;
    let mut args = json!({"selection":"involved","activation":"background-completion","activation_expires_ms":123});
    let declared = take_activation(&mut args).unwrap().unwrap();
    assert_eq!(declared["mode"], "background-completion");
    assert_eq!(declared["source"], "adapter-declared");
    assert_eq!(declared["expires_ms"], 123);
    assert_eq!(args, json!({"selection":"involved"}));
    for mut args in [
        json!({"activation":"responsive"}),
        json!({"activation_expires_ms":1}),
        json!({"activation":"manual","activation_expires_ms":-1}),
    ] {
        assert!(take_activation(&mut args).is_err());
    }
}

#[test]
fn one_small_batch_notice_refers_to_all_immutable_receipts() {
    let items: Vec<_> = (1..=12)
        .map(|id| json!({"card":{"title":"🧠".repeat(60)},"receipt":{"id":id}}))
        .collect();
    let notices=notices(&json!({"store_id":"a".repeat(32),"agent":"x".repeat(80),"batch":"b".repeat(32),"items":items,"more":true})).unwrap();
    assert_eq!(notices.len(), 1);
    assert_eq!(notices[0]["receipt_count"], 12);
    assert_eq!(notices[0]["fetch"], json!(["batch", "b".repeat(32)]));
    assert!(serde_json::to_vec(&notices[0]).unwrap().len() < NOTICE_BYTES);
}

fn item(
    receipt_agent: &str,
    through_seq: i64,
    title: &str,
    messages: serde_json::Value,
) -> serde_json::Value {
    json!({
        "card": {"title": title, "priority": 1},
        "receipt": {"store_id": "a".repeat(32), "agent": receipt_agent, "id": 7, "through_seq": through_seq},
        "messages": messages,
    })
}

#[test]
fn notification_prefers_latest_peer_reply_over_stale_card_title() {
    let packet = json!({"items": [item("me", 8, "Stale task title", json!([
        {"seq": 4, "author": "me", "kind": "note", "body": "my draft"},
        {"seq": 8, "author": "peer", "kind": "answer", "body": "\n\nFresh reply\nmore detail"},
    ]))], "more": false});
    let notice = notices(&packet).unwrap().pop().unwrap();
    assert_eq!(notice["title"], "Stale task title");
    assert_eq!(
        notice["preview"],
        json!({"seq": 8, "author": "peer", "kind": "answer", "body": "Fresh reply"})
    );
}

#[test]
fn notification_preview_is_unicode_safe_and_receipt_bounded() {
    let packet = json!({"items": [item("me", 4, "Title", json!([
        {"seq": 4, "author": "李雷", "kind": "evidence", "body": "\n 🧠 first useful line \"quoted\"\nsecond line"},
        {"seq": 5, "author": "peer", "kind": "answer", "body": "later and excluded"},
    ]))], "more": false});
    let notice = notices(&packet).unwrap().pop().unwrap();
    assert_eq!(notice["preview"]["seq"], 4);
    assert_eq!(notice["preview"]["author"], "李雷");
    assert_eq!(notice["preview"]["body"], "🧠 first useful line \"quoted\"");
    assert_eq!(notice["receipt"]["through_seq"], 4);
    assert!(serde_json::to_vec(&notice).unwrap().len() < NOTICE_BYTES);
}

#[test]
fn notification_falls_back_to_title_without_an_eligible_peer_message() {
    let packet = json!({"items": [item("me", 4, "Read the original card", json!([
        {"seq": 4, "author": "me", "kind": "note", "body": "self only"},
        {"seq": 5, "author": "peer", "kind": "answer", "body": "too late"},
    ]))], "more": false});
    let notice = notices(&packet).unwrap().pop().unwrap();
    assert_eq!(notice["title"], "Read the original card");
    assert!(notice["preview"].is_null());
}

#[test]
fn batch_notification_shortens_preview_and_title_before_dropping_items() {
    let packet_agent = "x".repeat(80);
    let peer = "a".repeat(80);
    let items = vec![
        item(
            "me",
            i64::MAX,
            &"🧠".repeat(200),
            json!([{"seq": i64::MAX, "author": peer, "kind": "answer", "body": "🧠\"".repeat(2_000)}]),
        ),
        item(
            "me",
            2,
            &"title".repeat(200),
            json!([{"seq": 2, "author": "peer", "kind": "answer", "body": "🧠".repeat(2_000)}]),
        ),
    ];
    let notice = notices(&json!({"store_id": "a".repeat(32), "agent": packet_agent, "batch": "b".repeat(32), "items": items, "more": false})).unwrap().pop().unwrap();
    assert!(!notice["titles"].as_array().unwrap().is_empty());
    assert_eq!(notice["titles"][0]["id"], 7);
    assert!(!notice["titles"][0]["preview"]["body"]
        .as_str()
        .unwrap()
        .is_empty());
    assert_eq!(notice["receipt_count"], 2);
    assert_eq!(notice["batch"], "b".repeat(32));
    assert_eq!(notice["fetch"], json!(["batch", "b".repeat(32)]));
    assert!(serde_json::to_vec(&notice).unwrap().len() < NOTICE_BYTES);
}
