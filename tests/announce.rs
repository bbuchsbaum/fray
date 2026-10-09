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
fn a_restarted_notice_follows_and_supersedes_the_restart_notice() {
    let mut s = Store::memory().unwrap();
    let before = at(&mut s, "m", "announce", restart("upgrade"))["card"]["id"].clone();
    let after = at(
        &mut s,
        "m",
        "announce",
        json!({"action":"restarted","reason":"upgrade","from_build":"aaa111","to_build":"bbb222"}),
    );
    assert_eq!(
        after["card"]["tags"],
        json!(["maintenance", "maintenance:restarted"])
    );
    assert_eq!(after["card"]["author"], "fray");
    assert_eq!(after["maintenance"]["action"], "restarted");
    assert!(
        after["card"]["title"]
            .as_str()
            .unwrap()
            .starts_with("Fray daemon restarted: upgrade"),
        "{after}"
    );
    assert!(
        after["card"]["summary"]
            .as_str()
            .unwrap()
            .contains("Build aaa111 -> bbb222. Reason: upgrade. The daemon is back"),
        "{after}"
    );
    let old = at(&mut s, "m", "show", json!({"id":before}));
    assert_eq!(old["card"]["status"], "superseded");
}

#[test]
fn an_abandoned_notice_says_the_restart_did_not_complete() {
    let mut s = Store::memory().unwrap();
    let before = at(&mut s, "m", "announce", restart("upgrade"))["card"]["id"].clone();
    let after = at(
        &mut s,
        "m",
        "announce",
        json!({"action":"abandoned","reason":"upgrade (no replacement started)"}),
    );
    assert_eq!(
        after["card"]["title"],
        "Fray daemon restart abandoned: upgrade (no replacement started)"
    );
    assert_eq!(
        after["card"]["tags"],
        json!(["maintenance", "maintenance:abandoned"])
    );
    assert!(
        after["card"]["summary"]
            .as_str()
            .unwrap()
            .contains("did not complete"),
        "{after}"
    );
    let old = at(&mut s, "m", "show", json!({"id":before}));
    assert_eq!(old["card"]["status"], "superseded");
}

#[test]
fn the_system_name_is_reserved() {
    let mut s = Store::memory().unwrap();
    // The owner's rule: case, separators, a numeric suffix and common
    // substitutions are ignored; an unrelated longer name is not.
    for name in [
        "fray", "Fray", "f-r-a-y", "FRAY", "fray1", "fray-2", "f.r.a.y",
    ] {
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
    // The remedy the error names works: the agent can still leave, and then
    // the daemon can announce. It cannot rejoin under the name.
    at(&mut s, "fray", "leave", json!({}));
    let v = at(&mut s, "m", "announce", restart("x"));
    assert_eq!(v["card"]["author"], "fray");
    assert_eq!(
        refused(&mut s, Request::new("join", "fray", json!({}))),
        "reserved_system"
    );
    assert_eq!(
        refused(&mut s, Request::new("heartbeat", "fray", json!({}))),
        "reserved_system"
    );
}

#[test]
fn a_legacy_lookalike_can_still_leave() {
    let mut s = Store::memory().unwrap();
    s.conn
        .execute(
            "INSERT INTO agents(name,role,topics,enabled,joined_ms,last_seen_ms) VALUES('fray1','worker','[\"*\"]',1,?1,?1)",
            [NOW],
        )
        .unwrap();
    assert_eq!(
        refused(
            &mut s,
            Request::new("post", "fray1", json!({"title":"x","summary":"y"}))
        ),
        "reserved_system"
    );
    let mut leave = Request::new("leave", "fray1", json!({}));
    leave.session = Some("claude:legacy".into());
    assert_eq!(s.execute_at(&leave, NOW + 9_000).unwrap()["enabled"], false);
    // Stepping aside binds no session.
    assert_eq!(
        count(&s, "SELECT count(*) FROM sessions WHERE agent='fray1'"),
        0
    );
    // Once gone, the exemption is gone too.
    assert_eq!(refused(&mut s, leave), "reserved_system");
}

#[test]
fn the_notice_author_itself_cannot_leave() {
    let mut s = Store::memory().unwrap();
    at(&mut s, "m", "announce", restart("x"));
    let mut leave = Request::new("leave", "fray", json!({}));
    leave.session = Some("claude:abc".into());
    assert_eq!(refused(&mut s, leave), "reserved_system");
    assert_eq!(
        count(&s, "SELECT count(*) FROM sessions WHERE agent='fray'"),
        0
    );
    assert_eq!(
        count(
            &s,
            "SELECT count(*) FROM agents WHERE name='fray' AND enabled=0 AND last_seen_ms=0"
        ),
        1
    );
    // Nor can a name that never joined.
    assert_eq!(
        refused(&mut s, Request::new("leave", "fray2", json!({}))),
        "reserved_system"
    );
}

/// Mote items and requests addressed to `fray` (after it exists as the
/// notice author) are unknown recipients, never a sync-wide failure.
#[test]
fn mote_mail_to_fray_does_not_wedge_sync() {
    let mut s = Store::memory().unwrap();
    for who in ["alice", "bob"] {
        at(&mut s, who, "join", json!({"topics":[]}));
    }
    at(
        &mut s,
        "alice",
        "mote_bind",
        json!({"store":"/r/.mote","store_id":"st-A","cursor_mode":"admission_v1","genesis_digest":"genesis"}),
    );
    at(&mut s, "alice", "announce", restart("x"));
    let ingest = at(
        &mut s,
        "alice",
        "mote_ingest",
        json!({"store_id":"st-A","sync_revision":1,"after":null,"cursor":"a","claims":[],
            "items":[{"key":"k1","recipient":"fray","title":"t","summary":"s"},
                     {"key":"k2","recipient":"Fray1","title":"t","summary":"s"},
                     {"key":"k3","recipient":"bob","title":"t","summary":"s"}]}),
    );
    assert_eq!(
        ingest["unknown_recipients"],
        json!(["Fray1", "fray"]),
        "{ingest}"
    );
    assert_eq!(ingest["created"].as_array().unwrap().len(), 1, "{ingest}");
    let requests = at(
        &mut s,
        "alice",
        "mote_requests_sync",
        json!({"store_id":"st-A","requests":[
            {"msg_id":"m1","recipient":"fray","from":"alice","state":"open","body":"?"},
            {"msg_id":"m2","recipient":"bob","from":"alice","state":"open","body":"?"}]}),
    );
    assert_eq!(
        requests["unknown_recipients"].as_array().unwrap().len(),
        1,
        "{requests}"
    );
    assert_eq!(requests["unknown_recipients"][0]["recipient"], "fray");
    assert_eq!(
        requests["created"].as_array().unwrap().len(),
        1,
        "{requests}"
    );
    // Kept for R3, like any request nobody here can be asked.
    assert_eq!(
        count(
            &s,
            "SELECT count(*) FROM mote_requests_unknown WHERE recipient='fray'"
        ),
        1
    );
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
