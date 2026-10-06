//! The board's side of a keepalive (docs/design/keepalive.md, K1): the
//! conversation comes from the bound session, the companion session binds
//! beside the terminal's, a Monitor or drive already owning the wake is
//! refused, and stop requests and the daily budget are recorded.
use fray::{keepalive, model::Request, store::Store};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000_000;
const DAY: i64 = 86_400_000;

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

fn begin(store: &mut Store, actor: &str, session: Option<&str>, at: i64) -> Result<Value, String> {
    let req = Request::new("keepalive_start", actor, json!({"cwd":"/r"}))
        .with_session(session.map(str::to_owned));
    store
        .keepalive_begin(&req, "/r", "/h/keepalive/a.log", 1000, at)
        .map_err(|e| e.code)
}

fn status(store: &mut Store, actor: &str, at: i64) -> Value {
    ok(store, actor, None, "keepalive_status", json!({}), at)
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

/// What a keepalive's drive records when it begins.
fn drive_begins(store: &mut Store, at: i64) {
    ok(store, "alice", Some("keepalive:c1"), "join", json!({}), at);
    ok(
        store,
        "alice",
        Some("keepalive:c1"),
        "controller",
        json!({"run_id":"r1","state":"waiting","begin":true,
            "detail":{"host":"claude","keepalive":{"session":"keepalive:c1","companion":"claude:c1","fork":null,"paused":null}}}),
        at,
    );
}

fn board() -> Store {
    let mut s = Store::memory().unwrap();
    ok(&mut s, "observer", None, "join", json!({}), NOW);
    ok(&mut s, "alice", Some("claude:c1"), "join", json!({}), NOW);
    // K2 requires the terminal's UserPromptSubmit hook before a keepalive
    // starts; this completed synthetic turn leaves it available to run.
    ok(
        &mut s,
        "alice",
        Some("claude:c1"),
        "terminal_turn",
        json!({"turn":"begin"}),
        NOW,
    );
    ok(
        &mut s,
        "alice",
        Some("claude:c1"),
        "terminal_turn",
        json!({"turn":"end"}),
        NOW,
    );
    s
}

fn team(store: &mut Store, summary: &str) {
    ok(
        store,
        "owner",
        None,
        "owner_decide",
        json!({"title":"Team","summary":summary,"pin":true}),
        NOW,
    );
}

#[test]
fn only_the_pinned_owner_team_card_supplies_a_validated_model_and_budget() {
    let mut s = board();
    team(
        &mut s,
        "keepalive model: codex-synthetic\nkeepalive budget: 12345",
    );
    assert_eq!(
        keepalive::start_options(&s.conn, NOW).unwrap(),
        keepalive::StartOptions {
            model: Some("codex-synthetic".into()),
            budget: 12_345,
        }
    );
    let started = begin(&mut s, "alice", Some("claude:c1"), NOW + 1).unwrap();
    assert_eq!(started["model"], "codex-synthetic");

    let mut duplicate = board();
    team(&mut duplicate, "keepalive model: one\nkeepalive model: two");
    assert_eq!(
        keepalive::start_options(&duplicate.conn, NOW)
            .unwrap_err()
            .code,
        "invalid"
    );
    let mut invalid = board();
    team(&mut invalid, "keepalive budget: 0");
    assert_eq!(
        keepalive::start_options(&invalid.conn, NOW)
            .unwrap_err()
            .code,
        "invalid"
    );
}

#[test]
fn the_conversation_is_the_bound_sessions_never_a_request_field() {
    let mut s = board();
    let started = begin(&mut s, "alice", Some("claude:c1"), NOW + 1).unwrap();
    assert_eq!(started["state"], "starting");
    assert_eq!(started["host"], "claude");
    assert_eq!(started["companion"], "claude:c1");
    assert_eq!(started["session"], "keepalive:c1");
    assert_eq!(started["usage"]["budget"], 1000);
    // Starting again while it starts or runs reports it, starting nothing.
    let again = begin(&mut s, "alice", Some("claude:c1"), NOW + 2).unwrap();
    assert_eq!(again["already_running"], true);
}

#[test]
fn unbound_foreign_and_non_host_sessions_are_refused() {
    let mut s = board();
    assert_eq!(
        begin(&mut s, "alice", None, NOW + 1).unwrap_err(),
        "session_required"
    );
    // Another live session cannot start one for a name it does not hold.
    assert_eq!(
        begin(&mut s, "alice", Some("claude:other"), NOW + 1).unwrap_err(),
        "identity_busy"
    );
    ok(&mut s, "bob", Some("fray:managed"), "join", json!({}), NOW);
    assert_eq!(
        begin(&mut s, "bob", Some("fray:managed"), NOW + 1).unwrap_err(),
        "keepalive_host"
    );
    // A conversation id that could read as a flag never reaches a command.
    ok(&mut s, "carol", Some("codex:-x"), "join", json!({}), NOW);
    assert_eq!(
        begin(&mut s, "carol", Some("codex:-x"), NOW + 1).unwrap_err(),
        "invalid"
    );
    assert_eq!(
        begin(&mut s, "dave", Some("claude:d1"), NOW + 1).unwrap_err(),
        "not_joined"
    );
    assert_eq!(status(&mut s, "alice", NOW + 2)["state"], "off");
}

#[test]
fn an_armed_monitor_or_a_live_drive_already_owns_the_wake() {
    let mut s = board();
    s.listener_begin(
        "alice",
        "run",
        "conn",
        &json!({"selection":"involved","activation":{"mode":"native-monitor","expires_ms":null}})
            .to_string(),
        NOW,
    )
    .unwrap();
    assert_eq!(
        begin(&mut s, "alice", Some("claude:c1"), NOW + 1).unwrap_err(),
        "monitor_armed"
    );
    s.listener_end("alice", "conn").unwrap();
    ok(
        &mut s,
        "alice",
        Some("claude:c1"),
        "controller",
        json!({"run_id":"drive","state":"waiting","begin":true}),
        NOW + 2,
    );
    assert_eq!(
        begin(&mut s, "alice", Some("claude:c1"), NOW + 3).unwrap_err(),
        "controller_busy"
    );
}

#[test]
fn the_companion_binds_beside_the_terminal_and_survives_its_clear() {
    let mut s = board();
    // Before the daemon starts it, a keepalive session is just a collision.
    assert_eq!(
        run(
            &mut s,
            "alice",
            Some("keepalive:c1"),
            "heartbeat",
            json!({}),
            NOW + 1
        )
        .unwrap_err(),
        "keepalive_session"
    );
    begin(&mut s, "alice", Some("claude:c1"), NOW + 1).unwrap();
    drive_begins(&mut s, NOW + 2);
    // Both write, alternately, without displacing each other.
    ok(
        &mut s,
        "alice",
        Some("claude:c1"),
        "heartbeat",
        json!({}),
        NOW + 3,
    );
    ok(
        &mut s,
        "alice",
        Some("keepalive:c1"),
        "heartbeat",
        json!({}),
        NOW + 4,
    );
    ok(
        &mut s,
        "alice",
        Some("claude:c1"),
        "heartbeat",
        json!({}),
        NOW + 5,
    );
    // A third live session is still refused, and only that keepalive binds.
    assert_eq!(
        run(
            &mut s,
            "alice",
            Some("claude:c9"),
            "join",
            json!({}),
            NOW + 6
        )
        .unwrap_err(),
        "identity_busy"
    );
    assert_eq!(
        run(
            &mut s,
            "alice",
            Some("keepalive:c9"),
            "heartbeat",
            json!({}),
            NOW + 6
        )
        .unwrap_err(),
        "keepalive_session"
    );
    // The terminal's /clear rebinds both the interactive side and the
    // keepalive's next fork source.
    let cleared = ok(
        &mut s,
        "alice",
        Some("claude:c2"),
        "join",
        json!({"takeover":true,"continued":"clear"}),
        NOW + 7,
    );
    assert_eq!(cleared["session_replaced"]["session"], "claude:c1");
    ok(
        &mut s,
        "alice",
        Some("keepalive:c1"),
        "heartbeat",
        json!({}),
        NOW + 8,
    );
    assert_eq!(
        run(
            &mut s,
            "alice",
            Some("claude:c1"),
            "heartbeat",
            json!({}),
            NOW + 9
        )
        .unwrap_err(),
        "identity_busy"
    );
    let entry = roster_entry(&mut s, "alice", NOW + 10);
    assert_eq!(entry["session"]["bound"]["session"], "claude:c2");
    assert_eq!(entry["keepalive"]["state"], "keepalive");
    assert_eq!(entry["keepalive"]["companion"], "claude:c2");
    assert_eq!(status(&mut s, "alice", NOW + 10)["state"], "keepalive");
}

#[test]
fn the_keepalive_does_not_renew_the_names_peer_generation() {
    let mut s = board();
    begin(&mut s, "alice", Some("claude:c1"), NOW + 1).unwrap();
    let generation = |s: &mut Store| -> i64 {
        s.conn
            .query_row(
                "SELECT generation FROM peer_generations WHERE agent='alice'",
                [],
                |r| r.get(0),
            )
            .unwrap()
    };
    let before = generation(&mut s);
    drive_begins(&mut s, NOW + 2);
    for at in 3..6 {
        ok(
            &mut s,
            "alice",
            Some("keepalive:c1"),
            "heartbeat",
            json!({}),
            NOW + at,
        );
        ok(
            &mut s,
            "alice",
            Some("claude:c1"),
            "heartbeat",
            json!({}),
            NOW + at,
        );
    }
    assert_eq!(generation(&mut s), before);
}

#[test]
fn a_stop_request_reaches_the_drive_and_shows_as_stopping() {
    let mut s = board();
    begin(&mut s, "alice", Some("claude:c1"), NOW + 1).unwrap();
    drive_begins(&mut s, NOW + 2);
    let beat = json!({"run_id":"r1","state":"waiting"});
    let reply = ok(
        &mut s,
        "alice",
        Some("keepalive:c1"),
        "controller",
        beat.clone(),
        NOW + 3,
    );
    assert_eq!(reply["stop_requested"], false);
    assert_eq!(
        ok(
            &mut s,
            "alice",
            Some("claude:c1"),
            "keepalive_stop",
            json!({}),
            NOW + 4
        )["state"],
        "stopping"
    );
    assert!(s.keepalive_stop_requested("alice", "keepalive:c1").unwrap());
    let reply = ok(
        &mut s,
        "alice",
        Some("keepalive:c1"),
        "controller",
        beat,
        NOW + 5,
    );
    assert_eq!(reply["stop_requested"], true);
    assert_eq!(
        roster_entry(&mut s, "alice", NOW + 5)["keepalive"]["state"],
        "stopping"
    );
    ok(
        &mut s,
        "alice",
        Some("keepalive:c1"),
        "controller",
        json!({"run_id":"r1","state":"stopped","reason":"stopped"}),
        NOW + 6,
    );
    let stopped = status(&mut s, "alice", NOW + 7);
    assert_eq!(
        (stopped["state"].as_str(), stopped["reason"].as_str()),
        (Some("stopped"), Some("stopped"))
    );
    assert!(roster_entry(&mut s, "alice", NOW + 7)["keepalive"].is_null());
    // A name without one has nothing to stop.
    ok(&mut s, "bob", Some("claude:b1"), "join", json!({}), NOW);
    assert_eq!(
        run(
            &mut s,
            "bob",
            Some("claude:b1"),
            "keepalive_stop",
            json!({}),
            NOW + 8
        )
        .unwrap_err(),
        "not_found"
    );
}

#[test]
fn leave_from_the_terminal_stops_its_keepalive() {
    let mut s = board();
    begin(&mut s, "alice", Some("claude:c1"), NOW + 1).unwrap();
    drive_begins(&mut s, NOW + 2);
    ok(
        &mut s,
        "alice",
        Some("claude:c1"),
        "leave",
        json!({}),
        NOW + 3,
    );
    assert!(s.keepalive_stop_requested("alice", "keepalive:c1").unwrap());
}

#[test]
fn the_daily_budget_counts_reported_input_and_pauses_visibly() {
    let mut s = board();
    begin(&mut s, "alice", Some("claude:c1"), NOW + 1).unwrap();
    drive_begins(&mut s, NOW + 2);
    // Only the keepalive itself reports its usage.
    assert_eq!(
        run(
            &mut s,
            "alice",
            Some("claude:c1"),
            "keepalive_usage",
            json!({"input_tokens":5}),
            NOW + 3
        )
        .unwrap_err(),
        "keepalive_session"
    );
    let usage = ok(
        &mut s,
        "alice",
        Some("keepalive:c1"),
        "keepalive_usage",
        json!({"input_tokens":600}),
        NOW + 3,
    );
    assert_eq!(
        (
            usage["input_tokens"].as_i64(),
            usage["over_budget"].as_bool()
        ),
        (Some(600), Some(false))
    );
    let usage = ok(
        &mut s,
        "alice",
        Some("keepalive:c1"),
        "keepalive_usage",
        json!({"input_tokens":400}),
        NOW + 4,
    );
    assert_eq!(
        (
            usage["input_tokens"].as_i64(),
            usage["over_budget"].as_bool()
        ),
        (Some(1000), Some(true))
    );
    // Over budget the drive marks itself paused: visible, and not wakeable.
    ok(
        &mut s,
        "alice",
        Some("keepalive:c1"),
        "controller",
        json!({"run_id":"r1","state":"waiting",
            "detail":{"host":"claude","keepalive":{"session":"keepalive:c1","companion":"claude:c1","paused":"budget"}}}),
        NOW + 5,
    );
    assert_eq!(status(&mut s, "alice", NOW + 6)["state"], "paused");
    let entry = roster_entry(&mut s, "alice", NOW + 6);
    assert_eq!(entry["keepalive"]["paused"], "budget");
    assert_ne!(entry["reachability"], "wakeable");
    // A new day starts from nothing; a restart the same day would not.
    let tomorrow = (NOW / DAY + 1) * DAY + 1;
    ok(
        &mut s,
        "alice",
        Some("keepalive:c1"),
        "heartbeat",
        json!({}),
        tomorrow,
    );
    let fresh = status(&mut s, "alice", tomorrow)["usage"].clone();
    assert_eq!(
        (
            fresh["input_tokens"].as_i64(),
            fresh["over_budget"].as_bool()
        ),
        (Some(0), Some(false))
    );
}

#[test]
fn a_keepalive_that_never_began_stops_counting_as_starting() {
    let mut s = board();
    begin(&mut s, "alice", Some("claude:c1"), NOW + 1).unwrap();
    assert_eq!(status(&mut s, "alice", NOW + 2)["state"], "starting");
    assert_eq!(status(&mut s, "alice", NOW + 60_000)["state"], "stopped");
    // So it can be started again.
    let again = begin(&mut s, "alice", Some("claude:c1"), NOW + 60_001).unwrap();
    assert!(again.get("already_running").is_none());
    s.keepalive_abort("alice", NOW + 60_002).unwrap();
    assert_eq!(status(&mut s, "alice", NOW + 60_003)["state"], "stopped");
}

#[test]
fn a_stopping_keepalive_is_not_wakeable() {
    let mut s = board();
    begin(&mut s, "alice", Some("claude:c1"), NOW + 1).unwrap();
    drive_begins(&mut s, NOW + 2);
    assert_eq!(
        roster_entry(&mut s, "alice", NOW + 3)["reachability"],
        "wakeable"
    );
    ok(
        &mut s,
        "alice",
        Some("claude:c1"),
        "keepalive_stop",
        json!({}),
        NOW + 4,
    );
    assert_ne!(
        roster_entry(&mut s, "alice", NOW + 5)["reachability"],
        "wakeable"
    );
}

#[test]
fn the_keepalives_waits_leave_the_terminals_wait_row_alone() {
    let mut s = board();
    begin(&mut s, "alice", Some("claude:c1"), NOW + 1).unwrap();
    drive_begins(&mut s, NOW + 2);
    s.touch("alice", Some("claude:c1"), None, NOW + 3).unwrap();
    s.touch("alice", Some("keepalive:c1"), Some("w1"), NOW + 4)
        .unwrap();
    let held: String = s
        .conn
        .query_row(
            "SELECT session FROM session_waits WHERE agent='alice' AND session='claude:c1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(held, "claude:c1");
    // A keepalive's controller, rather than a lingering wake row, establishes
    // wakeability so terminal-busy deferral cannot be bypassed.
    let woken: i64 = s
        .conn
        .query_row(
            "SELECT count(*) FROM wake_waits WHERE agent='alice'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(woken, 0);
}

#[test]
fn a_busy_terminal_defers_its_keepalive_until_a_nonblocking_end() {
    let mut s = board();
    begin(&mut s, "alice", Some("claude:c1"), NOW + 1).unwrap();
    drive_begins(&mut s, NOW + 2);
    ok(
        &mut s,
        "alice",
        Some("claude:c1"),
        "terminal_turn",
        json!({"turn":"begin"}),
        NOW + 3,
    );
    let deferred = status(&mut s, "alice", NOW + 4);
    assert_eq!(deferred["state"], "deferred");
    assert_eq!(deferred["deferred"], "terminal busy");
    ok(
        &mut s,
        "alice",
        Some("claude:c1"),
        "terminal_turn",
        json!({"turn":"end"}),
        NOW + 5,
    );
    assert_eq!(status(&mut s, "alice", NOW + 6)["state"], "keepalive");
}

#[test]
fn away_actions_are_reserved_until_the_hook_reports_them_after_output() {
    let mut s = board();
    begin(&mut s, "alice", Some("claude:c1"), NOW + 1).unwrap();
    s.conn
        .execute(
            "INSERT INTO keepalive_actions(event_seq,agent,card_id,kind,follow_up) VALUES(999,'alice',42,'answer',NULL)",
            [],
        )
        .unwrap();
    let prompt = ok(
        &mut s,
        "alice",
        Some("claude:c1"),
        "terminal_turn",
        json!({"turn":"begin"}),
        NOW + 2,
    );
    assert_eq!(prompt["away"]["actions"][0]["card"], 42);
    assert!(!s
        .conn
        .query_row(
            "SELECT reported FROM keepalive_actions WHERE event_seq=999",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
    ok(
        &mut s,
        "alice",
        Some("claude:c1"),
        "terminal_turn",
        json!({"turn":"active","report_away":[999]}),
        NOW + 3,
    );
    assert!(s
        .conn
        .query_row(
            "SELECT reported FROM keepalive_actions WHERE event_seq=999",
            [],
            |r| r.get::<_, bool>(0)
        )
        .unwrap());
}

#[test]
fn terminal_begin_wins_the_atomic_keepalive_claim_before_any_child_can_start() {
    let mut s = board();
    begin(&mut s, "alice", Some("claude:c1"), NOW + 1).unwrap();
    drive_begins(&mut s, NOW + 2);
    ok(
        &mut s,
        "alice",
        Some("claude:c1"),
        "terminal_turn",
        json!({"turn":"begin"}),
        NOW + 3,
    );
    assert_eq!(
        run(
            &mut s,
            "alice",
            Some("keepalive:c1"),
            "keepalive_claim",
            json!({"run_id":"r1","companion":"claude:c1","prompt_generation":2,"detail":{"keepalive":{"session":"keepalive:c1"},"presented":[{"id":7,"through_seq":9}]}}),
            NOW + 4,
        )
        .unwrap_err(),
        "terminal_busy"
    );
    assert_eq!(
        s.conn
            .query_row(
                "SELECT state FROM controllers WHERE agent='alice'",
                [],
                |r| r.get::<_, String>(0)
            )
            .unwrap(),
        "waiting"
    );
}
