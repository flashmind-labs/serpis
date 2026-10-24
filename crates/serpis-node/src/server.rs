//! The node's server: one thread owns every chat, and connections reach it with messages.
//!
//! Chats are synchronous objects ([`Chat`] is polled and called, never shared), so they live on
//! one thread, the node's loop. The socket and its connections are tokio tasks that hand calls
//! to the loop and write what it sends back. The loop wakes when a call arrives or a chat has
//! news, answers calls, polls chats, and sends each attached client what changed, at most
//! about 60 times a second, so a streaming response goes out in batches of deltas.
//!
//! A client that can't keep up (its queue fills) is sent nothing more until it has room again,
//! and then the whole transcript of each chat it shows, instead of everything it missed, as
//! pi-durable does for slow watches.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use serde_json::{Value, json};
use serpis_chat::{Chat, ChatInit, Host, Level};
use serpis_proto::wire::{self, Call, ChatSummary, Event, ModelInfo, NoticeLevel, Reply, Request, Response, SessionInfo, tag};
use serpis_proto::ChatState;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc as tmpsc;

/// How many frames a client may have waiting before it is treated as lagging.
const QUEUE: usize = 1024;

/// The shortest time between two rounds of updates.
const BATCH: Duration = Duration::from_millis(16);

pub struct Config {
    pub socket: PathBuf,
    /// Settings, open chats and the log.
    pub dir: PathBuf,
    /// Models, credentials and sessions: `~/.serpis/agent` unless set.
    pub agent_dir: Option<PathBuf>,
}

impl Config {
    pub fn default_paths() -> Config {
        Config { socket: crate::socket_path(), dir: crate::node_dir(), agent_dir: None }
    }
}

enum Msg {
    Connected { client: u64, tx: tmpsc::Sender<Vec<u8>> },
    Call { client: u64, body: Vec<u8> },
    Gone { client: u64 },
    /// A chat has news.
    Wake,
    /// The answer to a call that took a while (packing, unpacking), done off this thread.
    Reply { client: u64, id: u64, response: Response },
    Stop,
}

/// A running node; dropping it does not stop it, [`Running::stop`] does.
pub struct Running {
    tx: mpsc::Sender<Msg>,
    thread: Option<std::thread::JoinHandle<()>>,
    pub socket: PathBuf,
}

impl Running {
    /// Stop the node: its chats are aborted, and their sessions stay on disk to reopen.
    pub fn stop(mut self) {
        let _ = self.tx.send(Msg::Stop);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        let _ = std::fs::remove_file(&self.socket);
    }

    /// Wait until the node stops by itself (it doesn't, until killed).
    pub fn wait(mut self) {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Start a node on `rt`: bind its socket, reopen the chats it had open, and serve.
pub fn start(config: Config, rt: tokio::runtime::Handle) -> Result<Running> {
    // Its settings and open chats live here, wherever the socket is.
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&config.dir).with_context(|| format!("creating {}", config.dir.display()))?;
    }
    let listener = {
        let _in_rt = rt.enter();
        bind(&config.socket)?
    };
    let (tx, rx) = mpsc::channel();
    let paths = config.agent_dir.clone().map(|agent_dir| serpis_ai::Paths { agent_dir }).unwrap_or_else(serpis_ai::Paths::default_paths);
    let registry = serpis_ai::Registry::new(paths);
    for e in registry.load_errors() {
        tracing::warn!("{e}");
    }
    let refresh = registry.clone();
    rt.spawn(async move {
        for e in refresh.refresh_catalogs(false).await {
            tracing::warn!("model catalog: {e}");
        }
    });
    let wake_tx = tx.clone();
    let msgs = tx.clone();
    let mut node = Node {
        msgs,
        transfer: config.dir.join("transfer"),
        chats: BTreeMap::new(),
        clients: HashMap::new(),
        settings: Settings::load(config.dir.join("settings.json")),
        registry,
        rt: rt.clone(),
        wake: Arc::new(move || {
            let _ = wake_tx.send(Msg::Wake);
        }),
        open_file: config.dir.join("open.json"),
        open_written: Vec::new(),
        session_cache: HashMap::new(),
    };
    node.reopen();
    let conn_tx = tx.clone();
    rt.spawn(accept(listener, conn_tx));
    let socket = config.socket.clone();
    let thread = std::thread::Builder::new().name("serpis-node".into()).spawn(move || {
        node.run(rx);
        let _ = std::fs::remove_file(&socket);
    })?;
    Ok(Running { tx, thread: Some(thread), socket: config.socket })
}

/// Bind the socket in an owner-only directory. A socket nobody answers on is a node that died
/// and is replaced; one that answers is a node already running.
fn bind(socket: &Path) -> Result<tokio::net::UnixListener> {
    use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
    let dir = socket.parent().context("the socket path has no directory")?;
    std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    if socket.exists() {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            bail!("a node is already running on {}", socket.display());
        }
        std::fs::remove_file(socket)?;
    }
    let std_listener = std::os::unix::net::UnixListener::bind(socket).with_context(|| format!("listening on {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    std_listener.set_nonblocking(true)?;
    // Converted on the runtime that serves it.
    Ok(tokio::net::UnixListener::from_std(std_listener)?)
}

async fn accept(listener: tokio::net::UnixListener, tx: mpsc::Sender<Msg>) {
    let me = nix::unistd::getuid().as_raw();
    let mut next = 0u64;
    loop {
        let Ok((stream, _)) = listener.accept().await else { continue };
        // The directory already keeps others out; this holds even if its mode is changed.
        match stream.peer_cred() {
            Ok(c) if c.uid() == me => {}
            other => {
                tracing::warn!("refused a connection from another user: {other:?}");
                continue;
            }
        }
        next += 1;
        tokio::spawn(connection(stream, next, tx.clone()));
    }
}

async fn connection(stream: tokio::net::UnixStream, client: u64, tx: mpsc::Sender<Msg>) {
    let (mut rd, mut wr) = stream.into_split();
    let (otx, mut orx) = tmpsc::channel::<Vec<u8>>(QUEUE);
    if tx.send(Msg::Connected { client, tx: otx }).is_err() {
        return;
    }
    let writer = tokio::spawn(async move {
        while let Some(bytes) = orx.recv().await {
            if wr.write_all(&bytes).await.is_err() {
                break;
            }
        }
    });
    let mut decoder = wire::Decoder::default();
    let mut buf = vec![0u8; 64 * 1024];
    'read: loop {
        let n = match rd.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        decoder.push(&buf[..n]);
        loop {
            match decoder.next() {
                Ok(Some((tag::CALL, body))) => {
                    if tx.send(Msg::Call { client, body }).is_err() {
                        break 'read;
                    }
                }
                // Frames of kinds this node doesn't know are skipped.
                Ok(Some(_)) => {}
                Ok(None) => break,
                Err(e) => {
                    tracing::warn!("client {client}: {e}");
                    break 'read;
                }
            }
        }
    }
    let _ = tx.send(Msg::Gone { client });
    let _ = writer.await;
}

/// The node's settings (`settings.json` in its directory): the same variables the desktop
/// has (`agent-model`, `agent-thinking`, `session-directory`, `chat-compact-auto`, ...), read
/// again whenever the file changes.
struct Settings {
    path: PathBuf,
    vars: serde_json::Map<String, Value>,
    modified: Option<SystemTime>,
    checked: Instant,
}

impl Settings {
    fn load(path: PathBuf) -> Settings {
        let mut s = Settings { path, vars: Default::default(), modified: None, checked: Instant::now() };
        s.reload();
        s
    }

    fn reload(&mut self) {
        self.modified = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        self.vars = match std::fs::read_to_string(&self.path) {
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(Value::Object(o)) => o,
                Ok(_) | Err(_) => {
                    tracing::warn!("{} isn't a JSON object of settings; ignoring it", self.path.display());
                    Default::default()
                }
            },
            Err(_) => Default::default(),
        };
    }

    fn refresh(&mut self) {
        if self.checked.elapsed() < Duration::from_secs(1) {
            return;
        }
        self.checked = Instant::now();
        if std::fs::metadata(&self.path).and_then(|m| m.modified()).ok() != self.modified {
            self.reload();
        }
    }
}

struct Slot {
    chat: Chat,
    /// The state clients were last sent.
    sent_state: Option<ChatState>,
    notices: Vec<(Level, String)>,
}

struct Client {
    tx: tmpsc::Sender<Vec<u8>>,
    attached: BTreeSet<String>,
    /// Its queue filled: it is sent nothing until it has room, then everything afresh.
    lagging: bool,
}

struct Node {
    /// To this node's own loop (answers of work done elsewhere).
    msgs: mpsc::Sender<Msg>,
    /// Where carried folders are packed and arrive (its user's alone).
    transfer: PathBuf,
    chats: BTreeMap<String, Slot>,
    clients: HashMap<u64, Client>,
    settings: Settings,
    registry: serpis_ai::Registry,
    rt: tokio::runtime::Handle,
    wake: serpis_chat::Wake,
    /// The session files of open chats, reopened when the node starts again.
    open_file: PathBuf,
    open_written: Vec<String>,
    session_cache: HashMap<PathBuf, serpis_chat::session::Info>,
}

/// What a chat on the node gets from it.
struct NodeHost<'a> {
    settings: &'a Settings,
    registry: &'a serpis_ai::Registry,
    rt: &'a tokio::runtime::Handle,
    notices: &'a mut Vec<(Level, String)>,
    chat: &'a str,
}

impl Host for NodeHost<'_> {
    fn var(&self, name: &str) -> Option<Value> {
        self.settings.vars.get(name).cloned()
    }
    fn hook(&mut self, _: &Chat, _: &str, _: &Value) -> Vec<Value> {
        // Hooks come with the node's own Lua config (later); none run yet.
        Vec::new()
    }
    fn notify(&mut self, level: Level, text: String) {
        self.notices.push((level, text));
    }
    fn tools(&self) -> Vec<serpis_agent::ToolRef> {
        let disabled: Vec<String> = self.var("agent-disabled-tools").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        serpis_agent::builtin_tools().into_iter().filter(|t| !disabled.iter().any(|d| d == t.name())).collect()
    }
    fn tool_hooks(&self) -> Option<Arc<dyn serpis_agent::ToolHooks>> {
        None
    }
    fn registry(&self) -> &serpis_ai::Registry {
        self.registry
    }
    fn runtime(&self) -> tokio::runtime::Handle {
        self.rt.clone()
    }
    fn chat_ref(&self) -> Value {
        json!(self.chat)
    }
}

impl Node {
    fn run(mut self, rx: mpsc::Receiver<Msg>) {
        let mut last = Instant::now();
        loop {
            let msg = match rx.recv_timeout(Duration::from_secs(1)) {
                Ok(m) => Some(m),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            let mut stop = false;
            for m in msg.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
                match m {
                    Msg::Connected { client, tx } => {
                        self.clients.insert(client, Client { tx, attached: BTreeSet::new(), lagging: false });
                    }
                    Msg::Call { client, body } => stop |= self.call(client, &body),
                    Msg::Gone { client } => {
                        self.clients.remove(&client);
                    }
                    Msg::Wake => {}
                    Msg::Reply { client, id, response } => self.reply(client, id, response),
                    Msg::Stop => stop = true,
                }
            }
            self.settings.refresh();
            self.poll();
            self.flush();
            self.remember_open();
            if stop {
                for slot in self.chats.values_mut() {
                    slot.chat.abort();
                }
                break;
            }
            // News arriving meanwhile goes out together.
            let wait = BATCH.saturating_sub(last.elapsed());
            if !wait.is_zero() {
                std::thread::sleep(wait);
            }
            last = Instant::now();
        }
    }

    fn host<'a>(settings: &'a Settings, registry: &'a serpis_ai::Registry, rt: &'a tokio::runtime::Handle, id: &'a str, slot: &'a mut Slot) -> (&'a mut Chat, NodeHost<'a>) {
        let Slot { chat, notices, .. } = slot;
        (chat, NodeHost { settings, registry, rt, notices, chat: id })
    }

    /// Run `f` on chat `id` with the node as its host.
    fn with_chat<R>(&mut self, id: &str, f: impl FnOnce(&mut Chat, &mut NodeHost) -> R) -> Result<R, String> {
        let Node { chats, settings, registry, rt, .. } = self;
        let slot = chats.get_mut(id).ok_or_else(|| format!("no chat {id} on this node"))?;
        let (chat, mut host) = Self::host(settings, registry, rt, id, slot);
        Ok(f(chat, &mut host))
    }

    fn poll(&mut self) {
        let ids: Vec<String> = self.chats.keys().cloned().collect();
        for id in ids {
            let _ = self.with_chat(&id, |c, h| c.poll(h));
        }
    }

    /// Send each attached client what changed in the chats it shows.
    fn flush(&mut self) {
        // Clients that had to stop catch up with everything at once, when they have room.
        let recovered: Vec<u64> = self.clients.iter().filter(|(_, c)| c.lagging && c.tx.capacity() > QUEUE / 2).map(|(id, _)| *id).collect();
        for id in recovered {
            if let Some(c) = self.clients.get_mut(&id) {
                c.lagging = false;
            }
            let chats: Vec<String> = self.clients[&id].attached.iter().cloned().collect();
            for chat in chats {
                self.send_whole(id, &chat);
            }
        }
        let ids: Vec<String> = self.chats.keys().cloned().collect();
        for id in ids {
            let watchers: Vec<u64> = self.clients.iter().filter(|(_, c)| c.attached.contains(&id)).map(|(k, _)| *k).collect();
            let slot = self.chats.get_mut(&id).unwrap();
            let splice = slot.chat.take_splice();
            let state = slot.chat.state();
            let state = (slot.sent_state.as_ref() != Some(&state)).then(|| {
                slot.sent_state = Some(state.clone());
                state
            });
            let notices = std::mem::take(&mut slot.notices);
            if watchers.is_empty() {
                continue;
            }
            let mut frames = Vec::new();
            if let Some(splice) = splice {
                frames.push(wire::encode(tag::EVENT, &Event::Splice { chat: id.clone(), splice }));
            }
            if let Some(state) = state {
                frames.push(wire::encode(tag::EVENT, &Event::State { chat: id.clone(), state }));
            }
            for (level, text) in notices {
                frames.push(wire::encode(tag::EVENT, &Event::Notice { chat: id.clone(), level: notice_level(level), text }));
            }
            for w in watchers {
                for f in &frames {
                    self.send_event(w, f.clone());
                }
            }
        }
    }

    fn send_event(&mut self, client: u64, frame: Vec<u8>) {
        let Some(c) = self.clients.get_mut(&client) else { return };
        if c.lagging {
            return;
        }
        match c.tx.try_send(frame) {
            Ok(()) => {}
            Err(tmpsc::error::TrySendError::Full(_)) => c.lagging = true,
            Err(tmpsc::error::TrySendError::Closed(_)) => {
                self.clients.remove(&client);
            }
        }
    }

    /// A chat's whole transcript and state, to one client.
    fn send_whole(&mut self, client: u64, id: &str) {
        let Some(slot) = self.chats.get(id) else { return };
        let frames = [
            wire::encode(tag::EVENT, &Event::Splice { chat: id.to_string(), splice: slot.chat.full_splice() }),
            wire::encode(tag::EVENT, &Event::State { chat: id.to_string(), state: slot.chat.state() }),
        ];
        for f in frames {
            self.send_event(client, f);
        }
    }

    fn reply(&mut self, client: u64, id: u64, response: Response) {
        let Some(c) = self.clients.get_mut(&client) else { return };
        // A reply can't be dropped: a client too far behind to take one is disconnected, and
        // starts afresh when it reconnects.
        if c.tx.try_send(wire::encode(tag::REPLY, &Reply { id, response })).is_err() {
            self.clients.remove(&client);
        }
    }

    /// Answer a call; true when it asked the node to stop.
    fn call(&mut self, client: u64, body: &[u8]) -> bool {
        let call: Call = match serde_json::from_slice(body) {
            Ok(c) => c,
            Err(e) => {
                let id = serde_json::from_slice::<Value>(body).ok().and_then(|v| v["id"].as_u64()).unwrap_or(0);
                let message = format!("serpis-node {} (protocol {}) can't read that call: {e}", crate::VERSION, wire::VERSION);
                self.reply(client, id, Response::Error { message });
                return false;
            }
        };
        let stopping = matches!(call.request, Request::Stop | Request::Restart);
        // Packing and unpacking take a while: done elsewhere, answered when done.
        if let Request::Pack { .. } | Request::Unpack { .. } = &call.request {
            self.answer_later(client, call.id, call.request);
            return false;
        }
        let response = match self.answer(client, call.request) {
            Ok(r) => r,
            Err(message) => Response::Error { message },
        };
        let stop = stopping && response == Response::Ok;
        self.reply(client, call.id, response);
        stop
    }

    fn answer(&mut self, client: u64, request: Request) -> Result<Response, String> {
        Ok(match request {
            Request::Hello { version, .. } => {
                if version != wire::VERSION {
                    return Err(format!("this node speaks protocol {}, the client {version}: update the older one", wire::VERSION));
                }
                Response::Hello {
                    version: wire::VERSION,
                    host: nix::unistd::gethostname().map(|h| h.to_string_lossy().into_owned()).unwrap_or_default(),
                    serpis: crate::VERSION.to_string(),
                    home: dirs::home_dir().unwrap_or_default().to_string_lossy().into_owned(),
                    setup: self.setup(),
                    build: build_id().to_string(),
                }
            }
            Request::List => Response::Chats { chats: self.chats.iter().map(|(id, s)| ChatSummary { chat: id.clone(), state: s.chat.state() }).collect() },
            Request::New { cwd, model, thinking } => {
                let cwd = from_home(&cwd);
                if !cwd.is_dir() {
                    return Err(format!("{} isn't a folder on this machine", cwd.display()));
                }
                let model = match model {
                    Some(spec) => Some(self.find_model(&spec)?),
                    None => serpis_chat::default_model(&self.registry, self.settings.vars.get("agent-model").and_then(Value::as_str)),
                };
                let thinking = thinking
                    .or_else(|| self.settings.vars.get("agent-thinking").and_then(Value::as_str).map(str::to_string))
                    .and_then(|t| serpis_ai::ThinkingLevel::parse(&t))
                    .unwrap_or(serpis_ai::ThinkingLevel::Medium);
                let chat = Chat::new(ChatInit { cwd, model, thinking }, self.wake.clone());
                let id = chat.session_id.clone();
                self.chats.insert(id.clone(), Slot { chat, sent_state: None, notices: Vec::new() });
                Response::Chat { chat: id }
            }
            Request::Resume { session } => Response::Chat { chat: self.resume(&from_home(&session))? },
            Request::Attach { chat } => {
                if !self.chats.contains_key(&chat) {
                    return Err(format!("no chat {chat} on this node"));
                }
                // Others are sent what changed first, so the splices after this one apply on top
                // of it for everyone.
                self.flush();
                let c = self.clients.get_mut(&client).ok_or("gone")?;
                c.attached.insert(chat.clone());
                self.send_whole(client, &chat);
                Response::Ok
            }
            Request::Detach { chat } => {
                if let Some(c) = self.clients.get_mut(&client) {
                    c.attached.remove(&chat);
                }
                Response::Ok
            }
            Request::Prompt { chat, text, images, follow_up, request_id } => {
                self.with_chat(&chat, |c, h| c.submit(h, text, images, follow_up, request_id.as_deref()))??;
                Response::Ok
            }
            Request::Abort { chat } => {
                self.with_chat(&chat, |c, _| c.abort())?;
                Response::Ok
            }
            Request::Compact { chat, instructions } => {
                self.with_chat(&chat, |c, h| c.compact(h, serpis_chat::CompactReason::Manual, instructions))?;
                Response::Ok
            }
            Request::SetModel { chat, model } => {
                let m = self.find_model(&model)?;
                self.with_chat(&chat, |c, _| c.set_model(m))?;
                Response::Ok
            }
            Request::SetThinking { chat, level } => {
                let l = serpis_ai::ThinkingLevel::parse(&level).ok_or_else(|| format!("unknown thinking level {level}"))?;
                self.with_chat(&chat, |c, _| c.set_thinking(l))?;
                Response::Ok
            }
            Request::Close { chat } => {
                let mut slot = self.chats.remove(&chat).ok_or_else(|| format!("no chat {chat} on this node"))?;
                slot.chat.abort();
                let frame = wire::encode(tag::EVENT, &Event::Closed { chat: chat.clone() });
                let watchers: Vec<u64> = self.clients.iter_mut().filter_map(|(k, c)| c.attached.remove(&chat).then_some(*k)).collect();
                for w in watchers {
                    self.send_event(w, frame.clone());
                }
                Response::Ok
            }
            Request::Models => {
                let configured = self.registry.configured_providers();
                let models = self
                    .registry
                    .models()
                    .into_iter()
                    .filter(|m| configured.contains(&m.provider))
                    .map(|m| ModelInfo { model: format!("{}/{}", m.provider, m.id), name: m.name.clone(), reasoning: m.reasoning, images: m.input.iter().any(|i| i == "image") })
                    .collect();
                Response::Models { models }
            }
            Request::Sessions { limit } => {
                let mut sessions: Vec<SessionInfo> = serpis_chat::session::list(&[self.session_root()], &mut self.session_cache)
                    .into_iter()
                    .map(|i| SessionInfo {
                        title: i.title(),
                        path: i.path.to_string_lossy().into_owned(),
                        id: i.id,
                        cwd: i.cwd.to_string_lossy().into_owned(),
                        modified: i.modified.and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0),
                        messages: i.messages,
                        moved_to: i.moved_to,
                    })
                    .collect();
                sessions.sort_by(|a, b| b.modified.cmp(&a.modified));
                sessions.truncate(limit.unwrap_or(usize::MAX));
                Response::Sessions { sessions }
            }
            Request::Export { chat } => {
                let slot = self.chats.get(&chat).ok_or_else(|| format!("no chat {chat} on this node"))?;
                if slot.chat.running() || slot.chat.compacting() {
                    return Err("the chat is mid-turn: stop it or wait for it to finish first".into());
                }
                let path = slot.chat.session.as_ref().filter(|w| w.exists()).map(|w| w.path.clone()).ok_or("the chat has no saved session (session-save is off, or nothing was said yet)")?;
                Response::Session { text: std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))? }
            }
            Request::Import { session, cwd } => {
                let cwd = from_home(&cwd);
                if !cwd.is_dir() {
                    return Err(format!("{} isn't a folder on this machine", cwd.display()));
                }
                let (id, _) = serpis_chat::session::rehome(&session, &cwd).map_err(|e| format!("{e:#}"))?;
                // An older copy of it open here gives way.
                if let Some(mut old) = self.chats.remove(&id) {
                    old.chat.abort();
                }
                let path = serpis_chat::session::import(&self.session_root(), &session, &cwd).map_err(|e| format!("{e:#}"))?;
                let chat = self.resume(&path)?;
                // Its instructions are this folder's now (AGENTS.md, the cwd it names).
                self.with_chat(&chat, |c, _| c.refresh_context())?;
                Response::Chat { chat }
            }
            Request::Moved { chat, to } => {
                self.with_chat(&chat, |c, h| c.append_entry(h, serpis_chat::session::MOVED_ENTRY, json!({"to": to})))?;
                self.answer(client, Request::Close { chat })?
            }
            Request::Fetch { name, offset, len } => {
                use std::io::{Read, Seek};
                let path = self.transfer_file(&name)?;
                let mut f = std::fs::File::open(&path).map_err(|e| format!("{name}: {e}"))?;
                f.seek(std::io::SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
                let mut buf = Vec::new();
                f.take(len.min(8 << 20)).read_to_end(&mut buf).map_err(|e| e.to_string())?;
                Response::Chunk { data: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, buf) }
            }
            Request::Put { name, offset, data } => {
                use std::io::{Seek, Write};
                let path = self.transfer_file(&name)?;
                let bytes = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, data).map_err(|e| e.to_string())?;
                let mut f = std::fs::OpenOptions::new().create(true).write(true).truncate(offset == 0).open(&path).map_err(|e| format!("{name}: {e}"))?;
                f.seek(std::io::SeekFrom::Start(offset)).map_err(|e| e.to_string())?;
                f.write_all(&bytes).map_err(|e| e.to_string())?;
                Response::Ok
            }
            Request::Drop { name } => {
                let _ = std::fs::remove_file(self.transfer_file(&name)?);
                Response::Ok
            }
            Request::Pack { .. } | Request::Unpack { .. } => unreachable!("answered later"),
            Request::Stop => Response::Ok,
            Request::Restart => {
                let busy: Vec<String> = self.chats.values().filter(|s| s.chat.running() || s.chat.compacting()).map(|s| s.chat.title.clone()).collect();
                if !busy.is_empty() {
                    return Err(format!("busy: {} chat(s) mid-turn ({})", busy.len(), busy.join(", ")));
                }
                Response::Ok
            }
            Request::Setup { auth, models, settings } => Response::Setup { added: self.apply_setup(auth, models, settings)? },
            Request::Dirs { path } => {
                let dir = from_home(&path);
                let mut dirs: Vec<String> = std::fs::read_dir(&dir)
                    .map_err(|e| format!("{}: {e}", dir.display()))?
                    .flatten()
                    .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .filter(|n| !n.starts_with('.'))
                    .collect();
                dirs.sort();
                Response::Dirs { path: dir.to_string_lossy().into_owned(), dirs }
            }
        })
    }

    /// A file in the transfer directory, by a name a client gave: only plain names (no paths),
    /// so no request reaches outside it.
    fn transfer_file(&self, name: &str) -> Result<PathBuf, String> {
        if name.is_empty() || name.starts_with('.') || !name.chars().all(|c| c.is_ascii_alphanumeric() || "._-".contains(c)) {
            return Err(format!("not a transfer name: {name}"));
        }
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&self.transfer).map_err(|e| e.to_string())?;
        Ok(self.transfer.join(name))
    }

    /// Pack or unpack on another thread, and answer when done.
    fn answer_later(&mut self, client: u64, id: u64, request: Request) {
        let (msgs, transfer) = (self.msgs.clone(), self.transfer.clone());
        let file = match &request {
            Request::Unpack { name, .. } => self.transfer_file(name).map(Some),
            _ => self.transfer_file("x").map(|_| None),
        };
        self.rt.spawn_blocking(move || {
            let response = (|| -> Result<Response, String> {
                match request {
                    Request::Pack { dir } => {
                        let dir = from_home(&dir);
                        let home = dirs::home_dir().unwrap_or_default();
                        let (path, packed) = crate::workspace::pack(&dir, &transfer, &home).map_err(|e| format!("{e:#}"))?;
                        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                        Ok(Response::Packed { name, size, packed: serde_json::to_value(packed).map_err(|e| e.to_string())? })
                    }
                    Request::Unpack { packed, places, .. } => {
                        let file = file?.ok_or("no file")?;
                        let packed: crate::workspace::Packed = serde_json::from_value(packed).map_err(|e| e.to_string())?;
                        let places: crate::workspace::Places = places.and_then(|p| serde_json::from_value(p).ok()).unwrap_or_default();
                        let placed = crate::workspace::place(&file, &packed, &places, &dirs::home_dir().unwrap_or_default()).map_err(|e| format!("{e:#}"));
                        let _ = std::fs::remove_file(&file);
                        let placed = placed?;
                        Ok(Response::Unpacked { cwd: placed.cwd.to_string_lossy().into_owned(), note: placed.note })
                    }
                    _ => Err("not answered later".into()),
                }
            })()
            .unwrap_or_else(|message| Response::Error { message });
            let _ = msgs.send(Msg::Reply { client, id, response });
        });
    }

    /// What this machine has for chats: the providers it has credentials for, and from where.
    fn setup(&self) -> wire::NodeSetup {
        let home = dirs::home_dir().unwrap_or_default();
        let short = |p: &Path| p.strip_prefix(&home).map(|r| format!("~/{}", r.display())).unwrap_or_else(|_| p.display().to_string());
        let mut sources = Vec::new();
        for f in [self.registry.auth_file_path(), self.registry.models_file_path()] {
            if f.exists() && let Some(d) = f.parent().map(short) && !sources.contains(&d) {
                sources.push(d);
            }
        }
        wire::NodeSetup { providers: self.registry.configured_providers(), sources }
    }

    /// Add what a client sent and this machine lacks: credentials of providers it has none for,
    /// models.json providers it doesn't define, settings it doesn't set. Files that hold keys are
    /// the user's alone. Starts from pi's files where those are the ones in effect, so they keep
    /// working.
    fn apply_setup(&mut self, auth: serde_json::Map<String, Value>, models: Option<Value>, settings: serde_json::Map<String, Value>) -> Result<Vec<String>, String> {
        use std::os::unix::fs::PermissionsExt;
        let paths = self.registry.paths().clone();
        let write = |path: &Path, v: &Value| -> Result<(), String> {
            let dir = path.parent().ok_or("no directory")?;
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
            let tmp = path.with_extension("json.part");
            std::fs::write(&tmp, serde_json::to_string_pretty(v).unwrap_or_default()).map_err(|e| format!("{}: {e}", tmp.display()))?;
            std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
            std::fs::rename(&tmp, path).map_err(|e| format!("{}: {e}", path.display()))
        };
        let read = |path: &Path| std::fs::read_to_string(path).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).unwrap_or_else(|| json!({}));
        let mut added = Vec::new();
        if !auth.is_empty() {
            let mut have = read(&self.registry.auth_file_path());
            let configured = self.registry.configured_providers();
            for (provider, cred) in auth {
                if have.get(&provider).is_none() && !configured.contains(&provider) {
                    have[&provider] = cred;
                    added.push(format!("auth:{provider}"));
                }
            }
            if added.iter().any(|a| a.starts_with("auth:")) {
                write(&paths.auth_json(), &have)?;
            }
        }
        if let Some(Value::Object(providers)) = models.as_ref().and_then(|m| m.get("providers")).cloned() {
            let mut have = read(&self.registry.models_file_path());
            let n = added.len();
            for (id, p) in providers {
                if have.pointer(&format!("/providers/{id}")).is_none() {
                    if have.get("providers").is_none_or(|v| !v.is_object()) {
                        have["providers"] = json!({});
                    }
                    have["providers"][&id] = p;
                    added.push(format!("models:{id}"));
                }
            }
            if added.len() > n {
                write(&paths.models_json(), &have)?;
            }
        }
        let n = added.len();
        for (k, v) in settings {
            if !self.settings.vars.contains_key(&k) {
                self.settings.vars.insert(k.clone(), v);
                added.push(format!("settings:{k}"));
            }
        }
        if added.len() > n {
            write(&self.settings.path.clone(), &Value::Object(self.settings.vars.clone()))?;
        }
        self.registry.reload();
        Ok(added)
    }

    fn find_model(&self, spec: &str) -> Result<serpis_ai::Model, String> {
        let (p, id) = spec.split_once('/').ok_or_else(|| format!("{spec}: a model is provider/id"))?;
        self.registry.find(p, id).ok_or_else(|| format!("this node has no model {spec}"))
    }

    fn session_root(&self) -> PathBuf {
        let host = NodeHost { settings: &self.settings, registry: &self.registry, rt: &self.rt, notices: &mut Vec::new(), chat: "" };
        host.session_root()
    }

    /// The running chat with session `path`, else a new one continuing it.
    fn resume(&mut self, path: &Path) -> Result<String, String> {
        if let Some((id, _)) = self.chats.iter().find(|(_, s)| s.chat.session.as_ref().is_some_and(|w| w.path == path)) {
            return Ok(id.clone());
        }
        let loaded = serpis_chat::session::load(path).map_err(|e| format!("{}: {e:#}", path.display()))?;
        let chat = Chat::new(ChatInit { cwd: loaded.cwd.clone(), model: None, thinking: serpis_ai::ThinkingLevel::Medium }, self.wake.clone());
        let id = if loaded.id.is_empty() { chat.session_id.clone() } else { loaded.id.clone() };
        if self.chats.contains_key(&id) {
            return Err(format!("chat {id} is already open from another file"));
        }
        self.chats.insert(id.clone(), Slot { chat, sent_state: None, notices: Vec::new() });
        self.with_chat(&id, |c, h| c.restore(h, &loaded))?;
        Ok(id)
    }

    /// Reopen the chats that were open when the node last stopped.
    fn reopen(&mut self) {
        let paths: Vec<String> = std::fs::read_to_string(&self.open_file).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default();
        for p in &paths {
            let p = PathBuf::from(p);
            if p.exists() {
                if let Err(e) = self.resume(&p) {
                    tracing::warn!("reopening {}: {e}", p.display());
                }
            }
        }
        self.open_written = paths;
    }

    /// Note the open chats' session files, for [`Node::reopen`].
    fn remember_open(&mut self) {
        let open: Vec<String> = self.chats.values().filter_map(|s| s.chat.session.as_ref().filter(|w| w.exists())).map(|w| w.path.to_string_lossy().into_owned()).collect();
        if open != self.open_written {
            if let Err(e) = std::fs::write(&self.open_file, serde_json::to_string(&open).unwrap_or_default()) {
                tracing::warn!("{}: {e}", self.open_file.display());
            }
            self.open_written = open;
        }
    }
}

/// This node's build: the SHA-256 of its executable (`SERPIS_NODE_BUILD` overrides it, for
/// tests of upgrades; set empty, the node says none, as nodes before build ids did).
pub fn build_id() -> &'static str {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| std::env::var("SERPIS_NODE_BUILD").unwrap_or_else(|_| std::env::current_exe().ok().and_then(|e| crate::deploy::sha256_file(&e).ok()).unwrap_or_default()))
}

fn notice_level(l: Level) -> NoticeLevel {
    match l {
        Level::Info => NoticeLevel::Info,
        Level::Warn => NoticeLevel::Warn,
        Level::Error => NoticeLevel::Error,
    }
}

/// A path a client named: `~/x` and relative paths start from this machine's home.
fn from_home(p: &str) -> PathBuf {
    let p = serpis_chat::expand_home(p);
    if p.is_absolute() { p } else { dirs::home_dir().unwrap_or_default().join(p) }
}
