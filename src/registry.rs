//! Advisory per-user registry of running daemons.
//!
//! After `serve` binds `bus.sock` it writes one record to
//! `$FRAY_STATE_DIR/daemons/<sha256(canonical home)>.json` (the state directory
//! defaults to `$HOME/.local/state/fray`). A clean exit removes the record only
//! while it still names this process. A crashed daemon leaves its record
//! behind until the home registers again or a reader prunes it.
//!
//! The registry only speeds discovery. The daemon lock remains the authority
//! over a home, so [`list`] confirms every record with a ping and never reports
//! a daemon as running from the file alone.
use crate::{
    client,
    model::*,
    sha256::{self, Sha256},
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    env,
    fs::{self, DirBuilder, OpenOptions},
    io::Write,
    os::unix::{
        ffi::OsStrExt,
        fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process,
};

/// One registered daemon, as written by the daemon itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    /// Canonical project home.
    pub home: PathBuf,
    pub socket: PathBuf,
    pub pid: u32,
    /// Package version of the daemon binary.
    pub version: String,
    /// Source commit the daemon binary was built from.
    pub build: String,
    pub protocol_version: u32,
    /// Resolved path of the running executable.
    pub exe: PathBuf,
    pub started_ms: i64,
    /// `full` or `normal` SQLite synchronous mode.
    pub durability: String,
}

/// Whether a registered home answers now. Never derived from the record alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Liveness {
    /// A daemon answered a ping on the home's socket.
    Running,
    /// Nothing accepts connections on the home's socket.
    Dead,
    /// The socket accepted but did not answer cleanly (for example a timeout).
    /// Treat as possibly alive: never prune or replace on this result.
    Unknown,
}

/// A record read back from the registry together with its confirmed liveness.
#[derive(Debug, Clone)]
pub struct Entry {
    /// The record file.
    pub path: PathBuf,
    pub record: Record,
    pub liveness: Liveness,
    /// The ping response when `liveness` is `Running`. It describes whichever
    /// daemon owns the home now, so prefer it over `record` for build skew.
    pub ping: Option<Value>,
}

/// The registry root: `$FRAY_STATE_DIR`, else `$HOME/.local/state/fray`.
pub fn state_dir() -> Result<PathBuf> {
    if let Some(dir) = env::var_os("FRAY_STATE_DIR").filter(|d| !d.is_empty()) {
        return Ok(PathBuf::from(dir));
    }
    let home = env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .ok_or_else(|| Error::new("unavailable", "neither FRAY_STATE_DIR nor HOME is set"))?;
    Ok(PathBuf::from(home).join(".local/state/fray"))
}

/// Where the record for a canonical `home` lives under `state_dir`.
pub fn record_path(state_dir: &Path, home: &Path) -> PathBuf {
    let mut hash = Sha256::new();
    hash.update(home.as_os_str().as_bytes());
    state_dir
        .join("daemons")
        .join(format!("{}.json", sha256::hex(hash.finalize())))
}

/// Lists every record in the default registry with its liveness.
pub fn list() -> Result<Vec<Entry>> {
    list_in(&state_dir()?)
}

/// Lists every record under `state_dir`, pinging each home, sorted by home.
///
/// Files that cannot be read, do not parse, or are not stored under the hash of
/// the home they name are skipped: they identify no home that could be checked.
pub fn list_in(state_dir: &Path) -> Result<Vec<Entry>> {
    let dir = state_dir.join("daemons");
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
        Err(error) => return Err(error.into()),
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = entry?.path();
        let name = path.file_name().unwrap_or_default().to_string_lossy();
        if name.starts_with('.') || !name.ends_with(".json") {
            continue;
        }
        let Some(record) = read(&path) else {
            continue;
        };
        if record_path(state_dir, &record.home) != path {
            continue;
        }
        let (liveness, ping) = probe(&record.home);
        out.push(Entry {
            path,
            record,
            liveness,
            ping,
        });
    }
    out.sort_by(|a, b| a.record.home.cmp(&b.record.home));
    Ok(out)
}

/// Removes a dead entry's record. Returns whether a file was removed.
///
/// The file is removed only if it still holds the same record and the home
/// still does not answer, so a daemon that re-registered meanwhile keeps its
/// record. Running and unknown entries are never pruned.
pub fn prune(entry: &Entry) -> Result<bool> {
    if entry.liveness != Liveness::Dead
        || read(&entry.path).as_ref() != Some(&entry.record)
        || probe(&entry.record.home).0 != Liveness::Dead
    {
        return Ok(false);
    }
    match fs::remove_file(&entry.path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.into()),
    }
}

fn read(path: &Path) -> Option<Record> {
    serde_json::from_slice(&fs::read(path).ok()?).ok()
}

fn probe(home: &Path) -> (Liveness, Option<Value>) {
    // The socket is derived from the home, not taken from the record.
    match client::rpc(home, &Request::new("ping", "", json!({})), 1) {
        Ok(ping) => (Liveness::Running, Some(ping)),
        Err(error) if error.code == "unavailable" => (Liveness::Dead, None),
        Err(_) => (Liveness::Unknown, None),
    }
}

/// The live daemon's registration; dropping it unregisters.
pub(crate) struct Registration {
    path: Option<PathBuf>,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Some(path) = &self.path {
            unregister(path, process::id());
        }
    }
}

/// Records the daemon that owns `home`. Call only after taking the daemon lock
/// and binding the socket. Failure is reported on stderr and never stops the
/// daemon, because the registry is advisory.
pub(crate) fn register(home: &Path, normal: bool) -> Registration {
    let record = Record {
        home: home.to_path_buf(),
        socket: home.join("bus.sock"),
        pid: process::id(),
        version: env!("CARGO_PKG_VERSION").into(),
        build: BUILD.into(),
        protocol_version: PROTOCOL_VERSION,
        exe: env::current_exe()
            .and_then(fs::canonicalize)
            .unwrap_or_default(),
        started_ms: now_ms(),
        durability: if normal { "normal" } else { "full" }.into(),
    };
    let path = state_dir().and_then(|dir| write(&dir, &record));
    match path {
        Ok(path) => Registration { path: Some(path) },
        Err(error) => {
            eprintln!("daemon registry not updated: {error}");
            Registration { path: None }
        }
    }
}

/// Atomically writes `record` (temp file + rename) and returns its path.
fn write(state_dir: &Path, record: &Record) -> Result<PathBuf> {
    let path = record_path(state_dir, &record.home);
    let dir = path.parent().expect("record path has a parent");
    DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let temp = dir.join(format!(".{name}.{}.tmp", record.pid));
    let result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temp)?;
        file.set_permissions(fs::Permissions::from_mode(0o600))?;
        file.write_all(&serde_json::to_vec_pretty(record)?)?;
        file.write_all(b"\n")?;
        // No fsync: the record is advisory, and on macOS sync_all is a
        // device-wide F_FULLFSYNC that would stall every store on the disk.
        // Rename keeps readers from seeing a partial file; after a power loss
        // an empty or stale record is skipped or classified dead.
        fs::rename(&temp, &path)?;
        Ok(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result
}

/// Removes the record at `path` only while it still names `pid`.
fn unregister(path: &Path, pid: u32) {
    if read(path).is_some_and(|record| record.pid == pid) {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch() -> PathBuf {
        let dir = env::temp_dir().join(format!("fray-registry-{}", &random_key().unwrap()[..8]));
        fs::create_dir(&dir).unwrap();
        dir
    }

    fn record(home: &Path, pid: u32) -> Record {
        Record {
            home: home.into(),
            socket: home.join("bus.sock"),
            pid,
            version: "0".into(),
            build: "b".into(),
            protocol_version: PROTOCOL_VERSION,
            exe: "/bin/fray".into(),
            started_ms: 1,
            durability: "full".into(),
        }
    }

    #[test]
    fn write_is_private_atomic_and_replaces() {
        let state = scratch();
        let home = state.join("home");
        let path = write(&state, &record(&home, 1)).unwrap();
        assert_eq!(path, record_path(&state, &home));
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        write(&state, &record(&home, 2)).unwrap();
        assert_eq!(read(&path).unwrap().pid, 2);
        let files: Vec<_> = fs::read_dir(path.parent().unwrap()).unwrap().collect();
        assert_eq!(files.len(), 1, "no temp file survives a write");
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn unregister_keeps_a_record_naming_another_pid() {
        let state = scratch();
        let home = state.join("home");
        let path = write(&state, &record(&home, 7)).unwrap();
        unregister(&path, 8);
        assert!(path.exists());
        unregister(&path, 7);
        assert!(!path.exists());
        fs::remove_dir_all(state).unwrap();
    }

    #[test]
    fn list_classifies_unserved_homes_dead_and_skips_foreign_files() {
        let state = scratch();
        let home = state.join("home");
        fs::create_dir(&home).unwrap();
        let path = write(&state, &record(&home, 9)).unwrap();
        // A record stored under another home's hash and a malformed file.
        fs::copy(&path, state.join("daemons/misfiled.json")).unwrap();
        fs::write(state.join("daemons/junk.json"), b"{").unwrap();
        let entries = list_in(&state).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].liveness, Liveness::Dead);
        assert!(entries[0].ping.is_none());
        assert!(prune(&entries[0]).unwrap());
        assert!(!path.exists());
        assert!(!prune(&entries[0]).unwrap());
        assert!(list_in(&state.join("absent")).unwrap().is_empty());
        fs::remove_dir_all(state).unwrap();
    }
}
