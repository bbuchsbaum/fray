use fray::{
    attention::{Options, LISTENER_TTL_MS},
    model::*,
    store::Store,
};
use serde_json::{json, Value};
use std::collections::HashMap;

const NOW: i64 = 1_800_000_000_000;
fn call(s: &mut Store, who: &str, op: &str, args: Value) -> Value {
    s.execute_at(&Request::new(op, who, args), NOW).unwrap()
}
fn setup() -> Store {
    let mut s = Store::memory().unwrap();
    for who in ["alice", "codex", "claude", "other"] {
        call(&mut s, who, "join", json!({"topics":[]}));
    }
    s
}
fn options(extra: Value) -> Options {
    let mut args = json!({"selection":"involved"});
    args.as_object_mut()
        .unwrap()
        .extend(extra.as_object().unwrap().clone());
    Options::parse(&args).unwrap()
}
fn emitted(packet: &Value) -> HashMap<i64, i64> {
    packet["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["receipt"]["id"].as_i64().unwrap(),
                i["receipt"]["through_seq"].as_i64().unwrap(),
            )
        })
        .collect()
}

#[test]
fn kind_and_priority_filters_keep_linked_objections_and_hidden_receipts() {
    let mut s = setup();
    let note = call(
        &mut s,
        "alice",
        "send",
        json!({"to":"codex","body":"ordinary","priority":2}),
    );
    let objection = call(
        &mut s,
        "alice",
        "annotate",
        json!({"id":note["card"]["id"],"kind":"objection","body":"Show the evidence"}),
    );
    let question = call(
        &mut s,
        "alice",
        "send",
        json!({"to":"codex","ask":true,"body":"Low urgency","priority":3}),
    );
    let args = json!({"selection":"involved","kinds":["objection"],"min_priority":1,"addressed_to_me":true});
    let packet = s
        .wake_packet("codex", &options(args.clone()), &HashMap::new(), NOW)
        .unwrap();
    let inbox = call(&mut s, "codex", "inbox", args);
    assert_eq!(packet["items"].as_array().unwrap().len(), 1);
    assert_eq!(packet["items"][0]["receipt"], inbox["items"][0]["receipt"]);
    assert_eq!(
        packet["items"][0]["card"]["id"],
        objection["follow_up"]["id"]
    );
    call(
        &mut s,
        "codex",
        "ack",
        json!({"receipts":[packet["items"][0]["receipt"]]}),
    );
    let remaining = call(&mut s, "codex", "inbox", json!({}));
    assert_eq!(remaining["total"], 2);
    assert!(remaining["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|i| i["card"]["id"] == question["card"]["id"]));
    // The parent matches its unhandled objection, even though it is a note.
    let parent = call(&mut s, "codex", "inbox", json!({"kinds":["objection"]}));
    assert_eq!(parent["items"][0]["card"]["id"], note["card"]["id"]);
    call(
        &mut s,
        "codex",
        "ack",
        json!({"receipts":[parent["items"][0]["receipt"]]}),
    );
    call(
        &mut s,
        "alice",
        "annotate",
        json!({"id":note["card"]["id"],"kind":"answer","body":"Evidence provided"}),
    );
    assert_eq!(
        call(&mut s, "codex", "inbox", json!({"kinds":["objection"]}))["total"],
        0
    );
    for args in [
        json!({"kinds":[]}),
        json!({"kinds":["x' OR 1=1 --"]}),
        json!({"kinds":"question"}),
        json!({"min_priority":4}),
    ] {
        assert!(Options::parse(&args).is_err());
        assert!(s
            .execute_at(&Request::new("inbox", "codex", args), NOW)
            .is_err());
    }
}

#[test]
fn packets_are_host_neutral_preserve_full_messages_and_exact_receipts() {
    let mut s = setup();
    for who in ["codex", "claude", "other"] {
        let sent = call(
            &mut s,
            "alice",
            "send",
            json!({"to":who,"body":"line one\nline two 🧠","ask":true}),
        );
        let packet = s
            .wake_packet(who, &options(json!({})), &HashMap::new(), NOW)
            .unwrap();
        assert_eq!(
            packet["items"][0]["messages"][0]["body"],
            "line one\nline two 🧠"
        );
        assert_eq!(packet["items"][0]["through_seq"], sent["event_seq"]);
        assert_eq!(packet["read_is_not_ack"], true);
        assert_eq!(call(&mut s, who, "inbox", json!({}))["total"], 1);
        let ack = packet["items"][0]["receipt"].clone();
        // Own reply advances the head, but not the delivered receipt.
        call(
            &mut s,
            who,
            "annotate",
            json!({"id":sent["card"]["id"],"body":"Handled"}),
        );
        call(&mut s, who, "ack", json!({"receipts":[ack]}));
        assert_eq!(call(&mut s, who, "inbox", json!({}))["total"], 0);
    }
}

#[test]
fn priority_pagination_and_scope_changes_never_skip_old_sequences() {
    let mut s = setup();
    let first = call(
        &mut s,
        "alice",
        "send",
        json!({"to":"codex","body":"Older ordinary","priority":3}),
    );
    let second = call(
        &mut s,
        "alice",
        "send",
        json!({"to":"codex","body":"New urgent","priority":0}),
    );
    let opts = options(json!({"limit":1}));
    let packet = s.wake_packet("codex", &opts, &HashMap::new(), NOW).unwrap();
    assert_eq!(packet["items"][0]["receipt"]["id"], second["card"]["id"]);
    assert_eq!(packet["more"], true);
    let mut seen = emitted(&packet);
    let packet = s.wake_packet("codex", &opts, &seen, NOW).unwrap();
    assert_eq!(packet["items"][0]["receipt"]["id"], first["card"]["id"]);
    seen.extend(emitted(&packet));
    let old = call(
        &mut s,
        "alice",
        "post",
        json!({"title":"Old hidden","summary":"Context","topic":"parser"}),
    );
    call(
        &mut s,
        "alice",
        "send",
        json!({"to":"codex","body":"Much newer"}),
    );
    let packet = s.wake_packet("codex", &opts, &seen, NOW).unwrap();
    seen.extend(emitted(&packet));
    call(&mut s, "codex", "follow", json!({"id":old["card"]["id"]}));
    let packet = s.wake_packet("codex", &opts, &seen, NOW).unwrap();
    assert_eq!(packet["items"][0]["receipt"]["id"], old["card"]["id"]);
    // Reconnect has no emitted set: ALL unacknowledged items remain recoverable.
    let packet = s
        .wake_packet(
            "codex",
            &options(json!({"budget":64000})),
            &HashMap::new(),
            NOW,
        )
        .unwrap();
    assert_eq!(packet["items"].as_array().unwrap().len(), 4);
}

#[test]
fn outgoing_answers_and_objection_followups_are_included_without_broadcasts() {
    let mut s = setup();
    call(&mut s, "codex", "join", json!({"role":"steward"}));
    call(
        &mut s,
        "alice",
        "send",
        json!({"to":"other","body":"Unrelated"}),
    );
    let outgoing = call(
        &mut s,
        "codex",
        "send",
        json!({"to":"alice","body":"Review?","ask":true}),
    );
    let objection = call(
        &mut s,
        "alice",
        "annotate",
        json!({"id":outgoing["card"]["id"],"kind":"objection","body":"Proof missing"}),
    );
    let packet = s
        .wake_packet(
            "codex",
            &options(json!({"budget":16000})),
            &HashMap::new(),
            NOW,
        )
        .unwrap();
    let items = packet["items"].as_array().unwrap();
    let parent = items
        .iter()
        .find(|i| i["card"]["id"] == outgoing["card"]["id"])
        .unwrap();
    assert!(parent["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["kind"] == "objection" && m["body"] == "Proof missing"));
    assert_eq!(parent["follow_ups"][0]["id"], objection["follow_up"]["id"]);
    assert!(!items.iter().any(|i| i["card"]["summary"] == "Unrelated"));
}

#[test]
fn byte_budget_counts_unicode_escaping_and_omissions_without_acknowledging() {
    let mut s = setup();
    let sent = call(
        &mut s,
        "alice",
        "send",
        json!({"to":"codex","body":"\"\n🧠".repeat(1000)}),
    );
    for n in 0..10 {
        call(
            &mut s,
            "alice",
            "annotate",
            json!({"id":sent["card"]["id"],"body":format!("{n} {}","🧠".repeat(1000))}),
        );
    }
    let packet = s
        .wake_packet(
            "codex",
            &options(json!({"budget":2000})),
            &HashMap::new(),
            NOW,
        )
        .unwrap();
    assert!(serde_json::to_vec(&packet).unwrap().len() < 2000);
    assert_eq!(packet["budget_truncated"], true);
    assert_eq!(packet["items"][0]["context_truncated"], true);
    assert!(packet["items"][0]["messages_omitted"].as_i64().unwrap() > 0);
    assert_eq!(call(&mut s, "codex", "inbox", json!({}))["total"], 1);
}

#[test]
fn listener_is_fenced_expires_and_does_not_resurrect_leave() {
    let mut s = setup();
    s.listener_begin("codex", "session-a", "connection-a", "{}", NOW)
        .unwrap();
    assert_eq!(
        s.listener_begin("codex", "session-b", "connection-b", "{}", NOW)
            .unwrap_err()
            .code,
        "listener_busy"
    );
    let controller = Request::new(
        "controller",
        "codex",
        json!({"run_id":"driver","state":"waiting","begin":true}),
    );
    assert_eq!(
        s.execute_at(&controller, NOW).unwrap_err().code,
        "controller_busy"
    );
    // Same client reconnects with a new connection fence; old cleanup is harmless.
    s.listener_begin("codex", "session-a", "connection-new", "{}", NOW + 1)
        .unwrap();
    s.listener_end("codex", "connection-a").unwrap();
    assert_eq!(
        s.listener_refresh("codex", "connection-a", NOW + 2)
            .unwrap_err()
            .code,
        "listener_lost"
    );
    let roster = call(&mut s, "", "agents", json!({}));
    let codex = roster["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["name"] == "codex")
        .unwrap();
    assert_eq!(codex["listener"]["state"], "armed");
    let roster = s
        .execute_at(
            &Request::new("agents", "", json!({})),
            NOW + 1 + LISTENER_TTL_MS,
        )
        .unwrap();
    assert_eq!(
        roster["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["name"] == "codex")
            .unwrap()["listener"]["state"],
        "stale"
    );
    assert_eq!(
        s.listener_refresh("codex", "connection-new", NOW + 1 + LISTENER_TTL_MS)
            .unwrap_err()
            .code,
        "listener_expired"
    );
    call(&mut s, "codex", "leave", json!({}));
    call(&mut s, "codex", "join", json!({}));
    assert!(s
        .listener_refresh("codex", "connection-new", NOW + 2)
        .is_err());
}

#[test]
fn lower_priority_items_never_break_or_shrink_a_fitting_urgent_receipt() {
    for (budget, urgent_body, sizes) in [
        (2000, "short".to_owned(), 1..180),
        (4000, "U".repeat(300), 650..850),
    ] {
        for size in sizes {
            let mut s = setup();
            call(
                &mut s,
                "alice",
                "send",
                json!({"to":"codex","body":urgent_body,"priority":0}),
            );
            let opts = options(json!({"budget":budget}));
            let alone = s.wake_packet("codex", &opts, &HashMap::new(), NOW).unwrap();
            call(
                &mut s,
                "alice",
                "send",
                json!({"to":"codex","body":"x".repeat(size),"priority":2}),
            );
            let packet = s.wake_packet("codex", &opts, &HashMap::new(), NOW).unwrap();
            assert!(serde_json::to_vec(&packet).unwrap().len() < budget as usize);
            assert_eq!(
                packet["items"][0], alone["items"][0],
                "budget={budget} size={size}"
            );
            // A deferred lower-priority item can always be delivered next.
            if packet["more"] == true {
                assert_eq!(
                    s.wake_packet("codex", &opts, &emitted(&packet), NOW)
                        .unwrap()["items"]
                        .as_array()
                        .unwrap()
                        .len(),
                    1
                );
            }
        }
    }
}

#[test]
fn lease_renewals_do_not_evict_conversation_content() {
    let mut s = setup();
    let task = call(
        &mut s,
        "alice",
        "post",
        json!({"kind":"task","title":"Review","summary":"Constraint","assignee":"codex"}),
    );
    let id = &task["card"]["id"];
    let claim = call(&mut s, "codex", "claim", json!({"id":id}));
    call(
        &mut s,
        "codex",
        "annotate",
        json!({"id":id,"body":"Important evidence"}),
    );
    for _ in 0..10 {
        call(
            &mut s,
            "codex",
            "renew",
            json!({"id":id,"fence":claim["card"]["fence"]}),
        );
    }
    call(&mut s, "codex", "annotate", json!({"id":id,"body":"Done"}));
    let packet = s
        .wake_packet(
            "alice",
            &options(json!({"budget":8000})),
            &HashMap::new(),
            NOW,
        )
        .unwrap();
    let messages = packet["items"][0]["messages"].as_array().unwrap();
    assert!(messages.iter().any(|m| m["body"] == "Important evidence"));
    assert!(!messages.iter().any(|m| m["kind"] == "renew"));
    assert_eq!(packet["items"][0]["messages_omitted"], 0);
}
