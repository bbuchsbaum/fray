//! The git guard (epic child 4, docs/design/mote-adapter.md section 7.1):
//! commit and push hooks that warn when staged or pushed paths are in another
//! agent's lane, or in another actor's Mote reservation where Mote is
//! adopted. Advisory by default, blocking with `FRAY_GUARD=block`, and never
//! blocking because Fray or Mote cannot be reached. `--no-verify` remains the
//! human escape hatch.
use crate::{git_lines, send, staged_paths};
use fray::{model::*, mote};
use serde_json::{json, Value};
use std::{
    fs,
    io::Read,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

/// Marks hook files written by `fray guard install`.
const MARKER: &str = "# fray-guard";

fn hook_script(hook: &str, fray: &Path) -> String {
    let fray = fray.display().to_string().replace('\'', "'\\''");
    let prior = format!("\"$(dirname \"$0\")/{hook}.fray-prior\"");
    // Runs any prior hook first, with the same arguments (and, for pre-push,
    // the same stdin), and stops with its status if it fails. Then asks fray,
    // found where it was installed or on PATH; a missing fray never blocks.
    let find = format!(
        "fray='{fray}'\n[ -x \"$fray\" ] || fray=$(command -v fray || true)\nif [ -z \"$fray\" ]; then echo 'fray guard: fray not found; not checked' >&2; exit 0; fi\n"
    );
    if hook == "pre-push" {
        format!(
            "#!/bin/sh\n{MARKER}: installed by `fray guard install`; the previous hook, if any, is {hook}.fray-prior\ninput=$(cat)\nif [ -x {prior} ]; then\n    printf '%s\\n' \"$input\" | {prior} \"$@\" || exit $?\nfi\n{find}printf '%s\\n' \"$input\" | \"$fray\" guard pre-push \"$@\"\n"
        )
    } else {
        format!(
            "#!/bin/sh\n{MARKER}: installed by `fray guard install`; the previous hook, if any, is {hook}.fray-prior\nif [ -x {prior} ]; then\n    {prior} \"$@\" || exit $?\nfi\n{find}\"$fray\" guard {hook}\n"
        )
    }
}

/// `fray guard install`: writes pre-commit and pre-push into the repository's
/// shared hooks directory (all worktrees use it), keeping any existing hook
/// as `<name>.fray-prior` and running it first. Idempotent.
pub fn install() -> Result<Value> {
    let here = std::env::current_dir()?;
    let Some(dir) = git_lines(
        &here,
        &["rev-parse", "--path-format=absolute", "--git-path", "hooks"],
    )
    .pop() else {
        return Err(Error::invalid("fray guard install needs a git repository"));
    };
    let dir = PathBuf::from(dir);
    fs::create_dir_all(&dir)?;
    let fray = std::env::current_exe()?;
    let mut installed = Vec::new();
    for hook in ["pre-commit", "pre-push"] {
        let path = dir.join(hook);
        let prior = dir.join(format!("{hook}.fray-prior"));
        if let Ok(existing) = fs::read_to_string(&path) {
            if !existing.contains(MARKER) {
                if prior.exists() {
                    return Err(Error::new(
                        "guard_conflict",
                        format!(
                            "{} exists and so does {}; merge them by hand, then rerun",
                            path.display(),
                            prior.display()
                        ),
                    ));
                }
                fs::rename(&path, &prior)?;
            }
        }
        // Write a new file and rename it into place, never rewrite in place.
        let tmp = dir.join(format!(".{hook}.fray-new"));
        fs::write(&tmp, hook_script(hook, &fray))?;
        fs::set_permissions(&tmp, fs::Permissions::from_mode(0o755))?;
        fs::rename(&tmp, &path)?;
        installed.push(json!({"hook":hook,"path":path.display().to_string(),
            "prior":prior.exists().then(|| prior.display().to_string())}));
    }
    Ok(
        json!({"guard":{"installed":installed,"fray":fray.display().to_string(),
        "mode":if blocking() {"block"} else {"warn"}}}),
    )
}

fn blocking() -> bool {
    std::env::var("FRAY_GUARD").as_deref() == Ok("block")
}

const ZERO: &str = "0000000000000000000000000000000000000000";

/// Paths touched by a push, from the ref-update lines git gives pre-push:
/// `<local ref> <local sha> <remote ref> <remote sha>`. A deletion is
/// ignored; a new branch covers the commits no remote-tracking ref has.
fn pushed_paths(top: &Path, remote: &str, input: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for line in input.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        let [_, local, _, remote_sha] = f.as_slice() else {
            continue;
        };
        if local.chars().all(|c| c == '0') {
            continue; // deletion
        }
        let range: Vec<String> = if remote_sha.chars().all(|c| c == '0') || *remote_sha == ZERO {
            vec![
                (*local).to_owned(),
                "--not".into(),
                format!("--remotes={remote}"),
            ]
        } else {
            vec![format!("{remote_sha}..{local}")]
        };
        let mut args: Vec<&str> = vec!["log", "--format=", "--name-only"];
        args.extend(range.iter().map(String::as_str));
        for p in git_lines(top, &args) {
            if !p.is_empty() && !paths.contains(&p) {
                paths.push(p);
            }
        }
    }
    paths
}

/// Other holders of any of `paths`: Mote reservations where Mote is paired
/// (active and orphaned; both block others), otherwise Fray lanes. `Err` means
/// the check could not be made.
fn holders(home: &Path, actor: &str, paths: &[String]) -> std::result::Result<Vec<Value>, String> {
    let cwd = std::env::current_dir().map_err(|e| e.to_string())?;
    let overlaps = |held: &Value| -> Vec<String> {
        let held: Vec<&str> = held
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .collect();
        paths
            .iter()
            .filter(|p| held.iter().any(|h| fray::store::paths_overlap(h, p)))
            .cloned()
            .collect()
    };
    match mote::locate(home, &cwd) {
        Ok(Some(path)) => {
            let store = mote::Store {
                store_id: mote::store_id(&path).map_err(|e| e.message)?,
                path,
            };
            let who = (!actor.is_empty()).then_some(actor);
            let board = match mote::run(&store, who, &["board"], mote::read_timeout()) {
                mote::Outcome::Ok(b) => b,
                other => return Err(format!("mote board: {other:?}")),
            };
            let mut found = Vec::new();
            for kind in ["active_reservations", "orphaned_reservations"] {
                for r in board[kind].as_array().into_iter().flatten() {
                    if r["actor"].as_str() == Some(actor) {
                        continue;
                    }
                    let hit = overlaps(&r["paths"]);
                    if !hit.is_empty() {
                        found.push(json!({"source":"mote","holder":r["actor"],"paths":hit,
                            "reservation":r["reservation_id"],"entity":r["entity"],
                            "orphaned":kind == "orphaned_reservations"}));
                    }
                }
            }
            Ok(found)
        }
        Ok(None) => {
            let lanes = send(home, actor, "lanes", json!({"paths":paths}), None, 10)
                .map_err(|e| format!("{}: {}", e.code, e.message))?;
            Ok(lanes["lanes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|l| l["agent"].as_str() != Some(actor))
                .filter_map(|l| {
                    let hit = overlaps(&l["paths"]);
                    (!hit.is_empty()).then(|| {
                        json!({"source":"lane","holder":l["agent"],"paths":hit,"lane":l["id"],
                            "purpose":l["purpose"],"state":l["state"],"stale":l["stale"]})
                    })
                })
                .collect())
        }
        Err(e) => Err(e.message),
    }
}

/// `fray guard pre-commit` and `fray guard pre-push`: exit status 1 only when
/// blocking is on and another holder was found. Everything else, including
/// an unreachable Fray or Mote, is a warning and exit 0.
pub fn check(home: &Path, actor: &str, stage: &str, remote: Option<&str>) -> Result<(Value, bool)> {
    let here = std::env::current_dir()?;
    let Some(top) = git_lines(&here, &["rev-parse", "--show-toplevel"])
        .pop()
        .map(PathBuf::from)
    else {
        return Ok((
            json!({"guard":{"note":"not in a git repository; not checked"}}),
            false,
        ));
    };
    let paths = if stage == "pre-push" {
        let mut input = String::new();
        std::io::stdin().read_to_string(&mut input)?;
        pushed_paths(&top, remote.unwrap_or("origin"), &input)
    } else {
        staged_paths(&top)
    };
    if paths.is_empty() {
        return Ok((json!({"guard":{"paths":[],"conflicts":[]}}), false));
    }
    let block = blocking();
    match holders(home, actor, &paths) {
        Err(why) => {
            eprintln!(
                "fray guard: could not check {} paths ({why}); not blocking",
                paths.len()
            );
            Ok((json!({"guard":{"paths":paths,"unchecked":why}}), false))
        }
        Ok(conflicts) => {
            if actor.is_empty() && !conflicts.is_empty() {
                eprintln!(
                    "fray guard: no FRAY_AGENT set, so every holder below counts as someone else"
                );
            }
            for c in &conflicts {
                let paths = c["paths"].as_array().map_or(0, Vec::len);
                let holder = c["holder"].as_str().unwrap_or("?");
                let what = if c["source"] == "mote" {
                    format!(
                        "reserved in Mote by {holder} ({}, {}{})",
                        c["reservation"].as_str().unwrap_or("?"),
                        c["entity"].as_str().unwrap_or("?"),
                        if c["orphaned"] == true {
                            ", orphaned"
                        } else {
                            ""
                        }
                    )
                } else {
                    format!(
                        "in {holder}'s lane {} ({}{})",
                        c["lane"],
                        c["purpose"].as_str().unwrap_or(""),
                        if c["stale"] == true { ", stale" } else { "" }
                    )
                };
                eprintln!(
                    "fray guard: {paths} path(s) {what}: {}",
                    c["paths"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if !conflicts.is_empty() {
                eprintln!(
                    "fray guard: ask the holder first (fray send HOLDER ...), take the lane when it frees (fray lane take PATHS --queue), or commit with --no-verify if you are sure.{}",
                    if block { " Blocking (FRAY_GUARD=block)." } else { " Not blocking; set FRAY_GUARD=block to refuse." }
                );
            }
            let refuse = block && !conflicts.is_empty();
            Ok((
                json!({"guard":{"paths":paths,"conflicts":conflicts,"blocked":refuse}}),
                refuse,
            ))
        }
    }
}
