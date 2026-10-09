//! A graceful restart (`shutdown` with `restart`): every long-lived client is
//! told to reconnect, nothing is acknowledged by that notice, and clients
//! resume from their own cursors on the replacement daemon.
use fray::model::{random_key, Request};
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Read, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Home {
    root: PathBuf,
    daemon: Option<Child>,
}

impl Home {
    fn new() -> Self {
        let root = PathBuf::from("/tmp").join(format!("fray-rs-{}", &random_key().unwrap()[..8]));
        std::fs::create_dir_all(&root).unwrap();
        let mut home = Self { root, daemon: None };
        home.serve();
        home
    }

    /// Starts a daemon on this home and waits until it accepts connections.
    fn serve(&mut self) {
        assert!(self.daemon.is_none());
        self.daemon = Some(
            Command::new(env!("CARGO_BIN_EXE_fray"))
                .args(["--home", self.root.to_str().unwrap(), "serve"])
                .env_remove("FRAY_AGENT")
                .env_remove("FRAY_SESSION")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let end = Instant::now() + Duration::from_secs(5);
        while UnixStream::connect(self.socket()).is_err() {
            assert!(Instant::now() < end, "daemon did not start");
            thread::sleep(Duration::from_millis(25));
        }
    }

    /// Waits for the stopped daemon to exit and reports how long that took.
    fn reap(&mut self) -> Duration {
        let started = Instant::now();
        let mut child = self.daemon.take().unwrap();
        let end = Instant::now() + Duration::from_secs(15);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                assert!(status.success(), "daemon exited {status}");
                return started.elapsed();
            }
            assert!(Instant::now() < end, "daemon did not exit");
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn socket(&self) -> PathBuf {
        self.root.join("bus.sock")
    }

    fn connect(&self) -> Conn {
        let stream = UnixStream::connect(self.socket()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        Conn {
            reader: BufReader::new(stream.try_clone().unwrap()),
            stream,
        }
    }

    fn command(&self, actor: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_fray"));
        command
            .args([
                "--home",
                self.root.to_str().unwrap(),
                "--as",
                actor,
                "--json",
            ])
            .env_remove("FRAY_AGENT")
            .env_remove("FRAY_SESSION")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CODEX_THREAD_ID");
        command
    }

    fn fray(&self, actor: &str, args: &[&str]) -> Value {
        let out = self.command(actor).args(args).output().unwrap();
        assert!(out.status.success(), "{args:?}: {}", text(&out));
        serde_json::from_slice(&out.stdout).unwrap()
    }

    /// `fray stop --restart`: returns once the daemon has released its lock.
    fn restart_stop(&mut self) -> Value {
        let stopped = self.fray("owner-test", &["stop", "--restart", "--reason", "upgrade"]);
        assert_eq!(stopped["exited"], true, "{stopped}");
        self.reap();
        stopped
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.kill();
            let _ = child.wait();
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
        self.send_keyed(op, actor, args, None);
    }

    fn send_keyed(&mut self, op: &str, actor: &str, args: Value, key: Option<&str>) {
        let mut frame = json!({"op": op, "actor": actor, "args": args});
        if let Some(key) = key {
            frame["key"] = json!(key);
        }
        self.stream
            .write_all((frame.to_string() + "\n").as_bytes())
            .unwrap();
    }

    /// The next frame, or None at end of stream.
    fn next(&mut self) -> Option<Value> {
        let mut line = String::new();
        if self.reader.read_line(&mut line).unwrap() == 0 {
            return None;
        }
        Some(serde_json::from_str(&line).unwrap_or_else(|e| panic!("bad frame {line:?}: {e}")))
    }

    fn get(&mut self) -> Value {
        self.next().expect("connection closed")
    }

    fn call(&mut self, op: &str, actor: &str, args: Value) -> Value {
        self.send(op, actor, args);
        let reply = self.get();
        assert_eq!(reply["ok"], true, "{op}: {reply}");
        reply["data"].clone()
    }
}

/// The restarting notice: a refusal older clients already treat as a
/// reconnectable `unavailable`, never an acknowledgment.
fn assert_restarting(frame: &Value, reason: &str) {
    assert_eq!(frame["ok"], false, "{frame}");
    assert_eq!(frame["type"], "restarting", "{frame}");
    assert_eq!(frame["reason"], reason, "{frame}");
    assert_eq!(frame["error"]["code"], "unavailable", "{frame}");
    assert_eq!(frame["error"]["details"]["restarting"], true, "{frame}");
    assert!(frame.get("data").is_none(), "{frame}");
}

fn wakeable(home: &Home, who: &str) -> bool {
    let end = Instant::now() + Duration::from_secs(5);
    while Instant::now() < end {
        let roster = home.fray("alice", &["agents"]);
        if roster["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["name"] == who && a["reachability"] == "wakeable")
        {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn ping_advertises_graceful_restart_and_rejects_bad_shutdown_arguments() {
    let mut home = Home::new();
    let ping = home.fray("alice", &["ping"]);
    assert!(
        ping["capabilities"]
            .as_array()
            .unwrap()
            .contains(&json!("graceful_restart")),
        "{ping}"
    );
    let mut c = home.connect();
    for args in [
        json!({"grace_ms": 10_001, "restart": true}),
        json!({"reason": "no restart"}),
        json!({"restart": true, "reason": ""}),
        json!({"restart": "yes"}),
        json!({"restart": true, "drain": true}),
    ] {
        c.send("shutdown", "", args.clone());
        let reply = c.get();
        assert_eq!(reply["ok"], false, "{args}: {reply}");
    }
    // None of those stopped it.
    assert_eq!(c.call("ping", "", json!({}))["protocol_version"], 2);
    let reply = c.call("shutdown", "", json!({"restart": true, "grace_ms": 0}));
    let pid = home.daemon.as_ref().unwrap().id();
    assert_eq!(
        reply,
        json!({"stopping": true, "restart": true, "reason": "restart", "grace_ms": 0, "pid": pid})
    );
    home.reap();
}

#[test]
fn plain_shutdown_still_closes_streams_without_a_notice() {
    let mut home = Home::new();
    let mut watch = home.connect();
    watch.send("watch", "alice", json!({}));
    assert_eq!(watch.get()["data"]["type"], "ready");
    let reply = home.connect().call("shutdown", "", json!({}));
    assert_eq!(reply, json!({"stopping": true}));
    assert_eq!(watch.next(), None, "plain shutdown sent a frame");
    home.reap();
}

#[test]
fn restart_notifies_every_stream_and_refuses_new_requests_retryably() {
    let mut home = Home::new();
    home.fray("alice", &["join"]);
    home.fray("bob", &["join"]);
    let mut watch = home.connect();
    watch.send("watch", "alice", json!({}));
    assert_eq!(watch.get()["data"]["type"], "ready");
    let mut attention = home.connect();
    attention.send("watch_attention", "bob", json!({"run_id": "r1"}));
    assert_eq!(attention.get()["data"]["type"], "ready");
    let mut wait = home.connect();
    wait.send("wait", "bob", json!({"timeout": null}));
    assert!(wakeable(&home, "bob"));
    // An open connection between requests, as a client mid-session holds it.
    let mut idle = home.connect();
    idle.call("ping", "", json!({}));

    let stopped = home.connect().call(
        "shutdown",
        "",
        json!({"restart": true, "reason": "upgrade"}),
    );
    assert_eq!(stopped["grace_ms"], 2000, "{stopped}");
    for stream in [&mut watch, &mut attention, &mut wait] {
        assert_restarting(&stream.get(), "upgrade");
        assert_eq!(stream.next(), None);
    }
    // A request after the notice is refused before it runs: retryable by key.
    idle.send_keyed(
        "post",
        "alice",
        json!({"title": "during restart", "summary": "s"}),
        Some("k-during"),
    );
    assert_restarting(&idle.get(), "upgrade");
    drop(idle);
    // Every client has gone, so the daemon exits well inside its grace.
    assert!(home.reap() < Duration::from_secs(2));
    assert!(!home.socket().exists());

    home.serve();
    let before = titles(&home);
    assert_eq!(count(&before, "during restart"), 0, "{before:?}");
    // The same key now commits it exactly once.
    let mut retry = home.connect();
    retry.send_keyed(
        "post",
        "alice",
        json!({"title": "during restart", "summary": "s"}),
        Some("k-during"),
    );
    let first = retry.get();
    assert_eq!(first["ok"], true, "{first}");
    retry.send_keyed(
        "post",
        "alice",
        json!({"title": "during restart", "summary": "s"}),
        Some("k-during"),
    );
    let again = retry.get();
    assert_eq!(again["data"]["card"]["id"], first["data"]["card"]["id"]);
    assert_eq!(count(&titles(&home), "during restart"), 1);
}

#[test]
fn an_in_flight_post_is_committed_and_acknowledged_or_refused_and_retryable() {
    let mut home = Home::new();
    home.fray("alice", &["join"]);
    // Posts race the restart. Each is either committed and acknowledged, or
    // refused as restarting and committed once when retried with its key:
    // never left unanswered.
    let mut conns: Vec<Conn> = (0..8).map(|_| home.connect()).collect();
    let mut stop = home.connect();
    for (i, conn) in conns.iter_mut().enumerate() {
        conn.send_keyed(
            "post",
            "alice",
            json!({"title": format!("race {i}"), "summary": "s"}),
            Some(&format!("k-race-{i}")),
        );
    }
    stop.call(
        "shutdown",
        "",
        json!({"restart": true, "reason": "upgrade"}),
    );
    let mut acknowledged = Vec::new();
    for (i, conn) in conns.iter_mut().enumerate() {
        let reply = conn
            .next()
            .unwrap_or_else(|| panic!("post {i} got no answer"));
        if reply["ok"] == true {
            acknowledged.push((i, reply["data"]["card"]["id"].clone()));
        } else {
            assert_restarting(&reply, "upgrade");
        }
    }
    drop(conns);
    home.reap();
    home.serve();
    let before = titles(&home);
    for (i, _) in &acknowledged {
        assert_eq!(count(&before, &format!("race {i}")), 1, "{before:?}");
    }
    assert_eq!(before.len(), acknowledged.len(), "{before:?}");
    // Retrying every post by its key commits each refused one exactly once and
    // returns the acknowledged ones unchanged.
    let mut c = home.connect();
    for i in 0..8 {
        c.send_keyed(
            "post",
            "alice",
            json!({"title": format!("race {i}"), "summary": "s"}),
            Some(&format!("k-race-{i}")),
        );
        let reply = c.get();
        assert_eq!(reply["ok"], true, "{reply}");
        if let Some((_, id)) = acknowledged.iter().find(|(j, _)| *j == i) {
            assert_eq!(&reply["data"]["card"]["id"], id, "{reply}");
        }
    }
    let after = titles(&home);
    for i in 0..8 {
        assert_eq!(count(&after, &format!("race {i}")), 1, "{after:?}");
    }
}

fn titles(home: &Home) -> Vec<String> {
    let page = home
        .connect()
        .call("query", "alice", json!({"all": true, "limit": 100}));
    page["items"]
        .as_array()
        .unwrap_or_else(|| panic!("{page}"))
        .iter()
        .filter_map(|card| card["title"].as_str().map(str::to_owned))
        .collect()
}

fn count(titles: &[String], title: &str) -> usize {
    titles.iter().filter(|t| *t == title).count()
}

#[test]
fn a_wait_across_a_restart_receives_a_later_message_once() {
    let mut home = Home::new();
    home.fray("alice", &["join"]);
    home.fray("bob", &["join"]);
    let waiter = home
        .command("bob")
        .args(["wait", "--new", "--timeout", "60"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    assert!(wakeable(&home, "bob"));
    let stopped = home.restart_stop();
    assert_eq!(stopped["reason"], "upgrade");
    home.serve();
    let sent = home.fray("alice", &["send", "bob", "after the restart"]);
    let out = waiter.wait_with_output().unwrap();
    assert!(out.status.success(), "{}", text(&out));
    let page: Value = serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("one JSON reply expected ({e}): {}", text(&out)));
    assert_eq!(page["total"], 1, "{page}");
    assert_eq!(page["timed_out"], false, "{page}");
    assert_eq!(
        page["items"][0]["card"]["id"], sent["card"]["id"],
        "{page}\n{sent}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    let lines: Vec<_> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    assert!(lines[0].contains("daemon restarting (upgrade)"), "{stderr}");
}

#[test]
fn watch_reconnect_continues_from_its_cursor_without_gaps_or_replays() {
    let mut home = Home::new();
    home.fray("alice", &["join"]);
    let mut watcher = home
        .command("alice")
        .args(["watch", "--reconnect", "--after", "0"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(watcher.stdout.take().unwrap()).lines();
    let mut next_frame = move || -> Value {
        serde_json::from_str(&lines.next().expect("watch ended").unwrap()).unwrap()
    };
    home.fray("alice", &["post", "before one"]);
    home.fray("alice", &["post", "before two"]);
    let mut seqs = Vec::new();
    let mut titles = Vec::new();
    let mut collect = |want: usize, next: &mut dyn FnMut() -> Value| {
        while titles.len() < want {
            let frame = next();
            if frame["type"] == "event" {
                seqs.push(frame["event"]["seq"].as_i64().unwrap());
                if let Some(title) = frame["event"]["payload"]["card"]["title"].as_str() {
                    titles.push(title.to_owned());
                }
            }
        }
    };
    collect(2, &mut next_frame);
    home.restart_stop();
    home.serve();
    home.fray("alice", &["post", "after one"]);
    home.fray("alice", &["post", "after two"]);
    collect(4, &mut next_frame);
    let _ = watcher.kill();
    let _ = watcher.wait();
    let mut stderr = String::new();
    watcher
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert_eq!(
        titles,
        ["before one", "before two", "after one", "after two"],
        "{seqs:?}"
    );
    // Every event, each once, in order: the cursor neither skipped nor replayed.
    assert!(
        seqs.windows(2).all(|w| w[1] == w[0] + 1),
        "gap or replay: {seqs:?}"
    );
    let lines: Vec<_> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    assert!(lines[0].contains("daemon restarting (upgrade)"), "{stderr}");
}

#[test]
fn an_attention_listener_reconnects_quietly_and_delivers_after_a_restart() {
    let mut home = Home::new();
    home.fray("alice", &["join"]);
    home.fray("bob", &["join"]);
    let mut listener = home
        .command("bob")
        .args(["watch", "--attention", "--reconnect", "--include-control"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(listener.stdout.take().unwrap()).lines();
    let mut errors = listener.stderr.take().unwrap();
    let errors = thread::spawn(move || {
        let mut stderr = String::new();
        errors.read_to_string(&mut stderr).unwrap();
        stderr
    });
    let mut next = move || -> Value {
        serde_json::from_str(&lines.next().expect("listener ended").unwrap()).unwrap()
    };
    assert_eq!(next()["type"], "ready");
    home.restart_stop();
    let gone = next();
    assert_eq!(gone["type"], "disconnected", "{gone}");
    assert_eq!(gone["reason"], "restarting", "{gone}");
    home.serve();
    assert_eq!(next()["type"], "ready");
    let sent = home.fray("alice", &["send", "bob", "after the restart"]);
    let packet = loop {
        let frame = next();
        if frame["type"] != "heartbeat" {
            break frame;
        }
    };
    assert_eq!(packet["type"], "attention", "{packet}");
    assert_eq!(
        packet["items"][0]["card"]["id"], sent["card"]["id"],
        "{packet}"
    );
    let _ = listener.kill();
    let _ = listener.wait();
    let stderr = errors.join().unwrap();
    let lines: Vec<_> = stderr.lines().collect();
    assert_eq!(lines.len(), 1, "{stderr}");
    assert!(lines[0].contains("daemon restarting (upgrade)"), "{stderr}");
}

/// A scripted daemon: answers each request with `respond(op, args, n)`,
/// where n counts earlier requests with that op, and records every request.
struct Mock {
    root: PathBuf,
    seen: std::sync::Arc<std::sync::Mutex<Vec<Value>>>,
}

impl Mock {
    fn new(respond: fn(&str, &Value, usize) -> Value) -> Self {
        let root = PathBuf::from("/tmp").join(format!("fray-rm-{}", &random_key().unwrap()[..8]));
        std::fs::create_dir_all(&root).unwrap();
        let listener = std::os::unix::net::UnixListener::bind(root.join("bus.sock")).unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        let log = seen.clone();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { return };
                let log = log.clone();
                thread::spawn(move || {
                    let reader = BufReader::new(stream.try_clone().unwrap());
                    for line in reader.lines() {
                        let Ok(line) = line else { return };
                        let req: Value = serde_json::from_str(&line).unwrap();
                        let op = req["op"].as_str().unwrap().to_owned();
                        let n = {
                            let mut log = log.lock().unwrap();
                            let n = log.iter().filter(|r| r["op"] == op).count();
                            log.push(req.clone());
                            n
                        };
                        let reply = respond(&op, &req["args"], n).to_string() + "\n";
                        if stream.write_all(reply.as_bytes()).is_err() {
                            return;
                        }
                    }
                });
            }
        });
        Self { root, seen }
    }

    fn requests(&self, op: &str) -> Vec<Value> {
        let seen = self.seen.lock().unwrap();
        seen.iter().filter(|r| r["op"] == op).cloned().collect()
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn restarting_frame() -> Value {
    json!({"ok":false,"type":"restarting","reason":"upgrade","error":{"code":"unavailable","message":"daemon restarting (upgrade)","details":{"restarting":true,"reason":"upgrade"}}})
}

fn hello() -> Value {
    json!({"ok":true,"data":{"version":"0.2.1","protocol_version":2,"capabilities":[],"cursor":0}})
}

#[test]
fn a_restart_pause_never_outlasts_the_callers_own_timeout() {
    // A daemon that stays "restarting": every request, ping included, is refused.
    let mock = Mock::new(|_, _, _| restarting_frame());
    for (req, timeout) in [
        (Request::new("agents", "alice", json!({})), 1),
        (Request::new("wait", "alice", json!({"timeout": 1})), 2),
    ] {
        let started = Instant::now();
        let error = fray::client::rpc(&mock.root, &req, timeout).unwrap_err();
        let elapsed = started.elapsed();
        assert_eq!(error.code, "unavailable", "{error:?}");
        assert_eq!(error.details.unwrap()["restarting"], true);
        // The budget, plus at most one bounded probe; never the 30 s window.
        assert!(
            elapsed < Duration::from_secs(timeout + 2),
            "{} with timeout {timeout}s paused {elapsed:?}",
            req.op
        );
    }
}

#[test]
fn a_resent_wait_without_a_timeout_keeps_what_is_left_of_the_default() {
    let mock = Mock::new(|op, _, n| match (op, n) {
        ("wait", 0) => restarting_frame(),
        ("wait", _) => json!({"ok":true,"data":{"items":[],"total":0,"timed_out":true}}),
        _ => hello(),
    });
    let page =
        fray::client::rpc(&mock.root, &Request::new("wait", "alice", json!({})), 305).unwrap();
    assert_eq!(page["timed_out"], true);
    let waits = mock.requests("wait");
    assert_eq!(waits.len(), 2, "{waits:?}");
    assert!(waits[0]["args"].get("timeout").is_none(), "{waits:?}");
    let resent = waits[1]["args"]["timeout"].as_u64().unwrap();
    assert!((295..=300).contains(&resent), "{waits:?}");
}

#[test]
fn stop_restart_fails_when_the_daemon_has_not_exited() {
    // The "daemon" acknowledges the restart, naming a process that stays
    // alive: this test itself.
    let mock = Mock::new(|op, _, _| match op {
        "shutdown" => {
            json!({"ok":true,"data":{"stopping":true,"restart":true,"reason":"upgrade","grace_ms":0,"pid":std::process::id()}})
        }
        _ => hello(),
    });
    let started = Instant::now();
    let out = Command::new(env!("CARGO_BIN_EXE_fray"))
        .args([
            "--home",
            mock.root.to_str().unwrap(),
            "--json",
            "stop",
            "--restart",
        ])
        .env_remove("FRAY_AGENT")
        .env_remove("FRAY_SESSION")
        .output()
        .unwrap();
    assert!(!out.status.success(), "{}", text(&out));
    let reply: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(reply["error"]["code"], "stop_incomplete", "{reply}");
    assert!(
        reply["error"]["message"]
            .as_str()
            .unwrap()
            .contains(&std::process::id().to_string()),
        "{reply}"
    );
    assert!(started.elapsed() < Duration::from_secs(6));
    assert_eq!(mock.requests("shutdown").len(), 1);
}
