//! Request-body fixtures: the exact JSON and headers each adapter would send, built from synthetic
//! models and keys. OpenRouter comes first because it is the venue serpis optimizes for. Whether
//! these bodies actually cache is checked end to end in serpis-agent's `tests/cache.rs`.

use serde_json::{Value, json};
use serpis_ai::api::{Request, build_request};
use serpis_ai::*;

const SID: &str = "session-0001";

fn model(api: &str, provider: &str, base_url: &str, id: &str) -> Model {
    serde_json::from_value(json!({
        "id": id, "api": api, "provider": provider, "baseUrl": base_url,
        "reasoning": false, "input": ["text", "image"],
    }))
    .unwrap()
}

fn openrouter(id: &str) -> Model {
    model("openai-completions", "openrouter", "https://openrouter.ai/api/v1", id)
}

fn tool(name: &str) -> Tool {
    Tool {
        name: name.into(),
        description: format!("The {name} tool."),
        parameters: json!({"type": "object", "properties": {"path": {"type": "string"}}, "required": ["path"]}),
    }
}

fn user(text: &str) -> Message {
    Message::User { content: vec![UserContent::Text { text: text.into() }], timestamp: 1 }
}

/// An assistant step that calls `read` on each path, followed by its results.
fn read_step(m: &Model, step: usize, paths: &[&str]) -> Vec<Message> {
    let mut a = AssistantMessage::empty(m);
    a.stop_reason = StopReason::ToolUse;
    a.content = paths
        .iter()
        .enumerate()
        .map(|(i, p)| {
            AssistantContent::ToolCall(ToolCall {
                id: format!("call_{step}_{i}"),
                name: "read".into(),
                arguments: json!({"path": p}),
                thought_signature: None,
                arguments_error: None,
            })
        })
        .collect();
    let mut out = vec![Message::Assistant(a)];
    for (i, p) in paths.iter().enumerate() {
        out.push(Message::ToolResult(ToolResultMessage {
            tool_call_id: format!("call_{step}_{i}"),
            tool_name: "read".into(),
            content: vec![UserContent::Text { text: format!("contents of {p}") }],
            is_error: false,
            details: Value::Null,
            timestamp: 1,
        }));
    }
    out
}

fn ctx(messages: Vec<Message>) -> Context {
    Context {
        system_prompt: "You are an expert coding assistant operating inside serpis.".into(),
        messages,
        tools: vec![tool("read"), tool("bash"), tool("edit")],
    }
}

fn opts(retention: CacheRetention) -> StreamOptions {
    StreamOptions {
        api_key: Some("test-key".into()),
        session_id: Some(SID.into()),
        cache_retention: retention,
        ..Default::default()
    }
}

fn build(m: &Model, c: &Context, retention: CacheRetention) -> Request {
    build_request(m, c, &opts(retention)).unwrap()
}

/// Every `cache_control` value in the body, in document order.
fn marks(v: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    fn walk(v: &Value, out: &mut Vec<Value>) {
        match v {
            Value::Object(o) => {
                for (k, x) in o {
                    if k == "cache_control" {
                        out.push(x.clone());
                    } else {
                        walk(x, out);
                    }
                }
            }
            Value::Array(a) => a.iter().for_each(|x| walk(x, out)),
            _ => {}
        }
    }
    walk(v, &mut out);
    out
}

fn has_mark(v: &Value) -> bool {
    !marks(v).is_empty()
}

fn strip_marks(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(o.iter().filter(|(k, _)| *k != "cache_control").map(|(k, x)| (k.clone(), strip_marks(x))).collect()),
        Value::Array(a) => Value::Array(a.iter().map(strip_marks).collect()),
        x => x.clone(),
    }
}

fn conversation(m: &Model) -> Vec<Message> {
    let mut msgs = vec![user("Fix the failing test")];
    msgs.extend(read_step(m, 1, &["src/lib.rs"]));
    msgs
}

// ---------------------------------------------------------------- OpenRouter

/// Indexes of the marked entries in `messages`.
fn marked(body: &Value) -> Vec<usize> {
    let msgs = body["messages"].as_array().unwrap();
    (0..msgs.len()).filter(|i| has_mark(&msgs[*i])).collect()
}

#[test]
fn openrouter_claude_marks_system_tools_and_both_request_ends() {
    let m = openrouter("anthropic/claude-sonnet-4.5");
    let r = build(&m, &ctx(conversation(&m)), CacheRetention::Short);
    assert_eq!(r.url, "https://openrouter.ai/api/v1/chat/completions");
    assert_eq!(r.header("authorization"), Some("Bearer test-key"));
    assert_eq!(r.header("x-session-id"), Some(SID));
    // system, user (the previous request's end), assistant, tool (this request's end)
    assert_eq!(marked(&r.body), vec![0, 1, 3]);
    let tools = r.body["tools"].as_array().unwrap();
    assert!(has_mark(tools.last().unwrap()) && !has_mark(&tools[0]), "only the last tool is marked");
    assert_eq!(marks(&r.body).len(), 4);
    assert!(marks(&r.body).iter().all(|c| *c == json!({"type": "ephemeral"})));
}

#[test]
fn a_first_request_marks_only_its_end() {
    let m = openrouter("anthropic/claude-sonnet-4.5");
    let r = build(&m, &ctx(vec![user("hello")]), CacheRetention::Short);
    assert_eq!(marked(&r.body), vec![0, 1]);
    assert_eq!(marks(&r.body).len(), 3);
}

#[test]
fn a_steering_message_does_not_move_the_previous_mark_into_this_request() {
    // tool results then a steering message, both after the newest assistant message: the
    // previous request ended at the first user message.
    let m = openrouter("anthropic/claude-sonnet-4.5");
    let mut msgs = conversation(&m);
    msgs.push(user("also check the tests"));
    let r = build(&m, &ctx(msgs), CacheRetention::Short);
    assert_eq!(marked(&r.body), vec![0, 1, 4]);
}

#[test]
fn openrouter_claude_long_retention_asks_for_one_hour() {
    let m = openrouter("anthropic/claude-sonnet-4.5");
    let r = build(&m, &ctx(conversation(&m)), CacheRetention::Long);
    assert!(marks(&r.body).iter().all(|c| *c == json!({"type": "ephemeral", "ttl": "1h"})));
}

#[test]
fn openrouter_no_retention_sends_no_marks_or_session() {
    let m = openrouter("anthropic/claude-sonnet-4.5");
    let r = build(&m, &ctx(conversation(&m)), CacheRetention::None);
    assert!(marks(&r.body).is_empty());
    assert_eq!(r.header("x-session-id"), None);
}

#[test]
fn openrouter_openai_models_rely_on_automatic_caching() {
    let m = openrouter("openai/gpt-5.1");
    let r = build(&m, &ctx(conversation(&m)), CacheRetention::Short);
    assert!(marks(&r.body).is_empty());
    assert_eq!(r.header("x-session-id"), Some(SID));
}

#[test]
fn openrouter_gemini_is_marked_like_claude() {
    let m = openrouter("google/gemini-2.5-pro");
    let r = build(&m, &ctx(conversation(&m)), CacheRetention::Short);
    assert_eq!(marks(&r.body).len(), 4);
}

#[test]
fn openrouter_under_another_provider_name_is_still_openrouter() {
    let m = model("openai-completions", "my-router", "https://openrouter.ai/api/v1", "anthropic/claude-sonnet-4.5");
    let r = build(&m, &ctx(conversation(&m)), CacheRetention::Short);
    assert_eq!(r.header("x-session-id"), Some(SID));
    assert_eq!(marks(&r.body).len(), 4);
}

#[test]
fn openrouter_anthropic_messages_path_marks_and_groups_the_session() {
    let m = model("anthropic-messages", "openrouter", "https://openrouter.ai/api", "anthropic/claude-sonnet-4.5");
    let r = build(&m, &ctx(conversation(&m)), CacheRetention::Short);
    assert_eq!(r.url, "https://openrouter.ai/api/v1/messages");
    assert_eq!(r.header("x-session-id"), Some(SID));
    assert_eq!(marks(&r.body).len(), 4, "system, last tool, both request ends");
}

#[test]
fn parallel_reads_keep_a_mark_on_the_previous_request_end() {
    // 10 parallel reads add 21 blocks, past the 20-block lookback from the last mark alone.
    let m = openrouter("anthropic/claude-sonnet-4.5");
    let paths: Vec<String> = (0..10).map(|i| format!("src/f{i}.rs")).collect();
    let paths: Vec<&str> = paths.iter().map(String::as_str).collect();
    let mut msgs = conversation(&m);
    msgs.extend(read_step(&m, 2, &paths));
    let r = build(&m, &ctx(msgs), CacheRetention::Short);
    let msgs = r.body["messages"].as_array().unwrap();
    let marked: Vec<usize> = (1..msgs.len()).filter(|i| has_mark(&msgs[*i])).collect();
    assert_eq!(marked, vec![3, msgs.len() - 1], "the first step's tool result and the last result");
}

// ---------------------------------------------------------------- direct providers

#[test]
fn anthropic_direct_uses_api_key_and_four_marks() {
    let m = model("anthropic-messages", "anthropic", "https://api.anthropic.com", "claude-sonnet-4-5");
    let r = build(&m, &ctx(conversation(&m)), CacheRetention::Short);
    assert_eq!(r.header("x-api-key"), Some("test-key"));
    assert_eq!(r.header("authorization"), None);
    assert_eq!(r.header("x-session-id"), None);
    assert_eq!(marks(&r.body).len(), 4);
}

#[test]
fn anthropic_oauth_leaves_the_identity_block_unmarked() {
    // The system prompt's mark right after it covers it, and a fifth mark would be rejected.
    let m = model("anthropic-messages", "anthropic", "https://api.anthropic.com", "claude-sonnet-4-5");
    let mut o = opts(CacheRetention::Short);
    o.api_key = Some("sk-ant-oat01-test".into());
    let r = build_request(&m, &ctx(conversation(&m)), &o).unwrap();
    assert_eq!(r.header("authorization"), Some("Bearer sk-ant-oat01-test"));
    assert_eq!(r.header("x-api-key"), None);
    assert!(!has_mark(&r.body["system"][0]));
    assert_eq!(marks(&r.body).len(), 4);
}

#[test]
fn configured_authorization_header_wins_over_the_key() {
    let m = openrouter("anthropic/claude-sonnet-4.5");
    let mut o = opts(CacheRetention::Short);
    o.headers.insert("Authorization".into(), "Bearer from-config".into());
    let r = build_request(&m, &ctx(conversation(&m)), &o).unwrap();
    assert_eq!(r.header("authorization"), Some("Bearer from-config"));
    assert_eq!(r.headers.keys().filter(|k| k.eq_ignore_ascii_case("authorization")).count(), 1);
}

#[test]
fn openai_direct_routes_by_session_key() {
    let m = model("openai-completions", "openai", "https://api.openai.com/v1", "gpt-5.1");
    let r = build(&m, &ctx(conversation(&m)), CacheRetention::Short);
    assert_eq!(r.body["prompt_cache_key"], SID);
    let r = build(&m, &ctx(conversation(&m)), CacheRetention::Long);
    assert_eq!(r.body["prompt_cache_retention"], "24h");
}

#[test]
fn openai_responses_routes_by_session_key() {
    let m = model("openai-responses", "openai", "https://api.openai.com/v1", "gpt-5.1");
    let r = build(&m, &ctx(conversation(&m)), CacheRetention::Short);
    assert_eq!(r.url, "https://api.openai.com/v1/responses");
    assert_eq!(r.body["prompt_cache_key"], SID);
}

#[test]
fn google_sends_key_header() {
    let m = model("google-generative-ai", "google", "", "gemini-2.5-pro");
    let r = build(&m, &ctx(conversation(&m)), CacheRetention::Short);
    assert_eq!(r.header("x-goog-api-key"), Some("test-key"));
    assert!(r.url.ends_with("/models/gemini-2.5-pro:streamGenerateContent?alt=sse"));
}

// ---------------------------------------------------------------- prefix stability

/// Appending a turn must leave every earlier byte unchanged (marks aside, since they move),
/// or the provider cannot reuse the cached prefix.
#[test]
fn appending_a_turn_never_rewrites_earlier_messages() {
    let models = [
        openrouter("anthropic/claude-sonnet-4.5"),
        openrouter("openai/gpt-5.1"),
        model("anthropic-messages", "anthropic", "https://api.anthropic.com", "claude-sonnet-4-5"),
        model("openai-responses", "openai", "https://api.openai.com/v1", "gpt-5.1"),
        model("google-generative-ai", "google", "", "gemini-2.5-pro"),
    ];
    for m in &models {
        let before = conversation(m);
        let mut after = before.clone();
        after.extend(read_step(m, 2, &["src/a.rs", "src/b.rs"]));
        after.push(user("Now run the tests"));
        let a = build(m, &ctx(before), CacheRetention::Short).body;
        let b = build(m, &ctx(after), CacheRetention::Short).body;
        let key = ["messages", "input", "contents"].into_iter().find(|k| a.get(*k).is_some()).unwrap();
        let (a_msgs, b_msgs) = (strip_marks(&a[key]), strip_marks(&b[key]));
        let (a_msgs, b_msgs) = (a_msgs.as_array().unwrap(), b_msgs.as_array().unwrap());
        assert_eq!(a_msgs[..], b_msgs[..a_msgs.len()], "{} {}: earlier messages changed", m.api, m.id);
        for k in ["system", "tools", "systemInstruction"] {
            assert_eq!(strip_marks(&a[k]), strip_marks(&b[k]), "{} {}: {k} changed", m.api, m.id);
        }
        assert_eq!(serde_json::to_string(&a).unwrap(), serde_json::to_string(&build(m, &ctx(conversation(m)), CacheRetention::Short).body).unwrap(), "{}: not deterministic", m.id);
    }
}

#[test]
fn a_gateway_in_front_of_openrouter_is_openrouter_through_compat() {
    let mut m = model("openai-completions", "gateway", "http://127.0.0.1:8080/v1", "anthropic/claude-sonnet-4.5");
    assert!(!m.is_openrouter());
    m.compat = Some(serde_json::from_value(json!({"openRouter": true})).unwrap());
    let r = build(&m, &ctx(conversation(&m)), CacheRetention::Short);
    assert_eq!(r.header("x-session-id"), Some(SID));
    assert_eq!(marks(&r.body).len(), 4);
}

#[test]
fn an_assistant_tool_step_without_text_has_empty_string_content_on_openrouter_except_anthropic_and_gemini() {
    // CoreWeave's DeepSeek template fails on null content next to tool calls.
    let content = |m: &Model| {
        let r = build(m, &ctx(conversation(m)), CacheRetention::Short);
        let msgs = r.body["messages"].as_array().unwrap().clone();
        msgs.into_iter().find(|x| x["role"] == "assistant").unwrap()["content"].clone()
    };
    assert_eq!(content(&openrouter("deepseek/deepseek-chat-v3.1")), json!(""));
    assert_eq!(content(&openrouter("openai/gpt-5.1")), json!(""));
    assert_eq!(content(&openrouter("anthropic/claude-sonnet-4.5")), Value::Null);
    assert_eq!(content(&openrouter("google/gemini-2.5-pro")), Value::Null);
    assert_eq!(content(&model("openai-completions", "openai", "https://api.openai.com/v1", "gpt-5.1")), Value::Null);
    let mut m = openrouter("anthropic/claude-sonnet-4.5");
    m.compat = Some(serde_json::from_value(json!({"emptyAssistantContentAsString": true})).unwrap());
    assert_eq!(content(&m), json!(""), "compat overrides the detection");
}
