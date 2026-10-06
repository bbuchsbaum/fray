use fray::{
    model::Request,
    store::{Store, IDENTITY_TTL_MS},
};
use serde_json::{json, Value};

const T: i64 = 1_900_000_000_000;

fn run(
    s: &mut Store,
    actor: &str,
    session: Option<&str>,
    op: &str,
    args: Value,
    at: i64,
) -> Result<Value, String> {
    s.execute_at(
        &Request::new(op, actor, args).with_session(session.map(str::to_owned)),
        at,
    )
    .map_err(|e| e.code)
}
fn ok(s: &mut Store, actor: &str, session: Option<&str>, op: &str, args: Value, at: i64) -> Value {
    run(s, actor, session, op, args, at).unwrap()
}
fn join(s: &mut Store, who: &str, session: Option<&str>, at: i64) {
    ok(s, who, session, "join", json!({}), at);
}
fn peers(s: &mut Store, who: &str, session: &str, limit: i64, at: i64) -> Value {
    ok(s, who, Some(session), "peers", json!({"limit":limit}), at)
}
fn present(
    s: &mut Store,
    who: &str,
    session: &str,
    listing: &Value,
    peers: Value,
    at: i64,
) -> Result<Value, String> {
    run(
        s,
        who,
        Some(session),
        "peer_present",
        json!({"store_id":listing["store_id"],"peers":peers}),
        at,
    )
}
fn peer(v: &Value, name: &str) -> Value {
    v["peers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == name)
        .unwrap()
        .clone()
}
fn shown(peer: &Value) -> Value {
    json!({"name":peer["name"],"generation":peer["generation"]})
}

#[test]
fn ended_session_peer_cursors_are_pruned_and_live_sessions_are_preserved() {
    let mut s = Store::memory().unwrap();
    join(&mut s, "reader", Some("codex:old"), T);
    join(&mut s, "other", Some("claude:live"), T);
    join(&mut s, "peer", Some("claude:peer"), T);
    for (who, session) in [("reader", "codex:old"), ("other", "claude:live")] {
        let listing = peers(&mut s, who, session, 20, T);
        present(
            &mut s,
            who,
            session,
            &listing,
            json!([shown(&peer(&listing, "peer"))]),
            T,
        )
        .unwrap();
    }
    for n in 0..12 {
        let session = format!("codex:new-{n}");
        ok(
            &mut s,
            "reader",
            Some(&session),
            "join",
            json!({"takeover":true}),
            T + n + 1,
        );
        let listing = peers(&mut s, "reader", &session, 20, T + n + 1);
        let before = s.highwater().unwrap();
        present(
            &mut s,
            "reader",
            &session,
            &listing,
            json!([shown(&peer(&listing, "peer"))]),
            T + n + 1,
        )
        .unwrap();
        assert_eq!(s.highwater().unwrap(), before);
        let count: i64 = s
            .conn
            .query_row(
                "SELECT count(*) FROM peer_seen WHERE reader='reader'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let other: i64 = s
            .conn
            .query_row(
                "SELECT count(*) FROM peer_seen WHERE reader='other' AND session='claude:live'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(other, 1);
    }
}

#[test]
fn first_session_surfaces_existing_peers_once_and_later_join_once() {
    let mut s = Store::memory().unwrap();
    join(&mut s, "early", Some("early:1"), T);
    join(&mut s, "late", Some("late:1"), T + 1);
    let fresh = peers(&mut s, "late", "late:1", 4, T + 2);
    let early = peer(&fresh, "early");
    present(
        &mut s,
        "late",
        "late:1",
        &fresh,
        json!([shown(&early)]),
        T + 3,
    )
    .unwrap();
    assert!(peers(&mut s, "late", "late:1", 4, T + 4)["peers"]
        .as_array()
        .unwrap()
        .is_empty());

    join(&mut s, "new", Some("new:1"), T + 5);
    let listed = peers(&mut s, "late", "late:1", 4, T + 6);
    let marker = peer(&listed, "new");
    assert_eq!(marker["generation"], 1);
    present(
        &mut s,
        "late",
        "late:1",
        &listed,
        json!([shown(&marker)]),
        T + 7,
    )
    .unwrap();
    join(&mut s, "new", Some("new:1"), T + 8);
    assert!(peers(&mut s, "late", "late:1", 4, T + 9)["peers"]
        .as_array()
        .unwrap()
        .is_empty());
}

#[test]
fn leave_rejoin_and_changed_session_activity_increment_generation() {
    let mut s = Store::memory().unwrap();
    join(&mut s, "reader", Some("reader:1"), T);
    join(&mut s, "peer", Some("peer:old"), T + 1);
    let first = peers(&mut s, "reader", "reader:1", 4, T + 2);
    let p1 = peer(&first, "peer");
    present(
        &mut s,
        "reader",
        "reader:1",
        &first,
        json!([shown(&p1)]),
        T + 3,
    )
    .unwrap();
    ok(&mut s, "peer", Some("peer:old"), "leave", json!({}), T + 4);
    join(&mut s, "peer", Some("peer:old"), T + 5);
    assert_eq!(
        peer(&peers(&mut s, "reader", "reader:1", 4, T + 6), "peer")["generation"],
        2
    );

    let seen = peers(&mut s, "reader", "reader:1", 4, T + 7);
    present(
        &mut s,
        "reader",
        "reader:1",
        &seen,
        json!([shown(&peer(&seen, "peer"))]),
        T + 8,
    )
    .unwrap();
    // A stale binding replaced by non-join activity is a new peer generation.
    ok(
        &mut s,
        "peer",
        Some("peer:new"),
        "heartbeat",
        json!({}),
        T + 5 + IDENTITY_TTL_MS + 1,
    );
    assert_eq!(
        peer(
            &peers(&mut s, "reader", "reader:1", 4, T + 6 + IDENTITY_TTL_MS),
            "peer"
        )["generation"],
        3
    );
}

#[test]
fn exact_presentation_is_session_local_and_does_not_consume_a_newer_rejoin_or_hidden_rows() {
    let mut s = Store::memory().unwrap();
    join(&mut s, "reader", Some("reader:a"), T);
    for (n, who) in ["one", "two", "three"].iter().enumerate() {
        join(&mut s, who, Some(&format!("{who}:1")), T + n as i64 + 1);
    }
    let page = peers(&mut s, "reader", "reader:a", 1, T + 10);
    assert_eq!(page["peers"].as_array().unwrap().len(), 1);
    assert_eq!(page["more"], true);
    present(
        &mut s,
        "reader",
        "reader:a",
        &page,
        json!([shown(&page["peers"][0])]),
        T + 11,
    )
    .unwrap();
    assert!(!peers(&mut s, "reader", "reader:a", 4, T + 12)["peers"]
        .as_array()
        .unwrap()
        .is_empty());

    let listed = peers(&mut s, "reader", "reader:a", 4, T + 13);
    let old = peer(&listed, "two");
    ok(&mut s, "two", Some("two:1"), "leave", json!({}), T + 14);
    join(&mut s, "two", Some("two:1"), T + 15);
    present(
        &mut s,
        "reader",
        "reader:a",
        &listed,
        json!([shown(&old)]),
        T + 16,
    )
    .unwrap();
    assert_eq!(
        peer(&peers(&mut s, "reader", "reader:a", 4, T + 17), "two")["generation"],
        2
    );

    ok(
        &mut s,
        "reader",
        Some("reader:a"),
        "leave",
        json!({}),
        T + 18,
    );
    join(&mut s, "reader", Some("reader:b"), T + 19);
    assert!(
        peers(&mut s, "reader", "reader:b", 4, T + 20)["peers"]
            .as_array()
            .unwrap()
            .len()
            >= 3
    );
}

#[test]
fn invalid_presentations_are_atomic_and_do_not_create_card_state() {
    let mut s = Store::memory().unwrap();
    join(&mut s, "reader", Some("reader:1"), T);
    join(&mut s, "peer", Some("peer:1"), T + 1);
    let listed = peers(&mut s, "reader", "reader:1", 4, T + 2);
    let marker = peer(&listed, "peer");
    assert_eq!(
        run(
            &mut s,
            "reader",
            None,
            "peer_present",
            json!({"store_id":listed["store_id"],"peers":[shown(&marker)]}),
            T + 3
        )
        .unwrap_err(),
        "session_required"
    );
    assert_eq!(
        present(
            &mut s,
            "reader",
            "reader:1",
            &json!({"store_id":"wrong"}),
            json!([shown(&marker)]),
            T + 4
        )
        .unwrap_err(),
        "receipt_mismatch"
    );
    assert_eq!(
        present(
            &mut s,
            "reader",
            "reader:1",
            &listed,
            json!([{"name":"peer","generation":99}]),
            T + 5
        )
        .unwrap_err(),
        "invalid"
    );
    // Failed requests leave the pending marker and no card/delivery state behind.
    assert_eq!(
        peer(&peers(&mut s, "reader", "reader:1", 4, T + 6), "peer")["generation"],
        1
    );
    assert_eq!(
        ok(
            &mut s,
            "reader",
            Some("reader:1"),
            "query",
            json!({}),
            T + 7
        )["total"],
        0
    );
    assert_eq!(
        present(
            &mut s,
            "reader",
            "reader:1",
            &listed,
            json!([shown(&marker)]),
            T + 8
        )
        .unwrap()["acknowledged"],
        false
    );
}
