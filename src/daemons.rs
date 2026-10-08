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
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

/// Lists the daemons in the default registry; see [`report_in`].
pub fn report(stale_only: bool, scan: bool) -> Result<Value> {
    report_in(&registry::state_dir()?, stale_only, scan)
}

/// Lists the daemons registered under `state_dir`, pruning dead records.
///
/// With `scan`, also finds unregistered daemons from the process table (see
/// [`scan`]). With `stale_only`, keeps only daemons whose build differs from
/// this client's; pruned records are still counted in `pruned`.
pub fn report_in(state_dir: &Path, stale_only: bool, scan: bool) -> Result<Value> {
    let now = now_ms();
    let mut daemons = Vec::new();
    let mut pruned = 0;
    let mut registered = BTreeSet::new();
    for entry in registry::list_in(state_dir)? {
        registered.insert(entry.record.home.clone());
        let item = registered_item(&entry, now)?;
        if item["pruned"] == true {
            pruned += 1;
        }
        daemons.push(item);
    }
    let mut unconfirmed = Vec::new();
    if scan {
        let found = self::scan(&registered)?;
        daemons.extend(found.daemons.into_iter().map(|d| unregistered_item(d, now)));
        unconfirmed = found.unconfirmed;
    }
    if stale_only {
        daemons.retain(|d| d["stale"] == true);
    }
    let mut out = json!({
        "client":{"version":env!("CARGO_PKG_VERSION"),"build":BUILD,"protocol_version":PROTOCOL_VERSION},
        "state_dir":state_dir,
        "stale_only":stale_only,
        "daemons":daemons,
        "pruned":pruned,
    });
    if scan {
        out["scan"] = json!({"unconfirmed":unconfirmed});
    }
    Ok(out)
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

fn unregistered_item(daemon: Found, now: i64) -> Value {
    let mut item = json!({
        "home":daemon.home,
        "registered":false,
        "pid":daemon.pid,
        "exe":daemon.exe,
        "durability":if daemon.normal { "normal" } else { "full" },
        "started_ms":daemon.uptime_ms.map(|up| now - up),
        "uptime_ms":daemon.uptime_ms,
        "pruned":false,
    });
    describe_live(&mut item, &daemon.home, &daemon.ping);
    finish(&mut item);
    item
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

/// A daemon found in the process table and confirmed by a ping.
struct Found {
    home: PathBuf,
    pid: u32,
    exe: String,
    normal: bool,
    uptime_ms: Option<i64>,
    ping: Value,
}

struct Scan {
    daemons: Vec<Found>,
    /// Fray `serve` processes whose home could not be confirmed.
    unconfirmed: Vec<Value>,
}

/// Transitional discovery (L3) of daemons started before the registry
/// existed. It may be removed once a release with the registry has shipped
/// and every daemon has restarted onto it.
///
/// Reads `ps -axo pid=,etime=,command=` and keeps processes whose program is
/// `fray` (or `fray.<suffix>`, such as `fray.previous`) running `serve` with
/// `--home`. `ps` joins argv with spaces, so a home that contains spaces is
/// ambiguous: every split that could end the home is tried, and only a home
/// that answers a ping counts. Homes in `skip` (already registered) are left
/// out. Not found: daemons whose home came from `FRAY_HOME` or a relative
/// `--home`, since `ps` shows neither; they are listed as unconfirmed when the
/// process is visible.
fn scan(skip: &BTreeSet<PathBuf>) -> Result<Scan> {
    let out = Command::new("ps")
        .args(["-ww", "-axo", "pid=,etime=,command="])
        .output()?;
    if !out.status.success() {
        return Err(Error::new(
            "unavailable",
            format!("ps failed: {}", String::from_utf8_lossy(&out.stderr).trim()),
        ));
    }
    let own = std::process::id();
    let mut seen = skip.clone();
    let mut result = Scan {
        daemons: vec![],
        unconfirmed: vec![],
    };
    for line in String::from_utf8_lossy(&out.stdout).lines() {
        let Some((pid, etime, command)) = split_ps_line(line) else {
            continue;
        };
        if pid == own {
            continue;
        }
        let Some(serve) = parse_serve(command) else {
            continue;
        };
        let mut confirmed = None;
        for candidate in &serve.homes {
            let path = Path::new(candidate);
            if !path.is_absolute() || !path.join("bus.sock").exists() {
                continue;
            }
            let home = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            if let Ok(ping) = client::rpc(&home, &Request::new("ping", "", json!({})), 1) {
                confirmed = Some((home, ping));
                break;
            }
        }
        match confirmed {
            Some((home, ping)) => {
                if seen.insert(home.clone()) {
                    result.daemons.push(Found {
                        home,
                        pid,
                        exe: serve.exe,
                        normal: serve.normal,
                        uptime_ms: etime,
                        ping,
                    });
                }
            }
            None => result
                .unconfirmed
                .push(json!({"pid":pid,"command":command})),
        }
    }
    result.daemons.sort_by(|a, b| a.home.cmp(&b.home));
    Ok(result)
}

/// `<pid> <etime> <command>` from one `ps` row.
fn split_ps_line(line: &str) -> Option<(u32, Option<i64>, &str)> {
    let line = line.trim_start();
    let (pid, rest) = line.split_once(char::is_whitespace)?;
    let rest = rest.trim_start();
    let (etime, command) = rest.split_once(char::is_whitespace)?;
    Some((pid.parse().ok()?, parse_etime(etime), command.trim()))
}

/// `ps` elapsed time, `[[dd-]hh:]mm:ss`, in milliseconds.
fn parse_etime(etime: &str) -> Option<i64> {
    let (days, clock) = match etime.split_once('-') {
        Some((d, c)) => (d.parse::<i64>().ok()?, c),
        None => (0, etime),
    };
    let mut secs = 0i64;
    let parts: Vec<_> = clock.split(':').collect();
    if !(2..=3).contains(&parts.len()) {
        return None;
    }
    for part in parts {
        secs = secs * 60 + part.parse::<i64>().ok()?;
    }
    Some((days * 86_400 + secs) * 1000)
}

struct Serve {
    exe: String,
    /// Candidate homes in order, shortest first.
    homes: Vec<String>,
    normal: bool,
}

/// Recognizes `fray ... --home <H> ... serve` in a space-joined command line.
fn parse_serve(command: &str) -> Option<Serve> {
    // A word boundary where an argument may start: the program name ends at
    // the first flag or at the subcommand.
    let starts = |s: &str| s.starts_with('-') || is_word(s, "serve");
    let boundaries: Vec<usize> = command
        .match_indices(' ')
        .map(|(i, _)| i)
        .filter(|&i| starts(&command[i + 1..]))
        .collect();
    let (&first, _) = boundaries.split_first()?;
    let exe = &command[..first];
    let name = exe.rsplit('/').next().unwrap_or(exe);
    if name != "fray" && !name.starts_with("fray.") {
        return None;
    }
    let args = &command[first..];
    if !args.split(' ').any(|word| word == "serve") {
        return None;
    }
    let start = [" --home=", " --home "]
        .iter()
        .filter_map(|flag| args.find(flag).map(|i| i + flag.len()))
        .min()?;
    let tail = &args[start..];
    let mut homes = Vec::new();
    for (i, _) in tail.match_indices(' ') {
        if starts(&tail[i + 1..]) {
            homes.push(tail[..i].to_owned());
        }
    }
    homes.push(tail.to_owned());
    homes.retain(|h| !h.is_empty());
    homes.dedup();
    Some(Serve {
        exe: exe.to_owned(),
        homes,
        normal: args.split(' ').any(|word| word == "--normal"),
    })
}

fn is_word(s: &str, word: &str) -> bool {
    s.strip_prefix(word)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with(' '))
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
        if d["registered"] == false {
            out.push_str(" (unregistered)");
        }
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
        } else if report["scan"].is_object() {
            "No daemons found.\n"
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
    if let Some(unconfirmed) = report["scan"]["unconfirmed"]
        .as_array()
        .filter(|u| !u.is_empty())
    {
        out.push_str(
            "Fray serve processes no ping confirmed (FRAY_HOME, a relative --home, or not answering):\n",
        );
        for p in unconfirmed {
            out.push_str(&format!("  pid {}: {}\n", p["pid"], clean(&p["command"])));
        }
    } else if !report["scan"].is_object() {
        out.push_str(
            "Daemons started before the registry are not listed; add --scan to find them.\n",
        );
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
    fn serve_lines_are_recognized_only_for_fray() {
        let s = parse_serve("/Users/me/.cargo/bin/fray --home /tmp/h serve").unwrap();
        assert_eq!(s.exe, "/Users/me/.cargo/bin/fray");
        assert_eq!(s.homes, ["/tmp/h", "/tmp/h serve"]);
        assert!(!s.normal);
        let s = parse_serve("fray.previous --home=/tmp/h serve --normal").unwrap();
        assert_eq!(s.homes[0], "/tmp/h");
        assert!(s.normal);
        // Global flags may follow the subcommand; a home may hold spaces.
        let s = parse_serve("/opt/my tools/fray serve --home /tmp/a b c --as x").unwrap();
        assert_eq!(s.exe, "/opt/my tools/fray");
        assert_eq!(s.homes, ["/tmp/a b c", "/tmp/a b c --as x"]);
        for other in [
            "python3 -m http.server --home /tmp/h serve",
            "/bin/sh -c read fray --home /tmp/h serve",
            "/usr/bin/frayed --home /tmp/h serve",
            "fray --home /tmp/h start",
            "fray serve",
            "/Users/me/.cargo/bin/fray --home /tmp/h wait --timeout 60",
        ] {
            assert!(parse_serve(other).is_none(), "{other}");
        }
    }

    #[test]
    fn ps_rows_and_elapsed_times_parse() {
        assert_eq!(parse_etime("05:07"), Some(307_000));
        assert_eq!(parse_etime("01:00:00"), Some(3_600_000));
        assert_eq!(parse_etime("2-00:00:01"), Some(172_801_000));
        assert_eq!(parse_etime("x"), None);
        let (pid, etime, command) =
            split_ps_line("  4242   1-02:03:04 /bin/fray --home /a b serve").unwrap();
        assert_eq!(pid, 4242);
        assert_eq!(etime, Some(93_784_000));
        assert_eq!(command, "/bin/fray --home /a b serve");
        assert!(split_ps_line("garbage").is_none());
    }

    #[test]
    fn restart_command_quotes_the_home() {
        assert_eq!(
            restart_command(Path::new("/tmp/it's here")),
            "fray --home '/tmp/it'\\''s here' restart"
        );
    }
}
