//! Read-only host-neutral listener diagnostics. Presence is never model progress.
use crate::{client, model::*};
use serde_json::{json, Value};
use std::path::Path;

pub fn inspect(home: &Path, actor: &str) -> Result<Value> {
    let mut report = json!({"diagnostic_version":1,"home":home,"agent":actor,"read_only":true,"checks":[],"model_response_guaranteed":false});
    let daemon = match client::rpc(home, &Request::new("ping", "", json!({})), 3) {
        Ok(value) => value,
        Err(error) => {
            report["daemon"] = json!({"reachable":false,"error":error});
            add(&mut report, "daemon_unavailable", "error", "Daemon is unreachable. Check the selected home and coordinate with its owner before starting or restarting it.");
            return Ok(finish(report));
        }
    };
    report["store_id"] = daemon["store_id"].clone();
    report["client_build"] = json!(BUILD);
    report["daemon"] = json!({"reachable":true,"version":daemon["version"],"build":daemon["build"],"protocol_version":daemon["protocol_version"],"capabilities":daemon["capabilities"],"capacity":daemon["capacity"]});
    if daemon["protocol_version"].as_u64() != Some(PROTOCOL_VERSION.into()) {
        add(&mut report, "protocol_mismatch", "error", "Client and daemon protocols differ; no operational request was sent. Coordinate an upgrade with the owner.");
        return Ok(finish(report));
    }
    if let Some((code, message)) = build_check(daemon["build"].as_str(), BUILD) {
        add(&mut report, code, "warning", message);
    }
    let missing: Vec<_> = [
        "attention_stream",
        "wait_indefinite",
        "attention_filters",
        "listener_activation",
        "read_batches",
    ]
    .into_iter()
    .filter(|wanted| {
        !daemon["capabilities"]
            .as_array()
            .is_some_and(|caps| caps.iter().any(|c| c == wanted))
    })
    .collect();
    if !missing.is_empty() {
        report["missing_capabilities"] = json!(missing);
        add(&mut report, "upgrade_required", "warning", "Selected daemon lacks some attention features. Upgrade requires coordination; doctor never restarts it.");
    }
    if daemon["capacity"]["long_lived"]
        .as_u64()
        .zip(daemon["capacity"]["long_limit"].as_u64())
        .is_some_and(|(n, max)| n >= max)
    {
        add(&mut report, "listeners_at_capacity", "warning", "Long-lived connection capacity is full. Short RPC slots remain reserved; retry subscription after a consumer exits.");
    }
    if actor.is_empty() {
        add(
            &mut report,
            "identity_required",
            "warning",
            "Supply --as NAME or FRAY_AGENT to diagnose a specific session.",
        );
        return Ok(finish(report));
    }
    let roster = match client::rpc(home, &Request::new("agents", actor, json!({})), 3) {
        Ok(value) => value,
        Err(error) => {
            report["roster_error"] = json!(error);
            add(
                &mut report,
                "roster_unavailable",
                "error",
                "Could not read agent presence; its state is unknown.",
            );
            return Ok(finish(report));
        }
    };
    let agent = roster["items"]
        .as_array()
        .and_then(|items| items.iter().find(|a| a["name"] == actor));
    let Some(agent) = agent else {
        add(
            &mut report,
            "identity_not_found",
            "warning",
            if roster["more"] == true {
                "Identity was not in the bounded roster; absence is not established."
            } else {
                "Identity is not registered on this board."
            },
        );
        return Ok(finish(report));
    };
    report["registration"] =
        json!({"enabled":agent["enabled"],"recently_seen":agent["recently_seen"]});
    report["listening"] = listening(agent, now_ms());
    // Read-only K2 visibility: terminal hook coverage and the keepalive's
    // durable state are reported without starting, stopping or rearming it.
    if daemon["capabilities"]
        .as_array()
        .is_some_and(|caps| caps.iter().any(|cap| cap == "keepalive"))
    {
        match client::rpc(home, &Request::new("keepalive_status", actor, json!({})), 3) {
            Ok(keepalive) => {
                report["keepalive"] = keepalive.clone();
                report["terminal_hooks"] = keepalive["terminal"].clone();
                if keepalive["daemon_sandboxed"] == true {
                    add(
                        &mut report,
                        "keepalive_daemon_sandboxed",
                        "error",
                        &sandbox_recovery(home, actor),
                    );
                }
                if matches!(
                    keepalive["state"].as_str(),
                    Some("deferred" | "paused" | "stopping")
                ) {
                    add(
                        &mut report,
                        "keepalive_waiting",
                        "warning",
                        &format!(
                            "Keepalive is {}: {}",
                            keepalive["state"].as_str().unwrap_or("unknown"),
                            keepalive["deferred"]
                                .as_str()
                                .or_else(|| keepalive["paused"].as_str())
                                .unwrap_or("turn in progress")
                        ),
                    );
                }
            }
            Err(error) => report["keepalive_error"] = json!(error),
        }
    } else {
        report["keepalive"] = json!({"supported":false});
    }
    if agent["enabled"] != true {
        add(&mut report, "identity_left", "warning", "Identity has left. An old listener or registration does not authorize automatic rejoin.");
    } else {
        let inbox = client::rpc(
            home,
            &Request::new("inbox", actor, json!({"selection":"involved","limit":1})),
            3,
        );
        match inbox {
            Ok(inbox) => {
                report["pending_conversations"] = inbox["total"].clone();
            }
            Err(error) => {
                report["attention_error"] = json!(error);
            }
        }
        if report["listening"]["live"] != true {
            add(&mut report, "not_listening", "warning", "No live listener or managed runner is visible. Pending messages alone cannot wake an idle host; arm its supported adapter or perform an explicit wait.");
        } else if report["listening"]["activation_expired"] == true {
            add(&mut report, "activation_expired", "warning", "Adapter's declared wake lifetime expired even though transport is connected. Rearm through the owning host.");
        } else if report["listening"]["activation"] == "unknown" {
            add(&mut report, "activation_unknown", "warning", "Transport is connected, but its host wake mechanism was not declared. Do not infer that an idle model will run.");
        } else if matches!(
            report["listening"]["activation"].as_str(),
            Some("manual" | "boundary")
        ) {
            add(&mut report, "idle_wake_unavailable", "warning", "This adapter requires an explicit read or a host turn boundary. Its connected transport does not start an idle model turn.");
        }
    }
    Ok(finish(report))
}

pub(crate) fn sandbox_recovery(home: &Path, actor: &str) -> String {
    let home = home.to_string_lossy().replace('\'', "'\\''");
    let actor = actor.replace('\'', "'\\''");
    format!("The daemon is sandboxed and cannot start a usable keepalive. From the owner's own shell, run `fray --home '{home}' stop`, then `fray --home '{home}' start`. After the daemon is reachable, retry `fray --home '{home}' --as '{actor}' keepalive` from the original bound host session and repository directory.")
}

pub fn listening(agent: &Value, now: i64) -> Value {
    let listener = &agent["listener"];
    let controller = &agent["controller"];
    if agent["enabled"] == true && controller["live"] == true {
        return json!({"live":true,"state":controller["state"],"transport":"managed-runner","activation":"managed","activation_source":"controller","activation_expired":false,"model_response_guaranteed":false});
    }
    let activation = listener
        .get("activation")
        .unwrap_or(&listener["selection"]["activation"]);
    let mode = activation["mode"].as_str().unwrap_or("unknown");
    let expires = activation["expires_ms"].as_i64();
    json!({"live":agent["enabled"] == true && listener["live"] == true,"state":listener["state"].as_str().unwrap_or("absent"),"transport":"listener","transport_expires_ms":listener["expires_ms"],"activation":mode,"activation_source":if mode == "unknown" {"none"} else {"adapter-declared"},"activation_expires_ms":expires,"activation_expired":expires.is_some_and(|end| now >= end),"once":listener["selection"]["once"] == true,"model_response_guaranteed":false})
}
fn add(report: &mut Value, code: &str, severity: &str, message: &str) {
    report["checks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"code":code,"severity":severity,"message":message}));
}
fn finish(mut report: Value) -> Value {
    let checks = report["checks"].as_array().unwrap();
    report["status"] = json!(if checks.iter().any(|c| c["severity"] == "error") {
        "blocked"
    } else if checks.is_empty() {
        "ready"
    } else {
        "needs_attention"
    });
    report
}

/// Whether a reachable, protocol-compatible daemon is the same build as this
/// client. A different build can still lack newer behavior.
pub fn build_check(daemon: Option<&str>, client: &str) -> Option<(&'static str, &'static str)> {
    match daemon {
        Some(build) if build == client => None,
        Some(_) => Some((
            "daemon_build_mismatch",
            "The daemon runs a different build than this client, so newer behavior may be missing. Install the main build on PATH, then restart the daemon after announcing it on the board.",
        )),
        None => Some((
            "daemon_build_unknown",
            "The daemon predates build reporting; it is older than this client. Install the main build on PATH, then restart the daemon after announcing it on the board.",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::sandbox_recovery;
    use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

    struct Temp(PathBuf);
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn sandbox_recovery_preserves_the_selected_home_and_literal_shell_arguments() {
        let temp = Temp(std::env::temp_dir().join(format!(
            "fray-recovery-command-{}",
            crate::model::random_key().unwrap()
        )));
        fs::create_dir(&temp.0).unwrap();
        let bin = temp.0.join("bin");
        let elsewhere = temp.0.join("elsewhere");
        fs::create_dir(&bin).unwrap();
        fs::create_dir(&elsewhere).unwrap();
        let stub = bin.join("fray");
        fs::write(&stub, "#!/bin/sh\nprintf '%s\\0' \"$@\"\n").unwrap();
        fs::set_permissions(&stub, fs::Permissions::from_mode(0o700)).unwrap();
        let home = temp.0.join("board space's $(touch leaked); *");
        let actor = "reviewer";
        let message = sandbox_recovery(&home, actor);
        assert!(message.contains("owner's own shell"));
        assert!(message.contains("original bound host session and repository directory"));
        let commands: Vec<_> = message.split('`').skip(1).step_by(2).collect();
        assert_eq!(commands.len(), 3);
        for (command, expected) in commands.iter().zip([
            vec!["--home", home.to_str().unwrap(), "stop"],
            vec!["--home", home.to_str().unwrap(), "start"],
            vec!["--home", home.to_str().unwrap(), "--as", actor, "keepalive"],
        ]) {
            let output = Command::new("/bin/sh")
                .args(["-c", command])
                .current_dir(&elsewhere)
                .env("PATH", &bin)
                .env_remove("FRAY_HOME")
                .env_remove("FRAY_AGENT")
                .output()
                .unwrap();
            assert!(output.status.success(), "{output:?}");
            let actual: Vec<_> = output
                .stdout
                .split(|&byte| byte == 0)
                .filter(|arg| !arg.is_empty())
                .map(|arg| std::str::from_utf8(arg).unwrap())
                .collect();
            assert_eq!(actual, expected);
        }
        assert!(!elsewhere.join("leaked").exists());
    }
}
