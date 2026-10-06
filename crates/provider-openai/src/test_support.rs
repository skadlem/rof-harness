use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::atomic::AtomicUsize;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use provider_core::{ProviderMessage, Request, Thinking};

use crate::client::{EndpointProfile, OpenAiCompat};

pub fn req_with_tool() -> Request {
    Request {
        messages: vec![
            ProviderMessage {
                role: "system".into(),
                content: "sys".into(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                thinking: None,
            },
            ProviderMessage {
                role: "user".into(),
                content: "hi".into(),
                tool_calls: Vec::new(),
                tool_call_id: None,
                thinking: None,
            },
        ],
        tools: vec![tool_core::ToolDeclaration {
            name: "read".into(),
            description: "read a file".into(),
            schema: serde_json::json!({
                "type": "object",
                "required": ["path"],
                "additionalProperties": false,
                "properties": {"path": {"type": "string"}}
            }),
        }],
        max_tokens: 64,
        thinking: Thinking::Auto,
        extras: serde_json::Value::Null,
    }
}

pub struct Canned {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

pub fn serve(canned: Vec<Canned>) -> (String, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = l.local_addr().unwrap().to_string();
    let bodies: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let b2 = bodies.clone();
    std::thread::spawn(move || {
        let hits = Arc::new(AtomicUsize::new(0));
        let _ = &hits;
        let mut n = 0usize;
        for stream in l.incoming() {
            let Ok(s) = stream else { break };
            s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut head = String::new();
            let mut len = 0usize;
            loop {
                let mut line = String::new();
                if r.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                if line == "\r\n" {
                    break;
                }
                head.push_str(&line);
                if let Some(v) = line
                    .strip_prefix("Content-Length:")
                    .or_else(|| line.strip_prefix("content-length:"))
                {
                    len = v.trim().parse().unwrap_or(0);
                }
            }
            let mut buf = vec![0u8; len];
            if len > 0 {
                r.read_exact(&mut buf).unwrap_or(());
            }
            b2.lock()
                .unwrap()
                .push(String::from_utf8_lossy(&buf).to_string());
            let c = &canned[n.min(canned.len() - 1)];
            let reason = match c.status {
                200 => "OK",
                429 => "Too Many Requests",
                502 => "Bad Gateway",
                _ => "Error",
            };
            let mut resp = format!(
                "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                c.status,
                reason,
                c.body.len()
            );
            for (hk, hv) in &c.headers {
                resp.push_str(&format!("{hk}: {hv}\r\n"));
            }
            resp.push_str("\r\n");
            let _ = r
                .into_inner()
                .write_all(format!("{resp}{}", c.body).as_bytes());
            n += 1;
            if n >= canned.len() + 2 {
                break;
            }
        }
    });
    (format!("http://{addr}"), bodies)
}

pub fn ok_body(text: &str) -> String {
    serde_json::json!({
        "choices": [{"message": {"content": text}, "finish_reason": "stop"}],
        "usage": {"prompt_tokens": 1, "completion_tokens": 2}
    })
    .to_string()
}

pub fn trunc_body(content: &str, reasoning: &str) -> String {
    serde_json::json!({
        "choices": [{
            "message": {"content": content, "reasoning_content": reasoning},
            "finish_reason": "length"
        }],
        "usage": {"prompt_tokens": 5, "completion_tokens": 64}
    })
    .to_string()
}

pub fn client_on(endpoint: &str, key_env: &str) -> OpenAiCompat {
    OpenAiCompat::new(endpoint, EndpointProfile::default()).with_key_env(key_env)
}
