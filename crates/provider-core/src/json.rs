use serde_json::Value;

/// Streaming-JSON salvage: valid parse -> repaired parse -> partial ->
/// partial-of-repaired -> `{}`. Truncated tool args must still decode, and a
/// hopeless payload must not throw: it yields an empty object while the
/// `MaxTokens` guard (not the parser) blocks execution.
pub fn parse_streaming_json(s: &str) -> Value {
    if let Ok(v) = serde_json::from_str(s) {
        return v;
    }
    let repaired = repair_json(s);
    if let Ok(v) = serde_json::from_str(&repaired) {
        return v;
    }
    if let Some(v) = partial_parse(s) {
        return v;
    }
    if let Some(v) = partial_parse(&repaired) {
        return v;
    }
    Value::Object(serde_json::Map::new())
}

/// Escape raw control chars inside strings; double backslashes that precede
/// an invalid escape (`\p` -> `\\p`) so a path never kills the parse.
fn repair_json(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut in_str = false;
    let mut esc = false;
    while let Some(c) = chars.next() {
        if in_str {
            if esc {
                out.push(c);
                esc = false;
                continue;
            }
            if c == '\\' {
                match chars.peek() {
                    Some('"' | '\\' | '/' | 'b' | 'f' | 'n' | 'r' | 't' | 'u') => {
                        out.push('\\');
                        esc = true;
                    }
                    _ => out.push_str("\\\\"),
                }
                continue;
            }
            match c {
                '"' => {
                    in_str = false;
                    out.push(c);
                }
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => {
                    out.push_str(&format!("\\u{:04x}", c as u32));
                }
                _ => out.push(c),
            }
        } else if c == '"' {
            in_str = true;
            out.push(c);
        } else {
            out.push(c);
        }
    }
    out
}

/// Close open braces/brackets/strings, drop dangling `,`/`:` first. Then one
/// retry with a trailing partial literal stripped (`{"a": tru` -> `{"a"` is
/// still unparseable, so this only rescues values cut at a clean boundary).
fn partial_parse(s: &str) -> Option<Value> {
    if let Ok(v) = serde_json::from_str::<Value>(&close_truncated(s)) {
        return Some(v);
    }
    let t = s.trim_end();
    let cut = t
        .trim_end_matches(|c: char| c.is_alphanumeric() || matches!(c, '.' | '+' | '-' | '_'))
        .trim_end()
        .trim_end_matches(',')
        .trim_end_matches(':')
        .trim_end();
    if cut.len() < t.len() {
        serde_json::from_str::<Value>(&close_truncated(cut)).ok()
    } else {
        None
    }
}

fn close_truncated(s: &str) -> String {
    let mut stack: Vec<char> = Vec::new();
    let mut in_str = false;
    let mut esc = false;
    for c in s.chars() {
        if in_str {
            if esc {
                esc = false;
            } else if c == '\\' {
                esc = true;
            } else if c == '"' {
                in_str = false;
            }
        } else if c == '"' {
            in_str = true;
        } else if c == '{' {
            stack.push('}');
        } else if c == '[' {
            stack.push(']');
        } else if (c == '}' || c == ']') && stack.last() == Some(&c) {
            stack.pop();
        }
    }
    let mut out = s.trim_end().to_string();
    loop {
        let t = out.trim_end();
        if t.ends_with(',') || t.ends_with(':') {
            out = t[..t.len() - 1].to_string();
        } else {
            out = t.to_string();
            break;
        }
    }
    if in_str {
        out.push('"');
    }
    while let Some(c) = stack.pop() {
        out.push(c);
    }
    out
}
