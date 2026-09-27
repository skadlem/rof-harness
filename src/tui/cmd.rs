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

/// Every command name the closed parser above accepts, in the order
/// `/help` lists them. This is the registry completion reads: there is no
/// second list, so a command cannot be added to `parse` and left out of
/// completion (the `a_command_added_to_the_parser_but_not_to_the_registry_fails`
/// test in `tests/tui_cmd.rs` reads `parse`'s arms and fails on a name
/// missing from here).
pub const COMMANDS: &[&str] = &[
    "quit",
    "help",
    "model",
    "models",
    "providers",
    "login",
    "logout",
    "provider",
    "attempts",
    "rounds",
    "thinking",
    "effort",
    "caps",
    "retry",
    "approve",
    "reject",
    "context",
    "undo",
    "diff",
    "trace",
    "display",
    "theme",
    "busy",
    "hotkeys",
];

/// The providers that resolve a base without a registry entry, so they are
/// always `/login`-able. Named here rather than read out of `auth.rs`
/// because `auth` deliberately exposes no list; `auth::base_for` is the
/// resolver and the test below asks it about every one of these.
pub const BUILTIN_PROVIDERS: &[&str] = &["openrouter", "go", "atria", "custom"];

/// The provider ids completion offers, sorted and unique.
///
/// Names only, from two non-secret sources: the built-ins, and the keys of
/// `auth::registry()` (the `~/.rof/providers.json` name → base map) plus the
/// keys of `auth::statuses()` (the cached verify outcomes). A base URL is
/// not a candidate and neither is any value from `auth::key_for` or the
/// credentials store — completion reads no secret and writes no
/// configuration.
pub fn provider_candidates() -> Vec<String> {
    let mut ids: Vec<String> = BUILTIN_PROVIDERS.iter().map(|s| s.to_string()).collect();
    ids.extend(super::auth::registry().into_keys());
    ids.extend(super::auth::statuses().into_keys());
    ids.sort();
    ids.dedup();
    ids
}

/// Every command as the composer spells it, `/name`, sorted so the
/// candidate list a press reports is the same every time.
pub fn command_candidates() -> Vec<String> {
    let mut names: Vec<String> = COMMANDS.iter().map(|name| format!("/{name}")).collect();
    names.sort();
    names
}

/// What one completion press did to the draft.
///
/// `Completed` carries the new draft text plus, when the prefix was
/// ambiguous, the candidates still in play — completion never picks one
/// silently, so an answer with candidates is a question, not a choice.
/// `NoMatch` carries the draft unchanged and the reason to show.
#[derive(Debug, PartialEq, Eq)]
pub enum Completion {
    Completed {
        text: String,
        candidates: Vec<String>,
    },
    NoMatch {
        text: String,
        reason: String,
    },
}

impl Completion {
    /// The draft as completion left it, byte-for-byte the input when
    /// nothing matched.
    pub fn text(&self) -> &str {
        match self {
            Completion::Completed { text, .. } | Completion::NoMatch { text, .. } => text,
        }
    }

    /// The candidates the prefix left open; empty when the match was
    /// unique (a settled line) or when nothing matched.
    pub fn candidates(&self) -> &[String] {
        match self {
            Completion::Completed { candidates, .. } => candidates,
            Completion::NoMatch { .. } => &[],
        }
    }

    /// What to tell the user; empty for a completed line, which the new
    /// draft already says.
    pub fn reason(&self) -> &str {
        match self {
            Completion::NoMatch { reason, .. } => reason,
            Completion::Completed { .. } => "",
        }
    }
}

/// Commands whose first argument is a provider id, and the separator each
/// one completes that id with. `/model` takes `provider/model`, so its
/// provider completes to the `/` and the model half is typed by hand —
/// `auth` keeps no offline catalog of model ids, and inventing one here
/// would be a guess dressed as metadata.
const PROVIDER_ARGUMENTS: &[(&str, &str)] = &[("login", ""), ("logout", ""), ("model", "/")];

/// Complete the composer draft. Pure: it reads no secret, mutates no
/// configuration, and never submits or clears the draft.
///
/// One press on a prefix with a single match completes the line. A prefix
/// with several matches completes to their longest common prefix and
/// REPORTS the candidates, because auto-picking one of them would run a
/// command the user did not type; pressing again at that prefix adds
/// nothing and reports the same list, so the behaviour is deterministic.
/// A prefix with no match leaves the draft exactly as typed and says why.
pub fn complete(input: &str) -> Completion {
    let Some(body) = input.strip_prefix('/') else {
        return no_match(input, "completion starts at /");
    };
    // The command name runs to the first space; what follows is the
    // argument, whose last whitespace-delimited token is the one being
    // typed.
    let (name, rest) = match body.find(char::is_whitespace) {
        Some(i) => (&body[..i], &body[i..]),
        None => (body, ""),
    };
    if rest.is_empty() {
        // Still typing the name: the candidates are the registry, and the
        // token carries the leading `/` the user typed, so the replaced
        // text is the candidate verbatim.
        return finish(input, "", &format!("/{name}"), command_candidates(), "");
    }
    let token = rest.rsplit(char::is_whitespace).next().unwrap_or("");
    let head = &rest[..rest.len() - token.len()];
    if !head.trim().is_empty() {
        // A second argument is not completed: only the first is metadata
        // this process actually knows, and a guess past it would be a
        // command the user never typed.
        return no_match(
            input,
            &format!("only the first argument of /{name} completes; the rest is yours"),
        );
    }
    match PROVIDER_ARGUMENTS.iter().find(|(cmd, _)| *cmd == name) {
        Some((_, separator)) => finish(
            input,
            &format!("/{name}{head}"),
            token,
            provider_candidates(),
            separator,
        ),
        None => no_match(input, &format!("/{name} takes no provider to complete")),
    }
}

/// Replace the token being typed with the best answer the candidates allow.
fn finish(
    input: &str,
    prefix: &str,
    token: &str,
    candidates: Vec<String>,
    separator: &str,
) -> Completion {
    let matches: Vec<String> = candidates
        .into_iter()
        .filter(|c| c.starts_with(token))
        .collect();
    match matches.len() {
        0 => no_match(input, &format!("nothing completes {prefix}{token}")),
        1 => Completion::Completed {
            text: format!("{prefix}{}{separator}", matches[0]),
            candidates: Vec::new(),
        },
        _ => {
            // As far as the candidates agree, so far the line is written;
            // where they differ, the user is told rather than guessed at.
            let common = common_prefix(&matches);
            let written = if common.len() > token.len() {
                common
            } else {
                token.to_string()
            };
            Completion::Completed {
                text: format!("{prefix}{written}"),
                candidates: matches,
            }
        }
    }
}

/// The longest prefix every candidate shares.
fn common_prefix(candidates: &[String]) -> String {
    let mut chars: Vec<char> = candidates[0].chars().collect();
    for candidate in &candidates[1..] {
        let keep = chars
            .iter()
            .zip(candidate.chars())
            .take_while(|(a, b)| **a == *b)
            .count();
        chars.truncate(keep);
    }
    chars.into_iter().collect()
}

fn no_match(input: &str, reason: &str) -> Completion {
    Completion::NoMatch {
        text: input.to_string(),
        reason: reason.to_string(),
    }
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
