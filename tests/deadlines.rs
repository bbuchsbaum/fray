//! No silent stalls R4: deadlines on asks. Set on the daemon's clock, kept in
//! events (never patchable tags), cleared only by the addressee's answer,
//! kept across reassignment, moved only by the asker.
use fray::{model::*, store::Store};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000_000;
const MIN: i64 = 60_000;

fn at(s: &mut Store, who: &str, op: &str, args: Value, now: i64) -> Result<Value> {
    s.execute_at(&Request::new(op, who, args), now)
}
fn board() -> Store {
    let mut s = Store::memory().unwrap();
    for who in ["alice", "helper", "bystander", "other"] {
        at(&mut s, who, "join", json!({"topics":[]}), NOW).unwrap();
    }
    s
}
fn overdue(s: &mut Store, now: i64) -> Vec<i64> {
    at(s, "alice", "brief", json!({}), now).unwrap()["overdue_asks"]["items"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|o| o["id"].as_i64().unwrap())
        .collect()
}
fn ask(s: &mut Store, within: i64) -> (i64, i64) {
    let sent = at(
        s,
        "alice",
        "send",
        json!({"to":"helper","body":"review?","ask":true,"respond_within_ms":within}),
        NOW,
    )
    .unwrap();
    (
        sent["card"]["id"].as_i64().unwrap(),
        sent["card"]["rev"].as_i64().unwrap(),
    )
}

#[test]
fn an_unanswered_ask_is_overdue_only_after_its_deadline() {
    let mut s = board();
    let (id, _) = ask(&mut s, 30 * MIN);
    assert!(overdue(&mut s, NOW + 29 * MIN).is_empty());
    assert_eq!(overdue(&mut s, NOW + 31 * MIN), vec![id]);
    let text = at(&mut s, "alice", "friction", json!({}), NOW + 31 * MIN).unwrap();
    let item = &text["friction"]["unanswered_asks"]["items"][0];
    assert_eq!(item["overdue"], true, "{item}");
    assert_eq!(item["soft_deadline"], false, "{item}");
}

#[test]
fn a_bystanders_reply_does_not_clear_it_but_the_addressees_does() {
    let mut s = board();
    let (id, _) = ask(&mut s, 30 * MIN);
    at(
        &mut s,
        "bystander",
        "annotate",
        json!({"id":id,"body":"+1"}),
        NOW + 5 * MIN,
    )
    .unwrap();
    assert_eq!(overdue(&mut s, NOW + 31 * MIN), vec![id]);
    at(
        &mut s,
        "helper",
        "annotate",
        json!({"id":id,"body":"seen, later today"}),
        NOW + 32 * MIN,
    )
    .unwrap();
    assert!(overdue(&mut s, NOW + 33 * MIN).is_empty());
}

#[test]
fn the_deadline_cannot_be_patched_away_and_survives_reassignment() {
    let mut s = board();
    let (id, rev) = ask(&mut s, 30 * MIN);
    // Tags and assignee change; the deadline lives in the creation event.
    at(
        &mut s,
        "alice",
        "patch",
        json!({"id":id,"expect":rev,"tags":[],"assignee":"other"}),
        NOW + MIN,
    )
    .unwrap();
    assert_eq!(overdue(&mut s, NOW + 31 * MIN), vec![id]);
    // The old addressee's answer no longer counts; the new one's does.
    at(
        &mut s,
        "helper",
        "annotate",
        json!({"id":id,"body":"not mine now"}),
        NOW + 32 * MIN,
    )
    .unwrap();
    assert_eq!(overdue(&mut s, NOW + 33 * MIN), vec![id]);
}

#[test]
fn only_the_asker_moves_the_deadline() {
    let mut s = board();
    let (id, _) = ask(&mut s, 30 * MIN);
    let err = at(
        &mut s,
        "helper",
        "annotate",
        json!({"id":id,"body":"I need a week","respond_within_ms":7*86_400_000}),
        NOW + MIN,
    )
    .unwrap_err();
    assert_eq!(err.code, "invalid");
    // alice extends by an hour from now: not overdue at the old deadline.
    at(
        &mut s,
        "alice",
        "annotate",
        json!({"id":id,"body":"take an hour","respond_within_ms":60*MIN}),
        NOW + 20 * MIN,
    )
    .unwrap();
    assert!(overdue(&mut s, NOW + 31 * MIN).is_empty());
    assert_eq!(overdue(&mut s, NOW + 81 * MIN), vec![id]);
}

#[test]
fn a_deadline_needs_an_ask_and_asks_without_one_use_the_soft_default() {
    let mut s = board();
    let err = at(
        &mut s,
        "alice",
        "send",
        json!({"to":"helper","body":"fyi","respond_within_ms":30*MIN}),
        NOW,
    )
    .unwrap_err();
    assert_eq!(err.code, "invalid");
    at(
        &mut s,
        "alice",
        "send",
        json!({"to":"helper","body":"whenever","ask":true}),
        NOW,
    )
    .unwrap();
    // No deadline: never in the brief, overdue in friction after 24 hours.
    assert!(overdue(&mut s, NOW + 25 * 60 * MIN).is_empty());
    let f = at(&mut s, "alice", "friction", json!({}), NOW + 25 * 60 * MIN).unwrap();
    let item = &f["friction"]["unanswered_asks"]["items"][0];
    assert_eq!(item["overdue"], true, "{item}");
    assert_eq!(item["soft_deadline"], true, "{item}");
}

/// Review of 9fbddc4, #81: no patch that answers nothing can hide an overdue
/// ask, whoever makes it.
#[test]
fn no_patch_hides_an_overdue_ask() {
    for (who, change) in [
        ("bystander", json!({"assignee":null})),
        ("helper", json!({"status":"blocked"})),
        ("bystander", json!({"kind":"note"})),
        ("bystander", json!({"assignee":"alice"})),
    ] {
        let mut s = board();
        let (id, rev) = ask(&mut s, 30 * MIN);
        let mut args = json!({"id":id,"expect":rev});
        args.as_object_mut()
            .unwrap()
            .extend(change.as_object().unwrap().clone());
        at(&mut s, who, "patch", args, NOW + MIN).unwrap();
        assert_eq!(overdue(&mut s, NOW + 31 * MIN), vec![id], "{who} {change}");
    }
}

/// With nobody (or only the asker) assigned, anyone else's reply answers it.
#[test]
fn an_unassigned_ask_is_answered_by_anyone_but_the_asker() {
    let mut s = board();
    let (id, rev) = ask(&mut s, 30 * MIN);
    at(
        &mut s,
        "alice",
        "patch",
        json!({"id":id,"expect":rev,"assignee":null}),
        NOW + MIN,
    )
    .unwrap();
    at(
        &mut s,
        "alice",
        "annotate",
        json!({"id":id,"body":"anyone?"}),
        NOW + 2 * MIN,
    )
    .unwrap();
    assert_eq!(overdue(&mut s, NOW + 31 * MIN), vec![id]);
    at(
        &mut s,
        "bystander",
        "annotate",
        json!({"id":id,"body":"me"}),
        NOW + 32 * MIN,
    )
    .unwrap();
    assert!(overdue(&mut s, NOW + 33 * MIN).is_empty());
}
