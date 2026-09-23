//! The owner's review screen. The decision binds to the current head of the
//! request (its revision is sent as `expect`), so the request is printed
//! LAST, directly above the prompt, between unquoted banners that only this
//! renderer writes. Every agent-written line, in the history and in the
//! request itself, is quoted with a margin; history bodies are folded, so no
//! agent text can scroll the request away or pass for a banner.
use serde_json::Value;

/// Newest history events shown; older ones are counted, not printed.
pub const HISTORY_EVENTS: usize = 20;
/// Lines shown per history body before folding.
const BODY_LINES: usize = 12;
/// Characters shown per line before truncation.
const LINE_CHARS: usize = 200;
const QUOTE: &str = "    | ";

/// Terminal-safe text: control characters become spaces, and invisible or
/// reordering format characters (bidi overrides and isolates, zero-width
/// characters, line/paragraph separators, BOM, soft hyphen) are shown
/// escaped, so nothing can hide or rearrange what the owner reads.
pub fn clean(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        let invisible = matches!(c,
            '\u{00AD}' | '\u{061C}' | '\u{180E}' | '\u{200B}'..='\u{200F}'
            | '\u{2028}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{FEFF}');
        if invisible {
            out.push_str(&format!("\\u{{{:04X}}}", c as u32));
        } else if c.is_control() {
            out.push(' ');
        } else {
            out.push(c);
        }
    }
    out
}

fn line(s: &str) -> String {
    let cleaned = clean(s);
    let mut it = cleaned.chars();
    let mut kept: String = it.by_ref().take(LINE_CHARS).collect();
    if it.next().is_some() {
        kept.push('…');
    }
    kept
}

fn quoted(out: &mut String, text: &str) {
    let lines: Vec<&str> = text.lines().collect();
    for l in lines.iter().take(BODY_LINES) {
        out.push_str(QUOTE);
        out.push_str(&line(l));
        out.push('\n');
    }
    if lines.len() > BODY_LINES {
        out.push_str(&format!(
            "{QUOTE}… {} more lines (fray thread ID --bodies)\n",
            lines.len() - BODY_LINES
        ));
    }
}

/// `thread` is a `show` result whose `history` holds the request's events in
/// order (all pages); `truncated` says whether history is incomplete.
pub fn render_request(thread: &Value, truncated: bool) -> String {
    let head = &thread["card"];
    let events: Vec<&Value> = thread["history"].as_array().into_iter().flatten().collect();
    let skip = events.len().saturating_sub(HISTORY_EVENTS);
    let mut out = String::from("\n  HISTORY (agent-written text is quoted with |):\n");
    if skip > 0 || truncated {
        out.push_str(&format!(
            "  ({} earlier event(s) not shown; read them with fray thread {} --bodies)\n",
            skip, head["id"]
        ));
    }
    for event in &events[skip..] {
        out.push_str(&format!(
            "  @{} {} {}\n",
            event["seq"],
            line(event["actor"].as_str().unwrap_or("")),
            line(
                event["kind"]
                    .as_str()
                    .or(event["op"].as_str())
                    .unwrap_or("")
            )
        ));
        if let Some(body) = event["body"].as_str() {
            quoted(&mut out, body);
        }
        if let Some(changed) = event["changed"].as_object().filter(|c| !c.is_empty()) {
            for (field, value) in changed {
                out.push_str(&format!(
                    "{QUOTE}changed {field} -> {}\n",
                    line(&value.to_string())
                ));
            }
        }
    }
    // The decided text: quoted like everything agent-written, wrapped (never
    // truncated: this is what is being approved), blank runs collapsed.
    let text = thread["full_text"]
        .as_str()
        .or(head["summary"].as_str())
        .unwrap_or("");
    let title = line(head["title"].as_str().unwrap_or(""));
    out.push_str(&format!(
        "\n==== DECIDING ON REQUEST #{} (revision {}, {}, from {}) ====\n",
        head["id"],
        head["rev"],
        line(head["status"].as_str().unwrap_or("")),
        line(head["author"].as_str().unwrap_or(""))
    ));
    out.push_str(&format!("TITLE:\n{QUOTE}{title}\nTEXT:\n"));
    let mut blank = false;
    for l in text.lines() {
        let cleaned = clean(l);
        if cleaned.trim().is_empty() {
            if !blank {
                out.push_str(QUOTE.trim_end());
                out.push('\n');
            }
            blank = true;
            continue;
        }
        blank = false;
        let chars: Vec<char> = cleaned.chars().collect();
        for chunk in chars.chunks(LINE_CHARS) {
            out.push_str(QUOTE);
            out.extend(chunk.iter());
            out.push('\n');
        }
    }
    out.push_str(&format!(
        "==== end of request #{} revision {}: {} ====\n",
        head["id"], head["rev"], title
    ));
    out
}
