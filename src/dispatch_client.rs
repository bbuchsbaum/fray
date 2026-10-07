//! Mote mutations run only in this client, outside daemon transactions.
use crate::{
    model::*,
    mote,
    mote_workflow::{self, Context},
};
use serde_json::{json, Value};

pub fn live_claim(context: &Context<'_>, issue: &str) -> Result<Option<Value>> {
    // Holder/lease and token are separate public reads. A stable accepted
    // history surrounding the board read prevents a mixed A-holder/B-token.
    for _ in 0..3 {
        let before = context.read(&["history", issue])?;
        let board = context.read(&["board"])?;
        let after = context.read(&["history", issue])?;
        let accepted = |history: &Value| -> Result<Vec<Value>> {
            Ok(history
                .as_array()
                .ok_or_else(|| Error::invalid("Mote history must be an array"))?
                .iter()
                .filter(|e| e["accepted"] == true)
                .cloned()
                .collect())
        };
        let history = accepted(&after)?;
        if accepted(&before)? != history {
            continue;
        }
        let Some(claim) = board["active_claims"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|c| c["id"] == issue)
        else {
            return Ok(None);
        };
        let token = history
            .iter()
            .rev()
            .find(|e| ["claim", "handoff"].contains(&e["kind"].as_str().unwrap_or("")))
            .and_then(|e| e["op_id"].as_str())
            .ok_or_else(|| {
                Error::new(
                    "mote_unconfirmed",
                    "live claim has no accepted public history token",
                )
            })?;
        if crate::mote::parse_ts_ms(string(claim, "lease_until_ts")?).is_none_or(|t| t <= now_ms())
        {
            return Ok(None);
        }
        return Ok(Some(
            json!({"holder":claim["claimed_by"],"token":token,"lease_until_ts":claim["lease_until_ts"]}),
        ));
    }
    Err(Error::new(
        "mote_unconfirmed",
        "claim changed during public readback; retry without assuming ownership",
    ))
}

fn unconfirmed(op: &Value, why: &str) -> Error {
    Error::new(
        "mote_unconfirmed",
        format!("{why}; retained exact operation key {}", op["key"]),
    )
    .with_details(json!({"operation":op}))
}
fn observation(op: &Value, step: &str) -> Option<Value> {
    op["observations"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .find(|o| o["step"] == step)
        .map(|o| o["observation"].clone())
}
fn args(value: &Value) -> Result<Vec<String>> {
    value
        .as_array()
        .ok_or_else(|| Error::invalid("saved argv missing"))?
        .iter()
        .map(|v| {
            v.as_str()
                .map(str::to_owned)
                .ok_or_else(|| Error::invalid("invalid saved argv"))
        })
        .collect()
}
fn existing(
    context: &Context<'_>,
    key: &str,
    kind: &str,
    explicit: &Value,
) -> Result<Option<Value>> {
    match context.get(key) {
        Ok(op) => {
            if op["kind"] != kind || op["payload"]["explicit"] != *explicit {
                return Err(Error::new(
                    "idempotency_conflict",
                    "operation key requires its exact original arguments",
                ));
            }
            context.validate_resume(&op)?;
            Ok(Some(op))
        }
        Err(e) if e.code == "not_found" => Ok(None),
        Err(e) => Err(e),
    }
}

#[allow(clippy::too_many_arguments)]
pub fn offer(
    context: &Context<'_>,
    issue: &str,
    tag: &str,
    title: &str,
    body: &str,
    offer_ttl_s: i64,
    claim_ttl_s: i64,
    key: &str,
) -> Result<Value> {
    context.require("stable_claim_order")?;
    let _lock = context.lock(key)?;
    let explicit = json!({"issue":issue,"tag":tag,"title":title,"body":body,"offer_ttl_s":offer_ttl_s,"claim_ttl_s":claim_ttl_s});
    let op = if let Some(op) = existing(context, key, "dispatch_offer", &explicit)? {
        op
    } else {
        let work = context.read(&["show", issue])?;
        if ["closed", "deleted"].contains(&work["status"].as_str().unwrap_or("")) {
            return Err(Error::invalid("cannot offer closed/deleted Mote work"));
        }
        if let Some(claim) = live_claim(context, issue)? {
            return Err(Error::new(
                "mote_claimed",
                format!("work already held: {claim}; use handoff from its current holder"),
            ));
        }
        context.prepare(key, "dispatch_offer", json!({"explicit":explicit}))?
    };
    execute_offer(context, op)
}
fn execute_offer(context: &Context<'_>, op: Value) -> Result<Value> {
    context.validate_resume(&op)?;
    if op["state"] == "completed" {
        return Ok(op);
    }
    let key = mote_workflow::digest(&json!([context.actor, op["key"], "offer"]))?;
    let result = context.rpc_keyed("dispatch_offer", op["payload"]["explicit"].clone(), &key)?;
    context.finalize(&op, json!({"offer":result}))
}

pub fn accept(
    context: &Context<'_>,
    id: i64,
    generation: i64,
    key: &str,
    progress: Option<&str>,
) -> Result<Value> {
    context.require("stable_claim_order")?;
    let _lock = context.lock(key)?;
    // A handoff packet has its own recipient recovery sequence.
    match context.rpc("dispatch_handoff_get", json!({"id":id})) {
        Ok(packet) => return accept_packet(context, packet, key),
        Err(e) if e.code == "not_found" => {}
        Err(e) => return Err(e),
    }
    if let Some(progress) = progress {
        let offer = context.rpc("dispatch_get", json!({"id":id}))?;
        let claim = live_claim(context, string(&offer, "issue")?)?
            .ok_or_else(|| Error::new("mote_unconfirmed", "no live claim for progress"))?;
        return context.rpc("dispatch_status",json!({"id":id,"expect_generation":generation,"key":offer["attempt_key"],"claim":claim,"progress_marker":progress}));
    }
    let explicit = json!({"id":id,"generation":generation});
    let op = if let Some(op) = existing(context, key, "dispatch_accept", &explicit)? {
        op
    } else {
        context.prepare(key, "dispatch_accept", json!({"explicit":explicit}))?
    };
    execute_accept(context, op)
}
fn execute_accept(context: &Context<'_>, mut op: Value) -> Result<Value> {
    context.validate_resume(&op)?;
    let id = integer(&op["payload"]["explicit"], "id")?;
    if op["state"] == "completed" {
        let offer = context.rpc("dispatch_get", json!({"id":id}))?;
        return Ok(
            json!({"operation":op,"historical_receipt":true,"current_claim":live_claim(context,string(&offer,"issue")?)?}),
        );
    }
    if let Some(confirmation) = observation(&op, "confirmation_prepared") {
        // A committed confirmation can be recovered after renewal, expiry,
        // session loss or handoff. Replay its bytes; never claim again.
        let status_key = mote_workflow::digest(&json!([context.actor, op["key"], "confirmed"]))?;
        let result = context.rpc_keyed("dispatch_status", confirmation.clone(), &status_key)?;
        let offer = context.rpc("dispatch_get", json!({"id":id}))?;
        let current = live_claim(context, string(&offer, "issue")?)?;
        return context.finalize(&op,json!({"accepted_receipt":true,"claim_observation":confirmation["claim"],"current_claim":current,"historical_receipt":Some(&confirmation["claim"])!=current.as_ref(),"offer":result}));
    }
    let generation = integer(&op["payload"]["explicit"], "generation")?;
    let offer = context.rpc(
        "dispatch_accept",
        json!({"id":id,"expect_generation":generation,"key":op["key"]}),
    )?;
    if context.rpc("dispatch_attempt_live", json!({"id":id}))?["live"] != true {
        return Err(unconfirmed(
            &op,
            "original accepting session ended; this operation cannot reacquire",
        ));
    }
    let issue = string(&offer, "issue")?;
    let mut claim = live_claim(context, issue)?;
    if let Some(current) = &claim {
        if current["holder"] != context.actor {
            return Err(unconfirmed(
                &op,
                &format!("another Mote holder won: {current}"),
            ));
        }
    } else {
        if observation(&op, "claim_prepared").is_some() {
            return Err(unconfirmed(&op,"prior claim attempt has no live readback; do not renew/reacquire with this old key, reconcile its bounded expiry"));
        }
        let argv = vec![
            "claim".to_owned(),
            issue.to_owned(),
            "--ttl".to_owned(),
            offer["claim_ttl_s"].to_string(),
        ];
        op = context.checkpoint(&op, "claim_prepared", json!({"argv":argv,"offer":offer}))?;
        let outcome = context.mutate(&argv);
        op = context.checkpoint(&op, "claim_attempt", mote_workflow::receipt(&outcome))?;
        claim = live_claim(context, issue)
            .map_err(|e| unconfirmed(&op, &format!("claim readback failed: {e}")))?;
    }
    let claim = claim
        .filter(|c| c["holder"] == context.actor)
        .ok_or_else(|| unconfirmed(&op, "Mote did not confirm this actor's live claim"))?;
    let confirmation = if let Some(saved) = observation(&op, "confirmation_prepared") {
        saved
    } else {
        let saved = json!({"id":id,"expect_generation":generation,"key":op["key"],"claim":claim});
        op = context.checkpoint(&op, "confirmation_prepared", saved.clone())?;
        saved
    };
    let status_key = mote_workflow::digest(&json!([context.actor, op["key"], "confirmed"]))?;
    let result = context.rpc_keyed("dispatch_status", confirmation.clone(), &status_key)?;
    context.finalize(
        &op,
        json!({"accepted_receipt":true,"claim_observation":confirmation["claim"],"current_claim":claim,"historical_receipt":confirmation["claim"]!=claim,"offer":result}),
    )
}

pub fn sync(context: &Context<'_>) -> Result<Value> {
    context.require("stable_claim_order")?;
    let offers = context.rpc("dispatch_list", json!({}))?;
    let mut observations = Vec::new();
    let mut errors = Vec::new();
    for offer in offers["offers"].as_array().into_iter().flatten() {
        let id = integer(offer, "id")?;
        let guard = match (offer["accepted_by"].as_str(), offer["attempt_key"].as_str()) {
            (Some(actor), Some(key)) => {
                mote_workflow::lock_operation(context.home, actor, key).ok()
            }
            _ => None,
        };
        let claim = match live_claim(context, string(offer, "issue")?) {
            Ok(claim) => claim,
            Err(e) => {
                errors.push(json!({"id":id,"error":e}));
                continue;
            }
        };
        let idle = offer["attempt_key"].is_null() || guard.is_some();
        match context.rpc("dispatch_sync",json!({"id":id,"expect_generation":offer["generation"],"expect_attempt":{"actor":offer["accepted_by"],"key":offer["attempt_key"],"session":offer["attempt_session"],"peer_generation":offer["attempt_peer_generation"]},"claim":claim,"transport_idle":idle})){
   Ok(result)=>observations.push(result),Err(e)=>errors.push(json!({"id":id,"error":e})),
  }
    }
    Ok(json!({"offers":observations,"errors":errors}))
}

fn reservation(context: &Context<'_>, id: &str) -> Result<Value> {
    let board = context.read(&["board"])?;
    board["active_reservations"]
        .as_array()
        .into_iter()
        .flatten()
        .chain(
            board["orphaned_reservations"]
                .as_array()
                .into_iter()
                .flatten(),
        )
        .find(|r| r["reservation_id"] == id)
        .cloned()
        .ok_or_else(|| {
            Error::new(
                "reservation_lost",
                format!("reservation {id} is no longer live; continuity cannot be claimed"),
            )
        })
}

fn reservation_error(op: &Value, error: Error) -> Error {
    if error.code == "reservation_lost" {
        error.with_details(json!({"operation":op}))
    } else {
        unconfirmed(op, &error.to_string())
    }
}
fn lost(op: &Value, why: &str, current: &Value) -> Error {
    Error::new("reservation_lost", why)
        .with_details(json!({"operation":op,"current_reservation":current}))
}

pub fn handoff(context: &Context<'_>, explicit: Value, key: &str) -> Result<Value> {
    context.require("holder_checked_handoff")?;
    let _lock = context.lock(key)?;
    let op = if let Some(op) = existing(context, key, "dispatch_handoff", &explicit)? {
        op
    } else {
        let id = integer(&explicit, "source_card")?;
        let source = context.rpc("show", json!({"id":id}))?;
        let issues: Vec<_> = source["card"]["tags"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|t| t.strip_prefix("mote:"))
            .collect();
        let [issue] = issues.as_slice() else {
            return Err(Error::invalid(
                "source card needs exactly one Mote work reference",
            ));
        };
        let issue = *issue;
        let claim = live_claim(context, issue)?
            .filter(|c| c["holder"] == context.actor)
            .ok_or_else(|| {
                Error::new(
                    "mote_not_holder",
                    "only the observed live Mote holder can hand off",
                )
            })?;
        let mut carriers = Vec::new();
        for pair in explicit["carriers"]
            .as_array()
            .ok_or_else(|| Error::invalid("carrier pairs must be an array"))?
        {
            let carrier = string(pair, "carrier")?;
            if carrier == issue {
                return Err(Error::invalid("reservation carrier must differ from the work issue; acceptance closes only carriers"));
            }
            let rv = string(pair, "reservation")?;
            let observed = reservation(context, rv)?;
            if observed["actor"] != context.actor || observed["entity"] != carrier {
                return Err(Error::new(
                    "reservation_lost",
                    "carrier reservation no longer held by sender on that carrier",
                ));
            }
            let carrier_work = context.read(&["show", carrier])?;
            if ["closed", "deleted"].contains(&carrier_work["status"].as_str().unwrap_or("")) {
                return Err(Error::new(
                    "reservation_lost",
                    "carrier must remain open until recipient accepts",
                ));
            }
            let mut packet = pair.clone();
            packet["observation"] = observed;
            carriers.push(packet);
        }
        let to = string(&explicit, "to")?;
        let note = serde_json::to_string(
            &json!({"state":explicit["state"],"next":explicit["next"],"evidence":explicit["evidence"]}),
        )?;
        let argv = vec![
            "handoff",
            issue,
            "--to",
            to,
            "--expect-holder",
            context.actor,
            "--expect-claim",
            string(&claim, "token")?,
            "--idempotency-key",
            key,
            "--note",
            &note,
        ]
        .into_iter()
        .map(str::to_owned)
        .collect::<Vec<_>>();
        let payload = json!({"explicit":explicit,"issue":issue,"claim":claim,"carriers":carriers,"argv":argv});
        context.prepare(key, "dispatch_handoff", payload)?
    };
    execute_handoff(context, op)
}

fn execute_handoff(context: &Context<'_>, mut op: Value) -> Result<Value> {
    context.validate_resume(&op)?;
    let p = &op["payload"];
    let packet_args = json!({"source_card":p["explicit"]["source_card"],"to":p["explicit"]["to"],"issue":p["issue"],"key":op["key"],"holder":context.actor,"claim_token":p["claim"]["token"],"state":p["explicit"]["state"],"next":p["explicit"]["next"],"evidence":p["explicit"]["evidence"],"carriers":p["carriers"]});
    let packet = context.rpc("dispatch_handoff_packet", packet_args)?;
    if op["state"] == "completed" {
        return Ok(json!({"operation":op,"packet":packet,"historical_receipt":true}));
    }
    if observation(&op, "packet").is_none() {
        op = context.checkpoint(&op, "packet", packet.clone())?;
    }
    let argv = args(&op["payload"]["argv"])?;
    let outcome = context.mutate(&argv);
    let receipt = mote_workflow::receipt(&outcome);
    op = context.checkpoint(&op, "handoff_attempt", receipt.clone())?;
    let data = &receipt["data"];
    let current = live_claim(context, string(&op["payload"], "issue")?)?;
    let current_transfer = matches!(outcome, mote::Outcome::Ok(_))
        && data["accepted"] == true
        && data["claim_current"] == true
        && data["outcome"] == "transferred"
        && current.as_ref().is_some_and(|c| {
            c["holder"] == op["payload"]["explicit"]["to"] && c["token"] == data["op_id"]
        });
    context.rpc("dispatch_handoff_record",json!({"id":packet["id"],"status":if current_transfer{"transferred"}else if current.as_ref().is_some_and(|c|c["holder"]==op["payload"]["explicit"]["to"]){"partial"}else{"lost"},"receipt":receipt}))?;
    if !current_transfer {
        return Err(unconfirmed(
            &op,
            "handoff is failed/unknown/historical, not a current recipient transfer",
        ));
    }
    context.finalize(&op,json!({"confirmed":true,"packet":packet,"receipt":receipt,"claim_observation":current,"reservations_transferred":false}))
}

fn accept_packet(context: &Context<'_>, packet: Value, key: &str) -> Result<Value> {
    context.require("holder_checked_handoff")?;
    if packet["payload"]["to"] != context.actor {
        return Err(Error::new(
            "not_recipient",
            "only intended recipient can accept this packet",
        ));
    }
    let explicit = json!({"packet_id":packet["id"]});
    let op = if let Some(op) = existing(context, key, "dispatch_adopt", &explicit)? {
        op
    } else {
        context.prepare(
            key,
            "dispatch_adopt",
            json!({"explicit":explicit,"packet":packet}),
        )?
    };
    execute_adopt(context, op)
}

fn execute_adopt(context: &Context<'_>, op: Value) -> Result<Value> {
    let packet_id = op["payload"]["packet"]["id"].clone();
    let result = execute_adopt_steps(context, op);
    if let Err(error) = &result {
        // Preserve partial progress as attention even if a readback fails.
        // The exact operation journal remains the recovery source.
        let status = if error.code == "reservation_lost" {
            "lost"
        } else {
            "partial"
        };
        let _ = context.rpc(
            "dispatch_handoff_record",
            json!({"id":packet_id,"status":status,"receipt":{"error":error}}),
        );
    }
    result
}

fn execute_adopt_steps(context: &Context<'_>, mut op: Value) -> Result<Value> {
    context.validate_resume(&op)?;
    if op["state"] == "completed" {
        return Ok(op);
    }
    let packet = op["payload"]["packet"].clone();
    let issue = string(&packet["payload"], "issue")?;
    let sender = string(&packet["payload"], "holder")?;
    let _claim = live_claim(context, issue)?
        .filter(|c| c["holder"] == context.actor)
        .ok_or_else(|| unconfirmed(&op, "recipient no longer holds the live Mote work claim"))?;
    let mut adopted = Vec::new();
    for carrier in packet["payload"]["carriers"]
        .as_array()
        .ok_or_else(|| Error::invalid("packet carrier array missing"))?
    {
        let rv = string(carrier, "reservation")?;
        let carrier_id = string(carrier, "carrier")?;
        let mut current = reservation(context, rv).map_err(|e| reservation_error(&op, e))?;
        let expected_paths = &carrier["observation"]["paths"];
        if current["paths"] != *expected_paths {
            return Err(lost(
                &op,
                "reservation paths changed; inspect current owner",
                &current,
            ));
        }
        if current["actor"] == context.actor && current["entity"] == issue {
            adopted.push(current);
            continue;
        }
        if current["actor"] != sender || current["entity"] != carrier_id {
            return Err(lost(
                &op,
                "reservation adopted by another actor/issue; no held-path claim",
                &current,
            ));
        }
        let work = context.read(&["show", carrier_id])?;
        if work["status"] != "closed" {
            let step = format!("close:{rv}");
            let argv = vec!["close".to_owned(), carrier_id.to_owned()];
            op = context.checkpoint(
                &op,
                &step,
                json!({"argv":argv,"reservation_before":current}),
            )?;
            let outcome = context.mutate(&argv);
            op = context.checkpoint(
                &op,
                &format!("close_result:{rv}"),
                mote_workflow::receipt(&outcome),
            )?;
            let work = context.read(&["show", carrier_id])?;
            if work["status"] != "closed" {
                return Err(unconfirmed(
                    &op,
                    "carrier closure unconfirmed; its reservation was never unreserved",
                ));
            }
        }
        current = reservation(context, rv).map_err(|e| reservation_error(&op, e))?;
        if current["actor"] == context.actor && current["entity"] == issue {
            adopted.push(current);
            continue;
        }
        if current["actor"] != sender
            || current["entity"] != carrier_id
            || current["paths"] != *expected_paths
        {
            return Err(lost(
                &op,
                "carrier ownership changed during orphan interval",
                &current,
            ));
        }
        let step = format!("adopt_prepared:{rv}");
        let saved = observation(&op, &step);
        let argv = if let Some(saved) = saved {
            args(&saved["argv"])?
        } else {
            let clock = string(&current, "clock")?;
            let ttl = carrier["ttl_s"]
                .as_u64()
                .filter(|t| (1..=86400).contains(t))
                .ok_or_else(|| Error::invalid("carrier adoption TTL must be 1..86400 seconds"))?
                .to_string();
            let argv = vec![
                "adopt",
                rv,
                "--issue",
                issue,
                "--expect-reservation",
                clock,
                "--ttl",
                &ttl,
            ]
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
            op = context.checkpoint(
                &op,
                &step,
                json!({"argv":argv,"reservation_before":current}),
            )?;
            argv
        };
        let outcome = context.mutate(&argv);
        op = context.checkpoint(
            &op,
            &format!("adopt_result:{rv}"),
            mote_workflow::receipt(&outcome),
        )?;
        let current = reservation(context, rv).map_err(|e| reservation_error(&op, e))?;
        if current["actor"] != context.actor
            || current["entity"] != issue
            || current["paths"] != *expected_paths
        {
            return Err(lost(
                &op,
                "adoption rejected/unknown/competed; no held-path claim",
                &current,
            ));
        }
        adopted.push(current);
    }
    let final_claim = live_claim(context, issue)?
        .filter(|c| c["holder"] == context.actor)
        .ok_or_else(|| unconfirmed(&op, "work claim changed before final handoff confirmation"))?;
    for row in &adopted {
        let current = reservation(context, string(row, "reservation_id")?)?;
        if current["actor"] != context.actor
            || current["entity"] != issue
            || current["paths"] != row["paths"]
        {
            return Err(unconfirmed(
                &op,
                "reservation changed before final handoff confirmation",
            ));
        }
    }
    context.rpc("dispatch_handoff_record",json!({"id":packet["id"],"status":"completed","receipt":{"claim":final_claim,"reservations":adopted}}))?;
    context.finalize(&op,json!({"confirmed":true,"packet_id":packet["id"],"claim_observation":final_claim,"reservations":adopted,"continuity":"observed live before each step; never unreserved"}))
}

pub fn resume(context: &Context<'_>, key: &str) -> Result<Value> {
    let _lock = context.lock(key)?;
    let op = context.get(key)?;
    context.require("stable_claim_order")?;
    match op["kind"].as_str() {
        Some("dispatch_offer") => execute_offer(context, op),
        Some("dispatch_accept") => execute_accept(context, op),
        Some("dispatch_handoff") => {
            context.require("holder_checked_handoff")?;
            execute_handoff(context, op)
        }
        Some("dispatch_adopt") => {
            context.require("holder_checked_handoff")?;
            execute_adopt(context, op)
        }
        _ => Err(Error::invalid("not a dispatch operation")),
    }
}
