use crate::{model::*, store::Store};
use fs2::FileExt;
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader, Read, Write},
    net::Shutdown,
    os::unix::{
        fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::Command,
    sync::{
        atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering},
        Arc, Condvar, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

pub const REQUEST_LIMIT: usize = 128 * 1024;

pub const RESPONSE_LIMIT: usize = 4 * 1024 * 1024;
const MAX_CLIENTS: usize = 128;
struct Limits {
    clients: usize,
    long: usize,
    descriptors: u64,
}
impl Limits {
    fn current() -> Result<Self> {
        // Query the inherited soft limit once at startup. A fixed POSIX shell
        // builtin keeps the core's five-dependency and no-unsafe contracts.
        // No user content or startup environment is interpreted by the shell.
        let result = Command::new("/bin/sh")
            .args(["-c", "ulimit -n"])
            .env_clear()
            .output()?;
        let reported = String::from_utf8_lossy(&result.stdout);
        let descriptors = if result.status.success() && reported.trim() == "unlimited" {
            u64::MAX
        } else if result.status.success() {
            reported.trim().parse::<u64>().map_err(|_| {
                Error::new(
                    "unavailable",
                    "could not read inherited file descriptor limit from /bin/sh ulimit -n",
                )
            })?
        } else {
            return Err(Error::new(
                "unavailable",
                "could not read inherited file descriptor limit from /bin/sh ulimit -n",
            ));
        };
        // Each connection can own a stream, buffered-reader clone and EOF clone.
        // Leave headroom for SQLite, logs, the listener and admission rejections.
        let clients = ((descriptors.saturating_sub(32) / 3).min(MAX_CLIENTS as u64)) as usize;
        if clients < 2 {
            return Err(Error::new("unavailable", "file descriptor limit too low; raise RLIMIT_NOFILE to at least 64 before starting Fray"));
        }
        let reserved = 16.min(clients / 2);
        Ok(Self {
            clients,
            long: clients - reserved,
            descriptors,
        })
    }
}
struct Shared {
    store: Mutex<Store>,
    changed: Condvar,
    stop: AtomicBool,
    clients: AtomicUsize,
    long_clients: AtomicUsize,
    metrics_readers: AtomicUsize,
    limits: Limits,
    socket: PathBuf,
    /// Open long-lived connections, for `occupancy`.
    long_lived: crate::occupancy::Registry,
    /// How a requested shutdown ends: set, under the store lock, before `stop`.
    stopping: Mutex<Stopping>,
}
/// A restart reason makes each long-lived handler tell its client to
/// reconnect; the grace bounds how long in-flight requests may finish.
#[derive(Default)]
struct Stopping {
    restart: Option<String>,
    grace: Duration,
}
/// A restarting daemon's refusal: nothing on this connection was executed or
/// acknowledged by it. Clients reconnect and resume from their own cursors.
/// The `unavailable` code keeps older clients on their reconnect paths.
fn restarting(shared: &Shared) -> Option<Error> {
    let reason = shared
        .stopping
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .restart
        .clone()?;
    Some(
        Error::new(
            "unavailable",
            format!("daemon restarting ({reason}); nothing was acknowledged, reconnect and resume from your cursor"),
        )
        .with_details(json!({"restarting":true,"reason":reason})),
    )
}
/// The failure envelope, marked as a `restarting` control frame when it is one.
fn failure_frame(error: Error) -> Value {
    let reason = error
        .details
        .as_ref()
        .filter(|d| d["restarting"] == true)
        .map(|d| d["reason"].clone());
    let mut frame = failure(error);
    if let Some(reason) = reason {
        frame["type"] = json!("restarting");
        frame["reason"] = reason;
    }
    frame
}
/// A stream handler that saw `stop`: on a restart, say so before closing.
fn stream_stopped(stream: &mut UnixStream, shared: &Shared) -> Result<()> {
    match restarting(shared) {
        Some(error) => write_frame(stream, &failure_frame(error)),
        None => Ok(()),
    }
}

pub fn read_frame<R: BufRead>(r: &mut R, limit: usize) -> io::Result<Option<String>> {
    let mut out = Vec::new();
    loop {
        let bytes = r.fill_buf()?;
        if bytes.is_empty() {
            return if out.is_empty() {
                Ok(None)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "unterminated JSON frame",
                ))
            };
        }
        let end = bytes.iter().position(|b| *b == b'\n');
        let n = end.map(|i| i + 1).unwrap_or(bytes.len());
        if out.len() + n > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "JSON frame exceeds size limit",
            ));
        }
        out.extend_from_slice(&bytes[..n]);
        r.consume(n);
        if end.is_some() {
            break;
        }
    }
    String::from_utf8(out)
        .map(Some)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}
pub fn write_frame<W: Write>(w: &mut W, v: &Value) -> Result<()> {
    serde_json::to_writer(&mut *w, v)?;
    w.write_all(b"\n")?;
    w.flush()?;
    Ok(())
}
pub fn initialize(home: &Path) -> Result<PathBuf> {
    fs::create_dir_all(home)?;
    fs::set_permissions(home, fs::Permissions::from_mode(0o700))?;
    let home = fs::canonicalize(home)?;
    if home.join("bus.sock").as_os_str().len() > 100 {
        return Err(Error::invalid(
            "Unix socket path too long; use a shorter absolute FRAY_HOME on local disk",
        ));
    }
    Ok(home)
}
pub fn serve(home: &Path, normal: bool) -> Result<()> {
    let limits = Limits::current()?;
    let home = initialize(home)?;
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(home.join("daemon.lock"))?;
    FileExt::try_lock_exclusive(&lock)
        .map_err(|_| Error::new("already_running", "another daemon holds this project lock"))?;
    let socket = home.join("bus.sock");
    // Only the lock owner may remove a stale socket. Never unlink the lock file.
    if let Ok(meta) = fs::symlink_metadata(&socket) {
        if !meta.file_type().is_socket() {
            return Err(Error::invalid(
                "bus.sock exists but is not a socket; refusing to remove it",
            ));
        }
        fs::remove_file(&socket)?;
    }
    let store = Store::open(&home.join("state.db"), normal)?;
    // The daemon lock proves old socket-owned listeners cannot still be attached.
    store.conn.execute("UPDATE listeners SET connected=0", [])?;
    // Keepalive drives outlive a restart in their own process groups. This
    // daemon has no child handle for them; it watches their recorded pids.
    for (agent, pid) in crate::keepalive::adoptable(&store.conn)? {
        eprintln!("keepalive drive for {agent:?} (pid {pid}) adopted");
        crate::keepalive::watch(agent, pid);
    }
    fs::set_permissions(home.join("state.db"), fs::Permissions::from_mode(0o600))?;
    let listener = UnixListener::bind(&socket)?;
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o600))?;
    // This daemon answers now: clients need no longer wait out a restart.
    crate::restart::clear_marker(&home);
    let registration = crate::registry::register(&home, normal);
    let shared = Arc::new(Shared {
        store: Mutex::new(store),
        changed: Condvar::new(),
        stop: AtomicBool::new(false),
        clients: AtomicUsize::new(0),
        long_clients: AtomicUsize::new(0),
        metrics_readers: AtomicUsize::new(0),
        limits,
        socket: socket.clone(),
        long_lived: Default::default(),
        stopping: Mutex::new(Stopping::default()),
    });
    eprintln!(
        "fray {} listening on {} (synchronous={})",
        env!("CARGO_PKG_VERSION"),
        socket.display(),
        if normal {
            "NORMAL: recent commits may be lost on power failure"
        } else {
            "FULL"
        }
    );
    for accepted in listener.incoming() {
        if shared.stop.load(Ordering::SeqCst) {
            break;
        }
        let mut stream = match accepted {
            Ok(stream) => stream,
            // Admission pressure and aborted connects must not kill the daemon.
            // The listener remains owned here; retry with backoff, checking stop
            // each time, instead of converting an OS accept failure into exit.
            Err(error) => {
                eprintln!("accept error: {error}; retrying");
                thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        if shared.clients.fetch_add(1, Ordering::SeqCst) >= shared.limits.clients {
            shared.clients.fetch_sub(1, Ordering::SeqCst);
            stream.set_write_timeout(Some(Duration::from_secs(1)))?;
            let _ = write_frame(
                &mut stream,
                &failure(Error::new(
                    "busy",
                    format!("{} connected clients; retry later", shared.limits.clients),
                )),
            );
            continue;
        }
        let shared = shared.clone();
        thread::spawn(move || {
            struct Count(Arc<Shared>);
            impl Drop for Count {
                fn drop(&mut self) {
                    self.0.clients.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _count = Count(shared.clone());
            if let Err(e) = connection(stream, &shared) {
                if !matches!(e.code.as_str(), "io") {
                    eprintln!("client error: {e}");
                }
            }
        });
    }
    // Refuse new connections at once, then let in-flight requests finish and
    // long-lived handlers send their notices, within the requested grace.
    drop(listener);
    let _ = fs::remove_file(socket);
    let grace = shared
        .stopping
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .grace;
    let deadline = Instant::now() + grace;
    while shared.clients.load(Ordering::SeqCst) > 0 && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    drop(registration);
    drop(lock);
    Ok(())
}
fn poisoned() -> Error {
    Error::new(
        "internal",
        "store mutex poisoned; restart daemon and inspect state",
    )
}
struct LongClient<'a>(&'a AtomicUsize);
impl Drop for LongClient<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
fn reserve_long_client(shared: &Shared) -> Result<LongClient<'_>> {
    if shared.long_clients.fetch_add(1, Ordering::SeqCst) >= shared.limits.long {
        shared.long_clients.fetch_sub(1, Ordering::SeqCst);
        return Err(Error::new(
            "busy",
            format!(
                "{} long-lived clients; {} connection slots reserved for short RPCs; retry later",
                shared.limits.long,
                shared.limits.clients - shared.limits.long
            ),
        ));
    }
    Ok(LongClient(&shared.long_clients))
}
fn clone_for_client(stream: &UnixStream) -> Result<UnixStream> {
    stream.try_clone().map_err(|error| {
        Error::new(
            "busy",
            format!("cannot allocate a connection descriptor: {error}; retry later"),
        )
    })
}
/// How often a connection between requests checks whether the daemon is
/// stopping, and how long it may stay idle at all.
const IDLE_POLL: Duration = Duration::from_millis(100);
const IDLE_LIMIT: Duration = Duration::from_secs(30);
/// Waits for the first bytes of the next request, or its end of stream:
/// true. False once the daemon is stopping with nothing begun, so an idle
/// connection never holds a drain open. Nothing is consumed while waiting.
fn await_request(reader: &mut BufReader<UnixStream>, shared: &Shared) -> Result<bool> {
    let deadline = Instant::now() + IDLE_LIMIT;
    reader.get_ref().set_read_timeout(Some(IDLE_POLL))?;
    let ready = loop {
        if shared.stop.load(Ordering::SeqCst) {
            break false;
        }
        match reader.fill_buf() {
            Ok(_) => break true,
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) =>
            {
                if Instant::now() >= deadline {
                    return Err(io::Error::from(io::ErrorKind::TimedOut).into());
                }
            }
            Err(e) => return Err(e.into()),
        }
    };
    reader
        .get_ref()
        .set_read_timeout(Some(Duration::from_secs(30)))?;
    Ok(ready)
}
fn connection(mut stream: UnixStream, shared: &Shared) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    stream.set_write_timeout(Some(Duration::from_secs(30)))?;
    let reader = match clone_for_client(&stream) {
        Ok(reader) => reader,
        Err(error) => {
            let _ = write_frame(&mut stream, &failure(error));
            return Ok(());
        }
    };
    let mut reader = BufReader::new(reader);
    // The first byte of a request that a finite wait's hang-up observer read
    // after the wait had replied. It begins the next frame.
    let mut carry = Vec::new();
    loop {
        if carry.is_empty() && reader.buffer().is_empty() && !await_request(&mut reader, shared)? {
            // Stopping, with no request begun on this connection: close it
            // now rather than hold the drain for its whole grace.
            return Ok(());
        }
        let frame = if carry.is_empty() {
            read_frame(&mut reader, REQUEST_LIMIT)?
        } else {
            let pending = std::mem::take(&mut carry);
            read_frame(&mut pending.as_slice().chain(&mut reader), REQUEST_LIMIT)?
        };
        let Some(line) = frame else {
            break;
        };
        let req: Request = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                write_frame(&mut stream, &failure(e.into()))?;
                continue;
            }
        };
        if shared.stop.load(Ordering::SeqCst) {
            return stream_stopped(&mut stream, shared);
        }
        let long_client = if matches!(req.op.as_str(), "watch" | "watch_attention")
            || (req.op == "wait" && req.args["timeout"] != 0)
        {
            match reserve_long_client(shared) {
                Ok(permit) => Some(permit),
                Err(error) => {
                    write_frame(&mut stream, &failure(error))?;
                    return Ok(());
                }
            }
        } else {
            None
        };
        let _open = long_client
            .as_ref()
            .map(|_| shared.long_lived.open(&req, now_ms()));
        match req.op.as_str() {
            "occupancy" => {
                let response = match occupancy(shared, &req) {
                    Ok(v) => success(v),
                    Err(e) => failure(e),
                };
                write_frame(&mut stream, &response)?;
            }
            "stats" | "friction" => {
                // Each WAL reader adds SQLite descriptors beyond the stream
                // admission budget. Keep those within its reserved headroom.
                if shared
                    .metrics_readers
                    .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| {
                        (n < 2).then_some(n + 1)
                    })
                    .is_err()
                {
                    write_frame(
                        &mut stream,
                        &failure(Error::new(
                            "metrics_busy",
                            "two metrics snapshots are already running; retry later",
                        )),
                    )?;
                    continue;
                }
                struct Reader<'a>(&'a AtomicUsize);
                impl Drop for Reader<'_> {
                    fn drop(&mut self) {
                        self.0.fetch_sub(1, Ordering::SeqCst);
                    }
                }
                let reader = Reader(&shared.metrics_readers);
                let response = match crate::stats::read_snapshot(
                    &shared.socket.with_file_name("state.db"),
                    &req,
                    now_ms(),
                ) {
                    Ok(value) => success(value),
                    Err(error) => failure(error),
                };
                drop(reader);
                write_frame(&mut stream, &response)?;
            }
            "watch_attention" => {
                if !reader.buffer().is_empty() {
                    write_frame(
                        &mut stream,
                        &failure(Error::new(
                            "protocol",
                            "attention subscription cannot be pipelined with further requests",
                        )),
                    )?;
                    return Ok(());
                }
                if let Err(e) = watch_attention(&mut stream, shared, &req) {
                    let _ = write_frame(&mut stream, &failure(e));
                }
                return Ok(());
            }
            "watch" => {
                if let Err(e) = watch(&mut stream, shared, &req) {
                    let _ = write_frame(&mut stream, &failure(e));
                }
                return Ok(());
            }
            "wait" => {
                let indefinite = req.args.get("timeout") == Some(&Value::Null);
                let terminal = if reader.buffer().is_empty() && req.args["timeout"] != 0 {
                    wait_cancellable(&mut stream, shared, &req, indefinite, &mut carry)?
                } else {
                    let result = if !reader.buffer().is_empty() {
                        Err(Error::new(
                            "protocol",
                            "wait cannot be pipelined with further requests",
                        ))
                    } else {
                        wait(shared, &req, None, None)
                    };
                    let terminal = wait_terminal(indefinite, &result);
                    write_frame(&mut stream, &wait_reply(result))?;
                    terminal
                };
                if terminal {
                    return Ok(());
                }
            }
            "keepalive_start" => {
                let response = match keepalive_start(shared, &req) {
                    Ok(v) => success(v),
                    Err(e) => failure(e),
                };
                write_frame(&mut stream, &response)?;
            }
            "keepalive_status" => {
                let result = shared.store.lock().map_err(|_| poisoned())?.execute(&req);
                let response = match result {
                    Ok(mut value) => {
                        value["daemon_sandboxed"] = json!(crate::keepalive::sandboxed());
                        // The daemon keeps the drive's pid, so a keepalive
                        // whose process is gone can be named.
                        if let Some(pid) = value["pid"].as_i64() {
                            value["pid_alive"] = json!(crate::keepalive::pid_alive(pid));
                        }
                        // What a drive compares with its own build and binary
                        // to move onto this daemon's (keepalive.md, restarts).
                        value["daemon_build"] = json!(BUILD);
                        value["daemon_exe"] = json!(std::env::current_exe()
                            .ok()
                            .and_then(|exe| exe.to_str().map(str::to_owned)));
                        success(value)
                    }
                    Err(error) => failure(error),
                };
                write_frame(&mut stream, &response)?;
            }
            "shutdown" => {
                let plan = match shutdown_plan(&req.args) {
                    Ok(plan) => plan,
                    Err(error) => {
                        write_frame(&mut stream, &failure(error))?;
                        continue;
                    }
                };
                let mut reply = json!({"stopping":true});
                if let Some(reason) = &plan.restart {
                    reply["restart"] = json!(true);
                    reply["reason"] = json!(reason);
                    reply["grace_ms"] = json!(plan.grace.as_millis() as u64);
                    // Lets the requester confirm this process has exited.
                    reply["pid"] = json!(std::process::id());
                    // Until a replacement binds, a client that cannot connect
                    // waits for it (within its own budget) instead of failing.
                    if let Some(home) = shared.socket.parent() {
                        if let Err(e) = crate::restart::write_marker(home, reason) {
                            eprintln!("restart marker not written: {e}");
                        }
                    }
                    eprintln!(
                        "restart requested ({reason}); draining for up to {}ms",
                        plan.grace.as_millis()
                    );
                }
                {
                    let _guard = shared.store.lock().map_err(|_| poisoned())?;
                    *shared.stopping.lock().unwrap_or_else(|e| e.into_inner()) = plan;
                    shared.stop.store(true, Ordering::SeqCst);
                    shared.changed.notify_all();
                }
                let result = write_frame(&mut stream, &success(reply));
                let _ = UnixStream::connect(&shared.socket); // Wake the blocking accept().
                return result;
            }
            _ => {
                let response = {
                    let mut store = shared.store.lock().map_err(|_| poisoned())?;
                    let before = store.highwater()?;
                    let result = store.execute(&req);
                    if result.is_ok()
                        && (store.highwater()? > before
                            || matches!(
                                req.op.as_str(),
                                "join"
                                    | "leave"
                                    | "follow"
                                    | "unfollow"
                                    | "mute"
                                    | "unmute"
                                    | "keepalive_stop"
                            ))
                    {
                        shared.changed.notify_all();
                    }
                    match result {
                        Ok(mut v) => {
                            if req.op == "ping" {
                                v["capabilities"]
                                    .as_array_mut()
                                    .unwrap()
                                    .push(json!("listener_activation"));
                                // The daemon itself starts keepalive drives.
                                v["capabilities"]
                                    .as_array_mut()
                                    .unwrap()
                                    .push(json!("keepalive"));
                                v["capabilities"]
                                    .as_array_mut()
                                    .unwrap()
                                    .push(json!("occupancy"));
                                // `shutdown` takes {reason, restart, grace_ms}.
                                v["capabilities"]
                                    .as_array_mut()
                                    .unwrap()
                                    .push(json!("graceful_restart"));
                                v["capacity"] = json!({"clients":shared.clients.load(Ordering::SeqCst),"long_lived":shared.long_clients.load(Ordering::SeqCst),"client_limit":shared.limits.clients,"long_limit":shared.limits.long,"short_reserved":shared.limits.clients-shared.limits.long,"descriptor_limit":shared.limits.descriptors});
                            }
                            success(v)
                        }
                        Err(e) => failure(e),
                    }
                };
                // A slow consumer never holds the database lock while writing.
                write_frame(&mut stream, &response)?;
            }
        }
    }
    Ok(())
}
/// `shutdown` with no arguments stops at once, as it always has. With
/// `restart`, clients are told to reconnect and in-flight requests get up to
/// `grace_ms` (default 2s, at most 10s) to finish.
fn shutdown_plan(args: &Value) -> Result<Stopping> {
    check_fields(args, &["reason", "restart", "grace_ms"])?;
    let restart = boolean(args, "restart", false)?;
    let reason = match args.get("reason") {
        Some(_) => {
            let reason = string(args, "reason")?;
            text(reason, "reason", 200, false)?;
            Some(reason.to_owned())
        }
        None => None,
    };
    if reason.is_some() && !restart {
        return Err(Error::invalid("reason applies only with restart"));
    }
    let grace = bounded(args, "grace_ms", if restart { 2000 } else { 0 }, 0, 10_000)?;
    Ok(Stopping {
        restart: restart.then(|| reason.unwrap_or_else(|| "restart".into())),
        grace: Duration::from_millis(grace as u64),
    })
}
/// `keepalive_start`: the daemon, not the agent, starts the keepalive's drive,
/// so the drive never inherits a sandbox from the agent's tool call
/// (docs/design/keepalive.md). The request supplies only the terminal's
/// directory; the host and conversation come from its bound session.
fn keepalive_start(shared: &Shared, req: &Request) -> Result<Value> {
    check_fields(&req.args, &["cwd"])?;
    let cwd = string(&req.args, "cwd")?;
    text(cwd, "cwd", 4096, false)?;
    let home = shared
        .socket
        .parent()
        .ok_or_else(|| Error::new("internal", "socket has no home directory"))?;
    if crate::keepalive::sandboxed() {
        return Err(Error::new(
            "sandboxed",
            crate::diagnostics::sandbox_recovery(home, &req.actor),
        ));
    }
    let cwd = crate::keepalive::repository_dir(home, Path::new(cwd))?;
    let cwd_text = cwd
        .to_str()
        .ok_or_else(|| Error::invalid("cwd must be valid UTF-8"))?;
    let log = crate::keepalive::file(home, &req.actor, "log");
    let begun = {
        let mut store = shared.store.lock().map_err(|_| poisoned())?;
        let options = crate::keepalive::start_options(&store.conn, now_ms())?;
        let begun = store.keepalive_begin(
            req,
            cwd_text,
            &log.to_string_lossy(),
            options.budget,
            now_ms(),
        )?;
        shared.changed.notify_all();
        begun
    };
    if begun["already_running"] == true {
        return Ok(begun);
    }
    let session = string(&begun, "session")?;
    let mut child = match crate::keepalive::spawn(home, &req.actor, session, &cwd, &log) {
        Ok(child) => child,
        Err(e) => {
            let store = shared.store.lock().map_err(|_| poisoned())?;
            store.keepalive_abort(&req.actor, now_ms())?;
            return Err(Error::new(
                "spawn_failed",
                format!("could not start the keepalive's drive: {e}"),
            ));
        }
    };
    let pid = child.id();
    // Reap the drive when it exits; it reports its own final state.
    let actor = req.actor.clone();
    thread::spawn(move || {
        if let Ok(status) = child.wait() {
            eprintln!("keepalive drive for {actor:?} (pid {pid}) exited {status}");
        }
    });
    let store = shared.store.lock().map_err(|_| poisoned())?;
    store.keepalive_spawned(&req.actor, pid)?;
    let mut status = crate::keepalive::status(&store.conn, &req.actor, now_ms())?;
    status["started"] = json!(true);
    status["command"] = json!([
        "fray",
        crate::keepalive::DRIVE_ARGS[0],
        crate::keepalive::DRIVE_ARGS[1]
    ]);
    Ok(status)
}
/// `occupancy`: who a restart would interrupt (see `crate::occupancy`).
/// A read under the store lock and nothing else: no presence, no
/// acknowledgment. The caller's own connection is not counted.
fn occupancy(shared: &Shared, req: &Request) -> Result<Value> {
    check_fields(&req.args, &[])?;
    let now = now_ms();
    let report =
        crate::occupancy::collect(&shared.store.lock().map_err(|_| poisoned())?.conn, now)?;
    let clients = crate::occupancy::Clients {
        connected: shared.clients.load(Ordering::SeqCst).saturating_sub(1),
        long_lived: shared.long_clients.load(Ordering::SeqCst),
    };
    Ok(crate::occupancy::finish(
        report,
        shared.long_lived.snapshot(),
        clients,
        now,
    ))
}
/// A wait error that ends the connection, and every indefinite wait.
fn wait_terminal(indefinite: bool, result: &Result<Value>) -> bool {
    indefinite
        || result
            .as_ref()
            .is_err_and(|e| matches!(e.code.as_str(), "protocol" | "unavailable"))
}
fn wait_reply(result: Result<Value>) -> Value {
    match result {
        Ok(v) => success(v),
        Err(e) => failure_frame(e),
    }
}
/// Runs a wait that the client's hang-up cancels, and replies. Returns whether
/// the connection ends here.
///
/// The reply is written as soon as the wait returns, before the hang-up
/// observer is joined: a finite wait's observer can be inside a bounded read,
/// and joining it first delayed every finite wait by up to that bound. A byte
/// the observer reads after the reply begins the client's next request, so it
/// is handed back through `carry` rather than treated as pipelining.
fn wait_cancellable(
    stream: &mut UnixStream,
    shared: &Shared,
    req: &Request,
    indefinite: bool,
    carry: &mut Vec<u8>,
) -> Result<bool> {
    let mut hangup = match clone_for_client(stream) {
        Ok(hangup) => hangup,
        // Descriptor pressure is a retryable `busy` reply, not a dropped connection.
        Err(error) => {
            write_frame(stream, &failure(error))?;
            return Ok(false);
        }
    };
    // Finite waits preserve sequential requests on the same connection. A bounded
    // socket read lets this EOF observer stop without shutting down that socket.
    // The store wakes on commits and cancellation, and once a minute to
    // refresh the waiter's presence; it never polls for changes.
    hangup.set_read_timeout(if indefinite {
        None
    } else {
        Some(Duration::from_millis(100))
    })?;
    // One claim decides whether input arrived during the wait (refused as
    // pipelining) or after its reply was committed (the next request): the
    // waiter moves RUNNING to DONE, or the observer moves RUNNING to INPUT or
    // HANGUP, never both.
    const RUNNING: u8 = 0;
    const DONE: u8 = 1;
    const INPUT: u8 = 2;
    const HANGUP: u8 = 3;
    let state = AtomicU8::new(RUNNING);
    let cancelled = AtomicBool::new(false);
    let unexpected_input = AtomicBool::new(false);
    let carried = Mutex::new(None);
    let written = thread::scope(|scope| {
        scope.spawn(|| {
            let mut byte = [0u8; 1];
            while state.load(Ordering::SeqCst) == RUNNING {
                let input = hangup.read(&mut byte);
                if matches!(&input, Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted))
                {
                    continue;
                }
                let data = matches!(input, Ok(n) if n > 0);
                let claim = if data { INPUT } else { HANGUP };
                if state
                    .compare_exchange(RUNNING, claim, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
                {
                    // The reply was already committed: this byte begins the
                    // client's next request.
                    if data {
                        *carried.lock().unwrap_or_else(|e| e.into_inner()) = Some(byte[0]);
                    }
                    return;
                }
                let _guard = shared.store.lock();
                unexpected_input.store(data, Ordering::SeqCst);
                cancelled.store(true, Ordering::SeqCst);
                shared.changed.notify_all();
                return;
            }
        });
        let mut result = wait(shared, req, Some(&cancelled), Some(&unexpected_input));
        if state.compare_exchange(RUNNING, DONE, Ordering::SeqCst, Ordering::SeqCst) == Err(INPUT) {
            result = Err(Error::new(
                "protocol",
                "wait is receive-only until its response; use a separate connection for other requests",
            ));
        }
        let terminal = wait_terminal(indefinite, &result);
        let written = write_frame(stream, &wait_reply(result));
        if terminal {
            // Nothing more is read on this connection; release the observer now.
            let _ = stream.shutdown(Shutdown::Read);
        }
        written.map(|()| terminal)
    });
    if let Some(byte) = carried.into_inner().unwrap_or_else(|e| e.into_inner()) {
        carry.push(byte);
    }
    if !indefinite {
        stream.set_read_timeout(Some(Duration::from_secs(30)))?;
    }
    written
}
/// A wait, tracked while it runs: an unfiltered wait makes its agent
/// wakeable (no-silent-stalls R1) through its own `wake_waits` row, removed
/// on every exit (answer, timeout, hang-up or error).
fn wait(
    shared: &Shared,
    req: &Request,
    cancelled: Option<&AtomicBool>,
    unexpected_input: Option<&AtomicBool>,
) -> Result<Value> {
    let wake_id = random_key()?;
    let result = wait_body(shared, req, cancelled, unexpected_input, &wake_id);
    if let Ok(store) = shared.store.lock() {
        let _ = store.wait_ended(&wake_id);
    }
    result
}

fn wait_body(
    shared: &Shared,
    req: &Request,
    cancelled: Option<&AtomicBool>,
    unexpected_input: Option<&AtomicBool>,
    wake_id: &str,
) -> Result<Value> {
    check_fields(
        &req.args,
        &[
            "after",
            "timeout",
            "limit",
            "selection",
            "card_ids",
            "addressed_to_me",
            "unresolved",
            "kinds",
            "min_priority",
        ],
    )?;
    let after = bounded(&req.args, "after", 0, 0, i64::MAX)?;
    let deadline = if req.args.get("timeout") == Some(&Value::Null) {
        None
    } else {
        Some(
            Instant::now()
                + Duration::from_secs(bounded(&req.args, "timeout", 300, 0, 86400)? as u64),
        )
    };
    let limit = bounded(&req.args, "limit", 12, 1, 100)?;
    let mut store = shared.store.lock().map_err(|_| poisoned())?;
    let start = store.highwater()?;
    if after > start {
        return Err(Error::new(
            "cursor_ahead",
            "cursor exceeds this store; rejoin rather than skip data",
        ));
    }
    // Waiting makes the agent reachable; refresh that while the wait lasts.
    const TOUCH: Duration = Duration::from_secs(60);
    let wake = {
        let sel = crate::store::InboxSelection::parse(&req.args)?;
        (sel.card_ids.is_empty()
            && sel.kinds.is_empty()
            && sel.min_priority.is_none()
            && !sel.addressed_to_me
            && !sel.unresolved)
            .then_some(wake_id)
    };
    store.touch(&req.actor, req.session.as_deref(), wake, now_ms())?;
    let mut touched = Instant::now();
    loop {
        if unexpected_input.is_some_and(|input| input.load(Ordering::SeqCst)) {
            return Err(Error::new("protocol", "unexpected input while waiting"));
        }
        if shared.stop.load(Ordering::SeqCst) {
            if let Some(error) = restarting(shared) {
                return Err(error);
            }
        }
        if shared.stop.load(Ordering::SeqCst) || cancelled.is_some_and(|c| c.load(Ordering::SeqCst))
        {
            return Err(Error::new(
                "unavailable",
                "wait cancelled or daemon stopped",
            ));
        }
        // A keepalive's drive waits until asked to stop, then exits at once.
        if let Some(session) = req
            .session
            .as_deref()
            .filter(|s| crate::keepalive::is_session(Some(s)))
        {
            if store.keepalive_stop_requested(&req.actor, session)? {
                return Ok(
                    json!({"items":[],"total":0,"store_id":store.identity()?,"timed_out":false,"stop_requested":true}),
                );
            }
        }
        let mut page = store.filtered_attention(
            &req.actor,
            after,
            limit,
            crate::store::InboxSelection::parse(&req.args)?,
            now_ms(),
        )?;
        let has_items = page["total"].as_i64().unwrap_or(0) > 0;
        let remaining = deadline.map(|end| end.saturating_duration_since(Instant::now()));
        let expired = remaining.is_some_and(|r| r.is_zero());
        if has_items || expired {
            page["store_id"] = json!(store.identity()?);
            page["timed_out"] = json!(expired && !has_items);
            // Returning at once on items that were already pending looks like
            // a wait that does not wait; say why and how to wait for new ones.
            let old = page["items"].as_array().is_some_and(|items| {
                !items.is_empty()
                    && items
                        .iter()
                        .all(|i| i["through_seq"].as_i64().is_some_and(|s| s <= start))
            });
            if old {
                page["note"] = json!(format!(
                    "returned at once: {} item(s) were already pending in this selection. Handle and ack them (fray ack --last), or wait only for new activity with --new, or on one conversation with --card N",
                    page["total"]
                ));
            }
            return Ok(page);
        }
        if touched.elapsed() >= TOUCH {
            store.touch(&req.actor, req.session.as_deref(), wake, now_ms())?;
            touched = Instant::now();
        }
        // The condition check and wait use the SAME mutex as commits: no lost
        // wake-up. Wake at least every TOUCH to refresh presence.
        let slice = remaining.map_or(TOUCH, |r| r.min(TOUCH));
        store = shared
            .changed
            .wait_timeout(store, slice)
            .map_err(|_| poisoned())?
            .0;
    }
}

fn watch_attention(stream: &mut UnixStream, shared: &Shared, req: &Request) -> Result<()> {
    let mut selection_args = req.args.clone();
    let activation = crate::notification::take_activation(&mut selection_args)?;
    let options = crate::attention::Options::parse(&selection_args)?;
    let run_id = string(&req.args, "run_id")?;
    let once = boolean(&req.args, "once", false)?;
    let deadline = req
        .args
        .get("timeout")
        .map(|_| {
            bounded(&req.args, "timeout", 300, 1, 86400)
                .map(|secs| Instant::now() + Duration::from_secs(secs as u64))
        })
        .transpose()?;
    let connection_id = random_key()?;
    let mut filters = json!({"selection":options.selection,"addressed_to_me":options.addressed_to_me,"unresolved":options.unresolved,"kinds":options.kinds,"min_priority":options.min_priority});
    if !options.card_ids.is_empty() {
        filters["card_ids"] = json!(options.card_ids);
    }
    if let Some(activation) = activation {
        filters["activation"] = activation;
    }
    if once {
        // A one-shot listener that returns after a delivery is not a lapse.
        filters["once"] = json!(true);
    }
    let filters = filters.to_string();
    let store_id = {
        let store = shared.store.lock().map_err(|_| poisoned())?;
        store.listener_begin(&req.actor, run_id, &connection_id, &filters, now_ms())?;
        store.identity()?
    };
    let mut hangup = clone_for_client(stream)?;
    hangup.set_read_timeout(None)?;
    let disconnected = AtomicBool::new(false);
    let unexpected_input = AtomicBool::new(false);
    let result = thread::scope(|scope| {
        // The stream is server-to-client only after subscription. A blocking EOF
        // reader notices cancellation immediately without polling the database.
        // Shutdown(Read) below releases it on every normal/error return path.
        scope.spawn(|| {
            let input = hangup.read(&mut [0u8; 1]);
            let _guard = shared.store.lock();
            unexpected_input.store(matches!(input, Ok(n) if n > 0), Ordering::SeqCst);
            disconnected.store(true, Ordering::SeqCst);
            shared.changed.notify_all();
        });
        let result = (|| {
            write_frame(
                stream,
                &success(json!({"type":"ready","store_id":store_id})),
            )?;
            let mut emitted = HashMap::new();
            let mut batch_deadline = None;
            let mut heartbeat = Instant::now() + Duration::from_secs(15);
            loop {
                let frame = {
                    let mut store = shared.store.lock().map_err(|_| poisoned())?;
                    loop {
                        if unexpected_input.load(Ordering::SeqCst) {
                            return Err(Error::new("protocol", "attention streams are receive-only; use a separate RPC connection for acknowledgments"));
                        }
                        if disconnected.load(Ordering::SeqCst) {
                            return Ok(());
                        }
                        if shared.stop.load(Ordering::SeqCst) {
                            drop(store);
                            return stream_stopped(stream, shared);
                        }
                        let now = Instant::now();
                        if deadline.is_some_and(|end| now >= end) {
                            break json!({"type":"timeout","store_id":store_id});
                        }
                        let packet = store.wake_packet(&req.actor, &options, &emitted, now_ms())?;
                        if !packet["items"].as_array().unwrap().is_empty() {
                            // Fixed window from first pending attention, never sliding under traffic.
                            let end = *batch_deadline
                                .get_or_insert(now + Duration::from_millis(options.settle_ms));
                            let urgent =
                                packet["items"][0]["card"]["priority"].as_i64().unwrap_or(3) <= 1;
                            if now >= end || urgent {
                                store.listener_refresh(&req.actor, &connection_id, now_ms())?;
                                break packet;
                            }
                        } else {
                            batch_deadline = None;
                        }
                        if now >= heartbeat {
                            store.listener_refresh(&req.actor, &connection_id, now_ms())?;
                            break json!({"type":"heartbeat","store_id":store_id});
                        }
                        let mut wake = heartbeat;
                        if let Some(end) = batch_deadline {
                            wake = wake.min(end);
                        }
                        if let Some(end) = deadline {
                            wake = wake.min(end);
                        }
                        // Commit and selection changes use this same lock: no startup/check-wait gap.
                        let (next, _) = shared
                            .changed
                            .wait_timeout(store, wake.saturating_duration_since(Instant::now()))
                            .map_err(|_| poisoned())?;
                        store = next;
                    }
                };
                // No database lock held during a potentially slow socket write.
                write_frame(stream, &success(frame.clone()))?;
                match frame["type"].as_str() {
                    Some("attention") => {
                        for item in frame["items"].as_array().unwrap() {
                            emitted.insert(
                                item["receipt"]["id"].as_i64().unwrap(),
                                item["receipt"]["through_seq"].as_i64().unwrap(),
                            );
                        }
                        if once {
                            return Ok(());
                        }
                        batch_deadline = None;
                    }
                    Some("timeout") => return Ok(()),
                    _ => {}
                }
                heartbeat = Instant::now() + Duration::from_secs(15);
            }
        })();
        // Keep the write half available for the caller's terminal error frame.
        let _ = stream.shutdown(Shutdown::Read);
        result
    });
    let cleanup = shared
        .store
        .lock()
        .map_err(|_| poisoned())?
        .listener_end(&req.actor, &connection_id);
    result.and(cleanup)
}
fn watch(stream: &mut UnixStream, shared: &Shared, req: &Request) -> Result<()> {
    check_fields(&req.args, &["after", "topic"])?;
    let topic = req
        .args
        .get("topic")
        .map(|_| string(&req.args, "topic").map(str::to_owned))
        .transpose()?;
    let (mut cursor, store_id) = {
        let store = shared.store.lock().map_err(|_| poisoned())?;
        let head = store.highwater()?;
        let cursor = bounded(&req.args, "after", head, 0, i64::MAX)?;
        crate::retention::check_cursor(&store.conn, cursor)?;
        if cursor > head {
            return Err(Error::new(
                "cursor_ahead",
                "cursor exceeds this store; rejoin",
            ));
        }
        (cursor, store.identity()?)
    };
    write_frame(
        stream,
        &success(json!({"type":"ready","cursor":cursor,"store_id":store_id})),
    )?;
    loop {
        let batch = {
            let mut store = shared.store.lock().map_err(|_| poisoned())?;
            loop {
                if shared.stop.load(Ordering::SeqCst) {
                    drop(store);
                    return stream_stopped(stream, shared);
                }
                let batch = store.events(cursor, 64)?;
                if !batch.is_empty() {
                    break batch;
                }
                let (next, timed) = shared
                    .changed
                    .wait_timeout(store, Duration::from_secs(15))
                    .map_err(|_| poisoned())?;
                store = next;
                if timed.timed_out() {
                    break Vec::new();
                }
            }
        };
        for event in batch {
            cursor = event["seq"]
                .as_i64()
                .ok_or_else(|| Error::new("internal", "missing event seq"))?;
            let t = event["payload"]["card"]["topic"].as_str().unwrap_or("");
            if topic
                .as_ref()
                .is_none_or(|wanted| wanted == "*" || wanted == t || t == "*")
            {
                write_frame(
                    stream,
                    &success(
                        json!({"type":"event","event":event,"cursor":cursor,"store_id":store_id}),
                    ),
                )?;
            }
        }
        write_frame(
            stream,
            &success(json!({"type":"checkpoint","cursor":cursor,"store_id":store_id})),
        )?;
    }
}

#[cfg(test)]
mod metrics_tests {
    use super::*;

    #[test]
    fn metrics_reply_while_the_publishers_store_mutex_is_held() {
        let dir = std::env::temp_dir().join(format!("fray-metrics-{}", random_key().unwrap()));
        fs::create_dir(&dir).unwrap();
        let store = Store::open(&dir.join("state.db"), false).unwrap();
        let shared = Arc::new(Shared {
            store: Mutex::new(store),
            changed: Condvar::new(),
            stop: AtomicBool::new(false),
            clients: AtomicUsize::new(0),
            long_clients: AtomicUsize::new(0),
            metrics_readers: AtomicUsize::new(0),
            limits: Limits {
                clients: 128,
                long: 112,
                descriptors: 512,
            },
            socket: dir.join("bus.sock"),
            long_lived: Default::default(),
            stopping: Mutex::new(Stopping::default()),
        });
        // Ordinary dispatch would block here. Both metrics must finish
        // before the publisher's guard is released.
        let guard = shared.store.lock().unwrap();
        for op in ["stats", "friction"] {
            let (mut client, server) = UnixStream::pair().unwrap();
            client
                .set_read_timeout(Some(Duration::from_secs(3)))
                .unwrap();
            let owner = shared.clone();
            let thread = thread::spawn(move || connection(server, &owner));
            write_frame(
                &mut client,
                &serde_json::to_value(Request::new(op, "", json!({}))).unwrap(),
            )
            .unwrap();
            let reply = read_frame(
                &mut BufReader::new(client.try_clone().unwrap()),
                RESPONSE_LIMIT,
            )
            .unwrap()
            .unwrap();
            let reply: Value = serde_json::from_str(&reply).unwrap();
            assert_eq!(reply["ok"], true, "{reply}");
            client.shutdown(Shutdown::Both).unwrap();
            thread.join().unwrap().unwrap();
        }
        drop(guard);
        drop(shared);
        fs::remove_dir_all(dir).unwrap();
    }
}
