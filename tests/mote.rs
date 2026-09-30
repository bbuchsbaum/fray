//! Epic child 5, slice 5a: the Mote adapter's transport, store binding and
//! outcome classification (docs/design/mote-adapter.md sections 1-4).
use fray::{
    model::{random_key, Request},
    mote::{self, classify, classify_result, Outcome, Store},
    store::Store as Board,
};
use serde_json::{json, Value};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
    time::{Duration, Instant},
};

struct Temp(PathBuf);
impl Temp {
    fn new(tag: &str) -> Self {
        let p =
            PathBuf::from("/tmp").join(format!("fray-mote-{tag}-{}", &random_key().unwrap()[..10]));
        fs::create_dir_all(&p).unwrap();
        Self(fs::canonicalize(&p).unwrap())
    }
}
impl Drop for Temp {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .current_dir(dir)
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "protocol.file.allow=always",
        ])
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn fake_store(dir: &Path, id: &str) -> PathBuf {
    let m = dir.join(".mote");
    fs::create_dir_all(&m).unwrap();
    fs::write(
        m.join("FORMAT.json"),
        json!({"schema_version":1,"store_id":id}).to_string(),
    )
    .unwrap();
    m
}

/// A stand-in `mote`: answers --version and `board`, can fail or hang.
fn stub(dir: &Path, body: &str) -> PathBuf {
    let p = dir.join("mote-stub");
    fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
    p
}

const STUB_OK: &str = r#"for a in "$@"; do last="$a"; done
case "$*" in
  *--version*) echo "${STUB_VERSION:-mote 0.1.0}";;
  *board*) echo '{"active_claims":[{"id":"bd-1","claimed_by":"alice"}]}';;
  *) echo "{}";;
esac"#;

#[test]
fn outcomes_are_classified_from_exit_code_stderr_and_json_together() {
    assert_eq!(
        classify(Some(0), "{\"a\":1}", ""),
        Outcome::Ok(json!({"a":1}))
    );
    assert_eq!(classify(Some(0), "", ""), Outcome::Ok(Value::Null));
    // `events` prints JSON lines; --version prints text.
    assert_eq!(
        classify(Some(0), "{\"e\":1}\n{\"e\":2}\n", ""),
        Outcome::Ok(json!([{"e":1},{"e":2}]))
    );
    assert_eq!(
        classify(Some(0), "mote 0.1.0\n", ""),
        Outcome::Ok(json!("mote 0.1.0"))
    );
    assert!(matches!(
        classify(Some(0), "{broken", ""),
        Outcome::Failed(_)
    ));
    // Exit 2 is a rejection only with `rejected` or accepted:false ...
    assert_eq!(
        classify(Some(2), "", "rejected: claim held by bob\n"),
        Outcome::Rejected("claim held by bob".into())
    );
    assert_eq!(
        classify(
            Some(2),
            "handoff claim rejected: stale clock",
            "handoff claim rejected: stale clock"
        ),
        Outcome::Rejected("stale clock".into())
    );
    assert_eq!(
        classify(
            Some(2),
            r#"{"accepted":false,"reason":"path conflict"}"#,
            ""
        ),
        Outcome::Rejected("path conflict".into())
    );
    // ... otherwise it is a usage error: an adapter bug, never a rejection.
    assert_eq!(
        classify(Some(2), "", "error: unexpected argument '--bogus'\n"),
        Outcome::Invalid("error: unexpected argument '--bogus'".into())
    );
    assert!(matches!(
        classify(Some(3), "", "actor unresolved"),
        Outcome::Invalid(_)
    ));
    assert!(matches!(classify(Some(1), "", "boom"), Outcome::Failed(_)));
    assert!(matches!(
        classify(Some(4), "", "hash mismatch"),
        Outcome::Failed(_)
    ));
    assert!(matches!(classify(None, "", ""), Outcome::Failed(_)));
    // preflight reports conflicts with exit 2: that is a result.
    assert_eq!(
        classify_result(Some(2), r#"{"conflicts":[{"actor":"bob"}]}"#, ""),
        Outcome::Ok(json!({"conflicts":[{"actor":"bob"}]}))
    );
    assert!(matches!(
        classify_result(Some(2), "", "error: usage"),
        Outcome::Invalid(_)
    ));
}

#[test]
fn an_ancestor_board_pairs_with_its_sibling_store() {
    let t = Temp::new("sib");
    let board = t.0.join(".fray");
    fs::create_dir_all(&board).unwrap();
    assert_eq!(
        mote::locate(&board, &t.0).unwrap(),
        None,
        "no .mote: not adopted"
    );
    let m = fake_store(&t.0, "st-A");
    let nested = t.0.join("a/b");
    fs::create_dir_all(&nested).unwrap();
    assert_eq!(mote::locate(&board, &nested).unwrap(), Some(m.clone()));
    assert_eq!(mote::store_id(&m).unwrap(), "st-A");
}

#[test]
fn a_worktree_outside_the_checkout_finds_the_main_worktrees_store() {
    let t = Temp::new("wt");
    let repo = t.0.join("repo");
    fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let m = fake_store(&repo, "st-B");
    let outside = t.0.join("elsewhere");
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            outside.to_str().unwrap(),
        ],
    );
    // The board for such a worktree is <git-common-dir>/fray.
    let board = repo.join(".git/fray");
    assert_eq!(mote::locate(&board, &outside).unwrap(), Some(m));
}

#[test]
fn bare_repositories_and_submodules_must_name_their_store() {
    let t = Temp::new("bare");
    let bare = t.0.join("bare.git");
    fs::create_dir_all(&bare).unwrap();
    git(&bare, &["init", "-q", "--bare"]);
    let err = mote::locate(&bare.join("fray"), &bare).unwrap_err();
    assert_eq!(err.code, "mote_store_required");

    let sub_src = t.0.join("sub-src");
    fs::create_dir_all(&sub_src).unwrap();
    git(&sub_src, &["init", "-q"]);
    git(&sub_src, &["commit", "-q", "--allow-empty", "-m", "s"]);
    let sup = t.0.join("super");
    fs::create_dir_all(&sup).unwrap();
    git(&sup, &["init", "-q"]);
    git(&sup, &["commit", "-q", "--allow-empty", "-m", "p"]);
    git(
        &sup,
        &["submodule", "add", "-q", sub_src.to_str().unwrap(), "sub"],
    );
    let sub = sup.join("sub");
    let board = sup.join(".git/modules/sub/fray");
    let err = mote::locate(&board, &sub).unwrap_err();
    assert_eq!(err.code, "mote_store_required");
}

#[test]
fn a_changed_store_id_is_refused_not_followed() {
    let t = Temp::new("id");
    let m = fake_store(&t.0, "st-A");
    let bin = stub(&t.0, STUB_OK);
    let store = Store {
        path: m.clone(),
        store_id: "st-A".into(),
    };
    assert!(matches!(
        mote::run_with(&bin, &store, Some("alice"), &["board"], mote::READ_TIMEOUT),
        Outcome::Ok(_)
    ));
    fake_store(&t.0, "st-OTHER");
    match mote::run_with(&bin, &store, Some("alice"), &["board"], mote::READ_TIMEOUT) {
        Outcome::Invalid(why) => assert!(why.contains("mote_store_mismatch"), "{why}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn the_store_actor_and_json_are_always_explicit() {
    let t = Temp::new("argv");
    let m = fake_store(&t.0, "st-A");
    let log = t.0.join("argv.log");
    let bin = stub(
        &t.0,
        &format!(
            r#"echo "$*|MOTE_ACTOR=${{MOTE_ACTOR:-}}" >> {}; echo '{{}}'"#,
            log.display()
        ),
    );
    let store = Store {
        path: m.clone(),
        store_id: "st-A".into(),
    };
    mote::run_with(
        &bin,
        &store,
        Some("alice"),
        &["who-has", "src/"],
        mote::READ_TIMEOUT,
    );
    mote::run_with(
        &bin,
        &store,
        None,
        &["events", "--after", "x"],
        mote::READ_TIMEOUT,
    );
    let lines: Vec<String> = fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(str::to_owned)
        .collect();
    assert_eq!(
        lines[0],
        format!(
            "--store {} --json --actor alice who-has src/|MOTE_ACTOR=",
            m.display()
        )
    );
    // events runs without --actor, which Mote would treat as a filter.
    assert!(!lines[1].contains("--actor"), "{}", lines[1]);
}

#[test]
fn a_timeout_stops_and_reaps_the_whole_mote_process_group() {
    let t = Temp::new("slow");
    let m = fake_store(&t.0, "st-A");
    let pidfile = t.0.join("grandchild.pid");
    // Mote starts a grandchild, then hangs.
    let bin = stub(
        &t.0,
        &format!("sleep 60 & echo $! > {}; sleep 60", pidfile.display()),
    );
    let store = Store {
        path: m,
        store_id: "st-A".into(),
    };
    let start = Instant::now();
    let outcome = mote::run_with(
        &bin,
        &store,
        Some("a"),
        &["board"],
        Duration::from_millis(1500),
    );
    assert!(start.elapsed() < Duration::from_secs(5));
    assert!(
        matches!(&outcome, Outcome::Failed(why) if why.contains("timed out")),
        "{outcome:?}"
    );
    let pid = fs::read_to_string(&pidfile)
        .expect("the stub records its grandchild before the timeout")
        .trim()
        .to_owned();
    // Give the kernel a moment to deliver the group SIGKILL.
    let mut alive = true;
    for _ in 0..50 {
        alive = Command::new("kill")
            .args(["-0", &pid])
            .status()
            .unwrap()
            .success();
        if !alive {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!alive, "grandchild {pid} outlived the timeout");
}

#[test]
fn a_board_binds_once_and_refuses_a_different_store() {
    let mut b = Board::memory().unwrap();
    let call =
        |b: &mut Board, args: Value| b.execute_at(&Request::new("mote_bind", "alice", args), 1);
    b.execute_at(&Request::new("join", "alice", json!({})), 1)
        .unwrap();
    let first = call(&mut b, json!({"store":"/r/.mote","store_id":"st-A"})).unwrap();
    assert_eq!(first["new"], true);
    let again = call(&mut b, json!({"store":"/r/.mote","store_id":"st-A"})).unwrap();
    assert_eq!(again["new"], false);
    let other = call(&mut b, json!({"store":"/x/.mote","store_id":"st-B"})).unwrap_err();
    assert_eq!(other.code, "mote_store_mismatch");
    assert!(other.message.contains("st-A"));
    let read = b
        .execute_at(&Request::new("mote_binding", "", json!({})), 1)
        .unwrap();
    assert_eq!(read["binding"]["store_id"], "st-A");
    assert_eq!(
        call(&mut b, json!({"store":"/r/.mote","store_id":"nope"}))
            .unwrap_err()
            .code,
        "invalid"
    );
}

/// A scratch project with a running board and, optionally, a Mote store.
struct Project {
    t: Temp,
}
impl Project {
    fn new(tag: &str, mote: bool) -> Self {
        let t = Temp::new(tag);
        fs::create_dir_all(t.0.join(".fray")).unwrap();
        if mote {
            fake_store(&t.0, "st-P");
        }
        let p = Self { t };
        p.fray(&[], "", &["start"]);
        p.fray(&[], "alice", &["join"]);
        p
    }
    fn fray(&self, env: &[(&str, &str)], actor: &str, args: &[&str]) -> (bool, String, String) {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_fray"));
        cmd.current_dir(&self.t.0)
            .env_remove("FRAY_AGENT")
            .env_remove("FRAY_SESSION")
            .env_remove("MOTE_STORE")
            .env_remove("MOTE_ACTOR")
            .env("FRAY_SESSION", format!("test:{actor}"))
            .args(["--home", self.t.0.join(".fray").to_str().unwrap()]);
        if !actor.is_empty() {
            cmd.args(["--as", actor]);
        }
        for (k, v) in env {
            cmd.env(k, v);
        }
        let out = cmd.args(args).output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
    fn status(&self, env: &[(&str, &str)], actor: &str) -> std::result::Result<Value, String> {
        let (ok, out, err) = self.fray(env, actor, &["--json", "mote", "status"]);
        if ok {
            Ok(serde_json::from_str::<Value>(&out).unwrap()["mote"].clone())
        } else {
            // With --json, errors are reported on stdout.
            Err(format!("{out}{err}"))
        }
    }
}
impl Drop for Project {
    fn drop(&mut self) {
        self.fray(&[], "", &["stop"]);
    }
}

#[test]
fn status_without_mote_says_not_adopted() {
    let p = Project::new("none", false);
    let s = p.status(&[], "alice").unwrap();
    assert_eq!(s["adopted"], false, "{s}");
}

#[test]
fn status_binds_reads_and_refuses_a_swapped_store() {
    let p = Project::new("bind", true);
    let bin = stub(&p.t.0, STUB_OK);
    let env = [("FRAY_MOTE_BIN", bin.to_str().unwrap())];
    let s = p.status(&env, "alice").unwrap();
    assert_eq!(s["adopted"], true);
    assert_eq!(s["store_id"], "st-P");
    assert_eq!(s["reads"]["ok"], true);
    assert_eq!(s["reads"]["active_claims"], 1);
    assert_eq!(s["binding"]["store_id"], "st-P");
    // Another store under the same path is refused, even read-only.
    fake_store(&p.t.0, "st-SWAPPED");
    let err = p.status(&env, "alice").unwrap_err();
    assert!(err.contains("mote_store_mismatch"), "{err}");
    let err = p.status(&env, "").unwrap_err();
    assert!(err.contains("mote_store_mismatch"), "{err}");
}

#[test]
fn status_refuses_an_unsupported_mote_and_warns_on_a_second_actor() {
    let p = Project::new("ver", true);
    let bin = stub(&p.t.0, STUB_OK);
    let b = bin.to_str().unwrap();
    let err = p
        .status(
            &[("FRAY_MOTE_BIN", b), ("STUB_VERSION", "mote 0.2.0")],
            "alice",
        )
        .unwrap_err();
    assert!(err.contains("mote_version"), "{err}");
    let s = p
        .status(
            &[("FRAY_MOTE_BIN", b), ("MOTE_ACTOR", "someone-else")],
            "alice",
        )
        .unwrap();
    let warnings = s["warnings"].to_string();
    assert!(warnings.contains("second actor"), "{warnings}");
    // An unreachable Mote degrades reads to advisory; it is not an error.
    let broken = stub(
        &p.t.0,
        r#"case "$*" in *--version*) echo "mote 0.1.0";; *) echo boom >&2; exit 1;; esac"#,
    );
    let s = p
        .status(&[("FRAY_MOTE_BIN", broken.to_str().unwrap())], "alice")
        .unwrap();
    assert_eq!(s["reads"]["ok"], false);
    assert!(s["warnings"].to_string().contains("advisory"), "{s}");
}

#[test]
fn status_against_the_real_mote_when_installed() {
    if Command::new("mote").arg("--version").output().is_err() {
        eprintln!("mote not installed; skipping the real-binary check");
        return;
    }
    let t = Temp::new("real");
    fs::create_dir_all(t.0.join(".fray")).unwrap();
    let init = Command::new("mote")
        .current_dir(&t.0)
        .arg("init")
        .output()
        .unwrap();
    assert!(
        init.status.success(),
        "{}",
        String::from_utf8_lossy(&init.stderr)
    );
    let p = Project { t };
    p.fray(&[], "", &["start"]);
    p.fray(&[], "alice", &["join"]);
    let s = p.status(&[], "alice").unwrap();
    assert_eq!(s["adopted"], true, "{s}");
    assert!(s["version"].as_str().unwrap().starts_with("mote 0.1."));
    assert_eq!(s["reads"]["ok"], true, "{s}");
    assert_eq!(s["binding"]["store_id"], s["store_id"]);
}
