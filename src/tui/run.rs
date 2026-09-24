//! Terminal runner for `rof chat`: alternate-screen event pump plus a
//! `--replay <trace.jsonl>` mode that renders a recorded trace with no live
//! loop. The live loop lands in Plan C; this file never touches the
//! orchestrator.

use std::time::Duration;

use crossterm::{
    event::{self, Event, KeyCode, KeyModifiers},
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
    ExecutableCommand,
};
use ratatui::{backend::CrosstermBackend, Terminal};

use super::app::App;
use super::cmd::Action;
use super::render::parse_lenient_line;
use super::splash;
use super::ui::draw;
use crate::obs::TraceEvent;

/// What the idle console hands back to main: the next goal to run, or quit.
/// Main owns the goal loop (`run_loop` blocks), so this enum is the whole
/// contract between the prompt UI and the goal runner.
pub enum LiveOut {
    Goal(String),
    Quit,
}

/// Live session: poll the shared trace, render at ~30fps, queue one goal.
/// Goal N+1 starts when goal N's run_loop future resolves. Slash actions
/// apply between goals via session-local env overrides (the loop reads the
/// same vars at call time, so no orchestrator changes).
pub fn run_live(trace: &crate::obs::TraceSink) -> anyhow::Result<LiveOut> {
    use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
    use crossterm::ExecutableCommand;
    // NOTE: the plan sketch shows `Result<()>` here, but main matches on
    // `LiveOut`, so this returns the outcome — otherwise nothing compiles.
    enable_raw_mode()?;
    std::io::stdout().execute(EnterAlternateScreen)?;
    let out = run_live_inner(trace);
    disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;
    out
}

/// Idle prompt between goals. Owns `App`, a `shown` cursor into
/// `trace.events()`, and a single-slot `pending` goal buffer: Enter queues
/// one goal, typing + Enter replaces it — never a list.
fn run_live_inner(trace: &crate::obs::TraceSink) -> anyhow::Result<LiveOut> {
    let backend = CrosstermBackend::new(std::io::stdout());
    let mut terminal = Terminal::new(backend)?;
    let mut app = App::new();
    app.transcript
        .push("rof chat — type a goal, or /help for commands.".to_string());
    let mut shown: usize = 0;
    let mut pending: Option<String> = None;
    let mut awaiting_key: Option<String> = None;
    // Double-press quit: the goal call blocks and is not interruptible, so
    // the first press only arms (with an indicator line naming that fact)
    // and the second press exits. Any other key disarms.
    let mut quit_armed = false;
    loop {
        for ev in trace.events().iter().skip(shown) {
            app.on_event(ev);
            shown += 1;
        }
        terminal.draw(|f| {
            if app.fresh {
                splash::draw(f);
            } else {
                draw(f, &app);
            }
        })?;
        if !event::poll(Duration::from_millis(33))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            // The splash consumes the first keypress outright.
            if app.fresh {
                app.fresh = false;
                continue;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
            {
                if quit_armed {
                    return Ok(LiveOut::Quit);
                }
                quit_armed = true;
                app.transcript.push(quit_hint());
                continue;
            }
            match key.code {
                KeyCode::Esc => {
                    if quit_armed {
                        return Ok(LiveOut::Quit);
                    }
                    quit_armed = true;
                    app.transcript.push(quit_hint());
                }
                // `q` quits only on an empty composer so goal text containing
                // `q` stays typeable (run_repl quits on any `q`; the console
                // cannot afford that).
                KeyCode::Char('q') if key.modifiers.is_empty() && app.input.is_empty() => {
                    if quit_armed {
                        return Ok(LiveOut::Quit);
                    }
                    quit_armed = true;
                    app.transcript.push(quit_hint());
                }
                KeyCode::Char(c)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    quit_armed = false;
                    app.input.push(c);
                }
                KeyCode::Backspace => {
                    quit_armed = false;
                    app.input.pop();
                }
                KeyCode::Up => app.scroll_lines(1),
                KeyCode::Down => app.scroll_lines(-1),
                KeyCode::PageUp => app.scroll_lines(10),
                KeyCode::PageDown => app.scroll_lines(-10),
                KeyCode::Home => app.scroll_lines(isize::MAX),
                KeyCode::End => app.scroll_lines(isize::MIN),
                KeyCode::Enter => {
                    quit_armed = false;
                    let text = std::mem::take(&mut app.input).trim().to_string();
                    if text.is_empty() {
                        if let Some(goal) = pending.take() {
                            return Ok(LiveOut::Goal(goal));
                        }
                        continue;
                    }
                    // A pending /login key capture wins over goal queueing:
                    // the next non-slash line is the key (never echoed,
                    // never queued as a goal). A slash line cancels it.
                    if let Some(prov) = awaiting_key.take() {
                        if text.starts_with('/') {
                            app.transcript
                                .push(format!("/login {prov} cancelled — key was not captured"));
                        } else {
                            match verify_blocking(&prov, text.trim()) {
                                Ok(ids) => {
                                    if let Err(e) = super::auth::store().save(&prov, text.trim()) {
                                        app.transcript
                                            .push(format!("login verified but save failed: {e}"));
                                    } else {
                                        app.transcript.push(format!(
                                            "logged in {prov} ({} models) — applies to the next goal",
                                            ids.len()
                                        ));
                                    }
                                }
                                Err(e) => app.transcript.push(format!("login failed: {e}")),
                            }
                            continue;
                        }
                    }
                    match super::cmd::parse(&text) {
                        None => {
                            pending = Some(text.clone());
                            app.transcript.push(format!(
                                "queued (single slot): {text} — Enter runs it, typing + Enter replaces it"
                            ));
                        }
                        Some(action) => {
                            if apply_action(&mut app, trace, action, &text, &mut awaiting_key) {
                                return Ok(LiveOut::Quit);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn quit_hint() -> String {
    "quit armed — press q/Esc/Ctrl-C again to exit (a running goal is a blocking call and cannot be interrupted mid-goal)".to_string()
}

fn env_present(k: &str) -> bool {
    std::env::var(k)
        .map(|v| !v.trim().is_empty())
        .unwrap_or(false)
}

/// Run the async key check from the sync prompt pump. Inside `chat` this
/// runs on a tokio worker, so `block_in_place` hands the thread back while
/// the verify call is in flight; outside a runtime a throwaway
/// current-thread runtime does the same job.
fn verify_blocking(provider: &str, key: &str) -> Result<Vec<String>, String> {
    match tokio::runtime::Handle::try_current() {
        Ok(h) => tokio::task::block_in_place(|| h.block_on(super::auth::verify(provider, key))),
        Err(_) => tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| e.to_string())?
            .block_on(super::auth::verify(provider, key)),
    }
}

/// Apply a slash action between goals. Display-only actions render into the
/// transcript; knob actions set the same env names the loop reads at call
/// time; `/model` re-points the model vars and `/login` verifies + saves,
/// both taking effect on the next goal because main rebuilds the config
/// and the provider clients per goal. `raw` is the full composer line —
/// `parse()` keeps only the first token, so the two-token `/model ctx`
/// and `/login <prov> <key>` forms read from here. Returns true on quit.
fn apply_action(
    app: &mut App,
    trace: &crate::obs::TraceSink,
    action: Action,
    raw: &str,
    awaiting_key: &mut Option<String>,
) -> bool {
    match action {
        Action::Quit => return true,
        Action::Help => app.transcript.push(super::cmd::help_text()),
        Action::Unknown(msg) => {
            app.transcript.push(msg);
            app.transcript.push(super::cmd::help_text());
        }
        Action::Models => {
            let logged = super::auth::store().providers();
            let st = super::auth::statuses();
            if logged.is_empty() {
                app.transcript.push(
                    "models: no provider logins in the credentials store (env keys still apply at call time)"
                        .to_string(),
                );
            } else {
                for p in &logged {
                    let s = st
                        .get(p)
                        .cloned()
                        .unwrap_or_else(|| "unverified".to_string());
                    app.transcript.push(format!("models: {p} [store, {s}]"));
                }
            }
            let mut envp: Vec<String> = Vec::new();
            if env_present("OR_TOKEN") {
                envp.push("openrouter (env OR_TOKEN)".to_string());
            }
            if env_present("ROF_TOKEN") {
                let base = std::env::var("ROF_CHAT_BASE").unwrap_or_default();
                envp.push(if base.trim().is_empty() {
                    "compat (env ROF_TOKEN)".to_string()
                } else {
                    format!("compat (env ROF_TOKEN + {base})")
                });
            }
            if !envp.is_empty() {
                app.transcript
                    .push(format!("models: env: {}", envp.join(", ")));
            }
        }
        Action::Context => app.transcript.push(format!(
            "context: {} | transcript lines: {}",
            app.status_line(),
            app.transcript.len()
        )),
        Action::Trace => app
            .transcript
            .push(format!("trace events: {}", trace.len())),
        Action::Hotkeys => app
            .transcript
            .push("hotkeys: q/Esc/Ctrl-C (twice) quit · Enter send · Backspace delete".to_string()),
        Action::Diff => app
            .transcript
            .push("diff view is not available in this console yet".to_string()),
        Action::Attempts(n) => {
            std::env::set_var("ROF_ATTEMPTS", n.to_string());
            app.transcript.push(format!(
                "attempts={n} (ROF_ATTEMPTS, applies to the next goal)"
            ));
        }
        Action::Rounds(n) => {
            std::env::set_var("ROF_MAX_ROUNDS", n.to_string());
            app.transcript.push(format!(
                "rounds={n} (ROF_MAX_ROUNDS, applies to the next goal)"
            ));
        }
        Action::Thinking(m) => {
            std::env::set_var("ROF_THINKING", &m);
            app.transcript.push(format!(
                "thinking={m} (ROF_THINKING, applies to the next goal)"
            ));
        }
        Action::Effort(m) => {
            std::env::set_var("ROF_REASONING_EFFORT", &m);
            app.transcript.push(format!(
                "effort={m} (ROF_REASONING_EFFORT, applies to the next goal)"
            ));
        }
        Action::Caps(a, b) => {
            std::env::set_var("ROF_IMPLEMENTER_MAX_TOKENS", a.to_string());
            std::env::set_var("ROF_REVIEWER_MAX_TOKENS", b.to_string());
            app.transcript.push(format!(
                "caps implementer={a} reviewer={b} (applies to the next goal)"
            ));
        }
        Action::Model(_) => {
            // Two-token `ctx` form rides the raw line: parse() keeps only
            // the first token, so Model("ctx") alone never names a model.
            let toks: Vec<&str> = raw.split_whitespace().collect();
            match toks.as_slice() {
                [_, "ctx", id] => {
                    std::env::set_var("ROF_CTX_MODEL", id);
                    app.transcript.push(format!(
                        "context model={id} (ROF_CTX_MODEL, applies to the next goal — clients rebuild per goal)"
                    ));
                }
                [_, spec] => {
                    if spec.contains('/') {
                        std::env::set_var("ROF_EXEC_MODEL", spec);
                        app.transcript.push(format!(
                            "model={spec} (ROF_EXEC_MODEL, applies to the next goal — clients rebuild per goal)"
                        ));
                    } else {
                        app.transcript
                            .push("/model needs <provider>/<model-id>".to_string());
                        app.transcript.push(super::cmd::help_text());
                    }
                }
                _ => {
                    app.transcript.push(
                        "/model needs <provider>/<model-id> (or /model ctx <provider>/<model-id>)"
                            .to_string(),
                    );
                }
            }
        }
        Action::Login(_) => {
            let toks: Vec<&str> = raw.split_whitespace().collect();
            match toks.as_slice() {
                [_, prov, key] => match verify_blocking(prov, key) {
                    Ok(ids) => match super::auth::store().save(prov, key) {
                        Ok(()) => app.transcript.push(format!(
                            "logged in {prov} ({} models) — applies to the next goal",
                            ids.len()
                        )),
                        Err(e) => app
                            .transcript
                            .push(format!("login verified but save failed: {e}")),
                    },
                    Err(e) => app.transcript.push(format!("login failed: {e}")),
                },
                [_, prov] => {
                    *awaiting_key = Some((*prov).to_string());
                    app.transcript.push(format!(
                        "/login {prov}: type the key and press Enter (input will echo; private terminal assumed)"
                    ));
                }
                _ => app
                    .transcript
                    .push("/login needs <provider> (openrouter/go/atria/custom)".to_string()),
            }
        }
        Action::Logout(p) => match super::auth::store().remove(&p) {
            Ok(true) => app.transcript.push(format!("logged out {p}")),
            Ok(false) => app.transcript.push(format!(
                "no login for {p} (env keys still apply at call time)"
            )),
            Err(e) => app.transcript.push(format!("logout failed: {e}")),
        },
        Action::Undo => app.transcript.push(
            "per-task rollback is automatic; cross-goal undo lands with Plan C Task 4".to_string(),
        ),
        Action::Retry(note) => app.transcript.push(format!(
            "retry is not available between goals yet{}",
            note.map(|n| format!(": {n}")).unwrap_or_default()
        )),
        Action::Approve(id) => app
            .transcript
            .push(format!("no pending proposal {id} in this console yet")),
        Action::Reject(id) => app
            .transcript
            .push(format!("no pending proposal {id} in this console yet")),
        Action::Display(m) => app.transcript.push(format!(
            "display={m}: single fullscreen view in this console"
        )),
        Action::Busy(m) => app.transcript.push(format!(
            "busy={m}: one goal at a time; Enter queues a single pending goal"
        )),
    }
    false
}

/// Render a recorded trace file in the fullscreen console. `q`/Esc/Ctrl-C
/// quits; anything else edits the (replay-inert) composer.
pub fn replay(path: &std::path::Path) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut app = App::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        match serde_json::from_str::<TraceEvent>(line) {
            Ok(ev) => app.on_event(&ev),
            Err(_) => app.transcript.push(parse_lenient_line(line)),
        }
    }
    run_repl(app)
}

/// Alternate-screen event pump: 30fps render cap, `q`/Esc/Ctrl-C quits,
/// chars/backspace/enter drive the composer buffer.
fn run_repl(mut app: App) -> anyhow::Result<()> {
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    stdout.execute(EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let result = pump(&mut terminal, &mut app);

    disable_raw_mode()?;
    std::io::stdout().execute(LeaveAlternateScreen)?;
    result
}

fn pump(
    terminal: &mut Terminal<CrosstermBackend<std::io::Stdout>>,
    app: &mut App,
) -> anyhow::Result<()> {
    loop {
        terminal.draw(|f| {
            if app.fresh {
                splash::draw(f);
            } else {
                draw(f, app);
            }
        })?;
        if !event::poll(Duration::from_millis(33))? {
            continue;
        }
        if let Event::Key(key) = event::read()? {
            // The splash consumes the first keypress outright.
            if app.fresh {
                app.fresh = false;
                continue;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL)
                && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
            {
                break;
            }
            match key.code {
                KeyCode::Esc => break,
                KeyCode::Char('q') if key.modifiers.is_empty() => break,
                KeyCode::Char(c)
                    if key.modifiers.is_empty() || key.modifiers == KeyModifiers::SHIFT =>
                {
                    app.input.push(c);
                }
                KeyCode::Backspace => {
                    app.input.pop();
                }
                KeyCode::Up => app.scroll_lines(1),
                KeyCode::Down => app.scroll_lines(-1),
                KeyCode::PageUp => app.scroll_lines(10),
                KeyCode::PageDown => app.scroll_lines(-10),
                KeyCode::Home => app.scroll_lines(isize::MAX),
                KeyCode::End => app.scroll_lines(isize::MIN),
                KeyCode::Enter => {
                    app.input.clear();
                }
                _ => {}
            }
        }
    }
    Ok(())
}
