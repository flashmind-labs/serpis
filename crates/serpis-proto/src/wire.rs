//! The conversation between a node and one client.
//!
//! One stream per client: a Unix-socket connection to the node on this machine, or the pipes of
//! `ssh <host> serpis agent` for one elsewhere. Who may open it is decided before serpis is
//! involved (socket permissions, sshd and its keys, rayfish's `ssh_allow`), so nothing here
//! authenticates: whoever has a stream is the user the node runs as.
//!
//! Frames are `[tag: u8][len: u32 BE][body]`, the body JSON. The client sends [`Call`]s, each
//! answered by a [`Reply`] with its id; the node also sends [`Event`]s, unasked, for the chats
//! the client attached to. Both ends skip tags they don't know, so a new kind of frame can be
//! added without breaking an older peer, and an undecodable call is answered with an error
//! that names the node's version rather than a closed stream.

use serde::{Deserialize, Serialize};

use crate::{ChatState, Image, Splice};

/// Bumped when a change would confuse an older peer; [`Request::Hello`] carries it.
pub const VERSION: u32 = 1;

/// Frames larger than this are a protocol error, not an allocation.
pub const MAX_FRAME: usize = 64 << 20;

pub mod tag {
    pub const CALL: u8 = 0x01;
    pub const REPLY: u8 = 0x02;
    pub const EVENT: u8 = 0x03;
}

/// A request and the id its reply will carry.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Call {
    pub id: u64,
    #[serde(flatten)]
    pub request: Request,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Request {
    /// The first call: which versions meet. Answered with [`Response::Hello`].
    Hello { version: u32, client: String },
    /// The chats running on the node.
    List,
    /// A new chat in `cwd` (a path on the node's machine). `model` is `provider/id`; without
    /// one, the node's default.
    New {
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking: Option<String>,
    },
    /// Continue a saved session (a session file on the node's machine) as a running chat.
    Resume { session: String },
    /// Send this client the chat's transcript and state, then every change to them.
    Attach { chat: String },
    Detach { chat: String },
    /// A user message: starts a run, or steers the running one (`follow_up`: after it).
    Prompt {
        chat: String,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<Image>,
        #[serde(default)]
        follow_up: bool,
        /// A client's id for this prompt: sending it again (after a lost reply) doesn't send
        /// the message twice.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        request_id: Option<String>,
    },
    Abort { chat: String },
    Compact {
        chat: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        instructions: Option<String>,
    },
    SetModel { chat: String, model: String },
    SetThinking { chat: String, level: String },
    /// Stop the chat and drop it from the node (its session stays on disk).
    Close { chat: String },
    /// The models the node can use (its own keys and models.json).
    Models,
    /// Saved sessions on the node, newest first.
    Sessions {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    /// Stop the node: its chats are aborted, their sessions kept to reopen when it starts again.
    Stop,
    /// Set the machine up with what the client sends: API keys by provider (auth.json entries),
    /// models.json's providers, and settings. Only what the machine doesn't have yet is added.
    Setup {
        #[serde(default)]
        auth: serde_json::Map<String, serde_json::Value>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        models: Option<serde_json::Value>,
        #[serde(default)]
        settings: serde_json::Map<String, serde_json::Value>,
    },
    /// The folders in a directory on the node (`~` and relative paths start from its home),
    /// for picking where a chat runs.
    Dirs { path: String },
}

/// The answer to the [`Call`] with the same id.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Reply {
    pub id: u64,
    #[serde(flatten)]
    pub response: Response,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Error { message: String },
    Hello {
        version: u32,
        host: String,
        serpis: String,
        home: String,
        /// What the machine has set up for chats.
        #[serde(default)]
        setup: NodeSetup,
    },
    Chats { chats: Vec<ChatSummary> },
    /// The chat a `New` or `Resume` made.
    Chat { chat: String },
    Models { models: Vec<ModelInfo> },
    Sessions { sessions: Vec<SessionInfo> },
    Dirs { path: String, dirs: Vec<String> },
    /// What a `Setup` added (`auth:openrouter`, `models:local`, `settings:agent-model`).
    Setup { added: Vec<String> },
}

/// What a machine has set up for chats.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeSetup {
    /// Providers it has credentials for.
    pub providers: Vec<String>,
    /// Where they come from (`~/.serpis/agent`, `~/.pi/agent`).
    pub sources: Vec<String>,
}

/// Something about an attached chat, unasked.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Splice { chat: String, splice: Splice },
    State { chat: String, state: ChatState },
    /// The chat was closed on the node.
    Closed { chat: String },
    /// Something to tell whoever shows the chat (a refused prompt, a cache warning).
    Notice { chat: String, level: NoticeLevel, text: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoticeLevel {
    Info,
    Warn,
    Error,
}

/// A chat running on a node: its id (the session id) and state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatSummary {
    pub chat: String,
    pub state: ChatState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    /// `provider/id`.
    pub model: String,
    pub name: String,
    pub reasoning: bool,
    pub images: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// The session file on the node.
    pub path: String,
    pub id: String,
    pub title: String,
    pub cwd: String,
    /// Milliseconds since the epoch.
    pub modified: u64,
    pub messages: usize,
}

/// One frame's bytes.
pub fn encode(tag: u8, body: &impl Serialize) -> Vec<u8> {
    let json = serde_json::to_vec(body).expect("protocol types serialize");
    let mut out = Vec::with_capacity(5 + json.len());
    out.push(tag);
    out.extend_from_slice(&(json.len() as u32).to_be_bytes());
    out.extend_from_slice(&json);
    out
}

/// Reassembles frames from a byte stream however it was cut.
#[derive(Default)]
pub struct Decoder {
    buf: Vec<u8>,
}

impl Decoder {
    pub fn push(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    /// The next whole frame: its tag and body. An oversized frame is an error; the stream
    /// can't be read any further.
    pub fn next(&mut self) -> Result<Option<(u8, Vec<u8>)>, String> {
        if self.buf.len() < 5 {
            return Ok(None);
        }
        let len = u32::from_be_bytes([self.buf[1], self.buf[2], self.buf[3], self.buf[4]]) as usize;
        if len > MAX_FRAME {
            return Err(format!("a {len}-byte frame is over the {MAX_FRAME}-byte limit"));
        }
        if self.buf.len() < 5 + len {
            return Ok(None);
        }
        let tag = self.buf[0];
        let body = self.buf[5..5 + len].to_vec();
        self.buf.drain(..5 + len);
        Ok(Some((tag, body)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frames_come_back_however_the_stream_is_cut() {
        let calls = [
            Call { id: 1, request: Request::Hello { version: VERSION, client: "test".into() } },
            Call { id: 2, request: Request::Prompt { chat: "c".into(), text: "hi".into(), images: vec![], follow_up: false, request_id: Some("r1".into()) } },
        ];
        let bytes: Vec<u8> = calls.iter().flat_map(|c| encode(tag::CALL, c)).collect();
        for cut in 1..bytes.len() {
            let mut d = Decoder::default();
            let mut got = Vec::new();
            for chunk in bytes.chunks(cut) {
                d.push(chunk);
                while let Some((t, body)) = d.next().unwrap() {
                    assert_eq!(t, tag::CALL);
                    got.push(serde_json::from_slice::<Call>(&body).unwrap());
                }
            }
            assert_eq!(got, calls, "cut every {cut} bytes");
        }
    }

    #[test]
    fn oversized_frames_are_refused() {
        let mut d = Decoder::default();
        d.push(&[tag::CALL, 0xff, 0xff, 0xff, 0xff]);
        assert!(d.next().is_err());
    }

    #[test]
    fn calls_read_as_documented() {
        let c: Call = serde_json::from_str(r#"{"id":7,"op":"new","cwd":"~/src/app"}"#).unwrap();
        assert_eq!(c, Call { id: 7, request: Request::New { cwd: "~/src/app".into(), model: None, thinking: None } });
        let r = serde_json::to_value(Reply { id: 7, response: Response::Chat { chat: "x".into() } }).unwrap();
        assert_eq!(r, serde_json::json!({"id": 7, "kind": "chat", "chat": "x"}));
    }
}
