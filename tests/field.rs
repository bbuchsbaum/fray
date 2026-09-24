//! Field report 2026-09-23: misrouted replies, /clear lockout, waits that
//! return at once, and the Codex hook.
use fray::{
    model::{random_key, Request},
    store::{Store, IDENTITY_TTL_MS},
};
use serde_json::{json, Value};
use std::{
    io::Write,
    path::Path,
    process::{Command, Stdio},
};

const NOW: i64 = 1_800_000_000_000;
const LATER: i64 = NOW + IDENTITY_TTL_MS + 1;

fn run(s: &mut Store, actor: &str, op: &str, args: Value, at: i64) -> Result<Value, String> {
    s.execute_at(&Request::new(op, actor, args), at)
        .map_err(|e| e.code)
}

fn in_session(
    s: &mut Store,
    actor: &str,
    session: &str,
    op: &str,
    args: Value,
    at: i64,
) -> Result<Value, String> {
    s.execute_at(
        &Request::new(op, actor, args).with_session(Some(session.to_owned())),
        at,
    )
    .map_err(|e| e.code)
}

/// codex, release and p1 joined at NOW; by LATER only codex and release are
/// still around.
fn board() -> Store {
    let mut s = Store::memory().unwrap();
    for who in ["codex", "release", "p1"] {
        run(&mut s, who, "join", json!({}), NOW).unwrap();
    }
    for who in ["codex", "release"] {
        run(&mut s, who, "heartbeat", json!({}), LATER).unwrap();
    }
    s
}

#[test]
fn a_third_partys_question_reaches_the_author_not_a_stale_assignee() {
    let mut s = board();
    // codex addresses the wrong (absent) Claude and is told so.
    let sent = run(
        &mut s,
        "codex",
        "send",
        json!({"to":"p1","body":"Tie rule?","ask":true}),
        LATER,
    )
    .unwrap();
    let notice = sent["notice"].as_str().unwrap();
    assert!(notice.contains("p1"), "{notice}");
    assert!(notice.contains("present now: release"), "{notice}");
    // release answers there with a question: it goes to codex, who asked.
    let reply = run(
        &mut s,
        "release",
        "annotate",
        json!({"id":sent["card"]["id"],"kind":"question","body":"Ties by id?"}),
        LATER,
    )
    .unwrap();
    assert_eq!(reply["follow_up"]["assignee"], "codex", "{reply}");
    assert!(reply["notice"].is_null());
}

#[test]
fn the_authors_question_goes_to_the_worker_and_absence_is_reported() {
    let mut s = board();
    let card = run(
        &mut s,
        "codex",
        "post",
        json!({"kind":"task","topic":"t","title":"Do it","summary":"x","assignee":"release"}),
        LATER,
    )
    .unwrap();
    let q = run(
        &mut s,
        "codex",
        "annotate",
        json!({"id":card["card"]["id"],"kind":"question","body":"Status?"}),
        LATER,
    )
    .unwrap();
    assert_eq!(q["follow_up"]["assignee"], "release");
    // The worker objects: it reaches the author.
    let o = run(
        &mut s,
        "release",
        "annotate",
        json!({"id":card["card"]["id"],"kind":"objection","body":"Spec is wrong"}),
        LATER,
    )
    .unwrap();
    assert_eq!(o["follow_up"]["assignee"], "codex");
    // Assigned to someone absent: it still goes there, and the sender is told.
    let stale = run(
        &mut s,
        "codex",
        "post",
        json!({"kind":"task","topic":"t","title":"Old","summary":"x","assignee":"p1"}),
        LATER,
    )
    .unwrap();
    let q = run(
        &mut s,
        "codex",
        "annotate",
        json!({"id":stale["card"]["id"],"kind":"question","body":"Still on it?"}),
        LATER,
    )
    .unwrap();
    assert_eq!(q["follow_up"]["assignee"], "p1");
    let notice = q["notice"].as_str().unwrap();
    assert!(
        notice.contains("p1") && notice.contains("--assignee"),
        "{notice}"
    );
}

#[test]
fn a_present_party_is_preferred_over_an_absent_one() {
    let mut s = board();
    // Authored by p1 (now absent), assigned to codex: release's question
    // would go to the author, but p1 is gone, so it goes to codex.
    let card = run(
        &mut s,
        "p1",
        "post",
        json!({"kind":"task","topic":"t","title":"T","summary":"x","assignee":"codex"}),
        NOW,
    )
    .unwrap();
    let q = run(
        &mut s,
        "release",
        "annotate",
        json!({"id":card["card"]["id"],"kind":"question","body":"Which?"}),
        LATER,
    )
    .unwrap();
    assert_eq!(q["follow_up"]["assignee"], "codex");
}

#[test]
fn clear_continues_the_identity_and_says_so() {
    let mut s = Store::memory().unwrap();
    in_session(&mut s, "release", "claude:old", "join", json!({}), NOW).unwrap();
    // A new session without the hook's continuation is still refused.
    assert_eq!(
        in_session(&mut s, "release", "claude:new", "join", json!({}), NOW + 1).unwrap_err(),
        "identity_busy"
    );
    let joined = in_session(
        &mut s,
        "release",
        "claude:new",
        "join",
        json!({"takeover":true,"continued":"clear"}),
        NOW + 2,
    )
    .unwrap();
    let reason = joined["session_replaced"]["reason"].as_str().unwrap();
    assert!(reason.starts_with("continued after /clear"), "{reason}");
    assert_eq!(
        in_session(
            &mut s,
            "release",
            "claude:x",
            "join",
            json!({"takeover":true,"continued":"anything"}),
            NOW + 3
        )
        .unwrap_err(),
        "invalid"
    );
}

struct Daemon {
    home: std::path::PathBuf,
}

impl Daemon {
    fn start() -> Daemon {
        let home = Path::new("/tmp").join(format!("fray-fld-{}", &random_key().unwrap()[..8]));
        let d = Daemon { home };
        d.cli("t", &["start"], &[]);
        d
    }
    fn cli(&self, actor: &str, args: &[&str], env: &[(&str, &str)]) -> (Value, i32) {
        let out = Command::new(env!("CARGO_BIN_EXE_fray"))
            .args([
                "--home",
                self.home.to_str().unwrap(),
                "--as",
                actor,
                "--json",
            ])
            .args(args)
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CODEX_THREAD_ID")
            .env_remove("FRAY_SESSION")
            .env_remove("FRAY_AGENT")
            .envs(env.iter().copied())
            .output()
            .unwrap();
        (
            serde_json::from_slice(&out.stdout).unwrap_or(Value::Null),
            out.status.code().unwrap_or(-1),
        )
    }
    fn hook(&self, env: &[(&str, &str)], input: Value) -> Value {
        let mut child = Command::new(env!("CARGO_BIN_EXE_fray"))
            .args([
                "--home",
                self.home.to_str().unwrap(),
                "hook",
                "--host",
                "codex",
            ])
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CODEX_THREAD_ID")
            .env_remove("FRAY_SESSION")
            .env_remove("FRAY_AGENT")
            .env_remove("FRAY_DRIVE")
            .envs(env.iter().copied())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success(), "hook failed");
        serde_json::from_slice(&out.stdout).unwrap()
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        self.cli("t", &["stop"], &[]);
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

#[test]
fn wait_explains_an_immediate_return_and_new_waits_for_new_activity() {
    let d = Daemon::start();
    d.cli("a", &["join"], &[]);
    d.cli("b", &["join"], &[]);
    d.cli("a", &["send", "b", "please look", "--ask"], &[]);
    let (old, code) = d.cli("b", &["wait", "--timeout", "5"], &[]);
    assert_eq!(code, 0);
    assert!(old["note"].as_str().unwrap().contains("--new"), "{old}");
    // --new ignores what is already pending and times out.
    let (new, code) = d.cli("b", &["wait", "--new", "--timeout", "1"], &[]);
    assert_eq!(code, 3, "{new}");
}

#[test]
fn the_codex_hook_joins_with_a_codex_session_and_ignores_other_events() {
    let d = Daemon::start();
    let env = [("FRAY_AGENT", "codex-pair")];
    let ignored = d.hook(
        &env,
        json!({"hook_event_name":"UserPromptSubmit","session_id":"t1"}),
    );
    assert_eq!(ignored, json!({}));
    let started = d.hook(
        &env,
        json!({"hook_event_name":"SessionStart","session_id":"t1","source":"startup"}),
    );
    assert_eq!(
        started["hookSpecificOutput"]["hookEventName"], "SessionStart",
        "{started}"
    );
    // The binding is codex:t1: a Claude session of the same name is refused.
    let (busy, code) = d.cli("codex-pair", &["join"], &[("CLAUDE_CODE_SESSION_ID", "c9")]);
    assert_ne!(code, 0, "{busy}");
    // A clear in the same Codex window continues the identity.
    let cleared = d.hook(
        &env,
        json!({"hook_event_name":"SessionStart","session_id":"t2","source":"clear"}),
    );
    assert!(cleared["hookSpecificOutput"].is_object(), "{cleared}");
    let (who, code) = d.cli("codex-pair", &["brief"], &[("CODEX_THREAD_ID", "t2")]);
    assert_eq!(code, 0, "{who}");
}
