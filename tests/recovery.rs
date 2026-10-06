use fray::{
    model::{random_key, Request},
    recovery,
    store::Store,
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::os::unix::fs::PermissionsExt;
use std::{collections::BTreeMap, fs, path::PathBuf};

const NOW: i64 = 1_800_000_000_000;

struct Temp(PathBuf);
impl Temp {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("fray-recovery-{}", random_key().unwrap()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn call(store: &mut Store, actor: &str, op: &str, args: Value) -> Value {
    store
        .execute_at(&Request::new(op, actor, args), NOW)
        .unwrap()
}

fn rows(connection: &Connection, sql: &str) -> Vec<String> {
    connection
        .prepare(sql)
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap()
}

fn evidence(path: &PathBuf) -> BTreeMap<&'static str, Vec<String>> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let mut result = BTreeMap::new();
    result.insert("integrity", rows(&connection, "PRAGMA integrity_check"));
    result.insert(
        "version",
        rows(
            &connection,
            "SELECT CAST(user_version AS TEXT) FROM pragma_user_version",
        ),
    );
    result.insert(
        "cards",
        rows(&connection, "SELECT id||':'||rev||':'||status||':'||author||':'||coalesce(assignee,'') FROM cards ORDER BY id"),
    );
    result.insert(
        "events",
        rows(
            &connection,
            "SELECT seq||':'||actor||':'||op||':'||card_id||':'||payload FROM events ORDER BY seq",
        ),
    );
    result.insert(
        "deliveries",
        rows(&connection, "SELECT agent||':'||card_id||':'||pending_seq||':'||ack_seq||':'||shown_seq FROM deliveries ORDER BY agent,card_id"),
    );
    result.insert(
        "presented",
        rows(&connection, "SELECT b.batch||':'||b.agent||':'||b.source||':'||i.card_id||':'||i.through_seq FROM presented_batches b JOIN presented_items i ON i.batch=b.batch ORDER BY b.batch,i.card_id"),
    );
    result.insert(
        "mote_cursor",
        rows(&connection, "SELECT key||':'||value FROM meta WHERE key IN ('mote_store','mote_store_id','mote_cursor') ORDER BY key"),
    );
    result.insert(
        "mote_events",
        rows(&connection, "SELECT store_id||':'||key||':'||recipient||':'||coalesce(card_id,'') FROM mote_events ORDER BY store_id,key,recipient"),
    );
    result
}

#[test]
fn online_backup_and_fresh_restore_preserve_board_state() {
    let temp = Temp::new();
    let home = temp.0.join("source");
    fs::create_dir(&home).unwrap();
    let mut store = Store::open(&home.join("state.db"), false).unwrap();
    for agent in ["writer", "reader"] {
        call(&mut store, agent, "join", json!({}));
    }
    let sent = call(
        &mut store,
        "writer",
        "send",
        json!({"to":"reader","ask":true,"body":"Please verify recovery"}),
    );
    let receipt = call(&mut store, "reader", "inbox", json!({"selection":"all"}))["items"][0]
        ["receipt"]
        .clone();
    call(
        &mut store,
        "reader",
        "present",
        json!({"source":"inbox","receipts":[receipt]}),
    );
    call(
        &mut store,
        "writer",
        "mote_bind",
        json!({"store":"/tmp/mote","store_id":"st-recovery"}),
    );
    call(
        &mut store,
        "writer",
        "mote_ingest",
        json!({"store_id":"st-recovery","after":null,"cursor":"cursor-1","items":[{
            "key":"mote-1","recipient":"reader","title":"Mote recovery","summary":"preserve cursor",
            "priority":1,"refs":["bd-recovery"]
        }]}),
    );
    assert_eq!(sent["card"]["id"], 1);
    drop(store);

    let before = evidence(&home.join("state.db"));
    let backup = temp.0.join("backup.sqlite");
    recovery::backup(&home, &backup).unwrap();
    let restored = temp.0.join("restored");
    recovery::restore(&restored, &backup).unwrap();
    assert_eq!(evidence(&restored.join("state.db")), before);
    assert_eq!(
        fs::metadata(restored.join("state.db"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn refuses_existing_outputs_and_cleans_failed_restore() {
    let temp = Temp::new();
    let home = temp.0.join("source");
    fs::create_dir(&home).unwrap();
    let store = Store::open(&home.join("state.db"), false).unwrap();
    drop(store);
    let output = temp.0.join("existing.sqlite");
    fs::write(&output, b"do not overwrite").unwrap();
    assert_eq!(
        recovery::backup(&home, &output).unwrap_err().code,
        "recovery"
    );
    assert_eq!(fs::read(&output).unwrap(), b"do not overwrite");

    let invalid = temp.0.join("invalid.sqlite");
    fs::write(&invalid, b"not SQLite").unwrap();
    let fresh = temp.0.join("fresh");
    assert_eq!(
        recovery::restore(&fresh, &invalid).unwrap_err().code,
        "database"
    );
    assert!(!fresh.exists());
}
