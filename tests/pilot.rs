use fray::{
    model::{random_key, Request},
    store::Store,
};
use serde_json::{json, Value};
const NOW: i64 = 1_800_000_000_000;
fn call(s: &mut Store, who: &str, op: &str, args: Value) -> Value {
    s.execute_at(&Request::new(op, who, args), NOW).unwrap()
}
fn setup() -> Store {
    let mut s = Store::memory().unwrap();
    for who in ["alice", "bob", "manager"] {
        call(&mut s, who, "join", json!({}));
    }
    s
}
fn selected(s: &mut Store, who: &str) -> Value {
    call(s, who, "inbox", json!({"selection":"involved"}))
}
fn note(s: &mut Store) -> Value {
    call(
        s,
        "alice",
        "post",
        json!({"topic":"parser","title":"Review","summary":"Evidence"}),
    )
}

#[test]
fn incidental_delivery_is_not_permanent_participation() {
    let mut s = setup();
    note(&mut s);
    let joined = call(&mut s, "bob", "join", json!({"topics":["tests"]}));
    assert_eq!(joined["subscriptions"]["retained_pending_outside_scope"], 1);
    assert_eq!(call(&mut s, "bob", "inbox", json!({}))["total"], 1);
    assert_eq!(selected(&mut s, "bob")["total"], 0);
    call(&mut s, "bob", "ack", json!({"id":1,"through":1}));
    call(
        &mut s,
        "alice",
        "annotate",
        json!({"id":1,"body":"Later evidence"}),
    );
    assert_eq!(call(&mut s, "bob", "inbox", json!({}))["total"], 0);
    call(&mut s, "bob", "follow", json!({"id":1}));
    assert_eq!(selected(&mut s, "bob")["items"][0]["through_seq"], 2);
    call(&mut s, "bob", "unfollow", json!({"id":1}));
    assert_eq!(
        call(&mut s, "bob", "inbox", json!({}))["items"][0]["through_seq"],
        2
    );
    call(&mut s, "bob", "ack", json!({"id":1,"through":2}));
    call(
        &mut s,
        "alice",
        "annotate",
        json!({"id":1,"body":"After unfollow"}),
    );
    assert_eq!(call(&mut s, "bob", "inbox", json!({}))["total"], 0);
    call(
        &mut s,
        "bob",
        "annotate",
        json!({"id":1,"body":"Deliberate contribution"}),
    );
    call(&mut s, "bob", "leave", json!({}));
    call(
        &mut s,
        "alice",
        "patch",
        json!({"id":1,"expect":1,"status":"resolved"}),
    );
    call(&mut s, "bob", "join", json!({}));
    assert_eq!(selected(&mut s, "bob")["items"][0]["through_seq"], 5);
}

#[test]
fn selected_manager_sees_incoming_outgoing_and_named_topics_not_firehose() {
    let mut s = setup();
    call(
        &mut s,
        "manager",
        "join",
        json!({"role":"steward","topics":["parser"]}),
    );
    call(
        &mut s,
        "alice",
        "send",
        json!({"to":"bob","body":"Unrelated"}),
    );
    assert_eq!(call(&mut s, "manager", "inbox", json!({}))["total"], 1);
    assert_eq!(selected(&mut s, "manager")["total"], 0);
    call(
        &mut s,
        "alice",
        "send",
        json!({"to":"manager","body":"Direct question","ask":true}),
    );
    let sent = call(
        &mut s,
        "manager",
        "send",
        json!({"to":"bob","body":"Please verify","ask":true}),
    );
    call(
        &mut s,
        "bob",
        "annotate",
        json!({"id":sent["card"]["id"],"body":"Verified"}),
    );
    note(&mut s);
    assert_eq!(selected(&mut s, "manager")["total"], 3);
    assert_eq!(
        s.selected_attention("manager", 0, 100, "involved", NOW)
            .unwrap(),
        selected(&mut s, "manager")
            .as_object()
            .map(|v| {
                let mut v = v.clone();
                v.remove("store_id");
                Value::Object(v)
            })
            .unwrap()
    );
}

#[test]
fn batch_ack_is_atomic_store_and_actor_qualified_and_uses_delivery_not_head() {
    let mut s = setup();
    note(&mut s);
    note(&mut s);
    let page = call(&mut s, "bob", "inbox", json!({}));
    let receipts: Vec<_> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v["receipt"].clone())
        .collect();
    for field in ["store_id", "agent"] {
        let mut bad = receipts.clone();
        bad[1][field] = json!("wrong");
        let err = s
            .execute_at(&Request::new("ack", "bob", json!({"receipts":bad})), NOW)
            .unwrap_err();
        assert_eq!(err.code, "receipt_mismatch");
        assert_eq!(call(&mut s, "bob", "inbox", json!({}))["total"], 2);
    }
    let mut bad = receipts.clone();
    bad[1]["through_seq"] = json!(9999);
    assert!(s
        .execute_at(&Request::new("ack", "bob", json!({"receipts":bad})), NOW)
        .is_err());
    assert_eq!(call(&mut s, "bob", "inbox", json!({}))["total"], 2);
    call(
        &mut s,
        "bob",
        "annotate",
        json!({"id":1,"body":"My answer advances the head, not my receipt"}),
    );
    call(
        &mut s,
        "alice",
        "annotate",
        json!({"id":2,"body":"New peer update must remain pending"}),
    );
    call(&mut s, "bob", "ack", json!({"receipts":receipts}));
    assert_eq!(
        call(
            &mut s,
            "bob",
            "receipt_status",
            json!({"receipts":receipts})
        )["handled"],
        2
    );
    let pending = call(&mut s, "bob", "inbox", json!({}));
    assert_eq!(pending["total"], 1);
    assert_eq!(pending["items"][0]["through_seq"], 4);
    assert_eq!(pending["items"][0]["ack_seq"], 2);
    assert_eq!(
        call(&mut s, "bob", "show", json!({"id":1}))["card"]["status"],
        "open"
    );
}

fn controller(
    s: &mut Store,
    run: &str,
    state: &str,
    begin: bool,
    now: i64,
) -> fray::model::Result<Value> {
    s.execute_at(
        &Request::new(
            "controller",
            "bob",
            json!({"run_id":run,"state":state,"begin":begin}),
        ),
        now,
    )
}
#[test]
fn controller_lease_fences_old_runs_and_does_not_resurrect_leave() {
    let mut s = setup();
    controller(&mut s, "first", "waiting", true, NOW).unwrap();
    assert_eq!(
        controller(&mut s, "duplicate", "waiting", true, NOW + 1)
            .unwrap_err()
            .code,
        "controller_busy"
    );
    controller(&mut s, "first", "running", false, NOW + 30000).unwrap();
    let agents = s
        .execute_at(&Request::new("agents", "", json!({})), NOW + 130000)
        .unwrap();
    let bob = agents["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "bob")
        .unwrap();
    assert_eq!(bob["recently_seen"], true);
    assert_eq!(bob["controller"]["state"], "running");
    let agents = s
        .execute_at(&Request::new("agents", "", json!({})), NOW + 150000)
        .unwrap();
    let bob = agents["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == "bob")
        .unwrap();
    assert_eq!(bob["controller"]["state"], "stale");
    assert_eq!(bob["controller"]["live"], false);
    controller(&mut s, "second", "waiting", true, NOW + 150000).unwrap();
    assert_eq!(
        controller(&mut s, "first", "failed", false, NOW + 150001)
            .unwrap_err()
            .code,
        "controller_lost"
    );
    s.execute_at(&Request::new("leave", "bob", json!({})), NOW + 150002)
        .unwrap();
    assert_eq!(
        controller(&mut s, "second", "running", false, NOW + 150003)
            .unwrap_err()
            .code,
        "not_joined"
    );
    s.execute_at(&Request::new("join", "bob", json!({})), NOW + 150004)
        .unwrap();
    assert_eq!(
        controller(&mut s, "second", "running", false, NOW + 150005)
            .unwrap_err()
            .code,
        "controller_lost"
    );
}

#[test]
fn migration_preserves_receipts_but_only_backfills_contributors_once() {
    let dir = std::env::temp_dir().join(format!("fray-migrate-{}", random_key().unwrap()));
    std::fs::create_dir(&dir).unwrap();
    let path = dir.join("state.db");
    let identity;
    {
        let mut s = Store::open(&path, false).unwrap();
        for who in ["alice", "bob", "carol"] {
            call(&mut s, who, "join", json!({}));
        }
        identity = s.identity().unwrap();
        note(&mut s);
        call(&mut s, "bob", "ack", json!({"id":1,"through":1}));
        call(
            &mut s,
            "carol",
            "annotate",
            json!({"id":1,"body":"Contributed"}),
        );
        // Exact v1 layout: the only v2 schema changes are these two tables.
        s.conn
            .execute_batch(
                "DROP TABLE participants; DROP TABLE controllers; PRAGMA user_version=1;",
            )
            .unwrap();
    }
    {
        let mut s = Store::open(&path, false).unwrap();
        assert_eq!(s.identity().unwrap(), identity);
        let pending = call(&mut s, "bob", "inbox", json!({}));
        assert_eq!(pending["items"][0]["through_seq"], 2);
        assert_eq!(pending["items"][0]["ack_seq"], 1);
        assert_eq!(selected(&mut s, "bob")["total"], 0);
        call(&mut s, "bob", "join", json!({"topics":[]}));
        call(&mut s, "carol", "join", json!({"topics":[]}));
        call(
            &mut s,
            "alice",
            "annotate",
            json!({"id":1,"body":"After migration"}),
        );
        assert_eq!(
            call(&mut s, "bob", "inbox", json!({}))["items"][0]["through_seq"],
            2
        );
        assert_eq!(selected(&mut s, "carol")["items"][0]["through_seq"], 3);
        call(&mut s, "carol", "unfollow", json!({"id":1}));
    }
    {
        let mut s = Store::open(&path, false).unwrap();
        assert_eq!(
            selected(&mut s, "carol")["total"],
            0,
            "migration must not re-follow on every restart"
        );
        assert_eq!(
            s.conn
                .query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            2
        );
        assert_eq!(
            s.conn
                .query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))
                .unwrap(),
            "ok"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}
