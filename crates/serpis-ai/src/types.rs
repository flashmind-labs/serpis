//! Core message, model and usage types. Field names serialize like pi-ai's so
//! transcripts, `models.json` and catalog entries are interchangeable.

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    #[default]
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    pub const ALL: [ThinkingLevel; 7] = [
        ThinkingLevel::Off,
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::Xhigh,
        ThinkingLevel::Max,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            ThinkingLevel::Off => "off",
            ThinkingLevel::Minimal => "minimal",
            ThinkingLevel::Low => "low",
            ThinkingLevel::Medium => "medium",
            ThinkingLevel::High => "high",
            ThinkingLevel::Xhigh => "xhigh",
            ThinkingLevel::Max => "max",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|l| l.as_str() == s)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UserContent {
    Text { text: String },
    #[serde(rename_all = "camelCase")]
    Image { data: String, mime_type: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thought_signature: Option<String>,
    /// Set when the model's arguments text was not a valid JSON object; `arguments` is then `{}`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arguments_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum AssistantContent {
    #[serde(rename_all = "camelCase")]
    Text {
        text: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text_signature: Option<String>,
    },
    #[serde(rename_all = "camelCase")]
    Thinking {
        thinking: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking_signature: Option<String>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        redacted: bool,
    },
    ToolCall(ToolCall),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum StopReason {
    #[default]
    Pending,
    Stop,
    Length,
    ToolUse,
    Error,
    Aborted,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Cost {
    pub input: f64,
    pub output: f64,
    #[serde(rename = "cacheRead")]
    pub cache_read: f64,
    #[serde(rename = "cacheWrite")]
    pub cache_write: f64,
    pub total: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Usage {
    pub input: u64,
    pub output: u64,
    pub cache_read: u64,
    pub cache_write: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write_1h: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<u64>,
    pub total_tokens: u64,
    pub cost: Cost,
}

impl Usage {
    pub fn recompute_total(&mut self) {
        self.total_tokens = self.input + self.output + self.cache_read + self.cache_write;
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AssistantMessage {
    pub content: Vec<AssistantContent>,
    pub api: String,
    pub provider: String,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub response_model: Option<String>,
    pub usage: Usage,
    pub stop_reason: StopReason,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_stop_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    pub timestamp: u64,
}

impl AssistantMessage {
    pub fn empty(model: &Model) -> Self {
        AssistantMessage {
            content: Vec::new(),
            api: model.api.clone(),
            provider: model.provider.clone(),
            model: model.id.clone(),
            response_id: None,
            response_model: None,
            usage: Usage::default(),
            stop_reason: StopReason::Pending,
            raw_stop_reason: None,
            error_message: None,
            timestamp: now_ms(),
        }
    }

    pub fn tool_calls(&self) -> impl Iterator<Item = &ToolCall> {
        self.content.iter().filter_map(|c| match c {
            AssistantContent::ToolCall(tc) => Some(tc),
            _ => None,
        })
    }

    pub fn text(&self) -> String {
        let mut out = String::new();
        for c in &self.content {
            if let AssistantContent::Text { text, .. } = c {
                out.push_str(text);
            }
        }
        out
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolResultMessage {
    pub tool_call_id: String,
    pub tool_name: String,
    pub content: Vec<UserContent>,
    #[serde(default)]
    pub is_error: bool,
    /// Tool-specific data for renderers (diffs, truncation info, ...). Never sent to the model.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub details: Value,
    pub timestamp: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "camelCase")]
pub enum Message {
    User {
        content: Vec<UserContent>,
        timestamp: u64,
    },
    Assistant(AssistantMessage),
    ToolResult(ToolResultMessage),
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Message::User {
            content: vec![UserContent::Text { text: text.into() }],
            timestamp: now_ms(),
        }
    }
}

/// A tool declaration as the model sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// JSON schema of the arguments object.
    pub parameters: Value,
}

/// Everything a request needs besides the model and options.
#[derive(Debug, Clone, Default)]
pub struct Context {
    pub system_prompt: String,
    pub messages: Vec<Message>,
    pub tools: Vec<Tool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CostTier {
    pub input_tokens_above: u64,
    pub input: f64,
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCost {
    #[serde(default)]
    pub input: f64,
    #[serde(default)]
    pub output: f64,
    #[serde(default)]
    pub cache_read: f64,
    #[serde(default)]
    pub cache_write: f64,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tiers: Vec<CostTier>,
}

fn default_input() -> Vec<String> {
    vec!["text".into()]
}
fn default_context_window() -> u64 {
    128_000
}
fn default_max_tokens() -> u64 {
    16_384
}

/// A chat model. Unknown catalog fields are kept in `extra` so nothing is lost on round trips.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Model {
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub api: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub reasoning: bool,
    /// Per-level provider values. `None` (JSON null) marks a level as unsupported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking_level_map: Option<HashMap<String, Option<String>>>,
    #[serde(default = "default_input")]
    pub input: Vec<String>,
    #[serde(default)]
    pub cost: ModelCost,
    #[serde(default = "default_context_window")]
    pub context_window: u64,
    #[serde(default = "default_max_tokens")]
    pub max_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub headers: Option<HashMap<String, String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compat: Option<Map<String, Value>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sampling_params: Option<Map<String, Value>>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl Model {
    pub fn supports_images(&self) -> bool {
        self.input.iter().any(|i| i == "image")
    }

    pub fn compat_value(&self, key: &str) -> Option<&Value> {
        self.compat.as_ref().and_then(|c| c.get(key))
    }

    pub fn compat_bool(&self, key: &str) -> Option<bool> {
        self.compat_value(key).and_then(Value::as_bool)
    }

    pub fn compat_str(&self, key: &str) -> Option<&str> {
        self.compat_value(key).and_then(Value::as_str)
    }

    /// `Some(None)` = explicitly unsupported (null), `None` = not mapped.
    pub fn thinking_map(&self, level: ThinkingLevel) -> Option<Option<&str>> {
        self.thinking_level_map
            .as_ref()
            .and_then(|m| m.get(level.as_str()))
            .map(|v| v.as_deref())
    }

    pub fn display_name(&self) -> &str {
        if self.name.is_empty() { &self.id } else { &self.name }
    }
}

/// Thinking levels a model accepts, mirroring pi-ai's `getSupportedThinkingLevels`.
pub fn supported_thinking_levels(model: &Model) -> Vec<ThinkingLevel> {
    if !model.reasoning {
        return vec![ThinkingLevel::Off];
    }
    ThinkingLevel::ALL
        .into_iter()
        .filter(|level| match model.thinking_map(*level) {
            Some(None) => false,
            mapped => {
                if matches!(level, ThinkingLevel::Xhigh | ThinkingLevel::Max) {
                    mapped.is_some()
                } else {
                    true
                }
            }
        })
        .collect()
}

pub fn clamp_thinking_level(model: &Model, level: ThinkingLevel) -> ThinkingLevel {
    let available = supported_thinking_levels(model);
    if available.contains(&level) {
        return level;
    }
    let all = ThinkingLevel::ALL;
    let idx = all.iter().position(|l| *l == level).unwrap_or(0);
    for cand in &all[idx..] {
        if available.contains(cand) {
            return *cand;
        }
    }
    for cand in all[..idx].iter().rev() {
        if available.contains(cand) {
            return *cand;
        }
    }
    available.first().copied().unwrap_or(ThinkingLevel::Off)
}

pub fn calculate_cost(model: &Model, usage: &mut Usage) {
    let input_tokens = usage.input + usage.cache_read + usage.cache_write;
    let (mut input, mut output, mut cache_read, mut cache_write) = (
        model.cost.input,
        model.cost.output,
        model.cost.cache_read,
        model.cost.cache_write,
    );
    let mut matched: i64 = -1;
    for tier in &model.cost.tiers {
        if input_tokens > tier.input_tokens_above && tier.input_tokens_above as i64 > matched {
            input = tier.input;
            output = tier.output;
            cache_read = tier.cache_read;
            cache_write = tier.cache_write;
            matched = tier.input_tokens_above as i64;
        }
    }
    // Anthropic charges 2x base input for 1h cache writes.
    let long_write = usage.cache_write_1h.unwrap_or(0);
    let short_write = usage.cache_write.saturating_sub(long_write);
    usage.cost.input = input / 1e6 * usage.input as f64;
    usage.cost.output = output / 1e6 * usage.output as f64;
    usage.cost.cache_read = cache_read / 1e6 * usage.cache_read as f64;
    usage.cost.cache_write = (cache_write * short_write as f64 + input * 2.0 * long_write as f64) / 1e6;
    usage.cost.total = usage.cost.input + usage.cost.output + usage.cost.cache_read + usage.cost.cache_write;
}
