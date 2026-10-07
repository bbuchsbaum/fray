//! Durable routing and observed workflow state; never executes Mote.
use crate::{model::*, store};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

pub fn mutate(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    match req.op.as_str() {
        "dispatch_offer" => offer(conn, req, now),
        "dispatch_accept" => accept(conn, req, now),
        "dispatch_status" => confirmed(conn, req, now),
        "dispatch_sync" => sync(conn, req, now),
        "dispatch_handoff_packet" => packet(conn, req, now),
        "dispatch_handoff_record" => handoff_record(conn, req, now),
        _ => Err(Error::invalid("unknown dispatch mutation")),
    }
}
pub fn read(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    match req.op.as_str() {
        "dispatch_get" => get(conn, integer(&req.args, "id")?),
        "dispatch_list" => {
            let mut query = conn.prepare(
            "SELECT id FROM dispatch_offers WHERE status NOT IN ('returned','cancelled','handed_off') ORDER BY updated_ms,id LIMIT 20",
            )?;
            let ids = query
                .query_map([], |r| r.get::<_, i64>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({"offers":ids.into_iter().map(|id|get(conn,id)).collect::<Result<Vec<_>>>()?}))
        }
        "dispatch_candidates" => Ok(
            json!({"agents":eligible(conn,&req.actor,string(&req.args,"tag")?,req.args.get("id").and_then(Value::as_i64),now)?}),
        ),
        "dispatch_handoff_get" => {
            check_fields(&req.args, &["id", "for_accept"])?;
            let id = integer(&req.args, "id")?;
            let mut packet = handoff_get(conn, id)?;
            if req.args["for_accept"] == true {
                if packet["payload"]["to"] != req.actor {
                    return Err(Error::new(
                        "not_recipient",
                        "only intended recipient can accept this packet",
                    ));
                }
                if store::get_card(conn, id)?.terminal() {
                    return Err(Error::new(
                        "closed",
                        "handoff packet is terminal; no new ownership mutations are permitted",
                    ));
                }
                packet["accept_checked"] = json!(true);
            }
            Ok(packet)
        }
        "dispatch_attempt_live" => {
            Ok(json!({"live":attempt_live(conn,&get(conn,integer(&req.args,"id")?)?,now)?}))
        }
        _ => Err(Error::invalid("unknown dispatch read")),
    }
}

fn eligible(
    conn: &Connection,
    sender: &str,
    tag: &str,
    card: Option<i64>,
    now: i64,
) -> Result<Vec<String>> {
    let mut q=conn.prepare("SELECT name FROM agents WHERE enabled=1 AND name NOT IN (?1,'owner','mote','escalation') AND EXISTS(SELECT 1 FROM json_each(agents.topics) WHERE value='*' OR value=?2 OR ?2='*') ORDER BY name")?;
    let names = q
        .query_map(params![sender, tag], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut result = Vec::new();
    for name in names {
        let idle: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_status WHERE agent=? AND lower(trim(text))='idle')",
            [&name],
            |r| r.get(0),
        )?;
        let busy:bool=conn.query_row("SELECT EXISTS(SELECT 1 FROM terminal_turns WHERE agent=? AND busy_since_ms IS NOT NULL AND last_hook_ms>?)",params![name,now-crate::keepalive::BUSY_STALE_MS],|r|r.get(0))?;
        let muted: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM muted_cards WHERE agent=? AND card_id=?)",
            params![name, card],
            |r| r.get(0),
        )?;
        if !busy
            && !muted
            && (idle || store::agent_waiting(conn, &name, now)?)
            && store::agent_live(conn, &name, now)?
            && store::reachability(conn, &name, now)? != store::Reach::Absent
        {
            result.push(name);
        }
    }
    Ok(result)
}

fn offer(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    let a = &req.args;
    check_fields(
        a,
        &[
            "issue",
            "tag",
            "title",
            "body",
            "offer_ttl_s",
            "claim_ttl_s",
        ],
    )?;
    let issue = string(a, "issue")?;
    text(issue, "issue", 160, false)?;
    let tag = string(a, "tag")?;
    if !valid_topic(tag) {
        return Err(Error::invalid("invalid dispatch tag"));
    }
    let offer_ttl = bounded(a, "offer_ttl_s", 300, 1, 86400)?;
    let claim_ttl = bounded(a, "claim_ttl_s", 300, 1, 3600)?;
    text(string(a, "body")?, "body", 8000, false)?;
    let card = store::create_card(
        conn,
        &req.actor,
        &json!({"kind":"task","topic":tag,"title":string(a,"title")?,"summary":clip(string(a,"body")?,1900),"tags":[format!("mote:{issue}")]}),
        json!({"body":a["body"],"dispatch":true,"issue":issue}),
        now,
    )?;
    let id = integer(&card["card"], "id")?;
    let peers = eligible(conn, &req.actor, tag, Some(id), now)?;
    let to = peers.first();
    let status = if to.is_some() { "open" } else { "no_eligible" };
    conn.execute("INSERT INTO dispatch_offers(id,sender,issue,tag,generation,status,offered_to,attempted,offer_ttl_s,offer_until_ms,claim_ttl_s,created_ms,updated_ms) VALUES(?,?,?,?,1,?,?,?,?,?,?,?,?)",params![id,req.actor,issue,tag,status,to,serde_json::to_string(&to.into_iter().collect::<Vec<_>>())?,offer_ttl,now+offer_ttl*1000,claim_ttl,now,now])?;
    route(conn, &req.actor, id, to.map(String::as_str), status, now)?;
    Ok(json!({"offer":get(conn,id)?,"card":store::get_card(conn,id)?,"eligible":peers}))
}

fn route(
    conn: &Connection,
    actor: &str,
    id: i64,
    to: Option<&str>,
    status: &str,
    now: i64,
) -> Result<()> {
    conn.execute(
        "UPDATE cards SET assignee=?,rev=rev+1,updated_ms=? WHERE id=?",
        params![to, now, id],
    )?;
    store::emit(
        conn,
        actor,
        "dispatch_route",
        id,
        json!({"status":status,"recipient":to,"ownership":"Mote; explicit fray accept required"}),
        now,
        true,
    )?;
    Ok(())
}

fn accept(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    let a = &req.args;
    check_fields(a, &["id", "expect_generation", "key"])?;
    let id = integer(a, "id")?;
    let offer = get(conn, id)?;
    if store::get_card(conn, id)?.terminal() {
        return Err(Error::new("closed", "cannot accept a terminal offer"));
    }
    if offer["generation"] != integer(a, "expect_generation")? {
        return Err(Error::new("conflict", "offer generation changed"));
    }
    let key = string(a, "key")?;
    if offer["accepted_by"] == req.actor
        && offer["attempt_key"] == key
        && ["pending", "accepted", "stalled"].contains(&offer["status"].as_str().unwrap_or(""))
    {
        return Ok(offer);
    }
    if offer["status"] != "open" || offer["offer_until_ms"].as_i64().unwrap_or(0) <= now {
        return Err(Error::new(
            "conflict",
            format!(
                "offer {}: pending/current winner {}; generation {}",
                offer["status"], offer["accepted_by"], offer["generation"]
            ),
        ));
    }
    if !eligible(
        conn,
        string(&offer, "sender")?,
        string(&offer, "tag")?,
        Some(id),
        now,
    )?
    .contains(&req.actor)
    {
        return Err(Error::new(
            "dispatch_ineligible",
            "accept requires a live idle/waiting, unmuted peer matching the offer tag",
        ));
    }
    conn.execute("UPDATE dispatch_offers SET status='pending',accepted_by=?,attempt_key=?,attempt_session=?,attempt_peer_generation=(SELECT generation FROM peer_generations WHERE agent=?),attempt_started_ms=?,updated_ms=? WHERE id=?",params![req.actor,key,req.session,req.actor,now,now,id])?;
    store::emit(
        conn,
        &req.actor,
        "dispatch_attempt",
        id,
        json!({"pending_actor":req.actor,"operation_key":key,"ownership_confirmed":false}),
        now,
        true,
    )?;
    get(conn, id)
}

fn confirmed(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    let a = &req.args;
    check_fields(
        a,
        &["id", "expect_generation", "key", "claim", "progress_marker"],
    )?;
    let id = integer(a, "id")?;
    let offer = get(conn, id)?;
    if store::get_card(conn, id)?.terminal() {
        return Err(Error::new(
            "closed",
            "cannot confirm or reopen a terminal offer",
        ));
    }
    if offer["generation"] != a["expect_generation"]
        || offer["attempt_key"] != a["key"]
        || offer["accepted_by"] != req.actor
        || a["claim"]["holder"] != req.actor
    {
        return Err(Error::new(
            "conflict",
            "acceptance attempt or observed Mote holder changed",
        ));
    }
    if !attempt_live(conn, &offer, now)? {
        return Err(Error::new(
            "dispatch_session_ended",
            "original accepting session ended; reconcile before requeue",
        ));
    }
    let token = string(&a["claim"], "token")?;
    let lease = string(&a["claim"], "lease_until_ts")?;
    if mote_time(lease)? <= now {
        return Err(Error::new("mote_unconfirmed", "observed claim is expired"));
    }
    let progress = a.get("progress_marker").and_then(Value::as_str);
    conn.execute("UPDATE dispatch_offers SET status='accepted',observed_claim_token=?,observed_lease_until=?,progress_marker=coalesce(?,progress_marker),updated_ms=? WHERE id=?",params![token,lease,progress,now,id])?;
    conn.execute(
        "UPDATE cards SET status='active',assignee=?,rev=rev+1,updated_ms=? WHERE id=?",
        params![req.actor, now, id],
    )?;
    store::emit(
        conn,
        &req.actor,
        "dispatch_confirmed",
        id,
        json!({"claim_observation":a["claim"],"progress_marker":progress,"ownership":"Mote readback"}),
        now,
        true,
    )?;
    get(conn, id)
}

fn mote_time(value: &str) -> Result<i64> {
    crate::mote::parse_ts_ms(value).ok_or_else(|| Error::invalid("invalid Mote lease timestamp"))
}
fn attempt_live(conn: &Connection, offer: &Value, now: i64) -> Result<bool> {
    let Some(actor) = offer["accepted_by"].as_str() else {
        return Ok(false);
    };
    let enabled: bool = conn
        .query_row("SELECT enabled FROM agents WHERE name=?", [actor], |r| {
            r.get(0)
        })
        .optional()?
        .unwrap_or(false);
    if !enabled {
        return Ok(false);
    }
    let generation: Option<i64> = conn
        .query_row(
            "SELECT generation FROM peer_generations WHERE agent=?",
            [actor],
            |r| r.get(0),
        )
        .optional()?;
    if generation.map(|g| json!(g)) != Some(offer["attempt_peer_generation"].clone()) {
        return Ok(false);
    }
    if let Some(session) = offer["attempt_session"].as_str() {
        return Ok(conn.query_row("SELECT EXISTS(SELECT 1 FROM sessions WHERE agent=? AND session=? AND ended_ms IS NULL)",params![actor,session],|r|r.get(0))? && store::agent_live(conn,actor,now)?);
    }
    store::agent_live(conn, actor, now)
}

fn notify(conn: &Connection, actor: &str, offer: &Value, body: &str, now: i64) -> Result<()> {
    let sender = string(offer, "sender")?;
    let notice = store::create_card(
        conn,
        actor,
        &json!({"kind":"note","topic":format!("@{sender}"),"assignee":sender,"title":format!("Dispatch #{}: {}",offer["id"],clip(body,65)),"summary":clip(body,1900),"priority":1,"tags":[format!("dispatch:{}",offer["id"]),format!("mote:{}",offer["issue"].as_str().unwrap_or(""))]}),
        Value::Null,
        now,
    )?;
    store::force_delivery(
        conn,
        sender,
        integer(&notice["card"], "id")?,
        integer(&notice, "event_seq")?,
        now,
    )?;
    Ok(())
}

fn sync(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    let a = &req.args;
    check_fields(
        a,
        &[
            "id",
            "expect_generation",
            "expect_attempt",
            "claim",
            "transport_idle",
        ],
    )?;
    let id = integer(a, "id")?;
    let mut offer = get(conn, id)?;
    if store::get_card(conn, id)?.terminal() {
        conn.execute(
            "UPDATE dispatch_offers SET status='cancelled',updated_ms=? WHERE id=?",
            params![now, id],
        )?;
        return get(conn, id);
    }
    conn.execute(
        "UPDATE dispatch_offers SET updated_ms=? WHERE id=?",
        params![now, id],
    )?;
    if offer["generation"] != a["expect_generation"] {
        return Err(Error::new(
            "conflict",
            "offer generation changed during reconciliation",
        ));
    }
    if a["expect_attempt"]
        != json!({"actor":offer["accepted_by"],"key":offer["attempt_key"],"session":offer["attempt_session"],"peer_generation":offer["attempt_peer_generation"]})
    {
        return Err(Error::new(
            "conflict",
            "acceptance attempt changed during reconciliation; re-read before routing",
        ));
    }
    let pending =
        ["pending", "accepted", "stalled"].contains(&offer["status"].as_str().unwrap_or(""));
    if pending
        && offer["progress_marker"].is_null()
        && !attempt_live(conn, &offer, now)?
        && offer["status"] != "stalled"
    {
        conn.execute(
            "UPDATE dispatch_offers SET status='stalled',updated_ms=? WHERE id=?",
            params![now, id],
        )?;
        notify(
            conn,
            &req.actor,
            &offer,
            "Accepting peer disappeared before progress; Mote ownership is being reconciled",
            now,
        )?;
        offer["status"] = json!("stalled");
    }
    if pending && !a["claim"].is_null() {
        conn.execute("UPDATE dispatch_offers SET observed_claim_token=?,observed_lease_until=?,updated_ms=? WHERE id=?",params![a["claim"]["token"].as_str(),a["claim"]["lease_until_ts"].as_str(),now,id])?;
        return get(conn, id);
    }
    let ended = pending
        && offer["progress_marker"].is_null()
        && a["claim"].is_null()
        && a["transport_idle"] == true
        && (offer["observed_claim_token"].is_string()
            || now
                >= offer["attempt_started_ms"].as_i64().unwrap_or(now)
                    + offer["claim_ttl_s"].as_i64().unwrap_or(300) * 1000);
    let expired = ["open", "no_eligible"].contains(&offer["status"].as_str().unwrap_or(""))
        && now >= offer["offer_until_ms"].as_i64().unwrap_or(now);
    if !ended && !expired || !a["claim"].is_null() {
        return get(conn, id);
    }
    let attempted: Vec<String> = serde_json::from_value(offer["attempted"].clone())?;
    let peers = eligible(
        conn,
        string(&offer, "sender")?,
        string(&offer, "tag")?,
        Some(id),
        now,
    )?;
    let to = peers.iter().find(|p| !attempted.contains(p));
    let mut next_attempted = attempted;
    if let Some(to) = to {
        next_attempted.push(to.clone());
    }
    let status = if to.is_some() { "open" } else { "returned" };
    conn.execute("UPDATE dispatch_offers SET generation=generation+1,status=?,offered_to=?,attempted=?,offer_until_ms=?,accepted_by=NULL,attempt_key=NULL,attempt_session=NULL,attempt_peer_generation=NULL,attempt_started_ms=NULL,observed_claim_token=NULL,observed_lease_until=NULL,progress_marker=NULL,updated_ms=? WHERE id=?",params![status,to,serde_json::to_string(&next_attempted)?,now+offer["offer_ttl_s"].as_i64().unwrap_or(300)*1000,now,id])?;
    conn.execute("UPDATE cards SET status='open' WHERE id=?", [id])?;
    route(
        conn,
        &req.actor,
        id,
        to.map(String::as_str).or_else(|| offer["sender"].as_str()),
        status,
        now,
    )?;
    notify(
        conn,
        &req.actor,
        &offer,
        if ended {
            "Mote release/expiry confirmed; work requeued"
        } else {
            "Unaccepted offer expired; routed to the next eligible peer or returned to sender"
        },
        now,
    )?;
    get(conn, id)
}

fn get(conn: &Connection, id: i64) -> Result<Value> {
    let row:Option<String>=conn.query_row("SELECT json_object('id',id,'sender',sender,'issue',issue,'tag',tag,'generation',generation,'status',status,'offered_to',offered_to,'attempted',json(attempted),'offer_ttl_s',offer_ttl_s,'offer_until_ms',offer_until_ms,'claim_ttl_s',claim_ttl_s,'accepted_by',accepted_by,'attempt_key',attempt_key,'attempt_session',attempt_session,'attempt_peer_generation',attempt_peer_generation,'attempt_started_ms',attempt_started_ms,'observed_claim_token',observed_claim_token,'observed_lease_until',observed_lease_until,'progress_marker',progress_marker) FROM dispatch_offers WHERE id=?",[id],|r|r.get(0)).optional()?;
    serde_json::from_str(&row.ok_or_else(|| Error::new("not_found", "not a dispatch offer card"))?)
        .map_err(Into::into)
}

fn packet(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    let a = &req.args;
    check_fields(
        a,
        &[
            "source_card",
            "to",
            "issue",
            "key",
            "holder",
            "claim_token",
            "state",
            "next",
            "evidence",
            "carriers",
        ],
    )?;
    let source = store::get_card(conn, integer(a, "source_card")?)?;
    if !source
        .tags
        .contains(&format!("mote:{}", string(a, "issue")?))
    {
        return Err(Error::invalid(
            "handoff work must match source card's Mote reference",
        ));
    }
    if string(a, "holder")? != req.actor || string(a, "to")? == req.actor {
        return Err(Error::invalid(
            "handoff requires current holder and another recipient",
        ));
    }
    let key = string(a, "key")?;
    if let Some(id) = conn
        .query_row(
            "SELECT id FROM dispatch_handoffs WHERE sender=? AND operation_key=?",
            params![req.actor, key],
            |r| r.get::<_, i64>(0),
        )
        .optional()?
    {
        let old = handoff_get(conn, id)?;
        if old["payload"] != *a {
            return Err(Error::new(
                "idempotency_conflict",
                "handoff packet key already has different bytes",
            ));
        }
        return Ok(old);
    }
    if source.terminal() {
        return Err(Error::new("closed", "cannot hand off a terminal card"));
    }
    text(string(a, "state")?, "state", 4000, false)?;
    text(string(a, "next")?, "next", 4000, false)?;
    if !a["evidence"].is_array() || !a["carriers"].is_array() {
        return Err(Error::invalid("handoff evidence/carriers must be arrays"));
    }
    let to = string(a, "to")?;
    let result = store::create_card(
        conn,
        &req.actor,
        &json!({"kind":"task","topic":format!("@{to}"),"assignee":to,"title":format!("Handoff #{}: {}",source.id,clip(&source.title,65)),"summary":clip(&format!("{}\nNext: {}\nMote claim transfer and reservation adoption require explicit accept.",string(a,"state")?,string(a,"next")?),1900),"tags":[format!("mote:{}",string(a,"issue")?),format!("handoff-source:{}",source.id)]}),
        json!({"handoff":a}),
        now,
    )?;
    let id = integer(&result["card"], "id")?;
    conn.execute("INSERT INTO dispatch_handoffs(id,source_card,sender,recipient,issue,operation_key,payload,status,updated_ms) VALUES(?,?,?,?,?,?,?,'prepared',?)",params![id,source.id,req.actor,to,string(a,"issue")?,key,serde_json::to_string(a)?,now])?;
    handoff_get(conn, id)
}
fn handoff_get(conn: &Connection, id: i64) -> Result<Value> {
    let row: Option<(String, String, Option<String>)> = conn
        .query_row(
            "SELECT payload,status,receipt FROM dispatch_handoffs WHERE id=?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (payload, status, receipt) =
        row.ok_or_else(|| Error::new("not_found", "not a handoff packet card"))?;
    let card = store::get_card(conn, id)?;
    Ok(
        json!({"id":id,"payload":serde_json::from_str::<Value>(&payload)?,"status":status,"card_status":card.status,"receipt":receipt.map(|s|serde_json::from_str::<Value>(&s)).transpose()?}),
    )
}
fn handoff_record(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    let a = &req.args;
    check_fields(a, &["id", "status", "receipt"])?;
    let id = integer(a, "id")?;
    let old = handoff_get(conn, id)?;
    let status = string(a, "status")?;
    if !["transferred", "partial", "completed", "lost"].contains(&status) {
        return Err(Error::invalid("invalid handoff observation status"));
    }
    if old["payload"]["holder"] != req.actor && old["payload"]["to"] != req.actor {
        return Err(Error::new(
            "not_party",
            "only handoff sender/recipient may record an observed result",
        ));
    }
    if old["status"] == "completed" && status != "completed" {
        // An older sender retry cannot discard recipient adoption evidence.
        return Ok(old);
    }
    conn.execute(
        "UPDATE dispatch_handoffs SET status=?,receipt=?,updated_ms=? WHERE id=?",
        params![status, serde_json::to_string(&a["receipt"])?, now, id],
    )?;
    if matches!(status, "transferred" | "completed") {
        let source = integer(&old["payload"], "source_card")?;
        conn.execute(
            "UPDATE dispatch_offers SET status='handed_off',updated_ms=? WHERE id=?",
            params![now, source],
        )?;
    }
    let seq = store::emit(
        conn,
        &req.actor,
        "handoff_observed",
        id,
        json!({"status":status,"receipt":a["receipt"]}),
        now,
        true,
    )?;
    if matches!(status, "partial" | "lost") {
        conn.execute("UPDATE cards SET priority=1 WHERE id=?", [id])?;
        for party in [
            string(&old["payload"], "holder")?,
            string(&old["payload"], "to")?,
        ] {
            store::force_delivery(conn, party, id, integer(&seq, "event_seq")?, now)?;
        }
    }
    handoff_get(conn, id)
}
