use crate::model::*;
use rusqlite::{
    params, params_from_iter, types::Value as SqlValue, Connection, OptionalExtension, Row,
    TransactionBehavior,
};
use serde_json::{json, Value};
use std::{path::Path, time::Duration};

const COLS:&str="id,rev,kind,topic,title,summary,status,priority,pinned,tags,author,assignee,lease_owner,lease_until_ms,fence,created_ms,updated_ms,last_seq";
/// The reserved identity for the project owner (see owner-authority design).
pub const OWNER: &str = "owner";
/// The reserved identity that authors attention derived from Mote. It never
/// joins or acts; it only lets a Mote event reach the agent who synced it.
pub const MOTE: &str = "mote";
/// The reserved identity that authors escalations of stuck requests
/// (no-silent-stalls R3). Like `mote`, it never joins or acts.
pub const ESCALATION: &str = "escalation";

/// Names a reader could mistake for the owner (OWNER, 0wner, o-w-n-e-r,
/// owner1 …): case, separators and a numeric suffix are ignored.
fn owner_lookalike(name: &str) -> bool {
    let alnum: String = name
        .to_lowercase()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .collect();
    // A numeric suffix first (owner1), then common character substitutions
    // (0wner, own3r, 0vvner).
    let folded: String = alnum
        .trim_end_matches(|c: char| c.is_ascii_digit())
        .chars()
        .map(|c| match c {
            '0' => 'o',
            '3' => 'e',
            '1' => 'l',
            _ => c,
        })
        .collect::<String>()
        .replace("vv", "w");
    folded == OWNER
}

/// Marks an ask card the owner approved or declined; set only by the owner.
const DECIDED: &str = "authority:decided";

fn decided_tags(c: &Card) -> Vec<String> {
    let mut tags = c.tags.clone();
    if !tags.iter().any(|t| t == DECIDED) {
        tags.push(DECIDED.to_owned());
    }
    tags.retain(|t| !t.is_empty());
    tags
}

/// Cards only the owner may change: the owner's own, and requests the owner
/// has decided (so a decision can never be moved onto different content).
fn owner_controlled(c: &Card) -> bool {
    c.author == OWNER || c.tags.iter().any(|t| t == DECIDED)
}

/// Only the owner may use `authority` tags (in any case or spelling), so a
/// tag can never read as owner authority on an agent's card.
fn reserve_authority_tags(actor: &str, tags: &[String]) -> Result<()> {
    let reserved = |t: &String| {
        t.to_lowercase()
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .collect::<String>()
            .starts_with("authority")
    };
    if actor != OWNER && tags.iter().any(reserved) {
        return Err(Error::new(
            "reserved_owner",
            "authority: tags are reserved for the owner",
        ));
    }
    Ok(())
}
const ACTIVE: &str = "c.status NOT IN ('resolved','superseded','withdrawn')";
// Topic scope is an attention filter, not a visibility/security boundary.
const RELEVANT:&str="(c.pinned=1 OR c.topic='*' OR c.author=? OR c.assignee=? OR c.lease_owner=? OR EXISTS(SELECT 1 FROM agents a WHERE a.name=? AND (a.role='steward' OR EXISTS(SELECT 1 FROM participants p WHERE p.agent=a.name AND p.card_id=c.id) OR (c.kind IN ('task','question') AND c.assignee IS NULL) OR EXISTS(SELECT 1 FROM json_each(a.topics) t WHERE t.value=c.topic OR (t.value='*' AND substr(c.topic,1,1)<>'@')))))";
// One predicate for selected inboxes, blocking waits, runner packets and progress checks.
const INVOLVED: &str = "(c.author=?1 OR c.assignee=?1 OR c.lease_owner=?1 OR EXISTS(SELECT 1 FROM participants p WHERE p.agent=?1 AND p.card_id=c.id) OR EXISTS(SELECT 1 FROM agents a,json_each(a.topics) t WHERE a.name=?1 AND t.value<>'*' AND t.value=c.topic))";
// An old mute must not hide a question subsequently assigned to this actor.
const UNMUTED: &str = "(NOT EXISTS(SELECT 1 FROM muted_cards m WHERE m.agent=?1 AND m.card_id=c.id) OR (c.kind='question' AND c.assignee=?1))";

pub fn selection(args: &Value) -> Result<&str> {
    let value = args
        .get("selection")
        .map(|_| string(args, "selection"))
        .transpose()?
        .unwrap_or("all");
    if !["all", "involved"].contains(&value) {
        return Err(Error::invalid("selection: all|involved"));
    }
    Ok(value)
}

pub struct InboxSelection<'a> {
    pub mode: &'a str,
    pub card_ids: Vec<i64>,
    pub addressed_to_me: bool,
    pub unresolved: bool,
    pub kinds: Vec<String>,
    pub min_priority: Option<i64>,
}
impl<'a> InboxSelection<'a> {
    pub fn parse(args: &'a Value) -> Result<Self> {
        Ok(Self {
            mode: selection(args)?,
            card_ids: attention_cards(args)?,
            addressed_to_me: boolean(args, "addressed_to_me", false)?,
            unresolved: boolean(args, "unresolved", false)?,
            kinds: attention_kinds(args)?,
            min_priority: args
                .get("min_priority")
                .map(|_| bounded(args, "min_priority", 3, 0, 3))
                .transpose()?,
        })
    }
    pub(crate) fn condition(&self) -> String {
        let mut condition = if self.mode == "involved" {
            INVOLVED
        } else {
            "1"
        }
        .to_owned();
        condition.push_str(" AND ");
        condition.push_str(UNMUTED);
        if !self.card_ids.is_empty() {
            // Typed integers only: no caller-controlled SQL fragments.
            let ids = self
                .card_ids
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(",");
            condition.push_str(&format!(" AND c.id IN ({ids})"));
        }
        if self.addressed_to_me {
            condition.push_str(" AND c.assignee=?1");
        }
        if self.unresolved {
            condition.push_str(" AND ");
            condition.push_str(ACTIVE);
        }
        if !self.kinds.is_empty() {
            // Only fixed literals enter this SQL, even if a caller built the
            // selection directly; an unknown kind matches nothing.
            let kinds = self
                .kinds
                .iter()
                .map(|kind| {
                    ATTENTION_KINDS
                        .iter()
                        .find(|known| **known == kind.as_str())
                        .map_or("NULL".to_owned(), |known| format!("'{known}'"))
                })
                .collect::<Vec<_>>()
                .join(",");
            condition.push_str(&format!(" AND (c.kind IN ({kinds}) OR EXISTS(SELECT 1 FROM events e WHERE e.card_id=c.id AND e.op='annotate' AND e.actor<>d.agent AND e.seq>d.ack_seq AND e.seq<=d.pending_seq AND json_extract(e.payload,'$.detail.kind') IN ({kinds})) OR EXISTS(SELECT 1 FROM events child JOIN events parent ON parent.seq=json_extract(child.payload,'$.detail.annotation_seq') WHERE child.card_id=c.id AND child.op='post' AND parent.op='annotate' AND json_extract(parent.payload,'$.detail.kind') IN ({kinds})))"));
        }
        if let Some(priority) = self.min_priority {
            condition.push_str(&format!(" AND c.priority<={priority}"));
        }
        condition
    }
}
impl<'a> From<&'a str> for InboxSelection<'a> {
    fn from(mode: &'a str) -> Self {
        Self {
            mode,
            card_ids: Vec::new(),
            addressed_to_me: false,
            unresolved: false,
            kinds: Vec::new(),
            min_priority: None,
        }
    }
}

const ATTENTION_KINDS: [&str; 8] = [
    "goal",
    "task",
    "question",
    "decision",
    "note",
    "evidence",
    "objection",
    "answer",
];

pub(crate) fn attention_cards(args: &Value) -> Result<Vec<i64>> {
    let Some(value) = args.get("card_ids") else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| Error::invalid("card_ids must be an array of positive integers"))?;
    if values.is_empty() || values.len() > 16 {
        return Err(Error::invalid("card_ids must contain 1..16 IDs"));
    }
    let mut ids = values
        .iter()
        .map(|id| {
            id.as_i64()
                .filter(|id| *id > 0)
                .ok_or_else(|| Error::invalid("card_ids must be positive integers"))
        })
        .collect::<Result<Vec<_>>>()?;
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

pub(crate) fn attention_kinds(args: &Value) -> Result<Vec<String>> {
    let Some(value) = args.get("kinds") else {
        return Ok(Vec::new());
    };
    let values = value
        .as_array()
        .ok_or_else(|| Error::invalid("kinds must be a nonempty array"))?;
    if values.is_empty() || values.len() > 8 {
        return Err(Error::invalid(
            "kinds must contain 1..8 card or annotation kinds",
        ));
    }
    values
        .iter()
        .map(|value| {
            let kind = value
                .as_str()
                .ok_or_else(|| Error::invalid("kinds must contain strings"))?;
            if !ATTENTION_KINDS.contains(&kind) {
                return Err(Error::invalid("unknown attention kind"));
            }
            Ok(kind.to_owned())
        })
        .collect()
}

pub struct Store {
    pub conn: Connection,
}
impl Store {
    pub fn open(path: &Path, normal: bool) -> Result<Self> {
        let conn = Connection::open(path)?;
        Self::configure(&conn, normal)?;
        Ok(Self { conn })
    }
    pub fn memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        Self::configure(&conn, false)?;
        Ok(Self { conn })
    }
    fn configure(conn: &Connection, normal: bool) -> Result<()> {
        conn.busy_timeout(Duration::from_secs(5))?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > 3 {
            return Err(Error::new(
                "schema_version",
                "database was created by a newer Fray",
            ));
        }
        conn.execute_batch(
            "PRAGMA foreign_keys=ON; PRAGMA journal_mode=WAL; PRAGMA cache_size=-16384;",
        )?;
        conn.execute_batch(if normal {
            "PRAGMA synchronous=NORMAL;"
        } else {
            "PRAGMA synchronous=FULL;"
        })?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        conn.execute_batch(include_str!("schema.sql"))?;
        conn.execute_batch(include_str!("dispatch.sql"))?;
        conn.execute("INSERT OR IGNORE INTO delivery_routes(agent,card_id,routed_at_ms) SELECT agent,card_id,NULL FROM deliveries", [])?;
        conn.execute("INSERT OR IGNORE INTO peer_generations(agent,generation,session,joined_ms) SELECT name,1,(SELECT session FROM sessions WHERE agent=agents.name AND ended_ms IS NULL AND session NOT GLOB 'keepalive:*' ORDER BY last_seen_ms DESC LIMIT 1),joined_ms FROM agents WHERE enabled=1",[])?;
        if version == 1 {
            // Preserve every receipt. Reconstruct deliberate participation, not old fan-out.
            conn.execute_batch("INSERT OR IGNORE INTO participants(agent,card_id) SELECT DISTINCT e.actor,e.card_id FROM events e JOIN agents a ON a.name=e.actor;")?;
        }
        conn.execute(
            "INSERT OR IGNORE INTO meta(key,value) VALUES('store_id',?)",
            [random_key()?],
        )?;
        conn.execute_batch("COMMIT")?;
        Ok(())
    }
    pub fn highwater(&self) -> Result<i64> {
        highwater(&self.conn)
    }
    /// A wait in progress: the agent is reachable (it will hear what
    /// arrives), which is not the same as active. It never touches the
    /// activity timestamps, so the WAIT_HOLD_MS window cannot renew itself.
    /// A wait with no card, kind, priority, addressed or unresolved filter
    /// passes its own `wake` id: it will wake for anything assigned to the
    /// agent, so its row makes the agent wakeable (see `reachability`) until
    /// `wait_ended`. Each wait has its own row, so a filtered or finished
    /// wait never hides another that is still armed.
    pub fn touch(
        &self,
        actor: &str,
        session: Option<&str>,
        wake: Option<&str>,
        now: i64,
    ) -> Result<()> {
        // One row per name and session, so a keepalive's waits never
        // overwrite its terminal's.
        self.conn.execute(
            "INSERT INTO session_waits(agent,session,refreshed_ms) SELECT name,coalesce(?2,''),?3 FROM agents WHERE name=?1 AND enabled=1 ON CONFLICT(agent,session) DO UPDATE SET refreshed_ms=excluded.refreshed_ms",
            params![actor, session, now],
        )?;
        self.conn.execute(
            "DELETE FROM session_waits WHERE refreshed_ms<=?",
            [now - WAIT_FRESH_MS],
        )?;
        // A keepalive is wakeable through its live controller alone, which
        // knows when it is paused, stopping or deferred to a busy terminal;
        // a wake row would outlast those.
        let wake = wake.filter(|_| !crate::keepalive::is_session(session));
        if let Some(id) = wake {
            self.conn.execute(
                "INSERT INTO wake_waits(wait_id,agent,refreshed_ms) SELECT ?1,name,?3 FROM agents WHERE name=?2 AND enabled=1 ON CONFLICT(wait_id) DO UPDATE SET refreshed_ms=excluded.refreshed_ms",
                params![id, actor, now],
            )?;
            // Rows a stopped daemon never removed.
            self.conn.execute(
                "DELETE FROM wake_waits WHERE refreshed_ms<=?",
                [now - WAIT_FRESH_MS],
            )?;
        }
        Ok(())
    }
    /// The wait has ended (answered, timed out, hung up or failed): it no
    /// longer makes its agent wakeable.
    pub fn wait_ended(&self, wake: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM wake_waits WHERE wait_id=?", [wake])?;
        Ok(())
    }
    /// Record a keepalive start for the request's bound session (see
    /// `keepalive::begin`); `cwd` was already checked against the repository.
    /// The daemon spawns the drive only after this commits.
    pub fn keepalive_begin(
        &mut self,
        req: &Request,
        cwd: &str,
        log: &str,
        budget: i64,
        now: i64,
    ) -> Result<Value> {
        if !valid_name(&req.actor) || req.actor == OWNER {
            return Err(Error::invalid(
                "set --as/FRAY_AGENT to the terminal's own identity",
            ));
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        registered(&tx, &req.actor)?;
        bind_session(&tx, req, now)?;
        let result = crate::keepalive::begin(&tx, req, cwd, log, budget, now)?;
        tx.commit()?;
        Ok(result)
    }
    pub fn keepalive_spawned(&self, agent: &str, pid: u32) -> Result<()> {
        crate::keepalive::spawned(&self.conn, agent, pid)
    }
    pub fn keepalive_abort(&self, agent: &str, now: i64) -> Result<()> {
        crate::keepalive::abort(&self.conn, agent, now)
    }
    pub fn keepalive_stop_requested(&self, agent: &str, session: &str) -> Result<bool> {
        crate::keepalive::stop_requested(&self.conn, agent, session)
    }
    pub fn identity(&self) -> Result<String> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key='store_id'", [], |r| {
                r.get(0)
            })?)
    }
    pub fn execute(&mut self, req: &Request) -> Result<Value> {
        self.execute_at(req, now_ms())
    }
    pub fn execute_at(&mut self, req: &Request, now: i64) -> Result<Value> {
        if !req.args.is_object() {
            return Err(Error::invalid("args must be an object"));
        }
        let write = matches!(
            req.op.as_str(),
            "join"
                | "leave"
                | "heartbeat"
                | "post"
                | "send"
                | "patch"
                | "annotate"
                | "claim"
                | "renew"
                | "release"
                | "ack"
                | "expose"
                | "present"
                | "follow"
                | "unfollow"
                | "mute"
                | "unmute"
                | "controller"
                | "owner_decide"
                | "owner_answer"
                | "lane_take"
                | "lane_release"
                | "set_status"
                | "mote_bind"
                | "mote_operation_prepare"
                | "mote_operation_checkpoint"
                | "mote_operation_finalize"
                | "mote_ingest"
                | "mote_sync_failed"
                | "mote_requests_sync"
                | "escalate_tick"
                | "peer_present"
                | "review_request"
                | "review_subject"
                | "mote_review_request"
                | "mote_review_successor"
                | "keepalive_stop"
                | "keepalive_usage"
                | "keepalive_claim"
                | "terminal_turn"
                | "dispatch_offer"
                | "dispatch_accept"
                | "dispatch_handoff_packet"
                | "dispatch_handoff_record"
                | "dispatch_sync"
                | "dispatch_status"
        );
        if !write {
            let mut v = read(&self.conn, req, now)?;
            v["store_id"] = json!(self.identity()?);
            return Ok(v);
        }
        if !valid_name(&req.actor) {
            return Err(Error::invalid(
                "set --as/FRAY_AGENT to a unique terminal identity (1-80 name characters)",
            ));
        }
        // The owner identity writes only through owner operations, and owner
        // operations only as the owner. Collision prevention against accidents
        // and injected text, not authentication (docs/design/owner-authority.md).
        let owner_op = req.op.starts_with("owner_");
        if req.op == "join"
            && req
                .actor
                .to_lowercase()
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                == MOTE
        {
            return Err(Error::new(
                "reserved_mote",
                "`mote` is reserved for attention derived from Mote; choose another name",
            ));
        }
        if req.op == "join"
            && req
                .actor
                .to_lowercase()
                .chars()
                .filter(char::is_ascii_alphanumeric)
                .collect::<String>()
                == ESCALATION
        {
            return Err(Error::new(
                "reserved_escalation",
                "`escalation` is reserved for escalations of stuck requests; choose another name",
            ));
        }
        if req.actor != OWNER && req.op == "join" && owner_lookalike(&req.actor) {
            return Err(Error::new(
                "reserved_owner",
                "that name could be mistaken for the owner; choose another",
            ));
        }
        if (req.actor == OWNER) != owner_op {
            return Err(Error::new(
                "reserved_owner",
                "the owner identity is used only through `fray owner ...` from the owner's interactive terminal",
            ));
        }
        if let Some(key) = &req.key {
            text(key, "key", 128, false)?;
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut signature = req.clone();
        signature.key = None;
        let signature = serde_json::to_string(&signature)?;
        if let Some(key) = &req.key {
            let prior: Option<(String, String)> = tx
                .query_row(
                    "SELECT request,response FROM requests WHERE actor=? AND key=?",
                    params![req.actor, key],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let Some((original, response)) = prior {
                if original != signature {
                    return Err(Error::new(
                        "idempotency_conflict",
                        "key already used for a different request",
                    ));
                }
                return Ok(serde_json::from_str(&response)?);
            }
        }
        if owner_op {
            tx.execute(
                "INSERT OR IGNORE INTO agents(name,role,topics,enabled,joined_ms,last_seen_ms) VALUES(?,'worker',?,0,?,0)",
                params![OWNER, "[]", now],
            )?;
        } else if req.op != "join" {
            registered(&tx, &req.actor)?;
        }
        let replaced = bind_session(&tx, req, now)?;
        // A transaction-local clock lets the insert trigger record first
        // routing without scanning the entire deliveries table on each write.
        tx.execute("INSERT INTO meta(key,value) VALUES('routing_clock_ms',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value", [now.to_string()])?;
        let mut result = mutate(&tx, req, now)?;
        tx.execute("DELETE FROM meta WHERE key='routing_clock_ms'", [])?;
        crate::keepalive::acted(&tx, req, &result)?;
        // The terminal's /clear or compaction moves its keepalive along.
        if let (true, Some(session), Some("clear" | "compact")) = (
            req.op == "join",
            req.session.as_deref(),
            req.args.get("continued").and_then(Value::as_str),
        ) {
            if !crate::keepalive::is_session(Some(session)) {
                crate::keepalive::rebind(&tx, &req.actor, session)?;
            }
        }
        // A keepalive speaks beside its terminal, not as a new arrival: its
        // session never renews the name's peer generation.
        if req.session.is_some()
            && !crate::keepalive::is_session(req.session.as_deref())
            && !matches!(
                req.op.as_str(),
                "join" | "leave" | "present" | "expose" | "peer_present" | "terminal_turn"
            )
            && req.actor != OWNER
        {
            crate::presence::joined(&tx, &req.actor, req.session.as_deref(), true, now)?;
        }
        if let Some(replaced) = replaced {
            result["session_replaced"] = replaced;
        }
        if req.op == "leave" {
            tx.execute(
                "UPDATE sessions SET ended_ms=?,ended_reason='left' WHERE agent=? AND ended_ms IS NULL AND (?3 IS NULL OR session=?3)",
                params![now, req.actor, req.session],
            )?;
            // `leave` from the terminal stops its keepalive too.
            tx.execute(
                "UPDATE keepalives SET stop_requested=1 WHERE agent=?",
                [&req.actor],
            )?;
        }
        // A listener registering what it displayed is not the agent acting,
        // nor is a hook's turn bookkeeping; keep presence tied to deliberate
        // activity.
        if !matches!(
            req.op.as_str(),
            "present" | "peer_present" | "terminal_turn"
        ) {
            tx.execute(
                "UPDATE agents SET last_seen_ms=? WHERE name=?",
                params![now, req.actor],
            )?;
        }
        let identity: String =
            tx.query_row("SELECT value FROM meta WHERE key='store_id'", [], |r| {
                r.get(0)
            })?;
        result["store_id"] = json!(identity);
        if let Some(key) = &req.key {
            tx.execute(
                "INSERT INTO requests(actor,key,request,response) VALUES(?,?,?,?)",
                params![req.actor, key, signature, serde_json::to_string(&result)?],
            )?;
        }
        tx.commit()?;
        Ok(result)
    }
    pub fn events(&self, after: i64, limit: i64) -> Result<Vec<Value>> {
        events(&self.conn, after, None, limit)
    }
    pub fn attention(
        &self,
        actor: &str,
        after: i64,
        limit: i64,
        fresh: bool,
        now: i64,
    ) -> Result<Value> {
        inbox(&self.conn, actor, after, limit, fresh, "all".into(), 0, now)
    }
    pub fn selected_attention(
        &self,
        actor: &str,
        after: i64,
        limit: i64,
        selection: &str,
        now: i64,
    ) -> Result<Value> {
        inbox(
            &self.conn,
            actor,
            after,
            limit,
            false,
            selection.into(),
            0,
            now,
        )
    }
    pub fn filtered_attention(
        &self,
        actor: &str,
        after: i64,
        limit: i64,
        selection: InboxSelection<'_>,
        now: i64,
    ) -> Result<Value> {
        inbox(
            &self.conn,
            actor,
            after,
            limit,
            false,
            selection,
            ADDRESSED_FULL_TEXT_BUDGET,
            now,
        )
    }
}
fn highwater(conn: &Connection) -> Result<i64> {
    Ok(conn.query_row("SELECT coalesce(max(seq),0) FROM events", [], |r| r.get(0))?)
}
pub(crate) fn registered(conn: &Connection, actor: &str) -> Result<()> {
    let exists: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM agents WHERE name=? AND enabled=1)",
        [actor],
        |r| r.get(0),
    )?;
    if !exists {
        return Err(Error::new(
            "not_joined",
            format!("{actor:?} has not joined; run fray --as NAME join"),
        ));
    }
    Ok(())
}
fn assignee_known(conn: &Connection, who: &Option<String>) -> Result<()> {
    if let Some(who) = who {
        let exists: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM agents WHERE name=?)",
            [who],
            |r| r.get(0),
        )?;
        if !exists {
            let near = nearest_agents(conn, who)?;
            let hint = if near.is_empty() {
                String::new()
            } else {
                format!(" Did you mean {}?", near.join(", "))
            };
            return Err(Error::new(
                "unknown_agent",
                format!(
                    "no agent named {who:?} has joined.{hint} To leave a message for an agent that will join later, use send --pending {who}."
                ),
            ));
        }
    }
    Ok(())
}

/// Registered names that are probably what a mistyped or shortened name meant:
/// a prefix either way (codex -> codex-attention-0923), or a small edit distance.
fn nearest_agents(conn: &Connection, who: &str) -> Result<Vec<String>> {
    let mut s = conn.prepare("SELECT name FROM agents")?;
    let names = s
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let wanted = who.to_lowercase();
    let mut scored: Vec<(usize, String)> = names
        .into_iter()
        .filter_map(|name| {
            let lower = name.to_lowercase();
            let score = if lower.starts_with(&wanted) || wanted.starts_with(&lower) {
                0
            } else {
                edit_distance(&lower, &wanted)
            };
            (score <= (wanted.chars().count() / 4).max(2)).then_some((score, name))
        })
        .collect();
    scored.sort();
    Ok(scored.into_iter().take(3).map(|(_, name)| name).collect())
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let above = row[j + 1];
            row[j + 1] = (above + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(ca != *cb));
            diagonal = above;
        }
    }
    row[b.len()]
}
fn row_card(r: &Row<'_>) -> rusqlite::Result<Card> {
    let raw: String = r.get(9)?;
    let tags = serde_json::from_str(&raw).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(9, rusqlite::types::Type::Text, Box::new(e))
    })?;
    Ok(Card {
        id: r.get(0)?,
        rev: r.get(1)?,
        kind: r.get(2)?,
        topic: r.get(3)?,
        title: r.get(4)?,
        summary: r.get(5)?,
        status: r.get(6)?,
        priority: r.get(7)?,
        pinned: r.get(8)?,
        tags,
        author: r.get(10)?,
        assignee: r.get(11)?,
        lease_owner: r.get(12)?,
        lease_until_ms: r.get(13)?,
        fence: r.get(14)?,
        created_ms: r.get(15)?,
        updated_ms: r.get(16)?,
        last_seq: r.get(17)?,
    })
}
pub(crate) fn get_card(conn: &Connection, id: i64) -> Result<Card> {
    conn.query_row(
        &format!("SELECT {COLS} FROM cards WHERE id=?"),
        [id],
        row_card,
    )
    .optional()?
    .ok_or_else(|| Error::new("not_found", format!("card {id}")))
}
fn validate_card(c: &Card) -> Result<()> {
    if !["goal", "task", "question", "decision", "note"].contains(&c.kind.as_str()) {
        return Err(Error::invalid("kind: goal|task|question|decision|note"));
    }
    if ![
        "open",
        "active",
        "blocked",
        "resolved",
        "superseded",
        "withdrawn",
    ]
    .contains(&c.status.as_str())
    {
        return Err(Error::invalid(
            "status: open|active|blocked|resolved|superseded|withdrawn",
        ));
    }
    if !valid_topic(&c.topic) {
        return Err(Error::invalid("invalid topic"));
    }
    text(&c.title, "title", 160, false)?;
    text(&c.summary, "summary", 2000, false)?;
    if !(0..=3).contains(&c.priority) {
        return Err(Error::invalid("priority is 0 (urgent) through 3 (FYI)"));
    }
    tags(&json!(c.tags))?;
    Ok(())
}
fn check_lease(c: &Card, actor: &str, fence: i64, now: i64, allow_expired: bool) -> Result<()> {
    if c.lease_owner.as_deref() != Some(actor)
        || c.fence != fence
        || (!allow_expired && c.lease_until_ms <= now)
    {
        return Err(Error::new("lease_lost",format!("card {} requires the current owner's live fencing token; read and reclaim if expired",c.id)));
    }
    Ok(())
}
pub(crate) fn emit(
    conn: &Connection,
    actor: &str,
    op: &str,
    id: i64,
    extra: Value,
    now: i64,
    notify: bool,
) -> Result<Value> {
    conn.execute(
        "INSERT INTO events(ts_ms,actor,op,card_id,payload) VALUES(?,?,?,?, '{}')",
        params![now, actor, op, id],
    )?;
    let seq = conn.last_insert_rowid();
    conn.execute("UPDATE cards SET last_seq=? WHERE id=?", params![seq, id])?;
    let card = get_card(conn, id)?;
    let payload = json!({"card":card,"detail":extra});
    let serialized = serde_json::to_string(&payload)?;
    conn.execute(
        "UPDATE events SET payload=? WHERE seq=?",
        params![serialized, seq],
    )?;
    conn.execute(
        "INSERT INTO event_fts(rowid,text) VALUES(?,?)",
        params![seq, serialized],
    )?;
    if notify {
        // Fan-out and head update commit together. Leave suppresses every route.
        // Mute suppresses a card unless it is a request assigned to the recipient,
        // open or closed: its final outcome must reach them (#19 FU-A).
        conn.execute("INSERT INTO deliveries(agent,card_id,pending_seq)
          SELECT a.name,?1,?2 FROM agents a WHERE a.name<>?3 AND a.enabled=1
            AND (NOT EXISTS(SELECT 1 FROM muted_cards m WHERE m.agent=a.name AND m.card_id=?1)
              OR (?10=1 AND a.name=?7)) AND (
            a.name=?6 OR a.name=?7 OR a.name=?8 OR
            EXISTS(SELECT 1 FROM participants p WHERE p.agent=a.name AND p.card_id=?1) OR
            ?4=1 OR ?5='*' OR ?9=1 OR a.role='steward' OR
              EXISTS(SELECT 1 FROM json_each(a.topics) t WHERE t.value=?5 OR (t.value='*' AND substr(?5,1,1)<>'@')))
          ON CONFLICT(agent,card_id) DO UPDATE SET pending_seq=excluded.pending_seq",
          params![id,seq,actor,card.pinned,card.topic,card.author,card.assignee,card.lease_owner,op=="post" && matches!(card.kind.as_str(),"task"|"question") && card.assignee.is_none(),card.kind=="question"])?;
        conn.execute(
            "INSERT OR IGNORE INTO participants(agent,card_id) VALUES(?,?)",
            params![actor, id],
        )?;
        // Participation neither sends the actor their own event nor acknowledges
        // unseen updates. Existing receipts (including legacy zero rows) stay intact.
    }
    Ok(json!({"card":card,"event_seq":seq}))
}
pub(crate) fn create_card(
    conn: &Connection,
    actor: &str,
    args: &Value,
    detail: Value,
    now: i64,
) -> Result<Value> {
    let mut card: Card = serde_json::from_value(
        json!({"id":0,"rev":1,"kind":"note","topic":"general","title":"","summary":"","status":"open","priority":2,"pinned":false,"tags":[],"author":actor,"assignee":null,"lease_owner":null,"lease_until_ms":0,"fence":0,"created_ms":now,"updated_ms":now,"last_seq":0}),
    )?;
    apply_fields(&mut card, args)?;
    validate_card(&card)?;
    reserve_authority_tags(actor, &card.tags)?;
    assignee_known(conn, &card.assignee)?;
    conn.execute("INSERT INTO cards(kind,topic,title,summary,status,priority,pinned,tags,author,assignee,created_ms,updated_ms) VALUES(?,?,?,?,?,?,?,?,?,?,?,?)",params![card.kind,card.topic,card.title,card.summary,card.status,card.priority,card.pinned,serde_json::to_string(&card.tags)?,actor,card.assignee,now,now])?;
    emit(
        conn,
        actor,
        "post",
        conn.last_insert_rowid(),
        detail,
        now,
        true,
    )
}
pub(crate) fn mutate(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    let a = &req.args;
    let actor = req.actor.as_str();
    match req.op.as_str() {
        op if op.starts_with("dispatch_") => crate::dispatch::mutate(conn, req, now),
        "mote_review_request" => crate::mote_workflow::review_request(conn, req, now),
        "mote_review_successor" => crate::mote_workflow::successor(conn, req, now),
        "mote_operation_prepare" => {
            crate::mote_workflow::prepare(conn, actor, req.session.as_deref(), a, now)
        }
        "mote_operation_checkpoint" => crate::mote_workflow::checkpoint(conn, actor, a, now),
        "mote_operation_finalize" => crate::mote_workflow::finalize(conn, actor, a, now),
        "join" => {
            check_fields(a, &["role", "topics", "takeover", "continued"])?;
            let existing: Option<(String, String, bool)> = conn
                .query_row(
                    "SELECT role,topics,enabled FROM agents WHERE name=?",
                    [actor],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            let role = a
                .get("role")
                .map(|_| string(a, "role"))
                .transpose()?
                .unwrap_or(existing.as_ref().map(|e| e.0.as_str()).unwrap_or("worker"));
            if !["worker", "steward", "reviewer"].contains(&role) {
                return Err(Error::invalid("role: worker|steward|reviewer"));
            }
            let topics = if let Some(v) = a.get("topics") {
                serde_json::to_string(&tags(v)?)?
            } else {
                existing
                    .as_ref()
                    .map(|e| e.1.clone())
                    .unwrap_or("[\"*\"]".into())
            };
            conn.execute("INSERT INTO agents(name,role,topics,joined_ms,last_seen_ms) VALUES(?,?,?,?,?) ON CONFLICT(name) DO UPDATE SET role=excluded.role,topics=excluded.topics,enabled=1,last_seen_ms=excluded.last_seen_ms",params![actor,role,topics,now,now])?;
            crate::presence::joined(
                conn,
                actor,
                req.session
                    .as_deref()
                    .filter(|s| !crate::keepalive::is_session(Some(s))),
                existing.as_ref().is_some_and(|e| e.2),
                now,
            )?;
            // Seed from live heads, never from the historical event stream. On resume,
            // refresh the pending head without changing any explicit acknowledgment.
            let sql=format!("INSERT INTO deliveries(agent,card_id,pending_seq) SELECT ?1,c.id,c.last_seq FROM cards c WHERE ({ACTIVE} OR c.assignee=?1 OR c.author=?1 OR c.lease_owner=?1 OR EXISTS(SELECT 1 FROM participants p WHERE p.agent=?1 AND p.card_id=c.id) OR EXISTS(SELECT 1 FROM deliveries d WHERE d.agent=?1 AND d.card_id=c.id)) AND {RELEVANT} AND {UNMUTED} AND NOT EXISTS(SELECT 1 FROM events e WHERE e.seq=c.last_seq AND e.actor=?) ON CONFLICT(agent,card_id) DO UPDATE SET pending_seq=max(deliveries.pending_seq,excluded.pending_seq)");
            conn.execute(&sql, params![actor, actor, actor, actor, actor, actor])?;
            let mut result = brief(conn, actor, 12000, now)?;
            let retained: i64 = conn.query_row(&format!("SELECT count(*) FROM deliveries d JOIN cards c ON c.id=d.card_id WHERE d.agent=? AND d.pending_seq>d.ack_seq AND NOT coalesce({RELEVANT},0)"), params![actor,actor,actor,actor,actor], |r| r.get(0))?;
            result["subscriptions"] = json!({"topics":serde_json::from_str::<Value>(&topics)?,"retained_pending_outside_scope":retained,"note":"Existing receipts are retained, not acknowledged. Incidental receipt does not follow a conversation. Steward still receives all traffic; drive defaults to involved selection."});
            Ok(result)
        }
        "mute" | "unmute" => {
            check_fields(a, &["id"])?;
            let id = integer(a, "id")?;
            let card = get_card(conn, id)?;
            if req.op == "mute" {
                // Consistent with the delivery exemption: a request assigned to
                // you is never muted, open or closed, so a mute is never accepted
                // and then silently ignored (#19 review FU-1).
                if card.kind == "question" && card.assignee.as_deref() == Some(actor) {
                    return Err(Error::new("cannot_mute_assigned_request", "a question or objection assigned to you cannot be muted (its outcome must reach you); resolve it or arrange a handoff"));
                }
                conn.execute(
                    "INSERT OR IGNORE INTO muted_cards(agent,card_id) VALUES(?,?)",
                    params![actor, id],
                )?;
            } else {
                let removed = conn.execute(
                    "DELETE FROM muted_cards WHERE agent=? AND card_id=?",
                    params![actor, id],
                )?;
                if removed > 0 {
                    // Unmute restores eligible routing; it is not an implicit
                    // follow. Retain missed peer updates even if the actor wrote
                    // the latest event, without manufacturing an own-event receipt.
                    // Only for cards already routed to the actor, that directly
                    // involve it, or that a rejoin would seed (active and
                    // relevant): a closed card never routed gets no receipt
                    // (#19 FU-B), and unmute agrees with join (FU-2).
                    let sql = format!("INSERT INTO deliveries(agent,card_id,pending_seq) SELECT ?1,c.id,(SELECT max(e.seq) FROM events e WHERE e.card_id=c.id AND e.actor<>?1 AND e.op<>'renew') FROM cards c WHERE c.id=?2 AND (EXISTS(SELECT 1 FROM deliveries d WHERE d.agent=?1 AND d.card_id=c.id) OR c.author=?1 OR c.assignee=?1 OR c.lease_owner=?1 OR EXISTS(SELECT 1 FROM participants p WHERE p.agent=?1 AND p.card_id=c.id) OR ({ACTIVE} AND {RELEVANT})) AND EXISTS(SELECT 1 FROM events e WHERE e.card_id=c.id AND e.actor<>?1 AND e.op<>'renew') ON CONFLICT(agent,card_id) DO UPDATE SET pending_seq=max(deliveries.pending_seq,excluded.pending_seq)");
                    conn.execute(&sql, params![actor, id, actor, actor, actor, actor])?;
                }
            }
            Ok(json!({"id":id,"muted":req.op=="mute","read_is_not_ack":true}))
        }
        "leave" | "heartbeat" => {
            check_fields(a, &[])?;
            conn.execute(
                "UPDATE agents SET enabled=?,last_seen_ms=? WHERE name=?",
                params![req.op != "leave", now, actor],
            )?;
            if req.op == "leave" {
                conn.execute("DELETE FROM session_waits WHERE agent=?", [actor])?;
                conn.execute("DELETE FROM wake_waits WHERE agent=?", [actor])?;
                conn.execute("UPDATE controllers SET state='stopped',updated_ms=?,reason='left' WHERE agent=?", params![now,actor])?;
                conn.execute("UPDATE listeners SET connected=0 WHERE agent=?", [actor])?;
            }
            Ok(json!({"agent":actor,"enabled":req.op!="leave"}))
        }
        "controller" => {
            check_fields(a, &["run_id", "state", "begin", "reason", "detail"])?;
            let run_id = string(a, "run_id")?;
            text(run_id, "run_id", 128, false)?;
            let state = string(a, "state")?;
            if !["waiting", "running", "failed", "stopped"].contains(&state) {
                return Err(Error::invalid(
                    "controller state: waiting|running|failed|stopped",
                ));
            }
            let reason = a.get("reason").map(|_| string(a, "reason")).transpose()?;
            if let Some(reason) = reason {
                text(reason, "reason", 160, true)?;
            }
            let detail = match a.get("detail") {
                Some(d) if d.is_object() && serde_json::to_string(d)?.len() <= 8000 => {
                    Some(serde_json::to_string(d)?)
                }
                Some(_) => {
                    return Err(Error::invalid(
                        "controller detail must be an object of at most 8000 bytes",
                    ))
                }
                None => None,
            };
            if boolean(a, "begin", false)? {
                if state != "waiting" {
                    return Err(Error::invalid("controller begins waiting"));
                }
                let occupied: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM controllers WHERE agent=?1 AND state IN ('waiting','running') AND updated_ms>?2) OR EXISTS(SELECT 1 FROM listeners WHERE agent=?1 AND connected=1 AND updated_ms>?3)",params![actor,now-120000,now-crate::attention::LISTENER_TTL_MS],|r|r.get(0))?;
                if occupied {
                    return Err(Error::new(
                        "controller_busy",
                        "a live controller already owns this identity; use a different --as name",
                    ));
                }
                conn.execute("INSERT INTO controllers(agent,run_id,state,updated_ms,reason) VALUES(?,?,?,?,NULL) ON CONFLICT(agent) DO UPDATE SET run_id=excluded.run_id,state=excluded.state,updated_ms=excluded.updated_ms,reason=NULL",params![actor,run_id,state,now])?;
                // A new run starts with its own diagnostics, never a predecessor's.
                conn.execute("DELETE FROM controller_details WHERE agent=?", [actor])?;
            } else {
                let updated = conn.execute("UPDATE controllers SET state=?,updated_ms=?,reason=? WHERE agent=? AND run_id=? AND state IN ('waiting','running') AND updated_ms>?",params![state,now,reason,actor,run_id,now-120000])?;
                if updated == 0 {
                    return Err(Error::new(
                        "controller_lost",
                        "controller lease expired, stopped, or belongs to another run",
                    ));
                }
            }
            if let Some(detail) = detail {
                conn.execute("INSERT INTO controller_details(agent,run_id,detail) VALUES(?,?,?) ON CONFLICT(agent) DO UPDATE SET run_id=excluded.run_id,detail=excluded.detail",params![actor,run_id,detail])?;
            }
            let mut result = json!({"run_id":run_id,"state":state,"updated_ms":now});
            // A keepalive's drive learns of a stop request here.
            if let Some(session) = req
                .session
                .as_deref()
                .filter(|s| crate::keepalive::is_session(Some(s)))
            {
                result["stop_requested"] =
                    json!(crate::keepalive::stop_requested(conn, actor, session)?);
            }
            Ok(result)
        }
        "keepalive_stop" => crate::keepalive::stop(conn, req, now),
        "keepalive_usage" => crate::keepalive::usage(conn, req, now),
        "keepalive_claim" => crate::keepalive::claim(conn, req, now),
        "terminal_turn" => crate::keepalive::mark(conn, req, now),
        "follow" | "unfollow" => {
            check_fields(a, &["id"])?;
            let id = integer(a, "id")?;
            let card = get_card(conn, id)?;
            if req.op == "follow" {
                conn.execute(
                    "INSERT OR IGNORE INTO participants(agent,card_id) VALUES(?,?)",
                    params![actor, id],
                )?;
                let other: bool = conn.query_row(
                    "SELECT actor<>? FROM events WHERE seq=?",
                    params![actor, card.last_seq],
                    |r| r.get(0),
                )?;
                if other {
                    conn.execute("INSERT INTO deliveries(agent,card_id,pending_seq) VALUES(?,?,?) ON CONFLICT(agent,card_id) DO UPDATE SET pending_seq=max(deliveries.pending_seq,excluded.pending_seq)",params![actor,id,card.last_seq])?;
                }
            } else {
                conn.execute(
                    "DELETE FROM participants WHERE agent=? AND card_id=?",
                    params![actor, id],
                )?;
            }
            Ok(
                json!({"id":id,"following":req.op=="follow","note":"Receipts are retained. Author, assignee, owner and current subscriptions still route updates; contributing follows again."}),
            )
        }
        "send" => {
            check_fields(
                a,
                &[
                    "to",
                    "body",
                    "title",
                    "ask",
                    "priority",
                    "refs",
                    "pending",
                    "respond_within_ms",
                ],
            )?;
            let respond_by = respond_by(a, now)?;
            if respond_by.is_some() && !boolean(a, "ask", false)? {
                return Err(Error::invalid("--respond-within needs --ask"));
            }
            let target = string(a, "to")?;
            if !valid_name(target) {
                return Err(Error::invalid("to must be a registered agent name"));
            }
            // Explicitly leave mail for an agent that has not joined yet: it
            // is registered disabled and receives the message when it joins.
            // Only on request, so a typo still fails with suggestions.
            if boolean(a, "pending", false)? && target != OWNER && owner_lookalike(target) {
                return Err(Error::new(
                    "reserved_owner",
                    "that name could be mistaken for the owner; choose another",
                ));
            }
            if boolean(a, "pending", false)? {
                conn.execute(
                    "INSERT OR IGNORE INTO agents(name,role,topics,enabled,joined_ms,last_seen_ms) VALUES(?,'worker',?,0,?,0)",
                    params![target, "[\"*\"]", now],
                )?;
            }
            let body = string(a, "body")?;
            text(body, "body", 8000, false)?;
            let title = a
                .get("title")
                .map(|_| string(a, "title").map(str::to_owned))
                .transpose()?
                .unwrap_or_else(|| clip(body.trim().lines().next().unwrap_or(body), 36));
            let refs = a.get("refs").map(tags).transpose()?.unwrap_or_default();
            // Keep current heads bounded; the immutable creation event holds full text.
            let summary = if body.len() <= 2000 {
                body.to_owned()
            } else {
                format!(
                    "{}\n[Full message: fray thread ID --bodies]",
                    clip(body, 400)
                )
            };
            let mut result = create_card(
                conn,
                actor,
                &json!({
                    "kind":if boolean(a,"ask",false)? {"question"} else {"note"},
                    "topic":format!("@{target}"),"title":title,"summary":summary,
                    "assignee":target,"priority":bounded(a,"priority",2,0,3)?,"tags":refs
                }),
                match respond_by {
                    Some(by) => json!({"body":body,"respond_by_ms":by}),
                    None => json!({"body":body}),
                },
                now,
            )?;
            if target != OWNER && target != actor {
                result["reachability"] = json!(reachability(conn, target, now)?.as_str());
            }
            if let Some(notice) = absence_notice(conn, target, actor, now)? {
                result["notice"] = json!(notice);
            }
            Ok(result)
        }
        "post" => {
            check_fields(
                a,
                &[
                    "kind", "topic", "title", "summary", "status", "priority", "pinned", "tags",
                    "assignee",
                ],
            )?;
            create_card(conn, actor, a, Value::Null, now)
        }
        "peer_present" => crate::presence::presented(conn, req),
        "review_request" => {
            check_fields(
                a,
                &["to", "title", "body", "baseline", "candidate", "mote_ref"],
            )?;
            let to = string(a, "to")?;
            if to == actor {
                return Err(Error::new(
                    "self_review",
                    "address a review request to a peer",
                ));
            }
            let baseline = crate::review::version(string(a, "baseline")?)?;
            let candidate = crate::review::version(string(a, "candidate")?)?;
            let body = string(a, "body")?;
            text(body, "body", 8000, false)?;
            let mote_ref = a
                .get("mote_ref")
                .map(|_| string(a, "mote_ref"))
                .transpose()?;
            if let Some(reference) = mote_ref {
                text(reference, "mote_ref", 200, false)?;
                if !reference.starts_with("mote:") || reference.len() == 5 {
                    return Err(Error::invalid(
                        "mote_ref must be a mote: candidate/issue reference",
                    ));
                }
            }
            let review = json!({"baseline":baseline,"candidate":candidate,"subject_rev":1,"mote_ref":mote_ref,"advisory":true});
            let summary = if body.len() <= 2000 {
                body.to_owned()
            } else {
                format!(
                    "{}\n[Full review: fray thread ID --bodies]",
                    clip(body, 400)
                )
            };
            let mut result = create_card(
                conn,
                actor,
                &json!({"kind":"question","topic":format!("@{to}"),"title":string(a,"title")?,"summary":summary,"assignee":to,"tags":mote_ref.into_iter().collect::<Vec<_>>()}),
                json!({"body":body,"review":review}),
                now,
            )?;
            let id = result["card"]["id"].as_i64().unwrap();
            conn.execute("INSERT INTO review_subjects(card_id,baseline,candidate,subject_rev,mote_ref) VALUES(?,?,?,1,?)",params![id,baseline,candidate,mote_ref])?;
            result["review"] = review;
            Ok(result)
        }
        "review_subject" => {
            check_fields(a, &["id", "expect", "at"])?;
            let c = get_card(conn, integer(a, "id")?)?;
            if crate::mote_workflow::review_binding(conn, c.id)?.is_some() {
                return Err(Error::invalid("Mote candidate subjects are immutable; use review successor after Mote supersession"));
            }
            if c.author != actor {
                return Err(Error::new(
                    "not_author",
                    "only the review requester may move its candidate",
                ));
            }
            if c.terminal() {
                return Err(Error::new(
                    "closed",
                    "reopen or create a new review before moving its candidate",
                ));
            }
            let previous = crate::review::subject(conn, c.id)?
                .ok_or_else(|| Error::new("not_review", "card has no review subject"))?;
            let at = crate::review::version(string(a, "at")?)?;
            if previous["subject_rev"] != integer(a, "expect")? {
                return Err(Error::new(
                    "conflict",
                    "review subject changed; read before retrying",
                ));
            }
            if previous["candidate"] == at {
                return Err(Error::invalid(
                    "candidate is unchanged; no new review round was created",
                ));
            }
            conn.execute(
                "UPDATE review_subjects SET candidate=?,subject_rev=subject_rev+1 WHERE card_id=?",
                params![at, c.id],
            )?;
            conn.execute(
                "UPDATE cards SET rev=rev+1,updated_ms=? WHERE id=?",
                params![now, c.id],
            )?;
            let review = crate::review::context(conn, c.id, false)?;
            let mut result = emit(
                conn,
                actor,
                "review_subject",
                c.id,
                json!({"body":format!("Review candidate changed from {} to {at}; previous verdicts are stale.",previous["candidate"]),"previous":previous["candidate"],"review":review}),
                now,
                true,
            )?;
            result["review"] = review;
            Ok(result)
        }
        "patch" => {
            check_fields(
                a,
                &[
                    "id",
                    "expect",
                    "fence",
                    "kind",
                    "topic",
                    "title",
                    "summary",
                    "status",
                    "priority",
                    "pinned",
                    "tags",
                    "assignee",
                    "over_objection",
                ],
            )?;
            let mut c = get_card(conn, integer(a, "id")?)?;
            if ["title", "summary", "kind"]
                .iter()
                .any(|key| a.get(key).is_some())
                && crate::review::subject(conn, c.id)?.is_some()
            {
                return Err(Error::new("review_scope_frozen","review request title, summary and kind are immutable; create a new request for a different scope"));
            }
            let was_resolved = c.status == "resolved";
            if c.rev != integer(a, "expect")? {
                return Err(Error::new(
                    "conflict",
                    format!("card {} is revision {}; read before retrying", c.id, c.rev),
                ));
            }
            // Authority follows the author, so only the owner may change an
            // owner card; others can reply to it, never rewrite it.
            if owner_controlled(&c) && actor != OWNER {
                return Err(Error::new(
                    "reserved_owner",
                    "owner cards and owner-decided requests can only be changed by the owner; reply instead",
                ));
            }
            let closing_objection = matches!(
                a["status"].as_str(),
                Some("resolved" | "superseded" | "withdrawn")
            ) && !c.terminal()
                && objection_origin(conn, c.id)?.is_some();
            if closing_objection
                && objection_origin(conn, c.id)?.is_some_and(|(objector, _)| objector != actor)
                && actor != OWNER
            {
                return Err(Error::new("objection_authority", "only the objector or owner may close this objection; reply with evidence or ask them to resolve it"));
            }
            if c.lease_owner.is_some() && !closing_objection {
                check_lease(&c, actor, integer(a, "fence")?, now, false)?;
            }
            if a.as_object().map_or(0, |x| {
                x.keys()
                    .filter(|k| !["id", "expect", "fence", "over_objection"].contains(&k.as_str()))
                    .count()
            }) == 0
            {
                return Err(Error::invalid("patch has no fields"));
            }
            apply_fields(&mut c, a)?;
            validate_card(&c)?;
            reserve_authority_tags(actor, &c.tags)?;
            assignee_known(conn, &c.assignee)?;
            // Resolving past an open objection must be deliberate and visible.
            // Superseding or withdrawing is not a claim that the work is right.
            let mut detail = Value::Null;
            if c.status == "resolved" && !was_resolved {
                let open = open_objections(conn, c.id)?;
                if !open.is_empty() {
                    let Some(reason) = a.get("over_objection") else {
                        return Err(Error::new(
                            "open_objections",
                            format!(
                                "card {} has open objections {open:?}; resolve them first, or patch with --over-objection REASON",
                                c.id
                            ),
                        ));
                    };
                    let reason = reason
                        .as_str()
                        .ok_or_else(|| Error::invalid("over_objection must be a string"))?;
                    text(reason, "over_objection", 2000, false)?;
                    detail = json!({"over_objection":reason,"open_objections":open});
                }
            }
            if c.terminal() {
                c.lease_owner = None;
                c.lease_until_ms = 0;
                c.fence += 1;
            }
            conn.execute("UPDATE cards SET rev=rev+1,kind=?,topic=?,title=?,summary=?,status=?,priority=?,pinned=?,tags=?,assignee=?,lease_owner=?,lease_until_ms=?,fence=?,updated_ms=? WHERE id=?",params![c.kind,c.topic,c.title,c.summary,c.status,c.priority,c.pinned,serde_json::to_string(&c.tags)?,c.assignee,c.lease_owner,c.lease_until_ms,c.fence,now,c.id])?;
            let override_ids = detail["open_objections"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            if closing_objection {
                detail["objection_resolved_by"] = json!(actor);
            }
            let result = emit(conn, actor, "patch", c.id, detail, now, true)?;
            let seq = integer(&result, "event_seq")?;
            if closing_objection {
                force_delivery(conn, &c.author, c.id, seq, now)?;
                objection_notice(conn, actor, &c.author, c.id, "Objection resolved", &format!("{} closed objection #{} as {}. Read `fray thread {} --bodies` for the evidence.", actor, c.id, c.status, c.id), now)?;
            }
            if !override_ids.is_empty() {
                force_delivery(conn, OWNER, c.id, seq, now)?;
                let text = format!(
                    "{actor} resolved #{} over open objections {:?}. Reason: {}",
                    c.id,
                    override_ids,
                    a["over_objection"].as_str().unwrap_or("")
                );
                let mut recipients = std::collections::BTreeSet::from([OWNER.to_owned()]);
                for child in override_ids.iter().filter_map(Value::as_i64) {
                    if let Some((objector, _)) = objection_origin(conn, child)? {
                        force_delivery(conn, &objector, c.id, seq, now)?;
                        recipients.insert(objector);
                    }
                }
                for recipient in recipients {
                    objection_notice(
                        conn,
                        actor,
                        &recipient,
                        c.id,
                        "Resolved over objection",
                        &text,
                        now,
                    )?;
                }
            }
            Ok(result)
        }
        "annotate" => {
            check_fields(
                a,
                &[
                    "id",
                    "kind",
                    "body",
                    "refs",
                    "ack_batch",
                    "review_verdict",
                    "respond_within_ms",
                ],
            )?;
            let c = get_card(conn, integer(a, "id")?)?;
            // Only the requester moves its ask's deadline, and only on an ask.
            let new_deadline = respond_by(a, now)?;
            let asked_at_creation = || -> Result<bool> {
                Ok(conn
                    .query_row(
                        "SELECT json_extract(payload,'$.card.kind')='question' FROM events WHERE card_id=? ORDER BY seq LIMIT 1",
                        [c.id],
                        |r| r.get(0),
                    )
                    .optional()?
                    .unwrap_or(false))
            };
            // Ask-ness from the creation event, as for overdue: a kind patch
            // cannot stop the asker moving its deadline.
            if new_deadline.is_some() && (c.author != actor || !asked_at_creation()?) {
                return Err(Error::invalid(
                    "only the author of an ask can set its --respond-within",
                ));
            }
            let body = string(a, "body")?;
            text(body, "body", 8000, false)?;
            let verdict = a
                .get("review_verdict")
                .map(|v| crate::review::prepare_verdict(conn, c.id, actor, v))
                .transpose()?;
            let kind = a
                .get("kind")
                .map(|_| string(a, "kind"))
                .transpose()?
                .unwrap_or("note");
            if !["note", "evidence", "objection", "question", "answer"].contains(&kind) {
                return Err(Error::invalid(
                    "annotation kind: note|evidence|objection|question|answer",
                ));
            }
            if let Some(v) = &verdict {
                let expected_kind = match v["verdict"].as_str().unwrap() {
                    "object" => "objection",
                    "blocked" => "question",
                    _ => "evidence",
                };
                if kind != expected_kind {
                    return Err(Error::invalid(
                        "review verdict annotation kind does not match its verdict",
                    ));
                }
            }
            // Only an explicitly named receipt is handled. This shares the reply's
            // transaction: invalid receipts or a failed reply change neither.
            let acknowledged = a
                .get("ack_batch")
                .map(|_| {
                    let batch = string(a, "ack_batch")?;
                    mutate(
                        conn,
                        &Request::new("ack", actor, json!({"batch":batch,"ids":[c.id]})),
                        now,
                    )
                })
                .transpose()?;
            let unseen = unseen_peer_updates(conn, req, c.id, now)?;
            let refs = a.get("refs").map(tags).transpose()?.unwrap_or_default();
            reserve_authority_tags(actor, &refs)?;
            let mut merged = c.tags.clone();
            merged.extend(refs.iter().cloned());
            merged.sort();
            merged.dedup();
            let merged = tags(&json!(merged))?;
            // A reply never changes an owner card: its refs stay on the reply.
            if merged != c.tags && (!owner_controlled(&c) || actor == OWNER) {
                conn.execute(
                    "UPDATE cards SET tags=?,rev=rev+1,updated_ms=? WHERE id=?",
                    params![serde_json::to_string(&merged)?, now, c.id],
                )?;
            }
            // An actionable annotation becomes a durable question, not a buried reply.
            let mut routed_absent = None;
            let mut passed_over: Option<String> = None;
            let follow_up = if matches!(kind, "question" | "objection") {
                // Route to the conversation partner. The author's question goes
                // to whoever is working the card (lease owner, then assignee);
                // anyone else's goes to a live claim holder, else to the
                // author, who asked, not to a merely assigned party. An
                // assignee objecting to its own card reaches the author.
                let lease_owner = if c.lease_until_ms > now {
                    c.lease_owner.clone()
                } else {
                    None
                };
                let parties = [lease_owner.clone(), c.assignee.clone()];
                let order = if c.assignee.as_deref() == Some(OWNER) && actor != OWNER {
                    // A request waiting on the owner stays with the owner: an
                    // objection to it is for the one deciding.
                    vec![Some(OWNER.to_owned()), lease_owner, Some(c.author.clone())]
                } else if actor == c.author || parties.iter().flatten().any(|who| who == actor) {
                    vec![lease_owner, c.assignee.clone(), Some(c.author.clone())]
                } else {
                    vec![lease_owner, Some(c.author.clone()), c.assignee.clone()]
                };
                let candidates: Vec<String> = order
                    .into_iter()
                    .flatten()
                    .filter(|who| who != actor)
                    .collect();
                // Prefer a party who can be woken, then one who is present;
                // if none is either, keep the first and say so, rather than
                // leave the question with nobody.
                let mut reach = Vec::new();
                for who in &candidates {
                    reach.push(if who == OWNER {
                        Reach::Wakeable
                    } else {
                        reachability(conn, who, now)?
                    });
                }
                let pick = |want: Reach| {
                    candidates
                        .iter()
                        .zip(&reach)
                        .find(|(_, r)| **r == want)
                        .map(|(who, _)| who.clone())
                };
                let mut target = pick(Reach::Wakeable);
                if target.is_none() {
                    target = pick(Reach::Present);
                    routed_absent.clone_from(&target);
                }
                // Passing over the party the order names first is said, not
                // silent: they may be the one actively working.
                if let (Some(first), Some(chosen)) = (candidates.first(), &target) {
                    if first != chosen {
                        let state = reach[0].as_str();
                        passed_over =
                            Some(format!(
                            "{first} ({state}, nothing armed) was passed over for {chosen}, who {}",
                            if routed_absent.is_some() { "is present" } else { "can be woken" }
                        ));
                    }
                }
                let target = match target {
                    Some(who) => who,
                    None => {
                        let who = candidates
                            .first()
                            .cloned()
                            .unwrap_or_else(|| actor.to_string());
                        routed_absent = Some(who.clone());
                        who
                    }
                };
                // Name the concern, not the parent: nested parent titles are unreadable.
                let first_line = body
                    .lines()
                    .map(str::trim)
                    .find(|line| !line.is_empty())
                    .unwrap_or("");
                let title = format!(
                    "{} on #{}: {}",
                    if kind == "objection" {
                        "Objection"
                    } else {
                        "Question"
                    },
                    c.id,
                    clip(first_line, 60)
                );
                let summary=format!("{}\nFull context: annotation on card #{}. Resolve this question explicitly after addressing it.",clip(body,400),c.id);
                let priority = if kind == "objection" {
                    c.priority.min(1)
                } else {
                    c.priority
                };
                let mut follow_up_tags = refs.clone();
                follow_up_tags.push(format!("parent:{}", c.id));
                follow_up_tags.sort();
                follow_up_tags.dedup();
                let follow_up_tags = tags(&json!(follow_up_tags))?;
                conn.execute("INSERT INTO cards(kind,topic,title,summary,status,priority,tags,author,assignee,created_ms,updated_ms) VALUES('question',?,?,?,'open',?,?,?,?,?,?)",params![c.topic,title,summary,priority,serde_json::to_string(&follow_up_tags)?,actor,target,now,now])?;
                Some(conn.last_insert_rowid())
            } else {
                None
            };
            let mut result = emit(
                conn,
                actor,
                "annotate",
                c.id,
                {
                    let mut d = json!({"kind":kind,"body":body,"follow_up_id":follow_up,"refs":refs,"review":verdict});
                    if let Some(by) = new_deadline {
                        d["respond_by_ms"] = json!(by);
                    }
                    d
                },
                now,
                true,
            )?;
            if let Some(verdict) = &verdict {
                crate::review::record_verdict(
                    conn,
                    c.id,
                    actor,
                    integer(&result, "event_seq")?,
                    verdict,
                )?;
                result["review"] = crate::review::context(conn, c.id, true)?;
            }
            if let Some(id) = follow_up {
                let child = emit(
                    conn,
                    actor,
                    "post",
                    id,
                    json!({"parent_card":c.id,"annotation_seq":result["event_seq"]}),
                    now,
                    true,
                )?;
                result["follow_up"] = child["card"].clone();
                result["cursor"] = child["event_seq"].clone();
                let mut notes = Vec::new();
                if let Some(note) = passed_over {
                    notes.push(format!("{note}."));
                }
                if let Some(who) = routed_absent {
                    if let Some(notice) = absence_notice(conn, &who, actor, now)? {
                        notes.push(notice);
                    }
                }
                if !notes.is_empty() {
                    result["notice"] = json!(format!(
                        "{} Reroute with: fray patch {id} --expect 1 --assignee NAME",
                        notes.join(" ")
                    ));
                }
            }
            if let Some(ack) = acknowledged {
                result["ack_batch"] = ack["batch"].clone();
                result["acknowledged"] = ack["acknowledged"].clone();
            }
            if unseen["count"].as_i64().unwrap_or(0) > 0 {
                result["reply_warning"] = json!(format!(
                    "Reply posted, but {} peer update(s) through @{} were not in your acknowledged history or this session's inbox/thread receipts. Read fray thread {} --unread before continuing; background notifications do not count as a thread read.",
                    unseen["count"], unseen["through_seq"], c.id
                ));
                result["unseen_updates"] = unseen;
            }
            Ok(result)
        }
        "claim" | "renew" | "release" => {
            check_fields(a, &["id", "fence", "ttl"])?;
            let c = get_card(conn, integer(a, "id")?)?;
            if req.op=="claim" && conn.query_row("SELECT EXISTS(SELECT 1 FROM dispatch_offers WHERE id=?1) OR EXISTS(SELECT 1 FROM dispatch_handoffs WHERE id=?1)",[c.id],|r|r.get::<_,bool>(0))? {
                return Err(Error::invalid("use fray accept for Mote-backed offers/handoff packets; card leases do not claim Mote work"));
            }
            // Leasing an owner card would edit it and redirect objections to
            // the leaseholder instead of the owner.
            if owner_controlled(&c) && actor != OWNER && req.op != "release" {
                return Err(Error::new(
                    "reserved_owner",
                    "owner cards cannot be claimed; reply to it instead",
                ));
            }
            let ttl = bounded(a, "ttl", 900, 1, 86400)?;
            match req.op.as_str() {
                "claim" => {
                    if c.terminal() {
                        return Err(Error::new("closed", "cannot claim a terminal card"));
                    }
                    if c.lease_owner.is_some() && c.lease_until_ms > now {
                        return Err(Error::new(
                            "claimed",
                            format!(
                                "card {} claimed by {:?} until {}; use renew for your live claim",
                                c.id, c.lease_owner, c.lease_until_ms
                            ),
                        ));
                    }
                    conn.execute("UPDATE cards SET lease_owner=?,lease_until_ms=?,fence=fence+1,rev=rev+1,status=CASE WHEN status='open' AND kind='task' THEN 'active' ELSE status END WHERE id=?",params![actor,now+ttl*1000,c.id])?;
                }
                "renew" => {
                    check_lease(&c, actor, integer(a, "fence")?, now, false)?;
                    // Heartbeats extend a lease without invalidating content revisions.
                    conn.execute(
                        "UPDATE cards SET lease_until_ms=? WHERE id=?",
                        params![now + ttl * 1000, c.id],
                    )?;
                }
                _ => {
                    check_lease(&c, actor, integer(a, "fence")?, now, true)?;
                    conn.execute("UPDATE cards SET lease_owner=NULL,lease_until_ms=0,fence=fence+1,rev=rev+1 WHERE id=?",[c.id])?;
                }
            }
            emit(
                conn,
                actor,
                &req.op,
                c.id,
                Value::Null,
                now,
                req.op != "renew",
            )
        }
        "ack" => {
            let last = if boolean(a, "last", false)? {
                check_fields(a, &["last", "ids"])?;
                Some(last_batch(conn, actor, req.session.as_deref(), now)?)
            } else {
                None
            };
            if let Some(batch) = last
                .as_ref()
                .map(|b| json!(b))
                .or_else(|| a.get("batch").cloned())
            {
                if last.is_none() {
                    check_fields(a, &["batch", "ids"])?;
                }
                let batch = batch
                    .as_str()
                    .ok_or_else(|| Error::invalid("batch must be a string"))?;
                let items = batch_items(conn, actor, batch, now)?;
                let chosen: Vec<(i64, i64)> = match a.get("ids") {
                    None => items,
                    Some(ids) => {
                        let ids = ids
                            .as_array()
                            .filter(|ids| !ids.is_empty() && ids.len() <= 100)
                            .ok_or_else(|| Error::invalid("ids must be a nonempty array"))?;
                        let mut chosen = Vec::new();
                        for id in ids {
                            let id = id
                                .as_i64()
                                .ok_or_else(|| Error::invalid("ids must be integers"))?;
                            let item =
                                items.iter().find(|(card, _)| *card == id).ok_or_else(|| {
                                    Error::new(
                                        "batch_mismatch",
                                        format!("card #{id} is not in batch {batch}"),
                                    )
                                })?;
                            chosen.push(*item);
                        }
                        chosen
                    }
                };
                let mut acknowledged = Vec::new();
                for (id, through) in chosen {
                    acknowledged.push(mutate(
                        conn,
                        &Request::new("ack", actor, json!({"id":id,"through":through})),
                        now,
                    )?);
                }
                return Ok(json!({"batch":batch,"acknowledged":acknowledged}));
            }
            if let Some(receipts) = a.get("receipts") {
                check_fields(a, &["receipts"])?;
                let receipts = receipts
                    .as_array()
                    .ok_or_else(|| Error::invalid("receipts must be an array"))?;
                if receipts.len() > 100 {
                    return Err(Error::invalid("too many receipts"));
                }
                let store_id: String =
                    conn.query_row("SELECT value FROM meta WHERE key='store_id'", [], |r| {
                        r.get(0)
                    })?;
                let mut acknowledged = Vec::new();
                for receipt in receipts {
                    check_fields(receipt, &["store_id", "agent", "id", "through_seq"])?;
                    if string(receipt, "store_id")? != store_id
                        || string(receipt, "agent")? != actor
                    {
                        return Err(Error::new(
                            "receipt_mismatch",
                            "receipt belongs to another store or agent",
                        ));
                    }
                    acknowledged.push(mutate(conn,&Request::new("ack",actor,json!({"id":integer(receipt,"id")?,"through":integer(receipt,"through_seq")?})),now)?);
                }
                return Ok(json!({"acknowledged":acknowledged}));
            }
            check_fields(a, &["id", "through"])?;
            let id = integer(a, "id")?;
            let through = integer(a, "through")?;
            let pending: Option<i64> = conn
                .query_row(
                    "SELECT pending_seq FROM deliveries WHERE agent=? AND card_id=?",
                    params![actor, id],
                    |r| r.get(0),
                )
                .optional()?;
            let pending = pending
                .ok_or_else(|| Error::new("not_found", "no delivery for this agent/card"))?;
            if through < 0 || through > pending {
                return Err(Error::invalid(
                    "through must not exceed the card's pending delivery sequence",
                ));
            }
            conn.execute(
                "UPDATE deliveries SET ack_seq=max(ack_seq,?) WHERE agent=? AND card_id=?",
                params![through, actor, id],
            )?;
            let acked: i64 = conn.query_row(
                "SELECT ack_seq FROM deliveries WHERE agent=? AND card_id=?",
                params![actor, id],
                |r| r.get(0),
            )?;
            Ok(json!({"id":id,"ack_seq":acked,"pending_seq":pending,"still_pending":acked<pending}))
        }
        "owner_decide" => {
            // A standing owner decision (e.g. the charter), pinned for everyone.
            check_fields(a, &["title", "summary", "pin"])?;
            create_card(
                conn,
                OWNER,
                &json!({
                    "kind":"decision","topic":"*","title":string(a,"title")?,
                    "summary":string(a,"summary")?,"pinned":boolean(a,"pin",true)?,
                    "tags":["authority:owner"]
                }),
                Value::Null,
                now,
            )
        }
        "owner_answer" => {
            // The owner answers a request from the owner queue. The asker is
            // routed the answer like any reply; approve/decline also close it.
            check_fields(a, &["id", "verdict", "body", "expect"])?;
            let id = integer(a, "id")?;
            let verdict = string(a, "verdict")?;
            if !["approve", "decline", "answer"].contains(&verdict) {
                return Err(Error::invalid("verdict: approve|decline|answer"));
            }
            let body = string(a, "body")?;
            // A decision binds to exactly the version the owner saw: if the
            // request changed since it was displayed, nothing is recorded.
            let shown = get_card(conn, id)?;
            if verdict != "answer" && shown.terminal() {
                // A closed request cannot be decided (or decided twice).
                return Err(Error::new(
                    "already_closed",
                    format!(
                        "request #{id} is already {}; nothing was recorded",
                        shown.status
                    ),
                ));
            }
            if verdict != "answer" && integer(a, "expect")? != shown.rev {
                return Err(Error::new(
                    "conflict",
                    format!(
                        "request #{id} changed since it was shown (now revision {}); review it again",
                        shown.rev
                    ),
                ));
            }
            let what = format!("revision {}: {:?}", shown.rev, shown.title);
            let text = match verdict {
                "approve" => format!("APPROVED by the owner ({what}). {body}"),
                "decline" => format!("DECLINED by the owner ({what}). {body}"),
                // A reply records which version it answers, so a later edit
                // of the request cannot borrow it.
                _ => format!("{body}\n(Owner reply to {what}.)"),
            };
            let mut result = mutate(
                conn,
                &Request::new(
                    "annotate",
                    OWNER,
                    json!({"id":id,"kind":"answer","body":text.trim_end()}),
                ),
                now,
            )?;
            let card = get_card(conn, id)?;
            if verdict != "answer" && !card.terminal() {
                // An agent's lease on the request never blocks the owner's
                // decision: clear it (and advance the fence) first.
                if card.lease_owner.is_some() {
                    conn.execute(
                        "UPDATE cards SET lease_owner=NULL,lease_until_ms=0,fence=fence+1 WHERE id=?",
                        [id],
                    )?;
                }
                let card = get_card(conn, id)?;
                result = mutate(
                    conn,
                    &Request::new(
                        "patch",
                        OWNER,
                        json!({"id":id,"expect":card.rev,"status":if verdict == "approve" {"resolved"} else {"withdrawn"},"tags":decided_tags(&card),"over_objection":"The owner decided this request."}),
                    ),
                    now,
                )?;
            }
            result["verdict"] = json!(verdict);
            Ok(result)
        }
        "lane_take" => {
            // Declare the paths you are working on. Advisory, never a lock:
            // overlapping another agent's held lane is refused unless you
            // queue behind it, which only orders notifications.
            check_fields(a, &["paths", "purpose", "card", "queue"])?;
            let paths: Vec<String> = a["paths"]
                .as_array()
                .filter(|p| !p.is_empty() && p.len() <= 32)
                .ok_or_else(|| Error::invalid("paths must be 1..32 strings"))?
                .iter()
                .map(|p| {
                    p.as_str()
                        .ok_or_else(|| Error::invalid("paths must be strings"))
                        .and_then(|p| lane_path(p).map(|_| p.to_owned()))
                })
                .collect::<Result<_>>()?;
            let purpose = string(a, "purpose")?;
            text(purpose, "purpose", 200, false)?;
            let card = a.get("card").map(|_| integer(a, "card")).transpose()?;
            if let Some(card) = card {
                get_card(conn, card)?;
            }
            let queue = boolean(a, "queue", false)?;
            let lanes = live_lanes(conn, now)?;
            let conflicts = lane_conflicts(&lanes, actor, &paths, None, now);
            if !conflicts.is_empty() && !queue {
                let holders: Vec<String> = conflicts
                    .iter()
                    .map(|l| {
                        format!(
                            "lane {} ({}{}) by {}: {} [{}]",
                            l["id"],
                            l["state"].as_str().unwrap_or(""),
                            if l["stale"] == true { ", stale" } else { "" },
                            l["agent"].as_str().unwrap_or(""),
                            l["paths"]
                                .as_array()
                                .into_iter()
                                .flatten()
                                .filter_map(Value::as_str)
                                .collect::<Vec<_>>()
                                .join(", "),
                            l["purpose"].as_str().unwrap_or("")
                        )
                    })
                    .collect();
                // If a queue is waiting on the actor's own held lane, the way
                // forward is to finish and release it.
                let mine: Vec<String> = lanes
                    .iter()
                    .filter(|l| l["agent"] == actor && l["state"] == "held")
                    .filter(|l| {
                        conflicts.iter().any(|c| {
                            c["state"] == "queued"
                                && lane_values(c)
                                    .iter()
                                    .any(|q| lane_values(l).iter().any(|m| paths_overlap(q, m)))
                        })
                    })
                    .map(|l| l["id"].to_string())
                    .collect();
                let release = if mine.is_empty() {
                    String::new()
                } else {
                    format!(
                        " They are queued behind your held lane {}: finish and release it to let the queue proceed.",
                        mine.join(", ")
                    )
                };
                return Err(Error::new(
                    "lane_busy",
                    format!(
                        "overlaps {}.{release} Ask them, or rerun the same `fray lane take` with --queue to be told when it frees",
                        holders.join("; ")
                    ),
                ));
            }
            let state = if conflicts.is_empty() {
                "held"
            } else {
                "queued"
            };
            conn.execute(
                "INSERT INTO lanes(agent,paths,purpose,card_id,state,created_ms) VALUES(?,?,?,?,?,?)",
                params![actor, serde_json::to_string(&paths)?, purpose, card, state, now],
            )?;
            // Taking paths inside a region someone queued for is allowed for a
            // while (it may be how the holder finishes); they are told.
            if state == "held" {
                for l in lanes.iter().filter(|l| {
                    l["state"] == "queued"
                        && l["agent"] != actor
                        && lane_values(l)
                            .iter()
                            .any(|q| paths.iter().any(|p| paths_overlap(q, p)))
                }) {
                    // Once per queued lane, not once per take.
                    let told: bool = conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM cards c, json_each(c.tags) t WHERE c.assignee=? AND t.value=?)",
                        params![l["agent"].as_str().unwrap_or(""), format!("lane-inside:{}", l["id"])],
                        |r| r.get(0),
                    )?;
                    if told {
                        continue;
                    }
                    lane_notice_tagged(
                        conn,
                        actor,
                        l["agent"].as_str().unwrap_or(""),
                        &format!("lane-inside:{}", l["id"]),
                        "Work continues inside your queued lane",
                        &format!(
                            "{actor} took {} inside lane {} you are queued for, while finishing its held work. After {} min of queueing, new takes wait behind you.",
                            paths.join(", "),
                            l["id"],
                            LANE_PATIENCE_MS / 60_000
                        ),
                        now,
                    )?;
                }
            }
            let id = conn.last_insert_rowid();
            let mut result = json!({"lane":{"id":id,"agent":actor,"paths":paths,"purpose":purpose,"card":card,"state":state},"waiting_on":conflicts});
            if paths
                .iter()
                .any(|p| lane_path(p).is_ok_and(|p| p.is_empty()))
            {
                result["warning"] = json!(
                    "this lane covers the whole repository; release it and take narrower paths unless that is intended"
                );
            }
            Ok(result)
        }
        "lane_release" => {
            // Release (or hand over) a lane. The next queued lane that no
            // longer conflicts is promoted, and its agent is told.
            check_fields(a, &["id", "to", "reason"])?;
            let id = integer(a, "id")?;
            let lanes = live_lanes(conn, now)?;
            let lane = lanes
                .iter()
                .find(|l| l["id"] == id)
                .ok_or_else(|| Error::new("not_found", format!("no live lane {id}")))?
                .clone();
            let holder = lane["agent"].as_str().unwrap_or("").to_owned();
            if holder != actor && lane["stale"] != true {
                return Err(Error::new(
                    "lane_not_yours",
                    format!("lane {id} is held by {holder}, who is still present; ask them to release it"),
                ));
            }
            let to = a.get("to").map(|_| string(a, "to")).transpose()?;
            if to.is_some() && holder != actor {
                return Err(Error::new(
                    "lane_not_yours",
                    format!("only {holder} can hand over lane {id}; release it, then take (or --queue) it yourself so earlier queuers keep their turn"),
                ));
            }
            if to == Some(holder.as_str()) {
                return Err(Error::invalid(format!("lane {id} is already {holder}'s")));
            }
            if let (Some(to), true) = (to, lane["state"] == "held") {
                // Handing over a held lane must not leave two agents holding
                // overlapping paths (e.g. the giver's narrower lane inside it).
                let others: Vec<Value> = lanes.iter().filter(|l| l["id"] != id).cloned().collect();
                let paths: Vec<String> = serde_json::from_value(lane["paths"].clone())?;
                let clash: Vec<String> = lane_conflicts(&others, to, &paths, None, now)
                    .iter()
                    .filter(|l| l["state"] == "held")
                    .map(|l| format!("lane {} by {}", l["id"], l["agent"].as_str().unwrap_or("")))
                    .collect();
                if !clash.is_empty() {
                    return Err(Error::new(
                        "lane_busy",
                        format!(
                            "handing over lane {id} would overlap {}; release or hand over those first. Nothing was changed",
                            clash.join(", ")
                        ),
                    ));
                }
            }
            if let Some(to) = to {
                let joined: bool = conn.query_row(
                    "SELECT EXISTS(SELECT 1 FROM agents WHERE name=? AND enabled=1)",
                    [to],
                    |r| r.get(0),
                )?;
                if !joined {
                    return Err(Error::new(
                        "unknown_agent",
                        format!(
                            "{to} has not joined, so it cannot hold a lane; nothing was changed"
                        ),
                    ));
                }
            }
            let reason = match (&to, holder == actor) {
                (Some(to), _) => format!("handed to {to}"),
                (None, true) => a
                    .get("reason")
                    .map(|_| string(a, "reason"))
                    .transpose()?
                    .unwrap_or("released")
                    .to_owned(),
                (None, false) => format!("stale; released by {actor}"),
            };
            text(&reason, "reason", 200, false)?;
            if let Some(to) = to {
                // A handover moves the lane as it is, in place: a queued lane
                // stays queued and keeps its turn.
                conn.execute("UPDATE lanes SET agent=? WHERE id=?", params![to, id])?;
            } else {
                conn.execute(
                    "UPDATE lanes SET released_ms=?,released_reason=? WHERE id=?",
                    params![now, reason, id],
                )?;
            }
            let paths: Vec<String> = serde_json::from_value(lane["paths"].clone())?;
            let mut handed = Value::Null;
            if holder != actor {
                // The previous holder hears that its lane was released.
                lane_notice(
                    conn,
                    actor,
                    &holder,
                    "Your stale lane was released",
                    &format!(
                        "{actor} released your lane {id} ({}) because you were not present.",
                        paths.join(", ")
                    ),
                    now,
                )?;
            }
            if let Some(to) = to {
                handed = json!(id);
                lane_notice(
                    conn,
                    actor,
                    to,
                    "Lane handed to you",
                    &format!(
                        "{actor} handed you lane {id}: {} ({}).",
                        paths.join(", "),
                        lane["purpose"].as_str().unwrap_or("")
                    ),
                    now,
                )?;
            }
            // Promote queued lanes, oldest first, once nothing held blocks them.
            let mut promoted = Vec::new();
            for queued in live_lanes(conn, now)?
                .iter()
                .filter(|l| l["state"] == "queued")
            {
                let agent = queued["agent"].as_str().unwrap_or("").to_owned();
                let qpaths: Vec<String> = serde_json::from_value(queued["paths"].clone())?;
                if lane_conflicts(
                    &live_lanes(conn, now)?,
                    &agent,
                    &qpaths,
                    queued["id"].as_i64(),
                    now,
                )
                .is_empty()
                {
                    conn.execute(
                        "UPDATE lanes SET state='held' WHERE id=?",
                        [queued["id"].as_i64()],
                    )?;
                    lane_notice(
                        conn,
                        actor,
                        &agent,
                        "Your queued lane is free",
                        &format!(
                            "Lane {} ({}) is now held by you.",
                            queued["id"],
                            qpaths.join(", ")
                        ),
                        now,
                    )?;
                    promoted.push(queued["id"].clone());
                }
            }
            Ok(json!({"released":id,"reason":reason,"handed_to_lane":handed,"promoted":promoted}))
        }
        "mote_bind" => {
            // Binds this board to one Mote store, once (docs/design/mote-adapter.md
            // section 2). A later bind to a different store is refused, never
            // silently followed.
            check_fields(a, &["store", "store_id", "cursor_mode", "genesis_digest"])?;
            let store = string(a, "store")?;
            let store_id = string(a, "store_id")?;
            text(store, "store", 4096, false)?;
            if !store_id.starts_with("st-") || store_id.len() > 64 {
                return Err(Error::invalid("store_id must be a Mote store id (st-...)"));
            }
            let bound = mote_binding(conn)?;
            if !bound.is_null() {
                if bound["store_id"] != store_id {
                    return Err(Error::new(
                        "mote_store_mismatch",
                        format!(
                            "this board is bound to Mote store {} at {}; {store} is {store_id}. Set MOTE_STORE to the bound store. There is no rebind; a different store needs a different board",
                            bound["store_id"].as_str().unwrap_or(""),
                            bound["store"].as_str().unwrap_or("")
                        ),
                    ));
                }
                configure_mote_ordering(conn, a)?;
                return Ok(json!({"binding":mote_binding(conn)?,"new":false}));
            }
            conn.execute(
                "INSERT INTO meta(key,value) VALUES('mote_store',?1),('mote_store_id',?2)",
                params![store, store_id],
            )?;
            configure_mote_ordering(conn, a)?;
            Ok(json!({"binding":mote_binding(conn)?,"new":true}))
        }
        "mote_ingest" => {
            // Mote events become attention exactly once per recipient, and the
            // cursor moves only forward, in the same transaction
            // (docs/design/mote-adapter.md section 6).
            check_fields(
                a,
                &[
                    "store_id",
                    "after",
                    "cursor",
                    "items",
                    "claims",
                    "reconcile",
                    "sync_revision",
                    "initialized",
                ],
            )?;
            let store_id = string(a, "store_id")?;
            let bound = mote_binding(conn)?;
            if bound.is_null() || bound["store_id"] != store_id {
                return Err(Error::new(
                    "mote_store_mismatch",
                    "events are from a Mote store this board is not bound to; run fray mote status",
                ));
            }
            let current = bound["cursor"].as_str().map(str::to_owned);
            let admission = bound["cursor_mode"] == "admission_v1";
            if admission && a["sync_revision"] != bound["sync_revision"] {
                return Err(Error::new(
                    "mote_cursor_moved",
                    "admission sync generation changed; no cards written",
                ));
            }
            let after = match a.get("after") {
                None | Some(Value::Null) => None,
                Some(_) => Some(string(a, "after")?.to_owned()),
            };
            if after != current {
                return Err(Error::new(
                    "mote_cursor_moved",
                    format!(
                        "another sync advanced the Mote cursor to {}; nothing was written",
                        current.as_deref().unwrap_or("(unset)")
                    ),
                ));
            }
            let cursor = if admission {
                match &a["cursor"] {
                    Value::Null => None,
                    Value::String(s) => Some(s.as_str()),
                    _ => return Err(Error::invalid("admission cursor must be a raw ID or null")),
                }
            } else {
                Some(string(a, "cursor")?)
            };
            if admission && current.is_some() && cursor.is_none() {
                return Err(Error::invalid(
                    "an initialized raw cursor cannot be cleared",
                ));
            }
            if let Some(cursor) = cursor {
                text(cursor, "cursor", 200, false)?;
            }
            if !admission
                && current
                    .as_deref()
                    .is_some_and(|c| cursor.is_some_and(|cursor| cursor < c))
            {
                return Err(Error::invalid("the Mote cursor only moves forward"));
            }
            let items = a["items"]
                .as_array()
                .filter(|i| i.len() <= 100)
                .ok_or_else(|| Error::invalid("items must be an array of at most 100"))?;
            conn.execute(
                "INSERT OR IGNORE INTO agents(name,role,topics,enabled,joined_ms,last_seen_ms) VALUES(?,'worker','[]',0,?,0)",
                params![MOTE, now],
            )?;
            let claims = match a.get("claims") {
                None | Some(Value::Null) => Vec::new(),
                Some(v) => v
                    .as_array()
                    .filter(|c| c.len() <= 100)
                    .ok_or_else(|| Error::invalid("claims must be an array of at most 100"))?
                    .clone(),
            };
            // Claim transitions, in event order, become cards here, where the
            // previous holder is known consistently with the cursor.
            let mut items: Vec<Value> = items.clone();
            let claim_table = if admission {
                "mote_feed_claims"
            } else {
                "mote_claims"
            };
            for c in &claims {
                check_fields(c, &["entity", "to", "by", "op_id", "released", "seed"])?;
                let entity = string(c, "entity")?;
                let op_id = string(c, "op_id")?;
                text(entity, "entity", 200, false)?;
                let to = c["to"].as_str();
                let by = c["by"].as_str().unwrap_or("");
                let stored: Option<(Option<String>, String)> = conn
                    .query_row(
                        &format!(
                            "SELECT holder,op_id FROM {claim_table} WHERE store_id=? AND entity=?"
                        ),
                        params![store_id, entity],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                // Transitions apply in op order, once. A chunk that did not
                // advance the cursor is replayed (an interrupted or concurrent
                // sync); its older transitions must not be compared against a
                // holder that newer ones already set, or they would invent a
                // change of hands.
                if !admission
                    && stored
                        .as_ref()
                        .is_some_and(|(_, seen)| op_id <= seen.as_str())
                {
                    continue;
                }
                if admission
                    && conn.execute(
                        "INSERT OR IGNORE INTO mote_claim_events(store_id,op_id) VALUES(?,?)",
                        params![store_id, op_id],
                    )? == 0
                {
                    continue;
                }
                let previous = stored.map(|(holder, _)| holder);
                let holder = if c["released"] == true { None } else { to };
                conn.execute(
                    &format!("INSERT INTO {claim_table}(store_id,entity,holder,op_id) VALUES(?1,?2,?3,?4) ON CONFLICT(store_id,entity) DO UPDATE SET holder=excluded.holder,op_id=excluded.op_id"),
                    params![store_id, entity, holder, op_id],
                )?;
                if admission && c["seed"] == true {
                    // Baseline both views quietly. Otherwise the next live
                    // board reconciliation invents a handoff from no holder.
                    conn.execute("INSERT INTO mote_claims(store_id,entity,holder,op_id) VALUES(?1,?2,?3,?4) ON CONFLICT(store_id,entity) DO UPDATE SET holder=excluded.holder,op_id=excluded.op_id",params![store_id,entity,holder,op_id])?;
                }
                if c["seed"] == true || c["released"] == true {
                    continue;
                }
                let Some(to) = to else { continue };
                let key = format!("claim:{entity}:{op_id}");
                if by != to {
                    items.push(json!({"key":key,"recipient":to,
                        "title":format!("Mote: {by} handed you {entity}"),
                        "summary":format!("{by} transferred the Mote claim on {entity} to you. Mote owns the claim: `mote show {entity}` for the work, and look for a Fray handoff packet from {by}."),
                        "priority":1,"refs":[entity]}));
                } else if admission {
                    // A holder's own claim owes them no card. Record that the
                    // feed saw it, so same-sync reconciliation does not tell
                    // them what they just did.
                    conn.execute(
                        "INSERT OR IGNORE INTO mote_events(store_id,key,recipient) VALUES(?,?,?)",
                        params![store_id, format!("claimself:{entity}:{op_id}"), to],
                    )?;
                }
                // The previous holder hears when someone else moved their claim (a
                // third-party handoff, or taking over one that expired), not when
                // they handed it off themselves.
                if let Some(prev) = previous.flatten().filter(|p| p != to && p != by) {
                    items.push(json!({"key":key,"recipient":prev,
                        "title":format!("Mote: your claim on {entity} is now {to}'s"),
                        "summary":format!("The Mote claim on {entity} you held (possibly expired) now belongs to {to}, by {by}. If that was not agreed, raise it with {by}."),
                        "priority":1,"refs":[entity]}));
                }
            }
            // Reconciliation against Mote's live board (section 6). The board
            // is the truth when it was read; each entry applies only if Fray
            // still records the holder the reader saw (compare-and-set), so a
            // concurrent sync is never overwritten. History is not consulted:
            // it cannot tell a handoff from a renewal, and op order can be
            // overturned by a late op. Cards say what changed, never who did it.
            let reconcile = match a.get("reconcile") {
                None | Some(Value::Null) => Vec::new(),
                Some(v) => v
                    .as_array()
                    .filter(|c| c.len() <= 100)
                    .ok_or_else(|| Error::invalid("reconcile must be an array of at most 100"))?
                    .clone(),
            };
            let mut raced = Vec::new();
            for r in &reconcile {
                check_fields(
                    r,
                    &[
                        "entity",
                        "expect",
                        "holder",
                        "lease_until",
                        "marker",
                        "covered_by_feed",
                    ],
                )?;
                let entity = string(r, "entity")?;
                text(entity, "entity", 200, false)?;
                let marker = string(r, "marker")?;
                let expect = r["expect"].as_str();
                let holder = r["holder"].as_str();
                let stored: Option<Option<String>> = conn
                    .query_row(
                        "SELECT holder FROM mote_claims WHERE store_id=? AND entity=?",
                        params![store_id, entity],
                        |r| r.get(0),
                    )
                    .optional()?;
                if stored.clone().flatten().as_deref() != expect {
                    raced.push(entity.to_owned());
                    continue;
                }
                let mut covered = false;
                if !r["covered_by_feed"].is_null() {
                    let proof = &r["covered_by_feed"];
                    check_fields(proof, &["op_id", "cursor"])?;
                    let op_id = string(proof, "op_id")?;
                    if admission
                        && proof["cursor"].as_str() == current.as_deref()
                        && holder.is_some()
                    {
                        // Check both the feed's exact final transition and its
                        // delivered recipient card (or the holder's own claim,
                        // which owes none) under the snapshot CAS.
                        covered = conn.query_row("SELECT EXISTS(SELECT 1 FROM mote_feed_claims f JOIN mote_events e ON e.store_id=f.store_id AND e.recipient=f.holder AND ((e.key=? AND e.card_id IS NOT NULL) OR e.key=?) WHERE f.store_id=? AND f.entity=? AND f.op_id=? AND f.holder=?)",params![format!("claim:{entity}:{op_id}"),format!("claimself:{entity}:{op_id}"),store_id,entity,op_id,holder],|row| row.get(0))?;
                    }
                }
                conn.execute(
                    "INSERT INTO mote_claims(store_id,entity,holder,op_id) VALUES(?1,?2,?3,?4) ON CONFLICT(store_id,entity) DO UPDATE SET holder=excluded.holder,op_id=excluded.op_id",
                    params![store_id, entity, holder, marker],
                )?;
                if covered {
                    continue;
                }
                // A release or expiry the feed missed is recorded quietly:
                // nobody is told anything that might be false.
                let Some(holder) = holder else { continue };
                let key = format!(
                    "claimstate:{entity}:{holder}:{}",
                    r["lease_until"].as_str().unwrap_or("")
                );
                let was = expect.unwrap_or("nobody");
                items.push(json!({"key":key,"recipient":holder,
                    "title":format!("Mote: you now hold {entity}"),
                    "summary":format!("Mote records you as the holder of the claim on {entity}; Fray last saw {was}. Found by reconciliation against Mote's board, so how it changed hands is not known. `mote show {entity}` for the work."),
                    "priority":1,"refs":[entity]}));
                if let Some(prev) = expect {
                    items.push(json!({"key":key,"recipient":prev,
                        "title":format!("Mote: {entity} is now held by {holder}"),
                        "summary":format!("Mote now records {holder} as the holder of the claim on {entity}, which Fray last saw you holding. Found by reconciliation, so how it changed hands is not known; if you did not expect it, check with {holder}."),
                        "priority":1,"refs":[entity]}));
                }
            }
            let (mut created, mut duplicate, mut unknown, mut invalid) =
                (Vec::new(), 0, Vec::new(), Vec::new());
            for item in &items {
                check_fields(
                    item,
                    &[
                        "key",
                        "recipient",
                        "title",
                        "summary",
                        "priority",
                        "refs",
                        "subject",
                        "final",
                    ],
                )?;
                let key = string(item, "key")?;
                let recipient = string(item, "recipient")?;
                text(key, "key", 300, false)?;
                // A Mote actor that has never joined this board has no inbox here.
                if recipient == MOTE
                    || !conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM agents WHERE name=?)",
                        [recipient],
                        |r| r.get::<_, bool>(0),
                    )?
                {
                    unknown.push(recipient.to_owned());
                    continue;
                }
                // A subject item reports current state: it is new when the state
                // differs from the last one delivered to this recipient. Other
                // items are new when their key has never been delivered.
                let subject = item["subject"].as_str();
                if let Some(subject) = subject {
                    let last: Option<(String, bool)> = conn
                        .query_row(
                            "SELECT state,final FROM mote_subjects WHERE store_id=? AND subject=? AND recipient=?",
                            params![store_id, subject, recipient],
                            |r| Ok((r.get(0)?, r.get(1)?)),
                        )
                        .optional()?;
                    // Unchanged, or anything after a final state: nothing to say.
                    if last
                        .as_ref()
                        .is_some_and(|(state, fin)| state == key || *fin)
                    {
                        duplicate += 1;
                        continue;
                    }
                } else if conn.execute(
                    "INSERT OR IGNORE INTO mote_events(store_id,key,recipient) VALUES(?,?,?)",
                    params![store_id, key, recipient],
                )? == 0
                {
                    duplicate += 1;
                    continue;
                }
                let mut tags = vec!["mote".to_owned()];
                for r in item["refs"].as_array().into_iter().flatten() {
                    if let Some(r) = r.as_str() {
                        tags.push(format!("mote:{r}"));
                    }
                }
                let title = clip(item["title"].as_str().unwrap_or(""), 160);
                let summary = clip(item["summary"].as_str().unwrap_or(""), 2000);
                // One malformed item must never stop every later event: it is
                // skipped and reported, and its dedupe row is undone.
                let card = match create_card(
                    conn,
                    MOTE,
                    &json!({"kind":"note","topic":format!("@{recipient}"),"title":title,
                        "summary":summary,"priority":item.get("priority").cloned().unwrap_or(json!(1)),
                        "tags":tags,"assignee":recipient}),
                    json!({"mote_key":key}),
                    now,
                ) {
                    Ok(card) => card,
                    Err(e) if e.code == "invalid" || e.code == "reserved_owner" => {
                        if subject.is_none() {
                            conn.execute(
                                "DELETE FROM mote_events WHERE store_id=? AND key=? AND recipient=?",
                                params![store_id, key, recipient],
                            )?;
                        }
                        invalid.push(json!({"key":key,"recipient":recipient,"error":e.message}));
                        continue;
                    }
                    Err(e) => return Err(e),
                };
                let id = card["card"]["id"].as_i64().unwrap_or(0);
                if let Some(subject) = subject {
                    conn.execute(
                        "INSERT INTO mote_subjects(store_id,subject,recipient,state,final) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(store_id,subject,recipient) DO UPDATE SET state=excluded.state,final=excluded.final",
                        params![store_id, subject, recipient, key, item["final"] == true],
                    )?;
                }
                conn.execute(
                    "UPDATE mote_events SET card_id=? WHERE store_id=? AND key=? AND recipient=?",
                    params![id, store_id, key, recipient],
                )?;
                created.push(id);
            }
            if let Some(cursor) = cursor {
                conn.execute("INSERT INTO meta(key,value) VALUES('mote_cursor',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[cursor])?;
            } else {
                conn.execute("DELETE FROM meta WHERE key='mote_cursor'", [])?;
            }
            if admission {
                conn.execute("INSERT INTO meta(key,value) VALUES('mote_cursor_initialized',?) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[if a["initialized"]==false {"false"}else{"true"}])?;
                conn.execute(
                    "UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='mote_sync_revision'",
                    [],
                )?;
            }
            conn.execute("DELETE FROM meta WHERE key='mote_sync_timeouts'", [])?;
            // When any agent last synced: background runners pace themselves
            // on this, so a board syncs about once per interval, not once per
            // runner.
            conn.execute(
                "INSERT INTO meta(key,value) VALUES('mote_last_sync_ms',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [now.to_string()],
            )?;
            unknown.sort();
            unknown.dedup();
            Ok(
                json!({"created":created,"duplicate":duplicate,"unknown_recipients":unknown,"invalid":invalid,"raced":raced,"cursor":cursor,"synced_by":actor,"sync_revision":mote_binding(conn)?["sync_revision"]}),
            )
        }
        "mote_requests_sync" => {
            // State, not events (R2): each request is carded once while open,
            // to its addressee, and settled when Mote shows it answered.
            // Fray never acts in Mote; acking or closing the card is not a
            // Mote answer, and the card says so.
            check_fields(a, &["store_id", "requests"])?;
            let store_id = string(a, "store_id")?;
            let bound = mote_binding(conn)?;
            if bound.is_null() || bound["store_id"] != store_id {
                return Err(Error::new(
                    "mote_store_mismatch",
                    "requests are from a Mote store this board is not bound to; run fray mote status",
                ));
            }
            let requests = a["requests"]
                .as_array()
                .ok_or_else(|| Error::invalid("requests must be an array"))?;
            if requests.len() > 500 {
                return Err(Error::invalid("at most 500 requests per call"));
            }
            let (mut created, mut settled, mut unknown, mut invalid) =
                (Vec::new(), Vec::new(), Vec::new(), Vec::new());
            // Cards here are authored by the reserved, never-enabled `mote`
            // identity, as mote_ingest's are; register it on first use.
            conn.execute(
                "INSERT OR IGNORE INTO agents(name,role,topics,enabled,joined_ms,last_seen_ms) VALUES(?,'worker','[]',0,?,0)",
                params![MOTE, now],
            )?;
            for r in requests {
                check_fields(
                    r,
                    &[
                        "msg_id",
                        "recipient",
                        "from",
                        "state",
                        "body",
                        "entity",
                        "sent_ms",
                    ],
                )?;
                // When Mote says it was sent; never later than now.
                let sent_ms = r["sent_ms"].as_i64().map(|t| t.min(now));
                let msg_id = string(r, "msg_id")?;
                let recipient = string(r, "recipient")?;
                let state = string(r, "state")?;
                if let Err(e) = text(msg_id, "msg_id", 200, false) {
                    invalid.push(
                        json!({"msg_id":clip(msg_id, 60),"recipient":recipient,"error":e.message}),
                    );
                    continue;
                }
                if !["open", "responded", "declined", "resolved"].contains(&state) {
                    return Err(Error::invalid("state: open|responded|declined|resolved"));
                }
                let row: Option<(i64, String)> = conn
                    .query_row(
                        "SELECT card_id,state FROM mote_requests WHERE store_id=? AND msg_id=? AND recipient=?",
                        params![store_id, msg_id, recipient],
                        |x| Ok((x.get(0)?, x.get(1)?)),
                    )
                    .optional()?;
                match (row, state) {
                    // Already carded, or answered before it was ever carded.
                    (Some((_, ref was)), "open") if was == "open" => {}
                    (Some((_, ref was)), _) if was != "open" => {}
                    (None, s) if s != "open" => {
                        conn.execute(
                            "DELETE FROM mote_requests_unknown WHERE store_id=? AND msg_id=? AND recipient=?",
                            params![store_id, msg_id, recipient],
                        )?;
                    }
                    (None, _) => {
                        let joined: bool = conn.query_row(
                            "SELECT EXISTS(SELECT 1 FROM agents WHERE name=?)",
                            [recipient],
                            |x| x.get(0),
                        )?;
                        if recipient == MOTE || !joined {
                            // Kept for R3: nobody here can be asked, so it
                            // escalates once its grace period passes.
                            conn.execute(
                                "INSERT INTO mote_requests_unknown(store_id,msg_id,recipient,sender,first_seen_ms,last_seen_ms) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(store_id,msg_id,recipient) DO UPDATE SET last_seen_ms=excluded.last_seen_ms",
                                params![store_id, msg_id, recipient, r["from"].as_str().unwrap_or("?"), sent_ms.unwrap_or(now), now],
                            )?;
                            unknown.push(json!({"recipient":recipient,"msg_id":msg_id}));
                            continue;
                        }
                        // Mote text is untrusted input: control characters are
                        // replaced, over-long refs dropped, and an item that
                        // still fails is skipped and reported, never allowed to
                        // stop the rest (mote-adapter.md section 6; #79).
                        let scrub = |t: &str| -> String {
                            t.chars()
                                .map(|ch| {
                                    if ch.is_control() && ch != '\n' {
                                        '\u{FFFD}'
                                    } else {
                                        ch
                                    }
                                })
                                .collect()
                        };
                        let from = scrub(r["from"].as_str().unwrap_or("?"));
                        let body = scrub(r["body"].as_str().unwrap_or(""));
                        let mut tags = vec!["mote".to_owned()];
                        for t in [Some(msg_id), r["entity"].as_str().filter(|e| !e.is_empty())]
                            .into_iter()
                            .flatten()
                        {
                            let tag = format!("mote:{t}");
                            if tag.len() <= 80 && !tag.chars().any(char::is_control) {
                                tags.push(tag);
                            }
                        }
                        // The instructions first, so a long request cannot clip them.
                        let card = match create_card(
                            conn,
                            MOTE,
                            &json!({"kind":"question","topic":format!("@{recipient}"),
                                "title":clip(&format!("Mote request from {from}: {}", body.lines().next().unwrap_or("")), 160),
                                "summary":clip(&format!("{from} asked you this in Mote ({msg_id}). Answer there: `mote msg reply {msg_id} TEXT` (or --kind decline). Acking or closing this card is not a Mote answer; it settles here when Mote shows the request answered.\n\n{body}"), 2000),
                                "priority":1,"tags":tags,"assignee":recipient}),
                            json!({"mote_request":msg_id,"from":from,"sent_ms":sent_ms}),
                            now,
                        ) {
                            Ok(card) => card,
                            Err(e) if e.code == "invalid" || e.code == "reserved_owner" => {
                                invalid.push(json!({"msg_id":msg_id,"recipient":recipient,"error":e.message}));
                                continue;
                            }
                            Err(e) => return Err(e),
                        };
                        let id = card["card"]["id"].as_i64().unwrap_or(0);
                        conn.execute(
                            "INSERT INTO mote_requests(store_id,msg_id,recipient,card_id,state) VALUES(?,?,?,?,'open')",
                            params![store_id, msg_id, recipient, id],
                        )?;
                        // Carded now (the addressee joined): no longer an
                        // unknown-recipient request.
                        conn.execute(
                            "DELETE FROM mote_requests_unknown WHERE store_id=? AND msg_id=? AND recipient=?",
                            params![store_id, msg_id, recipient],
                        )?;
                        created.push(id);
                    }
                    (Some((card_id, _)), answered) => {
                        // Open here, answered in Mote: settle the card.
                        conn.execute(
                            "UPDATE mote_requests SET state=? WHERE store_id=? AND msg_id=? AND recipient=?",
                            params![answered, store_id, msg_id, recipient],
                        )?;
                        let c = get_card(conn, card_id)?;
                        // Already closed here: recorded, but not counted as
                        // settled by this sync.
                        if c.terminal() {
                            continue;
                        }
                        {
                            conn.execute(
                                "UPDATE cards SET rev=rev+1,status='resolved',lease_owner=NULL,lease_until_ms=0,fence=fence+1,updated_ms=? WHERE id=?",
                                params![now, card_id],
                            )?;
                            emit(
                                conn,
                                MOTE,
                                "patch",
                                card_id,
                                json!({"mote_request":msg_id,"mote_state":answered,
                                    "note":format!("{answered} in Mote")}),
                                now,
                                true,
                            )?;
                        }
                        settled.push(card_id);
                    }
                }
            }
            Ok(
                json!({"created":created,"settled":settled,"unknown_recipients":unknown,"invalid":invalid}),
            )
        }
        "escalate_tick" => {
            // R3: find stuck requests and escalate each to every present
            // steward, once, board-wide; settle escalations whose request has
            // cleared. The daemon decides what is stuck, so a runner can
            // neither forge nor revive one.
            check_fields(a, &["interval_ms"])?;
            let interval = a
                .get("interval_ms")
                .map(|_| bounded(a, "interval_ms", 60_000, 100, 3_600_000))
                .transpose()?;
            escalate_tick(conn, interval, now)
        }
        "mote_sync_failed" => {
            // Consecutive timed-out syncs; the client reseeds at the tail after
            // three, and reconciliation covers the gap.
            check_fields(a, &[])?;
            conn.execute(
                "INSERT INTO meta(key,value) VALUES('mote_sync_timeouts','1') ON CONFLICT(key) DO UPDATE SET value=CAST(value AS INTEGER)+1",
                [],
            )?;
            let n: i64 = conn.query_row(
                "SELECT CAST(value AS INTEGER) FROM meta WHERE key='mote_sync_timeouts'",
                [],
                |r| r.get(0),
            )?;
            Ok(json!({"consecutive_timeouts":n}))
        }
        "set_status" => {
            // One current status line per agent, updated in place; empty clears.
            check_fields(a, &["text"])?;
            let status = string(a, "text")?;
            text(status, "status", 200, true)?;
            if status.trim().is_empty() {
                conn.execute("DELETE FROM agent_status WHERE agent=?", [actor])?;
            } else {
                conn.execute(
                    "INSERT INTO agent_status(agent,text,updated_ms) VALUES(?,?,?) ON CONFLICT(agent) DO UPDATE SET text=excluded.text,updated_ms=excluded.updated_ms",
                    params![actor, status, now],
                )?;
            }
            Ok(json!({"agent":actor,"status":status}))
        }
        "present" => {
            // Records exactly what a CLI showed. Presentation is exposure, never ACK.
            check_fields(a, &["source", "receipts"])?;
            let source = string(a, "source")?;
            if !["inbox", "wait", "attention", "thread"].contains(&source) {
                return Err(Error::invalid("source: inbox|wait|attention|thread"));
            }
            let receipts = a["receipts"]
                .as_array()
                .filter(|r| !r.is_empty() && r.len() <= 100)
                .ok_or_else(|| Error::invalid("receipts must be a nonempty array"))?;
            let store_id: String =
                conn.query_row("SELECT value FROM meta WHERE key='store_id'", [], |r| {
                    r.get(0)
                })?;
            let mut items = Vec::new();
            for receipt in receipts {
                check_fields(receipt, &["store_id", "agent", "id", "through_seq"])?;
                if string(receipt, "store_id")? != store_id || string(receipt, "agent")? != actor {
                    return Err(Error::new(
                        "receipt_mismatch",
                        "receipt belongs to another store or agent",
                    ));
                }
                let id = integer(receipt, "id")?;
                let through = integer(receipt, "through_seq")?;
                let pending: Option<i64> = conn
                    .query_row(
                        "SELECT pending_seq FROM deliveries WHERE agent=? AND card_id=?",
                        params![actor, id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if !pending.is_some_and(|pending| through > 0 && through <= pending) {
                    return Err(Error::invalid(
                        "each receipt must name a delivered version of this agent's card",
                    ));
                }
                if items.iter().any(|(card, _)| *card == id) {
                    return Err(Error::invalid("a batch lists each card once"));
                }
                items.push((id, through));
            }
            let batch = random_key()?;
            conn.execute(
                "INSERT INTO presented_batches(batch,agent,session,source,created_ms) VALUES(?,?,?,?,?)",
                params![batch, actor, req.session, source, now],
            )?;
            for (id, through) in &items {
                conn.execute(
                    "INSERT INTO presented_items(batch,card_id,through_seq) VALUES(?,?,?)",
                    params![batch, id, through],
                )?;
            }
            // Bounded retention: the newest 32 batches per agent, none older than a day.
            let stale = "SELECT batch FROM presented_batches WHERE agent=?1 AND (created_ms<?2 OR batch NOT IN (SELECT batch FROM presented_batches WHERE agent=?1 ORDER BY created_ms DESC,rowid DESC LIMIT 32))";
            conn.execute(
                &format!("DELETE FROM presented_items WHERE batch IN ({stale})"),
                params![actor, now - BATCH_TTL_MS],
            )?;
            conn.execute(
                &format!("DELETE FROM presented_batches WHERE batch IN ({stale})"),
                params![actor, now - BATCH_TTL_MS],
            )?;
            Ok(
                json!({"batch":{"id":batch,"source":source,"items":items.iter().map(|(id,through)|json!({"id":id,"through_seq":through})).collect::<Vec<_>>()},"acknowledged":false}),
            )
        }
        "expose" => {
            check_fields(a, &["receipts"])?;
            let receipts = a["receipts"]
                .as_array()
                .ok_or_else(|| Error::invalid("receipts must be an array"))?;
            if receipts.len() > 100 {
                return Err(Error::invalid("too many receipts"));
            }
            for r in receipts {
                check_fields(r, &["id", "through"])?;
                let through = integer(r, "through")?;
                if through < 0 {
                    return Err(Error::invalid("through must be nonnegative"));
                }
                conn.execute("UPDATE deliveries SET shown_seq=max(shown_seq,?),shown_at_ms=? WHERE agent=? AND card_id=? AND pending_seq>=?",params![through,now,actor,integer(r,"id")?,through])?;
            }
            Ok(json!({"exposed":receipts.len(),"acknowledged":false}))
        }
        _ => Err(Error::invalid("unknown mutation")),
    }
}
fn apply_fields(c: &mut Card, a: &Value) -> Result<()> {
    for k in ["kind", "topic", "title", "summary", "status"] {
        if a.get(k).is_some() {
            let s = string(a, k)?.to_string();
            match k {
                "kind" => c.kind = s,
                "topic" => c.topic = s,
                "title" => c.title = s,
                "summary" => c.summary = s,
                _ => c.status = s,
            }
        }
    }
    if a.get("priority").is_some() {
        c.priority = integer(a, "priority")?;
    }
    if a.get("pinned").is_some() {
        c.pinned = boolean(a, "pinned", false)?;
    }
    if let Some(v) = a.get("tags") {
        c.tags = tags(v)?;
    }
    if let Some(v) = a.get("assignee") {
        c.assignee = if v.is_null() {
            None
        } else {
            Some(string(a, "assignee")?.into())
        };
    }
    Ok(())
}
fn read(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    let a = &req.args;
    let actor = &req.actor;
    match req.op.as_str() {
        "ping" => {
            check_fields(a, &[])?;
            Ok(
                json!({"version":env!("CARGO_PKG_VERSION"),"build":BUILD,"protocol_version":PROTOCOL_VERSION,"capabilities":["mote_rpc_receipts","dispatch","mote_admission_order","mote_workflows","peer_discovery","review_subjects","reply_ack_batch","agents_all","reply_refs","long_messages","inbox_filters","attention_stream","wait_filters","attention_filters","wait_indefinite","read_batches","thread_unread","thread_compact","sessions","objection_gate","card_attention","mute","idle_readiness","ack_last","pending_send","addressed_full_text","owner_channel","lanes","session_continue","partner_routing","stats","mote_adapter","mote_sync","mote_reconcile","mote_subjects","mote_requests","ask_deadlines","escalations"],"cursor":highwater(conn)?,"time_ms":now}),
            )
        }
        "brief" => {
            check_fields(a, &["budget"])?;
            registered(conn, actor)?;
            brief(
                conn,
                actor,
                bounded(a, "budget", 12000, 2000, 64000)? as usize,
                now,
            )
        }
        "query" => query(conn, a, actor, now),
        "keepalive_status" => {
            check_fields(a, &["agent"])?;
            let agent = a
                .get("agent")
                .map(|_| string(a, "agent"))
                .transpose()?
                .unwrap_or(actor);
            crate::keepalive::status(conn, agent, now)
        }
        "peers" => crate::presence::delta(conn, req, now),
        "show" => {
            check_fields(
                a,
                &[
                    "id", "history", "after", "limit", "receipts", "unread", "compact",
                ],
            )?;
            let compact = boolean(a, "compact", false)?;
            let c = get_card(conn, integer(a, "id")?)?;
            let mut v = json!({"card":c,"cursor":highwater(conn)?});
            let review = crate::review::context(conn, c.id, true)?;
            if !review.is_null() {
                v["review"] = review;
            }
            if c.author == OWNER {
                v["card"]["authority"] = json!("owner (unsigned)");
            }
            v["full_text"] = json!(full_text(conn, &c)?);
            open_follow_ups(conn, c.id, &mut v)?;
            objection_presentation(conn, c.id, &mut v)?;
            if boolean(a, "unread", false)? {
                // Unread = this reader's delivered-but-unacknowledged range. The
                // receipt never reaches past the events actually returned.
                registered(conn, actor)?;
                let limit = bounded(a, "limit", 20, 1, 100)?;
                let delivery: Option<(i64, i64)> = conn
                    .query_row(
                        "SELECT pending_seq,ack_seq FROM deliveries WHERE agent=? AND card_id=?",
                        params![actor, c.id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let (pending, ack) = delivery.unwrap_or((0, 0));
                // Continue after a page without acknowledging it. ACK is cumulative,
                // so a continuation receipt is honest only if the skipped prefix
                // (ack, after] was actually presented to this reader.
                let start = bounded(a, "after", ack, 0, i64::MAX)?.max(ack);
                let prefix_presented = start == ack
                    || conn.query_row(
                        "SELECT EXISTS(SELECT 1 FROM presented_batches b JOIN presented_items i ON i.batch=b.batch WHERE b.agent=? AND i.card_id=? AND i.through_seq>=?)",
                        params![actor, c.id, start],
                        |r| r.get::<_, bool>(0),
                    )?;
                // Your own messages are context you already have, not news. They
                // are skipped (and counted), and the cumulative receipt still
                // covers them, so they never linger as "unread".
                let unread = unread_events(conn, c.id, actor, start, pending, limit + 1)?;
                let more = unread.len() > limit as usize;
                let unread: Vec<Value> = unread.into_iter().take(limit as usize).collect();
                let through = if more {
                    unread
                        .last()
                        .and_then(|e| e["seq"].as_i64())
                        .unwrap_or(start)
                } else {
                    pending.max(start)
                };
                let store_id: String =
                    conn.query_row("SELECT value FROM meta WHERE key='store_id'", [], |r| {
                        r.get(0)
                    })?;
                let own_skipped: i64 = conn.query_row(
                    "SELECT count(*) FROM events WHERE card_id=? AND seq>? AND seq<=? AND actor=?",
                    params![c.id, start, through, actor],
                    |r| r.get(0),
                )?;
                v["own_skipped"] = json!(own_skipped);
                v["unread"] = json!(compact_events(&unread));
                v["ack_seq"] = json!(ack);
                v["pending_seq"] = json!(pending);
                v["more"] = json!(more);
                v["next_after"] = json!(through);
                v["after"] = json!(start);
                v["read_is_not_ack"] = json!(true);
                v["receipt"] = if through > ack && through <= pending && prefix_presented {
                    json!({"store_id":store_id,"agent":actor,"id":c.id,"through_seq":through})
                } else {
                    Value::Null
                };
                if !prefix_presented && through > start {
                    v["receipt_withheld"] = json!(format!(
                        "events after @{ack} through @{start} were not presented to you; read from the start or present that page first"
                    ));
                }
                return Ok(v);
            }
            if boolean(a, "receipts", false)? {
                let mut s=conn.prepare("SELECT d.agent,d.pending_seq,d.ack_seq,d.shown_seq,d.shown_at_ms,a.enabled FROM deliveries d JOIN agents a ON a.name=d.agent WHERE d.card_id=? AND d.pending_seq>0 ORDER BY d.agent LIMIT 101")?;
                let rows=s.query_map([c.id],|r|Ok(json!({"agent":r.get::<_,String>(0)?,"pending_seq":r.get::<_,i64>(1)?,"ack_seq":r.get::<_,i64>(2)?,"exposed_seq":r.get::<_,i64>(3)?,"exposed_at_ms":r.get::<_,i64>(4)?,"enabled":r.get::<_,bool>(5)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
                v["receipts_more"] = json!(rows.len() > 100);
                v["receipts"] = json!(rows.into_iter().take(100).collect::<Vec<_>>());
            }
            if boolean(a, "history", false)? {
                let limit = bounded(a, "limit", 20, 1, 100)?;
                let after = bounded(a, "after", 0, 0, i64::MAX)?;
                let e = events(conn, after, Some(c.id), limit + 1)?;
                v["more"] = json!(e.len() > limit as usize);
                let e: Vec<_> = e.into_iter().take(limit as usize).collect();
                v["next_after"] = json!(e.last().and_then(|v| v["seq"].as_i64()).unwrap_or(after));
                v["history"] = if compact {
                    json!(compact_events(&e))
                } else {
                    json!(e)
                };
            }
            Ok(v)
        }
        "inbox" => {
            check_fields(
                a,
                &[
                    "after",
                    "limit",
                    "fresh",
                    "selection",
                    "card_ids",
                    "addressed_to_me",
                    "unresolved",
                    "kinds",
                    "min_priority",
                    "full_text_budget",
                ],
            )?;
            inbox(
                conn,
                actor,
                bounded(a, "after", 0, 0, i64::MAX)?,
                bounded(a, "limit", 20, 1, 100)?,
                boolean(a, "fresh", false)?,
                InboxSelection::parse(a)?,
                bounded(
                    a,
                    "full_text_budget",
                    ADDRESSED_FULL_TEXT_BUDGET as i64,
                    0,
                    64_000,
                )? as usize,
                now,
            )
        }
        "batch" => {
            // Fetch a presented batch. Validates ownership; never acknowledges.
            check_fields(a, &["batch"])?;
            registered(conn, actor)?;
            let batch = string(a, "batch")?;
            let items = batch_items(conn, actor, batch, now)?;
            let (source, created): (String, i64) = conn.query_row(
                "SELECT source,created_ms FROM presented_batches WHERE batch=?",
                [batch],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )?;
            let store_id: String =
                conn.query_row("SELECT value FROM meta WHERE key='store_id'", [], |r| {
                    r.get(0)
                })?;
            let mut out = Vec::new();
            for (id, through) in items {
                let (pending, ack): (i64, i64) = conn.query_row(
                    "SELECT pending_seq,ack_seq FROM deliveries WHERE agent=? AND card_id=?",
                    params![actor, id],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                out.push(json!({"card":get_card(conn,id)?.compact(now),"receipt":{"store_id":store_id,"agent":actor,"id":id,"through_seq":through},"handled":ack>=through,"newer_pending":pending>through}));
            }
            Ok(
                json!({"batch":{"id":batch,"source":source,"created_ms":created},"items":out,"read_is_not_ack":true}),
            )
        }
        "receipt_status" => {
            check_fields(a, &["receipts"])?;
            registered(conn, actor)?;
            let receipts = a["receipts"]
                .as_array()
                .ok_or_else(|| Error::invalid("receipts must be an array"))?;
            if receipts.len() > 100 {
                return Err(Error::invalid("too many receipts"));
            }
            let identity: String =
                conn.query_row("SELECT value FROM meta WHERE key='store_id'", [], |r| {
                    r.get(0)
                })?;
            let mut handled = 0;
            for r in receipts {
                check_fields(r, &["store_id", "agent", "id", "through_seq"])?;
                if string(r, "store_id")? != identity || string(r, "agent")? != actor {
                    return Err(Error::new(
                        "receipt_mismatch",
                        "receipt belongs to another store or agent",
                    ));
                }
                let through = integer(r, "through_seq")?;
                if through <= 0 {
                    return Err(Error::invalid("through_seq must be positive"));
                }
                let ack: Option<i64> = conn
                    .query_row(
                        "SELECT ack_seq FROM deliveries WHERE agent=? AND card_id=?",
                        params![actor, integer(r, "id")?],
                        |r| r.get(0),
                    )
                    .optional()?;
                if ack.is_some_and(|ack| ack >= through) {
                    handled += 1;
                }
            }
            Ok(json!({"handled":handled,"total":receipts.len()}))
        }
        "search_history" => {
            check_fields(a, &["q", "limit", "offset"])?;
            let q = string(a, "q")?;
            text(q, "q", 1000, false)?;
            let limit = bounded(a, "limit", 20, 1, 100)?;
            let offset = bounded(a, "offset", 0, 0, 1000000)?;
            let mut s=conn.prepare("SELECT e.seq,e.ts_ms,e.actor,e.op,e.card_id,snippet(event_fts,0,'[',']','…',24) FROM event_fts JOIN events e ON e.seq=event_fts.rowid WHERE event_fts MATCH ? ORDER BY e.seq DESC LIMIT ? OFFSET ?")?;
            let items=s.query_map(params![q,limit+1,offset],|r|Ok(json!({"seq":r.get::<_,i64>(0)?,"ts_ms":r.get::<_,i64>(1)?,"actor":r.get::<_,String>(2)?,"op":r.get::<_,String>(3)?,"card_id":r.get::<_,i64>(4)?,"historical":true,"excerpt":r.get::<_,String>(5)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let more = items.len() > limit as usize;
            Ok(
                json!({"items":items.into_iter().take(limit as usize).collect::<Vec<_>>(),"more":more,"next_offset":offset+limit,"cursor":highwater(conn)?,"warning":"Historical text may be superseded. Use show for the current head."}),
            )
        }
        "lanes" => {
            check_fields(a, &["paths"])?;
            let lanes = live_lanes(conn, now)?;
            // With paths: only lanes that could touch them (preflight).
            let lanes = match a.get("paths").and_then(Value::as_array) {
                Some(paths) => {
                    let paths: Vec<String> = paths
                        .iter()
                        .filter_map(|p| p.as_str().map(str::to_owned))
                        .collect();
                    lanes
                        .into_iter()
                        .filter(|l| {
                            l["paths"].as_array().into_iter().flatten().any(|h| {
                                h.as_str()
                                    .is_some_and(|h| paths.iter().any(|p| paths_overlap(h, p)))
                            })
                        })
                        .collect()
                }
                None => lanes,
            };
            Ok(json!({"lanes":lanes}))
        }
        "agents" => {
            check_fields(a, &["limit", "all"])?;
            roster(
                conn,
                now,
                bounded(a, "limit", 100, 1, 100)?,
                boolean(a, "all", false)?,
            )
        }
        "mote_binding" => {
            check_fields(a, &[])?;
            Ok(json!({"binding":mote_binding(conn)?}))
        }
        "mote_operation_get" => {
            check_fields(a, &["key"])?;
            Ok(json!({"operation":crate::mote_workflow::operation(conn,actor,string(a,"key")?)?}))
        }
        "mote_rpc_receipt" => crate::mote_workflow::rpc_receipt(conn, actor, a),
        op if op.starts_with("dispatch_") => crate::dispatch::read(conn, req, now),
        "stuck_requests" => {
            // R3, read only: what is stuck now, and whether anyone is ticking.
            check_fields(a, &[])?;
            let (last, ticking) = tick_state(conn, now)?;
            // Every Mote request the board already tracks, so a read-only
            // scan of Mote lists only the ones no runner has brought (#85).
            let mut s = conn.prepare(
                "SELECT msg_id FROM mote_requests UNION SELECT msg_id FROM mote_requests_unknown LIMIT 5000",
            )?;
            let known = s
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({"stuck":stuck_set(conn, now)?,"last_tick_ms":last,
                "ticking":ticking,"known_msg_ids":known}))
        }
        "mote_requests_tracked" => {
            // The requests carded and still open here, so a sync can read
            // their current state from Mote even after they leave its open
            // listing.
            check_fields(a, &["store_id"])?;
            let store_id = string(a, "store_id")?;
            let mut s = conn.prepare(
                "SELECT msg_id,recipient FROM mote_requests WHERE store_id=? AND state='open' ORDER BY msg_id LIMIT 500",
            )?;
            let rows = s
                .query_map([store_id], |r| {
                    Ok(json!({"msg_id":r.get::<_,String>(0)?,"recipient":r.get::<_,String>(1)?}))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            // Board agents, so a sync reads their requests before those of
            // Mote actors who never joined (review of 405e671, #80).
            let mut s = conn.prepare("SELECT name FROM agents WHERE enabled=1 ORDER BY name")?;
            let joined = s
                .query_map([], |r| r.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            // Requests to actors not on the board, still thought open, so a
            // sync reads them and sees them answered (#84).
            let mut s = conn.prepare(
                "SELECT msg_id,recipient FROM mote_requests_unknown WHERE store_id=? ORDER BY first_seen_ms LIMIT 200",
            )?;
            let unknown = s
                .query_map([store_id], |r| {
                    Ok(json!({"msg_id":r.get::<_,String>(0)?,"recipient":r.get::<_,String>(1)?}))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({"tracked":rows,"joined":joined,"unknown":unknown}))
        }
        "mote_claims" => {
            // Every live holder Fray has recorded for a store, for
            // reconciliation against Mote's live board (docs/design/
            // mote-adapter.md section 6). Released rows are left out, so they
            // can never push live claims past the listing limit.
            check_fields(a, &["store_id"])?;
            let store_id = string(a, "store_id")?;
            let mut s = conn.prepare(
                "SELECT entity,holder FROM mote_claims WHERE store_id=? AND holder IS NOT NULL ORDER BY entity LIMIT 10001",
            )?;
            let rows = s
                .query_map([store_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            let more = rows.len() > 10_000;
            let holders: serde_json::Map<String, Value> = rows
                .into_iter()
                .take(10_000)
                .map(|(e, h)| (e, json!(h)))
                .collect();
            Ok(json!({"holders":holders,"more":more,"coverage_supported":true}))
        }
        "stats" => {
            check_fields(a, &["window_ms"])?;
            let window = match a.get("window_ms") {
                None | Some(Value::Null) => None,
                Some(_) => Some(bounded(a, "window_ms", 0, 1, i64::MAX / 2)?),
            };
            crate::stats::stats(conn, window, now)
        }
        "friction" => {
            check_fields(a, &[])?;
            crate::stats::friction(conn, now)
        }
        _ => Err(Error::invalid(format!("unknown operation: {}", req.op))),
    }
}
fn prefix_cols() -> String {
    COLS.split(',')
        .map(|x| format!("c.{x}"))
        .collect::<Vec<_>>()
        .join(",")
}
fn select_page(
    conn: &Connection,
    condition: &str,
    values: Vec<SqlValue>,
    order: &str,
    limit: i64,
    offset: i64,
    now: i64,
) -> Result<Value> {
    let total: i64 = conn.query_row(
        &format!("SELECT count(*) FROM cards c WHERE {condition}"),
        params_from_iter(values.iter()),
        |r| r.get(0),
    )?;
    let sql = format!(
        "SELECT {} FROM cards c WHERE {condition} ORDER BY {order} LIMIT ? OFFSET ?",
        prefix_cols()
    );
    let mut values = values;
    values.push(limit.into());
    values.push(offset.into());
    let mut s = conn.prepare(&sql)?;
    let cards = s
        .query_map(params_from_iter(values.iter()), row_card)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut items = Vec::new();
    for card in &cards {
        let mut item = card.compact(now);
        objection_presentation(conn, card.id, &mut item)?;
        items.push(item);
    }
    Ok(
        json!({"items":items,"total":total,"more":offset+(cards.len() as i64)<total,"next_offset":offset+cards.len() as i64,"cursor":highwater(conn)?}),
    )
}
fn query(conn: &Connection, a: &Value, actor: &str, now: i64) -> Result<Value> {
    check_fields(
        a,
        &[
            "q",
            "topic",
            "kind",
            "status",
            "tag",
            "assignee",
            "owner",
            "unowned",
            "all",
            "sort",
            "limit",
            "offset",
            "stale_secs",
            "scope",
        ],
    )?;
    for (field, allowed) in [
        (
            "kind",
            &["goal", "task", "question", "decision", "note"][..],
        ),
        (
            "status",
            &[
                "open",
                "active",
                "blocked",
                "resolved",
                "superseded",
                "withdrawn",
            ][..],
        ),
    ] {
        if a.get(field).is_some() && !allowed.contains(&string(a, field)?) {
            return Err(Error::invalid(format!("invalid {field} filter")));
        }
    }
    let mut where_parts = vec!["1=1".to_string()];
    let mut p: Vec<SqlValue> = Vec::new();
    if !boolean(a, "all", false)? && a.get("status").is_none() {
        where_parts.push(ACTIVE.into());
    }
    for key in ["topic", "kind", "status", "assignee"] {
        if a.get(key).is_some() {
            where_parts.push(format!("c.{key}=?"));
            p.push(string(a, key)?.to_string().into());
        }
    }
    if a.get("owner").is_some() {
        where_parts.push("c.lease_owner=? AND c.lease_until_ms>?".into());
        p.push(string(a, "owner")?.to_string().into());
        p.push(now.into());
    }
    if let Some(v) = a.get("q") {
        let q = v
            .as_str()
            .ok_or_else(|| Error::invalid("q must be a string"))?;
        text(q, "q", 1000, false)?;
        where_parts.push("c.id IN (SELECT rowid FROM card_fts WHERE card_fts MATCH ?)".into());
        p.push(q.to_string().into());
    }
    if a.get("tag").is_some() {
        where_parts.push("EXISTS(SELECT 1 FROM json_each(c.tags) t WHERE t.value=?)".into());
        p.push(string(a, "tag")?.to_string().into());
    }
    if boolean(a, "unowned", false)? {
        where_parts.push("(c.lease_owner IS NULL OR c.lease_until_ms<=?)".into());
        p.push(now.into());
    }
    if a.get("stale_secs").is_some() {
        let secs = bounded(a, "stale_secs", 0, 0, 86400 * 365)?;
        where_parts.push("c.updated_ms<=?".into());
        p.push((now - secs * 1000).into());
    }
    if boolean(a, "scope", false)? {
        registered(conn, actor)?;
        where_parts.push(RELEVANT.into());
        for _ in 0..4 {
            p.push(actor.to_string().into());
        }
    }
    let sort = a
        .get("sort")
        .map(|_| string(a, "sort"))
        .transpose()?
        .unwrap_or("priority");
    let order = match sort {
        "priority" => "c.pinned DESC,c.priority ASC,c.created_ms ASC,c.id ASC",
        "recent" => "c.last_seq DESC,c.id DESC",
        "oldest" => "c.created_ms ASC,c.id ASC",
        "id" => "c.id ASC",
        _ => return Err(Error::invalid("sort: priority|recent|oldest|id")),
    };
    select_page(
        conn,
        &where_parts.join(" AND "),
        p,
        order,
        bounded(a, "limit", 20, 1, 100)?,
        bounded(a, "offset", 0, 0, 1000000)?,
        now,
    )
}
/// How long a session's binding to a name survives without activity.
pub const IDENTITY_TTL_MS: i64 = 30 * 60_000;
/// How long a queued lane lets its holder keep taking paths inside it. After
/// that the queue comes first: the holder finishes, releases, and waits its turn.
pub const LANE_PATIENCE_MS: i64 = 10 * 60_000;
/// How long waiting (an armed wait or listener) keeps an otherwise idle agent
/// holding its lanes. Waiting always counts as reachable for routing.
pub const WAIT_HOLD_MS: i64 = 4 * 60 * 60_000;
/// A wait refreshes its row every minute; older rows are not waiting.
const WAIT_FRESH_MS: i64 = 150_000;

fn session_label(session: &str) -> String {
    clip(session, 20)
}

/// Bind the request's host session to its agent name. Two live sessions
/// never silently share a name: the second is refused unless it joins with
/// `takeover`, which ends the first visibly. A stale binding yields.
/// Returns what was replaced, if anything. Legacy (session-less) requests pass.
///
/// The one exception is a companion pair: the keepalive session the daemon
/// started for this name binds beside its interactive session, in a slot of
/// its own. Neither displaces the other, so a `/clear` takeover in the
/// terminal leaves the keepalive bound (docs/design/keepalive.md).
fn bind_session(conn: &Connection, req: &Request, now: i64) -> Result<Option<Value>> {
    let Some(session) = req.session.as_deref() else {
        return Ok(None);
    };
    if session.is_empty()
        || session.len() > 128
        || !session
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || ":._-".contains(c))
    {
        return Err(Error::invalid(
            "session must be 1..128 characters of [A-Za-z0-9:._-]",
        ));
    }
    let actor = &req.actor;
    if crate::keepalive::is_session(Some(session)) {
        if !crate::keepalive::authorized(conn, actor, session)? {
            return Err(Error::new(
                "keepalive_session",
                format!("{session:?} is not a keepalive the daemon started for {actor:?}; only `fray keepalive` starts one"),
            ));
        }
        // Only the daemon's current keepalive is authorized, so any other
        // open keepalive binding of this name is an earlier one.
        conn.execute(
            "UPDATE sessions SET ended_ms=?,ended_reason='keepalive replaced' WHERE agent=? AND ended_ms IS NULL AND session GLOB 'keepalive:*' AND session<>?",
            params![now, actor, session],
        )?;
        conn.execute(
            "INSERT INTO sessions(session,agent,started_ms,last_seen_ms) VALUES(?,?,?,?) ON CONFLICT(session,agent) DO UPDATE SET last_seen_ms=excluded.last_seen_ms,ended_ms=NULL,ended_reason=NULL",
            params![session, actor, now, now],
        )?;
        return Ok(None);
    }
    let takeover = req.op == "join" && boolean(&req.args, "takeover", false)?;
    // Set by the host hook when a new session continues the same window
    // (Claude Code's /clear): recorded as such, not as a hostile takeover.
    let continued = if req.op == "join" {
        match req.args.get("continued").and_then(Value::as_str) {
            None => None,
            Some(s @ ("clear" | "compact")) => Some(s),
            Some(_) => return Err(Error::invalid("continued: clear|compact")),
        }
    } else {
        None
    };
    let bound: Option<(String, i64)> = conn
        .query_row(
            "SELECT session,last_seen_ms FROM sessions WHERE agent=? AND ended_ms IS NULL AND session NOT GLOB 'keepalive:*' ORDER BY last_seen_ms DESC LIMIT 1",
            [actor],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let replaced = match bound {
        Some((held, _)) if held == session => {
            conn.execute(
                "UPDATE sessions SET last_seen_ms=? WHERE session=? AND agent=?",
                params![now, session, actor],
            )?;
            return Ok(None);
        }
        Some((held, seen))
            if now - session_seen(conn, actor, &held, seen, now)? < IDENTITY_TTL_MS
                && !takeover =>
        {
            return Err(Error::new(
                "identity_busy",
                format!(
                    "{actor:?} is in use by another live session ({}, last seen {}s ago). Choose a distinct --as NAME; use join --takeover only if that session is gone (for example, you ran /clear and that was you)",
                    session_label(&held),
                    (now - seen) / 1000
                ),
            ));
        }
        Some((held, seen)) => {
            let reason = if let Some(how) = continued {
                format!("continued after /{how} by {}", session_label(session))
            } else if now - seen < IDENTITY_TTL_MS {
                format!("takeover by {}", session_label(session))
            } else {
                format!("stale; replaced by {}", session_label(session))
            };
            conn.execute(
                "UPDATE sessions SET ended_ms=?,ended_reason=? WHERE session=? AND agent=?",
                params![now, reason, held, actor],
            )?;
            Some(json!({"session":session_label(&held),"last_seen_ms":seen,"reason":reason}))
        }
        None => None,
    };
    conn.execute(
        "INSERT INTO sessions(session,agent,started_ms,last_seen_ms) VALUES(?,?,?,?) ON CONFLICT(session,agent) DO UPDATE SET last_seen_ms=excluded.last_seen_ms,ended_ms=NULL,ended_reason=NULL",
        params![session, actor, now, now],
    )?;
    Ok(replaced)
}

/// When a bound session was last seen, counting its own wait in progress
/// within WAIT_HOLD_MS of the session's last real activity.
fn session_seen(conn: &Connection, agent: &str, session: &str, seen: i64, now: i64) -> Result<i64> {
    let waiting: Option<i64> = conn
        .query_row(
            "SELECT refreshed_ms FROM session_waits WHERE agent=? AND session=? AND refreshed_ms>?",
            params![agent, session, now - WAIT_FRESH_MS],
            |r| r.get(0),
        )
        .optional()?;
    Ok(match waiting {
        Some(t) if now - seen < WAIT_HOLD_MS => t.max(seen),
        _ => seen,
    })
}

/// The live binding for `roster`, plus a recent takeover so a displaced
/// session can see what happened.
fn session_status(conn: &Connection, agent: &str, now: i64) -> Result<Value> {
    let bound = conn
        .query_row(
            "SELECT session,started_ms,last_seen_ms FROM sessions WHERE agent=? AND ended_ms IS NULL AND session NOT GLOB 'keepalive:*' ORDER BY last_seen_ms DESC LIMIT 1",
            [agent],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?)),
        )
        .optional()?
        .map(|(session, since, seen)| -> Result<Value> {
            let live = now - session_seen(conn, agent, &session, seen, now)? < IDENTITY_TTL_MS;
            Ok(json!({"session":session_label(&session),"since_ms":since,"last_seen_ms":seen,"live":live}))
        })
        .transpose()?;
    let takeover = conn
        .query_row(
            "SELECT session,ended_ms,ended_reason FROM sessions WHERE agent=? AND (ended_reason LIKE 'takeover%' OR ended_reason LIKE 'continued%') AND ended_ms>? ORDER BY ended_ms DESC LIMIT 1",
            params![agent, now - IDENTITY_TTL_MS],
            |r| Ok(json!({"displaced":session_label(&r.get::<_,String>(0)?),"at_ms":r.get::<_,i64>(1)?,"reason":r.get::<_,String>(2)?})),
        )
        .optional()?;
    Ok(json!({"bound":bound,"recent_takeover":takeover}))
}

/// A lane path, normalized for comparison: repo-relative, no `..`, no
/// control or invisible characters, no blank segments. `./` and repeated or trailing slashes are dropped;
/// a glob (`*`, `?`, `[`) stands for its whole directory; comparison is
/// case-insensitive. Deliberately conservative: when unsure, paths overlap.
/// Returns the prefix ("" means the whole repository).
fn lane_path(p: &str) -> Result<String> {
    if p.is_empty()
        || p.len() > 200
        || p.starts_with('/')
        || p.split('/').any(|seg| seg == "..")
        || p.chars().any(|c| c.is_control() || invisible(c))
        || p.split('/')
            .any(|seg| !seg.is_empty() && seg.trim().is_empty())
    {
        return Err(Error::invalid(format!(
            "lane path {p:?}: repo-relative, no .., 1..200 characters, no blank names"
        )));
    }
    let mut parts: Vec<&str> = p
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect();
    // A glob stands for its directory. So does a non-ASCII name: the same
    // name can arrive precomposed or decomposed (NFC/NFD), and without
    // Unicode tables the safe comparison is the whole enclosing directory.
    if let Some(cut) = parts
        .iter()
        .position(|s| s.contains(['*', '?', '[']) || !s.is_ascii())
    {
        parts.truncate(cut);
    }
    Ok(parts.join("/").to_ascii_lowercase())
}

/// Characters that render as nothing or reorder text: zero-width, bidi
/// controls, word joiners, BOM, variation selectors and tag characters.
fn invisible(c: char) -> bool {
    matches!(c as u32,
        0x00AD | 0x034F | 0x061C | 0x115F | 0x1160 | 0x17B4 | 0x17B5 | 0x180B..=0x180F
        | 0x200B..=0x200F | 0x2028..=0x202E | 0x2060..=0x206F | 0x3164 | 0xFE00..=0xFE0F
        | 0xFEFF | 0xFFA0 | 0xFFF0..=0xFFFB | 0x1D173..=0x1D17A | 0xE0000..=0xE0FFF)
}

/// Whether two lane paths could touch the same file: equal, or one is a
/// directory prefix of the other. Unparseable paths count as overlapping:
/// a safety check must never report them clear.
pub fn paths_overlap(a: &str, b: &str) -> bool {
    let (Ok(a), Ok(b)) = (lane_path(a), lane_path(b)) else {
        return true;
    };
    a.is_empty()
        || b.is_empty()
        || a == b
        || b.starts_with(&format!("{a}/"))
        || a.starts_with(&format!("{b}/"))
}

/// At most `max` bytes, cut on a character boundary, marked when cut.
fn clip(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_owned();
    }
    let mut end = max.saturating_sub('…'.len_utf8());
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}…", &s[..end])
}

/// The Mote store this board is bound to, or null.
fn configure_mote_ordering(conn: &Connection, args: &Value) -> Result<()> {
    let Some(mode) = args.get("cursor_mode").and_then(Value::as_str) else {
        return Ok(());
    };
    if mode != "admission_v1" {
        return Err(Error::invalid("explicit cursor mode must be admission_v1"));
    }
    let digest = string(args, "genesis_digest")?;
    text(digest, "genesis_digest", 100, false)?;
    let old = mote_binding(conn)?;
    if old["cursor_mode"] == "admission_v1" {
        if old["genesis_digest"] != digest {
            return Err(Error::new(
                "mote_store_mismatch",
                "authority genesis changed under existing binding",
            ));
        }
        return Ok(());
    }
    conn.execute("INSERT INTO meta(key,value) VALUES('mote_cursor_mode','admission_v1'),('mote_authority_genesis',?),('mote_cursor_initialized','false'),('mote_sync_revision','1') ON CONFLICT(key) DO UPDATE SET value=excluded.value",[digest])?;
    conn.execute("DELETE FROM meta WHERE key='mote_cursor'", [])?;
    Ok(())
}

fn mote_binding(conn: &Connection) -> Result<Value> {
    let get = |key: &str| -> Result<Option<String>> {
        Ok(conn
            .query_row("SELECT value FROM meta WHERE key=?", [key], |r| r.get(0))
            .optional()?)
    };
    Ok(match (get("mote_store")?, get("mote_store_id")?) {
        (Some(store), Some(store_id)) => json!({"store":store,"store_id":store_id,
            "cursor":get("mote_cursor")?,
            "cursor_mode":get("mote_cursor_mode")?.unwrap_or_else(||"legacy_filename".into()),
            "cursor_initialized":get("mote_cursor_initialized")?.as_deref()==Some("true") || get("mote_cursor")?.is_some(),
            "sync_revision":get("mote_sync_revision")?.and_then(|n|n.parse::<i64>().ok()).unwrap_or(0),
            "genesis_digest":get("mote_authority_genesis")?,
            "consecutive_timeouts":get("mote_sync_timeouts")?.and_then(|n| n.parse::<i64>().ok()).unwrap_or(0),
            "last_sync_ms":get("mote_last_sync_ms")?.and_then(|n| n.parse::<i64>().ok())}),
        _ => Value::Null,
    })
}
/// Whether an agent is still present, meaning reachable: a live session or
/// recent write, an armed (connected) listener or running drive loop, which
/// will hear a lane notice, or a recent presentation of receipts (inbox, wait,
/// thread or hook). Polling an empty inbox is not recorded; arm a wait or set
/// a status instead.
pub(crate) fn agent_live(conn: &Connection, agent: &str, now: i64) -> Result<bool> {
    let last = last_active(conn, agent)?;
    let active = last.is_some_and(|t| now - t < IDENTITY_TTL_MS);
    Ok(
        active
            || (agent_waiting(conn, agent, now)? && last.is_some_and(|t| now - t < WAIT_HOLD_MS)),
    )
}

/// The agent's last deliberate activity: a live session, a write, a running
/// drive loop, or receipts shown to it. Waiting is not activity.
fn last_active(conn: &Connection, agent: &str) -> Result<Option<i64>> {
    Ok(conn.query_row(
        "SELECT max(t) FROM (SELECT last_seen_ms AS t FROM sessions WHERE agent=?1 AND ended_ms IS NULL UNION ALL SELECT last_seen_ms FROM agents WHERE name=?1 AND enabled=1 UNION ALL SELECT updated_ms FROM controllers WHERE agent=?1 AND state IN ('waiting','running') UNION ALL SELECT max(created_ms) FROM presented_batches WHERE agent=?1 UNION ALL SELECT max(shown_at_ms) FROM deliveries WHERE agent=?1)",
        [agent],
        |r| r.get(0),
    )?)
}

/// Whether something is armed to hear the agent's mail right now: a connected
/// listener or a wait in progress.
pub(crate) fn agent_waiting(conn: &Connection, agent: &str, now: i64) -> Result<bool> {
    // The interactive side's waits, as agent_waits held them: a keepalive
    // speaks for itself through its controller.
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM listeners WHERE agent=?1 AND connected=1) OR EXISTS(SELECT 1 FROM session_waits WHERE agent=?1 AND session NOT GLOB 'keepalive:*' AND refreshed_ms>?2)",
        params![agent, now - WAIT_FRESH_MS],
        |r| r.get(0),
    )?)
}

/// Whether a card assigned to the agent will reach it, and when.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Reach {
    /// Something armed will wake it: a live drive controller, an armed
    /// listener under the strict `idle_readiness` test, or an unfiltered
    /// `fray wait` in progress.
    Wakeable,
    /// Recently active, but nothing armed: it sees new mail only at its next
    /// turn, if it has one.
    Present,
    /// Neither.
    Absent,
}
impl Reach {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Reach::Wakeable => "wakeable",
            Reach::Present => "present",
            Reach::Absent => "absent",
        }
    }
}

/// The one reachability test (docs/design/no-silent-stalls.md, R1), used for
/// routing, send notices and friction. Not for lanes, which ask whether the
/// holder is still working (`agent_live`).
pub(crate) fn reachability(conn: &Connection, agent: &str, now: i64) -> Result<Reach> {
    if wake_state(conn, agent, now)?.2 {
        return Ok(Reach::Wakeable);
    }
    let waiting_unfiltered: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM wake_waits w JOIN agents a ON a.name=w.agent WHERE w.agent=?1 AND a.enabled=1 AND w.refreshed_ms>?2)",
        params![agent, now - WAIT_FRESH_MS],
        |r| r.get(0),
    )?;
    if waiting_unfiltered {
        return Ok(Reach::Wakeable);
    }
    if last_active(conn, agent)?.is_some_and(|t| now - t < IDENTITY_TTL_MS)
        || agent_waiting(conn, agent, now)?
    {
        return Ok(Reach::Present);
    }
    Ok(Reach::Absent)
}

/// A note for a sender when nothing will wake the recipient, naming who can
/// be woken. None when the recipient is wakeable, is the owner, or is the
/// sender.
fn absence_notice(conn: &Connection, who: &str, actor: &str, now: i64) -> Result<Option<String>> {
    if who == OWNER || who == actor {
        return Ok(None);
    }
    let reach = reachability(conn, who, now)?;
    if reach == Reach::Wakeable {
        return Ok(None);
    }
    let seen: i64 = conn
        .query_row("SELECT last_seen_ms FROM agents WHERE name=?", [who], |r| {
            r.get(0)
        })
        .optional()?
        .unwrap_or(0);
    let enabled: bool = conn
        .query_row("SELECT enabled FROM agents WHERE name=?", [who], |r| {
            r.get(0)
        })
        .optional()?
        .unwrap_or(false);
    let when = if !enabled && seen <= 0 {
        "has not joined yet; it will see this when it joins".to_owned()
    } else if reach == Reach::Present {
        format!(
            "is present (last active {} min ago) but has nothing armed to wake it; it will see this at its next turn, if it has one",
            (now - seen).max(0) / 60_000
        )
    } else {
        format!(
            "was last active {} min ago and has nothing armed; it will see this when it next reads",
            (now - seen).max(0) / 60_000
        )
    };
    let mut s =
        conn.prepare("SELECT name FROM agents WHERE enabled=1 ORDER BY last_seen_ms DESC")?;
    let names = s
        .query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (mut wakeable, mut present) = (Vec::new(), Vec::new());
    for name in names {
        if name == who || name == actor || name == OWNER {
            continue;
        }
        match reachability(conn, &name, now)? {
            Reach::Wakeable => wakeable.push(name),
            Reach::Present => present.push(name),
            Reach::Absent => {}
        }
    }
    let mut others = Vec::new();
    if !wakeable.is_empty() {
        others.push(format!("wakeable now: {}", wakeable.join(", ")));
    }
    if !present.is_empty() {
        others.push(format!("present, nothing armed: {}", present.join(", ")));
    }
    let present = if others.is_empty() {
        "nobody else is present".to_owned()
    } else {
        others.join("; ")
    };
    Ok(Some(format!("{who} {when} ({present}).")))
}

/// Live (unreleased) lanes, with a stale flag for holders no longer present.
pub(crate) fn live_lanes(conn: &Connection, now: i64) -> Result<Vec<Value>> {
    let mut s = conn.prepare(
        "SELECT id,agent,paths,purpose,card_id,state,created_ms FROM lanes WHERE released_ms IS NULL ORDER BY id",
    )?;
    let rows = s
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, Option<i64>>(4)?,
                r.get::<_, String>(5)?,
                r.get::<_, i64>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut out = Vec::new();
    for (id, agent, paths, purpose, card, state, created) in rows {
        let stale = !agent_live(conn, &agent, now)?;
        out.push(json!({"id":id,"agent":agent,"paths":serde_json::from_str::<Value>(&paths)?,"purpose":purpose,"card":card,"state":state,"created_ms":created,"stale":stale}));
    }
    Ok(out)
}

fn lane_values(l: &Value) -> Vec<String> {
    l["paths"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| p.as_str().map(str::to_owned))
        .collect()
}

/// Other agents' lanes that block `paths`: any overlapping held lane, and any
/// overlapping queued lane that was there first (queues are first come,
/// first served). `before` is the queued lane's own id, or None for a new take.
/// A queued lane that is itself waiting on one of the actor's held lanes does
/// not block the actor: it is waiting for the actor to finish, and blocking
/// the actor would deadlock both. That exemption lasts LANE_PATIENCE_MS
/// from the queued lane's creation (a handover keeps it).
fn lane_conflicts(
    lanes: &[Value],
    actor: &str,
    paths: &[String],
    before: Option<i64>,
    now: i64,
) -> Vec<Value> {
    let lane_paths = lane_values;
    let overlap =
        |a: &[String], b: &[String]| a.iter().any(|x| b.iter().any(|y| paths_overlap(x, y)));
    let mine: Vec<Vec<String>> = lanes
        .iter()
        .filter(|l| l["agent"] == actor && l["state"] == "held")
        .map(lane_paths)
        .collect();
    // The exemption covers work strictly inside what the queuer waits for,
    // never retaking the whole of it, so a holder cannot starve the queue.
    let covers = |p: &str, q: &str| match (lane_path(p), lane_path(q)) {
        (Ok(p), Ok(q)) => p.is_empty() || p == q || q.starts_with(&format!("{p}/")),
        _ => true,
    };
    let exempt = |l: &Value| {
        let theirs = lane_paths(l);
        l["created_ms"]
            .as_i64()
            .is_some_and(|since| now - since < LANE_PATIENCE_MS)
            && mine.iter().any(|m| overlap(m, &theirs))
            && !paths.iter().any(|p| theirs.iter().any(|q| covers(p, q)))
    };
    lanes
        .iter()
        .filter(|l| l["agent"] != actor)
        .filter(|l| {
            l["state"] == "held"
                || (l["state"] == "queued"
                    && before.is_none_or(|b| l["id"].as_i64().is_some_and(|id| id < b))
                    && !exempt(l))
        })
        .filter(|l| overlap(&lane_paths(l), paths))
        .cloned()
        .collect()
}

/// Tell an agent something about its lane, as an addressed note.
fn lane_notice(
    conn: &Connection,
    actor: &str,
    to: &str,
    title: &str,
    text: &str,
    now: i64,
) -> Result<()> {
    lane_notice_tagged(conn, actor, to, "lane", title, text, now)
}

fn lane_notice_tagged(
    conn: &Connection,
    actor: &str,
    to: &str,
    tag: &str,
    title: &str,
    text: &str,
    now: i64,
) -> Result<()> {
    if to == actor {
        return Ok(());
    }
    let mut tags = vec!["lane".to_owned(), tag.to_owned()];
    tags.dedup();
    create_card(
        conn,
        actor,
        &json!({"kind":"note","topic":format!("@{to}"),"title":title,"summary":text,"assignee":to,"tags":tags}),
        json!({"body":text}),
        now,
    )?;
    Ok(())
}

fn agent_status_text(conn: &Connection, agent: &str) -> Result<Value> {
    Ok(conn
        .query_row(
            "SELECT text,updated_ms FROM agent_status WHERE agent=?",
            [agent],
            |r| Ok(json!({"text":r.get::<_,String>(0)?,"updated_ms":r.get::<_,i64>(1)?})),
        )
        .optional()?
        .unwrap_or(Value::Null))
}

/// Paths of an agent's held lanes, for the roster ("who's where").
fn held_lane_paths(conn: &Connection, agent: &str) -> Result<Vec<Value>> {
    let mut s = conn.prepare(
        "SELECT id,paths,purpose FROM lanes WHERE agent=? AND state='held' AND released_ms IS NULL ORDER BY id",
    )?;
    let rows = s
        .query_map([agent], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter()
        .map(|(id, paths, purpose)| {
            Ok(json!({"id":id,"paths":serde_json::from_str::<Value>(&paths)?,"purpose":purpose}))
        })
        .collect()
}

const BATCH_TTL_MS: i64 = 86_400_000;

/// Conservative crossing-message signal, not a claim about comprehension.
/// Background watch/wait presentations cannot conceal a newer peer update.
fn unseen_peer_updates(conn: &Connection, req: &Request, id: i64, now: i64) -> Result<Value> {
    let (pending, ack): (i64, i64) = conn
        .query_row(
            "SELECT pending_seq,ack_seq FROM deliveries WHERE agent=? AND card_id=?",
            params![req.actor, id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .unwrap_or((0, 0));
    // Without a session, unrelated callers have the same NULL binding. Do not
    // treat their presentations as this caller's explicit read (as with ack --last).
    let read: i64 = if let Some(session) = &req.session {
        conn.query_row(
        "SELECT coalesce(max(i.through_seq),0) FROM presented_items i JOIN presented_batches b ON b.batch=i.batch WHERE b.agent=? AND i.card_id=? AND b.session IS ? AND b.source IN ('inbox','thread') AND b.created_ms>=?",
        params![req.actor, id, session, now - BATCH_TTL_MS],
        |r| r.get(0),
        )?
    } else {
        0
    };
    let since = ack.max(read);
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM events WHERE card_id=? AND seq>? AND seq<=? AND actor<>? AND op<>'renew'",
        params![id, since, pending, req.actor],
        |r| r.get(0),
    )?;
    Ok(json!({"count":count,"after_seq":since,"through_seq":pending}))
}

/// The newest batch this session explicitly read (inbox or thread). Waits and
/// attention packets are excluded: both often run in the background, so the
/// newest one may have arrived after what the agent actually read. Ack those
/// by their batch token.
/// Without a session there is no safe notion of "last": fail closed.
fn last_batch(conn: &Connection, actor: &str, session: Option<&str>, now: i64) -> Result<String> {
    let session = session.ok_or_else(|| {
        Error::new(
            "session_required",
            "ack --last needs a host session (Claude/Codex sessions are detected automatically; else set FRAY_SESSION); use ack --batch ID",
        )
    })?;
    conn.query_row(
        "SELECT batch FROM presented_batches WHERE agent=? AND session=? AND source IN ('inbox','thread') AND created_ms>=? ORDER BY created_ms DESC,rowid DESC LIMIT 1",
        params![actor, session, now - BATCH_TTL_MS],
        |r| r.get(0),
    )
    .optional()?
    .ok_or_else(|| {
        Error::new(
            "batch_unknown",
            "this session has no inbox/thread batch to acknowledge (waits and attention packets are acked by their batch token); nothing was acknowledged",
        )
    })
}

/// The exact (card, through_seq) pairs one presented batch showed this actor.
fn batch_items(conn: &Connection, actor: &str, batch: &str, now: i64) -> Result<Vec<(i64, i64)>> {
    let owner: Option<(String, i64)> = conn
        .query_row(
            "SELECT agent,created_ms FROM presented_batches WHERE batch=?",
            [batch],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let (agent, created) = owner.ok_or_else(|| {
        Error::new(
            "batch_unknown",
            "unknown or expired batch; nothing was acknowledged",
        )
    })?;
    if agent != actor {
        return Err(Error::new(
            "batch_foreign",
            "batch was presented to another agent; nothing was acknowledged",
        ));
    }
    if created < now - BATCH_TTL_MS {
        return Err(Error::new(
            "batch_unknown",
            "unknown or expired batch; nothing was acknowledged",
        ));
    }
    let mut s = conn.prepare(
        "SELECT card_id,through_seq FROM presented_items WHERE batch=? ORDER BY card_id",
    )?;
    let items = s
        .query_map([batch], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(items)
}

/// One event without the full card head each raw event repeats. Messages
/// keep their complete body; state changes keep only the fields they changed.
fn compact_event(e: &Value, previous: Option<&Value>) -> Value {
    let detail = &e["payload"]["detail"];
    let card = &e["payload"]["card"];
    let mut out = json!({"seq":e["seq"],"ts_ms":e["ts_ms"],"actor":e["actor"],"op":e["op"]});
    if e["actor"] == OWNER {
        out["authority"] = json!("owner (unsigned)");
    }
    if let Some(kind) = detail["kind"].as_str() {
        out["kind"] = json!(kind);
    }
    if let Some(body) = detail["body"].as_str() {
        out["body"] = json!(body);
    } else if e["op"] == "post" {
        out["title"] = card["title"].clone();
        out["body"] = card["summary"].clone();
    }
    for key in [
        "refs",
        "review",
        "follow_up_id",
        "parent_card",
        "over_objection",
        "open_objections",
        "objection_resolved_by",
    ] {
        if !detail[key].is_null() && detail[key] != json!([]) {
            out[key] = detail[key].clone();
        }
    }
    if e["op"] != "annotate" && e["op"] != "post" {
        let mut changed = serde_json::Map::new();
        // Every conversation-state field a patch/claim/release can change.
        for key in [
            "kind",
            "topic",
            "title",
            "summary",
            "status",
            "assignee",
            "priority",
            "pinned",
            "lease_owner",
            "tags",
        ] {
            if previous.is_none_or(|p| p[key] != card[key]) {
                changed.insert(key.to_owned(), card[key].clone());
            }
        }
        out["changed"] = Value::Object(changed);
    }
    out
}

fn compact_events(events: &[Value]) -> Vec<Value> {
    let mut previous: Option<&Value> = None;
    events
        .iter()
        .map(|e| {
            let out = compact_event(e, previous);
            previous = Some(&e["payload"]["card"]);
            out
        })
        .collect()
}

const FOLLOW_UP_LIMIT: usize = 50;

fn objection_origin(conn: &Connection, id: i64) -> Result<Option<(String, i64)>> {
    Ok(conn.query_row("SELECT coalesce(json_extract(child.payload,'$.detail.source_objector'),parent.actor),coalesce(json_extract(child.payload,'$.detail.parent_card'),parent.card_id) FROM events child LEFT JOIN events parent ON parent.seq=json_extract(child.payload,'$.detail.annotation_seq') WHERE child.card_id=? AND child.op='post' AND ((parent.op='annotate' AND json_extract(parent.payload,'$.detail.kind')='objection') OR (json_extract(child.payload,'$.detail.kind')='objection' AND json_extract(child.payload,'$.detail.source_objector') IS NOT NULL)) ORDER BY child.seq LIMIT 1", [id], |r| Ok((r.get(0)?, r.get(1)?))).optional()?)
}

pub(crate) fn force_delivery(
    conn: &Connection,
    to: &str,
    card: i64,
    seq: i64,
    now: i64,
) -> Result<()> {
    conn.execute("INSERT OR IGNORE INTO agents(name,role,topics,enabled,joined_ms,last_seen_ms) VALUES(?,'worker','[]',0,?,0)", params![to,now])?;
    conn.execute("INSERT INTO deliveries(agent,card_id,pending_seq) VALUES(?,?,?) ON CONFLICT(agent,card_id) DO UPDATE SET pending_seq=max(pending_seq,excluded.pending_seq)", params![to,card,seq])?;
    Ok(())
}

/// A fresh addressed notice guarantees visibility even if the source card
/// was muted. The mute remains intact, and absent recipients retain the
/// notice for their next join. Request-key replay deduplicates the transaction.
fn objection_notice(
    conn: &Connection,
    actor: &str,
    to: &str,
    id: i64,
    title: &str,
    body: &str,
    now: i64,
) -> Result<()> {
    force_delivery(conn, to, id, get_card(conn, id)?.last_seq, now)?;
    let notice = create_card(
        conn,
        actor,
        &json!({"kind":"note","topic":format!("@{to}"),"title":format!("{title}: #{id}"),"summary":clip(body,1900),"priority":1,"assignee":to,"tags":["objection-notice",format!("objection-source:{id}")]}),
        Value::Null,
        now,
    )?;
    force_delivery(
        conn,
        to,
        integer(&notice["card"], "id")?,
        integer(&notice, "event_seq")?,
        now,
    )
}

/// Compact, bounded current objection state; immutable origins cannot be
/// erased by editing a title, kind or routing tag.
fn objection_presentation(conn: &Connection, id: i64, value: &mut Value) -> Result<()> {
    let mut query = conn.prepare("SELECT c.id,c.status,coalesce(json_extract(child.payload,'$.detail.source_objector'),parent.actor),(SELECT e.actor FROM events e WHERE e.card_id=c.id AND json_extract(e.payload,'$.detail.objection_resolved_by') IS NOT NULL ORDER BY e.seq DESC LIMIT 1) FROM events child LEFT JOIN events parent ON parent.seq=json_extract(child.payload,'$.detail.annotation_seq') JOIN cards c ON c.id=child.card_id WHERE child.op='post' AND coalesce(json_extract(child.payload,'$.detail.parent_card'),parent.card_id)=? AND ((parent.op='annotate' AND json_extract(parent.payload,'$.detail.kind')='objection') OR (json_extract(child.payload,'$.detail.kind')='objection' AND json_extract(child.payload,'$.detail.source_objector') IS NOT NULL)) ORDER BY c.id LIMIT 6")?;
    let mut rows = query.query_map([id], |r| Ok(json!({"id":r.get::<_,i64>(0)?,"status":r.get::<_,String>(1)?,"objector":r.get::<_,String>(2)?,"resolved_by":r.get::<_,Option<String>>(3)?})))?.collect::<rusqlite::Result<Vec<_>>>()?;
    if !rows.is_empty() {
        let more = rows.len() > 5;
        rows.truncate(5);
        value["objections"] =
            json!({"items":rows,"more":more,"fetch":format!("fray thread {id} --bodies")});
    }
    let override_event: Option<(String,String)> = conn.query_row("SELECT actor,json_extract(payload,'$.detail.over_objection') FROM events WHERE card_id=? AND json_extract(payload,'$.detail.over_objection') IS NOT NULL ORDER BY seq DESC LIMIT 1", [id], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
    if let Some((by, reason)) = override_event {
        value["objection_override"] = json!({"by":by,"reason":clip(&reason,240),"fetch":format!("fray thread {id} --bodies")});
    }
    if let Some((objector, parent)) = objection_origin(conn, id)? {
        let resolved: Option<String> = conn.query_row("SELECT json_extract(payload,'$.detail.objection_resolved_by') FROM events WHERE card_id=? AND json_extract(payload,'$.detail.objection_resolved_by') IS NOT NULL ORDER BY seq DESC LIMIT 1", [id], |r| r.get(0)).optional()?;
        value["objection"] = json!({"objector":objector,"parent":parent,"resolved_by":resolved});
    }
    Ok(())
}

/// The complete current text of a card. A long send keeps only a bounded
/// head on the card; the whole message lives in its creation event, or, for
/// a linked question/objection card, in the annotation that raised it. The
/// original is used exactly while the card still shows its creation head and
/// that head is not the original itself (it was clipped, or had a pointer
/// line added); after a patch the current summary is the truth. No lengths.
fn full_text(conn: &Connection, card: &Card) -> Result<String> {
    let original: Option<(Option<String>, Option<String>, bool)> = conn
        .query_row(
            "SELECT coalesce(json_extract(e.payload,'$.detail.body'),(SELECT json_extract(a.payload,'$.detail.body') FROM events a WHERE a.seq=json_extract(e.payload,'$.detail.annotation_seq') AND a.op='annotate')),json_extract(e.payload,'$.card.summary'),json_extract(e.payload,'$.detail.annotation_seq') IS NOT NULL FROM events e WHERE e.card_id=? AND e.op='post' ORDER BY e.seq LIMIT 1",
            [card.id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    Ok(match original {
        Some((Some(body), Some(head), linked)) if head == card.summary && body != head => {
            match head.lines().last().filter(|_| linked) {
                // Keep a linked card's pointer and "resolve" instruction.
                Some(pointer) if pointer.starts_with("Full context:") => {
                    format!("{body}\n{pointer}")
                }
                _ => body,
            }
        }
        _ => card.summary.clone(),
    })
}

/// Open follow-ups on `card` that originated as objections (not questions).
fn open_objections(conn: &Connection, card: i64) -> Result<Vec<i64>> {
    let mut s = conn.prepare(&format!(
        "SELECT c.id FROM cards c JOIN events child ON child.card_id=c.id AND child.op='post' LEFT JOIN events parent ON parent.seq=json_extract(child.payload,'$.detail.annotation_seq') WHERE {ACTIVE} AND coalesce(json_extract(child.payload,'$.detail.parent_card'),parent.card_id)=? AND ((parent.op='annotate' AND json_extract(parent.payload,'$.detail.kind')='objection') OR (json_extract(child.payload,'$.detail.kind')='objection' AND json_extract(child.payload,'$.detail.source_objector') IS NOT NULL)) ORDER BY c.id"
    ))?;
    let ids = s
        .query_map([card], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<i64>>>()?;
    Ok(ids)
}

/// Open questions and objections raised against a card, via their parent tag.
/// Never silently truncated: an omission is flagged with a continuation.
fn open_follow_ups(conn: &Connection, card: i64, v: &mut Value) -> Result<()> {
    let mut s = conn.prepare(&format!(
        "SELECT c.id,c.title,c.status,c.assignee,c.author FROM cards c WHERE {ACTIVE} AND EXISTS(SELECT 1 FROM json_each(c.tags) t WHERE t.value=?1) AND EXISTS(SELECT 1 FROM events e WHERE e.card_id=c.id AND e.op='post' AND json_extract(e.payload,'$.detail.parent_card')=?2) ORDER BY c.id LIMIT {}",
        FOLLOW_UP_LIMIT + 1
    ))?;
    let mut rows = s
        .query_map(params![format!("parent:{card}"), card], |r| {
            Ok(json!({"id":r.get::<_,i64>(0)?,"title":r.get::<_,String>(1)?,"status":r.get::<_,String>(2)?,"assignee":r.get::<_,Option<String>>(3)?,"author":r.get::<_,String>(4)?}))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = rows.len() > FOLLOW_UP_LIMIT;
    rows.truncate(FOLLOW_UP_LIMIT);
    v["follow_ups"] = json!(rows);
    v["follow_ups_more"] = json!(more);
    if more {
        v["follow_ups_next"] = json!(format!(
            "fray query --tag parent:{card} --sort id --offset {FOLLOW_UP_LIMIT}"
        ));
    }
    Ok(())
}

/// Events on `card` in (after, until] written by anyone except `reader`.
fn unread_events(
    conn: &Connection,
    card: i64,
    reader: &str,
    after: i64,
    until: i64,
    limit: i64,
) -> Result<Vec<Value>> {
    let mut s = conn.prepare(
        "SELECT seq,ts_ms,actor,op,card_id,payload FROM events WHERE card_id=? AND seq>? AND seq<=? AND actor<>? ORDER BY seq LIMIT ?",
    )?;
    let raw = s
        .query_map(params![card, after, until, reader, limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raw.into_iter().map(|(seq,ts,actor,op,id,payload)|Ok(json!({"seq":seq,"ts_ms":ts,"actor":actor,"op":op,"card_id":id,"payload":serde_json::from_str::<Value>(&payload)?}))).collect()
}

fn events(conn: &Connection, after: i64, card: Option<i64>, limit: i64) -> Result<Vec<Value>> {
    if card.is_none() {
        crate::retention::check_cursor(conn, after)?;
    }
    let sql="SELECT seq,ts_ms,actor,op,card_id,payload FROM events WHERE seq>? AND (? IS NULL OR card_id=?) ORDER BY seq LIMIT ?";
    let mut s = conn.prepare(sql)?;
    let raw = s
        .query_map(params![after, card, card, limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
                r.get::<_, i64>(4)?,
                r.get::<_, String>(5)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    raw.into_iter().map(|(seq,ts,actor,op,id,payload)|Ok(json!({"seq":seq,"ts_ms":ts,"actor":actor,"op":op,"card_id":id,"payload":serde_json::from_str::<Value>(&payload)?}))).collect()
}
/// Bytes of full message text one inbox page may carry for addressed items.
const ADDRESSED_FULL_TEXT_BUDGET: usize = 16_000;

// One read path shared by every consumer; each argument is a distinct,
// caller-chosen dimension, so a parameter struct would only rename them.
#[allow(clippy::too_many_arguments)]
fn inbox(
    conn: &Connection,
    actor: &str,
    after: i64,
    limit: i64,
    fresh: bool,
    selection: InboxSelection<'_>,
    full_budget: usize,
    now: i64,
) -> Result<Value> {
    registered(conn, actor)?;
    if after > 0 {
        crate::retention::check_cursor(conn, after)?;
    }
    let condition=format!("d.agent=?1 AND d.pending_seq>d.ack_seq AND d.pending_seq>?2 AND (?3=0 OR d.pending_seq>d.shown_seq OR d.shown_at_ms<=?4) AND {}",selection.condition());
    let total: i64 = conn.query_row(
        &format!(
            "SELECT count(*) FROM deliveries d JOIN cards c ON c.id=d.card_id WHERE {condition}"
        ),
        params![actor, after, fresh, now - 60000],
        |r| r.get(0),
    )?;
    let mut s=conn.prepare(&format!("SELECT d.card_id,d.pending_seq,d.ack_seq FROM deliveries d JOIN cards c ON c.id=d.card_id WHERE {condition} ORDER BY c.priority ASC,(c.assignee=?1) DESC,d.pending_seq ASC LIMIT ?5"))?;
    let rows = s
        .query_map(params![actor, after, fresh, now - 60000, limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut items = Vec::new();
    let store_id: String =
        conn.query_row("SELECT value FROM meta WHERE key='store_id'", [], |r| {
            r.get(0)
        })?;
    // Full text for what is addressed to you (assigned to you, or replies on
    // a question/task you asked), within a page-wide budget measured in encoded
    // JSON bytes; broadcasts keep previews.
    let mut full_budget = full_budget;
    let encoded = |text: &str| serde_json::to_string(text).map_or(usize::MAX, |t| t.len());
    for (id, pending, ack) in rows {
        let card = get_card(conn, id)?;
        let addressed = card.assignee.as_deref() == Some(actor)
            || (card.author == actor && matches!(card.kind.as_str(), "question" | "task"));
        let count: i64 = conn.query_row(
            "SELECT count(*) FROM events WHERE card_id=? AND op='annotate' AND seq>? AND seq<=?",
            params![id, ack, pending],
            |r| r.get(0),
        )?;
        let mut s=conn.prepare("SELECT seq,actor,json_extract(payload,'$.detail.kind'),json_extract(payload,'$.detail.body'),json_extract(payload,'$.detail.follow_up_id') FROM events WHERE card_id=? AND op='annotate' AND seq>? AND seq<=? ORDER BY seq DESC LIMIT 2")?;
        let raw = s
            .query_map(params![id, ack, pending], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, Option<i64>>(4)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut compact = card.compact(now);
        objection_presentation(conn, id, &mut compact)?;
        let review = crate::review::context(conn, id, false)?;
        if !review.is_null() {
            compact["review"] = review;
        }
        let mut full = false;
        if addressed {
            let text = full_text(conn, &card)?;
            let cost = encoded(&text);
            if cost <= full_budget {
                full_budget -= cost;
                compact["summary"] = json!(text);
                compact["summary_truncated"] = json!(false);
                full = true;
            } else if text != card.summary {
                // The preview is of a head that itself omits the full message.
                compact["summary_truncated"] = json!(true);
            }
            // Otherwise keep compact()'s own flag: a short ask shown whole is
            // not truncated just because full text was not requested.
        }
        let mut annotations = Vec::new();
        for (seq, who, kind, body, follow_up) in raw {
            let cost = encoded(&body);
            let whole = addressed && cost <= full_budget;
            if whole {
                full_budget -= cost;
            }
            let excerpt = if whole {
                body.clone()
            } else {
                clip(&body, 200)
            };
            let authority = (who == OWNER).then_some("owner (unsigned)");
            annotations.push(json!({"seq":seq,"actor":who,"authority":authority,"kind":kind,"excerpt":excerpt,"excerpt_truncated":!whole && body.chars().count()>200,"full":whole,"follow_up_id":follow_up}));
        }
        items.push(json!({"card":compact,"addressed":addressed,"full_text":full,"through_seq":pending,"ack_seq":ack,"receipt":{"store_id":store_id,"agent":actor,"id":id,"through_seq":pending},"annotations":annotations,"annotation_count":count,"annotations_omitted":(count-2).max(0)}));
    }
    Ok(
        json!({"agent":actor,"selection":selection.mode,"card_ids":selection.card_ids,"addressed_to_me":selection.addressed_to_me,"unresolved":selection.unresolved,"kinds":selection.kinds,"min_priority":selection.min_priority,"cursor":highwater(conn)?,"items":items,"total":total,"more":(items.len() as i64)<total,"read_is_not_ack":true}),
    )
}
fn roster(conn: &Connection, now: i64, limit: i64, all: bool) -> Result<Value> {
    // Never-joined recipients with no open mail are historical routing records,
    // not peers waiting to join. Preserve them for history and explicit --all.
    let mut s=conn.prepare("SELECT name,role,topics,enabled,last_seen_ms FROM agents a WHERE ?1 OR enabled=1 OR last_seen_ms>0 OR EXISTS(SELECT 1 FROM cards c WHERE c.assignee=a.name AND c.status IN ('open','active','blocked')) ORDER BY enabled DESC,last_seen_ms DESC,name LIMIT ?2")?;
    let raw = s
        .query_map(params![all, limit + 1], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, i64>(4)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let more = raw.len() > limit as usize;
    let mut items = Vec::new();
    for (name, role, topics, enabled, last) in raw.into_iter().take(limit as usize) {
        let controller = conn.query_row("SELECT c.run_id,c.state,c.updated_ms,c.reason,d.detail FROM controllers c LEFT JOIN controller_details d ON d.agent=c.agent AND d.run_id=c.run_id WHERE c.agent=?",[&name],|r|{
            let state:String=r.get(1)?;
            let updated:i64=r.get(2)?;
            let active=matches!(state.as_str(),"waiting"|"running");
            let detail=r.get::<_,Option<String>>(4)?.and_then(|d|serde_json::from_str::<Value>(&d).ok());
            Ok(json!({"run_id":r.get::<_,String>(0)?,"state":if active && now-updated>=120000 {"stale"} else {&state},"last_state":state,"live":enabled && active && now-updated<120000,"updated_ms":updated,"reason":r.get::<_,Option<String>>(3)?,"detail":detail}))
        }).optional()?;
        let listener = crate::attention::listener_status(conn, &name, enabled, now)?;
        items.push(json!({"name":name,"role":role,"topics":serde_json::from_str::<Value>(&topics)?,"enabled":enabled,"recently_seen":enabled && (now-last<120000 || agent_waiting(conn,&name,now)?),"pending":!enabled && last==0,"last_seen_ms":last,"controller":controller,"listener":listener,"session":session_status(conn,&name,now)?,"status":agent_status_text(conn,&name)?,"lanes":held_lane_paths(conn,&name)?,"reachability":reachability(conn,&name,now)?.as_str(),"keepalive":crate::keepalive::brief(conn,&name,now)?}));
    }
    Ok(json!({"items":items,"more":more}))
}
pub(crate) fn get_card_pub(conn: &Connection, id: i64) -> Result<Card> {
    get_card(conn, id)
}

/// `respond_within_ms`, if given, as an absolute deadline on the daemon's
/// clock (no-silent-stalls R4): one minute to thirty days.
fn respond_by(a: &Value, now: i64) -> Result<Option<i64>> {
    a.get("respond_within_ms")
        .map(|_| bounded(a, "respond_within_ms", 0, 60_000, 30 * 86_400_000).map(|ms| now + ms))
        .transpose()
}

/// An ask's deadline: the latest `respond_by_ms` its author recorded, in the
/// creation event or a later reply, with that event's sequence. Kept in
/// events, never in tags, so nobody can patch it away; reassignment keeps it.
pub(crate) fn ask_deadline(
    conn: &Connection,
    card: i64,
    author: &str,
) -> Result<Option<(i64, i64)>> {
    Ok(conn
        .query_row(
            "SELECT json_extract(payload,'$.detail.respond_by_ms'),seq FROM events WHERE card_id=? AND actor=? AND op IN ('post','annotate') AND json_extract(payload,'$.detail.respond_by_ms') IS NOT NULL ORDER BY seq DESC LIMIT 1",
            params![card, author],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// Whether an open ask is overdue: past its deadline with no answer since
/// the deadline was set. An answer is an annotation by its addressee; with no
/// addressee other than the asker, any annotation by someone else. Whether
/// it is an ask comes from its creation event, so changing the kind, status
/// (short of closing it) or assignee cannot hide it (review of 9fbddc4, #81).
pub(crate) fn ask_overdue(conn: &Connection, c: &Card, now: i64) -> Result<Option<i64>> {
    if c.terminal() {
        return Ok(None);
    }
    let asked: bool = conn
        .query_row(
            "SELECT json_extract(payload,'$.card.kind')='question' FROM events WHERE card_id=? ORDER BY seq LIMIT 1",
            [c.id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false);
    if !asked {
        return Ok(None);
    }
    let Some((by, seq)) = ask_deadline(conn, c.id, &c.author)? else {
        return Ok(None);
    };
    if now < by {
        return Ok(None);
    }
    let answered: bool = match c.assignee.as_deref().filter(|a| *a != c.author) {
        Some(to) => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE card_id=? AND actor=? AND op='annotate' AND seq>?)",
            params![c.id, to, seq],
            |r| r.get(0),
        )?,
        None => conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE card_id=? AND actor<>? AND op='annotate' AND seq>?)",
            params![c.id, c.author, seq],
            |r| r.get(0),
        )?,
    };
    Ok((!answered).then_some(by))
}

/// R3 timings: an addressee nothing can wake gets this long before its
/// request counts as stuck; a Mote request is overdue after Mote's own
/// default request horizon; an unknown-recipient row unseen this long is gone;
/// a board with no tick this long is "not ticking".
/// The grace period; FRAY_STUCK_GRACE_MS (0 to one day) overrides the
/// 15-minute default, for tuning and tests.
pub fn stuck_grace_ms() -> i64 {
    std::env::var("FRAY_STUCK_GRACE_MS")
        .ok()
        .and_then(|v| v.parse::<i64>().ok())
        .filter(|v| (0..=86_400_000).contains(v))
        .unwrap_or(15 * 60_000)
}
const MOTE_HORIZON_MS: i64 = 60 * 60_000;
const UNKNOWN_FRESH_MS: i64 = 2 * 60 * 60_000;
const TICK_STALE_MS: i64 = 5 * 60_000;

/// When a runner last ticked, and whether that is recent: within twice the
/// runners' interval, and never less than five minutes.
fn tick_state(conn: &Connection, now: i64) -> Result<(Option<i64>, bool)> {
    let get = |key: &str| -> Result<Option<i64>> {
        Ok(conn
            .query_row("SELECT value FROM meta WHERE key=?", [key], |r| {
                r.get::<_, String>(0)
            })
            .optional()?
            .and_then(|v| v.parse::<i64>().ok()))
    };
    let last = get("last_tick_ms")?;
    let window = get("tick_interval_ms")?.map_or(TICK_STALE_MS, |i| (2 * i).max(TICK_STALE_MS));
    Ok((last, last.is_some_and(|t| now - t < window)))
}

/// Requests that are stuck now (R3): open asks whose addressee nothing can
/// wake and who was never shown them, after a grace period; asks past their
/// deadline (R4) or, for Mote requests, past Mote's horizon from when it was
/// sent; and open Mote requests to actors who never joined. Whether a card is
/// an ask comes from its creation event, so a patch cannot hide it (#86).
/// Escalation cards are never stuck. Oldest first.
pub(crate) fn stuck_set(conn: &Connection, now: i64) -> Result<Vec<Value>> {
    let mut out = Vec::new();
    let mut s = conn.prepare(&format!(
        "SELECT c.id FROM cards c WHERE {ACTIVE} AND c.author<>?2 AND (c.assignee IS NULL OR c.assignee<>?1) AND ((c.author=?3 AND json_extract((SELECT e.payload FROM events e WHERE e.card_id=c.id ORDER BY e.seq LIMIT 1),'$.card.kind')='question') OR (c.assignee IS NOT NULL AND c.assignee<>c.author AND json_extract((SELECT e.payload FROM events e WHERE e.card_id=c.id ORDER BY e.seq LIMIT 1),'$.card.kind')='question') OR EXISTS(SELECT 1 FROM events e WHERE e.card_id=c.id AND e.actor=c.author AND json_extract(e.payload,'$.detail.respond_by_ms') IS NOT NULL)) ORDER BY c.id LIMIT 1000"
    ))?;
    let ids = s
        .query_map(params![OWNER, ESCALATION, MOTE], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in ids {
        out.extend(card_stuck(conn, id, now)?);
    }
    out.extend(unknown_stuck(conn, now, None)?);
    Ok(out)
}

/// One card's stuck entries (empty when it is not stuck). The subject names
/// the addressee, so a re-route to someone else is a new stuck request.
fn card_stuck(conn: &Connection, id: i64, now: i64) -> Result<Vec<Value>> {
    let c = get_card(conn, id)?;
    if c.terminal() || c.author == ESCALATION || c.assignee.as_deref() == Some(OWNER) {
        return Ok(Vec::new());
    }
    let (first_kind, sent_ms, from): (Option<String>, Option<i64>, Option<String>) = conn
        .query_row(
            "SELECT json_extract(payload,'$.card.kind'),json_extract(payload,'$.detail.sent_ms'),json_extract(payload,'$.detail.from') FROM events WHERE card_id=? ORDER BY seq LIMIT 1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?
        .unwrap_or((None, None, None));
    let asked = first_kind.as_deref() == Some("question");
    // Overdue applies with or without an addressee (R4); unreachable needs
    // someone other than the asker to be unreachable.
    let to = c
        .assignee
        .clone()
        .filter(|a| *a != c.author)
        .unwrap_or_default();
    let reach = if to.is_empty() {
        Reach::Absent
    } else {
        reachability(conn, &to, now)?
    };
    // A Mote request card is authored by `mote`; its asker is the sender.
    let requester = if c.author == MOTE {
        from.unwrap_or_else(|| MOTE.to_owned())
    } else {
        c.author.clone()
    };
    let since = sent_ms.unwrap_or(c.created_ms).min(c.created_ms);
    let mut reasons = Vec::new();
    if c.author == MOTE {
        if asked && now - since >= MOTE_HORIZON_MS {
            reasons.push("overdue");
        }
    } else if ask_overdue(conn, &c, now)?.is_some() {
        reasons.push("overdue");
    }
    // Shown to the addressee, or answered by it, proves it arrived; then only
    // deadlines apply (#87).
    let shown: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM deliveries WHERE agent=?1 AND card_id=?2 AND shown_at_ms>0) OR EXISTS(SELECT 1 FROM presented_items i JOIN presented_batches b ON b.batch=i.batch WHERE b.agent=?1 AND i.card_id=?2) OR EXISTS(SELECT 1 FROM events WHERE card_id=?2 AND actor=?1 AND op='annotate')",
        params![to, id],
        |r| r.get(0),
    )?;
    if !to.is_empty()
        && asked
        && reach != Reach::Wakeable
        && !shown
        && now - since >= stuck_grace_ms()
    {
        reasons.push("unreachable");
    }
    let addressee = if to.is_empty() {
        "nobody".to_owned()
    } else {
        to.clone()
    };
    // Why a keepalive serving the addressee is not answering, if it is not.
    let keepalive_state = if reasons.is_empty() || to.is_empty() {
        Value::Null
    } else {
        crate::keepalive::brief(conn, &to, now)?["state"].clone()
    };
    Ok(reasons
        .into_iter()
        .map(|reason| {
            json!({"subject":format!("card:{id}:{addressee}"),"card_id":id,"reason":reason,
                "addressee":addressee,"requester":requester,"title":c.title,"rev":c.rev,
                "reachability":reach.as_str(),"age_min":(now-since)/60_000,
                "keepalive":keepalive_state.clone()})
        })
        .collect())
}

/// Open Mote requests to actors not on the board, past the grace period;
/// one request when `only` names it.
fn unknown_stuck(conn: &Connection, now: i64, only: Option<&str>) -> Result<Vec<Value>> {
    let mut s = conn.prepare(
        "SELECT msg_id,recipient,sender,first_seen_ms FROM mote_requests_unknown WHERE last_seen_ms>?1 AND first_seen_ms<=?2 AND (?3 IS NULL OR msg_id=?3) ORDER BY first_seen_ms LIMIT 200",
    )?;
    let rows = s
        .query_map(
            params![now - UNKNOWN_FRESH_MS, now - stuck_grace_ms(), only],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(msg, to, from, first)| {
            json!({"subject":format!("mreq:{msg}:{to}"),"msg_id":msg,"reason":"unreachable",
                "addressee":to,"requester":from,"title":format!("Mote request {msg}"),
                "reachability":"not on this board","age_min":(now-first)/60_000})
        })
        .collect())
}

/// Whether `subject` is still stuck for `reason`, checked directly, so a
/// request outside a scan window is never settled by mistake (#83).
fn still_stuck(conn: &Connection, subject: &str, reason: &str, now: i64) -> Result<bool> {
    let live = if let Some(rest) = subject.strip_prefix("card:") {
        match rest.split(':').next().and_then(|id| id.parse::<i64>().ok()) {
            Some(id) => card_stuck(conn, id, now)?,
            None => Vec::new(),
        }
    } else if let Some(rest) = subject.strip_prefix("mreq:") {
        unknown_stuck(conn, now, rest.split(':').next())?
    } else {
        Vec::new()
    };
    Ok(live
        .iter()
        .any(|i| i["subject"] == subject && i["reason"] == reason))
}

/// After a steward closes its escalation while the request is still stuck,
/// it is reminded this long after the escalation it closed (#83).
const ESCALATION_REMIND_MS: i64 = 60 * 60_000;
/// New escalation cards per tick; the rest follow on later ticks.
const ESCALATIONS_PER_TICK: usize = 20;

/// One R3 tick (see `stuck_set`): escalations for present stewards, at most
/// one open per request, reason and steward; a reminder when a steward closed
/// theirs and it is still stuck an hour later; settled ones for requests
/// that have cleared, each checked directly.
fn escalate_tick(conn: &Connection, interval_ms: Option<i64>, now: i64) -> Result<Value> {
    conn.execute(
        "INSERT INTO meta(key,value) VALUES('last_tick_ms',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        [now.to_string()],
    )?;
    if let Some(ms) = interval_ms {
        conn.execute(
            "INSERT INTO meta(key,value) VALUES('tick_interval_ms',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
            [ms.to_string()],
        )?;
    }
    let stuck = stuck_set(conn, now)?;
    conn.execute(
        "INSERT OR IGNORE INTO agents(name,role,topics,enabled,joined_ms,last_seen_ms) VALUES(?,'worker','[]',0,?,0)",
        params![ESCALATION, now],
    )?;
    let mut st = conn.prepare(
        "SELECT name FROM agents WHERE enabled=1 AND role='steward' AND name<>? ORDER BY name",
    )?;
    let stewards = st
        .query_map([ESCALATION], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut present = Vec::new();
    for name in stewards {
        if reachability(conn, &name, now)? != Reach::Absent {
            present.push(name);
        }
    }
    let (mut created, mut settled, mut deferred) = (Vec::new(), Vec::new(), 0);
    for item in &stuck {
        let subject = item["subject"].as_str().unwrap_or("");
        let reason = item["reason"].as_str().unwrap_or("");
        let to = item["addressee"].as_str().unwrap_or("");
        let from = item["requester"].as_str().unwrap_or("");
        for steward in present.iter().filter(|s| *s != to && *s != from) {
            let open: Option<(i64, i64, i64)> = conn
                .query_row(
                    "SELECT id,card_id,created_ms FROM escalations WHERE subject=? AND reason=? AND recipient=? AND settled_ms IS NULL ORDER BY id DESC LIMIT 1",
                    params![subject, reason, steward],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            if let Some((_, card_id, at)) = open {
                let closed = get_card(conn, card_id)?.terminal();
                if !(closed && now - at >= ESCALATION_REMIND_MS) {
                    continue;
                }
            }
            if created.len() >= ESCALATIONS_PER_TICK {
                deferred += 1;
                continue;
            }
            if let Some((eid, _, _)) = open {
                conn.execute(
                    "UPDATE escalations SET settled_ms=? WHERE id=?",
                    params![now, eid],
                )?;
            }
            let id = escalation_card(conn, item, steward, open.is_some(), now)?;
            conn.execute(
                "INSERT INTO escalations(subject,reason,recipient,card_id,created_ms) VALUES(?,?,?,?,?)",
                params![subject, reason, steward, id, now],
            )?;
            created.push(id);
        }
    }
    let mut st =
        conn.prepare("SELECT id,subject,reason,card_id FROM escalations WHERE settled_ms IS NULL")?;
    let open = st
        .query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (eid, subject, reason, card_id) in open {
        if still_stuck(conn, &subject, &reason, now)? {
            continue;
        }
        conn.execute(
            "UPDATE escalations SET settled_ms=? WHERE id=?",
            params![now, eid],
        )?;
        let c = get_card(conn, card_id)?;
        if !c.terminal() {
            conn.execute(
                "UPDATE cards SET rev=rev+1,status='resolved',updated_ms=? WHERE id=?",
                params![now, card_id],
            )?;
            emit(
                conn,
                ESCALATION,
                "patch",
                card_id,
                json!({"note":"no longer stuck"}),
                now,
                true,
            )?;
        }
        settled.push(card_id);
    }
    Ok(
        json!({"stuck":stuck.len(),"created":created,"settled":settled,"deferred":deferred,"stewards":present}),
    )
}

fn escalation_card(
    conn: &Connection,
    item: &Value,
    steward: &str,
    reminder: bool,
    now: i64,
) -> Result<i64> {
    let to = item["addressee"].as_str().unwrap_or("");
    let from = item["requester"].as_str().unwrap_or("");
    let actions = match item["card_id"].as_i64() {
        Some(id) => format!(
            "Re-route it with `fray patch {id} --expect {} --assignee NAME`, answer it yourself, or queue it for the owner with `fray ask-owner --card {id} ...`.",
            item["rev"]
        ),
        None => format!(
            "{to} has not joined this board. Ask them another way, answer it in Mote, or tell {from} (`mote msg send --to {from} ...`)."
        ),
    };
    let why = if item["reason"] == "overdue" {
        "is past its deadline with no answer from its addressee"
    } else {
        "is addressed to someone nothing can wake, who has not seen it"
    };
    let card = create_card(
        conn,
        ESCALATION,
        &json!({"kind":"note","topic":format!("@{steward}"),"priority":1,
            "title":clip(&format!("{}Stuck ({}): {}", if reminder { "Still " } else { "" }, item["reason"].as_str().unwrap_or(""), item["title"].as_str().unwrap_or("")), 160),
            "summary":clip(&format!("{from}'s request to {to} ({}, {} min old; {to} is {}) {why}. Nothing re-routes it automatically. {actions}", item["subject"].as_str().unwrap_or(""), item["age_min"], item["reachability"].as_str().unwrap_or("?")), 2000),
            "tags":["escalation", format!("escalation:{}", item["subject"].as_str().unwrap_or(""))],
            "assignee":steward}),
        json!({"escalation":item}),
        now,
    )?;
    Ok(card["card"]["id"].as_i64().unwrap_or(0))
}

/// The strict "armed" test: `(enabled, listening, armed)`. A live drive
/// controller, or a live, unexpired, unfiltered listener whose host wake
/// mechanism is declared (managed, native-monitor or background-completion).
fn wake_state(conn: &Connection, actor: &str, now: i64) -> Result<(bool, Value, bool)> {
    let enabled: bool = conn
        .query_row("SELECT enabled FROM agents WHERE name=?", [actor], |r| {
            r.get(0)
        })
        .optional()?
        .unwrap_or(false);
    // A keepalive paused by its budget, or asked to stop, takes no further
    // turns: it does not wake. Nor does one deferred to its busy terminal
    // (a fresh busy mark from the session it serves), so R3 escalates.
    let controller = conn.query_row("SELECT c.state,c.updated_ms FROM controllers c LEFT JOIN controller_details d ON d.agent=c.agent AND d.run_id=c.run_id WHERE c.agent=?1 AND json_extract(d.detail,'$.keepalive.paused') IS NULL AND NOT EXISTS(SELECT 1 FROM keepalives k WHERE k.agent=c.agent AND k.stop_requested=1 AND k.session=json_extract(d.detail,'$.keepalive.session')) AND NOT EXISTS(SELECT 1 FROM keepalives k JOIN terminal_turns t ON t.agent=k.agent AND t.session=k.companion WHERE k.agent=c.agent AND k.session=json_extract(d.detail,'$.keepalive.session') AND t.busy_since_ms IS NOT NULL AND t.last_hook_ms>?2)", params![actor, now - crate::keepalive::BUSY_STALE_MS], |r| {
        let state: String = r.get(0)?;
        let updated: i64 = r.get(1)?;
        Ok(json!({"state":state,"live":enabled && matches!(state.as_str(),"waiting"|"running") && now-updated<120000}))
    }).optional()?;
    let listener =
        crate::attention::listener_status(conn, actor, enabled, now)?.unwrap_or(Value::Null);
    let listening = crate::diagnostics::listening(
        &json!({"enabled":enabled,"controller":controller,"listener":listener}),
        now,
    );
    // A narrowly filtered stream may be alive but cannot promise to deliver all
    // future answers. Be conservative rather than call that session armed.
    let filters = &listener["selection"];
    let unfiltered = ["card_ids", "kinds"]
        .iter()
        .all(|key| filters[*key].as_array().is_none_or(Vec::is_empty))
        && filters["min_priority"].is_null()
        && filters["addressed_to_me"] != true
        && filters["unresolved"] != true;
    let armed = listening["live"] == true
        && listening["activation_expired"] != true
        && matches!(
            listening["activation"].as_str(),
            Some("managed" | "native-monitor" | "background-completion")
        )
        && (listening["transport"] == "managed-runner" || unfiltered);
    Ok((enabled, listening, armed))
}

/// The command that arms a wake for `actor`, the one source for `fray arm`
/// and idle readiness. `host` is how the host wakes: "native-monitor" (a
/// host monitor that runs the stream, e.g. Claude Code's Monitor tool) or
/// "background-completion" (a host that resumes when a background command
/// finishes, so the listener returns after one delivery). `expires_ms` is
/// the absolute Unix time the host mechanism stops.
pub fn arm_command(actor: &str, host: Option<&str>, expires_ms: Option<i64>) -> String {
    let mut cmd =
        format!("fray --as {actor} watch --attention --notification --selection involved");
    match host {
        Some("background-completion") => cmd.push_str(" --once --activation background-completion"),
        Some(mode) => cmd.push_str(&format!(" --reconnect --activation {mode}")),
        None => {}
    }
    if let (Some(_), Some(ms)) = (host, expires_ms) {
        cmd.push_str(&format!(" --activation-expires-ms {ms}"));
    }
    cmd
}

/// Why nothing armed covers `actor` now: its declared activation expired, a
/// one-shot listener returned after delivering (to be rearmed, not a lapse),
/// or nothing was armed.
fn unarmed_reason(listening: &Value, now: i64) -> (&'static str, String) {
    let expires = listening["activation_expires_ms"].as_i64();
    if listening["activation_expired"] == true {
        let ago = (now - expires.unwrap_or(now)).max(0) / 60_000;
        return (
            "lapsed",
            format!("your wake lapsed: its declared activation expired {ago} min ago"),
        );
    }
    if listening["once"] == true && listening["live"] != true {
        return (
            "rearm",
            "your one-shot listener has returned (after a delivery, a timeout or the end of its session); rearm it when you finish handling"
                .into(),
        );
    }
    ("unarmed", "nothing is armed to wake you".into())
}

fn idle_readiness(conn: &Connection, actor: &str, now: i64) -> Result<Value> {
    let (enabled, listening, armed) = wake_state(conn, actor, now)?;
    let outgoing: i64 = conn.query_row(&format!("SELECT count(*) FROM cards c WHERE {ACTIVE} AND json_extract((SELECT e.payload FROM events e WHERE e.card_id=c.id ORDER BY e.seq LIMIT 1),'$.card.kind')='question' AND c.author=?1 AND c.assignee IS NOT NULL AND c.assignee<>?1 AND NOT EXISTS(SELECT 1 FROM muted_cards m WHERE m.agent=?1 AND m.card_id=c.id)"), [actor], |r| r.get(0))?;
    let mut out = json!({"enabled":enabled,"open_requests_awaiting_others":outgoing,"armed":armed,"listening":listening,"model_response_guaranteed":false});
    if enabled && outgoing > 0 && !armed {
        out["warning"] = json!(format!("{outgoing} open requests awaiting others; no armed listener covering their replies. A connected transport or a hook alone cannot wake an idle host."));
        // `fray arm` prints the full command with a declared activation and
        // expiry; the bare watch alone would not count as armed.
        out["arm_command"] = json!(format!("fray --as {actor} arm"));
        out["arm_guidance"] = json!("Run `fray arm` (or `fray arm --host background-completion`) and start the command it prints through the host's supported notification tool, with the same lifetime. In hosts without idle wake support, use an explicit fray wait --timeout none while the session is active; do not promise automatic wake after returning control.");
    }
    // Asks addressed to this agent that nothing will wake it for once its
    // turn ends (no-silent-stalls R5). The next hook context says so first.
    let incoming: i64 = conn.query_row(
        // Requestness comes from creation. A Mote card remains open until
        // Mote says so; a local annotation is never a substitute response.
        &format!("SELECT count(*) FROM cards c WHERE {ACTIVE} AND json_extract((SELECT e.payload FROM events e WHERE e.card_id=c.id ORDER BY e.seq LIMIT 1),'$.card.kind')='question' AND c.assignee=?1 AND c.author<>?1 AND (c.author=?2 AND EXISTS(SELECT 1 FROM mote_requests m WHERE m.card_id=c.id AND m.state='open') OR c.author<>?2 AND NOT EXISTS(SELECT 1 FROM events e WHERE e.card_id=c.id AND e.actor=?1 AND e.op='annotate'))"),
        params![actor, MOTE],
        |r| r.get(0),
    )?;
    // Wakeable by any means (R1), including an unfiltered wait in progress,
    // is covered; the hint rearms the way this host last declared it wakes.
    if enabled && incoming > 0 && reachability(conn, actor, now)? != Reach::Wakeable {
        let (kind, why) = unarmed_reason(&listening, now);
        let arm = if listening["activation"] == "background-completion" {
            format!("fray --as {actor} arm --host background-completion")
        } else {
            format!("fray --as {actor} arm")
        };
        out["lapse"] = json!({
            "kind": kind,
            "open_asks_to_you": incoming,
            "message": format!("{incoming} open ask(s) are addressed to you and {why}. When this turn ends nothing will bring you back for them. Run `{arm}` and start the command it prints through your host's monitor, or answer them now (an ask stays open until its author resolves it)."),
            "arm": arm,
        });
    }
    Ok(out)
}

fn brief(conn: &Connection, actor: &str, budget: usize, now: i64) -> Result<Value> {
    let p = || vec![SqlValue::Text(actor.into()); 4];
    let context = select_page(
        conn,
        &format!("{ACTIVE} AND (c.pinned=1 OR c.kind IN ('goal','decision')) AND {RELEVANT}"),
        p(),
        "c.pinned DESC,c.priority,c.id",
        6,
        0,
        now,
    )?;
    let mut work_p = p();
    work_p.push(now.into());
    work_p.push(actor.to_string().into());
    let work=select_page(conn,&format!("{ACTIVE} AND {RELEVANT} AND c.kind IN ('task','question') AND c.status IN ('open','active') AND (c.lease_owner IS NULL OR c.lease_until_ms<=?) AND (c.assignee IS NULL OR c.assignee=?)"),work_p,"c.priority,c.created_ms,c.id",6,0,now)?;
    let blockers = select_page(
        conn,
        &format!("c.status='blocked' AND {RELEVANT}"),
        p(),
        "c.priority,c.created_ms,c.id",
        4,
        0,
        now,
    )?;
    let claimed = select_page(
        conn,
        &format!("{ACTIVE} AND c.lease_owner=? AND c.lease_until_ms>?"),
        vec![actor.to_string().into(), now.into()],
        "c.priority,c.id",
        6,
        0,
        now,
    )?;
    let store_id: String =
        conn.query_row("SELECT value FROM meta WHERE key='store_id'", [], |r| {
            r.get(0)
        })?;
    let mut out = json!({"store_id":store_id,"agent":actor,"cursor":highwater(conn)?,"time_ms":now,"attention":inbox(conn,actor,0,8,false,"all".into(),budget/4,now)?,"context":context,"blockers":blockers,"claimed":claimed,"available":work,"agents":roster(conn,now,12,false)?,"budget_bytes":budget,"budget_truncated":false,"note":"Current heads, not a historical digest. 'more' means additional live items exist. Attention acknowledgment does not resolve work."});
    out["idle_readiness"] = idle_readiness(conn, actor, now)?;
    // Stewards see when escalation is pull-only (R3): stuck requests exist
    // and no runner has ticked recently. Read only; brief never syncs.
    let steward: bool = conn
        .query_row(
            "SELECT role='steward' FROM agents WHERE name=?",
            [actor],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false);
    if steward && !tick_state(conn, now)?.1 {
        let stuck = stuck_set(conn, now)?;
        if !stuck.is_empty() {
            out["escalation_note"] = json!(format!(
                "{} stuck request(s), and no runner is ticking, so nobody is being escalated to: they are seen only when someone reads them. `fray stuck` and `fray owner review` list them; a `fray watch --attention` or `fray drive` (not --once) keeps escalation running.",
                stuck.len()
            ));
        }
    }
    // The asker's overdue asks (R4), oldest deadline first, at most 5.
    let mut st = conn.prepare(&format!(
        "SELECT c.id FROM cards c WHERE {ACTIVE} AND c.author=?1 AND EXISTS(SELECT 1 FROM events e WHERE e.card_id=c.id AND e.actor=?1 AND json_extract(e.payload,'$.detail.respond_by_ms') IS NOT NULL) ORDER BY c.id LIMIT 200"
    ))?;
    let ids = st
        .query_map([actor], |r| r.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut overdue = Vec::new();
    for id in ids {
        let c = get_card(conn, id)?;
        if let Some(by) = ask_overdue(conn, &c, now)? {
            overdue.push(json!({"id":id,"title":c.title,"assignee":c.assignee,
                "due_ms":by,"overdue_min":(now-by)/60_000}));
        }
    }
    overdue.sort_by_key(|o| o["due_ms"].as_i64());
    if !overdue.is_empty() {
        let total = overdue.len();
        overdue.truncate(5);
        out["overdue_asks"] = json!({"total":total,"more":total > 5,"items":overdue});
    }
    // The byte budget is hard, not a promise based on an estimated token count.
    while serde_json::to_vec(&out)?.len() > budget {
        let mut removed = false;
        for key in [
            "agents",
            "available",
            "claimed",
            "context",
            "blockers",
            "attention",
            "overdue_asks",
        ] {
            if let Some(items) = out[key]["items"].as_array_mut() {
                if items.pop().is_some() {
                    out[key]["more"] = json!(true);
                    out[key]["next_offset"] =
                        json!(out[key]["items"].as_array().map_or(0, Vec::len));
                    removed = true;
                    break;
                }
            }
        }
        out["budget_truncated"] = json!(true);
        if !removed {
            // Readiness metadata can itself exhaust a small budget (e.g. with
            // the maximum agent name). Keep the warning and arm command while
            // dropping optional explanation/diagnostics after all rows are gone.
            for key in ["arm_guidance", "listening"] {
                if out["idle_readiness"]
                    .as_object_mut()
                    .is_some_and(|readiness| readiness.remove(key).is_some())
                {
                    out["idle_readiness"]["details_omitted"] = json!(true);
                    removed = true;
                    break;
                }
            }
        }
        if !removed {
            break;
        }
    }
    Ok(out)
}
