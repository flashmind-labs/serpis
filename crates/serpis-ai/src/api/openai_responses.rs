//! OpenAI Responses API, ported from pi-ai `openai-responses.ts` / `openai-responses-shared.ts`.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::stream::*;
use crate::transform::{short_hash, transform_messages};
use crate::types::*;

const TOOL_CALL_PROVIDERS: [&str; 3] = ["openai", "openai-codex", "opencode"];
const MIN_OUTPUT_TOKENS: u64 = 16;

fn normalize_id_part(part: &str) -> String {
    let s: String = part
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .take(64)
        .collect();
    s.trim_end_matches('_').to_string()
}

fn tool_result_output(model: &Model, content: &[UserContent]) -> Value {
    let text = content
        .iter()
        .filter_map(|c| match c {
            UserContent::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    let images: Vec<(&String, &String)> = content
        .iter()
        .filter_map(|c| match c {
            UserContent::Image { data, mime_type } => Some((data, mime_type)),
            _ => None,
        })
        .collect();
    let has_text = !text.trim().is_empty();
    if images.is_empty() || !model.supports_images() {
        return Value::String(if has_text {
            text
        } else if !images.is_empty() {
            "(see attached image)".into()
        } else {
            "(no tool output)".into()
        });
    }
    let mut out = Vec::new();
    if has_text {
        out.push(json!({"type": "input_text", "text": text}));
    }
    for (data, mime) in images {
        out.push(json!({"type": "input_image", "detail": "auto", "image_url": format!("data:{mime};base64,{data}")}));
    }
    Value::Array(out)
}

pub fn convert_messages(model: &Model, ctx: &Context) -> Vec<Value> {
    let mut items: Vec<Value> = Vec::new();
    let allowed = TOOL_CALL_PROVIDERS.contains(&model.provider.as_str());
    let norm = |id: &str| -> String {
        if !allowed || !id.contains('|') {
            return normalize_id_part(id);
        }
        let (call, item) = id.split_once('|').unwrap();
        // Foreign ids (from other providers) are hashed into an fc_ item id.
        let mut item_id = format!("fc_{}", short_hash(item));
        item_id.truncate(64);
        format!("{}|{}", normalize_id_part(call), item_id)
    };
    let messages = transform_messages(&ctx.messages, model, Some(&norm));
    let dev_role = model.compat_bool("supportsDeveloperRole") != Some(false);
    let instruction_role = if model.reasoning && dev_role { "developer" } else { "system" };
    if !ctx.system_prompt.is_empty() {
        items.push(json!({"role": instruction_role, "content": ctx.system_prompt}));
    }
    let mut msg_index = 0;
    for msg in &messages {
        match msg {
            Message::User { content, .. } => {
                let parts: Vec<Value> = content
                    .iter()
                    .map(|c| match c {
                        UserContent::Text { text } => json!({"type": "input_text", "text": text}),
                        UserContent::Image { data, mime_type } => json!({
                            "type": "input_image", "detail": "auto",
                            "image_url": format!("data:{mime_type};base64,{data}")
                        }),
                    })
                    .collect();
                if !parts.is_empty() {
                    items.push(json!({"role": "user", "content": parts}));
                }
            }
            Message::Assistant(a) => {
                let same_provider_api = a.provider == model.provider && a.api == model.api;
                let different_model = same_provider_api && a.model != model.id;
                let mut text_block_index = 0;
                for block in &a.content {
                    match block {
                        AssistantContent::Thinking { thinking_signature, .. } => {
                            if let Some(item) = thinking_signature.as_deref().and_then(|s| serde_json::from_str::<Value>(s).ok()) {
                                if item.is_object() {
                                    items.push(item);
                                }
                            }
                        }
                        AssistantContent::Text { text, text_signature } => {
                            let sig = text_signature.as_deref().and_then(|s| {
                                serde_json::from_str::<Value>(s)
                                    .ok()
                                    .filter(Value::is_object)
                                    .or_else(|| Some(json!({"id": s})))
                            });
                            let fallback = if text_block_index == 0 {
                                format!("msg_pi_{msg_index}")
                            } else {
                                format!("msg_pi_{msg_index}_{text_block_index}")
                            };
                            text_block_index += 1;
                            let mut id = sig
                                .as_ref()
                                .and_then(|s| s["id"].as_str().map(str::to_string))
                                .unwrap_or(fallback);
                            if id.len() > 64 {
                                id = format!("msg_{}", short_hash(&id));
                            }
                            let mut m = json!({
                                "type": "message", "role": "assistant", "status": "completed", "id": id,
                                "content": [{"type": "output_text", "text": text, "annotations": []}],
                            });
                            if let Some(phase) = sig.as_ref().and_then(|s| s.get("phase")).filter(|p| p.is_string()) {
                                m["phase"] = phase.clone();
                            }
                            items.push(m);
                        }
                        AssistantContent::ToolCall(tc) => {
                            let (call_id, item_id) = match tc.id.split_once('|') {
                                Some((c, i)) => (c.to_string(), Some(i.to_string())),
                                None => (tc.id.clone(), None),
                            };
                            let item_id = item_id.filter(|i| !different_model && i.starts_with("fc_"));
                            let mut m = json!({
                                "type": "function_call", "call_id": call_id, "name": tc.name,
                                "arguments": tc.arguments.to_string(),
                            });
                            if let Some(i) = item_id {
                                m["id"] = Value::String(i);
                            }
                            items.push(m);
                        }
                    }
                }
                msg_index += 1;
            }
            Message::ToolResult(tr) => {
                let call_id = tr.tool_call_id.split('|').next().unwrap_or("").to_string();
                items.push(json!({"type": "function_call_output", "call_id": call_id, "output": tool_result_output(model, &tr.content)}));
                msg_index += 1;
            }
        }
    }
    items
}

pub async fn stream_inner(model: Model, ctx: Context, opts: StreamOptions, em: &mut Emitter) -> anyhow::Result<()> {
    let api_key = opts.api_key.clone().unwrap_or_default();
    let mut p = Map::new();
    p.insert("model".into(), model.id.clone().into());
    p.insert("input".into(), Value::Array(convert_messages(&model, &ctx)));
    p.insert("stream".into(), true.into());
    p.insert("store".into(), false.into());
    if opts.cache_retention != CacheRetention::None {
        if let Some(sid) = &opts.session_id {
            p.insert("prompt_cache_key".into(), sid.chars().take(64).collect::<String>().into());
        }
        if opts.cache_retention == CacheRetention::Long && model.compat_bool("supportsLongCacheRetention") != Some(false) {
            p.insert("prompt_cache_retention".into(), "24h".into());
        }
    }
    if let Some(max) = opts.max_tokens {
        if model.compat_bool("supportsMaxOutputTokens") != Some(false) {
            p.insert("max_output_tokens".into(), max.max(MIN_OUTPUT_TOKENS).into());
        }
    }
    if let Some(t) = opts.temperature {
        p.insert("temperature".into(), t.into());
    }
    let mut tools: Vec<Value> = ctx
        .tools
        .iter()
        .map(|t| json!({"type": "function", "name": t.name, "description": t.description, "parameters": t.parameters, "strict": false}))
        .collect();
    if opts.native_web_search {
        tools.push(json!({"type": "web_search"}));
    }
    if !tools.is_empty() {
        p.insert("tools".into(), Value::Array(tools));
    }
    let effort = opts
        .reasoning
        .map(|r| clamp_thinking_level(&model, r))
        .filter(|r| *r != ThinkingLevel::Off);
    if model.reasoning {
        if let Some(e) = effort {
            let v = match model.thinking_map(e) {
                Some(Some(v)) => v.to_string(),
                _ => e.as_str().to_string(),
            };
            p.insert("reasoning".into(), json!({"effort": v, "summary": "auto"}));
            p.insert("include".into(), json!(["reasoning.encrypted_content"]));
        } else if model.provider != "github-copilot" && model.thinking_map(ThinkingLevel::Off) != Some(None) {
            let off = model.thinking_map(ThinkingLevel::Off).flatten().unwrap_or("none");
            p.insert("reasoning".into(), json!({"effort": off}));
        }
        if model.provider == "xai" {
            p.insert("include".into(), json!(["reasoning.encrypted_content"]));
        }
    }
    if let Some(sp) = &model.sampling_params {
        for (k, v) in sp {
            p.insert(k.clone(), v.clone());
        }
    }

    let mut headers: HashMap<String, String> = HashMap::new();
    headers.insert("User-Agent".into(), crate::USER_AGENT.into());
    if let Some(sid) = &opts.session_id {
        if model.provider == "openrouter" || model.base_url.contains("openrouter.ai") {
            headers.insert("x-session-id".into(), sid.clone());
        }
    }
    if let Some(h) = &model.headers {
        headers.extend(h.clone());
    }
    headers.extend(opts.headers.clone());
    let url = format!("{}/responses", model.base_url.trim_end_matches('/'));
    let body = Value::Object(p);
    let client = http_client();
    let resp = send_with_retry(
        || {
            let mut rb = client.post(&url).json(&body);
            if !api_key.is_empty() {
                rb = rb.bearer_auth(&api_key);
            }
            for (k, v) in &headers {
                rb = rb.header(k, v);
            }
            rb
        },
        opts.max_retries,
        &opts.cancel,
    )
    .await?;
    em.emit(StreamEvent::Start);

    enum Slot {
        Thinking(usize),
        Text(usize),
        Tool(usize, Option<String>),
    }
    let mut slots: HashMap<u64, Slot> = HashMap::new();
    let mut terminal = false;
    for_each_sse(resp, &opts.cancel, |ev| {
        let Ok(e) = serde_json::from_str::<Value>(&ev.data) else { return Ok(true) };
        let oi = e["output_index"].as_u64().unwrap_or(0);
        let delta = e["delta"].as_str().unwrap_or("");
        match e["type"].as_str().unwrap_or("") {
            "response.created" => em.output.response_id = e.pointer("/response/id").and_then(Value::as_str).map(str::to_string),
            "response.output_item.added" => {
                let item = &e["item"];
                match item["type"].as_str() {
                    Some("reasoning") => {
                        let i = em.start_thinking(None);
                        slots.insert(oi, Slot::Thinking(i));
                    }
                    Some("message") => {
                        let i = em.start_text();
                        slots.insert(oi, Slot::Text(i));
                    }
                    Some("function_call") => {
                        let id = format!("{}|{}", item["call_id"].as_str().unwrap_or(""), item["id"].as_str().unwrap_or(""));
                        let i = em.start_tool_call(id, item["name"].as_str().unwrap_or("").to_string());
                        slots.insert(oi, Slot::Tool(i, Some(item["arguments"].as_str().unwrap_or("").to_string())));
                    }
                    _ => {}
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(Slot::Thinking(i)) = slots.get(&oi) {
                    em.push_thinking(*i, delta);
                }
            }
            "response.reasoning_summary_part.done" => {
                if let Some(Slot::Thinking(i)) = slots.get(&oi) {
                    em.push_thinking(*i, "\n\n");
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                if let Some(Slot::Text(i)) = slots.get(&oi) {
                    em.push_text(*i, delta);
                }
            }
            "response.function_call_arguments.delta" => {
                if let Some(Slot::Tool(i, Some(json))) = slots.get_mut(&oi) {
                    json.push_str(delta);
                    let (i, snapshot) = (*i, json.clone());
                    em.tool_call_delta(i, &snapshot, delta);
                }
            }
            "response.output_item.done" => {
                let item = &e["item"];
                match (item["type"].as_str(), slots.remove(&oi)) {
                    (Some("reasoning"), Some(Slot::Thinking(i))) => {
                        let summary = item["summary"]
                            .as_array()
                            .map(|a| a.iter().filter_map(|s| s["text"].as_str()).collect::<Vec<_>>().join("\n\n"))
                            .unwrap_or_default();
                        if let Some(AssistantContent::Thinking { thinking, thinking_signature, .. }) = em.output.content.get_mut(i) {
                            if !summary.is_empty() {
                                *thinking = summary;
                            }
                            *thinking_signature = Some(item.to_string());
                        }
                        em.finish_block(i, None);
                    }
                    (Some("message"), Some(Slot::Text(i))) => {
                        let full: String = item["content"]
                            .as_array()
                            .map(|a| a.iter().map(|c| c["text"].as_str().or(c["refusal"].as_str()).unwrap_or("")).collect())
                            .unwrap_or_default();
                        let mut sig = json!({"v": 1, "id": item["id"]});
                        if item["phase"].is_string() {
                            sig["phase"] = item["phase"].clone();
                        }
                        if let Some(AssistantContent::Text { text, text_signature }) = em.output.content.get_mut(i) {
                            *text = full;
                            *text_signature = Some(sig.to_string());
                        }
                        em.finish_block(i, None);
                    }
                    (Some("function_call"), Some(Slot::Tool(i, json))) => {
                        let args = item["arguments"].as_str().map(str::to_string).or(json).unwrap_or_default();
                        em.finish_block(i, Some(&args));
                    }
                    (_, Some(slot)) => {
                        slots.insert(oi, slot);
                    }
                    _ => {}
                }
            }
            "response.completed" | "response.incomplete" => {
                terminal = true;
                let r = &e["response"];
                if let Some(id) = r["id"].as_str() {
                    em.output.response_id = Some(id.to_string());
                }
                let u = &r["usage"];
                if u.is_object() {
                    let cached = u.pointer("/input_tokens_details/cached_tokens").and_then(Value::as_u64).unwrap_or(0);
                    let write = u.pointer("/input_tokens_details/cache_write_tokens").and_then(Value::as_u64).unwrap_or(0);
                    em.output.usage = Usage {
                        input: u["input_tokens"].as_u64().unwrap_or(0).saturating_sub(cached + write),
                        output: u["output_tokens"].as_u64().unwrap_or(0),
                        cache_read: cached,
                        cache_write: write,
                        reasoning: u.pointer("/output_tokens_details/reasoning_tokens").and_then(Value::as_u64),
                        total_tokens: u["total_tokens"].as_u64().unwrap_or(0),
                        ..Default::default()
                    };
                }
                calculate_cost(&model, &mut em.output.usage);
                let status = r["status"].as_str().unwrap_or("completed");
                let reason = r.pointer("/incomplete_details/reason").and_then(Value::as_str);
                em.output.raw_stop_reason = Some(match reason {
                    Some(reason) => format!("{status}.{reason}"),
                    None => status.to_string(),
                });
                em.output.stop_reason = match (status, reason) {
                    ("incomplete", Some("max_output_tokens")) => StopReason::Length,
                    ("incomplete", r) => {
                        em.output.error_message = Some(match r {
                            Some(r) => format!("Response incomplete: {r}"),
                            None => "Response incomplete without a provider reason".into(),
                        });
                        StopReason::Error
                    }
                    ("failed" | "cancelled", _) => StopReason::Error,
                    _ => StopReason::Stop,
                };
                if em.output.stop_reason == StopReason::Stop && em.output.tool_calls().next().is_some() {
                    em.output.stop_reason = StopReason::ToolUse;
                }
            }
            "error" => anyhow::bail!("Error Code {}: {}", e["code"], e["message"].as_str().unwrap_or("Unknown error")),
            "response.failed" => {
                let r = &e["response"];
                let msg = if r["error"].is_object() {
                    format!(
                        "{}: {}",
                        r.pointer("/error/code").and_then(Value::as_str).unwrap_or("unknown"),
                        r.pointer("/error/message").and_then(Value::as_str).unwrap_or("no message")
                    )
                } else {
                    "Unknown error (no error details in response)".to_string()
                };
                anyhow::bail!("{msg}");
            }
            _ => {}
        }
        Ok(true)
    })
    .await?;
    if !terminal {
        anyhow::bail!("OpenAI Responses stream ended before a terminal response event");
    }
    if em.output.stop_reason == StopReason::ToolUse && !slots.values().all(|s| !matches!(s, Slot::Tool(..))) {
        anyhow::bail!("OpenAI Responses stream completed with an unfinished tool call");
    }
    if em.output.stop_reason == StopReason::Error {
        anyhow::bail!("{}", em.output.error_message.clone().unwrap_or_else(|| "Response failed".into()));
    }
    Ok(())
}
