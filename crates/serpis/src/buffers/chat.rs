//! Agent chat buffer.
//!
//! The chat itself (conversation, agent, session file, cache bookkeeping, compaction) runs in a
//! node, this machine's or another's ([`crate::remote`]); this buffer shows its transcript and
//! sends it what you type. The transcript arrives as `serpis_proto` splices and the rest as its
//! `ChatState`, the same for every window (or phone) that shows the chat.
//!
//! The transcript is generated styled text (Emacs-style faces and blocks): messages, thinking,
//! foldable tool cards and diff views. The prompt is an editable area pinned to the bottom.
//! Both form one composite [`TextSurface`] (transcript lines first, then prompt lines), so
//! motions, vim, search and yank move seamlessly between them.

use std::ops::Range;
use std::time::{Duration, Instant};

use egui::{Color32, Rect, Stroke, Vec2};
use serde_json::Value;
use serpis_proto::wire::Request;
use serpis_ai::*;
use serpis_proto::{CacheHit, Entry, Tone};

use crate::buffer::{Activity, ActivityState, Buffer, Modeline};
use crate::editor::{Editor, MainSender};
use crate::impl_any;
use crate::text::{Block, CursorShape, DocView, LineSource, Pos, StyledLine, TextArea, TextSurface, ViewOptions, char_len};
use crate::theme::Face;

pub use serpis_chat::{CompactReason, fmt_tokens};

/// How this window shows a transcript entry: how it is folded, its images decoded, and when a
/// running tool started (by this clock).
#[derive(Debug, Clone, Default)]
struct View {
    /// Tool output folded by hand (None: the default for the tool).
    expanded: Option<bool>,
    /// Thinking folded by hand: Some(true) all of it, Some(false) only its header; None follows
    /// chat-show-thinking.
    thinking: Option<bool>,
    images: Vec<crate::images::ImageRef>,
    started: Option<Instant>,
}

impl View {
    fn of(e: &Entry, now: Instant) -> View {
        match e {
            Entry::User { images, .. } => View { images: images.iter().filter_map(|i| crate::images::from_base64(&i.data)).collect(), ..View::default() },
            Entry::Tool { running_ms: Some(ms), .. } => View { started: now.checked_sub(Duration::from_millis(*ms)), ..View::default() },
            _ => View::default(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Transcript,
    Prompt,
}

/// The chat a buffer shows: one running in a machine's node, this machine's
/// ([`crate::remote::LOCAL`]) or another's.
pub struct NodeChat {
    /// The machine: [`crate::remote::LOCAL`], or one of `remote-hosts` / rayfish.
    pub machine: String,
    /// The chat's id on its node (its session id).
    pub chat: String,
    /// The chat as its node last said.
    pub state: serpis_proto::ChatState,
    /// Whether the connection to the node is up.
    pub online: bool,
    main: MainSender,
    /// Requests waiting for the next tick, where the editor is free to send them.
    outbox: Vec<(Request, Option<Answer>)>,
}

type Answer = Box<dyn FnOnce(&mut Editor, Result<serpis_proto::wire::Response, String>)>;

impl NodeChat {
    fn new(machine: String, chat: String, state: serpis_proto::ChatState, main: MainSender) -> NodeChat {
        NodeChat { machine, chat, state, online: true, main, outbox: Vec::new() }
    }

    /// Send a request about this chat to its node, at the next tick. Everything a chat buffer
    /// asks goes this way, so its requests reach the node in the order they were made.
    fn post(&mut self, request: Request) {
        self.outbox.push((request, None));
        self.main.repaint();
    }

    /// [`NodeChat::post`], and `done` gets the answer.
    fn post_then(&mut self, request: Request, done: impl FnOnce(&mut Editor, Result<serpis_proto::wire::Response, String>) + 'static) {
        self.outbox.push((request, Some(Box::new(done))));
        self.main.repaint();
    }

    /// Send what was posted.
    fn flush(&mut self, ed: &mut Editor) {
        for (request, done) in self.outbox.drain(..) {
            match done {
                Some(done) => crate::remote::request(ed, &self.machine, request, done),
                None => crate::remote::send(ed, &self.machine, request),
            }
        }
    }

    /// Whether it runs on this machine.
    pub fn local(&self) -> bool {
        self.machine == crate::remote::LOCAL
    }
}

pub struct ChatBuffer {
    /// The chat this buffer shows, and where it runs.
    pub node: NodeChat,
    /// The transcript as last received, and how each entry is shown here.
    entries: Vec<Entry>,
    views: Vec<View>,
    pub prompt: TextArea,
    lines: Vec<StyledLine>,
    line_entry: Vec<Option<usize>>,
    /// Transcript lines that belong to thinking blocks (click or TAB folds them).
    thinking_lines: Vec<usize>,
    /// chat-show-thinking as last drawn; changing it unfolds what was folded by hand.
    show_thinking: bool,
    dirty: bool,
    pub view: DocView,
    pub focus: Focus,
    /// When the current run started.
    started: Option<Instant>,
    prompt_history: Vec<String>,
    history_index: Option<usize>,
    /// Width of the transcript's content column (tables are laid out for it).
    text_w: f32,
    /// Theme and font state the transcript was rendered with (cards carry its colors).
    style_key: u64,
    /// Where the composer's controls were drawn last frame (tests click them).
    pub composer_hits: Vec<(&'static str, Rect)>,
    /// Images waiting in the composer, sent with the next message.
    pub attachments: Vec<Attachment>,
    /// chat-cache-indicator and chat-cache-status, as last read.
    show_cache: (bool, bool),
}

const PREVIEW_LINES: usize = 10;

impl ChatBuffer {
    /// A chat buffer. `params`: `machine` (default: this one), and what it shows: `chat`, a chat
    /// running there (attached by whoever made the buffer); `session`, a saved session there to
    /// continue (its `chat` id when known); else a new chat in `cwd` (default: the editor's
    /// directory).
    pub fn new(ed: &mut Editor, params: &Value) -> Self {
        let mut view = DocView::default();
        view.follow_tail = true;
        view.padding = egui::vec2(18.0, 12.0);
        let mut prompt = TextArea::new("");
        prompt.view.padding = egui::vec2(14.0, 10.0);
        let machine = params.get("machine").and_then(Value::as_str).unwrap_or(crate::remote::LOCAL).to_string();
        let cwd = params.get("cwd").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| ed.cwd.to_string_lossy().into_owned());
        let given = params.get("chat").and_then(Value::as_str).map(str::to_string);
        let session = params.get("session").and_then(Value::as_str).map(str::to_string);
        // The client names a new chat, so what is typed into it can go before the node answers.
        let chat = given.clone().unwrap_or_else(|| uuid::Uuid::now_v7().to_string());
        let local = machine == crate::remote::LOCAL;
        let state = serpis_proto::ChatState {
            cwd: cwd.clone(),
            session_id: chat.clone(),
            // What the node will say, shown until it does.
            model: if local { default_model(ed).map(|m| format!("{}/{}", m.provider, m.id)) } else { None },
            thinking: default_thinking(ed).as_str().to_string(),
            session: session.clone(),
            ..Default::default()
        };
        let c = ChatBuffer {
            node: NodeChat::new(machine.clone(), chat.clone(), state, ed.main_sender()),
            entries: Vec::new(),
            views: Vec::new(),
            prompt,
            lines: Vec::new(),
            line_entry: Vec::new(),
            thinking_lines: Vec::new(),
            show_thinking: true,
            dirty: true,
            view,
            focus: Focus::Prompt,
            started: None,
            prompt_history: Vec::new(),
            history_index: None,
            text_w: 0.0,
            style_key: 0,
            composer_hits: Vec::new(),
            attachments: Vec::new(),
            show_cache: (true, true),
        };
        match (session, given) {
            (Some(path), _) => crate::remote::resume_into(ed, &machine, &path, &chat),
            (None, Some(_)) => {}
            (None, None) => {
                let model = params.get("model").and_then(Value::as_str).map(str::to_string).or_else(|| if local { ed.var_str("agent-model").filter(|m| !m.is_empty()) } else { None });
                let thinking = if local { Some(default_thinking(ed).as_str().to_string()) } else { None };
                crate::remote::start_chat(ed, &machine, &chat, &cwd, model, thinking);
            }
        }
        c
    }

    /// Show `chat` on `machine` from now on, instead of what this buffer showed.
    pub fn become_remote(&mut self, ed: &mut Editor, machine: &str, chat: &str) {
        // What was asked of the chat it showed goes first.
        self.node.flush(ed);
        let cwd = std::mem::take(&mut self.node.state.cwd);
        self.node = NodeChat::new(machine.to_string(), chat.to_string(), serpis_proto::ChatState { cwd, ..Default::default() }, ed.main_sender());
        self.entries.clear();
        self.views.clear();
        self.dirty = true;
        if let Some(id) = ed.this_buffer() {
            self.publish(ed, id);
        }
    }

    /// The session file this chat is saved in, on its machine.
    pub fn session_path(&self) -> Option<std::path::PathBuf> {
        self.node.state.session.as_ref().map(std::path::PathBuf::from)
    }

    /// No messages yet: the chat can still move to another machine as it is.
    pub fn is_empty(&self) -> bool {
        !self.entries.iter().any(|e| matches!(e, Entry::User { .. })) && !self.node.state.running
    }

    pub fn chat_id(&self) -> String {
        self.node.chat.clone()
    }

    pub fn set_online(&mut self, online: bool) {
        self.node.online = online;
        self.dirty = true;
    }

    /// Something the chat's node sent about it.
    pub fn node_event(&mut self, ed: &mut Editor, ev: serpis_proto::wire::Event) {
        use serpis_proto::wire::{Event, NoticeLevel};
        let r = &mut self.node;
        r.online = true;
        let at = if r.local() { String::new() } else { format!("{}: ", r.machine) };
        match ev {
            Event::Splice { splice, .. } => {
                let users = |c: &ChatBuffer| c.entries.iter().filter(|e| matches!(e, Entry::User { .. })).count();
                let before = users(self);
                self.apply_splice(splice);
                // A message was sent (and written to the session): the session list changes.
                if self.node.local() && self.node.state.session.is_some() && users(self) > before {
                    crate::session_ui::rescan(ed);
                }
                // Up and down recall what was sent in this chat, wherever it was typed.
                if self.prompt_history.is_empty() {
                    self.prompt_history = self.entries.iter().filter_map(|e| if let Entry::User { text, .. } = e { Some(text.clone()) } else { None }).collect();
                }
            }
            Event::State { state, .. } => {
                let (was, had_session) = (r.state.running, r.state.session.is_some());
                r.state = state;
                match (was, r.state.running) {
                    (false, true) => self.started = Some(Instant::now()),
                    (true, false) => self.started = None,
                    _ => {}
                }
                // Its first message wrote its session: it is listed with this machine's, and the
                // list is read again as runs start and end (what it says changes).
                if r.local() && r.state.session.is_some() {
                    if !had_session {
                        session_created(ed, std::path::Path::new(&r.state.cwd));
                    } else if was != r.state.running {
                        crate::session_ui::rescan(ed);
                    }
                }
                self.dirty = true;
            }
            Event::Notice { level, text, .. } => match level {
                NoticeLevel::Info => ed.message(format!("{at}{text}")),
                NoticeLevel::Warn => ed.warn(format!("{at}{text}")),
                NoticeLevel::Error => ed.error(format!("{at}{text}")),
            },
            Event::Closed { .. } => {
                r.online = false;
                let m = if r.local() { "this machine".to_string() } else { r.machine.clone() };
                ed.warn(format!("The chat was closed on {m}"));
            }
            Event::Message { .. } | Event::ToolCall { .. } | Event::TerminalOutput { .. } | Event::TerminalExited { .. } | Event::PairRequested { .. } => {}
        }
        if let Some(id) = ed.this_buffer() {
            self.publish(ed, id);
        }
    }

    /// The chat as the header, composer and sidebar show it.
    pub fn state_now(&self) -> serpis_proto::ChatState {
        self.node.state.clone()
    }

    /// The model's id without its provider, or None.
    fn model_short(&self) -> Option<String> {
        self.node.state.model.as_deref().map(|m| short_model(m.split_once('/').map_or(m, |(_, id)| id)).to_string())
    }

    /// Show a transcript splice. Entries that stay the same thing (the same tool call, the
    /// same response) keep how they were folded here.
    pub fn apply_splice(&mut self, s: serpis_proto::Splice) {
        let from = s.from.min(self.entries.len());
        let now = Instant::now();
        let mut views: Vec<View> = s.entries.iter().map(|e| View::of(e, now)).collect();
        for (i, v) in views.iter_mut().enumerate() {
            let (Some(old), Some(was)) = (self.entries.get(from + i), self.views.get(from + i)) else { break };
            match (&s.entries[i], old) {
                (Entry::Tool { id, .. }, Entry::Tool { id: oid, .. }) if id == oid => {
                    v.expanded = was.expanded;
                    // The same running call keeps its clock (no jitter from splice to splice).
                    if v.started.is_some() && was.started.is_some() {
                        v.started = was.started;
                    }
                }
                (Entry::Assistant { .. }, Entry::Assistant { .. }) => v.thinking = was.thinking,
                _ => {}
            }
        }
        self.entries.truncate(from);
        self.views.truncate(from);
        self.entries.extend(s.entries);
        self.views.extend(views);
        self.dirty = true;
    }

    /// The transcript as this window has it, for comparing with the chat's.
    pub fn shown(&self) -> &[Entry] {
        &self.entries
    }

    pub fn running(&self) -> bool {
        self.node.state.running
    }

    pub fn abort(&mut self) {
        self.node.post(Request::Abort { chat: self.node.chat.clone() });
    }

    /// The work running on the side (background commands, agents), the latest first.
    pub fn background_tasks(&self) -> Vec<serpis_proto::Task> {
        self.entries.iter().rev().flat_map(Entry::walk).filter_map(|e| if let Entry::Tool { task: Some(t), .. } = e { t.running().then(|| (**t).clone()) } else { None }).collect()
    }

    /// Ask the agent to stop task `task` (one of [`Self::background_tasks`]).
    pub fn stop_task(&mut self, task: &str) {
        self.node.post(Request::StopTask { chat: self.node.chat.clone(), task: task.to_string() });
    }

    /// The first tool call waiting for the person: (id, name, args, what "always" allows).
    pub fn waiting(&self) -> Option<(String, String, Value, Option<String>)> {
        self.entries.iter().flat_map(Entry::walk).find_map(|e| match e {
            Entry::Tool { id, name, args, ask: Some(a), .. } => Some((id.clone(), name.clone(), args.clone(), a.always.clone())),
            _ => None,
        })
    }

    /// Answer the first tool call waiting for the person; false when none waits.
    pub fn answer(&mut self, decision: serpis_proto::Decision) -> bool {
        let Some((tool, ..)) = self.waiting() else { return false };
        self.node.post(Request::Answer { chat: self.node.chat.clone(), tool: tool.clone(), decision });
        // Shown at once; the node's splice confirms it.
        if let Some(Entry::Tool { ask, .. }) = serpis_proto::find_tool_mut(&mut self.entries, &tool) {
            *ask = None;
        }
        self.dirty = true;
        true
    }

    pub fn set_model(&mut self, m: Model) {
        self.set_model_spec(&format!("{}/{}", m.provider, m.id));
    }

    /// Use model `spec` (`provider/id`, one of the chat's machine).
    pub fn set_model_spec(&mut self, spec: &str) {
        self.node.post(Request::SetModel { chat: self.node.chat.clone(), model: spec.to_string() });
        // Shown at once; the node says so too, or why not.
        self.node.state.model = Some(spec.to_string());
        self.dirty = true;
    }

    /// The thinking levels its model supports (all, when this window doesn't know the model).
    pub fn thinking_levels(&self, ed: &Editor) -> Vec<ThinkingLevel> {
        self.node.state.model.as_deref().and_then(|m| ed.find_model(m)).map(|m| serpis_ai::supported_thinking_levels(&m)).unwrap_or_else(|| ThinkingLevel::ALL.to_vec())
    }

    pub fn cycle_thinking(&mut self, ed: &Editor) -> ThinkingLevel {
        let levels = self.thinking_levels(ed);
        let now = self.thinking_level();
        let next = levels[(levels.iter().position(|l| *l == now).map_or(0, |p| p + 1)) % levels.len()];
        self.set_thinking(next);
        next
    }

    pub fn set_thinking(&mut self, level: ThinkingLevel) {
        self.node.post(Request::SetThinking { chat: self.node.chat.clone(), level: level.as_str().into() });
        self.node.state.thinking = level.as_str().to_string();
        self.dirty = true;
    }

    /// The thinking level, as shown.
    pub fn thinking_level(&self) -> ThinkingLevel {
        ThinkingLevel::parse(&self.node.state.thinking).unwrap_or(ThinkingLevel::Medium)
    }

    /// Send the prompt: start a run, or steer the running one.
    pub fn send(&mut self, ed: &mut Editor, follow_up: bool) {
        let text = self.prompt.text().trim().to_string();
        if text.is_empty() && self.attachments.is_empty() {
            return;
        }
        self.prompt.clear();
        self.prompt_history.push(text.clone());
        self.history_index = None;
        self.submit(ed, text, follow_up);
    }

    /// Submit text as a user message (also used by Lua `serpis.agent.prompt`), with the images
    /// waiting in the composer. Refused (a `before-prompt` hook's `{block = "reason"}`, no model,
    /// a compaction running), the reason is shown and the text and images come back to the
    /// composer, to edit or send later.
    pub fn submit(&mut self, ed: &mut Editor, text: String, follow_up: bool) {
        let attachments = std::mem::take(&mut self.attachments);
        let images: Vec<serpis_proto::Image> = attachments.iter().map(|a| serpis_proto::Image { mime: a.encoded.mime.clone(), data: a.encoded.data.to_string() }).collect();
        // A lost reply is safe to retry with its id.
        let request_id = Some(uuid::Uuid::now_v7().to_string());
        let request = Request::Prompt { chat: self.node.chat.clone(), text: text.clone(), images, follow_up, request_id };
        self.view.follow_tail = true;
        let (machine, chat, local) = (self.node.machine.clone(), self.node.chat.clone(), self.node.local());
        self.node.post_then(request, move |ed, r| {
            let Err(why) = r else { return };
            ed.warn(if local { why } else { format!("{machine}: {why}") });
            if let Some(id) = crate::remote::chat_buffer(ed, &machine, &chat) {
                ed.with_buffer_as::<ChatBuffer, _>(id, move |c, _| {
                    if c.prompt.doc.is_empty() {
                        c.prompt.set_text(&text);
                        c.prompt.move_to_end();
                    }
                    if c.attachments.is_empty() {
                        c.attachments = attachments;
                    }
                });
            }
        });
        // Sent now, after what was asked before it.
        self.node.flush(ed);
    }

    /// Replace the messages before a cut point with a summary (chat-compact).
    pub fn compact(&mut self, instructions: Option<String>) {
        self.node.post(Request::Compact { chat: self.node.chat.clone(), instructions });
    }

    pub fn context(&self) -> (Option<u64>, u64) {
        self.node.state.context
    }

    /// `↑12k ↓3.4k R1.2M W40k $1.234 45.2%/200k`, as pi's footer: the session's input, output,
    /// cache reads and writes, cost, then context used of the window.
    pub fn usage_label(&self) -> String {
        let u = &self.node.state.usage;
        let mut parts = vec![format!("↑{}", fmt_tokens(u.input)), format!("↓{}", fmt_tokens(u.output))];
        if u.cache_read > 0 {
            parts.push(format!("R{}", fmt_tokens(u.cache_read)));
        }
        if u.cache_write > 0 {
            parts.push(format!("W{}", fmt_tokens(u.cache_write)));
        }
        parts.push(format!("${:.3}", u.cost.total));
        let (used, window) = self.context();
        if window > 0 {
            parts.push(format!("{:.1}%/{}", used.unwrap_or(0) as f64 * 100.0 / window as f64, fmt_tokens(window)));
        }
        parts.join(" ")
    }

    /// The transcript as shown, brought up to date first when it changed since it was drawn (a
    /// chat that isn't on screen).
    pub fn transcript_text(&mut self, ed: &Editor) -> String {
        if self.dirty {
            self.rebuild(ed);
        }
        self.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
    }

    /// Rebuild the system prompt with the next message (AGENTS.md / CLAUDE.md, prompt vars).
    pub fn refresh_context(&mut self) {
        self.node.post(Request::RefreshContext { chat: self.node.chat.clone() });
    }

    /// The prompt cache: {hit_rate, state = "warm"|"cold"|nil, left (seconds), misses}.
    pub fn cache_json(&self) -> Value {
        serpis_chat::cache_json_at(&self.node.state.cache_info, std::time::SystemTime::now())
    }

    /// What throwing the warm cache away would cost on the next message: (tokens, dollars).
    pub fn cache_invalidation(&self) -> Option<(u64, f64)> {
        let i = self.node.state.cache_info.get("invalidation")?;
        Some((i["tokens"].as_u64()?, i["cost"].as_f64()?))
    }

    /// Attach an image file to the next message.
    pub fn attach_file(&mut self, path: &std::path::Path) -> anyhow::Result<()> {
        let (image, encoded) = crate::images::load_file(path).ok_or_else(|| anyhow::anyhow!("{} isn't an image serpis can read", path.display()))?;
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        self.attachments.push(Attachment { image, encoded, name });
        self.focus = Focus::Prompt;
        Ok(())
    }

    /// This chat as seen from outside (`Editor::chats`, `serpis.chat`).
    pub fn info(&self) -> crate::chat_api::ChatInfo {
        let s = &self.node.state;
        crate::chat_api::ChatInfo {
            model: s.model.clone(),
            thinking: s.thinking.clone(),
            running: s.running,
            compacting: s.compacting,
            usage: s.usage.clone(),
            title: s.title.clone(),
            cwd: s.cwd.clone(),
            session: s.session.clone(),
            session_id: self.node.chat.clone(),
            entries: s.custom.clone(),
            context: s.context,
            cache: self.cache_json(),
            machine: (!self.node.local()).then(|| self.node.machine.clone()),
            node: (self.node.machine.clone(), self.node.chat.clone()),
            keep_going: s.keep_going,
            permission_mode: s.permission_mode.clone(),
            background: self.background_tasks().len(),
            limit: self.held_at_limit().map(|(l, _)| l).or_else(|| self.limit_warning()),
            continues_at: self.held_at_limit().and_then(|(_, c)| c),
        }
    }

    pub fn publish(&self, ed: &mut Editor, id: crate::BufferId) {
        ed.chats.insert(id, self.info());
    }

    /// Record a custom entry in the session (created if needed) and keep it for resume.
    pub fn append_entry(&mut self, kind: &str, data: Value) {
        self.node.post(Request::Append { chat: self.node.chat.clone(), kind: kind.to_string(), data: data.clone() });
        // Read back at once, before the node's next state says it.
        self.node.state.custom.push((kind.to_string(), data));
    }

    /// Whether a turn cut off by its node stopping (a crash, a reboot) continues on its own.
    pub fn keep_going(&self) -> bool {
        self.node.state.keep_going
    }

    /// This chat's own choice of [`ChatBuffer::keep_going`], kept in its session.
    pub fn set_keep_going(&mut self, on: bool) {
        self.append_entry(serpis_chat::KEEP_GOING, serde_json::json!({"on": on}));
        self.node.state.keep_going = on;
    }

    /// Name the session (`session_info`), e.g. after `buffer-set-title`.
    pub fn set_session_name(&mut self, name: &str) {
        self.node.post(Request::Rename { chat: self.node.chat.clone(), name: name.to_string() });
    }

    // ------------------------------------------------------------------
    // Transcript rendering to styled lines
    // ------------------------------------------------------------------

    fn rebuild(&mut self, ed: &Editor) {
        let t = &ed.theme;
        let show_thinking = ed.var_bool("chat-show-thinking");
        let mut thinking_lines = Vec::new();
        let k = t.ui_scale();
        let m = Measure::Fonts(&ed.ctx, t);
        let mut out: Vec<StyledLine> = Vec::new();
        let mut owner: Vec<Option<usize>> = Vec::new();
        let push = |out: &mut Vec<StyledLine>, owner: &mut Vec<Option<usize>>, l: StyledLine, e: Option<usize>| {
            out.push(l);
            owner.push(e);
        };
        let mut prev_tool = false;
        for (ei, (e, v)) in self.entries.iter().zip(&self.views).enumerate() {
            let is_tool = matches!(e, Entry::Tool { .. });
            match e {
                Entry::User { text, queued, .. } => {
                    let images = &v.images;
                    // A bubble on the right, as wide as its text (or its images).
                    let blk = Block::new(t.user_bg).shrink(0.82);
                    let mut lines: Vec<StyledLine> = Vec::new();
                    if *queued {
                        lines.push(StyledLine::new("Queued · steers the running agent", Face::Meta));
                    }
                    for (i, img) in images.iter().enumerate() {
                        let mut l = StyledLine::new(format!("[image {}×{}]", img.w, img.h), Face::Meta).image(img.id, img.fit(320.0 * k, 240.0 * k));
                        if i > 0 {
                            l.pad_top = 6.0 * k;
                        }
                        lines.push(l);
                    }
                    if !images.is_empty()
                        && !text.is_empty()
                        && let Some(l) = lines.last_mut()
                    {
                        l.pad_bottom = 8.0 * k;
                    }
                    lines.extend(text.lines().map(|l| StyledLine::new(l, Face::UserText)));
                    if lines.is_empty() {
                        lines.push(StyledLine::new("", Face::UserText));
                    }
                    let n = lines.len();
                    for (i, l) in lines.into_iter().enumerate() {
                        let mut l = l.block(blk.nth(i, n)).indent(14.0 * k);
                        if i == 0 {
                            l = l.gap(26.0 * k);
                            l.pad_top = 9.0 * k;
                        }
                        if i + 1 == n {
                            l.pad_bottom = 9.0 * k;
                        }
                        push(&mut out, &mut owner, l, Some(ei));
                    }
                }
                Entry::Assistant { blocks, model, streaming, continuation, usage, error, cache } => {
                    let fold = &v.thinking;
                    // Turns that only call tools get no label of their own.
                    let has_content = blocks.iter().any(|b| !matches!(b, AssistantContent::ToolCall(_)));
                    if !continuation && (has_content || *streaming || error.is_some()) {
                        let label = format!("◆  {}", short_model(model));
                        let diamond = "◆".len();
                        push(&mut out, &mut owner, StyledLine::new(label, Face::Label).spans(vec![(0..diamond, Face::Accent)]).gap(26.0 * k).pad(0.0, 4.0 * k), Some(ei));
                    }
                    // How much it has thought, while it writes (Claude Code's estimate).
                    let thought = if *streaming { self.node.state.thinking_tokens.filter(|n| *n > 0).map(|n| format!(" · ~{} tokens", fmt_tokens(n))).unwrap_or_default() } else { String::new() };
                    // Nothing to show yet (its thinking hidden, or not sent at all): it is thinking.
                    let visible = blocks.iter().any(|b| match b {
                        AssistantContent::Text { text, .. } => !text.trim().is_empty(),
                        AssistantContent::Thinking { thinking, redacted, .. } => *redacted || !thinking.trim().is_empty(),
                        AssistantContent::ToolCall(_) => true,
                    });
                    if *streaming && !visible && error.is_none() {
                        let blk = Block::new(Color32::TRANSPARENT).bar(t.accent);
                        push(&mut out, &mut owner, StyledLine::new(format!("Thinking…{thought}"), Face::Meta).block(blk.nth(0, 1)).indent(14.0 * k).gap(10.0 * k).pad(2.0 * k, 2.0 * k), Some(ei));
                    }
                    for b in blocks {
                        match b {
                            AssistantContent::Thinking { thinking, redacted, .. } => {
                                let body: Vec<&str> = thinking.trim().lines().filter(|l| !l.trim().is_empty()).collect();
                                if body.is_empty() && !*redacted {
                                    continue;
                                }
                                // Hidden thinking keeps its header, so you still see that the model thought.
                                let shown: &[&str] = match fold {
                                    Some(true) => &body,
                                    Some(false) => &[],
                                    None if !show_thinking => &[],
                                    None if *streaming || body.len() <= 4 => &body,
                                    None => &body[body.len() - 3..],
                                };
                                let blk = Block::new(Color32::TRANSPARENT).bar(t.border);
                                let head = if *streaming { format!("Thinking…{thought}") } else if body.len() > shown.len() && body.len() > 1 { format!("Thought · {} lines", body.len()) } else { "Thought".to_string() };
                                let n = shown.len() + 1;
                                thinking_lines.push(out.len());
                                push(&mut out, &mut owner, StyledLine::new(head, Face::Meta).block(blk.nth(0, n)).indent(14.0 * k).gap(10.0 * k).pad(2.0 * k, 2.0 * k), Some(ei));
                                for (i, l) in shown.iter().enumerate() {
                                    let mut line = StyledLine::new(*l, Face::Thinking).block(blk.nth(i + 1, n)).indent(14.0 * k);
                                    if i + 2 == n {
                                        line.pad_bottom = 2.0 * k;
                                    }
                                    thinking_lines.push(out.len());
                                    push(&mut out, &mut owner, line, Some(ei));
                                }
                            }
                            AssistantContent::Text { text, .. } => {
                                for l in markdown_lines(text, t, &m, self.text_w) {
                                    push(&mut out, &mut owner, l, Some(ei));
                                }
                            }
                            AssistantContent::ToolCall(_) => {}
                        }
                    }
                    if let Some(err) = error {
                        let blk = Block::new(t.red.gamma_multiply(0.10)).border(t.red.gamma_multiply(0.45));
                        let lines: Vec<&str> = err.lines().collect();
                        let n = lines.len().max(1);
                        for (i, l) in lines.iter().enumerate() {
                            let mut line = StyledLine::new(*l, Face::Error).block(blk.nth(i, n)).indent(14.0 * k);
                            if i == 0 {
                                line = line.gap(10.0 * k);
                                line.pad_top = 8.0 * k;
                            }
                            if i + 1 == n {
                                line.pad_bottom = 8.0 * k;
                            }
                            push(&mut out, &mut owner, line, Some(ei));
                        }
                    }
                    // Usage closes an answer; turns that only call tools don't repeat it.
                    let calls_tools = blocks.iter().any(|b| matches!(b, AssistantContent::ToolCall(_)));
                    if let Some(u) = usage.as_ref().filter(|_| !calls_tools || error.is_some()) {
                        if u.total_tokens > 0 {
                            let mut line = StyledLine::new(format_usage(u), Face::Meta).gap(8.0 * k);
                            if let Some(h) = cache.filter(|_| self.show_cache.0) {
                                let label = crate::cache_watch::Hit::from(h).label();
                                line.text.push_str(" · ");
                                let start = line.text.len();
                                line.text.push_str(&label);
                                let face = match h {
                                    CacheHit::Miss => Face::MetaWarn,
                                    CacheHit::Hit(_) => Face::MetaGood,
                                    CacheHit::New => Face::Meta,
                                };
                                line.spans.push((start..line.text.len(), face));
                            }
                            push(&mut out, &mut owner, line, Some(ei));
                        }
                    }
                }
                Entry::Tool { name, args, partial, result, elapsed_ms, ask, task, steps, .. } => {
                    let gap = if prev_tool { 6.0 } else { 14.0 } * k;
                    let side = Side { task: task.as_deref(), steps };
                    for l in tool_lines(name, args, partial.as_deref(), result.as_ref(), ask.as_ref(), side, v.expanded, v.started, elapsed_ms.map(Duration::from_millis), t, gap) {
                        push(&mut out, &mut owner, l, Some(ei));
                    }
                }
                Entry::Info { text, tone } => {
                    let face = match tone {
                        Tone::Meta => Face::Meta,
                        Tone::MetaWarn => Face::MetaWarn,
                        Tone::Warning => Face::Warning,
                        Tone::Error => Face::Error,
                    };
                    for (i, l) in text.lines().enumerate() {
                        push(&mut out, &mut owner, StyledLine::new(l, face).gap(if i == 0 { 12.0 * k } else { 0.0 }), Some(ei));
                    }
                }
            }
            prev_tool = is_tool;
        }
        if out.is_empty() {
            push(&mut out, &mut owner, StyledLine::new("", Face::Default), None);
        }
        self.lines = out;
        self.line_entry = owner;
        self.thinking_lines = thinking_lines;
        self.dirty = false;
        let n = self.lines.len();
        if self.view.cursor.line >= n {
            self.view.cursor = Pos::new(n - 1, 0);
        }
    }

    /// Whether entry `ei`'s thinking is shown in full.
    fn thinking_full(&self, ei: usize) -> bool {
        let Some(Entry::Assistant { blocks, streaming, .. }) = self.entries.get(ei) else { return false };
        self.views[ei].thinking.unwrap_or_else(|| {
            let lines = blocks.iter().map(|b| match b {
                AssistantContent::Thinking { thinking, .. } => thinking.trim().lines().filter(|l| !l.trim().is_empty()).count(),
                _ => 0,
            });
            self.show_thinking && (*streaming || lines.max().unwrap_or(0) <= 4)
        })
    }

    /// Toggle the fold of the entry at transcript line `line`.
    pub fn toggle_fold_at(&mut self, line: usize) -> bool {
        let Some(Some(ei)) = self.line_entry.get(line).copied() else { return false };
        if self.thinking_lines.binary_search(&line).is_ok() {
            let full = self.thinking_full(ei);
            if let Some(Entry::Assistant { .. }) = self.entries.get(ei) {
                self.views[ei].thinking = Some(!full);
                self.dirty = true;
                return true;
            }
        }
        if let Some(Entry::Tool { name, result, .. }) = self.entries.get(ei) {
            let default = default_expanded(name, result.as_ref());
            let v = &mut self.views[ei];
            v.expanded = Some(!v.expanded.unwrap_or(default));
            self.dirty = true;
            return true;
        }
        false
    }

    pub fn toggle_fold_at_cursor(&mut self) -> bool {
        if self.focus == Focus::Transcript {
            self.toggle_fold_at(self.view.cursor.line)
        } else {
            false
        }
    }

    pub fn history_prev(&mut self) {
        if self.prompt_history.is_empty() {
            return;
        }
        let i = match self.history_index {
            None => self.prompt_history.len() - 1,
            Some(i) => i.saturating_sub(1),
        };
        self.history_index = Some(i);
        let t = self.prompt_history[i].clone();
        self.prompt.set_text(&t);
        self.prompt.move_to_end();
    }

    pub fn history_next(&mut self) {
        let Some(i) = self.history_index else { return };
        if i + 1 >= self.prompt_history.len() {
            self.history_index = None;
            self.prompt.clear();
        } else {
            self.history_index = Some(i + 1);
            let t = self.prompt_history[i + 1].clone();
            self.prompt.set_text(&t);
            self.prompt.move_to_end();
        }
    }

    fn t_len(&self) -> usize {
        self.lines.len()
    }

}

fn short_model(id: &str) -> &str {
    id.rsplit('/').next().unwrap_or(id)
}

/// A duration people read at a glance: 4.2s (with `tenths`) or 42s, 11m 40s, 2h 05m.
pub fn fmt_duration(d: Duration, tenths: bool) -> String {
    let s = d.as_secs();
    match s {
        0..=9 if tenths => format!("{:.1}s", d.as_secs_f32()),
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m {:02}s", s / 60, s % 60),
        _ => format!("{}h {:02}m", s / 3600, s % 3600 / 60),
    }
}

fn format_usage(u: &Usage) -> String {
    let mut s = format!("↳ {} in · {} out", fmt_tokens(u.input), fmt_tokens(u.output));
    if u.cache_read > 0 {
        s.push_str(&format!(" · {} cached", fmt_tokens(u.cache_read)));
    }
    if u.cost.total > 0.0 {
        s.push_str(&format!(" · ${:.4}", u.cost.total));
    }
    s
}

/// Inline markdown: `code` and **bold** become spans; markers are removed.
fn inline_spans(text: &str) -> (String, Vec<(Range<usize>, Face)>) {
    let mut out = String::with_capacity(text.len());
    let mut spans = Vec::new();
    let mut rest = text;
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('`') {
            if let Some(end) = r.find('`') {
                let s = out.len();
                out.push_str(&r[..end]);
                spans.push((s..out.len(), Face::InlineCode));
                rest = &r[end + 1..];
                continue;
            }
        }
        if let Some(r) = rest.strip_prefix("**") {
            if let Some(end) = r.find("**") {
                let s = out.len();
                out.push_str(&r[..end]);
                spans.push((s..out.len(), Face::Bold));
                rest = &r[end + 2..];
                continue;
            }
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        rest = &rest[c.len_utf8()..];
    }
    (out, spans)
}

/// Cells of a markdown table row: `| a | b |` → `["a", "b"]` (`\|` stays a pipe).
fn table_cells(line: &str) -> Vec<String> {
    let l = line.trim();
    let l = l.strip_prefix('|').unwrap_or(l);
    let l = if l.ends_with('|') && !l.ends_with("\\|") { &l[..l.len() - 1] } else { l };
    let mut cells = vec![String::new()];
    let mut chars = l.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' if chars.peek() == Some(&'|') => {
                cells.last_mut().unwrap().push('|');
                chars.next();
            }
            '|' => cells.push(String::new()),
            c => cells.last_mut().unwrap().push(c),
        }
    }
    cells.into_iter().map(|c| c.trim().to_string()).collect()
}

/// A delimiter row's column alignments (`:--`, `:-:`, `--:`), if `line` is one.
fn table_alignments(line: &str) -> Option<Vec<egui::Align>> {
    let l = line.trim();
    if !l.contains('-') || !l.chars().all(|c| matches!(c, '|' | '-' | ':' | ' ' | '\t')) {
        return None;
    }
    let cells = table_cells(l);
    cells
        .iter()
        .map(|c| {
            let ok = !c.is_empty() && c.trim_matches(':').chars().all(|ch| ch == '-');
            ok.then(|| match (c.starts_with(':'), c.ends_with(':')) {
                (true, true) => egui::Align::Center,
                (false, true) => egui::Align::Max,
                _ => egui::Align::Min,
            })
        })
        .collect()
}

/// Measures text exactly as the transcript lays it out (tests count characters instead).
pub enum Measure<'a> {
    Fonts(&'a egui::Context, &'a crate::theme::Theme),
    Chars(f32),
}

impl Measure<'_> {
    fn width(&self, text: &str, face: Face) -> f32 {
        match self {
            _ if text.is_empty() => 0.0,
            Measure::Chars(w) => text.chars().count() as f32 * w,
            Measure::Fonts(ctx, theme) => {
                let job = crate::text::layout_job(&StyledLine::new(text, face), theme, f32::INFINITY);
                ctx.fonts_mut(|f| f.layout_job(job)).size().x
            }
        }
    }

    fn scale(&self) -> f32 {
        match self {
            Measure::Fonts(_, t) => t.ui_scale(),
            Measure::Chars(_) => 1.0,
        }
    }
}

/// Byte ranges of the lines `text` wraps into at `width` (at spaces; long words break).
fn wrap_ranges(text: &str, width: f32, face: Face, m: &Measure) -> Vec<Range<usize>> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut end = 0;
    for (i, word) in text.split_inclusive(' ').scan(0, |at, w| {
        let s = *at;
        *at += w.len();
        Some((s, w))
    }) {
        let word_end = i + word.trim_end().len();
        if end > start && m.width(&text[start..word_end], face) > width {
            out.push(start..end);
            start = i;
        }
        // A word wider than the line breaks by characters.
        while m.width(&text[start..word_end], face) > width {
            let mut cut = start;
            for (ci, ch) in text[start..word_end].char_indices() {
                if cut > start && m.width(&text[start..start + ci + ch.len_utf8()], face) > width {
                    break;
                }
                cut = start + ci + ch.len_utf8();
            }
            if cut <= start || cut >= word_end {
                break;
            }
            out.push(start..cut);
            start = cut;
        }
        end = word_end;
    }
    if end > start || out.is_empty() {
        out.push(start..end.max(start));
    }
    out
}

/// A markdown table in the reading font: measured columns, a tinted header, hairlines
/// between rows, and cells that wrap when the table is wider than `avail`.
fn table_lines(header: &str, aligns: &[egui::Align], rows: &[&str], t: &crate::theme::Theme, m: &Measure, avail: f32) -> Vec<StyledLine> {
    let k = m.scale();
    let pad_x = 12.0 * k;
    let gap = 22.0 * k;
    let parse = |l: &str| -> Vec<(String, Vec<(Range<usize>, Face)>)> { table_cells(l).iter().map(|c| inline_spans(c)).collect() };
    let head = parse(header);
    let body: Vec<_> = rows.iter().map(|r| parse(r)).collect();
    let cols = std::iter::once(&head).chain(&body).map(Vec::len).max().unwrap_or(0).max(aligns.len()).max(1);
    let face_of = |row: usize| if row == 0 { Face::Bold } else { Face::AssistantText };
    let all: Vec<&Vec<(String, Vec<(Range<usize>, Face)>)>> = std::iter::once(&head).chain(&body).collect();
    let cell = |r: usize, c: usize| all[r].get(c).map(|(s, _)| s.as_str()).unwrap_or("");
    let natural: Vec<f32> = (0..cols).map(|c| (0..all.len()).map(|r| m.width(cell(r, c), face_of(r))).fold(0.0, f32::max)).collect();
    let minimum: Vec<f32> = (0..cols)
        .map(|c| {
            let word = (0..all.len()).flat_map(|r| cell(r, c).split(' ').map(move |w| (r, w))).map(|(r, w)| m.width(w, face_of(r))).fold(0.0, f32::max);
            natural[c].min(word.max(56.0 * k)).min(160.0 * k)
        })
        .collect();
    // The widest columns give up room first.
    let budget = (avail - 2.0 * pad_x - gap * (cols - 1) as f32).max(cols as f32 * 40.0 * k);
    let mut widths = natural.clone();
    while widths.iter().sum::<f32>() > budget + 0.5 {
        let over = widths.iter().sum::<f32>() - budget;
        let Some(i) = (0..cols).filter(|i| widths[*i] > minimum[*i] + 0.5).max_by(|a, b| widths[*a].total_cmp(&widths[*b])) else { break };
        let next = (0..cols).filter(|j| *j != i).map(|j| widths[j]).fold(minimum[i], f32::max);
        widths[i] -= over.min((widths[i] - next).max(4.0 * k)).min(widths[i] - minimum[i]);
    }
    let col_x: Vec<f32> = (0..cols).map(|c| widths[..c].iter().sum::<f32>() + gap * c as f32).collect();
    let border = t.border;
    let card = Block::new(Color32::TRANSPARENT).border(border);
    let head_bg = Block { bg: t.bg_elevated, ..card };
    let space = m.width(" ", Face::AssistantText);
    let mut out: Vec<StyledLine> = Vec::new();
    for (r, row) in all.iter().enumerate() {
        let face = face_of(r);
        let wrapped: Vec<Vec<Range<usize>>> = (0..cols).map(|c| wrap_ranges(cell(r, c), widths[c], face, m)).collect();
        let height = wrapped.iter().map(Vec::len).max().unwrap_or(1);
        for li in 0..height {
            let mut text = String::new();
            let mut spans = Vec::new();
            let mut pads = Vec::new();
            let mut x = 0.0;
            for c in 0..cols {
                if c > 0 {
                    text.push(' ');
                    x += space;
                }
                let (src, src_spans) = row.get(c).cloned().unwrap_or_default();
                let range = wrapped[c].get(li).cloned().unwrap_or(0..0);
                let seg = &src[range.clone()];
                let w = m.width(seg, face);
                let slack = (widths[c] - w).max(0.0);
                let align = match aligns.get(c).copied().unwrap_or(egui::Align::Min) {
                    egui::Align::Max => slack,
                    egui::Align::Center => slack / 2.0,
                    egui::Align::Min => 0.0,
                };
                let at = text.len();
                pads.push((at, (col_x[c] + align - x).max(0.0)));
                text.push_str(seg);
                for (sr, f) in src_spans {
                    let (s0, s1) = (sr.start.max(range.start), sr.end.min(range.end));
                    if s0 < s1 {
                        spans.push((at + s0 - range.start, at + s1 - range.start, f));
                    }
                }
                x = col_x[c] + align + w;
            }
            let mut line = StyledLine::new(text, face).spans(spans.into_iter().map(|(a, b, f)| (a..b, f)).collect()).indent(pad_x);
            line.pads = pads;
            line.block = Some(if r == 0 { head_bg } else { card });
            if li == 0 {
                line.pad_top = 7.0 * k;
            }
            if li + 1 == height {
                line.pad_bottom = 7.0 * k;
                line.rule_below = Some(border);
            }
            out.push(line);
        }
    }
    let n = out.len();
    for (i, l) in out.iter_mut().enumerate() {
        if let Some(b) = l.block.as_mut() {
            b.top = i == 0;
            b.bottom = i + 1 == n;
        }
    }
    if let Some(first) = out.first_mut() {
        first.gap_above = 12.0 * k;
    }
    out
}

/// A list item: (nesting level, marker, text) for `- x`, `* x`, `+ x` and `1. x`.
fn list_item(raw: &str) -> Option<(usize, String, &str)> {
    let trimmed = raw.trim_start();
    let level = (raw.len() - trimmed.len()) / 2;
    if let Some(b) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")).or_else(|| trimmed.strip_prefix("+ ")) {
        return Some((level, "•".into(), b));
    }
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && digits < 4 {
        let rest = &trimmed[digits..];
        if let Some(b) = rest.strip_prefix(". ").or_else(|| rest.strip_prefix(") ")) {
            return Some((level, format!("{}.", &trimmed[..digits]), b));
        }
    }
    None
}

/// Markdown to styled lines for a column `avail` points wide.
fn markdown_lines(text: &str, t: &crate::theme::Theme, m: &Measure, avail: f32) -> Vec<StyledLine> {
    let k = m.scale();
    let mut out: Vec<StyledLine> = Vec::new();
    let src: Vec<&str> = text.split('\n').collect();
    // Space owed to the next line: paragraph breaks, the start of the message.
    let mut gap = 2.0 * k;
    let mut prev_list = false;
    let mut i = 0;
    let code_card = Block::new(t.tool_bg).border(t.border);
    while i < src.len() {
        let raw = src[i];
        let trimmed = raw.trim_start();
        if trimmed.is_empty() {
            gap = gap.max(10.0 * k);
            prev_list = false;
            i += 1;
            continue;
        }
        // Fenced code: a bordered card, the language as a small caption.
        if trimmed.starts_with("```") {
            let lang = trimmed.trim_start_matches('`').trim().to_string();
            let mut code: Vec<&str> = Vec::new();
            i += 1;
            while i < src.len() && !src[i].trim_start().starts_with("```") {
                code.push(src[i]);
                i += 1;
            }
            i += 1;
            let mut lines: Vec<StyledLine> = Vec::new();
            if !lang.is_empty() {
                lines.push(StyledLine::new(lang.clone(), Face::Meta).pad(8.0 * k, 0.0));
            }
            let hl = crate::highlight::highlight(&lang, &code, Face::Code).unwrap_or_default();
            lines.extend(code.iter().enumerate().map(|(j, l)| StyledLine::new(*l, Face::Code).spans(hl.get(j).cloned().unwrap_or_default())));
            if lines.is_empty() {
                lines.push(StyledLine::new("", Face::Code));
            }
            let n = lines.len();
            for (j, mut l) in lines.into_iter().enumerate() {
                l = l.block(code_card.nth(j, n)).indent(14.0 * k);
                if j == 0 {
                    l.gap_above = gap.max(10.0 * k);
                    l.pad_top = l.pad_top.max(10.0 * k);
                }
                if j + 1 == n {
                    l.pad_bottom = 10.0 * k;
                }
                out.push(l);
            }
            gap = 10.0 * k;
            prev_list = false;
            continue;
        }
        // A table: a row of cells, a delimiter row, then more rows.
        if trimmed.starts_with('|') {
            if let Some(aligns) = src.get(i + 1).and_then(|l| table_alignments(l)) {
                let rows: Vec<&str> = src[i + 2..].iter().take_while(|l| l.trim_start().starts_with('|')).copied().collect();
                i += 2 + rows.len();
                let mut lines = table_lines(raw, &aligns, &rows, t, m, avail);
                if let Some(f) = lines.first_mut() {
                    f.gap_above = gap.max(12.0 * k);
                }
                out.extend(lines);
                gap = 12.0 * k;
                prev_list = false;
                continue;
            }
        }
        i += 1;
        // A thematic break.
        if trimmed.len() >= 3 && (trimmed.chars().all(|c| c == '-') || trimmed.chars().all(|c| c == '*') || trimmed.chars().all(|c| c == '_')) {
            out.push(StyledLine::new("", Face::Meta).rule(t.border).gap(gap).pad(6.0 * k, 6.0 * k));
            gap = 10.0 * k;
            continue;
        }
        if let Some(h) = trimmed.strip_prefix('#') {
            let level = 1 + h.chars().take_while(|c| *c == '#').count();
            let (s, spans) = inline_spans(h.trim_start_matches('#').trim());
            let face = if level <= 2 { Face::Heading } else { Face::Bold };
            out.push(StyledLine::new(s, face).spans(spans).gap(gap.max(if level <= 2 { 18.0 } else { 14.0 } * k)).pad(0.0, 4.0 * k));
            gap = 0.0;
            prev_list = false;
            continue;
        }
        if let Some((level, marker, body)) = list_item(raw) {
            let (s, spans) = inline_spans(body);
            let indent = 22.0 * k + level as f32 * 20.0 * k;
            let g = if prev_list { 3.0 * k } else { gap.max(4.0 * k) };
            out.push(StyledLine::new(s, Face::AssistantText).spans(spans).indent(indent).gap(g).marker(marker, Face::Dim));
            gap = 0.0;
            prev_list = true;
            continue;
        }
        if let Some(q) = trimmed.strip_prefix('>') {
            let (s, spans) = inline_spans(q.trim_start());
            let blk = Block::new(Color32::TRANSPARENT).bar(t.border).nth(0, 1);
            out.push(StyledLine::new(s, Face::Dim).spans(spans).block(blk).indent(14.0 * k).gap(gap).pad(2.0 * k, 2.0 * k));
            gap = 0.0;
            continue;
        }
        let (s, spans) = inline_spans(raw.trim_end());
        out.push(StyledLine::new(s, Face::AssistantText).spans(spans).gap(gap));
        gap = 0.0;
        prev_list = false;
    }
    out
}

/// Highlight spans moved right by `by` bytes (past a prefix the code didn't have).
fn shift(spans: Option<&crate::highlight::Spans>, by: usize) -> crate::highlight::Spans {
    spans.map(|s| s.iter().map(|(r, f)| (r.start + by..r.end + by, *f)).collect()).unwrap_or_default()
}

/// Where the code starts in an edit diff line (`+12 code`, `-3 code`, ` 7 code`).
fn diff_code_start(l: &str) -> Option<usize> {
    let b = l.as_bytes();
    if !matches!(b.first(), Some(b'+' | b'-' | b' ')) {
        return None;
    }
    let mut i = 1;
    while b.get(i) == Some(&b' ') {
        i += 1;
    }
    let digits = i;
    while b.get(i).is_some_and(u8::is_ascii_digit) {
        i += 1;
    }
    (i > digits && b.get(i) == Some(&b' ')).then_some(i + 1)
}

fn default_expanded(name: &str, result: Option<&ToolResultMessage>) -> bool {
    match result {
        Some(r) if r.is_error => true,
        // Images are what you want to see.
        Some(r) if r.content.iter().any(|c| matches!(c, UserContent::Image { .. })) => true,
        // Claude Code's plan (its todo list, a plan to approve) is what you want to see.
        _ => matches!(name, "edit" | "write" | "TodoWrite" | "ExitPlanMode" | "AskUserQuestion"),
    }
}

fn arg<'a>(args: &'a Value, k: &str) -> &'a str {
    args.get(k).and_then(Value::as_str).unwrap_or("")
}

/// A tool call as a card: a header row (status, name, what it acts on, time), a hairline,
/// then its output or diff.
#[allow(clippy::too_many_arguments)]
/// What kind of work a task is, as Claude Code counts them: ("Shell", "shell", "shells").
fn task_kind(t: &serpis_proto::Task) -> (&'static str, &'static str, &'static str) {
    match t.kind.as_str() {
        "bash" => ("Shell", "shell", "shells"),
        "monitor" => ("Monitor", "monitor", "monitors"),
        "agent" | "remote_agent" | "in_process_teammate" => ("Agent", "agent", "agents"),
        "local_workflow" => ("Workflow", "workflow", "workflows"),
        _ => ("Task", "task", "tasks"),
    }
}

/// What runs in the background, counted by kind: "1 shell · 1 monitor", "2 agents".
pub fn background_counts(running: &[serpis_proto::Task]) -> String {
    let mut counts: Vec<(&str, &str, usize)> = Vec::new();
    // In a fixed order, as Claude Code lists them: shells first.
    let order = ["shell", "monitor", "agent", "workflow", "task"];
    let mut running: Vec<&serpis_proto::Task> = running.iter().collect();
    running.sort_by_key(|t| order.iter().position(|o| *o == task_kind(t).1));
    for t in running {
        let (_, one, many) = task_kind(t);
        match counts.iter_mut().find(|(o, ..)| *o == one) {
            Some(c) => c.2 += 1,
            None => counts.push((one, many, 1)),
        }
    }
    counts.iter().map(|(one, many, n)| if *n == 1 { format!("1 {one}") } else { format!("{n} {many}") }).collect::<Vec<_>>().join(" · ")
}

/// Claude Code's permission mode as its prompt line says it, and its color.
pub fn permission_mode_label(mode: &str, t: &crate::theme::Theme) -> (String, Color32) {
    match mode {
        "acceptEdits" => ("⏵⏵ accept edits on".into(), t.purple),
        "plan" => ("⏸ plan mode on".into(), t.cyan),
        "auto" => ("⏵⏵ auto mode on".into(), t.yellow),
        "dontAsk" => ("⏵⏵ don't ask on".into(), t.yellow),
        "bypassPermissions" => ("⏵⏵ bypass permissions on".into(), t.red),
        _ => ("asks before acting".into(), t.fg_dim),
    }
}

impl ChatBuffer {
    /// Switch Claude Code's permission mode for this chat.
    /// Stopped at a usage limit (until it resets): the limit, and when the chat goes on by
    /// itself (None: it waits for you).
    pub fn held_at_limit(&self) -> Option<(serpis_proto::Limit, Option<i64>)> {
        let s = &self.node.state;
        let now = unix_now();
        s.limit.clone().filter(|l| l.reached() && !s.running && l.resets_at.is_none_or(|t| t > now)).map(|l| (l, s.continues_at))
    }

    /// Close to a usage limit (Claude Code warned), until it resets.
    pub fn limit_warning(&self) -> Option<serpis_proto::Limit> {
        let now = unix_now();
        self.node.state.limit.clone().filter(|l| l.status == "allowed_warning" && l.resets_at.is_none_or(|t| t > now))
    }

    /// Whether the chat, stopped at a usage limit, goes on by itself when it resets.
    pub fn set_continue_after_limit(&mut self, on: bool) {
        self.node.post(Request::SetContinueAfterLimit { chat: self.node.chat.clone(), on });
        // Shown at once (roughly: the node adds its delay); its state confirms it.
        self.node.state.continues_at = if on { self.node.state.limit.as_ref().and_then(|l| l.resets_at) } else { None };
        self.dirty = true;
    }

    pub fn set_permission_mode(&mut self, mode: &str) {
        self.node.post(Request::SetPermissionMode { chat: self.node.chat.clone(), mode: mode.to_string() });
        // Shown at once; the node's state confirms it.
        self.node.state.permission_mode = Some(mode.to_string());
        self.dirty = true;
    }
}

/// The bar of a chat stopped at a usage limit: "Session limit reached · resets in 1h 12m ·
/// goes on by itself then".
fn limit_line(limit: &serpis_proto::Limit, continues: Option<i64>, now: i64) -> String {
    let name = limit.name();
    let mut s = format!("{}{} reached", name[..1].to_uppercase(), &name[1..]);
    if let Some(t) = limit.resets_at {
        s.push_str(&format!(" · resets in {}", crate::claude_usage::until(t as u64, now as u64)));
    }
    s.push_str(if continues.is_some() { " · goes on by itself then" } else { " · waits for you" });
    s
}

/// A chat stopped at a usage limit, in the sidebar: "Session limit · goes on in 59m",
/// "Weekly limit · resets in 2d 3h · waits for you".
fn limit_status(limit: &serpis_proto::Limit, continues: Option<i64>, now: i64) -> String {
    let name = limit.name();
    let name = format!("{}{}", name[..1].to_uppercase(), &name[1..]);
    let until = |t: i64| crate::claude_usage::until(t as u64, now as u64);
    match (continues, limit.resets_at) {
        (Some(at), _) => format!("{name} · goes on in {}", until(at)),
        (None, Some(t)) => format!("{name} · resets in {} · waits for you", until(t)),
        (None, None) => format!("{name} · waits for you"),
    }
}

fn unix_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

/// What a tool call started on the side: the work's state, and a subagent's steps.
#[derive(Clone, Copy)]
struct Side<'a> {
    task: Option<&'a serpis_proto::Task>,
    steps: &'a [Entry],
}

/// How work on the side is going, in a few words: "in background · 12s", "3 tools · 1.2k
/// tokens", "stopped".
fn task_status(task: &serpis_proto::Task) -> String {
    let mut parts = Vec::new();
    match task.status.as_str() {
        "running" if task.background => parts.push("in background".to_string()),
        "running" => {}
        "failed" => parts.push("failed".into()),
        "stopped" => parts.push("stopped".into()),
        _ if task.background => parts.push("done in background".into()),
        _ => {}
    }
    if task.tool_uses > 0 {
        parts.push(format!("{} tool{}", task.tool_uses, if task.tool_uses == 1 { "" } else { "s" }));
    }
    if task.tokens > 0 {
        parts.push(format!("{} tokens", fmt_tokens(task.tokens)));
    }
    if task.duration_ms >= 1000 {
        parts.push(fmt_duration(Duration::from_millis(task.duration_ms), false));
    }
    parts.join(" · ")
}

/// A subagent's steps as rows: each tool call on a line (its status, name and what it acts
/// on), and what it said.
fn step_rows(steps: &[Entry], depth: usize, out: &mut Vec<(StyledLine, Option<Color32>)>) {
    let pad = "    ".repeat(depth);
    for (i, e) in steps.iter().enumerate() {
        match e {
            Entry::Tool { name, args, result, ask, task, steps: inner, .. } => {
                let (icon, face) = match result {
                    None if ask.is_some() => ("?", Face::Warning),
                    None => ("●", Face::Warning),
                    Some(r) if r.is_error => ("✗", Face::Error),
                    Some(_) => ("✓", Face::ToolOk),
                };
                let mut text = format!("{pad}{icon} ");
                let mut spans = vec![(pad.len()..text.len(), face)];
                let n0 = text.len();
                text.push_str(name);
                spans.push((n0..text.len(), Face::ToolHeader));
                let summary = serpis_chat::tool_summary(name, args);
                if !summary.is_empty() {
                    text.push_str("  ");
                    let s0 = text.len();
                    text.push_str(&summary);
                    spans.push((s0..text.len(), Face::ToolArgs));
                }
                if let Some(t) = task.as_deref().map(task_status).filter(|s| !s.is_empty()) {
                    text.push_str("   ");
                    let s0 = text.len();
                    text.push_str(&t);
                    spans.push((s0..text.len(), Face::Meta));
                }
                out.push((StyledLine::new(text, Face::ToolArgs).spans(spans), None));
                step_rows(inner, depth + 1, out);
            }
            Entry::Assistant { blocks, error, .. } => {
                // What it said last is its answer; what it said on the way, dimmer.
                let last = !steps[i + 1..].iter().any(|e| matches!(e, Entry::Assistant { .. }));
                let face = if error.is_some() { Face::Error } else if last { Face::AssistantText } else { Face::Dim };
                let text: String = blocks.iter().filter_map(|b| if let AssistantContent::Text { text, .. } = b { Some(text.as_str()) } else { None }).collect::<Vec<_>>().join("\n");
                for l in text.lines().chain(error.as_deref().into_iter()) {
                    out.push((StyledLine::new(format!("{pad}{l}"), face), None));
                }
            }
            _ => {}
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn tool_lines(
    name: &str,
    args: &Value,
    partial: Option<&str>,
    result: Option<&ToolResultMessage>,
    ask: Option<&serpis_proto::Ask>,
    side: Side,
    expanded: Option<bool>,
    started: Option<Instant>,
    elapsed: Option<Duration>,
    t: &crate::theme::Theme,
    gap: f32,
) -> Vec<StyledLine> {
    let k = t.ui_scale();
    let working = side.task.is_some_and(|t| t.running());
    let (icon, icon_face) = match result {
        None if ask.is_some() => ("?", Face::Warning),
        None if started.is_some() => ("●", Face::Warning),
        None => ("○", Face::Meta),
        Some(r) if r.is_error => ("✗", Face::Error),
        // Returned, and still at it in the background.
        Some(_) if working => ("●", Face::Accent),
        Some(_) if side.task.is_some_and(|t| t.status == "failed") => ("✗", Face::Error),
        Some(_) if side.task.is_some_and(|t| t.status == "stopped") => ("■", Face::Meta),
        Some(_) => ("✓", Face::ToolOk),
    };
    let card = Block::new(t.tool_bg).border(t.border);
    // What it would do is what you need to see to allow it.
    let expanded = expanded.unwrap_or_else(|| ask.is_some() || default_expanded(name, result));
    let mut header = format!("{icon}  ");
    let mut spans = vec![(0..icon.len(), icon_face)];
    let s0 = header.len();
    header.push_str(name);
    spans.push((s0..header.len(), Face::ToolHeader));
    let summary = serpis_chat::tool_summary(name, args);
    if !summary.is_empty() {
        header.push_str("  ");
        let s1 = header.len();
        header.push_str(&summary);
        spans.push((s1..header.len(), Face::ToolArgs));
    }
    let time = match (elapsed, started) {
        _ if ask.is_some() => "waiting for you".to_string(),
        _ if side.task.is_some_and(|t| !task_status(t).is_empty()) => task_status(side.task.unwrap()),
        (Some(e), _) if e.as_millis() >= 100 => fmt_duration(e, true),
        (None, Some(s)) => fmt_duration(s.elapsed(), false),
        _ => String::new(),
    };
    if !time.is_empty() {
        header.push_str("   ");
        let s2 = header.len();
        header.push_str(&time);
        spans.push((s2..header.len(), Face::Meta));
    }

    // Body lines, each with an optional row tint (diffs).
    let mut body: Vec<(StyledLine, Option<Color32>)> = Vec::new();
    // A subagent: what it does, as it goes (its own words close it).
    step_rows(side.steps, 0, &mut body);
    if body.is_empty()
        && let Some(task) = side.task
        && (task.kind == "bash" || task.kind == "monitor")
        && task.background
    {
        // A command in the background: how it is going instead of where its output goes.
        let (line, face) = match task.status.as_str() {
            "running" => ("Running in the background · C-c C-k stops it".to_string(), Face::Meta),
            "failed" => (task.summary.clone().unwrap_or_else(|| "It failed.".into()), Face::Error),
            "stopped" => ("Stopped.".to_string(), Face::Meta),
            _ => (task.summary.clone().unwrap_or_else(|| "Finished.".into()), Face::ToolOutput),
        };
        body.push((StyledLine::new(line, face), None));
    } else if body.is_empty() && side.task.is_some_and(|t| t.running() && t.kind == "agent") {
        body.push((StyledLine::new(side.task.and_then(|t| t.summary.clone()).unwrap_or_else(|| "Starting…".into()), Face::Meta), None));
    }
    if !body.is_empty() && side.task.is_some_and(|t| t.running()) && side.task.is_some_and(|t| t.kind == "agent") {
        let now = side.task.and_then(|t| t.last_tool.clone()).map(|n| format!("● using {n}…")).unwrap_or_else(|| "● working…".into());
        body.push((StyledLine::new(now, Face::Meta), None));
    }
    let diff = result.and_then(|r| r.details.get("diff")).and_then(Value::as_str);
    if let (Some(d), Some(r)) = (diff, result) {
        if !r.is_error {
            // Lines are `+12 code`: highlight the code after the sign and line number.
            let lines: Vec<&str> = d.lines().collect();
            let starts: Vec<Option<usize>> = lines.iter().map(|l| diff_code_start(l)).collect();
            let code: Vec<&str> = lines.iter().zip(&starts).map(|(l, s)| s.map_or("", |s| &l[s..])).collect();
            let hl = crate::highlight::highlight(arg(args, "path"), &code, Face::Code).unwrap_or_default();
            for (i, l) in lines.iter().enumerate() {
                let (face, tint) = match l.chars().next() {
                    Some('+') => (Face::DiffAdd, Some(t.diff_add_bg)),
                    Some('-') => (Face::DiffDel, Some(t.diff_del_bg)),
                    _ => (Face::DiffContext, None),
                };
                let spans = starts[i].map(|s| shift(hl.get(i), s)).unwrap_or_default();
                body.push((StyledLine::new(*l, face).spans(spans), tint));
            }
        }
    }
    if body.is_empty() {
        if name == "write" && result.is_none_or(|r| !r.is_error) {
            let content = arg(args, "content");
            let lines: Vec<&str> = content.lines().collect();
            if !lines.is_empty() {
                body.push((StyledLine::new(format!("{} lines", lines.len()), Face::Meta), None));
                let hl = crate::highlight::highlight(arg(args, "path"), &lines, Face::DiffAdd).unwrap_or_default();
                for (i, l) in lines.iter().enumerate() {
                    let spans = shift(hl.get(i), 2);
                    body.push((StyledLine::new(format!("+ {l}"), Face::DiffAdd).spans(spans), Some(t.diff_add_bg)));
                }
            }
        } else if name == "edit" && result.is_none() {
            // Not run yet (waiting to be allowed): the change it asks to make.
            let one = [args.clone()];
            let edits = args.get("edits").and_then(Value::as_array).map(Vec::as_slice).unwrap_or(&one);
            for e in edits {
                for l in arg(e, "old_string").lines() {
                    body.push((StyledLine::new(format!("- {l}"), Face::DiffDel), Some(t.diff_del_bg)));
                }
                for l in arg(e, "new_string").lines() {
                    body.push((StyledLine::new(format!("+ {l}"), Face::DiffAdd), Some(t.diff_add_bg)));
                }
            }
        } else if name == "TodoWrite" {
            // Claude Code's todo list, as a checklist.
            for todo in args.get("todos").and_then(Value::as_array).into_iter().flatten() {
                let (mark, text, face) = match todo["status"].as_str() {
                    Some("completed") => ("☑", todo["content"].as_str(), Face::Dim),
                    Some("in_progress") => ("◐", todo["activeForm"].as_str().or(todo["content"].as_str()), Face::Bold),
                    _ => ("☐", todo["content"].as_str(), Face::ToolOutput),
                };
                body.push((StyledLine::new(format!("{mark} {}", text.unwrap_or("")), face), None));
            }
        } else if name == "ExitPlanMode" {
            // Its plan is Markdown: headings stand out, the rest reads as prose.
            for l in arg(args, "plan").lines() {
                let heading = l.trim_start().strip_prefix('#').map(|h| h.trim_start_matches('#').trim());
                body.push(match heading {
                    Some(h) => (StyledLine::new(h, Face::Bold), None),
                    None => (StyledLine::new(l, Face::AssistantText), None),
                });
            }
        } else if name == "AskUserQuestion" {
            // Claude Code's questions, with their options; then what was answered.
            for q in args.get("questions").and_then(Value::as_array).into_iter().flatten() {
                let header = q["header"].as_str().filter(|h| !h.is_empty()).map(|h| format!("{h} · ")).unwrap_or_default();
                let multi = if q["multiSelect"].as_bool() == Some(true) { "  (any of)" } else { "" };
                body.push((StyledLine::new(format!("{header}{}{multi}", q["question"].as_str().unwrap_or("")), Face::Bold), None));
                for o in q["options"].as_array().into_iter().flatten() {
                    let desc = o["description"].as_str().filter(|d| !d.is_empty()).map(|d| format!(" — {d}")).unwrap_or_default();
                    body.push((StyledLine::new(format!("  · {}{desc}", o["label"].as_str().unwrap_or("")), Face::ToolOutput), None));
                }
            }
            if let Some(r) = result {
                for l in tool_text(r).lines() {
                    body.push((StyledLine::new(l, if r.is_error { Face::ToolError } else { Face::AssistantText }), None));
                }
            }
        } else if name == "bash" {
            let cmd = arg(args, "command");
            for l in cmd.lines().skip(1) {
                body.push((StyledLine::new(l, Face::ToolArgs), None));
            }
            let out = result.map(|r| tool_text(r)).or(partial.map(str::to_string)).unwrap_or_default();
            let face = if result.is_some_and(|r| r.is_error) { Face::ToolError } else { Face::ToolOutput };
            for l in out.lines() {
                body.push((StyledLine::new(l, face), None));
            }
        } else if let Some(r) = result {
            let face = if r.is_error { Face::ToolError } else { Face::ToolOutput };
            // Images (read on an image file, screenshots): shown, not described.
            for c in &r.content {
                if let UserContent::Image { data, .. } = c
                    && let Some(img) = crate::images::from_base64(data)
                {
                    body.push((StyledLine::new(format!("[image {}×{}]", img.w, img.h), Face::Meta).image(img.id, img.fit(360.0 * k, 260.0 * k)).pad(4.0 * k, 4.0 * k), None));
                }
            }
            let text = tool_text(r);
            let lines: Vec<&str> = text.lines().collect();
            let hl = if name == "read" && !r.is_error { crate::highlight::highlight(arg(args, "path"), &lines, face) } else { None }.unwrap_or_default();
            for (i, l) in lines.iter().enumerate() {
                body.push((StyledLine::new(*l, face).spans(hl.get(i).cloned().unwrap_or_default()), None));
            }
        }
    }

    if let Some(a) = ask {
        if name == "AskUserQuestion" {
            body.push((StyledLine::new("Answer it? C-c C-y answer · C-c C-d skip", Face::Warning), None));
        } else if name == "ExitPlanMode" {
            body.push((StyledLine::new("Approve this plan? C-c C-y yes · C-c C-a yes, accepting edits · C-c C-d keep planning", Face::Warning), None));
        } else {
            let always = a.always.as_ref().map(|w| format!(" · C-c C-a always ({w})")).unwrap_or_default();
            body.push((StyledLine::new(format!("Allow it? C-c C-y yes{always} · C-c C-d no"), Face::Warning), None));
        }
    }
    let total = body.len();
    let shown = if expanded {
        total
    } else if name == "bash" || result.is_none() || !side.steps.is_empty() {
        total.min(PREVIEW_LINES)
    } else {
        total.min(if name == "read" { 0 } else { 4 })
    };
    // Bash previews show the tail, everything else the head.
    let tail = name == "bash" || !side.steps.is_empty();
    let mut slice: Vec<(StyledLine, Option<Color32>)> = if tail && !expanded && total > shown { body[total - shown..].to_vec() } else { body[..shown].to_vec() };
    if total > shown {
        let more = (StyledLine::new(format!("{} more lines · TAB or click to expand", total - shown), Face::Meta), None);
        if tail {
            slice.insert(0, more);
        } else {
            slice.push(more);
        }
    }

    let n = slice.len() + 1;
    let mut head = StyledLine::new(header, Face::ToolArgs).spans(spans).block(card.nth(0, n)).indent(12.0 * k).gap(gap).pad(7.0 * k, 7.0 * k);
    if n > 1 {
        head.rule_below = Some(t.border);
    }
    let mut lines = vec![head];
    for (i, (l, tint)) in slice.into_iter().enumerate() {
        let blk = Block { bg: tint.map(|c| blend(t.tool_bg, c)).unwrap_or(t.tool_bg), ..card.nth(i + 1, n) };
        let mut l = l.block(blk).indent(12.0 * k);
        if i == 0 {
            l.pad_top = 6.0 * k;
        }
        if i + 2 == n {
            l.pad_bottom = 8.0 * k;
        }
        lines.push(l);
    }
    lines
}

/// `over` (translucent) painted on `base`, as an opaque color.
fn blend(base: Color32, over: Color32) -> Color32 {
    let a = over.a() as f32 / 255.0;
    let mix = |b: u8, o: u8| (b as f32 * (1.0 - a) + o as f32).round().min(255.0) as u8;
    Color32::from_rgb(mix(base.r(), over.r()), mix(base.g(), over.g()), mix(base.b(), over.b()))
}

fn tool_text(r: &ToolResultMessage) -> String {
    r.content
        .iter()
        .filter_map(|c| match c {
            UserContent::Text { text } => Some(text.clone()),
            UserContent::Image { .. } => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// An image waiting in the composer.
#[derive(Debug, Clone)]
pub struct Attachment {
    pub image: crate::images::ImageRef,
    pub encoded: crate::images::Encoded,
    /// Where it came from (a file name, or "pasted image").
    pub name: String,
}

/// `agent-model` var, then pi's settings (`defaultProvider`/`defaultModel`), then any model with credentials.
pub fn default_model(ed: &Editor) -> Option<Model> {
    serpis_chat::default_model(&ed.registry, ed.var_str("agent-model").as_deref())
}

/// `agent-thinking`, then pi's `defaultThinkingLevel`, else medium.
pub fn default_thinking(ed: &Editor) -> ThinkingLevel {
    ed.var_str("agent-thinking")
        .and_then(|s| ThinkingLevel::parse(&s))
        .or_else(|| pi_setting("defaultThinkingLevel").and_then(|s| ThinkingLevel::parse(&s)))
        .unwrap_or(ThinkingLevel::Medium)
}

/// A chat on this machine wrote its session for the first time, in `cwd`.
fn session_created(ed: &mut Editor, cwd: &std::path::Path) {
    // A project you have chatted in stays in the sidebar with its sessions.
    if ed.var("project-remember").and_then(Value::as_bool).unwrap_or(true) {
        if let Some(root) = ed.project_root_of(cwd) {
            crate::sidebar::add_project(ed, &root);
        }
    }
    crate::session_ui::rescan(ed);
}

pub use serpis_chat::pi_setting;

/// Runs `before-tool-call` / `after-tool-call` editor hooks on the UI thread.
struct Lines<'a>(&'a [StyledLine]);
impl LineSource for Lines<'_> {
    fn len(&self) -> usize {
        self.0.len()
    }
    fn styled(&self, i: usize) -> StyledLine {
        self.0[i].clone()
    }
    fn hash(&self, i: usize) -> u64 {
        <[StyledLine] as LineSource>::hash(self.0, i)
    }
}

impl Buffer for ChatBuffer {
    fn kind(&self) -> &str {
        "chat"
    }

    fn poll(&mut self, ed: &mut Editor) {
        self.node.flush(ed);
        if self.running() {
            // Keep elapsed timers ticking.
            self.dirty = true;
            ed.ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, focused: bool, ed: &mut Editor) {
        let show = (ed.var("chat-cache-indicator").and_then(Value::as_bool).unwrap_or(true), ed.var("chat-cache-status").and_then(Value::as_bool).unwrap_or(true));
        if show != self.show_cache {
            self.show_cache = show;
            self.dirty = true;
        }
        if show.1 && self.cache_json()["state"] == "warm" {
            ui.ctx().request_repaint_after(Duration::from_secs(30));
        }
        // A centered reading column; tables are laid out for its width.
        let k = ed.theme.ui_scale();
        self.view.padding = egui::vec2(24.0 * k, 18.0 * k);
        self.view.max_width = ed.var("chat-max-width").and_then(Value::as_f64).unwrap_or(860.0) as f32 * k;
        let text_w = (rect.width() - 2.0 * self.view.padding.x).min(self.view.max_width).max(40.0);
        if (text_w - self.text_w).abs() > 1.0 {
            self.text_w = text_w;
            self.dirty = true;
        }
        if ed.var_bool("chat-show-thinking") != self.show_thinking {
            // C-t (chat-toggle-thinking) shows or hides all of it, including what was folded by hand.
            self.show_thinking = !self.show_thinking;
            for v in &mut self.views {
                v.thinking = None;
            }
        }
        let show_thinking = self.show_thinking as u64;
        let style_key = ed.theme.layout_key() ^ ed.theme_generation.rotate_left(32) ^ show_thinking.wrapping_mul(0x9e37_79b9_7f4a_7c15);
        if style_key != self.style_key {
            self.style_key = style_key;
            self.dirty = true;
        }
        if self.dirty {
            self.rebuild(ed);
        }
        let theme = ed.theme.clone();
        let id = ed.current_buffer();
        let shape = if focused { ed.cursor_shape(id) } else { None }.unwrap_or(CursorShape::Bar);

        // The composer: a floating card on the reading column, growing with its text up to 40%
        // of the window, with a toolbar (model, thinking, usage, send).
        let k = theme.ui_scale();
        let toolbar_h = (38.0 * k).round();
        self.prompt.view.padding = egui::vec2(16.0 * k, 12.0 * k);
        let col_w = (rect.width() - 2.0 * self.view.padding.x).min(self.view.max_width).max(80.0);
        // As tall as the text needs once wrapped, up to 40% of the window; beyond that it scrolls.
        let src = crate::text::PlainLines { lines: &self.prompt.doc.lines, face: Face::UserText };
        let one_line = ui.fonts_mut(|f| f.row_height(&theme.prop())).max((theme.font_size * 1.55).round()) + 2.0 * self.prompt.view.padding.y;
        let content_h = self.prompt.view.measure(ui, col_w, &src, &theme);
        let text_h = content_h.min((rect.height() * 0.4 - toolbar_h).max(one_line)).max(one_line);
        // Attached images: a row of thumbnails above the text.
        let thumb_h = (56.0 * k).round();
        let att_h = if self.attachments.is_empty() { 0.0 } else { thumb_h + 14.0 * k };
        let left = rect.center().x - col_w / 2.0;
        let card = Rect::from_min_max(egui::pos2(left, rect.bottom() - 14.0 * k - text_h - toolbar_h - att_h), egui::pos2(left + col_w, rect.bottom() - 14.0 * k));
        // A tool call waiting for the person: a bar above the composer to answer it.
        let waiting = self.waiting();
        // Else stopped at a usage limit: a bar that says until when, and what happens then.
        let held = if waiting.is_none() { self.held_at_limit() } else { None };
        let barred = waiting.is_some() || held.is_some();
        let ask_h = if barred { (40.0 * k).round() } else { 0.0 };
        let ask_bar = Rect::from_min_max(egui::pos2(left, card.top() - 8.0 * k - ask_h), egui::pos2(left + col_w, card.top() - 8.0 * k));
        let transcript_rect = Rect::from_min_max(rect.min, egui::pos2(rect.right(), card.top() - 6.0 * k - if barred { ask_h + 8.0 * k } else { 0.0 }));

        // Transcript.
        let t_focus = focused && self.focus == Focus::Transcript;
        let selection = if focused { crate::packages::vim::selection_kind(ed, id) } else { Default::default() };
        let resp = self.view.show(ui, transcript_rect, &Lines(&self.lines), &theme, &ViewOptions {
            focused: t_focus,
            cursor: shape,
            show_cursor: t_focus,
            selection,
        });
        if resp.clicked {
            self.focus = Focus::Transcript;
            if let Some(l) = resp.clicked_line {
                self.toggle_fold_at(l);
            }
        }

        let painter = ui.painter().clone();
        let p_focus = focused && self.focus == Focus::Prompt;
        let radius = (14.0 * k).round() as u8;
        // The transcript fades out behind the composer instead of ending at a hard edge.
        let fade_h = 18.0 * k;
        for i in 0..6 {
            let a = (i + 1) as f32 / 6.0;
            let band = Rect::from_min_max(egui::pos2(rect.left() + 1.0, card.top() - fade_h + fade_h * i as f32 / 6.0), egui::pos2(rect.right() - 6.0, card.top() - fade_h + fade_h * (i + 1) as f32 / 6.0));
            painter.rect_filled(band, 0.0, crate::chrome::tint(theme.bg_alt, a * 0.9));
        }
        painter.rect_filled(Rect::from_min_max(egui::pos2(rect.left() + 1.0, card.top()), egui::pos2(rect.right() - 6.0, rect.bottom() - 1.0)), 0.0, theme.bg_alt);
        let shadow = egui::epaint::Shadow { offset: [0, 6], blur: 22, spread: 0, color: Color32::from_black_alpha(if theme.dark { 90 } else { 28 }) };
        painter.add(shadow.as_shape(card, radius));
        painter.rect_filled(card, radius, theme.bg_elevated);
        let outline = if p_focus { crate::chrome::tint(theme.accent, 0.6) } else { theme.border };
        painter.rect_stroke(card, radius, Stroke::new(1.0, outline), egui::StrokeKind::Inside);

        let mut hits: Vec<(&'static str, Rect)> = Vec::new();
        let mut remove = None;
        let mut x = card.left() + 14.0 * k;
        for (i, a) in self.attachments.iter().enumerate() {
            let size = a.image.fit(thumb_h * 2.0, thumb_h);
            let r = Rect::from_min_size(egui::pos2(x, card.top() + 12.0 * k), size);
            if r.right() > card.right() - 14.0 * k {
                break;
            }
            crate::images::paint(&painter, a.image.id, r, 6.0 * k, theme.bg_alt);
            painter.rect_stroke(r, 6.0 * k, Stroke::new(1.0, theme.border), egui::StrokeKind::Inside);
            // Remove: a small × on the corner.
            let xr = Rect::from_center_size(egui::pos2(r.right() - 2.0 * k, r.top() + 2.0 * k), egui::vec2(16.0 * k, 16.0 * k));
            let resp = ui.interact(xr, ui.scope_id().with(("attachment-remove", i)), egui::Sense::click()).on_hover_text(format!("Remove {}", a.name));
            let tip = ui.interact(r, ui.scope_id().with(("attachment", i)), egui::Sense::hover());
            let _ = tip.on_hover_text(format!("{} · {}×{}", a.name, a.image.w, a.image.h));
            painter.circle_filled(xr.center(), 8.0 * k, if resp.hovered() { theme.fg } else { theme.fg_dim });
            let (c, d) = (xr.center(), 3.0 * k);
            for (p0, p1) in [(c + egui::vec2(-d, -d), c + egui::vec2(d, d)), (c + egui::vec2(-d, d), c + egui::vec2(d, -d))] {
                painter.line_segment([p0, p1], Stroke::new(1.5 * k, theme.bg_elevated));
            }
            if resp.clicked() {
                remove = Some(i);
            }
            hits.push((["attachment-0", "attachment-1", "attachment-2", "attachment-3"].get(i).copied().unwrap_or("attachment"), r));
            hits.push((["attachment-0-remove", "attachment-1-remove", "attachment-2-remove", "attachment-3-remove"].get(i).copied().unwrap_or("attachment-remove"), xr));
            x = r.right() + 10.0 * k;
        }
        if let Some(i) = remove {
            self.attachments.remove(i);
            self.focus = Focus::Prompt;
        }
        let text_rect = Rect::from_min_max(egui::pos2(card.left(), card.top() + att_h), egui::pos2(card.right(), card.bottom() - toolbar_h));
        let src = crate::text::PlainLines { lines: &self.prompt.doc.lines, face: Face::UserText };
        let presp = self.prompt.view.show(ui, text_rect, &src, &theme, &ViewOptions {
            focused: p_focus,
            cursor: shape,
            show_cursor: p_focus,
            selection,
        });
        if presp.clicked {
            self.focus = Focus::Prompt;
        }
        if self.prompt.doc.is_empty() {
            let hint = if self.running() { "Steer the agent…" } else { "Ask anything…" };
            painter.text(text_rect.min + self.prompt.view.padding + egui::vec2(2.0, 0.0), egui::Align2::LEFT_TOP, hint, theme.prop(), theme.fg_faint);
        }

        // Toolbar. Clicks run commands after this frame (the buffer is busy drawing itself).
        let bar = Rect::from_min_max(egui::pos2(card.left() + 8.0 * k, card.bottom() - toolbar_h), egui::pos2(card.right() - 8.0 * k, card.bottom() - 4.0 * k));
        let cy = bar.center().y;
        let small = egui::FontId::proportional(theme.font_size - 2.0);
        let id = ui.scope_id().with("composer");
        let mut run: Option<&'static str> = None;
        let pill = |ui: &mut egui::Ui, x: f32, key: &str, label: &str, tip: &str, icon: Option<Color32>| -> (Rect, bool) {
            let w = crate::chrome::text_width(&painter, label, small.clone()) + if icon.is_some() { 30.0 } else { 20.0 } * k;
            let r = Rect::from_min_size(egui::pos2(x, cy - 13.0 * k), egui::vec2(w, 26.0 * k));
            let resp = ui.interact(r, id.with(key), egui::Sense::click()).on_hover_text(tip);
            if resp.hovered() {
                painter.rect_filled(r, 8.0 * k, crate::chrome::hover_bg(&theme));
            }
            let mut tx = r.left() + 10.0 * k;
            if let Some(c) = icon {
                crate::chrome::diamond(&painter, egui::pos2(tx + 4.0 * k, cy), 4.5 * k, c);
                tx += 14.0 * k;
            }
            crate::chrome::text(&painter, egui::pos2(tx, cy), label, small.clone(), if resp.hovered() { theme.fg } else { theme.fg_dim }, w);
            (r, resp.clicked())
        };
        let model = self.model_short().unwrap_or_else(|| "Choose a model".into());
        let (mr, clicked) = pill(ui, bar.left(), "model", &model, "Model (chat-select-model)", Some(theme.accent));
        if clicked {
            run = Some("chat-select-model");
        }
        let thinking = format!("Thinking · {}", self.thinking_level().as_str());
        let (tr, clicked) = pill(ui, mr.right() + 2.0 * k, "thinking", &thinking, "Thinking level (chat-select-thinking)", None);
        if clicked {
            run = Some("chat-select-thinking");
        }
        // Where the chat runs: this machine, or another one's node.
        let machine = match &self.node {
            r if r.local() && r.online => "local".to_string(),
            r if r.local() => "local · offline".to_string(),
            r if r.online => format!("⇄ {}", r.machine),
            r => format!("⇄ {} · offline", r.machine),
        };
        let (wr, clicked) = pill(ui, tr.right() + 2.0 * k, "machine", &machine, "Where this chat runs (chat-select-machine)", None);
        if clicked {
            run = Some("chat-select-machine");
        }
        // Whether a turn cut off by its machine's node stopping goes on by itself.
        let (label, tip) = if self.keep_going() {
            ("↻ Keeps going", "If its machine's serpis-node stops mid-turn (a crash, a reboot), the turn continues when it starts again. Click to make it wait for you instead (chat-toggle-keep-going; default: chat-keep-going)")
        } else {
            ("⏸ Waits", "If its machine's serpis-node stops mid-turn (a crash, a reboot), the turn waits for you to send a message. Click to make it keep going (chat-toggle-keep-going; default: chat-keep-going)")
        };
        let (mut kr, clicked) = pill(ui, wr.right() + 2.0 * k, "keep-going", label, tip, None);
        if clicked {
            run = Some("chat-toggle-keep-going");
        }
        // Claude Code's permission mode, as its own prompt line shows it.
        if let Some(mode) = self.node.state.permission_mode.clone() {
            let (label, color) = permission_mode_label(&mode, &theme);
            let w = crate::chrome::text_width(&painter, &label, small.clone()) + 20.0 * k;
            let r = Rect::from_min_size(egui::pos2(kr.right() + 2.0 * k, cy - 13.0 * k), egui::vec2(w, 26.0 * k));
            let resp = ui.interact(r, id.with("permission-mode"), egui::Sense::click()).on_hover_text("Claude Code's permission mode: S-TAB cycles it (chat-cycle-permission-mode), a click picks one (chat-select-permission-mode)");
            if resp.hovered() {
                painter.rect_filled(r, 8.0 * k, crate::chrome::hover_bg(&theme));
            }
            crate::chrome::text(&painter, egui::pos2(r.left() + 10.0 * k, cy), &label, small.clone(), color, w);
            hits.push(("permission-mode", r));
            kr = r;
            if resp.clicked() {
                run = Some("chat-select-permission-mode");
            }
        }
        // Work the agent left running on the side (Claude Code's background shells, agents).
        let running = self.background_tasks();
        if !running.is_empty() {
            let label = format!("· {}", background_counts(&running));
            let mut tip: Vec<String> = running.iter().map(|t| format!("{}: {}", task_kind(t).0, t.description)).collect();
            tip.push("Running in the background. Click (or C-c C-k) to stop one (chat-stop-task)".into());
            let (br, clicked) = pill(ui, kr.right() + 2.0 * k, "background", &label, &tip.join("\n"), None);
            hits.push(("background", br));
            kr = br;
            if clicked {
                run = Some("chat-stop-task");
            }
        }
        // Close to a usage limit: how close, until it resets.
        if let Some(l) = self.limit_warning() {
            let label = format!("⚠ {}% of {}", (l.utilization.unwrap_or(0.0) * 100.0).round(), l.name());
            let tip = match l.resets_at {
                Some(t) => format!("Claude Code says this chat's account is close to its {}: it resets in {}. claude-code-usage shows all its limits", l.name(), crate::claude_usage::until(t as u64, unix_now() as u64)),
                None => format!("Claude Code says this chat's account is close to its {}. claude-code-usage shows all its limits", l.name()),
            };
            let w = crate::chrome::text_width(&painter, &label, small.clone()) + 20.0 * k;
            let r = Rect::from_min_size(egui::pos2(kr.right() + 2.0 * k, cy - 13.0 * k), egui::vec2(w, 26.0 * k));
            let resp = ui.interact(r, id.with("limit"), egui::Sense::click()).on_hover_text(tip);
            if resp.hovered() {
                painter.rect_filled(r, 8.0 * k, crate::chrome::hover_bg(&theme));
            }
            crate::chrome::text(&painter, egui::pos2(r.left() + 10.0 * k, cy), &label, small.clone(), theme.yellow, w);
            hits.push(("limit", r));
            kr = r;
            if resp.clicked() {
                run = Some("claude-code-usage");
            }
        }
        if let Some((limit, continues)) = &held {
            let r = ask_bar;
            painter.rect_filled(r, 10.0 * k, theme.bg_elevated);
            painter.rect_stroke(r, 10.0 * k, Stroke::new(1.0, crate::chrome::tint(theme.red, 0.7)), egui::StrokeKind::Inside);
            let cy = r.center().y;
            let mut x = r.right() - 8.0 * k;
            let (key, label, tip, main) = if continues.is_some() {
                ("limit-wait", "Don't go on", "Wait for you instead of going on by itself when the limit resets (chat-toggle-continue-after-limit)", false)
            } else {
                ("limit-go", "Go on at reset", "Go on by itself when the limit resets (chat-toggle-continue-after-limit; default: claude-code-continue-after-limit)", true)
            };
            let w = crate::chrome::text_width(&painter, label, small.clone()) + 22.0 * k;
            let b = Rect::from_min_size(egui::pos2(x - w, cy - 13.0 * k), egui::vec2(w, 26.0 * k));
            x = b.left() - 6.0 * k;
            let resp = ui.interact(b, id.with(key), egui::Sense::click()).on_hover_text(tip);
            let (bg, fg) = if main { (theme.accent, crate::chrome::on(theme.accent, &theme)) } else { (if resp.hovered() { crate::chrome::hover_bg(&theme) } else { theme.chip() }, theme.fg) };
            painter.rect_filled(b, 8.0 * k, if main && resp.hovered() { theme.accent.gamma_multiply(1.15) } else { bg });
            crate::chrome::text(&painter, egui::pos2(b.left() + 11.0 * k, cy), label, small.clone(), fg, w);
            hits.push((key, b));
            if resp.clicked() {
                run = Some("chat-toggle-continue-after-limit");
            }
            let label = limit_line(limit, *continues, unix_now());
            let lx = r.left() + 14.0 * k;
            crate::chrome::text(&painter, egui::pos2(lx, cy), &label, small.clone(), theme.fg, (x - lx - 8.0 * k).max(0.0));
            hits.push(("limit-bar", r));
            // Its countdown moves on.
            ui.ctx().request_repaint_after(Duration::from_secs(20));
        }
        if let Some((_, name, args, always)) = &waiting {
            let r = ask_bar;
            painter.rect_filled(r, 10.0 * k, theme.bg_elevated);
            painter.rect_stroke(r, 10.0 * k, Stroke::new(1.0, crate::chrome::tint(theme.yellow, 0.7)), egui::StrokeKind::Inside);
            let cy = r.center().y;
            // Right to left: allow (the main one), always, deny.
            let mut x = r.right() - 8.0 * k;
            let mut button = |ui: &mut egui::Ui, key: &'static str, label: &str, tip: &str, main: bool| -> bool {
                let w = crate::chrome::text_width(&painter, label, small.clone()) + 22.0 * k;
                let b = Rect::from_min_size(egui::pos2(x - w, cy - 13.0 * k), egui::vec2(w, 26.0 * k));
                x = b.left() - 6.0 * k;
                let resp = ui.interact(b, id.with(key), egui::Sense::click()).on_hover_text(tip);
                let (bg, fg) = if main { (theme.accent, crate::chrome::on(theme.accent, &theme)) } else { (if resp.hovered() { crate::chrome::hover_bg(&theme) } else { theme.chip() }, theme.fg) };
                painter.rect_filled(b, 8.0 * k, if main && resp.hovered() { theme.accent.gamma_multiply(1.15) } else { bg });
                crate::chrome::text(&painter, egui::pos2(b.left() + 11.0 * k, cy), label, small.clone(), fg, w);
                hits.push((key, b));
                resp.clicked()
            };
            let question = name == "AskUserQuestion";
            let plan = name == "ExitPlanMode";
            if plan {
                if button(ui, "allow", "Approve", "Approve the plan: it goes on asking before edits (C-c C-y; chat-approve-plan picks another mode)", true) {
                    run = Some("chat-allow");
                }
                if button(ui, "always", "Accept edits", "Approve the plan, and accept its edits from here (C-c C-a)", false) {
                    run = Some("chat-allow-always");
                }
                if button(ui, "deny", "Keep planning", "Say what should change; it plans on (C-c C-d)", false) {
                    run = Some("chat-keep-planning");
                }
            } else if question {
                if button(ui, "allow", "Answer", "Answer its questions in the minibuffer (C-c C-y)", true) {
                    run = Some("chat-answer-questions");
                }
                if button(ui, "deny", "Skip", "Don't answer: it goes on without (C-c C-d)", false) {
                    run = Some("chat-deny");
                }
            } else {
                if button(ui, "allow", "Allow", "Allow it once (C-c C-y)", true) {
                    run = Some("chat-allow");
                }
                if let Some(a) = always {
                    if button(ui, "always", "Always", &format!("Allow it, and {a} from now on (C-c C-a)"), false) {
                        run = Some("chat-allow-always");
                    }
                }
                if button(ui, "deny", "Deny", "Refuse it (C-c C-d; chat-deny with a reason tells the agent why)", false) {
                    run = Some("chat-deny");
                }
            }
            let summary = serpis_chat::tool_summary(name, args);
            let label = if plan {
                "Claude Code's plan is ready".to_string()
            } else if question {
                format!("Claude Code asks: {summary}")
            } else if summary.is_empty() {
                format!("Allow {name}?")
            } else {
                format!("Allow {name} {summary}?")
            };
            let lx = r.left() + 14.0 * k;
            crate::chrome::text(&painter, egui::pos2(lx, cy), &label, small.clone(), theme.fg, (x - lx - 8.0 * k).max(0.0));
            hits.push(("ask", r));
        }
        // The activity and usage labels fit between these pills and the send button.
        let (think_r, tr) = (tr, kr);

        // Send (or stop) on the right.
        let d = 28.0 * k;
        let send = Rect::from_center_size(egui::pos2(bar.right() - d / 2.0, cy), Vec2::splat(d));
        let has_text = !self.prompt.doc.is_empty();
        let sresp = ui.interact(send, id.with("send"), egui::Sense::click()).on_hover_text(if self.running() { "Stop (C-c C-c)" } else { "Send (RET)" });
        if self.running() {
            painter.circle_filled(send.center(), d / 2.0, if sresp.hovered() { theme.fg } else { theme.fg_dim });
            painter.rect_filled(Rect::from_center_size(send.center(), Vec2::splat(9.0 * k)), 2.0, theme.bg_elevated);
            if sresp.clicked() {
                run = Some("chat-abort");
            }
        } else {
            let bg = if has_text { if sresp.hovered() { theme.accent.gamma_multiply(1.15) } else { theme.accent } } else { theme.chip() };
            painter.circle_filled(send.center(), d / 2.0, bg);
            let fg = if has_text { crate::chrome::on(theme.accent, &theme) } else { theme.fg_faint };
            crate::chrome::arrow_up(&painter, Rect::from_center_size(send.center(), Vec2::splat(11.0 * k)), fg);
            if sresp.clicked() && has_text {
                run = Some("chat-send");
            }
        }

        // Activity or usage between the pills and the send button.
        let right = send.left() - 12.0 * k;
        let mut right = right;
        if self.running() {
            let label = format!("Working {}", fmt_duration(self.started.map(|s| s.elapsed()).unwrap_or_default(), false));
            let w = crate::chrome::text_width(&painter, &label, small.clone());
            if right - w - 18.0 * k > tr.right() {
                crate::chrome::text(&painter, egui::pos2(right - w, cy), &label, small.clone(), theme.accent, w + 1.0);
                crate::chrome::spinner(&painter, egui::pos2(right - w - 11.0 * k, cy), 5.0 * k, ui.input(|i| i.time), theme.accent);
                right -= w + 30.0 * k;
            }
            ui.ctx().request_repaint_after(Duration::from_millis(60));
        }
        // Session usage and context, like pi's footer.
        if ed.var("chat-show-usage").and_then(Value::as_bool).unwrap_or(true) {
            let label = self.usage_label();
            let w = crate::chrome::text_width(&painter, &label, small.clone());
            if right - w > tr.right() + 8.0 * k {
                crate::chrome::text(&painter, egui::pos2(right - w, cy), &label, small.clone(), theme.fg_faint, w + 1.0);
            }
        }
        hits.extend([("model", mr), ("thinking", think_r), ("machine", wr), ("keep-going", tr), ("send", send), ("card", card)]);
        self.composer_hits = hits;
        if let Some(cmd) = run {
            self.focus = Focus::Prompt;
            ed.main_sender().send(move |ed| ed.call(cmd));
        }
    }

    /// Images attach to the next message; other files go into the prompt as paths.
    fn handle_files(&mut self, paths: &[std::path::PathBuf], ed: &mut Editor) -> bool {
        let mut others = Vec::new();
        for p in paths {
            if crate::images::is_image_path(p) {
                if let Err(e) = self.attach_file(p) {
                    ed.error(format!("{e:#}"));
                }
            } else {
                others.push(p.to_string_lossy().into_owned());
            }
        }
        if !others.is_empty() {
            self.handle_text(&others.join(" "), ed);
        }
        self.dirty = true;
        true
    }

    fn handle_text(&mut self, text: &str, _ed: &mut Editor) -> bool {
        if self.focus != Focus::Prompt {
            self.focus = Focus::Prompt;
            self.prompt.move_to_end();
        }
        crate::commands::insert_replacing_selection(&mut self.prompt, text);
        true
    }

    fn surface(&mut self) -> Option<&mut dyn TextSurface> {
        Some(self)
    }

    fn surface_ref(&self) -> Option<&dyn TextSurface> {
        Some(self)
    }

    fn modeline(&self) -> Modeline {
        // The model: clicking it chooses another, as the composer's model pill does.
        let model = self.model_short().unwrap_or_else(|| "Choose a model".into());
        let mut status = vec![model];
        let mut status_commands = vec![Some("chat-select-model".into())];
        // Where it runs: clicking it chooses another machine (for a chat with no messages yet).
        let r = &self.node;
        if !r.local() {
            status.insert(0, if r.online { format!("⇄ {}", r.machine) } else { format!("⇄ {} · offline", r.machine) });
            status_commands.insert(0, Some("chat-select-machine".into()));
        }
        // The cache: clicking it lists its recent misses.
        if let Some(c) = self.state_now().cache.filter(|_| self.show_cache.1) {
            status.insert(0, c);
            status_commands.insert(0, Some("chat-cache-report".into()));
        }
        Modeline { title: self.state_now().title, status, status_commands, busy: self.running() }
    }

    fn activity(&self) -> Activity {
        let s = self.state_now();
        if s.compacting {
            return Activity::new(ActivityState::Working, "compacting");
        }
        if s.running {
            return Activity { since: self.started, ..Activity::new(ActivityState::Working, s.step.unwrap_or_else(|| "working".into())) };
        }
        if !self.node.online {
            return Activity::new(ActivityState::Error, "offline");
        }
        // Stopped at a usage limit isn't a failure: it says until when, and what then.
        if let Some((limit, continues)) = self.held_at_limit() {
            return Activity::new(ActivityState::Idle, limit_status(&limit, continues, unix_now()));
        }
        match &s.last_error {
            Some(e) => Activity::new(ActivityState::Error, e.lines().next().unwrap_or("")),
            // Idle chats say which model they talk to.
            None => Activity::new(ActivityState::Idle, self.model_short().as_deref().unwrap_or("no model")),
        }
    }

    fn on_kill(&mut self, ed: &mut Editor) {
        // Closing the view leaves a running chat running (remote-close-chat stops it); an idle
        // one nobody else shows is closed, its session kept to resume.
        self.node.post(Request::Detach { chat: self.node.chat.clone(), close: true });
        self.node.flush(ed);
    }

    fn directory(&self) -> Option<std::path::PathBuf> {
        let cwd = &self.node.state.cwd;
        (!cwd.is_empty()).then(|| std::path::PathBuf::from(cwd))
    }

    fn machine(&self) -> Option<String> {
        (!self.node.local()).then(|| self.node.machine.clone())
    }

    fn parts(&self) -> usize {
        2
    }
    fn part(&self) -> usize {
        usize::from(self.focus == Focus::Prompt)
    }
    fn set_part(&mut self, part: usize) {
        self.focus = if part == 0 { Focus::Transcript } else { Focus::Prompt };
    }

    fn full_text(&mut self) -> Option<String> {
        let mut t: Vec<String> = self.lines.iter().map(|l| l.text.clone()).collect();
        t.extend(self.prompt.doc.lines.iter().cloned());
        Some(t.join("\n"))
    }

    impl_any!();
}

/// The surface is the part that has focus: the prompt while typing, the transcript while
/// reading. Motions stay inside it (`gg`, `k` on the first line, visual selections); switching
/// is explicit (`chat-focus-transcript`, `chat-focus-prompt`, a click, or entering insert state
/// from the transcript).
impl TextSurface for ChatBuffer {
    fn line_count(&self) -> usize {
        match self.focus {
            Focus::Transcript => self.t_len(),
            Focus::Prompt => self.prompt.line_count(),
        }
    }
    fn line(&self, i: usize) -> String {
        match self.focus {
            Focus::Transcript => self.lines.get(i).map(|l| l.text.clone()).unwrap_or_default(),
            Focus::Prompt => self.prompt.line(i),
        }
    }
    fn cursor(&self) -> Pos {
        match self.focus {
            Focus::Transcript => self.view.cursor,
            Focus::Prompt => self.prompt.cursor(),
        }
    }
    fn set_cursor(&mut self, p: Pos) {
        match self.focus {
            Focus::Prompt => self.prompt.set_cursor(p),
            Focus::Transcript => {
                let t = self.t_len();
                let line = p.line.min(t.saturating_sub(1));
                let col = p.col.min(self.lines.get(line).map(|l| char_len(&l.text)).unwrap_or(0));
                self.view.cursor = Pos::new(line, col);
                self.view.reveal_cursor();
                // Moving up into history stops auto-follow; reaching the end resumes it.
                self.view.follow_tail = line + 1 >= t;
            }
        }
    }
    fn anchor(&self) -> Option<Pos> {
        match self.focus {
            Focus::Transcript => self.view.anchor,
            Focus::Prompt => self.prompt.anchor(),
        }
    }
    fn set_anchor(&mut self, a: Option<Pos>) {
        match self.focus {
            Focus::Transcript => self.view.anchor = a,
            Focus::Prompt => self.prompt.set_anchor(a),
        }
    }
    fn editable_at(&self, _p: Pos) -> bool {
        self.focus == Focus::Prompt
    }
    fn focus_editable(&mut self) -> bool {
        self.focus = Focus::Prompt;
        true
    }
    fn insert(&mut self, at: Pos, text: &str) -> Pos {
        match self.focus {
            Focus::Prompt => self.prompt.insert(at, text),
            Focus::Transcript => at,
        }
    }
    fn delete(&mut self, from: Pos, to: Pos) -> String {
        match self.focus {
            Focus::Prompt => self.prompt.delete(from, to),
            Focus::Transcript => String::new(),
        }
    }
    fn undo(&mut self) -> bool {
        self.prompt.undo()
    }
    fn redo(&mut self) -> bool {
        self.prompt.redo()
    }
    fn undo_boundary(&mut self) {
        self.prompt.undo_boundary();
    }
    fn visible_lines(&self) -> Range<usize> {
        match self.focus {
            Focus::Transcript => self.view.visible_lines(),
            Focus::Prompt => self.prompt.visible_lines(),
        }
    }
    fn screen_rect(&self, p: Pos) -> Option<Rect> {
        match self.focus {
            Focus::Transcript => self.view.screen_rect(p),
            Focus::Prompt => self.prompt.screen_rect(p),
        }
    }
    fn code_spans(&self, i: usize) -> Vec<(usize, usize)> {
        let Some(l) = self.lines.get(i).filter(|_| self.focus == Focus::Transcript) else { return Vec::new() };
        let col = |b: usize| l.text[..b.min(l.text.len())].chars().count();
        l.spans.iter().filter(|(_, f)| *f == Face::InlineCode).map(|(r, _)| (col(r.start), col(r.end))).filter(|(a, b)| b > a).collect()
    }
    fn display_move(&self, p: Pos, rows: isize, goal_x: Option<f32>) -> Option<(Pos, f32)> {
        match self.focus {
            Focus::Transcript => self.view.display_move(p, rows, goal_x),
            Focus::Prompt => self.prompt.display_move(p, rows, goal_x),
        }
    }
    fn reveal_cursor(&mut self) {
        match self.focus {
            Focus::Transcript => self.view.reveal_cursor(),
            Focus::Prompt => self.prompt.reveal_cursor(),
        }
    }
}

/// The tools a chat's agent gets: the built-in ones, then extension tools (which replace
/// built-ins of the same name), minus agent-disabled-tools.
/// Whether to warn about `action` throwing away a cache worth `cost` dollars (cache-warn,
/// cache-warn-actions, cache-warn-threshold).
pub fn cache_warning_applies(ed: &mut Editor, action: &str, cost: f64) -> bool {
    serpis_chat::cache_warning_applies(&|name: &str| ed.var(name).cloned(), action, cost)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_lines_find_their_code() {
        assert_eq!(diff_code_start("+12 let x = 1;"), Some(4));
        assert_eq!(diff_code_start("- 3 }"), Some(4));
        assert_eq!(diff_code_start("  7 fn f()"), Some(4));
        assert_eq!(diff_code_start("   ..."), None);
        assert_eq!(diff_code_start("@@ hunk"), None);
    }

    #[test]
    fn durations_read_at_a_glance() {
        let f = |s: f32, tenths| fmt_duration(Duration::from_secs_f32(s), tenths);
        assert_eq!(f(4.25, true), "4.2s");
        assert_eq!(f(4.25, false), "4s");
        assert_eq!(f(28.0, true), "28s");
        assert_eq!(f(700.0, false), "11m 40s");
        assert_eq!(f(3600.0 * 2.0 + 300.0, false), "2h 05m");
    }

    /// Lines that belong to the table (they carry a bordered card).
    fn table(lines: &[StyledLine]) -> Vec<&StyledLine> {
        lines.iter().filter(|l| l.block.is_some_and(|b| b.border.is_some())).collect()
    }

    #[test]
    fn tables_measure_columns() {
        let t = crate::theme::Theme::default();
        let m = Measure::Chars(8.0);
        let md = "Results:\n\n| Run | Lead |\n|---|--:|\n| 1 connection, 60s | **91%** |\n| 3 | `x` |\n\nafter";
        let lines = markdown_lines(md, &t, &m, 2000.0);
        let rows = table(&lines);
        assert_eq!(rows.len(), 3, "header and two rows, no ASCII rules");
        assert_eq!(rows[0].text, "Run Lead");
        assert_eq!(rows[0].face, Face::Bold);
        assert_eq!(rows[1].text, "1 connection, 60s 91%");
        // Column 2 starts at the same x in every row: 17 chars of column 1 plus the gap.
        let x2 = |l: &StyledLine, at: usize| -> f32 { l.pads.iter().filter(|(o, _)| *o <= at).map(|(_, w)| *w).sum::<f32>() + l.text[..at].chars().count() as f32 * 8.0 };
        let col2 = 17.0 * 8.0 + 22.0;
        assert_eq!(x2(rows[0], 4), col2, "Lead starts the second column");
        assert_eq!(x2(rows[1], 18), col2 + 8.0, "91% right-aligned under Lead");
        // Cell markup is kept as spans; rows end with hairlines; the card has rounded ends.
        assert!(rows[1].spans.iter().any(|(r, f)| *f == Face::Bold && &rows[1].text[r.clone()] == "91%"));
        assert!(rows[2].spans.iter().any(|(r, f)| *f == Face::InlineCode && &rows[2].text[r.clone()] == "x"));
        assert!(rows[0].block.unwrap().top && rows[2].block.unwrap().bottom);
        assert!(rows.iter().all(|l| l.rule_below.is_some()));
        assert!(lines.iter().any(|l| l.text == "after"));
        assert!(table(&markdown_lines("| not a table", &t, &m, 2000.0)).is_empty());
    }

    #[test]
    fn narrow_tables_wrap_inside_cells() {
        let t = crate::theme::Theme::default();
        let m = Measure::Chars(8.0);
        let md = "| Run | Result |\n|---|---|\n| 1 connection, 180s, Rust and Python together | ~1% / -90ms in both |\n| 3 | ok |";
        let rows = markdown_lines(md, &t, &m, 300.0);
        let rows = table(&rows);
        assert!(rows.len() > 3, "the long row wraps: {:?}", rows.iter().map(|l| &l.text).collect::<Vec<_>>());
        // Every physical line fits: its last cell ends inside the column.
        for l in &rows {
            let end = l.pads.iter().map(|(_, w)| *w).sum::<f32>() + l.text.chars().count() as f32 * 8.0;
            assert!(end <= 300.0 - 24.0 + 0.5, "{} ends at {end}", l.text);
        }
        assert!(rows.iter().any(|l| l.text.contains("together")));
    }

    #[test]
    fn lists_hang_their_markers() {
        let t = crate::theme::Theme::default();
        let lines = markdown_lines("- one\n- two\n  - nested\n3. three", &t, &Measure::Chars(8.0), 800.0);
        let markers: Vec<&str> = lines.iter().filter_map(|l| l.marker.as_ref().map(|(m, _)| m.as_str())).collect();
        assert_eq!(markers, ["•", "•", "•", "3."]);
        assert_eq!(lines[0].text, "one");
        assert!(lines[2].indent > lines[1].indent);
    }
}
