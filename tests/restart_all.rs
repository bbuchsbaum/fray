//! `fray restart --all-stale` (daemon lifecycle L10) against real daemons on
//! scratch homes, each with a private registry (`FRAY_STATE_DIR`) and a scan
//! limited to the scratch root (`FRAY_SCAN_ROOT`), so no other daemon on the
//! machine is ever listed, let alone restarted.
//!
//! A stale daemon here is a real daemon of another build: a copy of this
//! build's `fray` whose embedded build string is rewritten to another of the
//! same length (and re-signed ad hoc on macOS). It runs the same code, writes
//! its own registry record and reports the other build in `ping` and
//! `--version`, so the restart sees exactly what an out-of-date install looks
//! like, without needing an old binary. Set FRAY_OLD_BIN to an installed
//! pre-registry `fray` to also restart real old daemons found by `--scan`.
use fray::{
    model::{random_key, Request, BUILD},
    registry::{self, Record},
};
use serde_json::{json, Value};
use std::{
    io::Write,
    os::unix::{fs::PermissionsExt, net::UnixStream},
    path::{Path, PathBuf},
    process::{Command, Output},
    thread,
    time::{Duration, Instant},
};

const BIN: &str = env!("CARGO_BIN_EXE_fray");

struct Scratch {
    root: PathBuf,
    state: PathBuf,
    homes: Vec<PathBuf>,
}

impl Scratch {
    fn new() -> Self {
        // Short, so every bus.sock path stays within the Unix limit.
        let root = PathBuf::from("/tmp").join(format!("fray-ra-{}", &random_key().unwrap()[..8]));
        std::fs::create_dir_all(&root).unwrap();
        let root = std::fs::canonicalize(root).unwrap();
        Self {
            state: root.join("state"),
            root,
            homes: vec![],
        }
    }

    fn home(&mut self, name: &str) -> PathBuf {
        let home = self.root.join(name);
        std::fs::create_dir_all(&home).unwrap();
        self.homes.push(home.clone());
        home
    }

    /// A copy of this build's binary that reports another build.
    fn stale_bin(&self) -> String {
        let path = self.root.join("bin/fray.stale");
        if path.exists() {
            return path.to_str().unwrap().to_owned();
        }
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let other: String = format!("{:0<1$}", "stale", BUILD.len())
            .chars()
            .take(BUILD.len())
            .collect();
        assert_ne!(other, BUILD);
        let bytes = std::fs::read(BIN).unwrap();
        let (from, to) = (BUILD.as_bytes(), other.as_bytes());
        let mut out = Vec::with_capacity(bytes.len());
        let (mut i, mut replaced) = (0, 0);
        while i < bytes.len() {
            if bytes[i..].starts_with(from) {
                out.extend_from_slice(to);
                i += from.len();
                replaced += 1;
            } else {
                out.push(bytes[i]);
                i += 1;
            }
        }
        assert!(replaced > 0, "build string {BUILD} not found in {BIN}");
        std::fs::write(&path, out).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        if cfg!(target_os = "macos") {
            let signed = Command::new("codesign")
                .args(["-f", "-s", "-"])
                .arg(&path)
                .output()
                .unwrap();
            assert!(signed.status.success(), "codesign: {}", text(&signed));
        }
        let version = Command::new(&path).arg("--version").output().unwrap();
        assert_eq!(
            String::from_utf8_lossy(&version.stdout).trim(),
            format!("fray {} ({other})", env!("CARGO_PKG_VERSION"))
        );
        path.to_str().unwrap().to_owned()
    }

    fn command(&self, bin: &str, state: &Path) -> Command {
        let mut command = Command::new(bin);
        command
            .env("FRAY_STATE_DIR", state)
            .env(fray::daemons::SCAN_ROOT, &self.root)
            .env("USER", "tester")
            .env_remove("FRAY_AGENT")
            .env_remove("FRAY_SESSION")
            .env_remove("FRAY_HOME")
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CODEX_THREAD_ID");
        command
    }

    /// `fray --home HOME start` with `bin`, registered in `state`.
    fn start_in(&self, bin: &str, state: &Path, home: &Path) {
        let out = self
            .command(bin, state)
            .arg("--home")
            .arg(home)
            .arg("start")
            .output()
            .unwrap();
        assert!(out.status.success(), "start: {}", text(&out));
    }

    fn start(&self, bin: &str, home: &Path) {
        self.start_in(bin, &self.state, home);
    }

    fn on(&self, bin: &str, home: &Path, args: &[&str]) -> Output {
        self.command(bin, &self.state)
            .arg("--home")
            .arg(home)
            .args(args)
            .output()
            .unwrap()
    }

    /// `fray --json restart --all-stale ARGS`: exit status and report.
    fn all_stale(&self, args: &[&str]) -> (i32, Value) {
        let out = self
            .command(BIN, &self.state)
            .args(["--json", "restart", "--all-stale"])
            .args(args)
            .output()
            .unwrap();
        let report = serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{args:?} gave no JSON ({e}): {}", text(&out)));
        (out.status.code().unwrap(), report)
    }

    fn all_stale_text(&self, args: &[&str]) -> (i32, String) {
        let out = self
            .command(BIN, &self.state)
            .args(["restart", "--all-stale"])
            .args(args)
            .output()
            .unwrap();
        (
            out.status.code().unwrap(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
        )
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Daemons started through `fray start` or a restart are detached.
        for home in &self.homes {
            for _ in 0..3 {
                let Some(ping) = ping(home) else { break };
                let _ = self.on(BIN, home, &["stop"]);
                match ping["pid"].as_u64() {
                    Some(pid) => {
                        if !fray::client::await_exit(pid as u32, Duration::from_secs(5)) {
                            let _ = Command::new("kill").args(["-9", &pid.to_string()]).status();
                        }
                    }
                    None => thread::sleep(Duration::from_millis(300)),
                }
            }
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn ping(home: &Path) -> Option<Value> {
    fray::client::probe(home, &Request::new("ping", "", json!({})), 2).ok()
}

fn text(out: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}

fn result<'a>(report: &'a Value, home: &Path) -> &'a Value {
    report["results"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["home"] == json!(home))
        .unwrap_or_else(|| panic!("{} not in {report}", home.display()))
}

fn listed(report: &Value, home: &Path) -> bool {
    report["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["home"] == json!(home))
}

/// The maintenance notices posted on a home.
fn notices(home: &Path) -> usize {
    let db = rusqlite::Connection::open(home.join("state.db")).unwrap();
    db.busy_timeout(Duration::from_secs(5)).unwrap();
    db.query_row("SELECT count(*) FROM cards WHERE author='fray'", [], |r| {
        r.get::<_, i64>(0)
    })
    .unwrap() as usize
}

/// A long-lived `wait` held open by `actor`: a holder a restart would cut off.
fn hold_wait(home: &Path, actor: &str) -> UnixStream {
    let mut stream = UnixStream::connect(home.join("bus.sock")).unwrap();
    let frame = json!({"op":"wait","actor":actor,"args":{"timeout":60}}).to_string() + "\n";
    stream.write_all(frame.as_bytes()).unwrap();
    stream
}

fn until(what: &str, mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(10);
    while !ready() {
        assert!(Instant::now() < end, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(50));
    }
}

/// A registry record for a home whose daemon may be alive but does not
/// answer: its socket is gone while the recorded pid (this test) lives.
fn unreachable_record(state: &Path, home: &Path) {
    let record = Record {
        home: home.into(),
        socket: home.join("bus.sock"),
        pid: std::process::id(),
        version: "0.0.1".into(),
        build: "0ld0ld0ld0ld".into(),
        protocol_version: fray::model::PROTOCOL_VERSION,
        exe: "/old/fray".into(),
        started_ms: fray::model::now_ms() - 90_000,
        durability: "full".into(),
    };
    let path = registry::record_path(state, home);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
}

#[test]
fn idle_stale_daemons_restart_busy_ones_are_skipped_and_current_ones_untouched() {
    let mut s = Scratch::new();
    let stale = s.stale_bin();
    let idle: Vec<PathBuf> = ["a", "b", "c"].iter().map(|n| s.home(n)).collect();
    let busy = s.home("d");
    let current = s.home("e");
    let gone = s.home("f");
    for home in idle.iter().chain([&busy]) {
        s.start(&stale, home);
    }
    s.start(BIN, &current);
    unreachable_record(&s.state, &gone);
    let joined = s.on(&stale, &busy, &["--as", "bob", "join"]);
    assert!(joined.status.success(), "{}", text(&joined));
    let held = hold_wait(&busy, "bob");
    until("bob's wait to show", || {
        s.on(BIN, &busy, &["restart", "--dry-run"]).status.code() == Some(3)
    });
    let pid = |home: &Path| ping(home).unwrap()["pid"].as_u64().unwrap();
    let before: Vec<u64> = idle.iter().map(|h| pid(h)).collect();
    let (busy_pid, current_pid) = (pid(&busy), pid(&current));

    // The dry run says what would happen and changes nothing.
    let (code, report) = s.all_stale(&["--dry-run"]);
    assert_eq!(code, 4, "{report}");
    assert_eq!(report["dry_run"], true);
    assert_eq!(report["client_build"], BUILD);
    assert_eq!(report["target_build"], BUILD);
    for home in &idle {
        let r = result(&report, home);
        assert_eq!(r["outcome"], "would_restart", "{r}");
        assert_eq!(r["preflight"]["verdict"], "idle", "{r}");
    }
    let r = result(&report, &busy);
    assert_eq!(r["outcome"], "skipped_busy", "{r}");
    assert_eq!(r["holders"][0]["actor"], "bob", "{r}");
    assert_eq!(result(&report, &gone)["outcome"], "skipped_state");
    assert_eq!(result(&report, &gone)["state"], "unreachable");
    assert!(!listed(&report, &current), "{report}");
    assert_eq!(report["counts"]["would_restart"], 3);
    assert_eq!(report["counts"]["skipped_busy"], 1);
    assert_eq!(report["counts"]["skipped_state"], 1);
    assert_eq!(report["counts"]["restarted"], 0);
    let after: Vec<u64> = idle.iter().map(|h| pid(h)).collect();
    assert_eq!(after, before, "a dry run restarts nothing");
    assert!(idle.iter().all(|h| notices(h) == 0));

    // The run: idle ones restarted onto this build, one at a time.
    let (code, report) = s.all_stale(&[]);
    assert_eq!(code, 4, "skips without failures: {report}");
    for (home, old) in idle.iter().zip(&before) {
        let r = result(&report, home);
        assert_eq!(r["outcome"], "restarted", "{r}");
        assert_eq!(r["restart"]["after"]["build"], BUILD, "{r}");
        assert_eq!(r["restart"]["before"]["pid"], *old, "{r}");
        let now = ping(home).unwrap();
        assert_eq!(now["build"], BUILD);
        assert_ne!(now["pid"], *old);
        assert_eq!(notices(home), 2, "restart and restarted notices");
    }
    let r = result(&report, &busy);
    assert_eq!(r["outcome"], "skipped_busy", "{r}");
    assert_eq!(r["holders"][0]["actor"], "bob", "{r}");
    assert!(r["hint"].as_str().unwrap().contains("--force"), "{r}");
    assert_eq!(r["refusal"]["action"], "refused", "{r}");
    assert_eq!(pid(&busy), busy_pid, "a busy daemon is never forced");
    assert_eq!(notices(&busy), 0);
    assert_eq!(pid(&current), current_pid, "a current daemon is untouched");
    assert!(!listed(&report, &current));
    assert_eq!(result(&report, &gone)["outcome"], "skipped_state");
    assert_eq!(report["counts"]["restarted"], 3);
    assert_eq!(report["counts"]["failed"], 0);
    assert_eq!(report["exit_code"], 4);

    // The table names the busy holder and the way past it.
    let (code, shown) = s.all_stale_text(&["--dry-run"]);
    assert_eq!(code, 4, "{shown}");
    assert!(shown.contains("skipped-busy"), "{shown}");
    assert!(shown.contains(busy.to_str().unwrap()), "{shown}");
    assert!(shown.contains("bob"), "{shown}");
    assert!(shown.contains("--force"), "{shown}");
    assert!(shown.contains("skipped-state"), "{shown}");
    assert!(shown.contains("unreachable"), "{shown}");
    assert!(!shown.contains(idle[0].to_str().unwrap()), "{shown}");

    // Once the holder leaves, the busy one goes too; the unreachable record
    // is still only listed.
    drop(held);
    until("bob's wait to end", || {
        s.on(BIN, &busy, &["restart", "--dry-run"]).status.code() != Some(3)
    });
    let (code, report) = s.all_stale(&["--allow-armed"]);
    assert_eq!(code, 4, "{report}");
    assert_eq!(result(&report, &busy)["outcome"], "restarted", "{report}");
    assert!(
        ping(&gone).is_none(),
        "an unreachable home is never started"
    );
    std::fs::remove_file(registry::record_path(&s.state, &gone)).unwrap();
    let (code, report) = s.all_stale(&[]);
    assert_eq!(code, 0, "{report}");
    assert!(report["results"].as_array().unwrap().is_empty(), "{report}");
    let (code, shown) = s.all_stale_text(&[]);
    assert_eq!(code, 0);
    assert!(shown.contains("No stale daemons."), "{shown}");
}

#[test]
fn armed_daemons_are_skipped_unless_allowed_and_never_forced_implicitly() {
    let mut s = Scratch::new();
    let stale = s.stale_bin();
    let home = s.home("a");
    s.start(&stale, &home);
    assert!(s
        .on(&stale, &home, &["--as", "carol", "join"])
        .status
        .success());
    // A finite wait that has returned leaves its row armed.
    let out = s.on(&stale, &home, &["--as", "carol", "wait", "--timeout", "1"]);
    assert_eq!(out.status.code(), Some(3), "{}", text(&out));
    let pid = ping(&home).unwrap()["pid"].clone();
    let (code, report) = s.all_stale(&[]);
    assert_eq!(code, 4, "{report}");
    let r = result(&report, &home);
    assert_eq!(r["outcome"], "skipped_armed", "{r}");
    assert_eq!(r["holders"][0]["actor"], "carol", "{r}");
    assert!(r["hint"].as_str().unwrap().contains("--allow-armed"), "{r}");
    assert_eq!(ping(&home).unwrap()["pid"], pid);
    let (code, shown) = s.all_stale_text(&[]);
    assert_eq!(code, 4);
    assert!(shown.contains("skipped-armed"), "{shown}");
    assert!(shown.contains("carol"), "{shown}");
    let (code, report) = s.all_stale(&["--allow-armed"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(result(&report, &home)["outcome"], "restarted");
    assert_eq!(ping(&home).unwrap()["build"], BUILD);
}

#[test]
fn a_failed_restart_is_named_with_its_step_and_exits_1() {
    let mut s = Scratch::new();
    let stale = s.stale_bin();
    let home = s.home("a");
    s.start(&stale, &home);
    // A binary that claims one build and runs another: stale is measured
    // against the binary being started, and the restart fails verification.
    let wrapper = s.root.join("bin/fray-other");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'fray 0.0.0 (not-this-build)'; exit 0; fi\nexec '{BIN}' \"$@\"\n"
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
    let (code, report) = s.all_stale(&["--exe", wrapper.to_str().unwrap()]);
    assert_eq!(code, 1, "{report}");
    assert_eq!(report["target_build"], "not-this-build");
    let r = result(&report, &home);
    assert_eq!(r["outcome"], "failed", "{r}");
    assert_eq!(r["error"]["code"], "restart_unverified", "{r}");
    assert_eq!(report["counts"]["failed"], 1);
    let (code, shown) = s.all_stale_text(&["--dry-run", "--exe", wrapper.to_str().unwrap()]);
    assert_eq!(
        code, 0,
        "the dry run cannot see a verification failure: {shown}"
    );
    assert!(shown.contains("would-restart"), "{shown}");
}

#[test]
fn scan_adds_unregistered_daemons_under_the_scan_root() {
    let mut s = Scratch::new();
    let stale = s.stale_bin();
    let home = s.home("u");
    // Registered elsewhere: invisible to our registry, visible to `ps`.
    let elsewhere = s.root.join("elsewhere");
    s.start_in(&stale, &elsewhere, &home);
    let pid = ping(&home).unwrap()["pid"].clone();
    let (code, report) = s.all_stale(&[]);
    assert_eq!(code, 0, "{report}");
    assert!(!listed(&report, &home), "{report}");
    // A stale daemon outside the scan root is never considered.
    let mut outside = Scratch::new();
    let other = outside.home("x");
    outside.start_in(&stale, &outside.root.join("elsewhere"), &other);
    let other_pid = ping(&other).unwrap()["pid"].clone();
    let (code, report) = s.all_stale(&["--scan", "--dry-run"]);
    assert_eq!(code, 0, "{report}");
    assert!(!listed(&report, &other), "{report}");
    let r = result(&report, &home);
    assert_eq!(r["registered"], false, "{r}");
    assert_eq!(r["outcome"], "would_restart", "{r}");
    assert_eq!(ping(&home).unwrap()["pid"], pid);
    let (code, report) = s.all_stale(&["--scan"]);
    assert_eq!(code, 0, "{report}");
    assert_eq!(result(&report, &home)["outcome"], "restarted", "{report}");
    assert_eq!(ping(&home).unwrap()["build"], BUILD);
    assert!(!listed(&report, &other), "{report}");
    assert_eq!(ping(&other).unwrap()["pid"], other_pid);
    // --scan means nothing without --all-stale.
    let out = s.on(BIN, &home, &["restart", "--scan", "--dry-run"]);
    assert_eq!(out.status.code(), Some(2), "{}", text(&out));
}

/// Real pre-registry daemons, when FRAY_OLD_BIN names one (e.g. the installed
/// 5e78536 `fray`): found only by `--scan`, refused without `--no-announce`
/// because they cannot post the notice, then restarted onto this build.
#[test]
fn real_old_daemons_when_one_is_given() {
    let Ok(old) = std::env::var("FRAY_OLD_BIN") else {
        eprintln!("FRAY_OLD_BIN unset; skipping the real old-daemon restart");
        return;
    };
    let mut s = Scratch::new();
    let homes: Vec<PathBuf> = ["o1", "o2"].iter().map(|n| s.home(n)).collect();
    for home in &homes {
        s.start(&old, home);
    }
    let (code, report) = s.all_stale(&[]);
    assert_eq!(code, 0, "pre-registry daemons are not registered: {report}");
    assert!(report["results"].as_array().unwrap().is_empty(), "{report}");
    let (code, report) = s.all_stale(&["--scan", "--dry-run"]);
    let announces = ping(&homes[0]).unwrap()["capabilities"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c == "announce");
    for home in &homes {
        let r = result(&report, home);
        assert_eq!(r["registered"], false, "{r}");
        if announces {
            assert_eq!(r["outcome"], "would_restart", "{r}");
        } else {
            assert_eq!(r["outcome"], "would_fail", "{r}");
            assert_eq!(r["error"]["code"], "announce_unsupported", "{r}");
            assert_eq!(code, 1, "{report}");
        }
    }
    let (code, report) = s.all_stale(&["--scan", "--no-announce"]);
    assert_eq!(code, 0, "{report}");
    for home in &homes {
        let r = result(&report, home);
        assert_eq!(r["outcome"], "restarted", "{r}");
        assert_eq!(ping(home).unwrap()["build"], BUILD);
    }
}
