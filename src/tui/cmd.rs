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
    ProviderAdd(String),
    ProviderList,
    /// Display-only listing of every known provider: base, whether a key
    /// is present, and the cached verify status. Never a key value — see
    /// the `Providers` arm in `run.rs`.
    Providers,
    ProviderRm(String),
    Approve(String),
    Reject(String),
    Display(String),
    /// `/theme` with no name LISTS the themes and the current one; with a
    /// name it selects that theme. The name is validated in the closed
    /// parser, so an unknown one arrives as [`Action::Unknown`] and can
    /// never be resolved later into a quiet fallback.
    Theme(Option<String>),
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
        "providers" => Action::Providers,
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
        "provider" => match (one(0).as_deref(), one(1), one(2)) {
            (Some("add"), Some(_), Some(_)) => Action::ProviderAdd(one(1).unwrap()),
            (Some("list"), _, _) => Action::ProviderList,
            (Some("rm"), Some(_), _) => Action::ProviderRm(one(1).unwrap()),
            _ => Action::Unknown("/provider takes add <name> <base-url> | list | rm <name>".into()),
        },
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
        "theme" => match one(0) {
            // The listing: no argument is a read, not an error.
            None => Action::Theme(None),
            Some(name) => match super::theme::Theme::parse(&name) {
                Some(_) => Action::Theme(Some(name)),
                // Refused here, at the closed parser, naming the valid
                // choices. Nothing downstream can turn this into the
                // default theme by omission.
                None => Action::Unknown(super::theme::Theme::unknown_name_line(&name)),
            },
        },
        "busy" => match one(0) {
            Some(m) if ["interrupt", "queue", "steer"].contains(&m.as_str()) => Action::Busy(m),
            _ => Action::Unknown("/busy takes interrupt/queue/steer".into()),
        },
        other => Action::Unknown(format!("/{other}")),
    })
}

pub fn help_text() -> String {
    "/quit /help /model <p/m> /model ctx|verify|fallback <p/m> /models /providers /login [provider] /logout <provider> /provider add|list|rm /attempts 1-5 /rounds N /thinking off|low|on /effort low|medium|high|none /caps <i> <r> /retry [note] /approve|reject <id> /context /undo /diff /trace /display fullscreen|regular /theme [name] /busy interrupt|queue|steer /hotkeys\n/busy: steer is the default — Enter during a run steers the live goal · queue stores exactly one next goal · interrupt arms the stop path (q/Esc/Ctrl-C)".to_string()
}

/// What `/busy <mode>` reports, per mode. The modes are postures, not
/// commands, so the copy says what Enter will do under each one. No
/// credential is ever named here: a `/busy` line is not a place for one.
pub fn busy_line(mode: &str) -> String {
    match mode {
        "steer" => {
            "busy=steer: the default — Enter during a run steers the live goal (nothing is interrupted)".to_string()
        }
        "queue" => {
            "busy=queue: Enter during a run stores exactly one next goal, started when the live one finishes".to_string()
        }
        "interrupt" => {
            "busy=interrupt: Enter during a run arms the stop path — press q/Esc/Ctrl-C to stop and exit".to_string()
        }
        other => format!("/busy takes steer|queue|interrupt (got {other})"),
    }
}
