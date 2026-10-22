//! What a chat's prompt cache is doing, from what each request was sent with and what the
//! provider reported back. A chat never changes bytes it already sent (the cache test suite in
//! serpis-testkit holds it to that), so a miss has one of a few known causes: the user changed
//! the model, the thinking level, the tools or the system prompt; the entry expired; or OpenRouter
//! moved the chat to another upstream. Anything else is reported as unexplained.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use serpis_ai::{CacheRetention, Model, Usage};

/// What a request was sent with, as far as the cache cares.
#[derive(Debug, Clone, PartialEq)]
pub struct Sent {
    /// `provider/model-id`.
    pub model: String,
    pub thinking: String,
    /// The tools' declarations (name, description, parameters), so a changed description
    /// counts as a tools change.
    pub tools: Vec<String>,
    /// A hash of the system prompt.
    pub prompt: u64,
    pub retention: CacheRetention,
    /// Compactions so far: the first request after one sends a new conversation.
    pub compactions: u64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cause {
    Compacted,
    Model,
    Thinking,
    Tools,
    Prompt,
    Expired { idle: Duration, ttl: Duration },
    Upstream { from: String, to: String },
    /// An automatic, best-effort cache didn't reuse a prefix it could have.
    Provider,
    Unexplained,
}

impl Cause {
    /// One word, for hooks and scripts.
    pub fn word(&self) -> &'static str {
        match self {
            Cause::Compacted => "compacted",
            Cause::Model => "model",
            Cause::Thinking => "thinking",
            Cause::Tools => "tools",
            Cause::Prompt => "prompt",
            Cause::Expired { .. } => "expired",
            Cause::Upstream { .. } => "upstream",
            Cause::Provider => "provider",
            Cause::Unexplained => "unexplained",
        }
    }

    pub fn describe(&self) -> String {
        match self {
            Cause::Compacted => "the conversation was compacted".into(),
            Cause::Model => "the model changed".into(),
            Cause::Thinking => "the thinking level changed".into(),
            Cause::Tools => "the tools changed".into(),
            Cause::Prompt => "the system prompt was refreshed".into(),
            Cause::Expired { idle, ttl } => format!("idle {} with a {} cache", fmt_duration(*idle), fmt_duration(*ttl)),
            Cause::Upstream { from, to } => format!("OpenRouter moved the chat from {from} to {to}"),
            Cause::Provider => "the provider didn't reuse its cache this time (its caching is best-effort)".into(),
            Cause::Unexplained => "unexplained (bytes already sent may have changed: please report it)".into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Miss {
    pub cause: Cause,
    /// Prompt tokens that were not read from the cache.
    pub tokens: u64,
    /// What reading them from the cache would have saved, in dollars.
    pub cost: f64,
}

impl Miss {
    pub fn notice(&self) -> String {
        format!("cache miss: {}; rewrote {} tokens ({})", self.cause.describe(), fmt_tokens(self.tokens), fmt_cost(self.cost))
    }
}

/// How one response used the cache.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hit {
    /// Nothing to read yet (the chat's first request, or nothing was cached before).
    New,
    /// Read this share of the prompt from the cache.
    Hit(f64),
    Miss,
}

impl From<Hit> for serpis_proto::CacheHit {
    fn from(h: Hit) -> Self {
        match h {
            Hit::New => serpis_proto::CacheHit::New,
            Hit::Hit(r) => serpis_proto::CacheHit::Hit(r),
            Hit::Miss => serpis_proto::CacheHit::Miss,
        }
    }
}

impl From<serpis_proto::CacheHit> for Hit {
    fn from(h: serpis_proto::CacheHit) -> Self {
        match h {
            serpis_proto::CacheHit::New => Hit::New,
            serpis_proto::CacheHit::Hit(r) => Hit::Hit(r),
            serpis_proto::CacheHit::Miss => Hit::Miss,
        }
    }
}

impl Hit {
    pub fn label(self) -> String {
        match self {
            Hit::New => "cache new".into(),
            Hit::Hit(s) => format!("cache {:.0}%", s * 100.0),
            Hit::Miss => "cache miss".into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    Warm(Duration),
    Cold,
}

#[derive(Debug, Clone)]
struct Last {
    sent: Sent,
    /// Tokens the request left in the cache (read + written).
    cached: u64,
    prompt_tokens: u64,
    ended: Instant,
    upstream: Option<String>,
    model: Model,
}

#[derive(Debug, Clone, Default)]
pub struct CacheWatch {
    last: Option<Last>,
    /// Recent misses, newest last.
    pub misses: VecDeque<Miss>,
    read: u64,
    prompt: u64,
}

const KEEP_MISSES: usize = 20;
/// Tokens a hit may fall short of the previous request's cache (block rounding).
const MISS_SLACK: u64 = 256;

/// How long an entry written with `r` lives.
pub fn ttl(r: CacheRetention) -> Duration {
    match r {
        CacheRetention::Long => Duration::from_secs(60 * 60),
        CacheRetention::Short => Duration::from_secs(5 * 60),
        CacheRetention::None => Duration::ZERO,
    }
}

/// Dollars per token to write `tokens` into the cache instead of reading them from it.
fn rewrite_cost(model: &Model, retention: CacheRetention, tokens: u64) -> f64 {
    let c = &model.cost;
    let write = if c.cache_write <= 0.0 {
        c.input
    } else if retention == CacheRetention::Long {
        c.input * 2.0
    } else {
        c.cache_write
    };
    (write - c.cache_read).max(0.0) * tokens as f64 / 1e6
}

impl CacheWatch {
    /// Start from a resumed conversation's usage (its cache state is unknown).
    pub fn from_usages<'a>(usages: impl Iterator<Item = &'a Usage>) -> Self {
        let mut w = CacheWatch::default();
        for u in usages {
            w.read += u.cache_read;
            w.prompt += u.input + u.cache_read + u.cache_write;
        }
        w
    }

    /// Account for a response to a request sent with `sent` at `started` that ended at `ended`.
    pub fn observe(&mut self, sent: Sent, model: &Model, usage: &Usage, upstream: Option<String>, started: Instant, ended: Instant) -> (Hit, Option<Miss>) {
        let prompt_tokens = usage.input + usage.cache_read + usage.cache_write;
        if prompt_tokens == 0 {
            // A failed request: the provider reported nothing, and the cache is as it was.
            return (Hit::New, None);
        }
        self.read += usage.cache_read;
        self.prompt += prompt_tokens;
        // A hit reads all the previous request left in the cache (OpenAI-style caches round down
        // to 128-token blocks); reading less than that is a miss, even when a prefix such as the
        // tools still hits.
        let expected = self.last.as_ref().map(|l| l.cached).unwrap_or(0);
        let short = expected.saturating_sub(usage.cache_read);
        let miss = (expected > 0 && short > MISS_SLACK.max(expected / 50)).then(|| {
            let last = self.last.as_ref().unwrap();
            let idle = started.saturating_duration_since(last.ended);
            let cause = if last.sent.compactions != sent.compactions {
                Cause::Compacted
            } else if last.sent.model != sent.model {
                Cause::Model
            } else if last.sent.thinking != sent.thinking {
                Cause::Thinking
            } else if last.sent.tools != sent.tools {
                Cause::Tools
            } else if last.sent.prompt != sent.prompt {
                Cause::Prompt
            } else if idle > ttl(last.sent.retention) {
                Cause::Expired { idle, ttl: ttl(last.sent.retention) }
            } else {
                match (&last.upstream, &upstream) {
                    (Some(from), Some(to)) if from != to => Cause::Upstream { from: from.clone(), to: to.clone() },
                    // Only an explicit cache serves an unchanged prefix reliably enough to blame
                    // serpis; serpis' own requests are held to that by the cache test suite.
                    _ if !model.explicit_prompt_cache() => Cause::Provider,
                    _ => Cause::Unexplained,
                }
            };
            let tokens = prompt_tokens - usage.cache_read;
            Miss { cause, tokens, cost: rewrite_cost(model, sent.retention, tokens) }
        });
        let hit = if miss.is_some() {
            Hit::Miss
        } else if usage.cache_read == 0 {
            Hit::New
        } else {
            Hit::Hit(usage.cache_read as f64 / prompt_tokens as f64)
        };
        if let Some(m) = &miss {
            self.misses.push_back(m.clone());
            if self.misses.len() > KEEP_MISSES {
                self.misses.pop_front();
            }
        }
        // OpenAI-style caches report no writes, but keep the prompt they just saw.
        let cached = if usage.cache_write > 0 { usage.cache_read + usage.cache_write } else if usage.cache_read > 0 { prompt_tokens } else { 0 };
        self.last = Some(Last { sent, cached, prompt_tokens, ended, upstream, model: model.clone() });
        (hit, miss)
    }

    /// Share of all prompt tokens so far that were read from the cache.
    pub fn hit_rate(&self) -> Option<f64> {
        (self.prompt > 0).then(|| self.read as f64 / self.prompt as f64)
    }

    /// Whether the chat's cache entry is still alive at `now`: None before anything was cached.
    pub fn state(&self, now: Instant) -> Option<State> {
        let last = self.last.as_ref().filter(|l| l.cached > 0)?;
        let left = ttl(last.sent.retention).saturating_sub(now.saturating_duration_since(last.ended));
        Some(if left.is_zero() { State::Cold } else { State::Warm(left) })
    }

    /// What throwing the warm cache away would cost on the next message: (tokens, dollars).
    pub fn invalidation(&self, now: Instant) -> Option<(u64, f64)> {
        let last = self.last.as_ref()?;
        matches!(self.state(now), Some(State::Warm(_))).then(|| (last.prompt_tokens, rewrite_cost(&last.model, last.sent.retention, last.prompt_tokens)))
    }

    /// When the cache expired and how big the prompt is, if it did (sending now rewrites it).
    pub fn expired(&self, now: Instant) -> Option<(Duration, u64, f64)> {
        let last = self.last.as_ref()?;
        (self.state(now) == Some(State::Cold)).then(|| {
            let ago = now.saturating_duration_since(last.ended).saturating_sub(ttl(last.sent.retention));
            (ago, last.prompt_tokens, rewrite_cost(&last.model, last.sent.retention, last.prompt_tokens))
        })
    }

    /// Carry on from a resumed session: its last response (`usage`, `upstream`) ended `age` ago and
    /// was sent with `sent`.
    pub fn resume(&mut self, sent: Sent, model: &Model, usage: &Usage, upstream: Option<String>, age: Duration) {
        let prompt_tokens = usage.input + usage.cache_read + usage.cache_write;
        let cached = if usage.cache_write > 0 { usage.cache_read + usage.cache_write } else if usage.cache_read > 0 { prompt_tokens } else { 0 };
        let now = Instant::now();
        let ended = now.checked_sub(age).unwrap_or(now);
        self.last = Some(Last { sent, cached, prompt_tokens, ended, upstream, model: model.clone() });
    }

    /// Pretend the last response ended `d` earlier (tests: `serpis.debug.cache_age`).
    pub fn age(&mut self, d: Duration) {
        if let Some(l) = &mut self.last {
            l.ended = l.ended.checked_sub(d).unwrap_or(l.ended);
        }
    }
}

/// "~$0.32", or "<$0.01" for less than a cent.
pub fn fmt_cost(c: f64) -> String {
    if c < 0.01 { "<$0.01".into() } else { format!("~${c:.2}") }
}

/// Whether a resumed chat keeps the system prompt it was sent with (chat-resume-system-prompt):
/// "warm" while its cache can still be alive (`age` < `ttl`), "always", or "never" (rebuild it).
/// Once the cache has expired, keeping an outdated prompt saves nothing.
pub fn keep_prompt_on_resume(mode: &str, age: Duration, ttl: Duration) -> bool {
    match mode {
        "always" => true,
        "never" => false,
        _ => age < ttl,
    }
}

/// A short hash of the tool declarations a request sends.
pub fn declaration_key(t: &serpis_ai::Tool) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    serde_json::json!({"d": t.description, "p": t.parameters}).to_string().hash(&mut h);
    format!("{}#{:x}", t.name, h.finish())
}

pub fn fmt_duration(d: Duration) -> String {
    let s = d.as_secs();
    if s >= 3600 {
        format!("{}h {:02}m", s / 3600, (s % 3600) / 60)
    } else if s >= 60 {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// `950`, `12.3k`, `1.2M`.
pub fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model() -> Model {
        serde_json::from_value(serde_json::json!({"id": "m", "api": "anthropic-messages", "provider": "p",
            "cost": {"input": 3.0, "output": 15.0, "cacheRead": 0.3, "cacheWrite": 3.75}})).unwrap()
    }

    fn sent() -> Sent {
        Sent { model: "p/m".into(), thinking: "off".into(), tools: vec!["read".into()], prompt: 1, retention: CacheRetention::Long, compactions: 0 }
    }

    fn usage(input: u64, read: u64, write: u64) -> Usage {
        Usage { input, cache_read: read, cache_write: write, ..Default::default() }
    }

    struct T {
        w: CacheWatch,
        now: Instant,
    }

    impl T {
        fn new() -> T {
            T { w: CacheWatch::default(), now: Instant::now() }
        }
        fn send(&mut self, s: Sent, u: Usage, upstream: Option<&str>) -> (Hit, Option<Miss>) {
            let start = self.now;
            self.now += Duration::from_secs(2);
            self.w.observe(s, &model(), &u, upstream.map(str::to_string), start, self.now)
        }
    }

    #[test]
    fn a_growing_chat_hits_and_reports_its_share() {
        let mut t = T::new();
        assert_eq!(t.send(sent(), usage(0, 0, 2000), None), (Hit::New, None));
        let (hit, miss) = t.send(sent(), usage(0, 2000, 100), None);
        assert_eq!(miss, None);
        assert!(matches!(hit, Hit::Hit(s) if (s - 2000.0 / 2100.0).abs() < 1e-9));
        assert!((t.w.hit_rate().unwrap() - 2000.0 / 4100.0).abs() < 1e-9);
        assert!(matches!(t.w.state(t.now), Some(State::Warm(_))));
    }

    #[test]
    fn each_change_is_its_own_cause() {
        let cases: Vec<(Box<dyn Fn(&mut Sent)>, &str)> = vec![
            (Box::new(|s| s.model = "p/other".into()), "model"),
            (Box::new(|s| s.thinking = "high".into()), "thinking"),
            (Box::new(|s| s.tools.push("bash".into())), "tools"),
            (Box::new(|s| s.prompt = 2), "prompt"),
            (Box::new(|s| s.compactions = 1), "compacted"),
        ];
        for (change, word) in cases {
            let mut t = T::new();
            t.send(sent(), usage(0, 0, 2000), None);
            let mut s = sent();
            change(&mut s);
            let (hit, miss) = t.send(s, usage(0, 0, 2100), None);
            assert_eq!(hit, Hit::Miss);
            let miss = miss.unwrap();
            assert_eq!(miss.cause.word(), word);
            assert_eq!(miss.tokens, 2100);
            // 2100 tokens at 2x input (1h writes) instead of the read price.
            assert!((miss.cost - 2100.0 * (6.0 - 0.3) / 1e6).abs() < 1e-12);
        }
    }

    #[test]
    fn a_pause_past_the_ttl_is_expired_and_a_fallback_is_upstream() {
        let mut t = T::new();
        let mut s = sent();
        s.retention = CacheRetention::Short;
        t.send(s.clone(), usage(0, 0, 2000), Some("Anthropic"));
        t.now += Duration::from_secs(6 * 60);
        assert_eq!(t.w.state(t.now), Some(State::Cold));
        assert!(t.w.expired(t.now).is_some());
        let (_, miss) = t.send(s.clone(), usage(0, 0, 2100), Some("Anthropic"));
        assert!(matches!(miss.unwrap().cause, Cause::Expired { .. }));
        let (_, miss) = t.send(s, usage(0, 0, 2200), Some("Amazon Bedrock"));
        assert_eq!(miss.unwrap().cause, Cause::Upstream { from: "Anthropic".into(), to: "Amazon Bedrock".into() });
    }

    #[test]
    fn a_miss_that_still_reads_the_tools_is_a_miss() {
        let mut t = T::new();
        t.send(sent(), usage(0, 0, 2600), None);
        let mut s = sent();
        s.prompt = 2;
        // The system prompt changed: only the tools before it (1,300 tokens) are read.
        assert_eq!(t.send(s, usage(0, 1300, 1400), None).1.unwrap().cause, Cause::Prompt);
        // OpenAI-style rounding (up to 127 tokens short) is not a miss.
        let mut t = T::new();
        t.send(sent(), usage(1500, 0, 0), None);
        t.send(sent(), usage(100, 1408, 0), None);
        assert_eq!(t.send(sent(), usage(200, 1408, 0), None).1, None);
    }

    #[test]
    fn an_unexplained_miss_says_so() {
        let mut t = T::new();
        t.send(sent(), usage(0, 0, 2000), None);
        assert_eq!(t.send(sent(), usage(0, 300, 1800), None).1.unwrap().cause, Cause::Unexplained);
    }

    #[test]
    fn failed_requests_and_automatic_caches_are_not_misses() {
        let mut t = T::new();
        t.send(sent(), usage(0, 0, 2000), None);
        assert_eq!(t.send(sent(), usage(0, 0, 0), None), (Hit::New, None), "no usage: the cache is as it was");
        assert_eq!(t.send(sent(), usage(0, 2000, 50), None).1, None);
        // OpenAI-style: no writes reported; the second request reads the first one's prompt. An
        // explicit cache that misses with no other cause is unexplained; an automatic one is the
        // provider's best effort.
        let mut t = T::new();
        t.send(sent(), usage(1500, 0, 0), None);
        assert_eq!(t.send(sent(), usage(100, 1500, 0), None).1, None);
        assert_eq!(t.send(sent(), usage(1700, 0, 0), None).1.unwrap().cause, Cause::Unexplained);
        let auto: Model = serde_json::from_value(serde_json::json!({"id": "deepseek/deepseek-chat", "api": "openai-completions", "provider": "openrouter"})).unwrap();
        let mut w = CacheWatch::default();
        let now = Instant::now();
        w.observe(sent(), &auto, &usage(1500, 0, 0), None, now, now);
        w.observe(sent(), &auto, &usage(100, 1500, 0), None, now, now);
        assert_eq!(w.observe(sent(), &auto, &usage(1700, 0, 0), None, now, now).1.unwrap().cause, Cause::Provider);
    }

    #[test]
    fn a_resumed_chat_keeps_its_prompt_only_while_the_cache_can_be_warm() {
        let h = Duration::from_secs(3600);
        assert!(keep_prompt_on_resume("warm", Duration::from_secs(600), h));
        assert!(!keep_prompt_on_resume("warm", Duration::from_secs(2 * 3600), h), "an expired cache: rebuild");
        assert!(keep_prompt_on_resume("always", Duration::from_secs(30 * 86400), h));
        assert!(!keep_prompt_on_resume("never", Duration::ZERO, h));
    }

    #[test]
    fn a_resumed_watch_is_warm_and_judges_the_first_request() {
        let mut t = T::new();
        t.w.resume(sent(), &model(), &usage(0, 9000, 1000), Some("Anthropic".into()), Duration::from_secs(600));
        assert!(matches!(t.w.state(t.now), Some(State::Warm(left)) if left.as_secs().abs_diff(3000) < 5));
        let mut s = sent();
        s.prompt = 2;
        assert_eq!(t.send(s, usage(0, 1200, 9000), Some("Anthropic")).1.unwrap().cause, Cause::Prompt);
    }

    #[test]
    fn invalidating_a_warm_cache_has_a_price_and_a_cold_one_does_not() {
        let mut t = T::new();
        t.send(sent(), usage(0, 0, 10_000), None);
        let (tokens, cost) = t.w.invalidation(t.now).unwrap();
        assert_eq!(tokens, 10_000);
        assert!((cost - 10_000.0 * 5.7 / 1e6).abs() < 1e-12);
        t.w.age(Duration::from_secs(2 * 3600));
        assert_eq!(t.w.invalidation(t.now), None);
    }
}
