//! The phone app's core. It reaches machines over ssh in-process ([`ssh`]), runs their node's
//! agent there (whatever serpis left installed: it never installs one), and is serpis's own
//! protocol client over that, unchanged ([`serpis_node::client::Client`]), so the phone speaks
//! to nodes exactly as the desktop does.
//!
//! Swift sees a small surface through UniFFI: a [`Core`] that connects machines and makes calls,
//! and a [`Listener`] it implements to hear their events. Requests, answers and events cross as
//! the protocol's own JSON, which the app decodes into its types.

pub mod ssh;

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use russh::keys::ssh_key::rand_core::UnwrapErr;
use russh::keys::ssh_key::LineEnding;
use russh::keys::{Algorithm, PrivateKey};
use serpis_node::client::Client;
use serpis_proto::wire::Request;

uniffi::setup_scaffolding!();

#[derive(Debug, thiserror::Error, uniffi::Error)]
pub enum MobileError {
    /// The machine's host key was never accepted on this device.
    #[error("{host} isn't known yet (its key: {fingerprint})")]
    NewHost { host: String, fingerprint: String },
    /// The machine shows a different host key from the one accepted.
    #[error("{host}'s key changed from {had} to {now}")]
    HostKeyChanged { host: String, had: String, now: String },
    /// The machine didn't let this device's key in.
    #[error("{host} didn't accept this device's key")]
    KeyRefused { host: String },
    /// Reached, but serpis isn't set up there.
    #[error("{message}")]
    NoNode { message: String },
    #[error("{message}")]
    Failed { message: String },
}

fn failed(e: impl std::fmt::Display) -> MobileError {
    MobileError::Failed { message: e.to_string() }
}

/// What the app hears from its machines.
#[uniffi::export(with_foreign)]
pub trait Listener: Send + Sync {
    /// Something happened in one of `machine`'s chats this device shows: a protocol `Event`, as
    /// JSON.
    fn event(&self, machine: String, event: String);
    /// The connection to `machine` ended (`reason` says why, when known).
    fn closed(&self, machine: String, reason: String);
}

struct Machine {
    client: Arc<Client>,
    _ssh: ssh::Connection,
    _tasks: ssh::Tasks,
    /// Replaced by a newer connection, or let go of: its end isn't the machine's.
    superseded: Arc<std::sync::atomic::AtomicBool>,
}

/// This device's way to its machines.
#[derive(uniffi::Object)]
pub struct Core {
    rt: tokio::runtime::Runtime,
    key: Arc<PrivateKey>,
    known: ssh::KnownHosts,
    listener: Arc<dyn Listener>,
    machines: Mutex<HashMap<String, Arc<Machine>>>,
}

#[uniffi::export]
impl Core {
    /// A core keeping what it learns (accepted host keys) in `data_dir`, signing with `key` (an
    /// OpenSSH private key the app keeps in its Keychain), or with a key made now when there is
    /// none: [`Core::private_key`] gives it to keep.
    #[uniffi::constructor]
    pub fn new(data_dir: String, key: Option<String>, listener: Arc<dyn Listener>) -> Result<Arc<Core>, MobileError> {
        let key = match key {
            Some(k) => PrivateKey::from_openssh(k.trim()).map_err(|e| failed(format!("this device's key can't be read: {e}")))?,
            None => {
                let mut k = PrivateKey::random(&mut UnwrapErr(getrandom::SysRng), Algorithm::Ed25519).map_err(failed)?;
                k.set_comment("serpis phone");
                k
            }
        };
        let rt = tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().thread_name("serpis-core").build().map_err(failed)?;
        std::fs::create_dir_all(&data_dir).map_err(failed)?;
        let known = ssh::KnownHosts::at(std::path::Path::new(&data_dir).join("known_hosts.json"));
        Ok(Arc::new(Core { rt, key: Arc::new(key), known, listener, machines: Mutex::new(HashMap::new()) }))
    }

    /// This device's private key, for the app to keep.
    pub fn private_key(&self) -> String {
        self.key.to_openssh(LineEnding::LF).map(|k| k.to_string()).unwrap_or_default()
    }

    /// The line to put in a machine's `~/.ssh/authorized_keys`.
    pub fn public_key(&self) -> String {
        self.key.public_key().to_openssh().unwrap_or_default()
    }

    /// Accept `fingerprint` as `address`'s host key (after [`MobileError::NewHost`] or
    /// [`MobileError::HostKeyChanged`], when the person says so).
    pub fn accept_host(&self, address: String, fingerprint: String) -> Result<(), MobileError> {
        let (_, host, port) = ssh::parse_address(&address).map_err(failed)?;
        self.known.accept(&format!("{host}:{port}"), &fingerprint).map_err(failed)
    }

    /// Connect `machine` (the app's name for it) at `address` (`user@host[:port]`), replacing
    /// a connection it had: the node's hello, as JSON (`{host, home, serpis}`).
    pub async fn connect(&self, machine: String, address: String) -> Result<String, MobileError> {
        let (key, known, listener) = (self.key.clone(), self.known.clone(), self.listener.clone());
        let m = machine.clone();
        let made = self
            .rt
            .spawn(async move {
                let (user, host, port) = ssh::parse_address(&address).map_err(failed)?;
                let conn = ssh::Connection::open(&host, port, &user, key, known).await.map_err(|r| match r {
                    ssh::Refused::HostKey(ssh::HostCheck::Changed(had, now)) => MobileError::HostKeyChanged { host: host.clone(), had, now },
                    ssh::Refused::HostKey(check) => MobileError::NewHost { host: host.clone(), fingerprint: if let ssh::HostCheck::New(f) = check { f } else { String::new() } },
                    ssh::Refused::Key => MobileError::KeyRefused { host: host.clone() },
                    ssh::Refused::Other(e) => failed(format!("{e:#}")),
                })?;
                let ssh::Exec { read, write, ended, tasks } = conn.exec(serpis_node::deploy::FOUND_COMMAND).await.map_err(|e| failed(format!("{e:#}")))?;
                let (client, mut events) = match Client::over_stream(read, write, "serpis-ios").await {
                    Ok(c) => c,
                    Err(e) => {
                        // What the far side said is the reason, once it has said it.
                        for _ in 0..30 {
                            if ended.lock().unwrap().done {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(100)).await;
                        }
                        let ended = ended.lock().unwrap();
                        let said = ended.stderr.trim().to_string();
                        return Err(match (ended.status, said.is_empty()) {
                            (Some(127), false) | (Some(127), true) => MobileError::NoNode { message: if said.is_empty() { format!("serpis-node is not set up on {host}") } else { said } },
                            (_, false) => failed(said),
                            (_, true) => failed(format!("{e:#}")),
                        });
                    }
                };
                let hello = serde_json::json!({"host": client.hello.host, "home": client.hello.home, "serpis": client.hello.serpis}).to_string();
                let superseded = Arc::new(std::sync::atomic::AtomicBool::new(false));
                let machine = Arc::new(Machine { client: Arc::new(client), _ssh: conn, _tasks: tasks, superseded: superseded.clone() });
                // Events go to the app as they come; the end of them is the connection closing,
                // unless a newer one took its place.
                let em = m.clone();
                tokio::spawn(async move {
                    while let Some(ev) = events.recv().await {
                        if let Ok(json) = serde_json::to_string(&ev) {
                            listener.event(em.clone(), json);
                        }
                    }
                    if !superseded.load(std::sync::atomic::Ordering::Relaxed) {
                        let why = ended.lock().unwrap().stderr.trim().to_string();
                        listener.closed(em, why);
                    }
                });
                Ok::<_, MobileError>((machine, hello))
            })
            .await
            .map_err(failed)??;
        if let Some(old) = self.machines.lock().unwrap().insert(machine, made.0) {
            old.superseded.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(made.1)
    }

    /// Send `machine` a protocol request (JSON) and return its answer (JSON). An `Error` answer
    /// is a [`MobileError::Failed`] with its message.
    pub async fn call(&self, machine: String, request: String) -> Result<String, MobileError> {
        let request: Request = serde_json::from_str(&request).map_err(|e| failed(format!("not a request: {e}")))?;
        let m = self.machines.lock().unwrap().get(&machine).cloned().ok_or_else(|| failed(format!("{machine} isn't connected")))?;
        self.rt
            .spawn(async move {
                let r = m.client.call(request).await.map_err(|e| failed(format!("{e:#}")))?;
                serde_json::to_string(&r).map_err(failed)
            })
            .await
            .map_err(failed)?
    }

    /// Whether `machine` is connected now.
    pub fn is_connected(&self, machine: String) -> bool {
        self.machines.lock().unwrap().get(&machine).is_some_and(|m| m.client.is_open())
    }

    /// Stop talking to `machine` (its chats keep running there).
    pub fn disconnect(&self, machine: String) {
        if let Some(old) = self.machines.lock().unwrap().remove(&machine) {
            old.superseded.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}
