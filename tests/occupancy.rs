//! `occupancy` (daemon lifecycle L4): who a restart would interrupt. It names
//! open long-lived connections, armed waits and live keepalive drives, never
//! counts the caller, and changes nothing.
use fray::model::{now_ms, random_key};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Daemon {
    root: PathBuf,
    child: Child,
}

impl Daemon {
    fn start() -> Self {
        let root = PathBuf::from("/tmp").join(format!("fray-oc-{}", &random_key().unwrap()[..8]));
        std::fs::create_dir_all(&root).unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_fray"))
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

    fn rpc(&self, op: &str, actor: &str, args: Value) -> Value {
        let mut conn = self.connect();
        conn.send(op, actor, args);
        let reply = conn.get();
        assert_eq!(reply["ok"], true, "{op}: {reply}");
        reply["data"].clone()
    }

    fn occupancy(&self) -> Value {
        self.rpc("occupancy", "probe", json!({}))
    }

    fn db(&self) -> Connection {
        let db = Connection::open(self.root.join("state.db")).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db
    }

    /// Poll occupancy until `done` holds, or fail after a few seconds.
    fn until(&self, what: &str, done: impl Fn(&Value) -> bool) -> Value {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let report = self.occupancy();
            if done(&report) {
                return report;
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
    fn send(&mut self, op: &str, actor: &str, args: Value) {
        let frame = json!({"op": op, "actor": actor, "args": args}).to_string() + "\n";
        self.stream.write_all(frame.as_bytes()).unwrap();
    }

    fn get(&mut self) -> Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad frame {line:?}: {e}"))
    }
}

fn reasons(report: &Value) -> String {
    report["reasons"].to_string()
}

#[test]
fn an_idle_daemon_is_idle_without_counting_the_caller_or_recording_it() {
    let d = Daemon::start();
    let ping = d.rpc("ping", "", json!({}));
    assert!(
        ping["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("occupancy")),
        "{ping}"
    );
    let report = d.occupancy();
    assert_eq!(report["verdict"], "idle", "{report}");
    assert_eq!(report["reasons"], json!([]));
    assert_eq!(report["holders"], json!([]));
    assert_eq!(
        report["clients"]["connected"], 0,
        "the caller is not counted"
    );
    assert_eq!(report["clients"]["long_lived"], 0);
    assert_eq!(report["long_lived"], json!([]));
    // Another short client is reported but does not make the daemon busy.
    let _other = d.connect();
    let report = d.until("second client counted", |r| r["clients"]["connected"] == 1);
    assert_eq!(report["verdict"], "idle", "{report}");
    // Asking leaves no agent, session or wait behind.
    let mut asking = d.connect();
    let frame = json!({"op":"occupancy","actor":"probe","session":"probe-session","args":{}});
    asking
        .stream
        .write_all((frame.to_string() + "\n").as_bytes())
        .unwrap();
    assert_eq!(asking.get()["ok"], true);
    let db = d.db();
    for table in ["agents", "sessions", "session_waits", "presented_batches"] {
        let n: i64 = db
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0, "occupancy wrote to {table}");
    }
    // Unknown arguments are refused, as for every op.
    let mut bad = d.connect();
    bad.send("occupancy", "probe", json!({"force":true}));
    assert_eq!(bad.get()["ok"], false);
}

#[test]
fn an_attached_wait_is_busy_and_names_its_actor_until_it_hangs_up() {
    let d = Daemon::start();
    d.rpc("join", "waiter", json!({}));
    let mut wait = d.connect();
    wait.send("wait", "waiter", json!({"timeout": 30}));
    let report = d.until("wait registered", |r| {
        r["long_lived"].as_array().is_some_and(|l| !l.is_empty())
    });
    assert_eq!(report["verdict"], "busy", "{report}");
    assert_eq!(report["holders"], json!(["waiter"]));
    let open = &report["long_lived"][0];
    assert_eq!(open["op"], "wait");
    assert_eq!(open["actor"], "waiter");
    assert_eq!(open["keepalive"], false);
    assert!(open["since_ms"].as_i64().unwrap() <= report["time_ms"].as_i64().unwrap());
    assert_eq!(report["clients"]["long_lived"], 1);
    assert_eq!(
        report["clients"]["connected"], 1,
        "the waiter, not the caller"
    );
    assert!(
        reasons(&report).contains("open wait connection by \\\"waiter\\\""),
        "{report}"
    );
    // A hang-up deregisters the connection; the wait it armed still counts
    // until its row expires.
    drop(wait);
    let report = d.until("wait deregistered", |r| r["long_lived"] == json!([]));
    assert_eq!(report["verdict"], "busy", "{report}");
    assert!(
        reasons(&report).contains("armed wait for \\\"waiter\\\""),
        "{report}"
    );
}

#[test]
fn an_armed_wait_is_busy_until_it_expires() {
    let d = Daemon::start();
    d.rpc("join", "armed", json!({}));
    let db = d.db();
    let now = now_ms();
    db.execute(
        "INSERT INTO session_waits(agent,session,refreshed_ms) VALUES('armed','s1',?)",
        [now],
    )
    .unwrap();
    let report = d.occupancy();
    assert_eq!(report["verdict"], "busy", "{report}");
    assert_eq!(report["holders"], json!(["armed"]));
    let wait = &report["waits"][0];
    assert_eq!(wait["session"], "s1");
    assert!(wait["expires_ms"].as_i64().unwrap() > report["time_ms"].as_i64().unwrap());
    // Expired: not refreshed within the wait window.
    db.execute(
        "UPDATE session_waits SET refreshed_ms=? WHERE agent='armed'",
        [now - 200_000],
    )
    .unwrap();
    let report = d.occupancy();
    assert_eq!(report["verdict"], "idle", "{report}");
    assert_eq!(report["waits"], json!([]));
}

#[test]
fn a_live_keepalive_drive_is_busy_and_a_dead_one_is_not() {
    let d = Daemon::start();
    d.rpc("join", "kept", json!({}));
    let db = d.db();
    let insert = |pid: i64, started: i64| {
        db.execute(
            "INSERT OR REPLACE INTO keepalives(agent,session,companion,host,cwd,log,pid,budget,stop_requested,started_ms) VALUES('kept','keepalive:x','c','claude','/','/dev/null',?,1,0,?)",
            params![pid, started],
        )
        .unwrap();
    };
    // This test process stands in for the drive.
    insert(std::process::id() as i64, now_ms() - 600_000);
    let report = d.occupancy();
    assert_eq!(report["verdict"], "busy", "{report}");
    assert_eq!(report["holders"], json!(["kept"]));
    assert_eq!(report["keepalives"][0]["pid_alive"], true);
    assert_eq!(report["keepalives"][0]["live"], true);
    assert!(
        reasons(&report).contains("keepalive drive for \\\"kept\\\""),
        "{report}"
    );
    // A drive that has exited.
    let mut gone = Command::new("/usr/bin/true").spawn().unwrap();
    let pid = gone.id() as i64;
    gone.wait().unwrap();
    insert(pid, now_ms() - 600_000);
    let report = d.occupancy();
    assert_eq!(report["verdict"], "idle", "{report}");
    assert_eq!(report["keepalives"][0]["pid_alive"], false);
    assert_eq!(report["keepalives"][0]["live"], false);
}

#[test]
fn an_attention_stream_is_busy_as_a_connection_and_a_live_listener() {
    let d = Daemon::start();
    d.rpc("join", "listener", json!({}));
    let mut stream = d.connect();
    stream.send("watch_attention", "listener", json!({"run_id": "run-1"}));
    assert_eq!(stream.get()["data"]["type"], "ready");
    let report = d.occupancy();
    assert_eq!(report["verdict"], "busy", "{report}");
    assert_eq!(report["holders"], json!(["listener"]));
    assert_eq!(report["long_lived"][0]["op"], "watch_attention");
    assert_eq!(report["listeners"][0]["agent"], "listener");
    assert!(
        reasons(&report).contains("live listener for \\\"listener\\\""),
        "{report}"
    );
    drop(stream);
    let report = d.until("listener ended", |r| {
        r["long_lived"] == json!([]) && r["listeners"] == json!([])
    });
    assert_eq!(report["verdict"], "idle", "{report}");
}
