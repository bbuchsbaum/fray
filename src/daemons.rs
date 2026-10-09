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
//!
//! Daemons are asked with [`client::probe`]: one request each, no reconnect
//! wait and no stderr notes, all within [`BUDGET`]. A daemon reached after the
//! budget is spent is listed without occupancy rather than stalling the report.
//!
//! Occupancy is shown as the restart preflight reads it
//! ([`crate::restart::classify`]): `idle`, `busy` or `armed`, with live
//! holders, armed waiters and keepalive drives that would survive a restart.
//! The `occupancy` op's own verdict, which counts every live keepalive and
//! armed wait as busy, stays in the JSON under `occupancy`; the reading
//! shown is under `preflight`.
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
    time::{Duration, Instant},
};

/// The whole report's time for pings and occupancy beyond the registry's own
/// pings. One wedged daemon costs at most a per-request timeout.
pub const BUDGET: Duration = Duration::from_secs(10);

/// Limits `--scan` to homes under a directory; see [`scan`].
pub const SCAN_ROOT: &str = "FRAY_SCAN_ROOT";

/// Lists the daemons in the default registry; see [`report_in`].
pub fn report(stale_only: bool, scan: bool) -> Result<Value> {
    report_in(&registry::state_dir()?, stale_only, scan)
}

/// Lists the daemons registered under `state_dir`, pruning dead records.
///
/// With `scan`, also finds unregistered daemons from the process table (see
/// [`scan`]). With `stale_only`, keeps only daemons whose build differs from
/// this client's, plus records pruned now (each is reported exactly once).
pub fn report_in(state_dir: &Path, stale_only: bool, scan: bool) -> Result<Value> {
    report_against(state_dir, stale_only, scan, BUILD)
}

/// [`report_in`], with `stale` meaning a build other than `build` (say the
/// binary `fray restart --all-stale --exe` would start) rather than this
/// client's. The report names it as `compared_build`.
pub fn report_against(
    state_dir: &Path,
    stale_only: bool,
    scan: bool,
    build: &str,
) -> Result<Value> {
    let now = now_ms();
    let deadline = Instant::now() + BUDGET;
    let mut daemons = Vec::new();
    let mut pruned = 0;
    let mut registered = BTreeSet::new();
    for entry in registry::list_in(state_dir)? {
        registered.insert(entry.record.home.clone());
        let item = registered_item(&entry, build, now, deadline)?;
        if item["pruned"] == true {
            pruned += 1;
        }
        daemons.push(item);
    }
    let mut unconfirmed = Vec::new();
    if scan {
        let found = self::scan(&registered, deadline)?;
        daemons.extend(
            found
                .daemons
                .into_iter()
                .map(|d| unregistered_item(d, build, now, deadline)),
        );
        unconfirmed = found.unconfirmed;
    }
    if stale_only {
        // A pruned record is reported once, whatever the filter.
        daemons.retain(|d| d["stale"] == true || d["pruned"] == true);
    }
    let mut out = json!({
        "client":{"version":env!("CARGO_PKG_VERSION"),"build":BUILD,"protocol_version":PROTOCOL_VERSION},
        "compared_build":build,
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

fn registered_item(entry: &Entry, build: &str, now: i64, deadline: Instant) -> Result<Value> {
    let record = &entry.record;
    let mut item = json!({
        "home":record.home,
        "registered":true,
        "pid":record.pid,
        "pid_mismatch":Value::Null,
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
            describe_live(&mut item, &record.home, &ping, build, deadline);
            // The ping describes whoever serves the home now. A record left
            // by another process (say a crash, then a pre-registry daemon on
            // the same home) is stale: its pid, exe and start time are not
            // this daemon's. Older daemons do not send a pid.
            if let Some(pid) = ping["pid"].as_u64() {
                let mismatch = pid != u64::from(record.pid);
                item["pid_mismatch"] = json!(mismatch);
                if mismatch {
                    item["record_pid"] = json!(record.pid);
                    item["pid"] = json!(pid);
                    for field in ["exe", "started_ms", "uptime_ms"] {
                        item[field] = Value::Null;
                    }
                    item["durability"] = json!("unknown");
                }
            }
        }
        Liveness::Unknown => {
            // Only the record describes it; its build may still show skew.
            item["state"] = json!("unreachable");
            item["stale"] = json!(record.build != build);
            item["occupancy"] = Value::Null;
            item["occupancy_error"] = Value::Null;
            item["preflight"] = Value::Null;
        }
        Liveness::Dead => {
            item["state"] = json!("dead");
            item["stale"] = Value::Null;
            item["occupancy"] = Value::Null;
            item["occupancy_error"] = Value::Null;
            item["preflight"] = Value::Null;
            item["uptime_ms"] = Value::Null;
            item["pruned"] = json!(registry::prune(entry)?);
        }
    }
    finish(&mut item);
    Ok(item)
}

fn unregistered_item(daemon: Found, build: &str, now: i64, deadline: Instant) -> Value {
    let mut item = json!({
        "home":daemon.home,
        "registered":false,
        "pid":daemon.pid,
        "pid_mismatch":daemon.ping["pid"].as_u64().map(|pid| pid != u64::from(daemon.pid)),
        // Read from `ps`: the space-joined argv, not a resolved executable.
        "exe":Value::Null,
        "command":daemon.command,
        "durability":"unknown",
        "started_ms":daemon.uptime_ms.map(|up| now - up),
        "uptime_ms":daemon.uptime_ms,
        "pruned":false,
    });
    describe_live(&mut item, &daemon.home, &daemon.ping, build, deadline);
    finish(&mut item);
    item
}

/// Fills in what a ping says about the daemon that owns `home` now, and its
/// occupancy when it offers that op.
fn describe_live(item: &mut Value, home: &Path, ping: &Value, build: &str, deadline: Instant) {
    for field in ["version", "build", "protocol_version"] {
        item[field] = ping[field].clone();
    }
    let compatible = ping["protocol_version"].as_u64() == Some(u64::from(PROTOCOL_VERSION));
    item["state"] = json!(if compatible {
        "running"
    } else {
        "incompatible"
    });
    item["stale"] = json!(ping["build"].as_str() != Some(build));
    item["occupancy"] = Value::Null;
    item["occupancy_error"] = Value::Null;
    item["preflight"] = Value::Null;
    let offers = ping["capabilities"]
        .as_array()
        .is_some_and(|caps| caps.iter().any(|c| c == "occupancy"));
    if !(compatible && offers) {
        return;
    }
    if Instant::now() >= deadline {
        item["occupancy_error"] = json!(Error::new(
            "deadline",
            "not asked: the report's time budget was spent"
        ));
        return;
    }
    match client::probe(home, &Request::new("occupancy", "", json!({})), 2) {
        Ok(o) => {
            item["occupancy"] =
                json!({"verdict":o["verdict"],"holders":o["holders"],"clients":o["clients"]});
            item["preflight"] = reading(&crate::restart::classify(home, ping.clone(), o));
        }
        Err(error) => item["occupancy_error"] = json!(error),
    }
}

/// The preflight's reading, compactly: its verdict and the actors behind each
/// class, each named once.
fn reading(pre: &crate::restart::Preflight) -> Value {
    let names = |items: Vec<&crate::restart::Interruption>| {
        let names: BTreeSet<&str> = items
            .into_iter()
            .map(|i| i.actor.as_str())
            .filter(|a| !a.is_empty())
            .collect();
        json!(names)
    };
    json!({
        "verdict":pre.verdict.as_str(),
        "live":names(pre.live().collect()),
        "armed":names(pre.armed().collect()),
        "survives":names(pre.survives().collect()),
    })
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
    command: String,
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
/// Reads this user's processes from `ps -ww -x -U <uid> -o pid=,etime=,command=`
/// and keeps those whose program is an existing file named `fray` (or
/// `fray.<suffix>`, such as `fray.previous`) running `serve` with `--home`. A
/// wrapper (`timeout 60 fray ...`, `sh script fray ...`) is not that file, so
/// only the daemon process itself is reported. `ps` joins argv with spaces, so a home that contains spaces is
/// ambiguous: every split that could end the home is tried, and only a home
/// that answers a ping counts. Homes in `skip` (already registered) are left
/// out before any ping. Not found: daemons whose home came from `FRAY_HOME` or a relative
/// `--home`, since `ps` shows neither; they are listed as unconfirmed when the
/// process is visible.
///
/// `FRAY_SCAN_ROOT=DIR` keeps only processes with a candidate home under
/// `DIR` (compared canonically) and drops the rest unreported. It filters
/// the process table only, never registry entries (`FRAY_STATE_DIR` picks
/// the registry). It exists so tests (and anyone
/// rehearsing `fray restart --all-stale --scan`) can scan scratch homes
/// without touching the other daemons on the machine.
fn scan(skip: &BTreeSet<PathBuf>, deadline: Instant) -> Result<Scan> {
    // Both sides canonical, so `<root>/../elsewhere` is not under the root;
    // a root or candidate that cannot be resolved is not under it either.
    let root = std::env::var_os(SCAN_ROOT)
        .filter(|r| !r.is_empty())
        .map(|r| fs::canonicalize(PathBuf::from(r)).ok());
    let under_root = |candidate: &str| match &root {
        None => true,
        Some(None) => false,
        Some(Some(root)) => fs::canonicalize(candidate).is_ok_and(|p| p.starts_with(root)),
    };
    let uid = Command::new("id").arg("-u").output()?;
    let uid = String::from_utf8_lossy(&uid.stdout).trim().to_owned();
    if uid.is_empty() || !uid.bytes().all(|b| b.is_ascii_digit()) {
        return Err(Error::new("unavailable", "id -u gave no user id"));
    }
    let out = Command::new("ps")
        .args(["-ww", "-x", "-U", &uid, "-o", "pid=,etime=,command="])
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
        let Some(serve) = parse_serve(command, is_program) else {
            continue;
        };
        if !serve.homes.iter().any(|h| under_root(h)) {
            continue;
        }
        let mut confirmed = None;
        let mut known = false;
        let mut reason = "no candidate home answered a ping";
        for candidate in &serve.homes {
            let path = Path::new(candidate);
            if !path.is_absolute() || !path.join("bus.sock").exists() || !under_root(candidate) {
                continue;
            }
            let home = fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
            if seen.contains(&home) {
                // Registered, or already found through another process.
                known = true;
                break;
            }
            if Instant::now() >= deadline {
                reason = "not asked: the report's time budget was spent";
                break;
            }
            if let Ok(ping) = client::probe(&home, &Request::new("ping", "", json!({})), 1) {
                confirmed = Some((home, ping));
                break;
            }
        }
        match confirmed {
            Some((home, ping)) => {
                seen.insert(home.clone());
                result.daemons.push(Found {
                    home,
                    pid,
                    command: command.to_owned(),
                    uptime_ms: etime,
                    ping,
                });
            }
            None if known => {}
            None => result
                .unconfirmed
                .push(json!({"pid":pid,"command":command,"reason":reason})),
        }
    }
    result.daemons.sort_by(|a, b| a.home.cmp(&b.home));
    Ok(result)
}

/// This user's `fray ... --home <H> ... serve` processes whose home could be
/// the canonical `home`, from the same `ps` listing as [`scan`] but without
/// pinging anything. For `fray restart` on a daemon that reports no pid
/// (0.2.1 and earlier) and has no registry record. A process whose home `ps`
/// cannot show (`FRAY_HOME`, a relative `--home`) is not found.
pub fn serving_pids(home: &Path) -> Vec<u32> {
    let Ok(uid) = Command::new("id").arg("-u").output() else {
        return vec![];
    };
    let uid = String::from_utf8_lossy(&uid.stdout).trim().to_owned();
    let Ok(out) = Command::new("ps")
        .args(["-ww", "-x", "-U", &uid, "-o", "pid=,etime=,command="])
        .output()
    else {
        return vec![];
    };
    let own = std::process::id();
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(split_ps_line)
        .filter(|(pid, _, _)| *pid != own)
        .filter(|(_, _, command)| {
            parse_serve(command, is_program).is_some_and(|serve| {
                serve.homes.iter().any(|candidate| {
                    let path = Path::new(candidate);
                    path.is_absolute()
                        && fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()) == home
                })
            })
        })
        .map(|(pid, _, _)| pid)
        .collect()
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
    /// Candidate homes in order, shortest first.
    homes: Vec<String>,
}

/// Whether `exe`, a program as `ps` shows it, is an existing file: a path, or
/// a bare name found on `PATH`.
fn is_program(exe: &str) -> bool {
    if exe.contains('/') {
        return Path::new(exe).is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(exe).is_file()))
}

/// Recognizes `fray ... --home <H> ... serve` in a space-joined command line,
/// where the program must satisfy `is_program`.
fn parse_serve(command: &str, is_program: impl Fn(&str) -> bool) -> Option<Serve> {
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
    if (name != "fray" && !name.starts_with("fray.")) || !is_program(exe) {
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
    Some(Serve { homes })
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
        if d["pid_mismatch"] == true {
            out.push_str(&format!(" (stale record names pid {})", d["record_pid"]));
        }
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
        let p = &d["preflight"];
        if o.is_object() && p.is_object() {
            let names = |key: &str| -> Vec<String> {
                p[key].as_array().into_iter().flatten().map(clean).collect()
            };
            let verdict = clean(&p["verdict"]);
            out.push_str(&format!("  {verdict}"));
            let holders = names(if verdict == "armed" { "armed" } else { "live" });
            if !holders.is_empty() {
                out.push_str(&format!(": {}", holders.join(", ")));
            }
            let mut notes = Vec::new();
            let survivors = names("survives").len();
            if survivors > 0 {
                notes.push(format!(
                    "{survivors} keepalive{} survive{}",
                    if survivors == 1 { "" } else { "s" },
                    if survivors == 1 { "s" } else { "" }
                ));
            }
            let n = o["clients"]["connected"].as_u64().unwrap_or(0);
            notes.push(format!("{n} client{}", if n == 1 { "" } else { "s" }));
            out.push_str(&format!(" ({})", notes.join(", ")));
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
            "Fray serve processes not confirmed (FRAY_HOME or a relative --home hide the home):\n",
        );
        for p in unconfirmed {
            out.push_str(&format!(
                "  pid {}: {} ({})\n",
                p["pid"],
                clean(&p["command"]),
                clean(&p["reason"])
            ));
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
        let any = |_: &str| true;
        let s = parse_serve("/Users/me/.cargo/bin/fray --home /tmp/h serve", any).unwrap();
        assert_eq!(s.homes, ["/tmp/h", "/tmp/h serve"]);
        let s = parse_serve("fray.previous --home=/tmp/h serve --normal", any).unwrap();
        assert_eq!(s.homes[0], "/tmp/h");
        // Global flags may follow the subcommand; the program and the home
        // may hold spaces.
        let program = |exe: &str| exe == "/opt/my tools/fray";
        let s = parse_serve("/opt/my tools/fray serve --home /tmp/a b c --as x", program).unwrap();
        assert_eq!(s.homes, ["/tmp/a b c", "/tmp/a b c --as x"]);
        for other in [
            "python3 -m http.server --home /tmp/h serve",
            "/bin/sh -c read fray --home /tmp/h serve",
            "/usr/bin/frayed --home /tmp/h serve",
            "fray --home /tmp/h start",
            "fray serve",
            "/Users/me/.cargo/bin/fray --home /tmp/h wait --timeout 60",
        ] {
            assert!(parse_serve(other, any).is_none(), "{other}");
        }
        // A wrapper's argv ends in a word named fray, but the program `ps`
        // shows is the wrapper plus its arguments: not an existing file.
        let real = |exe: &str| exe == "/bin/fray";
        assert!(parse_serve("/bin/fray --home /tmp/h serve", real).is_some());
        for wrapper in [
            "timeout 60 /bin/fray --home /tmp/h serve",
            "/usr/bin/caffeinate -i /bin/fray --home /tmp/h serve",
            "/bin/sh wait.sh /bin/fray --home /tmp/h serve",
        ] {
            assert!(parse_serve(wrapper, real).is_none(), "{wrapper}");
        }
        assert!(is_program("/bin/sh") && is_program("sh"));
        assert!(!is_program("/bin/sh wait.sh /bin/fray"));
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
