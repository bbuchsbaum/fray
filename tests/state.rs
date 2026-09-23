use fray::{model::Request, store::Store};
use serde_json::{json, Value};
const NOW: i64 = 1_800_000_000_000;
fn call(s: &mut Store, who: &str, op: &str, args: Value) -> Value {
    s.execute_at(&Request::new(op, who, args), NOW).unwrap()
}
fn setup() -> Store {
    let mut s = Store::memory().unwrap();
    for who in ["alice", "bob"] {
        call(&mut s, who, "join", json!({}));
    }
    s
}
fn post(s: &mut Store) -> Value {
    call(
        s,
        "alice",
        "post",
        json!({"kind":"task","topic":"parser","title":"Repair parser","summary":"Handle escaped Unicode safely"}),
    )
}
#[test]
fn post_updates_head_event_search_and_inbox() {
    let mut s = setup();
    let r = post(&mut s);
    assert_eq!(r["card"]["rev"], 1);
    assert_eq!(call(&mut s, "bob", "inbox", json!({}))["total"], 1);
    assert_eq!(
        call(&mut s, "alice", "query", json!({"q":"Unicode"}))["total"],
        1
    );
    assert_eq!(s.events(0, 100).unwrap().len(), 1);
}
#[test]
fn explicit_ack_does_not_resolve_task() {
    let mut s = setup();
    let r = post(&mut s);
    let id = r["card"]["id"].clone();
    call(
        &mut s,
        "bob",
        "ack",
        json!({"id":id,"through":r["event_seq"]}),
    );
    assert_eq!(call(&mut s, "bob", "inbox", json!({}))["total"], 0);
    assert_eq!(
        call(&mut s, "bob", "show", json!({"id":id}))["card"]["status"],
        "open"
    );
}
#[test]
fn reading_does_not_ack() {
    let mut s = setup();
    post(&mut s);
    for _ in 0..3 {
        assert_eq!(call(&mut s, "bob", "inbox", json!({}))["total"], 1);
    }
}
#[test]
fn stale_ack_cannot_swallow_new_revision() {
    let mut s = setup();
    let r = post(&mut s);
    call(
        &mut s,
        "alice",
        "patch",
        json!({"id":r["card"]["id"],"expect":1,"summary":"New information"}),
    );
    let a = call(
        &mut s,
        "bob",
        "ack",
        json!({"id":r["card"]["id"],"through":r["event_seq"]}),
    );
    assert_eq!(a["still_pending"], true);
    assert_eq!(call(&mut s, "bob", "inbox", json!({}))["total"], 1);
}
#[test]
fn reject_future_ack() {
    let mut s = setup();
    post(&mut s);
    assert!(s
        .execute_at(
            &Request::new("ack", "bob", json!({"id":1,"through":999})),
            NOW
        )
        .is_err());
}
#[test]
fn repeated_updates_coalesce() {
    let mut s = setup();
    post(&mut s);
    for rev in 1..101 {
        call(
            &mut s,
            "alice",
            "patch",
            json!({"id":1,"expect":rev,"summary":format!("head {rev}")}),
        );
    }
    let page = call(&mut s, "bob", "inbox", json!({}));
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["card"]["rev"], 101);
    assert_eq!(s.events(0, 1000).unwrap().len(), 101);
}
#[test]
fn same_revision_only_one_writer_wins() {
    let mut s = setup();
    post(&mut s);
    call(
        &mut s,
        "alice",
        "patch",
        json!({"id":1,"expect":1,"summary":"winner"}),
    );
    let e = s
        .execute_at(
            &Request::new("patch", "bob", json!({"id":1,"expect":1,"summary":"loser"})),
            NOW,
        )
        .unwrap_err();
    assert_eq!(e.code, "conflict");
    assert_eq!(
        call(&mut s, "bob", "show", json!({"id":1}))["card"]["summary"],
        "winner"
    );
}
#[test]
fn annotation_does_not_invalidate_content_revision() {
    let mut s = setup();
    post(&mut s);
    call(
        &mut s,
        "bob",
        "annotate",
        json!({"id":1,"body":"Evidence","kind":"evidence"}),
    );
    assert_eq!(
        call(&mut s, "alice", "show", json!({"id":1}))["card"]["rev"],
        1
    );
    call(
        &mut s,
        "alice",
        "patch",
        json!({"id":1,"expect":1,"summary":"Revised conclusion"}),
    );
}
#[test]
fn actionable_annotations_create_persistent_questions() {
    let mut s = setup();
    post(&mut s);
    let r = call(
        &mut s,
        "bob",
        "annotate",
        json!({"id":1,"body":"This races with cancellation","kind":"objection"}),
    );
    assert_eq!(r["follow_up"]["kind"], "question");
    assert_eq!(r["follow_up"]["assignee"], "alice");
    assert_eq!(
        call(&mut s, "alice", "query", json!({"tag":"parent:1"}))["total"],
        1
    );
    assert_eq!(s.events(0, 100).unwrap().len(), 3);
}
#[test]
fn lease_exclusion() {
    let mut s = setup();
    post(&mut s);
    call(&mut s, "alice", "claim", json!({"id":1,"ttl":10}));
    let e = s
        .execute_at(&Request::new("claim", "bob", json!({"id":1})), NOW)
        .unwrap_err();
    assert_eq!(e.code, "claimed");
}
#[test]
fn expired_claim_is_reclaimable_and_fenced() {
    let mut s = setup();
    post(&mut s);
    let old = call(&mut s, "alice", "claim", json!({"id":1,"ttl":1}));
    let new = s
        .execute_at(
            &Request::new("claim", "bob", json!({"id":1,"ttl":30})),
            NOW + 1001,
        )
        .unwrap();
    assert!(new["card"]["fence"].as_i64().unwrap() > old["card"]["fence"].as_i64().unwrap());
    let e=s.execute_at(&Request::new("patch","alice",json!({"id":1,"expect":new["card"]["rev"],"fence":old["card"]["fence"],"summary":"stale write"})),NOW+1002).unwrap_err();
    assert_eq!(e.code, "lease_lost");
}
#[test]
fn expired_owner_cannot_renew() {
    let mut s = setup();
    post(&mut s);
    let r = call(&mut s, "alice", "claim", json!({"id":1,"ttl":1}));
    assert_eq!(
        s.execute_at(
            &Request::new("renew", "alice", json!({"id":1,"fence":r["card"]["fence"]})),
            NOW + 1000
        )
        .unwrap_err()
        .code,
        "lease_lost"
    );
}
#[test]
fn renew_keeps_content_revision() {
    let mut s = setup();
    post(&mut s);
    let r = call(&mut s, "alice", "claim", json!({"id":1,"ttl":60}));
    let n = call(
        &mut s,
        "alice",
        "renew",
        json!({"id":1,"fence":r["card"]["fence"],"ttl":120}),
    );
    assert_eq!(n["card"]["rev"], r["card"]["rev"]);
}
#[test]
fn renew_is_not_attention_spam() {
    let mut s = setup();
    post(&mut s);
    let r = call(&mut s, "alice", "claim", json!({"id":1}));
    call(
        &mut s,
        "bob",
        "ack",
        json!({"id":1,"through":r["event_seq"]}),
    );
    call(
        &mut s,
        "alice",
        "renew",
        json!({"id":1,"fence":r["card"]["fence"]}),
    );
    assert_eq!(call(&mut s, "bob", "inbox", json!({}))["total"], 0);
}
#[test]
fn resolve_releases_lease() {
    let mut s = setup();
    post(&mut s);
    let r = call(&mut s, "alice", "claim", json!({"id":1}));
    let closed = call(
        &mut s,
        "alice",
        "patch",
        json!({"id":1,"expect":r["card"]["rev"],"fence":r["card"]["fence"],"status":"resolved","summary":"Tests pass"}),
    );
    assert_eq!(closed["card"]["lease_owner"], Value::Null);
}
#[test]
fn idempotent_retry_is_one_event() {
    let mut s = setup();
    let mut r = Request::new(
        "post",
        "alice",
        json!({"title":"Retry","summary":"same request"}),
    );
    r.key = Some("stable-key".into());
    let a = s.execute_at(&r, NOW).unwrap();
    let b = s.execute_at(&r, NOW + 1).unwrap();
    assert_eq!(a, b);
    assert_eq!(s.events(0, 100).unwrap().len(), 1);
}
#[test]
fn key_reuse_with_different_payload_is_error() {
    let mut s = setup();
    let mut r = Request::new(
        "post",
        "alice",
        json!({"title":"Retry","summary":"same request"}),
    );
    r.key = Some("stable-key".into());
    s.execute_at(&r, NOW).unwrap();
    r.args["summary"] = json!("changed");
    assert_eq!(
        s.execute_at(&r, NOW).unwrap_err().code,
        "idempotency_conflict"
    );
}
#[test]
fn failed_mutation_leaves_no_event_or_delivery() {
    let mut s = setup();
    let before = s.highwater().unwrap();
    let e = s.execute_at(
        &Request::new(
            "post",
            "alice",
            json!({"title":"Bad","summary":"bad","assignee":"typo"}),
        ),
        NOW,
    );
    assert!(e.is_err());
    assert_eq!(s.highwater().unwrap(), before);
}
#[test]
fn current_search_does_not_return_old_head() {
    let mut s = setup();
    post(&mut s);
    call(
        &mut s,
        "alice",
        "patch",
        json!({"id":1,"expect":1,"summary":"Latest normalized head"}),
    );
    assert_eq!(
        call(&mut s, "alice", "query", json!({"q":"Unicode"}))["total"],
        0
    );
    assert!(
        !call(&mut s, "alice", "search_history", json!({"q":"Unicode"}))["items"]
            .as_array()
            .unwrap()
            .is_empty()
    );
}
#[test]
fn new_agent_gets_live_heads_not_closed_history() {
    let mut s = setup();
    for _ in 0..100 {
        let r = post(&mut s);
        call(
            &mut s,
            "alice",
            "patch",
            json!({"id":r["card"]["id"],"expect":1,"status":"resolved"}),
        );
    }
    post(&mut s);
    let b = call(&mut s, "newcomer", "join", json!({}));
    assert_eq!(b["attention"]["total"], 1);
    assert_eq!(b["available"]["total"], 1);
}
#[test]
fn briefing_honors_byte_budget_and_signals_omissions() {
    let mut s = setup();
    for n in 0..30 {
        call(
            &mut s,
            "alice",
            "post",
            json!({"kind":"task","title":format!("task {n}"),"summary":"🧠".repeat(400)}),
        );
    }
    let b = call(&mut s, "bob", "brief", json!({"budget":2000}));
    assert!(serde_json::to_vec(&b).unwrap().len() <= 2000);
    assert_eq!(b["budget_truncated"], true);
    assert_eq!(b["attention"]["more"], true);
}
#[test]
fn filters_and_sort_are_structured() {
    let mut s = setup();
    post(&mut s);
    call(
        &mut s,
        "alice",
        "post",
        json!({"title":"Urgent","summary":"x","priority":0,"topic":"other","tags":["fast"]}),
    );
    assert_eq!(
        call(&mut s, "bob", "query", json!({"sort":"priority"}))["items"][0]["title"],
        "Urgent"
    );
    assert_eq!(
        call(
            &mut s,
            "bob",
            "query",
            json!({"topic":"parser","kind":"task"})
        )["total"],
        1
    );
    assert_eq!(
        call(&mut s, "bob", "query", json!({"tag":"fast"}))["total"],
        1
    );
}
#[test]
fn unknown_mutation_fields_fail_closed() {
    let mut s = setup();
    assert!(s
        .execute_at(
            &Request::new(
                "post",
                "alice",
                json!({"title":"x","summary":"x","priorty":0})
            ),
            NOW
        )
        .is_err());
}
#[test]
fn unassigned_new_work_has_global_discovery() {
    let mut s = setup();
    call(&mut s, "carol", "join", json!({"topics":["unrelated"]}));
    post(&mut s);
    assert_eq!(call(&mut s, "carol", "inbox", json!({}))["total"], 1);
}
#[test]
fn scoped_notes_are_filtered_but_broadcasts_are_not() {
    let mut s = setup();
    call(&mut s, "carol", "join", json!({"topics":["unrelated"]}));
    call(
        &mut s,
        "alice",
        "post",
        json!({"title":"Private scope not secrecy","summary":"x","topic":"parser"}),
    );
    assert_eq!(call(&mut s, "carol", "inbox", json!({}))["total"], 0);
    call(
        &mut s,
        "alice",
        "post",
        json!({"title":"Broadcast","summary":"x","topic":"*"}),
    );
    assert_eq!(call(&mut s, "carol", "inbox", json!({}))["total"], 1);
}
#[test]
fn exposing_does_not_ack_and_reminders_return() {
    let mut s = setup();
    post(&mut s);
    call(
        &mut s,
        "bob",
        "expose",
        json!({"receipts":[{"id":1,"through":1}]}),
    );
    assert_eq!(
        call(&mut s, "bob", "inbox", json!({"fresh":true}))["total"],
        0
    );
    assert_eq!(call(&mut s, "bob", "inbox", json!({}))["total"], 1);
    assert_eq!(
        s.execute_at(
            &Request::new("inbox", "bob", json!({"fresh":true})),
            NOW + 60001
        )
        .unwrap()["total"],
        1
    );
}
#[test]
fn history_pagination_has_no_duplicate_sequences() {
    let mut s = setup();
    post(&mut s);
    for _ in 0..5 {
        call(&mut s, "bob", "annotate", json!({"id":1,"body":"detail"}));
    }
    let one = call(
        &mut s,
        "alice",
        "show",
        json!({"id":1,"history":true,"limit":2}),
    );
    assert_eq!(one["more"], true);
    let two = call(
        &mut s,
        "alice",
        "show",
        json!({"id":1,"history":true,"limit":2,"after":one["next_after"]}),
    );
    assert!(
        two["history"][0]["seq"].as_i64().unwrap() > one["history"][1]["seq"].as_i64().unwrap()
    );
}
#[test]
fn annotations_never_silently_disappear_from_counts() {
    let mut s = setup();
    post(&mut s);
    for _ in 0..5 {
        call(
            &mut s,
            "alice",
            "annotate",
            json!({"id":1,"body":"Evidence"}),
        );
    }
    let p = call(&mut s, "bob", "inbox", json!({}));
    assert_eq!(p["items"][0]["annotation_count"], 5);
    assert_eq!(p["items"][0]["annotations_omitted"], 3);
}
#[test]
fn blocked_tasks_are_not_suggested_as_available() {
    let mut s = setup();
    post(&mut s);
    call(
        &mut s,
        "alice",
        "patch",
        json!({"id":1,"expect":1,"status":"blocked","summary":"Awaiting external evidence"}),
    );
    let b = call(&mut s, "bob", "brief", json!({}));
    assert_eq!(b["available"]["total"], 0);
    assert_eq!(b["blockers"]["total"], 1);
}
#[test]
fn scope_changes_seed_new_live_heads() {
    let mut s = setup();
    call(&mut s, "carol", "join", json!({"topics":["other"]}));
    call(
        &mut s,
        "alice",
        "post",
        json!({"title":"Decision","summary":"x","kind":"decision","topic":"parser"}),
    );
    assert_eq!(call(&mut s, "carol", "inbox", json!({}))["total"], 0);
    assert_eq!(
        call(&mut s, "carol", "join", json!({"topics":["parser"]}))["attention"]["total"],
        1
    );
}
#[test]
fn receipts_are_queryable_by_sender() {
    let mut s = setup();
    post(&mut s);
    let v = call(&mut s, "alice", "show", json!({"id":1,"receipts":true}));
    assert_eq!(v["receipts"][0]["agent"], "bob");
}
#[test]
fn release_invalidates_old_token() {
    let mut s = setup();
    post(&mut s);
    let c = call(&mut s, "alice", "claim", json!({"id":1}));
    let r = call(
        &mut s,
        "alice",
        "release",
        json!({"id":1,"fence":c["card"]["fence"]}),
    );
    assert!(r["card"]["fence"].as_i64().unwrap() > c["card"]["fence"].as_i64().unwrap());
}
#[test]
fn database_identity_persists() {
    let path = std::env::temp_dir().join(format!(
        "fray-test-{}.db",
        fray::model::random_key().unwrap()
    ));
    let id = { Store::open(&path, false).unwrap().identity().unwrap() };
    assert_eq!(Store::open(&path, false).unwrap().identity().unwrap(), id);
    let _ = std::fs::remove_file(path);
}
