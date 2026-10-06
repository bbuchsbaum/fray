use mote::{
    Store, authority,
    candidate::AuthorizationStatus,
    ids,
    op::{CandidateRevokeOp, Op},
    publish, reducer,
};
use serde_json::{Value, json};
use std::{
    path::Path,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};
use tempfile::TempDir;

fn git(root: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .current_dir(root)
        .args(args)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().into()
}
fn command(root: &Path, actor: &str, args: &[&str]) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_mote"));
    cmd.current_dir(root)
        .args([
            "--store",
            root.to_str().unwrap(),
            "--actor",
            actor,
            "--json",
        ])
        .args(args);
    cmd
}
fn run(root: &Path, actor: &str, args: &[&str]) -> Value {
    let out = command(root, actor, args).output().unwrap();
    assert!(
        out.status.success(),
        "mote {args:?}: {} {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    serde_json::from_slice(&out.stdout).unwrap()
}
struct Fixture {
    root: TempDir,
    candidate: String,
    before: String,
    after: String,
    phase: String,
    authorization: String,
}
impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().unwrap();
        let p = root.path();
        git(p, &["init", "-q", "-b", "main"]);
        git(p, &["config", "user.name", "Test"]);
        git(p, &["config", "user.email", "test@example.invalid"]);
        std::fs::write(p.join("work.txt"), "base\n").unwrap();
        git(p, &["add", "work.txt"]);
        git(p, &["commit", "-qm", "base"]);
        let before = git(p, &["rev-parse", "HEAD"]);
        let store = Store::init(p).unwrap();
        let issue = ids::new_bead_id();
        publish::publish_op(
            &store,
            &mote::op::make_create(
                "author".into(),
                issue.clone(),
                mote::op::ScalarSet {
                    title: Some("fenced landing".into()),
                    ..Default::default()
                },
                jiff::Timestamp::now(),
            ),
        )
        .unwrap();
        git(p, &["switch", "-qc", "candidate"]);
        std::fs::write(p.join("work.txt"), "candidate\n").unwrap();
        git(p, &["commit", "-qam", "candidate"]);
        let after = git(p, &["rev-parse", "HEAD"]);
        let proposal = run(
            p,
            "proposer",
            &[
                "candidate",
                "propose",
                "--issue",
                &issue,
                "--base",
                &before,
                "--path",
                "work.txt",
                "--authorizer",
                "author",
                "--reviewer",
                "reviewer",
                "--idempotency-key",
                "proposal",
            ],
        );
        let candidate = proposal["candidate_id"].as_str().unwrap().to_string();
        run(
            p,
            "reviewer",
            &[
                "candidate",
                "review",
                &candidate,
                "approve",
                "--idempotency-key",
                "review",
            ],
        );
        let auth = run(
            p,
            "author",
            &[
                "candidate",
                "authorize",
                &candidate,
                "--grantee",
                "lander",
                "--idempotency-key",
                "auth",
            ],
        );
        run(
            p,
            "author",
            &[
                "candidate",
                "evidence",
                "target-scope",
                &candidate,
                "--target",
                "main",
                "--idempotency-key",
                "scope",
            ],
        );
        Self {
            root,
            candidate,
            before,
            after,
            phase: auth["phase"]["op_id"].as_str().unwrap().into(),
            authorization: auth["authorization"]["op_id"].as_str().unwrap().into(),
        }
    }
    fn store(&self) -> Store {
        Store::open(&self.root.path().join(".mote")).unwrap()
    }
    fn land(&self, actor: &str) -> Command {
        command(
            self.root.path(),
            actor,
            &[
                "candidate",
                "land",
                &self.candidate,
                "--target",
                "main",
                "--before",
                &self.before,
                "--expect-phase",
                &self.phase,
                "--expect-authorization",
                &self.authorization,
                "--idempotency-key",
                "landing",
            ],
        )
    }
    fn revoke(&self) -> Command {
        command(
            self.root.path(),
            "author",
            &[
                "candidate",
                "revoke",
                &self.candidate,
                "--expect",
                &self.authorization,
                "--idempotency-key",
                "revoke",
            ],
        )
    }
    fn tip(&self) -> String {
        git(self.root.path(), &["rev-parse", "main"])
    }
}
fn payload(out: &Output) -> Value {
    serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
        panic!(
            "stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        )
    })
}
fn wait_signal(path: &Path) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        assert!(Instant::now() < deadline, "missing checkpoint signal");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn review_wait_pause(path: &Path, child: &mut std::process::Child) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !path.exists() {
        if let Some(status) = child.try_wait().unwrap() {
            panic!("owned child exited before pause: {status}");
        }
        if Instant::now() >= deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("owned child did not reach pause");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn review_regression_confirmed_journal_io_failure_is_nonzero() {
    use std::os::unix::fs::PermissionsExt;
    let f = Fixture::new();
    let signal = f.root.path().join("review-confirmed-pause");
    let mut child = f.land("lander")
        .env("MOTE_TEST_AUTHORITY_PAUSE", "landing-confirmed-op")
        .env("MOTE_TEST_AUTHORITY_SIGNAL", &signal)
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    review_wait_pause(&signal, &mut child);
    let dir = authority::directory(&f.store());
    let original = std::fs::metadata(&dir).unwrap().permissions();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    std::fs::remove_file(&signal).unwrap();
    let out = child.wait_with_output().unwrap();
    std::fs::set_permissions(&dir, original).unwrap();
    let first = payload(&out);
    let active = dir.join("landing-active.json");
    let retained: Value = serde_json::from_slice(&std::fs::read(&active).unwrap()).unwrap();
    let blocked = command(f.root.path(), "author", &["new", "post-landing"]).output().unwrap();
    let retry = f.land("lander").output().unwrap();
    println!("REVIEW_IO {}", json!({"exit":out.status.code(),"result":first,"durable_phase":retained["phase"],"active_existed":true,"writer_exit":blocked.status.code(),"writer_stderr":String::from_utf8_lossy(&blocked.stderr),"retry_exit":retry.status.code(),"retry":payload(&retry),"active_after_retry":active.exists()}));
    assert!(retry.status.success(), "exact recovery failed: {retry:?}");
    assert!(!active.exists());
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("landing"));
    assert_eq!(first["git_updated"], true);
    assert!(!out.status.success(), "journal I/O failure incorrectly returned success: {first}");
    assert_eq!(first["outcome"], "recovery_required");
}

#[test]
fn review_regression_ref_moved_after_final_check_is_nonzero() {
    let f = Fixture::new();
    let signal = f.root.path().join("review-ref-pause");
    let mut child = f.land("lander")
        .env("MOTE_TEST_AUTHORITY_PAUSE", "landing-updated")
        .env("MOTE_TEST_AUTHORITY_SIGNAL", &signal)
        .stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    review_wait_pause(&signal, &mut child);
    git(f.root.path(), &["commit", "-q", "--allow-empty", "-m", "external ref movement"]);
    let moved = git(f.root.path(), &["rev-parse", "HEAD"]);
    git(f.root.path(), &["update-ref", "refs/heads/main", &moved, &f.after]);
    std::fs::remove_file(&signal).unwrap();
    let out = child.wait_with_output().unwrap();
    let first = payload(&out);
    println!("REVIEW_MOVED_REF {}", json!({"exit":out.status.code(),"result":first,"moved_oid":moved,"actual_target":f.tip()}));
    assert_eq!(f.tip(), moved, "must not reset external movement");
    assert_eq!(first["git_updated"], true);
    assert_eq!(first["current_oid"], moved);
    assert_eq!(first["target_current"], false);
    assert!(!out.status.success(), "first landing call reported success after target moved before confirmation: {first}");
}

fn review_claim_order(enabled: bool) -> Value {
    let root = TempDir::new().unwrap();
    let store = Store::init(root.path()).unwrap();
    let issue = ids::new_bead_id();
    let base = jiff::Timestamp::now();
    let earlier = jiff::Timestamp::from_second(base.as_second() + 1).unwrap();
    let later = jiff::Timestamp::from_second(base.as_second() + 2).unwrap();
    publish::publish_op(&store, &mote::op::make_create("owner".into(), issue.clone(), mote::op::ScalarSet {title:Some("claim order".into()),..Default::default()},base)).unwrap();
    if enabled { authority::Writer::acquire(&store).unwrap().enable().unwrap(); }
    let alice = publish::publish_op(&store, &mote::op::make_claim("alice".into(),issue.clone(),"alice".into(),3600,None,later)).unwrap();
    let initially = reducer::replay_store(&store).unwrap();
    assert!(initially.was_accepted(alice.as_str()));
    assert_eq!(initially.beads[&issue].claim.as_ref().unwrap().claimed_by,"alice");
    let bob = publish::publish_op(&store, &mote::op::make_claim("bob".into(),issue.clone(),"bob".into(),3600,None,earlier)).unwrap();
    let state = reducer::replay_store(&store).unwrap();
    json!({"authority_enabled":enabled,"alice_first_accepted":true,"alice_still_accepted":state.was_accepted(alice.as_str()),"bob_accepted":state.was_accepted(bob.as_str()),"final_holder":state.beads[&issue].claim.as_ref().unwrap().claimed_by})
}

#[test]
fn review_regression_first_claim_remains_winner_without_activation() {
    let result = review_claim_order(false);
    println!("REVIEW_LEGACY_CLAIM {result}");
    assert_eq!(result["alice_still_accepted"], true, "later admission changed the previously accepted winner");
}

#[test]
fn review_regression_activated_claim_order_positive_control() {
    let result = review_claim_order(true);
    println!("REVIEW_FENCED_CLAIM {result}");
    assert_eq!(result["alice_still_accepted"], true);
    assert_eq!(result["bob_accepted"], false);
    assert_eq!(result["final_holder"], "alice");
}
