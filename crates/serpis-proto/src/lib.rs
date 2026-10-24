//! What a serpis node and its clients say to each other.
//!
//! A chat runs in one place, its node (a headless serpis on this machine or on a box), and any
//! number of clients show it: the GUI, the iOS app. The node keeps the transcript; clients keep
//! a copy that it updates with [`Splice`]s: "from entry N on, the transcript is now these". While
//! a response streams, only the last entry or two change, so a splice is small, and a client
//! that joins late gets everything as one splice from 0.
//!
//! A transcript [`Entry`] is what a chat shows, not what it sends to the model: it includes
//! notices (compaction, cache misses), queued messages and errors, so every client shows the
//! same chat without running any of the chat's logic itself. How an entry is folded is the
//! viewer's choice and is not part of it.

pub mod wire;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serpis_ai::{AssistantContent, ToolResultMessage, Usage};

/// One item of a chat's transcript.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Entry {
    User {
        text: String,
        /// Sent while the agent ran: it steers the run, and has not been read yet.
        queued: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<Image>,
    },
    Assistant {
        blocks: Vec<AssistantContent>,
        model: String,
        streaming: bool,
        /// Continues the previous assistant entry (text after a tool call in the same turn).
        continuation: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        usage: Option<Usage>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cache: Option<CacheHit>,
    },
    Tool {
        id: String,
        name: String,
        args: Value,
        /// Output so far, while it runs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        partial: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<ToolResultMessage>,
        /// How long it has been running, when the splice was made (clients count on from there).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        running_ms: Option<u64>,
        /// How long it took, once done.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        elapsed_ms: Option<u64>,
    },
    Info {
        text: String,
        tone: Tone,
    },
}

/// An image in a user message, as the model received it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Image {
    pub mime: String,
    /// Base64.
    pub data: String,
}

/// How a response used the prompt cache.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "share", rename_all = "snake_case")]
pub enum CacheHit {
    /// Nothing to read yet.
    New,
    /// Read this share of the prompt from the cache.
    Hit(f64),
    Miss,
}

/// What kind of notice an [`Entry::Info`] is, for its color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Tone {
    /// Small secondary text (compaction, resume markers).
    Meta,
    /// A notice worth a look (a cache miss).
    MetaWarn,
    Warning,
    Error,
}

/// The transcript from entry `from` on is now `entries` (anything after it is gone).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Splice {
    pub from: usize,
    pub entries: Vec<Entry>,
}

impl Splice {
    /// Apply to a client's copy of the transcript.
    pub fn apply(self, transcript: &mut Vec<Entry>) {
        transcript.truncate(self.from.min(transcript.len()));
        transcript.extend(self.entries);
    }
}

/// The chat around its transcript: what a header, a sidebar row or a chat list shows.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ChatState {
    pub title: String,
    /// `provider/id`.
    pub model: Option<String>,
    pub thinking: String,
    pub running: bool,
    pub compacting: bool,
    /// What the agent is doing now: `bash cargo test`, `thinking`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<String>,
    pub cwd: String,
    pub session_id: String,
    /// The whole conversation's usage.
    pub usage: Usage,
    /// Context the conversation fills, and the model's window.
    pub context: (Option<u64>, u64),
    /// `cache 94% · warm 41m`, when anything was cached.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    /// The session file on the chat's machine, once written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,
    /// Custom session entries (`serpis.chat.append`): (type, data), oldest first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub custom: Vec<(String, serde_json::Value)>,
    /// The prompt cache in detail: {hit_rate, state, left, misses}.
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub cache_info: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(text: &str) -> Entry {
        Entry::Info { text: text.into(), tone: Tone::Meta }
    }

    #[test]
    fn splices_replace_the_tail() {
        let mut t = vec![info("a"), info("b"), info("c")];
        Splice { from: 1, entries: vec![info("B")] }.apply(&mut t);
        assert_eq!(t, vec![info("a"), info("B")]);
        // A splice past the end (a client that missed nothing) appends.
        Splice { from: 9, entries: vec![info("d")] }.apply(&mut t);
        assert_eq!(t, vec![info("a"), info("B"), info("d")]);
        Splice { from: 0, entries: vec![] }.apply(&mut t);
        assert!(t.is_empty());
    }

    #[test]
    fn entries_survive_json() {
        let entries = vec![
            Entry::User { text: "hi".into(), queued: true, images: vec![Image { mime: "image/png".into(), data: "AAAA".into() }] },
            Entry::Assistant {
                blocks: vec![AssistantContent::Text { text: "hello".into(), text_signature: None }],
                model: "m".into(),
                streaming: false,
                continuation: false,
                usage: Some(Usage::default()),
                error: None,
                cache: Some(CacheHit::Hit(0.94)),
            },
            Entry::Tool { id: "1".into(), name: "bash".into(), args: serde_json::json!({"command": "ls"}), partial: Some("a\n".into()), result: None, running_ms: Some(1200), elapsed_ms: None },
            Entry::Info { text: "Compacted".into(), tone: Tone::MetaWarn },
        ];
        let s = Splice { from: 3, entries };
        let back: Splice = serde_json::from_str(&serde_json::to_string(&s).unwrap()).unwrap();
        assert_eq!(back, s);
    }
}
