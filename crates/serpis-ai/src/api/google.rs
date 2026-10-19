//! Google Generative AI (Gemini API), ported from pi-ai `google-generative-ai.ts` / `google-shared.ts`.

use serde_json::{Map, Value, json};

use crate::stream::*;
use crate::transform::transform_messages;
use crate::types::*;

fn gemini_major(id: &str) -> Option<u32> {
    let id = id.to_lowercase();
    let rest = id.strip_prefix("gemini-live-").or_else(|| id.strip_prefix("gemini-"))?;
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

fn requires_tool_call_id(id: &str) -> bool {
    id.starts_with("claude-") || id.starts_with("gpt-oss-") || gemini_major(id).is_some_and(|v| v >= 3)
}

fn uses_thinking_level(id: &str) -> bool {
    let id = id.to_lowercase();
    let after_gemini = |prefix: &str| id.contains(prefix);
    (gemini_major(&id).is_some_and(|v| v >= 4))
        || (after_gemini("gemini-3") && (id.contains("-pro") || id.contains("-flash")))
        || id == "gemini-flash-latest"
        || id == "gemini-flash-lite-latest"
        || id.contains("gemma-4")
        || id.contains("gemma4")
}

fn valid_signature(sig: Option<&str>) -> Option<String> {
    let s = sig?;
    let ok = !s.is_empty()
        && s.len() % 4 == 0
        && s.trim_end_matches('=').chars().all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '/');
    ok.then(|| s.to_string())
}

fn strip_schema_meta(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(
            o.iter()
                .filter(|(k, _)| {
                    !matches!(
                        k.as_str(),
                        "$schema" | "$id" | "$anchor" | "$dynamicAnchor" | "$vocabulary" | "$comment" | "$defs" | "definitions"
                    )
                })
                .map(|(k, v)| (k.clone(), strip_schema_meta(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

pub fn convert_messages(model: &Model, ctx: &Context) -> Vec<Value> {
    let norm = |id: &str| -> String {
        if !requires_tool_call_id(&model.id) {
            return id.to_string();
        }
        id.chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
            .take(64)
            .collect()
    };
    let messages = transform_messages(&ctx.messages, model, Some(&norm));
    let mut contents: Vec<Value> = Vec::new();
    let multimodal_fr = gemini_major(&model.id).is_none_or(|v| v >= 3);
    for msg in &messages {
        match msg {
            Message::User { content, .. } => {
                let parts: Vec<Value> = content
                    .iter()
                    .map(|c| match c {
                        UserContent::Text { text } => json!({"text": text}),
                        UserContent::Image { data, mime_type } => json!({"inlineData": {"mimeType": mime_type, "data": data}}),
                    })
                    .collect();
                if !parts.is_empty() {
                    contents.push(json!({"role": "user", "parts": parts}));
                }
            }
            Message::Assistant(a) => {
                let same = a.provider == model.provider && a.model == model.id;
                let mut parts = Vec::new();
                for block in &a.content {
                    match block {
                        AssistantContent::Text { text, text_signature } => {
                            let sig = if same { valid_signature(text_signature.as_deref()) } else { None };
                            if text.trim().is_empty() && sig.is_none() {
                                continue;
                            }
                            let mut p = json!({"text": text});
                            if let Some(s) = sig {
                                p["thoughtSignature"] = s.into();
                            }
                            parts.push(p);
                        }
                        AssistantContent::Thinking {
                            thinking,
                            thinking_signature,
                            ..
                        } => {
                            if same {
                                let sig = valid_signature(thinking_signature.as_deref());
                                if thinking.trim().is_empty() && sig.is_none() {
                                    continue;
                                }
                                let mut p = json!({"thought": true, "text": thinking});
                                if let Some(s) = sig {
                                    p["thoughtSignature"] = s.into();
                                }
                                parts.push(p);
                            } else if !thinking.trim().is_empty() {
                                parts.push(json!({"text": thinking}));
                            }
                        }
                        AssistantContent::ToolCall(tc) => {
                            let mut fc = json!({"name": tc.name, "args": if tc.arguments.is_object() { tc.arguments.clone() } else { json!({}) }});
                            if requires_tool_call_id(&model.id) {
                                fc["id"] = tc.id.clone().into();
                            }
                            let mut p = json!({"functionCall": fc});
                            if let Some(s) = if same { valid_signature(tc.thought_signature.as_deref()) } else { None } {
                                p["thoughtSignature"] = s.into();
                            }
                            parts.push(p);
                        }
                    }
                }
                if !parts.is_empty() {
                    contents.push(json!({"role": "model", "parts": parts}));
                }
            }
            Message::ToolResult(tr) => {
                let text = tr
                    .content
                    .iter()
                    .filter_map(|c| match c {
                        UserContent::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let images: Vec<Value> = if model.supports_images() {
                    tr.content
                        .iter()
                        .filter_map(|c| match c {
                            UserContent::Image { data, mime_type } => Some(json!({"inlineData": {"mimeType": mime_type, "data": data}})),
                            _ => None,
                        })
                        .collect()
                } else {
                    Vec::new()
                };
                let value = if !text.trim().is_empty() {
                    text
                } else if !images.is_empty() {
                    "(see attached image)".into()
                } else {
                    String::new()
                };
                let mut fr = json!({
                    "name": tr.tool_name,
                    "response": if tr.is_error { json!({"error": value}) } else { json!({"output": value}) },
                });
                if !images.is_empty() && multimodal_fr {
                    fr["parts"] = Value::Array(images.clone());
                }
                if requires_tool_call_id(&model.id) {
                    fr["id"] = tr.tool_call_id.clone().into();
                }
                let part = json!({"functionResponse": fr});
                let merged = contents.last_mut().is_some_and(|last| {
                    if last["role"] == "user" && last["parts"].as_array().is_some_and(|p| p.iter().any(|x| x.get("functionResponse").is_some())) {
                        last["parts"].as_array_mut().unwrap().push(part.clone());
                        true
                    } else {
                        false
                    }
                });
                if !merged {
                    contents.push(json!({"role": "user", "parts": [part]}));
                }
                if !images.is_empty() && !multimodal_fr {
                    let mut parts = vec![json!({"text": "Tool result image:"})];
                    parts.extend(images);
                    contents.push(json!({"role": "user", "parts": parts}));
                }
            }
        }
    }
    contents
}

fn budget_for(model: &Model, level: &str) -> i64 {
    let table: Option<[i64; 4]> = if model.id.contains("2.5-pro") {
        Some([128, 2048, 8192, 32768])
    } else if model.id.contains("2.5-flash-lite") {
        Some([512, 2048, 8192, 24576])
    } else if model.id.contains("2.5-flash") {
        Some([128, 2048, 8192, 24576])
    } else {
        None
    };
    let idx = match level {
        "minimal" => 0,
        "low" => 1,
        "medium" => 2,
        _ => 3,
    };
    table.map(|t| t[idx]).unwrap_or(-1)
}

fn resolve_level(model: &Model, level: ThinkingLevel) -> String {
    match model.thinking_map(level) {
        Some(Some(v)) => v.to_lowercase(),
        _ => match level {
            ThinkingLevel::Xhigh | ThinkingLevel::Max => "high".into(),
            l => l.as_str().into(),
        },
    }
}

pub async fn stream_inner(model: Model, ctx: Context, opts: StreamOptions, em: &mut Emitter) -> anyhow::Result<()> {
    let api_key = opts.api_key.clone().unwrap_or_default();
    let mut config = Map::new();
    if let Some(t) = opts.temperature {
        config.insert("temperature".into(), t.into());
    }
    if let Some(m) = opts.max_tokens {
        config.insert("maxOutputTokens".into(), m.into());
    }
    let level = opts
        .reasoning
        .map(|r| clamp_thinking_level(&model, r))
        .filter(|r| *r != ThinkingLevel::Off);
    if model.reasoning {
        let tc = match level {
            Some(l) => {
                let resolved = resolve_level(&model, l);
                if uses_thinking_level(&model.id) {
                    json!({"includeThoughts": true, "thinkingLevel": resolved.to_uppercase()})
                } else {
                    json!({"includeThoughts": true, "thinkingBudget": budget_for(&model, &resolved)})
                }
            }
            None => {
                let fallback = clamp_thinking_level(&model, ThinkingLevel::Off);
                if !uses_thinking_level(&model.id) || fallback == ThinkingLevel::Off {
                    json!({"thinkingBudget": 0})
                } else {
                    json!({"thinkingLevel": resolve_level(&model, fallback).to_uppercase()})
                }
            }
        };
        config.insert("thinkingConfig".into(), tc);
    }
    let mut body = json!({"contents": convert_messages(&model, &ctx), "generationConfig": config});
    if !ctx.system_prompt.is_empty() {
        body["systemInstruction"] = json!({"parts": [{"text": ctx.system_prompt}]});
    }
    let mut tools = Vec::new();
    if !ctx.tools.is_empty() {
        let decls: Vec<Value> = ctx
            .tools
            .iter()
            .map(|t| json!({"name": t.name, "description": t.description, "parametersJsonSchema": strip_schema_meta(&t.parameters)}))
            .collect();
        tools.push(json!({"functionDeclarations": decls}));
    }
    if opts.native_web_search {
        tools.push(json!({"googleSearch": {}}));
    }
    if !tools.is_empty() {
        body["tools"] = Value::Array(tools);
    }

    let base = if model.base_url.is_empty() { "https://generativelanguage.googleapis.com/v1beta" } else { model.base_url.trim_end_matches('/') };
    let url = format!("{base}/models/{}:streamGenerateContent?alt=sse", model.id);
    let mut headers = model.headers.clone().unwrap_or_default();
    headers.extend(opts.headers.clone());
    let client = http_client();
    let resp = send_with_retry(
        || {
            let mut rb = client.post(&url).json(&body).header("x-goog-api-key", &api_key);
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

    let mut current: Option<(usize, bool)> = None; // (content index, is_thinking)
    let mut counter = 0u32;
    for_each_sse(resp, &opts.cancel, |ev| {
        let Ok(chunk) = serde_json::from_str::<Value>(&ev.data) else { return Ok(true) };
        if em.output.response_id.is_none() {
            em.output.response_id = chunk["responseId"].as_str().map(str::to_string);
        }
        let cand = &chunk["candidates"][0];
        if let Some(parts) = cand.pointer("/content/parts").and_then(Value::as_array) {
            for part in parts {
                if let Some(text) = part["text"].as_str() {
                    let thinking = part["thought"].as_bool() == Some(true);
                    if current.is_none_or(|(_, t)| t != thinking) {
                        if let Some((i, _)) = current {
                            em.finish_block(i, None);
                        }
                        let i = if thinking { em.start_thinking(None) } else { em.start_text() };
                        current = Some((i, thinking));
                    }
                    let (i, _) = current.unwrap();
                    let sig = part["thoughtSignature"].as_str().filter(|s| !s.is_empty()).map(str::to_string);
                    if thinking {
                        em.push_thinking(i, text);
                        if let (Some(s), Some(AssistantContent::Thinking { thinking_signature, .. })) = (sig, em.output.content.get_mut(i)) {
                            *thinking_signature = Some(s);
                        }
                    } else {
                        em.push_text(i, text);
                        if let (Some(s), Some(AssistantContent::Text { text_signature, .. })) = (sig, em.output.content.get_mut(i)) {
                            *text_signature = Some(s);
                        }
                    }
                }
                if let Some(fc) = part.get("functionCall") {
                    if let Some((i, _)) = current.take() {
                        em.finish_block(i, None);
                    }
                    let name = fc["name"].as_str().unwrap_or("").to_string();
                    let provided = fc["id"].as_str().map(str::to_string);
                    let dup = provided.as_ref().is_some_and(|p| em.output.tool_calls().any(|t| &t.id == p));
                    let id = match provided {
                        Some(p) if !dup => p,
                        _ => {
                            counter += 1;
                            format!("{name}_{}_{counter}", now_ms())
                        }
                    };
                    let args = if fc["args"].is_object() { fc["args"].clone() } else { json!({}) };
                    let i = em.start_tool_call(id, name);
                    if let Some(AssistantContent::ToolCall(tc)) = em.output.content.get_mut(i) {
                        tc.arguments = args.clone();
                        tc.thought_signature = part["thoughtSignature"].as_str().map(str::to_string);
                    }
                    let s = args.to_string();
                    em.tool_call_delta(i, &s, &s);
                    em.finish_block(i, None);
                }
            }
        }
        if let Some(reason) = cand["finishReason"].as_str() {
            em.output.raw_stop_reason = Some(reason.to_string());
            em.output.stop_reason = match reason {
                "STOP" => StopReason::Stop,
                "MAX_TOKENS" => StopReason::Length,
                _ => StopReason::Error,
            };
            if em.output.stop_reason == StopReason::Stop && em.output.tool_calls().next().is_some() {
                em.output.stop_reason = StopReason::ToolUse;
            }
        }
        let u = &chunk["usageMetadata"];
        if u.is_object() {
            let n = |k: &str| u[k].as_u64().unwrap_or(0);
            em.output.usage = Usage {
                input: n("promptTokenCount").saturating_sub(n("cachedContentTokenCount")),
                output: n("candidatesTokenCount") + n("thoughtsTokenCount"),
                cache_read: n("cachedContentTokenCount"),
                reasoning: Some(n("thoughtsTokenCount")),
                total_tokens: n("totalTokenCount"),
                ..Default::default()
            };
            calculate_cost(&model, &mut em.output.usage);
        }
        Ok(true)
    })
    .await?;
    if let Some((i, _)) = current {
        em.finish_block(i, None);
    }
    if opts.cancel.is_cancelled() {
        anyhow::bail!("Request was aborted");
    }
    match em.output.stop_reason {
        StopReason::Pending => anyhow::bail!("Google stream ended without a finish reason"),
        StopReason::Error | StopReason::Aborted => anyhow::bail!(
            "Provider stopped with: {}",
            em.output.raw_stop_reason.clone().unwrap_or_else(|| "unknown".into())
        ),
        _ => Ok(()),
    }
}
