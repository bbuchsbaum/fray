//! Small host notifications. The immutable receipt survives host display limits;
//! full conversation context remains available through an explicit thread read.
use crate::model::*;
use serde_json::{json, Value};
pub const NOTICE_BYTES: usize = 768;
const PREVIEW_KIND_CHARS: usize = 40;
const PREVIEW_AUTHOR_CHARS: usize = 80;
const PREVIEW_BODY_CHARS: usize = 160;
const TITLE_FLOOR_CHARS: usize = 20;

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
        let packet_agent = packet["agent"].as_str();
        let titles: Vec<_> = items
            .iter()
            .take(3)
            .map(|item| {
                json!({
                    "id": item["receipt"]["id"],
                    "title": item["card"]["title"]
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .take(60)
                        .collect::<String>(),
                    "preview": preview(item, packet_agent),
                })
            })
            .collect();
        let mut notice = json!({"type":"attention_notice","notice_version":1,"store_id":packet["store_id"],"agent":packet["agent"],"batch":batch,"fetch":["batch",batch],"receipt_count":items.len(),"titles":titles,"more":packet["more"],"read_is_not_ack":true});
        while serde_json::to_vec(&notice)?.len() + 1 > NOTICE_BYTES {
            if shrink_batch_titles_to_floor(&mut notice) {
                continue;
            }
            let titles = notice["titles"].as_array_mut().unwrap();
            if titles.len() > 1 {
                titles.pop();
                continue;
            }
            if remove_preview_author(&mut titles[0]["preview"])
                || shorten_field(&mut titles[0]["title"], 0)
                || shrink_preview(&mut titles[0]["preview"], true)
                || shrink_preview(&mut titles[0]["preview"], false)
            {
                continue;
            }
            return Err(Error::new(
                "packet_budget",
                "minimal batch notice exceeds notification budget",
            ));
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
        let mut notice = json!({"type":"attention_notice","notice_version":1,"receipt":receipt,"title":title,"preview":preview(item, packet["agent"].as_str().or(Some(agent))),"priority":item["card"]["priority"],"fetch":["thread",id.to_string(),"--bodies"],"remaining_in_packet":items.len()-index-1,"more":packet["more"],"read_is_not_ack":true});
        while serde_json::to_vec(&notice)?.len() + 1 > NOTICE_BYTES {
            if shrink_notice_title(&mut notice, TITLE_FLOOR_CHARS) {
                continue;
            }
            if remove_preview_author(&mut notice["preview"])
                || shrink_notice_title(&mut notice, 0)
                || shrink_preview(&mut notice["preview"], true)
                || shrink_preview(&mut notice["preview"], false)
            {
                continue;
            }
            return Err(Error::new(
                "packet_budget",
                "minimal notification exceeds notification budget",
            ));
        }
        out.push(notice);
    }
    Ok(out)
}

fn preview(item: &Value, packet_agent: Option<&str>) -> Value {
    let through = item["receipt"]["through_seq"].as_i64();
    let agent = packet_agent.or_else(|| item["receipt"]["agent"].as_str());
    let latest = item["messages"].as_array().and_then(|messages| {
        messages
            .iter()
            .filter(|message| {
                message["seq"]
                    .as_i64()
                    .is_some_and(|seq| through.is_none_or(|limit| seq <= limit))
                    && agent.is_none_or(|agent| message["author"].as_str() != Some(agent))
            })
            .max_by_key(|message| message["seq"].as_i64().unwrap_or(i64::MIN))
    });
    match latest {
        Some(message) => json!({
            "seq": message["seq"],
            "author": clip(message["author"].as_str().unwrap_or(""), PREVIEW_AUTHOR_CHARS),
            "kind": clip(message["kind"].as_str().unwrap_or(""), PREVIEW_KIND_CHARS),
            "body": clip(first_nonempty_line(message["body"].as_str().unwrap_or("")), PREVIEW_BODY_CHARS),
        }),
        None => Value::Null,
    }
}

fn first_nonempty_line(body: &str) -> &str {
    body.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("")
}

fn shrink_preview(preview: &mut Value, preserve_one_char: bool) -> bool {
    let Some(body) = preview["body"].as_str() else {
        return false;
    };
    let floor = usize::from(preserve_one_char);
    if body.chars().count() <= floor {
        return false;
    }
    let shortened = body
        .chars()
        .take((body.chars().count() / 2).max(floor))
        .collect::<String>();
    preview["body"] = json!(shortened);
    true
}

fn shrink_batch_titles_to_floor(notice: &mut Value) -> bool {
    let Some(titles) = notice["titles"].as_array_mut() else {
        return false;
    };
    for title in titles.iter_mut().rev() {
        if shorten_field(&mut title["title"], TITLE_FLOOR_CHARS) {
            return true;
        }
    }
    false
}

fn shorten_field(value: &mut Value, floor: usize) -> bool {
    let Some(text) = value.as_str() else {
        return false;
    };
    let count = text.chars().count();
    if count <= floor {
        return false;
    }
    *value = json!(text
        .chars()
        .take((count / 2).max(floor))
        .collect::<String>());
    true
}

fn shrink_notice_title(notice: &mut Value, floor: usize) -> bool {
    if shorten_field(&mut notice["title"], floor) {
        notice["title_truncated"] = json!(true);
        true
    } else {
        false
    }
}

fn remove_preview_author(preview: &mut Value) -> bool {
    preview
        .as_object_mut()
        .and_then(|preview| preview.remove("author"))
        .is_some()
}
