//! `fray restart --all-stale` (docs/plans/2026-10-08-daemon-lifecycle.md, L10):
//! every out-of-date daemon in the registry, one at a time, through the
//! single-home restart ([`crate::restart::run`]).
//!
//! The candidates are `fray daemons --stale` ([`crate::daemons`]; with `scan`,
//! also unregistered daemons confirmed by a ping), where stale means a build
//! other than the binary being started (`--exe`, else this `fray`). Only a home
//! in state `running` is restarted. One that is `unreachable`, `incompatible`
//! or `dead` is listed as skipped with the reason and never touched. A busy
//! home is skipped unless `force` was given, and an armed one unless
//! `allow_armed` (or `force`) was; neither is ever implied. Old daemons read
//! in degraded mode often look armed (an agent seen within two minutes), so
//! their hint names `--allow-armed`.
//!
//! # Outcomes
//!
//! Each result's `outcome` is one of `restarted`, `started` (its daemon had
//! gone by the time it was reached), `skipped_busy`, `skipped_armed`,
//! `skipped_state` or `failed`. A dry run reads each preflight instead and
//! reports `would_restart`, `would_start` or `would_fail` (a daemon that
//! cannot post the notice, without `--no-announce`) in place of the first
//! three, and the same skips.
//!
//! # Exit status
//!
//! 0 when every stale daemon was restarted (or would be), or none is stale;
//! 1 when any failed (or would fail); otherwise 4, when at least one was
//! skipped. A dead record (pruned by the listing) is listed but does not
//! count as a skip: there was no daemon to restart.
//!
//! # `--json`
//!
//! `{client_build, target_build, exe, dry_run, scan, results, counts,
//! unconfirmed, exit_code}`. Each result is `{home, outcome, registered,
//! state, build, pid}` plus, by outcome: `restart` (the single-home restart's
//! JSON), `refusal` (its refusal JSON) with `holders` and `hint`, `reason`
//! for a state skip, `error` for a failure, and `preflight` in a dry run.
//! `counts` has every outcome above as a key.
use crate::{
    model::*,
    restart::{self, Class, Options, Outcome, Preflight, Verdict},
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const OUTCOMES: [&str; 9] = [
    "restarted",
    "started",
    "would_restart",
    "would_start",
    "skipped_busy",
    "skipped_armed",
    "skipped_state",
    "would_fail",
    "failed",
];

/// Restart (or with `dry_run`, preflight) every stale daemon registered under
/// `state_dir`, in home order. With `progress`, names each home on stderr
/// before acting on it.
pub fn run(
    state_dir: &Path,
    scan: bool,
    dry_run: bool,
    opts: &Options,
    progress: bool,
) -> Result<Value> {
    let exe = match &opts.exe {
        Some(exe) => std::fs::canonicalize(exe)
            .map_err(|e| Error::invalid(format!("--exe {}: {e}", exe.display())))?,
        None => std::env::current_exe()?,
    };
    let target = restart::exe_build(&exe)?;
    let listing = crate::daemons::report_against(state_dir, true, scan, &target)?;
    let mut results = Vec::new();
    for d in listing["daemons"].as_array().into_iter().flatten() {
        let home = PathBuf::from(d["home"].as_str().unwrap_or(""));
        let mut result = json!({
            "home":home,
            "registered":d["registered"],
            "state":d["state"],
            "build":d["build"],
            "pid":d["pid"],
        });
        let state = d["state"].as_str().unwrap_or("?");
        if state != "running" {
            result["outcome"] = json!("skipped_state");
            result["reason"] = json!(state_reason(state, d));
            results.push(result);
            continue;
        }
        if progress {
            let verb = if dry_run { "checking" } else { "restarting" };
            eprintln!("fray restart --all-stale: {verb} {}", home.display());
        }
        if dry_run {
            match restart::preflight(&home) {
                Ok(pre) => {
                    let outcome = predict(&pre, opts);
                    result["outcome"] = json!(outcome);
                    if outcome.starts_with("skipped") {
                        refused(&mut result, &pre);
                    } else if outcome == "would_fail" {
                        result["error"] = json!(announce_unsupported(&pre));
                    }
                    result["preflight"] = pre.to_json();
                }
                Err(e) => {
                    result["outcome"] = json!("failed");
                    result["error"] = json!(e);
                }
            }
        } else {
            match restart::run(&home, opts) {
                Ok(Outcome::Done(v)) => {
                    result["outcome"] = v["action"].clone();
                    result["restart"] = v;
                }
                Ok(Outcome::Refused(pre)) => {
                    result["outcome"] = json!(if pre.verdict == Verdict::Busy {
                        "skipped_busy"
                    } else {
                        "skipped_armed"
                    });
                    refused(&mut result, &pre);
                    result["refusal"] = Outcome::Refused(pre).to_json();
                }
                Err(e) => {
                    result["outcome"] = json!("failed");
                    result["error"] = json!(e);
                }
            }
        }
        results.push(result);
    }
    let mut counts = json!({});
    for outcome in OUTCOMES {
        counts[outcome] = json!(results.iter().filter(|r| r["outcome"] == outcome).count());
    }
    let mut out = json!({
        "client_build":BUILD,
        "target_build":target,
        "exe":exe,
        "dry_run":dry_run,
        "scan":scan,
        "results":results,
        "counts":counts,
        "unconfirmed":listing["scan"]["unconfirmed"].as_array().cloned().unwrap_or_default(),
    });
    out["exit_code"] = json!(exit_code(&out));
    Ok(out)
}

/// What the restart would do with this preflight under `opts`.
fn predict(pre: &Preflight, opts: &Options) -> &'static str {
    match pre.verdict {
        Verdict::Busy if !opts.force => "skipped_busy",
        Verdict::Armed if !(opts.force || opts.allow_armed) => "skipped_armed",
        Verdict::NotRunning => "would_start",
        _ if !opts.no_announce
            && !pre
                .daemon
                .as_ref()
                .is_some_and(|ping| restart::has(ping, "announce")) =>
        {
            "would_fail"
        }
        _ => "would_restart",
    }
}

fn announce_unsupported(pre: &Preflight) -> Error {
    let build = pre
        .daemon
        .as_ref()
        .and_then(|d| d["build"].as_str())
        .unwrap_or("unknown");
    Error::new(
        "announce_unsupported",
        format!("daemon build {build} cannot post a maintenance notice (no `announce` capability); rerun with --no-announce to restart it without one"),
    )
}

/// The holders and the hint of a skipped busy or armed home.
fn refused(result: &mut Value, pre: &Preflight) {
    let class = if pre.verdict == Verdict::Busy {
        Class::Live
    } else {
        Class::Armed
    };
    let holders: Vec<Value> = pre
        .interruptions
        .iter()
        .filter(|i| i.class == class)
        .map(|i| {
            json!({"actor":(!i.actor.is_empty()).then_some(&i.actor),"session":i.session,
                "kind":i.kind,"detail":i.detail})
        })
        .collect();
    result["holders"] = json!(holders);
    result["hint"] = json!(match pre.verdict {
        Verdict::Busy => "Rerun with --force to interrupt them (waits and watches reconnect to the replacement).".to_owned(),
        _ if pre.degraded => "This old daemon reads any agent seen within two minutes as possibly waiting; rerun with --allow-armed to accept a short gap for them.".to_owned(),
        _ => "Nothing is open, only armed waits; rerun with --allow-armed to accept a short gap for those agents.".to_owned(),
    });
}

fn state_reason(state: &str, d: &Value) -> String {
    let home = d["home"].as_str().unwrap_or("?");
    match state {
        "unreachable" => format!("unreachable: its socket gave no clean answer, so it may be alive and wedged; check it with `fray --home '{home}' ping` before restarting it by hand"),
        "incompatible" => format!(
            "incompatible: it speaks protocol {} and this client cannot read its occupancy; stop it with the binary that started it, then start it with this one",
            d["protocol_version"]
        ),
        "dead" => "dead: nothing answers on its socket; the record was pruned".to_owned(),
        other => format!("{other}: not running"),
    }
}

/// The `fray restart --all-stale` exit status for a report (see the module).
pub fn exit_code(report: &Value) -> i32 {
    let results = report["results"].as_array().map_or(&[][..], Vec::as_slice);
    let any = |f: &dyn Fn(&Value) -> bool| results.iter().any(f);
    if any(&|r| matches!(r["outcome"].as_str(), Some("failed" | "would_fail"))) {
        1
    } else if any(&|r| {
        r["outcome"]
            .as_str()
            .is_some_and(|o| o.starts_with("skipped"))
            && r["state"] != "dead"
    }) {
        4
    } else {
        0
    }
}

/// The summary table: one line per home, then the counts.
pub fn render(report: &Value) -> String {
    let mut out = format!(
        "fray restart --all-stale{}: target build {} ({}); client build {}\n",
        if report["dry_run"] == true {
            " --dry-run"
        } else {
            ""
        },
        clean(&report["target_build"]),
        clean(&report["exe"]),
        clean(&report["client_build"]),
    );
    let results = report["results"].as_array().map_or(&[][..], Vec::as_slice);
    if results.is_empty() {
        out.push_str("No stale daemons.\n");
    }
    for r in results {
        let outcome = r["outcome"].as_str().unwrap_or("?").replace('_', "-");
        out.push_str(&format!(
            "{outcome:<14} {}\n    {}\n",
            clean(&r["home"]),
            detail(r)
        ));
    }
    let counts = &report["counts"];
    let summary: Vec<String> = OUTCOMES
        .iter()
        .filter(|o| counts[**o].as_u64().unwrap_or(0) > 0)
        .map(|o| format!("{} {}", counts[*o], o.replace('_', "-")))
        .collect();
    if !summary.is_empty() {
        out.push_str(&format!("Summary: {}.\n", summary.join(", ")));
    }
    if let Some(unconfirmed) = report["unconfirmed"].as_array().filter(|u| !u.is_empty()) {
        out.push_str("Not considered (fray serve processes whose home could not be confirmed):\n");
        for p in unconfirmed {
            out.push_str(&format!("  pid {}: {}\n", p["pid"], clean(&p["command"])));
        }
    }
    out
}

fn detail(r: &Value) -> String {
    let holders = || {
        r["holders"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|h| {
                let who = h["actor"]
                    .as_str()
                    .map_or("(unnamed)".to_owned(), clean_str);
                let session = h["session"]
                    .as_str()
                    .map_or(String::new(), |s| format!(" session {}", clean_str(s)));
                format!("{who}{session}: {}", clean(&h["detail"]))
            })
            .collect::<Vec<_>>()
            .join("; ")
    };
    match r["outcome"].as_str().unwrap_or("") {
        "restarted" | "started" => {
            let v = &r["restart"];
            let mut s = format!(
                "build {} -> {}, pid {} -> {}",
                clean(&v["before"]["build"]),
                clean(&v["after"]["build"]),
                clean(&v["before"]["pid"]),
                clean(&v["after"]["pid"])
            );
            if let Some(label) = v["degraded"].as_str() {
                s += &format!(" (DEGRADED preflight: {})", clean_str(label));
            }
            for w in v["warnings"].as_array().into_iter().flatten() {
                s += &format!("; warning: {}", clean(w));
            }
            s
        }
        "would_restart" | "would_start" => {
            let p = &r["preflight"];
            let mut s = format!(
                "build {}, preflight {}",
                clean(&r["build"]),
                clean(&p["verdict"])
            );
            if let Some(label) = p["degraded"].as_str() {
                s += &format!(" (DEGRADED: {})", clean_str(label));
            }
            s
        }
        "skipped_busy" => format!("holders: {}. {}", holders(), clean(&r["hint"])),
        "skipped_armed" => format!("armed: {}. {}", holders(), clean(&r["hint"])),
        "skipped_state" => clean(&r["reason"]),
        _ => format!(
            "{}: {}",
            clean(&r["error"]["code"]),
            clean(&r["error"]["message"])
        ),
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

    fn report(outcomes: &[(&str, &str)]) -> Value {
        let results: Vec<Value> = outcomes
            .iter()
            .map(|(outcome, state)| json!({"home":"/h","outcome":outcome,"state":state}))
            .collect();
        json!({"results":results})
    }

    #[test]
    fn exit_status_is_failures_then_skips_and_dead_records_do_not_count() {
        assert_eq!(exit_code(&report(&[])), 0);
        assert_eq!(
            exit_code(&report(&[("restarted", "running"), ("started", "running")])),
            0
        );
        assert_eq!(
            exit_code(&report(&[
                ("restarted", "running"),
                ("skipped_state", "dead")
            ])),
            0
        );
        assert_eq!(
            exit_code(&report(&[
                ("restarted", "running"),
                ("skipped_busy", "running")
            ])),
            4
        );
        assert_eq!(exit_code(&report(&[("skipped_state", "unreachable")])), 4);
        assert_eq!(
            exit_code(&report(&[
                ("skipped_armed", "running"),
                ("failed", "running")
            ])),
            1
        );
        assert_eq!(exit_code(&report(&[("would_restart", "running")])), 0);
        assert_eq!(exit_code(&report(&[("would_fail", "running")])), 1);
    }

    #[test]
    fn the_table_names_holders_reasons_and_errors() {
        let report = json!({
            "dry_run":false,"target_build":"new","client_build":"new","exe":"/bin/fray",
            "results":[
                {"home":"/a","outcome":"restarted","state":"running","restart":{
                    "before":{"build":"old","pid":1},"after":{"build":"new","pid":2},"warnings":[]}},
                {"home":"/b","outcome":"skipped_busy","state":"running","hint":"rerun with --force",
                    "holders":[{"actor":"bob","session":"s1","detail":"open wait connection"}]},
                {"home":"/c","outcome":"skipped_state","state":"unreachable","reason":"unreachable: wedged"},
                {"home":"/d","outcome":"failed","state":"running","error":{"code":"startup","message":"no replacement"}},
            ],
            "counts":{"restarted":1,"skipped_busy":1,"skipped_state":1,"failed":1},
            "unconfirmed":[],
        });
        let shown = render(&report);
        assert!(
            shown.contains("restarted      /a\n    build old -> new, pid 1 -> 2\n"),
            "{shown}"
        );
        assert!(shown.contains("skipped-busy   /b\n    holders: bob session s1: open wait connection. rerun with --force\n"), "{shown}");
        assert!(
            shown.contains("skipped-state  /c\n    unreachable: wedged\n"),
            "{shown}"
        );
        assert!(
            shown.contains("failed         /d\n    startup: no replacement\n"),
            "{shown}"
        );
        assert!(
            shown.contains("Summary: 1 restarted, 1 skipped-busy, 1 skipped-state, 1 failed.\n"),
            "{shown}"
        );
        let none = render(&json!({"dry_run":true,"results":[],"counts":{},"unconfirmed":[]}));
        assert!(
            none.contains("--dry-run") && none.contains("No stale daemons."),
            "{none}"
        );
    }
}
