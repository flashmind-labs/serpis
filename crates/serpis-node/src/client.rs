//! Talking to a node: the one on this machine through its socket, or one elsewhere through
//! `ssh <host> serpis-node agent`.
//!
//! A [`Client`] is one stream to one node. Calls are answered in any order (each reply names
//! its call), and events for attached chats arrive on the receiver [`Client::connect`] returns.
//! When the stream ends, pending calls fail and the receiver closes; the caller reconnects.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, anyhow, bail};
use serpis_proto::wire::{self, Call, Event, Reply, Request, Response, tag};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, oneshot};

/// Where a node is.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// The node on this machine, at its socket.
    Local(PathBuf),
    /// A node reached over ssh: a host as ssh knows it (a name from `~/.ssh/config`,
    /// `user@box`, a rayfish name like `box.net.ray`).
    Ssh {
        host: String,
        /// The ssh program and fixed arguments (`["ssh"]`).
        ssh: Vec<String>,
        /// Options before the host.
        ssh_args: Vec<String>,
        /// Variables set for everything run there (`HOME`, `SERPIS_NODE_DIR`, ...).
        env: Vec<(String, String)>,
        /// What runs there instead of the node serpis installs ([`crate::deploy`]).
        command: Option<String>,
        /// The node builds serpis can install there.
        deploy: Vec<crate::deploy::NodeBinary>,
        /// Ask for a build for a system there isn't one for (see [`Client::connect_with`]),
        /// rather than running a serpis-node found there.
        ask: bool,
    },
    /// Any program that speaks the protocol on its stdin and stdout.
    Command(Vec<String>),
    /// This machine's own node, at its socket: started from `exe` when none is running, and
    /// restarted on it when the one running is another build (once its chats are idle).
    Own { socket: PathBuf, exe: PathBuf },
}

impl Target {
    /// `host`'s node over ssh, with options that suit a program with nobody at the terminal:
    /// no password prompts, and dead connections noticed within a minute. It runs a
    /// serpis-node found there until `deploy` is given builds to install.
    pub fn ssh(host: &str) -> Target {
        Target::Ssh {
            host: host.to_string(),
            ssh: vec!["ssh".into()],
            ssh_args: ["-T", "-o", "BatchMode=yes", "-o", "ConnectTimeout=10", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"].map(String::from).to_vec(),
            env: Vec::new(),
            command: None,
            deploy: Vec::new(),
            ask: false,
        }
    }
}

/// `env K=V ... ` in front of a command run over ssh (empty without variables).
fn env_prefix(env: &[(String, String)]) -> String {
    if env.is_empty() {
        return String::new();
    }
    let vars: Vec<String> = env.iter().map(|(k, v)| format!("{k}={}", shell_word(v))).collect();
    format!("env {} ", vars.join(" "))
}

fn shell_word(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+,:@%=~".contains(c)) { s.to_string() } else { format!("'{}'", s.replace('\'', r"'\''")) }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Response>>>>;

/// Gets a node build for a system ([`crate::deploy::PROBE`]) serpis has none for yet: builds or downloads it.
pub type Obtain = Arc<dyn Fn(String) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<crate::deploy::NodeBinary>> + Send>> + Send + Sync>;

/// What a connect is doing, for whoever shows it.
#[derive(Debug, Clone, PartialEq)]
pub enum Stage {
    /// Getting a node build for this system ([`crate::deploy::PROBE`]).
    Building(String),
    /// Copying the node to the machine.
    Installing,
    /// Restarting an older node on the new build.
    Upgrading,
}

pub struct Client {
    out: mpsc::UnboundedSender<Vec<u8>>,
    pending: Pending,
    next: AtomicU64,
    /// What the node said about itself.
    pub hello: Hello,
    /// Keeps the ssh (or other) process alive as long as the client.
    _child: Option<tokio::process::Child>,
    /// What the process said on stderr, once it is done saying it.
    stderr: Option<(Arc<Mutex<String>>, tokio::task::JoinHandle<()>)>,
    /// The runtime it was made on, where `send` waits for answers (it may be called elsewhere).
    rt: tokio::runtime::Handle,
}

#[derive(Debug, Clone, Default)]
pub struct Hello {
    pub host: String,
    pub serpis: String,
    pub home: String,
    /// What the machine has set up for chats (models, credentials).
    pub setup: serpis_proto::wire::NodeSetup,
    /// The node's build (the SHA-256 of its binary).
    pub build: String,
    /// It runs a build this client didn't install, and couldn't restart on the new one yet (a
    /// chat was mid-turn): ask again later ([`Client::restart`]).
    pub outdated: bool,
    /// What went wrong loading the node's Lua config.
    pub notes: Vec<String>,
}

impl Client {
    /// Connect and greet the node, installing it first if the machine asks for it. Must be
    /// called on a tokio runtime.
    pub async fn connect(target: &Target, name: &str) -> Result<(Client, mpsc::UnboundedReceiver<Event>)> {
        Self::connect_with(target, name, None, |_| {}).await
    }

    /// [`Client::connect`], getting a node build with `obtain` for a system serpis has none for,
    /// and telling `stage` what takes time (building, copying: the node is megabytes).
    pub async fn connect_with(target: &Target, name: &str, obtain: Option<Obtain>, stage: impl Fn(Stage)) -> Result<(Client, mpsc::UnboundedReceiver<Event>)> {
        let mut target = target.clone();
        if let Target::Ssh { ask, .. } = &mut target {
            *ask = obtain.is_some();
        }
        // At most: get a build, install it, connect.
        for _ in 0..3 {
            let Failed { stderr, error } = match Self::connect_once(&target, name).await {
                Ok(c) => return Self::upgrade(c, &target, name, &stage).await,
                Err(f) => f,
            };
            let Target::Ssh { host, ssh, ssh_args, env, deploy, ask, .. } = &mut target else { return Err(error) };
            if let Some(node) = crate::deploy::wanted(&stderr).and_then(|h| deploy.iter().find(|n| n.sha256 == h)).cloned() {
                stage(Stage::Installing);
                crate::deploy::upload(ssh, ssh_args, host, &env_prefix(env), &node).await?;
            } else if let (Some(system), Some(obtain)) = (crate::deploy::wanted_system(&stderr), &obtain) {
                stage(Stage::Building(system.to_string()));
                match obtain(system.to_string()).await {
                    Ok(node) => deploy.push(node),
                    Err(e) => {
                        // No build to be had: a serpis-node installed there may still do.
                        tracing::warn!("no serpis-node for {system}: {e:#}");
                        *ask = false;
                        return Self::connect_once(&target, name).await.map_err(|f| f.error.context(format!("serpis has no serpis-node for {system}: {e:#}")));
                    }
                }
            } else {
                return Err(error);
            }
        }
        bail!("couldn't install serpis-node on {}", describe(&target))
    }

    /// A node running a build this client doesn't carry (an older serpis installed it) is
    /// restarted on the new one when none of its chats is mid-turn; otherwise the connection
    /// to it is kept, marked outdated.
    async fn upgrade(c: (Client, mpsc::UnboundedReceiver<Event>), target: &Target, name: &str, stage: &impl Fn(Stage)) -> Result<(Client, mpsc::UnboundedReceiver<Event>)> {
        let wanted: Vec<String> = match target {
            Target::Ssh { deploy, command: None, .. } => deploy.iter().map(|n| n.sha256.clone()).collect(),
            Target::Own { exe, .. } => crate::deploy::sha256_file(exe).into_iter().collect(),
            _ => Vec::new(),
        };
        let (mut client, events) = c;
        if wanted.is_empty() || wanted.contains(&client.hello.build) {
            return Ok((client, events));
        }
        // A node that doesn't say its build predates Restart too: it is stopped as any node can
        // be, when none of its chats is busy (its open chats come back in the new one all the same).
        let restarted = if client.hello.build.is_empty() {
            match client.call(Request::List).await {
                Ok(Response::Chats { chats }) if chats.iter().all(|c| !c.state.running && !c.state.compacting) => client.call(Request::Stop).await.is_ok(),
                _ => false,
            }
        } else {
            client.restart().await.is_ok()
        };
        if !restarted {
            client.hello.outdated = true;
            return Ok((client, events));
        }
        stage(Stage::Upgrading);
        drop((client, events));
        // The old node is on its way out: the new one starts when the agent finds its socket gone.
        let mut last = None;
        for i in 0..20 {
            tokio::time::sleep(std::time::Duration::from_millis(150 * (i + 1).min(5))).await;
            match Self::connect_once(target, name).await {
                Ok(c) => return Ok(c),
                Err(f) => last = Some(f.error),
            }
        }
        Err(last.unwrap_or_else(|| anyhow!("the node didn't come back")).context("restarting serpis-node on the new build"))
    }

    /// Ask the node to restart (on a newer build), which it does unless a chat is mid-turn.
    pub async fn restart(&self) -> Result<()> {
        self.call(Request::Restart).await.map(|_| ())
    }

    async fn connect_once(target: &Target, name: &str) -> Result<(Client, mpsc::UnboundedReceiver<Event>), Failed> {
        let failed = |error: anyhow::Error| Failed { stderr: String::new(), error };
        let (mut client, events) = match target {
            Target::Local(path) => {
                let s = tokio::net::UnixStream::connect(path).await.with_context(|| format!("no node at {}", path.display())).map_err(failed)?;
                let (r, w) = s.into_split();
                Self::over(r, w, None)
            }
            Target::Own { socket, exe } => {
                let s = match tokio::net::UnixStream::connect(socket).await {
                    Ok(s) => s,
                    #[cfg(feature = "node")]
                    Err(_) => {
                        crate::bridge::start_node(exe, socket).map_err(failed)?;
                        crate::bridge::wait_for(socket).await.map_err(failed)?
                    }
                    #[cfg(not(feature = "node"))]
                    Err(e) => return Err(failed(anyhow!("no node at {} to connect to, and this build can't start {}: {e}", socket.display(), exe.display()))),
                };
                let (r, w) = s.into_split();
                Self::over(r, w, None)
            }
            Target::Ssh { host, ssh, ssh_args, env, command, deploy, ask } => {
                let mut argv = ssh.clone();
                argv.extend(ssh_args.iter().cloned());
                let run = command.clone().unwrap_or_else(|| crate::deploy::agent_command(deploy, *ask));
                argv.extend([host.clone(), "--".into(), format!("{}{run}", env_prefix(env))]);
                Self::spawn(&argv).map_err(failed)?
            }
            Target::Command(argv) => Self::spawn(argv).map_err(failed)?,
        };
        match client.call(Request::Hello { version: wire::VERSION, client: name.to_string() }).await {
            Ok(Response::Hello { host, serpis, home, setup, build, notes, .. }) => client.hello = Hello { host, serpis, home, setup, build, outdated: false, notes },
            Ok(other) => return Err(failed(anyhow!("the node answered hello with {other:?}"))),
            Err(e) => {
                // What the far side said is the reason: wait for all of it.
                let stderr = match client.stderr.take() {
                    Some((said, task)) => {
                        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), task).await;
                        said.lock().unwrap().clone()
                    }
                    None => String::new(),
                };
                let why = if stderr.trim().is_empty() || crate::deploy::wanted(&stderr).is_some() { e } else { anyhow!("{}", stderr.trim()) };
                return Err(Failed { stderr, error: why.context(format!("connecting to {}", describe(target))) });
            }
        }
        Ok((client, events))
    }

    fn spawn(argv: &[String]) -> Result<(Client, mpsc::UnboundedReceiver<Event>)> {
        let (program, args) = argv.split_first().context("empty command")?;
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting {program}"))?;
        let (r, w) = (child.stdout.take().unwrap(), child.stdin.take().unwrap());
        // What ssh says when it can't connect (no route, host key, refused) is the error the
        // user needs: it goes to the log, and to the failed call's error.
        let stderr = child.stderr.take().unwrap();
        let said = Arc::new(Mutex::new(String::new()));
        let s = said.clone();
        let task = tokio::spawn(async move {
            let mut lines = tokio::io::BufReader::new(stderr);
            let mut buf = String::new();
            let _ = lines.read_to_string(&mut buf).await;
            if !buf.trim().is_empty() {
                tracing::warn!("node connection: {}", buf.trim());
            }
            s.lock().unwrap().push_str(buf.trim());
        });
        let (mut c, e) = Self::over(r, w, Some(said.clone()));
        c._child = Some(child);
        c.stderr = Some((said, task));
        Ok((c, e))
    }

    /// Greet a node over a stream already open to it (an ssh channel the caller made, as the
    /// phone app does), and be its client.
    pub async fn over_stream(r: impl AsyncRead + Unpin + Send + 'static, w: impl AsyncWrite + Unpin + Send + 'static, name: &str) -> Result<(Client, mpsc::UnboundedReceiver<Event>)> {
        let (mut client, events) = Self::over(r, w, None);
        match client.call(Request::Hello { version: wire::VERSION, client: name.to_string() }).await? {
            Response::Hello { host, serpis, home, setup, build, notes, .. } => client.hello = Hello { host, serpis, home, setup, build, outdated: false, notes },
            other => bail!("the node answered hello with {other:?}"),
        }
        Ok((client, events))
    }

    fn over(mut r: impl AsyncRead + Unpin + Send + 'static, mut w: impl AsyncWrite + Unpin + Send + 'static, stderr: Option<Arc<Mutex<String>>>) -> (Client, mpsc::UnboundedReceiver<Event>) {
        let (out, mut out_rx) = mpsc::unbounded_channel::<Vec<u8>>();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let pending: Pending = Default::default();
        tokio::spawn(async move {
            while let Some(b) = out_rx.recv().await {
                if w.write_all(&b).await.is_err() || w.flush().await.is_err() {
                    break;
                }
            }
        });
        let p = pending.clone();
        tokio::spawn(async move {
            let mut d = wire::Decoder::default();
            let mut buf = vec![0u8; 64 * 1024];
            'read: loop {
                let n = match r.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(n) => n,
                };
                d.push(&buf[..n]);
                loop {
                    match d.next() {
                        Ok(Some((tag::REPLY, body))) => {
                            if let Ok(reply) = serde_json::from_slice::<Reply>(&body) {
                                if let Some(tx) = p.lock().unwrap().remove(&reply.id) {
                                    let _ = tx.send(reply.response);
                                }
                            }
                        }
                        Ok(Some((tag::TERMINAL, body))) => {
                            if let Some((name, data)) = wire::parse_terminal(&body) {
                                let _ = ev_tx.send(Event::TerminalOutput { name, data: data.to_vec() });
                            }
                        }
                        Ok(Some((tag::EVENT, body))) => match serde_json::from_slice::<Event>(&body) {
                            Ok(e) => {
                                let _ = ev_tx.send(e);
                            }
                            Err(e) => tracing::warn!("an event this client can't read: {e}"),
                        },
                        Ok(Some(_)) => {}
                        Ok(None) => break,
                        Err(e) => {
                            tracing::warn!("node stream: {e}");
                            break 'read;
                        }
                    }
                }
            }
            // Pending calls fail with what the transport said, if anything.
            let why = stderr.map(|s| s.lock().unwrap().clone()).filter(|s| !s.is_empty()).unwrap_or_else(|| "the node closed the connection".into());
            for (_, tx) in p.lock().unwrap().drain() {
                let _ = tx.send(Response::Error { message: why.clone() });
            }
        });
        (Client { out, pending, next: AtomicU64::new(1), hello: Hello::default(), _child: None, stderr: None, rt: tokio::runtime::Handle::current() }, ev_rx)
    }

    /// Type `bytes` into the terminal `name` (attached with `AttachTerminal`).
    pub fn send_terminal(&self, name: &str, bytes: &[u8]) {
        let _ = self.out.send(wire::terminal_frame(name, bytes));
    }

    /// Send a request and wait for its answer; the node's errors are errors. The request goes
    /// out when this is called, not when the answer is awaited, so calls made one after another
    /// reach the node in that order.
    pub fn call(&self, request: Request) -> impl std::future::Future<Output = Result<Response>> + Send + 'static {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let sent = self.out.send(wire::encode(tag::CALL, &Call { id, request })).is_ok();
        if !sent {
            self.pending.lock().unwrap().remove(&id);
        }
        async move {
            if !sent {
                bail!("the connection to the node is closed");
            }
            match rx.await.map_err(|_| anyhow!("the connection to the node is closed"))? {
                Response::Error { message } => Err(anyhow!(message)),
                r => Ok(r),
            }
        }
    }

    /// Send a request without waiting for its answer (errors are logged). Callable from any
    /// thread.
    pub fn send(self: &Arc<Self>, request: Request) {
        let answer = self.call(request);
        self.rt.spawn(async move {
            if let Err(e) = answer.await {
                tracing::warn!("node: {e:#}");
            }
        });
    }

    /// Whether the stream is still open.
    pub fn is_open(&self) -> bool {
        !self.out.is_closed()
    }
}

/// A connect that failed, and what the far side said meanwhile.
struct Failed {
    stderr: String,
    error: anyhow::Error,
}

fn describe(t: &Target) -> String {
    match t {
        Target::Local(p) | Target::Own { socket: p, .. } => format!("the node at {}", p.display()),
        Target::Ssh { host, .. } => host.clone(),
        Target::Command(a) => a.join(" "),
    }
}
