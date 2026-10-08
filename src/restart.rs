//! `fray restart` preflight (docs/plans/2026-10-08-daemon-lifecycle.md, L5):
//! what a restart of one home's daemon would interrupt, and whether it may go
//! ahead.
//!
//! Read-only. It sends only `ping` and then `occupancy` (or, on a daemon
//! without that capability, `agents`); neither joins, binds a session nor
//! records presence.
//!
//! # Two classes of interruption
//!
//! - **Live**: something a restart cuts off now. An open long-lived
//!   connection (a `wait`, `watch` or listener stream), a live listener, a
//!   running or waiting drive, or a live keepalive process.
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
                    "class":match i.class {Class::Live => "live", Class::Armed => "armed"},
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
    pre.verdict = if pre.live().next().is_some() {
        Verdict::Busy
    } else if pre.armed().next().is_some() {
        Verdict::Armed
    } else if pre.degraded {
        Verdict::DegradedIdle
    } else {
        Verdict::Idle
    };
    pre.daemon = Some(ping);
    pre.report = report;
    Ok(pre)
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
    for c in items("long_lived") {
        let actor = text(&c["actor"]);
        pre.interruptions.push(Interruption {
            class: Class::Live,
            kind: "connection",
            session: session(&c["session"]),
            present_sessions: Vec::new(),
            detail: format!(
                "open {} connection{}",
                c["op"].as_str().unwrap_or("?"),
                if c["keepalive"] == true {
                    " (keepalive drive)"
                } else {
                    ""
                }
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
    for c in items("controllers") {
        let actor = text(&c["agent"]);
        pre.interruptions.push(Interruption {
            class: Class::Live,
            kind: "drive",
            session: None,
            present_sessions: present_of(&actor),
            detail: format!(
                "live drive, {} (run {})",
                text(&c["state"]),
                text(&c["run_id"])
            ),
            actor,
        });
    }
    for k in items("keepalives")
        .into_iter()
        .filter(|k| k["live"] == true)
    {
        pre.interruptions.push(Interruption {
            class: Class::Live,
            kind: "keepalive",
            actor: text(&k["agent"]),
            session: session(&k["session"]),
            present_sessions: Vec::new(),
            detail: match k["pid"].as_i64() {
                Some(pid) => format!("keepalive drive process (pid {pid})"),
                None => "keepalive drive starting".into(),
            },
        });
    }
    // A wait row whose own connection or keepalive is live is that same
    // holder; only the rest are armed state with nothing open behind them.
    for w in items("waits") {
        let key = (text(&w["agent"]), session(&w["session"]));
        let held = pre
            .interruptions
            .iter()
            .any(|i| i.class == Class::Live && (&i.actor, &i.session) == (&key.0, &key.1));
        if held {
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
