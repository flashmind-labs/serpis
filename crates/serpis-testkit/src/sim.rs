//! A model of provider-side prompt caching, strict enough to catch any change to bytes already
//! sent. It reads raw request bodies the way a provider would and never uses serpis' own code.
//!
//! Two kinds of cache:
//! - *Explicit* (Anthropic messages, and OpenRouter chat completions for `anthropic/*` and
//!   `google/*` models): entries are written only at `cache_control` marks (at most 4, longer TTLs
//!   first). A request reads the longest live entry found within `lookback` blocks before one of
//!   its marks. A read resets that entry's TTL. Changing thinking or tool-choice parameters
//!   invalidates the messages but not tools or system.
//! - *Automatic* (everything else): every block boundary of every request is cached, and a request
//!   reads the longest live prefix sent before under the same routing key.
//!
//! Blocks are compared as exact JSON (marks removed): a string and a one-part array holding the
//! same text are different, as are reordered tools or a changed byte anywhere.
//!
//! Each routing key (`x-session-id`, else `prompt_cache_key`) sticks to one upstream picked from
//! its hash, as OpenRouter's sticky routing does; requests without one rotate between upstreams, and each
//! upstream has its own cache. [`Simulator::move_upstream`] stands in for a fallback.

use std::collections::HashMap;
use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::time::Duration;

use serde_json::{Value, json};

/// Which wire format a body uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wire {
    AnthropicMessages,
    ChatCompletions,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub lookback: usize,
    pub max_marks: usize,
    /// Smallest prefix (in estimated tokens) an explicit mark can write.
    pub min_tokens: u64,
    pub short_ttl: Duration,
    pub long_ttl: Duration,
    pub auto_ttl: Duration,
    pub auto_long_ttl: Duration,
    pub auto_min_tokens: u64,
    pub upstreams: Vec<String>,
    /// Automatic caches keep only a session's latest whole prompt, reusable only by a request
    /// that extends all of it: one that branches off an earlier prefix misses (DeepInfra's
    /// DeepSeek in the live check read 1 token of a 7,800-token shared prefix).
    pub latest_prompt_only: bool,
}

impl Default for Config {
    fn default() -> Self {
        Config {
            lookback: 20,
            max_marks: 4,
            min_tokens: 1024,
            short_ttl: Duration::from_secs(5 * 60),
            long_ttl: Duration::from_secs(60 * 60),
            auto_ttl: Duration::from_secs(10 * 60),
            auto_long_ttl: Duration::from_secs(24 * 60 * 60),
            auto_min_tokens: 1024,
            upstreams: vec!["upstream-a".into(), "upstream-b".into()],
            latest_prompt_only: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    Tools,
    System,
    Messages,
}

impl Segment {
    pub fn name(self) -> &'static str {
        match self {
            Segment::Tools => "tools",
            Segment::System => "system",
            Segment::Messages => "messages",
        }
    }
}

/// Why a request read less than the previous request in its session had cached.
#[derive(Debug, Clone, PartialEq)]
pub enum Miss {
    /// A different model: nothing carries over.
    Model,
    /// Thinking or tool-choice parameters changed, invalidating the messages.
    Params,
    /// The bytes were the same but the entry had expired.
    Expired,
    /// The session moved to another upstream, which has its own cache.
    Upstream,
    /// The cached entry exists but no mark was within the lookback window of it.
    Lookback,
    /// A block already sent changed. `index` counts within the segment.
    PrefixChanged { segment: &'static str, index: usize, before: String, after: String },
    /// None of the above: the simulator or the test is wrong.
    Unknown,
}

impl Miss {
    /// True for misses a harness can't avoid once the user asks for them (switching model or
    /// thinking) or that time causes. A changed prefix, a lookback miss or an unknown one is a bug.
    pub fn is_expected_kind(&self) -> bool {
        matches!(self, Miss::Model | Miss::Params | Miss::Expired | Miss::Upstream)
    }
}

/// What one request cost, in estimated tokens.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub read: u64,
    pub write: u64,
    /// The part of `write` stored with a 1h TTL.
    pub write_1h: u64,
    /// Uncached input.
    pub input: u64,
    pub upstream: String,
    pub explicit: bool,
    pub marks: usize,
    pub miss: Option<Miss>,
}

impl Outcome {
    pub fn prompt(&self) -> u64 {
        self.read + self.write + self.input
    }
}

#[derive(Debug, Clone)]
struct Block {
    segment: Segment,
    index: usize,
    json: String,
    tokens: u64,
    mark: Option<Duration>,
}

struct Parsed {
    model: String,
    params: String,
    blocks: Vec<Block>,
    explicit: bool,
    auto_long: bool,
}

#[derive(Clone)]
struct Entry {
    expires: u64,
    ttl: u64,
}

struct Prev {
    model: String,
    params: String,
    blocks: Vec<Block>,
    hashes: Vec<u64>,
    upstream: String,
    /// Index of the last block covered by a cache entry after the request, if any.
    cached_to: Option<usize>,
}

/// The simulated provider. Time is virtual: it only moves with [`Simulator::advance`].
pub struct Simulator {
    pub config: Config,
    now_ms: u64,
    store: HashMap<(String, u64), Entry>,
    sticky: HashMap<String, String>,
    rotate: usize,
    sessions: HashMap<String, Prev>,
    /// The automatic-cache entry each session wrote last (for `latest_prompt_only`).
    auto_paths: HashMap<String, Vec<(String, u64)>>,
}

fn h(seed: u64, s: &str) -> u64 {
    let mut x = DefaultHasher::new();
    seed.hash(&mut x);
    s.hash(&mut x);
    x.finish()
}

fn strip_marks(v: &Value) -> Value {
    match v {
        Value::Object(o) => Value::Object(o.iter().filter(|(k, _)| *k != "cache_control").map(|(k, x)| (k.clone(), strip_marks(x))).collect()),
        Value::Array(a) => Value::Array(a.iter().map(strip_marks).collect()),
        x => x.clone(),
    }
}

fn short(s: &str) -> String {
    if s.chars().count() > 300 { s.chars().take(300).collect::<String>() + "…" } else { s.to_string() }
}

impl Simulator {
    pub fn new(config: Config) -> Self {
        Simulator { config, now_ms: 0, store: HashMap::new(), sticky: HashMap::new(), rotate: 0, sessions: HashMap::new(), auto_paths: HashMap::new() }
    }

    pub fn advance(&mut self, d: Duration) {
        self.now_ms += d.as_millis() as u64;
    }

    /// Send every later request under `key` to another upstream (a provider fallback).
    pub fn move_upstream(&mut self, key: &str) {
        let cur = self.sticky.get(key).cloned().unwrap_or_default();
        let next = self.config.upstreams.iter().find(|u| **u != cur).cloned().unwrap_or(cur);
        self.sticky.insert(key.to_string(), next);
    }

    fn ttl_of(&self, cc: &Value) -> Option<Duration> {
        match cc.get("ttl").and_then(Value::as_str) {
            Some("1h") => Some(self.config.long_ttl),
            Some("5m") | None => Some(self.config.short_ttl),
            Some(_) => None,
        }
    }

    fn block(&self, segment: Segment, index: usize, v: &Value, mark: Option<&Value>) -> Block {
        let json = serde_json::to_string(&strip_marks(v)).unwrap();
        Block { segment, index, tokens: (json.len() as u64).div_ceil(4), json, mark: mark.and_then(|m| self.ttl_of(m)) }
    }

    fn parse(&self, wire: Wire, body: &Value) -> Parsed {
        let model = body["model"].as_str().unwrap_or_default().to_string();
        let mut blocks = Vec::new();
        let mut push = |b: Block| blocks.push(b);
        let pick = |keys: &[&str]| -> String {
            let o: serde_json::Map<String, Value> = keys.iter().filter_map(|k| body.get(*k).map(|v| (k.to_string(), v.clone()))).collect();
            Value::Object(o).to_string()
        };
        for (i, t) in body["tools"].as_array().into_iter().flatten().enumerate() {
            push(self.block(Segment::Tools, i, t, t.get("cache_control")));
        }
        match wire {
            Wire::AnthropicMessages => {
                match &body["system"] {
                    Value::String(s) => push(self.block(Segment::System, 0, &json!(s), None)),
                    Value::Array(a) => a.iter().enumerate().for_each(|(i, b)| push(self.block(Segment::System, i, b, b.get("cache_control")))),
                    _ => {}
                }
                let mut i = 0;
                for m in body["messages"].as_array().into_iter().flatten() {
                    let role = &m["role"];
                    match &m["content"] {
                        Value::Array(parts) => {
                            for (k, p) in parts.iter().enumerate() {
                                push(self.block(Segment::Messages, i, &json!({"role": role, "start": k == 0, "block": p}), p.get("cache_control")));
                                i += 1;
                            }
                        }
                        c => {
                            push(self.block(Segment::Messages, i, &json!({"role": role, "start": true, "block": c}), None));
                            i += 1;
                        }
                    }
                }
                Parsed { params: pick(&["thinking", "output_config", "tool_choice"]), model, blocks, explicit: true, auto_long: false }
            }
            Wire::ChatCompletions => {
                let explicit = model.starts_with("anthropic/") || model.starts_with("google/");
                let (mut si, mut mi) = (0, 0);
                for m in body["messages"].as_array().into_iter().flatten() {
                    let role = m["role"].as_str().unwrap_or_default();
                    let system = role == "system" || role == "developer";
                    let mut parts: Vec<(Value, Option<&Value>)> = Vec::new();
                    match &m["content"] {
                        Value::Array(a) => a.iter().for_each(|p| parts.push((p.clone(), p.get("cache_control")))),
                        Value::Null => {}
                        c => parts.push((c.clone(), m.get("cache_control"))),
                    }
                    for key in ["reasoning_details", "reasoning", "reasoning_content"] {
                        if let Some(r) = m.get(key).filter(|r| !r.is_null()) {
                            parts.push((json!({key: r}), None));
                        }
                    }
                    for tc in m["tool_calls"].as_array().into_iter().flatten() {
                        parts.push((tc.clone(), None));
                    }
                    for (k, (p, mark)) in parts.iter().enumerate() {
                        if system {
                            push(self.block(Segment::System, si, p, *mark));
                            si += 1;
                        } else {
                            let v = json!({"role": role, "start": k == 0, "tool_call_id": m.get("tool_call_id"), "block": p});
                            push(self.block(Segment::Messages, mi, &v, *mark));
                            mi += 1;
                        }
                    }
                }
                let auto_long = body["prompt_cache_retention"] == "24h";
                Parsed { params: pick(&["reasoning", "reasoning_effort", "thinking", "tool_choice"]), model, blocks, explicit, auto_long }
            }
        }
    }

    fn hashes(p: &Parsed) -> Vec<u64> {
        let mut acc = h(0, &p.model);
        let mut salted = false;
        p.blocks
            .iter()
            .map(|b| {
                if b.segment == Segment::Messages && !salted {
                    acc = h(acc, &p.params);
                    salted = true;
                }
                acc = h(acc, &b.json);
                acc
            })
            .collect()
    }

    fn live(&self, upstream: &str, hash: u64) -> bool {
        self.store.get(&(upstream.to_string(), hash)).is_some_and(|e| e.expires > self.now_ms)
    }

    /// Account for one request. `headers` are (name, value) pairs; names compare case-insensitively.
    /// An error is what the provider would reject with HTTP 400.
    pub fn request(&mut self, wire: Wire, headers: &[(String, String)], body: &Value) -> Result<Outcome, String> {
        let p = self.parse(wire, body);
        let header = |n: &str| headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(n)).map(|(_, v)| v.clone());
        let key = header("x-session-id").or_else(|| body["prompt_cache_key"].as_str().map(str::to_string));
        let upstream = match &key {
            Some(k) => {
                let n = self.config.upstreams.len() as u64;
                let assigned = self.config.upstreams[(h(0, k) % n) as usize].clone();
                self.sticky.entry(k.clone()).or_insert(assigned).clone()
            }
            None => {
                self.rotate += 1;
                self.config.upstreams[self.rotate % self.config.upstreams.len()].clone()
            }
        };
        let hashes = Self::hashes(&p);
        let cum: Vec<u64> = p.blocks.iter().scan(0, |a, b| {
            *a += b.tokens;
            Some(*a)
        }).collect();
        let total = cum.last().copied().unwrap_or(0);
        let marks: Vec<usize> = (0..p.blocks.len()).filter(|i| p.blocks[*i].mark.is_some()).collect();
        let session = key.clone().unwrap_or_default();
        // Whether the previous request's cached entry had expired, looked up before this request
        // writes anything over it.
        let prev_expired = self.sessions.get(&session).and_then(|prev| prev.cached_to.map(|e| (prev.upstream.clone(), prev.hashes[e]))).is_some_and(|k| {
            self.store.get(&k).is_some_and(|e| e.expires <= self.now_ms)
        });

        let (read_to, cached_to, write, write_1h) = if p.explicit {
            if marks.len() > self.config.max_marks {
                return Err(format!("A maximum of {} blocks with cache_control may be provided. Found {}.", self.config.max_marks, marks.len()));
            }
            if marks.windows(2).any(|w| p.blocks[w[0]].mark < p.blocks[w[1]].mark) {
                return Err("a ttl='1h' cache_control block must not come after a ttl='5m' block".into());
            }
            let mut read_to: Option<usize> = None;
            for &m in &marks {
                for q in m.saturating_sub(self.config.lookback - 1)..=m {
                    if self.live(&upstream, hashes[q]) && read_to.is_none_or(|r| q > r) {
                        read_to = Some(q);
                    }
                }
            }
            if let Some(q) = read_to {
                let e = self.store.get_mut(&(upstream.clone(), hashes[q])).unwrap();
                e.expires = self.now_ms + e.ttl;
            }
            let start = read_to.map(|q| cum[q]).unwrap_or(0);
            let (mut end, mut end_1h, mut cached_to) = (start, start, read_to);
            for &m in &marks {
                if read_to.is_some_and(|q| m <= q) || cum[m] < self.config.min_tokens {
                    continue;
                }
                let ttl = p.blocks[m].mark.unwrap();
                self.store.insert((upstream.clone(), hashes[m]), Entry { expires: self.now_ms + ttl.as_millis() as u64, ttl: ttl.as_millis() as u64 });
                end = cum[m];
                if ttl == self.config.long_ttl {
                    end_1h = cum[m];
                }
                cached_to = Some(m);
            }
            (read_to, cached_to, end - start, end_1h - start)
        } else {
            let read_to = (0..hashes.len()).rev().find(|i| cum[*i] >= self.config.auto_min_tokens && self.live(&upstream, hashes[*i]));
            let ttl = if p.auto_long { self.config.auto_long_ttl } else { self.config.auto_ttl }.as_millis() as u64;
            if self.config.latest_prompt_only {
                for k in self.auto_paths.remove(&session).unwrap_or_default() {
                    self.store.remove(&k);
                }
                if let Some(last) = hashes.last() {
                    self.auto_paths.insert(session.clone(), vec![(upstream.clone(), *last)]);
                    self.store.insert((upstream.clone(), *last), Entry { expires: self.now_ms + ttl, ttl });
                }
            } else {
                for x in &hashes {
                    self.store.insert((upstream.clone(), *x), Entry { expires: self.now_ms + ttl, ttl });
                }
            }
            let cached_to = (0..hashes.len()).rev().find(|i| cum[*i] >= self.config.auto_min_tokens);
            (read_to, cached_to, 0, 0)
        };
        let read = read_to.map(|q| cum[q]).unwrap_or(0);

        let miss = self.sessions.get(&session).and_then(|prev| {
            let mut expected = prev.cached_to?;
            if read_to.is_some_and(|q| q >= expected) {
                return None;
            }
            if prev.model != p.model {
                return Some(Miss::Model);
            }
            let lcp = prev.blocks.iter().zip(&p.blocks).take_while(|(a, b)| a.segment == b.segment && a.json == b.json).count();
            // A request no longer than the previous one that leaves it early is a side request off
            // the shared prefix (a summary) or a rewind: it can read up to the last entry before
            // the point where it leaves. A longer one that differs early changed bytes already sent.
            if lcp <= expected && p.blocks.len() <= prev.blocks.len() && prev.params == p.params {
                match (0..lcp.min(prev.hashes.len())).rev().find(|&i| self.store.contains_key(&(upstream.clone(), prev.hashes[i]))) {
                    Some(i) if read_to.is_some_and(|q| q >= i) => return None,
                    Some(i) => expected = i,
                    None => return None,
                }
            }
            if lcp <= expected {
                let first_message = p.blocks.iter().position(|b| b.segment == Segment::Messages).unwrap_or(p.blocks.len());
                if prev.params != p.params && lcp >= first_message.min(prev.blocks.len()) {
                    return Some(Miss::Params);
                }
                let (before, after) = (prev.blocks.get(lcp), p.blocks.get(lcp));
                let at = before.or(after).unwrap();
                return Some(Miss::PrefixChanged {
                    segment: at.segment.name(),
                    index: at.index,
                    before: before.map(|b| short(&b.json)).unwrap_or_else(|| "(missing)".into()),
                    after: after.map(|b| short(&b.json)).unwrap_or_else(|| "(missing)".into()),
                });
            }
            if prev.upstream != upstream {
                return Some(Miss::Upstream);
            }
            if prev.params != p.params {
                return Some(Miss::Params);
            }
            if prev_expired {
                return Some(Miss::Expired);
            }
            if p.explicit && !marks.iter().any(|m| *m >= expected && *m < expected + self.config.lookback) {
                return Some(Miss::Lookback);
            }
            Some(Miss::Unknown)
        });
        self.sessions.insert(session, Prev { model: p.model.clone(), params: p.params.clone(), blocks: p.blocks, hashes, upstream: upstream.clone(), cached_to });
        Ok(Outcome { read, write, write_1h, input: total - read - write, upstream, explicit: p.explicit, marks: marks.len(), miss })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CC: &str = r#"{"type":"ephemeral"}"#;

    fn cc() -> Value {
        serde_json::from_str(CC).unwrap()
    }

    /// An Anthropic body: a system prompt big enough to cache, then `n` user/assistant text
    /// blocks, the last marked (plus any extra marks at message indexes in `also`).
    fn body(system: &str, n: usize, also: &[usize]) -> Value {
        let msgs: Vec<Value> = (0..n)
            .map(|i| {
                let mut b = json!({"type": "text", "text": format!("message {i}")});
                if i + 1 == n || also.contains(&i) {
                    b["cache_control"] = cc();
                }
                json!({"role": if i % 2 == 0 { "user" } else { "assistant" }, "content": [b]})
            })
            .collect();
        json!({
            "model": "claude-x",
            "system": [{"type": "text", "text": system.repeat(1200), "cache_control": cc()}],
            "messages": msgs,
        })
    }

    fn sim() -> Simulator {
        Simulator::new(Config::default())
    }

    fn send(s: &mut Simulator, b: &Value) -> Outcome {
        s.request(Wire::AnthropicMessages, &[("x-session-id".into(), "s".into())], b).unwrap()
    }

    #[test]
    fn appending_reads_everything_sent_before() {
        let mut s = sim();
        let a = send(&mut s, &body("sys ", 3, &[]));
        assert_eq!((a.read, a.miss.clone()), (0, None));
        assert!(a.write > 1024);
        let b = send(&mut s, &body("sys ", 5, &[]));
        assert_eq!(b.read, a.write, "everything the first request wrote is read");
        assert_eq!(b.miss, None);
        assert!(b.write > 0 && b.write < 50, "only the two new blocks are written");
    }

    #[test]
    fn a_mark_more_than_lookback_blocks_past_the_entry_misses() {
        let mut s = sim();
        send(&mut s, &body("sys ", 2, &[]));
        let o = send(&mut s, &body("sys ", 2 + 20, &[]));
        assert_eq!(o.miss, Some(Miss::Lookback));
        assert!(o.read > 0, "the system prompt's own mark still hits");
        // A mark on the previous request's last block keeps the chain.
        let mut s = sim();
        send(&mut s, &body("sys ", 2, &[]));
        let o = send(&mut s, &body("sys ", 2 + 20, &[1]));
        assert_eq!(o.miss, None);
    }

    #[test]
    fn a_side_request_off_the_shared_prefix_is_not_a_miss_but_a_changed_one_is() {
        // A shorter request that branches after message 2 (a summary request): it reads what was
        // cached before the branch.
        let mut s = sim();
        send(&mut s, &body("sys ", 3, &[]));
        send(&mut s, &body("sys ", 6, &[]));
        let mut side = body("sys ", 4, &[2]);
        side["messages"][3]["content"][0]["text"] = json!("summarize");
        let o = send(&mut s, &side);
        assert_eq!(o.miss, None, "{o:?}");
        assert!(o.read > 0);
        // A longer request whose early message changed is still a changed prefix.
        let mut s = sim();
        send(&mut s, &body("sys ", 3, &[]));
        let mut changed = body("sys ", 6, &[]);
        changed["messages"][1]["content"][0]["text"] = json!("edited");
        assert!(matches!(send(&mut s, &changed).miss, Some(Miss::PrefixChanged { .. })));
    }

    #[test]
    fn a_latest_prompt_only_cache_misses_a_branch_but_serves_an_extension() {
        let mut s = Simulator::new(Config { latest_prompt_only: true, ..Config::default() });
        let chat = |n: usize, extra: Option<&str>| {
            let mut msgs = vec![json!({"role": "system", "content": "sys ".repeat(1200)})];
            msgs.extend((0..n).map(|i| json!({"role": "user", "content": format!("message {i}")})));
            if let Some(e) = extra {
                msgs.push(json!({"role": "user", "content": e}));
            }
            json!({"model": "deepseek/x", "messages": msgs, "prompt_cache_key": "k"})
        };
        s.request(Wire::ChatCompletions, &[], &chat(2, None)).unwrap();
        s.request(Wire::ChatCompletions, &[], &chat(4, None)).unwrap();
        let branch = s.request(Wire::ChatCompletions, &[], &chat(2, Some("summarize"))).unwrap();
        assert_eq!(branch.read, 0, "only the latest whole prompt is kept");
        s.request(Wire::ChatCompletions, &[], &chat(4, None)).unwrap();
        let extension = s.request(Wire::ChatCompletions, &[], &chat(4, Some("summarize"))).unwrap();
        assert!(extension.read > 0 && extension.miss.is_none());
    }

    #[test]
    fn more_than_four_marks_is_rejected() {
        let mut s = sim();
        let e = s.request(Wire::AnthropicMessages, &[], &body("sys ", 6, &[0, 1, 2, 3])).unwrap_err();
        assert!(e.contains("maximum of 4"), "{e}");
    }

    #[test]
    fn a_one_hour_mark_after_a_five_minute_mark_is_rejected() {
        let mut s = sim();
        let mut b = body("sys ", 2, &[]);
        b["messages"][1]["content"][0]["cache_control"] = json!({"type": "ephemeral", "ttl": "1h"});
        assert!(s.request(Wire::AnthropicMessages, &[], &b).unwrap_err().contains("1h"));
    }

    #[test]
    fn entries_expire_unless_read_and_reads_reset_the_ttl() {
        let mut s = sim();
        send(&mut s, &body("sys ", 2, &[]));
        s.advance(Duration::from_secs(4 * 60));
        assert_eq!(send(&mut s, &body("sys ", 2, &[])).miss, None);
        s.advance(Duration::from_secs(4 * 60));
        assert_eq!(send(&mut s, &body("sys ", 2, &[])).miss, None, "the read at 4m reset the TTL");
        s.advance(Duration::from_secs(6 * 60));
        let o = send(&mut s, &body("sys ", 2, &[]));
        assert_eq!((o.read, o.miss), (0, Some(Miss::Expired)));
    }

    #[test]
    fn one_hour_writes_survive_a_long_pause_and_are_accounted() {
        let mut s = sim();
        let mut b = body("sys ", 2, &[]);
        let long = json!({"type": "ephemeral", "ttl": "1h"});
        b["system"][0]["cache_control"] = long.clone();
        b["messages"][1]["content"][0]["cache_control"] = long;
        let a = send(&mut s, &b);
        assert_eq!(a.write_1h, a.write);
        s.advance(Duration::from_secs(50 * 60));
        assert_eq!(send(&mut s, &b).miss, None);
    }

    #[test]
    fn a_changed_byte_names_the_block() {
        let mut s = sim();
        send(&mut s, &body("sys ", 3, &[]));
        let o = send(&mut s, &body("SYS ", 4, &[]));
        match o.miss {
            Some(Miss::PrefixChanged { segment: "system", index: 0, .. }) => {}
            m => panic!("{m:?}"),
        }
    }

    #[test]
    fn a_string_and_a_one_part_array_are_different_bytes() {
        let mut s = sim();
        let mut a = body("sys ", 3, &[]);
        a["messages"][0]["content"] = json!("message 0");
        send(&mut s, &a);
        let o = send(&mut s, &body("sys ", 4, &[]));
        assert!(matches!(o.miss, Some(Miss::PrefixChanged { segment: "messages", index: 0, .. })), "{:?}", o.miss);
    }

    #[test]
    fn thinking_changes_invalidate_messages_but_not_system() {
        let mut s = sim();
        let a = send(&mut s, &body("sys ", 3, &[]));
        let mut b = body("sys ", 4, &[]);
        b["thinking"] = json!({"type": "enabled", "budget_tokens": 2048});
        let o = send(&mut s, &b);
        assert_eq!(o.miss, Some(Miss::Params));
        assert!(o.read > 0 && o.read < a.write, "the system prompt is still read");
    }

    #[test]
    fn a_different_model_starts_cold() {
        let mut s = sim();
        send(&mut s, &body("sys ", 3, &[]));
        let mut b = body("sys ", 4, &[]);
        b["model"] = json!("claude-y");
        assert_eq!(send(&mut s, &b).miss, Some(Miss::Model));
    }

    #[test]
    fn prefixes_below_the_minimum_are_not_written() {
        let mut s = sim();
        let mut b = body("", 2, &[]);
        b["system"][0]["text"] = json!("short");
        let o = send(&mut s, &b);
        assert_eq!((o.write, o.read), (0, 0));
        assert_eq!(o.input, o.prompt());
    }

    #[test]
    fn sessions_stick_to_an_upstream_and_a_fallback_misses() {
        let mut s = sim();
        send(&mut s, &body("sys ", 2, &[]));
        assert_eq!(send(&mut s, &body("sys ", 3, &[])).miss, None);
        s.move_upstream("s");
        let o = send(&mut s, &body("sys ", 4, &[]));
        assert_eq!((o.read, o.miss), (0, Some(Miss::Upstream)));
        // Without a session id requests rotate between upstreams and keep missing.
        let mut s = sim();
        s.request(Wire::AnthropicMessages, &[], &body("sys ", 2, &[])).unwrap();
        let o = s.request(Wire::AnthropicMessages, &[], &body("sys ", 3, &[])).unwrap();
        assert_eq!(o.miss, Some(Miss::Upstream));
    }

    #[test]
    fn automatic_caching_reads_the_longest_prefix_sent_before() {
        let mut s = sim();
        let chat = |n: usize| {
            let mut msgs = vec![json!({"role": "system", "content": "sys ".repeat(1200)})];
            msgs.extend((0..n).map(|i| json!({"role": "user", "content": format!("message {i}")})));
            json!({"model": "openai/gpt-x", "messages": msgs, "prompt_cache_key": "k"})
        };
        let a = s.request(Wire::ChatCompletions, &[], &chat(2)).unwrap();
        assert!(!a.explicit && a.read == 0 && a.write == 0);
        let b = s.request(Wire::ChatCompletions, &[], &chat(3)).unwrap();
        assert_eq!((b.read, b.miss), (a.prompt(), None));
    }

    #[test]
    fn openrouter_claude_is_explicit_and_reads_marks_on_parts_and_tools() {
        let mut s = sim();
        let req = |n: usize| {
            let mut msgs = vec![json!({"role": "system", "content": [{"type": "text", "text": "sys ".repeat(1200), "cache_control": cc()}]})];
            msgs.extend((0..n).map(|i| {
                let mut p = json!({"type": "text", "text": format!("message {i}")});
                if i + 1 == n {
                    p["cache_control"] = cc();
                }
                json!({"role": "user", "content": [p]})
            }));
            json!({"model": "anthropic/claude-x", "messages": msgs,
                   "tools": [{"type": "function", "function": {"name": "read"}, "cache_control": cc()}]})
        };
        let h = [("x-session-id".to_string(), "s".to_string())];
        let a = s.request(Wire::ChatCompletions, &h, &req(2)).unwrap();
        assert!(a.explicit && a.marks == 3 && a.write > 1024);
        let b = s.request(Wire::ChatCompletions, &h, &req(3)).unwrap();
        assert_eq!((b.read, b.miss), (a.write, None));
    }
}
