//! Small host notifications. The immutable receipt survives host display limits;
//! full conversation context remains available through an explicit thread read.
use crate::model::*;
use serde_json::{json, Value};
pub const NOTICE_BYTES: usize = 768;

/// Remove and validate optional adapter-declared metadata before core selection
/// validation. It describes a wake mechanism; it never proves host responsiveness.
pub fn take_activation(args: &mut Value) -> Result<Option<Value>> {
    let mode = args
        .get("activation")
        .map(|_| string(args, "activation").map(str::to_owned))
        .transpose()?;
    let expires = args
        .get("activation_expires_ms")
        .map(|_| bounded(args, "activation_expires_ms", 0, 0, i64::MAX))
        .transpose()?;
    if let Some(mode) = &mode {
        if ![
            "manual",
            "boundary",
            "managed",
            "native-monitor",
            "background-completion",
        ]
        .contains(&mode.as_str())
        {
            return Err(Error::invalid(
                "activation: manual|boundary|managed|native-monitor|background-completion",
            ));
        }
    } else if expires.is_some() {
        return Err(Error::invalid("activation_expires_ms requires activation"));
    }
    if let Some(args) = args.as_object_mut() {
        args.remove("activation");
        args.remove("activation_expires_ms");
    }
    Ok(mode.map(|mode| json!({"mode":mode,"expires_ms":expires,"source":"adapter-declared"})))
}

pub fn notices(packet: &Value) -> Result<Vec<Value>> {
    let items = packet["items"]
        .as_array()
        .ok_or_else(|| Error::new("protocol", "attention items missing"))?;
    let mut out = Vec::new();
    if let Some(batch) = packet["batch"].as_str() {
        if batch.len() != 32 || !batch.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::new("protocol", "invalid read batch identifier"));
        }
        let titles: Vec<_> = items.iter().take(3).map(|item| json!({"id":item["receipt"]["id"],"title":item["card"]["title"].as_str().unwrap_or("").chars().take(60).collect::<String>()})).collect();
        let mut notice = json!({"type":"attention_notice","notice_version":1,"store_id":packet["store_id"],"agent":packet["agent"],"batch":batch,"fetch":["batch",batch],"receipt_count":items.len(),"titles":titles,"more":packet["more"],"read_is_not_ack":true});
        while serde_json::to_vec(&notice)?.len() + 1 > NOTICE_BYTES {
            if notice["titles"].as_array_mut().unwrap().pop().is_none() {
                return Err(Error::new(
                    "packet_budget",
                    "minimal batch notice exceeds notification budget",
                ));
            }
        }
        return Ok(vec![notice]);
    }
    for (index, item) in items.iter().enumerate() {
        let receipt = &item["receipt"];
        let id = integer(receipt, "id")?;
        let through = integer(receipt, "through_seq")?;
        let agent = string(receipt, "agent")?;
        let store = string(receipt, "store_id")?;
        if id <= 0
            || through <= 0
            || !valid_name(agent)
            || store.len() != 32
            || !store.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err(Error::new("protocol", "invalid attention receipt"));
        }
        let title = item["card"]["title"].as_str().unwrap_or("");
        let mut notice = json!({"type":"attention_notice","notice_version":1,"receipt":receipt,"title":title,"priority":item["card"]["priority"],"fetch":["thread",id.to_string(),"--bodies"],"remaining_in_packet":items.len()-index-1,"more":packet["more"],"read_is_not_ack":true});
        while serde_json::to_vec(&notice)?.len() + 1 > NOTICE_BYTES {
            let title = notice["title"].as_str().unwrap();
            if title.is_empty() {
                return Err(Error::new(
                    "packet_budget",
                    "minimal notification exceeds notification budget",
                ));
            }
            notice["title"] = json!(title
                .chars()
                .take(title.chars().count() / 2)
                .collect::<String>());
            notice["title_truncated"] = json!(true);
        }
        out.push(notice);
    }
    Ok(out)
}
