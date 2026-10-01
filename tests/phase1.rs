use fray::{attention::Options, model::Request, session, store::Store};
use serde_json::{json, Value};
use std::collections::HashMap;
const NOW: i64 = 1_800_000_000_000;

fn call(s: &mut Store, actor: &str, op: &str, args: Value) -> Value {
    s.execute_at(&Request::new(op, actor, args), NOW).unwrap()
}
fn board() -> Store {
    let mut s = Store::memory().unwrap();
    for actor in ["author", "reviewer", "other"] {
        call(&mut s, actor, "join", json!({"topics":[]}));
    }
    s
}
fn send(s: &mut Store, body: &str) -> Value {
    call(
        s,
        "author",
        "send",
        json!({"to":"reviewer","body":body,"ask":true}),
    )
}
fn readiness(s: &mut Store, now: i64) -> Value {
    s.execute_at(
        &Request::new("brief", "author", json!({"budget":2000})),
        now,
    )
    .unwrap()
}

#[test]
fn provider_neutral_session_precedence_and_validation() {
    assert_eq!(
        session::resolve(Some("custom:x"), Some("c"), Some("o"))
            .unwrap()
            .as_deref(),
        Some("custom:x")
    );
    assert_eq!(
        session::resolve(None, Some("c"), Some("o"))
            .unwrap()
            .as_deref(),
        Some("claude:c")
    );
    assert_eq!(
        session::resolve(None, None, Some("o")).unwrap().as_deref(),
        Some("codex:o")
    );
    assert!(session::resolve(None, None, None).unwrap().is_none());
    for bad in ["", "bad space", "shell;value", &"x".repeat(129)] {
        assert!(session::resolve(Some(bad), None, None).is_err());
    }
}

#[test]
fn card_filter_is_shared_narrowing_not_routing_or_ack() {
    let mut s = board();
    let a = send(&mut s, "A");
    let b = send(&mut s, "B");
    let args = json!({"selection":"involved","card_ids":[b["card"]["id"]]});
    let inbox = call(&mut s, "reviewer", "inbox", args.clone());
    let packet = s
        .wake_packet(
            "reviewer",
            &Options::parse(&args).unwrap(),
            &HashMap::new(),
            NOW,
        )
        .unwrap();
    assert_eq!(inbox["total"], 1);
    assert_eq!(inbox["items"][0]["receipt"], packet["items"][0]["receipt"]);
    assert_eq!(packet["items"][0]["card"]["id"], b["card"]["id"]);
    assert_eq!(call(&mut s, "other", "inbox", args.clone())["total"], 0);
    let constrained = json!({"card_ids":[b["card"]["id"]],"min_priority":0});
    assert_eq!(call(&mut s, "reviewer", "inbox", constrained)["total"], 0);
    let all = call(&mut s, "reviewer", "inbox", json!({}));
    assert_eq!(all["total"], 2);
    assert!(all["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["card"]["id"] == a["card"]["id"]));
}

#[test]
fn card_filter_rejects_empty_overlong_and_nonpositive_values() {
    let mut s = board();
    for ids in [
        json!([]),
        json!([0]),
        json!([-1]),
        json!([1.1]),
        json!(["1"]),
        json!((1..=17).collect::<Vec<_>>()),
    ] {
        let args = json!({"card_ids":ids});
        assert!(Options::parse(&args).is_err());
        assert!(s
            .execute_at(&Request::new("inbox", "reviewer", args), NOW)
            .is_err());
    }
    assert_eq!(
        Options::parse(&json!({"card_ids":[2,1,2]}))
            .unwrap()
            .card_ids,
        vec![1, 2]
    );
}

#[test]
fn mute_retains_receipts_survives_rejoin_and_does_not_hide_a_direct_objection() {
    let mut s = board();
    let a = call(
        &mut s,
        "author",
        "send",
        json!({"to":"reviewer","body":"Review A"}),
    );
    let id = a["card"]["id"].clone();
    call(&mut s, "reviewer", "mute", json!({"id":id}));
    assert_eq!(call(&mut s, "reviewer", "inbox", json!({}))["total"], 0);
    let objection = call(
        &mut s,
        "author",
        "annotate",
        json!({"id":id,"kind":"objection","body":"Blocking evidence"}),
    );
    let page = call(&mut s, "reviewer", "join", json!({}));
    assert_eq!(page["attention"]["total"], 1);
    assert_eq!(
        page["attention"]["items"][0]["card"]["id"],
        objection["follow_up"]["id"]
    );
    let receipts = call(&mut s, "author", "show", json!({"id":id,"receipts":true}));
    let receipt = receipts["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["agent"] == "reviewer")
        .unwrap();
    assert_eq!(receipt["pending_seq"], a["event_seq"]);
    assert_eq!(receipt["ack_seq"], 0);
    call(&mut s, "reviewer", "unmute", json!({"id":id}));
    let page = call(&mut s, "reviewer", "inbox", json!({"card_ids":[id]}));
    assert_eq!(page["total"], 1);
    assert_eq!(page["items"][0]["through_seq"], objection["event_seq"]);
    assert_eq!(page["items"][0]["ack_seq"], 0);
}

#[test]
fn leave_stops_participant_and_direct_fanout_but_rejoin_catches_up() {
    let mut s = board();
    let a = send(&mut s, "Review A");
    call(
        &mut s,
        "reviewer",
        "annotate",
        json!({"id":a["card"]["id"],"body":"I participate"}),
    );
    call(&mut s, "reviewer", "leave", json!({}));
    let updated = call(
        &mut s,
        "author",
        "annotate",
        json!({"id":a["card"]["id"],"body":"New result"}),
    );
    let receipts = call(
        &mut s,
        "author",
        "show",
        json!({"id":a["card"]["id"],"receipts":true}),
    );
    let receipt = receipts["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["agent"] == "reviewer")
        .unwrap();
    assert_eq!(receipt["pending_seq"], a["event_seq"]);
    let joined = call(&mut s, "reviewer", "join", json!({}));
    assert_eq!(
        joined["attention"]["items"][0]["through_seq"],
        updated["event_seq"]
    );
}

#[test]
fn wake_warning_distinguishes_connected_expired_filtered_and_armed() {
    let mut s = board();
    send(&mut s, "Please review");
    let b = readiness(&mut s, NOW);
    assert!(b["idle_readiness"]["warning"].is_string());
    assert!(serde_json::to_vec(&b).unwrap().len() <= 2000);
    for (mode, expires, filtered, armed) in [
        ("manual", NOW + 10_000, false, false),
        ("boundary", NOW + 10_000, false, false),
        ("native-monitor", NOW, false, false),
        ("native-monitor", NOW + 10_000, true, false),
        ("native-monitor", NOW + 10_000, false, true),
        ("background-completion", NOW + 10_000, false, true),
    ] {
        let filters = json!({"selection":"involved","addressed_to_me":filtered,"activation":{"mode":mode,"expires_ms":expires}});
        s.listener_begin("author", "run", "connection", &filters.to_string(), NOW)
            .unwrap();
        let b = readiness(&mut s, NOW);
        assert_eq!(b["idle_readiness"]["armed"], armed, "{filters}");
        assert_eq!(b["idle_readiness"]["warning"].is_string(), !armed);
        s.listener_end("author", "connection").unwrap();
    }
    call(
        &mut s,
        "author",
        "controller",
        json!({"run_id":"managed","state":"waiting","begin":true}),
    );
    assert_eq!(readiness(&mut s, NOW)["idle_readiness"]["armed"], true);
    assert_eq!(
        readiness(&mut s, NOW + 120_000)["idle_readiness"]["armed"],
        false
    );
}

#[test]
fn readiness_warning_with_maximum_identity_fits_the_minimum_brief_budget() {
    let mut s = board();
    let actor = "a".repeat(80);
    call(&mut s, &actor, "join", json!({}));
    call(
        &mut s,
        &actor,
        "send",
        json!({"to":"reviewer","ask":true,"body":"Review requested"}),
    );
    let brief = call(&mut s, &actor, "brief", json!({"budget":2000}));
    assert!(serde_json::to_vec(&brief).unwrap().len() <= 2000);
    assert!(brief["idle_readiness"]["warning"].is_string());
    assert!(brief["idle_readiness"]["arm_command"].is_string());
    assert_eq!(brief["idle_readiness"]["open_requests_awaiting_others"], 1);
    // The hint is `fray arm`, short enough that the readiness details now
    // fit beside it; rows are still trimmed to the budget.
    assert_eq!(
        brief["idle_readiness"]["arm_command"],
        format!("fray --as {actor} arm")
    );
    assert_eq!(brief["budget_truncated"], true);
}

#[test]
fn direct_request_created_and_closed_while_away_is_delivered_on_rejoin() {
    let mut s = board();
    call(&mut s, "reviewer", "leave", json!({}));
    let request = send(&mut s, "Never mind this review");
    let closed = call(
        &mut s,
        "author",
        "patch",
        json!({"id":request["card"]["id"],"expect":1,"status":"withdrawn"}),
    );
    let shown = call(
        &mut s,
        "author",
        "show",
        json!({"id":request["card"]["id"],"receipts":true}),
    );
    assert!(shown["receipts"]
        .as_array()
        .unwrap()
        .iter()
        .all(|r| r["agent"] != "reviewer"));
    let joined = call(&mut s, "reviewer", "join", json!({}));
    assert_eq!(joined["attention"]["total"], 1);
    assert_eq!(
        joined["attention"]["items"][0]["through_seq"],
        closed["event_seq"]
    );
    assert_eq!(
        joined["attention"]["items"][0]["card"]["status"],
        "withdrawn"
    );
    assert_eq!(
        call(&mut s, "newcomer", "join", json!({}))["attention"]["total"],
        0
    );
}

#[test]
fn assigned_open_questions_and_objection_children_cannot_be_muted() {
    let mut s = board();
    let request = send(&mut s, "Review request");
    let objection = call(
        &mut s,
        "author",
        "annotate",
        json!({"id":request["card"]["id"],"kind":"objection","body":"Missing proof"}),
    );
    for id in [
        request["card"]["id"].clone(),
        objection["follow_up"]["id"].clone(),
    ] {
        let error = s
            .execute_at(&Request::new("mute", "reviewer", json!({"id":id})), NOW)
            .unwrap_err();
        assert_eq!(error.code, "cannot_mute_assigned_request");
        let page = call(
            &mut s,
            "reviewer",
            "inbox",
            json!({"card_ids":[id],"addressed_to_me":true,"unresolved":true}),
        );
        assert_eq!(page["total"], 1);
        assert_eq!(page["items"][0]["ack_seq"], 0);
    }
}

#[test]
fn a_prior_mute_cannot_hide_a_later_assigned_question() {
    let mut s = board();
    let note = call(
        &mut s,
        "author",
        "send",
        json!({"to":"reviewer","body":"Ordinary thread"}),
    );
    let id = note["card"]["id"].clone();
    call(&mut s, "other", "mute", json!({"id":id}));
    let assigned = call(
        &mut s,
        "author",
        "patch",
        json!({"id":id,"expect":1,"kind":"question","assignee":"other","priority":0}),
    );
    let args =
        json!({"selection":"involved","card_ids":[id],"addressed_to_me":true,"unresolved":true});
    let packet = s
        .wake_packet(
            "other",
            &Options::parse(&args).unwrap(),
            &HashMap::new(),
            NOW,
        )
        .unwrap();
    assert_eq!(
        packet["items"][0]["receipt"]["through_seq"],
        assigned["event_seq"]
    );
    assert_eq!(
        call(&mut s, "other", "join", json!({}))["attention"]["total"],
        1
    );
}

#[test]
fn unmute_does_not_subscribe_but_recovers_missed_peers_before_ones_own_reply() {
    let mut s = board();
    let note = call(
        &mut s,
        "author",
        "send",
        json!({"to":"reviewer","body":"Ordinary thread"}),
    );
    let id = note["card"]["id"].clone();
    call(&mut s, "other", "unmute", json!({"id":id}));
    call(&mut s, "other", "mute", json!({"id":id}));
    call(&mut s, "other", "unmute", json!({"id":id}));
    assert_eq!(
        call(&mut s, "other", "inbox", json!({"selection":"all"}))["total"],
        0
    );
    call(&mut s, "reviewer", "mute", json!({"id":id}));
    let peer = call(
        &mut s,
        "author",
        "annotate",
        json!({"id":id,"body":"Missed peer evidence"}),
    );
    call(
        &mut s,
        "reviewer",
        "annotate",
        json!({"id":id,"body":"My later note"}),
    );
    call(&mut s, "reviewer", "unmute", json!({"id":id}));
    let page = call(&mut s, "reviewer", "inbox", json!({}));
    assert_eq!(page["items"][0]["through_seq"], peer["event_seq"]);
    assert_eq!(page["items"][0]["ack_seq"], 0);
}
