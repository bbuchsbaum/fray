//! Epic child 4: the git guard (docs/design/mote-adapter.md section 7.1).
use fray::model::random_key;
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const FRAY: &str = env!("CARGO_BIN_EXE_fray");

struct Repo {
    root: PathBuf,
}

impl Repo {
    fn new(tag: &str) -> Self {
        let root =
            PathBuf::from("/tmp").join(format!("fray-guard-{tag}-{}", &random_key().unwrap()[..8]));
        fs::create_dir_all(&root).unwrap();
        let root = fs::canonicalize(&root).unwrap();
        let r = Self { root };
        r.git(&["init", "-q", "-b", "main"]);
        fs::write(
            r.root.join(".git/info/exclude"),
            ".fray/\n.mote/\n.worktrees/\n",
        )
        .unwrap();
        fs::write(r.root.join("README"), "x\n").unwrap();
        r.git(&["add", "README"]);
        r.git(&["commit", "-q", "-m", "base"]);
        r.fray("", &["start"]);
        for who in ["alice", "bob"] {
            r.fray(who, &["join"]);
        }
        r
    }
    fn git_in(&self, dir: &Path, actor: &str, args: &[&str]) -> Output {
        let mut cmd = Command::new("git");
        cmd.current_dir(dir)
            .args(["-c", "user.name=t", "-c", "user.email=t@t"])
            .args(args)
            .env_remove("FRAY_GUARD")
            .env_remove("MOTE_STORE")
            .env_remove("MOTE_ACTOR")
            .env("FRAY_SESSION", format!("test:{actor}"));
        if actor.is_empty() {
            cmd.env_remove("FRAY_AGENT");
        } else {
            cmd.env("FRAY_AGENT", actor);
        }
        cmd.output().unwrap()
    }
    fn git(&self, args: &[&str]) -> Output {
        let out = self.git_in(&self.root, "", args);
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        out
    }
    fn fray(&self, actor: &str, args: &[&str]) -> Output {
        let mut cmd = Command::new(FRAY);
        cmd.current_dir(&self.root)
            .env_remove("FRAY_AGENT")
            .env_remove("MOTE_STORE")
            .env_remove("MOTE_ACTOR")
            .env("FRAY_SESSION", format!("test:{actor}"))
            .args(["--home", self.root.join(".fray").to_str().unwrap()]);
        if !actor.is_empty() {
            cmd.args(["--as", actor]);
        }
        cmd.args(args).output().unwrap()
    }
    /// alice stages a change to `path` and commits it; returns (ok, stderr).
    fn commit(&self, dir: &Path, path: &str, env_block: bool) -> (bool, String) {
        let file = dir.join(path);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, random_key().unwrap()).unwrap();
        self.git_in(dir, "alice", &["add", path]);
        let mut cmd = Command::new("git");
        cmd.current_dir(dir)
            .args([
                "-c",
                "user.name=t",
                "-c",
                "user.email=t@t",
                "commit",
                "-q",
                "-m",
                path,
            ])
            .env("FRAY_AGENT", "alice")
            .env("FRAY_SESSION", "test:alice")
            .env_remove("MOTE_STORE")
            .env_remove("MOTE_ACTOR");
        if env_block {
            cmd.env("FRAY_GUARD", "block");
        } else {
            cmd.env_remove("FRAY_GUARD");
        }
        let out = cmd.output().unwrap();
        (
            out.status.success(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }
    fn install(&self) {
        let out = self.fray("alice", &["--json", "guard", "install"]);
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stdout)
        );
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        self.fray("", &["stop"]);
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn hook(dir: &Path, name: &str, body: &str) {
    let p = dir.join(".git/hooks").join(name);
    fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    fs::set_permissions(&p, fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn a_foreign_lane_warns_blocks_on_request_and_an_own_lane_is_quiet() {
    let r = Repo::new("lane");
    r.install();
    assert!(r
        .fray("bob", &["lane", "take", "src/", "--purpose", "parser"])
        .status
        .success());
    assert!(r
        .fray("alice", &["lane", "take", "docs/", "--purpose", "docs"])
        .status
        .success());
    let (ok, err) = r.commit(&r.root, "src/a.rs", false);
    assert!(ok, "advisory by default: {err}");
    assert!(
        err.contains("in bob's lane") && err.contains("src/a.rs"),
        "{err}"
    );
    let (ok, err) = r.commit(&r.root, "src/b.rs", true);
    assert!(!ok, "FRAY_GUARD=block refuses: {err}");
    // The refused change stays staged; unstage it before the next commit.
    r.git(&["reset", "-q"]);
    let (ok, err) = r.commit(&r.root, "docs/own.md", true);
    assert!(
        ok && !err.contains("fray guard:"),
        "own lane is quiet: {err}"
    );
}

#[test]
fn a_prior_hook_runs_first_and_its_failure_is_kept() {
    let r = Repo::new("chain");
    hook(&r.root, "pre-commit", "echo prior-ran >&2; exit 7");
    r.install();
    assert!(r.root.join(".git/hooks/pre-commit.fray-prior").exists());
    let (ok, err) = r.commit(&r.root, "x.txt", false);
    assert!(!ok && err.contains("prior-ran"), "{err}");
    // Reinstalling is idempotent: the prior hook is not wrapped twice.
    r.install();
    let text = fs::read_to_string(r.root.join(".git/hooks/pre-commit.fray-prior")).unwrap();
    assert!(text.contains("exit 7"));
}

#[test]
fn pre_push_checks_new_and_updated_branches_and_keeps_stdin_for_the_prior_hook() {
    let r = Repo::new("push");
    let remote = r.root.parent().unwrap().join(format!(
        "{}-remote.git",
        r.root.file_name().unwrap().to_string_lossy()
    ));
    let out = Command::new("git")
        .args(["init", "-q", "--bare", remote.to_str().unwrap()])
        .output()
        .unwrap();
    assert!(out.status.success());
    r.git(&["remote", "add", "origin", remote.to_str().unwrap()]);
    r.git(&["push", "-q", "origin", "main"]);
    let seen = r.root.join("prior-stdin");
    hook(&r.root, "pre-push", &format!("cat > {}", seen.display()));
    r.install();
    assert!(r
        .fray("bob", &["lane", "take", "src/", "--purpose", "parser"])
        .status
        .success());
    // A new branch with a commit in bob's lane, and an update to main.
    r.git(&["switch", "-q", "-c", "feature"]);
    let (ok, _) = r.commit(&r.root, "src/feature.rs", false);
    assert!(ok);
    r.git(&["switch", "-q", "main"]);
    let (ok, _) = r.commit(&r.root, "src/main_change.rs", false);
    assert!(ok);
    let out = r.git_in(&r.root, "alice", &["push", "origin", "main", "feature"]);
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(out.status.success(), "advisory: {err}");
    assert!(
        err.contains("src/feature.rs") && err.contains("src/main_change.rs"),
        "{err}"
    );
    // The prior hook saw both ref-update lines.
    let prior = fs::read_to_string(&seen).unwrap();
    assert_eq!(
        prior.lines().filter(|l| !l.is_empty()).count(),
        2,
        "{prior}"
    );
    // A deletion pushes no paths.
    let out = r.git_in(&r.root, "alice", &["push", "origin", "--delete", "feature"]);
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stderr).contains("in bob's lane"));
}

#[test]
fn every_worktree_shares_the_guard_and_the_board() {
    let r = Repo::new("wt");
    r.install();
    assert!(r
        .fray("bob", &["lane", "take", "src/", "--purpose", "parser"])
        .status
        .success());
    // Worktrees live inside the checkout, as this project keeps them
    // (.worktrees/), so they share the checkout's board.
    let wt = r.root.join(".worktrees/side");
    r.git(&["worktree", "add", "-q", "-b", "side", wt.to_str().unwrap()]);
    let (ok, err) = r.commit(&wt, "src/in_worktree.rs", false);
    let _ = Command::new("git")
        .current_dir(&r.root)
        .args(["worktree", "remove", "--force", wt.to_str().unwrap()])
        .output();
    assert!(ok);
    assert!(
        err.contains("in bob's lane") && err.contains("src/in_worktree.rs"),
        "{err}"
    );
}

#[test]
fn an_unreachable_daemon_warns_and_never_blocks() {
    let r = Repo::new("down");
    r.install();
    assert!(r
        .fray("bob", &["lane", "take", "src/", "--purpose", "parser"])
        .status
        .success());
    r.fray("", &["stop"]);
    let (ok, err) = r.commit(&r.root, "src/while_down.rs", true);
    assert!(ok, "never blocks because the board is down: {err}");
    assert!(err.contains("could not check"), "{err}");
}

#[test]
fn mote_reservations_decide_where_mote_is_adopted() {
    if Command::new("mote").arg("--version").output().is_err() {
        eprintln!("mote not installed; skipping");
        return;
    }
    let r = Repo::new("mote");
    let mote = |actor: &str, args: &[&str]| {
        Command::new("mote")
            .current_dir(&r.root)
            .env_remove("MOTE_STORE")
            .env_remove("MOTE_ACTOR")
            .arg("--store")
            .arg(r.root.join(".mote"))
            .args(["--actor", actor])
            .args(args)
            .output()
            .unwrap()
    };
    assert!(mote("bob", &["init"]).status.success());
    let out = mote("bob", &["new", "parser"]);
    let text = String::from_utf8_lossy(&out.stdout).into_owned();
    let bead = text
        .split_whitespace()
        .find(|w| w.starts_with("bd-"))
        .unwrap()
        .to_owned();
    assert!(mote("bob", &["claim", &bead]).status.success());
    assert!(mote("bob", &["reserve", "--issue", &bead, "lib/"])
        .status
        .success());
    r.install();
    let (ok, err) = r.commit(&r.root, "lib/core.rs", false);
    assert!(ok);
    assert!(
        err.contains("reserved in Mote by bob") && err.contains("lib/core.rs"),
        "{err}"
    );
    let (ok, _) = r.commit(&r.root, "lib/other.rs", true);
    assert!(!ok, "blocking applies to Mote reservations too");
}
