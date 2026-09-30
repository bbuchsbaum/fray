//! Epic child 7: `fray stats` and `fray friction`, derived read-only from the
//! durable event log.
use fray::{model::Request, store::Store};
use serde_json::{json, Value};
use std::process::Command;

const NOW: i64 = 1_800_000_000_000;
const MIN: i64 = 60_000;

fn at(s: &mut Store, actor: &str, op: &str, args: Value, t: i64) -> Value {
    s.execute_at(&Request::new(op, actor, args), t)
        .unwrap_or_else(|e| panic!("{op}: {} {}", e.code, e.message))
}

fn board() -> Store {
    let mut s = Store::memory().unwrap();
    for who in ["claude", "codex", "deepseek"] {
        at(&mut s, who, "join", json!({"topics": []}), NOW);
    }
    s
}

fn stats(s: &mut Store, window_ms: Option<i64>, t: i64) -> Value {
    at(s, "claude", "stats", json!({"window_ms": window_ms}), t)["stats"].clone()
}

fn ask(s: &mut Store, from: &str, to: &str, body: &str, t: i64) -> i64 {
    at(
        s,
        from,
        "send",
        json!({"to": to, "body": body, "ask": true}),
        t,
    )["card"]["id"]
        .as_i64()
        .unwrap()
}

fn resolve(s: &mut Store, actor: &str, id: i64, t: i64) -> Value {
    let rev = at(s, actor, "show", json!({"id": id}), t)["card"]["rev"].clone();
    at(
        s,
        actor,
        "patch",
        json!({"id": id, "expect": rev, "status": "resolved"}),
        t,
    )
}

#[test]
fn an_empty_board_reports_nothing_measured_rather_than_zero_latency() {
    let mut s = board();
    let st = stats(&mut s, None, NOW);
    assert_eq!(st["asks"]["created"], 0);
    assert_eq!(st["asks"]["first_response"]["n"], 0);
    assert!(st["asks"]["first_response"]["p50_ms"].is_null());
    assert!(st["asks"]["oldest_open_age_ms"].is_null());
    assert_eq!(st["window"]["all_history"], true);
    // What the store cannot measure is named, not silently zero.
    let unavailable: Vec<&str> = st["unavailable"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| u["metric"].as_str().unwrap())
        .collect();
    assert!(unavailable.iter().any(|m| m.contains("wake")));
    assert!(unavailable.iter().any(|m| m.contains("acknowledgment")));
    assert!(unavailable.iter().any(|m| m.contains("reviews")));
}

#[test]
fn response_and_resolution_times_replay_from_the_log() {
    let mut s = board();
    let a = ask(&mut s, "claude", "codex", "Is the parser total?", NOW);
    // The author's own follow-up is not a response.
    at(
        &mut s,
        "claude",
        "annotate",
        json!({"id": a, "body": "context"}),
        NOW + MIN,
    );
    at(
        &mut s,
        "codex",
        "annotate",
        json!({"id": a, "kind": "answer", "body": "Yes"}),
        NOW + 3 * MIN,
    );
    at(
        &mut s,
        "deepseek",
        "annotate",
        json!({"id": a, "body": "agree"}),
        NOW + 4 * MIN,
    );
    resolve(&mut s, "claude", a, NOW + 10 * MIN);
    let b = ask(&mut s, "codex", "claude", "Second?", NOW + 20 * MIN);
    let st = stats(&mut s, None, NOW + 60 * MIN);
    let asks = &st["asks"];
    assert_eq!(asks["created"], 2);
    assert_eq!(asks["responded"], 1);
    assert_eq!(asks["resolved"], 1);
    assert_eq!(asks["open"], 1);
    assert_eq!(asks["first_response"]["p50_ms"], 3 * MIN);
    assert_eq!(asks["resolution"]["max_ms"], 10 * MIN);
    assert_eq!(asks["oldest_open_unanswered_age_ms"], 40 * MIN);
    // The unanswered ask leads the friction report.
    let f = at(&mut s, "claude", "friction", json!({}), NOW + 60 * MIN)["friction"].clone();
    assert_eq!(f["unanswered_asks"]["total"], 1);
    assert_eq!(f["unanswered_asks"]["items"][0]["id"], b);
}

#[test]
fn objections_count_separately_and_overrides_are_visible() {
    let mut s = board();
    let task = at(
        &mut s,
        "claude",
        "post",
        json!({"title": "Land the parser", "summary": "ready", "kind": "task"}),
        NOW,
    )["card"]["id"]
        .as_i64()
        .unwrap();
    let objection = at(
        &mut s,
        "codex",
        "annotate",
        json!({"id": task, "kind": "objection", "body": "Empty input panics"}),
        NOW + MIN,
    )["follow_up"]["id"]
        .as_i64()
        .unwrap();
    // A linked question is an ask; an objection is not.
    at(
        &mut s,
        "deepseek",
        "annotate",
        json!({"id": task, "kind": "question", "body": "Which input?"}),
        NOW + MIN,
    );
    let st = stats(&mut s, None, NOW + 2 * MIN);
    assert_eq!(st["objections"]["created"], 1);
    assert_eq!(st["objections"]["open"], 1);
    assert_eq!(st["asks"]["created"], 1);
    // Resolving past the objection is refused, then overridden visibly.
    let rev = at(&mut s, "claude", "show", json!({"id": task}), NOW)["card"]["rev"].clone();
    let refused = s.execute_at(
        &Request::new(
            "patch",
            "claude",
            json!({"id": task, "expect": rev, "status": "resolved"}),
        ),
        NOW + 3 * MIN,
    );
    assert_eq!(refused.unwrap_err().code, "open_objections");
    at(
        &mut s,
        "claude",
        "patch",
        json!({"id": task, "expect": rev, "status": "resolved", "over_objection": "shipping behind a flag"}),
        NOW + 3 * MIN,
    );
    let st = stats(&mut s, None, NOW + 4 * MIN);
    assert_eq!(st["objections"]["overrides"], 1);
    let f = at(&mut s, "claude", "friction", json!({}), NOW + 4 * MIN)["friction"].clone();
    assert_eq!(f["open_objections"]["items"][0]["id"], objection);
    assert_eq!(f["open_objections"]["items"][0]["objector"], "codex");
    // The objector verifies and resolves; resolution time is recorded.
    at(
        &mut s,
        "claude",
        "annotate",
        json!({"id": objection, "kind": "answer", "body": "fixed"}),
        NOW + 5 * MIN,
    );
    resolve(&mut s, "codex", objection, NOW + 6 * MIN);
    let st = stats(&mut s, None, NOW + 7 * MIN);
    assert_eq!(st["objections"]["resolved"], 1);
    assert_eq!(st["objections"]["first_response"]["p50_ms"], 4 * MIN);
    assert_eq!(st["objections"]["resolution"]["p50_ms"], 5 * MIN);
}

#[test]
fn only_reassignment_away_from_someone_counts_as_a_possible_misroute() {
    let mut s = board();
    let id = at(
        &mut s,
        "claude",
        "post",
        json!({"title": "Unowned", "summary": "x", "kind": "task"}),
        NOW,
    )["card"]["id"]
        .as_i64()
        .unwrap();
    let patch = |s: &mut Store, assignee: &str, t: i64| {
        let rev = at(s, "claude", "show", json!({"id": id}), t)["card"]["rev"].clone();
        at(
            s,
            "claude",
            "patch",
            json!({"id": id, "expect": rev, "assignee": assignee}),
            t,
        );
    };
    patch(&mut s, "codex", NOW + MIN); // first assignment: not a misroute
    let st = stats(&mut s, None, NOW + MIN);
    assert_eq!(st["routing"]["reassignments"], 0);
    patch(&mut s, "deepseek", NOW + 2 * MIN);
    patch(&mut s, "codex", NOW + 3 * MIN);
    let st = stats(&mut s, None, NOW + 4 * MIN);
    assert_eq!(st["routing"]["reassignments"], 2);
    assert_eq!(st["routing"]["cards_reassigned"], 1);
    // A window after the reassignments excludes them.
    let st = stats(&mut s, Some(30_000), NOW + 4 * MIN);
    assert_eq!(st["routing"]["reassignments"], 0);
}

#[test]
fn the_window_selects_work_started_within_it() {
    let mut s = board();
    ask(&mut s, "claude", "codex", "old", NOW);
    ask(&mut s, "claude", "codex", "new", NOW + 100 * MIN);
    let st = stats(&mut s, Some(10 * MIN), NOW + 105 * MIN);
    assert_eq!(st["asks"]["created"], 1);
    assert_eq!(st["window"]["all_history"], false);
    assert_eq!(st["window"]["since_ms"], NOW + 95 * MIN);
    // A malformed window is rejected, not treated as all history.
    let bad = s.execute_at(
        &Request::new("stats", "claude", json!({"window_ms": 0})),
        NOW,
    );
    assert_eq!(bad.unwrap_err().code, "invalid");
}

#[test]
fn unacked_attention_is_current_and_shrinks_on_ack() {
    let mut s = board();
    let id = ask(&mut s, "claude", "codex", "ping", NOW);
    let st = stats(&mut s, None, NOW + 5 * MIN);
    let codex = st["attention"]["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["agent"] == "codex")
        .cloned()
        .unwrap();
    assert_eq!(codex["items"], 1);
    assert_eq!(codex["oldest_age_ms"], 5 * MIN);
    let seq = at(&mut s, "codex", "show", json!({"id": id}), NOW)["card"]["last_seq"].clone();
    at(
        &mut s,
        "codex",
        "ack",
        json!({"id": id, "through": seq}),
        NOW + 6 * MIN,
    );
    let st = stats(&mut s, None, NOW + 7 * MIN);
    assert!(st["attention"]["agents"]
        .as_array()
        .unwrap()
        .iter()
        .all(|a| a["agent"] != "codex"));
}

/// What `fray inbox` does: read, then record exactly the receipts it showed.
fn present_inbox(s: &mut Store, actor: &str, t: i64) {
    let page = at(s, actor, "inbox", json!({}), t);
    let receipts: Vec<Value> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["receipt"].clone())
        .collect();
    at(
        s,
        actor,
        "present",
        json!({"source": "inbox", "receipts": receipts}),
        t,
    );
}

#[test]
fn exposure_uses_the_first_time_a_version_was_shown() {
    let mut s = board();
    // An earlier showing establishes codex's retained presentation history.
    // It predates that history, so its own first showing is uncertain and
    // left out.
    ask(&mut s, "claude", "codex", "earlier", NOW - MIN);
    present_inbox(&mut s, "codex", NOW);
    ask(&mut s, "claude", "codex", "look", NOW + MIN);
    // Shown twice; only the first presentation counts.
    present_inbox(&mut s, "codex", NOW + 3 * MIN);
    present_inbox(&mut s, "codex", NOW + 9 * MIN);
    let st = stats(&mut s, None, NOW + 10 * MIN);
    let e = &st["exposure"]["publish_to_first_shown"];
    assert_eq!(e["n"], 1, "{st}");
    assert_eq!(e["max_ms"], 2 * MIN);
}

#[test]
fn a_pruned_first_showing_is_not_replaced_by_a_later_one() {
    // Review O4 of c7ff681: shown at T+1m, then re-shown 33 times from T+60m;
    // the retained "first" showing is an hour late.
    let mut s = board();
    ask(&mut s, "claude", "codex", "look", NOW);
    present_inbox(&mut s, "codex", NOW + MIN);
    for k in 0..33 {
        present_inbox(&mut s, "codex", NOW + 60 * MIN + k * 1000);
    }
    let st = stats(&mut s, None, NOW + 70 * MIN);
    let e = &st["exposure"]["publish_to_first_shown"];
    assert_eq!(e["n"], 0, "an uncertain first showing is left out: {st}");
}

#[test]
fn friction_notes_and_unreachable_requests_are_listed() {
    let mut s = board();
    at(
        &mut s,
        "codex",
        "post",
        json!({"title": "friction: preview truncated my evidence", "summary": "had to refetch", "kind": "note", "topic": "friction", "priority": 3, "tags": ["friction"]}),
        NOW,
    );
    ask(&mut s, "claude", "deepseek", "are you there?", NOW);
    // Hours later nobody has been active or waiting: deepseek cannot be reached.
    let later = NOW + 5 * 3_600_000;
    let f = at(&mut s, "claude", "friction", json!({}), later)["friction"].clone();
    assert_eq!(f["notes"]["total"], 1);
    assert_eq!(f["notes"]["items"][0]["author"], "codex");
    let unreachable = f["unreachable_addressed"]["items"].as_array().unwrap();
    assert!(
        unreachable
            .iter()
            .any(|u| u["agent"] == "deepseek" && u["open_requests"] == 1),
        "{f}"
    );
    assert_eq!(
        f["unanswered_asks"]["items"][0]["assignee_reachable"],
        false
    );
    assert_eq!(stats(&mut s, None, later)["friction_notes"], 1);
}

#[test]
fn stats_and_friction_never_write() {
    let mut s = board();
    let a = ask(&mut s, "claude", "codex", "q", NOW);
    at(&mut s, "codex", "inbox", json!({}), NOW + MIN);
    let before = (s.highwater().unwrap(), s.events(0, 1000).unwrap().len());
    let pending_before = at(&mut s, "codex", "show", json!({"id": a}), NOW)["card"].clone();
    stats(&mut s, None, NOW + 2 * MIN);
    at(&mut s, "codex", "friction", json!({}), NOW + 2 * MIN);
    assert_eq!(
        before,
        (s.highwater().unwrap(), s.events(0, 1000).unwrap().len())
    );
    assert_eq!(
        pending_before,
        at(&mut s, "codex", "show", json!({"id": a}), NOW)["card"]
    );
}

#[test]
fn the_cli_rejects_a_malformed_window_before_contacting_a_daemon() {
    for bad in ["0d", "7", "-1h", "1w", "h", "", "5\u{e9}", "+1d x"] {
        let out = Command::new(env!("CARGO_BIN_EXE_fray"))
            .args([
                "--home",
                "/nonexistent/fray-stats-test",
                "--as",
                "t",
                "stats",
                &format!("--since={bad}"),
            ])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{bad} accepted");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(err.contains("window must be"), "{bad}: {err}");
    }
    // A valid window gets past parsing and fails only for want of a daemon.
    let out = Command::new(env!("CARGO_BIN_EXE_fray"))
        .args([
            "--home",
            "/nonexistent/fray-stats-test",
            "--as",
            "t",
            "stats",
            "--since",
            "90m",
        ])
        .output()
        .unwrap();
    assert!(!String::from_utf8_lossy(&out.stderr).contains("window must be"));
}

#[test]
fn a_bystander_note_is_not_an_answer() {
    // Review O2 of c7ff681.
    let mut s = board();
    let a = ask(&mut s, "claude", "codex", "Is it safe?", NOW);
    at(
        &mut s,
        "deepseek",
        "annotate",
        json!({"id": a, "body": "+1"}),
        NOW + MIN,
    );
    let st = stats(&mut s, None, NOW + 5 * MIN);
    assert_eq!(st["asks"]["responded"], 0, "{st}");
    let f = at(&mut s, "claude", "friction", json!({}), NOW + 5 * MIN)["friction"].clone();
    assert_eq!(f["unanswered_asks"]["total"], 1);
    // The addressee's reply is the response.
    at(
        &mut s,
        "codex",
        "annotate",
        json!({"id": a, "kind": "answer", "body": "yes"}),
        NOW + 2 * MIN,
    );
    let st = stats(&mut s, None, NOW + 5 * MIN);
    assert_eq!(st["asks"]["first_response"]["p50_ms"], 2 * MIN);
    // An unaddressed question can be answered by anyone but its author.
    let open = at(
        &mut s,
        "claude",
        "post",
        json!({"title": "Anyone?", "summary": "x", "kind": "question", "topic": "*"}),
        NOW + 10 * MIN,
    )["card"]["id"]
        .as_i64()
        .unwrap();
    at(
        &mut s,
        "deepseek",
        "annotate",
        json!({"id": open, "kind": "answer", "body": "me"}),
        NOW + 11 * MIN,
    );
    let st = stats(&mut s, None, NOW + 12 * MIN);
    assert_eq!(st["asks"]["responded"], 2, "{st}");
}

#[test]
fn owner_requests_wait_in_the_owner_queue_not_unreachable() {
    // Review O1 of c7ff681: `fray ask-owner` sends a pending ask to the owner.
    let mut s = board();
    at(
        &mut s,
        "claude",
        "send",
        json!({"to": "owner", "body": "May I delete the fixtures?", "ask": true, "pending": true, "priority": 1, "refs": ["ask-owner"]}),
        NOW,
    );
    let f = at(&mut s, "claude", "friction", json!({}), NOW + 3_600_000)["friction"].clone();
    assert!(
        f["unreachable_addressed"]["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|u| u["agent"] != "owner"),
        "{f}"
    );
    let asks = f["unanswered_asks"]["items"].as_array().unwrap();
    assert_eq!(asks[0]["awaiting_owner"], true, "{f}");
    assert!(asks[0]["assignee_reachable"].is_null());
}

#[test]
fn unacked_age_runs_from_the_oldest_unacknowledged_event() {
    // Review O3 of c7ff681: a later third-party event must not reset the age.
    let mut s = board();
    let a = ask(&mut s, "claude", "codex", "status?", NOW);
    at(
        &mut s,
        "deepseek",
        "annotate",
        json!({"id": a, "body": "also curious"}),
        NOW + 50 * MIN,
    );
    let st = stats(&mut s, None, NOW + 60 * MIN);
    let codex = st["attention"]["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["agent"] == "codex")
        .cloned()
        .unwrap();
    assert_eq!(codex["oldest_age_ms"], 60 * MIN, "{st}");
    // After acking the first version, the age restarts at the next unacked event.
    at(
        &mut s,
        "codex",
        "ack",
        json!({"id": a, "through": 1}),
        NOW + 60 * MIN,
    );
    let st = stats(&mut s, None, NOW + 60 * MIN);
    let codex = st["attention"]["agents"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["agent"] == "codex")
        .cloned();
    assert_eq!(codex.unwrap()["oldest_age_ms"], 10 * MIN, "{st}");
}

#[test]
fn agents_who_left_and_muted_cards_are_not_unacked_attention() {
    let mut s = board();
    ask(&mut s, "claude", "codex", "one", NOW);
    let note = at(
        &mut s,
        "claude",
        "post",
        json!({"title": "fyi", "summary": "x", "kind": "note", "topic": "*"}),
        NOW,
    )["card"]["id"]
        .as_i64()
        .unwrap();
    at(&mut s, "deepseek", "mute", json!({"id": note}), NOW + MIN);
    at(&mut s, "codex", "leave", json!({}), NOW + MIN);
    let st = stats(&mut s, None, NOW + 2 * MIN);
    let agents = st["attention"]["agents"].as_array().unwrap();
    assert!(
        agents
            .iter()
            .all(|a| a["agent"] != "codex" && a["agent"] != "deepseek"),
        "{st}"
    );
}

#[test]
fn reopening_clears_resolution_and_kind_and_title_follow_the_card() {
    // Review N1 and N3 of c7ff681.
    let mut s = board();
    let a = ask(&mut s, "claude", "codex", "first", NOW);
    resolve(&mut s, "claude", a, NOW + 10 * MIN);
    let rev = at(&mut s, "claude", "show", json!({"id": a}), NOW)["card"]["rev"].clone();
    at(
        &mut s,
        "claude",
        "patch",
        json!({"id": a, "expect": rev, "status": "open", "title": "renamed"}),
        NOW + 20 * MIN,
    );
    let st = stats(&mut s, None, NOW + 30 * MIN);
    assert_eq!(st["asks"]["resolved"], 0, "{st}");
    assert_eq!(st["asks"]["open"], 1);
    let f = at(&mut s, "claude", "friction", json!({}), NOW + 30 * MIN)["friction"].clone();
    assert_eq!(f["unanswered_asks"]["items"][0]["title"], "renamed", "{f}");
    // A note patched into a question counts as an ask.
    let n = at(
        &mut s,
        "claude",
        "post",
        json!({"title": "maybe", "summary": "x", "kind": "note"}),
        NOW,
    )["card"]["id"]
        .as_i64()
        .unwrap();
    let rev = at(&mut s, "claude", "show", json!({"id": n}), NOW)["card"]["rev"].clone();
    at(
        &mut s,
        "claude",
        "patch",
        json!({"id": n, "expect": rev, "kind": "question"}),
        NOW + MIN,
    );
    assert_eq!(stats(&mut s, None, NOW + 30 * MIN)["asks"]["created"], 2);
}

#[test]
fn a_friction_note_over_the_summary_limit_fails_before_sending() {
    // Review N2 of c7ff681.
    let body = "x".repeat(2500);
    let out = Command::new(env!("CARGO_BIN_EXE_fray"))
        .args([
            "--home",
            "/nonexistent/fray-stats-test",
            "--as",
            "t",
            "friction",
            &body,
        ])
        .output()
        .unwrap();
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("friction note must contain"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
