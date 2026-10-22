//! Chats on other machines: the machines you can use, a connection to the node on each, and the
//! chat buffers that show their chats.
//!
//! A machine is an ssh destination (`remote-hosts`, and rayfish peers when `ray` is installed
//! and `remote-rayfish` is on). Its node is reached with `ssh <host> serpis-node agent`, so
//! sshd decides who gets in, with your keys or rayfish's keyless mesh SSH; serpis keeps no
//! credentials of its own. The chats run there entirely, agent and tools included, with that
//! machine's API keys; this window shows them and sends what you type.
//!
//! Connections open on first use and stay open. One that drops is retried with growing pauses
//! while any buffer still shows a chat of that machine, and those chats are attached again,
//! with their whole transcript, when it is back.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use serpis_node::client::{Client, Target};
use serpis_proto::wire::{Event, Request, Response};

use crate::buffer::BufferId;
use crate::buffers::chat::ChatBuffer;
use crate::editor::{BUILTIN, Editor};
use crate::minibuffer::Candidate;

/// A machine chats can run on.
#[derive(Debug, Clone, PartialEq)]
pub struct Machine {
    /// What it is called here (the host, unless `remote-hosts` names it).
    pub name: String,
    pub target: Target,
    /// Where it was found: "remote-hosts" or "rayfish".
    pub source: &'static str,
    /// rayfish's state for it (active, idle, offline), if known.
    pub detail: String,
}

type Waiter = Box<dyn FnOnce(&mut Editor, Result<Arc<Client>, String>)>;

enum Conn {
    Connecting(Vec<Waiter>),
    Up(Arc<Client>),
    Down { error: String, failures: u32 },
}

/// Connections to nodes, by machine name.
#[derive(Default)]
pub struct Remotes {
    conns: HashMap<String, Conn>,
    /// Answers awaited on the UI thread, by token (they needn't be Send).
    answers: HashMap<u64, Box<dyn FnOnce(&mut Editor, Result<Response, String>)>>,
    next_answer: u64,
    rayfish: Vec<Machine>,
    rayfish_checked: Option<Instant>,
    /// `ray status` is running.
    rayfish_loading: bool,
    /// The machine picker that is open (its prompt, to recognise it), filled in as peers arrive.
    picker: Option<(String, bool)>,
    /// Accounts being tried for machines, kept (remote-users) once a login with one works.
    trying: HashMap<String, String>,
    /// Machines offered setting up this session (asked once).
    offered: std::collections::HashSet<String>,
    /// The node builds serpis installs on machines, as last scanned: (directory, when it
    /// changed, builds).
    builds: Option<(std::path::PathBuf, Option<std::time::SystemTime>, Vec<serpis_node::deploy::NodeBinary>)>,
}

/// The machines chats can run on: `remote-hosts`, then rayfish peers.
pub fn machines(ed: &mut Editor) -> Vec<Machine> {
    let mut out = configured(ed);
    if ed.var("remote-rayfish").and_then(Value::as_bool).unwrap_or(true) {
        refresh_rayfish(ed);
        // A configured machine replaces the peer it reaches (`alice@box.net.ray` says which
        // account to log in as, which rayfish can't know).
        let reached: Vec<String> = out.iter().filter_map(|o| ssh_host(&o.target)).collect();
        for m in &ed.remote.rayfish {
            if !out.iter().any(|o| o.name == m.name) && !reached.iter().any(|h| *h == m.name) {
                out.push(m.clone());
            }
        }
    }
    out
}

/// The node builds to install on machines (`remote-node-builds`), scanned again when the
/// directory changes.
fn node_builds(ed: &mut Editor) -> Vec<serpis_node::deploy::NodeBinary> {
    if !ed.var("remote-deploy").and_then(Value::as_bool).unwrap_or(true) {
        return Vec::new();
    }
    let dir = crate::expand_home(&ed.var_str("remote-node-builds").unwrap_or_else(|| "~/.serpis/nodes".into()));
    // One directory per serpis version: a node is installed with the serpis it belongs to.
    let dir = dir.join(serpis_node::VERSION);
    let modified = std::fs::metadata(&dir).and_then(|m| m.modified()).ok();
    if let Some((d, m, builds)) = &ed.remote.builds
        && *d == dir
        && *m == modified
    {
        return builds.clone();
    }
    let builds = serpis_node::deploy::scan(&dir);
    ed.remote.builds = Some((dir, modified, builds.clone()));
    builds
}

/// This serpis's source tree, when it runs from one (a checkout built with cargo): it can then
/// build the node for any system it meets. Installed serpis has none, and finds or downloads it.
fn source_root() -> Option<std::path::PathBuf> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent()?.parent()?.to_path_buf();
    root.join("scripts/build-node.sh").is_file().then_some(root)
}

/// How serpis gets a node build for a system it has none for yet: from its source, into
/// remote-node-builds (None when it can't, or remote-deploy is off).
fn obtain(ed: &Editor) -> Option<serpis_node::client::Obtain> {
    if !ed.var("remote-deploy").and_then(Value::as_bool).unwrap_or(true) {
        return None;
    }
    let root = source_root()?;
    let store = crate::expand_home(&ed.var_str("remote-node-builds").unwrap_or_else(|| "~/.serpis/nodes".into())).join(serpis_node::VERSION);
    Some(Arc::new(move |system: String| {
        let (root, store) = (root.clone(), store.clone());
        Box::pin(async move { build_node(&root, &store, &system).await })
    }))
}

/// Build the node for `system` (`uname -sm`) with scripts/build-node.sh into `store`.
async fn build_node(root: &std::path::Path, store: &std::path::Path, system: &str) -> anyhow::Result<serpis_node::deploy::NodeBinary> {
    use anyhow::{Context, bail};
    let triple = serpis_node::deploy::triple_for(system).with_context(|| format!("serpis can't build for {system}"))?;
    let here = tokio::process::Command::new("uname").arg("-sm").output().await.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let mut cmd = tokio::process::Command::new(root.join("scripts/build-node.sh"));
    cmd.arg("--store").env("SERPIS_NODE_STORE", store);
    if here != system {
        // Another system: cross-compiled with zig, which needs these two.
        // (`cargo zigbuild --version` is an error: the binary answers for itself.)
        let zigbuild = tokio::process::Command::new("cargo-zigbuild").arg("--version").output().await.is_ok_and(|o| o.status.success());
        if !zigbuild {
            bail!("building it for {system} needs cargo-zigbuild: cargo install cargo-zigbuild");
        }
        let base = triple.split(".2.").next().unwrap_or(triple);
        let targets = tokio::process::Command::new("rustup").args(["target", "list", "--installed"]).output().await.map(|o| String::from_utf8_lossy(&o.stdout).into_owned()).unwrap_or_default();
        if !targets.lines().any(|t| t.trim() == base) {
            bail!("building it for {system} needs its Rust target: rustup target add {base}");
        }
        cmd.arg(triple);
    }
    let out = cmd.output().await.context("running scripts/build-node.sh")?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        bail!("building it for {system} failed: {}", err.lines().rev().take(5).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n"));
    }
    serpis_node::deploy::scan(store).into_iter().find(|n| n.os_arch == system).with_context(|| format!("the build for {system} isn't in {}", store.display()))
}

/// The host an ssh target reaches, without its user.
fn ssh_host(t: &Target) -> Option<String> {
    match t {
        Target::Ssh { host, .. } => Some(host.rsplit('@').next().unwrap_or(host).to_string()),
        _ => None,
    }
}

fn ssh_target(ed: &mut Editor, host: &str, command: Option<String>) -> Target {
    // The account being tried, else the one remote-users keeps, for a host given without one.
    let account = (!host.contains('@')).then(|| ed.remote.trying.get(host).cloned().or_else(|| ed.var("remote-users").and_then(|u| u.get(host)?.as_str().map(str::to_string)))).flatten();
    let login = account.map(|a| format!("{a}@{host}")).unwrap_or_else(|| host.to_string());
    let mut t = Target::ssh(&login);
    let Target::Ssh { ssh, ssh_args, command: run, deploy, .. } = &mut t else { unreachable!() };
    if let Some(program) = ed.var_str("remote-ssh-command").filter(|p| !p.is_empty()) {
        *ssh = vec![program];
    }
    if let Some(Value::Array(extra)) = ed.var("remote-ssh-args") {
        ssh_args.extend(extra.iter().filter_map(Value::as_str).map(str::to_string));
    }
    *run = command.or_else(|| ed.var_str("remote-node-command")).filter(|c| !c.is_empty());
    *deploy = node_builds(ed);
    t
}

/// `remote-hosts`: `"box"`, or `{name, host, env, node_command, command}` (`env`: variables for
/// what runs there; `command`: a program that speaks the protocol on stdio instead of ssh, e.g.
/// for a container).
fn configured(ed: &mut Editor) -> Vec<Machine> {
    let Some(Value::Array(list)) = ed.var("remote-hosts").cloned() else { return Vec::new() };
    list.iter()
        .filter_map(|h| {
            let (name, target) = match h {
                Value::String(host) => (host.clone(), ssh_target(ed, host, None)),
                Value::Object(o) => {
                    let host = o.get("host").and_then(Value::as_str);
                    let name = o.get("name").and_then(Value::as_str).or(host)?.to_string();
                    let target = match o.get("command") {
                        Some(Value::Array(argv)) => Target::Command(argv.iter().filter_map(Value::as_str).map(str::to_string).collect()),
                        _ => {
                            let mut t = ssh_target(ed, host?, o.get("node_command").and_then(Value::as_str).map(str::to_string));
                            if let (Target::Ssh { env, .. }, Some(Value::Object(vars))) = (&mut t, o.get("env")) {
                                env.extend(vars.iter().filter_map(|(k, v)| Some((k.clone(), v.as_str()?.to_string()))));
                            }
                            t
                        }
                    };
                    (name, target)
                }
                _ => return None,
            };
            Some(Machine { name, target, source: "remote-hosts", detail: String::new() })
        })
        .collect()
}

/// The `ray` command: `remote-rayfish-command`, else `ray` on PATH, else (macOS) the one inside
/// Rayfish.app, which the app doesn't put on PATH.
fn ray_command(ed: &Editor) -> String {
    if let Some(c) = ed.var_str("remote-rayfish-command").filter(|c| !c.is_empty()) {
        return c;
    }
    let on_path = std::env::var_os("PATH").is_some_and(|p| std::env::split_paths(&p).any(|d| d.join("ray").is_file()));
    let bundled = "/Applications/Rayfish.app/Contents/MacOS/ray";
    if !on_path && cfg!(target_os = "macos") && std::path::Path::new(bundled).is_file() { bundled.into() } else { "ray".into() }
}

/// Ask rayfish for its peers, at most every 30 seconds, in the background.
fn refresh_rayfish(ed: &mut Editor) {
    if ed.remote.rayfish_checked.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
        return;
    }
    ed.remote.rayfish_checked = Some(Instant::now());
    ed.remote.rayfish_loading = true;
    let main = ed.main_sender();
    let ray = ray_command(ed);
    ed.rt.spawn(async move {
        // A daemon that isn't running answers in words, not JSON: no peers, then.
        let out = tokio::process::Command::new(ray).args(["status", "--json"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().await;
        let peers = out.ok().and_then(|o| serde_json::from_slice::<Value>(&o.stdout).ok()).map(|s| rayfish_peers(&s)).unwrap_or_default();
        main.send(move |ed| {
            let targets: Vec<Machine> = peers.into_iter().map(|(host, state)| Machine { target: ssh_target(ed, &host, None), name: host, source: "rayfish", detail: state }).collect();
            ed.remote.rayfish = targets;
            ed.remote.rayfish_loading = false;
            refresh_picker(ed);
        });
    });
}

/// `hostname.network.ray` and its state, for every peer in `ray status --json`.
pub fn rayfish_peers(status: &Value) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for net in status["networks"].as_array().into_iter().flatten() {
        let Some(network) = net["name"].as_str() else { continue };
        for p in net["peers"].as_array().into_iter().flatten() {
            if let Some(h) = p["hostname"].as_str().filter(|h| !h.is_empty()) {
                out.push((format!("{h}.{network}.ray"), p["state"].as_str().unwrap_or("").to_string()));
            }
        }
    }
    out
}

fn target_of(ed: &mut Editor, name: &str) -> Target {
    match machines(ed).into_iter().find(|m| m.name == name) {
        // Built now, not when the peer was found: the account to try may have changed since.
        Some(m) if m.source == "rayfish" => ssh_target(ed, name, None),
        Some(m) => m.target,
        None => ssh_target(ed, name, None),
    }
}

/// Run `f` with a connection to `machine`'s node, connecting first if needed.
pub fn with_client(ed: &mut Editor, machine: &str, f: impl FnOnce(&mut Editor, Result<Arc<Client>, String>) + 'static) {
    match ed.remote.conns.get_mut(machine) {
        Some(Conn::Up(c)) if c.is_open() => {
            let c = c.clone();
            return f(ed, Ok(c));
        }
        Some(Conn::Connecting(waiting)) => return waiting.push(Box::new(f)),
        _ => {}
    }
    ed.remote.conns.insert(machine.to_string(), Conn::Connecting(vec![Box::new(f)]));
    let key = busy_key(machine);
    ed.busy_start(&key, &format!("Connecting to {machine}…"));
    tracing::info!("connecting to {machine}");
    let target = target_of(ed, machine);
    let obtain = obtain(ed);
    let (main, name) = (ed.main_sender(), machine.to_string());
    ed.rt.spawn(async move {
        // The first connect to a machine installs its node there, which takes a few seconds, and
        // the first to a new kind of system builds the node for it first, which takes minutes.
        let stage = {
            let (main, name, key) = (main.clone(), name.clone(), key.clone());
            move |s: serpis_node::client::Stage| {
                let label = match s {
                    serpis_node::client::Stage::Building(system) => format!("Building serpis-node for {system} (once)…"),
                    serpis_node::client::Stage::Installing => format!("Installing serpis-node on {name}…"),
                    serpis_node::client::Stage::Upgrading => format!("Updating serpis-node on {name}…"),
                };
                let key = key.clone();
                main.send(move |ed| ed.busy_start(&key, &label));
            }
        };
        let result = Client::connect_with(&target, "serpis", obtain, stage).await;
        let (client, events) = match result {
            Ok((c, e)) => (Ok(Arc::new(c)), Some(e)),
            Err(e) => (Err(format!("{e:#}")), None),
        };
        let pump_main = main.clone();
        let pump_name = name.clone();
        main.send(move |ed| connected(ed, &name, client));
        // Events go to the chats that show them, in order; the end of the stream is a drop.
        if let Some(mut events) = events {
            while let Some(ev) = events.recv().await {
                let n = pump_name.clone();
                pump_main.send(move |ed| route(ed, &n, ev));
            }
            pump_main.send(move |ed| dropped(ed, &pump_name));
        }
    });
}

fn busy_key(machine: &str) -> String {
    format!("remote-connect:{machine}")
}

fn connected(ed: &mut Editor, machine: &str, client: Result<Arc<Client>, String>) {
    ed.busy_stop(&busy_key(machine));
    let waiting = match ed.remote.conns.remove(machine) {
        Some(Conn::Connecting(w)) => w,
        _ => Vec::new(),
    };
    // A refused login, unless the account was named in remote-hosts (user@host): most likely the
    // account here (or the one tried, or remembered) isn't one there. Ask, and carry on.
    if let Err(e) = &client
        && e.contains("Permission denied")
        && !account_named(ed, machine)
    {
        ed.remote.trying.remove(machine);
        return ask_account(ed, machine, e.clone(), waiting);
    }
    // An account that worked is kept for next time; a wrong one never is.
    if client.is_ok()
        && let Some(account) = ed.remote.trying.remove(machine)
    {
        remember_account(ed, machine, &account);
    }
    match &client {
        Ok(c) => {
            ed.remote.conns.insert(machine.to_string(), Conn::Up(c.clone()));
            // A machine with no models or keys can't run a chat: offer to copy this one's.
            if c.hello.setup.providers.is_empty() {
                offer_setup(ed, machine, false);
            }
            // An older node a chat kept busy: it restarts on the new build once they are idle.
            if c.hello.outdated {
                ed.message(format!("{machine} runs an older serpis-node: it updates once its chats are idle"));
                retry_upgrade(ed, machine);
            }
            ed.run_hook("remote-connected", &json!({"machine": machine, "host": c.hello.host, "serpis": c.hello.serpis}));
        }
        Err(e) => {
            let failures = 1;
            ed.remote.conns.insert(machine.to_string(), Conn::Down { error: e.clone(), failures });
            ed.run_hook("remote-disconnected", &json!({"machine": machine, "error": e}));
        }
    }
    for w in waiting {
        w(ed, client.clone());
    }
}

/// Whether `remote-hosts` names the account for `machine` itself (`user@host`).
fn account_named(ed: &Editor, machine: &str) -> bool {
    let Some(Value::Array(list)) = ed.var("remote-hosts") else { return false };
    list.iter().any(|h| match h {
        Value::String(host) => host == machine && host.contains('@'),
        Value::Object(o) => {
            let host = o.get("host").and_then(Value::as_str).unwrap_or("");
            (o.get("name").and_then(Value::as_str) == Some(machine) || host == machine) && host.contains('@')
        }
        _ => false,
    })
}

/// Ask for the account to log in to `machine` as, remember it (`remote-users`) and connect
/// again; whoever was waiting for the connection gets that one. Cancelled, they get `error`.
fn ask_account(ed: &mut Editor, machine: &str, error: String, waiting: Vec<Waiter>) {
    ed.remote.conns.insert(machine.to_string(), Conn::Down { error: error.clone(), failures: 1 });
    let m = machine.to_string();
    let waiting = std::rc::Rc::new(std::cell::RefCell::new(Some(waiting)));
    let on_cancel = waiting.clone();
    let err = error.clone();
    ed.completing_read(
        "Username: ",
        Vec::new(),
        false,
        move |ed, _, typed| {
            let account = typed.trim().to_string();
            let waiting = waiting.borrow_mut().take().unwrap_or_default();
            if account.is_empty() {
                return waiting.into_iter().for_each(|w| w(ed, Err(err.clone())));
            }
            // Tried now, and kept only if it gets in.
            ed.remote.trying.insert(m.clone(), account);
            ed.remote.conns.remove(&m);
            for w in waiting {
                with_client(ed, &m, w);
            }
        },
    );
    ed.minibuffer_preview(
        None,
        Some(Box::new(move |ed| {
            for w in on_cancel.borrow_mut().take().unwrap_or_default() {
                w(ed, Err(error.clone()));
            }
        })),
        false,
    );
}

/// Log in to `machine` as `account` from now on (`remote-users`, kept for next time).
pub fn remember_account(ed: &mut Editor, machine: &str, account: &str) {
    let mut users = match ed.var("remote-users") {
        Some(Value::Object(o)) => o.clone(),
        _ => Default::default(),
    };
    users.insert(machine.to_string(), json!(account));
    ed.set_var("remote-users", Value::Object(users.clone()));
    ed.persist_state("remote-users", Value::Object(users));
    ed.message(format!("Logged in to {machine} as {account} (kept in remote-users)"));
}

/// What this machine can give another to run chats: API keys by provider (not OAuth logins,
/// whose tokens belong to one machine), models.json, and the default model and thinking level.
struct Setup {
    auth: serde_json::Map<String, Value>,
    oauth: Vec<String>,
    models: Option<Value>,
    settings: serde_json::Map<String, Value>,
}

fn local_setup(ed: &Editor) -> Setup {
    let mut auth = serde_json::Map::new();
    let mut oauth = Vec::new();
    for p in ed.registry.configured_providers() {
        if ed.registry.is_oauth(&p) {
            oauth.push(p);
        } else if let Some(key) = ed.registry.api_key(&p).filter(|k| !k.is_empty()) {
            auth.insert(p, json!({"type": "api_key", "key": key}));
        }
    }
    let models = std::fs::read_to_string(ed.registry.models_file_path()).ok().and_then(|t| serde_json::from_str::<Value>(&t).ok()).filter(|m| m.get("providers").is_some_and(|p| p.as_object().is_some_and(|o| !o.is_empty())));
    let mut settings = serde_json::Map::new();
    for k in ["agent-model", "agent-thinking"] {
        if let Some(v) = ed.var_str(k).filter(|v| !v.is_empty()) {
            settings.insert(k.into(), json!(v));
        }
    }
    if !settings.contains_key("agent-model")
        && let Some(m) = crate::buffers::chat::default_model(ed)
    {
        settings.insert("agent-model".into(), json!(format!("{}/{}", m.provider, m.id)));
    }
    Setup { auth, oauth, models, settings }
}

/// Offer to copy this machine's keys and models to `machine` (once a session, unless `asked`
/// for, and never for a machine it was declined for), when no other prompt is open.
pub fn offer_setup(ed: &mut Editor, machine: &str, asked: bool) {
    if !asked {
        let declined = ed.var("remote-setup-declined").and_then(|v| v.as_array().cloned()).unwrap_or_default();
        if !ed.var("remote-offer-setup").and_then(Value::as_bool).unwrap_or(true) || declined.iter().any(|d| d.as_str() == Some(machine)) || !ed.remote.offered.insert(machine.to_string()) {
            return;
        }
    }
    if ed.minibuffer.is_some() {
        // Another question is open (the folder to start in): after it.
        let (main, m) = (ed.main_sender(), machine.to_string());
        ed.rt.spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            main.send(move |ed| offer_setup(ed, &m, true));
        });
        return;
    }
    let setup = local_setup(ed);
    let mut have: Vec<String> = setup.auth.keys().cloned().collect();
    if let Some(Value::Object(p)) = setup.models.as_ref().and_then(|m| m.get("providers")) {
        have.extend(p.keys().filter(|k| !setup.auth.contains_key(*k)).cloned());
    }
    if have.is_empty() {
        if asked {
            ed.message("This machine has no API keys or models to copy");
        }
        return;
    }
    let what = format!("{} · over the ssh connection, readable by your account there only", have.join(", "));
    let cands = vec![
        Candidate::new("Copy my API keys and models").detail(what).value("copy"),
        Candidate::new("Not now").value("later"),
        Candidate::new("Never for this machine").value("never"),
    ];
    let m = machine.to_string();
    ed.completing_read(&format!("{machine} has no models: "), cands, true, move |ed, c, _| match c.map(|c| c.value).as_deref() {
        Some("copy") => copy_setup(ed, &m, setup),
        Some("never") => {
            let mut declined = ed.var("remote-setup-declined").and_then(|v| v.as_array().cloned()).unwrap_or_default();
            declined.push(json!(m));
            ed.set_var("remote-setup-declined", Value::Array(declined.clone()));
            ed.persist_state("remote-setup-declined", Value::Array(declined));
            ed.message(format!("Won't offer to set up {m} again (remote-setup copies when you want)"));
        }
        _ => {}
    });
}

fn copy_setup(ed: &mut Editor, machine: &str, setup: Setup) {
    let Setup { auth, oauth, models, settings } = setup;
    let default_model = settings.get("agent-model").and_then(Value::as_str).map(str::to_string);
    let m = machine.to_string();
    ed.busy_start(&format!("remote-setup:{m}"), &format!("Copying keys and models to {m}…"));
    request(ed, machine, Request::Setup { auth, models, settings }, move |ed, r| {
        ed.busy_stop(&format!("remote-setup:{m}"));
        match r {
            Ok(Response::Setup { added }) => {
                let of = |kind: &str| added.iter().filter_map(|a| a.strip_prefix(kind)).collect::<Vec<_>>();
                let mut parts = Vec::new();
                if !of("auth:").is_empty() {
                    parts.push(format!("API keys for {}", of("auth:").join(", ")));
                }
                if !of("models:").is_empty() {
                    parts.push(format!("models of {}", of("models:").join(", ")));
                }
                if !of("settings:").is_empty() {
                    parts.push("the default model".to_string());
                }
                let mut text = if added.is_empty() { format!("{m} already had all of it") } else { format!("Set up {m}: {}", parts.join(", ")) };
                if !oauth.is_empty() {
                    text.push_str(&format!(". Log in there for {} (logins aren't copied: their tokens belong to one machine)", oauth.join(", ")));
                }
                ed.message(text);
                // Its chats that had no model can use the default one now.
                if let Some(model) = default_model {
                    for id in chats_on(ed, &m) {
                        ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| {
                            if c.remote.as_ref().is_some_and(|r| r.state.model.is_none()) {
                                c.set_remote_model(&model);
                            }
                        });
                    }
                }
            }
            Ok(other) => ed.error(format!("{m}: unexpected answer {other:?}")),
            Err(e) => ed.error(format!("{m}: {e}")),
        }
    });
}

/// The stream to `machine` ended: its chats show it, and it is retried while any is shown.
fn dropped(ed: &mut Editor, machine: &str) {
    let failures = match ed.remote.conns.remove(machine) {
        Some(Conn::Down { failures, .. }) => failures + 1,
        _ => 1,
    };
    let error = "the connection to the node closed".to_string();
    ed.remote.conns.insert(machine.to_string(), Conn::Down { error: error.clone(), failures });
    ed.run_hook("remote-disconnected", &json!({"machine": machine, "error": error}));
    for id in chats_on(ed, machine) {
        ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.set_online(false));
    }
    schedule_reconnect(ed, machine, failures);
}

fn schedule_reconnect(ed: &mut Editor, machine: &str, failures: u32) {
    if chats_on(ed, machine).is_empty() {
        return;
    }
    let wait = Duration::from_secs((2u64 << failures.min(4)).min(30));
    let (main, name) = (ed.main_sender(), machine.to_string());
    ed.rt.spawn(async move {
        tokio::time::sleep(wait).await;
        main.send(move |ed| reconnect(ed, &name));
    });
}

/// Ask an outdated node to restart on the new build every minute until it does (its chats then
/// come back through the usual reconnect).
fn retry_upgrade(ed: &mut Editor, machine: &str) {
    let (main, m) = (ed.main_sender(), machine.to_string());
    ed.rt.spawn(async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        main.send(move |ed| {
            let Some(Conn::Up(c)) = ed.remote.conns.get(&m) else { return };
            let c = c.clone();
            let (main, m2) = (ed.main_sender(), m.clone());
            ed.rt.spawn(async move {
                if c.restart().await.is_err() {
                    main.send(move |ed| retry_upgrade(ed, &m2));
                }
            });
        });
    });
}

/// Connect to `machine` again and attach every chat of it that is shown.
pub fn reconnect(ed: &mut Editor, machine: &str) {
    if matches!(ed.remote.conns.get(machine), Some(Conn::Up(c)) if c.is_open()) {
        return;
    }
    let failures = match ed.remote.conns.get(machine) {
        Some(Conn::Down { failures, .. }) => *failures,
        _ => 0,
    };
    let name = machine.to_string();
    with_client(ed, machine, move |ed, c| match c {
        Ok(c) => {
            for id in chats_on(ed, &name) {
                if let Some(chat) = ed.with_buffer_as::<ChatBuffer, _>(id, |b, _| b.remote_chat_id()).flatten() {
                    c.send(Request::Attach { chat });
                }
            }
        }
        Err(_) => {
            if let Some(Conn::Down { failures: f, .. }) = ed.remote.conns.get_mut(&name) {
                *f = failures + 1;
            }
            schedule_reconnect(ed, &name, failures + 1);
        }
    });
}

/// Whether `machine` is connected, and the last error if it isn't.
pub fn status(ed: &Editor, machine: &str) -> (bool, Option<String>) {
    match ed.remote.conns.get(machine) {
        Some(Conn::Up(c)) if c.is_open() => (true, None),
        Some(Conn::Down { error, .. }) => (false, Some(error.clone())),
        _ => (false, None),
    }
}

/// Send `request` to `machine`'s node; `done` gets the answer on the UI thread.
pub fn request(ed: &mut Editor, machine: &str, request: Request, done: impl FnOnce(&mut Editor, Result<Response, String>) + 'static) {
    with_client(ed, machine, move |ed, c| match c {
        Err(e) => done(ed, Err(e)),
        Ok(c) => {
            let token = ed.remote.next_answer;
            ed.remote.next_answer += 1;
            ed.remote.answers.insert(token, Box::new(done));
            let main = ed.main_sender();
            ed.rt.spawn(async move {
                let r = c.call(request).await.map_err(|e| format!("{e:#}"));
                main.send(move |ed| {
                    if let Some(done) = ed.remote.answers.remove(&token) {
                        done(ed, r);
                    }
                });
            });
        }
    });
}

/// Send `request` to `machine`'s node and report a failure (for actions on a chat).
pub fn send(ed: &mut Editor, machine: &str, request: Request) {
    let name = machine.to_string();
    self::request(ed, machine, request, move |ed, r| {
        if let Err(e) = r {
            ed.error(format!("{name}: {e}"));
        }
    });
}

/// Buffers showing chats of `machine`.
pub fn chats_on(ed: &Editor, machine: &str) -> Vec<BufferId> {
    ed.buffers
        .iter()
        .filter(|(_, s)| s.buf.as_ref().and_then(|b| b.as_any().downcast_ref::<ChatBuffer>()).and_then(|c| c.remote.as_ref()).is_some_and(|r| r.machine == machine))
        .map(|(id, _)| id)
        .collect()
}

fn chat_buffer(ed: &Editor, machine: &str, chat: &str) -> Option<BufferId> {
    ed.buffers.iter().find_map(|(id, s)| {
        let r = s.buf.as_ref()?.as_any().downcast_ref::<ChatBuffer>()?.remote.as_ref()?;
        (r.machine == machine && r.chat == chat).then_some(id)
    })
}

fn route(ed: &mut Editor, machine: &str, ev: Event) {
    let chat = match &ev {
        Event::Splice { chat, .. } | Event::State { chat, .. } | Event::Closed { chat } | Event::Notice { chat, .. } => chat.clone(),
    };
    let Some(id) = chat_buffer(ed, machine, &chat) else { return };
    let ev = Some(ev);
    let mut ev = ev;
    if ed.with_buffer_as::<ChatBuffer, _>(id, |c, ed| c.remote_event(ed, ev.take().unwrap())).is_none() {
        // The buffer is busy (its own hook is running): try again next tick.
        if let Some(ev) = ev {
            let m = machine.to_string();
            ed.defer(move |ed| route(ed, &m, ev));
        }
    }
}

/// Open a buffer showing `chat` on `machine` (the one that already does, if any).
pub fn open_chat(ed: &mut Editor, machine: &str, chat: &str, show: bool) -> BufferId {
    let id = match chat_buffer(ed, machine, chat) {
        Some(id) => id,
        None => {
            let buf = ChatBuffer::new_remote(ed, machine, chat);
            let id = ed.add_buffer("*chat*", Box::new(buf));
            send(ed, machine, Request::Attach { chat: chat.to_string() });
            id
        }
    };
    if show {
        ed.show_buffer(id);
    }
    id
}

/// Start a chat in `dir` on `machine` and show it.
pub fn new_chat(ed: &mut Editor, machine: &str, dir: &str, model: Option<String>) {
    let m = machine.to_string();
    request(ed, machine, Request::New { cwd: dir.to_string(), model, thinking: None }, move |ed, r| match r {
        Ok(Response::Chat { chat }) => {
            open_chat(ed, &m, &chat, true);
        }
        Ok(other) => ed.error(format!("{m}: unexpected answer {other:?}")),
        Err(e) => ed.error(format!("{m}: {e}")),
    });
}

/// Turn an empty chat buffer into one showing a new chat on `machine`.
fn move_empty_chat(ed: &mut Editor, id: BufferId, machine: &str, dir: &str) {
    let m = machine.to_string();
    request(ed, machine, Request::New { cwd: dir.to_string(), model: None, thinking: None }, move |ed, r| match r {
        Ok(Response::Chat { chat }) => {
            ed.with_buffer_as::<ChatBuffer, _>(id, |c, ed| c.become_remote(ed, &m, &chat));
            send(ed, &m, Request::Attach { chat });
        }
        Ok(other) => ed.error(format!("{m}: unexpected answer {other:?}")),
        Err(e) => ed.error(format!("{m}: {e}")),
    });
}

/// Move chat buffer `id` (running on `from`, None for here) to machine `to` ("local": here),
/// into a folder picked there: its session goes over and continues there, and the machine it
/// left keeps it, marked as moved. Only an idle chat moves.
fn move_chat(ed: &mut Editor, id: BufferId, from: Option<String>, to: String) {
    let to = if to == "local" { None } else { Some(to) };
    if from == to {
        return ed.message(format!("The chat already runs on {}", to.as_deref().unwrap_or("this machine")));
    }
    let busy = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.running() || c.state_now().compacting).unwrap_or(true);
    if busy {
        return ed.message("The chat is working: stop it (C-c C-c) or let it finish, then move it");
    }
    let target = to.clone();
    let pick = move |ed: &mut Editor, f: Box<dyn FnOnce(&mut Editor, String)>| match &target {
        Some(m) => pick_dir(ed, &m.clone(), f),
        None => ed.read_directory("Continue it in: ", move |ed, d| f(ed, d.to_string_lossy().into_owned())),
    };
    // Its folder can come along, or the chat continues in one that is there.
    let dir = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.state_now().cwd).unwrap_or_default();
    let name = std::path::Path::new(&dir).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let there = to.clone().unwrap_or_else(|| "this machine".into());
    let cands = vec![
        Candidate::new(format!("Bring {name} with it")).detail(format!("into ~/{name} on {there} (its git branch and uncommitted work, or its files)")).value("bring"),
        Candidate::new(format!("Use a folder that is on {there}")).value("pick"),
    ];
    ed.completing_read("Its folder: ", cands, true, move |ed, c, _| match c.map(|c| c.value).as_deref() {
        Some("bring") => {
            let (f2, t2) = (from.clone(), to.clone());
            carry(ed, id, from, to, dir, move |ed, r| match r {
                Ok(cwd) => fetch_session(ed, id, f2, t2, cwd),
                Err(e) => moved_failed(ed, id, format!("bringing its folder: {e}")),
            });
        }
        Some(_) => pick(ed, Box::new(move |ed, dir| fetch_session(ed, id, from, to, dir))),
        None => {}
    });
}

/// Connections to `machines`, in order; `f` gets them all, or the first failure.
fn clients(ed: &mut Editor, mut machines: Vec<String>, mut got: Vec<Arc<Client>>, f: Box<dyn FnOnce(&mut Editor, Result<Vec<Arc<Client>>, String>)>) {
    if machines.is_empty() {
        return f(ed, Ok(got));
    }
    let m = machines.remove(0);
    with_client(ed, &m, move |ed, c| match c {
        Ok(c) => {
            got.push(c);
            clients(ed, machines, got, f)
        }
        Err(e) => f(ed, Err(e)),
    });
}

/// Carry folder `dir` (on `from`, None: here) to a new folder in the home of `to` (None:
/// here), over the nodes' own connections; `done` gets the chat's folder there.
fn carry(ed: &mut Editor, id: BufferId, from: Option<String>, to: Option<String>, dir: String, done: impl FnOnce(&mut Editor, Result<String, String>) + 'static) {
    let key = format!("remote-move:{id:?}");
    ed.busy_start(&key, "Packing the folder…");
    let machines: Vec<String> = from.iter().chain(to.iter()).cloned().collect();
    let (has_from, has_to) = (from.is_some(), to.is_some());
    clients(ed, machines, Vec::new(), Box::new(move |ed, c| {
        let mut c = match c {
            Ok(c) => c.into_iter(),
            Err(e) => return done(ed, Err(e)),
        };
        let src = if has_from { c.next() } else { None };
        let dst = if has_to { c.next() } else { None };
        // The callback stays here; the result of the work comes back to it.
        let token = ed.remote.next_answer;
        ed.remote.next_answer += 1;
        ed.remote.answers.insert(token, Box::new(move |ed, r| done(ed, r.and_then(|r| match r {
            Response::Unpacked { cwd } => Ok(cwd),
            other => Err(format!("unexpected answer {other:?}")),
        }))));
        let main = ed.main_sender();
        let progress = {
            let main = main.clone();
            move |label: String| {
                let key = key.clone();
                main.send(move |ed| ed.busy_start(&key, &label));
            }
        };
        ed.rt.spawn(async move {
            let r = carry_folder(src, dst, dir, progress).await.map(|cwd| Response::Unpacked { cwd }).map_err(|e| format!("{e:#}"));
            main.send(move |ed| {
                if let Some(done) = ed.remote.answers.remove(&token) {
                    done(ed, r);
                }
            });
        });
    }));
}

/// Where this machine packs and receives carried folders (its user's alone).
fn transfer_dir() -> std::path::PathBuf {
    let d = dirs::home_dir().unwrap_or_default().join(".serpis/transfer");
    use std::os::unix::fs::DirBuilderExt;
    let _ = std::fs::DirBuilder::new().recursive(true).mode(0o700).create(&d);
    d
}

const CHUNK: u64 = 4 << 20;

/// The work of [`carry`]: pack where the folder is, move the bytes, unpack where it goes.
async fn carry_folder(src: Option<Arc<Client>>, dst: Option<Arc<Client>>, dir: String, progress: impl Fn(String)) -> anyhow::Result<String> {
    use anyhow::{Context, bail};
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as B64;
    let mb = |n: u64| format!("{:.1}", n as f64 / (1u64 << 20) as f64);
    let home = dirs::home_dir().unwrap_or_default();
    // Packed: here (a local file), or there (a name on the source node).
    let (local, remote, size, packed) = match &src {
        None => {
            let (file, packed) = tokio::task::spawn_blocking(move || serpis_node::workspace::pack(std::path::Path::new(&dir), &transfer_dir())).await??;
            let size = std::fs::metadata(&file)?.len();
            (Some(file), None, size, serde_json::to_value(packed)?)
        }
        Some(s) => match s.call(Request::Pack { dir }).await? {
            Response::Packed { name, size, packed } => (None, Some(name), size, packed),
            other => bail!("unexpected answer {other:?}"),
        },
    };
    let read = |offset: u64| {
        let (local, remote, src) = (local.clone(), remote.clone(), src.clone());
        async move {
            match (&local, &remote, &src) {
                (Some(file), _, _) => {
                    use std::io::{Read, Seek};
                    let mut f = std::fs::File::open(file)?;
                    f.seek(std::io::SeekFrom::Start(offset))?;
                    let mut buf = Vec::new();
                    f.take(CHUNK).read_to_end(&mut buf)?;
                    anyhow::Ok(buf)
                }
                (_, Some(name), Some(s)) => match s.call(Request::Fetch { name: name.clone(), offset, len: CHUNK }).await? {
                    Response::Chunk { data } => Ok(B64.decode(data)?),
                    other => bail!("unexpected answer {other:?}"),
                },
                _ => bail!("nothing packed"),
            }
        }
    };
    let result = match &dst {
        Some(t) => {
            let name = format!("put-{}", uuid::Uuid::now_v7().simple());
            let mut offset = 0;
            while offset < size || offset == 0 {
                let chunk = read(offset).await?;
                if chunk.is_empty() && offset > 0 {
                    break;
                }
                t.call(Request::Put { name: name.clone(), offset, data: B64.encode(&chunk) }).await?;
                offset += chunk.len() as u64;
                progress(format!("Bringing the folder: {} of {} MB…", mb(offset), mb(size)));
                if chunk.is_empty() {
                    break;
                }
            }
            progress("Unpacking the folder…".into());
            match t.call(Request::Unpack { name, packed, under: "~".into() }).await? {
                Response::Unpacked { cwd } => Ok(cwd),
                other => Err(anyhow::anyhow!("unexpected answer {other:?}")),
            }
        }
        None => {
            let file = transfer_dir().join(format!("in-{}", uuid::Uuid::now_v7().simple()));
            let mut out = std::fs::File::create(&file)?;
            let mut offset = 0;
            while offset < size {
                let chunk = read(offset).await?;
                if chunk.is_empty() {
                    break;
                }
                std::io::Write::write_all(&mut out, &chunk)?;
                offset += chunk.len() as u64;
                progress(format!("Bringing the folder: {} of {} MB…", mb(offset), mb(size)));
            }
            drop(out);
            progress("Unpacking the folder…".into());
            let packed: serpis_node::workspace::Packed = serde_json::from_value(packed)?;
            let f2 = file.clone();
            let r = tokio::task::spawn_blocking(move || serpis_node::workspace::unpack(&f2, &packed, &home)).await.context("unpacking")?;
            let _ = std::fs::remove_file(&file);
            r.map(|p| p.to_string_lossy().into_owned())
        }
    };
    // What was packed is no longer needed, whichever way it went.
    if let Some(f) = &local {
        let _ = std::fs::remove_file(f);
    }
    if let (Some(name), Some(s)) = (&remote, &src) {
        let _ = s.call(Request::Drop { name: name.clone() }).await;
    }
    result
}

/// The chat's session text: from its file here, or exported by its node.
fn fetch_session(ed: &mut Editor, id: BufferId, from: Option<String>, to: Option<String>, dir: String) {
    let key = format!("remote-move:{id:?}");
    ed.busy_start(&key, &format!("Moving the chat to {}…", to.as_deref().unwrap_or("this machine")));
    match &from {
        None => {
            let text = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.session_path()).flatten().and_then(|p| std::fs::read_to_string(p).ok());
            match text {
                Some(text) => deliver(ed, id, from, to, dir, text),
                None => {
                    ed.busy_stop(&key);
                    ed.error("This chat isn't saved (session-save is off), so it can't move");
                }
            }
        }
        Some(m) => {
            let chat = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.remote_chat_id()).flatten().unwrap_or_default();
            request(ed, &m.clone(), Request::Export { chat }, move |ed, r| match r {
                Ok(Response::Session { text }) => deliver(ed, id, from, to, dir, text),
                Ok(other) => moved_failed(ed, id, format!("unexpected answer {other:?}")),
                Err(e) => moved_failed(ed, id, e),
            });
        }
    }
}

/// Continue the session on `to`, then let the machine it left know.
fn deliver(ed: &mut Editor, id: BufferId, from: Option<String>, to: Option<String>, dir: String, text: String) {
    let to_name = to.clone().unwrap_or_else(|| "local".into());
    let leave = move |ed: &mut Editor| match &from {
        // Marked as moved, and closed there.
        Some(m) => {
            let chat = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.remote_chat_id()).flatten().unwrap_or_default();
            send(ed, &m.clone(), Request::Moved { chat, to: to_name.clone() });
        }
        None => {
            ed.with_buffer_as::<ChatBuffer, _>(id, |c, ed| c.append_entry(ed, crate::session::MOVED_ENTRY, json!({"to": to_name})));
        }
    };
    match to {
        None => {
            let root = crate::session_ui::session_root(ed);
            let loaded = crate::session::import(&root, &text, std::path::Path::new(&dir)).and_then(|p| crate::session::load(&p));
            match loaded {
                Ok(l) => {
                    leave(ed);
                    ed.with_buffer_as::<ChatBuffer, _>(id, |c, ed| c.continue_here(ed, &l));
                    moved(ed, id, "this machine");
                }
                Err(e) => moved_failed(ed, id, format!("{e:#}")),
            }
        }
        Some(m) => {
            let m2 = m.clone();
            request(ed, &m, Request::Import { session: text, cwd: dir }, move |ed, r| match r {
                Ok(Response::Chat { chat }) => {
                    leave(ed);
                    ed.with_buffer_as::<ChatBuffer, _>(id, |c, ed| c.become_remote(ed, &m2, &chat));
                    send(ed, &m2, Request::Attach { chat });
                    moved(ed, id, &m2);
                }
                Ok(other) => moved_failed(ed, id, format!("unexpected answer {other:?}")),
                Err(e) => moved_failed(ed, id, e),
            });
        }
    }
}

fn moved(ed: &mut Editor, id: BufferId, to: &str) {
    ed.busy_stop(&format!("remote-move:{id:?}"));
    ed.message(format!("The chat continues on {to}"));
}

fn moved_failed(ed: &mut Editor, id: BufferId, error: String) {
    ed.busy_stop(&format!("remote-move:{id:?}"));
    ed.error(format!("The chat couldn't move: {error}"));
}

/// Pick a machine (`local` first), then call `f` with its name ("local" for this one).
fn pick_machine(ed: &mut Editor, prompt: &str, with_local: bool, f: impl FnOnce(&mut Editor, String) + 'static) {
    let cands = machine_candidates(ed, with_local);
    let loading = ed.remote.rayfish_loading;
    if cands.is_empty() && !loading {
        ed.message("No machines: add ssh hosts to remote-hosts (or install rayfish), e.g. serpis.var.set(\"remote-hosts\", {\"gpu-box\"})");
        return;
    }
    ed.remote.picker = Some((prompt.to_string(), with_local));
    ed.completing_read(prompt, cands, false, move |ed, c, typed| {
        ed.remote.picker = None;
        let name = c.map(|c| c.value).unwrap_or(typed);
        if !name.trim().is_empty() {
            f(ed, name.trim().to_string());
        }
    });
    // rayfish peers are still being asked for: they join the list when they arrive.
    ed.minibuffer_loading(loading);
}

/// The open machine picker, again with what is known now (rayfish answered).
fn refresh_picker(ed: &mut Editor) {
    let Some((prompt, with_local)) = ed.remote.picker.clone() else { return };
    if ed.minibuffer.as_ref().is_none_or(|m| m.prompt != prompt) {
        ed.remote.picker = None;
        return;
    }
    let cands = machine_candidates(ed, with_local);
    ed.minibuffer_set_candidates(cands);
    ed.minibuffer_loading(ed.remote.rayfish_loading);
}

fn machine_candidates(ed: &mut Editor, with_local: bool) -> Vec<Candidate> {
    let mut cands = Vec::new();
    if with_local {
        cands.push(Candidate::new("local").detail("this machine, in serpis"));
    }
    for m in machines(ed) {
        let (up, err) = status(ed, &m.name);
        let detail = match (up, err, m.detail.is_empty()) {
            (true, _, _) => "connected".to_string(),
            (false, Some(e), _) => format!("unreachable: {}", e.lines().next().unwrap_or("")),
            (false, None, false) => format!("{} · {}", m.source, m.detail),
            (false, None, true) => m.source.to_string(),
        };
        let detail = match ed.var("remote-users").and_then(|u| u.get(&m.name)?.as_str().map(str::to_string)) {
            Some(account) => format!("{detail} · as {account}"),
            None => detail,
        };
        cands.push(Candidate::new(m.name.clone()).detail(detail));
    }
    cands
}

/// Pick a folder on `machine` (its home's folders, or a typed path), then call `f` with it.
fn pick_dir(ed: &mut Editor, machine: &str, f: impl FnOnce(&mut Editor, String) + 'static) {
    let m = machine.to_string();
    request(ed, machine, Request::Dirs { path: "~".into() }, move |ed, r| match r {
        Ok(Response::Dirs { path, dirs }) => {
            let mut cands = vec![Candidate::new("~").detail(path.clone()).value("~")];
            cands.extend(dirs.into_iter().map(|d| Candidate::new(format!("~/{d}"))));
            ed.completing_read(&format!("Folder on {m}: "), cands, false, move |ed, c, typed| {
                let dir = c.map(|c| c.value).unwrap_or(typed);
                f(ed, if dir.trim().is_empty() { "~".into() } else { dir.trim().to_string() });
            });
        }
        Ok(other) => ed.error(format!("{m}: unexpected answer {other:?}")),
        Err(e) => ed.error(format!("{m}: {e}")),
    });
}

/// Remote chats that were open, to reopen next time (`session-restore-on-start`).
pub fn open_chats(ed: &Editor) -> Value {
    let list: Vec<Value> = ed
        .buffers
        .values()
        .filter_map(|s| s.buf.as_ref()?.as_any().downcast_ref::<ChatBuffer>()?.remote.as_ref().map(|r| json!({"machine": r.machine, "chat": r.chat})))
        .collect();
    Value::Array(list)
}

pub fn reopen(ed: &mut Editor, list: &Value) {
    for v in list.as_array().into_iter().flatten() {
        if let (Some(m), Some(c)) = (v["machine"].as_str(), v["chat"].as_str()) {
            open_chat(ed, m, c, false);
        }
    }
}

pub fn install(ed: &mut Editor) {
    let o = BUILTIN;
    ed.defvar("remote-hosts", json!([]), "Machines chats can run on: ssh destinations (\"gpu-box\", \"me@lab\", a rayfish name like \"box.home.ray\"), or {name, host, env = {VAR = value}, node_command} / {name, command = {argv}} tables. serpis installs its node on each the first time (remote-deploy).");
    ed.defvar("remote-rayfish", json!(true), "Also offer the peers of your rayfish networks (`ray status --json`) as machines, as hostname.network.ray.");
    ed.defvar("remote-rayfish-command", json!(""), "The ray command for remote-rayfish (\"\": ray on PATH, else the one in /Applications/Rayfish.app).");
    ed.defvar("remote-node-command", json!(""), "What runs on a machine to reach its node over ssh, instead of the node serpis installs there (\"\": serpis's own, else serpis-node from PATH, ~/.local/bin or ~/.cargo/bin).");
    ed.defvar("remote-deploy", json!(true), "Install serpis-node on a machine the first time serpis connects to it (into ~/.serpis/bin there, named and checked by its SHA-256), from remote-node-builds.");
    ed.defvar("remote-node-builds", json!("~/.serpis/nodes"), "Where serpis keeps the node builds it installs on machines: <dir>/<serpis version>/<target triple>/serpis-node. A build is made the first time a machine of that system needs one (from serpis's source, when it runs from a checkout), or by scripts/build-node.sh --store.");
    ed.defvar("remote-users", Editor::persisted_state("remote-users").unwrap_or_else(|| json!({})), "The account to log in as on each machine named without one (a rayfish peer, a bare host): {[\"box.net.ray\"] = \"alice\"}. serpis asks when a machine refuses your account here, and keeps the answer.");
    ed.defvar("remote-offer-setup", json!(true), "When a machine has no models or API keys, offer to copy this machine's there (remote-setup does it any time).");
    ed.defvar("remote-setup-declined", Editor::persisted_state("remote-setup-declined").unwrap_or_else(|| json!([])), "Machines not to offer setting up again.");
    ed.defvar("remote-ssh-command", json!("ssh"), "The ssh program serpis reaches machines with.");
    ed.defvar("remote-ssh-args", json!([]), "More ssh options for machines (after -T, BatchMode and keepalives), e.g. {\"-o\", \"StrictHostKeyChecking=yes\"}.");

    ed.define_command("remote-new-chat", "Start a chat on another machine: pick the machine, then a folder there (params: machine, folder, model).", o, |ed, a| {
        let (machine, dir, model) = (a.str(0).map(str::to_string), a.str(1).map(str::to_string), a.str(2).map(str::to_string));
        match (machine, dir) {
            (Some(m), Some(d)) => new_chat(ed, &m, &d, model),
            (Some(m), None) => pick_dir(ed, &m.clone(), move |ed, d| new_chat(ed, &m, &d, model)),
            _ => pick_machine(ed, "Machine: ", false, move |ed, m| pick_dir(ed, &m.clone(), move |ed, d| new_chat(ed, &m, &d, model))),
        }
        Ok(())
    });
    ed.define_command("chat-select-machine", "Choose where this chat runs: the machine you pick, then a folder there (`local`: this one). A chat with messages moves with them: it continues there, and the machine it left keeps its session, marked as moved.", o, |ed, _| {
        let id = crate::commands::current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
        let empty = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.is_empty()).unwrap_or(false);
        if !empty {
            // A chat with messages moves with them.
            let from = ed.buffer_machine(id);
            pick_machine(ed, "Move this chat to: ", from.is_some(), move |ed, to| move_chat(ed, id, from, to));
            return Ok(());
        }
        pick_machine(ed, "Run this chat on: ", true, move |ed, m| {
            if m == "local" {
                ed.with_buffer_as::<ChatBuffer, _>(id, |c, ed| c.become_local(ed));
                return;
            }
            pick_dir(ed, &m.clone(), move |ed, d| move_empty_chat(ed, id, &m, &d));
        });
        Ok(())
    });
    ed.define_command("remote-attach-chat", "Show a chat running on another machine (params: machine, chat id).", o, |ed, a| {
        if let (Some(m), Some(c)) = (a.str(0), a.str(1)) {
            open_chat(ed, m, c, true);
            return Ok(());
        }
        let given = a.str(0).map(str::to_string);
        let list = move |ed: &mut Editor, m: String| {
            let mm = m.clone();
            request(ed, &m, Request::List, move |ed, r| match r {
                Ok(Response::Chats { chats }) if chats.is_empty() => ed.message(format!("No chats are running on {mm}")),
                Ok(Response::Chats { chats }) => {
                    let cands = chats
                        .into_iter()
                        .map(|c| {
                            let title = if c.state.title.is_empty() { "New chat".to_string() } else { c.state.title.clone() };
                            let what = if c.state.running { c.state.step.clone().unwrap_or_else(|| "working".into()) } else { "idle".into() };
                            Candidate::new(title).detail(format!("{} · {what}", c.state.cwd)).value(c.chat)
                        })
                        .collect();
                    ed.completing_read(&format!("Chat on {mm}: "), cands, true, move |ed, c, _| {
                        if let Some(c) = c {
                            open_chat(ed, &mm, &c.value, true);
                        }
                    });
                }
                Ok(other) => ed.error(format!("{mm}: unexpected answer {other:?}")),
                Err(e) => ed.error(format!("{mm}: {e}")),
            });
        };
        match given {
            Some(m) => list(ed, m),
            None => pick_machine(ed, "Machine: ", false, list),
        }
        Ok(())
    });
    ed.define_command("remote-resume-chat", "Resume a saved chat session of another machine there (param: machine).", o, |ed, a| {
        let resume = |ed: &mut Editor, m: String| {
            let mm = m.clone();
            request(ed, &m, Request::Sessions { limit: Some(200) }, move |ed, r| match r {
                Ok(Response::Sessions { sessions }) => {
                    let cands = sessions.into_iter().map(|s| Candidate::new(if s.title.is_empty() { s.id.clone() } else { s.title.clone() }).detail(format!("{} · {} messages", s.cwd, s.messages)).value(s.path)).collect();
                    ed.completing_read(&format!("Resume on {mm}: "), cands, true, move |ed, c, _| {
                        let Some(c) = c else { return };
                        let m2 = mm.clone();
                        request(ed, &mm, Request::Resume { session: c.value }, move |ed, r| match r {
                            Ok(Response::Chat { chat }) => {
                                open_chat(ed, &m2, &chat, true);
                            }
                            Ok(other) => ed.error(format!("{m2}: unexpected answer {other:?}")),
                            Err(e) => ed.error(format!("{m2}: {e}")),
                        });
                    });
                }
                Ok(other) => ed.error(format!("{mm}: unexpected answer {other:?}")),
                Err(e) => ed.error(format!("{mm}: {e}")),
            });
        };
        match a.str(0).map(str::to_string) {
            Some(m) => resume(ed, m),
            None => pick_machine(ed, "Machine: ", false, resume),
        }
        Ok(())
    });
    ed.define_command("remote-close-chat", "Stop this chat on its machine and close it there (its session stays, to resume). Closing the window or killing the buffer only stops showing it.", o, |ed, _| {
        let id = crate::commands::current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
        let Some((m, chat)) = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.remote.as_ref().map(|r| (r.machine.clone(), r.chat.clone()))).flatten() else {
            anyhow::bail!("This chat runs here, not on another machine");
        };
        send(ed, &m, Request::Close { chat });
        ed.kill_buffer(id);
        Ok(())
    });
    ed.define_command("remote-setup", "Copy this machine's API keys, models and default model to another machine, adding what it lacks (param: machine; default: the current chat's).", o, |ed, a| {
        let here = crate::commands::current_chat(ed).and_then(|id| ed.buffer_machine(id));
        match a.str(0).map(str::to_string).or(here) {
            Some(m) => offer_setup(ed, &m, true),
            None => pick_machine(ed, "Set up: ", false, |ed, m| offer_setup(ed, &m, true)),
        }
        Ok(())
    });
    ed.define_command("remote-stop-node", "Stop the node on a machine: its chats stop, and come back when it starts again (param: machine).", o, |ed, a| {
        let stop = |ed: &mut Editor, m: String| {
            let mm = m.clone();
            request(ed, &m, Request::Stop, move |ed, r| match r {
                Ok(_) => ed.message(format!("Stopped the node on {mm}")),
                Err(e) => ed.error(format!("{mm}: {e}")),
            });
        };
        match a.str(0).map(str::to_string) {
            Some(m) => stop(ed, m),
            None => pick_machine(ed, "Stop the node on: ", false, stop),
        }
        Ok(())
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rayfish_peers_are_named_as_its_dns_names_them() {
        // The shape of `ray status --json` (ray-proto's NetworkStatus / PeerStatus).
        let status = json!({"networks": [
            {"name": "home", "peers": [{"hostname": "gpu", "state": "active"}, {"hostname": null, "state": "idle"}, {"hostname": "nas", "state": "offline"}]},
            {"name": "work", "peers": [{"hostname": "ci", "state": "idle"}]},
        ]});
        assert_eq!(rayfish_peers(&status), [("gpu.home.ray", "active"), ("nas.home.ray", "offline"), ("ci.work.ray", "idle")].map(|(a, b)| (a.to_string(), b.to_string())));
        assert!(rayfish_peers(&json!({})).is_empty());
    }
}
