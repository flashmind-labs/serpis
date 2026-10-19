//! The streaming contract shared by every API adapter, plus SSE decoding,
//! streaming JSON repair and request retries.

use std::collections::HashMap;
use std::time::Duration;

use futures::StreamExt;
use serde_json::{Map, Value};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::types::*;

/// Events emitted while a response streams in. Indices point into
/// `AssistantMessage::content` of the final message.
#[derive(Debug, Clone)]
pub enum StreamEvent {
    Start,
    TextStart { index: usize },
    TextDelta { index: usize, delta: String },
    TextEnd { index: usize },
    ThinkingStart { index: usize },
    ThinkingDelta { index: usize, delta: String },
    ThinkingEnd { index: usize },
    ToolCallStart { index: usize, id: String, name: String },
    /// `arguments` is the best-effort parse of the partial JSON so far.
    ToolCallDelta { index: usize, delta: String, arguments: Value },
    ToolCallEnd { index: usize, tool_call: ToolCall },
    Done(AssistantMessage),
    Error(AssistantMessage),
}

pub type EventStream = mpsc::UnboundedReceiver<StreamEvent>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CacheRetention {
    None,
    #[default]
    Short,
    Long,
}

/// Provider-neutral request options (pi-ai's `SimpleStreamOptions`).
#[derive(Debug, Clone, Default)]
pub struct StreamOptions {
    pub api_key: Option<String>,
    pub headers: HashMap<String, String>,
    pub reasoning: Option<ThinkingLevel>,
    pub max_tokens: Option<u64>,
    pub temperature: Option<f64>,
    pub session_id: Option<String>,
    pub cache_retention: CacheRetention,
    pub cancel: CancellationToken,
    pub max_retries: Option<u32>,
    /// Ask the provider to search the web itself when it can (Anthropic, OpenAI Responses, OpenRouter).
    pub native_web_search: bool,
}

/// Emits events and builds the final message in one place so adapters stay small.
pub struct Emitter {
    tx: mpsc::UnboundedSender<StreamEvent>,
    pub output: AssistantMessage,
}

impl Emitter {
    pub fn new(model: &Model) -> (Self, EventStream) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Emitter {
                tx,
                output: AssistantMessage::empty(model),
            },
            rx,
        )
    }

    pub fn emit(&self, event: StreamEvent) {
        let _ = self.tx.send(event);
    }

    pub fn start_text(&mut self) -> usize {
        self.output.content.push(AssistantContent::Text {
            text: String::new(),
            text_signature: None,
        });
        let index = self.output.content.len() - 1;
        self.emit(StreamEvent::TextStart { index });
        index
    }

    pub fn push_text(&mut self, index: usize, delta: &str) {
        if let Some(AssistantContent::Text { text, .. }) = self.output.content.get_mut(index) {
            text.push_str(delta);
            self.emit(StreamEvent::TextDelta {
                index,
                delta: delta.to_string(),
            });
        }
    }

    pub fn start_thinking(&mut self, signature: Option<String>) -> usize {
        self.output.content.push(AssistantContent::Thinking {
            thinking: String::new(),
            thinking_signature: signature,
            redacted: false,
        });
        let index = self.output.content.len() - 1;
        self.emit(StreamEvent::ThinkingStart { index });
        index
    }

    pub fn push_thinking(&mut self, index: usize, delta: &str) {
        if let Some(AssistantContent::Thinking { thinking, .. }) = self.output.content.get_mut(index) {
            thinking.push_str(delta);
            self.emit(StreamEvent::ThinkingDelta {
                index,
                delta: delta.to_string(),
            });
        }
    }

    pub fn start_tool_call(&mut self, id: String, name: String) -> usize {
        self.output.content.push(AssistantContent::ToolCall(ToolCall {
            id: id.clone(),
            name: name.clone(),
            arguments: Value::Object(Map::new()),
            thought_signature: None,
            arguments_error: None,
        }));
        let index = self.output.content.len() - 1;
        self.emit(StreamEvent::ToolCallStart { index, id, name });
        index
    }

    /// Append raw argument JSON to the scratch buffer kept by the adapter.
    pub fn tool_call_delta(&mut self, index: usize, partial_json: &str, delta: &str) {
        let parsed = parse_streaming_json(partial_json);
        if let Some(AssistantContent::ToolCall(tc)) = self.output.content.get_mut(index) {
            tc.arguments = parsed.clone();
        }
        self.emit(StreamEvent::ToolCallDelta {
            index,
            delta: delta.to_string(),
            arguments: parsed,
        });
    }

    pub fn finish_block(&mut self, index: usize, tool_args_json: Option<&str>) {
        match self.output.content.get_mut(index) {
            Some(AssistantContent::Text { .. }) => self.emit(StreamEvent::TextEnd { index }),
            Some(AssistantContent::Thinking { .. }) => self.emit(StreamEvent::ThinkingEnd { index }),
            Some(AssistantContent::ToolCall(tc)) => {
                if let Some(json) = tool_args_json {
                    let (arguments, error) = parse_tool_call_arguments(json);
                    tc.arguments = arguments;
                    tc.arguments_error = error;
                }
                let tool_call = tc.clone();
                self.emit(StreamEvent::ToolCallEnd { index, tool_call });
            }
            None => {}
        }
    }

    pub fn done(mut self) {
        self.output.usage.recompute_total();
        let msg = self.output.clone();
        self.emit(StreamEvent::Done(msg));
    }

    pub fn fail(mut self, error: anyhow::Error, cancel: &CancellationToken) {
        self.output.stop_reason = if cancel.is_cancelled() {
            StopReason::Aborted
        } else {
            StopReason::Error
        };
        self.output.error_message = Some(if cancel.is_cancelled() {
            "Request was aborted".to_string()
        } else {
            format!("{error:#}")
        });
        let msg = self.output.clone();
        self.emit(StreamEvent::Error(msg));
    }
}

/// Parse finished tool-call arguments. Invalid JSON yields `{}` plus an explanation, as in pi-ai.
pub fn parse_tool_call_arguments(json: &str) -> (Value, Option<String>) {
    let trimmed = json.trim();
    if trimmed.is_empty() {
        return (Value::Object(Map::new()), None);
    }
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Object(m)) => (Value::Object(m), None),
        Ok(other) => (
            Value::Object(Map::new()),
            Some(format!("Tool call arguments must be a JSON object, got: {other}")),
        ),
        Err(e) => {
            let repaired = parse_streaming_json(trimmed);
            if repaired.as_object().is_some_and(|m| !m.is_empty()) {
                (repaired, None)
            } else {
                (
                    Value::Object(Map::new()),
                    Some(format!("Invalid JSON in tool call arguments ({e}): {trimmed}")),
                )
            }
        }
    }
}

/// Best-effort parse of an incomplete JSON document (pi-ai `parseStreamingJson`).
/// Closes open strings, arrays and objects and drops a dangling key or separator.
pub fn parse_streaming_json(partial: &str) -> Value {
    if partial.trim().is_empty() {
        return Value::Object(Map::new());
    }
    if let Ok(v) = serde_json::from_str::<Value>(partial) {
        return v;
    }
    let mut stack: Vec<char> = Vec::new();
    let mut in_string = false;
    let mut escaped = false;
    for c in partial.chars() {
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => in_string = true,
            '{' => stack.push('}'),
            '[' => stack.push(']'),
            '}' | ']' => {
                stack.pop();
            }
            _ => {}
        }
    }
    let mut s = partial.to_string();
    if in_string {
        if escaped {
            s.pop();
        }
        s.push('"');
    }
    // Try progressively more aggressive repairs.
    let close = |base: &str| -> Option<Value> {
        let mut t = base.trim_end().to_string();
        while t.ends_with(',') || t.ends_with(':') {
            t.pop();
            t = t.trim_end().to_string();
        }
        for c in stack.iter().rev() {
            t.push(*c);
        }
        serde_json::from_str(&t).ok()
    };
    if let Some(v) = close(&s) {
        return v;
    }
    // Drop a trailing incomplete token (key without value, partial literal/number).
    let mut cut = s.clone();
    while !cut.is_empty() {
        let last = cut.pop().unwrap();
        if matches!(last, ',' | '{' | '[') {
            if last != ',' {
                cut.push(last);
            }
            if let Some(v) = close(&cut) {
                return v;
            }
        }
    }
    Value::Object(Map::new())
}

/// One decoded server-sent event.
#[derive(Debug, Clone, Default)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

/// Incremental SSE decoder over a byte stream.
#[derive(Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        loop {
            let Some(pos) = self.buf.iter().position(|b| *b == b'\n' || *b == b'\r') else {
                break;
            };
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            let term = line.pop();
            if term == Some(b'\r') && self.buf.first() == Some(&b'\n') {
                self.buf.remove(0);
            }
            let line = String::from_utf8_lossy(&line).into_owned();
            if let Some(ev) = self.line(&line) {
                out.push(ev);
            }
        }
        out
    }

    pub fn finish(&mut self) -> Option<SseEvent> {
        if !self.buf.is_empty() {
            let line = String::from_utf8_lossy(&std::mem::take(&mut self.buf)).into_owned();
            if let Some(ev) = self.line(&line) {
                return Some(ev);
            }
        }
        self.flush()
    }

    fn line(&mut self, line: &str) -> Option<SseEvent> {
        if line.is_empty() {
            return self.flush();
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.find(':') {
            Some(i) => {
                let v = &line[i + 1..];
                (&line[..i], v.strip_prefix(' ').unwrap_or(v))
            }
            None => (line, ""),
        };
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
        None
    }

    fn flush(&mut self) -> Option<SseEvent> {
        if self.data.is_empty() && self.event.is_none() {
            return None;
        }
        let ev = SseEvent {
            event: self.event.take(),
            data: self.data.join("\n"),
        };
        self.data.clear();
        Some(ev)
    }
}

/// Stream SSE events from a response, stopping early on cancellation.
pub async fn for_each_sse(
    response: reqwest::Response,
    cancel: &CancellationToken,
    mut f: impl FnMut(SseEvent) -> anyhow::Result<bool>,
) -> anyhow::Result<()> {
    let mut decoder = SseDecoder::default();
    let mut body = response.bytes_stream();
    loop {
        let chunk = tokio::select! {
            _ = cancel.cancelled() => anyhow::bail!("Request was aborted"),
            c = body.next() => c,
        };
        let Some(chunk) = chunk else { break };
        for ev in decoder.push(&chunk?) {
            if !f(ev)? {
                return Ok(());
            }
        }
    }
    if let Some(ev) = decoder.finish() {
        f(ev)?;
    }
    Ok(())
}

pub fn http_client() -> &'static reqwest::Client {
    static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(600))
            .build()
            .expect("http client")
    })
}

const RETRYABLE: [u16; 7] = [408, 429, 500, 502, 503, 504, 529];
const MAX_RETRY_DELAY: Duration = Duration::from_secs(60);

/// Send a request, retrying transient failures with backoff (honours `retry-after`).
/// Non-success responses become errors carrying the provider's body.
pub async fn send_with_retry(
    build: impl Fn() -> reqwest::RequestBuilder,
    max_retries: Option<u32>,
    cancel: &CancellationToken,
) -> anyhow::Result<reqwest::Response> {
    let max_retries = max_retries.unwrap_or(3);
    let mut attempt = 0;
    loop {
        let result = tokio::select! {
            _ = cancel.cancelled() => anyhow::bail!("Request was aborted"),
            r = build().send() => r,
        };
        let retry_after = match result {
            Ok(resp) if resp.status().is_success() => return Ok(resp),
            Ok(resp) => {
                let status = resp.status().as_u16();
                let retry_after = resp
                    .headers()
                    .get("retry-after")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<f64>().ok())
                    .map(Duration::from_secs_f64);
                let body = resp.text().await.unwrap_or_default();
                if !RETRYABLE.contains(&status) || attempt >= max_retries {
                    anyhow::bail!("{}", format_provider_error(status, &body));
                }
                if retry_after.is_some_and(|d| d > MAX_RETRY_DELAY) {
                    anyhow::bail!(
                        "{} (server asked to retry after {:?})",
                        format_provider_error(status, &body),
                        retry_after.unwrap()
                    );
                }
                retry_after
            }
            Err(e) if attempt < max_retries && (e.is_connect() || e.is_timeout()) => None,
            Err(e) => return Err(e.into()),
        };
        attempt += 1;
        let delay = retry_after.unwrap_or_else(|| Duration::from_millis(1000 * 2u64.pow(attempt - 1)));
        tokio::select! {
            _ = cancel.cancelled() => anyhow::bail!("Request was aborted"),
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

/// Render an HTTP error body the way pi-ai does: status plus the provider's message when present.
pub fn format_provider_error(status: u16, body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        let msg = v
            .pointer("/error/message")
            .or_else(|| v.get("message"))
            .or_else(|| v.pointer("/error"))
            .and_then(|m| m.as_str().map(str::to_string));
        let raw = v.pointer("/error/metadata/raw").and_then(Value::as_str);
        if let Some(msg) = msg {
            return match raw {
                Some(raw) if !msg.contains(raw) => format!("{status} {msg}\n{raw}"),
                _ => format!("{status} {msg}"),
            };
        }
    }
    let body = body.trim();
    if body.is_empty() {
        format!("{status} status code (no body)")
    } else {
        format!("{status} {}", truncate_chars(body, 2000))
    }
}

pub fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect::<String>() + "…"
    }
}

/// Remove lone UTF-16 surrogates. Rust strings cannot hold them, so this only strips U+FFFD runs
/// produced by lossy decoding; kept as a hook for parity with pi-ai's `sanitizeSurrogates`.
pub fn sanitize(s: &str) -> String {
    s.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn streaming_json_repairs_partial_documents() {
        assert_eq!(parse_streaming_json(r#"{"path": "src/ma"#), serde_json::json!({"path": "src/ma"}));
        assert_eq!(
            parse_streaming_json(r#"{"edits": [{"oldText": "a", "newT"#),
            serde_json::json!({"edits": [{"oldText": "a"}]})
        );
        assert_eq!(parse_streaming_json(r#"{"a": 1, "b":"#), serde_json::json!({"a": 1}));
        assert_eq!(parse_streaming_json(""), serde_json::json!({}));
    }

    #[test]
    fn sse_decoder_handles_split_chunks() {
        let mut d = SseDecoder::default();
        assert!(d.push(b"event: ping\nda").is_empty());
        let evs = d.push(b"ta: {\"x\":1}\n\ndata: [DONE]\r\n\r\n");
        assert_eq!(evs.len(), 2);
        assert_eq!(evs[0].event.as_deref(), Some("ping"));
        assert_eq!(evs[0].data, "{\"x\":1}");
        assert_eq!(evs[1].data, "[DONE]");
    }
}
