//! `fray restart --dry-run` (daemon lifecycle L5): who a restart would
//! interrupt, named by actor and session, with a distinct exit status for
//! idle, busy, armed-only, degraded-idle and not running. Read-only.
//!
//! The degraded path (a daemon without `occupancy`) runs against a fake
//! daemon that answers `ping` and `agents` the way 0.2.1 does, so it needs no
//! old binary. Set FRAY_OLD_BIN to an installed pre-occupancy `fray` to also
//! run it against a real one.
use fray::model::random_key;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

fn scratch(tag: &str) -> PathBuf {
    let root = PathBuf::from("/tmp").join(format!("fray-rp{tag}-{}", &random_key().unwrap()[..8]));
    std::fs::create_dir_all(&root).unwrap();
    root
}

/// `fray --home H [--json] restart --dry-run`: exit status and stdout.
fn dry_run(home: &Path, json: bool) -> (i32, String) {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_fray"));
    cmd.args(["--home", home.to_str().unwrap()]);
    if json {
        cmd.arg("--json");
    }
    let out = cmd
        .args(["restart", "--dry-run"])
        .env_remove("FRAY_AGENT")
        .env_remove("FRAY_SESSION")
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8(out.stdout).unwrap(),
    )
}

fn dry_run_json(home: &Path) -> (i32, Value) {
    let (code, out) = dry_run(home, true);
    let value: Value = serde_json::from_str(&out).unwrap_or_else(|e| panic!("{out:?}: {e}"));
    assert_eq!(value["exit_code"], code, "{value}");
    (code, value)
}

struct Daemon {
    root: PathBuf,
    child: Child,
}

impl Daemon {
    fn start_with(bin: &str) -> Self {
        let root = scratch("d");
        let child = Command::new(bin)
            .args(["--home", root.to_str().unwrap(), "serve"])
            .env_remove("FRAY_AGENT")
            .env_remove("FRAY_SESSION")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let daemon = Self { root, child };
        for _ in 0..200 {
            if UnixStream::connect(daemon.root.join("bus.sock")).is_ok() {
                return daemon;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("daemon did not start");
    }

    fn connect(&self) -> Conn {
        let stream = UnixStream::connect(self.root.join("bus.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        Conn {
            reader: BufReader::new(stream.try_clone().unwrap()),
            stream,
        }
    }

    fn join(&self, actor: &str, session: &str) {
        let mut conn = self.connect();
        conn.send("join", actor, session, json!({}));
        let reply = conn.get();
        assert_eq!(reply["ok"], true, "join: {reply}");
    }

    /// Poll the preflight until `done` holds, or fail after a few seconds.
    fn until(&self, what: &str, done: impl Fn(i32, &Value) -> bool) -> (i32, Value) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let (code, report) = dry_run_json(&self.root);
            if done(code, &report) {
                return (code, report);
            }
            assert!(Instant::now() < end, "{what}: {report}");
            thread::sleep(Duration::from_millis(25));
        }
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

struct Conn {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Conn {
    fn send(&mut self, op: &str, actor: &str, session: &str, args: Value) {
        let frame =
            json!({"op": op, "actor": actor, "session": session, "args": args}).to_string() + "\n";
        self.stream.write_all(frame.as_bytes()).unwrap();
    }

    fn get(&mut self) -> Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad frame {line:?}: {e}"))
    }
}

/// Everything a preflight could change, in a comparable form.
fn state(root: &Path) -> String {
    let db = Connection::open(root.join("state.db")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    let mut out = String::new();
    for sql in [
        "SELECT name||':'||enabled||':'||last_seen_ms FROM agents ORDER BY name",
        "SELECT agent||':'||session||':'||last_seen_ms||':'||coalesce(ended_ms,'') FROM sessions ORDER BY agent,session",
        "SELECT agent||':'||session||':'||refreshed_ms FROM session_waits ORDER BY agent,session",
        "SELECT count(*)||'' FROM events",
    ] {
        // An older store may lack a table (9d8580e has no session_waits).
        let Ok(mut stmt) = db.prepare(sql) else {
            out += "absent\n";
            continue;
        };
        let rows: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap();
        out += &format!("{rows:?}\n");
    }
    out
}

#[test]
fn an_idle_daemon_exits_zero_and_the_preflight_changes_nothing() {
    let d = Daemon::start_with(env!("CARGO_BIN_EXE_fray"));
    d.join("bob", "s-bob");
    let before = state(&d.root);
    let (code, report) = dry_run_json(&d.root);
    assert_eq!(code, 0, "{report}");
    assert_eq!(report["verdict"], "idle");
    assert_eq!(report["source"], "occupancy");
    assert_eq!(report["degraded"], Value::Null);
    assert_eq!(report["interrupts"], json!([]));
    assert_eq!(report["present"][0]["agent"], "bob", "{report}");
    assert_eq!(report["present"][0]["session"], "s-bob");
    let (code, text) = dry_run(&d.root, false);
    assert_eq!(code, 0);
    assert!(text.contains("verdict: idle (exit 0)"), "{text}");
    assert_eq!(state(&d.root), before, "a preflight must not mutate");
}

#[test]
fn an_open_wait_is_busy_and_once_it_returns_its_row_is_only_armed() {
    let d = Daemon::start_with(env!("CARGO_BIN_EXE_fray"));
    d.join("alice", "s-alice");
    let mut wait = d.connect();
    wait.send("wait", "alice", "s-alice", json!({"timeout": 30}));
    let (code, report) = d.until("wait open", |code, _| code == 3);
    assert_eq!(report["verdict"], "busy");
    // The open connection and its own wait row are one holder, not two.
    assert_eq!(
        report["interrupts"],
        json!([{"class":"live","kind":"connection","actor":"alice","session":"s-alice",
            "present_sessions":[],"detail":"open wait connection"}]),
        "{report}"
    );
    let (text_code, text) = dry_run(&d.root, false);
    assert_eq!((code, text_code), (3, 3));
    assert!(
        text.contains(
            "a restart would interrupt:\n  - alice (session s-alice): open wait connection"
        ),
        "{text}"
    );

    // The waiter hangs up: its wait row stays fresh for WAIT_FRESH_MS, but
    // nothing is open any more.
    drop(wait);
    let before = state(&d.root);
    let (code, report) = d.until("wait closed", |code, _| code != 3);
    assert_eq!(code, 5, "{report}");
    assert_eq!(report["verdict"], "armed");
    let armed = &report["interrupts"][0];
    assert_eq!(armed["class"], "armed");
    assert_eq!(armed["kind"], "wait");
    assert_eq!(armed["actor"], "alice");
    assert_eq!(armed["session"], "s-alice");
    assert_eq!(
        report["report"]["verdict"], "busy",
        "occupancy itself is unchanged"
    );
    let (_, text) = dry_run(&d.root, false);
    assert!(text.contains("armed only"), "{text}");
    assert!(
        text.contains("alice (session s-alice): armed wait"),
        "{text}"
    );
    assert_eq!(state(&d.root), before, "a preflight must not mutate");
}

#[test]
fn no_daemon_is_its_own_exit_status() {
    let root = scratch("n");
    let (code, report) = dry_run_json(&root);
    assert_eq!(code, 7);
    assert_eq!(report["verdict"], "not_running");
    assert_eq!(report["source"], "none");
    assert_eq!(report["daemon"], Value::Null);
    let _ = std::fs::remove_dir_all(&root);
}

/// A daemon that predates `occupancy`: it answers `ping` without that
/// capability, with its connection counts, and `agents` with a fixed roster.
/// Records every op it receives.
struct OldDaemon {
    root: PathBuf,
    ops: Arc<Mutex<Vec<String>>>,
}

impl OldDaemon {
    fn start(long_lived: u64, items: Value) -> Self {
        let root = scratch("o");
        let listener = UnixListener::bind(root.join("bus.sock")).unwrap();
        let ops = Arc::new(Mutex::new(Vec::new()));
        let seen = ops.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let seen = seen.clone();
                let items = items.clone();
                thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap();
                    for line in BufReader::new(stream).lines() {
                        let Ok(line) = line else { return };
                        let req: Value = serde_json::from_str(&line).unwrap();
                        let op = req["op"].as_str().unwrap().to_owned();
                        seen.lock().unwrap().push(op.clone());
                        let data = match op.as_str() {
                            "ping" => {
                                json!({"version":"0.2.1","build":"oldbuild","protocol_version":2,
                                "capabilities":["sessions","agents_all","keepalive"],
                                "capacity":{"clients":1,"long_lived":long_lived},"cursor":0,"time_ms":1})
                            }
                            "agents" => json!({"items":items,"more":false}),
                            _ => {
                                let frame = json!({"ok":false,"error":{"code":"invalid","message":format!("unknown operation: {op}")}});
                                writeln!(writer, "{frame}").unwrap();
                                continue;
                            }
                        };
                        writeln!(writer, "{}", json!({"ok":true,"data":data})).unwrap();
                    }
                });
            }
        });
        Self { root, ops }
    }

    fn ops(&self) -> Vec<String> {
        self.ops.lock().unwrap().clone()
    }
}

impl Drop for OldDaemon {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn roster_entry(name: &str, session: &str) -> Value {
    json!({"name":name,"enabled":true,"recently_seen":false,"controller":null,"listener":null,"keepalive":null,
        "session":{"bound":{"session":session,"live":true,"last_seen_ms":1,"since_ms":1},"recent_takeover":null}})
}

#[test]
fn an_old_daemon_with_nothing_live_is_degraded_idle_not_idle() {
    let old = OldDaemon::start(0, json!([roster_entry("bob", "s-bob")]));
    let (code, report) = dry_run_json(&old.root);
    assert_eq!(code, 6, "{report}");
    assert_eq!(report["verdict"], "degraded_idle");
    assert_eq!(report["source"], "agents");
    assert_eq!(
        report["degraded"],
        "daemon predates occupancy; watchers and waits not visible"
    );
    assert_eq!(report["present"][0]["agent"], "bob");
    let (_, text) = dry_run(&old.root, false);
    assert!(
        text.contains("degraded: daemon predates occupancy; watchers and waits not visible"),
        "{text}"
    );
    let ops = old.ops();
    assert!(
        ops.iter().all(|op| op == "ping" || op == "agents"),
        "only reads, and never occupancy: {ops:?}"
    );
}

#[test]
fn an_old_daemon_names_live_listeners_drives_and_keepalives_as_busy() {
    let mut listener = roster_entry("bob", "s-bob");
    listener["listener"] = json!({"live":true,"run_id":"r1"});
    let mut drive = roster_entry("carol", "s-carol");
    drive["controller"] = json!({"live":true,"state":"waiting","run_id":"r2"});
    let mut keepalive = roster_entry("dave", "s-dave");
    keepalive["keepalive"] = json!({"state":"keepalive","pid":4242});
    let old = OldDaemon::start(1, json!([listener, drive, keepalive]));
    let (code, report) = dry_run_json(&old.root);
    assert_eq!(code, 3, "{report}");
    assert_eq!(report["verdict"], "busy");
    assert!(report["degraded"].is_string());
    let kinds: Vec<(String, String)> = report["interrupts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["kind"].as_str().unwrap().to_owned(),
                i["actor"].as_str().unwrap_or("").to_owned(),
            )
        })
        .collect();
    assert_eq!(
        kinds,
        [
            ("connections", ""),
            ("listener", "bob"),
            ("drive", "carol"),
            ("keepalive", "dave")
        ]
        .map(|(k, a)| (k.to_owned(), a.to_owned()))
    );
    let (_, text) = dry_run(&old.root, false);
    assert!(
        text.contains("bob (no session recorded; present as s-bob): live listener (run r1)"),
        "{text}"
    );
    assert!(
        text.contains(
            "dave (no session recorded; present as s-dave): keepalive keepalive (pid 4242)"
        ),
        "{text}"
    );
}

#[test]
fn an_old_daemon_with_only_recent_activity_is_armed() {
    let mut recent = roster_entry("alice", "s-alice");
    recent["recently_seen"] = json!(true);
    let old = OldDaemon::start(0, json!([recent]));
    let (code, report) = dry_run_json(&old.root);
    assert_eq!(code, 5, "{report}");
    assert_eq!(report["verdict"], "armed");
    assert_eq!(report["interrupts"][0]["kind"], "recent_activity");
    assert_eq!(
        report["interrupts"][0]["present_sessions"],
        json!(["s-alice"])
    );
}

/// The same against a real pre-occupancy daemon, when FRAY_OLD_BIN names one
/// (e.g. an installed 0.2.1 `fray`). Skipped otherwise: CI has no old build.
#[test]
fn a_real_old_daemon_when_one_is_given() {
    let Ok(bin) = std::env::var("FRAY_OLD_BIN") else {
        eprintln!("FRAY_OLD_BIN unset; skipping the real old-daemon check");
        return;
    };
    let d = Daemon::start_with(&bin);
    d.join("bob", "s-bob");
    thread::sleep(Duration::from_millis(5));
    let before = state(&d.root);
    let (code, report) = dry_run_json(&d.root);
    assert_eq!(code, 5, "bob was just seen: {report}");
    assert_eq!(report["source"], "agents");
    assert_eq!(
        state(&d.root),
        before,
        "a degraded preflight must not mutate"
    );
    d.join("alice", "s-alice");
    let mut wait = d.connect();
    wait.send("wait", "alice", "s-alice", json!({"timeout": 30}));
    let (code, report) = d.until("old wait open", |code, _| code == 3);
    assert_eq!(code, 3);
    assert_eq!(report["interrupts"][0]["kind"], "connections", "{report}");
    assert!(
        report["interrupts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["actor"] == "alice" && i["kind"] == "recent_activity"),
        "{report}"
    );
}
