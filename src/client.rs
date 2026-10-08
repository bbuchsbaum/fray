use crate::{
    model::*,
    server::{initialize, read_frame, write_frame, RESPONSE_LIMIT},
};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    env,
    fs::{self, OpenOptions},
    io::BufReader,
    os::unix::{fs::OpenOptionsExt, net::UnixStream, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

pub fn home(explicit: Option<PathBuf>) -> Result<PathBuf> {
    let cwd = env::current_dir()?;
    if let Some(p) = explicit {
        return Ok(if p.is_absolute() { p } else { cwd.join(p) });
    }
    for ancestor in cwd.ancestors() {
        let p = ancestor.join(".fray");
        if p.is_dir() {
            return Ok(p);
        }
    }
    if let Some(home) = git_home(&cwd) {
        return Ok(home);
    }
    Ok(cwd.join(".fray"))
}
pub(crate) fn git_home(directory: &Path) -> Option<PathBuf> {
    // All Git worktrees use the common directory, not their private .git file.
    let out = Command::new("git")
        .current_dir(directory)
        .args(["rev-parse", "--git-common-dir"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let path = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    Some(
        if path.is_absolute() {
            path
        } else {
            directory.join(path)
        }
        .join("fray"),
    )
}

/// Read-only, bounded discovery. Never starts a daemon or changes home selection.
pub fn find(workspace: &Path, selected_home: &Path) -> Result<Value> {
    let workspace = fs::canonicalize(workspace)?;
    let mut candidates = vec![selected_home.to_path_buf()];
    for ancestor in workspace.ancestors() {
        candidates.push(ancestor.join(".fray"));
    }
    if let Some(home) = git_home(&workspace) {
        candidates.push(home);
    }
    for entry in fs::read_dir(&workspace)? {
        let entry = entry?;
        if !entry.file_type()?.is_dir() || entry.file_name().to_string_lossy().starts_with('.') {
            continue;
        }
        let path = entry.path();
        candidates.push(path.join(".fray"));
        if path.join(".git").exists() {
            if let Some(home) = git_home(&path) {
                candidates.push(home);
            }
        }
    }
    let homes: BTreeSet<_> = candidates
        .into_iter()
        .filter(|p| p.is_dir())
        .map(fs::canonicalize)
        .collect::<std::io::Result<_>>()?;
    let selected = fs::canonicalize(selected_home).unwrap_or_else(|_| selected_home.to_path_buf());
    let mut items = Vec::new();
    for home in homes {
        let mut item = json!({"home":home,"selected":home==selected});
        match rpc(&home, &Request::new("ping", "", json!({})), 1) {
            Ok(server) => {
                item["running"] = json!(true);
                item["server"] = server;
            }
            Err(error) => {
                item["running"] = json!(false);
                item["error"] = json!(error);
            }
        }
        items.push(item);
    }
    Ok(
        json!({"workspace":workspace,"selected_home":selected,"items":items,
        "note":"Read-only discovery of the selected home, ancestor .fray homes and immediate non-hidden child repositories. Use --home PATH or FRAY_HOME to select an existing board; discovery does not change routing."}),
    )
}
fn connect(home: &Path, timeout: u64) -> Result<BufReader<UnixStream>> {
    let stream = UnixStream::connect(home.join("bus.sock")).map_err(|e| {
        Error::new(
            "unavailable",
            format!("{e}; run fray start (home: {})", home.display()),
        )
    })?;
    stream.set_read_timeout(Some(Duration::from_secs(timeout.max(1))))?;
    stream.set_write_timeout(Some(Duration::from_secs(5)))?;
    Ok(BufReader::new(stream))
}
fn write_request(stream: &mut UnixStream, req: &Request) -> Result<()> {
    let serialized = serde_json::to_vec(req)?;
    if serialized.len() + 1 > crate::server::REQUEST_LIMIT {
        return Err(Error::invalid("request exceeds frame limit"));
    }
    write_frame(stream, &serde_json::to_value(req)?)
}
fn exchange(reader: &mut BufReader<UnixStream>, req: &Request) -> Result<Value> {
    write_request(reader.get_mut(), req)?;
    let line = read_frame(reader, RESPONSE_LIMIT)?.ok_or_else(|| {
        Error::new(
            "protocol",
            "connection closed before acknowledgment; retry a mutation with its same --key",
        )
    })?;
    unpack(serde_json::from_str(&line)?)
}
fn compatible(home: &Path, daemon: &Value) -> Result<()> {
    if daemon["protocol_version"].as_u64() == Some(u64::from(PROTOCOL_VERSION)) {
        return Ok(());
    }
    let version = daemon.get("version").unwrap_or(&Value::Null);
    let protocol = daemon.get("protocol_version").unwrap_or(&Value::Null);
    // Single-quote the explicit home, including embedded quotes, for a copyable
    // recovery command. Never restart a live daemon implicitly.
    let home = home.to_string_lossy().replace('\'', "'\\''");
    let recovery = if protocol
        .as_u64()
        .is_some_and(|p| p > u64::from(PROTOCOL_VERSION))
    {
        "Upgrade the client to support the daemon's protocol; do not restart it with an older binary.".to_string()
    } else {
        format!("After coordinating with the daemon owner, run: fray --home '{home}' stop && fray --home '{home}' start")
    };
    Err(Error::new("protocol_version",format!("Fray client {} (protocol {}) is incompatible with daemon {} (protocol {}; null means unknown). No operational request sent on this connection. {recovery}",env!("CARGO_PKG_VERSION"),PROTOCOL_VERSION,version,protocol)))
}
fn handshake(reader: &mut BufReader<UnixStream>, home: &Path) -> Result<Value> {
    let daemon = exchange(reader, &Request::new("ping", "", json!({})))?;
    compatible(home, &daemon)?;
    warn_build_mismatch(home, &daemon);
    Ok(daemon)
}
/// A compatible daemon from a different build can still lack newer behavior.
/// Say so once per process, on stderr only (stdout may carry attention).
fn warn_build_mismatch(home: &Path, daemon: &Value) {
    use std::sync::atomic::{AtomicBool, Ordering};
    static WARNED: AtomicBool = AtomicBool::new(false);
    if let Some(build) = daemon["build"].as_str() {
        if build != BUILD && !WARNED.swap(true, Ordering::Relaxed) {
            eprintln!(
                "fray: note: daemon build {build} differs from this client build {BUILD} ({}); run `fray doctor`",
                home.display()
            );
        }
    }
}
/// How long a client waits for a restarting daemon to accept connections again.
const RESTART_WINDOW: Duration = Duration::from_secs(30);
/// The reason a daemon gave when it refused or ended a request because it is
/// restarting. Nothing that request carried was executed or acknowledged.
pub(crate) fn restart_reason(error: &Error) -> Option<String> {
    let details = error.details.as_ref()?;
    (details["restarting"] == true).then(|| details["reason"].as_str().unwrap_or("").to_owned())
}
/// Waits until a daemon answers on this home again: the replacement. A
/// stopping daemon may still accept a connection, but never answers one.
/// False if none answers within the window.
pub fn await_daemon(home: &Path, window: Duration) -> bool {
    let deadline = Instant::now() + window;
    let mut delay = Duration::from_millis(50);
    loop {
        if rpc_mapped(home, &Request::new("ping", "", json!({})), 1).is_ok() {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(delay.min(deadline.saturating_duration_since(Instant::now())));
        delay = (delay * 2).min(Duration::from_millis(500));
    }
}
/// Waits until the daemon process `pid` has exited, so its lock is released
/// and a replacement can start. False if it is still running at the deadline.
///
/// This never touches the lock itself: probing with a try-lock could make a
/// replacement that is starting at that moment lose the lock and give up.
/// `bus.sock` is no proof either, since a restarting daemon removes it
/// before draining. A process that is gone, or a zombie (it has exited and
/// closed its descriptors, but nobody has reaped it yet), holds no lock.
pub fn await_exit(pid: u32, window: Duration) -> bool {
    let deadline = Instant::now() + window;
    loop {
        if process_exited(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_millis(50));
    }
}
fn process_exited(pid: u32) -> bool {
    // `ps` prints nothing for a pid that no longer exists, and `Z` for a zombie.
    Command::new("/bin/ps")
        .args(["-o", "stat=", "-p", &pid.to_string()])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .is_ok_and(|out| {
            let stat = String::from_utf8_lossy(&out.stdout);
            let stat = stat.trim();
            stat.is_empty() || stat.starts_with('Z')
        })
}
/// The server's timeout for a `wait` that names none.
const WAIT_DEFAULT_SECS: u64 = 300;
/// A request a restarting daemon refused was never executed, so it is resent
/// once the daemon is back, within the caller's own time budget (`timeout`
/// seconds; for a wait, what is left of its timeout) and never longer than
/// RESTART_WINDOW per pause. A wait resumes from the same cursor. One quiet
/// line on stderr says why it paused.
pub fn rpc(home: &Path, req: &Request, timeout: u64) -> Result<Value> {
    let started = Instant::now();
    let wait = req.op == "wait";
    let indefinite = wait && req.args.get("timeout") == Some(&Value::Null);
    let wait_secs = req.args["timeout"].as_u64().unwrap_or(WAIT_DEFAULT_SECS);
    // An indefinite wait has no budget of its own; the window still bounds it.
    let budget = (!indefinite).then(|| Duration::from_secs(timeout.max(1)));
    let mut attempt = req.clone();
    let mut attempt_timeout = timeout;
    let mut announced = false;
    loop {
        match rpc_mapped(home, &attempt, attempt_timeout) {
            Err(error) if !matches!(req.op.as_str(), "ping" | "shutdown") => {
                let Some(reason) = restart_reason(&error) else {
                    return Err(error);
                };
                if !announced {
                    eprintln!("fray: daemon restarting ({reason}); reconnecting");
                    announced = true;
                }
                let left = budget.map(|b| b.saturating_sub(started.elapsed()));
                let window = left.map_or(RESTART_WINDOW, |left| left.min(RESTART_WINDOW));
                if window.is_zero() || !await_daemon(home, window) {
                    return Err(error);
                }
                let elapsed = started.elapsed().as_secs();
                if wait && !indefinite {
                    let secs = wait_secs.saturating_sub(elapsed);
                    attempt.args["timeout"] = json!(secs);
                    attempt_timeout = secs
                        .saturating_add(5)
                        .min(timeout.saturating_sub(elapsed).max(1));
                } else if !indefinite {
                    attempt_timeout = timeout.saturating_sub(elapsed).max(1);
                }
            }
            other => return other,
        }
    }
}
fn rpc_mapped(home: &Path, req: &Request, timeout: u64) -> Result<Value> {
    rpc_inner(home, req, timeout).map_err(|error| {
        if req.op == "wait"
            && (matches!(error.code.as_str(), "io" | "busy")
                || (error.code == "protocol" && error.message.starts_with("connection closed")))
        {
            Error::new("unavailable", format!("wait transport failed: {error}"))
        } else {
            error
        }
    })
}
fn rpc_inner(home: &Path, req: &Request, timeout: u64) -> Result<Value> {
    let mut reader = connect(home, timeout.min(5))?;
    let mut wire_request = req.clone();
    wire_request.session = None;
    // Ping is diagnostic; shutdown is the deliberate recovery escape hatch.
    // All other requests are checked on the SAME connection, with no cached
    // compatibility decision that could survive a daemon replacement.
    if !matches!(req.op.as_str(), "ping" | "shutdown") {
        let daemon = handshake(&mut reader, home)?;
        wire_request = session_request(req, &daemon)?;
        // Optional excerpt control: a daemon without full text needs no limit.
        if !daemon["capabilities"]
            .as_array()
            .is_some_and(|caps| caps.iter().any(|c| c == "addressed_full_text"))
        {
            if let Some(args) = wire_request.args.as_object_mut() {
                args.remove("full_text_budget");
            }
        }
        // A hook continuing its identity after /clear degrades to a plain
        // join on a daemon that cannot record it, rather than failing.
        if req.args.get("continued").is_some()
            && !daemon["capabilities"]
                .as_array()
                .is_some_and(|caps| caps.iter().any(|c| c == "session_continue"))
        {
            if let Some(args) = wire_request.args.as_object_mut() {
                args.remove("continued");
                args.remove("takeover");
            }
        }
        let capability = match wire_request.op.as_str() {
            "mote_rpc_receipt" => Some((
                "mote_rpc_receipts",
                "committed Mote workflow response recovery",
            )),
            op if op.starts_with("dispatch_") => Some(("dispatch", "Mote-backed dispatch")),
            op if op.starts_with("mote_operation_") || op.starts_with("mote_review_") => {
                Some(("mote_workflows", "recoverable Mote workflows"))
            }
            "peers" | "peer_present" => Some(("peer_discovery", "session peer discovery")),
            "review_request" | "review_subject" => {
                Some(("review_subjects", "version-bound peer review"))
            }
            "agents" if req.args.get("all").is_some() => Some(("agents_all", "agents --all")),
            "join" if wire_request.args.get("takeover").is_some() => {
                Some(("sessions", "join --takeover"))
            }
            "patch" if req.args.get("over_objection").is_some() => {
                Some(("objection_gate", "patch --over-objection"))
            }
            "mute" | "unmute" => Some(("mute", "thread muting")),
            "stats" | "friction" => Some(("stats", "fray stats and friction")),
            "mote_bind" | "mote_binding" => Some(("mote_adapter", "the Mote adapter")),
            "mote_ingest" | "mote_sync_failed" => Some(("mote_sync", "fray mote sync")),
            "mote_claims" => Some(("mote_reconcile", "Mote claim reconciliation")),
            "send" | "annotate" if req.args.get("respond_within_ms").is_some() => {
                Some(("ask_deadlines", "--respond-within"))
            }
            "escalate_tick" | "stuck_requests" => {
                Some(("escalations", "escalation of stuck requests"))
            }
            "mote_requests_sync" | "mote_requests_tracked" => {
                Some(("mote_requests", "Mote request tracking"))
            }
            "present" | "batch" => Some(("read_batches", "immutable read batches")),
            "keepalive_start" | "keepalive_stop" | "keepalive_status" | "keepalive_usage" => {
                Some(("keepalive", "fray keepalive"))
            }
            "ack" if req.args.get("last").is_some() => Some(("ack_last", "ack --last")),
            "owner_decide" | "owner_answer" => Some(("owner_channel", "fray owner")),
            "lane_take" | "lane_release" | "lanes" | "set_status" => {
                Some(("lanes", "lanes and status"))
            }
            "send" if req.args.get("pending").is_some() => Some(("pending_send", "send --pending")),
            "ack" if req.args.get("batch").is_some() => {
                Some(("read_batches", "batch acknowledgments"))
            }
            "show" if req.args.get("unread").is_some() => {
                Some(("thread_unread", "unread conversation view"))
            }
            "show" if req.args.get("compact").is_some() => {
                Some(("thread_compact", "compact conversation view"))
            }
            "wait"
                if req.args.get("addressed_to_me").is_some()
                    || req.args.get("unresolved").is_some() =>
            {
                Some(("wait_filters", "wait filters"))
            }
            "annotate" if req.args.get("refs").is_some() => Some(("reply_refs", "reply --ref")),
            "send"
                if req.args["body"]
                    .as_str()
                    .is_some_and(|body| body.len() > 2000) =>
            {
                Some(("long_messages", "messages over 2,000 bytes"))
            }
            "inbox"
                if req.args.get("addressed_to_me").is_some()
                    || req.args.get("unresolved").is_some() =>
            {
                Some(("inbox_filters", "inbox filters"))
            }
            _ => None,
        };
        let mut capabilities: Vec<_> = capability.into_iter().collect();
        if req.op == "mote_bind" && req.args.get("cursor_mode").is_some()
            || req.op == "mote_ingest" && req.args.get("sync_revision").is_some()
        {
            capabilities.push((
                "mote_admission_order",
                "admission-ordered Mote synchronization",
            ));
        }
        if req.op == "annotate" && req.args.get("review_verdict").is_some() {
            capabilities.push(("review_subjects", "version-bound peer verdict"));
        }
        if req.op == "annotate" && req.args.get("ack_batch").is_some() {
            capabilities.push(("reply_ack_batch", "reply --ack-batch"));
        }
        if req.op == "wait" && req.args.get("timeout") == Some(&Value::Null) {
            capabilities.push(("wait_indefinite", "wait --timeout none"));
        }
        if matches!(req.op.as_str(), "wait" | "inbox") {
            check_attention_filters(&daemon, &req.args)?;
        }
        for (capability, feature) in capabilities {
            require_capability(&daemon, capability, feature)?;
        }
    }
    let indefinite = req.op == "wait" && req.args.get("timeout") == Some(&Value::Null);
    reader.get_mut().set_read_timeout(if indefinite {
        None
    } else {
        Some(Duration::from_secs(timeout.max(1)))
    })?;
    exchange(&mut reader, &wire_request)
}

fn session_request(req: &Request, daemon: &Value) -> Result<Request> {
    let mut wire = req.clone();
    wire.session = if daemon["capabilities"]
        .as_array()
        .is_some_and(|caps| caps.iter().any(|c| c == "sessions"))
    {
        match &req.session {
            Some(session) => crate::session::resolve(Some(session), None, None)?,
            None => crate::session::current()?,
        }
    } else {
        None
    };
    Ok(wire)
}

fn require_capability(daemon: &Value, capability: &str, feature: &str) -> Result<()> {
    if !daemon["capabilities"]
        .as_array()
        .is_some_and(|caps| caps.iter().any(|c| c == capability))
    {
        return Err(Error::new("unsupported_capability", format!("Daemon does not support {feature}. No operational request sent. Coordinate with the daemon owner to upgrade and restart it.")));
    }
    Ok(())
}
fn check_attention_filters(daemon: &Value, args: &Value) -> Result<()> {
    if args.get("card_ids").is_some() {
        require_capability(daemon, "card_attention", "card-specific attention")?;
    }
    if args.get("kinds").is_some() || args.get("min_priority").is_some() {
        require_capability(
            daemon,
            "attention_filters",
            "kind/priority attention filters",
        )?;
    }
    Ok(())
}

/// Generic NDJSON adapter surface. Control frames never reach model-facing stdout.
/// Reconnect deliberately re-offers unacknowledged receipts; consumers deduplicate
/// by (store_id, agent, id, through_seq), never by a global stream cursor.
pub fn watch_attention(home: &Path, actor: &str, mut args: Value, reconnect: bool) -> Result<()> {
    let notification = boolean(&args, "notification", false)?;
    let include_control = boolean(&args, "include_control", false)?;
    if let Some(args) = args.as_object_mut() {
        args.remove("notification");
        args.remove("include_control");
    }
    let mut selection_args = args.clone();
    let activation = crate::notification::take_activation(&mut selection_args)?;
    crate::attention::Options::parse(&selection_args)?;
    if !valid_name(actor) {
        return Err(Error::invalid(
            "watch --attention requires a unique --as name",
        ));
    }
    args["run_id"] = json!(random_key()?);
    let once = boolean(&args, "once", false)?;
    let deadline = args
        .get("timeout")
        .map(|_| {
            bounded(&args, "timeout", 300, 1, 86400)
                .map(|secs| Instant::now() + Duration::from_secs(secs as u64))
        })
        .transpose()?;
    let mut expected_store: Option<String> = None;
    let mut retry_delay = 1;
    let mut disconnected = false;
    // A restart's one notice line stands for the whole reconnect.
    let mut restarted = false;
    loop {
        if let Some(end) = deadline {
            let remaining = end.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(Error::new("wait_timeout", "attention deadline expired"));
            }
            args["timeout"] = json!(remaining.as_secs().saturating_add(1));
        }
        let result = (|| -> Result<()> {
            let mut reader = connect(home, 5)?;
            let daemon = handshake(&mut reader, home)?;
            if !daemon["capabilities"]
                .as_array()
                .is_some_and(|caps| caps.iter().any(|c| c == "attention_stream"))
            {
                return Err(Error::new("unsupported_capability", "Daemon does not support attention streams. No operational request sent; coordinate an upgrade with its owner."));
            }
            check_attention_filters(&daemon, &args)?;
            let mut wire_args = args.clone();
            if activation.is_some()
                && !daemon["capabilities"]
                    .as_array()
                    .is_some_and(|caps| caps.iter().any(|cap| cap == "listener_activation"))
            {
                wire_args.as_object_mut().unwrap().remove("activation");
                wire_args
                    .as_object_mut()
                    .unwrap()
                    .remove("activation_expires_ms");
                eprintln!("fray attention: daemon cannot record adapter activation; continuing with unknown activation");
            }
            if let Some(id) = daemon["store_id"].as_str() {
                if expected_store.as_ref().is_some_and(|old| old != id) {
                    return Err(Error::new(
                        "store_changed",
                        "database identity changed; rejoin before listening",
                    ));
                }
            }
            reader
                .get_mut()
                .set_read_timeout(Some(Duration::from_secs(45)))?;
            write_request(
                reader.get_mut(),
                &session_request(&Request::new("watch_attention", actor, wire_args), &daemon)?,
            )?;
            while let Some(line) = read_frame(&mut reader, RESPONSE_LIMIT)? {
                let mut data = unpack(serde_json::from_str(&line)?)?;
                let id = string(&data, "store_id")?;
                if expected_store.as_ref().is_some_and(|old| old != id) {
                    return Err(Error::new(
                        "store_changed",
                        "database identity changed; rejoin before listening",
                    ));
                }
                expected_store = Some(id.to_owned());
                if disconnected {
                    if !restarted {
                        eprintln!(
                            "fray attention: reconnected; unacknowledged receipts may repeat"
                        );
                    }
                    disconnected = false;
                    restarted = false;
                }
                retry_delay = 1;
                match data["type"].as_str() {
                    Some("attention") => {
                        if daemon["capabilities"]
                            .as_array()
                            .is_some_and(|caps| caps.iter().any(|cap| cap == "read_batches"))
                        {
                            let receipts: Vec<_> = data["items"]
                                .as_array()
                                .unwrap()
                                .iter()
                                .map(|item| item["receipt"].clone())
                                .collect();
                            // Registration precedes output so a notice can contain
                            // the token. It proves neither host exposure nor handling.
                            let presented = rpc(
                                home,
                                &Request::new(
                                    "present",
                                    actor,
                                    json!({"source":"attention","receipts":receipts}),
                                ),
                                10,
                            )?;
                            if presented["store_id"] != data["store_id"] {
                                return Err(Error::new(
                                    "store_changed",
                                    "batch came from a replaced database",
                                ));
                            }
                            data["batch"] = presented["batch"]["id"].clone();
                        }
                        // Broken consumer output is terminal, never an excuse to reconnect.
                        if notification {
                            for notice in crate::notification::notices(&data)? {
                                write_frame(&mut std::io::stdout().lock(), &notice)
                                    .map_err(|e| Error::new("output_closed", e.message))?;
                            }
                        } else {
                            write_frame(&mut std::io::stdout().lock(), &data)
                                .map_err(|e| Error::new("output_closed", e.message))?;
                        }
                        if once {
                            return Ok(());
                        }
                    }
                    Some("timeout") => {
                        return Err(Error::new("wait_timeout", "attention deadline expired"))
                    }
                    Some("ready" | "heartbeat") => {
                        if include_control {
                            data["control_version"] = json!(1);
                            data["agent"] = json!(actor);
                            write_frame(&mut std::io::stdout().lock(), &data)
                                .map_err(|e| Error::new("output_closed", e.message))?;
                        }
                    }
                    _ => return Err(Error::new("protocol", "unexpected attention frame")),
                }
            }
            Err(Error::new("disconnected", "attention connection closed"))
        })();
        match result {
            Err(e)
                if reconnect
                    && matches!(
                        e.code.as_str(),
                        "io" | "disconnected" | "unavailable" | "listener_expired" | "busy"
                    ) =>
            {
                let restart = restart_reason(&e);
                if !disconnected {
                    if include_control {
                        let reason = if restart.is_some() {
                            "restarting"
                        } else {
                            e.code.as_str()
                        };
                        let frame = json!({"type":"disconnected","control_version":1,"agent":actor,"store_id":expected_store,"reason":reason});
                        write_frame(&mut std::io::stdout().lock(), &frame)
                            .map_err(|e| Error::new("output_closed", e.message))?;
                    }
                    match &restart {
                        Some(reason) => eprintln!(
                            "fray attention: daemon restarting ({reason}); reconnecting, unacknowledged receipts may repeat"
                        ),
                        None => eprintln!("fray attention: {e}; reconnecting with backoff"),
                    }
                    disconnected = true;
                    restarted = restart.is_some();
                }
                if restart.is_some() {
                    // The daemon said it is coming back: reconnect when it does.
                    let mut window = RESTART_WINDOW;
                    if let Some(end) = deadline {
                        window = window.min(end.saturating_duration_since(Instant::now()));
                    }
                    await_daemon(home, window);
                    retry_delay = 1;
                    continue;
                }
                let mut delay = Duration::from_secs(retry_delay);
                if let Some(end) = deadline {
                    delay = delay.min(end.saturating_duration_since(Instant::now()));
                }
                thread::sleep(delay);
                retry_delay = (retry_delay * 2).min(15);
            }
            other => return other,
        }
    }
}
pub fn start(home: &Path, normal: bool) -> Result<Value> {
    let home = initialize(home)?;
    let ping = Request::new("ping", "", json!({}));
    match rpc(&home, &ping, 1) {
        Ok(v) => {
            compatible(&home, &v)?;
            return Ok(json!({"already_running":true,"home":home,"server":v}));
        }
        Err(e) if e.code == "unavailable" => {}
        Err(e) => return Err(e),
    }
    let log = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(home.join("daemon.log"))?;
    let mut command = Command::new("nohup");
    command
        .arg(env::current_exe()?)
        .arg("--home")
        .arg(&home)
        .arg("serve");
    if normal {
        command.arg("--normal");
    }
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::from(log.try_clone()?))
        .stderr(Stdio::from(log))
        .process_group(0)
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match rpc(&home, &ping, 1) {
            Ok(v) => {
                compatible(&home, &v)?;
                return Ok(json!({"started":true,"home":home,"server":v}));
            }
            Err(e) if e.code == "unavailable" => {}
            Err(e) => return Err(e),
        }
        if Instant::now() > deadline {
            let _ = child.try_wait();
            return Err(Error::new(
                "startup",
                format!(
                    "daemon did not become ready; inspect {}",
                    home.join("daemon.log").display()
                ),
            ));
        }
        // Competing starters are harmless: the daemon lock selects one winner.
        let _ = child.try_wait();
        thread::sleep(Duration::from_millis(25));
    }
}
pub fn watch(
    home: &Path,
    actor: &str,
    after: Option<i64>,
    topic: Option<String>,
    reconnect: bool,
) -> Result<()> {
    let mut cursor = after;
    let mut expected_store: Option<String> = None;
    // Once a restart is announced, the reconnect attempts that follow are quiet.
    let mut restarting = false;
    loop {
        let result = (|| -> Result<()> {
            let mut args = json!({});
            if let Some(n) = cursor {
                args["after"] = json!(n);
            }
            if let Some(t) = &topic {
                args["topic"] = json!(t);
            }
            let req = Request::new("watch", actor, args);
            let mut reader = connect(home, 5)?;
            let daemon = handshake(&mut reader, home)?;
            let req = session_request(&req, &daemon)?;
            reader
                .get_mut()
                .set_read_timeout(Some(Duration::from_secs(45)))?;
            write_request(reader.get_mut(), &req)?;
            while let Some(line) = read_frame(&mut reader, RESPONSE_LIMIT)? {
                let data = unpack(serde_json::from_str(&line)?)?;
                restarting = false;
                if let Some(id) = data["store_id"].as_str() {
                    if expected_store.as_ref().is_some_and(|old| old != id) {
                        return Err(Error::new(
                            "store_changed",
                            "database identity changed; rejoin before replaying",
                        ));
                    }
                    expected_store = Some(id.to_string());
                }
                // Cursor advances only after successful stdout write. That is transport
                // delivery, not proof that a model has understood or acted on the event.
                write_frame(&mut std::io::stdout().lock(), &data)?;
                if let Some(n) = data["cursor"].as_i64() {
                    cursor = Some(n);
                }
            }
            Err(Error::new("disconnected", "watch connection closed"))
        })();
        match result {
            Err(e)
                if reconnect
                    && matches!(e.code.as_str(), "io" | "disconnected" | "unavailable") =>
            {
                // Do not busy-loop on a closed pipe or repeatedly restart the daemon.
                if e.message.contains("Broken pipe") {
                    return Err(e);
                }
                if let Some(reason) = restart_reason(&e) {
                    eprintln!("fray watch: daemon restarting ({reason}); reconnecting at cursor {cursor:?}");
                    // Quiet only while a replacement is expected; after the
                    // window, report failures again.
                    restarting = await_daemon(home, RESTART_WINDOW);
                    continue;
                }
                if !restarting {
                    eprintln!("fray watch: {e}; reconnecting at cursor {cursor:?}");
                }
                thread::sleep(Duration::from_secs(1));
            }
            other => return other,
        }
    }
}
