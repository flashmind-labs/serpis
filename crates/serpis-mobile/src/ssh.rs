//! Reaching a machine in-process: an app can't run the `ssh` program. One ssh connection per
//! machine, authenticated with this device's key, its host key checked against the ones the
//! person accepted; the node is reached by running its agent on a channel, whose bytes are the
//! protocol, as `ssh <host> serpis-node agent` gives the desktop.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, Result, bail};
use russh::ChannelMsg;
use russh::keys::{HashAlg, PrivateKey, PrivateKeyWithHashAlg, PublicKey};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};

/// What became of a host key check.
#[derive(Debug, Clone, PartialEq)]
pub enum HostCheck {
    Known,
    /// Never seen: its fingerprint, for the person to accept.
    New(String),
    /// Not the one accepted before: (that one, this one).
    Changed(String, String),
}

/// The host keys accepted on this device, by `host:port` (a JSON file in the app's storage).
#[derive(Clone)]
pub struct KnownHosts {
    path: PathBuf,
}

impl KnownHosts {
    pub fn at(path: PathBuf) -> Self {
        KnownHosts { path }
    }

    fn read(&self) -> BTreeMap<String, String> {
        std::fs::read_to_string(&self.path).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
    }

    pub fn check(&self, at: &str, key: &PublicKey) -> HostCheck {
        let seen = fingerprint(key);
        match self.read().get(at) {
            Some(had) if *had == seen => HostCheck::Known,
            Some(had) => HostCheck::Changed(had.clone(), seen),
            None => HostCheck::New(seen),
        }
    }

    /// Accept `fingerprint` for `at` from now on.
    pub fn accept(&self, at: &str, fingerprint: &str) -> Result<()> {
        let mut all = self.read();
        all.insert(at.to_string(), fingerprint.to_string());
        std::fs::write(&self.path, serde_json::to_string_pretty(&all)?).with_context(|| format!("writing {}", self.path.display()))
    }
}

pub fn fingerprint(key: &PublicKey) -> String {
    key.fingerprint(HashAlg::Sha256).to_string()
}

struct Handler {
    at: String,
    known: KnownHosts,
    /// What the check found, for the caller to report when it refused the key.
    seen: Arc<Mutex<Option<HostCheck>>>,
}

impl russh::client::Handler for Handler {
    type Error = russh::Error;

    async fn check_server_key(&mut self, key: &russh::keys::PublicKeyOrCertificate) -> Result<bool, Self::Error> {
        let russh::keys::PublicKeyOrCertificate::PublicKey { key, .. } = key else {
            *self.seen.lock().unwrap() = Some(HostCheck::New("a certificate, which this app doesn't check".into()));
            return Ok(false);
        };
        let check = self.known.check(&self.at, key);
        let ok = check == HostCheck::Known;
        *self.seen.lock().unwrap() = Some(check);
        Ok(ok)
    }
}

/// Why a connection didn't come about, in the terms the app shows.
#[derive(Debug)]
pub enum Refused {
    /// The host key is new or changed (see [`HostCheck`]).
    HostKey(HostCheck),
    /// The machine didn't take this device's key.
    Key,
    Other(anyhow::Error),
}

/// An ssh connection to a machine.
pub struct Connection {
    handle: russh::client::Handle<Handler>,
}

impl Connection {
    pub async fn open(host: &str, port: u16, user: &str, key: Arc<PrivateKey>, known: KnownHosts) -> Result<Self, Refused> {
        let at = format!("{host}:{port}");
        let seen = Arc::new(Mutex::new(None));
        let config = Arc::new(russh::client::Config {
            keepalive_interval: Some(std::time::Duration::from_secs(15)),
            keepalive_max: 3,
            ..Default::default()
        });
        let handler = Handler { at, known, seen: seen.clone() };
        let connect = tokio::time::timeout(std::time::Duration::from_secs(15), russh::client::connect(config, (host, port), handler));
        let mut handle = match connect.await {
            Err(_) => return Err(Refused::Other(anyhow::anyhow!("{host} didn't answer within 15 seconds"))),
            Ok(Ok(h)) => h,
            Ok(Err(e)) => {
                return Err(match seen.lock().unwrap().take() {
                    Some(check @ (HostCheck::New(_) | HostCheck::Changed(..))) => Refused::HostKey(check),
                    _ => Refused::Other(anyhow::Error::from(e).context(format!("connecting to {host}"))),
                });
            }
        };
        let hash = handle.best_supported_rsa_hash().await.map_err(|e| Refused::Other(e.into()))?.flatten();
        let auth = handle.authenticate_publickey(user, PrivateKeyWithHashAlg::new(key, hash)).await.map_err(|e| Refused::Other(e.into()))?;
        if !auth.success() {
            return Err(Refused::Key);
        }
        Ok(Connection { handle })
    }

    /// Run `command` there; its stdout and stdin as a byte stream, and what it says on stderr
    /// (with how it ended) once it has ended.
    pub async fn exec(&self, command: &str) -> Result<Exec> {
        let channel = self.handle.channel_open_session().await.context("opening a channel")?;
        channel.exec(true, command).await.context("running the command")?;
        let (mut read, write) = channel.split();
        let (ours, theirs) = tokio::io::duplex(1 << 16);
        let (from_client, mut to_client) = tokio::io::split(theirs);
        let ended: Arc<Mutex<Ended>> = Default::default();
        let e = ended.clone();
        let mut writer = write.make_writer();
        // What the client writes goes to the channel; what the command prints comes back.
        let up = tokio::spawn(async move {
            let mut from_client = from_client;
            let _ = tokio::io::copy(&mut from_client, &mut writer).await;
            let _ = write.eof().await;
        });
        let down = tokio::spawn(async move {
            while let Some(msg) = read.wait().await {
                match msg {
                    ChannelMsg::Data { data } => {
                        if to_client.write_all(&data).await.is_err() {
                            break;
                        }
                    }
                    ChannelMsg::ExtendedData { data, .. } => e.lock().unwrap().stderr.push_str(&String::from_utf8_lossy(&data)),
                    ChannelMsg::ExitStatus { exit_status } => e.lock().unwrap().status = Some(exit_status),
                    ChannelMsg::Eof | ChannelMsg::Close => break,
                    _ => {}
                }
            }
            let _ = to_client.shutdown().await;
            e.lock().unwrap().done = true;
        });
        let (r, w) = tokio::io::split(ours);
        Ok(Exec { read: Box::new(r), write: Box::new(w), ended, tasks: Tasks(up, down) })
    }

    pub fn is_closed(&self) -> bool {
        self.handle.is_closed()
    }
}

#[derive(Default, Debug)]
pub struct Ended {
    pub stderr: String,
    pub status: Option<u32>,
    pub done: bool,
}

/// A command running on a machine.
pub struct Exec {
    pub read: Box<dyn AsyncRead + Unpin + Send>,
    pub write: Box<dyn AsyncWrite + Unpin + Send>,
    pub ended: Arc<Mutex<Ended>>,
    pub tasks: Tasks,
}

/// The tasks carrying a command's bytes: ended when this is dropped.
pub struct Tasks(tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>);

impl Drop for Tasks {
    fn drop(&mut self) {
        self.0.abort();
        self.1.abort();
    }
}

/// A user@host[:port] as the person typed it.
pub fn parse_address(address: &str) -> Result<(String, String, u16)> {
    let (user, rest) = address.trim().split_once('@').context("a machine is user@host (or user@host:port)")?;
    let (host, port) = match rest.rsplit_once(':') {
        Some((h, p)) if !h.contains(':') => (h, p.parse().context("the port is a number")?),
        _ => (rest, 22),
    };
    if user.is_empty() || host.is_empty() {
        bail!("a machine is user@host (or user@host:port)");
    }
    Ok((user.to_string(), host.to_string(), port))
}

#[cfg(test)]
mod tests {
    #[test]
    fn addresses_name_a_user_a_host_and_maybe_a_port() {
        assert_eq!(super::parse_address("me@box").unwrap(), ("me".into(), "box".into(), 22));
        assert_eq!(super::parse_address(" me@box.net.ray:2222 ").unwrap(), ("me".into(), "box.net.ray".into(), 2222));
        assert!(super::parse_address("box").is_err());
        assert!(super::parse_address("@box").is_err());
    }
}
