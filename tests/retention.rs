use fray::{
    model::{random_key, Request},
    recovery, retention,
    store::Store,
};
use rusqlite::params;
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

const NOW: i64 = 1_800_000_000_000;

struct Fixture {
    home: PathBuf,
    store: Store,
}
impl Fixture {
    fn new() -> Self {
        let key = random_key().unwrap();
        let home = std::env::temp_dir().join(format!("fray-retention-{}", &key[..8]));
        fs::create_dir(&home).unwrap();
        let mut store = Store::open(&home.join("state.db"), false).unwrap();
        for actor in ["writer", "reader"] {
            call(&mut store, actor, "join", json!({}), NOW);
        }
        Self { home, store }
    }
    fn close_post(&mut self, title: &str, body: &str) -> i64 {
        let id = call(
            &mut self.store,
            "writer",
            "post",
            json!({"kind":"question","topic":"*","title":title,"summary":body,"assignee":"reader"}),
            NOW - 10_000,
        )["card"]["id"]
            .as_i64()
            .unwrap();
        let rev = call(
            &mut self.store,
            "writer",
            "show",
            json!({"id":id}),
            NOW - 9_000,
        )["card"]["rev"]
            .clone();
        call(
            &mut self.store,
            "writer",
            "patch",
            json!({"id":id,"expect":rev,"status":"resolved"}),
            NOW - 8_000,
        );
        self.store
            .conn
            .execute(
                "UPDATE deliveries SET ack_seq=pending_seq WHERE card_id=?",
                [id],
            )
            .unwrap();
        id
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.home);
    }
}
fn call(store: &mut Store, actor: &str, op: &str, args: Value, now: i64) -> Value {
    store
        .execute_at(&Request::new(op, actor, args), now)
        .unwrap_or_else(|e| panic!("{op}: {} {}", e.code, e.message))
}
fn ids(v: &Value) -> Vec<i64> {
    v["cards"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_i64().unwrap())
        .collect()
}

fn serve(home: &std::path::Path) -> OwnedDaemon {
    let mut child = OwnedDaemon(
        Command::new(env!("CARGO_BIN_EXE_fray"))
            .args(["--home", home.to_str().unwrap(), "serve"])
            .env_remove("FRAY_AGENT")
            .env_remove("FRAY_SESSION")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        if UnixStream::connect(home.join("bus.sock")).is_ok() {
            return child;
        }
        if let Some(status) = child.0.try_wait().unwrap() {
            panic!("owned retention daemon exited before ready: {status}");
        }
        thread::sleep(Duration::from_millis(20));
    }
    panic!("owned retention daemon did not become ready");
}

struct OwnedDaemon(Child);
impl Drop for OwnedDaemon {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn cli(home: &std::path::Path, args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_fray"))
        .args(["--home", home.to_str().unwrap(), "--as", "writer", "--json"])
        .args(args)
        .env_remove("FRAY_AGENT")
        .env_remove("FRAY_SESSION")
        .output()
        .unwrap()
}

#[test]
fn dry_run_is_read_only_and_execution_archives_the_same_selection_with_a_restorable_database() {
    let mut f = Fixture::new();
    let compact = f.close_post("Old terminal", "secret old transcript needle");
    let keep = f.close_post("Young terminal", "newer");
    f.store
        .conn
        .execute(
            "UPDATE cards SET updated_ms=? WHERE id=?",
            params![NOW, keep],
        )
        .unwrap();
    let before_events: Vec<(i64, String)> = f
        .store
        .conn
        .prepare("SELECT seq,payload FROM events ORDER BY seq")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    let dry = retention::run(&f.home, NOW - 1_000, None, NOW).unwrap();
    assert_eq!(ids(&dry), vec![compact]);
    assert!(!f.home.join("daemon.lock").exists());
    assert!(!f.home.join("archive").exists());
    let after_dry: Vec<(i64, String)> = f
        .store
        .conn
        .prepare("SELECT seq,payload FROM events ORDER BY seq")
        .unwrap()
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .unwrap()
        .collect::<rusqlite::Result<_>>()
        .unwrap();
    assert_eq!(after_dry, before_events);
    let out = f.home.join("archive");
    let executed = retention::run(&f.home, NOW - 1_000, Some(&out), NOW).unwrap();
    assert_eq!(ids(&executed), ids(&dry));
    assert!(out.join(format!("thread-{compact}.md")).is_file());
    assert!(fs::read_to_string(out.join(format!("thread-{compact}.md")))
        .unwrap()
        .contains("secret old transcript needle"));
    let restore = f.home.join("restored");
    recovery::restore(&restore, &out.join("state-before.sqlite")).unwrap();
    let restored = Store::open(&restore.join("state.db"), false).unwrap();
    assert!(
        restored
            .conn
            .query_row("SELECT count(*) FROM events", [], |r| r.get::<_, i64>(0))
            .unwrap()
            > 0
    );
    let mut compacted = Store::open(&f.home.join("state.db"), false).unwrap();
    let raw: String = compacted
        .conn
        .query_row(
            "SELECT payload FROM events WHERE card_id=? ORDER BY seq LIMIT 1",
            [compact],
            |r| r.get(0),
        )
        .unwrap();
    assert!(raw.contains("\"compacted\":true"));
    assert_eq!(
        compacted
            .execute_at(
                &Request::new("search_history", "writer", json!({"q":"needle"})),
                NOW
            )
            .unwrap()["items"]
            .as_array()
            .unwrap()
            .len(),
        0
    );
    assert_eq!(
        compacted
            .conn
            .query_row(
                "SELECT count(*) FROM event_fts e JOIN events x ON x.seq=e.rowid WHERE x.card_id=?",
                [compact],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
}

#[test]
fn every_protected_reference_category_stays_out_of_the_plan() {
    let mut f = Fixture::new();
    let open_ref = f.close_post("open ref", "x");
    let linked = f.close_post("linked", "x");
    let review = f.close_post("review", "x");
    let mote = f.close_post("mote", "x");
    let receipt = f.close_post("receipt", "x");
    let batch = f.close_post("batch", "x");
    call(
        &mut f.store,
        "writer",
        "post",
        json!({"kind":"note","topic":"*","title":"Open","summary":format!("see #{open_ref}")}),
        NOW - 5_000,
    );
    let child = call(
        &mut f.store,
        "writer",
        "post",
        json!({"kind":"note","topic":"*","title":"Open child","summary":"child"}),
        NOW - 5_000,
    )["card"]["id"]
        .as_i64()
        .unwrap();
    f.store
        .conn
        .execute(
            "INSERT INTO events(ts_ms,actor,op,card_id,payload) VALUES(?,?,?,?,?)",
            params![
                NOW - 4_000,
                "writer",
                "annotate",
                child,
                serde_json::to_string_pretty(&json!({"detail":{"parent_card":linked}})).unwrap()
            ],
        )
        .unwrap();
    f.store.conn.execute("INSERT INTO review_subjects(card_id,baseline,candidate,subject_rev,mote_ref) VALUES(?,?,?,?,?)", params![review,"git:a","git:b",1,Option::<String>::None]).unwrap();
    f.store
        .conn
        .execute(
            "UPDATE cards SET tags='[\"mote:issue-1\"]' WHERE id=?",
            [mote],
        )
        .unwrap();
    f.store
        .conn
        .execute(
            "UPDATE deliveries SET pending_seq=pending_seq+1 WHERE agent='reader' AND card_id=?",
            [receipt],
        )
        .unwrap();
    f.store.conn.execute("INSERT INTO presented_batches(batch,agent,session,source,created_ms) VALUES('b','reader',NULL,'inbox',?)", [NOW - 1_000]).unwrap();
    f.store
        .conn
        .execute(
            "INSERT INTO presented_items(batch,card_id,through_seq) VALUES('b',?,1)",
            [batch],
        )
        .unwrap();
    let plan = retention::plan(&f.store.conn, NOW - 1_000).unwrap();
    assert!(ids(&plan).is_empty());
    let protected = plan["protected"].as_array().unwrap();
    for id in [open_ref, linked, review, mote, receipt, batch] {
        assert!(
            protected.iter().any(|v| v["card"] == id),
            "missing protected #{id}: {protected:?}"
        );
    }
    let historic = f.close_post("historic mote", "x");
    f.store.conn.execute("UPDATE events SET payload=json_set(payload, '$.mote_ref', 'mote:old-1') WHERE card_id=?", [historic]).unwrap();
    let after = retention::plan(&f.store.conn, NOW - 1_000).unwrap();
    assert!(after["protected"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["card"] == historic
            && v["reasons"].to_string().contains("historic Mote reference")));
}

#[test]
fn compaction_keeps_stats_and_rejects_expired_cursors_and_live_daemon_lock() {
    let mut f = Fixture::new();
    let id = f.close_post("Metric question", "body");
    call(
        &mut f.store,
        "reader",
        "annotate",
        json!({"id":id,"kind":"answer","body":"yes"}),
        NOW - 7_000,
    );
    f.store
        .conn
        .execute(
            "UPDATE deliveries SET ack_seq=pending_seq WHERE card_id=?",
            [id],
        )
        .unwrap();
    let all_before = call(
        &mut f.store,
        "writer",
        "stats",
        json!({"window_ms":null}),
        NOW,
    )["stats"]
        .clone();
    let recent_before = call(
        &mut f.store,
        "writer",
        "stats",
        json!({"window_ms":20_000}),
        NOW,
    )["stats"]
        .clone();
    let out = f.home.join("archive");
    let done = retention::run(&f.home, NOW - 1_000, Some(&out), NOW).unwrap();
    assert!(done["events_compacted"].as_i64().unwrap_or(0) > 0, "{done}");
    let mut store = Store::open(&f.home.join("state.db"), false).unwrap();
    let all_after = store
        .execute_at(
            &Request::new("stats", "writer", json!({"window_ms":null})),
            NOW,
        )
        .unwrap()["stats"]
        .clone();
    let recent_after = store
        .execute_at(
            &Request::new("stats", "writer", json!({"window_ms":20_000})),
            NOW,
        )
        .unwrap()["stats"]
        .clone();
    assert_eq!(all_after, all_before);
    assert_eq!(recent_after, recent_before);
    assert_eq!(
        retention::check_cursor(&store.conn, 0).unwrap_err().code,
        "cursor_compacted"
    );
    assert!(
        retention::check_cursor(&store.conn, done["compacted_through"].as_i64().unwrap()).is_ok()
    );
    drop(store);
    let file = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(f.home.join("daemon.lock"))
        .unwrap();
    fs2::FileExt::lock_exclusive(&file).unwrap();
    assert_eq!(
        retention::run(&f.home, NOW - 1_000, Some(&f.home.join("other")), NOW)
            .unwrap_err()
            .code,
        "daemon_running"
    );
}

#[test]
fn compacted_cursors_fail_over_the_daemon_wire_but_fresh_reads_and_thread_markers_work() {
    let mut f = Fixture::new();
    let id = f.close_post("Wire history", "body retained only in archive");
    drop(retention::run(&f.home, NOW - 1_000, Some(&f.home.join("archive")), NOW).unwrap());
    assert_eq!(f.store.events(0, 10).unwrap_err().code, "cursor_compacted");
    let _daemon = serve(&f.home);
    let watch = cli(&f.home, &["watch", "--after", "0"]);
    assert!(!watch.status.success());
    assert!(format!(
        "{}{}",
        String::from_utf8_lossy(&watch.stdout),
        String::from_utf8_lossy(&watch.stderr)
    )
    .contains("cursor_compacted"));
    let inbox = cli(&f.home, &["inbox", "--after", "1"]);
    assert!(!inbox.status.success());
    assert!(format!(
        "{}{}",
        String::from_utf8_lossy(&inbox.stdout),
        String::from_utf8_lossy(&inbox.stderr)
    )
    .contains("cursor_compacted"));
    let fresh = cli(&f.home, &["inbox"]);
    assert!(
        fresh.status.success(),
        "{}",
        String::from_utf8_lossy(&fresh.stderr)
    );
    let thread = cli(&f.home, &["thread", &id.to_string(), "--compact"]);
    assert!(
        thread.status.success(),
        "{}",
        String::from_utf8_lossy(&thread.stderr)
    );
    let thread_text = String::from_utf8_lossy(&thread.stdout);
    assert!(
        thread_text.contains("History compacted; full body archived at"),
        "{thread_text}"
    );
}
