//! Daemon transition courts use explicit synthetic ownership observations.
//! Real Mote ownership is covered by the separate subprocess acceptance court.
use fray::{
    model::{Request, Result},
    store::Store,
};
use serde_json::{json, Value};
const NOW: i64 = 1_800_000_000_000;
fn run(s: &mut Store, actor: &str, op: &str, mut args: Value, time: i64) -> Result<Value> {
    if op == "dispatch_sync" && args.get("expect_attempt").is_none() {
        let offer = s.execute_at(
            &Request::new("dispatch_get", actor, json!({"id":args["id"]})),
            time,
        )?;
        args["expect_attempt"] = json!({"actor":offer["accepted_by"],"key":offer["attempt_key"],"session":offer["attempt_session"],"peer_generation":offer["attempt_peer_generation"]});
    }
    s.execute_at(&Request::new(op, actor, args), time)
}
fn call(s: &mut Store, actor: &str, op: &str, args: Value) -> Value {
    run(s, actor, op, args, NOW).unwrap()
}
fn fixture() -> Store {
    let mut s = Store::memory().unwrap();
    for actor in ["writer", "alice", "bob"] {
        call(&mut s, actor, "join", json!({"topics":["rust"]}));
        call(&mut s, actor, "set_status", json!({"text":"idle"}));
    }
    s
}
fn offer(s: &mut Store, ttl: i64) -> Value {
    call(s,"writer","dispatch_offer",json!({"issue":"work","tag":"rust","title":"Bounded work","body":"Real instruction","offer_ttl_s":ttl,"claim_ttl_s":1}))["offer"].clone()
}
fn claim() -> Value {
    json!({"holder":"alice","token":"claim-token","lease_until_ts":"2030-01-01T00:00:00Z"})
}
#[test]
fn routing_selects_idle_peer_and_two_accept_attempts_have_one_winner() {
    let mut s = fixture();
    let offered = offer(&mut s, 300);
    let id = offered["id"].clone();
    assert_eq!(offered["offered_to"], "alice");
    let first = call(
        &mut s,
        "alice",
        "dispatch_accept",
        json!({"id":id,"expect_generation":1,"key":"a"}),
    );
    assert_eq!(first["status"], "pending");
    let duplicate = call(
        &mut s,
        "alice",
        "dispatch_accept",
        json!({"id":id,"expect_generation":1,"key":"a"}),
    );
    assert_eq!(duplicate, first);
    let other = run(
        &mut s,
        "bob",
        "dispatch_accept",
        json!({"id":id,"expect_generation":1,"key":"b"}),
        NOW,
    )
    .unwrap_err();
    assert_eq!(other.code, "conflict");
    assert!(other.message.contains("alice"));
    let accepted = call(
        &mut s,
        "alice",
        "dispatch_status",
        json!({"id":id,"expect_generation":1,"key":"a","claim":claim()}),
    );
    assert_eq!(accepted["status"], "accepted");
    let card = call(&mut s, "writer", "show", json!({"id":id}));
    assert_eq!(card["card"]["assignee"], "alice");
    assert!(card["card"]["lease_owner"].is_null());
    assert!(run(&mut s, "bob", "claim", json!({"id":id}), NOW).is_err());
}

#[test]
fn cancellation_during_mote_claim_does_not_reopen_the_offer() {
    let mut s = fixture();
    let id = offer(&mut s, 300)["id"].clone();
    call(
        &mut s,
        "alice",
        "dispatch_accept",
        json!({"id":id,"expect_generation":1,"key":"a"}),
    );
    let card = call(&mut s, "writer", "show", json!({"id":id}));
    call(
        &mut s,
        "writer",
        "patch",
        json!({"id":id,"expect":card["card"]["rev"],"status":"resolved"}),
    );
    let error = run(
        &mut s,
        "alice",
        "dispatch_status",
        json!({"id":id,"expect_generation":1,"key":"a","claim":claim()}),
        NOW,
    )
    .unwrap_err();
    assert_eq!(error.code, "closed");
    assert_eq!(
        call(&mut s, "writer", "show", json!({"id":id}))["card"]["status"],
        "resolved"
    );
}

#[test]
fn acceptance_start_after_snapshot_refuses_stale_reconciliation() {
    let mut s = fixture();
    let id = offer(&mut s, 300)["id"].clone();
    let stale = json!({"id":id,"expect_generation":1,"expect_attempt":{"actor":null,"key":null,"session":null,"peer_generation":null},"claim":null,"transport_idle":true});
    call(
        &mut s,
        "alice",
        "dispatch_accept",
        json!({"id":id,"expect_generation":1,"key":"a"}),
    );
    assert_eq!(
        run(&mut s, "writer", "dispatch_sync", stale, NOW + 2000)
            .unwrap_err()
            .code,
        "conflict"
    );
    assert_eq!(
        call(&mut s, "writer", "dispatch_get", json!({"id":id}))["generation"],
        1
    );
}

#[test]
fn leave_and_rejoin_without_host_session_cannot_resume_an_old_attempt() {
    let mut s = fixture();
    let id = offer(&mut s, 300)["id"].clone();
    call(
        &mut s,
        "alice",
        "dispatch_accept",
        json!({"id":id,"expect_generation":1,"key":"a"}),
    );
    call(&mut s, "alice", "leave", json!({}));
    call(&mut s, "alice", "join", json!({"topics":["rust"]}));
    let error = run(
        &mut s,
        "alice",
        "dispatch_status",
        json!({"id":id,"expect_generation":1,"key":"a","claim":claim()}),
        NOW,
    )
    .unwrap_err();
    assert_eq!(error.code, "dispatch_session_ended");
    let stalled = call(
        &mut s,
        "writer",
        "dispatch_sync",
        json!({"id":id,"expect_generation":1,"claim":claim(),"transport_idle":true}),
    );
    assert_eq!(stalled["status"], "stalled");
    assert_eq!(stalled["generation"], 1);
}
#[test]
fn no_idle_peer_and_muted_or_mismatched_acceptance_are_explicit() {
    let mut s = fixture();
    call(&mut s, "alice", "set_status", json!({"text":"busy"}));
    call(&mut s, "bob", "set_status", json!({"text":"busy"}));
    let none = offer(&mut s, 300);
    assert_eq!(none["status"], "no_eligible");
    call(&mut s, "alice", "set_status", json!({"text":"idle"}));
    let yes = offer(&mut s, 300);
    let id = yes["id"].clone();
    call(&mut s, "alice", "mute", json!({"id":id}));
    let rejected = run(
        &mut s,
        "alice",
        "dispatch_accept",
        json!({"id":id,"expect_generation":1,"key":"a"}),
        NOW,
    )
    .unwrap_err();
    assert_eq!(rejected.code, "dispatch_ineligible");
}
#[test]
fn disappearance_notifies_but_live_or_renewed_claim_prevents_requeue() {
    let mut s = fixture();
    let id = offer(&mut s, 300)["id"].clone();
    call(
        &mut s,
        "alice",
        "dispatch_accept",
        json!({"id":id,"expect_generation":1,"key":"a"}),
    );
    call(
        &mut s,
        "alice",
        "dispatch_status",
        json!({"id":id,"expect_generation":1,"key":"a","claim":claim()}),
    );
    call(&mut s, "alice", "leave", json!({}));
    let stalled = call(
        &mut s,
        "writer",
        "dispatch_sync",
        json!({"id":id,"expect_generation":1,"claim":claim(),"transport_idle":true}),
    );
    assert_eq!(stalled["status"], "stalled");
    assert_eq!(stalled["generation"], 1);
    let mut renewed = claim();
    renewed["token"] = json!("renewed");
    let retained = call(
        &mut s,
        "writer",
        "dispatch_sync",
        json!({"id":id,"expect_generation":1,"claim":renewed,"transport_idle":true}),
    );
    assert_eq!(retained["generation"], 1);
    assert_eq!(retained["observed_claim_token"], "renewed");
    let requeued = call(
        &mut s,
        "writer",
        "dispatch_sync",
        json!({"id":id,"expect_generation":1,"claim":null,"transport_idle":true}),
    );
    assert_eq!(requeued["generation"], 2);
    assert_eq!(requeued["offered_to"], "bob");
    let inbox = call(&mut s, "writer", "inbox", json!({}));
    assert!(inbox["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["card"]["title"]
            .as_str()
            .unwrap_or("")
            .contains("disappeared")));
}
#[test]
fn unknown_acceptance_waits_for_bounded_window_and_transport_then_old_generation_refuses() {
    let mut s = fixture();
    let id = offer(&mut s, 300)["id"].clone();
    call(
        &mut s,
        "alice",
        "dispatch_accept",
        json!({"id":id,"expect_generation":1,"key":"a"}),
    );
    let args = json!({"id":id,"expect_generation":1,"claim":null,"transport_idle":true});
    let pending = run(&mut s, "writer", "dispatch_sync", args.clone(), NOW + 500).unwrap();
    assert_eq!(pending["generation"], 1);
    let busy = run(
        &mut s,
        "writer",
        "dispatch_sync",
        json!({"id":id,"expect_generation":1,"claim":null,"transport_idle":false}),
        NOW + 2000,
    )
    .unwrap();
    assert_eq!(busy["generation"], 1);
    let next = run(&mut s, "writer", "dispatch_sync", args, NOW + 2000).unwrap();
    assert_eq!(next["generation"], 2);
    assert_eq!(
        run(
            &mut s,
            "alice",
            "dispatch_accept",
            json!({"id":id,"expect_generation":1,"key":"a"}),
            NOW + 2000
        )
        .unwrap_err()
        .code,
        "conflict"
    );
}
#[test]
fn offer_expiry_routes_next_peer_and_handoff_packet_retries_are_exact() {
    let mut s = fixture();
    let original = offer(&mut s, 1);
    let id = original["id"].clone();
    let next = run(
        &mut s,
        "writer",
        "dispatch_sync",
        json!({"id":id,"expect_generation":1,"claim":null,"transport_idle":true}),
        NOW + 2000,
    )
    .unwrap();
    assert_eq!(next["offered_to"], "bob");
    let payload = json!({"source_card":id,"to":"alice","issue":"work","key":"packet","holder":"writer","claim_token":"old","state":"done","next":"finish","evidence":[],"carriers":[]});
    let first = call(&mut s, "writer", "dispatch_handoff_packet", payload.clone());
    let second = call(&mut s, "writer", "dispatch_handoff_packet", payload.clone());
    assert_eq!(first["id"], second["id"]);
    let mut changed = payload;
    changed["next"] = json!("different");
    assert_eq!(
        run(&mut s, "writer", "dispatch_handoff_packet", changed, NOW)
            .unwrap_err()
            .code,
        "idempotency_conflict"
    );
    assert_eq!(
        run(
            &mut s,
            "bob",
            "dispatch_handoff_record",
            json!({"id":first["id"],"status":"completed","receipt":{}}),
            NOW
        )
        .unwrap_err()
        .code,
        "not_party"
    );
    call(
        &mut s,
        "writer",
        "dispatch_handoff_record",
        json!({"id":first["id"],"status":"transferred","receipt":{"transfer":"accepted"}}),
    );
    let completed = call(
        &mut s,
        "alice",
        "dispatch_handoff_record",
        json!({"id":first["id"],"status":"completed","receipt":{"adoption":"retained"}}),
    );
    for status in ["transferred", "partial", "lost"] {
        let retry = call(
            &mut s,
            "writer",
            "dispatch_handoff_record",
            json!({"id":first["id"],"status":status,"receipt":{"older_sender_retry":true}}),
        );
        assert_eq!(retry, completed);
    }
}

#[test]
fn handoff_acceptance_checks_recipient_and_all_terminal_card_states() {
    for status in ["withdrawn", "resolved", "superseded"] {
        let mut s = fixture();
        let id = offer(&mut s, 300)["id"].clone();
        let packet = call(
            &mut s,
            "writer",
            "dispatch_handoff_packet",
            json!({"source_card":id,"to":"alice","issue":"work","key":"packet","holder":"writer","claim_token":"old","state":"partial","next":"finish","evidence":[],"carriers":[]}),
        );
        let check = json!({"id":packet["id"],"for_accept":true});
        assert_eq!(
            run(&mut s, "bob", "dispatch_handoff_get", check.clone(), NOW)
                .unwrap_err()
                .code,
            "not_recipient"
        );
        assert_eq!(
            call(&mut s, "alice", "dispatch_handoff_get", check.clone())["accept_checked"],
            true
        );
        call(
            &mut s,
            "writer",
            "patch",
            json!({"id":packet["id"],"expect":1,"status":status}),
        );
        assert_eq!(
            run(&mut s, "alice", "dispatch_handoff_get", check, NOW)
                .unwrap_err()
                .code,
            "closed"
        );
        assert_eq!(
            call(
                &mut s,
                "alice",
                "dispatch_handoff_get",
                json!({"id":packet["id"]})
            )["card_status"],
            status
        );
    }
}
