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
use serpis_proto::wire::{self, Call, ChatSummary, Event, ModelInfo, NoticeLevel, Reply, Request, Response, SessionInfo, ToolSpec, tag};
use serpis_proto::ChatState;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc as tmpsc;

use crate::lua::{Action, Info, NodeLua};

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
    /// The Lua config (init.lua, extensions): the window's, `~/.config/serpis`, unless set.
    pub config_dir: Option<PathBuf>,
}

impl Config {
    pub fn default_paths() -> Config {
        Config { socket: crate::socket_path(), dir: crate::node_dir(), agent_dir: None, config_dir: None }
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
    /// Work for the node's thread from elsewhere (tool calls and their hooks, from agents).
    Run(Box<dyn FnOnce(&mut Node) + Send>),
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
    // Its build is the binary it started from: hashed now, before an update replaces the file.
    let _ = build_id();
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
    let (wake_tx, msgs) = (tx.clone(), tx.clone());
    let socket = config.socket.clone();
    let config_dir = config.config_dir.clone().unwrap_or_else(serpis_chat::config::config_dir);
    let rt2 = rt.clone();
    // Lua lives on the node's thread, so the node is made there.
    let thread = std::thread::Builder::new().name("serpis-node".into()).spawn(move || {
        let settings = Settings::load(config.dir.join("settings.json"));
        let lua = NodeLua::load(&config_dir, registry.clone(), settings.vars.clone());
        let mut node = Node {
            msgs,
            transfer: config.dir.join("transfer"),
            chats: BTreeMap::new(),
            clients: HashMap::new(),
            settings,
            registry,
            rt: rt2,
            wake: Arc::new(move || {
                let _ = wake_tx.send(Msg::Wake);
            }),
            open_file: config.dir.join("open.json"),
            open_written: Vec::new(),
            session_cache: HashMap::new(),
            lua,
            config_dir,
            tool_calls: HashMap::new(),
            next_call: 0,
            pairings: HashMap::new(),
        };
        // Who holds the socket (for whoever has to stop a node that no longer answers).
        let pid_file = node.open_file.with_file_name("node.pid");
        let _ = std::fs::write(&pid_file, std::process::id().to_string());
        node.reopen();
        node.run(rx);
        let _ = std::fs::remove_file(&socket);
        let _ = std::fs::remove_file(&pid_file);
    })?;
    rt.spawn(accept(listener, tx.clone()));
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
    // Terminals' bytes queue apart, and the writer takes everything else first: an answer or a
    // chat's news never waits behind a terminal printing.
    let (dtx, mut drx) = tmpsc::channel::<Vec<u8>>(TERMINAL_QUEUE);
    if tx.send(Msg::Connected { client, tx: otx.clone() }).is_err() {
        return;
    }
    let writer = tokio::spawn(async move {
        loop {
            let bytes = tokio::select! {
                biased;
                b = orx.recv() => b,
                b = drx.recv() => b,
            };
            let Some(bytes) = bytes else { break };
            if wr.write_all(&bytes).await.is_err() {
                break;
            }
        }
    });
    // The client's link to the terminal host, once it uses a terminal: its terminal calls and
    // bytes go there from here, without the node's thread.
    let mut terminals: Option<tmpsc::UnboundedSender<Vec<u8>>> = None;
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
                Ok(Some((tag::CALL, body))) => match serde_json::from_slice::<Call>(&body) {
                    Ok(mut call) if is_terminal(&call.request) => {
                        if let Request::NewTerminal { cwd, .. } = &mut call.request {
                            *cwd = from_home(cwd).to_string_lossy().into_owned();
                        }
                        let link = terminals.get_or_insert_with(|| terminal_link(otx.clone(), dtx.clone()));
                        let _ = link.send(wire::encode(tag::CALL, &call));
                    }
                    _ => {
                        if tx.send(Msg::Call { client, body }).is_err() {
                            break 'read;
                        }
                    }
                },
                Ok(Some((tag::TERMINAL, body))) => {
                    if let Some(link) = &terminals {
                        let _ = link.send(wire::frame(tag::TERMINAL, &body));
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
    drop(terminals);
    let _ = writer.await;
}

/// Frames of terminal bytes queued for a client; past that, a terminal's bytes are skipped
/// until there's room, and its screen is painted afresh (a slow link sees the screen as it is,
/// not a backlog).
const TERMINAL_QUEUE: usize = 256;

fn is_terminal(r: &Request) -> bool {
    matches!(
        r,
        Request::Terminals | Request::NewTerminal { .. } | Request::KillTerminal { .. } | Request::AttachTerminal { .. } | Request::DetachTerminal { .. } | Request::ResizeTerminal { .. } | Request::RepaintTerminal { .. }
    )
}

/// A link from a client to the terminal host (started if none runs): what is sent on it goes
/// there; what comes back goes to the client, answers and events on `control`, terminals' bytes
/// on `data`.
fn terminal_link(control: tmpsc::Sender<Vec<u8>>, data: tmpsc::Sender<Vec<u8>>) -> tmpsc::UnboundedSender<Vec<u8>> {
    let (link, mut to_host) = tmpsc::unbounded_channel::<Vec<u8>>();
    // Weak: the link ends when the client lets go of it.
    let again = link.downgrade();
    tokio::spawn(async move {
        let socket = crate::terminals::socket();
        let reached = async {
            crate::terminals::ensure(&std::env::current_exe()?, &socket).await?;
            Ok::<_, anyhow::Error>(tokio::net::UnixStream::connect(&socket).await?)
        };
        let stream = match reached.await {
            Ok(s) => s,
            Err(e) => {
                // Its calls are answered with why.
                let message = format!("this machine's terminals can't be reached: {e:#}");
                while let Some(f) = to_host.recv().await {
                    if f.first() == Some(&tag::CALL)
                        && let Ok(call) = serde_json::from_slice::<Call>(&f[5..])
                    {
                        let _ = control.send(wire::encode(tag::REPLY, &Reply { id: call.id, response: Response::Error { message: message.clone() } })).await;
                    }
                }
                return;
            }
        };
        let (mut rd, mut wr) = stream.into_split();
        tokio::spawn(async move {
            while let Some(f) = to_host.recv().await {
                if wr.write_all(&f).await.is_err() {
                    break;
                }
            }
        });
        // Terminals whose bytes were skipped, to paint afresh when there's room.
        let mut behind: std::collections::HashSet<String> = Default::default();
        let mut decoder = wire::Decoder::default();
        let mut buf = vec![0u8; 64 * 1024];
        'read: loop {
            // Behind on a terminal, it is checked now and then whether there's room again, even
            // if the terminal has gone quiet.
            let read = tokio::select! {
                r = rd.read(&mut buf) => Some(r),
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)), if !behind.is_empty() => None,
            };
            match read {
                Some(Ok(0) | Err(_)) => break,
                Some(Ok(n)) => decoder.push(&buf[..n]),
                None => {}
            }
            while let Ok(Some((t, body))) = decoder.next() {
                match t {
                    tag::TERMINAL => {
                        let Some((name, _)) = wire::parse_terminal(&body) else { continue };
                        if behind.contains(&name) {
                            continue;
                        }
                        match data.try_send(wire::frame(t, &body)) {
                            Ok(()) => {}
                            Err(tmpsc::error::TrySendError::Full(_)) => {
                                behind.insert(name);
                            }
                            Err(tmpsc::error::TrySendError::Closed(_)) => break 'read,
                        }
                    }
                    // The answers to this link's own repaint asks (id 0) are the link's.
                    tag::REPLY if serde_json::from_slice::<Reply>(&body).is_ok_and(|r| r.id == 0) => {}
                    _ => {
                        if control.send(wire::frame(t, &body)).await.is_err() {
                            break 'read;
                        }
                    }
                }
            }
            if !behind.is_empty() && data.capacity() >= TERMINAL_QUEUE / 2 {
                let Some(link) = again.upgrade() else { break };
                for name in behind.drain() {
                    let _ = link.send(wire::encode(tag::CALL, &Call { id: 0, request: Request::RepaintTerminal { name } }));
                }
            }
        }
    });
    link
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

    /// Read the file again if it changed: whether it did.
    fn refresh(&mut self) -> bool {
        if self.checked.elapsed() < Duration::from_secs(1) {
            return false;
        }
        self.checked = Instant::now();
        let changed = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok() != self.modified;
        if changed {
            self.reload();
        }
        changed
    }
}

struct Slot {
    chat: Chat,
    /// The state clients were last sent.
    sent_state: Option<ChatState>,
    notices: Vec<(Level, String)>,
    /// Its last view closed while it worked: it closes once it is done, unless shown again.
    close_when_idle: bool,
    /// Times in a row its turn was cut off by the node stopping and continued (0 once a turn
    /// ends on its own).
    cut: u32,
}

/// A chat open when the node last noted them ([`Node::reopen`]): its session file, whether it
/// was mid-turn, and how many times in a row its turn had already been continued.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
struct OpenChat {
    path: String,
    #[serde(default)]
    running: bool,
    #[serde(default)]
    cut: u32,
}

/// What `open.json` held: chats, or (from older nodes) their paths.
#[derive(serde::Deserialize)]
#[serde(untagged)]
enum Opened {
    Chat(OpenChat),
    Path(String),
}

struct Client {
    tx: tmpsc::Sender<Vec<u8>>,
    attached: BTreeSet<String>,
    /// Its queue filled: it is sent nothing until it has room, then everything afresh.
    lagging: bool,
    /// Tools it runs for the chats it shows (`Lend`).
    lends: Vec<ToolSpec>,
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
    open_written: Vec<OpenChat>,
    session_cache: HashMap<PathBuf, serpis_chat::session::Info>,
    /// The config's chat side: hooks, tools, settings.
    lua: NodeLua,
    config_dir: PathBuf,
    /// Calls of lent tools waiting for their client: by call id, (client, where the answer goes).
    tool_calls: HashMap<u64, (u64, tokio::sync::oneshot::Sender<serpis_agent::ToolResult>)>,
    next_call: u64,
    /// Phones being paired here, by pairing id.
    pairings: HashMap<String, Pairing>,
}

/// A phone being paired: until when its pairing key works, and the `serpis-node pair` waiting
/// for the desktop's answer (client, call id).
struct Pairing {
    until: Instant,
    waiting: Option<(u64, u64)>,
}

/// What a chat on the node gets from it.
struct NodeHost<'a> {
    registry: &'a serpis_ai::Registry,
    rt: &'a tokio::runtime::Handle,
    notices: &'a mut Vec<(Level, String)>,
    chat: &'a str,
    lua: &'a NodeLua,
    msgs: &'a mpsc::Sender<Msg>,
    /// Tools lent by the clients showing this chat: (client, tool).
    lent: Vec<(u64, ToolSpec)>,
}

impl Host for NodeHost<'_> {
    fn var(&self, name: &str) -> Option<Value> {
        self.lua.var(name)
    }
    fn hook(&mut self, chat: &Chat, name: &str, payload: &Value) -> Vec<Value> {
        // Handlers read the chat as it is now (serpis.chat.info).
        self.lua.state.borrow_mut().chats.insert(self.chat.to_string(), Info::of(self.chat, chat));
        self.lua.run_hook_for(Some(self.chat), name, payload)
    }
    fn notify(&mut self, level: Level, text: String) {
        self.notices.push((level, text));
    }
    fn tools(&self) -> Vec<serpis_agent::ToolRef> {
        // Built in, then the config's (which replace built-ins of the same name), then the ones
        // a client lends that the node has no tool of that name for.
        let mut tools = serpis_agent::builtin_tools();
        for spec in self.lua.tool_specs() {
            tools.retain(|t| t.name() != spec.name);
            tools.push(Arc::new(crate::lua::LuaTool { spec, chat: self.chat.to_string(), post: post(self.msgs) }));
        }
        for (client, spec) in &self.lent {
            if !tools.iter().any(|t| t.name() == spec.name) {
                tools.push(Arc::new(LentTool { spec: spec.clone(), client: *client, chat: self.chat.to_string(), msgs: self.msgs.clone() }));
            }
        }
        let disabled: Vec<String> = self.var("agent-disabled-tools").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
        tools.retain(|t| !disabled.iter().any(|d| d == t.name()));
        tools
    }
    fn tool_hooks(&self) -> Option<Arc<dyn serpis_agent::ToolHooks>> {
        Some(Arc::new(NodeToolHooks { msgs: self.msgs.clone(), chat: self.chat.to_string() }))
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

/// How the node's Lua is reached from an agent's task.
fn post(msgs: &mpsc::Sender<Msg>) -> crate::lua::Post {
    let msgs = msgs.clone();
    Arc::new(move |f| {
        let _ = msgs.send(Msg::Run(Box::new(move |node: &mut Node| {
            node.refresh_infos();
            f(&node.lua);
            node.after_lua();
        })));
    })
}

/// `before-tool-call` / `after-tool-call`, run by the node's Lua.
struct NodeToolHooks {
    msgs: mpsc::Sender<Msg>,
    chat: String,
}

impl serpis_agent::ToolHooks for NodeToolHooks {
    fn before_tool_call(&self, name: &str, args: Value) -> futures::future::BoxFuture<'static, serpis_agent::ToolGate> {
        use futures::FutureExt;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (name, a, chat) = (name.to_string(), args.clone(), self.chat.clone());
        let _ = self.msgs.send(Msg::Run(Box::new(move |node: &mut Node| {
            if !node.lua.has_hook("before-tool-call") {
                let _ = tx.send(None);
                return;
            }
            // The response asking for this tool first, so hooks see what it cost.
            let _ = node.with_chat(&chat, |c, h| c.poll(h));
            node.refresh_infos();
            let results = node.lua.run_hook_for(Some(&chat), "before-tool-call", &json!({"buffer": chat, "chat": chat, "name": name, "args": a}));
            node.after_lua();
            let gate = results.into_iter().find_map(|r| match r.get("block").and_then(Value::as_str) {
                Some(reason) => Some(serpis_agent::ToolGate::Block(reason.to_string())),
                None => r.get("args").cloned().map(serpis_agent::ToolGate::Allow),
            });
            let _ = tx.send(gate);
        })));
        async move { rx.await.ok().flatten().unwrap_or(serpis_agent::ToolGate::Allow(args)) }.boxed()
    }

    fn after_tool_call(&self, name: &str, result: serpis_agent::ToolResult) -> futures::future::BoxFuture<'static, serpis_agent::ToolResult> {
        use futures::FutureExt;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (name, text, is_error, chat) = (name.to_string(), result.text_content(), result.is_error, self.chat.clone());
        let _ = self.msgs.send(Msg::Run(Box::new(move |node: &mut Node| {
            if !node.lua.has_hook("after-tool-call") {
                let _ = tx.send(None);
                return;
            }
            node.refresh_infos();
            let results = node.lua.run_hook_for(Some(&chat), "after-tool-call", &json!({"buffer": chat, "chat": chat, "name": name, "text": text, "isError": is_error}));
            node.after_lua();
            let _ = tx.send(results.into_iter().find_map(|r| r.get("text").and_then(Value::as_str).map(str::to_string)));
        })));
        async move {
            match rx.await.ok().flatten() {
                Some(text) => {
                    let mut r = serpis_agent::ToolResult::text(text);
                    r.is_error = result.is_error;
                    r
                }
                None => result,
            }
        }
        .boxed()
    }
}

/// A tool a client lends (`Lend`): run there, through a `ToolCall` event.
struct LentTool {
    spec: ToolSpec,
    client: u64,
    chat: String,
    msgs: mpsc::Sender<Msg>,
}

impl serpis_agent::AgentTool for LentTool {
    fn name(&self) -> &str {
        &self.spec.name
    }
    fn description(&self) -> String {
        self.spec.description.clone()
    }
    fn parameters(&self) -> Value {
        self.spec.parameters.clone()
    }
    fn prompt_snippet(&self) -> Option<String> {
        Some(self.spec.description.lines().next().unwrap_or("").to_string())
    }
    fn execute(&self, _id: String, args: Value, _ctx: serpis_agent::ToolContext) -> futures::future::BoxFuture<'static, anyhow::Result<serpis_agent::ToolResult>> {
        use futures::FutureExt;
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (client, chat, name) = (self.client, self.chat.clone(), self.spec.name.clone());
        let _ = self.msgs.send(Msg::Run(Box::new(move |node: &mut Node| node.call_lent(client, chat, name, args, tx))));
        async move { rx.await.map_err(|_| anyhow::anyhow!("the node stopped")) }.boxed()
    }
}

impl Node {
    fn run(mut self, rx: mpsc::Receiver<Msg>) {
        let mut last = Instant::now();
        let mut next_timer: Option<Instant> = None;
        loop {
            let wait = next_timer.map_or(Duration::from_secs(1), |t| t.saturating_duration_since(Instant::now()).min(Duration::from_secs(1)));
            let msg = match rx.recv_timeout(wait) {
                Ok(m) => Some(m),
                Err(mpsc::RecvTimeoutError::Timeout) => None,
                Err(mpsc::RecvTimeoutError::Disconnected) => break,
            };
            let mut stop = false;
            for m in msg.into_iter().chain(std::iter::from_fn(|| rx.try_recv().ok())) {
                match m {
                    Msg::Connected { client, tx } => {
                        self.clients.insert(client, Client { tx, attached: BTreeSet::new(), lagging: false, lends: Vec::new() });
                    }
                    Msg::Call { client, body } => stop |= self.call(client, &body),
                    Msg::Gone { client } => self.gone(client),
                    Msg::Wake => {}
                    Msg::Reply { client, id, response } => self.reply(client, id, response),
                    Msg::Run(f) => f(&mut self),
                    Msg::Stop => stop = true,
                }
            }
            if self.settings.refresh() {
                self.lua.state.borrow_mut().file = self.settings.vars.clone();
            }
            let keep_going = self.lua.var("chat-keep-going").and_then(|v| v.as_bool()).unwrap_or(true);
            for slot in self.chats.values_mut() {
                slot.chat.keep_going_default = keep_going;
            }
            self.refresh_infos();
            next_timer = self.lua.run_timers();
            self.poll();
            self.after_lua();
            self.close_idle();
            self.flush();
            self.remember_open(false);
            if stop {
                for slot in self.chats.values_mut() {
                    slot.chat.abort();
                }
                // Stopped on purpose: nothing was cut off.
                self.remember_open(true);
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

    /// Run `f` on chat `id` with the node as its host.
    fn with_chat<R>(&mut self, id: &str, f: impl FnOnce(&mut Chat, &mut NodeHost) -> R) -> Result<R, String> {
        let lent: Vec<(u64, ToolSpec)> = self.clients.iter().filter(|(_, c)| c.attached.contains(id)).flat_map(|(k, c)| c.lends.iter().map(|t| (*k, t.clone()))).collect();
        let Node { chats, registry, rt, lua, msgs, .. } = self;
        let slot = chats.get_mut(id).ok_or_else(|| format!("no chat {id} on this node"))?;
        let Slot { chat, notices, .. } = slot;
        let mut host = NodeHost { registry, rt, notices, chat: id, lua, msgs, lent };
        Ok(f(chat, &mut host))
    }

    /// The chats as Lua reads them, before it runs.
    fn refresh_infos(&mut self) {
        let mut s = self.lua.state.borrow_mut();
        s.chats = self.chats.iter().map(|(id, slot)| (id.clone(), Info::of(id, &slot.chat))).collect();
    }

    /// Do what Lua asked (it may ask again, from the hooks that runs), and pass on what it said.
    fn after_lua(&mut self) {
        for _ in 0..8 {
            let actions = std::mem::take(&mut self.lua.state.borrow_mut().actions);
            if actions.is_empty() {
                break;
            }
            for a in actions {
                let r = match a {
                    Action::Prompt { chat, text } => self.with_chat(&chat, |c, h| c.submit(h, text, Vec::new(), false, None)).and_then(|r| r),
                    Action::SetModel { chat, spec } => self.find_model(&spec).and_then(|m| self.with_chat(&chat, |c, _| c.set_model(m))),
                    Action::SetThinking { chat, level } => match serpis_ai::ThinkingLevel::parse(&level) {
                        Some(l) => self.with_chat(&chat, |c, _| c.set_thinking(l)),
                        None => Err(format!("unknown thinking level {level}")),
                    },
                    Action::Append { chat, kind, data } => self.with_chat(&chat, |c, h| c.append_entry(h, &kind, data)),
                    Action::ReloadModels => {
                        self.registry.reload();
                        Ok(())
                    }
                    Action::Reload => {
                        self.reload_config();
                        Ok(())
                    }
                };
                if let Err(e) = r {
                    self.lua.state.borrow_mut().notices.push((None, Level::Error, format!("lua: {e}")));
                }
            }
        }
        let notices = std::mem::take(&mut self.lua.state.borrow_mut().notices);
        for (chat, level, text) in notices {
            match chat.and_then(|c| self.chats.get_mut(&c)) {
                Some(slot) => slot.notices.push((level, text)),
                None => {
                    let frame = wire::encode(tag::EVENT, &Event::Message { level: notice_level(level), text });
                    let all: Vec<u64> = self.clients.keys().copied().collect();
                    for c in all {
                        self.send_event(c, frame.clone());
                    }
                }
            }
        }
    }

    /// Close the chats whose last view closed, once they are idle (their sessions stay).
    fn close_idle(&mut self) {
        self.chats.retain(|_, s| !(s.close_when_idle && !s.chat.running() && !s.chat.compacting()));
    }

    /// Load the Lua config again: what went wrong, if anything.
    fn reload_config(&mut self) -> Vec<String> {
        let dir = self.config_dir.clone();
        self.lua.reload(&dir);
        self.lua.errors.clone()
    }

    /// A client left: its lent tools' calls fail, and the chats it alone showed may close.
    fn gone(&mut self, client: u64) {
        let shown = self.clients.remove(&client).map(|c| c.attached).unwrap_or_default();
        // A phone that gave up pairing: the code can be tried again while it lasts.
        for p in self.pairings.values_mut() {
            if p.waiting.is_some_and(|(c, _)| c == client) {
                p.waiting = None;
            }
        }
        // A chat it showed that never got a message (its session isn't written) and nobody else
        // shows is gone with it.
        for chat in shown {
            let unsaved = self.chats.get(&chat).is_some_and(|s| !s.chat.running() && s.chat.session.as_ref().is_none_or(|w| !w.exists()));
            if unsaved && !self.clients.values().any(|c| c.attached.contains(&chat)) {
                self.chats.remove(&chat);
            }
        }
        let calls: Vec<u64> = self.tool_calls.iter().filter(|(_, (c, _))| *c == client).map(|(k, _)| *k).collect();
        for k in calls {
            if let Some((_, tx)) = self.tool_calls.remove(&k) {
                let mut r = serpis_agent::ToolResult::text("The window running this tool closed before it finished");
                r.is_error = true;
                let _ = tx.send(r);
            }
        }
    }

    /// Ask `client` to run tool `name` it lent, for `chat`.
    fn call_lent(&mut self, client: u64, chat: String, name: String, args: Value, tx: tokio::sync::oneshot::Sender<serpis_agent::ToolResult>) {
        // The client that lent it, or another showing the chat that lends it too.
        let by = std::iter::once(client)
            .chain(self.clients.iter().filter(|(_, c)| c.attached.contains(&chat)).map(|(k, _)| *k))
            .find(|k| self.clients.get(k).is_some_and(|c| c.lends.iter().any(|t| t.name == name)));
        let Some(by) = by else {
            let mut r = serpis_agent::ToolResult::text(format!("{name} runs in a serpis window showing this chat, and none does now"));
            r.is_error = true;
            let _ = tx.send(r);
            return;
        };
        self.next_call += 1;
        let call = self.next_call;
        self.tool_calls.insert(call, (by, tx));
        self.send_event(by, wire::encode(tag::EVENT, &Event::ToolCall { chat, call, name, args }));
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
        tracing::debug!("client {client}: {}", String::from_utf8_lossy(&body[..body.len().min(160)]));
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
        // Installing the service runs commands: done elsewhere too.
        if let Request::InstallService | Request::UninstallService = &call.request {
            self.answer_service(client, call.id, call.request);
            return false;
        }
        // Claude Code's usage: its CLI is asked, which takes a moment.
        if let Request::ClaudeCodeUsage { advice } = call.request {
            self.answer_usage(client, call.id, advice);
            return false;
        }
        // Codex status: app-server reads the account and quotas, which can take a moment.
        if let Request::CodexStatus = call.request {
            self.answer_codex_status(client, call.id);
            return false;
        }
        // A phone proposing its key: answered once the desktop says.
        if let Request::Pair { pairing, key, label } = call.request {
            if let Err(message) = self.pair(client, call.id, pairing, key, label) {
                self.reply(client, call.id, Response::Error { message });
            }
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
                    notes: self.lua.errors.clone(),
                }
            }
            Request::List => Response::Chats { chats: self.chats.iter().map(|(id, s)| ChatSummary { chat: id.clone(), state: s.chat.state() }).collect() },
            Request::New { cwd, model, thinking, chat: id } => {
                if let Some(id) = &id
                    && (uuid::Uuid::parse_str(id).is_err() || self.chats.contains_key(id))
                {
                    return Err(format!("{id} can't be a new chat's id here"));
                }
                let cwd = from_home(&cwd);
                if !cwd.is_dir() {
                    return Err(format!("{} isn't a folder on this machine", cwd.display()));
                }
                let model = match model {
                    Some(spec) => Some(self.find_model(&spec)?),
                    None => serpis_chat::default_model(&self.registry, self.lua.var("agent-model").as_ref().and_then(Value::as_str)),
                };
                let thinking = thinking
                    .or_else(|| self.lua.var("agent-thinking").as_ref().and_then(Value::as_str).map(str::to_string))
                    .and_then(|t| serpis_ai::ThinkingLevel::parse(&t))
                    .unwrap_or(serpis_ai::ThinkingLevel::Medium);
                let mut chat = Chat::new(ChatInit { cwd, model, thinking }, self.wake.clone());
                if let Some(id) = id {
                    chat.session_id = id;
                }
                let id = chat.session_id.clone();
                self.chats.insert(id.clone(), Slot { chat, sent_state: None, notices: Vec::new(), close_when_idle: false, cut: 0 });
                Response::Chat { chat: id }
            }
            Request::Resume { session } => Response::Chat { chat: self.resume(&from_home(&session))? },
            Request::Attach { chat } => {
                let Some(slot) = self.chats.get_mut(&chat) else { return Err(format!("no chat {chat} on this node")) };
                slot.close_when_idle = false;
                // Others are sent what changed first, so the splices after this one apply on top
                // of it for everyone.
                self.flush();
                let c = self.clients.get_mut(&client).ok_or("gone")?;
                c.attached.insert(chat.clone());
                self.send_whole(client, &chat);
                Response::Ok
            }
            Request::Detach { chat, close } => {
                if let Some(c) = self.clients.get_mut(&client) {
                    c.attached.remove(&chat);
                }
                let watched = self.clients.values().any(|c| c.attached.contains(&chat));
                if close && !watched && let Some(slot) = self.chats.get_mut(&chat) {
                    slot.close_when_idle = true;
                }
                self.close_idle();
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
            Request::Answer { chat, tool, decision } => {
                if !self.with_chat(&chat, |c, _| c.answer(&tool, decision))? {
                    return Err("That tool call isn't waiting for an answer (any more)".into());
                }
                Response::Ok
            }
            Request::SetPermissionMode { chat, mode } => {
                self.with_chat(&chat, |c, h| c.set_permission_mode(h, &mode))??;
                Response::Ok
            }
            Request::Skills { chat } => {
                let skills = self.with_chat(&chat, |c, h| c.skills(h))?;
                Response::Skills {
                    skills: skills
                        .into_iter()
                        .map(|s| serpis_proto::wire::SkillInfo { name: s.name, description: s.description, source: s.source, path: s.file.to_string_lossy().into_owned(), explicit_only: s.explicit_only })
                        .collect(),
                }
            }
            Request::Logs { after } => {
                let (records, dropped) = crate::logs::since(after);
                Response::Logs { records, dropped }
            }
            Request::SetContinueAfterLimit { chat, on } => {
                self.with_chat(&chat, |c, h| c.set_continue_after_limit(h, on))??;
                Response::Ok
            }
            Request::BackgroundTask { chat, tool } => {
                if !self.with_chat(&chat, |c, _| c.background_task(&tool))? {
                    return Err("Nothing runs it: the chat's Claude Code has stopped".into());
                }
                Response::Ok
            }
            Request::StopTask { chat, task } => {
                if !self.with_chat(&chat, |c, _| c.stop_task(&task))? {
                    return Err("Nothing runs that task: the chat's Claude Code has stopped".into());
                }
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
            Request::RefreshContext { chat } => {
                self.with_chat(&chat, |c, _| c.refresh_context())?;
                Response::Ok
            }
            Request::Rename { chat, name } => {
                self.with_chat(&chat, |c, h| c.set_session_name(h, &name))?;
                Response::Ok
            }
            Request::AgeCache { chat, secs } => {
                self.with_chat(&chat, |c, _| c.cache.age(Duration::from_secs_f64(secs.max(0.0))))?;
                Response::Ok
            }
            Request::ReloadModels => {
                self.registry.reload();
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
                    .filter(|m| serpis_ai::api::chat_api(&m.api))
                    .map(|m| ModelInfo {
                        model: format!("{}/{}", m.provider, m.id),
                        name: m.display_name().to_string(),
                        reasoning: m.reasoning,
                        images: m.input.iter().any(|i| i == "image"),
                        context_window: m.context_window,
                        cost: (m.cost.input, m.cost.output),
                    })
                    .collect();
                Response::Models { models }
            }
            Request::Sessions { limit } => {
                let open: HashMap<PathBuf, bool> = self.chats.values().filter_map(|s| Some((s.chat.session.as_ref()?.path.clone(), s.chat.running() || s.chat.compacting()))).collect();
                // Its own, and the other directories the desktop lists (pi's, by default), so every
                // device lists the same chats.
                let mut roots = vec![self.session_root()];
                let more = self.lua.var("session-directories").unwrap_or_else(|| json!(["~/.pi/agent/sessions"]));
                for d in more.as_array().into_iter().flatten().filter_map(|d| d.as_str()).map(from_home) {
                    if !roots.contains(&d) {
                        roots.push(d);
                    }
                }
                let mut sessions: Vec<SessionInfo> = serpis_chat::session::list(&roots, &mut self.session_cache)
                    .into_iter()
                    .map(|i| SessionInfo {
                        open: open.contains_key(&i.path),
                        running: open.get(&i.path).copied().unwrap_or(false),
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
            Request::Pack { .. }
            | Request::ClaudeCodeUsage { .. }
            | Request::CodexStatus
            | Request::Unpack { .. }
            | Request::Terminals
            | Request::NewTerminal { .. }
            | Request::KillTerminal { .. }
            | Request::AttachTerminal { .. }
            | Request::DetachTerminal { .. }
            | Request::ResizeTerminal { .. }
            | Request::RepaintTerminal { .. }
            | Request::InstallService
            | Request::UninstallService
            | Request::Pair { .. } => unreachable!("answered elsewhere"),
            Request::StartPairing { pairing: id, key, minutes } => {
                let file = crate::keys::authorized_keys();
                crate::keys::start_pairing(&file, &crate::keys::node_exe(), &id, &key, minutes).map_err(|e| format!("{e:#}"))?;
                self.pairings.insert(id, Pairing { until: Instant::now() + Duration::from_secs(60 * minutes.clamp(1, 60) as u64), waiting: None });
                Response::Ok
            }
            Request::EndPairing { pairing: id } => {
                crate::keys::remove_pairing(&crate::keys::authorized_keys(), Some(&id)).map_err(|e| format!("{e:#}"))?;
                if let Some((c, call)) = self.pairings.remove(&id).and_then(|p| p.waiting) {
                    self.reply(c, call, Response::Paired { approved: false });
                }
                Response::Ok
            }
            Request::PairAnswer { pairing: id, approved } => {
                let waiting = self.pairings.get_mut(&id).and_then(|p| p.waiting.take());
                let Some((c, call)) = waiting else { return Err("no phone is waiting to pair".into()) };
                self.reply(c, call, Response::Paired { approved });
                Response::Ok
            }
            Request::AuthorizeKey { key, comment } => {
                crate::keys::add(&crate::keys::authorized_keys(), "", &key, &comment).map_err(|e| format!("{e:#}"))?;
                Response::Ok
            }
            Request::Stop => Response::Ok,
            Request::Restart => {
                let busy: Vec<String> = self.chats.values().filter(|s| s.chat.running() || s.chat.compacting()).map(|s| s.chat.title.clone()).collect();
                if !busy.is_empty() {
                    return Err(format!("busy: {} chat(s) mid-turn ({})", busy.len(), busy.join(", ")));
                }
                Response::Ok
            }
            Request::Setup { auth, models, settings } => Response::Setup { added: self.apply_setup(auth, models, settings)? },
            Request::SetVars { vars } => {
                let mut s = self.lua.state.borrow_mut();
                for (name, value) in vars {
                    if value.is_null() {
                        s.pushed.remove(&name);
                    } else {
                        s.pushed.insert(name, value);
                    }
                }
                Response::Ok
            }
            Request::Reload => Response::Reloaded { errors: self.reload_config() },
            Request::Append { chat, kind, data } => {
                self.with_chat(&chat, |c, h| c.append_entry(h, &kind, data))?;
                Response::Ok
            }
            Request::Lend { tools } => {
                self.clients.get_mut(&client).ok_or("gone")?.lends = tools;
                Response::Ok
            }
            Request::ToolDone { call, text, is_error } => {
                let (_, tx) = self.tool_calls.remove(&call).ok_or_else(|| format!("no tool call {call} waits"))?;
                let mut r = serpis_agent::ToolResult::text(text);
                r.is_error = is_error;
                let _ = tx.send(r);
                Response::Ok
            }
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

    /// Install or remove the node's service off this thread, and answer when done.
    fn answer_service(&mut self, client: u64, id: u64, request: Request) {
        let msgs = self.msgs.clone();
        self.rt.spawn(async move {
            let response = async {
                Ok::<_, anyhow::Error>(match request {
                    Request::InstallService => {
                        let (exe, node) = (std::env::current_exe()?, crate::socket_path());
                        let r = tokio::task::spawn_blocking(move || crate::service::install(&exe, &node)).await??;
                        Response::Service { installed: true, manager: format!("{:?}", r.installed.manager).to_lowercase(), file: r.installed.file, note: r.note }
                    }
                    Request::UninstallService => {
                        let r = tokio::task::spawn_blocking(crate::service::uninstall).await??;
                        Response::Service { installed: false, manager: r.as_ref().map(|i| format!("{:?}", i.manager).to_lowercase()).unwrap_or_default(), file: r.map(|i| i.file).unwrap_or_default(), note: None }
                    }
                    _ => anyhow::bail!("not a service request"),
                })
            }
            .await
            .unwrap_or_else(|e| Response::Error { message: format!("{e:#}") });
            let _ = msgs.send(Msg::Reply { client, id, response });
        });
    }

    /// Ask this machine's `claude` for its usage, and answer when it does.
    fn answer_usage(&mut self, client: u64, id: u64, advice: bool) {
        let set = self.lua.var("claude-code-path").and_then(|v| v.as_str().filter(|p| !p.is_empty()).map(from_home));
        let (msgs, binary) = (self.msgs.clone(), set.or_else(|| self.registry.claude_code_binary()));
        self.rt.spawn(async move {
            let response = match binary {
                None => Response::Error { message: "Claude Code isn't installed on this machine (claude not found; set claude-code-path)".into() },
                Some(b) => match serpis_ai::claude_code::usage(&b, advice).await {
                    Ok(usage) => Response::ClaudeCodeUsage { usage },
                    Err(e) => Response::Error { message: format!("{e:#}") },
                },
            };
            let _ = msgs.send(Msg::Reply { client, id, response });
        });
    }

    /// Ask this machine's Codex app-server for its account and usage status.
    fn answer_codex_status(&mut self, client: u64, id: u64) {
        let set = self.lua.var("codex-path").and_then(|v| v.as_str().filter(|p| !p.is_empty()).map(from_home));
        let (msgs, binary) = (self.msgs.clone(), set.or_else(|| self.registry.codex_binary()));
        self.rt.spawn(async move {
            let response = match binary {
                None => Response::Error { message: "Codex isn't installed on this machine (codex not found; set codex-path)".into() },
                Some(b) => match serpis_ai::codex::status(&b).await {
                    Ok(status) => Response::CodexStatus { status },
                    Err(e) => Response::Error { message: format!("{e:#}") },
                },
            };
            let _ = msgs.send(Msg::Reply { client, id, response });
        });
    }

    /// A phone proposes `key` for pairing `id` (through `serpis-node pair`): every other client
    /// is asked; the answer comes with `PairAnswer`.
    fn pair(&mut self, client: u64, call: u64, id: String, key: String, label: String) -> Result<(), String> {
        let key = crate::keys::public_key(&key).map_err(|e| e.to_string())?;
        let label: String = label.chars().filter(|c| c.is_alphanumeric() || " -_.'’".contains(*c)).take(40).collect();
        let label = if label.trim().is_empty() { "a phone".to_string() } else { label.trim().to_string() };
        let Some(p) = self.pairings.get_mut(&id).filter(|p| p.until > Instant::now()) else { return Err("this pairing is over: scan a new code".into()) };
        if p.waiting.is_some() {
            return Err("a phone is already pairing with this code".into());
        }
        p.waiting = Some((client, call));
        let frame = wire::encode(tag::EVENT, &Event::PairRequested { pairing: id, key, label });
        let others: Vec<u64> = self.clients.keys().copied().filter(|c| *c != client).collect();
        if others.is_empty() {
            return Err("nobody is there to let the phone in: open serpis on the desktop that showed the code".into());
        }
        for c in others {
            self.send_event(c, frame.clone());
        }
        Ok(())
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
            self.lua.state.borrow_mut().file = self.settings.vars.clone();
        }
        self.registry.reload();
        Ok(added)
    }

    fn find_model(&self, spec: &str) -> Result<serpis_ai::Model, String> {
        let (p, id) = spec.split_once('/').ok_or_else(|| format!("{spec}: a model is provider/id"))?;
        self.registry.find(p, id).ok_or_else(|| format!("this node has no model {spec}"))
    }

    fn session_root(&self) -> PathBuf {
        let host = NodeHost { registry: &self.registry, rt: &self.rt, notices: &mut Vec::new(), chat: "", lua: &self.lua, msgs: &self.msgs, lent: Vec::new() };
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
        self.chats.insert(id.clone(), Slot { chat, sent_state: None, notices: Vec::new(), close_when_idle: false, cut: 0 });
        self.with_chat(&id, |c, h| c.restore(h, &loaded))?;
        Ok(id)
    }

    /// Reopen the chats that were open when the node last stopped. One whose turn the stop cut
    /// off (a crash, a reboot, a kill) continues it, unless it doesn't keep going
    /// (`chat-keep-going`, or its own choice) or was cut off `chat-keep-going-limit` times in a
    /// row already (a turn that keeps stopping the node is parked). Either way it says so.
    fn reopen(&mut self) {
        let text = std::fs::read_to_string(&self.open_file).unwrap_or_default();
        let stopped = std::fs::metadata(&self.open_file).ok().and_then(|m| m.modified().ok()).and_then(|t| t.elapsed().ok());
        let opened: Vec<Opened> = serde_json::from_str(&text).unwrap_or_default();
        let open: Vec<OpenChat> = opened.into_iter().map(|o| match o {
            Opened::Chat(c) => c,
            Opened::Path(path) => OpenChat { path, running: false, cut: 0 },
        }).collect();
        let keep_going = self.lua.var("chat-keep-going").and_then(|v| v.as_bool()).unwrap_or(true);
        let limit = self.lua.var("chat-keep-going-limit").and_then(|v| v.as_u64()).unwrap_or(2) as u32;
        let ago = stopped.map(serpis_chat::fmt_ago).unwrap_or_else(|| "just now".into());
        for o in &open {
            let p = PathBuf::from(&o.path);
            if !p.exists() {
                continue;
            }
            let id = match self.resume(&p) {
                Ok(id) => id,
                Err(e) => {
                    tracing::warn!("reopening {}: {e}", p.display());
                    continue;
                }
            };
            if o.running {
                self.cut_off(&id, o.cut, keep_going, limit, &ago);
            }
        }
        self.open_written = open;
    }

    /// Chat `id`'s turn was cut off by the node stopping (`ago`), `cut` times in a row before.
    fn cut_off(&mut self, id: &str, cut: u32, keep_going: bool, limit: u32, ago: &str) {
        let Some(slot) = self.chats.get_mut(id) else { return };
        slot.chat.keep_going_default = keep_going;
        if !slot.chat.cut_off() {
            return;
        }
        let (keep, parked) = (slot.chat.keep_going(), cut >= limit);
        let continued = keep && !parked && self.with_chat(id, |c, h| c.continue_turn(h)).unwrap_or(false);
        let Some(slot) = self.chats.get_mut(id) else { return };
        let (text, tone) = if continued {
            slot.cut = cut + 1;
            (format!("↻ Continued: the node running this chat stopped mid-turn {ago}, and started again"), serpis_proto::Tone::Meta)
        } else if !keep {
            (format!("⏸ The node running this chat stopped mid-turn {ago}: the chat doesn't keep going on its own, so send a message to continue"), serpis_proto::Tone::Warning)
        } else if parked {
            (format!("⏸ Parked: the node stopped while this chat's turn ran {} times in a row (most recently {ago}), so it isn't continued again: send a message to continue", cut + 1), serpis_proto::Tone::Warning)
        } else {
            (format!("⏸ The node running this chat stopped mid-turn {ago}, and the turn couldn't continue: send a message to continue"), serpis_proto::Tone::Warning)
        };
        tracing::info!("chat {id}: {text}");
        slot.chat.notice(text.clone(), tone);
        let payload = json!({"buffer": id, "continued": continued, "parked": parked && keep, "times": cut + 1, "text": text});
        let chat_info = Info::of(id, &slot.chat);
        self.lua.state.borrow_mut().chats.insert(id.to_string(), chat_info);
        self.lua.run_hook_for(Some(id), "turn-cut-off", &payload);
    }

    /// Note the open chats (their session files, and which are mid-turn), for [`Node::reopen`].
    /// `stopping`: on purpose, so none is noted mid-turn.
    fn remember_open(&mut self, stopping: bool) {
        let mut open = Vec::new();
        for slot in self.chats.values_mut() {
            let running = !stopping && (slot.chat.running() || slot.chat.compacting());
            if !running {
                slot.cut = 0;
            }
            if let Some(w) = slot.chat.session.as_ref().filter(|w| w.exists()) {
                open.push(OpenChat { path: w.path.to_string_lossy().into_owned(), running, cut: slot.cut });
            }
        }
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
