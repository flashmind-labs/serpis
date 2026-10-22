//! Cache scenarios: real agent runs against the mock provider in `serpis-testkit`, whose
//! simulator applies the providers' caching rules to the exact bytes serpis sends. Each scenario
//! runs on every venue, OpenRouter first, and states which requests may miss and why. Any other
//! miss (a changed prefix, a lookback miss) fails with a per-request report.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use serpis_agent::system_prompt::{PromptOptions, build_system_prompt};
use serpis_agent::{AgentConfig, AgentEvent, AgentHandle, builtin_tools, run};
use serpis_ai::*;
use serpis_ai::CancellationToken;
use serpis_testkit::{Config, Miss, Mock, commands};

#[derive(Debug, Clone, Copy, PartialEq)]
enum Venue {
    OpenRouterClaude,
    OpenRouterClaudeMessages,
    OpenRouterOpenAi,
    AnthropicDirect,
    OpenAiDirect,
}

const VENUES: [Venue; 5] = [
    Venue::OpenRouterClaude,
    Venue::OpenRouterClaudeMessages,
    Venue::OpenRouterOpenAi,
    Venue::AnthropicDirect,
    Venue::OpenAiDirect,
];

impl Venue {
    fn openrouter(self) -> bool {
        matches!(self, Venue::OpenRouterClaude | Venue::OpenRouterClaudeMessages | Venue::OpenRouterOpenAi)
    }

    fn explicit(self) -> bool {
        matches!(self, Venue::OpenRouterClaude | Venue::OpenRouterClaudeMessages | Venue::AnthropicDirect)
    }

    fn model(self, base: &str, id_suffix: &str) -> Model {
        let (api, provider, url, id) = match self {
            Venue::OpenRouterClaude => ("openai-completions", "openrouter", format!("{base}/api/v1"), "anthropic/claude-sonnet-4.5"),
            Venue::OpenRouterClaudeMessages => ("anthropic-messages", "openrouter", format!("{base}/api"), "anthropic/claude-sonnet-4.5"),
            Venue::OpenRouterOpenAi => ("openai-completions", "openrouter", format!("{base}/api/v1"), "openai/gpt-5.1"),
            Venue::AnthropicDirect => ("anthropic-messages", "anthropic", base.to_string(), "claude-sonnet-4-5"),
            Venue::OpenAiDirect => ("openai-completions", "openai", format!("{base}/v1"), "gpt-5.1"),
        };
        serde_json::from_value(json!({
            "id": format!("{id}{id_suffix}"), "api": api, "provider": provider, "baseUrl": url,
            "reasoning": true, "input": ["text", "image"],
        }))
        .unwrap()
    }

    async fn mock(self) -> Mock {
        let mut config = Config::default();
        if !self.openrouter() {
            config.upstreams = vec!["direct".into()];
        }
        Mock::start_with(config, commands()).await
    }
}

fn workdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("serpis-cache-{}-{}", std::process::id(), rand_suffix()));
    std::fs::create_dir_all(&dir).unwrap();
    for i in 0..10 {
        std::fs::write(dir.join(format!("f{i}.txt")), format!("contents of file {i}\n").repeat(20)).unwrap();
    }
    dir
}

fn rand_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static N: AtomicU64 = AtomicU64::new(0);
    N.fetch_add(1, Ordering::Relaxed)
}

struct Chat {
    cfg: AgentConfig,
    history: Vec<Message>,
}

impl Chat {
    fn new(model: Model) -> Chat {
        let cwd = workdir();
        let tools = builtin_tools();
        let system_prompt = build_system_prompt(&tools, &cwd, &PromptOptions::default());
        let cfg = AgentConfig {
            model,
            reasoning: ThinkingLevel::Off,
            tools,
            system_prompt,
            api_key: Some("test-key".into()),
            headers: Default::default(),
            session_id: "chat-0001".into(),
            cwd,
            native_web_search: false,
            max_tokens: None,
            cache_retention: CacheRetention::Short,
            hooks: None,
            sequential_tools: false,
        };
        Chat { cfg, history: Vec::new() }
    }

    async fn send(&mut self, text: &str) {
        self.send_message(user(text), |_, _| {}).await;
    }

    /// Run one agent turn; `on` sees every event with the run's handle (to steer or abort).
    async fn send_message(&mut self, msg: Message, on: impl Fn(&AgentHandle, &AgentEvent)) {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let emit: serpis_agent::EmitFn = Arc::new(move |ev| {
            let _ = tx.send(ev);
        });
        let handle = run(self.history.clone(), vec![msg], self.cfg.clone(), emit);
        while let Some(ev) = rx.recv().await {
            on(&handle, &ev);
            match ev {
                AgentEvent::MessageEnd(m) => self.history.push(m),
                AgentEvent::AgentEnd { .. } => break,
                _ => {}
            }
        }
    }
}

fn user(text: &str) -> Message {
    Message::User { content: vec![UserContent::Text { text: text.into() }], timestamp: 1 }
}

/// A 1×1 PNG.
const PIXEL: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

fn kind(m: &Miss) -> &'static str {
    match m {
        Miss::Model => "model",
        Miss::Params => "params",
        Miss::Expired => "expired",
        Miss::Upstream => "upstream",
        Miss::Lookback => "lookback",
        Miss::PrefixChanged { .. } => "prefix",
        Miss::Unknown => "unknown",
    }
}

fn report(mock: &Mock) -> String {
    let mut s = String::from("  n  read  write  input  miss\n");
    for e in mock.log() {
        match &e.outcome {
            Ok(o) => s.push_str(&format!("{:>3} {:>5} {:>6} {:>6}  {:?}\n", e.n, o.read, o.write, o.input, o.miss)),
            Err(err) => s.push_str(&format!("{:>3} rejected: {err}\n", e.n)),
        }
    }
    s
}

/// Every miss in the log, as (request number, kind), must be exactly `expected`; no request may
/// be rejected; and the first request must cache its prefix.
fn assert_cache(venue: Venue, mock: &Mock, expected: &[(usize, &str)]) {
    let log = mock.log();
    let rejected: Vec<_> = log.iter().filter(|e| e.outcome.is_err()).collect();
    assert!(rejected.is_empty(), "{venue:?}: rejected requests\n{}", report(mock));
    let misses: Vec<(usize, &str)> = log.iter().filter_map(|e| e.outcome.as_ref().unwrap().miss.as_ref().map(|m| (e.n, kind(m)))).collect();
    assert_eq!(misses, expected, "{venue:?}: unexpected cache misses\n{}", report(mock));
    let first = log[0].outcome.as_ref().unwrap();
    if venue.explicit() {
        assert!(first.write > 0, "{venue:?}: the first request cached nothing (prefix below the minimum?)\n{}", report(mock));
    }
    if venue.openrouter() {
        assert!(
            log.iter().all(|e| e.headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("x-session-id") && v == "chat-0001")),
            "{venue:?}: a request went out without its session id"
        );
    }
}

/// The share of prompt tokens read from cache, over every request after the first.
fn hit_rate(mock: &Mock) -> f64 {
    let o = mock.outcomes();
    let (read, prompt) = o.iter().skip(1).fold((0, 0), |(r, p), x| (r + x.read, p + x.prompt()));
    read as f64 / prompt.max(1) as f64
}

#[tokio::test(flavor = "multi_thread")]
async fn a_conversation_with_tools_and_images_reads_everything_already_sent() {
    for venue in VENUES {
        let mock = venue.mock().await;
        let mut chat = Chat::new(venue.model(&mock.base, ""));
        chat.send("hello").await;
        chat.send("READ 3").await;
        let image = Message::User {
            content: vec![UserContent::Text { text: "what is this?".into() }, UserContent::Image { data: PIXEL.into(), mime_type: "image/png".into() }],
            timestamp: 1,
        };
        chat.send_message(image, |_, _| {}).await;
        chat.send("READ 1").await;
        chat.send("thanks").await;
        assert_cache(venue, &mock, &[]);
        assert!(hit_rate(&mock) > 0.8, "{venue:?}: hit rate {:.2}\n{}", hit_rate(&mock), report(&mock));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn ten_parallel_reads_keep_the_cache() {
    for venue in VENUES {
        let mock = venue.mock().await;
        let mut chat = Chat::new(venue.model(&mock.base, ""));
        chat.send("hello").await;
        chat.send("READ 10").await;
        chat.send("next").await;
        assert_cache(venue, &mock, &[]);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn steering_and_follow_ups_only_append() {
    for venue in VENUES {
        let mock = venue.mock().await;
        let mut chat = Chat::new(venue.model(&mock.base, ""));
        chat.send("hello").await;
        chat.send_message(user("READ 2"), |h, ev| {
            if let AgentEvent::ToolStart { .. } = ev {
                h.steer(user("also check the tests"));
            }
            if let AgentEvent::AgentStart = ev {
                h.follow_up(user("and summarize"));
            }
        })
        .await;
        chat.send("thanks").await;
        assert_cache(venue, &mock, &[]);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_abort_and_a_failed_response_leave_the_prefix_alone() {
    for venue in VENUES {
        let mock = venue.mock().await;
        let mut chat = Chat::new(venue.model(&mock.base, ""));
        chat.send("hello").await;
        chat.send_message(user("SLEEP"), |h, ev| {
            if let AgentEvent::ToolStart { .. } = ev {
                h.abort();
            }
        })
        .await;
        chat.send("after the abort").await;
        chat.send("FAIL").await;
        chat.send("after the failure").await;
        assert_cache(venue, &mock, &[]);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_pause_past_the_ttl_misses_only_on_short_retention() {
    for venue in VENUES {
        // Short: the pause outlives every provider's short TTL (5m explicit, 10m automatic).
        let mock = venue.mock().await;
        let mut chat = Chat::new(venue.model(&mock.base, ""));
        chat.send("hello").await;
        mock.advance(Duration::from_secs(11 * 60));
        chat.send("back").await;
        assert_cache(venue, &mock, &[(2, "expired")]);

        // Long: a 50-minute pause still hits.
        let mock = venue.mock().await;
        let mut chat = Chat::new(venue.model(&mock.base, ""));
        chat.cfg.cache_retention = CacheRetention::Long;
        chat.send("hello").await;
        mock.advance(Duration::from_secs(50 * 60));
        chat.send("back").await;
        assert_cache(venue, &mock, &[]);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn switching_thinking_or_model_is_the_stated_cause() {
    for venue in VENUES {
        let mock = venue.mock().await;
        let mut chat = Chat::new(venue.model(&mock.base, ""));
        chat.send("hello").await;
        chat.cfg.reasoning = ThinkingLevel::High;
        chat.send("think harder").await;
        chat.send("again").await;
        chat.cfg.model = venue.model(&mock.base, "-next");
        chat.send("new model").await;
        assert_cache(venue, &mock, &[(2, "params"), (4, "model")]);
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_openrouter_fallback_misses_once_then_recovers() {
    for venue in VENUES.into_iter().filter(|v| v.openrouter()) {
        let mock = venue.mock().await;
        let mut chat = Chat::new(venue.model(&mock.base, ""));
        chat.send("hello").await;
        chat.send("again").await;
        mock.with_sim(|s| s.move_upstream("chat-0001"));
        chat.send("after the fallback").await;
        chat.send("and again").await;
        assert_cache(venue, &mock, &[(3, "upstream")]);
        // Chat completions responses name their upstream, and the chat records it.
        if venue != Venue::OpenRouterClaudeMessages {
            let ups: Vec<Option<String>> = chat.history.iter().filter_map(|m| match m {
                Message::Assistant(a) => Some(a.upstream.clone()),
                _ => None,
            }).collect();
            assert!(ups.iter().all(Option::is_some), "{venue:?}: {ups:?}");
            assert_ne!(ups[1], ups[2], "{venue:?}: the fallback shows as a new upstream");
            assert_eq!(ups[2], ups[3]);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_same_conversation_is_sent_as_the_same_bytes() {
    for venue in VENUES {
        let mock = venue.mock().await;
        let mut a = Chat::new(venue.model(&mock.base, ""));
        a.send("hello").await;
        a.send("READ 2").await;
        let mut b = Chat::new(venue.model(&mock.base, ""));
        b.cfg.cwd = a.cfg.cwd.clone();
        b.cfg.system_prompt = a.cfg.system_prompt.clone();
        b.history = a.history.clone();
        b.send("next").await;
        a.send("next").await;
        let log = mock.log();
        let (x, y) = (&log[log.len() - 2].body, &log[log.len() - 1].body);
        assert_eq!(x.to_string(), y.to_string(), "{venue:?}: the same history serialized differently");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_compaction_summary_reads_the_cache_and_costs_one_miss_after() {
    use serpis_agent::compaction::{cut_point, instruction, summary_message};
    for venue in VENUES {
        let mock = venue.mock().await;
        let mut chat = Chat::new(venue.model(&mock.base, ""));
        chat.send("hello").await;
        chat.send("READ 10").await;
        chat.send("READ 3").await;
        chat.send("next").await;
        // The summary request: the messages before the cut plus the instruction, sent as the
        // agent sends everything.
        let cut = cut_point(&chat.history, 300).expect("something to compact");
        let mut msgs = chat.history[..cut].to_vec();
        msgs.push(instruction(None));
        let summary = serpis_agent::complete(&chat.cfg, msgs, CancellationToken::new()).await;
        assert!(summary.error_message.is_none(), "{venue:?}: {:?}", summary.error_message);
        let o = mock.outcomes();
        let s = o.last().unwrap();
        assert!(s.miss.is_none() && s.read > 0, "{venue:?}: the summary request should read the cache\n{}", report(&mock));
        assert!(s.write + s.input < 600, "{venue:?}: only the instruction is new\n{}", report(&mock));
        // The chat continues from the summary and the kept messages: one miss, then hits.
        let mut compacted = vec![summary_message("## Goal\nKeep testing.")];
        compacted.extend(chat.history[cut..].iter().cloned());
        chat.history = compacted;
        let n = mock.log().len();
        chat.send("after compaction").await;
        chat.send("and again").await;
        // The first request can't reuse the old conversation (a changed prefix, or a rewind the
        // simulator doesn't count); the one after it hits.
        let after: Vec<&str> = mock.log()[n..].iter().map(|e| e.outcome.as_ref().unwrap().miss.as_ref().map(kind).unwrap_or("none")).collect();
        assert!(matches!(after[..], ["prefix" | "none", "none"]), "{venue:?}: {after:?}\n{}", report(&mock));
    }
}
