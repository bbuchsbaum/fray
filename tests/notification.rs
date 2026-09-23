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
