//! Role routing reads Mote's live lease state; it never creates ownership.
use crate::{client, model::*, mote};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::Path};

pub fn choose(role: &str, policy: Option<&Value>, roster: &Value, sender: &str) -> Result<Value> {
    let mut holders = BTreeMap::new();
    if let Some(policy) = policy {
        if !policy["retired"].is_null() {
            return Err(Error::new(
                "role_retired",
                format!("role {role} is retired"),
            ));
        }
        let active = policy["coverage"]["active_assignment_ids"]
            .as_array()
            .ok_or_else(|| Error::new("mote_invalid", "role show omitted active assignment IDs"))?;
        for assignment in policy["assignments"].as_array().into_iter().flatten() {
            if assignment["disposition"] == "active"
                && active.contains(&assignment["assignment_id"])
            {
                if let Some(holder) = assignment["holder_actor"].as_str() {
                    holders.insert(holder.to_owned(), assignment["assignment_id"].clone());
                }
            }
        }
    }
    let mut eligible: Vec<&Value> = roster["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|agent| {
            agent["enabled"] == true
                && agent["name"] != sender
                && matches!(agent["reachability"].as_str(), Some("wakeable" | "present"))
                && if policy.is_some() {
                    agent["name"]
                        .as_str()
                        .is_some_and(|name| holders.contains_key(name))
                } else {
                    agent["role"] == role
                }
        })
        .collect();
    eligible.sort_by_key(|agent| {
        (
            agent["reachability"] != "wakeable",
            agent["name"].as_str().unwrap_or(""),
        )
    });
    let Some(agent) = eligible.first() else {
        return Err(Error::new(if roster["more"] == true { "role_roster_truncated" } else { "no_live_role" },
            format!("no present Fray recipient for role {role} in the available roster; no message was queued. Use fray team to find a live peer or staff the role in Mote")));
    };
    let name = agent["name"].as_str().unwrap();
    Ok(
        json!({"role":role,"recipient":name,"reachability":agent["reachability"],"assignment":holders.get(name),"authority":if policy.is_some(){"Mote role lease"}else{"Fray registration"}}),
    )
}

pub fn resolve(home: &Path, sender: &str, role: &str) -> Result<Value> {
    text(role, "role", 80, false)?;
    let cwd = std::env::current_dir()?;
    let policy = if let Some(path) = mote::locate(home, &cwd)? {
        let store = mote::Store {
            store_id: mote::store_id(&path)?,
            path,
        };
        let bound = client::rpc(home, &Request::new("mote_binding", sender, json!({})), 10)?;
        if !bound["binding"].is_null() && bound["binding"]["store_id"] != store.store_id {
            return Err(Error::new(
                "mote_store_mismatch",
                "role store differs from this board's bound Mote store",
            ));
        }
        match mote::run(
            &store,
            Some(sender),
            &["role", "show", role],
            mote::read_timeout(),
        ) {
            mote::Outcome::Ok(policy) => Some(policy),
            other => {
                return Err(Error::new(
                    "role_unavailable",
                    format!("could not resolve Mote role {role}: {other:?}; no message was queued"),
                ))
            }
        }
    } else {
        None
    };
    let roster = client::rpc(
        home,
        &Request::new("agents", sender, json!({"limit":100})),
        10,
    )?;
    choose(role, policy.as_ref(), &roster, sender)
}
