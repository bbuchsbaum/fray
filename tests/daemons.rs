//! `fray daemons` (daemon lifecycle L2) against real and fixture daemons.
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
    let ping = json!({"ok":true,"data":{"version":"0.0.1","build":build,
        "protocol_version":protocol,"capabilities":[]}});
    thread::spawn(move || {
        for stream in listener.incoming().flatten() {
            let ping = ping.clone();
            thread::spawn(move || {
                let mut writer = stream.try_clone().unwrap();
                for line in BufReader::new(stream).lines() {
                    if line.is_err() || writeln!(writer, "{ping}").is_err() {
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

    let all = s.report(&[]);
    assert_eq!(all["client"]["build"], json!(fray::model::BUILD));
    assert_eq!(all["pruned"], 0);
    let live = find(&all, &current).expect("current daemon listed");
    assert_eq!(live["state"], "running");
    assert_eq!(live["registered"], true);
    assert_eq!(live["stale"], false);
    assert_eq!(live["build"], json!(fray::model::BUILD));
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
    assert!(skewed.get("occupancy_error").is_none());
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

    let first = s.report(&[]);
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
