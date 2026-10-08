//! `occupancy`: who a daemon restart would interrupt (docs/plans/2026-10-08-daemon-lifecycle.md, L4).
//!
//! Read-only. It never touches presence, acknowledges or records a
//! presentation, and it never counts the caller's own connection.
//!
//! # What counts as busy
//!
//! The verdict is `busy` when any of these holds, and `idle` otherwise:
//!
//! - **An open long-lived connection** on this daemon other than the
//!   caller's: a `wait` with a non-zero timeout, a `watch` or a
//!   `watch_attention` (a listener or a drive's stream). A restart drops it.
//! - **A live listener**: `listeners.connected=1`, refreshed within
//!   `LISTENER_TTL_MS`.
//! - **A live controller** (a drive): state `waiting` or `running`,
//!   refreshed within `CONTROLLER_TTL_MS`.
//! - **A live keepalive drive**: its recorded pid is alive, or it was
//!   started within `STARTING_MS` and has no pid yet. A reused pid makes a
//!   dead keepalive look live; that errs toward busy.
//! - **An armed wait**: a `session_waits` row refreshed within
//!   `WAIT_FRESH_MS`. Its `expires_ms` is when it stops counting. A wait
//!   between two calls of an agent's wait loop still counts, so this can
//!   name a holder with no open connection; an expired row never counts.
//!
//! Reported but never busy on their own: other connected clients (short
//! RPCs reconnect), and sessions seen within the presence window
//! (`IDENTITY_TTL_MS`), which show who may come back but hold nothing open.
use crate::model::*;
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::{
    collections::{BTreeSet, HashMap},
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
};

struct Entry {
    op: String,
    actor: String,
    session: Option<String>,
    since_ms: i64,
}

/// The daemon's open long-lived connections. Each entry lives exactly as
/// long as the [`Open`] guard its connection holds.
#[derive(Default)]
pub struct Registry {
    next: AtomicU64,
    open: Mutex<HashMap<u64, Entry>>,
}

/// Registration of one long-lived connection; dropping it deregisters.
pub struct Open<'a> {
    registry: &'a Registry,
    id: u64,
}

impl Drop for Open<'_> {
    fn drop(&mut self) {
        self.registry
            .open
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
    }
}

impl Registry {
    /// Register a connection entering a long-lived op until the guard drops.
    pub fn open(&self, req: &Request, now: i64) -> Open<'_> {
        let id = self.next.fetch_add(1, Ordering::SeqCst);
        self.open.lock().unwrap_or_else(|e| e.into_inner()).insert(
            id,
            Entry {
                op: req.op.clone(),
                actor: req.actor.clone(),
                session: req.session.clone(),
                since_ms: now,
            },
        );
        Open { registry: self, id }
    }

    /// The open connections, oldest first.
    pub fn snapshot(&self) -> Vec<Value> {
        let open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        let mut items: Vec<(u64, &Entry)> = open.iter().map(|(id, e)| (*id, e)).collect();
        items.sort_by_key(|(id, e)| (e.since_ms, *id));
        items
            .into_iter()
            .map(|(id, e)| {
                json!({"id":id,"op":e.op,"actor":e.actor,"session":e.session,
                    "keepalive":crate::keepalive::is_session(e.session.as_deref()),"since_ms":e.since_ms})
            })
            .collect()
    }
}

/// The store's part of the report, read under the store lock. Pids are
/// checked afterwards by [`finish`], outside it.
pub fn collect(conn: &Connection, now: i64) -> Result<Value> {
    let rows = |sql: &str, cutoff: i64, f: &dyn Fn(&rusqlite::Row) -> rusqlite::Result<Value>| {
        let mut stmt = conn.prepare(sql)?;
        let items = stmt
            .query_map(params![cutoff], |r| f(r))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok::<_, Error>(items)
    };
    let listeners = rows(
        "SELECT agent,run_id,updated_ms FROM listeners WHERE connected=1 AND updated_ms>? ORDER BY agent",
        now - crate::attention::LISTENER_TTL_MS,
        &|r| {
            let updated: i64 = r.get(2)?;
            Ok(json!({"agent":r.get::<_,String>(0)?,"run_id":r.get::<_,String>(1)?,"updated_ms":updated,"expires_ms":updated+crate::attention::LISTENER_TTL_MS}))
        },
    )?;
    let controllers = rows(
        "SELECT agent,run_id,state,updated_ms FROM controllers WHERE state IN ('waiting','running') AND updated_ms>? ORDER BY agent",
        now - crate::keepalive::CONTROLLER_TTL_MS,
        &|r| {
            let updated: i64 = r.get(3)?;
            Ok(json!({"agent":r.get::<_,String>(0)?,"run_id":r.get::<_,String>(1)?,"state":r.get::<_,String>(2)?,"updated_ms":updated,"expires_ms":updated+crate::keepalive::CONTROLLER_TTL_MS}))
        },
    )?;
    // Every keepalive that has not been stopped, and any stopped one whose
    // drive may still be running.
    let keepalives = rows(
        "SELECT agent,session,pid,stop_requested,started_ms FROM keepalives WHERE stop_requested=0 OR pid IS NOT NULL OR started_ms>? ORDER BY agent",
        now - crate::keepalive::STARTING_MS,
        &|r| {
            Ok(json!({"agent":r.get::<_,String>(0)?,"session":r.get::<_,String>(1)?,"pid":r.get::<_,Option<i64>>(2)?,"stop_requested":r.get::<_,i64>(3)? == 1,"started_ms":r.get::<_,i64>(4)?}))
        },
    )?;
    let waits = rows(
        "SELECT agent,session,refreshed_ms FROM session_waits WHERE refreshed_ms>? ORDER BY agent,session",
        now - crate::store::WAIT_FRESH_MS,
        &|r| {
            let refreshed: i64 = r.get(2)?;
            let session: String = r.get(1)?;
            Ok(json!({"agent":r.get::<_,String>(0)?,"session":(!session.is_empty()).then_some(session),"refreshed_ms":refreshed,"expires_ms":refreshed+crate::store::WAIT_FRESH_MS}))
        },
    )?;
    let sessions = rows(
        "SELECT agent,session,last_seen_ms FROM sessions WHERE ended_ms IS NULL AND last_seen_ms>? ORDER BY last_seen_ms DESC",
        now - crate::store::IDENTITY_TTL_MS,
        &|r| {
            Ok(json!({"agent":r.get::<_,String>(0)?,"session":r.get::<_,String>(1)?,"last_seen_ms":r.get::<_,i64>(2)?}))
        },
    )?;
    Ok(json!({
        "listeners":listeners,
        "controllers":controllers,
        "keepalives":keepalives,
        "waits":waits,
        "sessions":sessions,
    }))
}

/// Connection counts, as the caller sees them: its own connection excluded.
pub struct Clients {
    pub connected: usize,
    pub long_lived: usize,
}

/// Complete the report: check keepalive pids, add the open connections and
/// counts, and decide the verdict (see the module documentation).
pub fn finish(mut report: Value, open: Vec<Value>, clients: Clients, now: i64) -> Value {
    let mut reasons = Vec::new();
    let mut holders = BTreeSet::new();
    let mut hold = |agent: &str, reason: String| {
        if !agent.is_empty() {
            holders.insert(agent.to_owned());
        }
        reasons.push(reason);
    };
    for c in &open {
        let actor = c["actor"].as_str().unwrap_or("");
        hold(
            actor,
            format!(
                "open {} connection{}{}",
                c["op"].as_str().unwrap_or("?"),
                if actor.is_empty() {
                    String::new()
                } else {
                    format!(" by {actor:?}")
                },
                if c["keepalive"] == true {
                    " (keepalive drive)"
                } else {
                    ""
                }
            ),
        );
    }
    for l in report["listeners"].as_array().into_iter().flatten() {
        let agent = l["agent"].as_str().unwrap_or("");
        hold(agent, format!("live listener for {agent:?}"));
    }
    for c in report["controllers"].as_array().into_iter().flatten() {
        let agent = c["agent"].as_str().unwrap_or("");
        hold(
            agent,
            format!(
                "live drive for {agent:?} ({})",
                c["state"].as_str().unwrap_or("?")
            ),
        );
    }
    for k in report["keepalives"].as_array_mut().into_iter().flatten() {
        let alive = k["pid"].as_i64().map(crate::keepalive::pid_alive);
        let starting = k["pid"].is_null()
            && k["stop_requested"] == false
            && now - k["started_ms"].as_i64().unwrap_or(0) < crate::keepalive::STARTING_MS;
        k["pid_alive"] = json!(alive);
        let live = alive == Some(true) || starting;
        k["live"] = json!(live);
        if live {
            let agent = k["agent"].as_str().unwrap_or("").to_owned();
            hold(
                &agent,
                match k["pid"].as_i64() {
                    Some(pid) => format!("keepalive drive for {agent:?} (pid {pid})"),
                    None => format!("keepalive for {agent:?} starting"),
                },
            );
        }
    }
    for w in report["waits"].as_array().into_iter().flatten() {
        let agent = w["agent"].as_str().unwrap_or("");
        hold(
            agent,
            format!(
                "armed wait for {agent:?} until {}",
                w["expires_ms"].as_i64().unwrap_or(0)
            ),
        );
    }
    let busy = !reasons.is_empty();
    report["verdict"] = json!(if busy { "busy" } else { "idle" });
    report["reasons"] = json!(reasons);
    report["holders"] = json!(holders);
    report["clients"] = json!({"connected":clients.connected,"long_lived":clients.long_lived});
    report["long_lived"] = json!(open);
    report["windows_ms"] = json!({
        "listener":crate::attention::LISTENER_TTL_MS,
        "controller":crate::keepalive::CONTROLLER_TTL_MS,
        "keepalive_starting":crate::keepalive::STARTING_MS,
        "wait":crate::store::WAIT_FRESH_MS,
        "presence":crate::store::IDENTITY_TTL_MS,
    });
    report["time_ms"] = json!(now);
    report
}
