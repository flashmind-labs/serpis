//! End to end: prompt caching against real providers through OpenRouter. It costs money, so it
//! never runs by default: it is ignored, and also needs SERPIS_LIVE_TEST=1 and OPENROUTER_API_KEY
//! in the environment (no key file is read). Run it with scripts/live-cache-check.sh.
//!
//! Each model gets a short conversation sent exactly as a chat sends it (serpis' adapters, a
//! session id, 1-hour retention): a first request that writes the cache, a follow-up turn, a step
//! with 12 parallel tool calls (past Anthropic's 20-block lookback), another turn, and a side
//! request off the shared prefix (what a compaction summary request is): the conversation up to
//! the tool step plus a new instruction. Every
//! request also goes through serpis-testkit's simulator; where the simulator expects a cache read
//! and the provider reports none, the simulator or serpis is wrong about that provider. For
//! explicit caches (Anthropic, Gemini) that fails the test; for best-effort automatic caches it is
//! reported. A random marker opens the system prompt so earlier runs' caches can't help.
//!
//! SERPIS_LIVE_MODELS=a/b,c/d replaces the default models. SERPIS_LIVE_ROUTING sets OpenRouter's
//! `provider` routing object on every request (it is a model's compat.openRouterRouting), e.g.
//! '{"order":["coreweave"],"allow_fallbacks":false}' to see an upstream's own errors instead of a
//! silent fallback.

use std::time::Duration;

use serde_json::{Value, json};
use serpis_ai::api::build_request;
use serpis_ai::*;
use serpis_testkit::{Config, Simulator, Wire};

/// For each family, the first of these OpenRouter lists is used.
const DEFAULT_MODELS: &[&[&str]] = &[
    &["anthropic/claude-haiku-5.5", "anthropic/claude-haiku-4.5"],
    &["google/gemini-2.5-flash", "google/gemini-2.0-flash-001"],
    &["openai/gpt-5-mini", "openai/gpt-4.1-mini"],
    &["deepseek/deepseek-chat-v3.1", "deepseek/deepseek-chat"],
];

fn enabled() -> Option<String> {
    let key = std::env::var("OPENROUTER_API_KEY").ok().filter(|k| !k.is_empty())?;
    (std::env::var("SERPIS_LIVE_TEST").as_deref() == Ok("1")).then_some(key)
}

/// OpenRouter's public model list: id → (input $/Mtok, cache read $/Mtok, cache write $/Mtok).
async fn catalog() -> std::collections::HashMap<String, (f64, f64, f64)> {
    let v: Value = reqwest::get("https://openrouter.ai/api/v1/models").await.expect("model list").json().await.expect("model list json");
    let per_m = |p: &Value, k: &str| p[k].as_str().and_then(|s| s.parse::<f64>().ok()).unwrap_or(0.0) * 1e6;
    v["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|m| {
            let p = &m["pricing"];
            Some((m["id"].as_str()?.to_string(), (per_m(p, "prompt"), per_m(p, "input_cache_read"), per_m(p, "input_cache_write"))))
        })
        .collect()
}

fn model(id: &str, price: (f64, f64, f64)) -> Model {
    let routing: Option<Value> = std::env::var("SERPIS_LIVE_ROUTING").ok().map(|r| serde_json::from_str(&r).expect("SERPIS_LIVE_ROUTING is JSON"));
    serde_json::from_value(json!({
        "id": id, "api": "openai-completions", "provider": "openrouter", "baseUrl": "https://openrouter.ai/api/v1",
        "reasoning": false, "input": ["text"], "maxTokens": 64,
        "cost": {"input": price.0, "output": 0.0, "cacheRead": price.1, "cacheWrite": price.2},
        "compat": routing.map(|r| json!({"openRouterRouting": r})),
    }))
    .unwrap()
}

/// About 7K tokens, above every model's minimum cacheable prompt.
fn system_prompt(marker: &str) -> String {
    let mut s = format!("Run {marker}. You are a terse assistant: answer in at most five words.\n\n");
    for i in 0..400 {
        s.push_str(&format!("Rule {i}: when the user mentions item {i}, answer precisely and keep it short.\n"));
    }
    s
}

fn tools() -> Vec<Tool> {
    vec![Tool {
        name: "read".into(),
        description: "Read a file and return its contents.".into(),
        parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
    }]
}

fn user(t: &str) -> Message {
    Message::User { content: vec![UserContent::Text { text: t.into() }], timestamp: 1 }
}

/// A recorded step with `n` parallel reads and their results.
fn read_step(m: &Model, n: usize) -> Vec<Message> {
    let mut a = AssistantMessage::empty(m);
    a.stop_reason = StopReason::ToolUse;
    a.content = (0..n)
        .map(|i| AssistantContent::ToolCall(ToolCall { id: format!("call_{i}"), name: "read".into(), arguments: json!({"path": format!("f{i}.txt")}), thought_signature: None, arguments_error: None }))
        .collect();
    let mut out = vec![Message::Assistant(a)];
    for i in 0..n {
        out.push(Message::ToolResult(ToolResultMessage {
            tool_call_id: format!("call_{i}"),
            tool_name: "read".into(),
            content: vec![UserContent::Text { text: format!("line one of f{i}\nline two of f{i}") }],
            is_error: false,
            details: Value::Null,
            timestamp: 1,
        }));
    }
    out
}

async fn send(m: &Model, ctx: &Context, o: &StreamOptions) -> AssistantMessage {
    let mut rx = serpis_ai::stream(m, ctx, o.clone());
    while let Some(ev) = rx.recv().await {
        if let StreamEvent::Done(a) | StreamEvent::Error(a) = ev {
            return a;
        }
    }
    let mut a = AssistantMessage::empty(m);
    a.stop_reason = StopReason::Error;
    a.error_message = Some("stream closed".into());
    a
}

#[tokio::test]
#[ignore = "costs money: run scripts/live-cache-check.sh"]
async fn real_providers_cache_like_the_simulator_says() {
    let Some(key) = enabled() else {
        eprintln!("skipped: set SERPIS_LIVE_TEST=1 and OPENROUTER_API_KEY (scripts/live-cache-check.sh)");
        return;
    };
    let cat = catalog().await;
    let wanted: Vec<Vec<String>> = match std::env::var("SERPIS_LIVE_MODELS") {
        Ok(list) => list.split(',').map(|s| vec![s.trim().to_string()]).collect(),
        Err(_) => DEFAULT_MODELS.iter().map(|f| f.iter().map(|s| s.to_string()).collect()).collect(),
    };
    let marker = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos());
    let session = format!("serpis-live-{marker}");
    let mut failures = Vec::new();
    let mut spent = 0.0;

    for family in wanted {
        let Some(id) = family.iter().find(|id| cat.contains_key(*id)) else {
            println!("\n{}: not on OpenRouter, skipped", family.join(" / "));
            continue;
        };
        let m = model(id, cat[id]);
        let explicit = id.starts_with("anthropic/") || id.starts_with("google/");
        println!("\n{id} ({} cache)", if explicit { "explicit" } else { "automatic" });
        println!("  request                 read   write   input  upstream          simulator");
        let o = StreamOptions {
            api_key: Some(key.clone()),
            session_id: Some(format!("{session}-{id}")),
            cache_retention: CacheRetention::Long,
            max_tokens: Some(64),
            max_retries: Some(1),
            ..Default::default()
        };
        let mut sim = Simulator::new(Config::default());
        let mut history = vec![user("Reply with OK.")];
        let mut upstream: Option<String> = None;
        let steps: [(&str, Option<usize>); 5] = [("first", None), ("next turn", None), ("12 parallel reads", Some(12)), ("another turn", None), ("side request", None)];
        let mut before_reads = 0;
        for (k, (label, reads)) in steps.into_iter().enumerate() {
            if k > 0 && k < 4 {
                match reads {
                    Some(n) => {
                        before_reads = history.len();
                        history.extend(read_step(&m, n))
                    }
                    None => history.push(user(&format!("Say OK again ({k})."))),
                }
            }
            let messages = if k == 4 {
                let mut side = history[..before_reads].to_vec();
                side.push(user("Stop here and summarize the conversation in one line."));
                side
            } else {
                history.clone()
            };
            let ctx = Context { system_prompt: system_prompt(&marker), messages, tools: tools() };
            let req = build_request(&m, &ctx, &o).expect("build request");
            let predicted = sim.request(Wire::ChatCompletions, &req.headers.clone().into_iter().collect::<Vec<_>>(), &req.body);
            let a = send(&m, &ctx, &o).await;
            let u = &a.usage;
            spent += (u.input as f64 * m.cost.input + u.cache_read as f64 * m.cost.cache_read + u.cache_write as f64 * m.cost.cache_write.max(m.cost.input)) / 1e6;
            let sim_note = match &predicted {
                Ok(p) if p.read > 0 => format!("expects a read (~{})", p.read),
                Ok(p) if p.write > 0 => format!("expects a write (~{})", p.write),
                Ok(_) => "expects nothing cached".into(),
                Err(e) => format!("rejects: {e}"),
            };
            println!(
                "  {label:<20} {:>7} {:>7} {:>7}  {:<16}  {sim_note}",
                u.cache_read,
                u.cache_write,
                u.input,
                a.upstream.as_deref().unwrap_or("-")
            );
            if a.stop_reason == StopReason::Error {
                let err = a.error_message.unwrap_or_default();
                // OpenRouter refuses models the account's privacy settings exclude (zero data
                // retention, data policy): that says nothing about caching.
                if err.contains("guardrail restrictions and data policy") {
                    println!("  skipped: the account's OpenRouter privacy settings exclude every endpoint for this model");
                } else {
                    failures.push(format!("{id} {label}: rejected: {err}"));
                }
                break;
            }
            if let Ok(p) = &predicted {
                if p.read > 0 && u.cache_read == 0 {
                    let msg = format!("{id} {label}: the simulator expects a cache read, the provider reported none");
                    if explicit { failures.push(msg) } else { println!("  warning: {msg} (best-effort cache)") }
                }
                if k == 0 && explicit && p.write > 0 && u.cache_write == 0 && u.cache_read == 0 {
                    println!("  note: no cache write reported (this provider may not report writes)");
                }
            }
            if let (Some(prev), Some(now)) = (&upstream, &a.upstream) {
                if prev != now {
                    println!("  warning: OpenRouter moved the conversation from {prev} to {now} (the sticky upstream failed or was unavailable)");
                }
            }
            upstream = a.upstream.clone().or(upstream);
            if k < 4 {
                history.push(Message::Assistant(a));
            }
            // Automatic caches take a moment to become readable.
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    }
    println!("\nabout ${spent:.3} spent");
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}
