//! Agent chat buffer.
//!
//! The transcript is generated styled text (Emacs-style faces and blocks): messages, thinking,
//! foldable tool cards and diff views. The prompt is an editable area pinned to the bottom.
//! Both form one composite [`TextSurface`] (transcript lines first, then prompt lines), so
//! motions, vim, search and yank move seamlessly between them.

use std::ops::Range;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use egui::{Rect, Stroke};
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

    /// Send the prompt: start a run, or steer the running one.
    pub fn send(&mut self, ed: &mut Editor, follow_up: bool) {
        let text = self.prompt.text().trim().to_string();
        if text.is_empty() {
            return;
        }
        self.prompt.clear();
        self.prompt_history.push(text.clone());
        self.history_index = None;
        self.submit(ed, text, follow_up);
    }

    /// Submit text as a user message (also used by Lua `serpis.agent.prompt`).
    pub fn submit(&mut self, ed: &mut Editor, text: String, follow_up: bool) {
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
            return;
        }
        let Some(model) = self.model.clone() else {
            ed.error("No model selected (chat-select-model)");
            return;
        };
        let payload = serde_json::json!({"text": text, "model": format!("{}/{}", model.provider, model.id)});
        ed.run_hook("before-prompt", &payload);
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
            hooks: Some(Arc::new(EditorToolHooks { main: ed.main_sender() })),
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
                    let payload = serde_json::json!({"text": a.text(), "stopReason": format!("{:?}", a.stop_reason)});
                    ed.run_hook("assistant-message", &payload);
                }
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
                let payload = serde_json::json!({"name": result.tool_name, "isError": result.is_error});
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
                ed.run_hook("agent-finished", &Value::Null);
            }
            AgentEvent::AgentStart | AgentEvent::TurnStart | AgentEvent::TurnEnd => {}
        }
    }

    // ------------------------------------------------------------------
    // Transcript rendering to styled lines
    // ------------------------------------------------------------------

    fn rebuild(&mut self, ed: &Editor) {
        let t = &ed.theme;
        let mut out: Vec<StyledLine> = Vec::new();
        let mut owner: Vec<Option<usize>> = Vec::new();
        let push = |out: &mut Vec<StyledLine>, owner: &mut Vec<Option<usize>>, l: StyledLine, e: Option<usize>| {
            out.push(l);
            owner.push(e);
        };
        for (ei, e) in self.entries.iter().enumerate() {
            match e {
                Entry::User { text, queued } => {
                    let blk = Block { bg: t.user_bg, bar: Some(t.accent), top: true, bottom: false };
                    let label = if *queued { "You · queued (steering)" } else { "You" };
                    push(&mut out, &mut owner, StyledLine::new(label, Face::UserLabel).block(blk).indent(10.0).gap(14.0), Some(ei));
                    let lines: Vec<&str> = text.lines().collect();
                    for (i, l) in lines.iter().enumerate() {
                        let b = Block { top: false, bottom: i + 1 == lines.len(), ..blk };
                        push(&mut out, &mut owner, StyledLine::new(*l, Face::UserText).block(b).indent(10.0), Some(ei));
                    }
                }
                Entry::Assistant { blocks, model, streaming, continuation, usage, error } => {
                    // Turns that only call tools get no label of their own.
                    let has_content = blocks.iter().any(|b| !matches!(b, AssistantContent::ToolCall(_)));
                    if !continuation && (has_content || *streaming || error.is_some()) {
                        let mut label = format!("◆ {}", short_model(model));
                        if *streaming && blocks.is_empty() {
                            label.push_str("  ·  thinking…");
                        }
                        push(&mut out, &mut owner, StyledLine::new(label, Face::Accent).gap(14.0), Some(ei));
                    }
                    for b in blocks {
                        match b {
                            AssistantContent::Thinking { thinking, redacted, .. } => {
                                let body: Vec<&str> = thinking.trim().lines().collect();
                                if body.is_empty() && !*redacted {
                                    continue;
                                }
                                let shown: &[&str] = if *streaming || body.len() <= 4 { &body } else { &body[body.len() - 3..] };
                                let blk = Block { bg: t.bg_alt, bar: Some(t.fg_faint), top: true, bottom: false };
                                let head = if *streaming { "thinking…".to_string() } else if body.len() > shown.len() { format!("thought · {} lines", body.len()) } else { "thought".to_string() };
                                push(&mut out, &mut owner, StyledLine::new(head, Face::Thinking).block(blk).indent(10.0).gap(4.0), Some(ei));
                                for (i, l) in shown.iter().enumerate() {
                                    push(&mut out, &mut owner, StyledLine::new(*l, Face::Thinking).block(Block { top: false, bottom: i + 1 == shown.len(), ..blk }).indent(10.0), Some(ei));
                                }
                            }
                            AssistantContent::Text { text, .. } => {
                                for l in markdown_lines(text, t) {
                                    push(&mut out, &mut owner, l, Some(ei));
                                }
                            }
                            AssistantContent::ToolCall(_) => {}
                        }
                    }
                    if let Some(err) = error {
                        for l in err.lines() {
                            push(&mut out, &mut owner, StyledLine::new(format!("✗ {l}"), Face::Error), Some(ei));
                        }
                    }
                    if let Some(u) = usage {
                        if u.total_tokens > 0 {
                            push(&mut out, &mut owner, StyledLine::new(format_usage(u), Face::Dim).gap(2.0), Some(ei));
                        }
                    }
                }
                Entry::Tool { name, args, partial, result, expanded, started, elapsed, .. } => {
                    for l in tool_lines(name, args, partial.as_deref(), result.as_ref(), *expanded, *started, *elapsed, t) {
                        push(&mut out, &mut owner, l, Some(ei));
                    }
                }
                Entry::Info { text, face } => {
                    for l in text.lines() {
                        push(&mut out, &mut owner, StyledLine::new(l, *face).gap(6.0), Some(ei));
                    }
                }
            }
        }
        if out.is_empty() {
            push(&mut out, &mut owner, StyledLine::new("", Face::Default), None);
        }
        self.lines = out;
        self.line_entry = owner;
        self.dirty = false;
        let n = self.lines.len();
        if self.view.cursor.line >= n {
            self.view.cursor = Pos::new(n - 1, 0);
        }
    }

    /// Toggle the fold of the entry at transcript line `line`.
    pub fn toggle_fold_at(&mut self, line: usize) -> bool {
        let Some(Some(ei)) = self.line_entry.get(line).copied() else { return false };
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

fn markdown_lines(text: &str, t: &crate::theme::Theme) -> Vec<StyledLine> {
    let mut out = Vec::new();
    let mut in_code = false;
    let code_blk = Block { bg: t.tool_bg, bar: None, top: false, bottom: false };
    let src: Vec<&str> = text.split('\n').collect();
    let mut first = true;
    for (i, raw) in src.iter().enumerate() {
        let trimmed = raw.trim_start();
        if trimmed.starts_with("```") {
            if !in_code {
                in_code = true;
                let lang = trimmed.trim_start_matches('`').trim();
                out.push(StyledLine::new(if lang.is_empty() { " ".to_string() } else { lang.to_string() }, Face::Dim).block(Block { top: true, ..code_blk }).indent(8.0).gap(4.0));
            } else {
                in_code = false;
                if let Some(last) = out.last_mut() {
                    if let Some(b) = &mut last.block {
                        b.bottom = true;
                    }
                }
            }
            continue;
        }
        if in_code {
            let mut l = StyledLine::new(*raw, Face::Code).block(code_blk).indent(8.0);
            if i + 1 == src.len() {
                l.block.as_mut().unwrap().bottom = true;
            }
            out.push(l);
            continue;
        }
        let gap = if first { 4.0 } else { 0.0 };
        first = false;
        if let Some(h) = trimmed.strip_prefix('#') {
            let h = h.trim_start_matches('#').trim();
            let (s, spans) = inline_spans(h);
            out.push(StyledLine { text: s, face: Face::Heading, spans, gap_above: 6.0, ..Default::default() });
            continue;
        }
        let (body, indent) = if let Some(b) = trimmed.strip_prefix("- ").or_else(|| trimmed.strip_prefix("* ")) {
            (format!("•  {b}"), 6.0 + (raw.len() - trimmed.len()) as f32 * 4.0)
        } else {
            (raw.to_string(), 0.0)
        };
        let (s, spans) = inline_spans(&body);
        out.push(StyledLine { text: s, face: Face::AssistantText, spans, indent, gap_above: gap, ..Default::default() });
    }
    out
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
) -> Vec<StyledLine> {
    let (icon, bar) = match result {
        None if started.is_some() => ("●", t.yellow),
        None => ("○", t.fg_faint),
        Some(r) if r.is_error => ("✗", t.red),
        Some(_) => ("✓", t.green),
    };
    let blk = Block { bg: t.tool_bg, bar: Some(bar), top: true, bottom: false };
    let expanded = expanded.unwrap_or_else(|| default_expanded(name, result));
    let mut header = format!("{icon} {name}  {}", tool_summary(name, args));
    if let Some(e) = elapsed {
        if e.as_millis() >= 100 {
            header.push_str(&format!("   {:.1}s", e.as_secs_f32()));
        }
    } else if let Some(s) = started {
        header.push_str(&format!("   {:.0}s", s.elapsed().as_secs_f32()));
    }
    let name_end = 2 + icon.len() + name.len();
    let mut lines = vec![StyledLine {
        text: header,
        face: Face::ToolArgs,
        spans: vec![(icon.len() + 1..name_end.saturating_sub(1).max(icon.len() + 1), Face::ToolHeader)],
        block: Some(blk),
        indent: 10.0,
        gap_above: 6.0,
    }];
    let body_blk = Block { top: false, ..blk };
    let mut body: Vec<StyledLine> = Vec::new();

    // Edit and write show diffs.
    let diff = result.and_then(|r| r.details.get("diff")).and_then(Value::as_str);
    if let (Some(d), Some(r)) = (diff, result) {
        if !r.is_error {
            for l in d.lines() {
                let face = match l.chars().next() {
                    Some('+') => Face::DiffAdd,
                    Some('-') => Face::DiffDel,
                    _ => Face::DiffContext,
                };
                body.push(StyledLine::new(l, face).block(body_blk).indent(10.0));
            }
        }
    }
    if body.is_empty() {
        if name == "write" && result.is_none_or(|r| !r.is_error) {
            let content = arg(args, "content");
            let n = content.lines().count();
            if n > 0 {
                body.push(StyledLine::new(format!("{n} lines"), Face::Dim).block(body_blk).indent(10.0));
                for l in content.lines() {
                    body.push(StyledLine::new(format!("+ {l}"), Face::DiffAdd).block(body_blk).indent(10.0));
                }
            }
        } else if name == "bash" {
            let cmd = arg(args, "command");
            if cmd.lines().count() > 1 {
                for l in cmd.lines().skip(1) {
                    body.push(StyledLine::new(l, Face::ToolArgs).block(body_blk).indent(10.0));
                }
            }
            let out = result.map(|r| tool_text(r)).or(partial.map(str::to_string)).unwrap_or_default();
            let face = if result.is_some_and(|r| r.is_error) { Face::ToolError } else { Face::ToolOutput };
            for l in out.lines() {
                body.push(StyledLine::new(l, face).block(body_blk).indent(10.0));
            }
        } else if let Some(r) = result {
            let face = if r.is_error { Face::ToolError } else { Face::ToolOutput };
            for l in tool_text(r).lines() {
                body.push(StyledLine::new(l, face).block(body_blk).indent(10.0));
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
    let slice: Vec<StyledLine> = if name == "bash" && !expanded && total > shown {
        body[total - shown..].to_vec()
    } else {
        body[..shown].to_vec()
    };
    if total > shown {
        let more = StyledLine::new(
            format!("… {} more lines  (TAB / click to expand)", total - shown),
            Face::Dim,
        )
        .block(body_blk)
        .indent(10.0);
        if name == "bash" {
            lines.push(more);
            lines.extend(slice);
        } else {
            lines.extend(slice);
            lines.push(more);
        }
    } else {
        lines.extend(slice);
    }
    if let Some(last) = lines.last_mut() {
        if let Some(b) = &mut last.block {
            b.bottom = true;
        }
    }
    lines
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
}

impl ToolHooks for EditorToolHooks {
    fn before_tool_call(&self, name: &str, args: Value) -> futures::future::BoxFuture<'static, ToolGate> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let name = name.to_string();
        let a = args.clone();
        self.main.send(move |ed| {
            let results = ed.run_hook("before-tool-call", &serde_json::json!({"name": name, "args": a}));
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
        self.main.send(move |ed| {
            let results = ed.run_hook("after-tool-call", &serde_json::json!({"name": name, "text": text, "isError": is_error}));
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
        if self.running() {
            // Keep elapsed timers ticking.
            self.dirty = true;
            ed.ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, focused: bool, ed: &mut Editor) {
        if self.dirty {
            self.rebuild(ed);
        }
        let theme = ed.theme.clone();
        let id = ed.current_buffer();
        let shape = if focused { ed.cursor_shape(id) } else { None }.unwrap_or(CursorShape::Bar);

        // Prompt geometry: grows with content up to 40% of the window.
        let row_h = ui.fonts_mut(|f| f.row_height(&theme.prop()));
        let footer_h = 22.0;
        let prompt_lines = self.prompt.doc.lines.len().max(1) as f32;
        let prompt_h = (prompt_lines * row_h + 2.0 * self.prompt.view.padding.y + footer_h).min(rect.height() * 0.4).max(row_h + 2.0 * self.prompt.view.padding.y + footer_h);
        let margin = 10.0;
        let prompt_rect = Rect::from_min_max(egui::pos2(rect.left() + margin, rect.bottom() - prompt_h - margin), egui::pos2(rect.right() - margin, rect.bottom() - margin));
        let transcript_rect = Rect::from_min_max(rect.min, egui::pos2(rect.right(), prompt_rect.top() - 4.0));

        // Transcript.
        let t_focus = focused && self.focus == Focus::Transcript;
        let resp = self.view.show(ui, transcript_rect, &Lines(&self.lines), &theme, &ViewOptions {
            focused: t_focus,
            cursor: shape,
            show_cursor: t_focus,
        });
        if resp.clicked {
            self.focus = Focus::Transcript;
            if let Some(l) = resp.clicked_line {
                self.toggle_fold_at(l);
            }
        }

        // Prompt box.
        let painter = ui.painter().clone();
        let p_focus = focused && self.focus == Focus::Prompt;
        painter.rect_filled(prompt_rect, theme.radius + 4, theme.bg_elevated);
        painter.rect_stroke(
            prompt_rect,
            theme.radius + 4,
            Stroke::new(1.0, if p_focus { theme.border_focused } else { theme.border }),
            egui::StrokeKind::Inside,
        );
        let text_rect = Rect::from_min_max(prompt_rect.min, egui::pos2(prompt_rect.right(), prompt_rect.bottom() - footer_h));
        let src = crate::text::PlainLines { lines: &self.prompt.doc.lines, face: Face::Default };
        let presp = self.prompt.view.show(ui, text_rect, &src, &theme, &ViewOptions {
            focused: p_focus,
            cursor: shape,
            show_cursor: p_focus,
        });
        if presp.clicked {
            self.focus = Focus::Prompt;
        }
        if self.prompt.doc.is_empty() {
            let hint = if self.running() { "Steer the agent… (RET queues, M-RET follow-up, C-c C-c abort)" } else { "Ask anything…  RET send · S-RET newline · C-c C-m model" };
            painter.text(text_rect.min + self.prompt.view.padding + egui::vec2(4.0, 0.0), egui::Align2::LEFT_TOP, hint, theme.prop(), theme.fg_faint);
        }

        // Footer: model, thinking, usage, activity.
        let footer = Rect::from_min_max(egui::pos2(prompt_rect.left() + 14.0, prompt_rect.bottom() - footer_h), egui::pos2(prompt_rect.right() - 14.0, prompt_rect.bottom() - 4.0));
        let small = egui::FontId::proportional(theme.font_size - 2.0);
        let model = self.model.as_ref().map(|m| format!("{} · {}", m.provider, short_model(&m.id))).unwrap_or_else(|| "no model".into());
        let left = format!("{model}  ·  thinking {}", self.thinking.as_str());
        painter.text(footer.left_center(), egui::Align2::LEFT_CENTER, left, small.clone(), theme.fg_dim);
        let mut right = String::new();
        if self.running() {
            let secs = self.started.map(|s| s.elapsed().as_secs()).unwrap_or(0);
            let spin = ["◐", "◓", "◑", "◒"][((ui.input(|i| i.time) * 6.0) as usize) % 4];
            right.push_str(&format!("{spin} working {secs}s  ·  "));
        }
        right.push_str(&format!("{} tok", fmt_tokens(self.totals.total_tokens)));
        if self.totals.cost.total > 0.0 {
            right.push_str(&format!("  ·  ${:.3}", self.totals.cost.total));
        }
        if let Some(m) = &self.model {
            if m.context_window > 0 {
                let last = self.history.iter().rev().find_map(|m| match m {
                    Message::Assistant(a) => Some(a.usage.input + a.usage.cache_read + a.usage.cache_write + a.usage.output),
                    _ => None,
                });
                if let Some(used) = last {
                    right.push_str(&format!("  ·  {:.0}% ctx", used as f64 * 100.0 / m.context_window as f64));
                }
            }
        }
        painter.text(footer.right_center(), egui::Align2::RIGHT_CENTER, right, small, if self.running() { theme.yellow } else { theme.fg_faint });
        if self.running() {
            ui.ctx().request_repaint_after(Duration::from_millis(120));
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
        status.push(self.cwd.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
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

    impl_any!();
}

/// Composite surface: transcript lines `[0, T)`, then prompt lines `[T, T+P)`.
impl TextSurface for ChatBuffer {
    fn line_count(&self) -> usize {
        self.t_len() + self.prompt.doc.lines.len()
    }
    fn line(&self, i: usize) -> String {
        let t = self.t_len();
        if i < t { self.lines[i].text.clone() } else { self.prompt.line(i - t) }
    }
    fn cursor(&self) -> Pos {
        match self.focus {
            Focus::Transcript => self.view.cursor,
            Focus::Prompt => {
                let c = self.prompt.cursor();
                Pos::new(c.line + self.t_len(), c.col)
            }
        }
    }
    fn set_cursor(&mut self, p: Pos) {
        let t = self.t_len();
        if p.line >= t {
            self.focus = Focus::Prompt;
            self.prompt.set_cursor(Pos::new(p.line - t, p.col));
        } else {
            self.focus = Focus::Transcript;
            let col = p.col.min(char_len(&self.lines[p.line].text));
            self.view.cursor = Pos::new(p.line, col);
            self.view.reveal_cursor();
            // Moving up into history stops auto-follow; reaching the end resumes it.
            self.view.follow_tail = p.line + 1 >= t;
        }
    }
    fn anchor(&self) -> Option<Pos> {
        match self.focus {
            Focus::Transcript => self.view.anchor,
            Focus::Prompt => self.prompt.anchor().map(|a| Pos::new(a.line + self.t_len(), a.col)),
        }
    }
    fn set_anchor(&mut self, a: Option<Pos>) {
        let t = self.t_len();
        match (self.focus, a) {
            (Focus::Prompt, Some(a)) if a.line >= t => self.prompt.set_anchor(Some(Pos::new(a.line - t, a.col))),
            (Focus::Prompt, None) => self.prompt.set_anchor(None),
            (Focus::Transcript, a) => self.view.anchor = a.filter(|a| a.line < t),
            _ => {}
        }
    }
    fn editable_at(&self, p: Pos) -> bool {
        p.line >= self.t_len()
    }
    fn insert(&mut self, at: Pos, text: &str) -> Pos {
        let t = self.t_len();
        if at.line < t {
            return at;
        }
        let e = self.prompt.insert(Pos::new(at.line - t, at.col), text);
        Pos::new(e.line + t, e.col)
    }
    fn delete(&mut self, from: Pos, to: Pos) -> String {
        let t = self.t_len();
        if from.line < t || to.line < t {
            return String::new();
        }
        self.prompt.delete(Pos::new(from.line - t, from.col), Pos::new(to.line - t, to.col))
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
        let r = self.view.visible_lines();
        if self.focus == Focus::Prompt {
            let t = self.t_len();
            return t..t + self.prompt.doc.lines.len();
        }
        r
    }
    fn screen_rect(&self, p: Pos) -> Option<Rect> {
        let t = self.t_len();
        if p.line >= t { self.prompt.screen_rect(Pos::new(p.line - t, p.col)) } else { self.view.screen_rect(p) }
    }
    fn reveal_cursor(&mut self) {
        match self.focus {
            Focus::Transcript => self.view.reveal_cursor(),
            Focus::Prompt => self.prompt.reveal_cursor(),
        }
    }
}
