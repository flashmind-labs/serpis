//! A local HTTP provider for tests. It speaks chat completions (OpenRouter's format included) and
//! Anthropic messages, runs every request through the [`Simulator`], and streams a scripted reply
//! whose usage carries the simulated cache reads and writes. Every request is logged.
//!
//! Point a model at it with `baseUrl` = `{base}/api/v1` (chat completions) or `{base}/api`
//! (Anthropic messages). `GET /control/advance/<seconds>` moves the simulator's clock and
//! `GET /control/move-upstream/<session id>` sends that session to another upstream.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::sim::{Config, Miss, Outcome, Simulator, Wire};

/// What the model answers.
#[derive(Debug, Clone)]
pub enum Reply {
    Text(String),
    /// Tool calls as (name, arguments), all in one response.
    ToolCalls(Vec<(String, Value)>),
    /// An HTTP error with this status and message.
    Error(u16, String),
}

pub type Script = Box<dyn FnMut(Wire, &Value) -> Reply + Send>;

#[derive(Debug, Clone)]
pub struct LogEntry {
    pub n: usize,
    pub wire: Wire,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Value,
    /// The simulator's verdict, or the 400 it would answer with.
    pub outcome: Result<Outcome, String>,
}

struct State {
    sim: Simulator,
    script: Script,
    log: Vec<LogEntry>,
    log_file: Option<PathBuf>,
}

#[derive(Clone)]
pub struct Mock {
    /// `http://127.0.0.1:<port>`
    pub base: String,
    state: Arc<Mutex<State>>,
}

/// The text of the last user turn, or None when the request ends with tool results.
pub fn last_user_text(wire: Wire, body: &Value) -> Option<String> {
    let last = body["messages"].as_array()?.last()?;
    let text = |c: &Value| match c {
        Value::String(s) => s.clone(),
        Value::Array(parts) => parts.iter().filter_map(|p| p["text"].as_str()).collect(),
        _ => String::new(),
    };
    match wire {
        Wire::ChatCompletions if last["role"] == "tool" => None,
        Wire::AnthropicMessages if last["content"].as_array().is_some_and(|a| a.iter().any(|b| b["type"] == "tool_result")) => None,
        _ => Some(text(&last["content"])),
    }
}

/// `READ <n>` reads f0.txt … f<n-1>.txt in one response, `SLEEP` runs `sleep 5`, `FAIL` answers
/// with an error, `OVERFLOW` fails the first time with a context-overflow error, a compaction
/// request gets a checkpoint summary, anything else gets "ok"; after tool results the model says
/// "done".
pub fn commands() -> Script {
    let mut overflowed = std::collections::HashSet::new();
    Box::new(move |wire, body| match last_user_text(wire, body) {
        None => Reply::Text("done".into()),
        Some(t) if t.contains("structured context checkpoint summary") => Reply::Text("## Goal\nKeep testing serpis.\n\n## Next Steps\n1. Carry on.".into()),
        Some(t) if t.starts_with("OVERFLOW") && overflowed.insert(t.clone()) => Reply::Error(400, "prompt is too long: 250000 tokens > 200000 maximum".into()),
        Some(t) if t.starts_with("READ ") => {
            let n: usize = t[5..].trim().parse().unwrap_or(1);
            Reply::ToolCalls((0..n).map(|i| ("read".to_string(), json!({"path": format!("f{i}.txt")}))).collect())
        }
        Some(t) if t.starts_with("SLEEP") => Reply::ToolCalls(vec![("bash".into(), json!({"command": "sleep 5"}))]),
        Some(t) if t.starts_with("FAIL") => Reply::Error(400, "synthetic failure".into()),
        Some(_) => Reply::Text("ok".into()),
    })
}

/// A miss as one word for logs: none, model, params, expired, upstream, lookback,
/// prefix:<segment>[<index>] or unknown.
pub fn miss_word(m: Option<&Miss>) -> String {
    match m {
        None => "none".into(),
        Some(Miss::Model) => "model".into(),
        Some(Miss::Params) => "params".into(),
        Some(Miss::Expired) => "expired".into(),
        Some(Miss::Upstream) => "upstream".into(),
        Some(Miss::Lookback) => "lookback".into(),
        Some(Miss::PrefixChanged { segment, index, .. }) => format!("prefix:{segment}[{index}]"),
        Some(Miss::Unknown) => "unknown".into(),
    }
}

impl Mock {
    pub async fn start(script: Script) -> Mock {
        Self::start_with(Config::default(), script).await
    }

    pub async fn start_with(config: Config, script: Script) -> Mock {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind mock provider");
        let base = format!("http://{}", listener.local_addr().unwrap());
        let state = Arc::new(Mutex::new(State { sim: Simulator::new(config), script, log: Vec::new(), log_file: None }));
        let st = state.clone();
        tokio::spawn(async move {
            while let Ok((sock, _)) = listener.accept().await {
                let st = st.clone();
                tokio::spawn(async move {
                    let _ = serve(sock, st).await;
                });
            }
        });
        Mock { base, state }
    }

    pub fn log(&self) -> Vec<LogEntry> {
        self.state.lock().unwrap().log.clone()
    }

    pub fn outcomes(&self) -> Vec<Outcome> {
        self.log().into_iter().filter_map(|e| e.outcome.ok()).collect()
    }

    /// Move the simulator's clock.
    pub fn advance(&self, d: Duration) {
        self.state.lock().unwrap().sim.advance(d);
    }

    pub fn with_sim<R>(&self, f: impl FnOnce(&mut Simulator) -> R) -> R {
        f(&mut self.state.lock().unwrap().sim)
    }

    pub fn set_script(&self, script: Script) {
        self.state.lock().unwrap().script = script;
    }

    /// Also append each request's verdict to `path` as one JSON line: {n, path, session, read,
    /// write, write_1h, input, marks, upstream, miss} (miss as [`miss_word`], or "rejected" with
    /// an `error`). Changed blocks are logged in full as `before` / `after`.
    pub fn log_to(&self, path: PathBuf) {
        self.state.lock().unwrap().log_file = Some(path);
    }
}

fn log_line(e: &LogEntry) -> Value {
    let session = e
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case("x-session-id"))
        .map(|(_, v)| v.clone())
        .or_else(|| e.body["prompt_cache_key"].as_str().map(str::to_string));
    match &e.outcome {
        Ok(o) => {
            let mut v = json!({"n": e.n, "path": e.path, "session": session, "read": o.read, "write": o.write, "write_1h": o.write_1h,
                               "input": o.input, "marks": o.marks, "upstream": o.upstream, "miss": miss_word(o.miss.as_ref())});
            if let Some(Miss::PrefixChanged { before, after, .. }) = &o.miss {
                v["before"] = json!(before);
                v["after"] = json!(after);
            }
            v
        }
        Err(err) => json!({"n": e.n, "path": e.path, "session": session, "miss": "rejected", "error": err}),
    }
}

async fn serve(mut sock: TcpStream, state: Arc<Mutex<State>>) -> anyhow::Result<()> {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 16384];
    let head_end = loop {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
    };
    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let path = lines.next().unwrap_or_default().split(' ').nth(1).unwrap_or_default().to_string();
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        .collect();
    let len: usize = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("content-length")).and_then(|(_, v)| v.parse().ok()).unwrap_or(0);
    while buf.len() < head_end + len {
        let n = sock.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        buf.extend_from_slice(&chunk[..n]);
    }
    if let Some(rest) = path.strip_prefix("/control/") {
        {
            let mut st = state.lock().unwrap();
            match rest.split_once('/') {
                Some(("advance", secs)) => st.sim.advance(Duration::from_secs(secs.parse().unwrap_or(0))),
                Some(("move-upstream", key)) => st.sim.move_upstream(key),
                _ => {}
            }
        }
        return respond(&mut sock, 200, "text/plain", "ok").await;
    }
    let body: Value = serde_json::from_slice(&buf[head_end..]).unwrap_or(Value::Null);
    let wire = if path.ends_with("/messages") { Wire::AnthropicMessages } else { Wire::ChatCompletions };

    let (n, outcome, reply) = {
        let mut st = state.lock().unwrap();
        let outcome = st.sim.request(wire, &headers, &body);
        let reply = (st.script)(wire, &body);
        let n = st.log.len() + 1;
        let entry = LogEntry { n, wire, path: path.clone(), headers, body: body.clone(), outcome: outcome.clone() };
        if let Some(f) = &st.log_file {
            if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(f) {
                let _ = writeln!(f, "{}", log_line(&entry));
            }
        }
        st.log.push(entry);
        (n, outcome, reply)
    };
    let outcome = match (outcome, reply) {
        (Err(e), _) => return respond(&mut sock, 400, "application/json", &json!({"error": {"type": "invalid_request_error", "message": e}}).to_string()).await,
        (_, Reply::Error(status, msg)) => return respond(&mut sock, status, "application/json", &json!({"error": {"message": msg}}).to_string()).await,
        (Ok(o), reply) => (o, reply),
    };
    let model = body["model"].as_str().unwrap_or_default();
    let sse = match wire {
        Wire::ChatCompletions => chat_sse(n, model, &outcome.0, &outcome.1),
        Wire::AnthropicMessages => anthropic_sse(n, model, &outcome.0, &outcome.1),
    };
    respond(&mut sock, 200, "text/event-stream", &sse).await
}

async fn respond(sock: &mut TcpStream, status: u16, ctype: &str, body: &str) -> anyhow::Result<()> {
    let head = format!("HTTP/1.1 {status} X\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
    sock.write_all(head.as_bytes()).await?;
    sock.write_all(body.as_bytes()).await?;
    sock.shutdown().await?;
    Ok(())
}

const OUTPUT_TOKENS: u64 = 10;

fn chat_sse(n: usize, model: &str, o: &Outcome, reply: &Reply) -> String {
    let chunk = |choices: Value| json!({"id": format!("gen-{n}"), "object": "chat.completion.chunk", "model": model, "provider": o.upstream, "choices": choices});
    let mut events = Vec::new();
    let finish = match reply {
        Reply::ToolCalls(calls) => {
            let tcs: Vec<Value> = calls
                .iter()
                .enumerate()
                .map(|(i, (name, args))| json!({"index": i, "id": format!("call_{n}_{i}"), "type": "function", "function": {"name": name, "arguments": args.to_string()}}))
                .collect();
            events.push(chunk(json!([{"index": 0, "delta": {"role": "assistant", "tool_calls": tcs}, "finish_reason": null}])));
            "tool_calls"
        }
        Reply::Text(t) => {
            events.push(chunk(json!([{"index": 0, "delta": {"role": "assistant", "content": t}, "finish_reason": null}])));
            "stop"
        }
        Reply::Error(..) => unreachable!(),
    };
    events.push(chunk(json!([{"index": 0, "delta": {}, "finish_reason": finish}])));
    let mut last = chunk(json!([]));
    last["usage"] = json!({
        "prompt_tokens": o.prompt(),
        "completion_tokens": OUTPUT_TOKENS,
        "total_tokens": o.prompt() + OUTPUT_TOKENS,
        "prompt_tokens_details": {"cached_tokens": o.read, "cache_write_tokens": o.write},
    });
    events.push(last);
    let mut s: String = events.iter().map(|e| format!("data: {e}\n\n")).collect();
    s.push_str("data: [DONE]\n\n");
    s
}

fn anthropic_sse(n: usize, model: &str, o: &Outcome, reply: &Reply) -> String {
    let mut events: Vec<(&str, Value)> = vec![(
        "message_start",
        json!({"type": "message_start", "message": {"id": format!("msg_{n}"), "type": "message", "role": "assistant", "model": model, "content": [],
            "usage": {"input_tokens": o.input, "cache_read_input_tokens": o.read, "cache_creation_input_tokens": o.write,
                      "cache_creation": {"ephemeral_5m_input_tokens": o.write - o.write_1h, "ephemeral_1h_input_tokens": o.write_1h}, "output_tokens": 1}}}),
    )];
    let stop = match reply {
        Reply::Text(t) => {
            events.push(("content_block_start", json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}})));
            events.push(("content_block_delta", json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": t}})));
            events.push(("content_block_stop", json!({"type": "content_block_stop", "index": 0})));
            "end_turn"
        }
        Reply::ToolCalls(calls) => {
            for (i, (name, args)) in calls.iter().enumerate() {
                events.push(("content_block_start", json!({"type": "content_block_start", "index": i, "content_block": {"type": "tool_use", "id": format!("toolu_{n}_{i}"), "name": name, "input": {}}})));
                events.push(("content_block_delta", json!({"type": "content_block_delta", "index": i, "delta": {"type": "input_json_delta", "partial_json": args.to_string()}})));
                events.push(("content_block_stop", json!({"type": "content_block_stop", "index": i})));
            }
            "tool_use"
        }
        Reply::Error(..) => unreachable!(),
    };
    events.push(("message_delta", json!({"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {"output_tokens": OUTPUT_TOKENS}})));
    events.push(("message_stop", json!({"type": "message_stop"})));
    events.iter().map(|(name, e)| format!("event: {name}\ndata: {e}\n\n")).collect()
}
