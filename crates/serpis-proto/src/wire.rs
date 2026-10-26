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
    /// A terminal's bytes, raw ([`terminal_frame`]): what was typed, client to node; what it
    /// printed, node to client (an attached one).
    pub const TERMINAL: u8 = 0x04;
}

/// A [`tag::TERMINAL`] frame: the terminal's name, then its bytes.
pub fn terminal_frame(name: &str, bytes: &[u8]) -> Vec<u8> {
    let name = &name.as_bytes()[..name.len().min(255)];
    let len = 1 + name.len() + bytes.len();
    let mut out = Vec::with_capacity(5 + len);
    out.push(tag::TERMINAL);
    out.extend_from_slice(&(len as u32).to_be_bytes());
    out.push(name.len() as u8);
    out.extend_from_slice(name);
    out.extend_from_slice(bytes);
    out
}

/// A [`tag::TERMINAL`] frame's name and bytes.
pub fn parse_terminal(body: &[u8]) -> Option<(String, &[u8])> {
    let n = *body.first()? as usize;
    let name = std::str::from_utf8(body.get(1..1 + n)?).ok()?;
    Some((name.to_string(), &body[1 + n..]))
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
    /// one, the node's default. `chat`: its id, chosen by the client (a UUID), so it can send
    /// what it has for the chat before the answer comes; without one, the node picks.
    New {
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        chat: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        thinking: Option<String>,
    },
    /// Continue a saved session (a session file on the node's machine) as a running chat.
    Resume { session: String },
    /// Send this client the chat's transcript and state, then every change to them.
    Attach { chat: String },
    /// Stop sending this client the chat. `close`: also close the chat, when it is idle and no
    /// other client shows it (its session stays, to resume).
    Detach {
        chat: String,
        #[serde(default)]
        close: bool,
    },
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
    /// Read the project's instructions again with the next message (rewrites the prompt cache).
    RefreshContext { chat: String },
    /// Name the chat's session.
    Rename { chat: String, name: String },
    /// Move the chat's cache clock back `secs` seconds (for tests of expiry).
    AgeCache { chat: String, secs: f64 },
    /// Read models.json and auth.json again.
    ReloadModels,
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
    /// An idle chat's session file, to continue it on another machine.
    Export { chat: String },
    /// Continue a chat from another machine: its session file's text, in `cwd` here (a path on
    /// this machine). An older copy of the same chat here is replaced. Answered with `Chat`.
    Import { session: String, cwd: String },
    /// The chat continues elsewhere (`to`, a machine): mark its session so, and close it here.
    Moved { chat: String, to: String },
    /// Pack a folder here (its repository, when in one) for carrying to another machine:
    /// answered with `Packed`, a file to `Fetch` and `Drop`.
    Pack { dir: String },
    /// `len` bytes of a packed file from `offset`.
    Fetch { name: String, offset: u64, len: u64 },
    /// Bytes of a file being carried here (base64), written at `offset`.
    Put { name: String, offset: u64, data: String },
    /// Put a carried folder in its place here (where it was before, beside a clone of it, or
    /// a new folder as `places` says: {repo, plain} templates): answered with `Unpacked`, the
    /// chat's folder in it, and what was kept aside on the way.
    Unpack {
        name: String,
        packed: serde_json::Value,
        #[serde(default)]
        under: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        places: Option<serde_json::Value>,
    },
    /// Forget a packed or carried file.
    Drop { name: String },
    /// Stop the node so a newer build can start, unless a chat is mid-turn (then an error says
    /// so): its open chats reopen in the new one.
    Restart,
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
    /// Settings for the node's chats, over what its config says (null: unset): the settings of
    /// the window on the node's own machine, which runs the same config, and what it changes.
    SetVars { vars: serde_json::Map<String, serde_json::Value> },
    /// Load the node's Lua config again (init.lua and extensions): answered with `Reloaded`.
    Reload,
    /// Add a custom entry to a chat's session (`serpis.chat.append`).
    Append { chat: String, kind: String, data: serde_json::Value },
    /// Tools this client runs for the chats it shows (a window's `serpis_lua`): its agents get
    /// them while this client is attached, and calls come as `ToolCall` events. Replaces what
    /// it lent before.
    Lend { tools: Vec<ToolSpec> },
    /// The result of a `ToolCall`.
    ToolDone {
        call: u64,
        text: String,
        #[serde(default)]
        is_error: bool,
    },
    /// The folders in a directory on the node (`~` and relative paths start from its home),
    /// for picking where a chat runs.
    Dirs { path: String },
    /// The terminals on the node's machine (held by its terminal host, `serpis-node
    /// terminals`), with what each is doing.
    Terminals,
    /// Start a terminal there: the user's login shell in `cwd` (`~` and relative paths start
    /// from its home). Answered with `Terminal`; `AttachTerminal` shows it.
    NewTerminal {
        cwd: String,
        cols: u16,
        rows: u16,
        /// Prompt marks and directory reports from a zsh (serpis's shell integration).
        #[serde(default)]
        integration: bool,
    },
    /// End a terminal (its programs are hung up).
    KillTerminal { name: String },
    /// Show a terminal: what it prints comes as [`tag::TERMINAL`] frames from now on, starting
    /// with its screen (and, with `history`, what it printed before, for a fresh view's
    /// scrollback). What is typed goes the same way, client to node.
    AttachTerminal {
        name: String,
        cols: u16,
        rows: u16,
        #[serde(default)]
        history: bool,
    },
    /// Stop showing it (it keeps running).
    DetachTerminal { name: String },
    /// The size the terminal is shown at.
    ResizeTerminal { name: String, cols: u16, rows: u16 },
    /// Start the node when the machine boots and again when it crashes (a systemd user unit, a
    /// launchd agent), so chats cut off by either continue without anyone connecting. Answered
    /// with `Service`.
    InstallService,
    /// Stop doing that. Answered with `Service` (`installed` false).
    UninstallService,
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
        /// This node's build: the SHA-256 of its binary (the name serpis installs it under).
        #[serde(default)]
        build: String,
        /// What went wrong loading its Lua config.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        notes: Vec<String>,
    },
    Chats { chats: Vec<ChatSummary> },
    /// The chat a `New` or `Resume` made.
    Chat { chat: String },
    Models { models: Vec<ModelInfo> },
    Sessions { sessions: Vec<SessionInfo> },
    Dirs { path: String, dirs: Vec<String> },
    /// A session file's text (`Export`).
    Session { text: String },
    Packed { name: String, size: u64, packed: serde_json::Value },
    /// Bytes of a packed file (base64).
    Chunk { data: String },
    Unpacked {
        cwd: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
    /// The Lua config loaded again: what went wrong, if anything.
    Reloaded { errors: Vec<String> },
    /// What a `Setup` added (`auth:openrouter`, `models:local`, `settings:agent-model`).
    Setup { added: Vec<String> },
    Terminals { terminals: Vec<TerminalInfo> },
    /// The terminal a `NewTerminal` started, by its name there.
    Terminal { name: String },
    /// The node's service: `systemd` or `launchd`, its file, and what is left to do.
    Service {
        installed: bool,
        manager: String,
        file: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        note: Option<String>,
    },
}

/// A terminal on a machine, as its terminal node tells it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TerminalInfo {
    /// What it is called there.
    pub name: String,
    /// The title its program set, else the command.
    pub title: String,
    pub command: String,
    /// Where its foreground program is, when the machine can say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// The program in the foreground and its arguments; empty at the shell's prompt.
    #[serde(default)]
    pub foreground: Vec<String>,
    /// Clients attached to it now.
    pub attached: usize,
    /// Seconds since it last printed.
    pub idle: u64,
    /// Milliseconds since the epoch.
    #[serde(default)]
    pub started: u64,
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
    /// Something to tell every client (the node's Lua said it, outside any chat).
    Message { level: NoticeLevel, text: String },
    /// Run a tool this client lent (`Lend`) for `chat`, and answer with `ToolDone`.
    ToolCall { chat: String, call: u64, name: String, args: serde_json::Value },
    /// An attached terminal ended (`code`: how its program exited, when known).
    TerminalExited {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<i32>,
    },
    /// What an attached terminal printed: a [`tag::TERMINAL`] frame, as the client hands it on
    /// (never sent as an event).
    #[serde(skip)]
    TerminalOutput { name: String, data: Vec<u8> },
}

/// A tool a client lends: what the model is told about it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
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
    #[serde(default)]
    pub context_window: u64,
    /// Dollars per million tokens: input, output.
    #[serde(default)]
    pub cost: (f64, f64),
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
    /// The machine the chat moved to, when it continues elsewhere.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_to: Option<String>,
    /// Open on the node now (a client can attach to it); `running`: working on a turn.
    #[serde(default)]
    pub open: bool,
    #[serde(default)]
    pub running: bool,
}

/// A frame made again from its tag and body (as [`Decoder::next`] gives them), to pass on.
pub fn frame(tag: u8, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(5 + body.len());
    out.push(tag);
    out.extend_from_slice(&(body.len() as u32).to_be_bytes());
    out.extend_from_slice(body);
    out
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
    fn requests_round_trip_inside_calls() {
        // A request's fields sit beside the call's own `id`: none may be named so.
        let requests = [
            Request::New { cwd: "~".into(), model: None, thinking: None, chat: Some("c1".into()) },
            Request::Detach { chat: "c1".into(), close: true },
            Request::ToolDone { call: 3, text: "t".into(), is_error: false },
            Request::SetVars { vars: serde_json::Map::from_iter([("a".to_string(), serde_json::json!(1))]) },
        ];
        for request in requests {
            let call = Call { id: 9, request };
            let json = serde_json::to_string(&call).unwrap();
            assert_eq!(json.matches("\"id\"").count(), 1, "{json}");
            assert_eq!(serde_json::from_str::<Call>(&json).unwrap(), call);
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
        assert_eq!(c, Call { id: 7, request: Request::New { cwd: "~/src/app".into(), model: None, thinking: None, chat: None } });
        let r = serde_json::to_value(Reply { id: 7, response: Response::Chat { chat: "x".into() } }).unwrap();
        assert_eq!(r, serde_json::json!({"id": 7, "kind": "chat", "chat": "x"}));
    }
}
