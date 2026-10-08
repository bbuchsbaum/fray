//! The advisory daemon registry against real daemon processes.
use fray::registry::{self, Liveness, Record};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const FRAY: &str = env!("CARGO_BIN_EXE_fray");

/// A private state directory plus homes; every daemon here is our own child.
struct Scratch {
    root: PathBuf,
    state: PathBuf,
    children: Vec<Child>,
}

impl Scratch {
    fn new() -> Self {
        let root = std::env::temp_dir().join(format!(
            "fray-reg-{}",
            &fray::model::random_key().unwrap()[..8]
        ));
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

    fn command(&self, state: &Path, home: &Path, args: &[&str]) -> Command {
        let mut command = Command::new(FRAY);
        command
            .arg("--home")
            .arg(home)
            .args(args)
            .env("FRAY_STATE_DIR", state)
            .env_remove("FRAY_AGENT")
            .env_remove("FRAY_SESSION");
        command
    }

    fn run(&self, home: &Path, args: &[&str]) -> Output {
        self.command(&self.state, home, args).output().unwrap()
    }

    /// `serve` as a direct child, ready once its record and socket exist.
    fn serve(&mut self, home: &Path) -> u32 {
        let child = self
            .command(&self.state, home, &["serve"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        self.children.push(child);
        let path = registry::record_path(&self.state, home);
        wait_until("record and ping", || {
            read(&path).is_some_and(|r| r.pid == pid) && self.ping(home)
        });
        pid
    }

    fn ping(&self, home: &Path) -> bool {
        self.run(home, &["ping"]).status.success()
    }

    fn kill(&mut self, pid: u32) {
        let child = self.children.iter_mut().find(|c| c.id() == pid).unwrap();
        // `Child::kill` is SIGKILL on this Unix-only crate.
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
        // Daemons from `fray start` are not our children; stop them politely.
        if let Ok(entries) = registry::list_in(&self.state) {
            for entry in entries {
                if entry.liveness == Liveness::Running {
                    let _ = self.run(&entry.record.home, &["stop"]);
                }
            }
        }
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn read(path: &Path) -> Option<Record> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    while !ready() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

fn mode(path: &Path) -> u32 {
    fs::metadata(path).unwrap().permissions().mode() & 0o777
}

#[test]
fn start_registers_and_stop_unregisters() {
    let s = Scratch::new();
    let home = s.home("a");
    assert!(s.run(&home, &["start"]).status.success());
    let path = registry::record_path(&s.state, &home);
    let record = read(&path).expect("start returns after the record is written");
    assert_eq!(record.home, home);
    assert_eq!(record.socket, home.join("bus.sock"));
    assert_eq!(record.version, env!("CARGO_PKG_VERSION"));
    assert_eq!(record.build, fray::model::BUILD);
    assert_eq!(record.protocol_version, fray::model::PROTOCOL_VERSION);
    assert_eq!(record.exe, fs::canonicalize(FRAY).unwrap());
    assert_eq!(record.durability, "full");
    assert!(record.pid > 0 && record.started_ms > 0);
    assert_eq!(mode(&path), 0o600);
    assert_eq!(mode(path.parent().unwrap()), 0o700);

    let entries = registry::list_in(&s.state).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].liveness, Liveness::Running);
    assert_eq!(entries[0].ping.as_ref().unwrap()["build"], record.build);

    assert!(s.run(&home, &["stop"]).status.success());
    wait_until("record removal", || !path.exists());
    assert!(registry::list_in(&s.state).unwrap().is_empty());
}

#[test]
fn sigkill_leaves_a_record_that_readers_classify_dead() {
    let mut s = Scratch::new();
    let home = s.home("a");
    let pid = s.serve(&home);
    s.kill(pid);
    let path = registry::record_path(&s.state, &home);
    assert_eq!(read(&path).unwrap().pid, pid, "a crash leaves the record");
    let entries = registry::list_in(&s.state).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].record.pid, pid);
    assert_eq!(entries[0].liveness, Liveness::Dead);
    assert!(entries[0].ping.is_none());
    assert!(registry::prune(&entries[0]).unwrap());
    assert!(!path.exists());
}

#[test]
fn a_daemon_losing_the_lock_race_writes_nothing() {
    let mut s = Scratch::new();
    let home = s.home("a");
    let winner = s.serve(&home);
    let path = registry::record_path(&s.state, &home);
    let before = fs::read(&path).unwrap();
    let other_state = s.root.join("other-state");
    for state in [&s.state, &other_state] {
        let loser = s.command(state, &home, &["serve"]).output().unwrap();
        assert!(!loser.status.success());
        assert!(String::from_utf8_lossy(&loser.stderr).contains("already_running"));
    }
    assert_eq!(fs::read(&path).unwrap(), before);
    assert!(!other_state.exists(), "the loser created no registry");
    assert_eq!(read(&path).unwrap().pid, winner);
}

#[test]
fn homes_register_separately_and_reregistration_replaces() {
    let mut s = Scratch::new();
    let (a, b) = (s.home("a"), s.home("b"));
    let first = s.serve(&a);
    s.serve(&b);
    let entries = registry::list_in(&s.state).unwrap();
    assert_eq!(entries.len(), 2);
    assert!(entries.iter().all(|e| e.liveness == Liveness::Running));
    assert_eq!(
        entries.iter().map(|e| &e.record.home).collect::<Vec<_>>(),
        [&a, &b]
    );

    // A crashed daemon's stale record is replaced in place by its successor.
    s.kill(first);
    let second = s.serve(&a);
    assert_ne!(first, second);
    let names: Vec<_> = fs::read_dir(s.state.join("daemons"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 2, "one record per home and no temp files");
    let entries = registry::list_in(&s.state).unwrap();
    assert_eq!(entries[0].record.pid, second);
    assert_eq!(entries[0].liveness, Liveness::Running);
    assert!(
        !registry::prune(&entries[0]).unwrap(),
        "running is never pruned"
    );
}
