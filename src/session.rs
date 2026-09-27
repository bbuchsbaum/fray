//! Host-neutral identity binding. This prevents accidental name collisions;
//! session IDs are asserted by callers and are not authentication credentials.
use crate::model::{Error, Result};
use std::{env, sync::OnceLock};

static SESSION: OnceLock<Option<String>> = OnceLock::new();

pub fn resolve(
    explicit: Option<&str>,
    claude: Option<&str>,
    codex: Option<&str>,
) -> Result<Option<String>> {
    let value = explicit
        .map(str::to_owned)
        .or_else(|| claude.map(|id| format!("claude:{id}")))
        .or_else(|| codex.map(|id| format!("codex:{id}")));
    if let Some(value) = &value {
        if value.is_empty()
            || value.len() > 128
            || !value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || ":._-".contains(c))
        {
            return Err(Error::invalid(
                "session must be 1..128 characters of [A-Za-z0-9:._-]",
            ));
        }
    }
    Ok(value)
}

fn from_host(explicit: Option<&str>, hook_session: Option<&str>) -> Result<Option<String>> {
    let claude = env::var("CLAUDE_CODE_SESSION_ID").ok();
    let codex = env::var("CODEX_THREAD_ID").ok();
    // One host started inside the other inherits the outer host's variable.
    // The caller is the nearer host in the process tree.
    let claude = match (&claude, &codex) {
        (Some(_), Some(_)) if nearer_host() == Some("codex") => None,
        _ => claude,
    };
    resolve(
        explicit.or(env::var("FRAY_SESSION").ok().as_deref()),
        claude.as_deref().or(hook_session),
        codex.as_deref(),
    )
}

/// The nearest Claude Code or Codex process among this process's ancestors.
fn nearer_host() -> Option<&'static str> {
    let mut pid = std::process::id();
    for _ in 0..32 {
        let out = std::process::Command::new("ps")
            .args(["-o", "ppid=,comm=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        let line = String::from_utf8_lossy(&out.stdout).trim().to_owned();
        let (ppid, comm) = line.split_once(char::is_whitespace)?;
        let name = comm.trim().rsplit('/').next().unwrap_or("");
        if name.starts_with("codex") {
            return Some("codex");
        }
        if name.starts_with("claude") {
            return Some("claude");
        }
        pid = ppid.trim().parse().ok().filter(|p| *p > 1)?;
    }
    None
}

/// Called once by the CLI before any RPC. Managed children inherit the exact
/// same binding, even if their own host exports a different provider session ID.
pub fn configure(explicit: Option<&str>, hook_session: Option<&str>, managed: bool) -> Result<()> {
    let mut value = from_host(explicit, hook_session)?;
    if value.is_none() && managed {
        value = Some(format!("fray:{}", crate::model::random_key()?));
    }
    SESSION
        .set(value)
        .map_err(|_| Error::invalid("session already configured"))
}

pub fn current() -> Result<Option<String>> {
    match SESSION.get() {
        Some(value) => Ok(value.clone()),
        None => from_host(None, None),
    }
}
