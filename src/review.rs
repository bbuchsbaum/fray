//! Immutable review references and advisory verdicts. Mote remains the acceptance ledger.
use crate::model::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

pub fn version(value: &str) -> Result<&str> {
    let valid = value.split_once(':').is_some_and(|(kind, hash)| {
        ((kind == "git" && matches!(hash.len(), 40 | 64))
            || (kind == "manifest" && hash.len() == 64))
            && hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    });
    if !valid {
        return Err(Error::invalid("version must be git:<full lowercase 40/64 hex commit> or manifest:<64 hex SHA-256>; mutable refs and +dirty are not review identities"));
    }
    Ok(value)
}

pub fn subject(conn: &Connection, id: i64) -> Result<Option<Value>> {
    Ok(conn.query_row("SELECT baseline,candidate,subject_rev,mote_ref FROM review_subjects WHERE card_id=?", [id], |r| Ok(json!({"baseline":r.get::<_,String>(0)?,"candidate":r.get::<_,String>(1)?,"subject_rev":r.get::<_,i64>(2)?,"mote_ref":r.get::<_,Option<String>>(3)?,"advisory":true}))).optional()?)
}

pub fn context(conn: &Connection, id: i64, history: bool) -> Result<Value> {
    let Some(mut result) = subject(conn, id)? else {
        return Ok(Value::Null);
    };
    let mut query = conn.prepare("SELECT event_seq,reviewer,subject_rev,version,verdict FROM review_verdicts WHERE card_id=? ORDER BY event_seq DESC LIMIT ?")?;
    let limit = if history { 21 } else { 1 };
    let mut verdicts = query.query_map(params![id,limit],|r| Ok(json!({"event_seq":r.get::<_,i64>(0)?,"reviewer":r.get::<_,String>(1)?,"subject_rev":r.get::<_,i64>(2)?,"version":r.get::<_,String>(3)?,"verdict":r.get::<_,String>(4)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for verdict in &mut verdicts {
        verdict["stale"] = json!(verdict["subject_rev"] != result["subject_rev"]);
    }
    // The compact header must describe the peer currently asked to review,
    // rather than letting an unsolicited verdict replace that peer's result.
    let card = crate::store::get_card(conn, id)?;
    result["requested_reviewer"] = json!(card.assignee);
    result["latest_verdict"] = if let Some(reviewer) = card.assignee.as_deref() {
        let mut latest: Option<Value> = conn.query_row(
            "SELECT event_seq,reviewer,subject_rev,version,verdict FROM review_verdicts WHERE card_id=? AND reviewer=? ORDER BY event_seq DESC LIMIT 1",
            params![id, reviewer],
            |r| Ok(json!({"event_seq":r.get::<_,i64>(0)?,"reviewer":r.get::<_,String>(1)?,"subject_rev":r.get::<_,i64>(2)?,"version":r.get::<_,String>(3)?,"verdict":r.get::<_,String>(4)?})),
        ).optional()?;
        if let Some(verdict) = &mut latest {
            verdict["stale"] = json!(verdict["subject_rev"] != result["subject_rev"]);
        }
        latest.unwrap_or(Value::Null)
    } else {
        verdicts.first().cloned().unwrap_or(Value::Null)
    };
    if history {
        result["verdicts_more"] = json!(verdicts.len() > 20);
        verdicts.truncate(20);
        result["verdicts"] = json!(verdicts);
    }
    Ok(result)
}

pub fn prepare_verdict(conn: &Connection, id: i64, actor: &str, args: &Value) -> Result<Value> {
    check_fields(args, &["verdict", "at", "expect"])?;
    let verdict = string(args, "verdict")?;
    if !["approve", "object", "blocked"].contains(&verdict) {
        return Err(Error::invalid("verdict: approve|object|blocked"));
    }
    let at = version(string(args, "at")?)?;
    let current =
        subject(conn, id)?.ok_or_else(|| Error::new("not_review", "card has no review subject"))?;
    let expected = integer(args, "expect")?;
    if current["subject_rev"] != expected || current["candidate"] != at {
        return Err(Error::new(
            "conflict",
            format!(
                "review subject changed: now s{} at {}; reread before giving a verdict",
                current["subject_rev"], current["candidate"]
            ),
        ));
    }
    let card = crate::store::get_card(conn, id)?;
    if card.terminal() {
        return Err(Error::new(
            "closed",
            "reopen or create a new review before giving a verdict",
        ));
    }
    if card.author == actor {
        return Err(Error::new(
            "self_review",
            "the request author cannot give its own peer verdict",
        ));
    }
    Ok(
        json!({"verdict":verdict,"version":at,"subject_rev":expected,"baseline":current["baseline"],"mote_ref":current["mote_ref"],"advisory":true}),
    )
}

pub fn record_verdict(
    conn: &Connection,
    id: i64,
    actor: &str,
    event: i64,
    verdict: &Value,
) -> Result<()> {
    conn.execute("INSERT INTO review_verdicts(event_seq,card_id,reviewer,subject_rev,version,verdict) VALUES(?,?,?,?,?,?)",params![event,id,actor,integer(verdict,"subject_rev")?,string(verdict,"version")?,string(verdict,"verdict")?])?;
    Ok(())
}
