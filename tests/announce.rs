//! Daemon maintenance notices (`announce`): authored by the reserved `fray`
//! identity, project-wide, with the requester recorded. The caller needs no
//! join and leaves no participant, session or presence behind.
use fray::{model::*, store::Store};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000_000;

fn at(s: &mut Store, who: &str, op: &str, args: Value) -> Value {
    s.execute_at(&Request::new(op, who, args), NOW)
        .unwrap_or_else(|e| panic!("{op}: {e:?}"))
}
fn refused(s: &mut Store, req: Request) -> String {
    s.execute_at(&req, NOW).unwrap_err().code
}
fn restart(reason: &str) -> Value {
    json!({"action":"restart","reason":reason,"from_build":"aaa111","to_build":"bbb222"})
}
fn names(s: &mut Store, all: bool) -> Vec<String> {
    at(s, "alice", "agents", json!({"all":all}))["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap().to_owned())
        .collect()
}
fn count(s: &Store, sql: &str) -> i64 {
    s.conn.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn notice_is_authored_by_fray_without_a_join() {
    let mut s = Store::memory().unwrap();
    at(&mut s, "alice", "join", json!({}));
    at(&mut s, "bob", "join", json!({"topics":[]}));
    let mut args = restart("install new build");
    args["requested_by"] = json!("osuser");
    let v = at(&mut s, "", "announce", args);
    let card = &v["card"];
    assert_eq!(card["author"], "fray");
    assert_eq!(card["topic"], "*");
    assert_eq!(card["kind"], "note");
    assert_eq!(card["tags"], json!(["maintenance", "maintenance:restart"]));
    assert_eq!(v["maintenance"]["requested_by"], "osuser");
    assert_eq!(v["maintenance"]["from_build"], "aaa111");
    assert_eq!(v["maintenance"]["to_build"], "bbb222");
    let id = card["id"].as_i64().unwrap();
    // The structured payload is on the creation event too.
    let detail: String = s
        .conn
        .query_row(
            "SELECT payload FROM events WHERE card_id=? ORDER BY seq LIMIT 1",
            [id],
            |r| r.get(0),
        )
        .unwrap();
    let detail: Value = serde_json::from_str(&detail).unwrap();
    assert_eq!(detail["detail"]["maintenance"]["action"], "restart");
    assert_eq!(
        detail["detail"]["maintenance"]["reason"],
        "install new build"
    );
    // Routed like a '*' broadcast, even to a reader with no topics.
    for who in ["alice", "bob"] {
        let inbox = at(&mut s, who, "inbox", json!({}));
        assert!(inbox["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["card"]["id"] == id));
    }
    // No participant, session or presence for anyone new.
    assert_eq!(names(&mut s, false), ["alice", "bob"]);
    assert_eq!(
        count(&s, "SELECT count(*) FROM participants WHERE agent<>'fray'"),
        0
    );
    assert_eq!(
        count(
            &s,
            "SELECT count(*) FROM agents WHERE name='fray' AND enabled=0 AND last_seen_ms=0"
        ),
        1
    );
}

#[test]
fn an_identified_caller_is_recorded_but_not_registered() {
    let mut s = Store::memory().unwrap();
    at(&mut s, "alice", "join", json!({}));
    let mut req = Request::new("announce", "restarter", restart("upgrade"));
    req.session = Some("claude:abc".into());
    let v = s.execute_at(&req, NOW).unwrap();
    assert_eq!(v["maintenance"]["requested_by"], "restarter");
    assert_eq!(v["maintenance"]["requested_session"], "claude:abc");
    assert_eq!(names(&mut s, false), ["alice"]);
    assert!(!names(&mut s, true).contains(&"restarter".to_owned()));
    assert_eq!(
        count(&s, "SELECT count(*) FROM sessions WHERE agent='restarter'"),
        0
    );
    // A joined caller's presence is not renewed by announcing.
    let seen = |s: &Store| -> i64 {
        s.conn
            .query_row(
                "SELECT last_seen_ms FROM agents WHERE name='alice'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    let before = seen(&s);
    s.execute_at(
        &Request::new("announce", "alice", restart("again")),
        NOW + 5_000,
    )
    .unwrap();
    assert_eq!(seen(&s), before);
}

#[test]
fn retry_with_the_same_key_does_not_duplicate() {
    let mut s = Store::memory().unwrap();
    let mut req = Request::new("announce", "maint", restart("upgrade"));
    req.key = Some("k-1".into());
    let first = s.execute_at(&req, NOW).unwrap();
    let second = s.execute_at(&req, NOW + 1).unwrap();
    assert_eq!(first["card"]["id"], second["card"]["id"]);
    assert_eq!(count(&s, "SELECT count(*) FROM cards"), 1);
    let mut other = Request::new("announce", "maint", restart("different"));
    other.key = Some("k-1".into());
    assert_eq!(refused(&mut s, other), "idempotency_conflict");
    // Without --as the key is honoured just the same.
    let mut anon = restart("anon");
    anon["requested_by"] = json!("osuser");
    let mut req = Request::new("announce", "", anon);
    req.key = Some("k-2".into());
    s.execute_at(&req, NOW).unwrap();
    s.execute_at(&req, NOW + 1).unwrap();
    assert_eq!(count(&s, "SELECT count(*) FROM cards"), 2);
}

#[test]
fn a_new_notice_supersedes_the_open_one() {
    let mut s = Store::memory().unwrap();
    at(&mut s, "alice", "join", json!({}));
    let first = at(&mut s, "alice", "announce", restart("one"))["card"]["id"].clone();
    let second = at(&mut s, "alice", "announce", restart("two"))["card"]["id"].clone();
    let open = at(&mut s, "alice", "query", json!({"tag":"maintenance"}));
    let ids: Vec<&Value> = open["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["id"])
        .collect();
    assert_eq!(ids, [&second]);
    let old = at(&mut s, "alice", "show", json!({"id":first}));
    assert_eq!(old["card"]["status"], "superseded");
}

#[test]
fn the_system_name_is_reserved() {
    let mut s = Store::memory().unwrap();
    for name in ["fray", "Fray", "f-r-a-y", "FRAY"] {
        assert_eq!(
            refused(&mut s, Request::new("join", name, json!({}))),
            "reserved_system",
            "{name}"
        );
    }
    at(&mut s, "alice", "join", json!({}));
    assert_eq!(
        refused(&mut s, Request::new("post", "fray", json!({"title":"x"}))),
        "reserved_system"
    );
    assert_eq!(
        refused(&mut s, Request::new("announce", "fray", restart("x"))),
        "reserved_system"
    );
    let mut spoof = restart("x");
    spoof["requested_by"] = json!("fray");
    assert_eq!(
        refused(&mut s, Request::new("announce", "", spoof)),
        "invalid"
    );
    // The owner stays reserved for `fray owner`, announce included.
    assert_eq!(
        refused(&mut s, Request::new("announce", "owner", restart("x"))),
        "reserved_owner"
    );
    // Nothing can address or assign to the notice author.
    at(&mut s, "alice", "announce", restart("x"));
    assert_eq!(
        refused(
            &mut s,
            Request::new("send", "alice", json!({"to":"fray","body":"hi"}))
        ),
        "reserved_system"
    );
    assert_eq!(
        refused(
            &mut s,
            Request::new(
                "send",
                "alice",
                json!({"to":"Fray","body":"hi","pending":true})
            )
        ),
        "reserved_system"
    );
    assert_eq!(
        refused(
            &mut s,
            Request::new(
                "post",
                "alice",
                json!({"title":"t","summary":"s","kind":"task","assignee":"fray"})
            )
        ),
        "reserved_system"
    );
}

#[test]
fn arguments_are_validated() {
    let mut s = Store::memory().unwrap();
    for (who, args) in [
        ("", restart("no requester")),
        ("m", json!({"action":"reboot","reason":"x"})),
        ("m", json!({"action":"restart"})),
        ("m", json!({"action":"restart","reason":" "})),
        (
            "m",
            json!({"action":"restart","reason":"x","requested_by":"other"}),
        ),
        ("m", json!({"action":"restart","reason":"x","extra":1})),
        ("m", json!({"action":"restart","reason":"x","to_build":""})),
    ] {
        assert_eq!(
            refused(&mut s, Request::new("announce", who, args.clone())),
            "invalid",
            "{args}"
        );
    }
    assert_eq!(count(&s, "SELECT count(*) FROM cards"), 0);
}

#[test]
fn a_board_with_an_agent_already_named_fray_refuses_to_announce() {
    let mut s = Store::memory().unwrap();
    // Simulate a board from before the name was reserved.
    s.conn
        .execute(
            "INSERT INTO agents(name,role,topics,enabled,joined_ms,last_seen_ms) VALUES('fray','worker','[\"*\"]',1,?1,?1)",
            [NOW],
        )
        .unwrap();
    assert_eq!(
        refused(&mut s, Request::new("announce", "m", restart("x"))),
        "reserved_system"
    );
    assert_eq!(count(&s, "SELECT count(*) FROM cards"), 0);
}

#[test]
fn ping_advertises_announce() {
    let mut s = Store::memory().unwrap();
    let ping = at(&mut s, "", "ping", json!({}));
    assert!(ping["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c == "announce"));
}
