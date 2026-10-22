//! Property tests for prompt caching: generated transcripts, shaped the way the agent loop
//! records them, go through every adapter's `build_request` into serpis-testkit's simulator of
//! provider caches. OpenRouter venues come first.
//!
//! - Append-only means hits: no miss and no rejected request, whatever the shape of the turns.
//! - Determinism: the same history is the same bytes.
//! - A change the user makes (thinking, model), a pause past the TTL, or an OpenRouter fallback
//!   is reported as exactly that cause, once, and the next request hits again.

use proptest::prelude::*;
use serde_json::{Value, json};
use serpis_ai::api::build_request;
use serpis_ai::*;
use serpis_testkit::{Config, Miss, Simulator, Wire};

const SID: &str = "chat-0001";
const PIXEL: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

#[derive(Debug, Clone, Copy)]
enum Venue {
    OpenRouterClaude,
    OpenRouterClaudeMessages,
    OpenRouterGemini,
    OpenRouterOpenAi,
    AnthropicDirect,
    OpenAiDirect,
}

const VENUES: [Venue; 6] = [
    Venue::OpenRouterClaude,
    Venue::OpenRouterClaudeMessages,
    Venue::OpenRouterGemini,
    Venue::OpenRouterOpenAi,
    Venue::AnthropicDirect,
    Venue::OpenAiDirect,
];

impl Venue {
    fn openrouter(self) -> bool {
        !matches!(self, Venue::AnthropicDirect | Venue::OpenAiDirect)
    }

    fn model(self, suffix: &str) -> Model {
        let (api, provider, url, id) = match self {
            Venue::OpenRouterClaude => ("openai-completions", "openrouter", "https://openrouter.ai/api/v1", "anthropic/claude-sonnet-4.5"),
            Venue::OpenRouterClaudeMessages => ("anthropic-messages", "openrouter", "https://openrouter.ai/api", "anthropic/claude-sonnet-4.5"),
            Venue::OpenRouterGemini => ("openai-completions", "openrouter", "https://openrouter.ai/api/v1", "google/gemini-2.5-pro"),
            Venue::OpenRouterOpenAi => ("openai-completions", "openrouter", "https://openrouter.ai/api/v1", "openai/gpt-5.1"),
            Venue::AnthropicDirect => ("anthropic-messages", "anthropic", "https://api.anthropic.com", "claude-sonnet-4-5"),
            Venue::OpenAiDirect => ("openai-completions", "openai", "https://api.openai.com/v1", "gpt-5.1"),
        };
        serde_json::from_value(json!({"id": format!("{id}{suffix}"), "api": api, "provider": provider, "baseUrl": url, "reasoning": true, "input": ["text", "image"]})).unwrap()
    }

    fn sim(self) -> Simulator {
        let mut c = Config::default();
        if !self.openrouter() {
            c.upstreams = vec!["direct".into()];
        }
        Simulator::new(c)
    }
}

// ------------------------------------------------------------------ transcripts

#[derive(Debug, Clone)]
struct Call {
    arg: String,
    result: String,
    error: bool,
    image: bool,
}

#[derive(Debug, Clone)]
struct Step {
    thinking: Option<String>,
    text: Option<String>,
    calls: Vec<Call>,
    steer: Option<String>,
}

#[derive(Debug, Clone)]
enum End {
    Answer(String),
    Failed,
    Aborted(String),
}

#[derive(Debug, Clone)]
struct Turn {
    user: String,
    image: bool,
    steps: Vec<Step>,
    end: End,
}

/// Text with the characters that trip up serialization: quotes, newlines, accents, emoji.
fn text() -> impl Strategy<Value = String> {
    "[a-z][a-zA-Z0-9 .,:\"'\\\\é漢😀\n]{0,60}"
}

fn call() -> impl Strategy<Value = Call> {
    (text(), text(), prop::bool::weighted(0.15), prop::bool::weighted(0.1)).prop_map(|(arg, result, error, image)| Call { arg, result, error, image })
}

fn step() -> impl Strategy<Value = Step> {
    (prop::option::of(text()), prop::option::of(text()), prop::collection::vec(call(), 1..=10), prop::option::weighted(0.2, text()))
        .prop_map(|(thinking, text, calls, steer)| Step { thinking, text, calls, steer })
}

fn end() -> impl Strategy<Value = End> {
    prop_oneof![8 => text().prop_map(End::Answer), 1 => Just(End::Failed), 1 => text().prop_map(End::Aborted)]
}

fn turn() -> impl Strategy<Value = Turn> {
    (text(), prop::bool::weighted(0.15), prop::collection::vec(step(), 0..4), end()).prop_map(|(user, image, steps, end)| Turn { user, image, steps, end })
}

fn transcript() -> impl Strategy<Value = Vec<Turn>> {
    prop::collection::vec(turn(), 1..6)
}

fn user(text: &str, image: bool) -> Message {
    let mut content = vec![UserContent::Text { text: text.into() }];
    if image {
        content.push(UserContent::Image { data: PIXEL.into(), mime_type: "image/png".into() });
    }
    Message::User { content, timestamp: 1 }
}

fn assistant(m: &Model, content: Vec<AssistantContent>, stop: StopReason) -> Message {
    let mut a = AssistantMessage::empty(m);
    a.content = content;
    a.stop_reason = stop;
    if stop == StopReason::Error {
        a.error_message = Some("provider error".into());
    }
    Message::Assistant(a)
}

/// The history at each point the agent sends a request, for `m`.
fn request_points(m: &Model, turns: &[Turn]) -> Vec<Vec<Message>> {
    let mut history = Vec::new();
    let mut points = Vec::new();
    for (t, turn) in turns.iter().enumerate() {
        history.push(user(&turn.user, turn.image));
        points.push(history.clone());
        for (s, step) in turn.steps.iter().enumerate() {
            let mut content = Vec::new();
            if let Some(th) = &step.thinking {
                content.push(AssistantContent::Thinking { thinking: th.clone(), thinking_signature: Some(format!("sig-{t}-{s}")), redacted: false });
            }
            if let Some(tx) = &step.text {
                content.push(AssistantContent::Text { text: tx.clone(), text_signature: None });
            }
            for (i, c) in step.calls.iter().enumerate() {
                content.push(AssistantContent::ToolCall(ToolCall {
                    id: format!("call_{t}_{s}_{i}"),
                    name: ["read", "bash", "edit"][i % 3].into(),
                    arguments: json!({"path": c.arg, "n": i}),
                    thought_signature: None,
                    arguments_error: None,
                }));
            }
            history.push(assistant(m, content, StopReason::ToolUse));
            for (i, c) in step.calls.iter().enumerate() {
                let mut content = vec![UserContent::Text { text: c.result.clone() }];
                if c.image {
                    content.push(UserContent::Image { data: PIXEL.into(), mime_type: "image/png".into() });
                }
                history.push(Message::ToolResult(ToolResultMessage {
                    tool_call_id: format!("call_{t}_{s}_{i}"),
                    tool_name: ["read", "bash", "edit"][i % 3].into(),
                    content,
                    is_error: c.error,
                    details: Value::Null,
                    timestamp: 1,
                }));
            }
            if let Some(st) = &step.steer {
                history.push(user(st, false));
            }
            points.push(history.clone());
        }
        match &turn.end {
            End::Answer(a) => history.push(assistant(m, vec![AssistantContent::Text { text: a.clone(), text_signature: None }], StopReason::Stop)),
            End::Failed => history.push(assistant(m, vec![], StopReason::Error)),
            End::Aborted(a) => history.push(assistant(m, vec![AssistantContent::Text { text: a.clone(), text_signature: None }], StopReason::Aborted)),
        }
    }
    points
}

/// A system prompt and tools the size of serpis' own, so prefixes clear the cacheable minimum.
fn context(messages: Vec<Message>) -> Context {
    let tool = |name: &str| Tool {
        name: name.into(),
        description: format!("The {name} tool. ").repeat(30),
        parameters: json!({"type": "object", "properties": {"path": {"type": "string", "description": "A path."}, "n": {"type": "integer"}}, "required": ["path"]}),
    };
    Context {
        system_prompt: "You are an expert coding assistant operating inside serpis. ".repeat(80),
        messages,
        tools: vec![tool("read"), tool("bash"), tool("edit"), tool("write")],
    }
}

fn opts(retention: CacheRetention, reasoning: ThinkingLevel) -> StreamOptions {
    StreamOptions {
        api_key: Some("test-key".into()),
        session_id: Some(SID.into()),
        cache_retention: retention,
        reasoning: Some(reasoning),
        ..Default::default()
    }
}

fn wire(m: &Model) -> Wire {
    if m.api == "anthropic-messages" { Wire::AnthropicMessages } else { Wire::ChatCompletions }
}

fn send(sim: &mut Simulator, m: &Model, history: Vec<Message>, o: &StreamOptions) -> Result<serpis_testkit::Outcome, String> {
    let r = build_request(m, &context(history), o).map_err(|e| e.to_string())?;
    let headers: Vec<(String, String)> = r.headers.into_iter().collect();
    sim.request(wire(m), &headers, &r.body)
}

fn kind(m: &Option<Miss>) -> &'static str {
    match m {
        None => "none",
        Some(Miss::Model) => "model",
        Some(Miss::Params) => "params",
        Some(Miss::Expired) => "expired",
        Some(Miss::Upstream) => "upstream",
        Some(Miss::Lookback) => "lookback",
        Some(Miss::PrefixChanged { .. }) => "prefix",
        Some(Miss::Unknown) => "unknown",
    }
}

#[derive(Debug, Clone, Copy)]
enum Change {
    Thinking,
    Model,
    Pause,
    Fallback,
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 200, ..ProptestConfig::default() })]

    #[test]
    fn append_only_transcripts_always_hit(turns in transcript(), long in any::<bool>()) {
        let retention = if long { CacheRetention::Long } else { CacheRetention::Short };
        for venue in VENUES {
            let m = venue.model("");
            let mut sim = venue.sim();
            let o = opts(retention, ThinkingLevel::Medium);
            for (k, h) in request_points(&m, &turns).into_iter().enumerate() {
                let out = send(&mut sim, &m, h, &o);
                prop_assert!(out.is_ok(), "{venue:?} request {k} rejected: {:?}", out.err());
                let out = out.unwrap();
                prop_assert!(out.miss.is_none(), "{venue:?} request {k}: {:?}", out.miss);
                if k > 0 {
                    prop_assert!(out.read > 0, "{venue:?} request {k} read nothing: {out:?}");
                }
                if out.explicit {
                    prop_assert!(out.marks <= 4, "{venue:?} request {k}: {} marks", out.marks);
                }
            }
        }
    }

    #[test]
    fn the_same_history_is_the_same_bytes(turns in transcript()) {
        for venue in VENUES {
            let m = venue.model("");
            let o = opts(CacheRetention::Long, ThinkingLevel::Medium);
            for h in request_points(&m, &turns) {
                let a = build_request(&m, &context(h.clone()), &o).unwrap();
                let b = build_request(&m, &context(h), &o).unwrap();
                prop_assert_eq!(a.body.to_string(), b.body.to_string());
            }
        }
    }

    #[test]
    fn a_change_is_its_own_cause_and_the_cache_recovers(
        turns in transcript(),
        at in any::<prop::sample::Index>(),
        change in prop_oneof![Just(Change::Thinking), Just(Change::Model), Just(Change::Pause), Just(Change::Fallback)],
    ) {
        for venue in VENUES {
            if matches!(change, Change::Fallback) && !venue.openrouter() {
                continue;
            }
            let base = venue.model("");
            let points = request_points(&base, &turns);
            if points.len() < 3 {
                continue;
            }
            // Change before request k (not the first), and check k and k + 1.
            let k = 1 + at.index(points.len() - 2);
            let mut sim = venue.sim();
            let (mut m, mut level) = (base.clone(), ThinkingLevel::Medium);
            let want = match change {
                Change::Thinking => "params",
                Change::Model => "model",
                Change::Pause => "expired",
                Change::Fallback => "upstream",
            };
            for (i, _) in points.iter().enumerate() {
                if i == k {
                    match change {
                        Change::Thinking => level = ThinkingLevel::High,
                        Change::Model => m = venue.model("-next"),
                        Change::Pause => sim.advance(std::time::Duration::from_secs(11 * 60)),
                        Change::Fallback => sim.move_upstream(SID),
                    }
                }
                // The transcript as the current model would have recorded it.
                let h = request_points(&m, &turns)[i].clone();
                let out = send(&mut sim, &m, h, &opts(CacheRetention::Short, level)).unwrap();
                if i == k {
                    prop_assert_eq!(kind(&out.miss), want, "{:?} request {}: {:?}", venue, i, out.miss);
                } else if i < k || i == k + 1 {
                    prop_assert_eq!(kind(&out.miss), "none", "{:?} request {} (change at {}): {:?}", venue, i, k, out.miss);
                }
                if i > k + 1 {
                    break;
                }
            }
        }
    }
}
