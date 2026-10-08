//! `fray daemons` (daemon lifecycle L2/L3) against real and fixture daemons.
//! Every daemon here is our own child under a private state directory;
//! assertions name only our scratch homes, never other daemons on the machine.
use fray::{
    model::{random_key, PROTOCOL_VERSION},
    registry::{self, Liveness, Record},
};
use serde_json::{json, Value};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::unix::net::UnixListener,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const FRAY: &str = env!("CARGO_BIN_EXE_fray");

struct Scratch {
    root: PathBuf,
    state: PathBuf,
    children: Vec<Child>,
}

impl Scratch {
    fn new() -> Self {
        // Short, so every bus.sock path stays within the Unix limit.
        let root = PathBuf::from("/tmp").join(format!("fray-dm-{}", &random_key().unwrap()[..8]));
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        let state = root.join("state");
        Self {
            root,
            state,
            children: vec![],
        }
    }

    fn home(&self, name: &str) -> PathBuf {
        let home = self.root.join(name);
        fs::create_dir_all(&home).unwrap();
        home
    }

    fn fray(&self, state: &Path) -> Command {
        let mut command = Command::new(FRAY);
        command
            .env("FRAY_STATE_DIR", state)
            .env_remove("FRAY_HOME")
            .env_remove("FRAY_AGENT")
            .env_remove("FRAY_SESSION");
        command
    }

    fn daemons(&self, args: &[&str]) -> Output {
        let out = self
            .fray(&self.state)
            .arg("daemons")
            .args(args)
            .output()
            .unwrap();
        assert!(out.status.success(), "daemons {args:?}: {out:?}");
        out
    }

    fn report(&self, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        serde_json::from_slice(&self.daemons(&all).stdout).unwrap()
    }

    /// `serve` registered in `state`, as a direct child.
    fn serve(&mut self, state: &Path, home: &Path) -> u32 {
        let mut command = self.fray(state);
        command.arg("--home").arg(home).arg("serve");
        self.spawn(command, home)
    }

    fn spawn(&mut self, mut command: Command, home: &Path) -> u32 {
        let child = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        self.children.push(child);
        wait_until("ping", || {
            self.fray(&self.state)
                .arg("--home")
                .arg(home)
                .arg("ping")
                .output()
                .unwrap()
                .status
                .success()
        });
        pid
    }

    fn kill(&mut self, pid: u32) {
        let child = self.children.iter_mut().find(|c| c.id() == pid).unwrap();
        child.kill().unwrap();
        child.wait().unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for child in &mut self.children {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

/// A registered fixture daemon that answers every request with `ping`: an
/// installed build other than this client's, without new binaries.
fn fixture(state: &Path, home: &Path, build: &str, protocol: u32) {
    let ping = json!({"ok":true,"data":{"version":"0.0.1","build":build,
        "protocol_version":protocol,"capabilities":[]}});
    serve_fixture(state, home, build, protocol, Some(ping));
}

/// Registers `home` under this process's pid and serves its socket, replying
/// `reply` to every request, or accepting and never replying when `None`.
fn serve_fixture(state: &Path, home: &Path, build: &str, protocol: u32, reply: Option<Value>) {
    let record = Record {
        home: home.into(),
        socket: home.join("bus.sock"),
        pid: std::process::id(),
        version: "0.0.1".into(),
        build: build.into(),
        protocol_version: protocol,
        exe: "/old/fray".into(),
        started_ms: fray::model::now_ms() - 90_000,
        durability: "full".into(),
    };
    let path = registry::record_path(state, home);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
    let listener = UnixListener::bind(home.join("bus.sock")).unwrap();
    thread::spawn(move || {
        let mut held = vec![];
        for stream in listener.incoming().flatten() {
            let Some(reply) = reply.clone() else {
                held.push(stream);
                continue;
            };
            thread::spawn(move || {
                let mut writer = stream.try_clone().unwrap();
                for line in BufReader::new(stream).lines() {
                    if line.is_err() || writeln!(writer, "{reply}").is_err() {
                        break;
                    }
                }
            });
        }
    });
}

fn find<'a>(report: &'a Value, home: &Path) -> Option<&'a Value> {
    report["daemons"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["home"] == json!(home))
}

fn restart(home: &Path) -> String {
    format!("fray --home '{}' restart", home.display())
}

#[test]
fn stale_lists_only_daemons_on_another_build() {
    let mut s = Scratch::new();
    let current = s.home("current");
    let old = s.home("old");
    let ancient = s.home("ancient");
    let state = s.state.clone();
    s.serve(&state, &current);
    fixture(&s.state, &old, "0ld0ld0ld0ld", PROTOCOL_VERSION);
    fixture(&s.state, &ancient, "0ld0ld0ld0ld", PROTOCOL_VERSION - 1);

    let quiet = s.daemons(&["--json"]);
    assert!(quiet.stderr.is_empty(), "{quiet:?}");
    let all = s.report(&[]);
    assert_eq!(all["client"]["build"], json!(fray::model::BUILD));
    assert_eq!(all["pruned"], 0);
    assert!(all.get("scan").is_none(), "no scan unless asked");
    let live = find(&all, &current).expect("current daemon listed");
    assert_eq!(live["state"], "running");
    assert_eq!(live["registered"], true);
    assert_eq!(live["stale"], false);
    assert_eq!(live["build"], json!(fray::model::BUILD));
    assert_eq!(live["pid_mismatch"], false);
    assert!(live["occupancy_error"].is_null());
    assert!(live["restart"].is_null());
    assert_eq!(live["occupancy"]["verdict"], "idle");
    assert_eq!(live["occupancy"]["clients"]["connected"], 0);
    let skewed = find(&all, &old).expect("old daemon listed");
    assert_eq!(skewed["state"], "running");
    assert_eq!(skewed["stale"], true);
    assert_eq!(skewed["build"], "0ld0ld0ld0ld");
    assert_eq!(skewed["restart"], json!(restart(&old)));
    // An old daemon without the op is listed, not failed.
    assert!(skewed["occupancy"].is_null());
    assert!(skewed["occupancy_error"].is_null());
    assert!(
        skewed["pid_mismatch"].is_null(),
        "older daemons send no pid"
    );
    let other = find(&all, &ancient).expect("old-protocol daemon listed");
    assert_eq!(other["state"], "incompatible");
    assert_eq!(other["stale"], true);

    let stale = s.report(&["--stale"]);
    let homes: Vec<_> = stale["daemons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["home"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        homes,
        [ancient.to_str().unwrap(), old.to_str().unwrap()],
        "{stale}"
    );

    let text = String::from_utf8(s.daemons(&["--stale"]).stdout).unwrap();
    assert!(
        text.contains(&format!("  restart: {}\n", restart(&old))),
        "{text}"
    );
    assert!(text.contains(" STALE"), "{text}");
    assert!(!text.contains(current.to_str().unwrap()), "{text}");
    let text = String::from_utf8(s.daemons(&[]).stdout).unwrap();
    assert!(text.contains("idle (0 clients)"), "{text}");
    assert!(text.contains("occupancy not offered"), "{text}");
}

#[test]
fn a_dead_record_is_reported_pruned_once_then_disappears() {
    let mut s = Scratch::new();
    let home = s.home("gone");
    let state = s.state.clone();
    let pid = s.serve(&state, &home);
    s.kill(pid);
    let path = registry::record_path(&s.state, &home);
    assert!(path.exists(), "SIGKILL leaves the record behind");
    assert_eq!(
        registry::list_in(&s.state).unwrap()[0].liveness,
        Liveness::Dead
    );

    // Text mode under --stale still names the pruned record, once.
    let text = String::from_utf8(s.daemons(&["--stale"]).stdout).unwrap();
    assert!(
        text.contains(&format!(
            "{}\n  dead  pid {pid}  record pruned\n",
            home.display()
        )),
        "{text}"
    );
    assert!(text.contains("1 dead record(s) pruned"), "{text}");
    assert!(!path.exists());
    let pid = s.serve(&state, &home);
    s.kill(pid);
    let first = s.report(&["--stale"]);
    let dead = find(&first, &home).expect("dead record shown");
    assert_eq!(dead["state"], "dead");
    assert_eq!(dead["pruned"], true);
    assert_eq!(dead["pid"], pid);
    assert!(dead["restart"].is_null());
    assert_eq!(first["pruned"], 1);
    assert!(!path.exists());

    let second = s.report(&[]);
    assert!(find(&second, &home).is_none(), "{second}");
    assert_eq!(second["pruned"], 0);
    let text = String::from_utf8(s.daemons(&[]).stdout).unwrap();
    assert!(text.contains("No registered daemons."), "{text}");
}

#[test]
fn a_possibly_live_daemon_is_unreachable_and_never_pruned() {
    let s = Scratch::new();
    // Accepts connections but never answers: a wedged daemon.
    let wedged = s.home("wedged");
    serve_fixture(&s.state, &wedged, "0ld0ld0ld0ld", PROTOCOL_VERSION, None);
    // Refuses connections while its recorded pid (this process) is alive: a
    // full backlog or a removed socket, not proof of death.
    let refused = s.home("refused");
    let record = Record {
        home: refused.clone(),
        socket: refused.join("bus.sock"),
        pid: std::process::id(),
        version: "0.0.1".into(),
        build: fray::model::BUILD.into(),
        protocol_version: PROTOCOL_VERSION,
        exe: "/old/fray".into(),
        started_ms: 1,
        durability: "full".into(),
    };
    let path = registry::record_path(&s.state, &refused);
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();

    for _ in 0..2 {
        let report = s.report(&[]);
        assert_eq!(report["pruned"], 0);
        let w = find(&report, &wedged).expect("wedged daemon listed");
        assert_eq!(w["state"], "unreachable");
        assert_eq!(w["pruned"], false);
        assert_eq!(w["stale"], true, "the record's build still shows skew");
        assert_eq!(w["restart"], json!(restart(&wedged)));
        let r = find(&report, &refused).expect("refused daemon listed");
        assert_eq!(r["state"], "unreachable");
        assert_eq!(r["pruned"], false);
    }
    assert!(registry::record_path(&s.state, &wedged).exists());
    assert!(path.exists());
}

#[test]
fn a_record_naming_another_pid_is_flagged_and_the_ping_wins() {
    let mut s = Scratch::new();
    let home = s.home("replaced");
    let elsewhere = s.root.join("elsewhere");
    // The home is served by a daemon this registry does not know, while the
    // record still names another (live) process.
    let daemon = s.serve(&elsewhere, &home);
    let record = Record {
        home: home.clone(),
        socket: home.join("bus.sock"),
        pid: std::process::id(),
        version: "0.0.1".into(),
        build: "0ld0ld0ld0ld".into(),
        protocol_version: PROTOCOL_VERSION,
        exe: "/old/fray".into(),
        started_ms: 1,
        durability: "normal".into(),
    };
    let path = registry::record_path(&s.state, &home);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();

    let report = s.report(&[]);
    let d = find(&report, &home).unwrap();
    assert_eq!(d["state"], "running");
    assert_eq!(d["pid"], daemon);
    assert_eq!(d["pid_mismatch"], true);
    assert_eq!(d["record_pid"], std::process::id());
    assert_eq!(
        d["build"],
        json!(fray::model::BUILD),
        "build comes from the ping"
    );
    assert_eq!(d["stale"], false);
    assert!(d["exe"].is_null() && d["started_ms"].is_null() && d["uptime_ms"].is_null());
    assert_eq!(d["durability"], "unknown");
    let text = String::from_utf8(s.daemons(&[]).stdout).unwrap();
    assert!(
        text.contains(&format!(
            "pid {daemon} (stale record names pid {})",
            std::process::id()
        )),
        "{text}"
    );
}

#[test]
fn scan_finds_unregistered_daemons_by_their_command_line() {
    let mut s = Scratch::new();
    let elsewhere = s.root.join("elsewhere");
    // A home with a space: `ps` shows argv joined by spaces.
    let unregistered = s.home("un registered");
    let pid = s.serve(&elsewhere, &unregistered);
    // A daemon whose home came from FRAY_HOME has no --home in its argv, and a
    // shell whose argv mentions it is not a Fray process.
    let hidden = s.home("hidden");
    let mut command = s.fray(&elsewhere);
    command.env("FRAY_HOME", &hidden).arg("serve");
    s.spawn(command, &hidden);
    let mut decoy = Command::new("/bin/sh");
    decoy
        .args(["-c", "read x", "fray", "--home"])
        .arg(&hidden)
        .arg("serve")
        .stdin(Stdio::piped());
    let decoy = decoy.spawn().unwrap();
    let decoy_pid = decoy.id();
    s.children.push(decoy);
    // A wrapper whose argv runs the real fray binary: `ps` shows the wrapper.
    let script = s.root.join("wait.sh");
    fs::write(&script, "read x\n").unwrap();
    let mut wrapper = Command::new("/bin/sh");
    wrapper
        .arg(&script)
        .arg(FRAY)
        .arg("--home")
        .arg(&hidden)
        .arg("serve")
        .stdin(Stdio::piped());
    let wrapper = wrapper.spawn().unwrap();
    let wrapper_pid = wrapper.id();
    s.children.push(wrapper);

    let plain = s.report(&[]);
    assert!(find(&plain, &unregistered).is_none(), "{plain}");

    let scanned = s.report(&["--scan"]);
    let found = find(&scanned, &unregistered).expect("unregistered daemon found by scan");
    assert_eq!(found["registered"], false);
    assert_eq!(found["state"], "running");
    assert_eq!(found["pid"], pid);
    assert_eq!(found["stale"], false);
    assert_eq!(found["build"], json!(fray::model::BUILD));
    assert_eq!(found["occupancy"]["verdict"], "idle");
    assert!(found["uptime_ms"].as_i64().is_some());
    assert!(found["exe"].is_null());
    assert_eq!(found["durability"], "unknown");
    assert!(found["command"]
        .as_str()
        .unwrap()
        .ends_with(&format!("--home {} serve", unregistered.display())));
    assert_eq!(found["pid_mismatch"], false);
    assert!(find(&scanned, &hidden).is_none(), "{scanned}");
    let mentions = |pid: u32| {
        scanned["daemons"]
            .as_array()
            .unwrap()
            .iter()
            .chain(scanned["scan"]["unconfirmed"].as_array().unwrap())
            .any(|d| d["pid"] == pid)
    };
    assert!(!mentions(decoy_pid), "a non-Fray process is never reported");
    assert!(!mentions(wrapper_pid), "a wrapper is not the daemon");
    // Its own registry is not consulted twice: registered daemons stay registered.
    let registered = s.home("registered");
    let state = s.state.clone();
    s.serve(&state, &registered);
    let both = s.report(&["--scan"]);
    assert_eq!(find(&both, &registered).unwrap()["registered"], true);
    assert_eq!(
        both["daemons"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|d| d["home"] == json!(registered))
            .count(),
        1
    );
    let text = String::from_utf8(s.daemons(&["--scan"]).stdout).unwrap();
    assert!(
        text.contains(&format!(
            "{}\n  running (unregistered)  pid {pid}",
            unregistered.display()
        )),
        "{text}"
    );
}
