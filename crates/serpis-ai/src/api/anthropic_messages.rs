//! Anthropic Messages API, ported from pi-ai `anthropic-messages.ts`.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::api::Request;
use crate::stream::*;
use crate::transform::transform_messages;
use crate::types::*;

const CLAUDE_CODE_VERSION: &str = "2.1.280";
const CLAUDE_CODE_TOOLS: &[&str] = &[
    "Read", "Write", "Edit", "Bash", "Grep", "Glob", "AskUserQuestion", "EnterPlanMode", "ExitPlanMode", "KillShell",
    "NotebookEdit", "Skill", "Task", "TaskOutput", "TodoWrite", "WebFetch", "WebSearch",
];
const FINE_GRAINED_TOOL_STREAMING_BETA: &str = "fine-grained-tool-streaming-2025-05-14";
const INTERLEAVED_THINKING_BETA: &str = "interleaved-thinking-2025-05-14";
const SERVER_SIDE_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

fn to_cc_name(name: &str) -> String {
    CLAUDE_CODE_TOOLS
        .iter()
        .find(|t| t.eq_ignore_ascii_case(name))
        .map(|t| t.to_string())
        .unwrap_or_else(|| name.to_string())
}

fn from_cc_name(name: &str, tools: &[Tool]) -> String {
    tools
        .iter()
        .find(|t| t.name.eq_ignore_ascii_case(name))
        .map(|t| t.name.clone())
        .unwrap_or_else(|| name.to_string())
}

pub fn is_oauth_token(key: &str) -> bool {
    key.contains("sk-ant-oat")
}

fn normalize_tool_call_id(id: &str) -> String {
    id.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .take(64)
        .collect()
}

fn image_block(data: &str, mime: &str) -> Value {
    json!({"type": "image", "source": {"type": "base64", "media_type": mime, "data": data}})
}

fn convert_content_blocks(content: &[UserContent]) -> Value {
    if !content.iter().any(|c| matches!(c, UserContent::Image { .. })) {
        let text = content
            .iter()
            .filter_map(|c| match c {
                UserContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Value::String(text);
    }
    let mut blocks: Vec<Value> = content
        .iter()
        .map(|c| match c {
            UserContent::Text { text } => json!({"type": "text", "text": text}),
            UserContent::Image { data, mime_type } => image_block(data, mime_type),
        })
        .collect();
    if !blocks.iter().any(|b| b["type"] == "text") {
        blocks.insert(0, json!({"type": "text", "text": "(see attached image)"}));
    }
    Value::Array(blocks)
}

fn convert_messages(messages: &[Message], oauth: bool, cache_control: Option<&Value>, allow_empty_signature: bool) -> Vec<Value> {
    let mut params: Vec<Value> = Vec::new();
    let mut i = 0;
    while i < messages.len() {
        match &messages[i] {
            Message::User { content, .. } => {
                let blocks: Vec<Value> = content
                    .iter()
                    .filter(|c| !matches!(c, UserContent::Text { text } if text.trim().is_empty()))
                    .map(|c| match c {
                        UserContent::Text { text } => json!({"type": "text", "text": text}),
                        UserContent::Image { data, mime_type } => image_block(data, mime_type),
                    })
                    .collect();
                if !blocks.is_empty() {
                    params.push(json!({"role": "user", "content": blocks}));
                }
            }
            Message::Assistant(a) => {
                let mut blocks = Vec::new();
                for block in &a.content {
                    match block {
                        AssistantContent::Text { text, .. } => {
                            if !text.trim().is_empty() {
                                blocks.push(json!({"type": "text", "text": text}));
                            }
                        }
                        AssistantContent::Thinking {
                            thinking,
                            thinking_signature,
                            redacted,
                        } => {
                            if *redacted {
                                blocks.push(json!({"type": "redacted_thinking", "data": thinking_signature.clone().unwrap_or_default()}));
                                continue;
                            }
                            let sig = thinking_signature.as_deref().filter(|s| !s.trim().is_empty());
                            if thinking.trim().is_empty() && sig.is_none() {
                                continue;
                            }
                            match sig {
                                Some(sig) => blocks.push(json!({"type": "thinking", "thinking": thinking, "signature": sig})),
                                None if allow_empty_signature => {
                                    blocks.push(json!({"type": "thinking", "thinking": thinking, "signature": ""}))
                                }
                                None => blocks.push(json!({"type": "text", "text": thinking})),
                            }
                        }
                        AssistantContent::ToolCall(tc) => blocks.push(json!({
                            "type": "tool_use",
                            "id": tc.id,
                            "name": if oauth { to_cc_name(&tc.name) } else { tc.name.clone() },
                            "input": if tc.arguments.is_object() { tc.arguments.clone() } else { json!({}) },
                        })),
                    }
                }
                if !blocks.is_empty() {
                    params.push(json!({"role": "assistant", "content": blocks}));
                }
            }
            Message::ToolResult(_) => {
                let mut results = Vec::new();
                let mut error_images = Vec::new();
                while let Some(Message::ToolResult(tr)) = messages.get(i) {
                    let has_images = tr.content.iter().any(|c| matches!(c, UserContent::Image { .. }));
                    if tr.is_error && has_images {
                        // Anthropic rejects images in error results: send them after all results.
                        let text_only: Vec<UserContent> = tr
                            .content
                            .iter()
                            .map(|c| match c {
                                UserContent::Image { .. } => UserContent::Text {
                                    text: "(image attached after the tool results)".into(),
                                },
                                t => t.clone(),
                            })
                            .collect();
                        results.push(json!({"type": "tool_result", "tool_use_id": tr.tool_call_id, "content": convert_content_blocks(&text_only), "is_error": true}));
                        error_images.push(json!({"type": "text", "text": format!("Images from the error result of tool call {} ({}):", tr.tool_call_id, tr.tool_name)}));
                        for c in &tr.content {
                            if let UserContent::Image { data, mime_type } = c {
                                error_images.push(image_block(data, mime_type));
                            }
                        }
                    } else {
                        results.push(json!({"type": "tool_result", "tool_use_id": tr.tool_call_id, "content": convert_content_blocks(&tr.content), "is_error": tr.is_error}));
                    }
                    i += 1;
                }
                results.extend(error_images);
                params.push(json!({"role": "user", "content": results}));
                continue;
            }
        }
        i += 1;
    }
    if let Some(cc) = cache_control {
        // Mark this request's end and the previous request's end (the last user message before
        // the newest assistant message). One step can add more blocks than the provider looks
        // back over, so the previous mark keeps the cached prefix reachable.
        let last_assistant = params.iter().rposition(|m| m["role"] == "assistant");
        let previous = last_assistant.and_then(|a| params[..a].iter().rposition(|m| m["role"] == "user"));
        for i in [previous, params.len().checked_sub(1)].into_iter().flatten() {
            mark_last_block(&mut params[i], cc);
        }
    }
    params
}

fn mark_last_block(msg: &mut Value, cc: &Value) {
    if msg["role"] != "user" {
        return;
    }
    if let Some(b) = msg.get_mut("content").and_then(Value::as_array_mut).and_then(|b| b.last_mut()) {
        if matches!(b["type"].as_str(), Some("text" | "image" | "tool_result")) {
            b["cache_control"] = cc.clone();
        }
    }
}

fn map_effort(model: &Model, level: ThinkingLevel) -> &'static str {
    if let Some(Some(mapped)) = model.thinking_map(level) {
        for e in ["low", "medium", "high", "xhigh", "max"] {
            if e == mapped {
                return e;
            }
        }
    }
    match level {
        ThinkingLevel::Minimal | ThinkingLevel::Low => "low",
        ThinkingLevel::Medium => "medium",
        _ => "high",
    }
}

pub fn build_request(model: &Model, ctx: &Context, opts: &StreamOptions) -> anyhow::Result<Request> {
    let api_key = opts.api_key.clone().unwrap_or_default();
    let has_auth_header = opts.headers.keys().any(|k| k.eq_ignore_ascii_case("authorization"));
    if api_key.is_empty() && !has_auth_header && model.provider != "github-copilot" {
        anyhow::bail!("No API key for provider: {}", model.provider);
    }
    let oauth = is_oauth_token(&api_key);
    let is_openrouter = model.is_openrouter();
    let cb = |k: &str, d: bool| model.compat_bool(k).unwrap_or(d);
    let supports_eager = cb("supportsEagerToolInputStreaming", true);
    let send_affinity = cb("sendSessionAffinityHeaders", is_openrouter);
    let cache_on_tools = cb("supportsCacheControlOnTools", true);
    let supports_temperature = cb("supportsTemperature", true);
    let allow_empty_sig = cb("allowEmptySignature", false);
    let adaptive = cb("forceAdaptiveThinking", false);

    let cache_control = match opts.cache_retention {
        CacheRetention::None => None,
        CacheRetention::Long if model.supports_long_cache_retention() => Some(json!({"type": "ephemeral", "ttl": "1h"})),
        _ => Some(json!({"type": "ephemeral"})),
    };

    let messages = transform_messages(&ctx.messages, &model, Some(&normalize_tool_call_id));
    let reasoning = opts
        .reasoning
        .map(|r| clamp_thinking_level(&model, r))
        .filter(|r| *r != ThinkingLevel::Off);
    let mut max_tokens = opts.max_tokens.unwrap_or(model.max_tokens);
    let mut budget = 0;
    if let (Some(level), false) = (reasoning, adaptive) {
        // pi-ai adjustMaxTokensForThinking: leave room for the answer.
        budget = crate::api::thinking_budget_for_level(level);
        max_tokens = (opts.max_tokens.map(|m| m + budget).unwrap_or(model.max_tokens)).min(model.max_tokens);
        if max_tokens <= budget {
            budget = budget.min(max_tokens.saturating_sub(1024));
        }
        budget = budget.min(max_tokens.saturating_sub(1024));
    }

    let mut p = Map::new();
    p.insert("model".into(), model.id.clone().into());
    p.insert(
        "messages".into(),
        Value::Array(convert_messages(&messages, oauth, cache_control.as_ref(), allow_empty_sig)),
    );
    p.insert("max_tokens".into(), max_tokens.into());
    p.insert("stream".into(), true.into());

    let mut system = Vec::new();
    if oauth {
        // No mark of its own: the system prompt's mark right after it covers it, and the four
        // allowed marks go to the system prompt, the tools and the two message ends.
        system.push(json!({"type": "text", "text": "You are Claude Code, Anthropic's official CLI for Claude."}));
    }
    if !ctx.system_prompt.is_empty() {
        let mut b = json!({"type": "text", "text": ctx.system_prompt});
        if let Some(cc) = &cache_control {
            b["cache_control"] = cc.clone();
        }
        system.push(b);
    }
    if !system.is_empty() {
        p.insert("system".into(), Value::Array(system));
    }
    if let (Some(t), None) = (opts.temperature, reasoning) {
        if supports_temperature {
            p.insert("temperature".into(), t.into());
        }
    }

    let mut tools: Vec<Value> = ctx
        .tools
        .iter()
        .map(|t| {
            let schema = &t.parameters;
            let mut v = json!({
                "name": if oauth { to_cc_name(&t.name) } else { t.name.clone() },
                "description": t.description,
                "input_schema": {
                    "type": "object",
                    "properties": schema.get("properties").cloned().unwrap_or(json!({})),
                    "required": schema.get("required").cloned().unwrap_or(json!([])),
                },
            });
            if supports_eager {
                v["eager_input_streaming"] = true.into();
            }
            v
        })
        .collect();
    if let (Some(cc), true, Some(last)) = (&cache_control, cache_on_tools, tools.last_mut()) {
        last["cache_control"] = cc.clone();
    }
    if opts.native_web_search {
        tools.push(json!({"type": "web_search_20250305", "name": "web_search", "max_uses": 5}));
    }
    if !tools.is_empty() {
        p.insert("tools".into(), Value::Array(tools));
    }

    let off_unsupported = model.thinking_map(ThinkingLevel::Off) == Some(None);
    if model.reasoning {
        match reasoning {
            None if !off_unsupported => {
                p.insert("thinking".into(), json!({"type": "disabled"}));
            }
            None => {}
            Some(level) if adaptive => {
                p.insert("thinking".into(), json!({"type": "adaptive", "display": "summarized"}));
                p.insert("output_config".into(), json!({"effort": map_effort(&model, level)}));
            }
            Some(_) => {
                p.insert(
                    "thinking".into(),
                    json!({"type": "enabled", "budget_tokens": budget.max(1024), "display": "summarized"}),
                );
            }
        }
    }
    let fallbacks: Vec<Value> = model
        .compat_value("allowedFallbackModels")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|f| f.get("model").cloned()).map(|m| json!({"model": m})).collect())
        .unwrap_or_default();
    if !fallbacks.is_empty() {
        p.insert("fallbacks".into(), Value::Array(fallbacks.clone()));
    }

    let mut betas: Vec<&str> = Vec::new();
    if oauth {
        betas.extend(["claude-code-20250219", "oauth-2025-04-20"]);
    }
    if !ctx.tools.is_empty() && !supports_eager {
        betas.push(FINE_GRAINED_TOOL_STREAMING_BETA);
    }
    if reasoning.is_some() && !adaptive {
        betas.push(INTERLEAVED_THINKING_BETA);
    }
    if !fallbacks.is_empty() {
        betas.push(SERVER_SIDE_FALLBACK_BETA);
    }

    let mut headers: HashMap<String, String> = HashMap::new();
    headers.insert("accept".into(), "application/json".into());
    headers.insert("anthropic-version".into(), "2023-06-01".into());
    headers.insert("anthropic-dangerous-direct-browser-access".into(), "true".into());
    if oauth {
        headers.insert("user-agent".into(), format!("claude-cli/{CLAUDE_CODE_VERSION}"));
        headers.insert("x-app".into(), "cli".into());
    } else {
        headers.insert("user-agent".into(), crate::USER_AGENT.into());
    }
    if let (Some(sid), true) = (&opts.session_id, send_affinity && opts.cache_retention != CacheRetention::None) {
        let h = if is_openrouter { "x-session-id" } else { "x-session-affinity" };
        headers.insert(h.into(), sid.clone());
    }
    if let Some(h) = &model.headers {
        headers.extend(h.clone());
    }
    headers.extend(opts.headers.clone());
    let configured_beta = headers.iter().find(|(k, _)| k.eq_ignore_ascii_case("anthropic-beta")).map(|(_, v)| v.clone());
    if configured_beta.is_none() && !betas.is_empty() {
        headers.insert("anthropic-beta".into(), betas.join(","));
    }

    let req = Request {
        url: format!("{}/v1/messages", model.base_url.trim_end_matches('/')),
        headers,
        body: Value::Object(p),
    };
    Ok(if oauth || model.provider == "github-copilot" {
        req.with_auth("authorization", format!("Bearer {api_key}"))
    } else {
        req.with_auth("x-api-key", api_key)
    })
}

/// Turns Anthropic stream events (`message_start`, `content_block_delta`, ...) into the
/// message being built in an [`Emitter`]. Claude Code's `stream_event`s carry the same events.
pub struct EventReader {
    /// Anthropic block index -> (content index, partial json for tool calls)
    blocks: HashMap<u64, (usize, String)>,
    model_id: String,
    /// The model whose prices apply (a fallback model's, when one answered).
    usage_model: Model,
    /// The request's tools, when tool names were sent as Claude Code's.
    oauth_tools: Option<Vec<Tool>>,
}

impl EventReader {
    pub fn new(model: &Model, oauth_tools: Option<Vec<Tool>>) -> Self {
        EventReader { blocks: HashMap::new(), model_id: model.id.clone(), usage_model: model.clone(), oauth_tools }
    }

    /// Apply one event of type `ty`; Err for an `error` event.
    pub fn handle(&mut self, ty: &str, data: &Value, em: &mut Emitter) -> anyhow::Result<()> {
        match ty {
            "message_start" => {
                let m = &data["message"];
                em.output.response_id = m["id"].as_str().map(str::to_string);
                if let Some(rm) = m["model"].as_str().filter(|rm| *rm != self.model_id) {
                    em.output.response_model = Some(rm.to_string());
                    if let Some(cost) = self
                        .usage_model
                        .compat_value("allowedFallbackModels")
                        .and_then(Value::as_array)
                        .and_then(|a| a.iter().find(|f| f["model"] == rm))
                        .and_then(|f| serde_json::from_value::<ModelCost>(f["cost"].clone()).ok())
                    {
                        self.usage_model.cost = cost;
                    }
                }
                let u = &m["usage"];
                let usage = &mut em.output.usage;
                usage.input = u["input_tokens"].as_u64().unwrap_or(0);
                usage.output = u["output_tokens"].as_u64().unwrap_or(0);
                usage.cache_read = u["cache_read_input_tokens"].as_u64().unwrap_or(0);
                usage.cache_write = u["cache_creation_input_tokens"].as_u64().unwrap_or(0);
                usage.cache_write_1h = u.pointer("/cache_creation/ephemeral_1h_input_tokens").and_then(Value::as_u64);
                usage.recompute_total();
                calculate_cost(&self.usage_model, usage);
            }
            "content_block_start" => {
                let idx = data["index"].as_u64().unwrap_or(0);
                let cb = &data["content_block"];
                match cb["type"].as_str() {
                    Some("text") => {
                        let ci = em.start_text();
                        if let Some(t) = cb["text"].as_str().filter(|t| !t.is_empty()) {
                            em.push_text(ci, t);
                        }
                        self.blocks.insert(idx, (ci, String::new()));
                    }
                    Some("thinking") => {
                        let ci = em.start_thinking(cb["signature"].as_str().map(str::to_string));
                        self.blocks.insert(idx, (ci, String::new()));
                    }
                    Some("redacted_thinking") => {
                        let ci = em.start_thinking(cb["data"].as_str().map(str::to_string));
                        if let Some(AssistantContent::Thinking { thinking, redacted, .. }) = em.output.content.get_mut(ci) {
                            *thinking = "[Reasoning redacted]".into();
                            *redacted = true;
                        }
                        self.blocks.insert(idx, (ci, String::new()));
                    }
                    Some("tool_use") => {
                        let name = cb["name"].as_str().unwrap_or("");
                        let name = match &self.oauth_tools {
                        Some(tools) => from_cc_name(name, tools),
                        None => name.to_string(),
                    };
                        let ci = em.start_tool_call(cb["id"].as_str().unwrap_or("").to_string(), name);
                        self.blocks.insert(idx, (ci, String::new()));
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let idx = data["index"].as_u64().unwrap_or(0);
                let Some((ci, json)) = self.blocks.get_mut(&idx) else { return Ok(()) };
                let ci = *ci;
                let d = &data["delta"];
                match d["type"].as_str() {
                    Some("text_delta") => em.push_text(ci, d["text"].as_str().unwrap_or("")),
                    Some("thinking_delta") => em.push_thinking(ci, d["thinking"].as_str().unwrap_or("")),
                    Some("input_json_delta") => {
                        let delta = d["partial_json"].as_str().unwrap_or("");
                        json.push_str(delta);
                        let snapshot = json.clone();
                        em.tool_call_delta(ci, &snapshot, delta);
                    }
                    Some("signature_delta") => {
                        if let Some(AssistantContent::Thinking { thinking_signature, .. }) = em.output.content.get_mut(ci) {
                            thinking_signature.get_or_insert_with(String::new).push_str(d["signature"].as_str().unwrap_or(""));
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let idx = data["index"].as_u64().unwrap_or(0);
                if let Some((ci, json)) = self.blocks.remove(&idx) {
                    let is_tool = matches!(em.output.content.get(ci), Some(AssistantContent::ToolCall(_)));
                    em.finish_block(ci, is_tool.then_some(json.as_str()));
                }
            }
            "message_delta" => {
                if let Some(reason) = data.pointer("/delta/stop_reason").and_then(Value::as_str) {
                    em.output.raw_stop_reason = Some(reason.to_string());
                    let (sr, msg) = match reason {
                        "end_turn" | "pause_turn" | "stop_sequence" => (StopReason::Stop, None),
                        "max_tokens" => (StopReason::Length, None),
                        "tool_use" => (StopReason::ToolUse, None),
                        "refusal" => (
                            StopReason::Error,
                            Some(
                                data.pointer("/delta/stop_details/explanation")
                                    .and_then(Value::as_str)
                                    .unwrap_or("The model refused to complete the request")
                                    .to_string(),
                            ),
                        ),
                        other => (StopReason::Error, Some(format!("Provider stopped with: {other}"))),
                    };
                    em.output.stop_reason = sr;
                    if msg.is_some() {
                        em.output.error_message = msg;
                    }
                }
                let u = &data["usage"];
                if u.is_object() {
                    let usage = &mut em.output.usage;
                    if let Some(v) = u["input_tokens"].as_u64() {
                        usage.input = v;
                    }
                    if let Some(v) = u["output_tokens"].as_u64() {
                        usage.output = v;
                    }
                    if let Some(v) = u["cache_read_input_tokens"].as_u64() {
                        usage.cache_read = v;
                    }
                    if let Some(v) = u["cache_creation_input_tokens"].as_u64() {
                        usage.cache_write = v;
                    }
                    if let Some(v) = u.pointer("/output_tokens_details/thinking_tokens").and_then(Value::as_u64) {
                        usage.reasoning = Some(v);
                    }
                    usage.recompute_total();
                    calculate_cost(&self.usage_model, usage);
                }
            }
            "error" => {
                let msg = data.pointer("/error/message").and_then(Value::as_str).unwrap_or("Unknown error");
                anyhow::bail!("{msg}");
            }
            _ => {}
        }
        Ok(())
    }
}

pub async fn stream_inner(resp: reqwest::Response, model: Model, ctx: Context, opts: StreamOptions, em: &mut Emitter) -> anyhow::Result<()> {
    let oauth = is_oauth_token(opts.api_key.as_deref().unwrap_or_default());
    em.emit(StreamEvent::Start);

    let mut events = EventReader::new(&model, oauth.then(|| ctx.tools.clone()));
    for_each_sse(resp, &opts.cancel, |ev| {
        let Ok(data) = serde_json::from_str::<Value>(&ev.data) else { return Ok(true) };
        let ty = data["type"].as_str().unwrap_or(ev.event.as_deref().unwrap_or("")).to_string();
        events.handle(&ty, &data, em)?;
        Ok(true)
    })
    .await?;

    if opts.cancel.is_cancelled() {
        anyhow::bail!("Request was aborted");
    }
    match em.output.stop_reason {
        StopReason::Pending => anyhow::bail!("Anthropic stream ended without a stop reason"),
        StopReason::Error | StopReason::Aborted => {
            anyhow::bail!("{}", em.output.error_message.clone().unwrap_or_else(|| "An unknown error occurred".into()))
        }
        _ => Ok(()),
    }
}
