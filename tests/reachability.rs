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
    s.touch("helper", None, None, NOW + 40 * MIN).unwrap();
    assert_eq!(
        reach(&mut s, NOW + 40 * MIN).0,
        "present",
        "a filtered wait (e.g. --card) is present, not wakeable"
    );
    s.touch("helper", None, Some("w1"), NOW + 41 * MIN).unwrap();
    assert_eq!(reach(&mut s, NOW + 41 * MIN).0, "wakeable");
    // Each wait is its own row: a concurrent filtered wait, or a second
    // unfiltered wait that ends, leaves the first one armed.
    s.touch("helper", None, None, NOW + 41 * MIN).unwrap();
    s.touch("helper", None, Some("w2"), NOW + 41 * MIN).unwrap();
    s.wait_ended("w2").unwrap();
    assert_eq!(reach(&mut s, NOW + 41 * MIN).0, "wakeable");
    s.wait_ended("w1").unwrap();
    assert_eq!(
        reach(&mut s, NOW + 41 * MIN).0,
        "present",
        "a wait that ended wakes no one"
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
    // Passing over the author is said, with the way back.
    let notice = q["notice"].as_str().unwrap();
    assert!(
        notice.contains("helper (present, nothing armed) was passed over for steward"),
        "{notice}"
    );
    assert!(notice.contains("--assignee"), "{notice}");
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

/// R5: idle readiness names why an agent with asks addressed to it is not
/// covered: unarmed, lapsed, or a one-shot listener to rearm. Leaving is
/// none of these.
#[test]
fn a_lapse_is_told_apart_from_a_rearm_and_from_leaving() {
    let lapse = |s: &mut Store, now: i64| {
        at(s, "helper", "brief", json!({}), now)["idle_readiness"]["lapse"].clone()
    };
    let mut s = board();
    // No ask yet: nothing to say.
    assert!(lapse(&mut s, NOW).is_null());
    at(
        &mut s,
        "alice",
        "send",
        json!({"to":"helper","body":"review?","ask":true}),
        NOW,
    );
    let l = lapse(&mut s, NOW + MIN);
    assert_eq!(l["kind"], "unarmed", "{l}");
    assert!(l["arm"].as_str().unwrap().ends_with("arm"), "{l}");
    // Armed: covered.
    listen(&s, monitor(NOW + 30 * MIN), NOW + 2 * MIN);
    assert!(lapse(&mut s, NOW + 2 * MIN).is_null());
    // The declared activation expired: a lapse.
    listen(&s, monitor(NOW + 30 * MIN), NOW + 31 * MIN);
    let l = lapse(&mut s, NOW + 31 * MIN);
    assert_eq!(l["kind"], "lapsed", "{l}");
    s.listener_end("helper", "conn").unwrap();
    // A one-shot listener that returned: rearm, not a lapse.
    let mut once = monitor(NOW + 60 * MIN);
    once["once"] = json!(true);
    once["activation"]["mode"] = json!("background-completion");
    listen(&s, once, NOW + 32 * MIN);
    s.listener_end("helper", "conn").unwrap();
    let l = lapse(&mut s, NOW + 33 * MIN);
    assert_eq!(l["kind"], "rearm", "{l}");
    // The hint rearms the way this host wakes (#78).
    assert_eq!(
        l["arm"], "fray --as helper arm --host background-completion",
        "{l}"
    );
    // An unfiltered wait in progress covers the agent: no lapse.
    s.touch("helper", None, Some("w1"), NOW + 33 * MIN).unwrap();
    assert!(lapse(&mut s, NOW + 33 * MIN).is_null());
    s.wait_ended("w1").unwrap();
    // Having left, helper is told nothing.
    at(&mut s, "helper", "leave", json!({}), NOW + 34 * MIN);
    let left = s
        .execute_at(&Request::new("brief", "helper", json!({})), NOW + 34 * MIN)
        .unwrap_err();
    assert_eq!(left.code, "not_joined");
}

/// The lapse counts only asks the agent has not answered: replying, even
/// before the asker resolves the card, ends the reminder at Stop.
#[test]
fn an_answered_ask_no_longer_counts_toward_the_lapse() {
    let mut s = board();
    let sent = at(
        &mut s,
        "alice",
        "send",
        json!({"to":"helper","body":"review?","ask":true}),
        NOW,
    );
    let lapse = |s: &mut Store, now: i64| {
        at(s, "helper", "brief", json!({}), now)["idle_readiness"]["lapse"].clone()
    };
    assert_eq!(lapse(&mut s, NOW + MIN)["open_asks_to_you"], 1);
    at(
        &mut s,
        "helper",
        "annotate",
        json!({"id":sent["card"]["id"],"body":"done, see 1a2b3c"}),
        NOW + 2 * MIN,
    );
    assert!(lapse(&mut s, NOW + 3 * MIN).is_null());
}

/// R5 is based on the creation event, and a Mote reply is authoritative only
/// when the Mote sync observes it.
#[test]
fn kind_patches_and_local_mote_annotations_do_not_clear_a_lapse() {
    let mut s = board();
    let sent = at(
        &mut s,
        "alice",
        "send",
        json!({"to":"helper","body":"review?","ask":true}),
        NOW,
    );
    at(
        &mut s,
        "alice",
        "patch",
        json!({"id":sent["card"]["id"],"expect":1,"kind":"note"}),
        NOW + MIN,
    );
    let lapse = |s: &mut Store| {
        at(s, "helper", "brief", json!({}), NOW + 2 * MIN)["idle_readiness"]["lapse"].clone()
    };
    assert_eq!(lapse(&mut s)["open_asks_to_you"], 1);

    at(
        &mut s,
        "alice",
        "mote_bind",
        json!({"store":"/r/.mote","store_id":"st-A"}),
        NOW + 2 * MIN,
    );
    at(
        &mut s,
        "alice",
        "mote_requests_sync",
        json!({"store_id":"st-A","requests":[{"msg_id":"msg-1","recipient":"helper","from":"alice","state":"open","body":"review?"}]}),
        NOW + 2 * MIN,
    );
    let mote = at(&mut s, "helper", "inbox", json!({}), NOW + 2 * MIN);
    let mote_id = mote["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["card"]["author"] == "mote")
        .unwrap()["card"]["id"]
        .clone();
    at(
        &mut s,
        "helper",
        "annotate",
        json!({"id":mote_id,"body":"locally noted"}),
        NOW + 3 * MIN,
    );
    assert_eq!(lapse(&mut s)["open_asks_to_you"], 2);
}
