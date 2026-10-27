//! A small ssh server for tests of what reaches machines over ssh in-process (the phone app's
//! core): it lets in one key (or those in an authorized_keys file, read at every login, as sshd
//! does; their options aren't applied) and runs each command it is asked to as a process on this machine,
//! in the environment the server has (a test's throwaway home), the way sshd runs one with a
//! login shell. Listens on 127.0.0.1 only.

use std::sync::Arc;

use anyhow::{Context, Result};
use russh::keys::ssh_key::rand_core::UnwrapErr;
use russh::keys::{Algorithm, PrivateKey, PublicKey};
use russh::server::{Auth, Msg, PublicKeyOffer, Server as _, Session, VerifiedPublicKey};
use russh::{Channel, ChannelId, ChannelMsg};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Mutex;

/// A running server.
pub struct Sshd {
    pub port: u16,
    /// What it shows clients as its host key.
    pub host_key: PublicKey,
}

/// Who gets in.
#[derive(Clone)]
pub enum Authorized {
    Key(PublicKey),
    /// The keys in this authorized_keys file when they log in.
    File(std::path::PathBuf),
}

impl Authorized {
    fn lets_in(&self, key: &PublicKey) -> bool {
        match self {
            Authorized::Key(k) => k.key_data() == key.key_data(),
            Authorized::File(f) => std::fs::read_to_string(f).unwrap_or_default().lines().any(|l| {
                let fields: Vec<&str> = l.split_whitespace().collect();
                fields.windows(2).any(|w| PublicKey::from_openssh(&format!("{} {}", w[0], w[1])).is_ok_and(|k| k.key_data() == key.key_data()))
            }),
        }
    }
}

/// Serve on a free port of 127.0.0.1, letting in `authorized` alone, until the runtime ends.
pub async fn start(authorized: PublicKey) -> Result<Sshd> {
    start_for(Authorized::Key(authorized)).await
}

/// Serve on a free port of 127.0.0.1, letting in whom `authorized` says, until the runtime ends.
pub async fn start_for(authorized: Authorized) -> Result<Sshd> {
    let key = PrivateKey::random(&mut UnwrapErr(getrandom::SysRng), Algorithm::Ed25519).context("a host key")?;
    let host_key = key.public_key().clone();
    let config = Arc::new(russh::server::Config { keys: vec![key], auth_rejection_time: std::time::Duration::from_millis(10), auth_rejection_time_initial: Some(std::time::Duration::ZERO), ..Default::default() });
    let socket = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await?;
    let port = socket.local_addr()?.port();
    let mut server = Server { authorized, channels: Default::default() };
    tokio::spawn(async move {
        let _ = server.run_on_socket(config, &socket).await;
    });
    Ok(Sshd { port, host_key })
}

#[derive(Clone)]
struct Server {
    authorized: Authorized,
    /// Sessions opened and not yet given a command.
    channels: Arc<Mutex<Vec<Channel<Msg>>>>,
}

impl russh::server::Server for Server {
    type Handler = Self;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Self {
        Server { authorized: self.authorized.clone(), channels: Default::default() }
    }
}

impl russh::server::Handler for Server {
    type Error = russh::Error;

    async fn auth_publickey(&mut self, _user: &str, key: VerifiedPublicKey<'_>) -> Result<Auth, Self::Error> {
        Ok(if self.authorized.lets_in(key.public_key) { Auth::Accept } else { Auth::reject() })
    }

    async fn auth_publickey_offered(&mut self, _user: &str, offer: PublicKeyOffer<'_>) -> Result<Auth, Self::Error> {
        Ok(if self.authorized.lets_in(offer.public_key) { Auth::Accept } else { Auth::reject() })
    }

    async fn channel_open_session(&mut self, channel: Channel<Msg>, reply: russh::server::ChannelOpenHandle, _: &mut Session) -> Result<(), Self::Error> {
        self.channels.lock().await.push(channel);
        reply.accept().await;
        Ok(())
    }

    async fn exec_request(&mut self, id: ChannelId, data: &[u8], session: &mut Session) -> Result<(), Self::Error> {
        let channel = {
            let mut open = self.channels.lock().await;
            let Some(i) = open.iter().position(|c| c.id() == id) else { return Ok(()) };
            open.remove(i)
        };
        session.channel_success(id)?;
        let command = String::from_utf8_lossy(data).into_owned();
        tokio::spawn(run(channel, command));
        Ok(())
    }
}

/// Run `command` with `sh -c`, its stdio joined to the channel, and say how it ended.
async fn run(channel: Channel<Msg>, command: String) {
    let child = tokio::process::Command::new("sh")
        .args(["-c", &command])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn();
    let (mut read, write) = channel.split();
    let Ok(mut child) = child else {
        let _ = write.exit_status(127).await;
        let _ = write.close().await;
        return;
    };
    let (mut stdin, mut stdout, mut stderr) = (child.stdin.take().unwrap(), child.stdout.take().unwrap(), child.stderr.take().unwrap());
    let mut out = write.make_writer();
    let mut err = write.make_writer_ext(Some(1));
    let to_out = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut stdout, &mut out).await;
        let _ = out.flush().await;
    });
    let to_err = tokio::spawn(async move {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf).await;
        let _ = err.write_all(&buf).await;
        let _ = err.flush().await;
    });
    let from = tokio::spawn(async move {
        while let Some(msg) = read.wait().await {
            match msg {
                ChannelMsg::Data { data } => {
                    if stdin.write_all(&data).await.is_err() {
                        break;
                    }
                }
                ChannelMsg::Eof | ChannelMsg::Close => break,
                _ => {}
            }
        }
    });
    let status = child.wait().await.ok().and_then(|s| s.code()).unwrap_or(1);
    let _ = to_out.await;
    let _ = to_err.await;
    from.abort();
    let _ = write.exit_status(status as u32).await;
    let _ = write.eof().await;
    let _ = write.close().await;
}
