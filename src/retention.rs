//! Opt-in offline history compaction. Statistics remain replayable; full
//! conversations are archived and verified before any payload is replaced.
use crate::{archive, model::*, recovery};
use fs2::FileExt;
use rusqlite::{params, Connection, OpenFlags, OptionalExtension};
use serde_json::{json, Value};
use std::{fs, path::Path};

fn ids(conn: &Connection, sql: &str) -> Result<Vec<i64>> {
    Ok(conn
        .prepare(sql)?
        .query_map([], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn plan(conn: &Connection, before: i64) -> Result<Value> {
    let mut selected = Vec::new();
    let mut protected = Vec::new();
    let open = ids(
        conn,
        "SELECT id FROM cards WHERE status NOT IN ('resolved','superseded','withdrawn')",
    )?;
    let mut query = conn.prepare("SELECT id,updated_ms,tags FROM cards WHERE status IN ('resolved','superseded','withdrawn') ORDER BY id")?;
    let heads = query
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, updated, tags) in heads {
        if updated >= before {
            continue;
        }
        let mut reasons = Vec::new();
        let tags: Vec<String> = serde_json::from_str(&tags)?;
        if tags.iter().any(|s| s.starts_with("mote:")) {
            reasons.push("Mote reference");
        }
        // A current tag can be cleared during ordinary card editing. Mote is
        // still authoritative for the historical work this card represented,
        // so retain conservatively if any immutable event payload carried one.
        let historic_mote: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE card_id=?1 AND payload LIKE '%mote:%')",
            [id],
            |r| r.get(0),
        )?;
        if historic_mote && !reasons.contains(&"Mote reference") {
            reasons.push("historic Mote reference");
        }
        for (label, sql) in [
            ("review", "SELECT EXISTS(SELECT 1 FROM review_subjects WHERE card_id=?1)"),
            ("unacked receipt", "SELECT EXISTS(SELECT 1 FROM deliveries WHERE card_id=?1 AND pending_seq>ack_seq)"),
            ("presented batch", "SELECT EXISTS(SELECT 1 FROM presented_items WHERE card_id=?1)"),
            ("open linked request", "SELECT EXISTS(SELECT 1 FROM events e JOIN cards c ON c.id=e.card_id WHERE c.status NOT IN ('resolved','superseded','withdrawn') AND json_extract(e.payload,'$.detail.parent_card')=?1)"),
        ] {
            if conn.query_row(sql, [id], |r| r.get::<_,bool>(0))? { reasons.push(label); }
        }
        // Explicit references in an open head or its history protect the target.
        // Unknown reference syntax is conservatively retained when it names #ID.
        for source in &open {
            let sql = "SELECT title||' '||summary||' '||tags FROM cards WHERE id=?1 UNION ALL SELECT payload FROM events WHERE card_id=?1";
            let texts = conn
                .prepare(sql)?
                .query_map([source], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if texts.iter().any(|s| references(s, id)) {
                reasons.push("open reference");
                break;
            }
        }
        let remaining: i64 = conn.query_row("SELECT count(*) FROM events WHERE card_id=?1 AND coalesce(json_extract(payload,'$.compacted'),0)=0", [id], |r| r.get(0))?;
        if remaining == 0 {
            continue;
        }
        if reasons.is_empty() {
            selected.push(id);
        } else {
            protected.push(json!({"card":id,"reasons":reasons}));
        }
    }
    Ok(
        json!({"before_ms":before,"cards":selected,"protected":protected,
        "mode":"compact archived payloads; retain metric projections, IDs and timestamps"}),
    )
}

fn references(text: &str, id: i64) -> bool {
    [
        format!("#{id}"),
        format!("card:{id}"),
        format!("fray:{id}"),
        format!("\"question_card\":{id}"),
        format!("\"child_card\":{id}"),
    ]
    .iter()
    .any(|needle| {
        text.match_indices(needle).any(|(at, _)| {
            !text[at + needle.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit())
        })
    })
}

/// Run only while holding the same daemon lock as `serve`. A SQLite writer
/// transaction then fences any direct database writer through archive/commit.
/// Dry runs open the database read-only and create no output or lock file.
pub fn run(home: &Path, before: i64, destination: Option<&Path>, now: i64) -> Result<Value> {
    run_with_publication(
        home,
        before,
        destination,
        now,
        |home, destination, files| {
            archive::write_files(destination, files)?;
            recovery::backup(home, &destination.join("state-before.sqlite"))
        },
    )
}

fn run_with_publication(
    home: &Path,
    before: i64,
    destination: Option<&Path>,
    now: i64,
    publish: impl FnOnce(&Path, &Path, &[(String, Vec<u8>)]) -> Result<()>,
) -> Result<Value> {
    let checked_home = archive::safe_path(home, false)?;
    let home = checked_home.as_path();
    let checked_destination = destination
        .map(|path| archive::safe_path(path, true))
        .transpose()?;
    let destination = checked_destination.as_deref();
    if destination.is_none() {
        let conn =
            Connection::open_with_flags(home.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.execute_batch("BEGIN DEFERRED")?;
        return plan(&conn, before);
    }
    let destination = destination.unwrap();
    let lock_path = home.join("daemon.lock");
    if fs::symlink_metadata(&lock_path).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(Error::invalid("daemon lock must not be a symlink"));
    }
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(lock_path)?;
    FileExt::try_lock_exclusive(&lock).map_err(|_| {
        Error::new(
            "daemon_running",
            "pruning requires an offline board; coordinate its owner before stopping the daemon",
        )
    })?;
    let mut conn =
        Connection::open_with_flags(home.join("state.db"), OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA synchronous=FULL")?;
    let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let mut result = plan(&tx, before)?;
    let selected: Vec<i64> = result["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect();
    if selected.is_empty() {
        return Ok(result);
    }
    if destination.exists() || fs::symlink_metadata(destination).is_ok() {
        return Err(Error::new(
            "output_exists",
            "prune archive must be a fresh directory",
        ));
    }
    let files = archive::markdown(home, None)?;
    publish(home, destination, &files)?;
    for (name, bytes) in &files {
        if fs::read(destination.join(name))? != *bytes {
            return Err(Error::new(
                "archive_changed",
                "archive verification failed; no history was compacted",
            ));
        }
    }
    let archive_path = fs::canonicalize(destination)?;
    let mut floor = 0;
    let mut changed = 0;
    for id in &selected {
        let raw = tx
            .prepare("SELECT seq,payload FROM events WHERE card_id=?1 ORDER BY seq")?
            .query_map([id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        for (seq, raw) in raw {
            let original: Value = serde_json::from_str(&raw)?;
            let mut card = serde_json::Map::new();
            for key in [
                "kind",
                "status",
                "assignee",
                "author",
                "created_ms",
                "title",
                "lease_owner",
            ] {
                if let Some(value) = original["card"].get(key) {
                    card.insert(key.into(), value.clone());
                }
            }
            let mut detail = serde_json::Map::new();
            for key in ["parent_card", "annotation_seq", "kind", "open_objections"] {
                if let Some(value) = original["detail"].get(key) {
                    detail.insert(key.into(), value.clone());
                }
            }
            let archive = archive_path.join(format!("thread-{id}.md"));
            detail.insert(
                "body".into(),
                json!(format!(
                    "History compacted; full body archived at {}",
                    archive.display()
                )),
            );
            let compact = json!({"card":card,"detail":detail,"compacted":true,"archive":archive,"original_seq":seq});
            tx.execute(
                "UPDATE events SET payload=?1 WHERE seq=?2",
                params![serde_json::to_string(&compact)?, seq],
            )?;
            tx.execute("DELETE FROM event_fts WHERE rowid=?1", [seq])?;
            floor = floor.max(seq);
            changed += 1;
        }
        tx.execute(
            "UPDATE cards SET summary=?1 WHERE id=?2",
            params![
                format!(
                    "History archived to {}",
                    archive_path.join(format!("thread-{id}.md")).display()
                ),
                id
            ],
        )?;
    }
    let previous = cursor_floor(&tx)?;
    tx.execute("INSERT INTO meta(key,value) VALUES('compacted_through',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[floor.max(previous).to_string()])?;
    // Audit events are separate from conversation events so metrics remain
    // exactly reproducible, including their event count and historic windows.
    tx.execute_batch("CREATE TABLE IF NOT EXISTS retention_events(id INTEGER PRIMARY KEY AUTOINCREMENT,ts_ms INTEGER NOT NULL,payload TEXT NOT NULL CHECK(json_valid(payload)))")?;
    result["archive"] = json!(archive_path);
    result["events_compacted"] = json!(changed);
    result["compacted_through"] = json!(floor.max(previous));
    tx.execute(
        "INSERT INTO retention_events(ts_ms,payload) VALUES(?1,?2)",
        params![now, serde_json::to_string(&result)?],
    )?;
    result["retention_event"] = json!(tx.last_insert_rowid());
    tx.commit()?;
    Ok(result)
}

fn cursor_floor(conn: &Connection) -> Result<i64> {
    let floor: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key='compacted_through'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    floor
        .map(|s| {
            s.parse()
                .map_err(|_| Error::new("retention_metadata", "invalid compaction cursor"))
        })
        .unwrap_or(Ok(0))
}
pub fn check_cursor(conn: &Connection, after: i64) -> Result<()> {
    let floor = cursor_floor(conn)?;
    if floor > 0 && after < floor {
        return Err(Error::new("cursor_compacted",format!("history through event {floor} was archived; use a fresh snapshot or start watch at the current cursor")));
    }
    Ok(())
}

#[cfg(test)]
mod publication_tests {
    use super::*;
    use crate::store::Store;

    #[test]
    fn publication_failure_cannot_commit_compaction_or_its_cursor_and_audit() {
        let home = std::env::temp_dir().join(format!("fray-prune-sync-{}", random_key().unwrap()));
        fs::create_dir(&home).unwrap();
        let mut store = Store::open(&home.join("state.db"), false).unwrap();
        store
            .execute_at(&Request::new("join", "writer", json!({})), 10)
            .unwrap();
        let post = store
            .execute_at(
                &Request::new(
                    "post",
                    "writer",
                    json!({"kind":"note","topic":"*","title":"History","summary":"preserve me"}),
                ),
                10,
            )
            .unwrap();
        store
            .execute_at(
                &Request::new(
                    "patch",
                    "writer",
                    json!({"id":post["card"]["id"],"expect":1,"status":"resolved"}),
                ),
                20,
            )
            .unwrap();
        let payloads = || {
            store
                .conn
                .prepare("SELECT payload FROM events ORDER BY seq")
                .unwrap()
                .query_map([], |r| r.get::<_, String>(0))
                .unwrap()
                .collect::<rusqlite::Result<Vec<_>>>()
                .unwrap()
        };
        let before = payloads();
        let error = run_with_publication(
            &home,
            100,
            Some(&home.join("archive")),
            200,
            |source, dir, files| {
                archive::write_files(dir, files)?;
                recovery::backup(source, &dir.join("state-before.sqlite"))?;
                // Failure after visible publication models a directory-sync error:
                // readback alone cannot authorize destructive commit.
                Err(Error::new(
                    "injected_sync_failure",
                    "publication was not confirmed durable",
                ))
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "injected_sync_failure");
        assert_eq!(payloads(), before);
        assert_eq!(cursor_floor(&store.conn).unwrap(), 0);
        assert_eq!(
            store
                .conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE name='retention_events'",
                    [],
                    |r| r.get::<_, i64>(0)
                )
                .unwrap(),
            0
        );
        drop(store);
        fs::remove_dir_all(home).unwrap();
    }
}
