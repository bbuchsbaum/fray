use fray::{model::Request, store::Store};
use serde_json::{json, Value};

const NOW: i64 = 1_800_000_000_000;

fn call(store: &mut Store, actor: &str, op: &str, args: Value) -> Value {
    store
        .execute_at(&Request::new(op, actor, args), NOW)
        .unwrap()
}

fn board() -> Store {
    let mut s = Store::memory().unwrap();
    for who in ["lead", "worker", "peer"] {
        call(&mut s, who, "join", json!({"topics": ["release"]}));
    }
    s
}

fn item(page: &Value, id: &Value) -> Value {
    page["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| &i["card"]["id"] == id)
        .unwrap()
        .clone()
}

// The field report: "the lead's key request was truncated at 'Please append a …'".
const REQUEST: &str = "Thanks for the evidence. Please append a verification line to the archive README that names the exact commit, the command you ran, and the observed result, then reply here with the commit.";

#[test]
fn a_request_addressed_to_you_arrives_whole() {
    let mut s = board();
    let asked = call(
        &mut s,
        "lead",
        "send",
        json!({"to": "worker", "body": "Review the export?", "ask": true}),
    );
    let id = asked["card"]["id"].clone();
    call(
        &mut s,
        "lead",
        "annotate",
        json!({"id": id, "kind": "note", "body": REQUEST}),
    );
    let page = call(&mut s, "worker", "inbox", json!({}));
    let it = item(&page, &id);
    assert_eq!(it["addressed"], true);
    assert_eq!(it["annotations"][0]["excerpt"], REQUEST);
    assert_eq!(it["annotations"][0]["full"], true);
    assert_eq!(it["annotations"][0]["excerpt_truncated"], false);
}

#[test]
fn replies_to_your_own_request_arrive_whole() {
    let mut s = board();
    let asked = call(
        &mut s,
        "worker",
        "send",
        json!({"to": "lead", "body": "May I take the extractor lane?", "ask": true}),
    );
    let id = asked["card"]["id"].clone();
    call(
        &mut s,
        "lead",
        "annotate",
        json!({"id": id, "kind": "answer", "body": REQUEST}),
    );
    let it = item(&call(&mut s, "worker", "inbox", json!({})), &id);
    assert_eq!(it["addressed"], true);
    assert_eq!(it["annotations"][0]["excerpt"], REQUEST);
}

#[test]
fn broadcasts_keep_previews() {
    let mut s = board();
    let posted = call(
        &mut s,
        "lead",
        "post",
        json!({"title": "Release notes", "summary": "draft", "topic": "release"}),
    );
    let id = posted["card"]["id"].clone();
    call(
        &mut s,
        "lead",
        "annotate",
        json!({"id": id, "kind": "note", "body": "x".repeat(600)}),
    );
    let it = item(&call(&mut s, "peer", "inbox", json!({})), &id);
    assert_eq!(it["addressed"], false);
    assert_eq!(it["annotations"][0]["full"], false);
    assert_eq!(it["annotations"][0]["excerpt_truncated"], true);
    assert!(
        it["annotations"][0]["excerpt"]
            .as_str()
            .unwrap()
            .chars()
            .count()
            <= 201
    );
}

#[test]
fn full_text_is_bounded_per_page_and_the_fallback_is_flagged() {
    let mut s = board();
    // Three addressed requests with near-maximum bodies exceed the page budget.
    let mut ids = Vec::new();
    for n in 0..3 {
        let asked = call(
            &mut s,
            "lead",
            "send",
            json!({"to": "worker", "body": format!("Request {n}"), "ask": true}),
        );
        let id = asked["card"]["id"].clone();
        call(
            &mut s,
            "lead",
            "annotate",
            json!({"id": id, "kind": "note", "body": "y".repeat(7_900)}),
        );
        ids.push(id);
    }
    let page = call(&mut s, "worker", "inbox", json!({}));
    let fulls: Vec<bool> = ids
        .iter()
        .map(|id| item(&page, id)["annotations"][0]["full"] == true)
        .collect();
    assert_eq!(fulls.iter().filter(|f| **f).count(), 2, "{fulls:?}");
    let clipped = ids
        .iter()
        .map(|id| item(&page, id))
        .find(|it| it["annotations"][0]["full"] == false)
        .unwrap();
    assert_eq!(clipped["annotations"][0]["excerpt_truncated"], true);
    // The page as a whole stays bounded.
    assert!(serde_json::to_string(&page).unwrap().len() < 40_000);
}

fn long_request(s: &mut Store) -> Value {
    let asked = call(
        s,
        "lead",
        "send",
        json!({"to": "worker", "body": "Review the export?", "ask": true, "priority": 1}),
    );
    let id = asked["card"]["id"].clone();
    call(
        s,
        "lead",
        "annotate",
        json!({"id": id, "kind": "note", "body": "z".repeat(6_000)}),
    );
    id
}

#[test]
fn byte_capped_consumers_get_excerpts_not_nothing() {
    // The review BLOCK: full text made the hook (6,000-byte cap) and a small
    // brief drop the very request addressed to the reader.
    let mut s = board();
    let id = long_request(&mut s);
    // What the hook asks for: excerpts only, so the item fits and is surfaced.
    let hook = call(
        &mut s,
        "worker",
        "inbox",
        json!({"limit": 4, "addressed_to_me": true, "full_text_budget": 0}),
    );
    let it = item(&hook, &id);
    assert_eq!(it["annotations"][0]["full"], false);
    assert_eq!(it["annotations"][0]["excerpt_truncated"], true);
    assert!(serde_json::to_string(&hook).unwrap().len() < 6_000);
    // A small brief keeps the addressed item instead of dropping it.
    let brief = call(&mut s, "worker", "brief", json!({"budget": 4000}));
    let ids: Vec<Value> = brief["attention"]["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["card"]["id"].clone())
        .collect();
    assert!(ids.contains(&id), "{ids:?}");
    // An explicit inbox still gets the whole message.
    let full = call(&mut s, "worker", "inbox", json!({}));
    assert_eq!(item(&full, &id)["annotations"][0]["full"], true);
}

#[test]
fn json_escaping_counts_against_the_budget() {
    let mut s = board();
    let asked = call(
        &mut s,
        "lead",
        "send",
        json!({"to": "worker", "body": "Quote check", "ask": true}),
    );
    let id = asked["card"]["id"].clone();
    // 7,000 quote characters encode to 14,000+ bytes: over half the budget each.
    for _ in 0..2 {
        call(
            &mut s,
            "lead",
            "annotate",
            json!({"id": id, "kind": "note", "body": "\"".repeat(7_000)}),
        );
    }
    let page = call(&mut s, "worker", "inbox", json!({}));
    let it = item(&page, &id);
    let fulls = it["annotations"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["full"] == true)
        .count();
    assert_eq!(fulls, 1);
    assert!(serde_json::to_string(&page).unwrap().len() < 20_000);
}

#[test]
fn own_broadcasts_are_not_treated_as_requests() {
    let mut s = board();
    let posted = call(
        &mut s,
        "worker",
        "post",
        json!({"title": "FYI", "summary": "note", "topic": "release"}),
    );
    let id = posted["card"]["id"].clone();
    call(
        &mut s,
        "peer",
        "annotate",
        json!({"id": id, "kind": "note", "body": "x".repeat(600)}),
    );
    let it = item(&call(&mut s, "worker", "inbox", json!({})), &id);
    assert_eq!(it["addressed"], false);
    assert_eq!(it["annotations"][0]["full"], false);
}
