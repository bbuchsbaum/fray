//! The owner's review screen. What it renders is exactly what a decision
//! binds to: the current head of the request (whose revision is sent as
//! `expect`), then its history including every field later changed.
use serde_json::Value;

fn clean(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

pub fn render_request(thread: &Value) -> String {
    let head = &thread["card"];
    let mut out = format!(
        "\n#{} from {}  (revision {}, {})\n  TITLE: {}\n  CURRENT TEXT:\n",
        head["id"],
        clean(head["author"].as_str().unwrap_or("")),
        head["rev"],
        clean(head["status"].as_str().unwrap_or("")),
        clean(head["title"].as_str().unwrap_or(""))
    );
    for line in head["summary"].as_str().unwrap_or("").lines() {
        out.push_str(&format!("    {}\n", clean(line)));
    }
    out.push_str("  HISTORY:\n");
    for event in thread["history"].as_array().into_iter().flatten() {
        out.push_str(&format!(
            "  @{} {} {}\n",
            event["seq"],
            clean(event["actor"].as_str().unwrap_or("")),
            clean(
                event["kind"]
                    .as_str()
                    .or(event["op"].as_str())
                    .unwrap_or("")
            )
        ));
        if let Some(body) = event["body"].as_str() {
            for line in body.lines() {
                out.push_str(&format!("    {}\n", clean(line)));
            }
        }
        if let Some(changed) = event["changed"].as_object().filter(|c| !c.is_empty()) {
            for (field, value) in changed {
                out.push_str(&format!(
                    "    changed {field} -> {}\n",
                    clean(&value.to_string())
                ));
            }
        }
    }
    if thread["more"] == true {
        out.push_str("  (older history omitted; the current text above is what you decide on)\n");
    }
    out
}
