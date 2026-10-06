use fray::{
    archive,
    model::{random_key, Request},
    store::Store,
};
use serde_json::{json, Value};
use std::{fs, path::PathBuf};

const NOW: i64 = 1_800_000_000_000;
struct Fixture {
    dir: PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!("fray-export-{}", random_key().unwrap()));
        fs::create_dir(&dir).unwrap();
        let mut store = Store::open(&dir.join("state.db"), false).unwrap();
        for actor in ["writer", "reader"] {
            call(&mut store, actor, "join", json!({}), NOW);
        }
        Self { dir, store }
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}
fn call(store: &mut Store, actor: &str, op: &str, args: Value, now: i64) -> Value {
    store
        .execute_at(&Request::new(op, actor, args), now)
        .unwrap()
}
fn state(store: &Store) -> Vec<String> {
    ["SELECT name||':'||last_seen_ms FROM agents ORDER BY name",
     "SELECT agent||':'||card_id||':'||pending_seq||':'||ack_seq||':'||shown_seq FROM deliveries ORDER BY agent,card_id",
     "SELECT batch||':'||created_ms FROM presented_batches ORDER BY batch",
     "SELECT seq||':'||payload FROM events ORDER BY seq"].iter().flat_map(|sql| {
        store.conn.prepare(sql).unwrap().query_map([], |r| r.get::<_,String>(0)).unwrap()
            .collect::<rusqlite::Result<Vec<_>>>().unwrap()
    }).collect()
}

#[test]
fn board_and_export_are_read_only_and_export_is_byte_stable() {
    let mut f = Fixture::new();
    let review = call(
        &mut f.store,
        "writer",
        "review_request",
        json!({
            "to":"reader","title":"Parser <script>alert(1)</script>","body":"Evidence ``` and 🧠",
            "baseline":format!("git:{}", "a".repeat(40)),"candidate":format!("git:{}", "b".repeat(40)),"mote_ref":"mote:issue-42"
        }),
        NOW,
    );
    let id = review["card"]["id"].clone();
    call(
        &mut f.store,
        "reader",
        "annotate",
        json!({"id":id,"body":"Race in parser","kind":"objection",
        "review_verdict":{"verdict":"object","at":format!("git:{}", "b".repeat(40)),"expect":1}}),
        NOW + 100,
    );
    call(
        &mut f.store,
        "writer",
        "send",
        json!({"to":"owner","ask":true,"pending":true,"body":"Choose the policy"}),
        NOW + 200,
    );
    let before = state(&f.store);
    let snapshot = archive::board(&f.dir, NOW + 300).unwrap();
    let peek = archive::peek(&f.dir, NOW + 300).unwrap();
    assert!(peek["cards"][0].get("full_text").is_none());
    assert_eq!(snapshot["cards"].as_array().unwrap().len(), 3);
    let html = archive::board_html(&snapshot);
    assert!(!html.contains("<script>"));
    assert!(html.contains("&lt;script&gt;"));
    for label in [
        "Team",
        "Open conversations",
        "Lanes",
        "Owner decisions awaiting response",
        "Race in parser",
    ] {
        assert!(html.contains(label), "{label}");
    }
    let files = archive::markdown(&f.dir, None).unwrap();
    assert_eq!(files, archive::markdown(&f.dir, None).unwrap());
    let output = f.dir.join("export");
    archive::write_files(&output, &files).unwrap();
    archive::write_files(&output, &files).unwrap();
    let review_text =
        fs::read_to_string(output.join(format!("thread-{}.md", id.as_i64().unwrap()))).unwrap();
    for expected in [
        "mote:issue-42",
        "Race in parser",
        "Event 1",
        "\"reviewer\": \"reader\"",
        "\"verdict\": \"object\"",
        "Evidence ``` and 🧠",
    ] {
        assert!(review_text.contains(expected), "{expected}");
    }
    assert_eq!(state(&f.store), before);
}

#[test]
fn since_selects_touched_cards_and_keeps_history_before_the_boundary() {
    let mut f = Fixture::new();
    let a = call(
        &mut f.store,
        "writer",
        "send",
        json!({"to":"reader","body":"Old context"}),
        NOW,
    )["card"]["id"]
        .as_i64()
        .unwrap();
    let b = call(
        &mut f.store,
        "writer",
        "send",
        json!({"to":"reader","body":"Unchanged"}),
        NOW,
    )["card"]["id"]
        .as_i64()
        .unwrap();
    call(
        &mut f.store,
        "reader",
        "annotate",
        json!({"id":a,"body":"New evidence"}),
        NOW + 100,
    );
    let files = archive::markdown(&f.dir, Some(NOW + 50)).unwrap();
    assert!(files
        .iter()
        .any(|(name, _)| name == &format!("thread-{a}.md")));
    assert!(!files
        .iter()
        .any(|(name, _)| name == &format!("thread-{b}.md")));
    let body = &files
        .iter()
        .find(|(name, _)| name == &format!("thread-{a}.md"))
        .unwrap()
        .1;
    assert!(String::from_utf8_lossy(body).contains("Old context"));
}

#[test]
fn export_refuses_changed_files_and_symlinks_before_writing() {
    let f = Fixture::new();
    let dir = f.dir.join("out");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("index.md"), b"unrelated").unwrap();
    let files = vec![
        ("thread-1.md".into(), b"thread".to_vec()),
        ("index.md".into(), b"index".to_vec()),
    ];
    assert_eq!(
        archive::write_files(&dir, &files).unwrap_err().code,
        "output_exists"
    );
    assert!(!dir.join("thread-1.md").exists());
    fs::remove_file(dir.join("index.md")).unwrap();
    std::os::unix::fs::symlink(f.dir.join("state.db"), dir.join("index.md")).unwrap();
    assert_eq!(
        archive::write_files(&dir, &files).unwrap_err().code,
        "output_exists"
    );
    let missing = f.dir.join("missing");
    assert!(archive::board(&missing, NOW).is_err());
    assert!(!missing.exists());
    let escaping = vec![("../escape.md".into(), b"no".to_vec())];
    assert!(archive::write_files(&f.dir.join("unused"), &escaping).is_err());
    assert!(!f.dir.join("unused").exists());
}
