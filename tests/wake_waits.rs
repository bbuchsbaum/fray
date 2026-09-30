//! No silent stalls R1: an unfiltered `fray wait` makes its agent wakeable
//! only while it runs. Each wait counts on its own, and every way a wait ends,
//! including the client hanging up, stops it counting.
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
        let root = PathBuf::from("/tmp").join(format!("fray-ww-{}", &random_key().unwrap()[..8]));
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

fn reach(c: &mut Conn, who: &str) -> String {
    let roster = c.call("agents", "alice", json!({}));
    roster["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["name"] == who)
        .unwrap()["reachability"]
        .as_str()
        .unwrap()
        .to_owned()
}

/// Polls until `who` reads as `want`, for up to 3 s.
fn becomes(c: &mut Conn, who: &str, want: &str) -> bool {
    let end = Instant::now() + Duration::from_secs(3);
    while Instant::now() < end {
        if reach(c, who) == want {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    false
}

#[test]
fn each_wait_counts_on_its_own_and_a_hang_up_ends_it() {
    let d = Daemon::start();
    let mut c = d.connect();
    c.call("join", "alice", json!({}));
    c.call("join", "helper", json!({}));
    assert_eq!(reach(&mut c, "helper"), "present");
    // An unfiltered wait arms helper.
    let mut armed = d.connect();
    armed.send("wait", "helper", json!({"timeout": 60}));
    assert!(becomes(&mut c, "helper", "wakeable"));
    // A concurrent filtered wait does not disarm it...
    let mut filtered = d.connect();
    filtered.send(
        "wait",
        "helper",
        json!({"timeout": 60, "kinds": ["decision"]}),
    );
    thread::sleep(Duration::from_millis(200));
    assert_eq!(reach(&mut c, "helper"), "wakeable");
    // ...nor does a second unfiltered wait that times out.
    let mut short = d.connect();
    let reply = short.call("wait", "helper", json!({"timeout": 1}));
    assert_eq!(reply["timed_out"], true, "{reply}");
    assert_eq!(reach(&mut c, "helper"), "wakeable");
    // The armed wait's client hangs up: helper is no longer wakeable, at
    // once, though the filtered wait still runs.
    drop(armed);
    assert!(
        becomes(&mut c, "helper", "present"),
        "a hung-up wait still counted: {}",
        reach(&mut c, "helper")
    );
    drop(filtered);
}
