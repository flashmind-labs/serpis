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

use std::collections::HashMap;
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
    /// The review of what the agent wrote ([`crate::chat_review`]), read once for this chat.
    review: crate::chat_review::Saved,
    /// The chat id the review was read for (a restored chat may start under another).
    review_key: Option<String>,
    /// A comment being written in the composer (the prompt holds its text), and what was being
    /// typed before it, put back after.
    commenting: Option<(crate::chat_review::Writing, String)>,
    /// The review's comments listed in the composer.
    review_open: bool,
    /// Transcript lines that show a comment: (line, its id), to change it on a click.
    comment_lines: Vec<(usize, u64)>,
    /// Where `find:<i>` links go: (text to find, entry, after it rather than before it).
    find_targets: Vec<(String, usize, bool)>,
    /// The comment text last saved while writing, and when.
    writing_saved: (String, Option<Instant>),
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
/// How much of a command with no description a closed card's one line shows.
const COMPACT_COMMAND_CHARS: usize = 100;

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
            review: Default::default(),
            review_key: None,
            commenting: None,
            review_open: false,
            comment_lines: Vec::new(),
            find_targets: Vec::new(),
            writing_saved: (String::new(), None),
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
    /// What runs in the foreground now: tool calls waiting on a command or subagent (Claude
    /// Code's), which can move to the background. (tool id, task.)
    pub fn foreground_tasks(&self) -> Vec<(String, serpis_proto::Task)> {
        self.entries
            .iter()
            .filter_map(|e| match e {
                Entry::Tool { id, result: None, task: Some(t), .. } if t.running() && !t.background => Some((id.clone(), (**t).clone())),
                _ => None,
            })
            .collect()
    }

    pub fn background_task(&mut self, tool: &str) {
        self.node.post(Request::BackgroundTask { chat: self.node.chat.clone(), tool: tool.to_string() });
    }

    /// Show task `task` (one of [`Self::background_tasks`]): its card (or the subagent's it
    /// runs in) opened in the transcript, with the cursor on it.
    pub fn show_task(&mut self, task: &str, ed: &Editor) -> bool {
        let has = |e: &Entry| e.walk().into_iter().any(|e| matches!(e, Entry::Tool { task: Some(t), .. } if t.id == task));
        let Some(ei) = self.entries.iter().position(has) else { return false };
        if let Some(v) = self.views.get_mut(ei) {
            v.expanded = Some(true);
        }
        self.dirty = true;
        self.rebuild(ed);
        let Some(line) = self.line_entry.iter().position(|e| *e == Some(ei)) else { return false };
        self.focus = Focus::Transcript;
        self.view.cursor = Pos::new(line, 0);
        self.view.anchor = None;
        self.view.reveal_cursor();
        true
    }

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

    /// The text of the agent's last reply (what a notification quotes).
    pub fn last_reply(&self) -> Option<String> {
        self.entries.iter().rev().find_map(|e| match e {
            Entry::Assistant { blocks, .. } => {
                let text: Vec<&str> = blocks.iter().filter_map(|b| if let AssistantContent::Text { text, .. } = b { Some(text.trim()) } else { None }).filter(|t| !t.is_empty()).collect();
                (!text.is_empty()).then(|| text.join(" "))
            }
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
        // Writing a comment: sending saves it to the review.
        if self.commenting.is_some() {
            self.save_comment(ed);
            return;
        }
        let text = self.prompt.text().trim().to_string();
        let review = !self.review.drafts().is_empty();
        if text.is_empty() && self.attachments.is_empty() && !review {
            return;
        }
        self.prompt.clear();
        if !text.is_empty() {
            self.prompt_history.push(text.clone());
        }
        self.history_index = None;
        // A review waiting: it goes with what was typed, as one message.
        let text = if review {
            let message = crate::chat_review::prompt(&self.review.drafts(), &text);
            for c in self.review.comments.iter_mut().filter(|c| c.state == crate::chat_review::State::Draft) {
                c.state = crate::chat_review::State::Sent;
            }
            self.review_open = false;
            self.dirty = true;
            self.persist_review(ed);
            message
        } else {
            text
        };
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
    /// cache reads and writes, cost (none when nothing was charged: a Claude Code or Codex
    /// subscription), then context used of the window.
    pub fn usage_label(&self) -> String {
        usage_label(&self.node.state.usage)
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

    /// Put the cursor in the composer.
    pub fn focus_prompt(&mut self) {
        self.focus = Focus::Prompt;
        self.dirty = true;
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
        let output_lines = ed.var("chat-task-output-lines").and_then(Value::as_u64).unwrap_or(8) as usize;
        let shell_steps = ed.var("chat-shell-steps").and_then(Value::as_bool).unwrap_or(true);
        let shell_steps_shown = ed.var("chat-shell-steps-shown").and_then(Value::as_u64).unwrap_or(6) as usize;
        let tool_compact = ed.var("chat-tool-compact").and_then(Value::as_bool).unwrap_or(true);
        let tool_spinner = ed.var("chat-tool-spinner").and_then(Value::as_bool).unwrap_or(true);
        let mut thinking_lines = Vec::new();
        // Where in-chat links go (a review's places, a reply): see `find_targets`.
        let mut find_targets: Vec<(String, usize, bool)> = Vec::new();
        let k = t.ui_scale();
        let m = Measure::Fonts(&ed.ctx, t);
        // A readable line for prose: chat-measure characters of the reading font, an average
        // character being a 26th of its lowercase alphabet (as in Bringhurst's copyfitting).
        let diagrams = ed.var_bool("chat-diagrams");
        let measure = ed.var("chat-measure").and_then(Value::as_f64).filter(|n| *n > 0.0).map(|n| n as f32 * m.width("abcdefghijklmnopqrstuvwxyz", Face::AssistantText) / 26.0);
        // The report each finished run ends with (chat-report): entry index → its words.
        let reports = if ed.var_bool("chat-report") {
            let min_words = ed.var("chat-report-min-words").and_then(Value::as_u64).unwrap_or(80) as usize;
            report_entries(&self.entries, self.running(), min_words)
        } else {
            HashMap::new()
        };
        // The report's surface: chat-report-background, a color or "none" (a rule above it
        // instead); unset, the theme's own (report_bg).
        let report_bg = match ed.var("chat-report-background") {
            Some(Value::String(s)) if s == "none" => None,
            Some(Value::String(s)) => crate::themes::parse_hex(s).or(Some(t.report_bg)),
            _ => Some(t.report_bg),
        };
        let report_scale = ed.var("chat-report-scale").and_then(Value::as_f64).filter(|s| *s > 0.0).unwrap_or(1.0) as f32;
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
                    // A review serpis sent (of the agent's text, or a diff page's): drawn as one.
                    match crate::chat_review::parse_sent(text) {
                        Some(sent) => lines.extend(review_bubble_lines(&sent, ei, &mut find_targets, k)),
                        None => lines.extend(text.lines().map(|l| StyledLine::new(l, Face::UserText))),
                    }
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
                    let report = reports.get(&ei).copied();
                    let report_start = out.len();
                    if let Some(words) = report {
                        // Set apart from the steps before it, on its own surface (or after a rule),
                        // headed by what it is and how long it takes to read (230 words a minute).
                        if report_bg.is_none() {
                            push(&mut out, &mut owner, StyledLine::new("", Face::Meta).rule(t.border).gap(30.0 * k).pad(0.0, 14.0 * k), Some(ei));
                        }
                        let minutes = words.div_ceil(230).max(1);
                        // Written to the report skill: its outcome as a pill, and what needs you.
                        let shape = blocks.iter().find_map(|b| if let AssistantContent::Text { text, .. } = b { shaped_report(text) } else { None });
                        let mut label = String::new();
                        let mut spans = Vec::new();
                        if let Some(sh) = &shape {
                            let (word, face) = sh.outcome.badge();
                            label.push_str(&format!(" {word} "));
                            spans.push((0..label.len(), face));
                            label.push_str("   ");
                        }
                        let at = label.len();
                        label.push_str(&format!("Report  ·  {}  ·  {words} words, about {minutes} min", short_model(model)));
                        spans.push((at..at + "Report".len(), Face::Accent));
                        if let Some(n) = shape.as_ref().map(|sh| sh.needs_you()).filter(|n| *n > 0) {
                            let at = label.len();
                            label.push_str(&format!("  ·  {n} need{} you", if n == 1 { "s" } else { "" }));
                            spans.push((at..label.len(), Face::MetaWarn));
                        }
                        let mut head = StyledLine::new(label, Face::Label).spans(spans).pad(0.0, 4.0 * k);
                        if report_bg.is_some() {
                            head = head.gap(30.0 * k);
                            head.pad_top = 16.0 * k;
                        }
                        push(&mut out, &mut owner, head, Some(ei));
                    } else if !continuation && (has_content || *streaming || error.is_some()) {
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
                                let scale = if report.is_some() { report_scale } else { 1.0 };
                                if let Some(sh) = report.and_then(|_| shaped_report(text)) {
                                    for l in report_lines(&sh, t, &m, self.text_w, report_bg.unwrap_or(t.bg_alt), measure, scale, diagrams) {
                                        push(&mut out, &mut owner, l, Some(ei));
                                    }
                                    continue;
                                }
                                for mut l in markdown_lines(text, t, &m, self.text_w, diagrams) {
                                    // Prose (not code, tables or cards) wraps at chat-measure.
                                    if is_prose(&l) {
                                        l.measure = measure.map(|w| w * scale);
                                        if scale != 1.0 {
                                            l.scale = Some(scale);
                                        }
                                    }
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
                    // A turn that goes on to tools has its usage in the composer's total, so a run
                    // of them reads as a list. (Its calls are entries of their own by now.)
                    let calls_tools = blocks.iter().any(|b| matches!(b, AssistantContent::ToolCall(_))) || (tool_compact && matches!(self.entries.get(ei + 1), Some(Entry::Tool { .. })));
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
                    // A report's lines, its usage included, make one panel.
                    if let (Some(_), Some(bg)) = (report, report_bg) {
                        let n = out.len() - report_start;
                        let panel = match t.report_border {
                            Some(c) => Block::new(bg).border(c),
                            None => Block::new(bg),
                        };
                        for (i, l) in out[report_start..].iter_mut().enumerate() {
                            l.backdrop = Some(panel.nth(i, n));
                        }
                        if let Some(l) = out.last_mut() {
                            l.pad_bottom += 16.0 * k;
                        }
                    }
                }
                Entry::Tool { name, args, partial, result, elapsed_ms, ask, task, steps, .. } => {
                    let gap = if prev_tool { 6.0 } else { 14.0 } * k;
                    // The request behind this call missed the cache: said by the card (below).
                    let cache_miss = match self.entries.get(ei + 1) {
                        Some(Entry::Info { text, .. }) if tool_compact && crate::cache_watch::is_miss_notice(text) => Some(text.as_str()),
                        _ => None,
                    };
                    let side = Side { task: task.as_deref(), steps, output_lines, shell_steps, shell_steps_shown, tool_compact, tool_spinner, cache_miss };
                    for l in tool_lines(name, args, partial.as_deref(), result.as_ref(), ask.as_ref(), side, v.expanded, v.started, elapsed_ms.map(Duration::from_millis), t, gap) {
                        push(&mut out, &mut owner, l, Some(ei));
                    }
                }
                // A run of tools stays a list: its card has a warning sign that says this.
                Entry::Info { text, .. } if tool_compact && prev_tool && crate::cache_watch::is_miss_notice(text) => continue,
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
        // Review comments: what they quote marked, each shown under it.
        self.comment_lines = self.place_comments(&mut out, &mut owner, &mut thinking_lines, &mut find_targets, t, k);
        self.find_targets = find_targets;
        // Places in the code are links, wherever they are in the chat.
        for l in &mut out {
            if l.image.is_none() {
                l.links.extend(code_links(&l.text));
            }
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

/// The composer's usage, [`ChatBuffer::usage_label`].
fn usage_label(u: &Usage) -> String {
    let mut parts = vec![format!("↑{}", fmt_tokens(u.input)), format!("↓{}", fmt_tokens(u.output))];
    if u.cache_read > 0 {
        parts.push(format!("R{}", fmt_tokens(u.cache_read)));
    }
    if u.cache_write > 0 {
        parts.push(format!("W{}", fmt_tokens(u.cache_write)));
    }
    if u.cost.total > 0.0 {
        parts.push(format!("${:.3}", u.cost.total));
    }
    parts.join(" ")
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

/// Inline markdown, as CommonMark reads it: `code` (any run of backticks), **bold** and
/// __bold__, *italic* and _italic_, ***both***, ~~struck~~, [a link](url) (its text), and
/// backslash escapes. They nest: code in bold stays code, and its backticks go. Markers are
/// removed. `_` never emphasizes inside a word (snake_case names stay whole), and a marker
/// with nothing to close it stays as written.
fn inline_spans(text: &str) -> (String, Vec<(Range<usize>, Face)>) {
    let mut out = String::with_capacity(text.len());
    let mut spans = Vec::new();
    inline_into(text, Emphasis::default(), &mut out, &mut spans);
    (out, spans)
}

#[derive(Clone, Copy, Default)]
struct Emphasis {
    bold: bool,
    italic: bool,
    strike: bool,
    link: bool,
}

impl Emphasis {
    fn face(self) -> Option<Face> {
        Some(match self {
            Emphasis { link: true, .. } => Face::Link,
            Emphasis { strike: true, .. } => Face::Strike,
            Emphasis { bold: true, italic: true, .. } => Face::BoldItalic,
            Emphasis { bold: true, .. } => Face::Bold,
            Emphasis { italic: true, .. } => Face::Italic,
            _ => return None,
        })
    }
}

/// Append `s` in `face`, joining the span before it when it has the same face.
fn emit(out: &mut String, spans: &mut Vec<(Range<usize>, Face)>, s: &str, face: Option<Face>) {
    let start = out.len();
    out.push_str(s);
    let Some(face) = face else { return };
    match spans.last_mut() {
        Some((r, f)) if *f == face && r.end == start => r.end = out.len(),
        _ => spans.push((start..out.len(), face)),
    }
}

/// The length of the run of `c` at the start of `s`.
fn run_of(s: &str, c: char) -> usize {
    s.chars().take_while(|x| *x == c).count()
}

/// Where the code span opened by the `n` backticks at the start of `s` ends: (its text, the
/// bytes it takes).
fn code_span(s: &str) -> Option<(&str, usize)> {
    let n = run_of(s, '`');
    let body = &s[n..];
    let mut at = 0;
    while let Some(i) = body[at..].find('`') {
        let i = at + i;
        let m = run_of(&body[i..], '`');
        if m == n {
            let code = &body[..i];
            // One space each side is padding (`` `x` ``), unless it's all spaces.
            let code = if code.len() >= 2 && code.starts_with(' ') && code.ends_with(' ') && !code.trim().is_empty() { &code[1..code.len() - 1] } else { code };
            return Some((code, n + i + m));
        }
        at = i + m;
    }
    None
}

/// Where the emphasis opened by `n` × `c` (just before `s`) closes: the byte offset in `s` of
/// a closing run, skipping code spans and escapes.
fn emphasis_close(s: &str, c: char, n: usize) -> Option<usize> {
    let mut i = 0;
    let mut prev: Option<char> = None;
    while i < s.len() {
        let rest = &s[i..];
        let ch = rest.chars().next().unwrap();
        if ch == '\\' && rest.len() > 1 {
            let skip = 1 + rest[1..].chars().next().map_or(0, char::len_utf8);
            prev = rest[1..].chars().next();
            i += skip;
            continue;
        }
        if ch == '`' {
            if let Some((_, len)) = code_span(rest) {
                i += len;
                prev = Some('`');
                continue;
            }
        }
        if ch == c {
            let m = run_of(rest, c);
            let after = rest[m..].chars().next();
            // Right-flanking: not after a space; `_` not inside a word.
            let closes = i > 0 && prev.is_some_and(|p| !p.is_whitespace()) && (c != '_' || !after.is_some_and(char::is_alphanumeric));
            if closes && (m == n || (m > n && n == 1 && m != 2) || (m >= 3 && n == 2)) {
                return Some(i);
            }
            i += m * c.len_utf8();
            prev = Some(c);
            continue;
        }
        prev = Some(ch);
        i += ch.len_utf8();
    }
    None
}

fn inline_into(text: &str, em: Emphasis, out: &mut String, spans: &mut Vec<(Range<usize>, Face)>) {
    let mut i = 0;
    let mut prev: Option<char> = None;
    while i < text.len() {
        let rest = &text[i..];
        let ch = rest.chars().next().unwrap();
        // An escaped punctuation mark is itself.
        if ch == '\\' {
            if let Some(n) = rest[1..].chars().next().filter(char::is_ascii_punctuation) {
                emit(out, spans, &rest[1..1 + n.len_utf8()], em.face());
                i += 1 + n.len_utf8();
                prev = Some(n);
                continue;
            }
        }
        if ch == '`' {
            if let Some((code, len)) = code_span(rest) {
                emit(out, spans, code, Some(Face::InlineCode));
                i += len;
                prev = Some('`');
                continue;
            }
            // Unmatched: the whole run as written.
            let m = run_of(rest, '`');
            emit(out, spans, &rest[..m], em.face());
            i += m;
            prev = Some('`');
            continue;
        }
        if ch == '[' {
            if let Some(close) = rest.find("](") {
                if let Some(end) = rest[close + 2..].find(')') {
                    if !rest[1..close].contains('[') {
                        inline_into(&rest[1..close], Emphasis { link: true, ..em }, out, spans);
                        i += close + 2 + end + 1;
                        prev = Some(')');
                        continue;
                    }
                }
            }
        }
        if rest.starts_with("~~") {
            if let Some(end) = rest[2..].find("~~").filter(|e| *e > 0) {
                inline_into(&rest[2..2 + end], Emphasis { strike: true, ..em }, out, spans);
                i += 2 + end + 2;
                prev = Some('~');
                continue;
            }
        }
        if ch == '*' || ch == '_' {
            let n = run_of(rest, ch);
            let after = rest[n..].chars().next();
            // Left-flanking: before something that isn't a space; `_` not inside a word.
            let opens = after.is_some_and(|a| !a.is_whitespace()) && (ch != '_' || !prev.is_some_and(char::is_alphanumeric));
            // The longest emphasis that closes: 3 markers both, 2 bold, 1 italic.
            let found = if opens { (1..=n.min(3)).rev().find_map(|k| emphasis_close(&rest[k..], ch, k).map(|end| (k, end))) } else { None };
            match found {
                Some((k, end)) => {
                    let inner = Emphasis { bold: em.bold || k >= 2, italic: em.italic || k != 2, ..em };
                    inline_into(&rest[k..k + end], inner, out, spans);
                    i += k + end + k;
                }
                // Not emphasis: the run as written.
                None => {
                    emit(out, spans, &rest[..n], em.face());
                    i += n;
                }
            }
            prev = Some(ch);
            continue;
        }
        emit(out, spans, &rest[..ch.len_utf8()], em.face());
        i += ch.len_utf8();
        prev = Some(ch);
    }
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
    /// Tests: as `Chars`, but a `1` half as wide (a proportional font's digits).
    #[cfg(test)]
    NarrowOne(f32),
}

impl Measure<'_> {
    fn width(&self, text: &str, face: Face) -> f32 {
        match self {
            _ if text.is_empty() => 0.0,
            Measure::Chars(w) => text.chars().count() as f32 * w,
            #[cfg(test)]
            Measure::NarrowOne(w) => text.chars().map(|c| if c == '1' { w / 2.0 } else { *w }).sum(),
            Measure::Fonts(ctx, theme) => {
                let job = crate::text::layout_job(&StyledLine::new(text, face), theme, f32::INFINITY);
                ctx.fonts_mut(|f| f.layout_job(job)).size().x
            }
        }
    }

    /// `text` with its spans' faces (code, bold), as it is drawn.
    fn width_styled(&self, text: &str, spans: &[(Range<usize>, Face)], face: Face) -> f32 {
        match self {
            _ if text.is_empty() => 0.0,
            Measure::Fonts(ctx, theme) => {
                let job = crate::text::layout_job(&StyledLine::new(text, face).spans(spans.to_vec()), theme, f32::INFINITY);
                // Code at the end leaves its chip's room after it, taken by what follows.
                let room = if spans.iter().any(|(r, f)| *f == Face::InlineCode && r.end >= text.len()) { crate::text::CHIP_PAD } else { 0.0 };
                ctx.fonts_mut(|f| f.layout_job(job)).size().x + room
            }
            _ => self.width(text, face),
        }
    }

    fn scale(&self) -> f32 {
        match self {
            Measure::Fonts(_, t) => t.ui_scale(),
            _ => 1.0,
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
    // Digits as wide as the font's widest (tabular figures), so numbers in a column line up:
    // "71%" over "78%". Each is centered in that width by space around it (not in code, whose
    // digits are already). The room it adds, and where it goes in `range` of a cell.
    let slots = std::cell::RefCell::new(Vec::<(Face, f32)>::new());
    let slot = |face: Face| -> f32 {
        if let Some((_, w)) = slots.borrow().iter().find(|(f, _)| *f == face) {
            return *w;
        }
        let w = ('0'..='9').map(|d| m.width(d.encode_utf8(&mut [0; 4]), face)).fold(0.0, f32::max);
        slots.borrow_mut().push((face, w));
        w
    };
    let figures = |r: usize, c: usize, range: Range<usize>| -> (f32, Vec<(usize, f32)>) {
        let (src, spans) = all[r].get(c).map(|(s, sp)| (s.as_str(), sp.as_slice())).unwrap_or(("", &[]));
        let (mut extra, mut pads, mut carry) = (0.0, Vec::new(), 0.0);
        for (i, ch) in src[range.clone()].char_indices() {
            let face = spans.iter().find(|(sr, _)| sr.contains(&(range.start + i))).map_or(face_of(r), |(_, f)| *f);
            if !ch.is_ascii_digit() || face == Face::InlineCode {
                if carry > 0.0 {
                    pads.push((i, carry));
                    carry = 0.0;
                }
                continue;
            }
            let room = (slot(face) - m.width(&src[range.start + i..range.start + i + 1], face)).max(0.0);
            pads.push((i, carry + room / 2.0));
            carry = room / 2.0;
            extra += room;
        }
        (extra, pads)
    };
    // A cell's text in `range` as it is drawn: its code and bold in their own fonts.
    let drawn = |r: usize, c: usize, range: Range<usize>| -> f32 {
        let Some((src, spans)) = all[r].get(c) else { return 0.0 };
        let spans: Vec<(Range<usize>, Face)> = spans
            .iter()
            .filter_map(|(sr, f)| {
                let (a, b) = (sr.start.max(range.start), sr.end.min(range.end));
                (a < b).then(|| (a - range.start..b - range.start, *f))
            })
            .collect();
        m.width_styled(&src[range], &spans, face_of(r))
    };
    let natural: Vec<f32> = (0..cols).map(|c| (0..all.len()).map(|r| drawn(r, c, 0..cell(r, c).len()) + figures(r, c, 0..cell(r, c).len()).0).fold(0.0, f32::max)).collect();
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
                let (extra, digits) = figures(r, c, range.clone());
                let w = drawn(r, c, range.clone()) + extra;
                let slack = (widths[c] - w).max(0.0);
                let align = match aligns.get(c).copied().unwrap_or(egui::Align::Min) {
                    egui::Align::Max => slack,
                    egui::Align::Center => slack / 2.0,
                    egui::Align::Min => 0.0,
                };
                let at = text.len();
                pads.push((at, (col_x[c] + align - x).max(0.0)));
                pads.extend(digits.into_iter().map(|(o, p)| (at + o, p)));
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
/// The reports in a transcript: in each run (from a prompt to the next), the last assistant
/// message with text, once the run is over and if it has at least `min_words`. Entry index →
/// its words. Prompts sent while a run goes on (queued) steer it rather than end it.
fn report_entries(entries: &[Entry], running: bool, min_words: usize) -> HashMap<usize, usize> {
    let words = |e: &Entry| match e {
        Entry::Assistant { blocks, .. } => blocks.iter().map(|b| if let AssistantContent::Text { text, .. } = b { text.split_whitespace().count() } else { 0 }).sum(),
        _ => 0,
    };
    let mut out = HashMap::new();
    let mut last: Option<usize> = None;
    let mut finish = |last: &mut Option<usize>| {
        if let Some(i) = last.take() {
            let n = words(&entries[i]);
            if n >= min_words {
                out.insert(i, n);
            }
        }
    };
    for (i, e) in entries.iter().enumerate() {
        match e {
            Entry::User { queued: false, .. } => finish(&mut last),
            Entry::Assistant { .. } if words(e) > 0 => last = Some(i),
            _ => {}
        }
    }
    if !running {
        finish(&mut last);
    }
    out
}

/// How a report written to the report skill (skills/report) turned out: its first word.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Outcome {
    Done,
    Partly,
    Blocked,
}

impl Outcome {
    fn badge(self) -> (&'static str, Face) {
        match self {
            Outcome::Done => ("Done", Face::BadgeGood),
            Outcome::Partly => ("Partly done", Face::BadgeWarn),
            Outcome::Blocked => ("Blocked", Face::BadgeBad),
        }
    }
}

/// A report in the report skill's shape: the outcome, its first sentence (the headline), the
/// rest of the opening, then its `##` sections in order (title, body).
#[derive(Debug)]
struct Shaped {
    outcome: Outcome,
    headline: String,
    intro: String,
    sections: Vec<(String, String)>,
}

impl Shaped {
    /// Items under "Needs you".
    fn needs_you(&self) -> usize {
        self.sections.iter().filter(|(t, _)| t.eq_ignore_ascii_case("needs you")).map(|(_, b)| top_items(b)).sum()
    }
}

/// Top-level list items in markdown.
fn top_items(body: &str) -> usize {
    body.lines().filter(|l| !l.starts_with(' ') && list_item(l).is_some()).count()
}

/// `text` read as a report written to the report skill: it opens with `Done`, `Partly done`
/// or `Blocked` and a colon. None for anything else (it is shown as plain markdown).
fn shaped_report(text: &str) -> Option<Shaped> {
    let text = text.trim_start();
    let (head, body) = match text.find("\n## ") {
        Some(i) => (&text[..i], &text[i + 1..]),
        None => (text, ""),
    };
    let (first, rest) = head.split_once("\n\n").unwrap_or((head, ""));
    let first = first.trim().trim_start_matches("**");
    let lower = first.to_lowercase();
    let (outcome, n) = [("partly done", Outcome::Partly), ("partially done", Outcome::Partly), ("done", Outcome::Done), ("blocked", Outcome::Blocked)]
        .into_iter()
        .find(|(w, _)| lower.starts_with(w))
        .map(|(w, o)| (o, w.len()))?;
    let after = first[n..].trim_start_matches("**");
    let after = after.strip_prefix(':').or_else(|| after.strip_prefix(" —")).or_else(|| after.strip_prefix(" -"))?.trim_start();
    // The headline is the first sentence (a period and a space, outside inline code).
    let mut code = false;
    let mut cut = after.len();
    for (i, c) in after.char_indices() {
        if c == '`' {
            code = !code;
        } else if !code && c == '.' && after[i + 1..].starts_with(' ') {
            cut = i + 1;
            break;
        }
    }
    let mut headline = after[..cut].trim().to_string();
    if let Some(c) = headline.chars().next().filter(|c| c.is_lowercase()) {
        headline.replace_range(..c.len_utf8(), &c.to_uppercase().to_string());
    }
    let intro = [after[cut..].trim(), rest.trim()].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n\n");
    let sections = if body.is_empty() {
        Vec::new()
    } else {
        body.split("\n## ")
            .map(|part| {
                let part = part.strip_prefix("## ").unwrap_or(part);
                let (title, rest) = part.split_once('\n').unwrap_or((part, ""));
                (title.trim().to_string(), rest.to_string())
            })
            .collect()
    };
    Some(Shaped { outcome, headline, intro, sections })
}

/// What a check under "Checked" found, from the first word of its result ("**Tests:**
/// passes"): ✓, ✗, or ○ for what was not checked. None when it can't tell (a plain bullet).
fn check_mark(text: &str) -> Option<(&'static str, Face)> {
    let lower = text.trim().to_lowercase();
    if ["not checked", "not tested", "not run", "unchecked", "untested"].iter().any(|p| lower.starts_with(p)) {
        return Some(("○", Face::MetaWarn));
    }
    let result = lower.split_once(':').map_or(lower.as_str(), |(_, r)| r).trim_start();
    match result.split(|c: char| !c.is_alphanumeric()).next().unwrap_or("") {
        "pass" | "passes" | "passed" | "passing" | "ok" | "works" | "worked" | "succeeds" | "succeeded" | "green" | "clean" => Some(("✓", Face::ToolOk)),
        "fail" | "fails" | "failed" | "failing" | "broken" | "breaks" | "errors" | "red" => Some(("✗", Face::ToolError)),
        _ => None,
    }
}

/// A report in the skill's shape, drawn as its parts: the headline a size up, "Needs you" as a
/// callout card, checks marked by what they found, each section's heading with its count.
/// Prose wraps at `measure` and is `scale` times its usual size.
#[allow(clippy::too_many_arguments)]
fn report_lines(sh: &Shaped, t: &crate::theme::Theme, m: &Measure, avail: f32, surface: Color32, measure: Option<f32>, scale: f32, diagrams: bool) -> Vec<StyledLine> {
    let k = m.scale();
    let prose = |l: &mut StyledLine, s: f32| {
        if !is_prose(l) {
            return;
        }
        l.measure = measure.map(|w| w * s);
        if s != 1.0 {
            l.scale = Some(s);
        }
    };
    let mut out = Vec::new();
    let (text, spans) = inline_spans(&sh.headline);
    // A size up from the text, a little under the section headings, with a heading's leading.
    let mut headline = StyledLine::new(text, Face::Heading).spans(spans).gap(6.0 * k).pad(0.0, 6.0 * k);
    prose(&mut headline, scale * 0.9);
    out.push(headline);
    for mut l in markdown_lines(&sh.intro, t, m, avail, diagrams) {
        prose(&mut l, scale);
        out.push(l);
    }
    for (title, body) in &sh.sections {
        let kind = title.to_lowercase();
        let needs = kind == "needs you";
        let items = top_items(body);
        let mut head = StyledLine::new(title.clone(), Face::Heading).gap(30.0 * k).pad(0.0, if needs { 8.0 } else { 2.0 } * k);
        if items > 0 && ["needs you", "changed", "checked", "not done"].contains(&kind.as_str()) {
            let at = head.text.len();
            head.text.push_str(&format!("   {items}"));
            head.spans.push((at..head.text.len(), if needs { Face::MetaWarn } else { Face::Meta }));
        }
        prose(&mut head, scale);
        out.push(head);
        // Needs you's card takes its indent from the column.
        let inset = if needs { 16.0 * k } else { 0.0 };
        let mut lines = markdown_lines(body, t, m, avail - inset, diagrams);
        if needs {
            // A card the eye finds first: raised a little from the report, with a trace of the
            // theme's warning color and a bar of it (more of it muddies: tan, olive).
            let raised = crate::themes::mix(surface, t.fg, if t.dark { 0.04 } else { 0.0 });
            let card = Block::new(crate::themes::mix(raised, t.yellow, if t.dark { 0.05 } else { 0.07 })).bar(t.yellow);
            let n = lines.len();
            for (i, l) in lines.iter_mut().enumerate() {
                prose(l, scale);
                l.block = Some(card.nth(i, n));
                l.indent += inset;
                if i == 0 {
                    l.gap_above = 0.0;
                    l.pad_top += 12.0 * k;
                }
                if i + 1 == n {
                    l.pad_bottom += 12.0 * k;
                }
            }
        } else {
            for l in lines.iter_mut() {
                prose(l, scale);
                if kind == "checked" && l.marker.is_some() {
                    if let Some((mark, face)) = check_mark(&l.text) {
                        l.marker = Some((mark.to_string(), face));
                    }
                }
            }
        }
        out.extend(lines);
    }
    // The theme's border is drawn against its background: on the report's surface, the same
    // step from it, so tables' and code's edges show.
    let edge = crate::themes::mix(surface, t.fg, 0.18);
    for l in out.iter_mut() {
        if let Some(b) = l.block.as_mut().filter(|b| b.border == Some(t.border)) {
            b.border = Some(edge);
        }
        if l.rule_below == Some(t.border) {
            l.rule_below = Some(edge);
        }
    }
    out
}

/// Whether a line is prose, to take a report's size and wrap at chat-measure. Tables, code and
/// diagrams keep their size and the width they were laid out for (a table's rows are bordered
/// cards with no fill; a quote's lines have a bar and no border, and are prose).
fn is_prose(l: &StyledLine) -> bool {
    l.diagram.is_none() && l.block.is_none_or(|b| b.bg == Color32::TRANSPARENT && b.border.is_none())
}

fn markdown_lines(text: &str, t: &crate::theme::Theme, m: &Measure, avail: f32, diagrams: bool) -> Vec<StyledLine> {
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
            // A Mermaid diagram, drawn (chat-diagrams); one that can't be is shown as code, saying so.
            let mut caption = lang.clone();
            if diagrams && lang.eq_ignore_ascii_case("mermaid") {
                let src = code.join("\n");
                let font_px = t.format(Face::AssistantText).font_id.size;
                match crate::diagram::layout(&src, font_px, &mut |s| m.width(s, Face::AssistantText), avail) {
                    Ok(placed) => {
                        // Centered in the column, like a figure; copied, it is its source again.
                        let side = ((avail - placed.size().x) / 2.0).max(0.0);
                        out.push(StyledLine::new(format!("```mermaid\n{src}\n```"), Face::Meta).diagram(placed).indent(side).gap(gap.max(16.0 * k)).pad(4.0 * k, 4.0 * k));
                        gap = 16.0 * k;
                        prev_list = false;
                        continue;
                    }
                    Err(e) => caption = format!("mermaid · not drawn: {e}"),
                }
            }
            let mut lines: Vec<StyledLine> = Vec::new();
            if !caption.is_empty() {
                lines.push(StyledLine::new(caption, Face::Meta).pad(8.0 * k, 0.0));
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
            // More room above than below, so a heading reads as the start of its section.
            out.push(StyledLine::new(s, face).spans(spans).gap(gap.max(if level <= 2 { 30.0 } else { 22.0 } * k)).pad(0.0, 2.0 * k));
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

/// The places in the code a line of a chat names (`src/main.rs:12`, in prose or inline code,
/// your messages, thinking, or what a tool printed: grep's `src/a.rs:12:`, a compiler's
/// `--> src/main.rs:12:5`), as links: character columns and the reference.
fn code_links(text: &str) -> Vec<(Range<usize>, String)> {
    let mut out = Vec::new();
    let mut col = 0;
    for word in text.split_inclusive(char::is_whitespace) {
        let n = word.chars().count();
        // Brackets, quotes and punctuation around it aren't part of it.
        let core = word.trim_end().trim_start_matches(['(', '[', '{', '<', '"', '\'', '`']).trim_end_matches([')', ']', '}', '>', '"', '\'', '`', ',', ';', '.', ':']);
        if let Some(at) = word.find(core).filter(|_| code_ref(core).is_some()) {
            let start = col + word[..at].chars().count();
            out.push((start..start + core.chars().count(), core.to_string()));
        }
        col += n;
    }
    out
}

/// A place in the code, as agents cite it: `path:line`, also `path:line-line` and
/// `path:line:column`. The path has a `/` or a `.` and no spaces. (path, line).
fn code_ref(s: &str) -> Option<(String, u32)> {
    let (path, rest) = s.trim().split_once(':')?;
    if path.is_empty() || path.contains(char::is_whitespace) || !(path.contains('/') || path.contains('.')) {
        return None;
    }
    let digits = rest.find(|c: char| !c.is_ascii_digit()).unwrap_or(rest.len());
    let line: u32 = rest[..digits].parse().ok().filter(|n| *n > 0)?;
    let tail = &rest[digits..];
    let ok = tail.is_empty() || ((tail.starts_with('-') || tail.starts_with(':')) && tail.len() > 1 && tail[1..].chars().all(|c| c.is_ascii_digit()));
    ok.then(|| (path.trim_start_matches("./").to_string(), line))
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

/// `812 B`, `12.4 KB`, `3.1 MB`.
fn fmt_bytes(n: u64) -> String {
    match n {
        0..1000 => format!("{n} B"),
        1000..1_000_000 => format!("{:.1} KB", n as f64 / 1000.0),
        _ => format!("{:.1} MB", n as f64 / 1_000_000.0),
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
    /// Lines of a command's output its card shows (`chat-task-output-lines`; opened: all kept).
    output_lines: usize,
    /// A chain of commands shows a step a row (`chat-shell-steps`), this many on a closed card
    /// (`chat-shell-steps-shown`).
    shell_steps: bool,
    shell_steps_shown: usize,
    /// Commands and lookups that went well close to their header (`chat-tool-compact`).
    tool_compact: bool,
    /// What is running shows a spinner (`chat-tool-spinner`).
    tool_spinner: bool,
    /// The notice of a prompt cache miss on the request that made this call.
    cache_miss: Option<&'a str>,
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
                    let first = (name == "bash").then(|| crate::highlight::shell(&[summary.as_str()])).flatten();
                    match first.as_ref().and_then(|hl| hl.first()) {
                        Some(hl) => spans.extend(shift(Some(hl), s0)),
                        None => spans.push((s0..text.len(), Face::ToolArgs)),
                    }
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
    // Running, shown turning (the character stays in the text, unseen, for copying).
    let spinning = side.tool_spinner && ask.is_none() && if result.is_some() { working } else { started.is_some() };
    let mut header = format!("{icon}  ");
    let mut spans = vec![(0..icon.len(), if spinning { Face::Unseen } else { icon_face })];
    // What a command is for, in the agent's words, says it better than the tool's name.
    let description = if name == "bash" { arg(args, "description").lines().next().unwrap_or("").trim() } else { "" };
    let s0 = header.len();
    header.push_str(if description.is_empty() { name } else { description });
    spans.push((s0..header.len(), Face::ToolHeader));
    // Done and fine: just its line, opened to see how.
    let closes = side.tool_compact && matches!(name, "bash" | "read" | "grep" | "find" | "ls" | "web_search") && side.steps.is_empty();
    let compact = closes && !expanded && result.is_some_and(|r| !r.is_error) && !side.task.is_some_and(|t| t.running() || t.status == "failed");
    let summary = serpis_chat::tool_summary(name, args);
    // A command reads as one: its lines highlighted together (the first is the header's).
    let command: Vec<&str> = if name == "bash" { arg(args, "command").lines().collect() } else { Vec::new() };
    let command_hl = crate::highlight::shell(&command).unwrap_or_default();
    // A chain of commands: its steps show in rows of their own. A described command's show the
    // same way, as the header no longer has it.
    let whole = arg(args, "command");
    let chain = if name == "bash" && side.shell_steps { crate::shell_steps::split(whole) } else { None };
    let step_list = chain.clone().or_else(|| (!description.is_empty() && !whole.trim().is_empty()).then(|| vec![crate::shell_steps::Step { op: None, range: 0..whole.len() }]));
    if description.is_empty() {
        // Not described: what it does, as a chain's first step that isn't just a cd, in a line.
        let first_step = chain.as_ref().map(|steps| {
            let step = steps.iter().find(|s| !whole[s.range.clone()].starts_with("cd ")).unwrap_or(&steps[0]);
            whole[step.range.clone()].lines().next().unwrap_or("").to_string()
        });
        let line = first_step.as_deref().unwrap_or(&summary);
        let cut = if compact && line.chars().count() > COMPACT_COMMAND_CHARS { Some(format!("{}…", line.chars().take(COMPACT_COMMAND_CHARS).collect::<String>())) } else { None };
        let shown = cut.as_deref().unwrap_or(line);
        if !shown.is_empty() {
            header.push_str("  ");
            let s1 = header.len();
            header.push_str(shown);
            let hl = if shown == summary { command_hl.first().cloned() } else if name == "bash" { crate::highlight::shell(&[shown]).and_then(|h| h.into_iter().next()) } else { None };
            match hl {
                Some(hl) => spans.extend(shift(Some(&hl), s1)),
                None => spans.push((s1..header.len(), Face::ToolArgs)),
            }
        }
    }
    if let Some(steps) = &chain {
        header.push_str("   ");
        let s1 = header.len();
        header.push_str(&format!("{} steps", steps.len()));
        spans.push((s1..header.len(), Face::Meta));
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

    let mut hint = None;
    if let Some(miss) = side.cache_miss {
        header.push_str("   ");
        let s3 = header.len();
        header.push('⚠');
        spans.push((s3..header.len(), Face::MetaWarn));
        let at = header[..s3].chars().count();
        hint = Some((at..at + 1, miss.to_string()));
    }

    // Body lines, each with an optional row tint (diffs).
    let mut body: Vec<(StyledLine, Option<Color32>)> = Vec::new();
    // A subagent: what it does, as it goes (its own words close it).
    step_rows(side.steps, 0, &mut body);
    // What a command has printed so far (read as it runs): its last lines, all kept when the
    // card is opened.
    let printed = |body: &mut Vec<(StyledLine, Option<Color32>)>, task: &serpis_proto::Task| {
        let Some(out) = task.output.as_deref().filter(|o| !o.is_empty()) else { return };
        let lines: Vec<&str> = out.lines().collect();
        let n = if expanded { lines.len() } else { side.output_lines.min(lines.len()) };
        if n < lines.len() {
            body.push((StyledLine::new(format!("⋯ {} earlier lines (open the card for them)", lines.len() - n), Face::Meta), None));
        }
        for l in &lines[lines.len() - n..] {
            body.push((StyledLine::new(*l, Face::ToolOutput), None));
        }
    };
    let size = |task: &serpis_proto::Task| if task.output_bytes > 0 { format!(" · {} printed", fmt_bytes(task.output_bytes)) } else { String::new() };
    if body.is_empty()
        && let Some(task) = side.task
        && (task.kind == "bash" || task.kind == "monitor")
        && task.background
    {
        // A command in the background: how it is going, then the end of what it printed.
        let (line, face) = match task.status.as_str() {
            "running" => (format!("Running in the background{} · C-c C-k stops it", size(task)), Face::Meta),
            "failed" => (task.summary.clone().unwrap_or_else(|| "It failed.".into()), Face::Error),
            "stopped" => (format!("Stopped{}.", size(task)), Face::Meta),
            _ => (task.summary.clone().unwrap_or_else(|| "Finished.".into()), Face::ToolOutput),
        };
        body.push((StyledLine::new(line, face), None));
        printed(&mut body, task);
    } else if body.is_empty()
        && result.is_none()
        && let Some(task) = side.task.filter(|t| t.running() && t.kind == "bash")
    {
        // A command it waits for, printing as it goes (its result replaces this).
        body.push((StyledLine::new(format!("Running{} · C-c C-b moves it to the background", size(task)), Face::Meta), None));
        printed(&mut body, task);
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
        } else if name == "bash" || name == "serpis_lua" {
            // The rest of the command (or Lua code), then what it printed.
            let cmd = arg(args, if name == "bash" { "command" } else { "code" });
            for (i, l) in cmd.lines().enumerate().skip(1).filter(|_| step_list.is_none()) {
                body.push((StyledLine::new(l, Face::ToolArgs).spans(command_hl.get(i).cloned().unwrap_or_default()), None));
            }
            let out = result.map(|r| tool_text(r)).or(partial.map(str::to_string)).unwrap_or_default();
            let face = if result.is_some_and(|r| r.is_error) { Face::ToolError } else { Face::ToolOutput };
            // A rule between the command's lines and what it printed.
            let rule = body.len();
            if let Some((last, _)) = body.last_mut().filter(|_| rule > 0 && !out.is_empty()) {
                last.rule_below = Some(t.border);
                last.pad_bottom = 7.0 * k;
            }
            for l in out.lines() {
                body.push((StyledLine::new(l, face), None));
            }
            if rule > 0 && body.len() > rule {
                body[rule].0.pad_top = 7.0 * k;
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

    if compact {
        slice.clear();
    }
    // A command's steps stay in view above what it printed, however much that is.
    let mut rows = step_list.as_ref().filter(|_| !compact).map(|steps| chain_rows(whole, steps, &command_hl, if expanded { usize::MAX } else { side.shell_steps_shown }, t)).unwrap_or_default();
    if let (Some(last), Some((first, _))) = (rows.last_mut(), slice.first_mut()) {
        last.rule_below = Some(t.border);
        last.pad_bottom = 7.0 * k;
        first.pad_top = 7.0 * k;
    }
    slice.splice(0..0, rows.drain(..).map(|l| (l, None)));

    let n = slice.len() + 1;
    let mut head = StyledLine::new(header, Face::ToolArgs).spans(spans).block(card.nth(0, n)).indent(12.0 * k).gap(gap).pad(7.0 * k, 7.0 * k);
    head.spinner = spinning.then_some(t.accent);
    head.hint = hint;
    if n > 1 {
        head.rule_below = Some(t.border);
    }
    let mut lines = vec![head];
    for (i, (l, tint)) in slice.into_iter().enumerate() {
        let blk = Block { bg: tint.map(|c| blend(t.tool_bg, c)).unwrap_or(t.tool_bg), ..card.nth(i + 1, n) };
        let indent = l.indent;
        let mut l = l.block(blk).indent(indent + 12.0 * k);
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

/// A chain's steps as rows: each operator hangs in a gutter left of its step, so long steps wrap
/// clear of it and the steps line up. Closed, `shown` steps show (fewer when they run long).
/// Each operator is also in the text, unseen, after the step before it: what is selected and
/// copied still runs the same.
fn chain_rows(cmd: &str, steps: &[crate::shell_steps::Step], hl: &[crate::highlight::Spans], shown: usize, t: &crate::theme::Theme) -> Vec<StyledLine> {
    let k = t.ui_scale();
    // A lone step has no operators to hang.
    let gutter = if steps.len() > 1 { (t.mono_size * 1.2).ceil() + 8.0 } else { 0.0 };
    // Each line of the command, and where it starts.
    let lines: Vec<(usize, &str)> = cmd.lines().map(|l| (l.as_ptr() as usize - cmd.as_ptr() as usize, l)).collect();
    let mut rows = Vec::new();
    let mut steps_shown = 0;
    for (i, step) in steps.iter().enumerate() {
        let room = steps_shown < shown && (shown == usize::MAX || rows.len() < PREVIEW_LINES);
        if !room {
            let hidden = steps.len() - i;
            let more = if hidden == 1 { "1 more step".to_string() } else { format!("{hidden} more steps") };
            rows.push(StyledLine::new(format!("{more} · TAB or click to expand"), Face::Meta).indent(gutter).pad(3.0 * k, 0.0));
            break;
        }
        steps_shown += 1;
        let first = rows.len();
        for (n, (start, line)) in lines.iter().enumerate() {
            let (a, b) = (step.range.start.max(*start), step.range.end.min(start + line.len()));
            if a > b || (a == b && !step.range.contains(start)) {
                continue;
            }
            // A step running long (a heredoc's text) is cut too.
            if shown != usize::MAX && rows.len() >= PREVIEW_LINES {
                rows.push(StyledLine::new(format!("{} more lines · TAB or click to expand", lines.len() - n), Face::Meta).indent(gutter).pad(3.0 * k, 0.0));
                return rows;
            }
            let spans = hl.get(n).into_iter().flatten().filter_map(|(r, f)| {
                let (x, y) = (r.start + start, r.end + start);
                (x.max(a) < y.min(b)).then(|| (x.max(a) - a..y.min(b) - a, *f))
            });
            rows.push(StyledLine::new(&cmd[a..b], Face::ToolArgs).spans(spans.collect()).indent(gutter));
        }
        if first > 0 {
            rows[first].pad_top = 4.0 * k;
        }
        if let Some(op) = step.op {
            rows[first].marker = Some((op.to_string(), Face::SynKeyword));
            // The step before says it, for copying.
            let before = &mut rows[first - 1];
            let s = before.text.len();
            before.text.push(' ');
            before.text.push_str(op);
            before.spans.push((s..before.text.len(), Face::Unseen));
        }
    }
    rows
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

/// A chat's folder for its header: from home, or its last parts when that is long.
fn folder_label(cwd: &str, remote: bool) -> String {
    let full = if remote { cwd.to_string() } else { crate::pickers::tilde(std::path::Path::new(cwd)) };
    if full.chars().count() <= 32 {
        return full;
    }
    let parts: Vec<&str> = full.rsplit('/').take(2).collect();
    format!("…/{}/{}", parts[1], parts[0])
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

/// What was clicked in the composer's review header.
#[derive(Clone, Copy, Debug, PartialEq)]
enum ReviewAct {
    Save,
    SendNow,
    Cancel,
    Kind,
    Toggle,
    Discard,
    Edit(u64),
    Delete(u64),
}

/// A review serpis sent, drawn in your bubble: what it is, then each comment (its kind, where it
/// is as a link back to it, what it quotes, what you wrote), then what was typed with it.
fn review_bubble_lines(sent: &crate::chat_review::Sent, ei: usize, targets: &mut Vec<(String, usize, bool)>, k: f32) -> Vec<StyledLine> {
    use crate::chat_review::Ask;
    let mut out = Vec::new();
    let questions = sent.items.iter().filter(|i| i.ask == Ask::Question).count();
    let changes = sent.items.len() - questions;
    let mut parts = Vec::new();
    if questions > 0 {
        parts.push(format!("{questions} question{}", if questions == 1 { "" } else { "s" }));
    }
    if changes > 0 {
        parts.push(format!("{changes} {}", if sent.compared.is_some() { if changes == 1 { "fix" } else { "fixes" } } else if changes == 1 { "change" } else { "changes" }));
    }
    let head = match &sent.compared {
        Some(c) => format!("Review of {c}"),
        None => "Review".to_string(),
    };
    let text = if parts.is_empty() { head.clone() } else { format!("{head}  ·  {}", parts.join(", ")) };
    out.push(StyledLine::new(text, Face::Label).spans(vec![(0..head.len(), Face::Accent)]));
    for it in &sent.items {
        let (kind, face) = match (it.ask, sent.compared.is_some()) {
            (Ask::Question, _) => ("Question", Face::Accent),
            (Ask::Fix, true) => ("Fix", Face::MetaWarn),
            (Ask::Fix, false) => ("Change", Face::MetaWarn),
        };
        let place = if sent.compared.is_some() { it.place.clone() } else if it.place.is_empty() { "↑ in the report".to_string() } else { format!("↑ {}", it.place) };
        let mut l = StyledLine::new(format!("{kind}   {place}"), Face::Meta).spans(vec![(0..kind.len(), face)]);
        l.pad_top = 10.0 * k;
        if sent.compared.is_none() && !it.quote.is_empty() {
            targets.push((it.quote.join("\n"), ei, false));
            let at = kind.chars().count() + 3;
            l.links.push((at..at + place.chars().count(), format!("find:{}", targets.len() - 1)));
        }
        out.push(l);
        let shown = it.quote.len().min(3);
        for (j, q) in it.quote.iter().take(shown).enumerate() {
            let more = if j + 1 == shown && it.quote.len() > shown { " …" } else { "" };
            out.push(StyledLine::new(format!("│ {q}{more}"), if it.code { Face::CodeDim } else { Face::Dim }));
        }
        out.extend(it.text.lines().map(|t| StyledLine::new(t, Face::UserText)));
    }
    if !sent.also.is_empty() {
        let mut l = StyledLine::new("Also", Face::Label);
        l.pad_top = 10.0 * k;
        out.push(l);
        out.extend(sent.also.lines().map(|t| StyledLine::new(t, Face::UserText)));
    }
    out
}

/// The text of an assistant entry (its text blocks).
fn entry_text(e: &Entry) -> String {
    match e {
        Entry::Assistant { blocks, .. } => blocks.iter().filter_map(|b| if let AssistantContent::Text { text, .. } = b { Some(text.as_str()) } else { None }).collect::<Vec<_>>().join("\n\n"),
        _ => String::new(),
    }
}

/// Where `quote` is in entry `entry`'s lines of `out`: (line, character columns) for each line it
/// touches. Lines are joined with newlines, as a selection across them reads.
fn locate_quote(out: &[StyledLine], owner: &[Option<usize>], entry: usize, quote: &str) -> Option<Vec<(usize, Range<usize>)>> {
    let quote = quote.trim();
    if quote.is_empty() {
        return None;
    }
    let idx: Vec<usize> = (0..out.len()).filter(|i| owner.get(*i).copied().flatten() == Some(entry)).collect();
    let joined = idx.iter().map(|i| out[*i].text.as_str()).collect::<Vec<_>>().join("\n");
    let at = joined.find(quote)?;
    let end = at + quote.len();
    let mut segments = Vec::new();
    let mut pos = 0;
    for i in idx {
        let t = &out[i].text;
        let (s, e) = (pos, pos + t.len());
        let (a, b) = (at.max(s), end.min(e));
        if a < b {
            segments.push((i, t[..a - s].chars().count()..t[..b - s].chars().count()));
        }
        pos = e + 1;
    }
    Some(segments)
}

impl ChatBuffer {
    fn review_machine(&self) -> Option<String> {
        (!self.node.local()).then(|| self.node.machine.clone())
    }

    /// Read this chat's saved review, once. A comment that was being written comes back to the
    /// composer.
    fn ensure_review(&mut self) {
        if self.node.chat.is_empty() || self.review_key.as_deref() == Some(self.node.chat.as_str()) {
            return;
        }
        self.review_key = Some(self.node.chat.clone());
        self.commenting = None;
        self.review = crate::chat_review::load(self.review_machine().as_deref(), &self.node.chat);
        let aside = self.review.aside.take();
        if let Some(w) = self.review.writing.take() {
            if self.commenting.is_none() {
                let stash = aside.unwrap_or_else(|| self.prompt.text());
                self.prompt.set_text(&w.text);
                self.prompt.move_to_end();
                self.writing_saved = (w.text.clone(), Some(Instant::now()));
                self.commenting = Some((w, stash));
            }
        }
        self.dirty = true;
    }

    /// Keep the review (and the comment being written) on disk.
    pub fn persist_review(&mut self, ed: &mut Editor) {
        let Some(key) = self.review_key.clone() else { return };
        let text = self.prompt.text();
        let mut saved = self.review.clone();
        saved.writing = self.commenting.as_ref().map(|(w, _)| crate::chat_review::Writing { text: text.clone(), ..w.clone() });
        saved.aside = self.commenting.as_ref().map(|(_, aside)| aside.clone()).filter(|a| !a.is_empty());
        if let Err(e) = crate::chat_review::save(self.review_machine().as_deref(), &key, &saved) {
            ed.warn(format!("Couldn't save the review: {e:#}"));
        }
        self.writing_saved = (text, Some(Instant::now()));
    }

    /// The transcript's lines as drawn, for tests: text, marker, whether on a surface (a
    /// report's), in a card (and its color), scale, links and marks.
    pub fn lines_json(&mut self, ed: &Editor) -> Value {
        if self.dirty {
            self.rebuild(ed);
        }
        Value::Array(
            self.lines
                .iter()
                .map(|l| {
                    serde_json::json!({
                        "text": l.text,
                        "marker": l.marker.as_ref().map(|(m, _)| m.clone()),
                        "surface": l.backdrop.is_some(),
                        "card": l.block.map(|b| crate::themes::hex(b.bg)),
                        "border": l.block.and_then(|b| b.border).map(crate::themes::hex),
                        "measure": l.measure,
                        "bar": l.block.and_then(|b| b.bar).map(crate::themes::hex),
                        "scale": l.scale,
                        "links": l.links.iter().map(|(_, t)| t.clone()).collect::<Vec<_>>(),
                        "marks": l.marks.len(),
                        "diagram": l.diagram.as_ref().map(|d| serde_json::json!({
                            "kind": d.diagram.kind,
                            "width": d.size().x,
                            "height": d.size().y,
                            "text": d.text,
                            "nodes": d.diagram.nodes.iter().map(|(n, _)| n.clone()).collect::<Vec<_>>(),
                        })),
                        "column": self.text_w,
                        "indent": l.indent,
                    })
                })
                .collect(),
        )
    }

    /// The review as scripts see it.
    pub fn review_json(&mut self) -> Value {
        self.ensure_review();
        let text = self.prompt.text();
        serde_json::json!({
            "comments": self.review.comments.iter().map(crate::chat_review::to_json).collect::<Vec<_>>(),
            "drafts": self.review.drafts().len(),
            "writing": self.commenting.as_ref().map(|(w, _)| serde_json::json!({"quote": w.quote, "label": w.label, "kind": w.ask.name(), "text": text, "id": w.id})),
        })
    }

    /// The transcript's selection in order, else the cursor's whole line.
    fn selected_range(&self) -> (Pos, Pos) {
        let c = self.view.cursor;
        match self.view.anchor.filter(|a| *a != c) {
            Some(a) if a < c => (a, c),
            Some(a) => (c, a),
            None => (Pos::new(c.line, 0), Pos::new(c.line, char_len(self.lines.get(c.line).map_or("", |l| l.text.as_str())))),
        }
    }

    /// The transcript's text from `a` to `b`, lines joined with newlines.
    fn quote_of(&self, a: Pos, b: Pos) -> String {
        let mut out = Vec::new();
        for i in a.line..=b.line.min(self.lines.len().saturating_sub(1)) {
            let t: Vec<char> = self.lines[i].text.chars().collect();
            let s = if i == a.line { a.col.min(t.len()) } else { 0 };
            let e = if i == b.line { b.col.min(t.len()) } else { t.len() };
            out.push(t[s..e.max(s)].iter().collect::<String>());
        }
        out.join("\n").trim().to_string()
    }

    /// Where transcript line `line` of entry `entry` is, by its headings: "Changed 1" for the
    /// first item under "Changed", "Summary" for a paragraph under it; empty outside sections.
    fn label_at(&self, line: usize, entry: usize) -> String {
        let text = self.entries.get(entry).map(entry_text).unwrap_or_default();
        let titles: Vec<String> = text.lines().filter_map(|l| l.trim_start().strip_prefix('#').map(|h| h.trim_start_matches('#').trim().to_string())).filter(|t| !t.is_empty()).collect();
        // A heading line: its title (a report's has its count after it).
        let heading = |l: &StyledLine| -> Option<String> {
            if l.marker.is_some() {
                return None;
            }
            titles.iter().find(|t| l.text == **t || l.text.strip_prefix(t.as_str()).is_some_and(|r| !r.trim().is_empty() && r.trim().chars().all(|c| c.is_ascii_digit()))).cloned()
        };
        let Some(here) = self.lines.get(line) else { return String::new() };
        let item = here.marker.is_some();
        let mut count = 0;
        let mut i = line;
        loop {
            let Some(l) = self.lines.get(i) else { return String::new() };
            if self.line_entry.get(i).copied().flatten() != Some(entry) {
                return String::new();
            }
            if let Some(title) = heading(l) {
                return if item { format!("{title} {count}") } else { title };
            }
            if item && l.marker.is_some() && l.indent == here.indent {
                count += 1;
            }
            if i == 0 {
                return String::new();
            }
            i -= 1;
        }
    }

    fn focus_comment(&mut self, ed: &mut Editor) {
        self.focus = Focus::Prompt;
        let id = ed.current_buffer();
        if crate::packages::vim::state_of(ed, id).is_some() {
            crate::packages::vim::set_state(ed, id, crate::packages::vim::State::Insert);
        }
    }

    /// Start a comment on the transcript's selection (or the cursor's line): written in the
    /// composer, what was being typed there kept aside.
    pub fn start_comment(&mut self, ed: &mut Editor, ask: crate::chat_review::Ask) {
        self.ensure_review();
        if self.commenting.is_some() {
            self.focus_comment(ed);
            return;
        }
        let (a, b) = self.selected_range();
        let entry = self.line_entry.get(a.line).copied().flatten().filter(|e| matches!(self.entries.get(*e), Some(Entry::Assistant { .. })));
        let Some(entry) = entry else {
            ed.message("Select part of what the agent wrote to comment on it");
            return;
        };
        let quote = self.quote_of(a, b);
        if quote.is_empty() {
            ed.message("Select part of what the agent wrote to comment on it");
            return;
        }
        let label = self.label_at(a.line, entry);
        let opening = crate::chat_review::opening_of(&entry_text(&self.entries[entry]));
        let stash = self.prompt.text();
        self.prompt.set_text("");
        self.commenting = Some((crate::chat_review::Writing { id: None, entry, quote, label, opening, ask, text: String::new() }, stash));
        self.view.anchor = None;
        self.focus_comment(ed);
        self.dirty = true;
        self.persist_review(ed);
    }

    /// Change a saved comment, in the composer.
    pub fn edit_comment(&mut self, ed: &mut Editor, id: u64) {
        self.ensure_review();
        if self.commenting.is_some() {
            self.save_comment(ed);
        }
        let Some(c) = self.review.comments.iter().find(|c| c.id == id && c.state == crate::chat_review::State::Draft).cloned() else { return };
        let stash = self.prompt.text();
        self.prompt.set_text(&c.text);
        self.prompt.move_to_end();
        self.commenting = Some((crate::chat_review::Writing { id: Some(id), entry: c.entry, quote: c.quote, label: c.label, opening: c.opening, ask: c.ask, text: String::new() }, stash));
        self.focus_comment(ed);
        self.dirty = true;
        self.persist_review(ed);
    }

    /// Stop writing the comment: what it says, with the composer given back what it had.
    fn end_comment(&mut self) -> Option<crate::chat_review::Writing> {
        let (w, stash) = self.commenting.take()?;
        let text = self.prompt.text();
        self.prompt.set_text(&stash);
        self.prompt.move_to_end();
        self.dirty = true;
        Some(crate::chat_review::Writing { text, ..w })
    }

    /// Keep the comment being written in the review (emptied, a saved one is deleted).
    pub fn save_comment(&mut self, ed: &mut Editor) {
        if let Some(w) = self.end_comment() {
            self.review.keep(&w);
        }
        self.persist_review(ed);
    }

    pub fn cancel_comment(&mut self, ed: &mut Editor) {
        self.end_comment();
        self.persist_review(ed);
    }

    /// Send the comment being written now, on its own.
    pub fn send_comment_now(&mut self, ed: &mut Editor) {
        if self.commenting.as_ref().is_some() && self.prompt.text().trim().is_empty() {
            ed.message("Write the comment first");
            return;
        }
        let Some(w) = self.end_comment() else { return };
        let Some(id) = self.review.keep(&w) else { return };
        let Some(c) = self.review.comments.iter_mut().find(|c| c.id == id) else { return };
        c.state = crate::chat_review::State::Sent;
        let message = crate::chat_review::prompt(&[&*c], "");
        self.persist_review(ed);
        self.submit(ed, message, false);
    }

    /// Question or change: the other one, or `set`.
    pub fn cycle_comment_kind(&mut self, set: Option<crate::chat_review::Ask>) {
        use crate::chat_review::Ask;
        if let Some((w, _)) = self.commenting.as_mut() {
            w.ask = set.unwrap_or(if w.ask == Ask::Fix { Ask::Question } else { Ask::Fix });
            self.dirty = true;
        }
    }

    /// Drop the review's drafts. How many there were.
    pub fn discard_review(&mut self, ed: &mut Editor) -> usize {
        let n = self.review.drafts().len();
        self.review.comments.retain(|c| c.state != crate::chat_review::State::Draft);
        self.review_open = false;
        self.dirty = true;
        self.persist_review(ed);
        n
    }

    pub fn delete_comment(&mut self, ed: &mut Editor, id: u64) {
        self.review.comments.retain(|c| c.id != id);
        self.dirty = true;
        self.persist_review(ed);
    }

    /// Mark what each comment quotes and show it under that (cards): the review's drafts, the
    /// quote of one being written, and comments already sent (read from the review messages in
    /// the transcript, so they last), each of those with a link to the agent's answer once it
    /// quotes it, and that quote in the answer marked with a link back. Lines are inserted into
    /// `out` (with `owner` and the thinking lines moved along); returns the lines that show a
    /// draft, with its id (a click changes it).
    #[allow(clippy::too_many_arguments)]
    fn place_comments(&self, out: &mut Vec<StyledLine>, owner: &mut Vec<Option<usize>>, thinking: &mut [usize], targets: &mut Vec<(String, usize, bool)>, t: &crate::theme::Theme, k: f32) -> Vec<(usize, u64)> {
        use crate::chat_review::{Ask, State};
        let color = |ask: Ask| if ask == Ask::Question { t.accent } else { t.yellow };
        let editing = self.commenting.as_ref().and_then(|(w, _)| w.id);
        struct Card {
            id: Option<u64>,
            state: String,
            text: String,
            /// The answer: a `find:` target.
            reply: Option<usize>,
        }
        // (entry it was on, the quote, kind, its card)
        let mut places: Vec<(usize, String, Ask, Option<Card>)> = Vec::new();
        for c in self.review.comments.iter().filter(|c| c.state == State::Draft) {
            let state = if editing == Some(c.id) { "being changed in the composer" } else { "draft · click to change" };
            places.push((c.entry, c.quote.clone(), c.ask, Some(Card { id: Some(c.id), state: state.into(), text: c.text.clone(), reply: None })));
        }
        if let Some((w, _)) = self.commenting.as_ref().filter(|(w, _)| w.id.is_none()) {
            places.push((w.entry, w.quote.clone(), w.ask, None));
        }
        let assistant = |range: &mut dyn Iterator<Item = usize>| -> Vec<usize> { range.filter(|e| matches!(self.entries.get(*e), Some(Entry::Assistant { .. }))).collect() };
        for (ei, e) in self.entries.iter().enumerate() {
            let Entry::User { text, .. } = e else { continue };
            let Some(sent) = crate::chat_review::parse_sent(text).filter(|s| s.compared.is_none()) else { continue };
            for it in sent.items.iter().filter(|it| !it.quote.is_empty()) {
                let quote = it.quote.join("\n");
                // What it was about: the agent's message before the review that has it.
                let Some(on) = assistant(&mut (0..ei).rev()).into_iter().find(|a| locate_quote(out, owner, *a, &quote).is_some()) else { continue };
                // The answer: a message after it that quotes it (its first line, at least).
                let first = it.quote[0].trim();
                let answer = assistant(&mut (ei + 1..self.entries.len())).into_iter().find_map(|a| locate_quote(out, owner, a, first));
                let reply = answer.as_ref().map(|segments| {
                    targets.push((first.to_string(), ei, true));
                    let to = targets.len() - 1;
                    // The quote in the answer: marked, a link back to what it is about.
                    targets.push((quote.clone(), ei, false));
                    let back = format!("find:{}", targets.len() - 1);
                    for (line, _) in segments {
                        let l = &mut out[*line];
                        l.marker = Some(("↑".into(), Face::Accent));
                        let n = l.text.chars().count();
                        l.links.push((0..n, back.clone()));
                    }
                    to
                });
                let state = if reply.is_some() { "sent  ·  answered ↓" } else { "sent" };
                places.push((on, quote, it.ask, Some(Card { id: None, state: state.into(), text: it.text.clone(), reply })));
            }
        }
        let mut inserts: Vec<(usize, Vec<StyledLine>, Option<u64>)> = Vec::new();
        for (entry, quote, ask, card) in places {
            // Where it was, else where that text is now (the transcript moved: a compaction).
            let found = locate_quote(out, owner, entry, &quote).or_else(|| assistant(&mut (0..self.entries.len()).rev()).into_iter().find_map(|e| locate_quote(out, owner, e, &quote)));
            let Some(segments) = found else { continue };
            for (line, r) in &segments {
                out[*line].marks.push((r.clone(), color(ask)));
            }
            let (Some(c), Some((last, _))) = (card, segments.last()) else { continue };
            let kind = if ask == Ask::Question { "Question" } else { "Change" };
            let head = format!("{kind}  ·  {}", c.state);
            let mut first = StyledLine::new(head.clone(), Face::Meta).spans(vec![(0..kind.len(), if ask == Ask::Question { Face::Accent } else { Face::MetaWarn })]);
            if let Some(to) = c.reply {
                let label = "answered ↓";
                if let Some(at) = head.find(label) {
                    let (s, e) = (head[..at].chars().count(), head[..at].chars().count() + label.chars().count());
                    first.links.push((s..e, format!("find:{to}")));
                    first.spans.push((at..at + label.len(), Face::Accent));
                }
            }
            let mut lines = vec![first];
            lines.extend(c.text.lines().map(|l| StyledLine::new(l, Face::UserText)));
            let card = Block::new(t.bg_elevated).border(t.border);
            let n = lines.len();
            for (i, l) in lines.iter_mut().enumerate() {
                l.block = Some(card.nth(i, n));
                l.indent = 14.0 * k;
                if i == 0 {
                    l.gap_above = 6.0 * k;
                    l.pad_top = 7.0 * k;
                }
                if i + 1 == n {
                    l.pad_bottom = 8.0 * k;
                }
            }
            inserts.push((*last, lines, c.id));
        }
        inserts.sort_by_key(|i| i.0);
        let mut shift = 0;
        let mut placed = Vec::new();
        for (after, mut lines, id) in inserts {
            let at = after + 1 + shift;
            let o = owner.get(after + shift).copied().flatten();
            // Inside a surface (a report's): on it too, the surface going on under the card.
            if let Some(b) = out.get(after + shift).and_then(|l| l.backdrop) {
                let last = lines.len().saturating_sub(1);
                for (j, l) in lines.iter_mut().enumerate() {
                    l.backdrop = Some(Block { top: false, bottom: b.bottom && j == last, ..b });
                }
                if b.bottom {
                    if let Some(bd) = out[after + shift].backdrop.as_mut() {
                        bd.bottom = false;
                    }
                }
            }
            let n = lines.len();
            for (j, l) in lines.into_iter().enumerate() {
                out.insert(at + j, l);
                owner.insert(at + j, o);
            }
            for th in thinking.iter_mut().filter(|th| **th >= at) {
                *th += n;
            }
            if let Some(id) = id {
                placed.extend((at..at + n).map(|i| (i, id)));
            }
            shift += n;
        }
        placed
    }

    /// The bar beside a selection in what the agent wrote: Ask, Comment. What was clicked.
    fn draw_selection_bar(&self, ui: &mut egui::Ui, painter: &egui::Painter, area: Rect, theme: &crate::theme::Theme, k: f32, rects: &mut Vec<(&'static str, Rect)>) -> Option<crate::chat_review::Ask> {
        use crate::chat_review::Ask;
        if self.focus != Focus::Transcript || self.commenting.is_some() {
            return None;
        }
        let anchor = self.view.anchor.filter(|a| *a != self.view.cursor)?;
        let a = anchor.min(self.view.cursor);
        let on_agent = self.line_entry.get(a.line).copied().flatten().is_some_and(|e| matches!(self.entries.get(e), Some(Entry::Assistant { .. })));
        if !on_agent {
            return None;
        }
        let r = self.view.screen_rect(a)?;
        let small = egui::FontId::proportional(theme.font_size - 1.5);
        let keyf = egui::FontId::monospace(theme.mono_size - 2.5);
        let items = [("Ask", "a", Ask::Question), ("Comment", "c", Ask::Fix)];
        let widths: Vec<f32> = items.iter().map(|(l, key, _)| crate::chrome::text_width(painter, l, small.clone()) + crate::chrome::text_width(painter, key, keyf.clone()) + 28.0 * k).collect();
        let bar_w = widths.iter().sum::<f32>() + 8.0 * k;
        // Beside the selection, in the margin prose leaves (chat-measure), so it covers no text;
        // above the selection where there is no such room.
        let line_left = self.view.screen_rect(Pos::new(a.line, 0)).map_or(r.left(), |s| s.left());
        let margin_x = self.lines.get(a.line).and_then(|l| l.measure).map(|m| line_left + m + 16.0 * k);
        // Above, it is one line tall and covers that line whole (half a line looks struck through).
        let (x, top, bar_h) = match margin_x.filter(|x| x + bar_w <= area.right() - 8.0 * k) {
            Some(x) => (x, (r.center().y - 16.0 * k).max(area.top() + 4.0), 32.0 * k),
            None => {
                let h = r.height().max(24.0 * k);
                let above = r.top() - h;
                let top = if above > area.top() + 4.0 { above } else { r.bottom() };
                (r.left().clamp(area.left() + 8.0 * k, (area.right() - bar_w - 8.0 * k).max(area.left() + 8.0 * k)), top, h)
            }
        };
        let bar = Rect::from_min_size(egui::pos2(x, top), egui::vec2(bar_w, bar_h));
        // Over the transcript, whatever is drawn there after it.
        let painter = &ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, ui.scope_id().with("chat-selection-bar"))).with_clip_rect(area);
        let shadow = egui::epaint::Shadow { offset: [0, 6], blur: 18, spread: 0, color: Color32::from_black_alpha(if theme.dark { 110 } else { 36 }) };
        painter.add(shadow.as_shape(bar, 8));
        painter.rect_filled(bar, 8.0 * k, theme.bg_elevated);
        painter.rect_stroke(bar, 8.0 * k, Stroke::new(1.0, theme.border), egui::StrokeKind::Inside);
        let mut bx = bar.left() + 4.0 * k;
        let mut picked = None;
        for (i, ((label, key, ask), w)) in items.iter().zip(&widths).enumerate() {
            let b = Rect::from_min_size(egui::pos2(bx, bar.top() + 3.0 * k), egui::vec2(*w, bar_h - 6.0 * k));
            let resp = ui.interact(b, ui.scope_id().with(("selection-bar", i)), egui::Sense::click());
            rects.push((if i == 0 { "chat-selection-ask" } else { "chat-selection-comment" }, b));
            let main = i == 0;
            if main {
                painter.rect_filled(b, 6.0 * k, if resp.hovered() { theme.accent.gamma_multiply(1.15) } else { theme.accent });
            } else if resp.hovered() {
                painter.rect_filled(b, 6.0 * k, crate::chrome::hover_bg(theme));
            }
            let fg = if main { crate::chrome::on(theme.accent, theme) } else { theme.fg };
            let lw = crate::chrome::text(painter, egui::pos2(b.left() + 10.0 * k, b.center().y), label, small.clone(), fg, *w);
            crate::chrome::text(painter, egui::pos2(b.left() + 16.0 * k + lw, b.center().y), key, keyf.clone(), if main { fg.gamma_multiply(0.7) } else { theme.fg_faint }, *w);
            if resp.clicked() {
                picked = Some(*ask);
            }
            bx += w;
        }
        picked
    }

    /// The composer's header: the comment being written (what it quotes, Save, Send now,
    /// Cancel), or the review waiting to be sent (its count, its list, discard).
    fn draw_review_header(&self, ui: &mut egui::Ui, painter: &egui::Painter, area: Rect, theme: &crate::theme::Theme, k: f32, rects: &mut Vec<(&'static str, Rect)>) -> Option<ReviewAct> {
        use crate::chat_review::Ask;
        let small = egui::FontId::proportional(theme.font_size - 2.0);
        let mut act = None;
        let rects = std::cell::RefCell::new(rects);
        let button = |ui: &mut egui::Ui, right: f32, cy: f32, label: &str, main: bool, key: &'static str| -> (Rect, bool) {
            let w = crate::chrome::text_width(painter, label, small.clone()) + 20.0 * k;
            let r = Rect::from_min_max(egui::pos2(right - w, cy - 11.0 * k), egui::pos2(right, cy + 11.0 * k));
            let resp = ui.interact(r, ui.scope_id().with(("review-button", key)), egui::Sense::click());
            rects.borrow_mut().push((key, r));
            let bg = if main { theme.accent } else if resp.hovered() { crate::chrome::hover_bg(theme) } else { theme.chip() };
            painter.rect_filled(r, 6.0 * k, bg);
            let fg = if main { crate::chrome::on(theme.accent, theme) } else { theme.fg };
            painter.text(r.center(), egui::Align2::CENTER_CENTER, label, small.clone(), fg);
            (r, resp.clicked())
        };
        let kind_of = |ask: Ask| if ask == Ask::Question { ("Question", theme.accent) } else { ("Change", theme.yellow) };
        if let Some((w, _)) = &self.commenting {
            let cy = area.top() + 11.0 * k;
            let (kind, kc) = kind_of(w.ask);
            let kw = crate::chrome::text_width(painter, kind, small.clone());
            let kr = Rect::from_min_size(egui::pos2(area.left() - 4.0 * k, cy - 11.0 * k), egui::vec2(kw + 8.0 * k, 22.0 * k));
            let kresp = ui.interact(kr, ui.scope_id().with("review-kind"), egui::Sense::click()).on_hover_text("Question or change (M-k)");
            if kresp.hovered() {
                painter.rect_filled(kr, 5.0 * k, crate::chrome::hover_bg(theme));
            }
            crate::chrome::text(painter, egui::pos2(area.left(), cy), kind, small.clone(), kc, kw + 1.0);
            if kresp.clicked() {
                act = Some(ReviewAct::Kind);
            }
            let (r1, save) = button(ui, area.right(), cy, "Save", true, "save");
            let (r2, now) = button(ui, r1.left() - 6.0 * k, cy, "Send now", false, "now");
            let (r3, cancel) = button(ui, r2.left() - 6.0 * k, cy, "Cancel", false, "cancel");
            let place = if w.label.is_empty() { "on the selection".to_string() } else { format!("on {}", w.label) };
            crate::chrome::text(painter, egui::pos2(area.left() + kw + 8.0 * k, cy), &place, small.clone(), theme.fg_dim, (r3.left() - area.left() - kw - 16.0 * k).max(0.0));
            // What it quotes: two lines at most, beside a bar of its kind's color.
            let lines: Vec<&str> = w.quote.lines().collect();
            let shown = lines.len().clamp(1, 2);
            let top = area.top() + 26.0 * k;
            painter.rect_filled(Rect::from_min_size(egui::pos2(area.left(), top), egui::vec2(3.0 * k, shown as f32 * 19.0 * k)), 1.5 * k, kc.gamma_multiply(0.8));
            for (i, l) in lines.iter().take(shown).enumerate() {
                let text = if i + 1 == shown && lines.len() > shown { format!("{l} …") } else { l.to_string() };
                crate::chrome::text(painter, egui::pos2(area.left() + 12.0 * k, top + (i as f32 + 0.5) * 19.0 * k), &text, small.clone(), theme.fg_dim, area.width() - 12.0 * k);
            }
            if save {
                act = Some(ReviewAct::Save);
            } else if now {
                act = Some(ReviewAct::SendNow);
            } else if cancel {
                act = Some(ReviewAct::Cancel);
            }
            return act;
        }
        let drafts = self.review.drafts();
        let row = Rect::from_min_size(area.min, egui::vec2(area.width(), 30.0 * k));
        painter.rect_filled(row, 7.0 * k, theme.bg_alt);
        painter.rect_stroke(row, 7.0 * k, Stroke::new(1.0, theme.border), egui::StrokeKind::Inside);
        let cy = row.center().y;
        let questions = drafts.iter().filter(|c| c.ask == Ask::Question).count();
        let changes = drafts.len() - questions;
        let rw = crate::chrome::text(painter, egui::pos2(row.left() + 10.0 * k, cy), "Review", small.clone(), theme.accent, 80.0 * k);
        let mut parts = Vec::new();
        if questions > 0 {
            parts.push(format!("{questions} question{}", if questions == 1 { "" } else { "s" }));
        }
        if changes > 0 {
            parts.push(format!("{changes} change{}", if changes == 1 { "" } else { "s" }));
        }
        let summary = format!("{} comment{} · {} · sent with your message", drafts.len(), if drafts.len() == 1 { "" } else { "s" }, parts.join(", "));
        let (rx, discard) = button(ui, row.right() - 4.0 * k, cy, "×", false, "discard");
        let (rs, toggle) = button(ui, rx.left() - 4.0 * k, cy, if self.review_open { "Hide" } else { "Show" }, false, "toggle");
        crate::chrome::text(painter, egui::pos2(row.left() + 18.0 * k + rw, cy), &summary, small.clone(), theme.fg_dim, (rs.left() - row.left() - rw - 28.0 * k).max(0.0));
        if discard {
            act = Some(ReviewAct::Discard);
        } else if toggle {
            act = Some(ReviewAct::Toggle);
        }
        if self.review_open {
            for (i, c) in drafts.iter().enumerate() {
                let r = Rect::from_min_size(egui::pos2(row.left(), row.bottom() + 4.0 * k + i as f32 * 24.0 * k), egui::vec2(row.width(), 24.0 * k));
                let cy = r.center().y;
                let resp = ui.interact(r, ui.scope_id().with(("review-item", c.id)), egui::Sense::click()).on_hover_text("Change it");
                if resp.hovered() {
                    painter.rect_filled(r, 5.0 * k, crate::chrome::hover_bg(theme));
                }
                let (kind, kc) = kind_of(c.ask);
                let kw = crate::chrome::text(painter, egui::pos2(r.left() + 10.0 * k, cy), kind, small.clone(), kc, 90.0 * k);
                let mut x = r.left() + 18.0 * k + kw;
                if !c.label.is_empty() {
                    x += crate::chrome::text(painter, egui::pos2(x, cy), &c.label, small.clone(), theme.fg_faint, 140.0 * k) + 10.0 * k;
                }
                let xr = Rect::from_center_size(egui::pos2(r.right() - 14.0 * k, cy), egui::vec2(20.0 * k, 20.0 * k));
                let xresp = ui.interact(xr, ui.scope_id().with(("review-delete", c.id)), egui::Sense::click()).on_hover_text("Delete it");
                painter.text(xr.center(), egui::Align2::CENTER_CENTER, "×", small.clone(), if xresp.hovered() { theme.fg } else { theme.fg_faint });
                crate::chrome::text(painter, egui::pos2(x, cy), c.text.lines().next().unwrap_or(""), small.clone(), theme.fg, (xr.left() - x - 8.0 * k).max(0.0));
                if xresp.clicked() {
                    act = Some(ReviewAct::Delete(c.id));
                } else if resp.clicked() {
                    act = Some(ReviewAct::Edit(c.id));
                }
            }
        }
        act
    }

    /// Follow a `find:` link: to the text it names, in the agent's messages before the review
    /// that has it (or after it, for an answer).
    fn jump_to_find(&mut self, i: usize) {
        let Some((text, ei, after)) = self.find_targets.get(i).cloned() else { return };
        let order: Vec<usize> = if after { (ei + 1..self.entries.len()).collect() } else { (0..ei).rev().collect() };
        let found = order.into_iter().filter(|e| matches!(self.entries.get(*e), Some(Entry::Assistant { .. }))).find_map(|e| locate_quote(&self.lines, &self.line_entry, e, &text));
        if let Some((line, r)) = found.and_then(|s| s.into_iter().next()) {
            self.view.anchor = None;
            self.view.cursor = Pos::new(line, r.start);
            self.view.follow_tail = false;
            self.view.reveal_cursor();
            self.focus = Focus::Transcript;
        }
    }

    /// Height of the composer's header: the comment being written, or the review's row.
    fn review_header_h(&self, k: f32) -> f32 {
        if let Some((w, _)) = &self.commenting {
            let quote_lines = w.quote.lines().count().clamp(1, 2) as f32;
            return (12.0 + 24.0 + quote_lines * 19.0 + 8.0) * k;
        }
        let drafts = self.review.drafts().len();
        if drafts == 0 {
            return 0.0;
        }
        (40.0 + if self.review_open { drafts as f32 * 24.0 + 6.0 } else { 0.0 }) * k
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
        // The review: read once; a comment being written is kept as it is typed.
        self.ensure_review();
        if self.commenting.is_some() {
            let text = self.prompt.text();
            if text != self.writing_saved.0 && self.writing_saved.1.is_none_or(|t| t.elapsed() > Duration::from_millis(800)) {
                self.persist_review(ed);
            }
        }
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
        // Settings that change how the transcript is laid out.
        let settings = {
            use std::hash::{Hash, Hasher};
            let mut h = std::collections::hash_map::DefaultHasher::new();
            for name in ["chat-measure", "chat-diagrams", "chat-report", "chat-report-min-words", "chat-report-scale", "chat-report-background"] {
                ed.var(name).map(Value::to_string).hash(&mut h);
            }
            h.finish()
        };
        let style_key = ed.theme.layout_key() ^ ed.theme_generation.rotate_left(32) ^ show_thinking.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ settings.rotate_left(17);
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
        // Above the text: the comment being written, or the review waiting to be sent.
        let review_h = self.review_header_h(k);
        let card = Rect::from_min_max(egui::pos2(left, rect.bottom() - 14.0 * k - text_h - toolbar_h - att_h - review_h), egui::pos2(left + col_w, rect.bottom() - 14.0 * k));
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
                // A comment's card: change it.
                match self.comment_lines.iter().find(|(line, _)| *line == l).map(|(_, id)| *id) {
                    Some(id) => self.edit_comment(ed, id),
                    None => {
                        self.toggle_fold_at(l);
                    }
                }
            }
        }
        // A link within the chat: what a review's comment is about, or the answer to it.
        if let Some(i) = resp.link.as_deref().and_then(|l| l.strip_prefix("find:")).and_then(|i| i.parse::<usize>().ok()) {
            self.jump_to_find(i);
        }
        // A code reference: that line, in the diff page of the chat's folder (or the file).
        // (Its folder is asked for from the main loop: while drawn, the chat is out of its slot.)
        if let Some((path, line)) = resp.link.as_deref().filter(|l| !l.starts_with("find:")).and_then(code_ref) {
            ed.main_sender().send(move |ed| match ed.buffer_directory(id) {
                Some(dir) => crate::diff::show_line(ed, ed.buffer_machine(id), dir.to_string_lossy().into_owned(), &path, line),
                None => ed.message("This chat has no folder to look in"),
            });
        }

        let painter = ui.painter().clone();
        // A selection in what the agent wrote: a bar to ask about it or comment on it.
        let mut bar_rects = Vec::new();
        let picked = self.draw_selection_bar(ui, &painter, transcript_rect, &theme, k, &mut bar_rects);
        for (name, r) in bar_rects {
            ed.debug_rects.insert(name, r);
        }
        if let Some(ask) = picked {
            self.start_comment(ed, ask);
        }
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
        let mut header_rects = Vec::new();
        let review_act = if review_h > 0.0 { self.draw_review_header(ui, &painter, Rect::from_min_max(egui::pos2(card.left() + 12.0 * k, card.top() + att_h + 8.0 * k), egui::pos2(card.right() - 12.0 * k, card.top() + att_h + review_h)), &theme, k, &mut header_rects) } else { None };
        for (key, r) in header_rects {
            ed.debug_rects.insert(match key { "save" => "review-save", "now" => "review-send-now", "cancel" => "review-cancel", "discard" => "review-discard", _ => "review-toggle" }, r);
        }
        match review_act {
            Some(ReviewAct::Save) => self.save_comment(ed),
            Some(ReviewAct::SendNow) => self.send_comment_now(ed),
            Some(ReviewAct::Cancel) => self.cancel_comment(ed),
            Some(ReviewAct::Kind) => self.cycle_comment_kind(None),
            Some(ReviewAct::Toggle) => self.review_open = !self.review_open,
            Some(ReviewAct::Discard) => ed.main_sender().send(|ed| ed.call("chat-review-discard")),
            Some(ReviewAct::Edit(id)) => self.edit_comment(ed, id),
            Some(ReviewAct::Delete(id)) => self.delete_comment(ed, id),
            None => {}
        }
        if review_act.is_some() {
            self.focus = Focus::Prompt;
        }
        let review_h = self.review_header_h(k).min(review_h);
        let text_rect = Rect::from_min_max(egui::pos2(card.left(), card.top() + att_h + review_h), egui::pos2(card.right(), card.bottom() - toolbar_h));
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
        let pill_w = |label: &str, icon: bool| crate::chrome::text_width(&painter, label, small.clone()) + if icon { 30.0 } else { 20.0 } * k;
        // `max`: the widest it may be (its label is cut to fit).
        let pill = |ui: &mut egui::Ui, x: f32, key: &str, label: &str, tip: &str, icon: Option<Color32>, max: f32| -> (Rect, bool) {
            let w = pill_w(label, icon.is_some()).min(max);
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
            crate::chrome::text(&painter, egui::pos2(tx, cy), label, small.clone(), if resp.hovered() { theme.fg } else { theme.fg_dim }, (r.right() - tx - 6.0 * k).max(0.0));
            (r, resp.clicked())
        };
        let model = self.model_short().unwrap_or_else(|| "Choose a model".into());
        let thinking = format!("Thinking · {}", self.thinking_level().as_str());
        // Where the chat runs: this machine, or another one's node.
        let machine = match &self.node {
            r if r.local() && r.online => "local".to_string(),
            r if r.local() => "local · offline".to_string(),
            r if r.online => format!("⇄ {}", r.machine),
            r => format!("⇄ {} · offline", r.machine),
        };
        // Whether a turn cut off by its machine's node stopping goes on by itself.
        let (label, tip) = if self.keep_going() {
            ("↻ Keeps going", "If its machine's serpis-node stops mid-turn (a crash, a reboot), the turn continues when it starts again. Click to make it wait for you instead (chat-toggle-keep-going; default: chat-keep-going)")
        } else {
            ("⏸ Waits", "If its machine's serpis-node stops mid-turn (a crash, a reboot), the turn waits for you to send a message. Click to make it keep going (chat-toggle-keep-going; default: chat-keep-going)")
        };
        // What doesn't fit before the send button is left out: keep-going first, then the
        // machine, then thinking; the model is cut to what is left. The pills after them (the
        // permission mode, background work, a limit) always show.
        let running = self.background_tasks();
        let warning = self.limit_warning();
        let mut after = 0.0;
        if let Some(mode) = &self.node.state.permission_mode {
            after += pill_w(&permission_mode_label(mode, &theme).0, false) + 2.0 * k;
        }
        if !running.is_empty() {
            after += pill_w(&format!("· {}", background_counts(&running)), false) + 2.0 * k;
        }
        if let Some(l) = &warning {
            after += pill_w(&format!("⚠ {}% of {}", (l.utilization.unwrap_or(0.0) * 100.0).round(), l.name()), false) + 2.0 * k;
        }
        // Room is kept for the context meter too (drawn later, beside the send button), unless
        // the model wouldn't fit then.
        let meter = match self.context() {
            (used, window @ 1..) => {
                let label = format!("{:.0}%", used.unwrap_or(0) as f64 * 100.0 / window as f64);
                32.0 * k + crate::chrome::text_width(&painter, &label, small.clone()) + 26.0 * k
            }
            _ => 0.0,
        };
        let widths = [pill_w(&model, true), pill_w(&thinking, false), pill_w(&machine, false), pill_w(label, false)];
        // (pills shown, the model's widest) in `room`.
        let fit = |room: f32| {
            let mut shown = 4;
            while shown > 1 && widths[..shown].iter().sum::<f32>() + 2.0 * k * (shown - 1) as f32 > room {
                shown -= 1;
            }
            (shown, room - widths[1..shown].iter().map(|w| w + 2.0 * k).sum::<f32>())
        };
        let room = bar.width() - 28.0 * k - 8.0 * k - after;
        let (shown, model_max) = match fit(room - meter) {
            (_, m) if m < 40.0 * k => fit(room),
            f => f,
        };
        // A pill left out: nothing, where the next one would start.
        let hidden = |prev: Rect| (Rect::from_min_size(egui::pos2(prev.right(), cy - 13.0 * k), egui::vec2(0.0, 26.0 * k)), false);
        // Too narrow even for the model (a sliver of a window): nothing.
        let start = Rect::from_min_size(egui::pos2(bar.left() - 2.0 * k, cy - 13.0 * k), egui::vec2(0.0, 26.0 * k));
        let (mr, clicked) = if model_max >= 40.0 * k { pill(ui, bar.left(), "model", &model, "Model (chat-select-model)", Some(theme.accent), model_max) } else { hidden(start) };
        if clicked {
            run = Some("chat-select-model");
        }
        let (tr, clicked) = if shown > 1 { pill(ui, mr.right() + 2.0 * k, "thinking", &thinking, "Thinking level (chat-select-thinking)", None, f32::INFINITY) } else { hidden(mr) };
        if clicked {
            run = Some("chat-select-thinking");
        }
        let (wr, clicked) = if shown > 2 { pill(ui, tr.right() + 2.0 * k, "machine", &machine, "Where this chat runs (chat-select-machine)", None, f32::INFINITY) } else { hidden(tr) };
        if clicked {
            run = Some("chat-select-machine");
        }
        let (mut kr, clicked) = if shown > 3 { pill(ui, wr.right() + 2.0 * k, "keep-going", label, tip, None, f32::INFINITY) } else { hidden(wr) };
        let keep_r = kr;
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
        if !running.is_empty() {
            let label = format!("· {}", background_counts(&running));
            let mut tip: Vec<String> = running.iter().map(|t| format!("{}: {}", task_kind(t).0, t.description)).collect();
            tip.push("Running in the background. Click to see it (chat-show-task); C-c C-k stops one (chat-stop-task)".into());
            let (br, clicked) = pill(ui, kr.right() + 2.0 * k, "background", &label, &tip.join("\n"), None, f32::INFINITY);
            hits.push(("background", br));
            kr = br;
            if clicked {
                run = Some("chat-show-task");
            }
        }
        // Close to a usage limit: how close, until it resets.
        if let Some(l) = warning {
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
        // Something to send: text, images, or a review waiting (as `send` has it).
        let has_text = !self.prompt.doc.is_empty() || !self.attachments.is_empty() || !self.review.drafts().is_empty();
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

        // Usage, activity and the context meter between the pills and the send button. Short of
        // room, the meter stays (it says when the chat compacts), then the usage, then the
        // activity: room for the meter is kept while the others are placed.
        let mut right = send.left() - 12.0 * k;
        let (used, window) = self.context();
        let meter_label = format!("{:.0}%", used.unwrap_or(0) as f64 / window.max(1) as f64 * 100.0);
        let meter_w = if window > 0 { 26.0 * k + 6.0 * k + crate::chrome::text_width(&painter, &meter_label, small.clone()) + 14.0 * k } else { 0.0 };
        // Session usage, like pi's footer: first, so it shows while the agent works too. Short
        // of room (with the activity beside it), just what it cost, or its tokens in and out
        // where nothing was charged (a subscription).
        if ed.var("chat-show-usage").and_then(Value::as_bool).unwrap_or(true) {
            let full = self.usage_label();
            let u = &self.node.state.usage;
            let short = if u.cost.total > 0.0 { format!("${:.3}", u.cost.total) } else { format!("↑{} ↓{}", fmt_tokens(u.input), fmt_tokens(u.output)) };
            let others = if self.running() { 110.0 * k } else { 0.0 };
            let room = right - tr.right() - 8.0 * k - meter_w;
            let fw = crate::chrome::text_width(&painter, &full, small.clone());
            let label = if fw + others <= room { full } else { short };
            let w = crate::chrome::text_width(&painter, &label, small.clone());
            if w <= room {
                hits.push(("usage", Rect::from_min_max(egui::pos2(right - w, cy - 8.0 * k), egui::pos2(right, cy + 8.0 * k))));
                crate::chrome::text(&painter, egui::pos2(right - w, cy), &label, small.clone(), theme.fg_faint, w + 1.0);
                right -= w + 14.0 * k;
            }
        }
        if self.running() {
            let label = format!("Working {}", fmt_duration(self.started.map(|s| s.elapsed()).unwrap_or_default(), false));
            let w = crate::chrome::text_width(&painter, &label, small.clone());
            if right - w - 18.0 * k - meter_w > tr.right() {
                hits.push(("activity", Rect::from_min_max(egui::pos2(right - w - 16.0 * k, cy - 8.0 * k), egui::pos2(right, cy + 8.0 * k))));
                crate::chrome::text(&painter, egui::pos2(right - w, cy), &label, small.clone(), theme.accent, w + 1.0);
                crate::chrome::spinner(&painter, egui::pos2(right - w - 11.0 * k, cy), 5.0 * k, ui.input(|i| i.time), theme.accent);
                right -= w + 30.0 * k;
            }
            ui.ctx().request_repaint_after(Duration::from_millis(60));
        }
        // How full the context is: a small meter, yellow as it nears where it compacts.
        if window > 0 {
            let used = used.unwrap_or(0);
            let s = &self.node.state;
            let frac = used as f64 / window as f64;
            let near = match s.compacts_at {
                Some(at) => used as f64 >= at as f64 * 0.85,
                None => frac >= 0.85,
            };
            let color = if near { theme.yellow } else { theme.fg_faint };
            let label = format!("{:.0}%", frac * 100.0);
            let lw = crate::chrome::text_width(&painter, &label, small.clone());
            let bar_w = 26.0 * k;
            let w = bar_w + 6.0 * k + lw;
            if right - w - 14.0 * k > tr.right() {
                let r = Rect::from_min_max(egui::pos2(right - w, cy - 10.0 * k), egui::pos2(right, cy + 10.0 * k));
                let mut tip = format!("Context: {} of {} tokens ({:.0}%)", fmt_tokens(used), fmt_tokens(window), frac * 100.0);
                if let Some(at) = s.compacts_at {
                    tip.push_str(&format!("\nCompacts by itself at {} ({:.0}%)", fmt_tokens(at), at as f64 * 100.0 / window as f64));
                }
                if s.compactions > 0 {
                    tip.push_str(&format!("\nCompacted {} in this chat", if s.compactions == 1 { "once".to_string() } else { format!("{} times", s.compactions) }));
                }
                tip.push_str("\nchat-compact compacts it now");
                ui.interact(r, id.with("context"), egui::Sense::hover()).on_hover_text(tip);
                let bar = Rect::from_min_size(egui::pos2(r.left(), cy - 2.5 * k), egui::vec2(bar_w, 5.0 * k));
                painter.rect_filled(bar, 2.5 * k, theme.chip());
                painter.rect_filled(Rect::from_min_size(bar.min, egui::vec2((bar_w * frac.min(1.0) as f32).max(2.0 * k), bar.height())), 2.5 * k, if near { theme.yellow } else { crate::chrome::tint(theme.fg, 0.35) });
                // Where it compacts: a tick on the bar.
                if let Some(at) = s.compacts_at.filter(|a| *a < window) {
                    let x = bar.left() + bar_w * (at as f64 / window as f64) as f32;
                    painter.vline(x, (bar.top() - 2.0 * k)..=(bar.bottom() + 2.0 * k), Stroke::new(1.0, theme.fg_dim));
                }
                crate::chrome::text(&painter, egui::pos2(bar.right() + 6.0 * k, cy), &label, small.clone(), color, lw + 1.0);
                hits.push(("context", r));
            }
        }
        hits.extend([("model", mr), ("thinking", think_r), ("machine", wr), ("keep-going", keep_r), ("send", send), ("card", card)].into_iter().filter(|(_, r)| r.width() > 0.0));
        // The toolbar's row: its pills, labels and the send button.
        const TOOLBAR: [&str; 11] = ["model", "thinking", "machine", "keep-going", "permission-mode", "background", "limit", "context", "activity", "usage", "send"];
        let row: Vec<(&str, Rect)> = hits.iter().filter(|(n, _)| TOOLBAR.contains(n)).copied().collect();
        ed.check_row("composer", card, &row);
        self.composer_hits = hits;
        if let Some(cmd) = run {
            self.focus = Focus::Prompt;
            ed.main_sender().send(move |ed| ed.call(cmd));
        }
        // Keys for a selection (comment on it) and for a comment being written.
        if let Some(me) = ed.this_buffer() {
            let selecting = self.focus == Focus::Transcript && self.commenting.is_none() && self.view.anchor.is_some_and(|a| a != self.view.cursor);
            let commenting = self.commenting.is_some();
            if let Some(m) = ed.meta_mut(me) {
                for (mode, on) in [("chat-selection", selecting), ("chat-commenting", commenting)] {
                    let has = m.minor_modes.iter().any(|x| x == mode);
                    if on && !has {
                        m.minor_modes.insert(0, mode.to_string());
                    } else if !on && has {
                        m.minor_modes.retain(|x| x != mode);
                    }
                }
            }
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
        // Where it works: clicking it chooses another folder. Last: the first to fit in a narrow
        // window (the composer has the model too).
        status.push(folder_label(&self.state_now().cwd, !self.node.local()));
        status_commands.push(Some("chat-select-folder".into()));
        Modeline { title: self.state_now().title, status, status_commands, busy: self.running() }
    }

    fn activity(&self) -> Activity {
        let s = self.state_now();
        if s.compacting {
            return Activity::new(ActivityState::Working, "compacting");
        }
        // Asking you something (a permission, its questions, a plan): waiting for you, not working.
        if s.running
            && let Some((_, name, args, _)) = self.waiting()
        {
            let what = match name.as_str() {
                "ExitPlanMode" => "approve its plan".to_string(),
                "AskUserQuestion" => format!("answer: {}", serpis_chat::tool_summary(&name, &args)),
                _ => match serpis_chat::tool_summary(&name, &args) {
                    s if s.is_empty() => format!("allow {name}"),
                    s => format!("allow {name} {s}"),
                },
            };
            return Activity { since: self.started, ..Activity::new(ActivityState::Waiting, what) };
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
    fn range_rects(&self, line: usize, s: usize, e: usize) -> Vec<Rect> {
        match self.focus {
            Focus::Transcript => self.view.range_rects(line, s, e),
            Focus::Prompt => self.prompt.range_rects(line, s, e),
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
    fn usage_shows_a_cost_only_when_one_was_charged() {
        let mut u = Usage { input: 12_000, output: 3_400, cache_read: 1_200_000, ..Default::default() };
        // A Claude Code or Codex subscription: its tokens, no cost.
        assert_eq!(usage_label(&u), "↑12.0k ↓3.4k R1.2M");
        u.cost.total = 1.2344;
        assert_eq!(usage_label(&u), "↑12.0k ↓3.4k R1.2M $1.234");
    }

    /// Inline markdown as text and its faced parts.
    fn md(s: &str) -> (String, Vec<(String, Face)>) {
        let (text, spans) = inline_spans(s);
        let parts = spans.iter().map(|(r, f)| (text[r.clone()].to_string(), *f)).collect();
        (text, parts)
    }

    #[test]
    fn inline_markdown_nests_and_emphasizes() {
        use Face::*;
        let p = |a: &str, f: Face| (a.to_string(), f);
        // Code in bold: its backticks go, it stays code, and the rest is bold.
        assert_eq!(md("**`node_tape.rs:129-403` — the book feed**"), ("node_tape.rs:129-403 — the book feed".into(), vec![p("node_tape.rs:129-403", InlineCode), p(" — the book feed", Bold)]));
        assert_eq!(md("*Suggestion:* write it"), ("Suggestion: write it".into(), vec![p("Suggestion:", Italic)]));
        assert_eq!(md("- **Suggestion**, ideally"), ("- Suggestion, ideally".into(), vec![p("Suggestion", Bold)]));
        assert_eq!(md("_quietly_ and __loudly__"), ("quietly and loudly".into(), vec![p("quietly", Italic), p("loudly", Bold)]));
        assert_eq!(md("***both*** and ~~gone~~"), ("both and gone".into(), vec![p("both", BoldItalic), p("gone", Strike)]));
        assert_eq!(md("**bold with *italic* inside**"), ("bold with italic inside".into(), vec![p("bold with ", Bold), p("italic", BoldItalic), p(" inside", Bold)]));
        assert_eq!(md("see [the docs](https://example.com) now"), ("see the docs now".into(), vec![p("the docs", Link)]));
        // Code spans: any run of backticks, padding trimmed, no markdown inside.
        assert_eq!(md("``a ` b`` and ` x `"), ("a ` b and x".into(), vec![p("a ` b", InlineCode), p("x", InlineCode)]));
        assert_eq!(md("`**not bold**`"), ("**not bold**".into(), vec![p("**not bold**", InlineCode)]));
        assert_eq!(md("**`a*b`**"), ("a*b".into(), vec![p("a*b", InlineCode)]));
        // Left as written: names with underscores, lone markers, math, escapes, the unclosed.
        assert_eq!(md("skip_unknown and is_known_key"), ("skip_unknown and is_known_key".into(), vec![]));
        assert_eq!(md("a * b * c, 2*3"), ("a * b * c, 2*3".into(), vec![]));
        assert_eq!(md(r"\*not italic\*"), ("*not italic*".into(), vec![]));
        assert_eq!(md("**unclosed and `x"), ("**unclosed and `x".into(), vec![]));
        assert_eq!(md("plain"), ("plain".into(), vec![]));
    }

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
    fn a_finished_run_ends_with_its_report() {
        use super::report_entries;
        let user = |t: &str, queued: bool| Entry::User { text: t.into(), queued, images: Vec::new() };
        let said = |t: &str| Entry::Assistant { blocks: vec![AssistantContent::Text { text: t.into(), text_signature: None }], model: "m".into(), streaming: false, continuation: false, usage: None, error: None, cache: None };
        let long = "word ".repeat(100);
        let entries = vec![user("go", false), said("Looking."), said(&long), user("again", false), said("done"), user("more", false), said(&long)];
        // The first run's last message is long: its report. "done" is too short. The last run
        // still goes on.
        assert_eq!(report_entries(&entries, true, 80).into_iter().collect::<Vec<_>>(), vec![(2, 100)]);
        let mut done = report_entries(&entries, false, 80).into_iter().collect::<Vec<_>>();
        done.sort();
        assert_eq!(done, vec![(2, 100), (6, 100)]);
        // Only the last message of a run: narration before it is not a report.
        let entries = vec![user("go", false), said(&long), said("short"), user("next", false)];
        assert!(report_entries(&entries, false, 80).is_empty());
        // A prompt queued while it runs steers the run; it does not end it.
        let entries = vec![user("go", false), said(&long), user("also this", true), said(&long)];
        assert_eq!(report_entries(&entries, false, 80).into_keys().collect::<Vec<_>>(), vec![3]);
    }

    #[test]
    fn reports_in_the_skills_shape_are_read_as_parts() {
        use super::{Outcome, check_mark, shaped_report};
        let text = "Done: the change is sound, but `a.b` is counted twice. It moves retries.\n\n## Needs you\n\n1. **Fix it.** Now.\n2. **Check it.**\n\n## Checked\n\n- **Tests:** passes.\n- **Not checked:** load.\n";
        let sh = shaped_report(text).unwrap();
        assert_eq!(sh.outcome, Outcome::Done);
        // The first sentence, even with a period inside inline code.
        assert_eq!(sh.headline, "The change is sound, but `a.b` is counted twice.");
        assert_eq!(sh.intro, "It moves retries.");
        assert_eq!(sh.sections.iter().map(|(t, _)| t.as_str()).collect::<Vec<_>>(), ["Needs you", "Checked"]);
        assert_eq!(sh.needs_you(), 2);
        assert_eq!(shaped_report("**Partly done:** half of it.").unwrap().outcome, Outcome::Partly);
        assert_eq!(shaped_report("Blocked — no access.").unwrap().outcome, Outcome::Blocked);
        // Anything else stays plain markdown.
        assert!(shaped_report("I've finished the review.").is_none());
        assert!(shaped_report("Doneness is relative.").is_none());
        assert_eq!(check_mark("Tests: passes.").map(|m| m.0), Some("✓"));
        assert_eq!(check_mark("Lint: failed on 2 files").map(|m| m.0), Some("✗"));
        assert_eq!(check_mark("Not checked: the dashboards").map(|m| m.0), Some("○"));
        assert_eq!(check_mark("Benchmarks: 120 ms → 40 ms"), None);
    }

    #[test]
    fn code_references_are_links() {
        use super::code_ref;
        assert_eq!(code_ref("src/queue.rs:142"), Some(("src/queue.rs".into(), 142)));
        assert_eq!(code_ref("./main.rs:3-9"), Some(("main.rs".into(), 3)));
        assert_eq!(code_ref("lib.rs:7:12"), Some(("lib.rs".into(), 7)));
        for no in ["RequestQueue::retry", "localhost:8080", "a b.rs:3", "x.rs:", "x.rs:0", "x.rs:12abc", "Ordering::AcqRel"] {
            assert_eq!(code_ref(no), None, "{no}");
        }
        use super::code_links;
        let link = |text: &str| code_links(text).into_iter().map(|(r, s)| (r.start, s)).collect::<Vec<_>>();
        // Inline code (its backticks gone when drawn) and plain prose.
        assert_eq!(link("Fixed (src/queue.rs:142) in RequestQueue::retry."), vec![(7, "src/queue.rs:142".into())]);
        assert_eq!(link("See src/a.rs:3, then lib.rs:7:2."), vec![(4, "src/a.rs:3".into()), (21, "lib.rs:7:2".into())]);
        // What tools print: grep, a compiler.
        assert_eq!(link("src/a.rs:12:    let x = 1;"), vec![(0, "src/a.rs:12".into())]);
        assert_eq!(link("  --> src/main.rs:12:5"), vec![(6, "src/main.rs:12:5".into())]);
        // Columns count characters, not bytes.
        assert_eq!(link("é → src/a.rs:1"), vec![(4, "src/a.rs:1".into())]);
        assert!(code_links("at 12:30, Ordering::AcqRel, https://example.com/x").is_empty());
    }

    #[test]
    fn tables_measure_columns() {
        let t = crate::theme::Theme::default();
        let m = Measure::Chars(8.0);
        let md = "Results:\n\n| Run | Lead |\n|---|--:|\n| 1 connection, 60s | **91%** |\n| 3 | `x` |\n\nafter";
        let lines = markdown_lines(md, &t, &m, 2000.0, false);
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
        assert!(table(&markdown_lines("| not a table", &t, &m, 2000.0, false)).is_empty());
    }

    #[test]
    fn table_digits_line_up() {
        // A 1 half as wide as other digits: right-aligned, 71% starts where 78% does, the 1
        // centered in a digit's width; and a 1 in code is left as it is.
        let t = crate::theme::Theme::default();
        let m = Measure::NarrowOne(8.0);
        let rows = markdown_lines("| Hit rate |\n|--:|\n| 71% |\n| 78% |\n| `1` |", &t, &m, 2000.0, false);
        let rows = table(&rows);
        let x = |l: &StyledLine, at: usize| -> f32 { l.pads.iter().filter(|(o, _)| *o <= at).map(|(_, w)| *w).sum::<f32>() + m.width(&l.text[..at], l.face) };
        assert_eq!(x(rows[1], 0), x(rows[2], 0), "71% starts where 78% does");
        assert_eq!(x(rows[1], 1), x(rows[2], 1) + 2.0, "its 1 centered in a digit's width");
        assert_eq!(x(rows[1], 2), x(rows[2], 2), "and the % under the %");
        assert!(rows[3].pads.iter().all(|(o, _)| *o == 0), "code's digits as they are");
    }

    #[test]
    fn narrow_tables_wrap_inside_cells() {
        let t = crate::theme::Theme::default();
        let m = Measure::Chars(8.0);
        let md = "| Run | Result |\n|---|---|\n| 1 connection, 180s, Rust and Python together | ~1% / -90ms in both |\n| 3 | ok |";
        let rows = markdown_lines(md, &t, &m, 300.0, false);
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
        let lines = markdown_lines("- one\n- two\n  - nested\n3. three", &t, &Measure::Chars(8.0), 800.0, false);
        let markers: Vec<&str> = lines.iter().filter_map(|l| l.marker.as_ref().map(|(m, _)| m.as_str())).collect();
        assert_eq!(markers, ["•", "•", "•", "3."]);
        assert_eq!(lines[0].text, "one");
        assert!(lines[2].indent > lines[1].indent);
    }
}
