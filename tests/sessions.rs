use fray::{
    model::Request,
    store::{Store, IDENTITY_TTL_MS},
};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000_000;

fn run(
    store: &mut Store,
    actor: &str,
    session: Option<&str>,
    op: &str,
    args: Value,
    at: i64,
) -> Result<Value, String> {
    store
        .execute_at(
            &Request::new(op, actor, args).with_session(session.map(str::to_owned)),
            at,
        )
        .map_err(|e| e.code)
}

fn ok(
    store: &mut Store,
    actor: &str,
    session: Option<&str>,
    op: &str,
    args: Value,
    at: i64,
) -> Value {
    run(store, actor, session, op, args, at).unwrap()
}

fn roster_entry(store: &mut Store, name: &str, at: i64) -> Value {
    let roster = ok(store, "observer", None, "agents", json!({}), at);
    roster["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == name)
        .unwrap()
        .clone()
}

fn board() -> Store {
    let mut s = Store::memory().unwrap();
    ok(&mut s, "observer", None, "join", json!({}), NOW);
    s
}

#[test]
fn a_second_live_session_cannot_silently_share_a_name() {
    let mut s = board();
    ok(
        &mut s,
        "claude-release",
        Some("claude:aaa"),
        "join",
        json!({}),
        NOW,
    );
    // The field-report failure: another session joins under the same name.
    let err = run(
        &mut s,
        "claude-release",
        Some("claude:bbb"),
        "join",
        json!({}),
        NOW + 60_000,
    );
    assert_eq!(err.unwrap_err(), "identity_busy");
    // Its writes are refused too, so it cannot post under the name either.
    let err = run(
        &mut s,
        "claude-release",
        Some("claude:bbb"),
        "post",
        json!({"title":"#48","summary":"forged"}),
        NOW + 60_000,
    );
    assert_eq!(err.unwrap_err(), "identity_busy");
    // The holder keeps working, and the roster names the holding session.
    ok(
        &mut s,
        "claude-release",
        Some("claude:aaa"),
        "post",
        json!({"title":"#48","summary":"real"}),
        NOW + 61_000,
    );
    let entry = roster_entry(&mut s, "claude-release", NOW + 62_000);
    assert_eq!(entry["session"]["bound"]["session"], "claude:aaa");
    assert_eq!(entry["session"]["bound"]["live"], true);
}

#[test]
fn takeover_is_explicit_and_visible() {
    let mut s = board();
    ok(&mut s, "worker", Some("claude:old"), "join", json!({}), NOW);
    let joined = ok(
        &mut s,
        "worker",
        Some("claude:new"),
        "join",
        json!({"takeover": true}),
        NOW + 1_000,
    );
    assert_eq!(joined["session_replaced"]["session"], "claude:old");
    assert!(joined["session_replaced"]["reason"]
        .as_str()
        .unwrap()
        .starts_with("takeover by claude:new"));
    let entry = roster_entry(&mut s, "worker", NOW + 2_000);
    assert_eq!(entry["session"]["bound"]["session"], "claude:new");
    assert_eq!(
        entry["session"]["recent_takeover"]["displaced"],
        "claude:old"
    );
    // The displaced session is now the one refused.
    let err = run(
        &mut s,
        "worker",
        Some("claude:old"),
        "heartbeat",
        json!({}),
        NOW + 3_000,
    );
    assert_eq!(err.unwrap_err(), "identity_busy");
}

#[test]
fn a_stale_binding_yields_without_takeover() {
    let mut s = board();
    ok(
        &mut s,
        "worker",
        Some("claude:gone"),
        "join",
        json!({}),
        NOW,
    );
    let later = NOW + IDENTITY_TTL_MS + 1;
    let joined = ok(
        &mut s,
        "worker",
        Some("claude:fresh"),
        "join",
        json!({}),
        later,
    );
    assert!(joined["session_replaced"]["reason"]
        .as_str()
        .unwrap()
        .starts_with("stale"));
    // A stale replacement is not reported as a takeover.
    let entry = roster_entry(&mut s, "worker", later + 1);
    assert!(entry["session"]["recent_takeover"].is_null());
}

#[test]
fn activity_keeps_a_binding_live() {
    let mut s = board();
    ok(&mut s, "worker", Some("claude:a"), "join", json!({}), NOW);
    let mid = NOW + IDENTITY_TTL_MS - 1_000;
    ok(
        &mut s,
        "worker",
        Some("claude:a"),
        "heartbeat",
        json!({}),
        mid,
    );
    let err = run(
        &mut s,
        "worker",
        Some("claude:b"),
        "join",
        json!({}),
        mid + 2_000,
    );
    assert_eq!(err.unwrap_err(), "identity_busy");
}

#[test]
fn leave_ends_the_binding_and_one_session_may_hold_several_names() {
    let mut s = board();
    ok(&mut s, "claude", Some("claude:me"), "join", json!({}), NOW);
    ok(
        &mut s,
        "fixture-claude",
        Some("claude:me"),
        "join",
        json!({}),
        NOW,
    );
    ok(
        &mut s,
        "fixture-claude",
        Some("claude:me"),
        "leave",
        json!({}),
        NOW + 1_000,
    );
    // The other name stays bound; the left name is free immediately.
    assert_eq!(
        run(
            &mut s,
            "claude",
            Some("claude:other"),
            "join",
            json!({}),
            NOW + 2_000
        )
        .unwrap_err(),
        "identity_busy"
    );
    ok(
        &mut s,
        "fixture-claude",
        Some("claude:other"),
        "join",
        json!({}),
        NOW + 2_000,
    );
}

#[test]
fn legacy_callers_without_a_session_still_work() {
    let mut s = board();
    ok(&mut s, "worker", None, "join", json!({}), NOW);
    ok(
        &mut s,
        "worker",
        None,
        "post",
        json!({"title":"t","summary":"s"}),
        NOW,
    );
    let entry = roster_entry(&mut s, "worker", NOW);
    assert!(entry["session"]["bound"].is_null());
}

#[test]
fn malformed_sessions_are_rejected() {
    let mut s = board();
    for bad in ["", "has space", "semi;colon", &"x".repeat(129)] {
        assert_eq!(
            run(&mut s, "worker", Some(bad), "join", json!({}), NOW).unwrap_err(),
            "invalid",
            "{bad:?}"
        );
    }
}

fn card(s: &mut Store) -> (Value, Value) {
    for who in ["codex", "claude"] {
        ok(s, who, None, "join", json!({"topics": []}), NOW);
    }
    let posted = ok(
        s,
        "codex",
        None,
        "post",
        json!({"title":"Stream fix","summary":"ready","kind":"task","assignee":"codex"}),
        NOW,
    );
    let objection = ok(
        s,
        "claude",
        None,
        "annotate",
        json!({"id": posted["card"]["id"], "kind": "objection", "body": "Budget band kills the stream"}),
        NOW,
    )["follow_up"]
        .clone();
    (posted["card"].clone(), objection)
}

#[test]
fn resolving_over_an_open_objection_is_refused() {
    let mut s = board();
    let (c, objection) = card(&mut s);
    let err = run(
        &mut s,
        "codex",
        None,
        "patch",
        json!({"id": c["id"], "expect": c["rev"], "status": "resolved"}),
        NOW,
    );
    assert_eq!(err.unwrap_err(), "open_objections");
    // Resolving the objection first unblocks it.
    ok(
        &mut s,
        "claude",
        None,
        "patch",
        json!({"id": objection["id"], "expect": objection["rev"], "status": "resolved"}),
        NOW,
    );
    let head = ok(&mut s, "codex", None, "show", json!({"id": c["id"]}), NOW)["card"].clone();
    ok(
        &mut s,
        "codex",
        None,
        "patch",
        json!({"id": c["id"], "expect": head["rev"], "status": "resolved"}),
        NOW,
    );
}

#[test]
fn overriding_an_objection_is_recorded_in_the_thread() {
    let mut s = board();
    let (c, objection) = card(&mut s);
    ok(
        &mut s,
        "codex",
        None,
        "patch",
        json!({"id": c["id"], "expect": c["rev"], "status": "resolved", "over_objection": "User accepted the risk"}),
        NOW,
    );
    let thread = ok(
        &mut s,
        "claude",
        None,
        "show",
        json!({"id": c["id"], "history": true, "compact": true}),
        NOW,
    );
    let last = thread["history"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["actor"], "codex");
    assert_eq!(last["over_objection"], "User accepted the risk");
    assert_eq!(last["open_objections"], json!([objection["id"]]));
    // The objection itself stays open: overriding is not resolving it.
    assert_eq!(thread["follow_ups"][0]["id"], objection["id"]);
}

#[test]
fn questions_superseding_and_withdrawing_are_not_gated() {
    let mut s = board();
    let (c, _) = card(&mut s);
    // Withdrawing or superseding does not claim the work is right.
    ok(
        &mut s,
        "codex",
        None,
        "patch",
        json!({"id": c["id"], "expect": c["rev"], "status": "withdrawn"}),
        NOW,
    );
    // A plain question does not gate resolution.
    let posted = ok(
        &mut s,
        "codex",
        None,
        "post",
        json!({"title":"Other","summary":"s","kind":"task","assignee":"codex"}),
        NOW,
    );
    ok(
        &mut s,
        "claude",
        None,
        "annotate",
        json!({"id": posted["card"]["id"], "kind": "question", "body": "Which budget?"}),
        NOW,
    );
    let head = ok(
        &mut s,
        "codex",
        None,
        "show",
        json!({"id": posted["card"]["id"]}),
        NOW,
    )["card"]
        .clone();
    ok(
        &mut s,
        "codex",
        None,
        "patch",
        json!({"id": head["id"], "expect": head["rev"], "status": "resolved"}),
        NOW,
    );
}
