//! Roster changes are session-local exposure, separate from conversation receipts.
use crate::model::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

pub fn joined(
    conn: &Connection,
    actor: &str,
    session: Option<&str>,
    was_enabled: bool,
    now: i64,
) -> Result<()> {
    let prior: Option<(i64, Option<String>)> = conn
        .query_row(
            "SELECT generation,session FROM peer_generations WHERE agent=?",
            [actor],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match prior {
        None => {
            conn.execute(
                "INSERT INTO peer_generations(agent,generation,session,joined_ms) VALUES(?,1,?,?)",
                params![actor, session, now],
            )?;
        }
        Some((generation, bound))
            if !was_enabled || session.is_some_and(|s| Some(s) != bound.as_deref()) =>
        {
            conn.execute("UPDATE peer_generations SET generation=?,session=coalesce(?,session),joined_ms=? WHERE agent=?", params![generation+1,session,now,actor])?;
        }
        _ => {}
    }
    Ok(())
}

pub fn delta(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    check_fields(&req.args, &["limit"])?;
    crate::store::registered(conn, &req.actor)?;
    let limit = bounded(&req.args, "limit", 4, 1, 20)?;
    let mut query = conn.prepare("SELECT a.name,a.role,a.last_seen_ms,p.generation,p.joined_ms FROM agents a JOIN peer_generations p ON p.agent=a.name LEFT JOIN peer_seen s ON s.peer=a.name AND s.reader=?1 AND s.session=?2 WHERE a.enabled=1 AND a.name<>?1 AND p.generation>coalesce(s.generation,0) ORDER BY p.joined_ms,a.name LIMIT ?3")?;
    let rows = query
        .query_map(params![req.actor, req.session, limit + 1], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, i64>(2)?,
                r.get::<_, i64>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > limit as usize;
    let mut peers = Vec::new();
    for (name, role, seen, generation, joined) in rows.into_iter().take(limit as usize) {
        let listener = crate::attention::listener_status(conn, &name, true, now)?;
        let listener = listener.map(|l|json!({"state":l["state"],"live":l["live"],"activation":l["selection"]["activation"]["mode"]}));
        peers.push(json!({"name":name,"role":role,"generation":generation,"joined_ms":joined,"recently_seen":now-seen<120000,"last_seen_ms":seen,"listener":listener}));
    }
    Ok(
        json!({"peers":peers,"more":more,"session_bound":req.session.is_some(),"note":"Newly observed peer registrations, not proof of responsiveness. Use exact names; arm a supported host listener for idle wake."}),
    )
}

pub fn presented(conn: &Connection, req: &Request) -> Result<Value> {
    check_fields(&req.args, &["store_id", "peers"])?;
    let session = req.session.as_deref().ok_or_else(|| {
        Error::new(
            "session_required",
            "peer presentation needs a bound host session",
        )
    })?;
    // Exposure belongs to a host session. Once that session ended, its
    // private peer cursor can never be used again; keep live companion and
    // terminal sessions independently, rather than accumulating every clear.
    conn.execute("DELETE FROM peer_seen WHERE EXISTS(SELECT 1 FROM sessions s WHERE s.agent=peer_seen.reader AND s.session=peer_seen.session AND s.ended_ms IS NOT NULL)", [])?;
    let store: String = conn.query_row("SELECT value FROM meta WHERE key='store_id'", [], |r| {
        r.get(0)
    })?;
    if string(&req.args, "store_id")? != store {
        return Err(Error::new(
            "receipt_mismatch",
            "peer display belongs to another store",
        ));
    }
    let peers = req.args["peers"]
        .as_array()
        .filter(|p| p.len() <= 20)
        .ok_or_else(|| {
            Error::invalid("peers must be an array of at most 20 displayed generations")
        })?;
    for peer in peers {
        check_fields(peer, &["name", "generation"])?;
        let name = string(peer, "name")?;
        let generation = integer(peer, "generation")?;
        let current: Option<i64> = conn
            .query_row(
                "SELECT generation FROM peer_generations WHERE agent=?",
                [name],
                |r| r.get(0),
            )
            .optional()?;
        if name == req.actor || !current.is_some_and(|g| generation > 0 && generation <= g) {
            return Err(Error::invalid(
                "peer presentation must name an existing displayed generation",
            ));
        }
        conn.execute("INSERT INTO peer_seen(reader,session,peer,generation) VALUES(?,?,?,?) ON CONFLICT(reader,session,peer) DO UPDATE SET generation=max(generation,excluded.generation)",params![req.actor,session,name,generation])?;
    }
    Ok(json!({"presented":peers.len(),"acknowledged":false}))
}
