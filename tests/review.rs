use fray::{model::Request, store::Store};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000_000;
fn run(s: &mut Store, actor: &str, op: &str, args: Value) -> fray::model::Result<Value> {
    s.execute_at(&Request::new(op, actor, args), NOW)
}
fn call(s: &mut Store, actor: &str, op: &str, args: Value) -> Value {
    run(s, actor, op, args).unwrap()
}
fn version(c: char) -> String {
    format!("manifest:{}", c.to_string().repeat(64))
}
fn board() -> (Store, Value) {
    let mut s = Store::memory().unwrap();
    for actor in ["writer", "reader"] {
        call(&mut s, actor, "join", json!({"topics":[]}));
    }
    let request = call(
        &mut s,
        "writer",
        "review_request",
        json!({
            "to":"reader","title":"Parser evidence","body":"Check the parser and its tests",
            "baseline":version('a'),"candidate":version('b'),"mote_ref":"mote:parser-1"
        }),
    );
    (s, request["card"]["id"].clone())
}
fn verdict(s: &mut Store, id: &Value, round: i64, hash: char, choice: &str) -> Value {
    call(
        s,
        "reader",
        "annotate",
        json!({"id":id,"body":"Independent evidence",
        "kind":match choice {"object"=>"objection","blocked"=>"question",_=>"evidence"},
        "review_verdict":{"verdict":choice,"at":version(hash),"expect":round}}),
    )
}

#[test]
fn candidate_supersession_preserves_baseline_and_stales_even_aba_verdicts() {
    let (mut s, id) = board();
    let first = verdict(&mut s, &id, 1, 'b', "approve");
    assert_eq!(first["card"]["status"], "open");
    assert_eq!(first["review"]["latest_verdict"]["stale"], false);
    for (round, hash) in [(1, 'c'), (2, 'b')] {
        let moved = call(
            &mut s,
            "writer",
            "review_subject",
            json!({"id":id,"expect":round,"at":version(hash)}),
        );
        assert_eq!(moved["review"]["baseline"], version('a'));
        assert_eq!(moved["review"]["subject_rev"], round + 1);
        assert_eq!(moved["review"]["latest_verdict"]["stale"], true);
    }
    let shown = call(
        &mut s,
        "reader",
        "show",
        json!({"id":id,"history":true,"compact":true}),
    );
    assert_eq!(shown["review"]["mote_ref"], "mote:parser-1");
    assert!(shown["history"]
        .as_array()
        .unwrap()
        .iter()
        .any(|e| e["review"]["version"] == version('b') && e["review"]["subject_rev"] == 1));
    let inbox = call(&mut s, "reader", "inbox", json!({}));
    assert_eq!(inbox["items"][0]["card"]["review"]["subject_rev"], 3);
    assert_eq!(
        inbox["items"][0]["card"]["review"]["latest_verdict"]["stale"],
        true
    );
    let final_verdict = verdict(&mut s, &id, 3, 'b', "approve");
    assert_eq!(final_verdict["review"]["verdicts"][0]["stale"], false);
    assert_eq!(final_verdict["review"]["verdicts"][1]["stale"], true);
    assert_eq!(final_verdict["card"]["status"], "open");
}

#[test]
fn stale_verdict_or_invalid_receipt_writes_neither_evidence_nor_ack() {
    let (mut s, id) = board();
    let inbox = call(&mut s, "reader", "inbox", json!({}));
    let batch = call(
        &mut s,
        "reader",
        "present",
        json!({"source":"thread","receipts":[inbox["items"][0]["receipt"]]}),
    )["batch"]["id"]
        .clone();
    call(
        &mut s,
        "writer",
        "review_subject",
        json!({"id":id,"expect":1,"at":version('c')}),
    );
    let before = call(&mut s, "reader", "show", json!({"id":id,"history":true}));
    for (at, expect, ack, code) in [
        (version('b'), 1, batch.clone(), "conflict"),
        (version('c'), 1, batch.clone(), "conflict"),
        (version('b'), 2, batch.clone(), "conflict"),
        (version('c'), 2, json!("0".repeat(32)), "batch_unknown"),
    ] {
        let error=run(&mut s,"reader","annotate",json!({"id":id,"body":"Claimed check","kind":"objection","ack_batch":ack,"review_verdict":{"verdict":"object","at":at,"expect":expect}})).unwrap_err();
        assert_eq!(error.code, code);
        assert_eq!(
            call(&mut s, "reader", "show", json!({"id":id,"history":true})),
            before
        );
        assert_eq!(
            call(&mut s, "reader", "batch", json!({"batch":batch}))["items"][0]["handled"],
            false
        );
    }
    assert_eq!(
        s.conn
            .query_row("SELECT count(*) FROM review_verdicts", [], |r| r
                .get::<_, i64>(0))
            .unwrap(),
        0
    );
}

#[test]
fn review_objection_uses_existing_linked_closure_gate() {
    let (mut s, id) = board();
    let objection = verdict(&mut s, &id, 1, 'b', "object");
    assert_eq!(objection["follow_up"]["assignee"], "writer");
    let head = call(&mut s, "writer", "show", json!({"id":id}))["card"].clone();
    assert_eq!(
        run(
            &mut s,
            "writer",
            "patch",
            json!({"id":id,"expect":head["rev"],"status":"resolved"})
        )
        .unwrap_err()
        .code,
        "open_objections"
    );
    call(
        &mut s,
        "writer",
        "review_subject",
        json!({"id":id,"expect":1,"at":version('c')}),
    );
    let approved = verdict(&mut s, &id, 2, 'c', "approve");
    assert_eq!(approved["card"]["status"], "open");
    let child = call(
        &mut s,
        "writer",
        "show",
        json!({"id":objection["follow_up"]["id"]}),
    );
    assert_eq!(
        child["card"]["status"], "open",
        "a new candidate or approval must not close old objections"
    );
    let blocked = verdict(&mut s, &id, 2, 'c', "blocked");
    assert!(blocked["follow_up"]["id"].is_number());
}

#[test]
fn frozen_scope_authority_and_cas_are_enforced_without_freezing_routing() {
    let (mut s, id) = board();
    for field in ["title", "summary", "kind"] {
        let mut args = json!({"id":id,"expect":1});
        args[field] = json!(if field == "kind" { "note" } else { "New scope" });
        assert_eq!(
            run(&mut s, "writer", "patch", args).unwrap_err().code,
            "review_scope_frozen"
        );
    }
    assert_eq!(
        run(
            &mut s,
            "reader",
            "review_subject",
            json!({"id":id,"expect":1,"at":version('c')})
        )
        .unwrap_err()
        .code,
        "not_author"
    );
    assert_eq!(
        run(
            &mut s,
            "writer",
            "review_subject",
            json!({"id":id,"expect":0,"at":version('c')})
        )
        .unwrap_err()
        .code,
        "conflict"
    );
    assert_eq!(run(&mut s,"writer","annotate",json!({"id":id,"body":"Self check","kind":"evidence","review_verdict":{"verdict":"approve","at":version('b'),"expect":1}})).unwrap_err().code,"self_review");
    call(
        &mut s,
        "writer",
        "patch",
        json!({"id":id,"expect":1,"priority":1}),
    );
    // Subject revision is independent of the card revision changed by routing.
    assert_eq!(
        call(
            &mut s,
            "writer",
            "review_subject",
            json!({"id":id,"expect":1,"at":version('c')})
        )["review"]["subject_rev"],
        2
    );
}

#[test]
fn mutable_references_are_rejected_and_unicode_evidence_roundtrips() {
    let (mut s, _) = board();
    for value in [
        "HEAD",
        "git:abc",
        "git:main",
        "manifest:1234",
        &format!("git:{}+dirty", "a".repeat(40)),
        &format!("git:{}", "A".repeat(40)),
    ] {
        assert!(fray::review::version(value).is_err());
    }
    for value in [
        format!("git:{}", "a".repeat(40)),
        format!("git:{}", "b".repeat(64)),
        version('c'),
    ] {
        assert!(fray::review::version(&value).is_ok());
    }
    let body = "é🧠".repeat(1100);
    let request = call(
        &mut s,
        "writer",
        "review_request",
        json!({"to":"reader","title":"Unicode","body":body,"baseline":version('a'),"candidate":version('b')}),
    );
    assert!(request["card"]["summary"].as_str().unwrap().len() <= 2000);
    let shown = call(
        &mut s,
        "reader",
        "show",
        json!({"id":request["card"]["id"],"history":true,"compact":true}),
    );
    assert_eq!(shown["history"][0]["body"], body);
}

#[test]
fn schema_two_upgrade_preserves_identity_receipts_and_bound_peer_generation() {
    let dir = std::env::temp_dir().join(format!(
        "fray-review-migrate-{}",
        fray::model::random_key().unwrap()
    ));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("state.db");
    let identity;
    {
        let mut s = Store::open(&path, false).unwrap();
        s.execute_at(
            &Request::new("join", "writer", json!({})).with_session(Some("writer:old".into())),
            NOW,
        )
        .unwrap();
        call(&mut s, "reader", "join", json!({}));
        let c = call(
            &mut s,
            "writer",
            "send",
            json!({"to":"reader","body":"Existing evidence"}),
        );
        call(
            &mut s,
            "reader",
            "ack",
            json!({"id":c["card"]["id"],"through":c["event_seq"]}),
        );
        identity = s.identity().unwrap();
        s.conn.execute_batch("DROP TABLE review_verdicts; DROP TABLE review_subjects; DROP TABLE peer_seen; DROP TABLE peer_generations; PRAGMA user_version=2;").unwrap();
    }
    {
        let mut s = Store::open(&path, false).unwrap();
        assert_eq!(s.identity().unwrap(), identity);
        assert_eq!(
            s.conn
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            3
        );
        assert_eq!(call(&mut s, "reader", "inbox", json!({}))["total"], 0);
        s.execute_at(
            &Request::new("heartbeat", "writer", json!({})).with_session(Some("writer:old".into())),
            NOW + 1,
        )
        .unwrap();
        assert_eq!(
            call(&mut s, "reader", "peers", json!({}))["peers"][0]["generation"],
            1
        );
        assert_eq!(
            s.conn
                .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
        assert_eq!(
            s.conn
                .query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |r| r
                    .get::<_, i64>(
                    0
                ))
                .unwrap(),
            0
        );
        s.conn.execute_batch("PRAGMA user_version=4").unwrap();
    }
    assert_eq!(
        Store::open(&path, false).err().unwrap().code,
        "schema_version"
    );
    std::fs::remove_dir_all(dir).unwrap();
}
