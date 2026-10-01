//! No silent stalls R3: stuck requests escalate to every present steward as
//! a card assigned to them (so an `involved` listener is woken), once
//! board-wide, and settle when the request clears.
use fray::{model::*, store::Store};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000_000;
const MIN: i64 = 60_000;

fn at(s: &mut Store, who: &str, op: &str, args: Value, now: i64) -> Value {
    s.execute_at(&Request::new(op, who, args), now)
        .unwrap_or_else(|e| panic!("{op}: {e:?}"))
}
/// alice asks helper; steward and steward2 are stewards; runner ticks.
fn board() -> Store {
    let mut s = Store::memory().unwrap();
    for who in ["alice", "helper", "runner"] {
        at(&mut s, who, "join", json!({"topics":[]}), NOW);
    }
    for who in ["steward", "steward2"] {
        at(
            &mut s,
            who,
            "join",
            json!({"topics":[],"role":"steward"}),
            NOW,
        );
    }
    s
}
fn heartbeat(s: &mut Store, now: i64) {
    for who in ["alice", "steward", "steward2", "runner"] {
        at(s, who, "heartbeat", json!({}), now);
    }
}
fn ask(s: &mut Store, extra: Value) -> i64 {
    let mut args = json!({"to":"helper","body":"Review 1a2b3c please","ask":true});
    args.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    at(s, "alice", "send", args, NOW)["card"]["id"]
        .as_i64()
        .unwrap()
}
/// What `fray inbox` does: read, then record what was shown.
fn shown(s: &mut Store, who: &str, now: i64) {
    let page = at(s, who, "inbox", json!({"selection":"all"}), now);
    let receipts: Vec<Value> = page["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["receipt"].clone())
        .collect();
    at(
        s,
        who,
        "present",
        json!({"source":"inbox","receipts":receipts}),
        now,
    );
}
fn escalations(s: &mut Store, who: &str, now: i64) -> Vec<Value> {
    at(s, who, "inbox", json!({"selection":"involved"}), now)["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["card"]["author"] == "escalation")
        .map(|i| i["card"].clone())
        .collect()
}

#[test]
fn an_unreachable_ask_escalates_once_to_each_present_steward_after_the_grace() {
    let mut s = board();
    let id = ask(&mut s, json!({}));
    // Within the grace period: nothing.
    heartbeat(&mut s, NOW + 10 * MIN);
    let t = at(&mut s, "runner", "escalate_tick", json!({}), NOW + 10 * MIN);
    assert_eq!(t["created"].as_array().unwrap().len(), 0, "{t}");
    // helper took no turn since NOW; after 16 minutes it is stuck.
    heartbeat(&mut s, NOW + 50 * MIN);
    let t = at(&mut s, "runner", "escalate_tick", json!({}), NOW + 50 * MIN);
    assert_eq!(t["created"].as_array().unwrap().len(), 2, "{t}");
    // Another runner, under another identity, and a retry: nothing new.
    let t = at(&mut s, "alice", "escalate_tick", json!({}), NOW + 51 * MIN);
    assert_eq!(t["created"].as_array().unwrap().len(), 0, "{t}");
    // Each steward's involved inbox has it: an involved listener wakes.
    for who in ["steward", "steward2"] {
        let e = escalations(&mut s, who, NOW + 51 * MIN);
        assert_eq!(e.len(), 1, "{who}: {e:?}");
        assert_eq!(e[0]["assignee"], who);
        let summary = e[0]["summary"].as_str().unwrap();
        assert!(summary.contains(&format!("fray patch {id}")), "{summary}");
        assert!(summary.contains("Nothing re-routes it automatically"));
    }
    // Not the requester or the addressee.
    assert!(escalations(&mut s, "alice", NOW + 51 * MIN).is_empty());
}

#[test]
fn a_wakeable_or_shown_addressee_is_not_unreachable() {
    let mut s = board();
    ask(&mut s, json!({}));
    // helper is driven: wakeable.
    at(
        &mut s,
        "helper",
        "controller",
        json!({"run_id":"d","state":"waiting","begin":true}),
        NOW + 50 * MIN,
    );
    heartbeat(&mut s, NOW + 50 * MIN);
    let t = at(&mut s, "runner", "escalate_tick", json!({}), NOW + 50 * MIN);
    assert_eq!(t["stuck"], 0, "{t}");
    // A second ask that helper was shown (an inbox read), then nothing armed.
    let mut s = board();
    ask(&mut s, json!({}));
    shown(&mut s, "helper", NOW + MIN);
    heartbeat(&mut s, NOW + 50 * MIN);
    let t = at(&mut s, "runner", "escalate_tick", json!({}), NOW + 50 * MIN);
    assert_eq!(t["stuck"], 0, "shown means it arrived: {t}");
}

#[test]
fn an_overdue_ask_escalates_even_after_it_was_shown_and_settles_on_an_answer() {
    let mut s = board();
    let id = ask(&mut s, json!({"respond_within_ms": 30 * MIN}));
    shown(&mut s, "helper", NOW + MIN);
    heartbeat(&mut s, NOW + 31 * MIN);
    let t = at(&mut s, "runner", "escalate_tick", json!({}), NOW + 31 * MIN);
    assert_eq!(t["created"].as_array().unwrap().len(), 2, "{t}");
    let card = escalations(&mut s, "steward", NOW + 31 * MIN)[0].clone();
    assert!(card["title"]
        .as_str()
        .unwrap()
        .starts_with("Stuck (overdue)"));
    // helper answers: the escalations settle at the next tick.
    at(
        &mut s,
        "helper",
        "annotate",
        json!({"id":id,"body":"on it"}),
        NOW + 32 * MIN,
    );
    heartbeat(&mut s, NOW + 33 * MIN);
    let t = at(&mut s, "runner", "escalate_tick", json!({}), NOW + 33 * MIN);
    assert_eq!(t["settled"].as_array().unwrap().len(), 2, "{t}");
    let shown = at(
        &mut s,
        "steward",
        "show",
        json!({"id":card["id"]}),
        NOW + 33 * MIN,
    );
    assert_eq!(shown["card"]["status"], "resolved", "{shown}");
}

#[test]
fn escalations_are_never_themselves_escalated() {
    let mut s = board();
    ask(&mut s, json!({}));
    heartbeat(&mut s, NOW + 50 * MIN);
    at(&mut s, "runner", "escalate_tick", json!({}), NOW + 50 * MIN);
    // Hours later, with the stewards still present but never reading.
    heartbeat(&mut s, NOW + 300 * MIN);
    let t = at(
        &mut s,
        "runner",
        "escalate_tick",
        json!({}),
        NOW + 300 * MIN,
    );
    assert_eq!(t["created"].as_array().unwrap().len(), 0, "{t}");
}

#[test]
fn a_mote_request_to_someone_not_on_the_board_escalates_and_settles() {
    let mut s = board();
    at(
        &mut s,
        "alice",
        "mote_bind",
        json!({"store":"/r/.mote","store_id":"st-A"}),
        NOW,
    );
    let req = |state: &str| json!({"store_id":"st-A","requests":[{"msg_id":"msg-1","recipient":"zed","from":"alice","state":state,"body":"review?"}]});
    at(&mut s, "runner", "mote_requests_sync", req("open"), NOW);
    heartbeat(&mut s, NOW + 20 * MIN);
    at(
        &mut s,
        "runner",
        "mote_requests_sync",
        req("open"),
        NOW + 20 * MIN,
    );
    let t = at(&mut s, "runner", "escalate_tick", json!({}), NOW + 20 * MIN);
    assert_eq!(t["created"].as_array().unwrap().len(), 2, "{t}");
    let card = escalations(&mut s, "steward", NOW + 20 * MIN)[0].clone();
    assert!(card["summary"]
        .as_str()
        .unwrap()
        .contains("zed has not joined"));
    // Answered in Mote: the next sync drops it and the tick settles.
    at(
        &mut s,
        "runner",
        "mote_requests_sync",
        req("responded"),
        NOW + 21 * MIN,
    );
    let t = at(&mut s, "runner", "escalate_tick", json!({}), NOW + 21 * MIN);
    assert_eq!(t["settled"].as_array().unwrap().len(), 2, "{t}");
}

#[test]
fn a_steward_is_told_when_nobody_is_ticking() {
    let mut s = board();
    ask(&mut s, json!({}));
    heartbeat(&mut s, NOW + 50 * MIN);
    let b = at(&mut s, "steward", "brief", json!({}), NOW + 50 * MIN);
    let note = b["escalation_note"].as_str().unwrap();
    assert!(note.contains("no runner is ticking"), "{note}");
    // Not for a non-steward, and not once a runner ticks.
    assert!(at(&mut s, "alice", "brief", json!({}), NOW + 50 * MIN)["escalation_note"].is_null());
    at(&mut s, "runner", "escalate_tick", json!({}), NOW + 50 * MIN);
    let b = at(&mut s, "steward", "brief", json!({}), NOW + 51 * MIN);
    assert!(b["escalation_note"].is_null(), "{b}");
    // The read-only listing shows the stuck request either way.
    let st = at(&mut s, "alice", "stuck_requests", json!({}), NOW + 51 * MIN);
    assert_eq!(st["stuck"].as_array().unwrap().len(), 1, "{st}");
    assert_eq!(st["ticking"], true);
}

#[test]
fn the_escalation_name_is_reserved() {
    let mut s = Store::memory().unwrap();
    let e = s
        .execute_at(&Request::new("join", "Escalation", json!({})), NOW)
        .unwrap_err();
    assert_eq!(e.code, "reserved_escalation");
}

/// Review of 39ca448, #83: re-routing to someone else who cannot be woken
/// escalates again, and a steward who closed theirs while it is still stuck
/// is reminded an hour later.
#[test]
fn a_still_stuck_request_escalates_again_after_a_reroute_or_an_hour() {
    let mut s = board();
    at(&mut s, "other", "join", json!({"topics":[]}), NOW);
    let id = ask(&mut s, json!({}));
    heartbeat(&mut s, NOW + 20 * MIN);
    let t = at(&mut s, "runner", "escalate_tick", json!({}), NOW + 20 * MIN);
    assert_eq!(t["created"].as_array().unwrap().len(), 2, "{t}");
    // steward re-routes to `other`, who is not armed either.
    let rev =
        at(&mut s, "steward", "show", json!({"id":id}), NOW + 21 * MIN)["card"]["rev"].clone();
    at(
        &mut s,
        "steward",
        "patch",
        json!({"id":id,"expect":rev,"assignee":"other"}),
        NOW + 21 * MIN,
    );
    heartbeat(&mut s, NOW + 40 * MIN);
    let t = at(&mut s, "runner", "escalate_tick", json!({}), NOW + 40 * MIN);
    assert_eq!(t["created"].as_array().unwrap().len(), 2, "re-routed: {t}");
    assert_eq!(
        t["settled"].as_array().unwrap().len(),
        2,
        "old subject: {t}"
    );
    // steward closes its card; still stuck, nothing until an hour has passed.
    let mine = escalations(&mut s, "steward", NOW + 41 * MIN)
        .into_iter()
        .find(|c| c["status"] == "open")
        .unwrap();
    at(
        &mut s,
        "steward",
        "patch",
        json!({"id":mine["id"],"expect":mine["rev"],"status":"resolved"}),
        NOW + 41 * MIN,
    );
    heartbeat(&mut s, NOW + 60 * MIN);
    let t = at(&mut s, "runner", "escalate_tick", json!({}), NOW + 60 * MIN);
    assert_eq!(t["created"].as_array().unwrap().len(), 0, "{t}");
    heartbeat(&mut s, NOW + 101 * MIN);
    let t = at(
        &mut s,
        "runner",
        "escalate_tick",
        json!({}),
        NOW + 101 * MIN,
    );
    assert_eq!(t["created"].as_array().unwrap().len(), 1, "reminder: {t}");
}

/// #86: patching an ask's kind away does not hide it from escalation.
#[test]
fn a_kind_patch_does_not_hide_an_unreachable_ask() {
    let mut s = board();
    let id = ask(&mut s, json!({}));
    at(
        &mut s,
        "runner",
        "patch",
        json!({"id":id,"expect":1,"kind":"note"}),
        NOW + MIN,
    );
    heartbeat(&mut s, NOW + 20 * MIN);
    let t = at(&mut s, "runner", "escalate_tick", json!({}), NOW + 20 * MIN);
    assert_eq!(t["stuck"], 1, "{t}");
}

/// A burst of stuck requests is escalated a bounded number per tick.
#[test]
fn escalations_are_bounded_per_tick() {
    let mut s = board();
    for _ in 0..15 {
        ask(&mut s, json!({}));
    }
    heartbeat(&mut s, NOW + 20 * MIN);
    let t = at(&mut s, "runner", "escalate_tick", json!({}), NOW + 20 * MIN);
    assert_eq!(t["created"].as_array().unwrap().len(), 20, "{t}");
    assert_eq!(t["deferred"], 10, "{t}");
    let t = at(&mut s, "runner", "escalate_tick", json!({}), NOW + 21 * MIN);
    assert_eq!(t["created"].as_array().unwrap().len(), 10, "{t}");
}

/// Review of a8dabdf: a Mote request to someone who joins later is carded
/// and listed once, not also as a request to an unknown actor.
#[test]
fn a_request_carded_after_its_addressee_joins_is_listed_once() {
    let mut s = board();
    at(
        &mut s,
        "alice",
        "mote_bind",
        json!({"store":"/r/.mote","store_id":"st-A"}),
        NOW,
    );
    let req = json!({"store_id":"st-A","requests":[{"msg_id":"msg-9","recipient":"yan","from":"alice","state":"open","body":"x"}]});
    at(&mut s, "runner", "mote_requests_sync", req.clone(), NOW);
    at(&mut s, "yan", "join", json!({"topics":[]}), NOW + MIN);
    at(&mut s, "runner", "mote_requests_sync", req, NOW + MIN);
    let st = at(&mut s, "alice", "stuck_requests", json!({}), NOW + 40 * MIN);
    let subjects: Vec<&str> = st["stuck"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["subject"].as_str())
        .collect();
    assert_eq!(subjects.len(), 1, "{subjects:?}");
    assert!(subjects[0].starts_with("card:"), "{subjects:?}");
}

/// Review of a8dabdf: asks that can never be stuck (unassigned, no
/// deadline) do not crowd a stuck one out of the scan.
#[test]
fn unassigned_asks_without_deadlines_do_not_fill_the_scan() {
    let mut s = board();
    for _ in 0..1001 {
        at(
            &mut s,
            "alice",
            "post",
            json!({"kind":"question","topic":"t","title":"anyone?","summary":"x"}),
            NOW,
        );
    }
    ask(&mut s, json!({}));
    heartbeat(&mut s, NOW + 20 * MIN);
    let t = at(&mut s, "runner", "escalate_tick", json!({}), NOW + 20 * MIN);
    assert_eq!(t["stuck"], 1, "{t}");
}
