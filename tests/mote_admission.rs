use fray::{model::Request, mote, store::Store};
use serde_json::{json, Value};
#[test]
fn public_admission_feed_compatibility_court() {
    let out = std::process::Command::new("python3")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/scripts/mote_admission_integration.py"
        ))
        .arg(env!("CARGO_BIN_EXE_fray"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let result: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(result["passed"], 6);
}
fn call(s: &mut Store, actor: &str, op: &str, args: Value) -> Value {
    s.execute_at(&Request::new(op, actor, args), 1000).unwrap()
}
fn fixture() -> Store {
    let mut s = Store::memory().unwrap();
    for name in ["alice", "bob", "carol"] {
        call(&mut s, name, "join", json!({"topics":[]}));
    }
    call(
        &mut s,
        "alice",
        "mote_bind",
        json!({"store":"/tmp/.mote","store_id":"st-test","cursor_mode":"admission_v1","genesis_digest":"genesis"}),
    );
    s
}
fn ingest(s: &mut Store, revision: i64, after: Value, cursor: Value, claims: Value) -> Value {
    call(
        s,
        "alice",
        "mote_ingest",
        json!({"store_id":"st-test","sync_revision":revision,"after":after,"cursor":cursor,"items":[],"claims":claims}),
    )
}
#[test]
fn admission_cursor_accepts_earlier_spelling_and_deduplicates_claim_identity() {
    let mut s = fixture();
    let first = ingest(
        &mut s,
        1,
        Value::Null,
        json!("z"),
        json!([{"entity":"work","to":"alice","by":"alice","op_id":"z","seed":true}]),
    );
    assert_eq!(first["sync_revision"], 2);
    let moved = ingest(
        &mut s,
        2,
        json!("z"),
        json!("a"),
        json!([{"entity":"work","to":"bob","by":"alice","op_id":"a"}]),
    );
    assert_eq!(moved["created"].as_array().unwrap().len(), 1);
    let repeat = ingest(
        &mut s,
        3,
        json!("a"),
        json!("a"),
        json!([{"entity":"work","to":"alice","by":"alice","op_id":"z","seed":true},{"entity":"work","to":"bob","by":"alice","op_id":"a"}]),
    );
    assert!(repeat["created"].as_array().unwrap().is_empty());
    let stale = s
        .execute_at(
            &Request::new(
                "mote_ingest",
                "alice",
                json!({"store_id":"st-test","sync_revision":3,"after":"a","cursor":"a","items":[]}),
            ),
            1000,
        )
        .unwrap_err();
    assert_eq!(stale.code, "mote_cursor_moved");
}
#[test]
fn empty_raw_baseline_is_initialized_and_projected_only_ingest_preserves_anchor() {
    let mut s = fixture();
    ingest(&mut s, 1, Value::Null, Value::Null, json!([]));
    let binding = call(&mut s, "alice", "mote_binding", json!({}))["binding"].clone();
    assert_eq!(binding["cursor_initialized"], true);
    assert!(binding["cursor"].is_null());
    ingest(&mut s, 2, Value::Null, json!("future-raw"), json!([]));
    ingest(
        &mut s,
        3,
        json!("future-raw"),
        json!("future-raw"),
        json!([]),
    );
    let binding = call(&mut s, "alice", "mote_binding", json!({}))["binding"].clone();
    assert_eq!(binding["cursor"], "future-raw");
}
#[test]
fn migration_invalidates_legacy_ingest_and_genesis_cannot_change_silently() {
    let mut s = Store::memory().unwrap();
    call(&mut s, "alice", "join", json!({}));
    call(
        &mut s,
        "alice",
        "mote_bind",
        json!({"store":"/tmp/.mote","store_id":"st-test"}),
    );
    call(
        &mut s,
        "alice",
        "mote_ingest",
        json!({"store_id":"st-test","after":null,"cursor":"legacy","items":[]}),
    );
    call(
        &mut s,
        "alice",
        "mote_bind",
        json!({"store":"/tmp/.mote","store_id":"st-test","cursor_mode":"admission_v1","genesis_digest":"one"}),
    );
    let old = s
        .execute_at(
            &Request::new(
                "mote_ingest",
                "alice",
                json!({"store_id":"st-test","after":null,"cursor":"z","items":[]}),
            ),
            1000,
        )
        .unwrap_err();
    assert_eq!(old.code, "mote_cursor_moved");
    let changed=s.execute_at(&Request::new("mote_bind","alice",json!({"store":"/tmp/.mote","store_id":"st-test","cursor_mode":"admission_v1","genesis_digest":"two"})),1000).unwrap_err();
    assert_eq!(changed.code, "mote_store_mismatch");
}
#[test]
fn current_snapshot_ahead_of_feed_does_not_invent_reverse_handoffs() {
    let mut s = fixture();
    ingest(
        &mut s,
        1,
        Value::Null,
        json!("first"),
        json!([{"entity":"work","to":"alice","by":"alice","op_id":"first","seed":true}]),
    );
    call(
        &mut s,
        "alice",
        "mote_ingest",
        json!({"store_id":"st-test","sync_revision":2,"after":"first","cursor":"first","items":[],"reconcile":[{"entity":"work","expect":"alice","holder":"carol","lease_until":"later","marker":"snapshot-ahead"}]}),
    );
    let feed = ingest(
        &mut s,
        3,
        json!("first"),
        json!("second"),
        json!([{"entity":"work","to":"bob","by":"alice","op_id":"second"}]),
    );
    assert_eq!(feed["created"].as_array().unwrap().len(), 1);
    let carol = call(&mut s, "carol", "inbox", json!({}));
    assert!(!carol["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["card"]["title"]
            .as_str()
            .unwrap_or("")
            .contains("now bob")));
}
#[test]
fn holder_checked_handoff_event_is_a_claim_transition() {
    let events = json!([{"type":"claim.transferred","actor":"alice","event_id":"a","op_id":"a","data":{"entity":"work","to":"bob","expect_holder":"alice"}}]);
    let transitions = mote::claim_transitions(events.as_array().unwrap());
    assert_eq!(transitions.len(), 1);
    assert_eq!(transitions[0]["to"], "bob");
}

fn transferred(op: &str, entity: &str, to: &str) -> Value {
    json!({"type":"claim.transferred","actor":"alice","event_id":op,"op_id":op,"data":{"entity":entity,"to":to}})
}

#[test]
fn claim_coverage_uses_admission_identity_and_rejects_unseen_cycles() {
    let e = transferred("z", "work", "bob");
    let late = transferred("a", "work", "carol");
    assert_eq!(
        mote::claim_coverage(&[e.clone(), late.clone()], &[e.clone(), late.clone()])["work"],
        "a"
    );
    let other = transferred("other", "different-work", "carol");
    assert_eq!(
        mote::claim_coverage(std::slice::from_ref(&e), &[e.clone(), other])["work"],
        "z"
    );
    for verified in [
        vec![],
        vec![late.clone()],
        vec![e.clone(), late, transferred("b", "work", "bob")],
        vec![
            e.clone(),
            json!({"type":"issue.closed","event_id":"close","op_id":"close","data":{"entity":"work"}}),
            json!({"type":"issue.patched","event_id":"open","op_id":"open","data":{"entity":"work"}}),
        ],
        vec![
            e.clone(),
            json!({"type":"claim.unknown","event_id":"unknown","op_id":"unknown","data":{"entity":"work"}}),
        ],
        vec![
            e.clone(),
            json!({"type":"claim.transferred","event_id":"bad","op_id":"bad","data":{}}),
        ],
    ] {
        assert!(
            mote::claim_coverage(std::slice::from_ref(&e), &verified).is_empty(),
            "{verified:?}"
        );
    }
}

fn handoff_feed(s: &mut Store) {
    ingest(
        s,
        1,
        Value::Null,
        json!("z"),
        json!([{"entity":"work","to":"alice","by":"alice","op_id":"z","seed":true}]),
    );
    ingest(
        s,
        2,
        json!("z"),
        json!("a"),
        json!([{"entity":"work","to":"bob","by":"alice","op_id":"a"}]),
    );
}

#[test]
fn verified_handoff_reconciles_snapshot_without_duplicate_attention() {
    let mut s = fixture();
    handoff_feed(&mut s);
    let result = call(
        &mut s,
        "alice",
        "mote_ingest",
        json!({"store_id":"st-test","sync_revision":3,"after":"a","cursor":"a","items":[],"reconcile":[{"entity":"work","expect":"alice","holder":"bob","marker":"a","covered_by_feed":{"op_id":"a","cursor":"a"}}]}),
    );
    assert!(result["created"].as_array().unwrap().is_empty());
    assert_eq!(
        call(
            &mut s,
            "alice",
            "mote_claims",
            json!({"store_id":"st-test"})
        )["holders"]["work"],
        "bob"
    );
    let bob = call(&mut s, "bob", "inbox", json!({}));
    assert_eq!(bob["items"].as_array().unwrap().len(), 1);
    assert_eq!(
        bob["items"][0]["card"]["title"],
        "Mote: alice handed you work"
    );
}

#[test]
fn stale_coverage_or_missing_recipient_notice_keeps_normal_reconciliation() {
    for proof in [
        json!({"op_id":"z","cursor":"a"}),
        json!({"op_id":"a","cursor":"old"}),
    ] {
        let mut s = fixture();
        handoff_feed(&mut s);
        let result = call(
            &mut s,
            "alice",
            "mote_ingest",
            json!({"store_id":"st-test","sync_revision":3,"after":"a","cursor":"a","items":[],"reconcile":[{"entity":"work","expect":"alice","holder":"bob","marker":"snapshot","covered_by_feed":proof}]}),
        );
        assert_eq!(result["created"].as_array().unwrap().len(), 2);
    }
    // dave was not on the board when the feed saw the handoff, so no card
    // reached him; once he joins, reconciliation still tells him.
    let mut s = fixture();
    let fed = ingest(
        &mut s,
        1,
        Value::Null,
        json!("gift"),
        json!([{"entity":"work","to":"dave","by":"alice","op_id":"gift"}]),
    );
    assert_eq!(fed["unknown_recipients"], json!(["dave"]));
    call(&mut s, "dave", "join", json!({"topics":[]}));
    let result = call(
        &mut s,
        "alice",
        "mote_ingest",
        json!({"store_id":"st-test","sync_revision":2,"after":"gift","cursor":"gift","items":[],"reconcile":[{"entity":"work","expect":null,"holder":"dave","marker":"snapshot","covered_by_feed":{"op_id":"gift","cursor":"gift"}}]}),
    );
    assert_eq!(result["created"].as_array().unwrap().len(), 1);
}

#[test]
fn a_holders_own_claim_seen_by_the_feed_is_not_reconciled_back_to_them() {
    let mut s = fixture();
    let fed = ingest(
        &mut s,
        1,
        Value::Null,
        json!("self"),
        json!([{"entity":"work","to":"bob","by":"bob","op_id":"self"}]),
    );
    assert!(fed["created"].as_array().unwrap().is_empty());
    let result = call(
        &mut s,
        "alice",
        "mote_ingest",
        json!({"store_id":"st-test","sync_revision":2,"after":"self","cursor":"self","items":[],"reconcile":[{"entity":"work","expect":null,"holder":"bob","marker":"snapshot","covered_by_feed":{"op_id":"self","cursor":"self"}}]}),
    );
    assert!(result["created"].as_array().unwrap().is_empty(), "{result}");
    assert_eq!(
        call(
            &mut s,
            "alice",
            "mote_claims",
            json!({"store_id":"st-test"})
        )["holders"]["work"],
        "bob"
    );
    // Without the feed's proof it is ordinary reconciliation, which tells bob.
    let mut s = fixture();
    ingest(
        &mut s,
        1,
        Value::Null,
        json!("self"),
        json!([{"entity":"work","to":"bob","by":"bob","op_id":"self"}]),
    );
    let result = call(
        &mut s,
        "alice",
        "mote_ingest",
        json!({"store_id":"st-test","sync_revision":2,"after":"self","cursor":"self","items":[],"reconcile":[{"entity":"work","expect":null,"holder":"bob","marker":"snapshot"}]}),
    );
    assert_eq!(result["created"].as_array().unwrap().len(), 1);
}
