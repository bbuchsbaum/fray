//! A real SIGKILL/restart boundary for committed delivery and request state.
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixStream,
    path::PathBuf,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

struct Daemon {
    home: PathBuf,
    child: Child,
}

impl Daemon {
    fn start() -> Self {
        let home = std::env::temp_dir().join(format!(
            "fray-crash-recovery-{}",
            &fray::model::random_key().unwrap()[..8]
        ));
        fs::create_dir(&home).unwrap();
        Self::spawn(home)
    }

    fn spawn(home: PathBuf) -> Self {
        let child = Command::new(env!("CARGO_BIN_EXE_fray"))
            .args(["--home", home.to_str().unwrap(), "serve"])
            .env_remove("FRAY_AGENT")
            .env_remove("FRAY_SESSION")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut daemon = Self { home, child };
        daemon.wait_ready();
        daemon
    }

    fn socket(&self) -> PathBuf {
        self.home.join("bus.sock")
    }

    fn wait_ready(&mut self) {
        let deadline = Instant::now() + Duration::from_secs(8);
        while Instant::now() < deadline {
            if UnixStream::connect(self.socket()).is_ok() {
                return;
            }
            if let Some(status) = self.child.try_wait().unwrap() {
                panic!("owned crash-recovery daemon exited before ready: {status}");
            }
            thread::sleep(Duration::from_millis(20));
        }
        panic!("owned crash-recovery daemon did not become ready");
    }

    fn restart_after_sigkill(mut self) -> Self {
        // `Child::kill` is SIGKILL on this Unix-only crate. This is our own
        // spawned child, never a discovered daemon.
        self.child.kill().unwrap();
        self.child.wait().unwrap();
        let home = self.home.clone();
        std::mem::forget(self);
        Self::spawn(home)
    }

    fn connect(&self) -> Connection {
        Connection::open_with_flags(self.home.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.home);
    }
}

struct Peer {
    stream: UnixStream,
    reader: BufReader<UnixStream>,
}

impl Peer {
    fn connect(daemon: &Daemon) -> Self {
        let stream = UnixStream::connect(daemon.socket()).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        Self {
            reader: BufReader::new(stream.try_clone().unwrap()),
            stream,
        }
    }

    fn call(&mut self, op: &str, actor: &str, args: Value, key: Option<&str>) -> Value {
        self.send(op, actor, args, key);
        self.get(op)
    }

    fn send(&mut self, op: &str, actor: &str, args: Value, key: Option<&str>) {
        let mut request = json!({"op":op,"actor":actor,"args":args});
        if let Some(key) = key {
            request["key"] = json!(key);
        }
        self.stream
            .write_all(format!("{request}\n").as_bytes())
            .unwrap();
    }

    fn get(&mut self, op: &str) -> Value {
        let mut line = String::new();
        self.reader.read_line(&mut line).unwrap();
        let reply: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(reply["ok"], true, "{op}: {reply}");
        reply["data"].clone()
    }
}

fn assert_once(connection: &Connection, title: &str) {
    let (cards, events, incomplete): (i64, i64, i64) = connection
        .query_row(
            "SELECT
                (SELECT count(*) FROM cards WHERE title=?1),
                (SELECT count(*) FROM events e JOIN cards c ON c.id=e.card_id WHERE c.title=?1),
                (SELECT count(*) FROM deliveries d LEFT JOIN cards c ON c.id=d.card_id WHERE c.id IS NULL OR d.pending_seq<d.ack_seq)",
            [title],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!((cards, events, incomplete), (1, 1, 0), "{title}");
}

#[test]
fn sigkill_restart_preserves_pending_delivery_stale_ack_and_idempotency() {
    let daemon = Daemon::start();
    let mut alice = Peer::connect(&daemon);
    let mut bob = Peer::connect(&daemon);
    for agent in ["alice", "bob"] {
        alice.call("join", agent, json!({}), None);
    }
    let first = alice.call(
        "post",
        "alice",
        json!({"kind":"task","title":"Replay once","summary":"keyed request"}),
        Some("crash-replay"),
    );
    let sent = alice.call(
        "send",
        "alice",
        json!({"to":"bob","ask":true,"body":"Please recover this delivery"}),
        None,
    );
    let id = sent["card"]["id"].as_i64().unwrap();
    let old_receipt =
        bob.call("inbox", "bob", json!({"selection":"all"}), None)["items"][0]["receipt"].clone();
    let store_id = sent["store_id"].clone();
    drop(alice);
    drop(bob);

    let daemon = daemon.restart_after_sigkill();
    let mut alice = Peer::connect(&daemon);
    let mut bob = Peer::connect(&daemon);
    assert_eq!(
        alice.call("ping", "", json!({}), None)["store_id"],
        store_id
    );
    let changed = alice.call(
        "patch",
        "alice",
        json!({"id":id,"expect":1,"priority":1}),
        None,
    );
    bob.call("ack", "bob", json!({"receipts":[old_receipt]}), None);
    let remaining = bob.call("inbox", "bob", json!({"selection":"all"}), None);
    let item = remaining["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["card"]["id"] == id)
        .unwrap();
    assert_eq!(item["ack_seq"], sent["event_seq"]);
    assert_eq!(item["through_seq"], changed["event_seq"]);

    let replay = alice.call(
        "post",
        "alice",
        json!({"kind":"task","title":"Replay once","summary":"keyed request"}),
        Some("crash-replay"),
    );
    assert_eq!(replay, first);
    assert_eq!(
        alice.call("ping", "", json!({}), None)["cursor"],
        changed["event_seq"]
    );

    let connection = daemon.connect();
    let integrity: String = connection
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    let identity: String = connection
        .query_row("SELECT value FROM meta WHERE key='store_id'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(integrity, "ok");
    assert_eq!(identity, store_id.as_str().unwrap());
}

#[test]
fn interrupted_daemon_and_client_requests_reconcile_as_one_complete_mutation() {
    let mut daemon = Daemon::start();
    let mut alice = Peer::connect(&daemon);
    for agent in ["alice", "bob"] {
        alice.call("join", agent, json!({}), None);
    }

    for attempt in 0..4 {
        let key = format!("daemon-race-{attempt}");
        let title = format!("daemon-race-card-{attempt}");
        let body = "x".repeat(7_000);
        // The write puts a real request on the daemon's socket; SIGKILL races
        // publication without relying on a particular SQLite instruction.
        alice.send(
            "send",
            "alice",
            json!({"to":"bob","title":title,"body":body}),
            Some(&key),
        );
        daemon = daemon.restart_after_sigkill();
        alice = Peer::connect(&daemon);
        alice.call(
            "send",
            "alice",
            json!({"to":"bob","title":title,"body":"x".repeat(7_000)}),
            Some(&key),
        );
        assert_once(&daemon.connect(), &title);
    }

    let key = "client-race";
    let title = "client-race-card";
    let mut child = Command::new("python3")
        .args(["-c", r#"import json,socket,sys,time; s=socket.socket(socket.AF_UNIX); s.settimeout(5); s.connect(sys.argv[1]+'/bus.sock'); s.sendall((json.dumps({'op':'send','actor':'alice','key':sys.argv[2],'args':{'to':'bob','title':'client-race-card','body':'y'*7000}})+'\n').encode()); print('sent',flush=True); time.sleep(30)"#, daemon.home.to_str().unwrap(), key])
        .stdout(Stdio::piped()).stderr(Stdio::null()).spawn().unwrap();
    let mut sent = String::new();
    let handshake = BufReader::new(child.stdout.take().unwrap()).read_line(&mut sent);
    // Establish that the complete framed request reached the socket before
    // killing the client, without waiting for a publication response.
    let published = handshake.is_ok() && sent.trim() == "sent";
    child.kill().unwrap();
    child.wait().unwrap();
    assert!(
        published,
        "client failed to send its request before SIGKILL"
    );
    alice.call(
        "send",
        "alice",
        json!({"to":"bob","title":title,"body":"y".repeat(7_000)}),
        Some(key),
    );
    assert_once(&daemon.connect(), title);

    let after: i64 = daemon
        .connect()
        .query_row("SELECT coalesce(max(seq),0) FROM events", [], |row| {
            row.get(0)
        })
        .unwrap();
    let mut watcher = Peer::connect(&daemon);
    watcher.send("watch", "bob", json!({"after":after}), None);
    assert_eq!(watcher.get("watch")["type"], "ready");
    let mut waiter = Peer::connect(&daemon);
    waiter.send("wait", "bob", json!({"timeout":2}), None);
    let next = alice.call(
        "send",
        "alice",
        json!({"to":"bob","body":"post-restart wake"}),
        None,
    );
    assert!(waiter.get("wait")["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["card"]["id"] == next["card"]["id"]));
    assert_eq!(watcher.get("watch")["event"]["seq"], next["event_seq"]);
}
