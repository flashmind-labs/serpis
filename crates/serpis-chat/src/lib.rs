//! A chat without a window: the conversation, the agent running it, its session file, its prompt
//! cache bookkeeping, compaction, and the transcript every client of it shows.
//!
//! The desktop runs chats in-process with this crate, and `serpis-node` runs them on boxes; both
//! show them from the same [`serpis_proto::Entry`] transcript, handed out as splices. Nothing
//! here draws, and nothing depends on a GUI: what a chat needs from where it runs (settings,
//! hooks, tools, credentials, a runtime) comes through [`Host`].

pub mod cache_watch;
pub mod config;
pub mod session;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use serpis_agent::{AgentConfig, AgentEvent, AgentHandle, ToolHooks, ToolRef};
use serpis_ai::*;
use serpis_proto::{CacheHit, ChatState, Entry, Image, Splice, Tone};

pub use cache_watch::{fmt_cost, fmt_duration, fmt_tokens};

/// The session entry holding the system prompt a chat sends.
pub const SYSTEM_PROMPT_ENTRY: &str = "serpis-system-prompt";

/// How loud a notice is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// What a chat needs from where it runs: the desktop editor, or a node.
pub trait Host {
    /// A setting (a variable on the desktop, the node's settings on a box).
    fn var(&self, name: &str) -> Option<Value>;
    /// Run the handlers of hook `name`; their answers, in order. `chat` is the chat as it is
    /// now, for hosts that show it to the handlers.
    fn hook(&mut self, chat: &Chat, name: &str, payload: &Value) -> Vec<Value>;
    fn notify(&mut self, level: Level, text: String);
    /// The tools this chat's agent gets.
    fn tools(&self) -> Vec<ToolRef>;
    /// `before-tool-call` / `after-tool-call` handlers, called from the agent's task.
    fn tool_hooks(&self) -> Option<Arc<dyn ToolHooks>>;
    fn registry(&self) -> &Registry;
    fn runtime(&self) -> tokio::runtime::Handle;
    /// What hook payloads call this chat (`buffer` on the desktop).
    fn chat_ref(&self) -> Value;
    /// Where new session files go.
    fn session_root(&self) -> PathBuf {
        match self.var("session-directory").and_then(|v| v.as_str().map(str::to_string)) {
            Some(d) if !d.is_empty() => expand_home(&d),
            _ => self.registry().paths().agent_dir.join("sessions"),
        }
    }
    /// The chat wrote its session file for the first time.
    fn session_created(&mut self, _cwd: &Path) {}

    fn var_bool(&self, name: &str, default: bool) -> bool {
        self.var(name).and_then(|v| v.as_bool()).unwrap_or(default)
    }
    fn var_u64(&self, name: &str, default: u64) -> u64 {
        self.var(name).and_then(|v| v.as_u64()).unwrap_or(default)
    }
    fn var_str(&self, name: &str) -> Option<String> {
        self.var(name).and_then(|v| v.as_str().map(str::to_string))
    }
}

/// The model a new chat starts with: `agent_model` (`provider/id`, the `agent-model` setting),
/// then pi's settings (`defaultProvider`/`defaultModel`), then any model with credentials.
pub fn default_model(registry: &Registry, agent_model: Option<&str>) -> Option<Model> {
    let supported = |m: &Model| api::SUPPORTED_APIS.contains(&m.api.as_str()) || m.api == "azure-openai-responses";
    if let Some(m) = agent_model.and_then(|s| s.split_once('/')).and_then(|(p, id)| registry.find(p, id)) {
        return Some(m);
    }
    if let (Some(p), Some(id)) = (pi_setting("defaultProvider"), pi_setting("defaultModel")) {
        if let Some(m) = registry.find(&p, &id) {
            return Some(m);
        }
    }
    let configured = registry.configured_providers();
    registry.models().into_iter().find(|m| configured.contains(&m.provider) && supported(m))
}

/// A setting from pi's `~/.pi/agent/settings.json`.
pub fn pi_setting(key: &str) -> Option<String> {
    let p = dirs::home_dir()?.join(".pi/agent/settings.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
    v.get(key)?.as_str().map(str::to_string)
}

/// `~/x` → the home folder's `x`.
pub fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/").or_else(|| (p == "~").then_some("")) {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None => PathBuf::from(p),
    }
}

/// Why a chat compacts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompactReason {
    Manual,
    Threshold,
    Overflow,
}

impl CompactReason {
    pub fn word(self) -> &'static str {
        match self {
            CompactReason::Manual => "manual",
            CompactReason::Threshold => "threshold",
            CompactReason::Overflow => "overflow",
        }
    }
}

/// Work finished off the chat's thread, picked up by [`Chat::poll`].
enum Done {
    Agent(AgentEvent),
    Compacted { a: AssistantMessage, cut: usize, tokens: u64, reason: CompactReason },
    /// The usage limit it stopped at has reset: time to go on.
    LimitReset,
}

/// Called whenever the chat has something new for [`Chat::poll`] (the desktop repaints, a node
/// wakes its loop).
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// The transcript's entries, remembering the first one changed since the last splice was
/// taken, so clients get only what changed. Reads go through `Deref`; every change goes
/// through a method that notes where it happened.
#[derive(Default)]
struct Transcript {
    items: Vec<Entry>,
    /// The first entry changed since the last splice (None: nothing changed).
    changed: Option<usize>,
    /// When each running tool call started (clients are told how long it has run).
    started: HashMap<String, Instant>,
}

impl std::ops::Deref for Transcript {
    type Target = [Entry];
    fn deref(&self) -> &[Entry] {
        &self.items
    }
}

impl Transcript {
    fn mark(&mut self, i: usize) {
        self.changed = Some(self.changed.map_or(i, |c| c.min(i)));
    }

    fn push(&mut self, e: Entry) {
        self.mark(self.items.len());
        self.items.push(e);
    }

    fn clear(&mut self) {
        self.items.clear();
        self.started.clear();
        self.mark(0);
    }

    fn get_mut(&mut self, i: usize) -> Option<&mut Entry> {
        self.mark(i);
        self.items.get_mut(i)
    }

    fn last_mut(&mut self) -> Option<&mut Entry> {
        self.items.len().checked_sub(1).and_then(|i| self.get_mut(i))
    }

    /// Tool call `id`, a subagent's step included.
    fn tool_mut(&mut self, id: &str) -> Option<&mut Entry> {
        let i = self.items.iter().rposition(|e| e.holds_tool(id))?;
        self.mark(i);
        serpis_proto::find_tool_mut(&mut self.items[i..=i], id)
    }

    fn find_mut(&mut self, f: impl Fn(&Entry) -> bool) -> Option<&mut Entry> {
        let i = self.items.iter().position(f)?;
        self.get_mut(i)
    }

    fn update_all(&mut self, f: impl Fn(&Entry) -> bool, mut change: impl FnMut(&mut Entry)) {
        for i in 0..self.items.len() {
            if f(&self.items[i]) {
                self.mark(i);
                change(&mut self.items[i]);
            }
        }
    }

    /// An entry as clients see it: running tools say how long they have run.
    fn wire(&self, e: &Entry, now: Instant) -> Entry {
        let mut e = e.clone();
        if let Entry::Tool { id, running_ms, elapsed_ms: None, result: None, .. } = &mut e {
            *running_ms = self.started.get(id).map(|s| now.saturating_duration_since(*s).as_millis() as u64);
        }
        e
    }

    fn take_changes(&mut self) -> Option<Splice> {
        let from = self.changed.take()?.min(self.items.len());
        let now = Instant::now();
        Some(Splice { from, entries: self.items[from..].iter().map(|e| self.wire(e, now)).collect() })
    }

    fn all(&self) -> Splice {
        let now = Instant::now();
        Splice { from: 0, entries: self.items.iter().map(|e| self.wire(e, now)).collect() }
    }
}

/// How a new chat starts.
pub struct ChatInit {
    pub cwd: PathBuf,
    pub model: Option<Model>,
    pub thinking: ThinkingLevel,
}

pub struct Chat {
    entries: Transcript,
    pub history: Vec<Message>,
    agent: Option<AgentHandle>,
    /// The `claude` process of a chat on a Claude Code model, kept between turns.
    claude: serpis_agent::claude_code::ClaudeCode,
    /// Tool calls waiting for the person to allow them, by id: where the answer goes.
    asks: HashMap<String, serpis_agent::AskReply>,
    /// Claude Code's permission mode for this chat, once chosen or reported (else its
    /// machine's `claude-code-permission-mode`).
    permission_mode: Option<String>,
    /// How much the reply being written has thought (Claude Code's estimate).
    thinking_tokens: Option<u64>,
    /// Where its account stands against its usage limits, as Claude Code last said.
    limit: Option<serpis_proto::Limit>,
    /// How full Claude Code says its context is: (tokens, window, where it compacts by itself).
    claude_context: Option<(u64, u64, Option<u64>)>,
    /// Claude Code is compacting its context.
    claude_compacting: bool,
    /// It stopped at a usage limit, and goes on by itself at this time (unix seconds); the
    /// token cancels the wait.
    continues: Option<(i64, CancellationToken)>,
    /// The machine's `claude-code-permission-mode`, as last read (what the chat shows until it
    /// has its own).
    default_mode: String,
    tx: mpsc::Sender<Done>,
    rx: mpsc::Receiver<Done>,
    wake: Wake,
    pub model: Option<Model>,
    pub thinking: ThinkingLevel,
    pub session_id: String,
    pub cwd: PathBuf,
    pub totals: Usage,
    /// Short title: the first prompt unless set.
    pub title: String,
    /// Error of the last run, if it failed.
    pub last_error: Option<String>,
    /// Custom session entries (pi's `custom` entries): (type, data), oldest first.
    pub custom_entries: Vec<(String, Value)>,
    /// Where the conversation is recorded (created with the first prompt).
    pub session: Option<session::SessionWriter>,
    session_failed: bool,
    /// The system prompt this chat sends, built on its first message and kept so the provider can
    /// reuse its cache: (tool names it was built for, prompt). `refresh_context` drops it.
    system_prompt: Option<(Vec<String>, String)>,
    /// What the prompt cache is doing (reads, misses and their causes, warm or cold).
    pub cache: cache_watch::CacheWatch,
    /// What the running agent's requests are sent with.
    sent: Option<cache_watch::Sent>,
    request_started: Option<Instant>,
    /// Each history message's entry id in the session file (None: not saved, or a summary).
    entry_ids: Vec<Option<String>>,
    /// A compaction in progress (cancelled by abort).
    compacting: Option<CancellationToken>,
    /// Compactions so far: requests after one miss the cache for that reason.
    compactions: u64,
    /// A context overflow was already answered by compacting and retrying.
    overflow_retried: bool,
    /// A resumed session's system prompt, compared with the tools once they are all registered
    /// (the next poll).
    pending_resume: Option<PendingResume>,
    /// Prompts already submitted, by request id: a client retrying one gets the same answer.
    seen_requests: Vec<String>,
    /// Whether a turn cut off by its node stopping continues when the node starts again, unless
    /// the chat says otherwise ([`Chat::keep_going`]): the host's setting.
    pub keep_going_default: bool,
}

struct PendingResume {
    names: Vec<String>,
    prompt: String,
    last: AssistantMessage,
    model: Model,
    retention: CacheRetention,
    age: Duration,
}

impl Chat {
    pub fn new(init: ChatInit, wake: Wake) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut chat = Chat {
            entries: Transcript::default(),
            history: Vec::new(),
            agent: None,
            claude: Default::default(),
            asks: HashMap::new(),
            permission_mode: None,
            thinking_tokens: None,
            limit: None,
            continues: None,
            claude_context: None,
            claude_compacting: false,
            default_mode: "default".into(),
            tx,
            rx,
            wake,
            thinking: init.model.as_ref().map(|m| clamp_thinking_level(m, init.thinking)).unwrap_or(init.thinking),
            model: init.model,
            session_id: uuid::Uuid::now_v7().to_string(),
            cwd: init.cwd,
            totals: Usage::default(),
            title: String::new(),
            last_error: None,
            custom_entries: Vec::new(),
            session: None,
            session_failed: false,
            system_prompt: None,
            cache: Default::default(),
            sent: None,
            request_started: None,
            entry_ids: Vec::new(),
            keep_going_default: true,
            compacting: None,
            compactions: 0,
            overflow_retried: false,
            pending_resume: None,
            seen_requests: Vec::new(),
        };
        if chat.model.is_none() {
            chat.entries.push(Entry::Info {
                text: "No model configured. Run `chat-select-model` (C-c C-m) — models come from your pi/serpis models.json, auth.json and API key environment variables.".into(),
                tone: Tone::Warning,
            });
        }
        chat
    }

    /// Whether this chat runs on Claude Code (a `claude-code/*` model).
    pub fn is_claude_code(&self) -> bool {
        self.model.as_ref().is_some_and(|m| m.api == serpis_ai::claude_code::PROVIDER)
    }

    pub fn running(&self) -> bool {
        self.agent.is_some()
    }

    pub fn compacting(&self) -> bool {
        self.compacting.is_some() || self.claude_compacting
    }

    /// Where the chat's agent compacts its context by itself, in tokens (Claude Code's).
    pub fn compacts_at(&self) -> Option<u64> {
        self.claude_context.and_then(|(_, _, at)| at).filter(|_| self.is_claude_code())
    }

    /// The running agent (aborting through it takes effect at once).
    pub fn agent(&self) -> Option<&AgentHandle> {
        self.agent.as_ref()
    }

    /// The transcript as clients show it.
    pub fn entries(&self) -> &[Entry] {
        &self.entries
    }

    pub fn abort(&mut self) {
        if let Some(a) = &self.agent {
            a.abort();
        }
        if let Some(c) = &self.compacting {
            c.cancel();
        }
    }

    /// Switch this chat's Claude Code to permission mode `mode` (one of [`PERMISSION_MODES`]),
    /// at once if it is running, else from its next turn.
    pub fn set_permission_mode(&mut self, host: &mut dyn Host, mode: &str) -> Result<(), String> {
        if !PERMISSION_MODES.contains(&mode) {
            return Err(format!("{mode} isn't a Claude Code permission mode (one of {})", PERMISSION_MODES.join(", ")));
        }
        if self.permission_mode.as_deref() != Some(mode) {
            self.append_entry(host, CLAUDE_MODE, json!({"mode": mode}));
        }
        self.permission_mode = Some(mode.to_string());
        self.claude.set_permission_mode(mode);
        (self.wake)();
        Ok(())
    }

    /// Ask Claude Code to stop its background task `task` (a command, a subagent: the `id` of
    /// a tool entry's `task`). False when it has no process to ask.
    /// Move what tool call `tool` runs (a command, a subagent) to the background: its call
    /// returns and the turn goes on. False when Claude Code has no process to ask.
    pub fn background_task(&mut self, tool: &str) -> bool {
        self.claude.background_task(tool)
    }

    pub fn stop_task(&mut self, task: &str) -> bool {
        self.claude.stop_task(task)
    }

    pub fn clear(&mut self) {
        self.abort();
        self.claude.stop();
        self.stop_waiting_for_reset();
        self.limit = None;
        self.entries.clear();
        self.history.clear();
        self.totals = Usage::default();
        self.session_id = uuid::Uuid::now_v7().to_string();
        self.title.clear();
        self.last_error = None;
        self.session = None;
        self.system_prompt = None;
        self.cache = Default::default();
        self.entry_ids.clear();
        self.compactions = 0;
        self.custom_entries.clear();
    }

    pub fn set_model(&mut self, m: Model) {
        if m.api != serpis_ai::claude_code::PROVIDER {
            self.claude.stop();
        }
        self.thinking = clamp_thinking_level(&m, self.thinking);
        self.model = Some(m);
    }

    pub fn cycle_thinking(&mut self) {
        let levels = self.model.as_ref().map(supported_thinking_levels).unwrap_or_else(|| ThinkingLevel::ALL.to_vec());
        let i = levels.iter().position(|l| *l == self.thinking).map(|i| (i + 1) % levels.len()).unwrap_or(0);
        self.thinking = levels[i];
    }

    pub fn set_thinking(&mut self, level: ThinkingLevel) {
        self.thinking = self.model.as_ref().map(|m| clamp_thinking_level(m, level)).unwrap_or(level);
    }

    /// Show a notice in the transcript.
    pub fn notice(&mut self, text: impl Into<String>, tone: Tone) {
        self.entries.push(Entry::Info { text: text.into(), tone });
    }

    /// Submit a user message: start a run, or steer the running one (`follow_up`: after it).
    /// Err when it was refused (a `before-prompt` hook, no model, a compaction running), with
    /// why; the refusal was also reported to the host. A `request_id` seen before is answered
    /// Ok without sending it again, so a client can retry a send whose answer it lost.
    pub fn submit(&mut self, host: &mut dyn Host, text: String, images: Vec<Image>, follow_up: bool, request_id: Option<&str>) -> Result<(), String> {
        if let Some(r) = request_id {
            if self.seen_requests.iter().any(|s| s == r) {
                return Ok(());
            }
        }
        let refuse = |host: &mut dyn Host, level: Level, why: String| {
            host.notify(level, why.clone());
            Err(why)
        };
        let model_spec = self.model.as_ref().map(|m| format!("{}/{}", m.provider, m.id));
        let payload = json!({"buffer": host.chat_ref(), "text": text, "model": model_spec, "running": self.running()});
        if let Some(reason) = host.hook(self, "before-prompt", &payload).iter().find_map(|r| r.get("block").map(|b| b.as_str().unwrap_or("blocked").to_string())) {
            return refuse(host, Level::Warn, reason);
        }
        if self.compacting.is_some() {
            return refuse(host, Level::Info, "The chat is compacting its history: send again when it's done".into());
        }
        if !images.is_empty()
            && let Some(m) = self.model.as_ref().filter(|m| !m.input.iter().any(|i| i == "image"))
        {
            return refuse(host, Level::Error, format!("{} doesn't take images (chat-select-model, or remove the attachments)", m.id));
        }
        if self.model.is_none() {
            return refuse(host, Level::Error, "No model selected (chat-select-model)".into());
        }
        if let Some(r) = request_id {
            self.seen_requests.push(r.to_string());
            if self.seen_requests.len() > 256 {
                self.seen_requests.remove(0);
            }
        }
        if self.title.is_empty() {
            self.title = if text.is_empty() { "Image".into() } else { title_from(&text) };
        }
        // `/skill:name args`: the skill's instructions go with the message (shown as typed).
        let sent = match text.strip_prefix("/skill:") {
            Some(rest) => {
                let skills = self.skills(host);
                let name = rest.split_whitespace().next().unwrap_or("");
                match skills.iter().find(|s| s.name == name) {
                    // Claude Code's own skill: it loads it itself.
                    Some(s) if self.is_claude_code() && s.source == "claude" => format!("/{}", rest.trim()),
                    Some(_) => match serpis_agent::skills::expand(&text, &skills) {
                        Some(x) => x,
                        None => return refuse(host, Level::Error, format!("Couldn't read the skill {name}")),
                    },
                    None => return refuse(host, Level::Error, format!("No skill named {name} (chat-select-skill lists them)")),
                }
            }
            None => text.clone(),
        };
        let mut content = Vec::new();
        if !sent.is_empty() {
            content.push(UserContent::Text { text: sent });
        }
        for i in &images {
            content.push(UserContent::Image { data: i.data.clone(), mime_type: i.mime.clone() });
        }
        let msg = Message::User { content, timestamp: now_ms() };
        // Sent before the limit resets: it goes now (and waits again if still refused).
        self.stop_waiting_for_reset();
        if let Some(agent) = &self.agent {
            if follow_up {
                agent.follow_up(msg);
            } else {
                agent.steer(msg);
            }
            self.entries.push(Entry::User { text, queued: true, images });
            return Ok(());
        }
        if self.is_claude_code() {
            self.answer_cut_off_calls(host);
            return self.start_claude(host, vec![msg]);
        }
        let Some(cfg) = self.agent_config(host) else { return Err("No model selected".into()) };
        // A turn that was cut off and not continued (parked, or not kept going).
        self.answer_cut_off_calls(host);
        if let Some((ago, tokens, cost)) = self.cache.expired(Instant::now()) {
            if cache_warning_applies(&|n: &str| host.var(n), "expired", cost) {
                host.notify(Level::Info, format!("The prompt cache expired {} ago: this message re-caches {} tokens ({})", fmt_duration(ago), fmt_tokens(tokens), fmt_cost(cost)));
            }
        }
        self.start_agent(host, cfg, vec![msg]);
        Ok(())
    }

    fn ensure_session(&mut self, host: &dyn Host) {
        if self.session.is_none() && host.var_bool("session-save", true) {
            self.session = Some(session::SessionWriter::new(&host.session_root(), &self.cwd, &self.session_id));
        }
    }

    /// What this chat's requests are sent with (model, tools, system prompt, options), also
    /// noted for the cache bookkeeping. None without a model.
    fn agent_config(&mut self, host: &mut dyn Host) -> Option<AgentConfig> {
        let model = self.model.clone()?;
        self.ensure_session(host);
        let (provider, id, thinking) = (model.provider.clone(), model.id.clone(), self.thinking.as_str());
        self.record(host, |w| w.note_settings(&provider, &id, thinking).map(|_| ()));
        let tools = host.tools();
        let system_prompt = self.system_prompt_for(host, &tools);
        let native = host.var_bool("agent-native-web-search", true);
        let cache_retention = model.cache_retention(cache_retention(host, &model.provider));
        self.sent = Some(cache_watch::Sent {
            model: format!("{}/{}", model.provider, model.id),
            thinking: self.thinking.as_str().to_string(),
            tools: tools.iter().map(|t| cache_watch::declaration_key(&t.declaration())).collect(),
            prompt: prompt_hash(&system_prompt),
            retention: cache_retention,
            compactions: self.compactions,
        });
        Some(AgentConfig {
            api_key: host.registry().api_key(&model.provider),
            headers: host.registry().provider_headers(&model.provider),
            model,
            reasoning: self.thinking,
            tools,
            system_prompt,
            session_id: self.session_id.clone(),
            cwd: self.cwd.clone(),
            native_web_search: native,
            max_tokens: None,
            cache_retention,
            hooks: host.tool_hooks(),
            sequential_tools: false,
        })
    }

    /// Run the agent on the history plus `prompts` (none: continue, e.g. after compacting).
    fn start_agent(&mut self, host: &dyn Host, cfg: AgentConfig, prompts: Vec<Message>) {
        let (tx, wake) = (self.tx.clone(), self.wake.clone());
        let emit: serpis_agent::EmitFn = Arc::new(move |ev| {
            let _ = tx.send(Done::Agent(ev));
            wake();
        });
        let rt = host.runtime();
        let _guard = rt.enter();
        self.last_error = None;
        self.agent = Some(serpis_agent::run(self.history.clone(), prompts, cfg, emit));
    }

    /// Run a turn on Claude Code: the `claude` CLI runs it, with its own tools, permissions and
    /// context, and its events come back as an agent's would. Its session id is the chat's.
    fn start_claude(&mut self, host: &mut dyn Host, prompts: Vec<Message>) -> Result<(), String> {
        let Some(model) = self.model.clone() else { return Err("No model selected".into()) };
        let binary = match host.var_str("claude-code-path").filter(|p| !p.is_empty()) {
            Some(p) => Some(expand_home(&p)),
            None => host.registry().claude_code_binary(),
        };
        let Some(binary) = binary else {
            let why = "Claude Code isn't installed on this machine (claude not found; set claude-code-path)".to_string();
            host.notify(Level::Error, why.clone());
            return Err(why);
        };
        self.ensure_session(host);
        let (provider, id, thinking) = (model.provider.clone(), model.id.clone(), self.thinking.as_str());
        self.record(host, |w| w.note_settings(&provider, &id, thinking).map(|_| ()));
        // Thinking levels are Claude Code's efforts; `off` only where the model can do without.
        let (effort, mut env) = match model.thinking_map(self.thinking) {
            Some(Some("off")) => (None, vec![("MAX_THINKING_TOKENS".to_string(), "0".to_string())]),
            Some(Some(v)) => (Some(v.to_string()), Vec::new()),
            _ => (None, Vec::new()),
        };
        // Its commands run for an agent, as serpis's own agent's do (a shell config can tell).
        env.push((serpis_agent::AGENT_ENV.0.into(), serpis_agent::AGENT_ENV.1.into()));
        env.push(("SERPIS_SESSION_ID".into(), self.session_id.clone()));
        let args: Vec<String> = match host.var("claude-code-args") {
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(str::to_string).collect(),
            _ => Vec::new(),
        };
        // serpis's skills (pi's), for it to load as its own: those it has already (its
        // ~/.claude/skills) left out.
        let skills = host.var_bool("claude-code-skills", true).then(|| {
            let mut places = self.skill_places(host);
            places.claude = false;
            let theirs = serpis_agent::skills::claude_names(&places.home, &self.cwd);
            let ours: Vec<_> = if host.var_bool("agent-skills", true) { serpis_agent::skills::discover(&places) } else { Vec::new() };
            let ours: Vec<_> = ours.into_iter().filter(|s| !theirs.contains(&s.name)).collect();
            serpis_agent::skills::for_prompt(&ours, "Read")
        });
        let launch = serpis_agent::claude_code::Launch {
            binary,
            cwd: self.cwd.clone(),
            model: model.id.clone(),
            effort,
            permission_mode: self.permission_mode.clone().or_else(|| host.var_str("claude-code-permission-mode")).unwrap_or_else(|| "default".into()),
            thinking_display: Some(host.var_str("claude-code-thinking-display").unwrap_or_else(|| "summarized".into())).filter(|d| !d.is_empty()),
            args,
            skills,
            skills_restart: host.var_bool("claude-code-skills-restart", false),
            env,
        };
        let cfg = serpis_agent::claude_code::RunConfig {
            launch,
            session_id: self.session_id.clone(),
            model,
            hooks: host.tool_hooks(),
            prompts: host.var_str("claude-code-permission-prompts").and_then(|p| serpis_agent::claude_code::Prompts::parse(&p)).unwrap_or_default(),
            task_output_every: Some(Duration::from_millis(host.var_u64("claude-code-task-output-interval", 1000))).filter(|d| !d.is_zero()),
        };
        // Prompt cache bookkeeping is Claude Code's own: what it sends isn't serpis's to know.
        self.sent = None;
        let (tx, wake) = (self.tx.clone(), self.wake.clone());
        let emit: serpis_agent::EmitFn = Arc::new(move |ev| {
            let _ = tx.send(Done::Agent(ev));
            wake();
        });
        let rt = host.runtime();
        let _guard = rt.enter();
        self.last_error = None;
        self.agent = Some(self.claude.run(self.history.clone(), prompts, cfg, emit));
        Ok(())
    }

    /// Replace the messages before a cut point with a summary (manually, or automatically near
    /// the context window). The summary is asked for in the conversation itself, so the request
    /// reads the prompt cache; see `serpis_agent::compaction`.
    pub fn compact(&mut self, host: &mut dyn Host, reason: CompactReason, instructions: Option<String>) {
        use serpis_agent::compaction::{context_tokens, cut_point, estimate_tokens, summary_request};
        if self.is_claude_code() {
            // Claude Code compacts its own context (automatically, or with /compact).
            if reason == CompactReason::Manual {
                let text = match instructions {
                    Some(i) if !i.trim().is_empty() => format!("/compact {i}"),
                    _ => "/compact".to_string(),
                };
                let _ = self.submit(host, text, Vec::new(), true, None);
            }
            return;
        }
        if self.running() || self.compacting.is_some() {
            if reason == CompactReason::Manual {
                host.notify(Level::Info, "Wait for the agent to finish before compacting".into());
            }
            return;
        }
        let keep = host.var_u64("chat-compact-keep-tokens", 20_000);
        let Some(cut) = cut_point(&self.history, keep) else {
            if reason == CompactReason::Manual {
                host.notify(Level::Info, "Nothing to compact yet: the conversation is within chat-compact-keep-tokens".into());
            }
            return;
        };
        let tokens = context_tokens(&self.history).unwrap_or_else(|| self.history.iter().map(estimate_tokens).sum());
        let payload = json!({"buffer": host.chat_ref(), "reason": reason.word(), "tokens": tokens, "messages": cut});
        for r in host.hook(self, "before-compact", &payload) {
            match r {
                Value::Bool(false) => {
                    host.notify(Level::Info, "Compaction cancelled by a before-compact hook".into());
                    return;
                }
                Value::Object(o) if o.get("summary").is_some_and(Value::is_string) => {
                    let summary = o["summary"].as_str().unwrap_or_default().to_string();
                    self.apply_compaction(host, summary, cut, tokens, None);
                    return;
                }
                _ => {}
            }
        }
        let Some(mut cfg) = self.agent_config(host) else {
            host.notify(Level::Error, "No model selected (chat-select-model)".into());
            return;
        };
        let reserve = host.var_u64("chat-compact-reserve-tokens", 16_384);
        cfg.max_tokens = Some((reserve * 4 / 5).min(cfg.model.max_tokens).max(1024));
        let custom = instructions.or_else(|| host.var_str("chat-compact-instructions"));
        // The whole conversation extends the last request (every cache serves it); after an
        // overflow only the part before the cut fits.
        let msgs = summary_request(&self.history, cut, reason != CompactReason::Overflow, custom.as_deref());
        let cancel = CancellationToken::new();
        self.compacting = Some(cancel.clone());
        self.notice(format!("Compacting: summarizing {cut} earlier messages…"), Tone::Meta);
        let (tx, wake) = (self.tx.clone(), self.wake.clone());
        host.runtime().spawn(async move {
            let a = serpis_agent::complete(&cfg, msgs, cancel).await;
            let _ = tx.send(Done::Compacted { a, cut, tokens, reason });
            wake();
        });
    }

    fn finish_compaction(&mut self, host: &mut dyn Host, a: AssistantMessage, cut: usize, tokens: u64, reason: CompactReason) {
        self.compacting = None;
        add_usage(&mut self.totals, &a.usage);
        let summary = a.text().trim().to_string();
        let failure = match a.stop_reason {
            StopReason::Aborted => Some("stopped".to_string()),
            StopReason::Error => Some(a.error_message.clone().unwrap_or_else(|| "the request failed".into())),
            StopReason::Length => Some("the summary was cut off (raise chat-compact-reserve-tokens)".into()),
            _ if a.tool_calls().next().is_some() => Some("the model called a tool instead of summarizing".into()),
            _ if summary.is_empty() => Some("the model returned no summary".into()),
            _ => None,
        };
        if let Some(f) = failure {
            self.notice(format!("Compaction failed: {f}"), Tone::Error);
            return;
        }
        self.apply_compaction(host, summary, cut, tokens, Some(&a.usage));
        if reason == CompactReason::Overflow {
            // The turn that overflowed runs again on the compacted conversation.
            if let Some(cfg) = self.agent_config(host) {
                self.start_agent(host, cfg, Vec::new());
            }
        }
    }

    fn apply_compaction(&mut self, host: &mut dyn Host, summary: String, cut: usize, tokens: u64, usage: Option<&Usage>) {
        if let Some(first_kept) = self.entry_ids.get(cut).cloned().flatten() {
            self.record(host, |w| w.compaction(&summary, &first_kept, tokens).map(|_| ()));
        }
        let mut history = vec![serpis_agent::compaction::summary_message(&summary)];
        history.extend(self.history.drain(cut..));
        self.history = history;
        let mut ids = vec![None];
        ids.extend(self.entry_ids.drain(cut.min(self.entry_ids.len())..));
        self.entry_ids = ids;
        self.compactions += 1;
        let summary_tokens = serpis_agent::compaction::estimate_tokens(&self.history[0]);
        let mut text = format!("Compacted {cut} earlier messages ({} tokens of context) into a {}-token summary.", fmt_tokens(tokens), fmt_tokens(summary_tokens));
        if let Some(u) = usage {
            text.push_str(&format!(" The summary request read {} tokens from the prompt cache ({}).", fmt_tokens(u.cache_read), fmt_cost(u.cost.total).trim_start_matches('~')));
        }
        self.notice(text, Tone::Meta);
        let payload = json!({"buffer": host.chat_ref(), "summary": summary, "tokens": tokens, "messages": cut, "cache_read": usage.map(|u| u.cache_read)});
        host.hook(self, "chat-compacted", &payload);
    }

    /// After a run: compact when the context nears the window (chat-compact-auto), or when the
    /// run failed on a context overflow, then retry it once.
    fn compact_if_needed(&mut self, host: &mut dyn Host) {
        if !host.var_bool("chat-compact-auto", true) {
            return;
        }
        let Some(window) = self.model.as_ref().map(|m| m.context_window) else { return };
        let last = self.history.iter().rev().find_map(|m| match m {
            Message::Assistant(a) => Some(a.clone()),
            _ => None,
        });
        if let Some(a) = last.filter(|a| is_context_overflow(a, Some(window))) {
            if a.stop_reason == StopReason::Error && !self.overflow_retried {
                self.overflow_retried = true;
                return self.compact(host, CompactReason::Overflow, None);
            }
        }
        self.overflow_retried = false;
        let reserve = host.var_u64("chat-compact-reserve-tokens", 16_384);
        if serpis_agent::compaction::context_tokens(&self.history).is_some_and(|t| serpis_agent::compaction::should_compact(t, window, reserve)) {
            self.compact(host, CompactReason::Threshold, None);
        }
    }

    /// Context the conversation fills: the last response's tokens (what the next request sends,
    /// roughly), and the model's window. None before any response.
    pub fn context(&self) -> (Option<u64>, u64) {
        // Claude Code's own count (its window, what its system prompt and tools take).
        if let Some((used, window, _)) = self.claude_context.filter(|_| self.is_claude_code()) {
            return (Some(used), window);
        }
        let used = self.history.iter().rev().find_map(|m| match m {
            Message::Assistant(a) if !matches!(a.stop_reason, StopReason::Aborted | StopReason::Error) => Some(a.usage.input + a.usage.cache_read + a.usage.cache_write + a.usage.output),
            _ => None,
        });
        (used, self.model.as_ref().map(|m| m.context_window).unwrap_or(0))
    }

    /// The system prompt to send: the one this chat was started with, unless its tools changed
    /// (that misses the cache anyway, so the prompt catches up for free) or `agent-live-context`
    /// asks for a fresh one every time. Re-reading AGENTS.md / CLAUDE.md and the prompt vars on
    /// every message would rewrite the whole cached conversation whenever one of them changed.
    fn system_prompt_for(&mut self, host: &mut dyn Host, tools: &[ToolRef]) -> String {
        let names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
        let live = host.var_bool("agent-live-context", false);
        if let Some((n, p)) = &self.system_prompt {
            if *n == names && !live {
                return p.clone();
            }
        }
        let prompt = self.build_system_prompt(host, tools);
        // Saved with the session, so a resumed chat can send the same prompt (and read its cache).
        if self.system_prompt.as_ref().is_none_or(|(_, p)| *p != prompt) {
            self.append_entry(host, SYSTEM_PROMPT_ENTRY, json!({"prompt": prompt, "tools": names}));
        }
        self.system_prompt = Some((names, prompt.clone()));
        prompt
    }

    /// Where this chat's skills are looked for: the agent directory's `skills`, the project's
    /// (`.pi/skills`, `.agents/skills`), `agent-skill-paths` and pi's `skills` setting, and
    /// Claude Code's skill folders (`agent-skills-claude`).
    fn skill_places(&self, host: &dyn Host) -> serpis_agent::skills::Places {
        let agent_dir = host.registry().paths().agent_dir.clone();
        let mut paths: Vec<PathBuf> = match host.var("agent-skill-paths") {
            Some(Value::Array(a)) => a.iter().filter_map(Value::as_str).map(expand_home).collect(),
            _ => Vec::new(),
        };
        // pi's: its skills folder, and the paths its `skills` setting adds (from its directory).
        let pi_dir = host.var_bool("agent-skills-pi", true).then(serpis_ai::Paths::pi_agent_dir);
        if let Some(pi) = &pi_dir {
            let settings: Option<Value> = std::fs::read_to_string(pi.join("settings.json")).ok().and_then(|t| serde_json::from_str(&t).ok());
            if let Some(Value::Array(a)) = settings.as_ref().and_then(|v| v.get("skills")) {
                paths.extend(a.iter().filter_map(Value::as_str).filter(|p| !p.starts_with('!') && !p.starts_with('-')).map(|p| {
                    let p = expand_home(p.trim_start_matches('+'));
                    if p.is_absolute() { p } else { pi.join(p) }
                }));
            }
        }
        serpis_agent::skills::Places { agent_dir, pi_dir, cwd: self.cwd.clone(), paths, claude: host.var_bool("agent-skills-claude", false), home: dirs::home_dir().unwrap_or_default() }
    }

    /// The skills this chat has (none with `agent-skills` off).
    pub fn skills(&self, host: &dyn Host) -> Vec<serpis_agent::skills::Skill> {
        if !host.var_bool("agent-skills", true) {
            return Vec::new();
        }
        serpis_agent::skills::discover(&self.skill_places(host))
    }

    /// The system prompt as the project's instructions and settings make it now.
    fn build_system_prompt(&self, host: &dyn Host, tools: &[ToolRef]) -> String {
        let agent_dir = host.registry().paths().agent_dir.clone();
        let mut opts = serpis_agent::system_prompt::PromptOptions {
            context_files: serpis_agent::system_prompt::load_context_files(&self.cwd, &agent_dir),
            append: host.var_str("agent-append-system-prompt"),
            ..Default::default()
        };
        if let Some(Value::Array(g)) = host.var("agent-guidelines") {
            opts.guidelines = g.iter().filter_map(Value::as_str).map(str::to_string).collect();
        }
        // The skills it may load: named and described, read when a task matches (as pi does).
        let reader = ["read", "bash"].into_iter().find(|r| tools.iter().any(|t| t.name() == *r));
        if let Some(reader) = reader {
            let s = serpis_agent::skills::for_prompt(&self.skills(host), reader);
            if !s.is_empty() {
                opts.sections.insert("skills".into(), s);
            }
        }
        serpis_agent::system_prompt::build_system_prompt(tools, &self.cwd, &opts)
    }

    /// On resume: keep the system prompt the session was sent with while its cache can still
    /// be warm (chat-resume-system-prompt), and carry the cache state on from the last response.
    fn resume_system_prompt(&mut self, host: &dyn Host, l: &session::Loaded) {
        use cache_watch::{keep_prompt_on_resume, ttl};
        let saved = l.entries.iter().rev().find(|(k, _)| k == SYSTEM_PROMPT_ENTRY).and_then(|(_, d)| {
            let names: Vec<String> = d["tools"].as_array()?.iter().filter_map(Value::as_str).map(str::to_string).collect();
            Some((names, d["prompt"].as_str()?.to_string()))
        });
        let last = l.messages.iter().rev().find_map(|m| match m {
            Message::Assistant(a) if a.usage.input + a.usage.cache_read + a.usage.cache_write > 0 => Some(a),
            _ => None,
        });
        let (Some((names, prompt)), Some(last), Some(model)) = (saved, last.cloned(), self.model.clone()) else { return };
        let age = Duration::from_millis(now_ms().saturating_sub(last.timestamp));
        let retention = model.cache_retention(cache_retention(host, &model.provider));
        let mode = host.var_str("chat-resume-system-prompt").unwrap_or_else(|| "warm".into());
        if !keep_prompt_on_resume(&mode, age, ttl(retention)) {
            return;
        }
        self.system_prompt = Some((names.clone(), prompt.clone()));
        // Chats are restored while serpis starts, before extensions register their tools:
        // tools and instructions are compared on the next poll, once that is done.
        self.pending_resume = Some(PendingResume { names, prompt, last, model, retention, age });
    }

    fn finish_resume(&mut self, host: &mut dyn Host, p: PendingResume) {
        use cache_watch::{Sent, declaration_key, ttl};
        let tools = host.tools();
        let current: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
        // With other tools the next request misses anyway, and the prompt is rebuilt for them.
        if current == p.names {
            let changed = self.build_system_prompt(host, &tools) != p.prompt;
            if changed && host.var_bool("chat-resume-system-prompt-notice", true) {
                let left = ttl(p.retention).saturating_sub(p.age);
                self.notice(
                    format!(
                        "Kept the system prompt this chat was sent with, so its prompt cache (warm for about {} more) can be read. AGENTS.md / CLAUDE.md or the prompt settings changed since: chat-refresh-context picks them up and rewrites the cache once.",
                        fmt_duration(left)
                    ),
                    Tone::MetaWarn,
                );
            }
        }
        let sent = Sent {
            model: format!("{}/{}", p.model.provider, p.model.id),
            thinking: self.thinking.as_str().to_string(),
            tools: tools.iter().map(|t| declaration_key(&t.declaration())).collect(),
            prompt: prompt_hash(&p.prompt),
            retention: p.retention,
            compactions: self.compactions,
        };
        self.cache.resume(sent, &p.model, &p.last.usage, p.last.upstream.clone(), p.age);
    }

    /// Rebuild the system prompt with the next message (AGENTS.md / CLAUDE.md, prompt vars).
    pub fn refresh_context(&mut self) {
        self.system_prompt = None;
    }

    /// Account for a response's cache use; a miss is announced (chat-cache-miss-notice) and
    /// passed to the `cache-miss` hook.
    fn observe_cache(&mut self, host: &mut dyn Host, a: &AssistantMessage) -> Option<cache_watch::Hit> {
        let (sent, model) = (self.sent.clone()?, self.model.clone()?);
        let started = self.request_started.take().unwrap_or_else(Instant::now);
        let (hit, miss) = self.cache.observe(sent, &model, &a.usage, a.upstream.clone(), started, Instant::now());
        if let Some(m) = miss {
            let payload = json!({"buffer": host.chat_ref(), "cause": m.cause.word(), "text": m.cause.describe(), "tokens": m.tokens, "cost": m.cost});
            host.hook(self, "cache-miss", &payload);
            match host.var_str("chat-cache-miss-notice").as_deref().unwrap_or("inline") {
                "off" => {}
                "message" => host.notify(Level::Warn, m.notice()),
                _ => self.notice(m.notice(), Tone::MetaWarn),
            }
        }
        Some(hit)
    }

    /// The cache as scripts see it (`serpis.chat.cache`).
    pub fn cache_json(&self) -> Value {
        let now = Instant::now();
        let (state, left) = match self.cache.state(now) {
            Some(cache_watch::State::Warm(d)) => (Some("warm"), d.as_secs()),
            Some(cache_watch::State::Cold) => (Some("cold"), 0),
            None => (None, 0),
        };
        let misses: Vec<Value> = self.cache.misses.iter().map(|m| json!({"cause": m.cause.word(), "text": m.cause.describe(), "tokens": m.tokens, "cost": m.cost})).collect();
        json!({"hit_rate": self.cache.hit_rate(), "state": state, "left": left, "misses": misses})
    }

    /// The cache for clients, without the clock: {hit_rate, cached, warm_until (seconds since
    /// the epoch, while warm), misses}; [`cache_json_at`] turns it into what scripts see.
    pub fn cache_info(&self) -> Value {
        let state = self.cache.state(Instant::now());
        let until = match state {
            Some(cache_watch::State::Warm(d)) => std::time::SystemTime::now().checked_add(d).and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|t| t.as_secs_f64().round() as u64),
            _ => None,
        };
        let misses: Vec<Value> = self.cache.misses.iter().map(|m| json!({"cause": m.cause.word(), "text": m.cause.describe(), "tokens": m.tokens, "cost": m.cost})).collect();
        let invalidation = self.cache.invalidation(Instant::now()).map(|(tokens, cost)| json!({"tokens": tokens, "cost": cost}));
        json!({"hit_rate": self.cache.hit_rate(), "cached": state.is_some(), "warm_until": until, "misses": misses, "invalidation": invalidation})
    }

    /// The header's cache entry: "cache 94% · warm 41m" (None before anything was cached).
    pub fn cache_status(&self) -> Option<String> {
        let state = self.cache.state(Instant::now())?;
        let rate = self.cache.hit_rate().map(|r| format!("cache {:.0}%", r * 100.0)).unwrap_or_else(|| "cache".into());
        Some(match state {
            cache_watch::State::Warm(left) => format!("{rate} · warm {}", fmt_duration(left)),
            cache_watch::State::Cold => format!("{rate} · cold"),
        })
    }

    /// Record a custom entry in the session (created if needed) and keep it for resume.
    pub fn append_entry(&mut self, host: &mut dyn Host, kind: &str, data: Value) {
        self.ensure_session(host);
        let fields = json!({"customType": kind, "data": data});
        self.record(host, |w| w.append("custom", fields).map(|_| ()));
        self.custom_entries.push((kind.to_string(), data));
    }

    /// Whether a turn cut off by its node stopping continues when it starts again: the chat's
    /// own choice (its last `keep-going` entry, `{on = bool}`), else the host's setting.
    pub fn keep_going(&self) -> bool {
        self.custom_entries.iter().rev().find(|(k, _)| k == KEEP_GOING).and_then(|(_, d)| d["on"].as_bool()).unwrap_or(self.keep_going_default)
    }

    /// Whether the transcript stops mid-turn: the model was to answer next (a prompt, or tool
    /// results, were the last thing saved), or tools it called have no results.
    pub fn cut_off(&self) -> bool {
        match self.history.last() {
            Some(Message::User { .. } | Message::ToolResult(_)) => true,
            Some(Message::Assistant(a)) => !matches!(a.stop_reason, StopReason::Error | StopReason::Aborted) && a.tool_calls().next().is_some(),
            None => false,
        }
    }

    /// Continue a turn its node's stopping cut off (a crash, a restart, a reboot), from what was
    /// saved, as though it hadn't stopped. A tool call left without a result isn't run again
    /// (it may have done its work, or part of it): its result says it was cut off, and the model
    /// carries on from there. False when there is no turn to continue.
    pub fn continue_turn(&mut self, host: &mut dyn Host) -> bool {
        if self.running() || !self.cut_off() {
            return false;
        }
        self.answer_cut_off_calls(host);
        if self.is_claude_code() {
            return self.start_claude(host, Vec::new()).is_ok();
        }
        let Some(cfg) = self.agent_config(host) else { return false };
        self.start_agent(host, cfg, Vec::new());
        true
    }

    /// Give the last response's tool calls that have no result (their node stopped while they
    /// ran) the result that says so, which a request needs before it can go on.
    fn answer_cut_off_calls(&mut self, host: &mut dyn Host) {
        let asked = self.history.iter().rposition(|m| matches!(m, Message::Assistant(_)));
        let calls: Vec<ToolCall> = match asked.map(|i| &self.history[i]) {
            Some(Message::Assistant(a)) => a.tool_calls().cloned().collect(),
            _ => Vec::new(),
        };
        let answered: Vec<String> = asked
            .map(|i| self.history[i..].iter().filter_map(|m| if let Message::ToolResult(r) = m { Some(r.tool_call_id.clone()) } else { None }).collect())
            .unwrap_or_default();
        for c in calls.iter().filter(|c| !answered.contains(&c.id)) {
            let r = ToolResultMessage {
                tool_call_id: c.id.clone(),
                tool_name: c.name.clone(),
                content: vec![UserContent::Text { text: CUT_OFF_TOOL.into() }],
                is_error: true,
                details: Value::Null,
                timestamp: now_ms(),
            };
            if let Some(Entry::Tool { result, .. }) = self.tool_entry(&c.id) {
                *result = Some(r.clone());
            }
            let m = Message::ToolResult(r);
            let mut entry = None;
            self.record(host, |w| {
                entry = Some(w.message(&m)?);
                Ok(())
            });
            self.history.push(m);
            self.entry_ids.push(entry);
        }
    }

    /// Its usage limits as last reported, while that still holds (until the window resets).
    pub fn current_limit(&self) -> Option<serpis_proto::Limit> {
        let now = unix_now();
        self.limit.clone().filter(|l| l.status != "allowed" && l.resets_at.is_none_or(|t| t > now))
    }

    /// When it goes on by itself after the usage limit it stopped at resets (unix seconds).
    pub fn continues_at(&self) -> Option<i64> {
        self.continues.as_ref().map(|(at, _)| *at)
    }

    /// The turn just ended: refused at a usage limit, it waits for the limit to reset and goes
    /// on then (`claude-code-continue-after-limit`), else for the person.
    fn stopped_at_limit(&mut self, host: &mut dyn Host) {
        let refused = matches!(self.history.last(), Some(Message::Assistant(a)) if a.stop_reason == StopReason::Error);
        let Some(limit) = self.limit.clone().filter(|l| l.reached() && refused) else { return };
        let Some(at) = limit.resets_at else { return };
        let on = host.var_bool("claude-code-continue-after-limit", true);
        self.append_entry(host, CLAUDE_LIMIT, json!({"limit": limit, "at": self.history.len(), "continue": on}));
        if on {
            self.wait_for_reset(host, at);
        }
        host.hook(self, "usage-limit", &json!({"buffer": host.chat_ref(), "limit": limit, "continues_at": self.continues_at()}));
    }

    /// Go on by itself once the limit that resets at `at` has (`claude-code-limit-continue-delay`
    /// seconds after, as its reset can lag).
    fn wait_for_reset(&mut self, host: &mut dyn Host, at: i64) {
        self.stop_waiting_for_reset();
        let when = at + host.var_u64("claude-code-limit-continue-delay", 60) as i64;
        let cancel = CancellationToken::new();
        let (tx, wake, stop) = (self.tx.clone(), self.wake.clone(), cancel.clone());
        let wait = Duration::from_secs(when.saturating_sub(unix_now()).max(0) as u64);
        host.runtime().spawn(async move {
            tokio::select! {
                _ = tokio::time::sleep(wait) => {
                    let _ = tx.send(Done::LimitReset);
                    wake();
                }
                _ = stop.cancelled() => {}
            }
        });
        self.continues = Some((when, cancel));
        (self.wake)();
    }

    fn stop_waiting_for_reset(&mut self) {
        if let Some((_, c)) = self.continues.take() {
            c.cancel();
        }
    }

    /// Whether this chat, stopped at a usage limit, goes on by itself when it resets. Err when
    /// it isn't stopped at one.
    pub fn set_continue_after_limit(&mut self, host: &mut dyn Host, on: bool) -> Result<(), String> {
        let held = self.custom_entries.iter().rev().find(|(k, _)| k == CLAUDE_LIMIT).filter(|(_, d)| d["at"].as_u64() == Some(self.history.len() as u64)).map(|(_, d)| d.clone());
        let Some(mut d) = held.filter(|_| !self.running()) else { return Err("The chat isn't stopped at a usage limit".into()) };
        if on == self.continues.is_some() {
            return Ok(());
        }
        d["continue"] = json!(on);
        let at = d["limit"]["resets_at"].as_i64();
        self.append_entry(host, CLAUDE_LIMIT, d);
        match at {
            Some(at) if on => self.wait_for_reset(host, at),
            _ => self.stop_waiting_for_reset(),
        }
        (self.wake)();
        Ok(())
    }

    /// The limit it stopped at has reset: go on, with `claude-code-limit-continue-prompt`.
    fn go_on_after_limit(&mut self, host: &mut dyn Host) {
        if self.continues.take().is_none() || self.running() {
            return;
        }
        self.limit = None;
        let text = host.var_str("claude-code-limit-continue-prompt").unwrap_or_else(|| LIMIT_CONTINUE_PROMPT.into());
        if let Err(e) = self.submit(host, text, Vec::new(), false, None) {
            self.notice(format!("Couldn't go on after the usage limit reset: {e}"), Tone::Warning);
        }
    }

    /// Pick up what finished elsewhere (agent events, a compaction, a resume check). Call it
    /// whenever the chat's [`Wake`] fires, and once after creating or restoring it.
    pub fn poll(&mut self, host: &mut dyn Host) {
        if self.permission_mode.is_none() {
            self.default_mode = host.var_str("claude-code-permission-mode").unwrap_or_else(|| "default".into());
        }
        if let Some(p) = self.pending_resume.take() {
            self.finish_resume(host, p);
        }
        while let Ok(d) = self.rx.try_recv() {
            match d {
                Done::Agent(ev) => self.apply(ev, host),
                Done::Compacted { a, cut, tokens, reason } => self.finish_compaction(host, a, cut, tokens, reason),
                Done::LimitReset => self.go_on_after_limit(host),
            }
        }
    }

    fn ensure_text_segment(&mut self) {
        if !matches!(self.entries.last(), Some(Entry::Assistant { .. })) {
            let model = self.model.as_ref().map(|m| m.id.clone()).unwrap_or_default();
            self.entries.push(Entry::Assistant { blocks: Vec::new(), model, streaming: true, continuation: true, usage: None, error: None, cache: None });
        }
    }

    fn last_assistant(&mut self) -> Option<&mut Vec<AssistantContent>> {
        match self.entries.last() {
            Some(Entry::Assistant { .. }) => match self.entries.last_mut() {
                Some(Entry::Assistant { blocks, .. }) => Some(blocks),
                _ => None,
            },
            _ => None,
        }
    }

    fn tool_entry(&mut self, id: &str) -> Option<&mut Entry> {
        self.entries.tool_mut(id)
    }

    fn apply(&mut self, ev: AgentEvent, host: &mut dyn Host) {
        match ev {
            AgentEvent::MessageStart(Message::User { content, .. }) => {
                let (text, images) = user_parts(&content);
                let text = as_typed(text);
                // A queued (steering) message is already shown; un-queue it.
                if let Some(Entry::User { queued, .. }) = self.entries.find_mut(|e| matches!(e, Entry::User { text: t, queued: true, .. } if *t == text)) {
                    *queued = false;
                    return;
                }
                self.entries.push(Entry::User { text, queued: false, images });
            }
            AgentEvent::MessageStart(Message::Assistant(m)) => {
                // The agent sends a request right before the response starts.
                self.request_started = Some(Instant::now());
                self.entries.push(Entry::Assistant { blocks: Vec::new(), model: m.model, streaming: true, continuation: false, usage: None, error: None, cache: None });
            }
            AgentEvent::MessageStart(Message::ToolResult(_)) => {}
            AgentEvent::MessageDelta(d) => match d {
                StreamEvent::TextStart { .. } => {
                    self.ensure_text_segment();
                    if let Some(blocks) = self.last_assistant() {
                        blocks.push(AssistantContent::Text { text: String::new(), text_signature: None });
                    }
                }
                StreamEvent::TextDelta { delta, .. } => {
                    if let Some(AssistantContent::Text { text, .. }) = self.last_assistant().and_then(|b| b.last_mut()) {
                        text.push_str(&delta);
                    }
                }
                StreamEvent::ThinkingStart { .. } => {
                    self.ensure_text_segment();
                    if let Some(blocks) = self.last_assistant() {
                        blocks.push(AssistantContent::Thinking { thinking: String::new(), thinking_signature: None, redacted: false });
                    }
                }
                StreamEvent::ThinkingDelta { delta, .. } => {
                    if let Some(AssistantContent::Thinking { thinking, .. }) = self.last_assistant().and_then(|b| b.last_mut()) {
                        thinking.push_str(&delta);
                    }
                }
                StreamEvent::ToolCallStart { id, name, .. } => {
                    self.entries.push(Entry::Tool { id, name, args: Value::Object(Default::default()), partial: None, result: None, running_ms: None, elapsed_ms: None, ask: None, task: None, steps: vec![] })
                }
                StreamEvent::ToolCallDelta { arguments, .. } => {
                    let started = &self.entries.started;
                    let i = self.entries.iter().rposition(|e| matches!(e, Entry::Tool { id, result: None, .. } if !started.contains_key(id)));
                    if let Some(Entry::Tool { args, .. }) = i.and_then(|i| self.entries.get_mut(i)) {
                        *args = arguments;
                    }
                }
                StreamEvent::ToolCallEnd { tool_call, .. } => {
                    if let Some(Entry::Tool { args, .. }) = self.tool_entry(&tool_call.id) {
                        *args = tool_call.arguments;
                    }
                }
                _ => {}
            },
            AgentEvent::MessageEnd(m) => {
                if let Message::Assistant(a) = &m {
                    add_usage(&mut self.totals, &a.usage);
                    let observed = self.observe_cache(host, a).map(CacheHit::from);
                    let failed = matches!(a.stop_reason, StopReason::Error | StopReason::Aborted);
                    // Every segment of the response stops streaming; the first one gets its usage.
                    let first = self.entries.iter().rposition(|e| matches!(e, Entry::Assistant { continuation: false, .. }));
                    for i in (first.unwrap_or(0)..self.entries.len()).rev() {
                        if let Some(Entry::Assistant { streaming, usage, error, continuation, cache, .. }) = self.entries.get_mut(i) {
                            *streaming = false;
                            if !*continuation {
                                *usage = Some(a.usage.clone());
                                *cache = observed;
                                if failed {
                                    let msg = a.error_message.clone().unwrap_or_else(|| format!("{:?}", a.stop_reason));
                                    if a.stop_reason == StopReason::Error {
                                        self.last_error = Some(msg.clone());
                                    }
                                    *error = Some(msg);
                                }
                                break;
                            }
                        }
                    }
                    let payload = json!({
                        "buffer": host.chat_ref(),
                        "text": a.text(),
                        "stopReason": format!("{:?}", a.stop_reason),
                        "model": format!("{}/{}", a.provider, a.model),
                        "upstream": a.upstream,
                        "usage": usage_json(&a.usage),
                        "total": usage_json(&self.totals),
                    });
                    host.hook(self, "assistant-message", &payload);
                }
                let mut entry = None;
                self.record(host, |w| {
                    entry = Some(w.message(&m)?);
                    Ok(())
                });
                self.history.push(m);
                self.entry_ids.push(entry);
            }
            AgentEvent::ToolStart { id, name, args } => {
                if self.tool_entry(&id).is_none() {
                    self.entries.push(Entry::Tool { id: id.clone(), name, args: args.clone(), partial: None, result: None, running_ms: None, elapsed_ms: None, ask: None, task: None, steps: vec![] });
                }
                self.entries.started.insert(id.clone(), Instant::now());
                let args = self.about_task(args);
                if let Some(Entry::Tool { args: a, .. }) = self.tool_entry(&id) {
                    *a = args;
                }
            }
            AgentEvent::ToolUpdate { id, partial } => {
                if let Some(Entry::Tool { partial: p, .. }) = self.tool_entry(&id) {
                    *p = Some(partial.text_content());
                }
            }
            AgentEvent::ToolEnd { id, result, .. } => {
                let payload = json!({"buffer": host.chat_ref(), "name": result.tool_name, "isError": result.is_error});
                let elapsed = self.entries.started.remove(&id).map(|s| s.elapsed().as_millis() as u64);
                if let Some(Entry::Tool { result: r, elapsed_ms, .. }) = self.tool_entry(&id) {
                    *elapsed_ms = elapsed;
                    *r = Some(result);
                }
                host.hook(self, "tool-finished", &payload);
            }
            AgentEvent::ToolAsk { id, always, reply } => {
                let (name, args) = match serpis_proto::find_tool(&self.entries, &id) {
                    Some(Entry::Tool { name, args, .. }) => (name.clone(), args.clone()),
                    _ => (String::new(), Value::Null),
                };
                // A handler can answer for the person (an allow list, a policy).
                let payload = json!({"buffer": host.chat_ref(), "id": id, "name": name, "args": args, "always": always});
                let decided = host.hook(self, "tool-ask", &payload).into_iter().find_map(|r| match r {
                    Value::String(s) if s == "allow" => Some(serpis_agent::Answer::Allow),
                    Value::String(s) if s == "always" => Some(serpis_agent::Answer::Always),
                    Value::String(s) if s == "deny" => Some(serpis_agent::Answer::Deny(None)),
                    Value::Object(o) if o.contains_key("deny") => Some(serpis_agent::Answer::Deny(o["deny"].as_str().map(str::to_string))),
                    // A plan approved, going on in a mode: {plan = "acceptEdits"}.
                    Value::Object(o) if o["plan"].is_string() => Some(serpis_agent::Answer::Plan(o["plan"].as_str().unwrap_or("default").to_string())),
                    // The questions it asks, answered: {answers = {["Which one?"] = "B"}}.
                    Value::Object(o) if o["answers"].is_object() => Some(serpis_agent::Answer::Answers(
                        o["answers"].as_object().into_iter().flatten().map(|(q, a)| (q.clone(), a.as_str().map(str::to_string).unwrap_or_else(|| a.to_string()))).collect(),
                    )),
                    _ => None,
                });
                if let Some(a) = decided {
                    reply.send(a);
                    return;
                }
                // Not running while it waits: its clock starts when it's allowed.
                self.entries.started.remove(&id);
                if let Some(Entry::Tool { ask, .. }) = self.tool_entry(&id) {
                    *ask = Some(serpis_proto::Ask { always });
                }
                self.asks.insert(id, reply);
            }
            AgentEvent::AgentEnd { error } => {
                self.agent = None;
                self.drop_asks();
                if let Some(e) = error {
                    self.last_error = Some(e.clone());
                    self.notice(e, Tone::Error);
                }
                self.entries.update_all(
                    |e| matches!(e, Entry::User { queued: true, .. }),
                    |e| {
                        if let Entry::User { queued, .. } = e {
                            *queued = false;
                        }
                    },
                );
                host.hook(self, "agent-finished", &json!({"buffer": host.chat_ref(), "total": usage_json(&self.totals)}));
                self.stopped_at_limit(host);
                self.compact_if_needed(host);
            }
            AgentEvent::AgentStart | AgentEvent::TurnStart | AgentEvent::TurnEnd => {}
            AgentEvent::Task { tool, mut task } => {
                let mut ended = None;
                if let Some(Entry::Tool { name, task: t, steps, .. }) = self.tool_entry(&tool) {
                    // A shell task its Monitor tool started watches for events: a monitor.
                    if name == "Monitor" && task.kind == "bash" {
                        task.kind = "monitor".into();
                    }
                    // What it printed is read apart (TaskOutput): kept as it changes.
                    if let Some(old) = t.as_deref()
                        && task.output.is_none()
                    {
                        task.output = old.output.clone();
                        task.output_bytes = old.output_bytes;
                    }
                    if !task.running() {
                        ended = Some(json!({"id": tool, "task": task, "steps": steps}));
                    }
                    *t = Some(Box::new(task));
                }
                // How it ended is kept: a resumed chat shows it.
                if let Some(e) = ended {
                    self.append_entry(host, CLAUDE_TOOL, e);
                }
            }
            AgentEvent::Nested { parent, event } => self.apply_nested(&parent, *event),
            AgentEvent::PermissionMode(m) => {
                self.append_entry(host, CLAUDE_MODE, json!({"mode": m}));
                self.permission_mode = Some(m);
            }
            AgentEvent::ThinkingTokens(d) => *self.thinking_tokens.get_or_insert(0) += d,
            AgentEvent::Limit(l) => self.limit = Some(l),
            AgentEvent::Context { used, window, compacts_at } => self.claude_context = Some((used, window, compacts_at)),
            AgentEvent::Compacting(on) => self.claude_compacting = on,
            AgentEvent::Compacted { auto, before, after } => {
                self.claude_compacting = false;
                self.compactions += 1;
                let how = if auto { "Compacted by itself" } else { "Compacted" };
                let text = match after {
                    Some(a) => format!("{how}: {} → {} tokens", fmt_tokens(before), fmt_tokens(a)),
                    None => format!("{how}: {} tokens summarized", fmt_tokens(before)),
                };
                // Kept where it happened: a resumed chat shows it there.
                self.append_entry(host, CLAUDE_NOTE, json!({"text": text, "at": self.history.len()}));
                self.entries.push(Entry::Info { text, tone: Tone::Meta });
            }
            AgentEvent::TaskOutput { tool, output, bytes } => {
                let mut ended = None;
                if let Some(Entry::Tool { task: Some(t), steps, .. }) = self.tool_entry(&tool)
                    && (t.output.as_deref() != Some(output.as_str()) || t.output_bytes != bytes)
                {
                    t.output = (!output.is_empty()).then_some(output);
                    t.output_bytes = bytes;
                    // Read once more after it ended: kept with how it ended.
                    if !t.running() {
                        ended = Some(json!({"id": tool, "task": t, "steps": steps}));
                    }
                }
                if let Some(e) = ended {
                    self.append_entry(host, CLAUDE_TOOL, e);
                }
            }
            // Claude Code went on by itself (work it left running finished): a run of the
            // chat's, which it shows, records and can stop like any.
            AgentEvent::Adopted { handle, why } => {
                if self.agent.is_none() {
                    self.agent = Some(handle);
                    self.last_error = None;
                }
                let text = why.unwrap_or_else(|| "Claude Code went on by itself".into());
                self.append_entry(host, CLAUDE_NOTE, json!({"text": text, "at": self.history.len()}));
                self.entries.push(Entry::Info { text, tone: Tone::Meta });
            }
        }
    }

    /// A tool call's arguments as shown: one about a background task by its id (reading a
    /// shell's output, stopping it) also says which, as `about` (the task's description).
    fn about_task(&self, mut args: Value) -> Value {
        let id = ["task_id", "bash_id", "shell_id"].iter().find_map(|k| args[k].as_str()).map(str::to_string);
        let found = id.and_then(|id| {
            self.entries.iter().flat_map(Entry::walk).find_map(|e| match e {
                Entry::Tool { task: Some(t), .. } if t.id == id => Some(t.description.clone()),
                _ => None,
            })
        });
        if let (Some(d), Some(o)) = (found.filter(|d| !d.is_empty()), args.as_object_mut()) {
            o.insert("about".into(), json!(d));
        }
        args
    }

    /// A subagent's event, shown in the steps of its tool call `parent`: its replies (their
    /// text), its tool calls and their results.
    fn apply_nested(&mut self, parent: &str, ev: AgentEvent) {
        match ev {
            AgentEvent::MessageEnd(Message::Assistant(m)) => {
                // The next reply's thinking is counted afresh.
                self.thinking_tokens = None;
                let blocks: Vec<AssistantContent> = m.content.into_iter().filter(|b| matches!(b, AssistantContent::Text { text, .. } if !text.trim().is_empty())).collect();
                if blocks.is_empty() && m.error_message.is_none() {
                    return;
                }
                let step = Entry::Assistant { blocks, model: m.model, streaming: false, continuation: false, usage: None, error: m.error_message, cache: None };
                if let Some(Entry::Tool { steps, .. }) = self.tool_entry(parent) {
                    steps.push(step);
                }
            }
            AgentEvent::ToolStart { id, name, args } => {
                let args = self.about_task(args);
                let step = Entry::Tool { id, name, args, partial: None, result: None, running_ms: None, elapsed_ms: None, ask: None, task: None, steps: vec![] };
                if let Some(Entry::Tool { steps, .. }) = self.tool_entry(parent) {
                    steps.push(step);
                }
            }
            AgentEvent::ToolEnd { id, result, .. } => {
                if let Some(Entry::Tool { result: r, .. }) = self.tool_entry(&id) {
                    *r = Some(result);
                }
            }
            _ => {}
        }
    }

    /// Answer tool call `tool`, waiting for the person. False when it isn't waiting.
    pub fn answer(&mut self, tool: &str, decision: serpis_proto::Decision) -> bool {
        let Some(reply) = self.asks.remove(tool) else { return false };
        let a = match decision {
            serpis_proto::Decision::Allow => serpis_agent::Answer::Allow,
            serpis_proto::Decision::Always => serpis_agent::Answer::Always,
            serpis_proto::Decision::Deny { message } => serpis_agent::Answer::Deny(message),
            serpis_proto::Decision::Answers { answers } => serpis_agent::Answer::Answers(answers),
            serpis_proto::Decision::Plan { mode } => serpis_agent::Answer::Plan(mode),
        };
        let sent = reply.send(a);
        if let Some(Entry::Tool { ask, .. }) = self.tool_entry(tool) {
            *ask = None;
        }
        if sent {
            self.entries.started.insert(tool.to_string(), Instant::now());
        }
        (self.wake)();
        sent
    }

    /// Tool calls waiting for the person, oldest first.
    pub fn asking(&self) -> Vec<String> {
        self.entries.iter().flat_map(Entry::walk).filter_map(|e| if let Entry::Tool { id, ask: Some(_), .. } = e { Some(id.clone()) } else { None }).collect()
    }

    /// The run ended: nothing waits for an answer any more.
    fn drop_asks(&mut self) {
        self.asks.clear();
        for id in self.asking() {
            if let Some(Entry::Tool { ask, .. }) = self.tool_entry(&id) {
                *ask = None;
            }
        }
    }

    /// Write to the session file; the first failure is reported, later ones are silent.
    fn record(&mut self, host: &mut dyn Host, f: impl FnOnce(&mut session::SessionWriter) -> std::io::Result<()>) {
        let Some(w) = self.session.as_mut() else { return };
        let existed = w.exists();
        if let Err(e) = f(w) {
            if !self.session_failed {
                self.session_failed = true;
                host.notify(Level::Warn, format!("session {}: {e}", w.path.display()));
            }
        }
        if !existed && w.exists() {
            host.session_created(&self.cwd);
        }
    }

    /// Name the session (`session_info`), e.g. after renaming the chat.
    pub fn set_session_name(&mut self, host: &mut dyn Host, name: &str) {
        if self.session.as_ref().is_some_and(|w| w.exists()) {
            self.record(host, |w| w.set_name(name).map(|_| ()));
        }
    }

    /// Show a saved conversation and continue it: later messages append to the same file.
    pub fn restore(&mut self, host: &mut dyn Host, l: &session::Loaded) {
        self.abort();
        self.entries.clear();
        self.totals = Usage::default();
        self.history = l.messages.clone();
        self.entry_ids = l.message_ids.clone();
        // Requests keep the session's id, so they route to the provider cache they warmed.
        if !l.id.is_empty() {
            self.session_id = l.id.clone();
        }
        if l.cwd.is_dir() {
            self.cwd = l.cwd.clone();
        }
        if let Some((p, id)) = &l.model {
            match host.registry().find(p, id) {
                Some(m) => self.set_model(m),
                None => self.notice(format!("Model {p}/{id} of this session isn't available; using the current one."), Tone::Warning),
            }
        }
        if let Some(t) = l.thinking.as_deref().and_then(ThinkingLevel::parse) {
            self.thinking = self.model.as_ref().map(|m| clamp_thinking_level(m, t)).unwrap_or(t);
        }
        let mut first_prompt = None;
        let notes: Vec<(usize, String)> = l
            .entries
            .iter()
            .filter(|(k, _)| k == CLAUDE_NOTE)
            .filter_map(|(_, d)| Some((d["at"].as_u64()? as usize, d["text"].as_str()?.to_string())))
            .collect();
        for (i, m) in l.messages.iter().enumerate() {
            // Why Claude Code went on by itself, where it did.
            for (_, text) in notes.iter().filter(|(at, _)| *at == i) {
                self.entries.push(Entry::Info { text: text.clone(), tone: Tone::Meta });
            }
            match m {
                Message::User { content, .. } => {
                    let (text, images) = user_parts(content);
                    let text = as_typed(text);
                    first_prompt.get_or_insert_with(|| text.clone());
                    self.entries.push(Entry::User { text, queued: false, images });
                }
                Message::Assistant(a) => {
                    add_usage(&mut self.totals, &a.usage);
                    let failed = matches!(a.stop_reason, StopReason::Error | StopReason::Aborted);
                    self.entries.push(Entry::Assistant {
                        blocks: a.content.clone(),
                        model: a.model.clone(),
                        streaming: false,
                        continuation: false,
                        usage: Some(a.usage.clone()),
                        error: failed.then(|| a.error_message.clone().unwrap_or_else(|| format!("{:?}", a.stop_reason))),
                        cache: None,
                    });
                    for tc in a.tool_calls() {
                        self.entries.push(Entry::Tool { id: tc.id.clone(), name: tc.name.clone(), args: tc.arguments.clone(), partial: None, result: None, running_ms: None, elapsed_ms: None, ask: None, task: None, steps: vec![] });
                    }
                }
                Message::ToolResult(r) => {
                    if let Some(Entry::Tool { result, .. }) = self.tool_entry(&r.tool_call_id) {
                        *result = Some(r.clone());
                    }
                }
            }
        }
        self.title = l.name.clone().unwrap_or_else(|| title_from(first_prompt.as_deref().unwrap_or("")));
        let ago = std::fs::metadata(&l.path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).map(fmt_ago).unwrap_or_default();
        self.notice(format!("↺ Resumed session {} · {} messages · {ago}", &l.id[..l.id.len().min(8)], l.messages.len()), Tone::Meta);
        self.custom_entries = l.entries.clone();
        // What Claude Code's messages don't say: how its work on the side ended, its
        // subagents' steps, and the mode it was in.
        for (k, d) in &l.entries {
            if k == CLAUDE_TOOL
                && let (Some(id), Ok(task)) = (d["id"].as_str(), serde_json::from_value::<serpis_proto::Task>(d["task"].clone()))
            {
                let saved: Vec<Entry> = serde_json::from_value(d["steps"].clone()).unwrap_or_default();
                if let Some(Entry::Tool { task: t, steps, .. }) = self.tool_entry(id) {
                    *t = Some(Box::new(task));
                    *steps = saved;
                }
            }
        }
        self.permission_mode = l.entries.iter().rev().find(|(k, _)| k == CLAUDE_MODE).and_then(|(_, d)| d["mode"].as_str()).map(str::to_string);
        // Stopped at a usage limit, with nothing sent since: it still goes on when that resets
        // (at once if it already has).
        self.stop_waiting_for_reset();
        self.limit = None;
        if let Some((_, d)) = l.entries.iter().rev().find(|(k, _)| k == CLAUDE_LIMIT)
            && d["at"].as_u64() == Some(l.messages.len() as u64)
            && let Ok(limit) = serde_json::from_value::<serpis_proto::Limit>(d["limit"].clone())
        {
            if d["continue"].as_bool().unwrap_or(false)
                && let Some(at) = limit.resets_at
            {
                self.wait_for_reset(host, at);
            }
            self.limit = Some(limit);
        }
        self.system_prompt = None;
        self.cache = cache_watch::CacheWatch::from_usages(l.messages.iter().filter_map(|m| match m {
            Message::Assistant(a) => Some(&a.usage),
            _ => None,
        }));
        self.resume_system_prompt(host, l);
        self.session = Some(session::SessionWriter::resume(l));
        self.session_failed = false;
        self.last_error = None;
        (self.wake)();
    }

    /// What the running agent is doing right now.
    pub fn current_step(&self) -> String {
        // A subagent's prompt waits as much as the agent's own (those are found below).
        let own = self.entries.iter().any(|e| matches!(e, Entry::Tool { ask: Some(_), .. }));
        if !own && let Some(Entry::Tool { name, args, .. }) = self.entries.iter().flat_map(Entry::walk).find(|e| matches!(e, Entry::Tool { ask: Some(_), .. })) {
            let s = tool_summary(name, args);
            return if s.is_empty() { format!("waiting for you: {name}") } else { format!("waiting for you: {name} {s}") };
        }
        for e in self.entries.iter().rev() {
            match e {
                Entry::Tool { name, args, ask: Some(_), .. } if name == "AskUserQuestion" => {
                    return format!("waiting for your answer: {}", tool_summary(name, args));
                }
                Entry::Tool { name, ask: Some(_), .. } if name == "ExitPlanMode" => return "waiting for you to approve the plan".into(),
                Entry::Tool { name, args, ask: Some(_), .. } => {
                    let s = tool_summary(name, args);
                    return if s.is_empty() { format!("waiting for you: {name}") } else { format!("waiting for you: {name} {s}") };
                }
                Entry::Tool { name, args, result: None, .. } => {
                    let s = tool_summary(name, args);
                    return if s.is_empty() { name.clone() } else { format!("{name} {s}") };
                }
                Entry::Assistant { blocks, streaming: true, .. } => {
                    return match blocks.last() {
                        Some(AssistantContent::Thinking { .. }) | None => "thinking".into(),
                        _ => "writing".into(),
                    };
                }
                Entry::Tool { .. } | Entry::Assistant { .. } => break,
                _ => {}
            }
        }
        "working".into()
    }

    /// What changed in the transcript since the last call (None: nothing did).
    pub fn take_splice(&mut self) -> Option<Splice> {
        self.entries.take_changes()
    }

    /// The whole transcript, for a client that just attached.
    pub fn full_splice(&self) -> Splice {
        self.entries.all()
    }

    /// The chat around its transcript, as clients show it.
    pub fn state(&self) -> ChatState {
        ChatState {
            title: self.title.clone(),
            model: self.model.as_ref().map(|m| format!("{}/{}", m.provider, m.id)),
            thinking: self.thinking.as_str().to_string(),
            running: self.running(),
            compacting: self.compacting(),
            step: self.running().then(|| self.current_step()),
            cwd: self.cwd.to_string_lossy().into_owned(),
            session_id: self.session_id.clone(),
            usage: self.totals.clone(),
            context: self.context(),
            compacts_at: self.compacts_at(),
            compactions: self.compactions,
            cache: self.cache_status(),
            last_error: self.last_error.clone(),
            session: self.session.as_ref().filter(|w| w.exists()).map(|w| w.path.to_string_lossy().into_owned()),
            custom: self.custom_entries.clone(),
            cache_info: self.cache_info(),
            keep_going: self.keep_going(),
            permission_mode: self.is_claude_code().then(|| self.permission_mode.clone().unwrap_or_else(|| self.default_mode.clone())),
            thinking_tokens: self.thinking_tokens.filter(|_| self.running()),
            limit: self.current_limit(),
            continues_at: self.continues.as_ref().map(|(at, _)| *at),
        }
    }
}

/// The custom entry a stop at a usage limit is kept in: `{limit, at = messages before it,
/// continue = bool}` (a resumed chat still goes on when it resets).
pub const CLAUDE_LIMIT: &str = "claude-code-limit";

/// A user message as typed: `/skill:name args` for one a skill was sent in.
fn as_typed(text: String) -> String {
    serpis_agent::skills::as_typed(&text).unwrap_or(text)
}

/// What a chat stopped at a usage limit says when it goes on after it resets.
pub const LIMIT_CONTINUE_PROMPT: &str = "Your usage limit has reset. Continue where you left off.";

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// Claude Code's permission modes.
pub const PERMISSION_MODES: [&str; 6] = ["default", "acceptEdits", "plan", "auto", "dontAsk", "bypassPermissions"];

/// The custom entry a chat's own [`Chat::keep_going`] choice is kept in: `{on = bool}`.
pub const KEEP_GOING: &str = "keep-going";

/// The custom entries a Claude Code chat keeps what its messages don't say in: its permission
/// mode (`{mode}`), how work a tool call started ended and a subagent's steps (`{id, task,
/// steps}`), and why it took a turn of its own (`{text, at}`: before history message `at`).
pub const CLAUDE_MODE: &str = "claude-code-mode";
pub const CLAUDE_TOOL: &str = "claude-code-tool";
pub const CLAUDE_NOTE: &str = "claude-code-note";

/// What a tool call cut off by its node stopping gets as its result.
pub const CUT_OFF_TOOL: &str = "Not run to the end: the machine's serpis-node stopped while this tool ran (a crash, a restart or a reboot), and it was not run again. It may have done some or all of its work: check before running it again.";

/// [`Chat::cache_json`] from a client's copy of [`Chat::cache_info`].
pub fn cache_json_at(info: &Value, now: std::time::SystemTime) -> Value {
    let now = now.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let (state, left) = match (info["cached"].as_bool().unwrap_or(false), info["warm_until"].as_u64()) {
        (false, _) => (None, 0),
        (true, Some(until)) if until > now => (Some("warm"), until - now),
        (true, _) => (Some("cold"), 0),
    };
    json!({"hit_rate": info["hit_rate"], "state": state, "left": left, "misses": info.get("misses").cloned().unwrap_or(json!([]))})
}

/// The text and images of a user message.
pub fn user_parts(content: &[UserContent]) -> (String, Vec<Image>) {
    let mut text = Vec::new();
    let mut images = Vec::new();
    for c in content {
        match c {
            UserContent::Text { text: t } => text.push(t.clone()),
            UserContent::Image { data, mime_type } => images.push(Image { mime: mime_type.clone(), data: data.clone() }),
        }
    }
    (text.join("\n"), images)
}

/// A chat title from its first prompt: the first line, cut at a word near 48 characters.
pub fn title_from(text: &str) -> String {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty()).unwrap_or("");
    if line.chars().count() <= 48 {
        return line.to_string();
    }
    let cut: String = line.chars().take(48).collect();
    let cut = match cut.rfind(' ') {
        Some(i) if i > 24 => &cut[..i],
        _ => &cut,
    };
    format!("{}…", cut.trim_end())
}

pub fn add_usage(total: &mut Usage, u: &Usage) {
    total.input += u.input;
    total.output += u.output;
    total.cache_read += u.cache_read;
    total.cache_write += u.cache_write;
    total.total_tokens += u.total_tokens;
    total.cost.total += u.cost.total;
}

pub fn usage_json(u: &Usage) -> Value {
    json!({
        "input": u.input,
        "output": u.output,
        "cache_read": u.cache_read,
        "cache_write": u.cache_write,
        "total_tokens": u.total_tokens,
        "cost": u.cost.total,
    })
}

/// `now`, `5m`, `3h`, `2d`.
pub fn fmt_age(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

/// How long ago, in words: `just now`, `5m ago`.
pub fn fmt_ago(d: Duration) -> String {
    match fmt_age(d) {
        a if a == "now" => "just now".into(),
        a => format!("{a} ago"),
    }
}

fn arg<'a>(args: &'a Value, k: &str) -> &'a str {
    args.get(k).and_then(Value::as_str).unwrap_or("")
}

/// A tool call in a few words: `cargo test`, `src/main.rs`.
pub fn tool_summary(name: &str, args: &Value) -> String {
    // Claude Code's tools for its background shells and tasks: which one (by what it runs,
    // when known), and whether it waits for it.
    if name == "Monitor" {
        return args["description"].as_str().or(args["command"].as_str()).unwrap_or("").lines().next().unwrap_or("").to_string();
    }
    if matches!(name, "TaskOutput" | "BashOutput" | "TaskStop" | "KillShell" | "KillBash") {
        let which = ["about", "task_id", "bash_id", "shell_id"].iter().find_map(|k| args[k].as_str()).unwrap_or("").to_string();
        let waits = name == "TaskOutput" && args["block"].as_bool() != Some(false);
        return if waits { format!("{which} · waiting") } else { which };
    }
    match name {
        "bash" => arg(args, "command").lines().next().unwrap_or("").to_string(),
        "serpis_lua" => arg(args, "code").lines().next().unwrap_or("").to_string(),
        "serpis_api" => match (arg(args, "topic"), arg(args, "filter")) {
            ("", _) => "guide".into(),
            (t, "") => t.to_string(),
            (t, f) => format!("{t} · {f}"),
        },
        "read" => {
            let mut s = arg(args, "path").to_string();
            if let Some(o) = args.get("offset").and_then(Value::as_u64) {
                s.push_str(&format!(":{o}"));
            }
            if let Some(l) = args.get("limit").and_then(Value::as_u64) {
                s.push_str(&format!(" (+{l})"));
            }
            s
        }
        "edit" | "write" | "ls" => arg(args, "path").to_string(),
        "grep" => format!("/{}/ {}", arg(args, "pattern"), arg(args, "path")),
        "find" => format!("{} {}", arg(args, "pattern"), arg(args, "path")),
        "web_search" => {
            let mut q: Vec<String> = Vec::new();
            if !arg(args, "query").is_empty() {
                q.push(arg(args, "query").to_string());
            }
            if let Some(a) = args.get("queries").and_then(Value::as_array) {
                q.extend(a.iter().filter_map(Value::as_str).map(str::to_string));
            }
            q.join(" | ")
        }
        // Claude Code's own tools.
        "Task" | "Agent" => match arg(args, "subagent_type") {
            "" => arg(args, "description").to_string(),
            kind => format!("{} ({kind})", arg(args, "description")),
        },
        "WebFetch" => arg(args, "url").to_string(),
        "TodoWrite" => {
            let todos = args.get("todos").and_then(Value::as_array).cloned().unwrap_or_default();
            let done = todos.iter().filter(|t| t["status"] == "completed").count();
            let now = todos.iter().find(|t| t["status"] == "in_progress").and_then(|t| t["activeForm"].as_str().or(t["content"].as_str()));
            match now {
                Some(n) => format!("{done}/{} done · {n}", todos.len()),
                None => format!("{done}/{} done", todos.len()),
            }
        }
        "NotebookEdit" => arg(args, "notebook_path").to_string(),
        "Skill" => arg(args, "skill").to_string(),
        "ExitPlanMode" => "plan ready".into(),
        "AskUserQuestion" => {
            let qs = args["questions"].as_array().map(Vec::as_slice).unwrap_or_default();
            let first = qs.first().and_then(|q| q["question"].as_str()).unwrap_or("");
            if qs.len() > 1 { format!("{first} (+{} more)", qs.len() - 1) } else { first.to_string() }
        }
        _ => {
            let s = args.to_string();
            if s == "{}" { String::new() } else { s }
        }
    }
}

fn prompt_hash(prompt: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    prompt.hash(&mut h);
    h.finish()
}

/// Whether to warn about `action` throwing away a cache worth `cost` dollars (cache-warn,
/// cache-warn-actions, cache-warn-threshold).
pub fn cache_warning_applies(var: &dyn Fn(&str) -> Option<Value>, action: &str, cost: f64) -> bool {
    if var("cache-warn").as_ref().and_then(Value::as_str) == Some("off") {
        return false;
    }
    let listed = var("cache-warn-actions").and_then(|v| v.as_array().cloned()).is_none_or(|a| a.iter().any(|x| x.as_str() == Some(action)));
    let threshold = var("cache-warn-threshold").and_then(|v| v.as_f64()).unwrap_or(0.10);
    listed && cost >= threshold
}

/// `agent-cache-retention` for `provider`: a level, or {"default": level, "<provider>": level}.
pub fn cache_retention(host: &dyn Host, provider: &str) -> CacheRetention {
    let v = host.var("agent-cache-retention");
    let level = match &v {
        Some(Value::Object(o)) => o.get(provider).or_else(|| o.get("default")).and_then(Value::as_str),
        Some(v) => v.as_str(),
        None => None,
    };
    match level {
        Some("none") => CacheRetention::None,
        Some("short") => CacheRetention::Short,
        _ => CacheRetention::Long,
    }
}
