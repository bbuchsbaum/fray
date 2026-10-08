//! `fray daemons`: what is running and is it current
//! (docs/plans/2026-10-08-daemon-lifecycle.md, L2).
//!
//! Reads the advisory registry ([`crate::registry`]), confirms every home by a
//! ping, and compares each daemon's build with this client's [`BUILD`]. It
//! never starts or stops anything. The only write is pruning the record of a
//! dead daemon, which the report names.
//!
//! The state of each home is one of:
//!
//! - `running`: it answered a ping with this client's protocol;
//! - `incompatible`: it answered with another protocol version;
//! - `unreachable`: the socket accepted but gave no clean answer, so it may be
//!   alive (never pruned);
//! - `dead`: nothing accepts on its socket.
//!
//! A daemon is `stale` when its build differs from the client's. The client is
//! the comparison point because it is the binary the owner just installed.
use crate::{
    client,
    model::*,
    registry::{self, Entry, Liveness},
};
use serde_json::{json, Value};
use std::path::Path;

/// Lists the daemons in the default registry; see [`report_in`].
pub fn report(stale_only: bool) -> Result<Value> {
    report_in(&registry::state_dir()?, stale_only)
}

/// Lists the daemons registered under `state_dir`, pruning dead records.
///
/// With `stale_only`, keeps only daemons whose build differs from
/// this client's; pruned records are still counted in `pruned`.
pub fn report_in(state_dir: &Path, stale_only: bool) -> Result<Value> {
    let now = now_ms();
    let mut daemons = Vec::new();
    let mut pruned = 0;
    for entry in registry::list_in(state_dir)? {
        let item = registered_item(&entry, now)?;
        if item["pruned"] == true {
            pruned += 1;
        }
        daemons.push(item);
    }
    if stale_only {
        daemons.retain(|d| d["stale"] == true);
    }
    Ok(json!({
        "client":{"version":env!("CARGO_PKG_VERSION"),"build":BUILD,"protocol_version":PROTOCOL_VERSION},
        "state_dir":state_dir,
        "stale_only":stale_only,
        "daemons":daemons,
        "pruned":pruned,
    }))
}

/// The copyable command that restarts the daemon on `home` (`fray restart`, L9).
pub fn restart_command(home: &Path) -> String {
    let home = home.to_string_lossy().replace('\'', "'\\''");
    format!("fray --home '{home}' restart")
}

fn registered_item(entry: &Entry, now: i64) -> Result<Value> {
    let record = &entry.record;
    let mut item = json!({
        "home":record.home,
        "registered":true,
        "pid":record.pid,
        "version":record.version,
        "build":record.build,
        "protocol_version":record.protocol_version,
        "exe":record.exe,
        "durability":record.durability,
        "started_ms":record.started_ms,
        "uptime_ms":now.saturating_sub(record.started_ms).max(0),
        "pruned":false,
    });
    match entry.liveness {
        Liveness::Running => {
            let ping = entry.ping.clone().unwrap_or_default();
            describe_live(&mut item, &record.home, &ping);
        }
        Liveness::Unknown => {
            // Only the record describes it; its build may still show skew.
            item["state"] = json!("unreachable");
            item["stale"] = json!(record.build != BUILD);
            item["occupancy"] = Value::Null;
        }
        Liveness::Dead => {
            item["state"] = json!("dead");
            item["stale"] = Value::Null;
            item["occupancy"] = Value::Null;
            item["uptime_ms"] = Value::Null;
            item["pruned"] = json!(registry::prune(entry)?);
        }
    }
    finish(&mut item);
    Ok(item)
}

/// Fills in what a ping says about the daemon that owns `home` now, and its
/// occupancy when it offers that op.
fn describe_live(item: &mut Value, home: &Path, ping: &Value) {
    for field in ["version", "build", "protocol_version"] {
        item[field] = ping[field].clone();
    }
    let compatible = ping["protocol_version"].as_u64() == Some(u64::from(PROTOCOL_VERSION));
    item["state"] = json!(if compatible {
        "running"
    } else {
        "incompatible"
    });
    item["stale"] = json!(ping["build"].as_str() != Some(BUILD));
    item["occupancy"] = Value::Null;
    let offers = ping["capabilities"]
        .as_array()
        .is_some_and(|caps| caps.iter().any(|c| c == "occupancy"));
    if compatible && offers {
        match client::rpc(home, &Request::new("occupancy", "", json!({})), 5) {
            Ok(o) => {
                item["occupancy"] =
                    json!({"verdict":o["verdict"],"holders":o["holders"],"clients":o["clients"]});
            }
            Err(error) => item["occupancy_error"] = json!(error),
        }
    }
}

fn finish(item: &mut Value) {
    let restartable = item["stale"] == true && item["state"] != "dead";
    item["restart"] = if restartable {
        json!(restart_command(Path::new(
            item["home"].as_str().unwrap_or("")
        )))
    } else {
        Value::Null
    };
}

/// The compact human report.
pub fn render(report: &Value) -> String {
    let mut out = format!(
        "Client build {} ({})\n",
        clean(&report["client"]["build"]),
        clean(&report["client"]["version"])
    );
    let daemons = report["daemons"].as_array().map_or(&[][..], Vec::as_slice);
    let (mut running, mut stale) = (0, 0);
    for d in daemons {
        let state = d["state"].as_str().unwrap_or("?");
        if matches!(state, "running" | "incompatible") {
            running += 1;
        }
        if d["stale"] == true {
            stale += 1;
        }
        out.push_str(&format!("{}\n  {state}", clean(&d["home"])));
        out.push_str(&format!("  pid {}", d["pid"]));
        if state == "dead" {
            out.push_str(if d["pruned"] == true {
                "  record pruned\n"
            } else {
                "  record kept\n"
            });
            continue;
        }
        out.push_str(&format!("  build {}", clean(&d["build"])));
        if d["stale"] == true {
            out.push_str(" STALE");
        }
        if state == "incompatible" {
            out.push_str(&format!(" (protocol {})", d["protocol_version"]));
        }
        if let Some(up) = d["uptime_ms"].as_i64() {
            out.push_str(&format!("  up {}", duration(up)));
        }
        let o = &d["occupancy"];
        if o.is_object() {
            let holders: Vec<_> = o["holders"]
                .as_array()
                .into_iter()
                .flatten()
                .map(clean)
                .collect();
            out.push_str(&format!("  {}", clean(&o["verdict"])));
            if !holders.is_empty() {
                out.push_str(&format!(": {}", holders.join(", ")));
            }
            let n = o["clients"]["connected"].as_u64().unwrap_or(0);
            out.push_str(&format!(" ({n} client{})", if n == 1 { "" } else { "s" }));
        } else if let Some(code) = d["occupancy_error"]["code"].as_str() {
            out.push_str(&format!("  occupancy failed ({})", clean_str(code)));
        } else if state == "running" {
            out.push_str("  occupancy not offered (older daemon)");
        }
        out.push('\n');
        if let Some(command) = d["restart"].as_str() {
            out.push_str(&format!("  restart: {}\n", clean_str(command)));
        }
    }
    let pruned = report["pruned"].as_u64().unwrap_or(0);
    if daemons.is_empty() {
        out.push_str(if report["stale_only"] == true {
            "No stale daemons.\n"
        } else {
            "No registered daemons.\n"
        });
    } else {
        out.push_str(&format!("{running} running, {stale} stale"));
        if pruned > 0 {
            out.push_str(&format!(", {pruned} dead record(s) pruned"));
        }
        out.push_str(".\n");
    }
    out
}

fn duration(ms: i64) -> String {
    let s = ms / 1000;
    let (d, h, m) = (s / 86_400, s / 3600 % 24, s / 60 % 60);
    if d > 0 {
        format!("{d}d{h}h")
    } else if h > 0 {
        format!("{h}h{m}m")
    } else if m > 0 {
        format!("{m}m")
    } else {
        format!("{s}s")
    }
}

fn clean(v: &Value) -> String {
    match v {
        Value::String(s) => clean_str(s),
        Value::Null => "?".into(),
        other => clean_str(&other.to_string()),
    }
}

fn clean_str(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restart_command_quotes_the_home() {
        assert_eq!(
            restart_command(Path::new("/tmp/it's here")),
            "fray --home '/tmp/it'\\''s here' restart"
        );
    }
}
