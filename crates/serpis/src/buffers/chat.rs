//! Agent chat buffer.
//!
//! The transcript is generated styled text (Emacs-style faces and blocks): messages, thinking,
//! foldable tool cards and diff views. The prompt is an editable area pinned to the bottom.
//! Both form one composite [`TextSurface`] (transcript lines first, then prompt lines), so
//! motions, vim, search and yank move seamlessly between them.

use std::ops::Range;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use egui::{Color32, Rect, Stroke, Vec2};
use serde_json::Value;
use serpis_agent::{AgentConfig, AgentEvent, AgentHandle, ToolGate, ToolHooks, ToolResult};
use serpis_ai::*;

use crate::buffer::{Activity, ActivityState, Buffer, Modeline};
use crate::editor::{Editor, MainSender};
use crate::impl_any;
use crate::text::{Block, CursorShape, DocView, LineSource, Pos, StyledLine, TextArea, TextSurface, ViewOptions, char_len};
use crate::theme::Face;

#[derive(Debug, Clone)]
enum Entry {
    User {
        text: String,
        queued: bool,
    },
    Assistant {
        blocks: Vec<AssistantContent>,
        model: String,
        streaming: bool,
        continuation: bool,
        usage: Option<Usage>,
        error: Option<String>,
        /// Thinking folded by hand (click or TAB): Some(true) all of it, Some(false) only its
        /// header; None follows chat-show-thinking.
        thinking: Option<bool>,
    },
    Tool {
        id: String,
        name: String,
        args: Value,
        partial: Option<String>,
        result: Option<ToolResultMessage>,
        expanded: Option<bool>,
        started: Option<Instant>,
        elapsed: Option<Duration>,
    },
    Info {
        text: String,
        face: Face,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Transcript,
    Prompt,
}

pub struct ChatBuffer {
    entries: Vec<Entry>,
    pub history: Vec<Message>,
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
    agent: Option<AgentHandle>,
    tx: mpsc::Sender<AgentEvent>,
    rx: mpsc::Receiver<AgentEvent>,
    pub model: Option<Model>,
    pub thinking: ThinkingLevel,
    pub session_id: String,
    pub cwd: std::path::PathBuf,
    totals: Usage,
    started: Option<Instant>,
    prompt_history: Vec<String>,
    history_index: Option<usize>,
    /// Short title: the first prompt unless set (`chat-set-title`).
    pub title: String,
    /// Error of the last run, if it failed.
    last_error: Option<String>,
    /// Width of the transcript's content column (tables are laid out for it).
    text_w: f32,
    /// Theme and font state the transcript was rendered with (cards carry its colors).
    style_key: u64,
    /// Where the composer's controls were drawn last frame (tests click them).
    pub composer_hits: Vec<(&'static str, Rect)>,
    /// Custom session entries (pi's `custom` entries): (type, data), oldest first.
    pub custom_entries: Vec<(String, Value)>,
    /// Where the conversation is recorded (created with the first prompt).
    pub session: Option<crate::session::SessionWriter>,
    session_failed: bool,
}

const PREVIEW_LINES: usize = 10;

impl ChatBuffer {
    /// A chat in `params.cwd`, else in the editor's directory.
    pub fn new(ed: &mut Editor, params: &Value) -> Self {
        let (tx, rx) = mpsc::channel();
        let mut view = DocView::default();
        view.follow_tail = true;
        view.padding = egui::vec2(18.0, 12.0);
        let mut prompt = TextArea::new("");
        prompt.view.padding = egui::vec2(14.0, 10.0);
        let model = default_model(ed);
        let thinking = ed
            .var_str("agent-thinking")
            .and_then(|s| ThinkingLevel::parse(&s))
            .or_else(|| pi_setting("defaultThinkingLevel").and_then(|s| ThinkingLevel::parse(&s)))
            .unwrap_or(ThinkingLevel::Medium);
        let mut chat = ChatBuffer {
            entries: Vec::new(),
            history: Vec::new(),
            prompt,
            lines: Vec::new(),
            line_entry: Vec::new(),
            thinking_lines: Vec::new(),
            show_thinking: true,
            dirty: true,
            view,
            focus: Focus::Prompt,
            agent: None,
            tx,
            rx,
            model,
            thinking,
            session_id: uuid::Uuid::new_v4().to_string(),
            cwd: params.get("cwd").and_then(Value::as_str).map(crate::expand_home).unwrap_or_else(|| ed.cwd.clone()),
            totals: Usage::default(),
            started: None,
            prompt_history: Vec::new(),
            history_index: None,
            title: String::new(),
            last_error: None,
            text_w: 0.0,
            style_key: 0,
            composer_hits: Vec::new(),
            custom_entries: Vec::new(),
            session: None,
            session_failed: false,
        };
        if chat.model.is_none() {
            chat.entries.push(Entry::Info {
                text: "No model configured. Run `chat-select-model` (C-c C-m) — models come from your pi/serpis models.json, auth.json and API key environment variables.".into(),
                face: Face::Warning,
            });
        }
        chat
    }

    pub fn running(&self) -> bool {
        self.agent.is_some()
    }

    pub fn abort(&mut self) {
        if let Some(a) = &self.agent {
            a.abort();
        }
    }

    pub fn clear(&mut self) {
        self.abort();
        self.entries.clear();
        self.history.clear();
        self.totals = Usage::default();
        self.session_id = uuid::Uuid::new_v4().to_string();
        self.title.clear();
        self.last_error = None;
        self.session = None;
        self.custom_entries.clear();
        self.dirty = true;
    }

    pub fn set_model(&mut self, m: Model) {
        self.thinking = clamp_thinking_level(&m, self.thinking);
        self.model = Some(m);
        self.dirty = true;
    }

    pub fn cycle_thinking(&mut self) {
        let levels = self.model.as_ref().map(supported_thinking_levels).unwrap_or_else(|| ThinkingLevel::ALL.to_vec());
        let i = levels.iter().position(|l| *l == self.thinking).map(|i| (i + 1) % levels.len()).unwrap_or(0);
        self.thinking = levels[i];
    }

    pub fn set_thinking(&mut self, level: ThinkingLevel) {
        self.thinking = self.model.as_ref().map(|m| clamp_thinking_level(m, level)).unwrap_or(level);
        self.dirty = true;
    }

    /// Send the prompt: start a run, or steer the running one.
    pub fn send(&mut self, ed: &mut Editor, follow_up: bool) {
        let text = self.prompt.text().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.prompt.clear();
        self.prompt_history.push(text.clone());
        self.history_index = None;
        if !self.submit(ed, text.clone(), follow_up) {
            // Refused (a before-prompt hook): keep the text to edit or send later.
            self.prompt.set_text(&text);
            self.prompt.move_to_end();
        }
    }

    /// Submit text as a user message (also used by Lua `serpis.agent.prompt`). Returns false
    /// when a `before-prompt` hook refused it (`{block = "reason"}`).
    pub fn submit(&mut self, ed: &mut Editor, text: String, follow_up: bool) -> bool {
        let model_spec = self.model.as_ref().map(|m| format!("{}/{}", m.provider, m.id));
        let payload = serde_json::json!({
            "buffer": ed.this_buffer().map(crate::editor::buffer_id_to_u64),
            "text": text,
            "model": model_spec,
            "running": self.running(),
        });
        if let Some(reason) = ed.run_hook("before-prompt", &payload).iter().find_map(|r| r.get("block").map(|b| b.as_str().unwrap_or("blocked").to_string())) {
            ed.warn(reason);
            return false;
        }
        if self.title.is_empty() {
            self.title = title_from(&text);
        }
        let msg = Message::user_text(text.clone());
        if let Some(agent) = &self.agent {
            if follow_up {
                agent.follow_up(msg);
            } else {
                agent.steer(msg);
            }
            self.entries.push(Entry::User { text, queued: true });
            self.dirty = true;
            return true;
        }
        let Some(model) = self.model.clone() else {
            ed.error("No model selected (chat-select-model)");
            return false;
        };
        if self.session.is_none() && ed.var("session-save").and_then(Value::as_bool).unwrap_or(true) {
            self.session = Some(crate::session::SessionWriter::new(&crate::session_ui::session_root(ed), &self.cwd));
        }
        let (provider, id, thinking) = (model.provider.clone(), model.id.clone(), self.thinking.as_str());
        self.record(ed, |w| w.note_settings(&provider, &id, thinking).map(|_| ()));
        let mut tools = serpis_agent::builtin_tools();
        for (_, t) in &ed.agent_tools {
            tools.retain(|x| x.name() != t.name());
            tools.push(t.clone());
        }
        let disabled: Vec<String> = ed
            .var("agent-disabled-tools")
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
            .unwrap_or_default();
        tools.retain(|t| !disabled.iter().any(|d| d == t.name()));
        let agent_dir = ed.registry.paths().agent_dir.clone();
        let mut opts = serpis_agent::system_prompt::PromptOptions {
            context_files: serpis_agent::system_prompt::load_context_files(&self.cwd, &agent_dir),
            append: ed.var_str("agent-append-system-prompt"),
            ..Default::default()
        };
        if let Some(Value::Array(g)) = ed.var("agent-guidelines") {
            opts.guidelines = g.iter().filter_map(Value::as_str).map(str::to_string).collect();
        }
        let system_prompt = serpis_agent::system_prompt::build_system_prompt(&tools, &self.cwd, &opts);
        let native = ed.var("agent-native-web-search").and_then(Value::as_bool).unwrap_or(true);
        let cfg = AgentConfig {
            api_key: ed.registry.api_key(&model.provider),
            headers: ed.registry.provider_headers(&model.provider),
            model,
            reasoning: self.thinking,
            tools,
            system_prompt,
            session_id: self.session_id.clone(),
            cwd: self.cwd.clone(),
            native_web_search: native,
            max_tokens: None,
            hooks: Some(Arc::new(EditorToolHooks { main: ed.main_sender(), buffer: ed.this_buffer().map(crate::editor::buffer_id_to_u64) })),
            sequential_tools: false,
        };
        let tx = self.tx.clone();
        let repaint = ed.main_sender();
        let emit: serpis_agent::EmitFn = Arc::new(move |ev| {
            let _ = tx.send(ev);
            repaint.repaint();
        });
        let _guard = ed.rt.enter();
        self.last_error = None;
        self.agent = Some(serpis_agent::run(self.history.clone(), vec![msg], cfg, emit));
        self.started = Some(Instant::now());
        self.view.follow_tail = true;
        if let Some(id) = ed.this_buffer() {
            self.publish(ed, id);
        }
        true
    }

    /// Context the conversation fills: the last response's tokens (what the next request sends,
    /// roughly), and the model's window. None before any response.
    pub fn context(&self) -> (Option<u64>, u64) {
        let used = self.history.iter().rev().find_map(|m| match m {
            Message::Assistant(a) if !matches!(a.stop_reason, StopReason::Aborted | StopReason::Error) => {
                Some(a.usage.input + a.usage.cache_read + a.usage.cache_write + a.usage.output)
            }
            _ => None,
        });
        (used, self.model.as_ref().map(|m| m.context_window).unwrap_or(0))
    }

    /// `↑12k ↓3.4k R1.2M W40k $1.234 45.2%/200k`, as pi's footer: the session's input, output,
    /// cache reads and writes, cost, then context used of the window.
    pub fn usage_label(&self) -> String {
        let u = &self.totals;
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

    /// The transcript as shown (as of the last frame that drew it).
    pub fn transcript_text(&self) -> String {
        self.lines.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join("\n")
    }

    /// This chat as seen from outside (`Editor::chats`, `serpis.chat`).
    pub fn info(&self) -> crate::chat_api::ChatInfo {
        crate::chat_api::ChatInfo {
            model: self.model.as_ref().map(|m| format!("{}/{}", m.provider, m.id)),
            thinking: self.thinking.as_str().to_string(),
            running: self.running(),
            usage: self.totals.clone(),
            title: self.title.clone(),
            cwd: self.cwd.to_string_lossy().into_owned(),
            session: self.session.as_ref().filter(|w| w.exists()).map(|w| w.path.to_string_lossy().into_owned()),
            session_id: self.session_id.clone(),
            entries: self.custom_entries.clone(),
            agent: self.agent.clone(),
            context: self.context(),
        }
    }

    pub fn publish(&self, ed: &mut Editor, id: crate::BufferId) {
        ed.chats.insert(id, self.info());
    }

    /// Record a custom entry in the session (created if needed) and keep it for resume.
    pub fn append_entry(&mut self, ed: &mut Editor, kind: &str, data: Value) {
        if self.session.is_none() && ed.var("session-save").and_then(Value::as_bool).unwrap_or(true) {
            self.session = Some(crate::session::SessionWriter::new(&crate::session_ui::session_root(ed), &self.cwd));
        }
        let fields = serde_json::json!({"customType": kind, "data": data});
        self.record(ed, |w| w.append("custom", fields).map(|_| ()));
        self.custom_entries.push((kind.to_string(), data));
    }

    fn tool_entry(&mut self, id: &str) -> Option<&mut Entry> {
        self.entries.iter_mut().rev().find(|e| matches!(e, Entry::Tool { id: i, .. } if i == id))
    }

    fn last_assistant(&mut self) -> Option<&mut Entry> {
        match self.entries.last_mut() {
            Some(e @ Entry::Assistant { .. }) => Some(e),
            _ => None,
        }
    }

    fn ensure_text_segment(&mut self) {
        if !matches!(self.entries.last(), Some(Entry::Assistant { .. })) {
            let model = self.model.as_ref().map(|m| m.id.clone()).unwrap_or_default();
            self.entries.push(Entry::Assistant {
                blocks: Vec::new(),
                model,
                streaming: true,
                continuation: true,
                usage: None,
                error: None,
                thinking: None,
            });
        }
    }

    fn apply(&mut self, ev: AgentEvent, ed: &mut Editor) {
        self.dirty = true;
        match ev {
            AgentEvent::MessageStart(Message::User { content, .. }) => {
                let text = content
                    .iter()
                    .filter_map(|c| match c {
                        UserContent::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                // A queued (steering) message is already shown; un-queue it.
                if let Some(Entry::User { queued, .. }) = self.entries.iter_mut().find(|e| matches!(e, Entry::User { text: t, queued: true } if *t == text)) {
                    *queued = false;
                    return;
                }
                self.entries.push(Entry::User { text, queued: false });
            }
            AgentEvent::MessageStart(Message::Assistant(m)) => self.entries.push(Entry::Assistant {
                blocks: Vec::new(),
                model: m.model,
                streaming: true,
                continuation: false,
                usage: None,
                error: None,
                thinking: None,
            }),
            AgentEvent::MessageStart(Message::ToolResult(_)) => {}
            AgentEvent::MessageDelta(d) => match d {
                StreamEvent::TextStart { .. } => {
                    self.ensure_text_segment();
                    if let Some(Entry::Assistant { blocks, .. }) = self.last_assistant() {
                        blocks.push(AssistantContent::Text {
                            text: String::new(),
                            text_signature: None,
                        });
                    }
                }
                StreamEvent::TextDelta { delta, .. } => {
                    if let Some(Entry::Assistant { blocks, .. }) = self.last_assistant() {
                        if let Some(AssistantContent::Text { text, .. }) = blocks.last_mut() {
                            text.push_str(&delta);
                        }
                    }
                }
                StreamEvent::ThinkingStart { .. } => {
                    self.ensure_text_segment();
                    if let Some(Entry::Assistant { blocks, .. }) = self.last_assistant() {
                        blocks.push(AssistantContent::Thinking {
                            thinking: String::new(),
                            thinking_signature: None,
                            redacted: false,
                        });
                    }
                }
                StreamEvent::ThinkingDelta { delta, .. } => {
                    if let Some(Entry::Assistant { blocks, .. }) = self.last_assistant() {
                        if let Some(AssistantContent::Thinking { thinking, .. }) = blocks.last_mut() {
                            thinking.push_str(&delta);
                        }
                    }
                }
                StreamEvent::ToolCallStart { id, name, .. } => self.entries.push(Entry::Tool {
                    id,
                    name,
                    args: Value::Object(Default::default()),
                    partial: None,
                    result: None,
                    expanded: None,
                    started: None,
                    elapsed: None,
                }),
                StreamEvent::ToolCallDelta { index: _, arguments, .. } => {
                    if let Some(Entry::Tool { args, .. }) = self.entries.iter_mut().rev().find(|e| matches!(e, Entry::Tool { result: None, started: None, .. })) {
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
                    let failed = matches!(a.stop_reason, StopReason::Error | StopReason::Aborted);
                    for e in self.entries.iter_mut().rev() {
                        if let Entry::Assistant { streaming, usage, error, continuation, .. } = e {
                            *streaming = false;
                            if !*continuation {
                                *usage = Some(a.usage.clone());
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
                    let id = ed.this_buffer();
                    if let Some(id) = id {
                        self.publish(ed, id);
                    }
                    let payload = serde_json::json!({
                        "buffer": id.map(crate::editor::buffer_id_to_u64),
                        "text": a.text(),
                        "stopReason": format!("{:?}", a.stop_reason),
                        "model": format!("{}/{}", a.provider, a.model),
                        "usage": crate::chat_api::usage_json(&a.usage),
                        "total": crate::chat_api::usage_json(&self.totals),
                    });
                    ed.run_hook("assistant-message", &payload);
                }
                self.record(ed, |w| w.message(&m).map(|_| ()));
                self.history.push(m);
            }
            AgentEvent::ToolStart { id, name, args } => {
                if self.tool_entry(&id).is_none() {
                    self.entries.push(Entry::Tool {
                        id: id.clone(),
                        name,
                        args: args.clone(),
                        partial: None,
                        result: None,
                        expanded: None,
                        started: None,
                        elapsed: None,
                    });
                }
                if let Some(Entry::Tool { started, args: a, .. }) = self.tool_entry(&id) {
                    *started = Some(Instant::now());
                    *a = args;
                }
            }
            AgentEvent::ToolUpdate { id, partial } => {
                if let Some(Entry::Tool { partial: p, .. }) = self.tool_entry(&id) {
                    *p = Some(partial.text_content());
                }
            }
            AgentEvent::ToolEnd { id, result, .. } => {
                let payload = serde_json::json!({"buffer": ed.this_buffer().map(crate::editor::buffer_id_to_u64), "name": result.tool_name, "isError": result.is_error});
                if let Some(Entry::Tool { result: r, started, elapsed, .. }) = self.tool_entry(&id) {
                    *elapsed = started.map(|s| s.elapsed());
                    *r = Some(result);
                }
                ed.run_hook("tool-finished", &payload);
            }
            AgentEvent::AgentEnd { error } => {
                self.agent = None;
                if let Some(e) = error {
                    self.last_error = Some(e.clone());
                    self.entries.push(Entry::Info { text: e, face: Face::Error });
                }
                for e in &mut self.entries {
                    if let Entry::User { queued, .. } = e {
                        *queued = false;
                    }
                }
                let id = ed.this_buffer();
                if let Some(id) = id {
                    self.publish(ed, id);
                }
                ed.run_hook("agent-finished", &serde_json::json!({"buffer": id.map(crate::editor::buffer_id_to_u64), "total": crate::chat_api::usage_json(&self.totals)}));
            }
            AgentEvent::AgentStart | AgentEvent::TurnStart | AgentEvent::TurnEnd => {}
        }
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
        for (ei, e) in self.entries.iter().enumerate() {
            let is_tool = matches!(e, Entry::Tool { .. });
            match e {
                Entry::User { text, queued } => {
                    // A bubble on the right, as wide as its text.
                    let blk = Block::new(t.user_bg).shrink(0.82);
                    let mut lines: Vec<StyledLine> = Vec::new();
                    if *queued {
                        lines.push(StyledLine::new("Queued · steers the running agent", Face::Meta));
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
                Entry::Assistant { blocks, model, streaming, continuation, usage, error, thinking: fold } => {
                    // Turns that only call tools get no label of their own.
                    let has_content = blocks.iter().any(|b| !matches!(b, AssistantContent::ToolCall(_)));
                    if !continuation && (has_content || *streaming || error.is_some()) {
                        let mut label = format!("◆  {}", short_model(model));
                        if *streaming && blocks.is_empty() {
                            label.push_str("  ·  thinking…");
                        }
                        let diamond = "◆".len();
                        push(&mut out, &mut owner, StyledLine::new(label, Face::Label).spans(vec![(0..diamond, Face::Accent)]).gap(26.0 * k).pad(0.0, 4.0 * k), Some(ei));
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
                                let head = if *streaming { "Thinking…".to_string() } else if body.len() > shown.len() && body.len() > 1 { format!("Thought · {} lines", body.len()) } else { "Thought".to_string() };
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
                            push(&mut out, &mut owner, StyledLine::new(format_usage(u), Face::Meta).gap(8.0 * k), Some(ei));
                        }
                    }
                }
                Entry::Tool { name, args, partial, result, expanded, started, elapsed, .. } => {
                    let gap = if prev_tool { 6.0 } else { 14.0 } * k;
                    for l in tool_lines(name, args, partial.as_deref(), result.as_ref(), *expanded, *started, *elapsed, t, gap) {
                        push(&mut out, &mut owner, l, Some(ei));
                    }
                }
                Entry::Info { text, face } => {
                    let face = if *face == Face::Dim { Face::Meta } else { *face };
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
        let Some(Entry::Assistant { blocks, streaming, thinking, .. }) = self.entries.get(ei) else { return false };
        thinking.unwrap_or_else(|| {
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
            if let Some(Entry::Assistant { thinking, .. }) = self.entries.get_mut(ei) {
                *thinking = Some(!full);
                self.dirty = true;
                return true;
            }
        }
        if let Some(Entry::Tool { expanded, name, result, .. }) = self.entries.get_mut(ei) {
            let default = default_expanded(name, result.as_ref());
            *expanded = Some(!expanded.unwrap_or(default));
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

    /// Write to the session file; the first failure is reported, later ones are silent.
    fn record(&mut self, ed: &mut Editor, f: impl FnOnce(&mut crate::session::SessionWriter) -> std::io::Result<()>) {
        let Some(w) = self.session.as_mut() else { return };
        let existed = w.exists();
        if let Err(e) = f(w) {
            if !self.session_failed {
                self.session_failed = true;
                ed.warn(format!("session {}: {e}", w.path.display()));
            }
        }
        if !existed && w.exists() {
            // A project you have chatted in stays in the sidebar with its sessions.
            if ed.var("project-remember").and_then(Value::as_bool).unwrap_or(true) {
                let cwd = self.cwd.clone();
                if let Some(root) = ed.project_root_of(&cwd) {
                    crate::sidebar::add_project(ed, &root);
                }
            }
            crate::session_ui::rescan(ed);
        }
    }

    /// Name the session (`session_info`), e.g. after `buffer-set-title`.
    pub fn set_session_name(&mut self, ed: &mut Editor, name: &str) {
        if self.session.as_ref().is_some_and(|w| w.exists()) {
            self.record(ed, |w| w.set_name(name).map(|_| ()));
        }
    }

    /// Show a saved conversation and continue it: later messages append to the same file.
    pub fn restore(&mut self, ed: &mut Editor, l: &crate::session::Loaded) {
        self.abort();
        self.entries.clear();
        self.totals = Usage::default();
        self.history = l.messages.clone();
        if l.cwd.is_dir() {
            self.cwd = l.cwd.clone();
        }
        if let Some((p, id)) = &l.model {
            match ed.registry.find(p, id) {
                Some(m) => self.set_model(m),
                None => self.entries.push(Entry::Info { text: format!("Model {p}/{id} of this session isn't available; using the current one."), face: Face::Warning }),
            }
        }
        if let Some(t) = l.thinking.as_deref().and_then(ThinkingLevel::parse) {
            self.thinking = self.model.as_ref().map(|m| clamp_thinking_level(m, t)).unwrap_or(t);
        }
        for m in &l.messages {
            match m {
                Message::User { content, .. } => {
                    let text = content
                        .iter()
                        .map(|c| match c {
                            UserContent::Text { text } => text.clone(),
                            UserContent::Image { mime_type, .. } => format!("[image {mime_type}]"),
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    self.prompt_history.push(text.clone());
                    self.entries.push(Entry::User { text, queued: false });
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
                        thinking: None,
                    });
                    for tc in a.tool_calls() {
                        self.entries.push(Entry::Tool { id: tc.id.clone(), name: tc.name.clone(), args: tc.arguments.clone(), partial: None, result: None, expanded: None, started: None, elapsed: None });
                    }
                }
                Message::ToolResult(r) => {
                    let id = r.tool_call_id.clone();
                    if let Some(Entry::Tool { result, .. }) = self.tool_entry(&id) {
                        *result = Some(r.clone());
                    }
                }
            }
        }
        self.title = l.name.clone().unwrap_or_else(|| {
            let first = self.prompt_history.first().cloned().unwrap_or_default();
            title_from(&first)
        });
        let ago = std::fs::metadata(&l.path).ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok()).map(|d| crate::sidebar::fmt_age(d) + " ago").unwrap_or_default();
        self.entries.push(Entry::Info { text: format!("↺ Resumed session {} · {} messages · {ago}", &l.id[..l.id.len().min(8)], l.messages.len()), face: Face::Dim });
        self.custom_entries = l.entries.clone();
        self.session = Some(crate::session::SessionWriter::resume(l));
        self.session_failed = false;
        self.last_error = None;
        self.view.follow_tail = true;
        self.dirty = true;
    }

    /// What the running agent is doing right now.
    fn current_step(&self) -> String {
        for e in self.entries.iter().rev() {
            match e {
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
}

/// A chat title from its first prompt: the first line, cut at a word near 48 characters.
fn title_from(text: &str) -> String {
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

fn add_usage(total: &mut Usage, u: &Usage) {
    total.input += u.input;
    total.output += u.output;
    total.cache_read += u.cache_read;
    total.cache_write += u.cache_write;
    total.total_tokens += u.total_tokens;
    total.cost.total += u.cost.total;
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

pub fn fmt_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1e6)
    } else if n >= 1000 {
        format!("{:.1}k", n as f64 / 1e3)
    } else {
        n.to_string()
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
        _ => matches!(name, "edit" | "write"),
    }
}

fn arg<'a>(args: &'a Value, k: &str) -> &'a str {
    args.get(k).and_then(Value::as_str).unwrap_or("")
}

fn tool_summary(name: &str, args: &Value) -> String {
    match name {
        "bash" => arg(args, "command").lines().next().unwrap_or("").to_string(),
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
        _ => {
            let s = args.to_string();
            if s == "{}" { String::new() } else { s }
        }
    }
}

/// A tool call as a card: a header row (status, name, what it acts on, time), a hairline,
/// then its output or diff.
#[allow(clippy::too_many_arguments)]
fn tool_lines(
    name: &str,
    args: &Value,
    partial: Option<&str>,
    result: Option<&ToolResultMessage>,
    expanded: Option<bool>,
    started: Option<Instant>,
    elapsed: Option<Duration>,
    t: &crate::theme::Theme,
    gap: f32,
) -> Vec<StyledLine> {
    let k = t.ui_scale();
    let (icon, icon_face) = match result {
        None if started.is_some() => ("●", Face::Warning),
        None => ("○", Face::Meta),
        Some(r) if r.is_error => ("✗", Face::Error),
        Some(_) => ("✓", Face::ToolOk),
    };
    let card = Block::new(t.tool_bg).border(t.border);
    let expanded = expanded.unwrap_or_else(|| default_expanded(name, result));
    let mut header = format!("{icon}  ");
    let mut spans = vec![(0..icon.len(), icon_face)];
    let s0 = header.len();
    header.push_str(name);
    spans.push((s0..header.len(), Face::ToolHeader));
    let summary = tool_summary(name, args);
    if !summary.is_empty() {
        header.push_str("  ");
        let s1 = header.len();
        header.push_str(&summary);
        spans.push((s1..header.len(), Face::ToolArgs));
    }
    let time = match (elapsed, started) {
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
            let text = tool_text(r);
            let lines: Vec<&str> = text.lines().collect();
            let hl = if name == "read" && !r.is_error { crate::highlight::highlight(arg(args, "path"), &lines, face) } else { None }.unwrap_or_default();
            for (i, l) in lines.iter().enumerate() {
                body.push((StyledLine::new(*l, face).spans(hl.get(i).cloned().unwrap_or_default()), None));
            }
        }
    }

    let total = body.len();
    let shown = if expanded {
        total
    } else if name == "bash" || result.is_none() {
        total.min(PREVIEW_LINES)
    } else {
        total.min(if name == "read" { 0 } else { 4 })
    };
    // Bash previews show the tail, everything else the head.
    let mut slice: Vec<(StyledLine, Option<Color32>)> = if name == "bash" && !expanded && total > shown { body[total - shown..].to_vec() } else { body[..shown].to_vec() };
    if total > shown {
        let more = (StyledLine::new(format!("{} more lines · TAB or click to expand", total - shown), Face::Meta), None);
        if name == "bash" {
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
        .map(|c| match c {
            UserContent::Text { text } => text.clone(),
            UserContent::Image { mime_type, .. } => format!("[image {mime_type}]"),
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// `agent-model` var, then pi's settings (`defaultProvider`/`defaultModel`), then any model with credentials.
pub fn default_model(ed: &Editor) -> Option<Model> {
    let supported = |m: &Model| serpis_ai::api::SUPPORTED_APIS.contains(&m.api.as_str()) || m.api == "azure-openai-responses";
    if let Some(spec) = ed.var_str("agent-model") {
        if let Some((p, id)) = spec.split_once('/') {
            if let Some(m) = ed.registry.find(p, id) {
                return Some(m);
            }
        }
    }
    if let (Some(p), Some(id)) = (pi_setting("defaultProvider"), pi_setting("defaultModel")) {
        if let Some(m) = ed.registry.find(&p, &id) {
            return Some(m);
        }
    }
    let configured = ed.registry.configured_providers();
    ed.registry.models().into_iter().find(|m| configured.contains(&m.provider) && supported(m))
}

pub fn pi_setting(key: &str) -> Option<String> {
    let p = dirs::home_dir()?.join(".pi/agent/settings.json");
    let v: Value = serde_json::from_str(&std::fs::read_to_string(p).ok()?).ok()?;
    v.get(key)?.as_str().map(str::to_string)
}

/// Runs `before-tool-call` / `after-tool-call` editor hooks on the UI thread.
struct EditorToolHooks {
    main: MainSender,
    /// The chat whose agent calls the tools (hook payloads' `buffer`).
    buffer: Option<u64>,
}

impl ToolHooks for EditorToolHooks {
    fn before_tool_call(&self, name: &str, args: Value) -> futures::future::BoxFuture<'static, ToolGate> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let name = name.to_string();
        let a = args.clone();
        let buffer = self.buffer;
        self.main.send(move |ed| {
            // Apply the chat's pending events first (the response asking for this tool), so
            // hooks see what it cost and what assistant-message hooks did about it.
            if let Some(b) = buffer {
                ed.with_buffer(crate::editor::buffer_id_from_u64(b), |c, ed| c.poll(ed));
            }
            let results = ed.run_hook("before-tool-call", &serde_json::json!({"buffer": buffer, "name": name, "args": a}));
            let gate = results.into_iter().find_map(|r| {
                if let Some(reason) = r.get("block").and_then(Value::as_str) {
                    Some(ToolGate::Block(reason.to_string()))
                } else {
                    r.get("args").cloned().map(ToolGate::Allow)
                }
            });
            let _ = tx.send(gate);
        });
        Box::pin(async move { rx.await.ok().flatten().unwrap_or(ToolGate::Allow(args)) })
    }

    fn after_tool_call(&self, name: &str, result: ToolResult) -> futures::future::BoxFuture<'static, ToolResult> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let name = name.to_string();
        let text = result.text_content();
        let is_error = result.is_error;
        let buffer = self.buffer;
        self.main.send(move |ed| {
            let results = ed.run_hook("after-tool-call", &serde_json::json!({"buffer": buffer, "name": name, "text": text, "isError": is_error}));
            let _ = tx.send(results.into_iter().find_map(|r| r.get("text").and_then(Value::as_str).map(str::to_string)));
        });
        Box::pin(async move {
            match rx.await.ok().flatten() {
                Some(text) => ToolResult { content: vec![UserContent::Text { text }], ..result },
                None => result,
            }
        })
    }
}

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
        while let Ok(ev) = self.rx.try_recv() {
            self.apply(ev, ed);
        }
        if let Some(id) = ed.this_buffer() {
            self.publish(ed, id);
        }
        if self.running() {
            // Keep elapsed timers ticking.
            self.dirty = true;
            ed.ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, focused: bool, ed: &mut Editor) {
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
            for e in &mut self.entries {
                if let Entry::Assistant { thinking, .. } = e {
                    *thinking = None;
                }
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
        let left = rect.center().x - col_w / 2.0;
        let card = Rect::from_min_max(egui::pos2(left, rect.bottom() - 14.0 * k - text_h - toolbar_h), egui::pos2(left + col_w, rect.bottom() - 14.0 * k));
        let transcript_rect = Rect::from_min_max(rect.min, egui::pos2(rect.right(), card.top() - 6.0 * k));

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

        let text_rect = Rect::from_min_max(card.min, egui::pos2(card.right(), card.bottom() - toolbar_h));
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
        let model = self.model.as_ref().map(|m| short_model(&m.id).to_string()).unwrap_or_else(|| "Choose a model".into());
        let (mr, clicked) = pill(ui, bar.left(), "model", &model, "Model (chat-select-model)", Some(theme.accent));
        if clicked {
            run = Some("chat-select-model");
        }
        let thinking = format!("Thinking · {}", self.thinking.as_str());
        let (tr, clicked) = pill(ui, mr.right() + 2.0 * k, "thinking", &thinking, "Thinking level (chat-select-thinking)", None);
        if clicked {
            run = Some("chat-select-thinking");
        }

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
            let fg = if has_text { if theme.dark { theme.bg } else { Color32::WHITE } } else { theme.fg_faint };
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
        self.composer_hits = vec![("model", mr), ("thinking", tr), ("send", send), ("card", card)];
        if let Some(cmd) = run {
            self.focus = Focus::Prompt;
            ed.main_sender().send(move |ed| ed.call(cmd));
        }
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
        let mut status = Vec::new();
        if let Some(m) = &self.model {
            status.push(short_model(&m.id).to_string());
        }
        Modeline {
            title: self.title.clone(),
            status,
            busy: self.running(),
        }
    }

    fn activity(&self) -> Activity {
        if self.running() {
            return Activity { since: self.started, ..Activity::new(ActivityState::Working, self.current_step()) };
        }
        match &self.last_error {
            Some(e) => Activity::new(ActivityState::Error, e.lines().next().unwrap_or("")),
            // Idle chats say which model they talk to.
            None => Activity::new(ActivityState::Idle, self.model.as_ref().map(|m| short_model(&m.id)).unwrap_or("no model")),
        }
    }

    fn on_kill(&mut self, _ed: &mut Editor) {
        self.abort();
    }

    fn directory(&self) -> Option<std::path::PathBuf> {
        Some(self.cwd.clone())
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
    fn reveal_cursor(&mut self) {
        match self.focus {
            Focus::Transcript => self.view.reveal_cursor(),
            Focus::Prompt => self.prompt.reveal_cursor(),
        }
    }
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
