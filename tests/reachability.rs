//! No silent stalls, R1 (docs/design/no-silent-stalls.md): one reachability
//! test. "Wakeable" means something armed will wake the agent; recent
//! activity alone is only "present".
use fray::{model::*, store::Store};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000_000;
const MIN: i64 = 60_000;

fn at(s: &mut Store, who: &str, op: &str, args: Value, now: i64) -> Value {
    s.execute_at(&Request::new(op, who, args), now).unwrap()
}
fn board() -> Store {
    let mut s = Store::memory().unwrap();
    for who in ["alice", "helper", "steward"] {
        at(&mut s, who, "join", json!({"topics":[]}), NOW);
    }
    s
}
/// What alice's send to helper reports about helper at `now`.
fn reach(s: &mut Store, now: i64) -> (String, Option<String>) {
    let sent = at(
        s,
        "alice",
        "send",
        json!({"to":"helper","body":"review please","ask":true}),
        now,
    );
    (
        sent["reachability"].as_str().unwrap().to_owned(),
        sent["notice"].as_str().map(str::to_owned),
    )
}
fn listen(s: &Store, selection: Value, now: i64) {
    s.listener_begin("helper", "run", "conn", &selection.to_string(), now)
        .unwrap();
}
fn monitor(expires_ms: i64) -> Value {
    json!({"selection":"involved","addressed_to_me":false,"unresolved":false,"kinds":[],"min_priority":null,
        "activation":{"mode":"native-monitor","source":"adapter-declared","expires_ms":expires_ms}})
}

#[test]
fn a_turn_that_ended_minutes_ago_is_present_not_wakeable() {
    let mut s = board();
    // helper was active at NOW; five minutes later nothing is armed.
    let (state, notice) = reach(&mut s, NOW + 5 * MIN);
    assert_eq!(state, "present");
    let notice = notice.unwrap();
    assert!(notice.contains("nothing armed to wake it"), "{notice}");
    // Long after, it is absent.
    let (state, _) = reach(&mut s, NOW + 60 * MIN);
    assert_eq!(state, "absent");
}

#[test]
fn an_armed_monitor_is_wakeable_until_its_declared_expiry() {
    let mut s = board();
    listen(&s, monitor(NOW + 30 * MIN), NOW + MIN);
    let (state, notice) = reach(&mut s, NOW + MIN);
    assert_eq!(state, "wakeable");
    assert!(notice.is_none(), "{notice:?}");
    // The declared activation has expired; the transport is still connected
    // and refreshed, but nothing will wake the host.
    // (a fresh connection carrying the same, now past, declaration).
    listen(&s, monitor(NOW + 30 * MIN), NOW + 31 * MIN);
    let (state, _) = reach(&mut s, NOW + 31 * MIN);
    assert_ne!(state, "wakeable");
}

#[test]
fn a_filtered_or_undeclared_listener_is_not_wakeable() {
    let mut s = board();
    // Filtered to one card: other asks would not wake it.
    let mut filtered = monitor(NOW + 30 * MIN);
    filtered["card_ids"] = json!([1]);
    listen(&s, filtered, NOW);
    assert_eq!(reach(&mut s, NOW + MIN).0, "present");
    s.listener_end("helper", "conn").unwrap();
    // A connected listener with no declared host wake (manual).
    listen(
        &s,
        json!({"selection":"involved","addressed_to_me":false,"unresolved":false,"kinds":[],"min_priority":null}),
        NOW + 2 * MIN,
    );
    assert_eq!(reach(&mut s, NOW + 3 * MIN).0, "present");
}

#[test]
fn a_driven_agent_is_wakeable_even_mid_child() {
    let mut s = board();
    for state in ["waiting", "running"] {
        at(
            &mut s,
            "helper",
            "controller",
            json!({"run_id":"drive-1","state":state,"begin":state == "waiting"}),
            NOW,
        );
        assert_eq!(reach(&mut s, NOW + MIN).0, "wakeable", "{state}");
    }
}

#[test]
fn only_an_unfiltered_wait_in_progress_is_wakeable() {
    let mut s = board();
    s.touch("helper", None, false, NOW + 40 * MIN).unwrap();
    assert_eq!(
        reach(&mut s, NOW + 40 * MIN).0,
        "present",
        "a filtered wait (e.g. --card) is present, not wakeable"
    );
    s.touch("helper", None, true, NOW + 41 * MIN).unwrap();
    assert_eq!(reach(&mut s, NOW + 41 * MIN).0, "wakeable");
    s.wait_returned("helper").unwrap();
    assert_eq!(
        reach(&mut s, NOW + 41 * MIN).0,
        "present",
        "a wait that returned wakes no one"
    );
}

#[test]
fn a_question_goes_to_a_wakeable_party_first_and_names_who_else_is() {
    let mut s = board();
    // steward is driven; helper is merely present.
    at(
        &mut s,
        "steward",
        "controller",
        json!({"run_id":"drive-s","state":"waiting","begin":true}),
        NOW + 5 * MIN,
    );
    let (_, notice) = reach(&mut s, NOW + 5 * MIN);
    let notice = notice.unwrap();
    assert!(notice.contains("wakeable now: steward"), "{notice}");
    // helper (present) authored a task assigned to steward (wakeable). A
    // third party's question would go to the author, but the wakeable
    // assignee is preferred.
    let card = at(
        &mut s,
        "helper",
        "post",
        json!({"kind":"task","topic":"t","title":"T","summary":"x","assignee":"steward"}),
        NOW + 5 * MIN,
    );
    let q = at(
        &mut s,
        "alice",
        "annotate",
        json!({"id":card["card"]["id"],"kind":"question","body":"Which?"}),
        NOW + 6 * MIN,
    );
    assert_eq!(q["follow_up"]["assignee"], "steward", "{q}");
    assert!(q["notice"].is_null(), "{q}");
}

#[test]
fn friction_counts_a_present_but_unarmed_addressee_as_unreachable() {
    let mut s = board();
    reach(&mut s, NOW + 5 * MIN);
    let f = at(&mut s, "alice", "friction", json!({}), NOW + 5 * MIN);
    let items = f["friction"]["unanswered_asks"]["items"]
        .as_array()
        .unwrap();
    assert_eq!(items[0]["assignee_reachable"], false, "{f}");
    assert_eq!(
        f["friction"]["unreachable_addressed"]["items"][0]["agent"], "helper",
        "{f}"
    );
}
