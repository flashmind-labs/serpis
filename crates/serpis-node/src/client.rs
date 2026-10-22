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
    },
    /// Any program that speaks the protocol on its stdin and stdout.
    Command(Vec<String>),
}

impl Target {
    /// `host`'s node over ssh, with options that suit a program with nobody at the terminal:
    /// no password prompts, and dead connections noticed within a minute. It runs a
    /// serpis-node found there until `deploy` is given builds to install.
    pub fn ssh(host: &str) -> Target {
        Target::Ssh {
            host: host.to_string(),
            ssh: vec!["ssh".into()],
            ssh_args: ["-T", "-o", "BatchMode=yes", "-o", "ServerAliveInterval=15", "-o", "ServerAliveCountMax=3"].map(String::from).to_vec(),
            env: Vec::new(),
            command: None,
            deploy: Vec::new(),
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
}

#[derive(Debug, Clone, Default)]
pub struct Hello {
    pub host: String,
    pub serpis: String,
    pub home: String,
}

impl Client {
    /// Connect and greet the node, installing it first if the machine asks for it. Must be
    /// called on a tokio runtime.
    pub async fn connect(target: &Target, name: &str) -> Result<(Client, mpsc::UnboundedReceiver<Event>)> {
        Self::connect_reporting(target, name, || {}).await
    }

    /// [`Client::connect`], calling `installing` when it starts installing the node there (which
    /// takes a while: the node is megabytes).
    pub async fn connect_reporting(target: &Target, name: &str, installing: impl FnOnce()) -> Result<(Client, mpsc::UnboundedReceiver<Event>)> {
        match Self::connect_once(target, name).await {
            Err(Failed { stderr, error }) => {
                let Target::Ssh { host, ssh, ssh_args, env, deploy, .. } = target else { return Err(error) };
                let Some(node) = crate::deploy::wanted(&stderr).and_then(|h| deploy.iter().find(|n| n.sha256 == h)) else { return Err(error) };
                installing();
                crate::deploy::upload(ssh, ssh_args, host, &env_prefix(env), node).await?;
                Self::connect_once(target, name).await.map_err(|f| f.error)
            }
            ok => ok.map_err(|f| f.error),
        }
    }

    async fn connect_once(target: &Target, name: &str) -> Result<(Client, mpsc::UnboundedReceiver<Event>), Failed> {
        let failed = |error: anyhow::Error| Failed { stderr: String::new(), error };
        let (mut client, events) = match target {
            Target::Local(path) => {
                let s = tokio::net::UnixStream::connect(path).await.with_context(|| format!("no node at {}", path.display())).map_err(failed)?;
                let (r, w) = s.into_split();
                Self::over(r, w, None)
            }
            Target::Ssh { host, ssh, ssh_args, env, command, deploy } => {
                let mut argv = ssh.clone();
                argv.extend(ssh_args.iter().cloned());
                let run = command.clone().unwrap_or_else(|| crate::deploy::agent_command(deploy));
                argv.extend([host.clone(), "--".into(), format!("{}{run}", env_prefix(env))]);
                Self::spawn(&argv).map_err(failed)?
            }
            Target::Command(argv) => Self::spawn(argv).map_err(failed)?,
        };
        match client.call(Request::Hello { version: wire::VERSION, client: name.to_string() }).await {
            Ok(Response::Hello { host, serpis, home, .. }) => client.hello = Hello { host, serpis, home },
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
        (Client { out, pending, next: AtomicU64::new(1), hello: Hello::default(), _child: None, stderr: None }, ev_rx)
    }

    /// Send a request and wait for its answer; the node's errors are errors.
    pub async fn call(&self, request: Request) -> Result<Response> {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending.lock().unwrap().insert(id, tx);
        if self.out.send(wire::encode(tag::CALL, &Call { id, request })).is_err() {
            self.pending.lock().unwrap().remove(&id);
            bail!("the connection to the node is closed");
        }
        match rx.await.map_err(|_| anyhow!("the connection to the node is closed"))? {
            Response::Error { message } => Err(anyhow!(message)),
            r => Ok(r),
        }
    }

    /// Send a request without waiting for its answer (errors are logged).
    pub fn send(self: &Arc<Self>, request: Request) {
        let c = self.clone();
        tokio::spawn(async move {
            if let Err(e) = c.call(request).await {
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
        Target::Local(p) => format!("the node at {}", p.display()),
        Target::Ssh { host, .. } => host.clone(),
        Target::Command(a) => a.join(" "),
    }
}
