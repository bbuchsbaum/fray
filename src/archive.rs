//! Read-only owner snapshots and deterministic history export.
use crate::{model::*, store::Store};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::{fs, io::Write, path::Path};

fn reader(home: &Path) -> Result<Store> {
    let conn =
        Connection::open_with_flags(home.join("state.db"), OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    conn.busy_timeout(std::time::Duration::from_secs(5))?;
    conn.execute_batch("BEGIN DEFERRED")?;
    Ok(Store { conn })
}

/// The owner sees the whole board without joining or recording exposure.
pub fn board(home: &Path, now: i64) -> Result<Value> {
    snapshot(home, now, None)
}

/// A compact discovery snapshot; exposure and ownership remain unchanged.
pub fn peek(home: &Path, now: i64) -> Result<Value> {
    let mut value = snapshot(home, now, Some(20))?;
    for thread in value["cards"].as_array_mut().into_iter().flatten() {
        *thread = json!({"id":thread["card"]["id"],"title":thread["card"]["title"],
            "kind":thread["card"]["kind"],"status":thread["card"]["status"],
            "priority":thread["card"]["priority"],"assignee":thread["card"]["assignee"],
            "objections":thread["objections"]});
    }
    Ok(value)
}

fn snapshot(home: &Path, now: i64, limit: Option<usize>) -> Result<Value> {
    let mut store = reader(home)?;
    let mut query = store.conn.prepare("SELECT id FROM cards WHERE status NOT IN ('resolved','superseded','withdrawn') ORDER BY priority,id LIMIT ?1")?;
    let mut ids = query
        .query_map([limit.map_or(-1, |n| n as i64 + 1)], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(query);
    let more = limit.is_some_and(|n| ids.len() > n);
    if let Some(limit) = limit {
        ids.truncate(limit);
    }
    let mut cards = Vec::new();
    for id in ids {
        cards.push(store.execute_at(
            &Request::new("show", "", json!({"id":id,"compact":true})),
            now,
        )?);
    }
    let agents = store.execute_at(&Request::new("agents", "", json!({"limit":100})), now)?;
    let lanes = crate::store::live_lanes(&store.conn, now)?;
    Ok(
        json!({"store_id":store.identity()?,"cursor":store.highwater()?,"read_only":true,
        "cards":cards,"more_cards":more,"agents":agents,"lanes":lanes}),
    )
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

pub fn board_html(snapshot: &Value) -> String {
    let mut html = String::from(
        r#"<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Fray board</title>
<style>
:root{color-scheme:light dark;--bg:#f6f5f1;--fg:#252720;--muted:#5a6054;--rule:#cdd0c6;--accent:#245f49}*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--fg);font:16px/1.5 ui-sans-serif,system-ui,sans-serif}main{max-width:1100px;margin:auto;padding:32px 24px 64px}h1{font-size:2rem;margin:0 0 8px}h2{font-size:1.25rem;margin:36px 0 12px}p{max-width:72ch;color:var(--muted)}table{border-collapse:collapse;width:100%;font-variant-numeric:tabular-nums}th{text-align:left;color:var(--muted);font-size:.875rem}th,td{padding:12px 10px;border-bottom:1px solid var(--rule);vertical-align:top;overflow-wrap:anywhere}th:first-child,td:first-child{padding-left:0}a{color:var(--accent);text-underline-offset:3px}a:focus-visible{outline:2px solid var(--accent);outline-offset:4px}details{border-bottom:1px solid var(--rule);padding:14px 0}summary{cursor:pointer;font-weight:600;overflow-wrap:anywhere}summary:focus-visible{outline:2px solid var(--accent)}pre{white-space:pre-wrap;overflow-wrap:anywhere;font-size:.875rem}small{color:var(--muted)}::selection{background:#d6e9db;color:#183e2c}.scroll{overflow-x:auto}@media(prefers-color-scheme:dark){:root{--bg:#20251f;--fg:#edf0e7;--muted:#b8c2b0;--rule:#454f41;--accent:#a6d5b7}}@media(max-width:600px){main{padding:24px 16px}th,td{padding:10px 6px}table{font-size:.875rem}}
</style><main><h1>Fray board</h1><p>A read-only snapshot. Open asks and objections remain pending until their participants handle them.</p>"#,
    );
    html.push_str(&format!(
        "<small>Store {} · event {}</small>",
        escape(snapshot["store_id"].as_str().unwrap_or("")),
        snapshot["cursor"]
    ));
    html.push_str("<h2>Team</h2><div class=scroll><table><thead><tr><th scope=col>Agent</th><th scope=col>Role</th><th scope=col>Reachability</th><th scope=col>Status</th></tr></thead><tbody>");
    for agent in snapshot["agents"]["items"].as_array().into_iter().flatten() {
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            escape(agent["name"].as_str().unwrap_or("")),
            escape(agent["role"].as_str().unwrap_or("")),
            escape(agent["reachability"].as_str().unwrap_or("unknown")),
            escape(agent["status"].as_str().unwrap_or(""))
        ));
    }
    html.push_str("</tbody></table></div>");
    if snapshot["agents"]["more"] == true {
        html.push_str("<p>More team members exist; this roster is bounded.</p>");
    }
    html.push_str("<h2>Open conversations</h2>");
    let cards = snapshot["cards"].as_array();
    if cards.is_none_or(Vec::is_empty) {
        html.push_str("<p>No open conversations.</p>");
    }
    for thread in cards.into_iter().flatten() {
        let card = &thread["card"];
        html.push_str(&format!("<details id=card-{}><summary>#{} {} · {}</summary><p>{}</p><p>From {} · assigned to {}</p><pre>{}</pre></details>",
            card["id"], card["id"], escape(card["title"].as_str().unwrap_or("")), escape(card["status"].as_str().unwrap_or("")),
            escape(thread["full_text"].as_str().or(card["summary"].as_str()).unwrap_or("")),
            escape(card["author"].as_str().unwrap_or("")), escape(card["assignee"].as_str().unwrap_or("unassigned")),
            escape(&serde_json::to_string_pretty(&json!({"objections":thread["objections"],"objection_override":thread["objection_override"],"review":thread["review"],"tags":card["tags"]})).unwrap())));
    }
    html.push_str("<h2>Lanes</h2>");
    html.push_str(&format!(
        "<pre>{}</pre>",
        escape(&serde_json::to_string_pretty(&snapshot["lanes"]).unwrap())
    ));
    html.push_str("<h2>Owner decisions awaiting response</h2>");
    let mut count = 0;
    for thread in snapshot["cards"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| t["card"]["assignee"] == "owner")
    {
        count += 1;
        let card = &thread["card"];
        html.push_str(&format!(
            "<p><a href=\"#card-{}\">#{} {}</a></p>",
            card["id"],
            card["id"],
            escape(card["title"].as_str().unwrap_or(""))
        ));
    }
    if count == 0 {
        html.push_str("<p>No owner decisions are waiting.</p>");
    }
    html.push_str("</main></html>");
    html
}

fn fenced(value: &Value) -> Result<String> {
    let text = serde_json::to_string_pretty(value)?;
    let longest = text.split(|c| c != '`').map(str::len).max().unwrap_or(0);
    let fence = "`".repeat(longest.max(2) + 1);
    Ok(format!("{fence}json\n{text}\n{fence}\n"))
}

/// Export complete histories for cards touched since the requested timestamp.
/// Full histories retain the context preceding the selection boundary.
pub fn markdown(home: &Path, since: Option<i64>) -> Result<Vec<(String, Vec<u8>)>> {
    let mut store = reader(home)?;
    let mut query = store.conn.prepare("SELECT id FROM cards WHERE ?1 IS NULL OR EXISTS(SELECT 1 FROM events e WHERE e.card_id=cards.id AND e.ts_ms>=?1) ORDER BY id")?;
    let ids = query
        .query_map([since], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(query);
    let mut files = Vec::new();
    let mut index = format!(
        "# Fray history\n\nStore: `{}`\n\nSnapshot event: {}\n\n",
        store.identity()?,
        store.highwater()?
    );
    for id in ids {
        let head = store.execute_at(
            &Request::new("show", "", json!({"id":id,"compact":true})),
            0,
        )?;
        let mut query = store.conn.prepare(
            "SELECT seq,ts_ms,actor,op,payload FROM events WHERE card_id=? ORDER BY seq",
        )?;
        let events = query
            .query_map([id], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let name = format!("thread-{id}.md");
        index.push_str(&format!("- [Conversation #{id}]({name})\n"));
        let mut body = format!(
            "# Conversation #{id}\n\n## Current state\n\n{}\n",
            fenced(&head)?
        );
        for (seq, at, actor, op, payload) in events {
            body.push_str(&format!("## Event {seq}\n\n{}\n", fenced(&json!({"seq":seq,"time_ms":at,"actor":actor,"op":op,"payload":serde_json::from_str::<Value>(&payload)?}))?));
        }
        files.push((name, body.into_bytes()));
    }
    files.push(("index.md".into(), index.into_bytes()));
    Ok(files)
}

/// Never replace unrelated or changed exports, and never follow output symlinks.
pub fn write_files(directory: &Path, files: &[(String, Vec<u8>)]) -> Result<()> {
    for (name, _) in files {
        let mut components = Path::new(name).components();
        if !matches!(components.next(), Some(std::path::Component::Normal(_)))
            || components.next().is_some()
        {
            return Err(Error::invalid(
                "export filenames must be single path components",
            ));
        }
    }
    if fs::symlink_metadata(directory).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(Error::invalid("export directory must not be a symlink"));
    }
    fs::create_dir_all(directory)?;
    // Check every destination before publishing any file.
    for (name, bytes) in files {
        let path = directory.join(name);
        match fs::symlink_metadata(&path) {
            Ok(meta) => {
                if !meta.is_file() || meta.file_type().is_symlink() || fs::read(&path)? != *bytes {
                    return Err(Error::new("output_exists", format!("{} already contains different content; choose a fresh export directory", path.display())));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    for (name, bytes) in files {
        let path = directory.join(name);
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                file.write_all(bytes)?;
                file.sync_all()?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if fs::symlink_metadata(&path)?.file_type().is_symlink()
                    || fs::read(&path)? != *bytes
                {
                    return Err(Error::new(
                        "output_exists",
                        "export destination changed during write",
                    ));
                }
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}
