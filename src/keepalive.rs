//! Keepalive (docs/design/keepalive.md, slice K1): the daemon keeps an
//! interactive agent answerable after its turn ends by starting a detached
//! `fray drive --keepalive` under the agent's name. This is the board's side:
//! the record of what was started for whom, the companion session it may
//! bind, stop requests, the daily input-token budget, and the fixed spawn;
//! and (K2) the terminal's turns as its hooks report them, so the keepalive
//! and the terminal take turns, and what the keepalive did, so the terminal
//! hears of it at its next prompt.
//!
//! Experimental and not yet announced to agents (K3 changes the skill).
use crate::model::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    fs::{self, OpenOptions},
    os::unix::{
        fs::{OpenOptionsExt, PermissionsExt},
        process::CommandExt,
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
};

/// The session label a keepalive drive speaks with: `keepalive:CONVERSATION`.
pub const PREFIX: &str = "keepalive:";
/// Daily input tokens when the daemon's start environment sets none.
pub const DAILY_TOKENS: i64 = 2_000_000;
/// The owner's override, read from the daemon's start environment only.
pub const BUDGET_ENV: &str = "FRAY_KEEPALIVE_DAILY_TOKENS";
pub const MODEL_ENV: &str = "FRAY_KEEPALIVE_MODEL";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartOptions {
    pub model: Option<String>,
    pub budget: i64,
}

/// Owner-controlled start policy. Only the current pinned owner Team card can
/// override daemon-start defaults; duplicate or malformed explicit lines fail
/// closed rather than selecting an arbitrary value.
pub fn start_options(conn: &Connection, _now: i64) -> Result<StartOptions> {
    let fallback_budget = std::env::var(BUDGET_ENV)
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|v: &i64| *v > 0)
        .unwrap_or(DAILY_TOKENS);
    let fallback_model = std::env::var(MODEL_ENV).ok().filter(|v| !v.is_empty());
    let summary: Option<String> = conn.query_row(
        "SELECT summary FROM cards WHERE author='owner' AND pinned=1 AND title='Team' AND status NOT IN ('resolved','superseded','withdrawn') ORDER BY id DESC LIMIT 1", [], |r| r.get(0)
    ).optional()?;
    let mut model = None;
    let mut budget = None;
    for line in summary.as_deref().unwrap_or("").lines() {
        if let Some(value) = line.trim().strip_prefix("keepalive model:") {
            if model.replace(value.trim().to_owned()).is_some() {
                return Err(Error::invalid(
                    "duplicate keepalive model option in Team card",
                ));
            }
        }
        if let Some(value) = line.trim().strip_prefix("keepalive budget:") {
            if budget.replace(value.trim().to_owned()).is_some() {
                return Err(Error::invalid(
                    "duplicate keepalive budget option in Team card",
                ));
            }
        }
    }
    let model = match model {
        Some(value)
            if value.is_empty() || value.len() > 128 || value.chars().any(char::is_control) =>
        {
            return Err(Error::invalid(
                "keepalive model must be 1..128 non-control characters",
            ))
        }
        Some(value) => Some(value),
        None => fallback_model,
    };
    let budget = match budget {
        Some(value) => value
            .parse::<i64>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or_else(|| Error::invalid("keepalive budget must be a positive integer"))?,
        None => fallback_budget,
    };
    Ok(StartOptions { model, budget })
}
/// How long a started keepalive counts as starting before its drive begins.
pub(crate) const STARTING_MS: i64 = 20_000;
/// A controller not refreshed for this long is not live (as for drives).
pub(crate) const CONTROLLER_TTL_MS: i64 = 120_000;
const DAY_MS: i64 = 86_400_000;
/// A busy mark with no hook activity for this long is stale: an interrupt
/// that no hook event reported.
pub const BUSY_STALE_MS: i64 = 30 * 60_000;
/// Hook events that begin, continue or end an interactive terminal's turn
/// (`terminal_turn`'s `turn`).
pub const TURN_MARKS: [&str; 3] = ["begin", "active", "end"];
/// The only command the daemon runs, after its own executable. Nothing in a
/// request reaches it: the name, home and session go in the environment.
pub const DRIVE_ARGS: [&str; 2] = ["drive", "--keepalive"];
/// The run a keepalive carries into its daemon's binary when it re-executes
/// itself after a restart ("Across a daemon restart"). Only that drive's own
/// exec sets it; the drive takes it out of its environment at once, and no
/// daemon or drive Fray starts inherits it.
pub const HANDOFF_ENV: &str = "FRAY_KEEPALIVE_HANDOFF";

pub fn is_session(session: Option<&str>) -> bool {
    session.is_some_and(|s| s.starts_with(PREFIX))
}

/// An id a host gave a conversation, safe to pass as one argument: ASCII
/// letters, digits and '-', starting with a letter or digit (never a flag).
pub fn conversation_id(id: &str) -> bool {
    (1..=100).contains(&id.len())
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        && id.as_bytes()[0].is_ascii_alphanumeric()
}

/// The host and conversation of an interactive terminal's session.
pub fn companion(session: &str) -> Result<(&str, &str)> {
    let (host, id) = session
        .split_once(':')
        .filter(|(host, _)| matches!(*host, "claude" | "codex"))
        .ok_or_else(|| {
            Error::new(
                "keepalive_host",
                "a keepalive serves an interactive Claude Code or Codex session (claude:ID or codex:ID, from CLAUDE_CODE_SESSION_ID or CODEX_THREAD_ID); for other hosts run `fray drive`",
            )
        })?;
    if !conversation_id(id) {
        return Err(Error::invalid(
            "the session's conversation id must be 1..100 ASCII letters, digits or '-', starting with a letter or digit",
        ));
    }
    Ok((host, id))
}

/// Where a keepalive's drive writes its output: `HOME/keepalive/NAME.log`.
/// Agent names may contain '/', which cannot appear in a file name.
pub fn file(home: &Path, agent: &str, extension: &str) -> PathBuf {
    home.join("keepalive")
        .join(format!("{}.{extension}", agent.replace('/', "%2F")))
}

fn today(now: i64) -> i64 {
    now.div_euclid(DAY_MS)
}

/// Whether this daemon runs inside a seatbelt sandbox: a nested sandbox is
/// refused there and allowed outside. `CODEX_SANDBOX` is not trusted (it can
/// be unset). Without sandbox-exec (not macOS) there is no seatbelt.
pub fn sandboxed() -> bool {
    // Tests force either answer; release builds never read this.
    #[cfg(debug_assertions)]
    if let Ok(forced) = std::env::var("FRAY_TEST_SANDBOXED") {
        return forced == "1";
    }
    let probe = Path::new("/usr/bin/sandbox-exec");
    if !probe.exists() {
        return false;
    }
    !Command::new(probe)
        .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

fn git_common_dir(dir: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--git-common-dir"])
        .env_remove("GIT_DIR")
        .env_remove("GIT_COMMON_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    fs::canonicalize(if path.is_absolute() {
        path
    } else {
        dir.join(path)
    })
    .ok()
}

/// The terminal's directory, canonical, if it is this board's repository or
/// one of its git worktrees (they share the repository's common directory).
/// A resumed conversation runs, and is filed, under its working directory.
pub fn repository_dir(home: &Path, cwd: &Path) -> Result<PathBuf> {
    let outside = || {
        Error::new(
            "outside_repository",
            format!(
                "{} is not this board's repository or one of its git worktrees; run fray keepalive from the terminal's own working directory",
                cwd.display()
            ),
        )
    };
    if !cwd.is_absolute() {
        return Err(Error::invalid("cwd must be an absolute directory"));
    }
    let cwd = fs::canonicalize(cwd).map_err(|_| outside())?;
    if !cwd.is_dir() {
        return Err(outside());
    }
    let board = home
        .parent()
        .and_then(git_common_dir)
        .ok_or_else(|| {
            Error::new(
                "outside_repository",
                "this board's home is not inside a git repository, so no directory can be checked against it",
            )
        })?;
    match git_common_dir(&cwd) {
        Some(common) if common == board => Ok(cwd),
        _ => Err(outside()),
    }
}

/// Whether a process exists (signal 0 through the shell's `kill`, keeping
/// the crate free of unsafe code).
pub fn pid_alive(pid: i64) -> bool {
    pid > 0
        && Command::new("/bin/sh")
            .args([
                "-c",
                r#"kill -0 "$1" 2>/dev/null"#,
                "fray",
                &pid.to_string(),
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
}

/// How often a daemon checks that a drive it adopted still runs.
const ADOPTED_POLL: std::time::Duration = std::time::Duration::from_secs(5);

/// Drives an earlier daemon started that still run: they outlive a restart
/// (docs/design/keepalive.md, "Across a daemon restart"), and the new daemon
/// knows them only by their recorded pid.
pub fn adoptable(conn: &Connection) -> Result<Vec<(String, i64)>> {
    let mut s = conn.prepare("SELECT agent,pid FROM keepalives WHERE pid IS NOT NULL")?;
    let rows = s
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<(String, i64)>>>()?;
    Ok(rows
        .into_iter()
        .filter(|(_, pid)| pid_alive(*pid))
        .collect())
}

/// Watch an adopted drive and log its exit, as the starting daemon's reaper
/// would. It is not this daemon's child, so launchd or init reaps it.
pub fn watch(agent: String, pid: i64) {
    std::thread::spawn(move || {
        while pid_alive(pid) {
            std::thread::sleep(ADOPTED_POLL);
        }
        eprintln!("keepalive drive for {agent:?} (pid {pid}) exited");
    });
}

/// Start the keepalive's drive, detached: its own process group, stdin
/// closed, output appended to its log, and an explicit `FRAY_SESSION`. The
/// command is fixed; only the environment names whom it serves.
pub fn spawn(home: &Path, agent: &str, session: &str, cwd: &Path, log: &Path) -> Result<Child> {
    if let Some(dir) = log.parent() {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    let out = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(log)?;
    Ok(Command::new(std::env::current_exe()?)
        .args(DRIVE_ARGS)
        .env("FRAY_HOME", home)
        .env("FRAY_AGENT", agent)
        .env("FRAY_SESSION", session)
        // The daemon may have been started inside a host session; the drive
        // speaks only as its keepalive session.
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("CODEX_THREAD_ID")
        // Another drive's run is never this one's.
        .env_remove(HANDOFF_ENV)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(out.try_clone()?))
        .stderr(Stdio::from(out))
        .process_group(0)
        .spawn()?)
}

struct Record {
    session: String,
    companion: String,
    host: String,
    cwd: String,
    log: String,
    pid: Option<i64>,
    budget: i64,
    usage_day: i64,
    usage_tokens: i64,
    stop: bool,
    started: i64,
}

fn record(conn: &Connection, agent: &str) -> Result<Option<Record>> {
    Ok(conn
        .query_row(
            "SELECT session,companion,host,cwd,log,pid,budget,usage_day,usage_tokens,stop_requested,started_ms FROM keepalives WHERE agent=?",
            [agent],
            |r| {
                Ok(Record {
                    session: r.get(0)?,
                    companion: r.get(1)?,
                    host: r.get(2)?,
                    cwd: r.get(3)?,
                    log: r.get(4)?,
                    pid: r.get(5)?,
                    budget: r.get(6)?,
                    usage_day: r.get(7)?,
                    usage_tokens: r.get(8)?,
                    stop: r.get(9)?,
                    started: r.get(10)?,
                })
            },
        )
        .optional()?)
}

/// Whether `session` is the keepalive session the daemon started for
/// `agent`: only that one may bind beside the agent's interactive session.
pub(crate) fn authorized(conn: &Connection, agent: &str, session: &str) -> Result<bool> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM keepalives WHERE agent=? AND session=?)",
        params![agent, session],
        |r| r.get(0),
    )?)
}

pub(crate) fn stop_requested(conn: &Connection, agent: &str, session: &str) -> Result<bool> {
    Ok(conn
        .query_row(
            "SELECT stop_requested FROM keepalives WHERE agent=? AND session=?",
            params![agent, session],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// What the keepalive for `agent` is doing, and why not, if it is not.
pub fn status(conn: &Connection, agent: &str, now: i64) -> Result<Value> {
    let Some(k) = record(conn, agent)? else {
        return Ok(json!({"agent":agent,"state":"off"}));
    };
    let controller: Option<(String, i64, Option<String>, Option<Value>)> = conn
        .query_row(
            "SELECT c.state,c.updated_ms,c.reason,d.detail FROM controllers c LEFT JOIN controller_details d ON d.agent=c.agent AND d.run_id=c.run_id WHERE c.agent=?",
            [agent],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get::<_, Option<String>>(3)?
                        .and_then(|d| serde_json::from_str(&d).ok()),
                ))
            },
        )
        .optional()?;
    // The controller is this keepalive's when its run began after the start
    // and its detail names this keepalive session.
    let ours = controller.as_ref().filter(|(_, updated, _, detail)| {
        *updated >= k.started
            && detail
                .as_ref()
                .is_some_and(|d| d["keepalive"]["session"] == k.session.as_str())
    });
    let detail = ours.and_then(|c| c.3.clone()).unwrap_or(Value::Null);
    let keep = &detail["keepalive"];
    let live = ours.is_some_and(|(state, updated, _, _)| {
        matches!(state.as_str(), "waiting" | "running") && now - updated < CONTROLLER_TTL_MS
    });
    let terminal = terminal(conn, agent, &k.companion, now)?;
    let busy = terminal["busy"] == true;
    let state = if live {
        if k.stop {
            "stopping"
        } else if !keep["paused"].is_null() {
            "paused"
        } else if busy {
            // The terminal's turn comes first (docs/design/keepalive.md).
            "deferred"
        } else {
            "keepalive"
        }
    } else if ours.is_none() && !k.stop && now - k.started < STARTING_MS {
        "starting"
    } else if ours.is_some_and(|c| c.0 == "failed") {
        "failed"
    } else {
        "stopped"
    };
    let used = if k.usage_day == today(now) {
        k.usage_tokens
    } else {
        0
    };
    let model: Option<String> = conn
        .query_row(
            "SELECT model FROM keepalive_options WHERE agent=?",
            [agent],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten();
    Ok(json!({
        "agent":agent,"state":state,
        "turn":if live {ours.map(|c| c.0.clone())} else {None},
        "host":k.host,"companion":k.companion,"session":k.session,"cwd":k.cwd,"log":k.log,"model":model,
        "pid":k.pid,"started_ms":k.started,"stop_requested":k.stop,
        "fork":keep["fork"],"turns":detail["turn"],"paused":keep["paused"],
        // The build and binary the drive runs, as it reports them.
        "drive":{"build":detail["build"],"exe":detail["exe"]},
        "deferred":if live && busy {json!("terminal busy")} else {Value::Null},"terminal":terminal,
        "summary":keep["summary"],"failures":keep["failures"],"oversized":keep["oversized"],
        "reason":if live {None} else {ours.and_then(|c| c.2.clone())},
        "usage":{"day":today(now),"input_tokens":used,"budget":k.budget,"over_budget":used>=k.budget},
    }))
}

/// The roster's view: present only while a keepalive is starting, running,
/// stopping, paused or deferred.
pub(crate) fn brief(conn: &Connection, agent: &str, now: i64) -> Result<Value> {
    let s = status(conn, agent, now)?;
    Ok(
        if matches!(s["state"].as_str(), Some("off" | "stopped" | "failed")) {
            Value::Null
        } else {
            // Session ids are clipped for display, as everywhere in the roster.
            json!({"state":s["state"],"host":s["host"],"companion":clip(s["companion"].as_str().unwrap_or(""), 20),"paused":s["paused"],"deferred":s["deferred"],"pid":s["pid"]})
        },
    )
}

/// Validate a start request (bound and checked by the caller) and record it.
/// The host and conversation come from the request's bound session, never
/// from its fields. Returns the status, with `already_running` when a
/// keepalive already serves this name.
pub(crate) fn begin(
    conn: &Connection,
    req: &Request,
    cwd: &str,
    log: &str,
    budget: i64,
    now: i64,
) -> Result<Value> {
    let options = start_options(conn, now)?;
    let agent = req.actor.as_str();
    let session = req.session.as_deref().ok_or_else(|| {
        Error::new(
            "session_required",
            "fray keepalive needs the terminal's host session (CLAUDE_CODE_SESSION_ID or CODEX_THREAD_ID); for other hosts run `fray drive`",
        )
    })?;
    if is_session(Some(session)) {
        return Err(Error::new(
            "keepalive_host",
            "a keepalive cannot start another keepalive",
        ));
    }
    let (host, conversation) = companion(session)?;
    let current = status(conn, agent, now)?;
    match current["state"].as_str() {
        Some("starting" | "keepalive" | "paused" | "deferred") => {
            let mut current = current;
            current["already_running"] = json!(true);
            return Ok(current);
        }
        Some("stopping") => {
            return Err(Error::new(
                "keepalive_stopping",
                "this name's keepalive is stopping (its turn in progress finishes first); start it again once fray keepalive --status says stopped",
            ))
        }
        _ => {}
    }
    // A live listener or controller already owns this identity's wake. A
    // Claude whose Monitor is armed is wakeable already; a second controller
    // would fail with controller_busy.
    let listener: Option<String> = conn
        .query_row(
            "SELECT selection FROM listeners WHERE agent=? AND connected=1 AND updated_ms>?",
            params![agent, now - crate::attention::LISTENER_TTL_MS],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(selection) = listener {
        let selection: Value = serde_json::from_str(&selection).unwrap_or(Value::Null);
        if selection["activation"]["mode"] == "native-monitor" {
            return Err(Error::new(
                "monitor_armed",
                format!("{agent:?} has an armed Monitor, so it is already wakeable; a keepalive would be a second controller (controller_busy). Keep the Monitor, or stop it before fray keepalive"),
            ));
        }
        return Err(Error::new(
            "controller_busy",
            "a live listener already owns this identity's wake; stop it before fray keepalive",
        ));
    }
    let driven: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM controllers WHERE agent=? AND state IN ('waiting','running') AND updated_ms>?)",
        params![agent, now - CONTROLLER_TTL_MS],
        |r| r.get(0),
    )?;
    if driven {
        return Err(Error::new(
            "controller_busy",
            "a live drive already owns this identity; a keepalive would be a second controller",
        ));
    }
    // Without hooks the keepalive cannot see the terminal's turns, so it
    // could answer behind a terminal that is working. The turn that runs
    // `fray keepalive` began with a prompt; its UserPromptSubmit hook must
    // have reached the board from this very session.
    if terminal(conn, agent, session, now)?["prompts"]
        .as_i64()
        .unwrap_or(0)
        == 0
    {
        let events = if host == "codex" {
            "UserPromptSubmit, Stop, Interrupt and SessionStart"
        } else {
            "UserPromptSubmit, Stop, StopFailure, SessionEnd and SessionStart"
        };
        return Err(Error::new(
            "hooks_missing",
            format!("no prompt from this session has reached the board through `fray hook`, so a keepalive could not tell when the terminal is working. Install `fray hook --host {host}` for {events} (README, hooks), start a new session or turn, and run fray keepalive again"),
        ));
    }
    let label = format!("{PREFIX}{conversation}");
    // A restart keeps the day's usage: the budget is per name and day.
    conn.execute(
        "INSERT INTO keepalives(agent,session,companion,host,cwd,log,pid,budget,stop_requested,started_ms) VALUES(?1,?2,?3,?4,?5,?6,NULL,?7,0,?8) ON CONFLICT(agent) DO UPDATE SET session=excluded.session,companion=excluded.companion,host=excluded.host,cwd=excluded.cwd,log=excluded.log,pid=NULL,budget=excluded.budget,stop_requested=0,started_ms=excluded.started_ms",
        params![agent, label, session, host, cwd, log, budget, now],
    )?;
    conn.execute(
        "INSERT INTO keepalive_options(agent,model) VALUES(?,?) ON CONFLICT(agent) DO UPDATE SET model=excluded.model",
        params![agent, options.model],
    )?;
    status(conn, agent, now)
}

pub(crate) fn spawned(conn: &Connection, agent: &str, pid: u32) -> Result<()> {
    conn.execute(
        "UPDATE keepalives SET pid=? WHERE agent=?",
        params![pid, agent],
    )?;
    Ok(())
}

/// The drive could not be started: the record no longer counts as starting.
pub(crate) fn abort(conn: &Connection, agent: &str, now: i64) -> Result<()> {
    conn.execute(
        "UPDATE keepalives SET stop_requested=1,started_ms=min(started_ms,?) WHERE agent=?",
        params![now - STARTING_MS, agent],
    )?;
    Ok(())
}

/// A stop request on the keepalive: a turn in progress finishes, then the
/// drive exits instead of waiting again.
pub(crate) fn stop(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    check_fields(&req.args, &[])?;
    let changed = conn.execute(
        "UPDATE keepalives SET stop_requested=1 WHERE agent=?",
        [&req.actor],
    )?;
    if changed == 0 {
        return Err(Error::new(
            "not_found",
            format!("{:?} has no keepalive", req.actor),
        ));
    }
    status(conn, &req.actor, now)
}

/// Input tokens a keepalive turn read, as its host reported them, added to
/// the day's count. Only the keepalive's own session reports usage.
pub(crate) fn usage(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    check_fields(&req.args, &["input_tokens"])?;
    let tokens = bounded(&req.args, "input_tokens", 0, 0, 1_000_000_000)?;
    let session = req.session.as_deref().unwrap_or("");
    if !is_session(Some(session)) || !authorized(conn, &req.actor, session)? {
        return Err(Error::new(
            "keepalive_session",
            "only the keepalive's own drive reports its usage",
        ));
    }
    conn.execute(
        "UPDATE keepalives SET usage_tokens=CASE WHEN usage_day=?1 THEN usage_tokens+?2 ELSE ?2 END,usage_day=?1 WHERE agent=?3",
        params![today(now), tokens, req.actor],
    )?;
    Ok(status(conn, &req.actor, now)?["usage"].clone())
}

/// Atomically fence a background turn against a terminal prompt. The claimed
/// receipts are durable before the driver forks a host process, so a prompt
/// that wins the race sees them as already owned by the keepalive.
pub(crate) fn claim(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    check_fields(
        &req.args,
        &["run_id", "detail", "companion", "prompt_generation"],
    )?;
    let session = req
        .session
        .as_deref()
        .filter(|session| is_session(Some(session)))
        .ok_or_else(|| {
            Error::new(
                "keepalive_session",
                "keepalive claim requires its own session",
            )
        })?;
    let agent = req.actor.as_str();
    let companion: String = conn
        .query_row(
            "SELECT companion FROM keepalives WHERE agent=? AND session=? AND stop_requested=0",
            params![agent, session],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| Error::new("keepalive_stopped", "keepalive no longer runs"))?;
    if req.args["companion"].as_str() != Some(companion.as_str()) {
        return Err(Error::new(
            "terminal_changed",
            "terminal conversation changed; rebuild the keepalive packet",
        ));
    }
    let terminal = terminal(conn, agent, &companion, now)?;
    if terminal["busy"] == true {
        return Err(Error::new("terminal_busy", "terminal busy"));
    }
    if req.args["prompt_generation"].as_i64() != terminal["prompts"].as_i64() {
        return Err(Error::new(
            "terminal_changed",
            "terminal prompt generation changed; rebuild the keepalive packet",
        ));
    }
    let run_id = string(&req.args, "run_id")?;
    let detail = req.args["detail"].clone();
    if !detail.is_object() {
        return Err(Error::invalid("detail must be an object"));
    }
    let updated = conn.execute(
        "UPDATE controllers SET state='running',updated_ms=?,reason=NULL WHERE agent=? AND run_id=? AND state='waiting' AND updated_ms>?",
        params![now, agent, run_id, now - CONTROLLER_TTL_MS],
    )?;
    if updated == 0 {
        return Err(Error::new(
            "controller_lost",
            "keepalive controller is no longer waiting",
        ));
    }
    conn.execute(
        "INSERT INTO controller_details(agent,run_id,detail) VALUES(?,?,?) ON CONFLICT(agent) DO UPDATE SET run_id=excluded.run_id,detail=excluded.detail",
        params![agent, run_id, serde_json::to_string(&detail)?],
    )?;
    Ok(json!({"claimed":true,"companion":companion}))
}

/// The terminal's turn state for its host `session`, from its hooks: busy
/// from UserPromptSubmit until its turn ends, unless stale.
pub(crate) fn terminal(conn: &Connection, agent: &str, session: &str, now: i64) -> Result<Value> {
    let row: Option<(Option<i64>, i64, i64, Option<String>)> = conn
        .query_row(
            "SELECT busy_since_ms,last_hook_ms,prompts,transcript FROM terminal_turns WHERE agent=? AND session=?",
            params![agent, session],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((since, last, prompts, transcript)) = row else {
        return Ok(json!({"session":session,"busy":false,"prompts":0}));
    };
    let stale = since.is_some() && now - last >= BUSY_STALE_MS;
    Ok(
        json!({"session":session,"busy":since.is_some() && !stale,"busy_since_ms":since,
        "stale":stale,"last_hook_ms":last,"prompts":prompts,"transcript":transcript}),
    )
}

/// `terminal_turn`: a hook in the interactive terminal reports its turn
/// beginning (UserPromptSubmit), continuing (any other hook, a blocking
/// Stop) or ending (a Stop that does not block, StopFailure, SessionEnd,
/// Codex's Interrupt). Never from a keepalive's own session. Answers what
/// the terminal must hear: cards its keepalive is handling right now, and,
/// when a turn begins, what the keepalive did since the terminal's last turn
/// (each reported once).
pub(crate) fn mark(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    check_fields(&req.args, &["turn", "transcript", "report_away"])?;
    let turn = string(&req.args, "turn")?;
    if !TURN_MARKS.contains(&turn) {
        return Err(Error::invalid("turn: begin|active|end"));
    }
    let transcript = req
        .args
        .get("transcript")
        .filter(|t| !t.is_null())
        .map(|_| string(&req.args, "transcript"))
        .transpose()?;
    if let Some(path) = transcript {
        text(path, "transcript", 4096, false)?;
    }
    let session = req.session.as_deref().ok_or_else(|| {
        Error::new(
            "session_required",
            "a terminal's turns are recorded per host session",
        )
    })?;
    if is_session(Some(session)) {
        return Err(Error::new(
            "keepalive_session",
            "a keepalive's own turns are not its terminal's",
        ));
    }
    let agent = req.actor.as_str();
    conn.execute(
        "INSERT INTO terminal_turns(agent,session,busy_since_ms,last_hook_ms,prompts,transcript) VALUES(?1,?2,CASE WHEN ?3='begin' THEN ?4 END,?4,?3='begin',?5) ON CONFLICT(agent,session) DO UPDATE SET busy_since_ms=CASE ?3 WHEN 'begin' THEN coalesce(busy_since_ms,?4) WHEN 'end' THEN NULL ELSE busy_since_ms END,last_hook_ms=?4,prompts=prompts+(?3='begin'),transcript=coalesce(?5,transcript)",
        params![agent, session, turn, now, transcript],
    )?;
    let mut out = json!({"turn":turn,"handling":handling(conn, agent, now)?});
    if turn == "begin" {
        out["away"] = away(conn, agent, false)?;
    } else if let Some(seqs) = req.args["report_away"].as_array() {
        report_away(conn, agent, seqs)?;
    }
    Ok(out)
}

/// Cards the keepalive's background turn is handling right now: those
/// presented to its running turn. Empty when no turn runs.
pub(crate) fn handling(conn: &Connection, agent: &str, now: i64) -> Result<Vec<i64>> {
    let detail: Option<String> = conn.query_row(
        "SELECT d.detail FROM controllers c JOIN controller_details d ON d.agent=c.agent AND d.run_id=c.run_id JOIN keepalives k ON k.agent=c.agent WHERE c.agent=? AND c.state='running' AND c.updated_ms>? AND k.session=json_extract(d.detail,'$.keepalive.session')",
        params![agent, now - CONTROLLER_TTL_MS], |r| r.get(0),
    ).optional()?;
    let detail: Value = detail
        .and_then(|d| serde_json::from_str(&d).ok())
        .unwrap_or(Value::Null);
    Ok(detail["presented"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p["id"].as_i64())
        .collect())
}

/// What the keepalive posted since the terminal was last told, oldest first.
/// Mark exactly the returned rows reported; excess remains for the next prompt.
fn away(conn: &Connection, agent: &str, _report: bool) -> Result<Value> {
    let mut s = conn.prepare(
        "SELECT event_seq,card_id,kind,follow_up FROM keepalive_actions WHERE agent=? AND reported=0 ORDER BY event_seq LIMIT 20",
    )?;
    let rows = s.query_map([agent], |r| {
        Ok(json!({"seq":r.get::<_,i64>(0)?,"card":r.get::<_,i64>(1)?,"kind":r.get::<_,String>(2)?,"follow_up":r.get::<_,Option<i64>>(3)?}))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    let total: i64 = conn.query_row(
        "SELECT count(*) FROM keepalive_actions WHERE agent=? AND reported=0",
        [agent],
        |r| r.get(0),
    )?;
    Ok(json!({"actions":rows,"more":total - rows.len() as i64}))
}

fn report_away(conn: &Connection, agent: &str, seqs: &[Value]) -> Result<()> {
    if seqs.len() > 20 {
        return Err(Error::invalid(
            "report_away may contain at most 20 action IDs",
        ));
    }
    for seq in seqs {
        let seq = seq
            .as_i64()
            .ok_or_else(|| Error::invalid("report_away IDs must be integers"))?;
        let changed = conn.execute(
            "UPDATE keepalive_actions SET reported=1 WHERE event_seq=? AND agent=? AND reported=0",
            params![seq, agent],
        )?;
        if changed != 1 {
            return Err(Error::new(
                "away_changed",
                "a presented keepalive action was already reported or belongs to another agent",
            ));
        }
    }
    Ok(())
}

/// A keepalive's reply, recorded for its terminal's "while you were away".
pub(crate) fn acted(conn: &Connection, req: &Request, result: &Value) -> Result<()> {
    let session = req.session.as_deref();
    if req.op != "annotate" || !is_session(session) {
        return Ok(());
    }
    let (Some(seq), Some(card)) = (result["event_seq"].as_i64(), req.args["id"].as_i64()) else {
        return Ok(());
    };
    conn.execute(
        "INSERT OR IGNORE INTO keepalive_actions(event_seq,agent,card_id,kind,follow_up) VALUES(?,?,?,?,?)",
        params![seq, req.actor, card, req.args["kind"].as_str().unwrap_or("note"), result["follow_up"]["id"].as_i64()],
    )?;
    Ok(())
}

/// A terminal `/clear` or compaction starts a new conversation: move its
/// keepalive to that companion so its next background turn forks the new one.
pub(crate) fn rebind(conn: &Connection, agent: &str, session: &str) -> Result<()> {
    let Ok((host, _)) = companion(session) else {
        return Ok(());
    };
    conn.execute(
        "UPDATE keepalives SET companion=?1 WHERE agent=?2 AND host=?3 AND companion<>?1",
        params![session, agent, host],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_conversation_id_is_one_safe_argument() {
        assert!(conversation_id("0193a1b2-7c3d-7e4f-8a9b-0c1d2e3f4a5b"));
        assert!(!conversation_id("-x"));
        assert!(!conversation_id("--dangerously-skip-permissions"));
        assert!(!conversation_id("a b"));
        assert!(!conversation_id("a;b"));
        assert!(!conversation_id(""));
        assert!(!conversation_id(&"a".repeat(101)));
    }

    #[test]
    fn only_claude_and_codex_sessions_have_companions() {
        assert_eq!(companion("claude:abc-1").unwrap(), ("claude", "abc-1"));
        assert_eq!(companion("codex:t-2").unwrap(), ("codex", "t-2"));
        assert_eq!(companion("fray:k").unwrap_err().code, "keepalive_host");
        assert_eq!(companion("test:w").unwrap_err().code, "keepalive_host");
        assert_eq!(companion("claude:-p").unwrap_err().code, "invalid");
    }

    #[test]
    fn a_log_name_never_leaves_the_keepalive_directory() {
        let home = Path::new("/h");
        assert_eq!(
            file(home, "a/../b", "log"),
            Path::new("/h/keepalive/a%2F..%2Fb.log")
        );
        assert_eq!(file(home, "..", "log"), Path::new("/h/keepalive/...log"));
    }
}
