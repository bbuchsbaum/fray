//! The Mote adapter's transport (docs/design/mote-adapter.md, sections 1-4).
//!
//! Fray runs the `mote` binary in the client and parses its `--json` output;
//! it never opens Mote's files, apart from reading `FORMAT.json` for the store
//! id, and never calls Mote from the daemon or under the store lock. Mote owns
//! work, claims, reservations and candidates; nothing here records a
//! substitute for them.
use crate::model::*;
use serde_json::Value;
use std::{
    env, fs,
    io::Read,
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Bounded waits (section 1): reads degrade, mutations must be confirmed.
pub const READ_TIMEOUT: Duration = Duration::from_secs(10);
pub const MUTATION_TIMEOUT: Duration = Duration::from_secs(30);
/// The Mote releases this contract was verified against.
const SUPPORTED: &str = "mote 0.1.";

/// The binary to run: `FRAY_MOTE_BIN` if set (tests, unusual installs), else
/// `mote` on PATH.
fn binary() -> PathBuf {
    env::var_os("FRAY_MOTE_BIN")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("mote"))
}

/// How one Mote call ended, classified from exit code, stderr and JSON
/// together (section 4). Callers decide what a failure means for a read
/// (degrade) or a mutation (unconfirmed).
#[derive(Debug, PartialEq)]
pub enum Outcome {
    /// Exit 0, with the parsed JSON (Null when Mote printed nothing).
    Ok(Value),
    /// The reducer refused the op, with Mote's reason.
    Rejected(String),
    /// A usage error or version mismatch: an adapter bug, never a rejection.
    Invalid(String),
    /// Anything else: timeout, internal or store error, unparseable output.
    /// The outcome of a mutation is unknown until Mote is read again.
    Failed(String),
}

/// A bound Mote store.
#[derive(Debug, Clone, PartialEq)]
pub struct Store {
    pub path: PathBuf,
    pub store_id: String,
}

fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .current_dir(dir)
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

/// A path given as either `.mote` or its parent, as Mote itself accepts.
fn mote_dir(path: &Path) -> PathBuf {
    if path.file_name().is_some_and(|n| n == ".mote") {
        path.to_path_buf()
    } else {
        path.join(".mote")
    }
}

/// Where this board's Mote store is (section 2), or None when Mote is not
/// adopted. Uses the same resolution that chose the board: an ancestor
/// `.fray/` pairs with its sibling `.mote/`; a `<git-common-dir>/fray` board
/// pairs with `.mote/` in the main worktree. Bare repositories and submodules
/// must name the store with `MOTE_STORE`.
pub fn locate(board: &Path, cwd: &Path) -> Result<Option<PathBuf>> {
    if let Some(explicit) = env::var_os("MOTE_STORE").filter(|v| !v.is_empty()) {
        let explicit = PathBuf::from(explicit);
        let explicit = if explicit.is_absolute() {
            explicit
        } else {
            cwd.join(explicit)
        };
        return Ok(Some(mote_dir(&explicit)));
    }
    let candidate = if board.file_name().is_some_and(|n| n == ".fray") {
        board.parent().map(|root| root.join(".mote"))
    } else {
        if git(cwd, &["rev-parse", "--is-bare-repository"]).as_deref() == Some("true")
            || git(cwd, &["rev-parse", "--show-superproject-working-tree"])
                .is_some_and(|s| !s.is_empty())
        {
            return Err(Error::new(
                "mote_store_required",
                "a bare repository or submodule must name its Mote store with MOTE_STORE",
            ));
        }
        git(cwd, &["worktree", "list", "--porcelain"]).and_then(|list| {
            list.lines()
                .next()
                .and_then(|l| l.strip_prefix("worktree "))
                .map(|main| Path::new(main).join(".mote"))
        })
    };
    Ok(candidate.filter(|p| p.join("FORMAT.json").is_file()))
}

/// The store id from `FORMAT.json`: the one file Fray reads directly.
pub fn store_id(path: &Path) -> Result<String> {
    let format: Value = serde_json::from_slice(&fs::read(path.join("FORMAT.json"))?)?;
    format["store_id"]
        .as_str()
        .filter(|id| id.starts_with("st-"))
        .map(str::to_owned)
        .ok_or_else(|| Error::new("mote_store", "FORMAT.json has no store_id"))
}

/// `mote --version`, refusing releases this contract was not verified against.
pub fn version() -> Result<String> {
    match run_raw(&binary(), None, None, &["--version"], READ_TIMEOUT) {
        Outcome::Ok(Value::String(v)) if v.starts_with(SUPPORTED) => Ok(v),
        Outcome::Ok(Value::String(v)) => Err(Error::new(
            "mote_version",
            format!("{v} is not supported; this Fray supports {SUPPORTED}x"),
        )),
        Outcome::Ok(other) => Err(Error::new(
            "mote_version",
            format!("unrecognized version output: {other}"),
        )),
        Outcome::Rejected(e) | Outcome::Invalid(e) | Outcome::Failed(e) => {
            Err(Error::new("mote_unavailable", e))
        }
    }
}

/// Runs one Mote command against a bound store. `actor` is passed for every
/// command except `events`, where Mote treats `--actor` as a filter
/// (section 1). Never waits past `timeout`.
pub fn run(store: &Store, actor: Option<&str>, args: &[&str], timeout: Duration) -> Outcome {
    run_with(&binary(), store, actor, args, timeout)
}

/// [`run`] with an explicit Mote binary.
pub fn run_with(
    bin: &Path,
    store: &Store,
    actor: Option<&str>,
    args: &[&str],
    timeout: Duration,
) -> Outcome {
    // A binding is checked on every call: a different store under the same
    // path is refused, never used silently.
    match store_id(&store.path) {
        Ok(id) if id == store.store_id => {}
        Ok(id) => {
            return Outcome::Invalid(format!(
                "mote_store_mismatch: {} is now {id}, but this board is bound to {}",
                store.path.display(),
                store.store_id
            ))
        }
        Err(e) => return Outcome::Failed(e.message),
    }
    run_raw(bin, Some(&store.path), actor, args, timeout)
}

fn run_raw(
    bin: &Path,
    store: Option<&Path>,
    actor: Option<&str>,
    args: &[&str],
    timeout: Duration,
) -> Outcome {
    let mut cmd = Command::new(bin);
    if let Some(store) = store {
        cmd.arg("--store").arg(store).arg("--json");
    }
    if let Some(actor) = actor {
        cmd.args(["--actor", actor]);
    }
    cmd.args(args)
        .env_remove("MOTE_ACTOR")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        // Its own process group, so a timeout can stop everything it started.
        .process_group(0);
    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => return Outcome::Failed(format!("cannot run {}: {e}", bin.display())),
    };
    let pgid = child.id();
    // Drain both pipes on threads so a chatty Mote cannot block on a full pipe.
    let drain = |mut pipe: Box<dyn Read + Send>| {
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            buf
        })
    };
    let stdout = drain(Box::new(child.stdout.take().expect("piped stdout")));
    let stderr = drain(Box::new(child.stderr.take().expect("piped stderr")));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if start.elapsed() >= timeout => break None,
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(e) => return Outcome::Failed(format!("waiting for mote: {e}")),
        }
    };
    let Some(status) = status else {
        // Stop and reap the whole group before anyone re-reads (section 1).
        kill_group(pgid);
        let _ = child.wait();
        return Outcome::Failed(format!(
            "mote {} timed out after {:?}",
            args.first().unwrap_or(&""),
            timeout
        ));
    };
    let out = String::from_utf8_lossy(&stdout.join().unwrap_or_default()).into_owned();
    let err = String::from_utf8_lossy(&stderr.join().unwrap_or_default()).into_owned();
    classify(status.code(), &out, &err)
}

fn kill_group(pgid: u32) {
    let _ = Command::new("/bin/sh")
        .args([
            "-c",
            r#"kill -s KILL -- "-$1""#,
            "fray-mote",
            &pgid.to_string(),
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

/// Section 4. Exit 2 is shared by reducer rejections and usage errors; only
/// `rejected` in stderr, or `accepted:false` in JSON, makes it a rejection.
/// Commands whose exit 2 is a result (`preflight`) use [`classify_result`].
pub fn classify(code: Option<i32>, stdout: &str, stderr: &str) -> Outcome {
    let parsed = parse(stdout);
    let reason = || {
        let line = stderr
            .lines()
            .rev()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .trim();
        line.to_owned()
    };
    match code {
        Some(0) => match parsed {
            Ok(v) => Outcome::Ok(v),
            Err(e) => Outcome::Failed(format!("unparseable mote output: {e}")),
        },
        Some(2) => {
            if let Ok(v) = &parsed {
                if v["accepted"] == false {
                    return Outcome::Rejected(
                        v["reason"].as_str().unwrap_or("rejected").to_owned(),
                    );
                }
            }
            if stderr.contains("rejected") {
                let r = reason();
                Outcome::Rejected(
                    r.split_once("rejected:")
                        .map_or(r.as_str(), |(_, why)| why)
                        .trim()
                        .to_owned(),
                )
            } else {
                Outcome::Invalid(reason())
            }
        }
        Some(3) => Outcome::Invalid(reason()),
        Some(code) => Outcome::Failed(format!("mote exited {code}: {}", reason())),
        None => Outcome::Failed("mote was killed by a signal".into()),
    }
}

/// For commands whose exit 2 is a result, not a failure (`preflight` reports
/// conflicts that way): exit 0 or 2 with parseable JSON is `Ok`.
pub fn classify_result(code: Option<i32>, stdout: &str, stderr: &str) -> Outcome {
    if code == Some(2) {
        if let Ok(v) = parse(stdout) {
            if !v.is_null() {
                return Outcome::Ok(v);
            }
        }
    }
    classify(code, stdout, stderr)
}

/// Mote prints one JSON document, several JSON lines (`events`), plain text
/// (`--version`), or nothing (commands without JSON output).
fn parse(stdout: &str) -> std::result::Result<Value, String> {
    let text = stdout.trim();
    if text.is_empty() {
        return Ok(Value::Null);
    }
    if let Ok(v) = serde_json::from_str::<Value>(text) {
        return Ok(v);
    }
    let lines: std::result::Result<Vec<Value>, _> = text
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(serde_json::from_str)
        .collect();
    match lines {
        Ok(lines) => Ok(Value::Array(lines)),
        Err(_) if !text.starts_with(['{', '[']) => Ok(Value::String(text.to_owned())),
        Err(e) => Err(e.to_string()),
    }
}
