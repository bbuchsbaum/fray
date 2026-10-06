//! Offline board backup and recovery through SQLite's online backup API.
use crate::model::{random_key, Error, Result};
use rusqlite::{backup::Backup, Connection, OpenFlags};
use std::{
    fs::{self, OpenOptions},
    io,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

const STATE_DB: &str = "state.db";

fn recovery_error(message: impl Into<String>) -> Error {
    Error::new("recovery", message)
}

fn must_be_real_directory(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| recovery_error(format!("{label} does not exist: {}", path.display())))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(recovery_error(format!("{label} must be a real directory")));
    }
    Ok(())
}

fn reject_symlink_ancestors(path: &Path, label: &str) -> Result<()> {
    let resolved = path
        .canonicalize()
        .map_err(|e| recovery_error(format!("cannot resolve {label}: {e}")))?;
    for ancestor in resolved.ancestors() {
        let metadata = fs::symlink_metadata(ancestor).map_err(|e| {
            recovery_error(format!(
                "cannot inspect {label} ancestor {}: {e}",
                ancestor.display()
            ))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(recovery_error(format!(
                "{label} resolves through symlink: {}",
                ancestor.display()
            )));
        }
    }
    Ok(())
}

fn must_be_regular_file(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|_| recovery_error(format!("{label} does not exist: {}", path.display())))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(recovery_error(format!("{label} must be a regular file")));
    }
    Ok(())
}

fn require_absent(path: &Path) -> Result<()> {
    match fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Ok(_) => {
            return Err(recovery_error(format!(
                "output already exists: {}",
                path.display()
            )))
        }
        Err(e) => return Err(e.into()),
    }
    Ok(())
}

fn temporary(parent: &Path, prefix: &str) -> Result<PathBuf> {
    for _ in 0..16 {
        let path = parent.join(format!(".{prefix}-{}", random_key()?));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(_) => return Ok(path),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.into()),
        }
    }
    Err(recovery_error(
        "could not allocate a fresh recovery temporary file",
    ))
}

fn open_read_only(path: &Path) -> Result<Connection> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(5))?;
    Ok(connection)
}

fn validate(connection: &Connection) -> Result<(i64, String)> {
    let integrity: String = connection.query_row("PRAGMA integrity_check", [], |row| row.get(0))?;
    if integrity != "ok" {
        return Err(recovery_error(format!(
            "SQLite integrity check failed: {integrity}"
        )));
    }
    let version: i64 = connection.query_row("PRAGMA user_version", [], |row| row.get(0))?;
    if !(1..=3).contains(&version) {
        return Err(recovery_error(format!(
            "unsupported Fray schema version: {version}"
        )));
    }
    let store_id: String = connection
        .query_row("SELECT value FROM meta WHERE key='store_id'", [], |row| {
            row.get(0)
        })
        .map_err(|_| recovery_error("database is missing its Fray store identity"))?;
    if store_id.is_empty() || store_id.len() > 64 {
        return Err(recovery_error(
            "database has an invalid Fray store identity",
        ));
    }
    for table in ["cards", "events", "deliveries", "meta"] {
        let exists: bool = connection.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?1)",
            [table],
            |row| row.get(0),
        )?;
        if !exists {
            return Err(recovery_error(format!(
                "database is missing required Fray table {table}"
            )));
        }
    }
    if connection
        .prepare("PRAGMA foreign_key_check")?
        .query([])?
        .next()?
        .is_some()
    {
        return Err(recovery_error(
            "database contains broken foreign-key references",
        ));
    }
    Ok((version, store_id))
}

fn copy_database(source: &Connection, output: &Path) -> Result<()> {
    let mut destination = Connection::open(output)?;
    {
        let backup = Backup::new(source, &mut destination)?;
        backup.run_to_completion(64, Duration::from_millis(10), None)?;
    }
    destination.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)")?;
    destination.close().map_err(|(_, e)| e)?;
    Ok(())
}

fn publish_new(temporary: &Path, destination: &Path) -> Result<()> {
    match fs::hard_link(temporary, destination) {
        Ok(()) => {
            fs::remove_file(temporary)?;
            Ok(())
        }
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Err(recovery_error(format!(
            "output already exists: {}",
            destination.display()
        ))),
        Err(e) => Err(e.into()),
    }
}

/// Create a self-contained, immutable-by-convention copy of `home/state.db`.
/// The destination must be a new filename beneath an existing real directory.
pub fn backup(home: &Path, destination: &Path) -> Result<()> {
    reject_symlink_ancestors(home, "source home")?;
    must_be_real_directory(home, "source home")?;
    let source_path = home.join(STATE_DB);
    must_be_regular_file(&source_path, "source state database")?;
    reject_symlink_ancestors(&source_path, "source state database")?;

    let parent = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| recovery_error("backup output needs an existing parent directory"))?;
    reject_symlink_ancestors(parent, "backup output")?;
    must_be_real_directory(parent, "backup output parent")?;
    require_absent(destination)?;

    let source = open_read_only(&source_path)?;
    let original = validate(&source)?;
    let temporary = temporary(parent, "fray-backup")?;
    let outcome = (|| {
        copy_database(&source, &temporary)?;
        let copied = open_read_only(&temporary)?;
        if validate(&copied)? != original {
            return Err(recovery_error("backup database identity or schema changed"));
        }
        drop(copied);
        publish_new(&temporary, destination)
    })();
    if outcome.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    outcome
}

/// Restore `backup` into a strictly fresh, previously unused home directory.
pub fn restore(home: &Path, backup: &Path) -> Result<()> {
    if home.exists() {
        return Err(recovery_error(format!(
            "restore home must not already exist: {}",
            home.display()
        )));
    }
    let parent = home
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| recovery_error("restore home needs an existing parent directory"))?;
    reject_symlink_ancestors(parent, "restore home")?;
    must_be_real_directory(parent, "restore home parent")?;
    reject_symlink_ancestors(backup, "backup")?;
    must_be_regular_file(backup, "backup")?;
    let source = open_read_only(backup)?;
    let original = validate(&source)?;

    fs::DirBuilder::new().mode(0o700).create(home)?;
    let temporary = temporary(home, "fray-restore")?;
    let output = home.join(STATE_DB);
    let outcome = (|| {
        copy_database(&source, &temporary)?;
        let copied = open_read_only(&temporary)?;
        let restored = validate(&copied)?;
        if restored != original {
            return Err(recovery_error(
                "restored database identity or schema changed",
            ));
        }
        drop(copied);
        publish_new(&temporary, &output)
    })();
    if outcome.is_err() {
        let _ = fs::remove_dir_all(home);
    }
    outcome
}
