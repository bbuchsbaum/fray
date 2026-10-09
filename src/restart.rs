//! `fray restart` (docs/plans/2026-10-08-daemon-lifecycle.md): the preflight
//! (L5), what a restart of one home's daemon would interrupt and whether it
//! may go ahead, and the restart itself (L9, [`run`]).
//!
//! The preflight is read-only. It sends only `ping` and then `occupancy` (or,
//! on a daemon without that capability, `agents`); neither joins, binds a
//! session nor records presence.
//!
//! # Three classes
//!
//! - **Live**: something a restart cuts off now. An open long-lived
//!   connection (a `wait`, `watch` or listener stream), a live listener, a
//!   running or waiting drive, or a live keepalive process mid-turn.
//! - **Survives**: a live keepalive drive (with a recorded pid) whose drive
//!   is not running a turn, with its drive and stream. The replacement
//!   adopts it and it moves onto the new binary between turns (L8), so it
//!   never blocks a restart.
//! - **Armed**: a `session_waits` row with no open connection behind it. An
//!   agent's wait loop between two calls looks like this, and so does a
//!   finite `fray wait` that already returned, for up to `WAIT_FRESH_MS`
//!   (150 s) afterwards. Nothing is cut off; an agent that calls again while
//!   the daemon is down sees it briefly unavailable.
//!
//! A restart with only armed waits is not idle and not busy: the verdict is
//! `armed`, with its own exit code. It blocks by default because the
//! preflight cannot tell a loop between calls from a wait that has finished;
//! a caller that accepts a short gap for those agents may proceed on it.
//! `occupancy` itself still counts armed waits as `busy`; this split is the
//! preflight's reading of it, not a change to it.
//!
//! # Old daemons
//!
//! A daemon without the `occupancy` capability (0.2.1 and earlier) is read
//! through its `agents` listing and the connection counts in `ping`, and the
//! result says [`DEGRADED`]. Live listeners, drives, keepalives and any open
//! long-lived connection (counted, not named) make it `busy`; an agent seen
//! within two minutes or waiting, which that listing cannot separate, makes
//! it `armed`; otherwise it is `degraded_idle`, never plain `idle`.
use crate::model::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// The label on every preflight read without `occupancy`.
pub const DEGRADED: &str = "daemon predates occupancy; watchers and waits not visible";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Nothing would be interrupted (exit 0).
    Idle,
    /// Something live would be interrupted (exit 3).
    Busy,
    /// Only armed waits, or on an old daemon recent activity (exit 5).
    Armed,
    /// An old daemon showing nothing live; waits could not be seen (exit 6).
    DegradedIdle,
    /// No daemon answers on this home (exit 7).
    NotRunning,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Idle => "idle",
            Self::Busy => "busy",
            Self::Armed => "armed",
            Self::DegradedIdle => "degraded_idle",
            Self::NotRunning => "not_running",
        }
    }

    /// The `fray restart --dry-run` exit status.
    pub fn exit_code(self) -> i32 {
        match self {
            Self::Idle => 0,
            Self::Busy => 3,
            Self::Armed => 5,
            Self::DegradedIdle => 6,
            Self::NotRunning => 7,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Live,
    Armed,
    /// Held open but carried across a restart: a keepalive drive between
    /// turns re-attaches to the replacement (L8). Never blocks.
    Survives,
}

/// One thing a restart would interrupt, named by actor and session.
#[derive(Clone, Debug)]
pub struct Interruption {
    pub class: Class,
    /// `connection`, `listener`, `drive`, `keepalive`, `wait`, or on an old
    /// daemon `connections` (counted, unnamed) and `recent_activity`.
    pub kind: &'static str,
    /// Empty only for the unnamed connection count of an old daemon.
    pub actor: String,
    pub session: Option<String>,
    /// The holder's recently present sessions, when the interruption itself
    /// records none (listeners and drives).
    pub present_sessions: Vec<String>,
    pub detail: String,
}

#[derive(Clone, Debug)]
pub struct Preflight {
    pub home: PathBuf,
    pub verdict: Verdict,
    /// `occupancy`, `agents` (degraded) or `none` (not running).
    pub source: &'static str,
    pub degraded: bool,
    /// The daemon's `ping`, when one answered.
    pub daemon: Option<Value>,
    pub interruptions: Vec<Interruption>,
    /// Sessions seen recently that hold nothing open: who may come back.
    pub present: Vec<Value>,
    /// The daemon's own report (`occupancy`, or the `agents` listing).
    pub report: Value,
}

impl Preflight {
    pub fn live(&self) -> impl Iterator<Item = &Interruption> {
        self.interruptions.iter().filter(|i| i.class == Class::Live)
    }

    pub fn armed(&self) -> impl Iterator<Item = &Interruption> {
        self.interruptions
            .iter()
            .filter(|i| i.class == Class::Armed)
    }

    pub fn survives(&self) -> impl Iterator<Item = &Interruption> {
        self.interruptions
            .iter()
            .filter(|i| i.class == Class::Survives)
    }

    /// The stable `--json` form.
    pub fn to_json(&self) -> Value {
        let daemon = self.daemon.as_ref().map(|d| {
            json!({"version":d["version"],"build":d["build"],"protocol_version":d["protocol_version"],"store_id":d["store_id"]})
        });
        let interrupts: Vec<Value> = self
            .interruptions
            .iter()
            .map(|i| {
                json!({
                    "class":match i.class {Class::Live => "live", Class::Armed => "armed", Class::Survives => "survives"},
                    "kind":i.kind,
                    "actor":(!i.actor.is_empty()).then_some(&i.actor),
                    "session":i.session,
                    "present_sessions":i.present_sessions,
                    "detail":i.detail,
                })
            })
            .collect();
        json!({
            "home":self.home,
            "verdict":self.verdict.as_str(),
            "exit_code":self.verdict.exit_code(),
            "source":self.source,
            "degraded":self.degraded.then_some(DEGRADED),
            "daemon":daemon,
            "interrupts":interrupts,
            "present":self.present,
            "report":self.report,
        })
    }

    /// The human form, one holder per line.
    pub fn text(&self) -> String {
        let mut s = format!("restart preflight for {}\n", self.home.display());
        if let Some(d) = &self.daemon {
            s += &format!(
                "daemon: {} build {} (protocol {})\n",
                d["version"].as_str().unwrap_or("?"),
                d["build"].as_str().unwrap_or("?"),
                d["protocol_version"]
            );
        }
        if self.degraded {
            s += &format!("degraded: {DEGRADED}\n");
        }
        let line = |i: &Interruption| {
            let who = if i.actor.is_empty() {
                "(unnamed)".to_owned()
            } else {
                match (&i.session, i.present_sessions.as_slice()) {
                    (Some(session), _) => format!("{} (session {session})", i.actor),
                    (None, []) => format!("{} (no session recorded)", i.actor),
                    (None, present) => {
                        format!(
                            "{} (no session recorded; present as {})",
                            i.actor,
                            present.join(", ")
                        )
                    }
                }
            };
            format!("  - {who}: {}\n", i.detail)
        };
        s += &format!(
            "verdict: {} (exit {})\n",
            self.verdict.as_str(),
            self.verdict.exit_code()
        );
        match self.verdict {
            Verdict::NotRunning => {
                s += "no daemon answers on this home; a restart interrupts nothing\n"
            }
            Verdict::Idle => s += "a restart would interrupt nothing\n",
            Verdict::DegradedIdle => {
                s += "nothing live is visible; a wait or watcher on this daemon would not show\n"
            }
            _ => {}
        }
        if self.live().next().is_some() {
            s += "a restart would interrupt:\n";
            self.live().for_each(|i| s += &line(i));
        }
        if self.armed().next().is_some() {
            s += if self.degraded {
                "possibly waiting (this daemon cannot tell a wait from recent activity):\n"
            } else {
                "armed only (no open connection; a call during the restart finds the daemon briefly gone):\n"
            };
            self.armed().for_each(|i| s += &line(i));
        }
        if self.survives().next().is_some() {
            s += "survives the restart (re-attaches to the replacement between turns):\n";
            self.survives().for_each(|i| s += &line(i));
        }
        let present: Vec<String> = self
            .present
            .iter()
            .map(|p| match p["session"].as_str() {
                Some(session) => format!("{} ({session})", p["agent"].as_str().unwrap_or("?")),
                None => p["agent"].as_str().unwrap_or("?").to_owned(),
            })
            .collect();
        if !present.is_empty() {
            s += &format!(
                "recently present, holding nothing open: {}\n",
                present.join(", ")
            );
        }
        s
    }
}

/// Read what a restart of `home`'s daemon would interrupt. Never mutates.
pub fn preflight(home: &Path) -> Result<Preflight> {
    let mut pre = Preflight {
        home: home.to_path_buf(),
        verdict: Verdict::NotRunning,
        source: "none",
        degraded: false,
        daemon: None,
        interruptions: Vec::new(),
        present: Vec::new(),
        report: Value::Null,
    };
    let ping = match crate::client::rpc(home, &Request::new("ping", "", json!({})), 5) {
        Ok(ping) => ping,
        Err(e) if e.code == "unavailable" => return Ok(pre),
        Err(e) => return Err(e),
    };
    let has_occupancy = ping["capabilities"]
        .as_array()
        .is_some_and(|caps| caps.iter().any(|c| c == "occupancy"));
    let request = if has_occupancy {
        Request::new("occupancy", "", json!({}))
    } else {
        Request::new("agents", "", json!({"limit":100}))
    };
    let report = crate::client::rpc(home, &request, 10)?;
    if has_occupancy {
        pre.source = "occupancy";
        read_occupancy(&mut pre, &report);
    } else {
        pre.source = "agents";
        pre.degraded = true;
        read_agents(&mut pre, &ping, &report);
    }
    pre.verdict = decide(&pre);
    pre.daemon = Some(ping);
    pre.report = report;
    Ok(pre)
}

fn decide(pre: &Preflight) -> Verdict {
    if pre.live().next().is_some() {
        Verdict::Busy
    } else if pre.armed().next().is_some() {
        Verdict::Armed
    } else if pre.degraded {
        Verdict::DegradedIdle
    } else {
        Verdict::Idle
    }
}

impl Preflight {
    /// Survival assumes a replacement that adopts keepalive drives (L8).
    /// When the replacement is another build than this one, that cannot be
    /// known before it runs: count them as live instead.
    pub fn assume_no_adoption(&mut self, build: &str) {
        if self.verdict == Verdict::NotRunning {
            return;
        }
        for i in &mut self.interruptions {
            if i.class == Class::Survives {
                i.class = Class::Live;
                i.detail = format!(
                    "{}; the replacement (build {build}) may not adopt it",
                    i.detail
                );
            }
        }
        self.verdict = decide(self);
    }
}

fn text(v: &Value) -> String {
    v.as_str().unwrap_or("").to_owned()
}

fn session(v: &Value) -> Option<String> {
    v.as_str().filter(|s| !s.is_empty()).map(str::to_owned)
}

/// Seconds from `now` until `at`, for a reader.
fn until(at: &Value, now: &Value) -> String {
    match (at.as_i64(), now.as_i64()) {
        (Some(at), Some(now)) => format!("{}s", ((at - now).max(0) + 999) / 1000),
        _ => "?".into(),
    }
}

fn read_occupancy(pre: &mut Preflight, r: &Value) {
    let items = |key: &str| r[key].as_array().cloned().unwrap_or_default();
    let now = &r["time_ms"];
    let sessions = items("sessions");
    // Where a listener or drive records no session, the holder's present ones.
    let present_of = |agent: &str| -> Vec<String> {
        sessions
            .iter()
            .filter(|s| s["agent"] == agent)
            .filter_map(|s| session(&s["session"]))
            .filter(|s| !crate::keepalive::is_session(Some(s)))
            .collect()
    };
    // A keepalive drive outlives a restart and re-attaches (L8); only a turn
    // it is running now is cut off. A live keepalive with a recorded pid and
    // no running turn survives, with its drive and stream. One still
    // starting (no pid yet) cannot be re-adopted, so it stays live.
    let controllers = items("controllers");
    let surviving: Vec<String> = items("keepalives")
        .iter()
        .filter(|k| k["live"] == true && k["pid"].is_i64())
        .map(|k| text(&k["agent"]))
        .filter(|agent| {
            !controllers
                .iter()
                .any(|c| c["agent"] == agent.as_str() && c["state"] == "running")
        })
        .collect();
    let survives = |agent: &str| surviving.iter().any(|a| a == agent);
    for c in items("long_lived") {
        let actor = text(&c["actor"]);
        let keepalive = c["keepalive"] == true;
        pre.interruptions.push(Interruption {
            class: if keepalive && survives(&actor) {
                Class::Survives
            } else {
                Class::Live
            },
            kind: "connection",
            session: session(&c["session"]),
            present_sessions: Vec::new(),
            detail: format!(
                "open {} connection{}",
                c["op"].as_str().unwrap_or("?"),
                if keepalive { " (keepalive drive)" } else { "" }
            ),
            actor,
        });
    }
    for l in items("listeners") {
        let actor = text(&l["agent"]);
        pre.interruptions.push(Interruption {
            class: Class::Live,
            kind: "listener",
            session: None,
            present_sessions: present_of(&actor),
            detail: format!("live listener (run {})", text(&l["run_id"])),
            actor,
        });
    }
    for c in &controllers {
        let actor = text(&c["agent"]);
        let state = text(&c["state"]);
        let between_turns = state != "running" && survives(&actor);
        pre.interruptions.push(Interruption {
            class: if between_turns {
                Class::Survives
            } else {
                Class::Live
            },
            kind: "drive",
            session: None,
            present_sessions: present_of(&actor),
            detail: if between_turns {
                format!("keepalive drive between turns (run {})", text(&c["run_id"]))
            } else {
                format!("live drive, {state} (run {})", text(&c["run_id"]))
            },
            actor,
        });
    }
    for k in items("keepalives")
        .into_iter()
        .filter(|k| k["live"] == true)
    {
        let actor = text(&k["agent"]);
        let between_turns = survives(&actor);
        pre.interruptions.push(Interruption {
            class: if between_turns {
                Class::Survives
            } else {
                Class::Live
            },
            kind: "keepalive",
            session: session(&k["session"]),
            present_sessions: Vec::new(),
            detail: match k["pid"].as_i64() {
                Some(pid) if between_turns => {
                    format!("keepalive drive process (pid {pid}), not mid-turn")
                }
                Some(pid) => format!("keepalive drive process (pid {pid}), mid-turn"),
                None => "keepalive drive starting".into(),
            },
            actor,
        });
    }
    // A wait row whose own connection or keepalive is open is that same
    // holder; only the rest are armed state with nothing open behind them.
    // A surviving keepalive's own waits resume with it.
    for w in items("waits") {
        let key = (text(&w["agent"]), session(&w["session"]));
        let held = pre
            .interruptions
            .iter()
            .any(|i| i.class != Class::Armed && (&i.actor, &i.session) == (&key.0, &key.1));
        let keepalive_wait = crate::keepalive::is_session(key.1.as_deref()) && survives(&key.0);
        if held || keepalive_wait {
            continue;
        }
        pre.interruptions.push(Interruption {
            class: Class::Armed,
            kind: "wait",
            actor: key.0,
            session: key.1,
            present_sessions: Vec::new(),
            detail: format!(
                "armed wait with no open connection, counted for another {}",
                until(&w["expires_ms"], now)
            ),
        });
    }
    let holders: Vec<&str> = pre.interruptions.iter().map(|i| i.actor.as_str()).collect();
    pre.present = sessions
        .iter()
        .filter(|s| !holders.contains(&s["agent"].as_str().unwrap_or("")))
        .map(
            |s| json!({"agent":s["agent"],"session":s["session"],"last_seen_ms":s["last_seen_ms"]}),
        )
        .collect();
}

fn read_agents(pre: &mut Preflight, ping: &Value, r: &Value) {
    let open = ping["capacity"]["long_lived"].as_u64().unwrap_or(0);
    if open > 0 {
        pre.interruptions.push(Interruption {
            class: Class::Live,
            kind: "connections",
            actor: String::new(),
            session: None,
            present_sessions: Vec::new(),
            detail: format!(
                "{open} open long-lived connection(s) (waits, watches or listeners); this daemon cannot name them"
            ),
        });
    }
    for a in r["items"].as_array().into_iter().flatten() {
        let actor = text(&a["name"]);
        let bound = &a["session"]["bound"];
        let live_session = (bound["live"] == true)
            .then(|| session(&bound["session"]))
            .flatten();
        let present_sessions: Vec<String> = live_session.iter().cloned().collect();
        let mut found: Vec<(Class, &'static str, String)> = Vec::new();
        if a["listener"]["live"] == true {
            found.push((
                Class::Live,
                "listener",
                format!("live listener (run {})", text(&a["listener"]["run_id"])),
            ));
        }
        if a["controller"]["live"] == true {
            found.push((
                Class::Live,
                "drive",
                format!(
                    "live drive, {} (run {})",
                    text(&a["controller"]["state"]),
                    text(&a["controller"]["run_id"])
                ),
            ));
        }
        // The roster shows a keepalive only while it is not off, stopped
        // or failed.
        if a["keepalive"].is_object() {
            let state = text(&a["keepalive"]["state"]);
            found.push((
                Class::Live,
                "keepalive",
                match a["keepalive"]["pid"].as_i64() {
                    Some(pid) => format!("keepalive {state} (pid {pid})"),
                    None => format!("keepalive {state}"),
                },
            ));
        }
        if found.is_empty() && a["recently_seen"] == true {
            found.push((
                Class::Armed,
                "recent_activity",
                "seen within two minutes or waiting; this daemon cannot tell which".into(),
            ));
        }
        let held = !found.is_empty();
        for (class, kind, detail) in found {
            pre.interruptions.push(Interruption {
                class,
                kind,
                actor: actor.clone(),
                session: None,
                present_sessions: present_sessions.clone(),
                detail,
            });
        }
        if !held {
            if let Some(session) = live_session {
                pre.present.push(
                    json!({"agent":actor,"session":session,"last_seen_ms":bound["last_seen_ms"]}),
                );
            }
        }
    }
}

/// `<home>/daemon.restarting`: a restart is in progress on this home. Written
/// by `fray restart` for its whole run, or (when no restarter wrote one) by a
/// daemon accepting a restart shutdown; removed by the replacement once it
/// has bound its socket. While it is honoured, a client whose connect finds
/// no daemon treats that like a `restarting` refusal and waits for the
/// replacement within its own budget, instead of failing in the gap.
///
/// It is honoured only while its writer is alive and for at most
/// [`MARKER_FRESH_MS`]: a killed restarter, or a `fray stop --restart` with
/// no start after it, never makes clients of a dead home wait.
pub const MARKER: &str = "daemon.restarting";
/// The client's own restart window: no marker is honoured for longer.
pub const MARKER_FRESH_MS: i64 = 30_000;

/// Writes the marker atomically. Advisory: callers report a failure and go on.
pub fn write_marker(home: &Path, reason: &str) -> std::io::Result<()> {
    use std::{io::Write, os::unix::fs::OpenOptionsExt};
    let path = home.join(MARKER);
    let temp = home.join(format!(".{MARKER}.{}.tmp", std::process::id()));
    let result = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        let body = json!({"reason":reason,"pid":std::process::id(),"written_ms":now_ms()});
        file.write_all(format!("{body}\n").as_bytes())?;
        std::fs::rename(&temp, &path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

/// The reason of a fresh marker on `home`, if there is one.
pub fn marker(home: &Path) -> Option<String> {
    let body: Value = serde_json::from_slice(&std::fs::read(home.join(MARKER)).ok()?).ok()?;
    let written = body["written_ms"].as_i64()?;
    let age = now_ms() - written;
    let writer = body["pid"].as_i64()?;
    ((0..MARKER_FRESH_MS).contains(&age) && crate::keepalive::pid_alive(writer))
        .then(|| body["reason"].as_str().unwrap_or("").to_owned())
}

pub fn clear_marker(home: &Path) {
    let _ = std::fs::remove_file(home.join(MARKER));
}

/// What `fray restart` (without `--dry-run`) may do.
#[derive(Clone, Debug, Default)]
pub struct Options {
    /// Proceed although the preflight is `busy` (or `armed`).
    pub force: bool,
    /// Proceed although the preflight is `armed`.
    pub allow_armed: bool,
    /// Restart a daemon without the `announce` capability, with no notice.
    pub no_announce: bool,
    /// The notice's reason. Default: `upgrade to <build>`.
    pub reason: Option<String>,
    /// The binary to start. Default: the one running this command.
    pub exe: Option<PathBuf>,
    /// How long the old daemon may drain in-flight requests (at most 10 s).
    pub grace_ms: Option<u64>,
    /// Start the replacement with `--normal`. Default: the old daemon's
    /// durability from its registry record, else FULL.
    pub normal: Option<bool>,
    /// Who asked: the `--as` identity, else empty with `requested_by`.
    pub actor: String,
    pub requested_by: Option<String>,
}

/// How a restart that did not fail ended.
pub enum Outcome {
    /// The preflight refused (busy or armed); nothing was changed.
    Refused(Preflight),
    /// The daemon became busy (or armed) between the announcement and the
    /// shutdown: nothing was stopped, and the notice was followed by an
    /// `abandoned` one. The announcement state is the second field.
    Abandoned(Preflight, Value),
    /// Restarted (or, with no daemon running, started). The `--json` form.
    Done(Value),
}

impl Outcome {
    pub fn exit_code(&self) -> i32 {
        match self {
            Self::Refused(pre) | Self::Abandoned(pre, _) => pre.verdict.exit_code(),
            Self::Done(_) => 0,
        }
    }

    pub fn to_json(&self) -> Value {
        match self {
            Self::Refused(pre) => json!({
                "home":pre.home,
                "action":"refused",
                "verdict":pre.verdict.as_str(),
                "exit_code":pre.verdict.exit_code(),
                "hint":refusal_hint(pre.verdict),
                "preflight":pre.to_json(),
            }),
            Self::Abandoned(pre, notice) => json!({
                "home":pre.home,
                "action":"abandoned",
                "verdict":pre.verdict.as_str(),
                "exit_code":pre.verdict.exit_code(),
                "hint":refusal_hint(pre.verdict),
                "announce":notice,
                "preflight":pre.to_json(),
            }),
            Self::Done(v) => v.clone(),
        }
    }

    pub fn text(&self) -> String {
        match self {
            Self::Refused(pre) => format!("{}refused: {}\n", pre.text(), refusal_hint(pre.verdict)),
            Self::Abandoned(pre, _) => format!(
                "{}abandoned: it became {} after the restart was announced. {}\n",
                pre.text(),
                pre.verdict.as_str(),
                refusal_hint(pre.verdict)
            ),
            Self::Done(v) => done_text(v),
        }
    }
}

fn refusal_hint(verdict: Verdict) -> &'static str {
    match verdict {
        Verdict::Busy => "the daemon is busy; nothing was changed. Rerun with --force to interrupt the holders above (waits and watches reconnect to the replacement)",
        _ => "only armed waits; nothing was changed. Rerun with --allow-armed to accept a short gap for those agents",
    }
}

fn done_text(v: &Value) -> String {
    let side = |d: &Value| {
        if d.is_null() {
            "none".to_owned()
        } else {
            format!(
                "build {} pid {}",
                d["build"].as_str().unwrap_or("?"),
                d["pid"].as_u64().map_or("?".into(), |p| p.to_string())
            )
        }
    };
    let mut s = format!(
        "{} {}\n",
        v["action"].as_str().unwrap_or("?"),
        v["home"].as_str().unwrap_or("?")
    );
    if let Some(label) = v["degraded"].as_str() {
        s += &format!("DEGRADED preflight: {label}\n");
    }
    s += &format!(
        "binary: {} ({}, build {})\n",
        v["exe"]["path"].as_str().unwrap_or("?"),
        v["exe"]["source"].as_str().unwrap_or("?"),
        v["exe"]["build"].as_str().unwrap_or("?")
    );
    s += &format!("before: {}\n", side(&v["before"]));
    s += &format!("after:  {}\n", side(&v["after"]));
    let card = |c: &Value| c.as_i64().map_or("none".to_owned(), |id| format!("#{id}"));
    match v["announce"]["skipped"].as_str() {
        Some(why) => s += &format!("announcement: none ({why})\n"),
        None => {
            s += &format!(
                "announcement: restart {}, restarted {}\n",
                card(&v["announce"]["restart"]),
                card(&v["announce"]["restarted"])
            )
        }
    }
    for i in v["interrupted"].as_array().into_iter().flatten() {
        s += &format!(
            "interrupted: {} ({}): {}\n",
            i["actor"].as_str().unwrap_or("(unnamed)"),
            i["session"].as_str().unwrap_or("no session"),
            i["detail"].as_str().unwrap_or("")
        );
    }
    for i in v["survived"].as_array().into_iter().flatten() {
        s += &format!(
            "carried over: {}: {}\n",
            i["actor"].as_str().unwrap_or("?"),
            i["detail"].as_str().unwrap_or("")
        );
    }
    for w in v["warnings"].as_array().into_iter().flatten() {
        s += &format!("warning: {}\n", w.as_str().unwrap_or(""));
    }
    s += &format!("duration: {}ms\n", v["duration_ms"]);
    s
}

fn has(ping: &Value, capability: &str) -> bool {
    ping["capabilities"]
        .as_array()
        .is_some_and(|caps| caps.iter().any(|c| c == capability))
}

/// The build a `fray` binary reports: `fray <version> (<build>)`.
pub fn exe_build(exe: &Path) -> Result<String> {
    let same = std::env::current_exe()
        .and_then(std::fs::canonicalize)
        .is_ok_and(|own| std::fs::canonicalize(exe).is_ok_and(|exe| exe == own));
    if same {
        return Ok(BUILD.to_owned());
    }
    let out = std::process::Command::new(exe)
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| {
            Error::new(
                "exe_build",
                format!("cannot run {} --version: {e}", exe.display()),
            )
        })?;
    let text = String::from_utf8_lossy(&out.stdout);
    let build = text
        .trim()
        .rsplit_once('(')
        .and_then(|(_, rest)| rest.strip_suffix(')'))
        .filter(|b| !b.is_empty() && out.status.success());
    build.map(str::to_owned).ok_or_else(|| {
        Error::new(
            "exe_build",
            format!(
                "{} --version did not report a build (`fray <version> (<build>)`): {:?}",
                exe.display(),
                text.trim()
            ),
        )
    })
}

/// The store identity: from a ping when the daemon reports it, else read
/// from the home's database without a daemon.
fn store_id(home: &Path, ping: Option<&Value>) -> Option<String> {
    if let Some(id) = ping.and_then(|p| p["store_id"].as_str()) {
        return Some(id.to_owned());
    }
    let conn = rusqlite::Connection::open_with_flags(
        home.join("state.db"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .ok()?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(2));
    conn.query_row("SELECT value FROM meta WHERE key='store_id'", [], |r| {
        r.get(0)
    })
    .ok()
}

fn brief(ping: &Value) -> Value {
    json!({"pid":ping["pid"],"build":ping["build"],"version":ping["version"],"protocol_version":ping["protocol_version"]})
}

fn listing(items: Vec<&Interruption>) -> Vec<Value> {
    items
        .into_iter()
        .map(|i| {
            json!({"kind":i.kind,"actor":(!i.actor.is_empty()).then_some(&i.actor),
                "session":i.session,"present_sessions":i.present_sessions,"detail":i.detail})
        })
        .collect()
}

/// Post a maintenance notice as `fray`; the card id.
fn announce(home: &Path, opts: &Options, args: Value) -> Result<i64> {
    let mut args = args;
    if let Some(user) = &opts.requested_by {
        args["requested_by"] = json!(user);
    }
    let mut req = Request::new("announce", &opts.actor, args);
    req.key = Some(random_key()?);
    let v = crate::client::rpc(home, &req, 10)?;
    v["card"]["id"]
        .as_i64()
        .ok_or_else(|| Error::new("protocol", format!("announce returned no card: {v}")))
}

/// `fray restart`: preflight, announce, drain, start, verify (L9).
///
/// One run per home at a time (`restart.lock`). Refuses a `busy` daemon
/// without `force` and an `armed` one without `allow_armed` (or `force`),
/// changing nothing, and checks again after the announcement: a daemon that
/// became busy meanwhile is not stopped ([`Outcome::Abandoned`]). With a
/// replacement of another build, idle keepalives count as live. An old
/// daemon read in degraded mode proceeds with the label. With no daemon
/// running, it only starts one. A failed announcement never leads to a
/// restart, and any failure after a posted notice posts an `abandoned` one.
pub fn run(home: &Path, opts: &Options) -> Result<Outcome> {
    let started = std::time::Instant::now();
    let (exe, exe_source) = match &opts.exe {
        Some(exe) => (
            std::fs::canonicalize(exe)
                .map_err(|e| Error::invalid(format!("--exe {}: {e}", exe.display())))?,
            "--exe",
        ),
        None => (std::env::current_exe()?, "current_exe"),
    };
    let build = exe_build(&exe)?;
    let home = &crate::server::initialize(home)?;
    let _exclusive = lock(home)?;
    let check = |home: &Path| -> Result<Preflight> {
        let mut pre = preflight(home)?;
        if build != BUILD {
            pre.assume_no_adoption(&build);
        }
        Ok(pre)
    };
    let blocked = |pre: &Preflight| match pre.verdict {
        Verdict::Busy => !opts.force,
        Verdict::Armed => !(opts.force || opts.allow_armed),
        _ => false,
    };
    let pre = check(home)?;
    if blocked(&pre) {
        return Ok(Outcome::Refused(pre));
    }
    if pre.degraded {
        eprintln!("fray restart: DEGRADED preflight: {DEGRADED}");
    }
    let log = home.join("daemon.log");
    let mut warnings: Vec<String> = Vec::new();
    let record = crate::registry::state_dir()
        .ok()
        .map(|dir| crate::registry::record_path(&dir, home))
        .and_then(|path| std::fs::read(path).ok())
        .and_then(|bytes| serde_json::from_slice::<crate::registry::Record>(&bytes).ok());
    let mut out = json!({
        "home":home,
        "verdict":pre.verdict.as_str(),
        "degraded":pre.degraded.then_some(DEGRADED),
        "forced":opts.force && pre.verdict == Verdict::Busy,
        "exe":{"path":exe,"source":exe_source,"build":build},
        "log":log,
    });
    let Some(ping) = pre.daemon.clone() else {
        // Nothing to stop or announce to: start one.
        let normal = opts.normal.unwrap_or(false);
        let store = store_id(home, None);
        crate::client::start_exe(home, normal, &exe, std::time::Duration::from_secs(10), true)?;
        let after = verify(home, &build, store.as_deref(), None, &log)?;
        out["action"] = json!("started");
        out["before"] = Value::Null;
        out["after"] = brief(&after);
        out["store_id"] = json!(store_id(home, Some(&after)));
        out["announce"] =
            json!({"restart":null,"restarted":null,"skipped":"no daemon was running"});
        out["interrupted"] = json!([]);
        out["survived"] = json!([]);
        out["warnings"] = json!(warnings);
        out["duration_ms"] = json!(started.elapsed().as_millis() as u64);
        return Ok(Outcome::Done(out));
    };
    let from_build = ping["build"].as_str().unwrap_or("unknown").to_owned();
    let reason = opts
        .reason
        .clone()
        .unwrap_or_else(|| format!("upgrade to {build}"));
    let before_store = store_id(home, Some(&ping));
    // The old daemon's pid: its ping, else its registry record, else the
    // process table. Without one, only the socket shows it has gone.
    let (old_pid, pid_source) = if let Some(pid) = ping["pid"].as_u64() {
        (Some(pid), "ping")
    } else if let Some(r) = record.as_ref().filter(|r| r.build == from_build) {
        (Some(u64::from(r.pid)), "registry")
    } else {
        match crate::daemons::serving_pids(home).as_slice() {
            [pid] => (Some(u64::from(*pid)), "process_table"),
            _ => (None, "none"),
        }
    };
    let normal = opts.normal.unwrap_or_else(|| {
        record
            .as_ref()
            .is_some_and(|r| Some(u64::from(r.pid)) == old_pid && r.durability == "normal")
    });
    // Announce first. Without a posted notice, nothing restarts.
    let mut notice = json!({"restart":null,"restarted":null,"abandoned":null,"skipped":null});
    let announcing = !opts.no_announce;
    if opts.no_announce {
        notice["skipped"] = json!("--no-announce");
    } else if !has(&ping, "announce") {
        return Err(Error::new(
            "announce_unsupported",
            format!("daemon build {from_build} cannot post a maintenance notice (no `announce` capability); nothing was changed. Rerun with --no-announce to restart it without one"),
        )
        .with_details(pre.to_json()));
    } else {
        let args =
            json!({"action":"restart","reason":reason,"from_build":from_build,"to_build":build});
        let id = announce(home, opts, args).map_err(|e| {
            let message = format!(
                "the restart notice could not be posted ({}: {}); the daemon was not restarted",
                e.code, e.message
            );
            Error::new("announce_failed", message)
                .with_details(json!({"cause":e,"preflight":pre.to_json()}))
        })?;
        notice["restart"] = json!(id);
    }
    // Whoever arrived while the notice went out counts too.
    let notice_from = (from_build.as_str(), build.as_str());
    let pre = match check(home) {
        Ok(again) if blocked(&again) => {
            abandon(
                home,
                opts,
                &mut notice,
                &reason,
                "the daemon became busy",
                notice_from,
            );
            return Ok(Outcome::Abandoned(again, notice));
        }
        Ok(again) => again,
        Err(e) => {
            abandon(
                home,
                opts,
                &mut notice,
                &reason,
                "the second preflight failed",
                notice_from,
            );
            return Err(with_notice(e, &notice));
        }
    };
    // Drain: the old daemon tells long-lived clients to reconnect and lets
    // in-flight requests finish; an old one without that just stops.
    let graceful = has(&ping, "graceful_restart");
    if let Err(e) = write_marker(home, &reason) {
        warnings.push(format!("restart marker not written: {e}"));
    }
    let mut args = json!({});
    if graceful {
        args = json!({"restart":true,"reason":reason});
        if let Some(grace) = opts.grace_ms {
            args["grace_ms"] = json!(grace);
        }
    }
    let reply = match crate::client::rpc(home, &Request::new("shutdown", "", args), 10) {
        Ok(reply) => reply,
        Err(e) => {
            clear_marker(home);
            abandon(
                home,
                opts,
                &mut notice,
                &reason,
                "the shutdown was not accepted",
                notice_from,
            );
            let message = format!(
                "shutdown was not accepted; nothing restarted: {}",
                e.message
            );
            return Err(with_notice(Error::new(&e.code, message), &notice));
        }
    };
    let old_pid = reply["pid"].as_u64().or(old_pid);
    let grace = reply["grace_ms"].as_u64().unwrap_or(0);
    let window = std::time::Duration::from_millis(grace + 5000);
    let stopping = std::time::Instant::now();
    if !gone(home, window) {
        clear_marker(home);
        abandon(
            home,
            opts,
            &mut notice,
            &reason,
            "the old daemon did not stop",
            notice_from,
        );
        let message = format!(
            "the old daemon still answered {}ms after accepting the shutdown; nothing started",
            window.as_millis()
        );
        return Err(with_notice(Error::new("stop_incomplete", message), &notice));
    }
    // Its socket is silent. A pid that still looks alive may have been
    // reused: start anyway, since the daemon lock decides, and say so.
    if let Some(pid) = old_pid.and_then(|p| u32::try_from(p).ok()) {
        if !crate::client::await_exit(pid, window.saturating_sub(stopping.elapsed())) {
            warnings.push(format!("process {pid} still appears to run after its daemon stopped answering (ps -p {pid}); started the replacement anyway, which the daemon lock arbitrates"));
        }
    }
    let drained_ms = stopping.elapsed().as_millis() as u64;
    if old_pid.is_none() {
        warnings.push("the old daemon reported no pid and none was found; its exit was inferred from its socket, and the start retries until it releases the lock".into());
    }
    let start_window = std::time::Duration::from_secs(10);
    if let Err(e) = crate::client::start_exe(home, normal, &exe, start_window, true) {
        clear_marker(home);
        abandon(
            home,
            opts,
            &mut notice,
            &reason,
            "no replacement started",
            notice_from,
        );
        return Err(with_notice(Error::new(
            "startup",
            format!("the old daemon stopped but no replacement answered ({}); the home has no daemon. Inspect {}", e.message, log.display()),
        ), &notice));
    }
    // An exe of an older build does not clear the marker itself.
    clear_marker(home);
    let after = match verify(home, &build, before_store.as_deref(), old_pid, &log) {
        Ok(after) => after,
        Err(e) => {
            let why = "the replacement failed verification";
            abandon(home, opts, &mut notice, &reason, why, notice_from);
            return Err(with_notice(e, &notice));
        }
    };
    // An old daemon could not announce the restart; its replacement can at
    // least say it happened. An explicit --no-announce on a daemon that
    // could have announced means no notices at all.
    let unannounceable = !has(&ping, "announce");
    if announcing || unannounceable {
        let reason = if announcing {
            reason.clone()
        } else {
            format!("{reason}; no advance notice was possible: the old daemon (build {from_build}) predated restart notices")
        };
        if opts.actor.is_empty() && opts.requested_by.is_none() {
            warnings.push(
                "the restarted notice was not posted: no --as or $USER to record who asked".into(),
            );
        } else if has(&after, "announce") {
            let args = json!({"action":"restarted","reason":reason,"from_build":from_build,"to_build":after["build"]});
            match announce(home, opts, args) {
                Ok(id) => notice["restarted"] = json!(id),
                Err(e) => warnings.push(format!(
                    "the restarted notice could not be posted ({}: {})",
                    e.code, e.message
                )),
            }
        } else {
            warnings.push("the new daemon cannot post the restarted notice (no `announce`)".into());
        }
    }
    let interrupted: Vec<&Interruption> = pre
        .interruptions
        .iter()
        .filter(|i| i.class != Class::Survives)
        .collect();
    out["action"] = json!("restarted");
    out["before"] = json!({"pid":old_pid,"pid_source":pid_source,"build":from_build,"version":ping["version"],"protocol_version":ping["protocol_version"]});
    out["after"] = brief(&after);
    out["store_id"] = json!(before_store);
    out["stop"] = json!({"mode":if graceful {"graceful"} else {"plain"},"grace_ms":grace,"drained_ms":drained_ms});
    out["durability"] = json!(if normal { "normal" } else { "full" });
    out["announce"] = notice;
    out["interrupted"] = json!(listing(interrupted));
    out["survived"] = json!(listing(pre.survives().collect()));
    out["warnings"] = json!(warnings);
    out["duration_ms"] = json!(started.elapsed().as_millis() as u64);
    Ok(Outcome::Done(out))
}

/// Holds `<home>/restart.lock` for one `fray restart` at a time. Separate
/// from `daemon.lock`, so it never competes with a starting daemon.
fn lock(home: &Path) -> Result<std::fs::File> {
    use fs2::FileExt;
    use std::os::unix::fs::OpenOptionsExt;
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(home.join("restart.lock"))?;
    file.try_lock_exclusive().map_err(|_| {
        Error::new(
            "restart_in_progress",
            format!(
                "another fray restart is running on {}; nothing was changed",
                home.display()
            ),
        )
    })?;
    Ok(file)
}

/// After a posted restart notice, a failure says so on the board (best
/// effort: a home with no daemon has no board to tell).
fn abandon(
    home: &Path,
    opts: &Options,
    notice: &mut Value,
    reason: &str,
    why: &str,
    (from, to): (&str, &str),
) {
    if notice["restart"].is_null() {
        return;
    }
    let args = json!({"action":"abandoned","reason":format!("{reason} ({why})"),"from_build":from,"to_build":to});
    match announce(home, opts, args) {
        Ok(id) => notice["abandoned"] = json!(id),
        Err(e) => notice["abandoned_error"] = json!(format!("{}: {}", e.code, e.message)),
    }
}

/// Attach the announcement state to a failure, keeping its own details.
fn with_notice(e: Error, notice: &Value) -> Error {
    let mut details = match e.details.clone() {
        Some(Value::Object(map)) => Value::Object(map),
        Some(other) => json!({"cause":other}),
        None => json!({}),
    };
    details["announce"] = notice.clone();
    e.with_details(details)
}

/// Waits until nothing answers on the home's socket.
fn gone(home: &Path, window: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + window;
    loop {
        match crate::client::probe(home, &Request::new("ping", "", json!({})), 1) {
            Err(e) if e.code == "unavailable" => return true,
            _ if std::time::Instant::now() >= deadline => return false,
            _ => std::thread::sleep(std::time::Duration::from_millis(50)),
        }
    }
}

/// The replacement answers with the expected build, serves the same store,
/// and is not the old process. Otherwise a loud failure naming the log.
fn verify(
    home: &Path,
    build: &str,
    store: Option<&str>,
    old_pid: Option<u64>,
    log: &Path,
) -> Result<Value> {
    let after = crate::client::rpc(home, &Request::new("ping", "", json!({})), 5)?;
    let mut problems = Vec::new();
    if after["build"].as_str() != Some(build) {
        problems.push(format!(
            "it runs build {} but {build} was started",
            after["build"].as_str().unwrap_or("unknown")
        ));
    }
    let after_store = store_id(home, Some(&after));
    if let Some(store) = store {
        if after_store.as_deref() != Some(store) {
            problems.push(format!(
                "it serves store {} but the old daemon served {store}",
                after_store.as_deref().unwrap_or("unknown")
            ));
        }
    }
    if old_pid.is_some() && after["pid"].as_u64() == old_pid {
        problems.push("it is still the old process".into());
    }
    if problems.is_empty() {
        return Ok(after);
    }
    Err(Error::new(
        "restart_unverified",
        format!(
            "the daemon now answering on {} is not the expected replacement: {}. Inspect {}",
            home.display(),
            problems.join("; "),
            log.display()
        ),
    )
    .with_details(json!({"expected_build":build,"expected_store_id":store,"after":brief(&after),"after_store_id":after_store,"log":log})))
}
