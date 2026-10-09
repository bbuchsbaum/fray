//! `fray restart` (daemon lifecycle L9): preflight, announce, drain, start,
//! verify, against real daemons on scratch homes, plus fake old daemons for
//! the paths a current build cannot show. Set FRAY_OLD_BIN to an installed
//! pre-lifecycle `fray` to also restart a real old daemon.
use fray::model::random_key;
use rusqlite::Connection;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::{fs::PermissionsExt, net::UnixListener, net::UnixStream},
    path::PathBuf,
    process::{Command, Output, Stdio},
    sync::{Arc, Mutex},
    thread,
    time::{Duration, Instant},
};

const BIN: &str = env!("CARGO_BIN_EXE_fray");

/// A scratch home with its own daemon registry. Whatever daemon answers on
/// it at the end is stopped.
struct Home {
    root: PathBuf,
    home: PathBuf,
    state: PathBuf,
}

impl Home {
    fn new(tag: &str) -> Self {
        let root =
            PathBuf::from("/tmp").join(format!("fray-rc{tag}-{}", &random_key().unwrap()[..8]));
        std::fs::create_dir_all(root.join("h")).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        Self {
            home: root.join("h"),
            state: root.join("s"),
            root,
        }
    }

    fn command(&self, bin: &str) -> Command {
        let mut command = Command::new(bin);
        command
            .args(["--home", self.home.to_str().unwrap()])
            .env("FRAY_STATE_DIR", &self.state)
            .env("USER", "tester")
            .env_remove("FRAY_AGENT")
            .env_remove("FRAY_SESSION")
            .env_remove("FRAY_HOME")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CODEX_THREAD_ID");
        command
    }

    /// `fray --json ARGS`: exit status, stdout as JSON, stderr.
    fn json(&self, args: &[&str]) -> (i32, Value, String) {
        let out = self.command(BIN).arg("--json").args(args).output().unwrap();
        let value = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?} gave no JSON ({e}): {}", text(&out)));
        (
            out.status.code().unwrap(),
            value,
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    fn ok(&self, actor: &str, args: &[&str]) -> Value {
        let mut all = vec!["--as", actor];
        all.extend_from_slice(args);
        let (code, value, err) = self.json(&all);
        assert_eq!(code, 0, "{args:?}: {value} {err}");
        value
    }

    fn start(&self, bin: &str) {
        let out = self.command(bin).arg("start").output().unwrap();
        assert!(out.status.success(), "start: {}", text(&out));
    }

    fn ping(&self) -> Option<Value> {
        fray::client::probe(
            &self.home,
            &fray::model::Request::new("ping", "", json!({})),
            2,
        )
        .ok()
    }

    fn pid(&self) -> u64 {
        self.ping().unwrap()["pid"].as_u64().unwrap()
    }

    fn db(&self) -> Connection {
        let db = Connection::open(self.home.join("state.db")).unwrap();
        db.busy_timeout(Duration::from_secs(5)).unwrap();
        db
    }

    /// The maintenance notices, oldest first: (title, status).
    fn notices(&self) -> Vec<(String, String)> {
        let db = self.db();
        let mut stmt = db
            .prepare("SELECT title,status FROM cards WHERE author='fray' ORDER BY id")
            .unwrap();
        stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    fn connect(&self) -> Conn {
        let stream = UnixStream::connect(self.home.join("bus.sock")).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        Conn {
            reader: BufReader::new(stream.try_clone().unwrap()),
            stream,
        }
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        // Daemons started through `fray start` or `fray restart` are detached.
        for _ in 0..3 {
            let Some(pid) = self.ping().and_then(|p| p["pid"].as_u64()) else {
                // An old daemon reports no pid: stop it by request.
                if self.ping().is_some() {
                    let _ = self.command(BIN).arg("stop").output();
                    thread::sleep(Duration::from_millis(300));
                    continue;
                }
                break;
            };
            let _ = self.command(BIN).arg("stop").output();
            if !fray::client::await_exit(pid as u32, Duration::from_secs(5)) {
                let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
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

    fn get(&mut self) -> Option<Value> {
        let mut line = String::new();
        if self.reader.read_line(&mut line).unwrap_or(0) == 0 {
            return None;
        }
        Some(serde_json::from_str(&line).unwrap())
    }
}

/// Polls `fray restart --dry-run` until it exits `code`.
fn until_preflight(home: &Home, code: i32) -> Value {
    let end = Instant::now() + Duration::from_secs(10);
    loop {
        let (got, report, _) = home.json(&["restart", "--dry-run"]);
        if got == code {
            return report;
        }
        assert!(
            Instant::now() < end,
            "preflight never exited {code}: {report}"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn an_idle_restart_verifies_the_build_and_posts_both_notices() {
    let home = Home::new("i");
    home.start(BIN);
    home.ok("alice", &["join"]);
    let before = home.ping().unwrap();
    let (code, out, err) = home.json(&["restart", "--reason", "nightly upgrade"]);
    assert_eq!(code, 0, "{out} {err}");
    assert_eq!(out["action"], "restarted");
    assert_eq!(out["verdict"], "idle");
    assert_eq!(out["before"]["pid"], before["pid"]);
    assert_eq!(out["before"]["pid_source"], "ping");
    assert_eq!(out["exe"]["source"], "current_exe");
    assert_eq!(out["after"]["build"], before["build"]);
    assert_eq!(out["after"]["build"], out["exe"]["build"]);
    assert_eq!(out["stop"]["mode"], "graceful");
    assert_eq!(out["store_id"], before["store_id"]);
    assert_ne!(out["after"]["pid"], before["pid"]);
    let after = home.ping().unwrap();
    assert_eq!(after["pid"], out["after"]["pid"]);
    assert_eq!(after["store_id"], before["store_id"]);
    // One restart notice, superseded by the restarted notice that follows it.
    assert_eq!(
        home.notices(),
        [
            (
                "Fray daemon restart: nightly upgrade".to_owned(),
                "superseded".to_owned()
            ),
            (
                "Fray daemon restarted: nightly upgrade".to_owned(),
                "open".to_owned()
            ),
        ]
    );
    assert!(out["announce"]["restart"].is_i64(), "{out}");
    assert!(out["announce"]["restarted"].is_i64(), "{out}");
    // The requester is recorded, never registered.
    let joined: i64 = home
        .db()
        .query_row("SELECT count(*) FROM agents WHERE name='tester'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(joined, 0);
    // The registry now names the replacement.
    let record: Value = serde_json::from_slice(
        &std::fs::read(fray::registry::record_path(&home.state, &home.home)).unwrap(),
    )
    .unwrap();
    assert_eq!(record["pid"], out["after"]["pid"]);
    assert!(!home.home.join("daemon.restarting").exists());
    // The human form names both sides.
    let out = home
        .command(BIN)
        .args(["restart", "--no-announce"])
        .output()
        .unwrap();
    let shown = text(&out);
    assert!(out.status.success(), "{shown}");
    assert!(shown.contains("restarted "), "{shown}");
    assert!(
        shown.contains("announcement: none (--no-announce)"),
        "{shown}"
    );
    assert!(shown.contains("before: build "), "{shown}");
}

#[test]
fn no_daemon_running_is_simply_started() {
    let home = Home::new("n");
    let (code, out, err) = home.json(&["restart"]);
    assert_eq!(code, 0, "{out} {err}");
    assert_eq!(out["action"], "started");
    assert_eq!(out["verdict"], "not_running");
    assert_eq!(out["before"], Value::Null);
    assert_eq!(out["announce"]["restart"], Value::Null);
    assert!(home.ping().is_some());
    assert!(home.notices().is_empty());
}

#[test]
fn a_busy_daemon_is_refused_naming_the_holder() {
    let home = Home::new("b");
    home.start(BIN);
    home.ok("bob", &["join"]);
    let pid = home.pid();
    let mut wait = home.connect();
    wait.send("wait", "bob", json!({"timeout": 30}));
    until_preflight(&home, 3);
    let (code, out, _) = home.json(&["restart"]);
    assert_eq!(code, 3, "{out}");
    assert_eq!(out["action"], "refused");
    assert_eq!(out["verdict"], "busy");
    assert!(
        out["preflight"]["interrupts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["actor"] == "bob" && i["class"] == "live"),
        "{out}"
    );
    let shown = text(&home.command(BIN).arg("restart").output().unwrap());
    assert!(shown.contains("bob"), "{shown}");
    assert!(shown.contains("--force"), "{shown}");
    // Nothing changed: same daemon, no notice, no marker.
    assert_eq!(home.pid(), pid);
    assert!(home.notices().is_empty());
    assert!(!home.home.join("daemon.restarting").exists());
}

#[test]
fn force_interrupts_a_live_wait_that_resumes_and_receives_once() {
    let home = Home::new("f");
    home.start(BIN);
    home.ok("alice", &["join"]);
    home.ok("bob", &["join"]);
    let waiter = home
        .command(BIN)
        .args(["--json", "--as", "bob", "wait", "--new", "--timeout", "60"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    until_preflight(&home, 3);
    let (code, out, err) = home.json(&["restart", "--force"]);
    assert_eq!(code, 0, "{out} {err}");
    assert_eq!(out["forced"], true);
    assert!(
        out["interrupted"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["actor"] == "bob"),
        "{out}"
    );
    let sent = home.ok("alice", &["send", "bob", "after the restart"]);
    let got = waiter.wait_with_output().unwrap();
    assert!(got.status.success(), "{}", text(&got));
    let page: Value = serde_json::from_slice(&got.stdout).unwrap();
    assert_eq!(page["total"], 1, "{page}");
    assert_eq!(page["items"][0]["card"]["id"], sent["card"]["id"], "{page}");
}

#[test]
fn armed_waits_refuse_unless_allowed() {
    let home = Home::new("a");
    home.start(BIN);
    home.ok("bob", &["join"]);
    // A finite wait that has returned leaves its row armed.
    let out = home
        .command(BIN)
        .args(["--as", "bob", "wait", "--timeout", "1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3), "{}", text(&out));
    let pid = home.pid();
    let (code, out, _) = home.json(&["restart"]);
    assert_eq!(code, 5, "{out}");
    assert_eq!(out["verdict"], "armed");
    assert_eq!(home.pid(), pid);
    let (code, out, err) = home.json(&["restart", "--allow-armed"]);
    assert_eq!(code, 0, "{out} {err}");
    assert_eq!(out["action"], "restarted");
    assert_eq!(out["interrupted"][0]["actor"], "bob", "{out}");
    assert_eq!(out["interrupted"][0]["kind"], "wait", "{out}");
}

/// A keepalive drive between turns survives a restart (L8), so it never
/// blocks one; the same drive mid-turn does. Its rows are written directly,
/// with a stand-in process, and read by the preflight alone.
#[test]
fn an_idle_keepalive_survives_and_only_a_running_turn_blocks() {
    let home = Home::new("k");
    home.start(BIN);
    home.ok("alice", &["join"]);
    let mut stand_in = Command::new("sleep").arg("60").spawn().unwrap();
    let db = home.db();
    let now = fray::model::now_ms();
    db.execute(
        "INSERT INTO keepalives(agent,session,companion,host,cwd,log,pid,budget,stop_requested,started_ms) VALUES('alice','keepalive:c1','alice-k','claude','/tmp','/tmp/k.log',?,1000,0,?)",
        rusqlite::params![stand_in.id(), now],
    )
    .unwrap();
    db.execute(
        "INSERT INTO controllers(agent,run_id,state,updated_ms,reason) VALUES('alice','r1','waiting',?,NULL)",
        [now],
    )
    .unwrap();
    let report = until_preflight(&home, 0);
    let classes: Vec<(String, String)> = report["interrupts"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["kind"].as_str().unwrap().to_owned(),
                i["class"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert_eq!(
        classes,
        [
            ("drive".to_owned(), "survives".to_owned()),
            ("keepalive".to_owned(), "survives".to_owned())
        ],
        "{report}"
    );
    let shown = text(
        &home
            .command(BIN)
            .args(["restart", "--dry-run"])
            .output()
            .unwrap(),
    );
    assert!(shown.contains("survives the restart"), "{shown}");
    db.execute(
        "UPDATE controllers SET state='running',updated_ms=? WHERE agent='alice'",
        [fray::model::now_ms()],
    )
    .unwrap();
    let report = until_preflight(&home, 3);
    assert!(
        report["interrupts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "keepalive" && i["class"] == "live"),
        "{report}"
    );
    // Stop the stand-in before the daemon could ever adopt it.
    db.execute("DELETE FROM keepalives", []).unwrap();
    let _ = stand_in.kill();
    let _ = stand_in.wait();
}

#[test]
fn a_failed_announcement_never_restarts() {
    let fake = Fake::start(
        &["sessions", "agents_all", "announce", "graceful_restart"],
        |op, _| match op {
            "announce" => {
                Some(json!({"ok":false,"error":{"code":"internal","message":"disk full"}}))
            }
            _ => None,
        },
    );
    let (code, out, _) = fake.home.json(&["restart"]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(out["error"]["code"], "announce_failed", "{out}");
    assert!(
        out["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not restarted"),
        "{out}"
    );
    let ops = fake.ops();
    assert!(ops.contains(&"announce".to_owned()), "{ops:?}");
    assert!(!ops.contains(&"shutdown".to_owned()), "{ops:?}");
    assert!(!fake.home.home.join("daemon.restarting").exists());
}

#[test]
fn a_daemon_without_announce_needs_no_announce_and_old_daemons_are_restarted() {
    let fake = Fake::start(&["sessions", "agents_all"], |_, _| None);
    let (code, out, _) = fake.home.json(&["restart"]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(out["error"]["code"], "announce_unsupported", "{out}");
    assert!(!fake.ops().contains(&"shutdown".to_owned()));
    // Explicitly without a notice: a plain shutdown, then our daemon.
    let (code, out, err) = fake.home.json(&["restart", "--no-announce"]);
    assert_eq!(code, 0, "{out} {err}");
    assert!(err.contains("DEGRADED"), "{err}");
    assert_eq!(out["verdict"], "degraded_idle");
    assert_eq!(
        out["degraded"],
        "daemon predates occupancy; watchers and waits not visible"
    );
    assert_eq!(out["stop"]["mode"], "plain");
    assert_eq!(out["before"]["build"], "oldbuild");
    assert_eq!(out["before"]["pid"], Value::Null);
    assert_eq!(out["announce"]["skipped"], "--no-announce");
    assert!(
        out["warnings"][0]
            .as_str()
            .unwrap()
            .contains("reported no pid"),
        "{out}"
    );
    let shutdown = fake.requests("shutdown");
    assert_eq!(shutdown, [json!({})], "plain shutdown for an old daemon");
    assert_eq!(fake.home.ping().unwrap()["build"], out["exe"]["build"]);
}

#[test]
fn a_replacement_of_another_build_fails_loudly() {
    let home = Home::new("m");
    home.start(BIN);
    // A binary that claims one build and runs another.
    let wrapper = home.root.join("fray-other");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'fray 0.0.0 (not-this-build)'; exit 0; fi\nexec '{BIN}' \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (code, out, _) = home.json(&["restart", "--exe", wrapper.to_str().unwrap()]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(out["error"]["code"], "restart_unverified", "{out}");
    let message = out["error"]["message"].as_str().unwrap();
    assert!(message.contains("not-this-build"), "{message}");
    assert!(
        message.contains(home.home.join("daemon.log").to_str().unwrap()),
        "{message}"
    );
    // The restart notice went out, and an abandoned one followed it.
    let notices = home.notices();
    assert_eq!(notices.len(), 2, "{notices:?}");
    assert_eq!(notices[0].1, "superseded", "{notices:?}");
    assert!(
        notices[1].0.starts_with("Fray daemon restart abandoned: "),
        "{notices:?}"
    );
    assert!(
        out["error"]["details"]["announce"]["abandoned"].is_i64(),
        "{out}"
    );
}

#[test]
fn a_short_request_in_the_drain_gap_waits_for_the_replacement() {
    let home = Home::new("g");
    home.start(BIN);
    home.ok("alice", &["join"]);
    // A graceful stop with no restarter leaves the daemon's own marker, which
    // dies with it: a dead home's clients never wait for it.
    let stopped = home.ok("tester", &["stop", "--restart", "--reason", "gap"]);
    assert_eq!(stopped["exited"], true);
    assert!(home.home.join("daemon.restarting").exists());
    assert_eq!(fray::restart::marker(&home.home), None);
    let fails_fast = |what: &str| {
        let started = Instant::now();
        let out = home
            .command(BIN)
            .args(["--json", "--as", "alice", "agents"])
            .output()
            .unwrap();
        assert!(!out.status.success(), "{what}: {}", text(&out));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{what}: {:?}",
            started.elapsed()
        );
    };
    fails_fast("the daemon's marker after it exited");
    // A live restarter's marker (this test's process): the request waits.
    fray::restart::write_marker(&home.home, "gap").unwrap();
    assert_eq!(fray::restart::marker(&home.home).as_deref(), Some("gap"));
    let started = Instant::now();
    let request = home
        .command(BIN)
        .args(["--json", "--as", "alice", "agents"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    thread::sleep(Duration::from_millis(800));
    home.start(BIN);
    let out = request.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(started.elapsed() >= Duration::from_millis(800));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("daemon restarting (gap)"),
        "{}",
        text(&out)
    );
    // The replacement removed it on bind.
    assert!(!home.home.join("daemon.restarting").exists());
    let _ = home.command(BIN).arg("stop").output();
    let end = Instant::now() + Duration::from_secs(5);
    while home.ping().is_some() && Instant::now() < end {
        thread::sleep(Duration::from_millis(20));
    }
    fails_fast("no marker");
    let me = std::process::id();
    for (what, marker) in [
        (
            "an old marker of a live writer",
            json!({"reason":"old","pid":me,"written_ms":fray::model::now_ms() - 31_000}),
        ),
        (
            "a fresh marker of a dead writer",
            json!({"reason":"dead","pid":999_999,"written_ms":fray::model::now_ms()}),
        ),
    ] {
        std::fs::write(home.home.join("daemon.restarting"), marker.to_string()).unwrap();
        fails_fast(what);
    }
}

/// A connect that dies before even the handshake ping is answered (a daemon
/// exiting from a drain) sent nothing operational: with a live restarter's
/// marker, the request waits for the replacement.
#[test]
fn a_handshake_cut_off_by_an_exiting_daemon_is_resent_to_the_replacement() {
    let home = Home::new("h");
    let socket = home.home.join("bus.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    fray::restart::write_marker(&home.home, "cut").unwrap();
    let closer = thread::spawn(move || {
        // Accept one connection and close it unanswered, then go away.
        let (stream, _) = listener.accept().unwrap();
        let _ = std::fs::remove_file(&socket);
        drop(stream);
    });
    let request = home
        .command(BIN)
        .args(["--json", "--as", "alice", "agents"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    closer.join().unwrap();
    thread::sleep(Duration::from_millis(500));
    home.start(BIN);
    let out = request.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("daemon restarting (cut)"),
        "{}",
        text(&out)
    );
}

/// An idle open connection must not hold the drain for its whole grace.
#[test]
fn idle_connections_are_closed_early_in_the_drain() {
    let home = Home::new("d");
    let mut daemon = home
        .command(BIN)
        .arg("serve")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    while home.ping().is_none() {
        assert!(Instant::now() < end, "daemon did not start");
        thread::sleep(Duration::from_millis(25));
    }
    let mut idle = home.connect();
    idle.send("ping", "", json!({}));
    assert_eq!(idle.get().unwrap()["ok"], true);
    let mut stop = home.connect();
    let started = Instant::now();
    stop.send(
        "shutdown",
        "",
        json!({"restart":true,"reason":"drain","grace_ms":10000}),
    );
    assert_eq!(stop.get().unwrap()["ok"], true);
    loop {
        if daemon.try_wait().unwrap().is_some() {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "an idle connection held the drain"
        );
        thread::sleep(Duration::from_millis(20));
    }
    // It stayed open until the daemon exited, and was then closed.
    assert!(idle.get().is_none());
}

/// While something busy holds the drain, an idle connection is still
/// answered: its next request gets the retryable restarting refusal, never
/// a dropped connection.
#[test]
fn an_idle_connection_sending_during_a_drain_is_refused_retryably() {
    let home = Home::new("e");
    let mut daemon = home
        .command(BIN)
        .arg("serve")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    while home.ping().is_none() {
        assert!(Instant::now() < end, "daemon did not start");
        thread::sleep(Duration::from_millis(25));
    }
    let mut idle = home.connect();
    idle.send("ping", "", json!({}));
    assert_eq!(idle.get().unwrap()["ok"], true);
    // A request begun but not finished: busy until the grace ends.
    let mut partial = home.connect();
    partial.stream.write_all(b"{\"op\":\"ping\"").unwrap();
    thread::sleep(Duration::from_millis(100));
    let mut stop = home.connect();
    stop.send(
        "shutdown",
        "",
        json!({"restart":true,"reason":"held","grace_ms":2000}),
    );
    assert_eq!(stop.get().unwrap()["ok"], true);
    thread::sleep(Duration::from_millis(300));
    idle.send("agents", "alice", json!({}));
    let frame = idle.get().expect("refused, not dropped");
    assert_eq!(frame["type"], "restarting", "{frame}");
    assert_eq!(frame["error"]["details"]["restarting"], true, "{frame}");
    // A request arriving with the stop, already in the buffer, is answered.
    let started = Instant::now();
    while daemon.try_wait().unwrap().is_none() {
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "drain never ended"
        );
        thread::sleep(Duration::from_millis(20));
    }
}

/// A connection whose ping is already buffered when the stop arrives is
/// answered (or refused retryably), never closed unread.
#[test]
fn a_request_racing_the_stop_is_never_dropped_unread() {
    for _ in 0..10 {
        let home = Home::new("r");
        let mut daemon = home
            .command(BIN)
            .arg("serve")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let end = Instant::now() + Duration::from_secs(5);
        while home.ping().is_none() {
            assert!(Instant::now() < end, "daemon did not start");
            thread::sleep(Duration::from_millis(25));
        }
        let mut racer = home.connect();
        let mut stop = home.connect();
        racer.send("agents", "alice", json!({}));
        stop.send(
            "shutdown",
            "",
            json!({"restart":true,"reason":"race","grace_ms":2000}),
        );
        let frame = racer.get().expect("answered or refused, not dropped");
        assert!(
            frame["ok"] == true || frame["type"] == "restarting",
            "{frame}"
        );
        let _ = stop.get();
        let _ = daemon.wait();
    }
}

/// A daemon that predates the lifecycle work: it answers `ping` without a
/// pid (with the given capabilities), `agents` with nobody, and `shutdown`
/// by acknowledging and going away. `respond` (given the op and how many of
/// it came before) may answer any op first.
struct Fake {
    home: Home,
    requests: Arc<Mutex<Vec<Value>>>,
}

impl Fake {
    fn start(
        capabilities: &[&str],
        respond: impl Fn(&str, usize) -> Option<Value> + Send + Sync + 'static,
    ) -> Self {
        let respond = Arc::new(respond);
        let home = Home::new("o");
        let socket = home.home.join("bus.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let requests: Arc<Mutex<Vec<Value>>> = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        let capabilities: Vec<String> = capabilities.iter().map(|c| (*c).to_owned()).collect();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { return };
                let seen = seen.clone();
                let capabilities = capabilities.clone();
                let socket = socket.clone();
                let respond = respond.clone();
                thread::spawn(move || {
                    let mut writer = stream.try_clone().unwrap();
                    for line in BufReader::new(stream).lines() {
                        let Ok(line) = line else { return };
                        let req: Value = serde_json::from_str(&line).unwrap();
                        let op = req["op"].as_str().unwrap().to_owned();
                        let before = {
                            let mut seen = seen.lock().unwrap();
                            let before = seen.iter().filter(|r| r["op"] == op.as_str()).count();
                            seen.push(req.clone());
                            before
                        };
                        if op == "shutdown" {
                            let _ = std::fs::remove_file(&socket);
                        }
                        let frame = match respond(&op, before) {
                            Some(frame) => frame,
                            None => match op.as_str() {
                                "ping" => {
                                    json!({"ok":true,"data":{"version":"0.2.1","build":"oldbuild","protocol_version":2,
                                "capabilities":capabilities,"capacity":{"clients":1,"long_lived":0},"cursor":0,"time_ms":1}})
                                }
                                "agents" => json!({"ok":true,"data":{"items":[],"more":false}}),
                                "shutdown" => json!({"ok":true,"data":{"stopping":true}}),
                                other => {
                                    json!({"ok":false,"error":{"code":"invalid","message":format!("unknown operation: {other}")}})
                                }
                            },
                        };
                        if writeln!(writer, "{frame}").is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Self { home, requests }
    }

    fn ops(&self) -> Vec<String> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .map(|r| r["op"].as_str().unwrap().to_owned())
            .collect()
    }

    fn requests(&self, op: &str) -> Vec<Value> {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["op"] == op)
            .map(|r| r["args"].clone())
            .collect()
    }
}

#[test]
fn a_second_restart_on_the_same_home_fails_fast() {
    use fs2::FileExt;
    let home = Home::new("l");
    home.start(BIN);
    let pid = home.pid();
    let held = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .open(home.home.join("restart.lock"))
        .unwrap();
    held.try_lock_exclusive().unwrap();
    let (code, out, _) = home.json(&["restart"]);
    assert_eq!(code, 1, "{out}");
    assert_eq!(out["error"]["code"], "restart_in_progress", "{out}");
    assert_eq!(home.pid(), pid);
    assert!(home.notices().is_empty());
    drop(held);
    let (code, out, err) = home.json(&["restart"]);
    assert_eq!(code, 0, "{out} {err}");
}

/// Someone arriving between the announcement and the shutdown: the restart
/// is abandoned, said so on the board, and nothing is stopped.
#[test]
fn a_daemon_that_becomes_busy_after_the_announcement_is_not_stopped() {
    let fake = Fake::start(
        &["sessions", "agents_all", "announce", "graceful_restart"],
        |op, before| match (op, before) {
            ("agents", 0) => None,
            ("agents", _) => Some(json!({"ok":true,"data":{"more":false,"items":[{
                "name":"late","enabled":true,"recently_seen":true,"controller":null,"keepalive":null,
                "listener":{"live":true,"run_id":"r9"},"session":{"bound":null,"recent_takeover":null}}]}})),
            ("announce", n) => Some(json!({"ok":true,"data":{"card":{"id":10 + n}}})),
            _ => None,
        },
    );
    let (code, out, err) = fake.home.json(&["restart"]);
    assert_eq!(code, 3, "{out} {err}");
    assert_eq!(out["action"], "abandoned", "{out}");
    assert_eq!(out["announce"]["restart"], 10, "{out}");
    assert_eq!(out["announce"]["abandoned"], 11, "{out}");
    assert_eq!(out["preflight"]["interrupts"][0]["actor"], "late", "{out}");
    let announced: Vec<Value> = fake
        .requests("announce")
        .iter()
        .map(|a| a["action"].clone())
        .collect();
    assert_eq!(announced, [json!("restart"), json!("abandoned")]);
    assert!(!fake.ops().contains(&"shutdown".to_owned()));
    assert!(!fake.home.home.join("daemon.restarting").exists());
}

/// The old daemon's socket went silent but its pid still looks alive (a
/// reused pid): the replacement is started anyway, with a warning.
#[test]
fn a_silent_daemon_whose_pid_lingers_is_replaced_with_a_warning() {
    let lingering = Arc::new(Mutex::new(Command::new("sleep").arg("30").spawn().unwrap()));
    let pid = lingering.lock().unwrap().id();
    let fake = Fake::start(
        &["sessions", "agents_all", "graceful_restart"],
        move |op, _| {
            (op == "shutdown").then(|| {
            json!({"ok":true,"data":{"stopping":true,"restart":true,"reason":"x","grace_ms":0,"pid":pid}})
        })
        },
    );
    let (code, out, err) = fake.home.json(&["restart", "--no-announce"]);
    assert_eq!(code, 0, "{out} {err}");
    assert_eq!(out["before"]["pid"], pid, "{out}");
    assert!(
        out["warnings"].as_array().unwrap().iter().any(|w| w
            .as_str()
            .unwrap()
            .contains(&format!("process {pid} still appears"))),
        "{out}"
    );
    assert_eq!(fake.home.ping().unwrap()["build"], out["exe"]["build"]);
    let mut child = lingering.lock().unwrap();
    let _ = child.kill();
    let _ = child.wait();
}

/// A replacement of another build may not adopt keepalive drives, so an idle
/// one counts as live for it.
#[test]
fn an_idle_keepalive_blocks_a_restart_onto_another_build() {
    let home = Home::new("q");
    home.start(BIN);
    home.ok("alice", &["join"]);
    let mut stand_in = Command::new("sleep").arg("60").spawn().unwrap();
    let db = home.db();
    let now = fray::model::now_ms();
    db.execute(
        "INSERT INTO keepalives(agent,session,companion,host,cwd,log,pid,budget,stop_requested,started_ms) VALUES('alice','keepalive:c1','alice-k','claude','/tmp','/tmp/k.log',?,1000,0,?)",
        rusqlite::params![stand_in.id(), now],
    )
    .unwrap();
    until_preflight(&home, 0);
    let wrapper = home.root.join("fray-other");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'fray 0.0.0 (other-build)'; exit 0; fi\nexec '{BIN}' \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let pid = home.pid();
    let (code, out, _) = home.json(&["restart", "--exe", wrapper.to_str().unwrap()]);
    assert_eq!(code, 3, "{out}");
    assert!(
        out["preflight"]["interrupts"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["kind"] == "keepalive"
                && i["class"] == "live"
                && i["detail"].as_str().unwrap().contains("may not adopt")),
        "{out}"
    );
    assert_eq!(home.pid(), pid);
    db.execute("DELETE FROM keepalives", []).unwrap();
    let _ = stand_in.kill();
    let _ = stand_in.wait();
}

/// A real pre-lifecycle daemon, when FRAY_OLD_BIN names one (e.g. the
/// installed 0.2.1 `fray`): restarted onto this build without a notice,
/// its pid found from the process table. Skipped otherwise.
#[test]
fn a_real_old_daemon_when_one_is_given() {
    let Ok(old) = std::env::var("FRAY_OLD_BIN") else {
        eprintln!("FRAY_OLD_BIN unset; skipping the real old-daemon restart");
        return;
    };
    let home = Home::new("r");
    home.start(&old);
    let before = home.ping().unwrap();
    let (code, out, err) = home.json(&["restart", "--no-announce"]);
    assert_eq!(code, 0, "{out} {err}");
    assert_eq!(out["before"]["build"], before["build"]);
    assert_eq!(out["before"]["pid_source"], "process_table", "{out}");
    let pid = out["before"]["pid"].as_u64().unwrap();
    assert!(fray::client::await_exit(pid as u32, Duration::from_secs(1)));
    assert_eq!(home.ping().unwrap()["build"], out["exe"]["build"]);
    assert_eq!(out["store_id"], home.ping().unwrap()["store_id"]);
    // And back onto the old binary: verified against its own build.
    let (code, out, err) = home.json(&["restart", "--no-announce", "--exe", &old]);
    assert_eq!(code, 0, "{out} {err}");
    assert_eq!(out["after"]["build"], before["build"]);
}
