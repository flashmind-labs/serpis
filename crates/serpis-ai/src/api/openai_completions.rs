//! OpenAI Chat Completions (and the many compatible servers), ported from pi-ai
//! `openai-completions.ts` including its provider compat detection.

use std::collections::HashMap;

use serde_json::{Map, Value, json};

use crate::api::Request;
use crate::stream::*;
use crate::transform::{short_hash, transform_messages};
use crate::types::*;

#[derive(Debug, Clone)]
pub struct Compat {
    pub supports_store: bool,
    pub supports_developer_role: bool,
    pub supports_reasoning_effort: bool,
    pub supports_usage_in_streaming: bool,
    pub supports_finish_reason: bool,
    pub max_tokens_field: String,
    pub requires_tool_result_name: bool,
    pub requires_assistant_after_tool_result: bool,
    pub requires_thinking_as_text: bool,
    pub requires_reasoning_content_on_assistant_messages: bool,
    pub thinking_format: String,
    pub open_router_routing: Option<Value>,
    pub vercel_gateway_routing: Option<Value>,
    pub chat_template_kwargs: Map<String, Value>,
    pub zai_tool_stream: bool,
    pub thinking_token_budget_field: Option<String>,
    pub supports_strict_mode: bool,
    pub cache_control_format: Option<String>,
    pub send_session_affinity_headers: bool,
    pub session_affinity_format: String,
    pub supports_long_cache_retention: bool,
    pub vllm_priority: Option<Value>,
}

pub fn detect_compat(model: &Model) -> Compat {
    let provider = model.provider.as_str();
    let base = model.base_url.as_str();
    let is_zai = provider == "zai" || provider == "zai-coding-cn" || base.contains("api.z.ai") || base.contains("open.bigmodel.cn");
    let is_together = provider == "together" || base.contains("api.together.ai") || base.contains("api.together.xyz");
    let is_moonshot = provider == "moonshotai" || provider == "moonshotai-cn" || base.contains("api.moonshot.");
    let is_openrouter = model.is_openrouter();
    let is_cf_workers = provider == "cloudflare-workers-ai" || base.contains("api.cloudflare.com");
    let is_cf_gateway = provider == "cloudflare-ai-gateway" || base.contains("gateway.ai.cloudflare.com");
    let is_nvidia = provider == "nvidia" || base.contains("integrate.api.nvidia.com");
    let is_ant_ling = provider == "ant-ling" || base.contains("api.ant-ling.com");
    let is_cerebras = provider == "cerebras" || base.contains("cerebras.ai");
    let is_deepseek = provider == "deepseek" || base.to_lowercase().contains("deepseek.com");
    let is_grok = provider == "xai" || base.contains("api.x.ai");

    let non_standard = is_nvidia
        || is_cerebras
        || is_grok
        || is_together
        || base.contains("chutes.ai")
        || is_deepseek
        || is_zai
        || is_moonshot
        || provider == "opencode"
        || base.contains("opencode.ai")
        || is_cf_workers
        || is_cf_gateway
        || is_ant_ling;
    let use_max_tokens = base.contains("chutes.ai")
        || is_deepseek
        || is_moonshot
        || is_cf_gateway
        || is_together
        || is_nvidia
        || is_ant_ling
        || is_zai;
    let or_dev_role = is_openrouter && (model.id.starts_with("anthropic/") || model.id.starts_with("openai/"));

    Compat {
        supports_store: !non_standard,
        supports_developer_role: or_dev_role || (!non_standard && !is_openrouter),
        supports_reasoning_effort: !is_grok && !is_zai && !is_moonshot && !is_together && !is_cf_gateway && !is_nvidia && !is_ant_ling,
        supports_usage_in_streaming: true,
        supports_finish_reason: true,
        max_tokens_field: if use_max_tokens { "max_tokens" } else { "max_completion_tokens" }.into(),
        requires_tool_result_name: false,
        requires_assistant_after_tool_result: false,
        requires_thinking_as_text: false,
        requires_reasoning_content_on_assistant_messages: is_deepseek,
        thinking_format: if is_deepseek {
            "deepseek"
        } else if is_zai {
            "zai"
        } else if is_together {
            "together"
        } else if is_ant_ling {
            "ant-ling"
        } else if is_openrouter {
            "openrouter"
        } else {
            "openai"
        }
        .into(),
        open_router_routing: None,
        vercel_gateway_routing: None,
        chat_template_kwargs: Map::new(),
        zai_tool_stream: false,
        thinking_token_budget_field: None,
        supports_strict_mode: false,
        cache_control_format: (is_openrouter && (model.id.starts_with("anthropic/") || model.id.starts_with("google/"))).then(|| "anthropic".into()),
        send_session_affinity_headers: is_openrouter,
        session_affinity_format: if is_openrouter { "openrouter" } else { "openai" }.into(),
        supports_long_cache_retention: !(is_together || is_cf_workers || is_cf_gateway || is_nvidia || is_ant_ling),
        vllm_priority: None,
    }
}

/// Auto-detected compat overridden by explicit `model.compat` entries.
pub fn get_compat(model: &Model) -> Compat {
    let mut c = detect_compat(model);
    let Some(o) = &model.compat else { return c };
    let b = |k: &str, d: bool| o.get(k).and_then(Value::as_bool).unwrap_or(d);
    let s = |k: &str| o.get(k).and_then(Value::as_str).map(str::to_string);
    c.supports_store = b("supportsStore", c.supports_store);
    c.supports_developer_role = b("supportsDeveloperRole", c.supports_developer_role);
    c.supports_reasoning_effort = b("supportsReasoningEffort", c.supports_reasoning_effort);
    c.supports_usage_in_streaming = b("supportsUsageInStreaming", c.supports_usage_in_streaming);
    c.supports_finish_reason = b("supportsFinishReason", c.supports_finish_reason);
    c.max_tokens_field = s("maxTokensField").unwrap_or(c.max_tokens_field);
    c.requires_tool_result_name = b("requiresToolResultName", c.requires_tool_result_name);
    c.requires_assistant_after_tool_result = b("requiresAssistantAfterToolResult", c.requires_assistant_after_tool_result);
    c.requires_thinking_as_text = b("requiresThinkingAsText", c.requires_thinking_as_text);
    c.requires_reasoning_content_on_assistant_messages = b(
        "requiresReasoningContentOnAssistantMessages",
        c.requires_reasoning_content_on_assistant_messages,
    );
    c.thinking_format = s("thinkingFormat").unwrap_or(c.thinking_format);
    c.open_router_routing = o.get("openRouterRouting").cloned();
    c.vercel_gateway_routing = o.get("vercelGatewayRouting").cloned();
    if let Some(Value::Object(m)) = o.get("chatTemplateKwargs") {
        c.chat_template_kwargs = m.clone();
    }
    c.zai_tool_stream = b("zaiToolStream", c.zai_tool_stream);
    c.thinking_token_budget_field = s("thinkingTokenBudgetField").or(c.thinking_token_budget_field);
    c.supports_strict_mode = b("supportsStrictMode", c.supports_strict_mode);
    c.cache_control_format = s("cacheControlFormat").or(c.cache_control_format);
    c.send_session_affinity_headers = b("sendSessionAffinityHeaders", c.send_session_affinity_headers);
    c.session_affinity_format = s("sessionAffinityFormat").unwrap_or(c.session_affinity_format);
    c.supports_long_cache_retention = b("supportsLongCacheRetention", c.supports_long_cache_retention);
    c.vllm_priority = o.get("vllmPriority").cloned();
    c
}

fn normalize_tool_call_id(model: &Model, id: &str) -> String {
    if let Some(sep) = id.find('|') {
        let clean = |s: &str| s.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' }).collect::<String>();
        let call_id = clean(&id[..sep]);
        let item_id = clean(&id[sep + 1..]);
        let combined = if item_id.is_empty() { call_id.clone() } else { format!("{call_id}_{item_id}") };
        if combined.len() <= 40 {
            return combined;
        }
        let hash = &short_hash(id)[..8];
        let keep = 40usize.saturating_sub(hash.len() + 1).max(1);
        return format!("{}_{hash}", &call_id[..keep.min(call_id.len())]);
    }
    if model.provider == "openai" && id.len() > 40 {
        return id[..40].to_string();
    }
    id.to_string()
}

fn image_url(data: &str, mime: &str) -> Value {
    json!({"type": "image_url", "image_url": {"url": format!("data:{mime};base64,{data}")}})
}

pub fn convert_messages(model: &Model, ctx: &Context, compat: &Compat) -> Vec<Value> {
    let mut params: Vec<Value> = Vec::new();
    let norm = |id: &str| normalize_tool_call_id(model, id);
    let messages = transform_messages(&ctx.messages, model, Some(&norm));
    let instruction_role = if model.reasoning && compat.supports_developer_role { "developer" } else { "system" };
    if !ctx.system_prompt.is_empty() {
        params.push(json!({"role": instruction_role, "content": ctx.system_prompt}));
    }

    let mut last_role = "";
    let mut i = 0;
    while i < messages.len() {
        let msg = &messages[i];
        if compat.requires_assistant_after_tool_result && last_role == "toolResult" && matches!(msg, Message::User { .. }) {
            params.push(json!({"role": "assistant", "content": "I have processed the tool results."}));
        }
        match msg {
            Message::User { content, .. } => {
                let parts: Vec<Value> = content
                    .iter()
                    .filter(|c| !matches!(c, UserContent::Text { text } if text.is_empty()))
                    .map(|c| match c {
                        UserContent::Text { text } => json!({"type": "text", "text": text}),
                        UserContent::Image { data, mime_type } => image_url(data, mime_type),
                    })
                    .collect();
                if parts.is_empty() {
                    i += 1;
                    continue;
                }
                // A single text part is sent as a plain string for maximum compatibility.
                if parts.len() == 1 && parts[0]["type"] == "text" {
                    params.push(json!({"role": "user", "content": parts[0]["text"]}));
                } else {
                    params.push(json!({"role": "user", "content": parts}));
                }
                last_role = "user";
            }
            Message::Assistant(a) => {
                let mut out = Map::new();
                out.insert("role".into(), "assistant".into());
                out.insert(
                    "content".into(),
                    if compat.requires_assistant_after_tool_result { Value::String(String::new()) } else { Value::Null },
                );
                let text: String = a
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        AssistantContent::Text { text, .. } if !text.trim().is_empty() => Some(text.as_str()),
                        _ => None,
                    })
                    .collect();
                let thinking: Vec<(&String, Option<&String>)> = a
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        AssistantContent::Thinking {
                            thinking,
                            thinking_signature,
                            ..
                        } => Some((thinking, thinking_signature.as_ref())),
                        _ => None,
                    })
                    .collect();
                let reasoning_details: Option<Value> = thinking
                    .iter()
                    .filter_map(|(_, sig)| sig.and_then(|s| serde_json::from_str::<Value>(s).ok()).filter(Value::is_array))
                    .next();
                let non_empty: Vec<_> = thinking.iter().filter(|(t, _)| !t.trim().is_empty()).collect();
                if !non_empty.is_empty() {
                    if compat.requires_thinking_as_text {
                        let joined = non_empty.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join("\n\n");
                        let mut parts = vec![json!({"type": "text", "text": joined})];
                        if !text.is_empty() {
                            parts.push(json!({"type": "text", "text": text}));
                        }
                        out.insert("content".into(), Value::Array(parts));
                    } else {
                        if !text.is_empty() {
                            out.insert("content".into(), Value::String(text.clone()));
                        }
                        if reasoning_details.is_none() {
                            if let Some(sig) = non_empty[0].1 {
                                let sig = if model.provider == "opencode-go" && sig == "reasoning" { "reasoning_content" } else { sig.as_str() };
                                if matches!(sig, "reasoning" | "reasoning_content" | "reasoning_text") {
                                    let joined = non_empty.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>().join("\n");
                                    out.insert(sig.into(), Value::String(joined));
                                }
                            }
                        }
                    }
                } else if !text.is_empty() {
                    out.insert("content".into(), Value::String(text.clone()));
                }
                let tool_calls: Vec<Value> = a
                    .tool_calls()
                    .map(|tc| {
                        json!({
                            "id": tc.id,
                            "type": "function",
                            "function": {"name": tc.name, "arguments": tc.arguments.to_string()}
                        })
                    })
                    .collect();
                if !tool_calls.is_empty() {
                    out.insert("tool_calls".into(), Value::Array(tool_calls));
                }
                if let Some(rd) = reasoning_details {
                    out.insert("reasoning_details".into(), rd);
                }
                if compat.requires_reasoning_content_on_assistant_messages && model.reasoning && !out.contains_key("reasoning_content") {
                    out.insert("reasoning_content".into(), Value::String(String::new()));
                }
                let has_content = match out.get("content") {
                    Some(Value::String(s)) => !s.is_empty(),
                    Some(Value::Array(a)) => !a.is_empty(),
                    _ => false,
                };
                if has_content || out.contains_key("tool_calls") {
                    params.push(Value::Object(out));
                }
                last_role = "assistant";
            }
            Message::ToolResult(_) => {
                let mut images: Vec<Value> = Vec::new();
                while let Some(Message::ToolResult(tr)) = messages.get(i) {
                    let text = tr
                        .content
                        .iter()
                        .filter_map(|c| match c {
                            UserContent::Text { text } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let has_images = tr.content.iter().any(|c| matches!(c, UserContent::Image { .. }));
                    let body = if !text.trim().is_empty() {
                        text
                    } else if has_images {
                        "(see attached image)".into()
                    } else {
                        "(no tool output)".into()
                    };
                    let mut m = json!({"role": "tool", "content": body, "tool_call_id": tr.tool_call_id});
                    if compat.requires_tool_result_name {
                        m["name"] = Value::String(tr.tool_name.clone());
                    }
                    params.push(m);
                    if has_images && model.supports_images() {
                        for c in &tr.content {
                            if let UserContent::Image { data, mime_type } = c {
                                images.push(image_url(data, mime_type));
                            }
                        }
                    }
                    i += 1;
                }
                if !images.is_empty() {
                    if compat.requires_assistant_after_tool_result {
                        params.push(json!({"role": "assistant", "content": "I have processed the tool results."}));
                    }
                    let mut content = vec![json!({"type": "text", "text": "Attached image(s) from tool result:"})];
                    content.extend(images);
                    params.push(json!({"role": "user", "content": content}));
                    last_role = "user";
                } else {
                    last_role = "toolResult";
                }
                continue;
            }
        }
        i += 1;
    }
    params
}

fn convert_tools(tools: &[Tool], compat: &Compat) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            let mut f = json!({"name": t.name, "description": t.description, "parameters": t.parameters});
            if compat.supports_strict_mode {
                f["strict"] = Value::Bool(false);
            }
            json!({"type": "function", "function": f})
        })
        .collect()
}

/// Send text content as parts. A string would become parts only while it carries the mark, so the
/// same message would change bytes between requests and miss the cache.
fn text_as_parts(msg: &mut Value) {
    if let Some(Value::String(s)) = msg.get_mut("content") {
        if !s.is_empty() {
            let text = std::mem::take(s);
            msg["content"] = json!([{"type": "text", "text": text}]);
        }
    }
}

fn add_cache_control_to_text(msg: &mut Value, cc: &Value) -> bool {
    match msg.get_mut("content") {
        Some(Value::Array(parts)) => {
            for p in parts.iter_mut().rev() {
                if p["type"] == "text" {
                    p["cache_control"] = cc.clone();
                    return true;
                }
            }
            false
        }
        _ => false,
    }
}

fn apply_anthropic_cache_control(messages: &mut [Value], tools: Option<&mut Vec<Value>>, cc: &Value) {
    messages.iter_mut().for_each(text_as_parts);
    if let Some(m) = messages.iter_mut().find(|m| m["role"] == "system" || m["role"] == "developer") {
        add_cache_control_to_text(m, cc);
    }
    if let Some(last) = tools.and_then(|t| t.last_mut()) {
        last["cache_control"] = cc.clone();
    }
    // This request's end and the previous request's end (the last user or tool message before
    // the newest assistant message), as in the Anthropic adapter.
    let is_input = |m: &Value| matches!(m["role"].as_str(), Some("user" | "tool"));
    let last_assistant = messages.iter().rposition(|m| m["role"] == "assistant");
    let previous = last_assistant.and_then(|a| messages[..a].iter().rposition(is_input));
    let current = messages.iter().rposition(is_input);
    for i in [previous, current].into_iter().flatten() {
        add_cache_control_to_text(&mut messages[i], cc);
    }
}

fn mapped_effort(model: &Model, level: ThinkingLevel) -> Option<String> {
    match model.thinking_map(level) {
        Some(Some(v)) => Some(v.to_string()),
        Some(None) => None,
        None => Some(level.as_str().to_string()),
    }
}

pub fn build_params(model: &Model, ctx: &Context, opts: &StreamOptions, compat: &Compat) -> Value {
    let mut messages = convert_messages(model, ctx, compat);
    let effort = opts
        .reasoning
        .map(|r| clamp_thinking_level(model, r))
        .filter(|r| *r != ThinkingLevel::Off);
    let mut p = Map::new();
    p.insert("model".into(), model.id.clone().into());
    p.insert("stream".into(), true.into());
    let long = opts.cache_retention == CacheRetention::Long && compat.supports_long_cache_retention;
    if (model.base_url.contains("api.openai.com") && opts.cache_retention != CacheRetention::None) || long {
        if let Some(sid) = &opts.session_id {
            p.insert("prompt_cache_key".into(), sid.chars().take(64).collect::<String>().into());
        }
    }
    if long {
        p.insert("prompt_cache_retention".into(), "24h".into());
    }
    if compat.supports_usage_in_streaming {
        p.insert("stream_options".into(), json!({"include_usage": true}));
    }
    if compat.supports_store {
        p.insert("store".into(), false.into());
    }
    if let Some(max) = opts.max_tokens {
        p.insert(compat.max_tokens_field.clone(), max.into());
    }
    if let Some(t) = opts.temperature {
        p.insert("temperature".into(), t.into());
    }
    let mut tools = if !ctx.tools.is_empty() {
        Some(convert_tools(&ctx.tools, compat))
    } else if ctx.messages.iter().any(|m| matches!(m, Message::ToolResult(_))) {
        Some(Vec::new())
    } else {
        None
    };
    if compat.cache_control_format.as_deref() == Some("anthropic") && opts.cache_retention != CacheRetention::None {
        let cc = if opts.cache_retention == CacheRetention::Long && compat.supports_long_cache_retention {
            json!({"type": "ephemeral", "ttl": "1h"})
        } else {
            json!({"type": "ephemeral"})
        };
        apply_anthropic_cache_control(&mut messages, tools.as_mut(), &cc);
    }
    p.insert("messages".into(), Value::Array(messages));
    if let Some(t) = tools {
        if compat.zai_tool_stream && !t.is_empty() {
            p.insert("tool_stream".into(), true.into());
        }
        p.insert("tools".into(), Value::Array(t));
    }
    if let Some(prio) = &compat.vllm_priority {
        p.insert("priority".into(), prio.clone());
    }

    if model.reasoning {
        let off = model.thinking_map(ThinkingLevel::Off);
        match compat.thinking_format.as_str() {
            "zai" => {
                p.insert(
                    "thinking".into(),
                    if effort.is_some() { json!({"type": "enabled", "clear_thinking": false}) } else { json!({"type": "disabled"}) },
                );
                if let (Some(e), true) = (effort, compat.supports_reasoning_effort) {
                    if let Some(v) = mapped_effort(model, e) {
                        p.insert("reasoning_effort".into(), v.into());
                    }
                }
            }
            "qwen" => {
                p.insert("enable_thinking".into(), effort.is_some().into());
                if let (Some(e), true) = (effort, compat.supports_reasoning_effort) {
                    if let Some(v) = mapped_effort(model, e) {
                        p.insert("reasoning_effort".into(), v.into());
                    }
                }
            }
            "qwen-chat-template" => {
                p.insert(
                    "chat_template_kwargs".into(),
                    json!({"enable_thinking": effort.is_some(), "preserve_thinking": true}),
                );
            }
            "chat-template" => {
                let mut kw = compat.chat_template_kwargs.clone();
                kw.entry("enable_thinking").or_insert(effort.is_some().into());
                p.insert("chat_template_kwargs".into(), Value::Object(kw));
            }
            "deepseek" => {
                if effort.is_some() {
                    p.insert("thinking".into(), json!({"type": "enabled"}));
                } else if off != Some(None) {
                    p.insert("thinking".into(), json!({"type": "disabled"}));
                }
                if let (Some(e), true) = (effort, compat.supports_reasoning_effort) {
                    if let Some(v) = mapped_effort(model, e) {
                        p.insert("reasoning_effort".into(), v.into());
                    }
                }
            }
            "openrouter" => {
                if let Some(e) = effort {
                    if let Some(v) = mapped_effort(model, e) {
                        p.insert("reasoning".into(), json!({"effort": v}));
                    }
                } else if off != Some(None) {
                    let v = off.flatten().unwrap_or("none");
                    p.insert("reasoning".into(), json!({"effort": v}));
                }
            }
            "ant-ling" => {
                if let Some(Some(v)) = effort.map(|e| model.thinking_map(e).flatten()) {
                    p.insert("reasoning".into(), json!({"effort": v}));
                }
            }
            "together" => {
                p.insert("reasoning".into(), json!({"enabled": effort.is_some()}));
                if let (Some(e), true) = (effort, compat.supports_reasoning_effort) {
                    if let Some(v) = mapped_effort(model, e) {
                        p.insert("reasoning_effort".into(), v.into());
                    }
                }
            }
            "string-thinking" => {
                if let Some(e) = effort {
                    if let Some(v) = mapped_effort(model, e) {
                        p.insert("thinking".into(), v.into());
                    }
                } else if off != Some(None) {
                    p.insert("thinking".into(), off.flatten().unwrap_or("none").into());
                }
            }
            _ => {
                if compat.supports_reasoning_effort {
                    if let Some(e) = effort {
                        if let Some(v) = mapped_effort(model, e) {
                            p.insert("reasoning_effort".into(), v.into());
                        }
                    } else if let Some(Some(v)) = off {
                        p.insert("reasoning_effort".into(), v.into());
                    }
                }
            }
        }
        if let (Some(field), Some(e)) = (&compat.thinking_token_budget_field, effort) {
            let budget = crate::api::thinking_budget_for_level(e);
            p.insert(field.clone(), budget.into());
        }
    }

    if let Some(r) = &compat.open_router_routing {
        if r.as_object().is_some_and(|o| !o.is_empty()) {
            p.insert("provider".into(), r.clone());
        }
    }
    if let Some(Value::Object(r)) = &compat.vercel_gateway_routing {
        let mut gw = Map::new();
        for k in ["only", "order"] {
            if let Some(v) = r.get(k) {
                gw.insert(k.into(), v.clone());
            }
        }
        if !gw.is_empty() {
            p.insert("providerOptions".into(), json!({"gateway": gw}));
        }
    }
    if opts.native_web_search && compat.thinking_format == "openrouter" {
        p.insert("plugins".into(), json!([{"id": "web"}]));
    }
    if let Some(sp) = &model.sampling_params {
        for (k, v) in sp {
            p.insert(k.clone(), v.clone());
        }
    }
    Value::Object(p)
}

fn parse_usage(raw: &Value, model: &Model) -> Usage {
    let n = |v: Option<&Value>| v.and_then(Value::as_u64);
    let prompt = n(raw.get("prompt_tokens")).unwrap_or(0);
    let cache_read = n(raw.pointer("/prompt_tokens_details/cached_tokens"))
        .or_else(|| n(raw.get("prompt_cache_hit_tokens")))
        .or_else(|| n(raw.get("cached_tokens")))
        .unwrap_or(0);
    let cache_write = n(raw.pointer("/prompt_tokens_details/cache_write_tokens")).unwrap_or(0);
    let input = prompt.saturating_sub(cache_read + cache_write);
    let output = n(raw.get("completion_tokens")).unwrap_or(0);
    let mut u = Usage {
        input,
        output,
        cache_read,
        cache_write,
        reasoning: Some(n(raw.pointer("/completion_tokens_details/reasoning_tokens")).unwrap_or(0)),
        ..Default::default()
    };
    u.recompute_total();
    calculate_cost(model, &mut u);
    u
}

fn map_stop_reason(reason: &str) -> (StopReason, Option<String>) {
    match reason {
        "stop" | "end" => (StopReason::Stop, None),
        "length" => (StopReason::Length, None),
        "function_call" | "tool_calls" => (StopReason::ToolUse, None),
        other => (StopReason::Error, Some(format!("Provider finish_reason: {other}"))),
    }
}

fn merge_reasoning_detail(details: &mut Vec<Value>, d: &Value) {
    // OpenRouter streams reasoning_details as deltas: merge consecutive text/summary parts
    // with the same type and index; keep encrypted entries discrete.
    if let Some(last) = details.last_mut() {
        let same = last["type"] == d["type"] && last.get("index") == d.get("index");
        if same && d["type"] != "reasoning.encrypted" {
            for key in ["text", "summary"] {
                if let Some(add) = d.get(key).and_then(Value::as_str) {
                    let cur = last.get(key).and_then(Value::as_str).unwrap_or("").to_string();
                    last[key] = Value::String(cur + add);
                }
            }
            if let Some(sig) = d.get("signature").filter(|s| !s.is_null()) {
                last["signature"] = sig.clone();
            }
            return;
        }
    }
    details.push(d.clone());
}

pub fn build_request(model: &Model, ctx: &Context, opts: &StreamOptions) -> anyhow::Result<Request> {
    let compat = get_compat(model);
    let params = build_params(model, ctx, opts, &compat);
    let url = format!("{}/chat/completions", model.base_url.trim_end_matches('/'));
    let mut headers: HashMap<String, String> = HashMap::new();
    headers.insert("User-Agent".into(), crate::USER_AGENT.into());
    if let Some(h) = &model.headers {
        headers.extend(h.clone());
    }
    if let (Some(sid), true) = (&opts.session_id, compat.send_session_affinity_headers && opts.cache_retention != CacheRetention::None) {
        if compat.session_affinity_format == "openrouter" {
            headers.insert("x-session-id".into(), sid.clone());
        } else {
            if compat.session_affinity_format == "openai" {
                headers.insert("session_id".into(), sid.clone());
            }
            headers.insert("x-client-request-id".into(), sid.clone());
            headers.insert("x-session-affinity".into(), sid.clone());
        }
    }
    headers.extend(opts.headers.clone());
    let api_key = opts.api_key.clone().unwrap_or_default();
    let bearer = if api_key.is_empty() { String::new() } else { format!("Bearer {api_key}") };
    Ok(Request { url, headers, body: params }.with_auth("authorization", bearer))
}

pub async fn stream_inner(resp: reqwest::Response, model: Model, opts: StreamOptions, em: &mut Emitter) -> anyhow::Result<()> {
    let compat = get_compat(&model);
    em.emit(StreamEvent::Start);

    struct ToolBuf {
        index: usize,
        json: String,
    }
    let mut text_idx: Option<usize> = None;
    let mut thinking_idx: Option<usize> = None;
    let mut tools_by_stream: HashMap<i64, ToolBuf> = HashMap::new();
    let mut tool_order: Vec<i64> = Vec::new();
    let mut reasoning_details: Option<Vec<Value>> = None;
    let mut has_finish = false;

    for_each_sse(resp, &opts.cancel, |ev| {
        if ev.data.trim() == "[DONE]" {
            return Ok(false);
        }
        let Ok(chunk) = serde_json::from_str::<Value>(&ev.data) else { return Ok(true) };
        if let Some(err) = chunk.get("error") {
            anyhow::bail!("{}", err.get("message").and_then(Value::as_str).unwrap_or(&err.to_string()).to_string());
        }
        if em.output.response_id.is_none() {
            em.output.response_id = chunk.get("id").and_then(Value::as_str).map(str::to_string);
        }
        if let Some(m) = chunk.get("model").and_then(Value::as_str) {
            if !m.is_empty() && m != model.id && em.output.response_model.is_none() {
                em.output.response_model = Some(m.to_string());
            }
        }
        if let Some(p) = chunk.get("provider").and_then(Value::as_str).filter(|p| !p.is_empty()) {
            em.output.upstream = Some(p.to_string());
        }
        if let Some(u) = chunk.get("usage").filter(|u| u.is_object()) {
            em.output.usage = parse_usage(u, &model);
        }
        let Some(choice) = chunk.get("choices").and_then(|c| c.get(0)) else { return Ok(true) };
        if chunk.get("usage").is_none_or(Value::is_null) {
            if let Some(u) = choice.get("usage").filter(|u| u.is_object()) {
                em.output.usage = parse_usage(u, &model);
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            em.output.raw_stop_reason = Some(reason.to_string());
            let (sr, msg) = map_stop_reason(reason);
            em.output.stop_reason = sr;
            if msg.is_some() {
                em.output.error_message = msg;
            }
            has_finish = true;
        }
        let Some(delta) = choice.get("delta") else { return Ok(true) };

        // Reasoning first: some proxies put reasoning and answer in the same delta.
        for field in ["reasoning_content", "reasoning", "reasoning_text"] {
            if let Some(d) = delta.get(field).and_then(Value::as_str).filter(|s| !s.is_empty()) {
                let sig = if model.provider == "opencode-go" && field == "reasoning" { "reasoning_content" } else { field };
                let idx = *thinking_idx.get_or_insert_with(|| em.start_thinking(Some(sig.to_string())));
                em.push_thinking(idx, d);
                break;
            }
        }
        if let Some(Value::Array(details)) = delta.get("reasoning_details") {
            for d in details {
                if !d.is_object() || !d.get("type").is_some_and(Value::is_string) {
                    continue;
                }
                if thinking_idx.is_none() {
                    thinking_idx = Some(em.start_thinking(Some(String::new())));
                }
                merge_reasoning_detail(reasoning_details.get_or_insert_with(Vec::new), d);
            }
        }
        if let Some(c) = delta.get("content").and_then(Value::as_str).filter(|s| !s.is_empty()) {
            let idx = *text_idx.get_or_insert_with(|| em.start_text());
            em.push_text(idx, c);
        }
        if let Some(Value::Array(calls)) = delta.get("tool_calls") {
            for tc in calls {
                let stream_index = tc.get("index").and_then(Value::as_i64).unwrap_or(tool_order.len() as i64);
                let id = tc.get("id").and_then(Value::as_str).unwrap_or("").to_string();
                let name = tc.pointer("/function/name").and_then(Value::as_str).unwrap_or("").to_string();
                let buf = tools_by_stream.entry(stream_index).or_insert_with(|| {
                    tool_order.push(stream_index);
                    ToolBuf {
                        index: em.start_tool_call(id.clone(), name.clone()),
                        json: String::new(),
                    }
                });
                if let Some(AssistantContent::ToolCall(call)) = em.output.content.get_mut(buf.index) {
                    if call.id.is_empty() && !id.is_empty() {
                        call.id = id;
                    }
                    if call.name.is_empty() && !name.is_empty() {
                        call.name = name;
                    }
                }
                if let Some(args) = tc.pointer("/function/arguments").and_then(Value::as_str) {
                    buf.json.push_str(args);
                    let (index, json) = (buf.index, buf.json.clone());
                    em.tool_call_delta(index, &json, args);
                }
            }
        }
        Ok(true)
    })
    .await?;

    // Finish blocks in content order.
    if let (Some(idx), Some(details)) = (thinking_idx, &reasoning_details) {
        if let Some(AssistantContent::Thinking { thinking_signature, .. }) = em.output.content.get_mut(idx) {
            *thinking_signature = Some(Value::Array(details.clone()).to_string());
        }
    }
    let tool_json: HashMap<usize, String> = tools_by_stream.values().map(|b| (b.index, b.json.clone())).collect();
    for idx in 0..em.output.content.len() {
        em.finish_block(idx, tool_json.get(&idx).map(String::as_str));
    }
    if opts.cancel.is_cancelled() {
        anyhow::bail!("Request was aborted");
    }
    if !has_finish && !compat.supports_finish_reason {
        em.output.stop_reason = if em.output.tool_calls().next().is_some() { StopReason::ToolUse } else { StopReason::Stop };
    }
    if em.output.stop_reason == StopReason::Error {
        anyhow::bail!("{}", em.output.error_message.clone().unwrap_or_else(|| "Provider returned an error stop reason".into()));
    }
    if em.output.stop_reason == StopReason::Pending {
        anyhow::bail!("Stream ended without finish_reason");
    }
    Ok(())
}
