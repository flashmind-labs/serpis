//! Chats as an API: what each chat is doing (model, thinking, usage, session entries) and ways
//! to act on it, for Lua (`serpis.chat`), plugins and the chat's own hooks.
//!
//! Chats run in nodes (this machine's included), so reads use the snapshot each chat buffer keeps
//! in `Editor::chats` from what its node last said, and actions are requests to its node.

use serde_json::{Value, json};
use serpis_ai::{Model, ThinkingLevel, Usage};

use crate::buffer::BufferId;
use crate::buffers::chat::ChatBuffer;
use crate::editor::{Editor, buffer_id_to_u64};

/// A chat as seen from outside.
#[derive(Clone, Default)]
pub struct ChatInfo {
    /// `provider/model-id`.
    pub model: Option<String>,
    pub thinking: String,
    pub running: bool,
    /// Summarizing its history (chat-compact).
    pub compacting: bool,
    /// The whole session's usage (every message on the conversation).
    pub usage: Usage,
    pub title: String,
    pub cwd: String,
    pub session: Option<String>,
    /// The id the agent passes to tools (`SERPIS_SESSION_ID`).
    pub session_id: String,
    /// Custom session entries (`serpis.chat.append`), oldest first: (type, data).
    pub entries: Vec<(String, Value)>,
    /// Tokens the conversation fills (None before any response) and the model's window.
    pub context: (Option<u64>, u64),
    /// The prompt cache: {hit_rate, state = "warm"|"cold"|nil, left (seconds), misses}.
    pub cache: Value,
    /// The machine the chat runs on, when it isn't this one (`remote-hosts`).
    pub machine: Option<String>,
    /// Where it runs and its id there: what requests about it are sent with.
    pub node: (String, String),
    /// A turn cut off by its node stopping continues by itself (`chat-keep-going`).
    pub keep_going: bool,
    /// Claude Code's permission mode, for a chat on it.
    pub permission_mode: Option<String>,
    /// How many tasks run in the background (Claude Code's shells and agents).
    pub background: usize,
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

impl ChatInfo {
    pub fn to_json(&self, id: BufferId) -> Value {
        json!({
            "buffer": buffer_id_to_u64(id),
            "model": self.model,
            "thinking": self.thinking,
            "running": self.running,
            "compacting": self.compacting,
            "usage": usage_json(&self.usage),
            "title": self.title,
            "cwd": self.cwd,
            "session": self.session,
            "session_id": self.session_id,
            "machine": self.machine,
            "keep_going": self.keep_going,
            "permission_mode": self.permission_mode,
            "background": self.background,
            "context": {
                "used": self.context.0.unwrap_or(0),
                "window": self.context.1,
                "percent": if self.context.1 > 0 { self.context.0.unwrap_or(0) as f64 * 100.0 / self.context.1 as f64 } else { 0.0 },
            },
        })
    }
}

impl Editor {
    /// The chat `id`, or the current buffer when it is a chat.
    /// The chat whose agent runs with session id `sid` (tools see it as `SERPIS_SESSION_ID`).
    pub fn chat_by_session_id(&self, sid: &str) -> Option<BufferId> {
        self.chats.iter().find(|(_, c)| c.session_id == sid).map(|(id, _)| *id)
    }

    pub fn chat_id(&self, id: Option<BufferId>) -> anyhow::Result<BufferId> {
        let id = id.unwrap_or_else(|| self.current_buffer());
        match self.meta(id) {
            Some(m) if m.major == "chat" => Ok(id),
            Some(m) => anyhow::bail!("{} is not a chat", m.name),
            None => anyhow::bail!("no such buffer"),
        }
    }

    /// Run `f` on chat `id` now, or at the next tick if the chat is busy (handling an event
    /// whose hook called this).
    pub fn with_chat_soon(&mut self, id: BufferId, f: impl FnOnce(&mut ChatBuffer, &mut Editor) + 'static) {
        let free = self.buffers.get(id).is_some_and(|s| s.buf.is_some());
        if free {
            self.with_buffer_as::<ChatBuffer, _>(id, f);
        } else {
            self.defer(move |ed| {
                ed.with_buffer_as::<ChatBuffer, _>(id, f);
            });
        }
    }

    /// Stop chat `id`'s agent.
    pub fn abort_chat(&mut self, id: BufferId) {
        if let Some((machine, chat)) = self.chats.get(&id).map(|c| c.node.clone()) {
            crate::remote::send(self, &machine, serpis_proto::wire::Request::Abort { chat });
        }
    }

    /// A model by `provider/model-id` (the id may contain slashes: `openrouter/z-ai/glm-5.3`).
    pub fn find_model(&self, spec: &str) -> Option<Model> {
        let (provider, id) = spec.split_once('/')?;
        self.registry.find(provider, id)
    }

    /// Whether a provider has credentials (auth.json, environment or models.json).
    pub fn provider_configured(&self, provider: &str) -> bool {
        self.registry.configured_providers().iter().any(|p| p == provider)
    }

    /// Switch chat `id` to model `spec`; errors when it is unknown or has no credentials.
    pub fn set_chat_model(&mut self, id: BufferId, spec: &str) -> anyhow::Result<()> {
        // This machine's models are known here; another machine knows its own (and says so).
        if self.buffer_machine(id).is_none() {
            let model = self.find_model(spec).ok_or_else(|| anyhow::anyhow!("Model {spec} not found (see serpis.agent.models())"))?;
            if !self.provider_configured(&model.provider) {
                anyhow::bail!("No credentials for provider {}", model.provider);
            }
        }
        let spec = spec.to_string();
        self.with_chat_soon(id, move |c, ed| {
            c.set_model_spec(&spec);
            c.publish(ed, id);
        });
        Ok(())
    }

    /// Set chat `id`'s thinking level (clamped to what its model supports).
    pub fn set_chat_thinking(&mut self, id: BufferId, level: &str) -> anyhow::Result<()> {
        let l = ThinkingLevel::parse(level).ok_or_else(|| anyhow::anyhow!("Unknown thinking level {level} (off, minimal, low, medium, high, xhigh, max)"))?;
        self.with_chat_soon(id, move |c, ed| {
            c.set_thinking(l);
            c.publish(ed, id);
        });
        Ok(())
    }

    /// Add a custom entry to chat `id`'s session (pi's `custom` entries), kept on resume.
    pub fn append_chat_entry(&mut self, id: BufferId, kind: &str, data: Value) {
        let kind = kind.to_string();
        // The snapshot sees it at once, even when the chat writes it a tick later.
        if let Some(info) = self.chats.get_mut(&id) {
            info.entries.push((kind.clone(), data.clone()));
        }
        self.with_chat_soon(id, move |c, ed| {
            c.append_entry(&kind, data);
            c.publish(ed, id);
        });
    }
}
