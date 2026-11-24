//! `rof chat`: one `agent_loop::run` per stdin line over a single `LoopState`
//! (history, log and counters carry over); an emitter tap prints as it goes.

use std::collections::HashMap;
use std::io::{BufRead, Write};
use std::sync::{Arc, Mutex};

use agent_event::AgentEvent;
use agent_loop::{FailureKind, Input, NoBets, Outcome, Run};
use provider_core::LlmClient;

use crate::cli::{budget_for, run_config, Args};
use crate::exit::exit_code;
use crate::SessionBootstrap;

type Out = Arc<Mutex<dyn Write + Send>>;

fn say(out: &Out, s: &str) {
    if let Ok(mut o) = out.lock() {
        let _ = o.write_all(s.as_bytes());
        let _ = o.flush();
    }
}

/// Assistant text as it settles, plus one `name(args) ok|error` line per call.
fn tap(out: Out, streamed: bool) -> impl Fn(&AgentEvent) + Send + 'static {
    let calls = Mutex::new(HashMap::new());
    move |e| match e {
        AgentEvent::MessageEnd { message, .. } if !streamed && !message.content.is_empty() => {
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
    let SessionBootstrap {
        policy,
        registry: reg,
        mut agent_tree,
        mut state,
        gate,
        mut emitter,
        cancel,
        ..
    } = match crate::bootstrap_session(args, false).await {
        Ok(s) => s,
        Err((kind, message)) => {
            eprintln!("{message}");
            return exit_code(&Outcome::Failed { kind, message });
        }
    };
    let bets: &dyn agent_loop::BetsHook = if args.bets { &gate } else { &NoBets };
    emitter.on(tap(out.clone(), args.stream));
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
        let outcome = crate::agents::with_children(
            agent_tree.as_mut(),
            provider,
            args,
            &policy,
            agent_loop::run(
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
            ),
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

    struct Streamer(ScriptClient);

    #[async_trait::async_trait]
    impl LlmClient for Streamer {
        async fn complete(
            &self,
            model: &str,
            req: &provider_core::Request,
        ) -> Result<provider_core::Response, provider_core::LlmError> {
            self.0.complete(model, req).await
        }
        async fn complete_streaming(
            &self,
            model: &str,
            req: &provider_core::Request,
            sink: &mut (dyn FnMut(provider_core::Delta) + Send),
        ) -> Result<provider_core::Response, provider_core::LlmError> {
            sink(provider_core::Delta::Text("all ".into()));
            sink(provider_core::Delta::Text("good".into()));
            self.0.complete(model, req).await
        }
    }

    #[tokio::test]
    async fn stream_prints_deltas_once_not_again_on_message_end() {
        let dir = tmp();
        let inner = Streamer(ScriptClient::new(vec![text_resp()]));
        let mut args = args_for(&dir, None);
        args.stream = true;
        args.goal = String::new();
        let buf = Arc::new(Mutex::new(Vec::new()));
        let p = crate::StreamPrint {
            inner: &inner,
            out: buf.clone(),
        };
        let code = run_chat(&p, &args, "q\n".as_bytes(), buf.clone()).await;
        let text = String::from_utf8(buf.lock().unwrap().clone()).unwrap();
        assert_eq!(code, 0);
        assert_eq!(text.matches("all good").count(), 1, "{text}");
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
