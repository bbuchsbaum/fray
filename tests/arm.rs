//! No silent stalls R5: `fray arm` prints a command whose listener counts as
//! armed, and a Stop hook tells an unarmed agent with asks addressed to it
//! first.
use fray::model::random_key;
use serde_json::Value;
use std::{
    io::Write,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

const FRAY: &str = env!("CARGO_BIN_EXE_fray");

struct Board(PathBuf);
impl Board {
    fn new() -> Self {
        let root = PathBuf::from("/tmp").join(format!("fray-arm-{}", &random_key().unwrap()[..8]));
        std::fs::create_dir_all(&root).unwrap();
        let b = Self(root);
        assert!(b.fray("", &["start"]).status.success());
        for who in ["alice", "helper"] {
            assert!(b.fray(who, &["join"]).status.success());
        }
        b
    }
    fn home(&self) -> PathBuf {
        self.0.join(".fray")
    }
    fn cmd(&self, program: &str, actor: &str) -> Command {
        let mut c = Command::new(program);
        let bin = Path::new(FRAY).parent().unwrap();
        c.current_dir(&self.0)
            .env_remove("FRAY_AGENT")
            .env_remove("MOTE_STORE")
            .env_remove("MOTE_ACTOR")
            .env("FRAY_HOME", self.home())
            .env("FRAY_SESSION", format!("test:{actor}"))
            .env(
                "PATH",
                format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()),
            );
        c
    }
    fn fray(&self, actor: &str, args: &[&str]) -> Output {
        let mut c = self.cmd(FRAY, actor);
        if !actor.is_empty() {
            c.args(["--as", actor]);
        }
        c.args(args).output().unwrap()
    }
    fn json(&self, actor: &str, args: &[&str]) -> Value {
        let mut all = vec!["--json"];
        all.extend_from_slice(args);
        let out = self.fray(actor, &all);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).unwrap()
    }
    fn reach(&self, who: &str) -> String {
        let roster = self.json("alice", &["agents"]);
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
    fn stop_hook(&self) -> Value {
        let mut c = self.cmd(FRAY, "helper");
        let mut child = c
            .env("FRAY_AGENT", "helper")
            .args(["hook", "--host", "claude"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(br#"{"hook_event_name":"Stop","session_id":"s"}"#)
            .unwrap();
        let out = child.wait_with_output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap_or(Value::Null)
    }
}
impl Drop for Board {
    fn drop(&mut self) {
        self.fray("", &["stop"]);
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn the_printed_command_arms_and_an_unarmed_stop_is_told_first() {
    let b = Board::new();
    b.json("alice", &["send", "helper", "Review the parser?", "--ask"]);
    // Unarmed, with an ask addressed to it: the Stop hook blocks, and the
    // reason leads with the lapse and the arm command.
    let stop = b.stop_hook();
    assert_eq!(stop["decision"], "block", "{stop}");
    let reason = stop["reason"].as_str().unwrap();
    assert!(
        reason.starts_with("FIRST: 1 open ask(s) are addressed to you and nothing is armed"),
        "{reason}"
    );
    assert!(reason.contains("fray --as helper arm"), "{reason}");
    // The command `fray arm` prints, run as the host monitor would, arms.
    let arm = b.json("helper", &["arm", "--minutes", "5"]);
    let command = arm["arm"]["command"].as_str().unwrap().to_owned();
    assert!(command.contains("--activation-expires-ms"), "{command}");
    let mut monitor = b
        .cmd("sh", "helper")
        .args(["-c", &format!("exec {command}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let end = Instant::now() + Duration::from_secs(10);
    let mut state = b.reach("helper");
    while state != "wakeable" && Instant::now() < end {
        thread::sleep(Duration::from_millis(100));
        state = b.reach("helper");
    }
    assert_eq!(state, "wakeable");
    let brief = b.json("helper", &["brief"]);
    assert!(brief["idle_readiness"]["lapse"].is_null(), "{brief}");
    let _ = monitor.kill();
    let _ = monitor.wait();
    // The background-completion hint prints a one-shot listener (#78).
    let bg = b.json("helper", &["arm", "--host", "background-completion"]);
    let bg = bg["arm"]["command"].as_str().unwrap();
    assert!(
        bg.contains("--once --activation background-completion") && !bg.contains("--reconnect"),
        "{bg}"
    );
}
