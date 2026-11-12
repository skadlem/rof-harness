//! `rof chat`: one `agent_loop::run` per stdin line over a single `LoopState`
//! (history, log and counters carry over); an emitter tap prints as it goes.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use agent_event::{AgentEvent, Emitter};
use agent_loop::{FailureKind, Input, LoopState, NoBets, Outcome, Run};
use provider_core::LlmClient;
use tokio_util::sync::CancellationToken;

use crate::cli::{budget_for, run_config, Args};
use crate::exit::exit_code;
use crate::workdir::{exclude_sidecar, resolve_log_path};
use crate::{policy_for, preserve_log, registry_for, Gate};

type Out = Arc<Mutex<dyn Write + Send>>;

fn say(out: &Out, s: &str) {
    if let Ok(mut o) = out.lock() {
        let _ = o.write_all(s.as_bytes());
        let _ = o.flush();
    }
}

/// Assistant text as it settles, plus one `name(args) ok|error` line per call.
fn tap(out: Out) -> impl Fn(&AgentEvent) + Send + 'static {
    let calls = Mutex::new(HashMap::new());
    move |e| match e {
        AgentEvent::MessageEnd { message, .. } if !message.content.is_empty() => {
            say(&out, &format!("{}\n", message.content));
        }
        AgentEvent::ToolStart { id, name, args } => {
            let a: String = args.to_string().chars().take(80).collect();
            if let Ok(mut c) = calls.lock() {
                c.insert(id.clone(), format!("{name}({a})"));
            }
        }
        AgentEvent::ToolEnd { id, is_error, .. } => {
            let call = calls.lock().ok().and_then(|mut c| c.remove(id));
            let status = if *is_error { "error" } else { "ok" };
            say(
                &out,
                &format!("[tool] {} {status}\n", call.unwrap_or_default()),
            );
        }
        _ => {}
    }
}

/// Exit code: 0 on `/exit` or EOF; a cancel or a log/snapshot failure ends
/// the session with the matching `rof run` code. Halts and provider failures
/// are printed and the session continues.
pub(crate) async fn run_chat<P: LlmClient>(
    provider: &P,
    args: &Args,
    mut input: impl BufRead,
    out: Out,
) -> i32 {
    let reg = match registry_for(args, Arc::new(policy_for(args))) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("cannot configure tools: {e}");
            return 3;
        }
    };
    let mut state = LoopState::new();
    if let Some(p) = args.resume.as_deref() {
        if let Err(e) = state.restore_from(std::path::Path::new(p)) {
            eprintln!("--resume {p}: {e}");
            return 5;
        }
    }
    let gate = Gate {
        incremental: args.proof_cmd.is_some(),
    };
    let bets: &dyn agent_loop::BetsHook = if args.bets { &gate } else { &NoBets };
    let mut emitter = Emitter::new();
    emitter.on(tap(out.clone()));
    let cancel = CancellationToken::new();
    tokio::spawn({
        let cancel = cancel.clone();
        async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                cancel.cancel();
            }
        }
    });
    // Same sidecar rules as `run`: a fresh session archives the old WAL, a
    // resumed one keeps and appends to it.
    let log_path = resolve_log_path(args);
    if let (Some(p), None) = (log_path.as_deref(), args.resume.as_deref()) {
        if let Err(e) = preserve_log(p, &args.workdir) {
            eprintln!("cannot preserve WAL: {e}");
            return 5;
        }
    }
    let _ = snapshot::TreeService::new(&args.workdir).ensure();
    if let Some(p) = log_path.as_deref() {
        exclude_sidecar(&args.workdir, p);
    }
    let mut next = Some(args.goal.clone()).filter(|g| !g.is_empty());
    loop {
        let msg = match next.take() {
            Some(m) => m,
            None => {
                say(&out, "> ");
                let mut line = String::new();
                if cancel.is_cancelled() || input.read_line(&mut line).unwrap_or(0) == 0 {
                    return 0;
                }
                match line.trim() {
                    "" => continue,
                    "/exit" => return 0,
                    m => m.to_string(),
                }
            }
        };
        state.reset_for_input();
        state.budget = match budget_for(args) {
            // per user message, not per session
            Ok(b) => b,
            Err(e) => {
                eprintln!("{e}");
                return 2;
            }
        };
        let outcome = agent_loop::run(
            &mut state,
            Run {
                provider,
                registry: &reg,
                agent: "agent",
                workdir: &args.workdir,
                emitter: &mut emitter,
                bets,
                cfg: run_config(args),
            },
            vec![Input::User(msg)],
            &cancel,
        )
        .await;
        match &outcome {
            Outcome::Done => {}
            Outcome::Cancelled
            | Outcome::Failed {
                kind: FailureKind::Log | FailureKind::Snapshot,
                ..
            } => {
                say(&out, &format!("[{outcome:?}]\n"));
                return exit_code(&outcome);
            }
            o => say(&out, &format!("[{o:?}]\n")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::execute;
    use crate::fixtures::{args_for, edit_resp, text_resp, tmp, ScriptClient};
    use agent_log::{append_item, read_log, InputSource, Item, ItemKind, TurnEndReason};

    async fn chat(
        client: &ScriptClient,
        dir: &std::path::Path,
        goal: &str,
        stdin: &str,
    ) -> (i32, String) {
        let mut args = args_for(dir, None);
        args.goal = goal.into();
        let buf = Arc::new(Mutex::new(Vec::new()));
        let code = run_chat(client, &args, stdin.as_bytes(), buf.clone()).await;
        let text = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        (code, text)
    }

    fn has(msgs: &[provider_core::ProviderMessage], role: &str, needle: &str) -> bool {
        msgs.iter()
            .any(|m| m.role == role && m.content.contains(needle))
    }

    #[tokio::test]
    async fn second_line_sees_first_exchange_and_exit_is_clean() {
        let dir = tmp();
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        let client = ScriptClient::new(vec![text_resp(), text_resp()]);
        let (code, text) = chat(&client, &dir, "", "first q\nsecond q\n/exit\nnever read\n").await;
        assert_eq!(code, 0);
        let seen = client.seen.lock().unwrap();
        assert_eq!(seen.len(), 2, "one provider call per line, /exit stops");
        let second = &seen[1];
        assert!(has(second, "user", "first q") && has(second, "assistant", "all good"));
        assert!(has(second, "user", "second q"));
        assert!(text.contains("all good"), "assistant text printed: {text}");
    }

    #[tokio::test]
    async fn eof_exits_zero_and_goal_is_the_first_message() {
        let dir = tmp();
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        let client = ScriptClient::new(vec![text_resp()]);
        let (code, _) = chat(&client, &dir, "start here", "").await;
        assert_eq!(code, 0);
        assert!(has(&client.seen.lock().unwrap()[0], "user", "start here"));
    }

    #[tokio::test]
    async fn tool_call_line_is_printed() {
        let dir = tmp();
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        // edit, then a declare held by the verification nudge, then the real one.
        let client = ScriptClient::new(vec![
            edit_resp("note.txt", "hello", "bye"),
            text_resp(),
            text_resp(),
        ]);
        let (code, text) = chat(&client, &dir, "", "fix it\n").await;
        assert_eq!(code, 0);
        assert!(
            text.contains("[tool] edit(") && text.contains(" ok"),
            "{text}"
        );
    }

    fn item(seq: u64, kind: ItemKind) -> Item {
        Item {
            seq,
            id: format!("item-{seq}"),
            parent_id: None,
            recorded_at: std::time::SystemTime::now(),
            kind,
        }
    }

    #[tokio::test]
    async fn resume_carries_history_synthesizes_result_and_appends() {
        let dir = tmp();
        std::fs::write(dir.join("note.txt"), "hello\n").unwrap();
        let log = dir.join("old.jsonl");
        let asst = |text: &str, calls: serde_json::Value| ItemKind::Assistant {
            message: serde_json::json!({"content": text, "tool_calls": calls, "thinking": null}),
            stop_reason: "stop".into(),
            interrupted: false,
        };
        let input = |id: &str, text: &str| ItemKind::Input {
            input_id: id.into(),
            text: text.into(),
            source: InputSource::External,
        };
        let args_json = serde_json::json!({"path": "note.txt"});
        let kinds = vec![
            ItemKind::Header {
                version: agent_log::LOG_VERSION,
                session_id: "s".into(),
                cwd: "/w".into(),
                model: "m".into(),
            },
            ItemKind::TurnStart {
                turn_id: "turn-1".into(),
                prev_turn_id: None,
            },
            input("i1", "first q"),
            asst("first answer", serde_json::json!([])),
            ItemKind::TurnEnd {
                turn_id: "turn-1".into(),
                reason: TurnEndReason::Completed,
            },
            ItemKind::TurnStart {
                turn_id: "turn-2".into(),
                prev_turn_id: Some("turn-1".into()),
            },
            input("i2", "second q"),
            asst(
                "looking",
                serde_json::json!([{"id": "c9", "name": "view", "args": args_json}]),
            ),
            ItemKind::ToolCall {
                call_id: "c9".into(),
                tool: "view".into(),
                args: args_json,
            },
        ];
        for (i, k) in kinds.into_iter().enumerate() {
            append_item(&log, &item(i as u64 + 1, k)).unwrap();
        }
        let before = read_log(&log).unwrap().len();

        let client = ScriptClient::new(vec![text_resp()]);
        let mut args = args_for(&dir, None);
        args.goal = "continue".into();
        args.resume = Some(log.to_str().unwrap().into());
        let r = execute(&client, &args).await;
        assert!(matches!(r.outcome, Outcome::Done), "{:?}", r.outcome);

        let req = &client.seen.lock().unwrap()[0];
        assert!(has(req, "user", "first q") && has(req, "assistant", "first answer"));
        let tool = req
            .iter()
            .find(|m| m.role == "tool")
            .expect("synthesized result");
        assert_eq!(tool.tool_call_id.as_deref(), Some("c9"));
        assert!(tool.content.contains("outcome is unknown"));
        assert!(has(req, "user", "continue"));

        let after = read_log(&log).unwrap(); // also checks seq stays gapless
        assert!(after.len() > before, "new items append to the same log");
        assert!(after
            .iter()
            .any(|i| matches!(&i.kind, ItemKind::Input { text, .. } if text == "continue")));
        let headers = after
            .iter()
            .filter(|i| matches!(i.kind, ItemKind::Header { .. }))
            .count();
        assert_eq!(headers, 1);
    }
}
