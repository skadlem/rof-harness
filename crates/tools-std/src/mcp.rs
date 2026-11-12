//! Minimal MCP client over stdio: one server's tools become harness tools
//! named `mcp__<tool>`. Opt-in: nothing happens unless the integrator calls
//! [`register_mcp_stdio`].
//!
//! The server runs arbitrary external code, so registration demands
//! `ExecPolicy::Any` (the same "caller owns the process boundary" bit that
//! skips the exec allowlist). Per-agent access still goes through the
//! Registry's gate: grant the returned names like any other tool.
//! The child lives as long as any registered tool (the Registry) does.
//! shortcut: stdout lines are read unbounded and stderr is discarded; add a
//! line cap / stderr log if a hostile or chatty server shows up.

use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{ChildStdin, ChildStdout};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tool_core::{
    bound_text, CallStatus, Invocation, Registry, Tool, ToolCall, ToolDefinition, ToolError,
    ToolOutcome, MAX_MODEL_CHARS,
};

use crate::common::{dispatch, EXEC_TIMEOUT};
use crate::policy::ExecPolicy;
use crate::runner::GroupGuard;

const SETUP_TIMEOUT: Duration = Duration::from_secs(30);
const DESC_CAP: usize = 1024;
const MAX_PAGES: usize = 100;

struct Io {
    stdin: ChildStdin,
    out: Lines<BufReader<ChildStdout>>,
    next_id: u64,
}

struct Client {
    io: Mutex<Io>,
    _guard: GroupGuard,
}

async fn send(stdin: &mut ChildStdin, msg: &Value) -> Result<(), String> {
    let mut line = msg.to_string();
    line.push('\n');
    stdin
        .write_all(line.as_bytes())
        .await
        .map_err(|e| format!("mcp write: {e}"))?;
    stdin.flush().await.map_err(|e| format!("mcp write: {e}"))
}

impl Client {
    fn spawn(argv: &[String]) -> Result<Self, String> {
        let (bin, rest) = argv.split_first().ok_or("empty mcp argv")?;
        let mut cmd = std::process::Command::new(bin);
        cmd.args(rest)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt as _;
            cmd.process_group(0); // GroupGuard kills the whole tree on drop
        }
        let mut child = tokio::process::Command::from(cmd)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| format!("mcp spawn {bin}: {e}"))?;
        let stdin = child.stdin.take().ok_or("mcp stdin")?;
        let out = BufReader::new(child.stdout.take().ok_or("mcp stdout")?).lines();
        Ok(Self {
            io: Mutex::new(Io {
                stdin,
                out,
                next_id: 0,
            }),
            _guard: GroupGuard {
                pgid: child.id(),
                child: Some(child),
            },
        })
    }

    async fn notify(&self, method: &str) -> Result<(), String> {
        let mut io = self.io.lock().await;
        send(&mut io.stdin, &json!({"jsonrpc": "2.0", "method": method})).await
    }

    /// One request/response; calls are serialized by the mutex. A timed-out or
    /// cancelled request leaves its late reply in the pipe, skipped by id later.
    async fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<Value, String> {
        let mut io = self.io.lock().await;
        let Io {
            stdin,
            out,
            next_id,
        } = &mut *io;
        *next_id += 1;
        let id = *next_id;
        send(
            stdin,
            &json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}),
        )
        .await?;
        let wait = async {
            loop {
                let line = out
                    .next_line()
                    .await
                    .map_err(|e| format!("mcp read: {e}"))?
                    .ok_or("mcp server exited")?;
                let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                    continue; // log noise
                };
                match (msg.get("method").and_then(Value::as_str), msg.get("id")) {
                    // Server->client request: never leave it hanging.
                    (Some(m), Some(rid)) => {
                        let reply = if m == "ping" {
                            json!({"jsonrpc": "2.0", "id": rid, "result": {}})
                        } else {
                            json!({"jsonrpc": "2.0", "id": rid,
                                   "error": {"code": -32601, "message": "method not found"}})
                        };
                        send(stdin, &reply).await?;
                    }
                    (None, Some(rid)) if *rid == json!(id) => return Ok::<Value, String>(msg),
                    _ => {} // notification or stale reply
                }
            }
        };
        let msg = tokio::select! {
            r = tokio::time::timeout(timeout, wait) => {
                r.map_err(|_| format!("mcp {method} timed out"))??
            }
            _ = cancel.cancelled() => return Err(format!("mcp {method} cancelled")),
        };
        if let Some(e) = msg.get("error") {
            return Err(e
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("rpc error")
                .to_string());
        }
        Ok(msg.get("result").cloned().unwrap_or(Value::Null))
    }
}

pub struct McpTool {
    client: Arc<Client>,
    def: ToolDefinition,
    mcp_name: String,
}

#[async_trait]
impl Tool for McpTool {
    fn definition(&self) -> ToolDefinition {
        self.def.clone()
    }
    fn prepare(&self, call: &ToolCall) -> CallStatus {
        dispatch(call)
    }
    async fn execute(
        &self,
        inv: Invocation,
        cancel: CancellationToken,
    ) -> Result<ToolOutcome, ToolError> {
        let res = self
            .client
            .request(
                "tools/call",
                json!({"name": self.mcp_name, "arguments": inv.args}),
                EXEC_TIMEOUT,
                &cancel,
            )
            .await
            .map_err(ToolError::Failed)?;
        let text = res
            .get("content")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .map(|c| match c.get("type").and_then(Value::as_str) {
                        Some("text") => c
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .to_string(),
                        Some(t) => format!("[{t} content omitted]"),
                        None => "[unknown content omitted]".to_string(),
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();
        let bounded = bound_text(&text, MAX_MODEL_CHARS);
        if res.get("isError") == Some(&Value::Bool(true)) {
            return Err(ToolError::Failed(bounded));
        }
        Ok(ToolOutcome {
            truncated: bounded != text,
            content: bounded,
            success: true,
        })
    }
}

/// `mcp__` + the server's name restricted to `[A-Za-z0-9_-]`, capped at 60
/// chars; `_2`, `_3`... on collision with anything already registered.
fn free_name(registry: &Registry, raw: &str) -> String {
    let clean: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let base: String = format!("mcp__{clean}").chars().take(60).collect();
    let mut name = base.clone();
    let mut n = 1;
    while registry.resolve(&name).is_some() {
        n += 1;
        name = format!("{base}_{n}");
    }
    name
}

/// Spawn the server, handshake, list its tools and register each as
/// `mcp__<tool>`. Returns the registered names (grant them in the gate).
/// Needs `ExecPolicy::Any`: an MCP server is arbitrary external code.
/// Keep the Registry alive; dropping it (last tool) kills the server.
/// The server's `inputSchema` is passed through; `validate_strict` only
/// enforces `required`/`type`/`additionalProperties:false` and ignores the rest.
pub async fn register_mcp_stdio(
    registry: &mut Registry,
    argv: Vec<String>,
    policy: ExecPolicy,
) -> Result<Vec<String>, String> {
    if policy != ExecPolicy::Any {
        return Err("mcp tools run arbitrary external code: requires exec policy any".to_string());
    }
    let client = Arc::new(Client::spawn(&argv)?);
    let never = CancellationToken::new();
    client
        .request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "rof", "version": env!("CARGO_PKG_VERSION")}}),
            SETUP_TIMEOUT,
            &never,
        )
        .await?;
    client.notify("notifications/initialized").await?;
    let mut names = Vec::new();
    let mut cursor: Option<String> = None;
    for _ in 0..MAX_PAGES {
        let params = cursor.as_ref().map_or(json!({}), |c| json!({"cursor": c}));
        let res = client
            .request("tools/list", params, SETUP_TIMEOUT, &never)
            .await?;
        for t in res
            .get("tools")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(raw) = t.get("name").and_then(Value::as_str) else {
                continue;
            };
            let name = free_name(registry, raw);
            let desc = t.get("description").and_then(Value::as_str).unwrap_or("");
            registry.register(Arc::new(McpTool {
                client: client.clone(),
                mcp_name: raw.to_string(),
                def: ToolDefinition {
                    name: name.clone(),
                    description: bound_text(desc, DESC_CAP),
                    schema: t
                        .get("inputSchema")
                        .cloned()
                        .unwrap_or(json!({"type": "object"})),
                },
            }));
            names.push(name);
        }
        cursor = res
            .get("nextCursor")
            .and_then(Value::as_str)
            .map(str::to_string);
        if cursor.is_none() {
            break;
        }
    }
    Ok(names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use tool_core::GrantGate;

    const SERVER: &str = r#"mode=$1
[ -n "$2" ] && echo $$ > "$2"
ECHO='{"name":"echo","description":"d","inputSchema":{"type":"object","properties":{"text":{"type":"string"}},"required":["text"],"additionalProperties":false}}'
reply() { echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"result\":$1}"; }
while IFS= read -r line; do
  id=${line#*\"id\":}; id=${id%%,*}
  case "$line" in
  *'"method":"initialize"'*) reply '{"protocolVersion":"2024-11-05","capabilities":{}}';;
  *'"method":"tools/list"'*)
    case $mode in
    page) case "$line" in
          *'"cursor":"p2"'*) reply '{"tools":[{"name":"b.c","inputSchema":{"type":"object"}},{"name":"echo"}]}';;
          *) reply "{\"tools\":[$ECHO],\"nextCursor\":\"p2\"}";;
          esac;;
    *) reply "{\"tools\":[$ECHO]}";;
    esac;;
  *'"method":"tools/call"'*)
    case $mode in
    iserror) reply '{"content":[{"type":"text","text":"boom"}],"isError":true}';;
    rpcerr) echo "{\"jsonrpc\":\"2.0\",\"id\":$id,\"error\":{\"code\":-32602,\"message\":\"bad params\"}}";;
    die) exit 0;;
    noise) echo 'log line, not json'
           echo '{"jsonrpc":"2.0","method":"notifications/message","params":{}}'
           echo '{"jsonrpc":"2.0","id":99,"method":"ping"}'
           read -r r
           case "$r" in *'"result":{}'*) reply '{"content":[{"type":"text","text":"hi"},{"type":"image","data":"x"}]}';;
                        *) reply '{"content":[{"type":"text","text":"bad ping reply"}],"isError":true}';; esac;;
    *) reply '{"content":[{"type":"text","text":"hello"}]}';;
    esac;;
  esac
done
"#;

    fn argv(mode: &str, pidfile: &str) -> Vec<String> {
        ["sh", "-c", SERVER, "sh", mode, pidfile]
            .map(String::from)
            .to_vec()
    }

    fn registry() -> Registry {
        Registry::new(Arc::new(GrantGate::new(HashMap::from([(
            "a".to_string(),
            vec!["mcp__echo".to_string()],
        )]))))
    }

    async fn call(r: &Registry, name: &str) -> Result<ToolOutcome, String> {
        let c = ToolCall {
            call_id: "1".into(),
            name: name.into(),
            args: json!({"text": "x"}),
        };
        let CallStatus::Dispatch(inv) = r.prepare("a", c) else {
            return Err("not dispatched".into());
        };
        r.resolve(name)
            .unwrap()
            .execute(inv, CancellationToken::new())
            .await
            .map_err(|e| e.to_string())
    }

    async fn setup(mode: &str) -> Registry {
        let mut r = registry();
        register_mcp_stdio(&mut r, argv(mode, ""), ExecPolicy::Any)
            .await
            .unwrap();
        r
    }

    #[tokio::test]
    async fn handshake_registers_tool_with_server_schema_and_call_returns_text() {
        let r = setup("ok").await;
        let d = r.definitions();
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].name, "mcp__echo");
        assert_eq!(d[0].schema["required"], json!(["text"]));
        assert_eq!(call(&r, "mcp__echo").await.unwrap().content, "hello");
    }

    #[tokio::test]
    async fn requires_exec_policy_any() {
        let mut r = registry();
        assert!(
            register_mcp_stdio(&mut r, argv("ok", ""), ExecPolicy::Allowlist)
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn is_error_and_rpc_error_become_errors() {
        let e = call(&setup("iserror").await, "mcp__echo")
            .await
            .unwrap_err();
        assert!(e.contains("boom"), "{e}");
        let e = call(&setup("rpcerr").await, "mcp__echo").await.unwrap_err();
        assert!(e.contains("bad params"), "{e}");
    }

    #[tokio::test]
    async fn server_exit_mid_call_is_an_error_not_a_hang() {
        let r = setup("die").await;
        let e = tokio::time::timeout(Duration::from_secs(5), call(&r, "mcp__echo"))
            .await
            .expect("hung")
            .unwrap_err();
        assert!(e.contains("exited"), "{e}");
    }

    #[tokio::test]
    async fn noise_notifications_and_server_requests_are_handled() {
        let out = call(&setup("noise").await, "mcp__echo").await.unwrap();
        assert_eq!(out.content, "hi\n[image content omitted]");
    }

    #[tokio::test]
    async fn pagination_bad_names_and_collisions() {
        let mut r = registry();
        let first = register_mcp_stdio(&mut r, argv("ok", ""), ExecPolicy::Any)
            .await
            .unwrap();
        assert_eq!(first, ["mcp__echo"]);
        // Two pages; `b.c` is sanitized, both `echo`s collide and get suffixed.
        let names = register_mcp_stdio(&mut r, argv("page", ""), ExecPolicy::Any)
            .await
            .unwrap();
        assert_eq!(names, ["mcp__echo_2", "mcp__b_c", "mcp__echo_3"]);
    }

    #[tokio::test]
    async fn child_is_gone_after_drop() {
        let pidfile = std::env::temp_dir().join(format!("mcp-pid-{}", std::process::id()));
        let mut r = registry();
        register_mcp_stdio(
            &mut r,
            argv("ok", pidfile.to_str().unwrap()),
            ExecPolicy::Any,
        )
        .await
        .unwrap();
        let pid = std::fs::read_to_string(&pidfile)
            .unwrap()
            .trim()
            .to_string();
        let alive = || {
            std::fs::read_to_string(format!("/proc/{pid}/stat"))
                .map(|s| !s.contains(") Z"))
                .unwrap_or(false)
        };
        assert!(alive());
        drop(r);
        for _ in 0..50 {
            if !alive() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("child survived drop");
    }
}
