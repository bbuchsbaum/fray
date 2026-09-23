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
    resolve(
        explicit.or(env::var("FRAY_SESSION").ok().as_deref()),
        env::var("CLAUDE_CODE_SESSION_ID")
            .ok()
            .as_deref()
            .or(hook_session),
        env::var("CODEX_THREAD_ID").ok().as_deref(),
    )
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
