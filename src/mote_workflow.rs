//! Recoverable client workflows. Journals contain requests and observations;
//! ownership and acceptance remain in Mote. No external process runs in a
//! daemon transaction.
use crate::sha256;
use crate::{client, model::*, mote};
use fs2::FileExt;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    os::unix::fs::OpenOptionsExt,
    path::{Path, PathBuf},
};

pub fn digest(value: &Value) -> Result<String> {
    let mut hash = sha256::Sha256::new();
    hash.update(&serde_json::to_vec(value)?);
    Ok(sha256::hex(hash.finalize()))
}

pub fn operation(conn: &Connection, actor: &str, key: &str) -> Result<Value> {
    type OperationRow = (String, String, String, i64, String, String, Option<String>);
    let row: Option<OperationRow> = conn.query_row(
        "SELECT kind,payload,digest,rev,state,observations,result FROM mote_operations WHERE actor=? AND key=?",
        params![actor,key], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)),
    ).optional()?;
    let (kind, payload, digest, rev, state, observations, result) =
        row.ok_or_else(|| Error::new("not_found", "no operation with this actor/key"))?;
    Ok(
        json!({"actor":actor,"key":key,"kind":kind,"payload":serde_json::from_str::<Value>(&payload)?,"digest":digest,"rev":rev,"state":state,"observations":serde_json::from_str::<Value>(&observations)?,"result":result.map(|s|serde_json::from_str::<Value>(&s)).transpose()?}),
    )
}

pub fn prepare(
    conn: &Connection,
    actor: &str,
    session: Option<&str>,
    args: &Value,
    now: i64,
) -> Result<Value> {
    check_fields(args, &["key", "kind", "payload"])?;
    let key = string(args, "key")?;
    text(key, "key", 128, false)?;
    let kind = string(args, "kind")?;
    text(kind, "kind", 80, false)?;
    let payload = &args["payload"];
    if !payload.is_object() || serde_json::to_vec(payload)?.len() > 60_000 {
        return Err(Error::invalid(
            "operation payload must be an object of at most 60000 bytes",
        ));
    }
    let store_id = string(payload, "store_id")?;
    let bound: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key='mote_store_id'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if bound.as_deref() != Some(store_id) {
        return Err(Error::new(
            "mote_store_mismatch",
            "operation store is not the board's bound Mote store",
        ));
    }
    let digest = digest(payload)?;
    match operation(conn, actor, key) {
        Ok(prior) => {
            if prior["kind"] != kind || prior["payload"] != *payload {
                return Err(Error::new(
                    "idempotency_conflict",
                    "operation key already names a different exact request",
                ));
            }
            return Ok(json!({"operation":prior,"new":false}));
        }
        Err(e) if e.code == "not_found" => {}
        Err(e) => return Err(e),
    }
    if kind == "review" {
        validate_review_payload(conn, actor, payload)?;
    }
    conn.execute("INSERT INTO mote_operations(actor,key,kind,payload,digest,session,rev,state,observations,created_ms,updated_ms) VALUES(?,?,?,?,?,?,0,'pending','[]',?,?)", params![actor,key,kind,serde_json::to_string(payload)?,digest,session,now,now])?;
    Ok(json!({"operation":operation(conn,actor,key)?,"new":true}))
}

pub fn checkpoint(conn: &Connection, actor: &str, args: &Value, now: i64) -> Result<Value> {
    check_fields(args, &["key", "expect", "step", "observation"])?;
    let key = string(args, "key")?;
    let mut op = operation(conn, actor, key)?;
    if op["rev"] != integer(args, "expect")? || op["state"] != "pending" {
        return Err(Error::new(
            "conflict",
            "operation changed; reread before checkpointing",
        ));
    }
    let step = string(args, "step")?;
    text(step, "step", 100, false)?;
    let observations = op["observations"].as_array_mut().expect("journal array");
    observations.push(json!({"step":step,"at_ms":now,"observation":args["observation"]}));
    if serde_json::to_vec(observations)?.len() > 500_000 {
        return Err(Error::invalid(
            "journal observation limit reached; inspect existing receipts",
        ));
    }
    conn.execute(
        "UPDATE mote_operations SET rev=rev+1,observations=?,updated_ms=? WHERE actor=? AND key=?",
        params![serde_json::to_string(observations)?, now, actor, key],
    )?;
    Ok(json!({"operation":operation(conn,actor,key)?}))
}

pub fn finalize(conn: &Connection, actor: &str, args: &Value, now: i64) -> Result<Value> {
    check_fields(args, &["key", "expect", "result"])?;
    let key = string(args, "key")?;
    let op = operation(conn, actor, key)?;
    if op["state"] == "completed" {
        return Ok(json!({"operation":op,"already_completed":true}));
    }
    if op["rev"] != integer(args, "expect")? {
        return Err(Error::new(
            "conflict",
            "operation changed; reread before finalizing",
        ));
    }
    if !args["result"].is_object() {
        return Err(Error::invalid("operation result must be an object"));
    }
    let mut result = args["result"].clone();
    if op["kind"] == "review" {
        if result["confirmed"] != true || result["receipt"]["exit_code"] != 0 {
            return Err(Error::new(
                "mote_unconfirmed",
                "a failed/unknown Mote outcome cannot create a Fray verdict",
            ));
        }
        result["fray"] = record_review(conn, actor, &op["payload"], &result, now)?;
    }
    conn.execute("UPDATE mote_operations SET rev=rev+1,state='completed',result=?,updated_ms=? WHERE actor=? AND key=?",params![serde_json::to_string(&result)?,now,actor,key])?;
    Ok(json!({"operation":operation(conn,actor,key)?}))
}

pub fn review_binding(conn: &Connection, id: i64) -> Result<Option<Value>> {
    Ok(conn.query_row("SELECT store_id,candidate_id,proposer,observed_candidate,predecessor_card,successor_card FROM mote_review_subjects WHERE card_id=?",[id],|r|Ok(json!({"store_id":r.get::<_,String>(0)?,"candidate_id":r.get::<_,String>(1)?,"proposer":r.get::<_,String>(2)?,"observed_candidate":serde_json::from_str::<Value>(&r.get::<_,String>(3)?).unwrap_or(Value::Null),"predecessor_card":r.get::<_,Option<i64>>(4)?,"successor_card":r.get::<_,Option<i64>>(5)?}))).optional()?)
}

pub fn review_request(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    let a = &req.args;
    check_fields(
        a,
        &[
            "to",
            "title",
            "body",
            "candidate_view",
            "store_id",
            "predecessor_card",
        ],
    )?;
    let view = &a["candidate_view"];
    let id = string(view, "candidate_id")?;
    let store_id = string(a, "store_id")?;
    if view["identity"]["store_id"] != store_id || view["phase"]["value"] != "pending" {
        return Err(Error::invalid(
            "Mote review requires an observed pending candidate in the bound store",
        ));
    }
    let bound: Option<String> = conn
        .query_row(
            "SELECT value FROM meta WHERE key='mote_store_id'",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if bound.as_deref() != Some(store_id) {
        return Err(Error::new(
            "mote_store_mismatch",
            "candidate store differs from board binding",
        ));
    }
    let to = string(a, "to")?;
    if view["proposer"] == to {
        return Err(Error::new(
            "self_review",
            "candidate proposer cannot be its reviewer",
        ));
    }
    let baseline = format!("git:{}", string(&view["identity"], "base_oid")?);
    let candidate = format!("git:{}", string(&view["identity"], "commit_oid")?);
    let result = crate::store::mutate(
        conn,
        &Request::new(
            "review_request",
            &req.actor,
            json!({"to":to,"title":string(a,"title")?,"body":string(a,"body")?,"baseline":baseline,"candidate":candidate,"mote_ref":format!("mote:{id}")}),
        ),
        now,
    )?;
    let card_id = integer(&result["card"], "id")?;
    conn.execute("INSERT INTO mote_review_subjects(card_id,store_id,candidate_id,proposer,observed_candidate,predecessor_card) VALUES(?,?,?,?,?,?)",params![card_id,store_id,id,string(view,"proposer")?,serde_json::to_string(view)?,a.get("predecessor_card").and_then(Value::as_i64)])?;
    let mut result = result;
    result["review"] = crate::review::context(conn, card_id, false)?;
    Ok(result)
}

fn validate_review_payload(conn: &Connection, actor: &str, p: &Value) -> Result<()> {
    let id = integer(p, "card_id")?;
    let card = crate::store::get_card(conn, id)?;
    let subject =
        crate::review::subject(conn, id)?.ok_or_else(|| Error::invalid("not a review"))?;
    let binding = review_binding(conn, id)?
        .ok_or_else(|| Error::invalid("not an explicit Mote candidate review"))?;
    if card.terminal() || !binding["successor_card"].is_null() {
        return Err(Error::new(
            "closed",
            "review subject is closed or superseded",
        ));
    }
    if card.author == actor || binding["proposer"] == actor {
        return Err(Error::new(
            "self_review",
            "request author/proposer cannot give its own peer verdict",
        ));
    }
    if subject["subject_rev"] != p["subject_rev"]
        || subject["candidate"] != p["at"]
        || binding["candidate_id"] != p["candidate_id"]
        || binding["store_id"] != p["store_id"]
    {
        return Err(Error::new(
            "conflict",
            "review subject changed; reread before reviewing",
        ));
    }
    if !["approve", "object", "blocked"].contains(&string(p, "verdict")?) {
        return Err(Error::invalid("unknown verdict"));
    }
    text(string(p, "body")?, "body", 8000, false)?;
    Ok(())
}

fn record_review(
    conn: &Connection,
    actor: &str,
    p: &Value,
    result: &Value,
    now: i64,
) -> Result<Value> {
    let id = integer(p, "card_id")?;
    let card = crate::store::get_card(conn, id)?;
    let verdict = string(p, "verdict")?;
    let binding = review_binding(conn, id)?
        .ok_or_else(|| Error::invalid("missing immutable review binding"))?;
    let historical_only =
        result["current_review_matches"] != true || !binding["successor_card"].is_null();
    let kind = if historical_only {
        "evidence"
    } else {
        match verdict {
            "object" => "objection",
            "blocked" => "question",
            _ => "evidence",
        }
    };
    let review = json!({"verdict":verdict,"version":p["at"],"subject_rev":p["subject_rev"],"baseline":p["baseline"],"mote_ref":format!("mote:{}",string(p,"candidate_id")?),"advisory":false,"mote_receipt":result["receipt"],"accepted_historical":true,"historical_only":historical_only,"current_review_matches":result["current_review_matches"]});
    conn.execute(
        "UPDATE cards SET rev=rev+1,updated_ms=? WHERE id=?",
        params![now, id],
    )?;
    let mut recorded = crate::store::emit(
        conn,
        actor,
        if historical_only {
            "mote_review_receipt"
        } else {
            "annotate"
        },
        id,
        json!({"kind":kind,"body":p["body"],"review":review}),
        now,
        true,
    )?;
    if !historical_only {
        crate::review::record_verdict(conn, id, actor, integer(&recorded, "event_seq")?, &review)?;
    }
    if kind != "evidence" {
        recorded["follow_up"] = crate::store::create_card(
            conn,
            actor,
            &json!({"kind":"question","topic":format!("@{}",card.author),"assignee":card.author,"title":clip(&format!("{kind} on #{id}: {}",string(p,"body")?),120),"summary":format!("{}\nFull context: fray thread {id} --bodies",clip(string(p,"body")?,1500)),"tags":[format!("parent:{id}"),format!("mote:{}",string(p,"candidate_id")?)],"priority":1}),
            json!({"parent_card":id,"annotation_seq":recorded["event_seq"],"kind":kind}),
            now,
        )?;
    }
    recorded["review"] = crate::review::context(conn, id, true)?;
    Ok(recorded)
}

pub fn successor(conn: &Connection, req: &Request, now: i64) -> Result<Value> {
    let a = &req.args;
    check_fields(a, &["id", "expect", "predecessor_view", "candidate_view"])?;
    let id = integer(a, "id")?;
    let card = crate::store::get_card(conn, id)?;
    let old =
        review_binding(conn, id)?.ok_or_else(|| Error::invalid("not a Mote candidate review"))?;
    let subject =
        crate::review::subject(conn, id)?.ok_or_else(|| Error::invalid("not a review"))?;
    if card.author != req.actor {
        return Err(Error::new(
            "not_author",
            "only review requester can create its successor round",
        ));
    }
    if subject["subject_rev"] != integer(a, "expect")? || !old["successor_card"].is_null() {
        return Err(Error::new(
            "conflict",
            "review already has a successor or revision changed",
        ));
    }
    let previous = &a["predecessor_view"];
    let next = &a["candidate_view"];
    if previous["candidate_id"] != old["candidate_id"]
        || previous["identity"]["store_id"] != old["store_id"]
        || previous["phase"]["value"] != "superseded"
        || previous["supersession"]["successor_id"] != next["candidate_id"]
    {
        return Err(Error::invalid(
            "Mote has not confirmed this exact predecessor/successor relationship",
        ));
    }
    let result = review_request(
        conn,
        &Request::new(
            "mote_review_request",
            &req.actor,
            json!({"to":card.assignee,"title":card.title,"body":format!("Successor of review #{id}; previous approvals do not apply. {}",card.summary),"candidate_view":next,"store_id":old["store_id"],"predecessor_card":id}),
        ),
        now,
    )?;
    let next_id = integer(&result["card"], "id")?;
    let mut query = conn.prepare("SELECT c.id,c.title,c.summary,coalesce(json_extract(e.payload,'$.detail.source_objector'),c.author) FROM cards c JOIN events e ON e.card_id=c.id AND e.op='post' WHERE c.status NOT IN ('resolved','superseded','withdrawn') AND json_extract(e.payload,'$.detail.parent_card')=? AND (json_extract(e.payload,'$.detail.kind')='objection' OR EXISTS(SELECT 1 FROM events p WHERE p.seq=json_extract(e.payload,'$.detail.annotation_seq') AND json_extract(p.payload,'$.detail.kind')='objection')) ORDER BY c.id")?;
    let objections = query
        .query_map([id], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut carried = Vec::new();
    for (old_id, title, summary, objector) in objections {
        let follow = crate::store::create_card(
            conn,
            &req.actor,
            &json!({"kind":"question","topic":format!("@{}",req.actor),"assignee":req.actor,"title":clip(&title,120),"summary":clip(&format!("Carried open objection #{old_id} from review #{id}. {summary}"),1900),"tags":[format!("parent:{next_id}"),format!("mote:{}",string(next,"candidate_id")?)],"priority":1}),
            json!({"parent_card":next_id,"kind":"objection","carried_objection_from":old_id,"predecessor_review":id,"source_objector":objector}),
            now,
        )?;
        carried.push(follow["card"]["id"].clone());
    }
    conn.execute(
        "UPDATE mote_review_subjects SET successor_card=? WHERE card_id=?",
        params![next_id, id],
    )?;
    crate::store::emit(
        conn,
        &req.actor,
        "mote_review_successor",
        id,
        json!({"successor_card":next_id,"candidate_id":next["candidate_id"],"carried_objections":carried,"previous_approvals_stale":true}),
        now,
        true,
    )?;
    Ok(json!({"successor":result,"carried_objections":carried}))
}

/// Client-side only: every RPC is short; the OS lock serializes one actor/key
/// across process death without retaining a daemon lock or expiring a lease.
pub struct Context<'a> {
    pub home: &'a Path,
    pub actor: &'a str,
    pub store: mote::Store,
    pub cwd: PathBuf,
}
impl<'a> Context<'a> {
    pub fn bind(home: &'a Path, actor: &'a str) -> Result<Self> {
        if !valid_name(actor) || actor == crate::store::OWNER {
            return Err(Error::invalid(
                "Mote workflows need a joined agent identity (--as NAME)",
            ));
        }
        let daemon = client::rpc(home, &Request::new("ping", actor, json!({})), 10)?;
        if !daemon["capabilities"]
            .as_array()
            .is_some_and(|c| c.iter().any(|s| s == "mote_workflows"))
        {
            return Err(Error::new(
                "unsupported_capability",
                "daemon lacks mote_workflows; coordinate an upgrade before retrying",
            ));
        }
        let cwd = fs::canonicalize(std::env::current_dir()?)?;
        let path = mote::locate(home, &cwd)?.ok_or_else(|| {
            Error::new("mote_required", "no Mote store is paired with this board")
        })?;
        let store_id = mote::store_id(&path)?;
        mote::version()?;
        client::rpc(
            home,
            &Request::new(
                "mote_bind",
                actor,
                json!({"store":path,"store_id":store_id}),
            ),
            10,
        )?;
        Ok(Self {
            home,
            actor,
            store: mote::Store { path, store_id },
            cwd,
        })
    }
    pub fn rpc(&self, op: &str, args: Value) -> Result<Value> {
        client::rpc(self.home, &Request::new(op, self.actor, args), 10)
    }
    pub fn rpc_keyed(&self, op: &str, args: Value, key: &str) -> Result<Value> {
        let mut request = Request::new(op, self.actor, args);
        request.key = Some(key.to_owned());
        client::rpc(self.home, &request, 10)
    }
    pub fn read(&self, args: &[&str]) -> Result<Value> {
        match mote::run(&self.store, Some(self.actor), args, mote::read_timeout()) {
            mote::Outcome::Ok(data) => Ok(data),
            outcome => Err(Error::new(
                "mote_unconfirmed",
                format!("authority-critical read {args:?} failed: {outcome:?}"),
            )),
        }
    }
    pub fn require(&self, capability: &str) -> Result<Value> {
        let status = self.read(&["authority", "status"])?;
        if status["schema"] != "mote.authority-status.v1"
            || status["store_id"] != self.store.store_id
            || status["authority_version"] != 1
            || status["enabled"] != true
            || status["genesis_digest"]
                .as_str()
                .is_none_or(|s| s.is_empty())
            || !status["capabilities"].as_array().is_some_and(|caps| {
                caps.iter().any(|c| c == "stable_claim_order")
                    && caps.iter().any(|c| c == capability)
            })
        {
            return Err(Error::new("mote_authority_required",format!("enabled Mote authority v1 with {capability} required; status: {status}. Upgrade all writers, then explicitly enable authority; this read did not enable it")));
        }
        Ok(status)
    }
    pub fn lock(&self, key: &str) -> Result<File> {
        lock_operation(self.home, self.actor, key)
    }
    pub fn get(&self, key: &str) -> Result<Value> {
        Ok(self.rpc("mote_operation_get", json!({"key":key}))?["operation"].clone())
    }
    pub fn prepare(&self, key: &str, kind: &str, mut payload: Value) -> Result<Value> {
        payload["store_id"] = json!(self.store.store_id);
        payload["cwd"] = json!(self.cwd);
        Ok(self.rpc(
            "mote_operation_prepare",
            json!({"key":key,"kind":kind,"payload":payload}),
        )?["operation"]
            .clone())
    }
    pub fn checkpoint(&self, op: &Value, step: &str, observation: Value) -> Result<Value> {
        Ok(self.rpc(
            "mote_operation_checkpoint",
            json!({"key":op["key"],"expect":op["rev"],"step":step,"observation":observation}),
        )?["operation"]
            .clone())
    }
    pub fn finalize(&self, op: &Value, result: Value) -> Result<Value> {
        Ok(self.rpc(
            "mote_operation_finalize",
            json!({"key":op["key"],"expect":op["rev"],"result":result}),
        )?["operation"]
            .clone())
    }
    pub fn mutate(&self, args: &[String]) -> mote::Outcome {
        mote::run(
            &self.store,
            Some(self.actor),
            &args.iter().map(String::as_str).collect::<Vec<_>>(),
            mote::MUTATION_TIMEOUT,
        )
    }
    pub fn validate_resume(&self, op: &Value) -> Result<()> {
        if op["payload"]["store_id"] != self.store.store_id
            || op["payload"]["cwd"] != self.cwd.to_string_lossy().as_ref()
        {
            return Err(Error::new(
                "operation_context",
                "resume requires the original Mote store and working directory",
            ));
        }
        Ok(())
    }
}

/// Transport serialization only; a reconciler can check another attempt's
/// kernel lock without making a Mote call as that actor. Never unlink locks.
pub fn lock_operation(home: &Path, actor: &str, key: &str) -> Result<File> {
    text(key, "key", 128, false)?;
    if !valid_name(actor) {
        return Err(Error::invalid("invalid operation actor"));
    }
    let directory = home.join("operation-locks");
    fs::create_dir_all(&directory)?;
    let name = digest(&json!([actor, key]))?;
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(directory.join(name))?;
    FileExt::try_lock_exclusive(&file).map_err(|_|Error::new("operation_busy","this exact operation is running in another client; retry the same key when it finishes"))?;
    Ok(file)
}

fn pending(op: &Value, message: &str) -> Error {
    Error::new("mote_unconfirmed",format!("{message}; retained operation key {}. Resume this exact operation from its original working directory",op["key"])).with_details(json!({"operation":op}))
}

pub fn request_candidate(
    context: &Context<'_>,
    candidate: &str,
    to: &str,
    title: &str,
    body: &str,
    key: &str,
) -> Result<Value> {
    local_action(
        context,
        key,
        "review_request",
        json!({"candidate_id":candidate,"to":to,"title":title,"body":body}),
        || {
            let view = context.read(&["candidate", "show", candidate])?;
            Ok(
                json!({"to":to,"title":title,"body":body,"candidate_view":view,"store_id":context.store.store_id}),
            )
        },
        "mote_review_request",
    )
}

pub fn request_successor(
    context: &Context<'_>,
    id: i64,
    expect: i64,
    candidate: &str,
    key: &str,
) -> Result<Value> {
    local_action(
        context,
        key,
        "review_successor",
        json!({"card_id":id,"expect":expect,"candidate_id":candidate}),
        || {
            let shown = context.rpc("show", json!({"id":id}))?;
            let previous = shown["review"]["mote_candidate"]["candidate_id"]
                .as_str()
                .ok_or_else(|| Error::invalid("not an explicit Mote review subject"))?;
            let old = context.read(&["candidate", "show", previous])?;
            let next = context.read(&["candidate", "show", candidate])?;
            Ok(json!({"id":id,"expect":expect,"predecessor_view":old,"candidate_view":next}))
        },
        "mote_review_successor",
    )
}

fn local_action(
    context: &Context<'_>,
    key: &str,
    kind: &str,
    explicit: Value,
    read_args: impl FnOnce() -> Result<Value>,
    rpc: &str,
) -> Result<Value> {
    let _lock = context.lock(key)?;
    context.require("stable_claim_order")?;
    let op = match context.get(key) {
        Ok(op) => {
            if op["kind"] != kind || op["payload"]["explicit"] != explicit {
                return Err(Error::new(
                    "idempotency_conflict",
                    "same key requires the original review request arguments",
                ));
            }
            op
        }
        Err(e) if e.code == "not_found" => context.prepare(
            key,
            kind,
            json!({"explicit":explicit,"rpc":rpc,"rpc_args":read_args()?}),
        )?,
        Err(e) => return Err(e),
    };
    execute_local(context, op)
}

fn execute_local(context: &Context<'_>, op: Value) -> Result<Value> {
    context.validate_resume(&op)?;
    let mut op = if op["state"] == "completed" {
        op
    } else {
        let request_key = digest(&json!([context.actor, op["key"], "attention"]))?;
        let result = context.rpc_keyed(
            string(&op["payload"], "rpc")?,
            op["payload"]["rpc_args"].clone(),
            &request_key,
        )?;
        context.finalize(&op, json!({"fray":result}))?
    };
    let mut result = op["result"]["fray"].take();
    result["operation_key"] = op["key"].clone();
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
pub fn candidate_verdict(
    context: &Context<'_>,
    id: i64,
    expect: i64,
    at: &str,
    verdict: &str,
    body: &str,
    evidence: &[String],
    role: Option<&str>,
    key: &str,
) -> Result<Value> {
    let _lock = context.lock(key)?;
    context.require("stable_claim_order")?;
    crate::review::version(at)?;
    let explicit = json!({"card_id":id,"subject_rev":expect,"at":at,"verdict":verdict,"body":body,"evidence":evidence,"role":role});
    let op = match context.get(key) {
        Ok(op) => {
            for (field, value) in explicit.as_object().expect("object") {
                if op["payload"][field] != *value {
                    return Err(Error::new(
                        "idempotency_conflict",
                        "same key requires identical review arguments",
                    ));
                }
            }
            if op["kind"] != "review" {
                return Err(Error::new(
                    "idempotency_conflict",
                    "key names a different operation kind",
                ));
            }
            op
        }
        Err(e) if e.code == "not_found" => {
            let shown = context.rpc("show", json!({"id":id}))?;
            let subject = &shown["review"];
            let binding = &subject["mote_candidate"];
            let candidate = string(binding, "candidate_id")?;
            let view = context.read(&["candidate", "show", candidate])?;
            if view["identity"]["store_id"] != context.store.store_id
                || format!("git:{}", string(&view["identity"], "commit_oid")?) != at
                || view["phase"]["value"] != "pending"
            {
                return Err(Error::new(
                    "conflict",
                    "Mote candidate identity/phase does not match this immutable review",
                ));
            }
            if view["proposer"] == context.actor {
                return Err(Error::new(
                    "self_review",
                    "candidate proposer cannot review itself",
                ));
            }
            if role.is_none()
                && !view["policy"]["reviewers"]
                    .as_array()
                    .is_some_and(|rs| rs.iter().any(|r| r == context.actor))
            {
                return Err(Error::new("mote_ineligible","not a named reviewer; use an eligible explicit --from-role or ask Mote to amend policy"));
            }
            let mapped = match verdict {
                "approve" => "approve",
                "object" | "blocked" => "block",
                _ => return Err(Error::invalid("verdict: approve|object|blocked")),
            };
            let mut argv = vec![
                "candidate".to_owned(),
                "review".to_owned(),
                candidate.to_owned(),
                mapped.to_owned(),
                "--body".to_owned(),
                body.to_owned(),
                "--idempotency-key".to_owned(),
                key.to_owned(),
            ];
            let previous = view["reviews"][context.actor]["op_id"].as_str();
            if let Some(token) = previous {
                argv.extend(["--expect".to_owned(), token.to_owned()]);
            }
            for reference in evidence {
                argv.extend(["--evidence".to_owned(), reference.clone()]);
            }
            if let Some(role) = role {
                argv.extend(["--from-role".to_owned(), role.to_owned()]);
            }
            let mut payload = explicit;
            payload["candidate_id"] = json!(candidate);
            payload["baseline"] = subject["baseline"].clone();
            payload["argv"] = json!(argv);
            payload["previous_review"] = json!(previous);
            payload["mapped_verdict"] = json!(mapped);
            context.prepare(key, "review", payload)?
        }
        Err(e) => return Err(e),
    };
    execute_review(context, op)
}

fn strings(value: &Value) -> Result<Vec<String>> {
    value
        .as_array()
        .ok_or_else(|| Error::invalid("journal argv is not an array"))?
        .iter()
        .map(|s| {
            s.as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::invalid("journal argv must contain strings"))
        })
        .collect()
}

fn execute_review(context: &Context<'_>, mut op: Value) -> Result<Value> {
    context.validate_resume(&op)?;
    if op["state"] == "completed" {
        let current =
            context.read(&["candidate", "show", string(&op["payload"], "candidate_id")?])?;
        return Ok(
            json!({"operation":op,"historical_receipt":true,"current_candidate":current,"accepted_historical":true}),
        );
    }
    let accepted = op["observations"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|o| o["step"] == "review_accepted")
        .map(|o| o["observation"].clone());
    let accepted = match accepted {
        Some(receipt) => receipt,
        None => {
            let outcome = context.mutate(&strings(&op["payload"]["argv"])?);
            let receipt = receipt(&outcome);
            op = context.checkpoint(
                &op,
                if matches!(outcome, mote::Outcome::Ok(_)) {
                    "review_accepted"
                } else {
                    "review_attempt"
                },
                receipt.clone(),
            )?;
            if !matches!(outcome, mote::Outcome::Ok(_)) {
                return Err(pending(
                    &op,
                    "Mote did not confirm the verdict; no Fray verdict recorded",
                ));
            }
            receipt
        }
    };
    let candidate = string(&op["payload"], "candidate_id")?;
    let readback = context
        .read(&["candidate", "show", candidate])
        .map_err(|e| {
            pending(
                &op,
                &format!("accepted receipt retained but authoritative readback failed: {e}"),
            )
        })?;
    if readback["candidate_id"] != candidate
        || format!("git:{}", string(&readback["identity"], "commit_oid")?)
            != op["payload"]["at"].as_str().unwrap_or("")
        || readback["identity"]["store_id"] != context.store.store_id
    {
        return Err(pending(&op, "review readback identity mismatch"));
    }
    let review = &readback["reviews"][context.actor];
    let qualification_matches = if op["payload"]["role"].is_null() {
        review["qualification"]["kind"] == "named_reviewer"
    } else {
        review["qualification"]["kind"] == "role_assignment"
            && review["qualification"]
                == accepted["data"]["reviews"][context.actor]["qualification"]
    };
    let matches = qualification_matches
        && readback["phase"]["value"] == "pending"
        && review["verdict"] == op["payload"]["mapped_verdict"]
        && review["body"] == op["payload"]["body"]
        && review["evidence_refs"] == op["payload"]["evidence"];
    context.finalize(&op,json!({"confirmed":true,"accepted_historical":true,"receipt":accepted,"readback":readback,"current_review_matches":matches}))
}

pub fn land(
    context: &Context<'_>,
    candidate: &str,
    target: &str,
    before: Option<&str>,
    check: bool,
    key: Option<&str>,
) -> Result<Value> {
    context.require("checked_landing_results")?;
    if check {
        let view = context.read(&["candidate", "show", candidate])?;
        let old = git_oid(&context.cwd, target)?;
        let target_evidence_matches = view["evidence"].as_array().into_iter().flatten().any(|e| {
            e["outcome"] == "pass"
                && e["payload"]["kind"] == "git_target_scope"
                && e["payload"]["target_ref"] == target
                && e["payload"]["observed_target_oid"] == old
        });
        return Ok(
            json!({"candidate":view,"target":target,"before":old,"check_only":true,"eligible":view["landability_actor"]==context.actor && view["landability"]["landable"]==true && target_evidence_matches && before.is_none_or(|oid|oid==old),"target_evidence_matches":target_evidence_matches,"mutated":false,"note":"Read-only policy and target-evidence observation; the fenced mutation revalidates repository, Git scope and authorization."}),
        );
    }
    let key =
        key.ok_or_else(|| Error::invalid("fenced landing requires --key; reuse it for recovery"))?;
    let _lock = context.lock(key)?;
    let op = match context.get(key) {
        Ok(op) => {
            if op["kind"] != "land"
                || op["payload"]["candidate_id"] != candidate
                || op["payload"]["target"] != target
                || before.is_some_and(|oid| op["payload"]["before"] != oid)
            {
                return Err(Error::new(
                    "idempotency_conflict",
                    "same key requires the original landing candidate/target/preimage",
                ));
            }
            op
        }
        Err(e) if e.code == "not_found" => {
            let view = context.read(&["candidate", "show", candidate])?;
            if view["landability_actor"] != context.actor || view["landability"]["landable"] != true
            {
                return Err(Error::new(
                    "mote_not_landable",
                    format!("actor-specific Mote landability: {}", view["landability"]),
                ));
            }
            let old = git_oid(&context.cwd, target)?;
            if before.is_some_and(|oid| oid != old) {
                return Err(Error::new(
                    "conflict",
                    "target does not match caller's expected preimage",
                ));
            }
            let phase = string(&view["phase"], "op_id")?;
            let auth = string(&view["authorization"], "op_id")?;
            let argv = vec![
                "candidate",
                "land",
                candidate,
                "--target",
                target,
                "--before",
                &old,
                "--expect-phase",
                phase,
                "--expect-authorization",
                auth,
                "--idempotency-key",
                key,
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
            context.prepare(key,"land",json!({"candidate_id":candidate,"target":target,"before":old,"new_oid":view["identity"]["commit_oid"],"repository_id":view["identity"]["landing_repository_id"],"argv":argv}))?
        }
        Err(e) => return Err(e),
    };
    execute_land(context, op)
}

fn git_oid(cwd: &Path, target: &str) -> Result<String> {
    let output = std::process::Command::new("git")
        .current_dir(cwd)
        .args(["rev-parse", "--verify", "--end-of-options", target])
        .output()?;
    if !output.status.success() {
        return Err(Error::new("git", String::from_utf8_lossy(&output.stderr)));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

fn execute_land(context: &Context<'_>, mut op: Value) -> Result<Value> {
    context.validate_resume(&op)?;
    if op["state"] == "completed" {
        let current = git_oid(&context.cwd, string(&op["payload"], "target")?)?;
        return Ok(
            json!({"operation":op,"historical_receipt":true,"current_oid":current,"target_current":current==op["payload"]["new_oid"]}),
        );
    }
    // The Mote landing key is recoverable. Even after a saved success receipt,
    // reissuing these exact bytes checks any active cleanup barrier.
    let outcome = context.mutate(&strings(&op["payload"]["argv"])?);
    let receipt = receipt(&outcome);
    op = context.checkpoint(&op, "landing_attempt", receipt.clone())?;
    let data = &receipt["data"];
    if !matches!(outcome, mote::Outcome::Ok(_))
        || data["outcome"] != "landed"
        || data["target_current"] != true
        || data["new_oid"] != op["payload"]["new_oid"]
        || data["old_oid"] != op["payload"]["before"]
        || data["candidate_id"] != op["payload"]["candidate_id"]
        || data["actor"] != context.actor
        || data["idempotency_key"] != op["key"]
    {
        return Err(pending(&op,"landing was not confirmed as a current completion; Git may have changed, inspect the complete receipt"));
    }
    let current = git_oid(&context.cwd, string(&op["payload"], "target")?)
        .map_err(|e| pending(&op, &format!("landing ref readback failed: {e}")))?;
    let candidate = context
        .read(&["candidate", "show", string(&op["payload"], "candidate_id")?])
        .map_err(|e| pending(&op, &format!("landing candidate readback failed: {e}")))?;
    if current != op["payload"]["new_oid"] || candidate["phase"]["value"] != "landed" {
        return Err(pending(
            &op,
            "landing receipt is historical or target moved before final readback",
        ));
    }
    context.finalize(&op,json!({"confirmed":true,"receipt":receipt,"current_oid":current,"candidate":candidate,"pushed":false}))
}

pub fn resume(context: &Context<'_>, key: &str) -> Result<Value> {
    let _lock = context.lock(key)?;
    let op = context.get(key)?;
    match op["kind"].as_str() {
        Some("review_request" | "review_successor") => {
            context.require("stable_claim_order")?;
            execute_local(context, op)
        }
        Some("review") => {
            context.require("stable_claim_order")?;
            execute_review(context, op)
        }
        Some("land") => {
            context.require("checked_landing_results")?;
            execute_land(context, op)
        }
        Some("authority_enable") => execute_enable(context, op),
        _ => Err(Error::invalid(
            "this operation kind needs its workflow-specific resume command",
        )),
    }
}

pub fn enable_authority(context: &Context<'_>, key: &str) -> Result<Value> {
    let _lock = context.lock(key)?;
    let status = context.read(&["authority", "status"])?;
    if status["schema"] != "mote.authority-status.v1"
        || status["store_id"] != context.store.store_id
    {
        return Err(Error::new(
            "mote_authority_required",
            "Mote lacks the explicit authority activation contract",
        ));
    }
    let op = context.prepare(
        key,
        "authority_enable",
        json!({"argv":["authority","enable"],"all_writers_upgraded":true}),
    )?;
    execute_enable(context, op)
}

fn execute_enable(context: &Context<'_>, mut op: Value) -> Result<Value> {
    context.validate_resume(&op)?;
    if op["state"] == "completed" {
        return Ok(op);
    }
    let outcome = context.mutate(&strings(&op["payload"]["argv"])?);
    op = context.checkpoint(&op, "authority_enable", receipt(&outcome))?;
    if !matches!(outcome, mote::Outcome::Ok(_)) {
        return Err(pending(&op, "authority migration outcome unconfirmed"));
    }
    let status = context.require("stable_claim_order")?;
    context.finalize(&op, json!({"confirmed":true,"authority":status}))
}

pub fn receipt(outcome: &mote::Outcome) -> Value {
    match outcome {
        mote::Outcome::Ok(data) => json!({"exit_code":0,"data":data}),
        mote::Outcome::Reported { exit_code, data } => json!({"exit_code":exit_code,"data":data}),
        other => json!({"confirmed":false,"error":format!("{other:?}")}),
    }
}
