use fray::{
    model::{random_key, Request},
    store::Store,
};
use serde_json::{json, Value};
const NOW: i64 = 1_800_000_000_000;
fn call(s: &mut Store, who: &str, op: &str, args: Value) -> Value {
    s.execute_at(&Request::new(op, who, args), NOW).unwrap()
}
fn board() -> (Store, i64, i64) {
    let mut s = Store::memory().unwrap();
    for who in ["writer", "objector", "other"] {
        call(&mut s, who, "join", json!({"topics":[]}));
    }
    let parent = call(
        &mut s,
        "writer",
        "send",
        json!({"to":"objector","ask":true,"body":"Review this"}),
    )["card"]["id"]
        .as_i64()
        .unwrap();
    let child = call(
        &mut s,
        "objector",
        "annotate",
        json!({"id":parent,"kind":"objection","body":"Missing evidence"}),
    )["follow_up"]["id"]
        .as_i64()
        .unwrap();
    (s, parent, child)
}
#[test]
fn only_the_objector_or_owner_can_close_a_linked_objection() {
    for status in ["resolved", "withdrawn", "superseded"] {
        let (mut s, _, child) = board();
        let before = s.highwater().unwrap();
        for who in ["writer", "other"] {
            assert_eq!(
                s.execute_at(
                    &Request::new("patch", who, json!({"id":child,"expect":1,"status":status})),
                    NOW
                )
                .unwrap_err()
                .code,
                "objection_authority"
            );
            assert_eq!(s.highwater().unwrap(), before);
        }
        call(
            &mut s,
            "objector",
            "patch",
            json!({"id":child,"expect":1,"status":status}),
        );
        let shown = call(&mut s, "writer", "show", json!({"id":child}));
        assert_eq!(shown["objection"]["resolved_by"], "objector");
    }
    let (mut s, _, child) = board();
    call(
        &mut s,
        "owner",
        "owner_answer",
        json!({"id":child,"expect":1,"verdict":"approve","body":"Evidence accepted"}),
    );
    assert_eq!(
        call(&mut s, "writer", "show", json!({"id":child}))["objection"]["resolved_by"],
        "owner"
    );
}
#[test]
fn override_and_resolution_notices_survive_restart_mutes_and_stale_ack() {
    let dir = std::env::temp_dir().join(format!("fray-objection-{}", random_key().unwrap()));
    std::fs::create_dir(&dir).unwrap();
    let (memory, parent, child) = board();
    // Seed the file store through the same public operations.
    let mut s = Store::open(&dir.join("state.db"), false).unwrap();
    for who in ["writer", "objector", "other"] {
        call(&mut s, who, "join", json!({"topics":[]}));
    }
    call(
        &mut s,
        "writer",
        "send",
        json!({"to":"objector","ask":true,"body":"Review this"}),
    );
    call(
        &mut s,
        "objector",
        "annotate",
        json!({"id":parent,"kind":"objection","body":"Missing evidence"}),
    );
    drop(memory);
    let old = call(&mut s, "objector", "inbox", json!({}))["items"][0]["receipt"].clone();
    // Turn the parent into a task to permit muting it; objection origins stay.
    call(
        &mut s,
        "writer",
        "patch",
        json!({"id":parent,"expect":1,"kind":"task","assignee":"writer"}),
    );
    call(&mut s, "objector", "mute", json!({"id":parent}));
    call(&mut s, "objector", "leave", json!({}));
    call(
        &mut s,
        "writer",
        "patch",
        json!({"id":parent,"expect":2,"status":"resolved","over_objection":"Owner accepted the risk"}),
    );
    drop(s);
    let mut s = Store::open(&dir.join("state.db"), false).unwrap();
    call(&mut s, "objector", "join", json!({"topics":[]}));
    call(&mut s, "objector", "ack", json!({"receipts":[old]}));
    let inbox = call(&mut s, "objector", "inbox", json!({"selection":"involved"}));
    assert!(inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["card"]["title"] == format!("Resolved over objection: #{parent}")));
    assert!(s
        .conn
        .query_row(
            "SELECT pending_seq>ack_seq FROM deliveries WHERE agent='owner' AND card_id=?",
            [parent],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
    let shown = call(
        &mut s,
        "objector",
        "show",
        json!({"id":parent,"history":true,"compact":true}),
    );
    assert_eq!(
        shown["objection_override"]["reason"],
        "Owner accepted the risk"
    );
    assert_eq!(shown["objections"]["items"][0]["status"], "open");
    call(
        &mut s,
        "objector",
        "patch",
        json!({"id":child,"expect":1,"status":"resolved"}),
    );
    let shown = call(&mut s, "writer", "show", json!({"id":parent}));
    assert_eq!(shown["objections"]["items"][0]["resolved_by"], "objector");
    assert_eq!(shown["objections"]["items"][0]["status"], "resolved");
    drop(s);
    std::fs::remove_dir_all(dir).unwrap();
}
#[test]
fn objection_presentation_fits_the_minimum_brief_and_attention_budgets() {
    let (mut s, parent, _) = board();
    for _ in 0..10 {
        call(
            &mut s,
            "objector",
            "annotate",
            json!({"id":parent,"kind":"objection","body":"More missing evidence"}),
        );
    }
    let brief = call(&mut s, "writer", "brief", json!({"budget":2000}));
    assert!(serde_json::to_vec(&brief).unwrap().len() <= 2000);
    let options = fray::attention::Options::parse(&json!({"budget":2000})).unwrap();
    let packet = s
        .wake_packet("writer", &options, &std::collections::HashMap::new(), NOW)
        .unwrap();
    assert!(serde_json::to_vec(&packet).unwrap().len() < 2100);
}
