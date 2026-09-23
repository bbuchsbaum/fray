//! The owner's review screen. The decision binds to the current head of the
//! request (its revision is sent as `expect`), so the request is printed
//! LAST, directly above the prompt, between banners that only this renderer
//! writes. Every row the renderer emits obeys one rule: agent-derived text
//! appears only after the quote margin, in printable ASCII, and every row
//! fits 80 columns. Rows are built only through `quote_rows`, so no agent
//! text can soft-wrap onto a row of its own, scroll the request away, or pass
//! for a banner.
use serde_json::Value;

/// Newest history events shown; older ones are counted, not printed.
pub const HISTORY_EVENTS: usize = 20;
/// Rows shown per history body before folding.
const BODY_ROWS: usize = 12;
/// Columns of agent text per row; with the 6-column margin, 78 in total.
const WIDTH: usize = 72;
const QUOTE: &str = "    | ";
/// Request blocks taller than this get a scroll notice above the end banner
/// (a 24-row terminal minus the banners and the prompt).
const TALL: usize = 18;
/// Actor names are shown clipped so a header row stays within 80 columns.
const ACTOR: usize = 40;

/// Terminal-safe tokens by allowlist: each printable ASCII character is one
/// token of one column; every other character, and the backslash itself, is
/// one visible `\u{XXXX}` token. Escaping the backslash makes the escape text
/// unambiguous: an agent's literal `\u{00E9}` renders differently from é.
fn tokens(s: &str) -> Vec<String> {
    s.chars()
        .map(|c| match c {
            '\\' => "\\u{005C}".to_owned(),
            ' '..='~' => c.to_string(),
            '\t' => " ".to_owned(),
            _ => format!("\\u{{{:04X}}}", c as u32),
        })
        .collect()
}

/// Terminal-safe text (see `tokens`), for tests and other callers.
pub fn clean(s: &str) -> String {
    tokens(s).concat()
}

/// Wrap one logical line into rows of at most WIDTH columns without ever
/// splitting an escape.
fn wrap(s: &str) -> Vec<String> {
    let mut rows = vec![String::new()];
    for token in tokens(s) {
        if rows.last().is_some_and(|r| r.len() + token.len() > WIDTH) {
            rows.push(String::new());
        }
        rows.last_mut().unwrap().push_str(&token);
    }
    rows
}

/// Quote text: every logical line wrapped, every row behind the margin.
/// Blank runs collapse to one. Returns the rows (not yet joined).
fn quote_rows(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut blank = false;
    for line in text.lines() {
        // Only ASCII spaces count as blank; other whitespace is shown escaped.
        if line.bytes().all(|b| matches!(b, b' ' | b'\t' | b'\r')) {
            if !blank {
                out.push(QUOTE.trim_end().to_owned());
            }
            blank = true;
            continue;
        }
        blank = false;
        out.extend(wrap(line).into_iter().map(|row| format!("{QUOTE}{row}")));
    }
    out
}

fn push_rows(out: &mut String, rows: &[String]) {
    for row in rows {
        out.push_str(row);
        out.push('\n');
    }
}

/// `thread` is a `show` result whose `history` holds the request's events in
/// order (all pages); `truncated` says whether history is incomplete.
pub fn render_request(thread: &Value, truncated: bool) -> String {
    let head = &thread["card"];
    let id = head["id"].as_i64().unwrap_or(0);
    let rev = head["rev"].as_i64().unwrap_or(0);
    let events: Vec<&Value> = thread["history"].as_array().into_iter().flatten().collect();
    let skip = events.len().saturating_sub(HISTORY_EVENTS);
    let mut out = String::from("\n  HISTORY (agent-written text is quoted with |):\n");
    if skip > 0 {
        out.push_str(&format!(
            "  ({skip} earlier event(s) not shown: fray thread {id} --bodies)\n"
        ));
    }
    if truncated {
        out.push_str("  (history longer than review reads: NEWEST events may be missing)\n");
    }
    for event in &events[skip..] {
        // Header: seq (number), actor ([A-Za-z0-9-_.:/] names, clipped),
        // kind/op (fixed vocabularies). No free text, within 80 columns.
        let actor: String = clean(event["actor"].as_str().unwrap_or(""))
            .chars()
            .take(ACTOR)
            .collect();
        let kind = event["kind"]
            .as_str()
            .or(event["op"].as_str())
            .unwrap_or("");
        // Real owner events are marked by the renderer from the store's
        // authority field, which agents cannot set; a lookalike name is not.
        let owner = if event["authority"].is_string() {
            " [OWNER AUTHORITY]"
        } else {
            ""
        };
        out.push_str(&format!(
            "  @{} {} {}{owner}\n",
            event["seq"],
            actor,
            clean(kind).chars().take(16).collect::<String>()
        ));
        if let Some(body) = event["body"].as_str() {
            let rows = quote_rows(body);
            push_rows(&mut out, &rows[..rows.len().min(BODY_ROWS)]);
            if rows.len() > BODY_ROWS {
                out.push_str(&format!(
                    "{QUOTE}... {} more rows: fray thread {id} --bodies\n",
                    rows.len() - BODY_ROWS
                ));
            }
        }
        if let Some(changed) = event["changed"].as_object().filter(|c| !c.is_empty()) {
            for (field, value) in changed {
                // The whole line goes through the quoting path; the head
                // block below shows current values in full anyway.
                let rows = quote_rows(&format!("changed {field} -> {value}"));
                push_rows(&mut out, &rows[..rows.len().min(2)]);
                if rows.len() > 2 {
                    out.push_str(&format!("{QUOTE}... (see the request below)\n"));
                }
            }
        }
    }
    // The decided text: title and text are both what is being approved, so
    // they are wrapped, never cut.
    let text = thread["full_text"]
        .as_str()
        .or(head["summary"].as_str())
        .unwrap_or("");
    let status = clean(head["status"].as_str().unwrap_or(""));
    let author: String = clean(head["author"].as_str().unwrap_or(""))
        .chars()
        .take(ACTOR)
        .collect();
    out.push_str(&format!(
        "\n==== DECIDING #{id} rev {rev} ({status}) ====\nFROM: {author}\nTITLE:\n"
    ));
    let title = quote_rows(head["title"].as_str().unwrap_or(""));
    let body = quote_rows(text);
    push_rows(&mut out, &title);
    out.push_str("TEXT:\n");
    push_rows(&mut out, &body);
    // The banner never repeats agent text; it says how tall the request is so
    // one that scrolled its own start away is noticed.
    let rows = title.len() + body.len();
    if rows > TALL {
        out.push_str(&format!(
            "  ({rows} rows of request above: scroll up to read all of it)\n"
        ));
    }
    // Numbers only, so the banner always fits 80 columns.
    out.push_str(&format!("==== end of request #{id} revision {rev} ====\n"));
    out
}
