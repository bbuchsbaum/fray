//! Read-only collaboration metrics and friction, derived from durable state.
//!
//! Every event stores the card as it stood after the change, so status and
//! assignee transitions replay from the log without extra tables. What the
//! store does not keep durably (acknowledgment history, which reads followed a
//! truncated preview, host wake latency) is listed as unavailable, never
//! reported as zero.
use crate::model::*;
use crate::store::{agent_live, agent_waiting, live_lanes};
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
    resolved_ms: Option<i64>,
    status: String,
    assignee: Option<String>,
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
    events: i64,
}

fn replay(conn: &Connection) -> Result<Replay> {
    let mut annotation_kind: HashMap<i64, String> = HashMap::new();
    let mut r = Replay {
        cards: HashMap::new(),
        order: Vec::new(),
        reassignments: Vec::new(),
        overrides: Vec::new(),
        events: 0,
    };
    let mut s = conn.prepare(
        "SELECT seq,ts_ms,actor,op,card_id,
           json_extract(payload,'$.card.kind'),json_extract(payload,'$.card.status'),
           json_extract(payload,'$.card.assignee'),json_extract(payload,'$.card.author'),
           json_extract(payload,'$.card.created_ms'),json_extract(payload,'$.card.title'),
           json_extract(payload,'$.detail.kind'),json_extract(payload,'$.detail.parent_card'),
           json_extract(payload,'$.detail.annotation_seq'),
           coalesce(json_array_length(payload,'$.detail.open_objections'),0)
         FROM events ORDER BY seq",
    )?;
    let mut rows = s.query([])?;
    while let Some(row) = rows.next()? {
        r.events += 1;
        let seq: i64 = row.get(0)?;
        let ts: i64 = row.get(1)?;
        let actor: String = row.get(2)?;
        let op: String = row.get(3)?;
        let id: i64 = row.get(4)?;
        let status: String = row.get::<_, Option<String>>(6)?.unwrap_or_default();
        let assignee: Option<String> = row.get(7)?;
        let detail_kind: Option<String> = row.get(11)?;
        let parent: Option<i64> = row.get(12)?;
        let annotation_seq: Option<i64> = row.get(13)?;
        let overridden: i64 = row.get(14)?;
        if op == "annotate" {
            if let Some(kind) = &detail_kind {
                annotation_kind.insert(seq, kind.clone());
            }
        }
        let h = r.cards.entry(id).or_insert_with(|| {
            r.order.push(id);
            Hist {
                kind: String::new(),
                author: String::new(),
                title: String::new(),
                created_ms: ts,
                first_response_ms: None,
                resolved_ms: None,
                status: String::new(),
                assignee: None,
                raised_by_kind: annotation_seq.and_then(|a| annotation_kind.get(&a).cloned()),
                parent,
            }
        });
        if h.kind.is_empty() {
            h.kind = row.get::<_, Option<String>>(5)?.unwrap_or_default();
            h.author = row.get::<_, Option<String>>(8)?.unwrap_or_default();
            h.title = row.get::<_, Option<String>>(10)?.unwrap_or_default();
            h.created_ms = row.get::<_, Option<i64>>(9)?.unwrap_or(ts);
            h.assignee = assignee.clone();
        }
        if op == "annotate" && actor != h.author && h.first_response_ms.is_none() {
            h.first_response_ms = Some(ts);
        }
        if status == "resolved" && h.resolved_ms.is_none() {
            h.resolved_ms = Some(ts);
        }
        // Reassigning away from someone is a possible misroute: a signal, not
        // proof. A first assignment is not counted.
        if op == "patch" && h.assignee.is_some() && assignee != h.assignee {
            r.reassignments.push((id, ts));
        }
        if overridden > 0 {
            r.overrides.push((id, ts));
        }
        h.status = status;
        h.assignee = assignee;
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
    let r = replay(conn)?;
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
    // it, not when the socket delivered it. Presented batches are retained for
    // a bounded time, so this covers recent history only.
    let mut s = conn.prepare(
        "SELECT min(b.created_ms)-e.ts_ms FROM presented_items i
           JOIN presented_batches b ON b.batch=i.batch
           JOIN events e ON e.seq=i.through_seq
         WHERE e.ts_ms>=? GROUP BY b.agent,i.card_id,i.through_seq",
    )?;
    let exposure = s
        .query_map([since], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;

    let mut s = conn.prepare(
        "SELECT d.agent,count(*),max(?1-e.ts_ms) FROM deliveries d
           JOIN events e ON e.seq=d.pending_seq
         WHERE d.pending_seq>d.ack_seq GROUP BY d.agent ORDER BY 3 DESC,1",
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
    let friction_notes = r
        .order
        .iter()
        .filter(|id| in_window(&r.cards[id]))
        .filter(|id| {
            conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM cards c, json_each(c.tags) t WHERE c.id=? AND t.value='friction')",
                [**id],
                |r| r.get::<_, bool>(0),
            )
            .unwrap_or(false)
        })
        .count();

    Ok(json!({"stats":{
        "window":{"since_ms":since,"until_ms":now,"all_history":window_ms.is_none(),"events_total":r.events},
        "asks":asks,
        "objections":objections,
        "routing":{"reassignments":reassigned.len(),"cards_reassigned":cards_reassigned.len(),
            "note":"A reassignment away from an assignee is a possible misroute: a signal, not proof."},
        "exposure":{"publish_to_first_shown":summary(exposure),
            "note":"From presented batches, which are retained for a bounded time: recent history only. Exposure is not handling."},
        "attention":{"unacked_items":unacked.iter().map(|a| a["items"].as_i64().unwrap_or(0)).sum::<i64>(),
            "agents":unacked,"note":"Current state, not windowed."},
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
    let r = replay(conn)?;
    let mut open: Vec<(i64, &Hist)> = r
        .order
        .iter()
        .map(|id| (*id, &r.cards[id]))
        .filter(|(_, h)| !closed(&h.status))
        .collect();
    open.sort_by_key(|(id, h)| (h.created_ms, *id));
    let reachable = |agent: &str| -> Result<bool> {
        Ok(agent_live(conn, agent, now)? || agent_waiting(conn, agent, now)?)
    };
    let mut unanswered = Vec::new();
    for (id, h) in open
        .iter()
        .filter(|(_, h)| is_ask(h) && h.first_response_ms.is_none())
    {
        let assignee_reachable = match &h.assignee {
            Some(a) => Some(reachable(a)?),
            None => None,
        };
        unanswered.push(
            json!({"id":id,"title":h.title,"author":h.author,"assignee":h.assignee,
            "age_ms":now-h.created_ms,"assignee_reachable":assignee_reachable}),
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
                if i["assignee_reachable"] == false {
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
