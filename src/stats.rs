//! Read-only collaboration metrics and friction, derived from durable state.
//!
//! Every event stores the card as it stood after the change, so status and
//! assignee transitions replay from the log without extra tables. What the
//! store does not keep durably (acknowledgment history, which reads followed a
//! truncated preview, host wake latency) is listed as unavailable, never
//! reported as zero.
use crate::model::*;
use crate::store::{live_lanes, reachability, Reach, OWNER};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use std::collections::HashMap;

/// Items listed per friction category; the count is always complete.
const FRICTION_LIMIT: usize = 5;

struct Hist {
    kind: String,
    author: String,
    title: String,
    created_ms: i64,
    first_response_ms: Option<i64>,
    /// When the card last became resolved; cleared if it is reopened.
    resolved_ms: Option<i64>,
    status: String,
    assignee: Option<String>,
    lease_owner: Option<String>,
    /// The annotation that raised this card as a linked follow-up, if any.
    raised_by_kind: Option<String>,
    parent: Option<i64>,
}

fn closed(status: &str) -> bool {
    matches!(status, "resolved" | "superseded" | "withdrawn")
}

/// Nearest-rank percentiles over durations in milliseconds.
fn summary(mut xs: Vec<i64>) -> Value {
    if xs.is_empty() {
        return json!({"n":0,"p50_ms":null,"p90_ms":null,"max_ms":null});
    }
    xs.sort_unstable();
    let rank = |p: f64| xs[((p * xs.len() as f64).ceil() as usize).clamp(1, xs.len()) - 1];
    json!({"n":xs.len(),"p50_ms":rank(0.5),"p90_ms":rank(0.9),"max_ms":xs[xs.len()-1]})
}

/// Replays the event log into per-card histories, plus the counts that are
/// event-scoped rather than card-scoped (reassignments, overrides).
struct Replay {
    cards: HashMap<i64, Hist>,
    order: Vec<i64>,
    reassignments: Vec<(i64, i64)>,
    overrides: Vec<(i64, i64)>,
}

/// Which cards to replay. Each selected card is replayed from its first event,
/// so transitions always compare against the true previous state.
enum Cards {
    All,
    /// Cards with any event at or after this time.
    TouchedSince(i64),
    /// Cards that are not closed now.
    Open,
}

fn replay(conn: &Connection, cards: Cards) -> Result<Replay> {
    let mut r = Replay {
        cards: HashMap::new(),
        order: Vec::new(),
        reassignments: Vec::new(),
        overrides: Vec::new(),
    };
    let (filter, since) = match cards {
        Cards::All => ("1", 0),
        Cards::TouchedSince(t) => (
            "e.card_id IN (SELECT card_id FROM events WHERE ts_ms>=?1)",
            t,
        ),
        Cards::Open => (
            "e.card_id IN (SELECT id FROM cards WHERE status NOT IN ('resolved','superseded','withdrawn'))",
            0,
        ),
    };
    // The kind of the annotation that raised a follow-up is read by primary
    // key, so the parent card's history is not needed.
    let mut s = conn.prepare(&format!(
        "SELECT e.ts_ms,e.actor,e.op,e.card_id,
           json_extract(e.payload,'$.card.kind'),json_extract(e.payload,'$.card.status'),
           json_extract(e.payload,'$.card.assignee'),json_extract(e.payload,'$.card.author'),
           json_extract(e.payload,'$.card.created_ms'),json_extract(e.payload,'$.card.title'),
           json_extract(e.payload,'$.card.lease_owner'),
           json_extract(e.payload,'$.detail.parent_card'),
           CASE WHEN e.op='post' THEN (SELECT json_extract(a.payload,'$.detail.kind') FROM events a
             WHERE a.seq=json_extract(e.payload,'$.detail.annotation_seq') AND a.op='annotate') END,
           coalesce(json_array_length(e.payload,'$.detail.open_objections'),0)
         FROM events e WHERE {filter} AND ?1>=0 ORDER BY e.seq"
    ))?;
    let mut rows = s.query([since])?;
    while let Some(row) = rows.next()? {
        let ts: i64 = row.get(0)?;
        let actor: String = row.get(1)?;
        let op: String = row.get(2)?;
        let id: i64 = row.get(3)?;
        let kind: Option<String> = row.get(4)?;
        let status: String = row.get::<_, Option<String>>(5)?.unwrap_or_default();
        let assignee: Option<String> = row.get(6)?;
        let title: Option<String> = row.get(9)?;
        let lease_owner: Option<String> = row.get(10)?;
        let overridden: i64 = row.get(13)?;
        let h = match r.cards.entry(id) {
            std::collections::hash_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::hash_map::Entry::Vacant(e) => {
                r.order.push(id);
                e.insert(Hist {
                    kind: String::new(),
                    author: row.get::<_, Option<String>>(7)?.unwrap_or_default(),
                    title: String::new(),
                    created_ms: row.get::<_, Option<i64>>(8)?.unwrap_or(ts),
                    first_response_ms: None,
                    resolved_ms: None,
                    status: String::new(),
                    assignee: assignee.clone(),
                    lease_owner: lease_owner.clone(),
                    raised_by_kind: row.get(12)?,
                    parent: row.get(11)?,
                })
            }
        };
        // A response comes from whoever the card is addressed to (assignee or
        // lease holder) or the owner; a bystander's note is not an answer.
        // An unaddressed card may be answered by anyone but its author.
        if op == "annotate" && actor != h.author && h.first_response_ms.is_none() {
            let addressed = h.assignee.is_some() || h.lease_owner.is_some();
            if !addressed
                || actor == OWNER
                || h.assignee.as_deref() == Some(actor.as_str())
                || h.lease_owner.as_deref() == Some(actor.as_str())
            {
                h.first_response_ms = Some(ts);
            }
        }
        if status == "resolved" && h.status != "resolved" {
            h.resolved_ms = Some(ts);
        } else if status != "resolved" {
            h.resolved_ms = None;
        }
        // Reassigning away from someone is a possible misroute: a signal, not
        // proof. A first assignment is not counted.
        if op == "patch" && h.assignee.is_some() && assignee != h.assignee {
            r.reassignments.push((id, ts));
        }
        if overridden > 0 {
            r.overrides.push((id, ts));
        }
        if let Some(kind) = kind {
            h.kind = kind;
        }
        if let Some(title) = title {
            h.title = title;
        }
        h.status = status;
        h.assignee = assignee;
        h.lease_owner = lease_owner;
    }
    Ok(r)
}

fn is_objection(h: &Hist) -> bool {
    h.raised_by_kind.as_deref() == Some("objection")
}

fn is_ask(h: &Hist) -> bool {
    h.kind == "question" && !is_objection(h)
}

/// `fray stats`: the plan's success metrics, computed from the store.
pub(crate) fn stats(conn: &Connection, window_ms: Option<i64>, now: i64) -> Result<Value> {
    let since = window_ms.map_or(0, |w| now.saturating_sub(w));
    let r = replay(
        conn,
        if window_ms.is_some() {
            Cards::TouchedSince(since)
        } else {
            Cards::All
        },
    )?;
    let events_total: i64 = conn.query_row("SELECT count(*) FROM events", [], |r| r.get(0))?;
    let in_window = |h: &Hist| h.created_ms >= since;
    let (mut asks, mut objections) = (json!({}), json!({}));
    for (key, pick) in [
        ("asks", is_ask as fn(&Hist) -> bool),
        ("objections", is_objection),
    ] {
        let items: Vec<&Hist> = r
            .order
            .iter()
            .map(|id| &r.cards[id])
            .filter(|h| pick(h) && in_window(h))
            .collect();
        let responded: Vec<i64> = items
            .iter()
            .filter_map(|h| h.first_response_ms.map(|t| t - h.created_ms))
            .collect();
        let resolved: Vec<i64> = items
            .iter()
            .filter_map(|h| h.resolved_ms.map(|t| t - h.created_ms))
            .collect();
        let open: Vec<&&Hist> = items.iter().filter(|h| !closed(&h.status)).collect();
        let v = json!({
            "created": items.len(),
            "responded": responded.len(),
            "resolved": resolved.len(),
            "closed_unresolved": items.iter().filter(|h| matches!(h.status.as_str(), "superseded" | "withdrawn")).count(),
            "open": open.len(),
            "first_response": summary(responded),
            "resolution": summary(resolved),
            "oldest_open_age_ms": open.iter().map(|h| now - h.created_ms).max(),
            "oldest_open_unanswered_age_ms": open.iter().filter(|h| h.first_response_ms.is_none()).map(|h| now - h.created_ms).max(),
        });
        if key == "asks" {
            asks = v;
        } else {
            objections = v;
        }
    }
    objections["overrides"] = json!(r.overrides.iter().filter(|(_, t)| *t >= since).count());
    let reassigned: Vec<i64> = r
        .reassignments
        .iter()
        .filter(|(_, t)| *t >= since)
        .map(|(id, _)| *id)
        .collect();
    let mut cards_reassigned = reassigned.clone();
    cards_reassigned.sort_unstable();
    cards_reassigned.dedup();

    // First exposure of each published version: when a host was first shown
    // it, not when the socket delivered it. Presented batches are pruned oldest
    // first, so only versions published after an agent's oldest retained batch
    // are certain to have their first showing retained; older ones would
    // report a later re-showing as the first.
    let mut s = conn.prepare(
        "SELECT min(b.created_ms)-e.ts_ms FROM presented_items i
           JOIN presented_batches b ON b.batch=i.batch
           JOIN events e ON e.seq=i.through_seq
         WHERE e.ts_ms>=?1
           AND e.ts_ms>=(SELECT min(o.created_ms) FROM presented_batches o WHERE o.agent=b.agent)
         GROUP BY b.agent,i.card_id,i.through_seq",
    )?;
    let exposure = s
        .query_map([since], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    // Age runs from the oldest peer event the agent has not acknowledged, not
    // from the card's newest one. Agents who left and cards muted by the
    // agent are excluded, except a question assigned to it, whose outcome
    // must still reach it.
    let mut s = conn.prepare(
        "SELECT d.agent,count(*),max(?1-(SELECT min(e.ts_ms) FROM events e
             WHERE e.card_id=d.card_id AND e.seq>d.ack_seq AND e.seq<=d.pending_seq AND e.actor<>d.agent))
         FROM deliveries d JOIN agents a ON a.name=d.agent AND a.enabled=1
         WHERE d.pending_seq>d.ack_seq
           AND (NOT EXISTS(SELECT 1 FROM muted_cards m WHERE m.agent=d.agent AND m.card_id=d.card_id)
             OR EXISTS(SELECT 1 FROM cards c WHERE c.id=d.card_id AND c.kind='question' AND c.assignee=d.agent))
         GROUP BY d.agent ORDER BY 3 DESC,1",
    )?;
    let unacked = s
        .query_map([now], |r| {
            Ok(json!({"agent":r.get::<_,String>(0)?,"items":r.get::<_,i64>(1)?,"oldest_age_ms":r.get::<_,i64>(2)?}))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let lane_count =
        |sql: &str| -> Result<i64> { Ok(conn.query_row(sql, params![since], |r| r.get(0))?) };
    let live = live_lanes(conn, now)?;
    let lanes = json!({
        "taken": lane_count("SELECT count(*) FROM lanes WHERE created_ms>=?")?,
        "released": lane_count("SELECT count(*) FROM lanes WHERE released_ms>=? AND released_reason NOT LIKE 'stale;%'")?,
        "released_stale": lane_count("SELECT count(*) FROM lanes WHERE released_ms>=? AND released_reason LIKE 'stale;%'")?,
        "live_held": live.iter().filter(|l| l["state"] == "held").count(),
        "live_queued": live.iter().filter(|l| l["state"] == "queued").count(),
        "live_stale": live.iter().filter(|l| l["stale"] == true).count(),
    });
    let friction_notes: i64 = conn.query_row(
        "SELECT count(*) FROM cards c WHERE c.created_ms>=?
           AND EXISTS(SELECT 1 FROM json_each(c.tags) t WHERE t.value='friction')",
        [since],
        |r| r.get(0),
    )?;

    Ok(json!({"stats":{
        "window":{"since_ms":since,"until_ms":now,"all_history":window_ms.is_none(),"events_total":events_total},
        "asks":asks,
        "objections":objections,
        "routing":{"reassignments":reassigned.len(),"cards_reassigned":cards_reassigned.len(),
            "note":"A reassignment away from an assignee is a possible misroute: a signal, not proof."},
        "exposure":{"publish_to_first_shown":summary(exposure),
            "note":"From presented batches, which are retained for a bounded time: recent history only. Exposure is not handling."},
        "attention":{"unacked_items":unacked.iter().map(|a| a["items"].as_i64().unwrap_or(0)).sum::<i64>(),
            "agents":unacked,"note":"Current state, not windowed. Excludes agents who left and cards they muted."},
        "lanes":lanes,
        "friction_notes":friction_notes,
        "unavailable":[
            {"metric":"host wake and model response latency","reason":"measured by the wake benchmark (epic child 3), not recorded by the store"},
            {"metric":"acknowledgment latency over time","reason":"the store keeps only the current acknowledged head per agent and card"},
            {"metric":"hand-fetched truncations","reason":"reads are not recorded as events"},
            {"metric":"reviews per landing","reason":"reviews are not first-class records yet (epic child 1)"},
            {"metric":"lane handovers","reason":"a handover updates the lane in place without a durable record"}
        ]
    }}))
}

/// `fray friction` with no text: the worst current offenders, oldest first.
pub(crate) fn friction(conn: &Connection, now: i64) -> Result<Value> {
    let r = replay(conn, Cards::Open)?;
    let mut open: Vec<(i64, &Hist)> = r
        .order
        .iter()
        .map(|id| (*id, &r.cards[id]))
        .filter(|(_, h)| !closed(&h.status))
        .collect();
    open.sort_by_key(|(id, h)| (h.created_ms, *id));
    // The owner is reached through the owner queue (`fray owner review`), not
    // a session; requests to it wait for the owner, they are not lost.
    // Reachable means wakeable (no-silent-stalls R1): a present agent with
    // nothing armed may never take another turn.
    let reachable = |agent: &str| -> Result<bool> {
        Ok(agent == OWNER || reachability(conn, agent, now)? == Reach::Wakeable)
    };
    let mut unanswered = Vec::new();
    for (id, h) in open
        .iter()
        .filter(|(_, h)| is_ask(h) && h.first_response_ms.is_none())
    {
        let assignee_reachable = match h.assignee.as_deref() {
            Some(OWNER) | None => None,
            Some(a) => Some(reachable(a)?),
        };
        // A deadline (R4) if the asker set one, else the soft 24-hour
        // default, which applies only here.
        let card = crate::store::get_card_pub(conn, *id)?;
        let due = crate::store::ask_deadline(conn, *id, &card.author)?.map(|(by, _)| by);
        let overdue = match due {
            Some(_) => crate::store::ask_overdue(conn, &card, now)?.is_some(),
            None => now - h.created_ms > 24 * 3_600_000,
        };
        unanswered.push(
            json!({"id":id,"title":h.title,"author":h.author,"assignee":h.assignee,
            "age_ms":now-h.created_ms,"assignee_reachable":assignee_reachable,
            "awaiting_owner":h.assignee.as_deref()==Some(OWNER),
            "due_ms":due,"overdue":overdue,"soft_deadline":due.is_none()}),
        );
    }
    let objections: Vec<Value> = open
        .iter()
        .filter(|(_, h)| is_objection(h))
        .map(|(id, h)| json!({"id":id,"title":h.title,"objector":h.author,"parent":h.parent,"age_ms":now-h.created_ms}))
        .collect();
    // Requests addressed to someone nothing can currently reach.
    let mut addressed: HashMap<String, (i64, i64)> = HashMap::new();
    for (_, h) in open.iter().filter(|(_, h)| h.kind == "question") {
        if let Some(a) = &h.assignee {
            let e = addressed.entry(a.clone()).or_insert((0, 0));
            e.0 += 1;
            e.1 = e.1.max(now - h.created_ms);
        }
    }
    let mut unreachable = Vec::new();
    for (agent, (items, oldest)) in addressed {
        if !reachable(&agent)? {
            unreachable.push(json!({"agent":agent,"open_requests":items,"oldest_age_ms":oldest}));
        }
    }
    unreachable.sort_by_key(|v| -v["oldest_age_ms"].as_i64().unwrap_or(0));
    let stale: Vec<Value> = live_lanes(conn, now)?
        .into_iter()
        .filter(|l| l["stale"] == true)
        .map(|mut l| {
            l["age_ms"] = json!(now - l["created_ms"].as_i64().unwrap_or(now));
            l
        })
        .collect();
    let mut s = conn.prepare(
        "SELECT c.id,c.title,c.author,c.created_ms FROM cards c
         WHERE EXISTS(SELECT 1 FROM json_each(c.tags) t WHERE t.value='friction')
         ORDER BY c.created_ms DESC,c.id DESC",
    )?;
    let notes = s
        .query_map([], |r| {
            Ok(json!({"id":r.get::<_,i64>(0)?,"title":r.get::<_,String>(1)?,"author":r.get::<_,String>(2)?,"age_ms":now-r.get::<_,i64>(3)?}))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let section = |items: Vec<Value>| {
        let total = items.len();
        json!({"total":total,"items":items.into_iter().take(FRICTION_LIMIT).collect::<Vec<_>>()})
    };
    Ok(json!({"friction":{
        "unanswered_asks":section(unanswered),
        "open_objections":section(objections),
        "unreachable_addressed":section(unreachable),
        "stale_lanes":section(stale),
        "notes":section(notes),
        "time_ms":now
    }}))
}

fn age(ms: &Value) -> String {
    match ms.as_i64() {
        None => "—".into(),
        Some(ms) if ms < 60_000 => format!("{}s", ms / 1000),
        Some(ms) if ms < 3_600_000 => format!("{}m", ms / 60_000),
        Some(ms) if ms < 172_800_000 => format!("{:.1}h", ms as f64 / 3_600_000.0),
        Some(ms) => format!("{:.1}d", ms as f64 / 86_400_000.0),
    }
}

fn dist(v: &Value) -> String {
    if v["n"] == 0 {
        return "n=0".into();
    }
    format!(
        "n={} p50={} p90={} max={}",
        v["n"],
        age(&v["p50_ms"]),
        age(&v["p90_ms"]),
        age(&v["max_ms"])
    )
}

/// Human rendering for `fray stats`.
pub fn stats_text(v: &Value, clean: fn(&str) -> String) -> String {
    let s = &v["stats"];
    let mut out = String::new();
    let w = &s["window"];
    out.push_str(&if w["all_history"] == true {
        format!("FRAY STATS  all history  ({} events)\n", w["events_total"])
    } else {
        format!(
            "FRAY STATS  last {}  ({} events in store)\n",
            age(&json!(
                w["until_ms"].as_i64().unwrap_or(0) - w["since_ms"].as_i64().unwrap_or(0)
            )),
            w["events_total"]
        )
    });
    for (label, key) in [("Asks", "asks"), ("Objections", "objections")] {
        let a = &s[key];
        out.push_str(&format!(
            "\n{label}: created {}  responded {}  resolved {}  closed unresolved {}  open {}\n  first response  {}\n  resolution      {}\n  oldest open {}  oldest unanswered {}\n",
            a["created"], a["responded"], a["resolved"], a["closed_unresolved"], a["open"],
            dist(&a["first_response"]), dist(&a["resolution"]),
            age(&a["oldest_open_age_ms"]), age(&a["oldest_open_unanswered_age_ms"])
        ));
        if key == "objections" {
            out.push_str(&format!("  resolved over objection: {}\n", a["overrides"]));
        }
    }
    let r = &s["routing"];
    out.push_str(&format!(
        "\nRouting: {} reassignments on {} cards (possible misroutes: a signal, not proof)\n",
        r["reassignments"], r["cards_reassigned"]
    ));
    out.push_str(&format!(
        "Exposure, publish to first shown: {} (recent history only)\n",
        dist(&s["exposure"]["publish_to_first_shown"])
    ));
    out.push_str(&format!(
        "Unacked attention now: {} items\n",
        s["attention"]["unacked_items"]
    ));
    for a in s["attention"]["agents"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "  {:<24} {:>4} items, oldest {}\n",
            clean(a["agent"].as_str().unwrap_or("")),
            a["items"],
            age(&a["oldest_age_ms"])
        ));
    }
    let l = &s["lanes"];
    out.push_str(&format!(
        "Lanes: taken {}  released {}  released stale {}  live held {}  queued {}  stale {}\n",
        l["taken"],
        l["released"],
        l["released_stale"],
        l["live_held"],
        l["live_queued"],
        l["live_stale"]
    ));
    out.push_str(&format!("Friction notes: {}\n", s["friction_notes"]));
    out.push_str("\nNot measured here:\n");
    for u in s["unavailable"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "  {}: {}\n",
            clean(u["metric"].as_str().unwrap_or("")),
            clean(u["reason"].as_str().unwrap_or(""))
        ));
    }
    out
}

/// Renders one friction item, given the control-character cleaner.
type Line = fn(&Value, fn(&str) -> String) -> String;

/// Human rendering for `fray friction`.
pub fn friction_text(v: &Value, clean: fn(&str) -> String) -> String {
    let f = &v["friction"];
    let mut out = String::from("FRAY FRICTION  current offenders, oldest first\n");
    let sections: [(&str, &str, Line); 5] = [
        ("Unanswered asks", "unanswered_asks", |i, c| {
            format!(
                "#{} {} ({} -> {}{})",
                i["id"],
                c(i["title"].as_str().unwrap_or("")),
                c(i["author"].as_str().unwrap_or("")),
                c(i["assignee"].as_str().unwrap_or("anyone")),
                if i["awaiting_owner"] == true {
                    ", in the owner queue"
                } else if i["overdue"] == true {
                    ", overdue"
                } else if i["assignee_reachable"] == false {
                    ", unreachable"
                } else {
                    ""
                }
            )
        }),
        ("Open objections", "open_objections", |i, c| {
            format!(
                "#{} {} (by {}, on #{})",
                i["id"],
                c(i["title"].as_str().unwrap_or("")),
                c(i["objector"].as_str().unwrap_or("")),
                i["parent"]
            )
        }),
        (
            "Addressed to someone nothing can reach",
            "unreachable_addressed",
            |i, c| {
                format!(
                    "{}: {} open requests",
                    c(i["agent"].as_str().unwrap_or("")),
                    i["open_requests"]
                )
            },
        ),
        ("Stale lanes", "stale_lanes", |i, c| {
            format!(
                "lane {} {} ({})",
                i["id"],
                c(i["agent"].as_str().unwrap_or("")),
                c(&i["paths"].to_string())
            )
        }),
        ("Friction notes", "notes", |i, c| {
            format!(
                "#{} {} (by {})",
                i["id"],
                c(i["title"].as_str().unwrap_or("")),
                c(i["author"].as_str().unwrap_or(""))
            )
        }),
    ];
    for (label, key, line) in sections {
        let s = &f[key];
        out.push_str(&format!("\n{label}: {}\n", s["total"]));
        for i in s["items"].as_array().into_iter().flatten() {
            let when = if key == "unreachable_addressed" {
                &i["oldest_age_ms"]
            } else {
                &i["age_ms"]
            };
            out.push_str(&format!("  {:>6}  {}\n", age(when), line(i, clean)));
        }
    }
    out
}
