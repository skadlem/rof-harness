//! Slash-command registry: one parser driving help, completion, and dispatch.

#[derive(Debug, PartialEq)]
pub enum Action {
    Quit,
    Help,
    Models,
    Context,
    Undo,
    Diff,
    Trace,
    Hotkeys,
    Model(String),
    Login(Option<String>),
    Logout(String),
    Attempts(u8),
    Rounds(u32),
    Thinking(String),
    Effort(String),
    Caps(usize, usize),
    Retry(Option<String>),
    Approve(String),
    Reject(String),
    Display(String),
    Busy(String),
    Unknown(String),
}

/// Parse composer input. `None` = plain goal text. Commands parse ONLY at
/// message start (claude rule); `/attempts 9` (out of 1-5) is Unknown, never
/// an error — the runner prints help instead of failing the session.
pub fn parse(input: &str) -> Option<Action> {
    let t = input.trim();
    if !t.starts_with('/') {
        return None;
    }
    let mut parts = t[1..].split_whitespace();
    let name = parts.next().unwrap_or("");
    let rest: Vec<&str> = parts.collect();
    let one = |i: usize| rest.get(i).map(|s| s.to_string());
    Some(match name {
        "quit" => Action::Quit,
        "help" => Action::Help,
        "models" => Action::Models,
        "context" => Action::Context,
        "undo" => Action::Undo,
        "diff" => Action::Diff,
        "trace" => Action::Trace,
        "hotkeys" => Action::Hotkeys,
        "model" => match one(0) {
            Some(m) => Action::Model(m),
            None => Action::Unknown("/model needs <provider>/<model-id>".into()),
        },
        "login" => Action::Login(one(0)),
        "logout" => match one(0) {
            Some(p) => Action::Logout(p),
            None => Action::Unknown("/logout needs <provider>".into()),
        },
        "attempts" => match one(0)
            .and_then(|v| v.parse::<u8>().ok())
            .filter(|n| (1..=5).contains(n))
        {
            Some(n) => Action::Attempts(n),
            None => Action::Unknown("/attempts takes 1-5".into()),
        },
        "rounds" => match one(0)
            .and_then(|v| v.parse::<u32>().ok())
            .filter(|n| *n >= 1)
        {
            Some(n) => Action::Rounds(n),
            None => Action::Unknown("/rounds takes >= 1".into()),
        },
        "thinking" => match one(0) {
            Some(m) if ["off", "low", "on"].contains(&m.as_str()) => Action::Thinking(m),
            _ => Action::Unknown("/thinking takes off/low/on".into()),
        },
        "effort" => match one(0) {
            Some(m) if ["low", "medium", "high", "none"].contains(&m.as_str()) => Action::Effort(m),
            _ => Action::Unknown("/effort takes low/medium/high/none".into()),
        },
        "caps" => match (
            one(0).and_then(|v| v.parse::<usize>().ok()),
            one(1).and_then(|v| v.parse::<usize>().ok()),
        ) {
            (Some(a), Some(b)) => Action::Caps(a, b),
            _ => Action::Unknown("/caps takes <implementer> <reviewer>".into()),
        },
        "retry" => Action::Retry(one(0)),
        "approve" => match one(0) {
            Some(id) => Action::Approve(id),
            None => Action::Unknown("/approve needs <id>".into()),
        },
        "reject" => match one(0) {
            Some(id) => Action::Reject(id),
            None => Action::Unknown("/reject needs <id>".into()),
        },
        "display" => match one(0) {
            Some(m) if ["fullscreen", "regular"].contains(&m.as_str()) => Action::Display(m),
            _ => Action::Unknown("/display takes fullscreen/regular".into()),
        },
        "busy" => match one(0) {
            Some(m) if ["interrupt", "queue", "steer"].contains(&m.as_str()) => Action::Busy(m),
            _ => Action::Unknown("/busy takes interrupt/queue/steer".into()),
        },
        other => Action::Unknown(format!("/{other}")),
    })
}

pub fn help_text() -> String {
    "/quit /help /model <p/m> /models /login [provider] /logout <provider> /attempts 1-5 /rounds N /thinking off|low|on /effort low|medium|high|none /caps <i> <r> /retry [note] /approve|reject <id> /context /undo /diff /trace /display fullscreen|regular /busy interrupt|queue|steer /hotkeys".to_string()
}
