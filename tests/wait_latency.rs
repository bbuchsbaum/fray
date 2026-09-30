//! Epic child 3: a finite wait replies as soon as attention arrives, and a
//! request that follows the reply on the same connection is served intact.
use fray::model::random_key;
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
        let root = PathBuf::from("/tmp").join(format!("fray-wl-{}", &random_key().unwrap()[..8]));
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
            if UnixStream::connect(daemon.socket()).is_ok() {
                return daemon;
            }
            thread::sleep(Duration::from_millis(25));
        }
        panic!("daemon did not start");
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

    fn call(&mut self, op: &str, actor: &str, args: Value) -> Value {
        self.send(op, actor, args);
        let reply = self.get();
        assert_eq!(reply["ok"], true, "{op}: {reply}");
        reply["data"].clone()
    }
}

fn board() -> (Daemon, Conn) {
    let d = Daemon::start();
    let mut c = d.connect();
    c.call("join", "pub", json!({}));
    c.call("join", "sub", json!({}));
    (d, c)
}

/// Arms a finite wait on `waiter`, publishes, and returns the reply delay.
fn waited(waiter: &mut Conn, publisher: &mut Conn, n: usize) -> Duration {
    let cursor = publisher.call("ping", "pub", json!({}))["cursor"].clone();
    waiter.send(
        "wait",
        "sub",
        json!({"after": cursor, "timeout": 10, "selection": "all"}),
    );
    thread::sleep(Duration::from_millis(150));
    publisher.call(
        "send",
        "pub",
        json!({"to": "sub", "body": format!("trial {n}"), "ask": true, "priority": 2}),
    );
    let sent = Instant::now();
    let reply = waiter.get();
    let delay = sent.elapsed();
    assert_eq!(reply["ok"], true, "{reply}");
    assert!(
        !reply["data"]["items"].as_array().unwrap().is_empty(),
        "{reply}"
    );
    delay
}

#[test]
fn a_finite_wait_replies_without_the_hang_up_poll_delay() {
    let (d, mut publisher) = board();
    let mut waiter = d.connect();
    let mut delays: Vec<Duration> = (0..9)
        .map(|n| waited(&mut waiter, &mut publisher, n))
        .collect();
    delays.sort();
    // Before the fix the reply waited for the observer's 100 ms read timeout
    // (median about 95 ms on the reference machine).
    assert!(
        delays[delays.len() / 2] < Duration::from_millis(50),
        "median {:?} of {delays:?}",
        delays[delays.len() / 2]
    );
}

#[test]
fn a_request_right_after_a_wait_reply_is_served_intact() {
    let (d, mut publisher) = board();
    let mut waiter = d.connect();
    for n in 0..20 {
        waited(&mut waiter, &mut publisher, n);
        // Sent at once, while the observer may still be inside its bounded
        // read: its first byte must reach the next frame, not be lost.
        let pong = waiter.call("ping", "sub", json!({}));
        assert!(pong["cursor"].is_i64(), "{pong}");
        let inbox = waiter.call("inbox", "sub", json!({"selection": "all"}));
        assert!(inbox["items"].is_array(), "{inbox}");
    }
}

#[test]
fn input_during_a_wait_is_still_refused() {
    let (d, mut publisher) = board();
    let mut waiter = d.connect();
    let cursor = publisher.call("ping", "pub", json!({}))["cursor"].clone();
    waiter.send("wait", "sub", json!({"after": cursor, "timeout": 10}));
    thread::sleep(Duration::from_millis(150));
    waiter.send("ping", "sub", json!({}));
    let reply = waiter.get();
    assert_eq!(reply["ok"], false, "{reply}");
    assert_eq!(reply["error"]["code"], "protocol", "{reply}");
}
