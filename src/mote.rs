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
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

/// Bounded waits (section 1): reads degrade, mutations must be confirmed.
pub const READ_TIMEOUT: Duration = Duration::from_secs(10);
pub const MUTATION_TIMEOUT: Duration = Duration::from_secs(30);

/// The read timeout, overridable with `FRAY_MOTE_READ_TIMEOUT_MS` for a large
/// store on a slow disk, or for tests.
pub fn read_timeout() -> Duration {
    env::var("FRAY_MOTE_READ_TIMEOUT_MS")
        .ok()
        .and_then(|ms| ms.parse::<u64>().ok())
        .filter(|ms| (1..=600_000).contains(ms))
        .map_or(READ_TIMEOUT, Duration::from_millis)
}
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

/// Where this board's Mote store is (section 2), or None when this board is
/// not paired with one. Pairing follows the resolution that chose the board,
/// and only that:
///
/// - `MOTE_STORE`, if set, names the store for any board;
/// - a board `<root>/.fray` that is an ancestor of the working directory pairs
///   with `<root>/.mote`;
/// - the board this directory's own repository selects (`<git-common-dir>/fray`)
///   pairs with `.mote/` in that repository's main worktree.
///
/// Any other board (an explicit `--home` or `FRAY_HOME` elsewhere) is never
/// paired implicitly with whatever repository the shell is in. Bare
/// repositories and submodules must name their store with `MOTE_STORE`.
pub fn locate(board: &Path, cwd: &Path) -> Result<Option<PathBuf>> {
    if let Some(raw) = env::var_os("MOTE_STORE").filter(|v| !v.is_empty()) {
        let given = PathBuf::from(&raw);
        let dir = mote_dir(&if given.is_absolute() {
            given
        } else {
            cwd.join(given)
        });
        let named = |why: &str| {
            Error::new(
                "mote_store",
                format!(
                    "MOTE_STORE={} ({}): {why}",
                    raw.to_string_lossy(),
                    dir.display()
                ),
            )
        };
        let dir = fs::canonicalize(&dir).map_err(|e| named(&e.to_string()))?;
        if !dir.join("FORMAT.json").is_file() {
            return Err(named("not a Mote store (no FORMAT.json)"));
        }
        return Ok(Some(dir));
    }
    let canonical = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    let (board, cwd) = (canonical(board), canonical(cwd));
    let store = |root: &Path| Some(root.join(".mote")).filter(|p| p.join("FORMAT.json").is_file());
    if board.file_name().is_some_and(|n| n == ".fray") {
        if let Some(root) = board.parent().filter(|root| cwd.starts_with(root)) {
            return Ok(store(root));
        }
    }
    if crate::client::git_home(&cwd).map(|b| canonical(&b)) != Some(board) {
        return Ok(None);
    }
    let Some(list) = git(&cwd, &["worktree", "list", "--porcelain"]) else {
        return Ok(None);
    };
    // The first entry is the main worktree, or the bare repository itself;
    // this holds from any linked worktree, unlike --is-bare-repository.
    let main: Vec<&str> = list.lines().take_while(|l| !l.is_empty()).collect();
    let common = git(
        &cwd,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    );
    let submodule = common.as_deref().is_some_and(|dir| {
        let parts: Vec<_> = Path::new(dir).components().collect();
        parts
            .iter()
            .position(|c| c.as_os_str() == ".git")
            // Also under a linked worktree: .git/worktrees/<wt>/modules/<sub>.
            .is_some_and(|git| parts[git..].iter().any(|c| c.as_os_str() == "modules"))
    });
    if main.contains(&"bare") || submodule {
        return Err(Error::new(
            "mote_store_required",
            "a bare repository or submodule must name its Mote store with MOTE_STORE",
        ));
    }
    Ok(main
        .iter()
        .find_map(|l| l.strip_prefix("worktree "))
        .and_then(|root| store(Path::new(root))))
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
        .env_remove("MOTE_STORE")
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
    // A background process Mote left behind can hold them open after Mote
    // exits, so collecting them is bounded by the same deadline.
    let (tx, rx) = mpsc::channel();
    for (index, pipe) in [
        (
            0,
            Box::new(child.stdout.take().expect("piped stdout")) as Box<dyn Read + Send>,
        ),
        (1, Box::new(child.stderr.take().expect("piped stderr"))),
    ] {
        let tx = tx.clone();
        let mut pipe = pipe;
        thread::spawn(move || {
            let mut buf = Vec::new();
            let _ = pipe.read_to_end(&mut buf);
            let _ = tx.send((index, buf));
        });
    }
    drop(tx);
    let deadline = Instant::now() + timeout;
    let timed_out = |child: &mut std::process::Child| {
        // Stop and reap the whole group before anyone re-reads (section 1).
        kill_group(pgid);
        let _ = child.wait();
        Outcome::Failed(format!(
            "mote {} timed out after {:?}",
            args.first().unwrap_or(&""),
            timeout
        ))
    };
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => return timed_out(&mut child),
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(e) => return Outcome::Failed(format!("waiting for mote: {e}")),
        }
    };
    let mut pipes = [Vec::new(), Vec::new()];
    for _ in 0..2 {
        match rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
            Ok((index, buf)) => pipes[index] = buf,
            Err(_) => return timed_out(&mut child),
        }
    }
    let out = String::from_utf8_lossy(&pipes[0]).into_owned();
    let err = String::from_utf8_lossy(&pipes[1]).into_owned();
    match args.first().copied() {
        // `events` prints JSON lines: always an array, `[]` when empty.
        Some("events") if status.code() == Some(0) => match parse_lines(&out) {
            Ok(v) => Outcome::Ok(v),
            Err(e) => Outcome::Failed(format!("unparseable mote events: {e}")),
        },
        // `preflight` reports conflicts with exit 2: that is its result.
        Some("preflight") => classify_result(status.code(), &out, &err),
        _ => classify(status.code(), &out, &err),
    }
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

/// Section 4. Exit 2 is shared by reducer rejections and usage errors; only a
/// `rejected:` line in stderr, or `accepted:false` in JSON, makes it a
/// rejection. The transport uses [`classify_result`] for `preflight`, whose
/// exit 2 is a result.
pub fn classify(code: Option<i32>, stdout: &str, stderr: &str) -> Outcome {
    let parsed = parse(stdout);
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    // Clap ends a usage error with "For more information, try '--help'";
    // the message is the `error:` line.
    let message = || {
        lines
            .iter()
            .find(|l| l.starts_with("error"))
            .or(lines.first())
            .copied()
            .unwrap_or("")
            .to_owned()
    };
    let rejection = lines.iter().find_map(|l| {
        l.strip_prefix("rejected:")
            .or_else(|| l.split_once(" rejected: ").map(|(_, why)| why))
    });
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
            match rejection {
                Some(why) => Outcome::Rejected(why.trim().to_owned()),
                None => Outcome::Invalid(message()),
            }
        }
        Some(3) => Outcome::Invalid(message()),
        Some(code) => Outcome::Failed(format!("mote exited {code}: {}", message())),
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

/// One JSON document, plain text (`--version`), or nothing (commands without
/// JSON output print nothing on success).
fn parse(stdout: &str) -> std::result::Result<Value, String> {
    let text = stdout.trim();
    if text.is_empty() {
        return Ok(Value::Null);
    }
    match serde_json::from_str::<Value>(text) {
        Ok(v) => Ok(v),
        Err(_) if !text.starts_with(['{', '[']) => Ok(Value::String(text.to_owned())),
        Err(e) => Err(e.to_string()),
    }
}

/// JSON lines (`events`): always an array, whatever the count.
pub fn parse_lines(stdout: &str) -> std::result::Result<Value, String> {
    stdout
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|e| e.to_string()))
        .collect::<std::result::Result<Vec<Value>, _>>()
        .map(Value::Array)
}

/// The event categories a sync reads (section 6). Candidates join in the
/// reconciliation slice, where their landability is re-read.
pub const SYNC_KINDS: &str = "claim,reservation";

/// An op-id-shaped cursor for this instant. `events --after` compares ids as
/// strings, so seeding here skips history (section 6: start at the tail).
pub fn tail_cursor(now: std::time::SystemTime) -> String {
    let d = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let secs = d.as_secs() as i64;
    let (days, rem) = (secs.div_euclid(86_400), secs.rem_euclid(86_400));
    // Civil date from days since 1970-01-01 (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}{month:02}{day:02}T{:02}{:02}{:02}.{:06}Z",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60,
        d.subsec_micros()
    )
}

fn paths(data: &Value) -> String {
    data["paths"]
        .as_array()
        .map(|p| {
            p.iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        })
        .unwrap_or_default()
}

/// Which Mote events become attention, for whom (section 6). Each item is
/// keyed by the event id, so replaying an event yields no second card.
pub fn attention_items(events: &[Value]) -> Vec<Value> {
    let mut items = Vec::new();
    for e in events {
        let (Some(kind), Some(key)) = (e["type"].as_str(), e["event_id"].as_str()) else {
            continue;
        };
        let data = &e["data"];
        let entity = data["entity"].as_str().unwrap_or("");
        match kind {
            "reservation.expiring" | "reservation.expired" => {
                let Some(holder) = data["holder"].as_str().or(e["actor"].as_str()) else {
                    continue;
                };
                let expired = kind == "reservation.expired";
                items.push(serde_json::json!({
                    "key": key,
                    "recipient": holder,
                    "title": format!("Mote reservation {} on {}", if expired { "expired" } else { "expiring" }, paths(data)),
                    "summary": format!(
                        "Your Mote reservation {} for {entity} on {} {} at {}. Mote owns it: check with `mote who-has PATH`; renew or re-reserve through Mote if you still need the paths.",
                        data["reservation_id"].as_str().unwrap_or(""),
                        paths(data),
                        if expired { "expired" } else { "expires" },
                        data["deadline"].as_str().unwrap_or("its deadline")
                    ),
                    "priority": 1,
                    "refs": [entity],
                }));
            }
            "claim.acquired" => {
                let (Some(actor), Some(to)) = (e["actor"].as_str(), data["to"].as_str()) else {
                    continue;
                };
                // Claiming for oneself is not news; a claim given to someone
                // else is a handoff they need to hear about.
                if actor == to {
                    continue;
                }
                items.push(serde_json::json!({
                    "key": key,
                    "recipient": to,
                    "title": format!("Mote: {actor} handed you {entity}"),
                    "summary": format!(
                        "{actor} transferred the Mote claim on {entity} to you. Mote owns the claim: `mote show {entity}` for the work, and look for a Fray handoff packet from {actor}."
                    ),
                    "priority": 1,
                    "refs": [entity],
                }));
            }
            _ => {}
        }
    }
    items
}
