use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fmt, io,
    time::{SystemTime, UNIX_EPOCH},
};

pub type Result<T> = std::result::Result<T, Error>;
/// Version of the JSON protocol, independent of the binary's package version.
pub const PROTOCOL_VERSION: u32 = 2;
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Error {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<Value>,
}
impl Error {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            details: None,
        }
    }
    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
    pub fn invalid(message: impl Into<String>) -> Self {
        Self::new("invalid", message)
    }
}
impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Error {}
impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Self::new("io", e.to_string())
    }
}
impl From<rusqlite::Error> for Error {
    fn from(e: rusqlite::Error) -> Self {
        Self::new("database", e.to_string())
    }
}
impl From<serde_json::Error> for Error {
    fn from(e: serde_json::Error) -> Self {
        Self::invalid(e.to_string())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub op: String,
    #[serde(default)]
    pub actor: String,
    #[serde(default)]
    pub key: Option<String>,
    #[serde(default = "empty_object")]
    pub args: Value,
    /// The host session speaking for `actor` (e.g. `claude:<id>`). Sent only
    /// to daemons advertising `sessions`; absent means a legacy caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
}
fn empty_object() -> Value {
    json!({})
}
impl Request {
    pub fn new(op: &str, actor: &str, args: Value) -> Self {
        Self {
            op: op.into(),
            actor: actor.into(),
            key: None,
            args,
            session: None,
        }
    }
    pub fn with_session(mut self, session: Option<String>) -> Self {
        self.session = session;
        self
    }
}
pub fn success(data: Value) -> Value {
    json!({"ok":true,"data":data})
}
pub fn failure(error: Error) -> Value {
    json!({"ok":false,"error":error})
}
pub fn unpack(v: Value) -> Result<Value> {
    if v["ok"].as_bool() == Some(true) {
        Ok(v["data"].clone())
    } else {
        Err(serde_json::from_value(v["error"].clone())
            .unwrap_or_else(|_| Error::new("protocol", "invalid response envelope")))
    }
}
pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}
/// The source commit this binary was built from (`<sha>` or `<sha>-dirty`).
pub const BUILD: &str = env!("FRAY_BUILD");
pub fn random_key() -> Result<String> {
    use io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}
pub fn valid_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 80
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.:/".contains(&b))
}
pub fn valid_topic(s: &str) -> bool {
    s == "*" || valid_name(s) || s.strip_prefix('@').is_some_and(valid_name)
}
pub fn check_fields(args: &Value, allowed: &[&str]) -> Result<()> {
    let obj = args
        .as_object()
        .ok_or_else(|| Error::invalid("args must be an object"))?;
    for k in obj.keys() {
        if !allowed.contains(&k.as_str()) {
            return Err(Error::invalid(format!("unknown field: {k}")));
        }
    }
    Ok(())
}
pub fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| Error::invalid(format!("{key} must be a string")))
}
pub fn integer(v: &Value, key: &str) -> Result<i64> {
    v.get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| Error::invalid(format!("{key} must be an integer")))
}
pub fn bounded(v: &Value, key: &str, default: i64, lo: i64, hi: i64) -> Result<i64> {
    let n = if v.get(key).is_some() {
        integer(v, key)?
    } else {
        default
    };
    if !(lo..=hi).contains(&n) {
        return Err(Error::invalid(format!("{key} must be in {lo}..={hi}")));
    }
    Ok(n)
}
pub fn boolean(v: &Value, key: &str, default: bool) -> Result<bool> {
    match v.get(key) {
        None => Ok(default),
        Some(v) => v
            .as_bool()
            .ok_or_else(|| Error::invalid(format!("{key} must be boolean"))),
    }
}
pub fn text(s: &str, field: &str, max: usize, empty: bool) -> Result<()> {
    if (!empty && s.trim().is_empty()) || s.len() > max || s.contains('\0') {
        return Err(Error::invalid(format!(
            "{field} must contain {}..={max} UTF-8 bytes, without NUL",
            if empty { 0 } else { 1 }
        )));
    }
    Ok(())
}
pub fn tags(v: &Value) -> Result<Vec<String>> {
    let arr = v
        .as_array()
        .ok_or_else(|| Error::invalid("tags/topics must be an array"))?;
    if arr.len() > 16 {
        return Err(Error::invalid("at most 16 tags/topics"));
    }
    let mut result = Vec::new();
    for x in arr {
        let s = x
            .as_str()
            .ok_or_else(|| Error::invalid("tags/topics must be strings"))?;
        if !valid_topic(s) {
            return Err(Error::invalid("invalid tag/topic"));
        }
        result.push(s.to_string());
    }
    result.sort();
    result.dedup();
    Ok(result)
}
pub fn clip(s: &str, max: usize) -> String {
    let mut it = s.chars();
    let mut out: String = it.by_ref().take(max).collect();
    if it.next().is_some() {
        out.push('…');
    }
    out
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Card {
    pub id: i64,
    pub rev: i64,
    pub kind: String,
    pub topic: String,
    pub title: String,
    pub summary: String,
    pub status: String,
    pub priority: i64,
    pub pinned: bool,
    pub tags: Vec<String>,
    pub author: String,
    pub assignee: Option<String>,
    pub lease_owner: Option<String>,
    pub lease_until_ms: i64,
    pub fence: i64,
    pub created_ms: i64,
    pub updated_ms: i64,
    pub last_seq: i64,
}
impl Card {
    pub fn terminal(&self) -> bool {
        matches!(
            self.status.as_str(),
            "resolved" | "superseded" | "withdrawn"
        )
    }
    pub fn compact(&self, now: i64) -> Value {
        let mut v = serde_json::to_value(self).expect("Card serialization cannot fail");
        v["summary"] = json!(clip(&self.summary, 240));
        v["summary_truncated"] = json!(self.summary.chars().count() > 240);
        v["lease_live"] = json!(self.lease_owner.is_some() && self.lease_until_ms > now);
        if self.author == "owner" {
            // Written only through `fray owner`; unsigned (Tier 1).
            v["authority"] = json!("owner (unsigned)");
        }
        v
    }
}
