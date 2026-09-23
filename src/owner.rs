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
/// Characters per screen row of agent text. The margin plus this stays under
/// 80 columns, and with ASCII-only output one character is one column, so no
/// agent text can soft-wrap onto a row of its own.
const LINE_CHARS: usize = 72;
const QUOTE: &str = "    | ";
/// Request text taller than this gets a scroll warning in the end banner.
const TALL: usize = 30;

/// Terminal-safe text by allowlist: printable ASCII passes; every other
/// character (invisible tags and variation selectors, bidi controls, wide or
/// combining characters, all controls) is shown as a visible `\u{...}`
/// escape, so nothing can be hidden, reordered or misaligned on screen.
pub fn clean(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        match c {
            ' '..='~' => out.push(c),
            '\t' => out.push(' '),
            _ => out.push_str(&format!("\\u{{{:04X}}}", c as u32)),
        }
    }
    out
}

fn line(s: &str) -> String {
    let cleaned = clean(s);
    if cleaned.len() > LINE_CHARS {
        format!("{}...", &cleaned[..LINE_CHARS - 3])
    } else {
        cleaned
    }
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
            "{QUOTE}... {} more lines (fray thread ID --bodies)\n",
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
    if skip > 0 {
        out.push_str(&format!(
            "  ({skip} earlier event(s) not shown; read them with fray thread {} --bodies)\n",
            head["id"]
        ));
    }
    if truncated {
        out.push_str(
            "  (history is longer than review reads; the NEWEST events may be missing here)\n",
        );
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

    out.push_str(&format!(
        "\n==== DECIDING ON REQUEST #{} (revision {}, {}, from {}) ====\n",
        head["id"],
        head["rev"],
        line(head["status"].as_str().unwrap_or("")),
        line(head["author"].as_str().unwrap_or(""))
    ));
    // Title and text are both what is being approved: wrapped, never cut.
    out.push_str("TITLE:\n");
    for chunk in clean(head["title"].as_str().unwrap_or(""))
        .as_bytes()
        .chunks(LINE_CHARS)
    {
        out.push_str(QUOTE);
        out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
        out.push('\n');
    }
    out.push_str("TEXT:\n");
    let mut rows = 0;
    let mut blank = false;
    for l in text.lines() {
        let cleaned = clean(l);
        if cleaned.trim().is_empty() {
            if !blank {
                out.push_str(QUOTE.trim_end());
                out.push('\n');
                rows += 1;
            }
            blank = true;
            continue;
        }
        blank = false;
        for chunk in cleaned.as_bytes().chunks(LINE_CHARS) {
            out.push_str(QUOTE);
            out.push_str(std::str::from_utf8(chunk).unwrap_or(""));
            out.push('\n');
            rows += 1;
        }
    }
    // The banner never repeats agent text; it says how tall the text is so
    // a request that scrolled its own start away is noticed.
    let scroll = if rows > TALL {
        format!(", {rows} lines: scroll up to read it all")
    } else {
        String::new()
    };
    out.push_str(&format!(
        "==== end of request #{} revision {}{scroll} ====\n",
        head["id"], head["rev"]
    ));
    out
}
