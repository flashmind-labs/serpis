//! Where chats run: the node of each machine, this one's included, a connection to each, and
//! the chat buffers that show their chats.
//!
//! Every chat runs in a node, and this window only shows it. This machine's node ([`LOCAL`]) is
//! reached at its socket and started when none runs (from the `serpis-node` next to serpis,
//! else one serpis builds); it outlives the window, so a chat keeps going when the window
//! closes, and any window or phone that reaches this machine can show it and drive it.
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

/// This machine, as a machine name: its own node.
pub const LOCAL: &str = "local";

/// A machine chats can run on.
#[derive(Debug, Clone, PartialEq)]
pub struct Machine {
    /// What it is called here (the host, unless `remote-hosts` names it).
    pub name: String,
    pub target: Target,
    /// Where it was found: "remote-hosts", "added" (machine-add) or "rayfish".
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
    /// Machines being reached only to list things: a refused login there isn't asked about.
    quiet: std::collections::HashSet<String>,
    /// Saved sessions of other machines, as they last said (`fetch_sessions`).
    pub sessions: HashMap<String, Vec<serpis_proto::wire::SessionInfo>>,
    /// Machines asked for their sessions that haven't answered yet.
    pub sessions_pending: usize,
    /// Called as each machine's sessions arrive (an open picker shows them).
    pub on_sessions: Option<Box<dyn Fn(&mut Editor)>>,
    /// What carrying a moving chat's folder kept aside, for the message when it is done.
    move_notes: HashMap<String, String>,
    /// The node builds serpis installs on machines, as last scanned: (directory, when it
    /// changed, builds).
    builds: Option<(std::path::PathBuf, Option<std::time::SystemTime>, Vec<serpis_node::deploy::NodeBinary>)>,
    /// The tools this window lends changed: lent again to every node at the next tick.
    pub tools_changed: bool,
    /// Settings changed since they were last sent to this machine's node.
    vars_changed: serde_json::Map<String, Value>,
    /// How many times this window loaded the config: after the first, this machine's node loads
    /// it again too.
    config_loads: u32,
    /// How each machine was last reached (its terminals' streams go the same way).
    pub targets: HashMap<String, Target>,
    /// What the machines' terminals are doing.
    pub terminals: crate::terminals::Polls,
    /// What machine-add is doing.
    pub adding: crate::machines::Adding,
    /// The phone being paired (`phone-pair`).
    pub pairing: Option<crate::pairing::Active>,
    /// Addresses this machine might be reached at, as `phone-pair` finds them: (address, where
    /// it comes from).
    pub pair_addresses: Vec<(String, String)>,
    /// Each machine's connection and the buffers showing its terminals, for their bytes, which
    /// go between them without the window's thread.
    pub wires: crate::terminals::Wires,
}

/// The machines chats can run on: `remote-hosts`, the ones added (`remote-machines`), then
/// rayfish peers.
pub fn machines(ed: &mut Editor) -> Vec<Machine> {
    let mut out = configured(ed);
    for (name, host) in crate::machines::added(ed) {
        if !out.iter().any(|o| o.name == name) {
            let target = ssh_target(ed, &host, None);
            out.push(Machine { name, target, source: "added", detail: String::new() });
        }
    }
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
    // One directory per node source stamp: a node built from other sources is never taken for
    // this serpis's.
    let dir = dir.join(serpis_node::STAMP);
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

/// The node builds published with this release, `<target triple>=<sha256>,…`, built in by
/// .github/workflows/release.yml. A serpis built from source has none, and builds its own.
const RELEASED_NODES: Option<&str> = option_env!("SERPIS_NODE_SHA256");

/// The checksum published for a target triple.
fn released_sha256<'a>(nodes: &'a str, triple: &str) -> Option<&'a str> {
    nodes.split(',').find_map(|n| n.trim().strip_prefix(triple)?.strip_prefix('='))
}

/// How serpis gets a node build for a system it has none for yet, into remote-node-builds: a
/// release downloads the one published with it, a checkout builds it from its source (None when
/// it can do neither, or remote-deploy is off).
fn obtain(ed: &Editor) -> Option<serpis_node::client::Obtain> {
    if !ed.var("remote-deploy").and_then(Value::as_bool).unwrap_or(true) {
        return None;
    }
    let store = crate::expand_home(&ed.var_str("remote-node-builds").unwrap_or_else(|| "~/.serpis/nodes".into())).join(serpis_node::STAMP);
    if let (Some(nodes), Some(releases)) = (RELEASED_NODES, ed.var_str("remote-node-releases").filter(|u| !u.is_empty())) {
        return Some(Arc::new(move |system: String| {
            let (releases, store) = (releases.clone(), store.clone());
            Box::pin(async move { download_node(&releases, concat!("v", env!("CARGO_PKG_VERSION")), nodes, &store, &system).await })
        }));
    }
    let root = source_root()?;
    Some(Arc::new(move |system: String| {
        let (root, store) = (root.clone(), store.clone());
        Box::pin(async move { build_node(&root, &store, &system).await })
    }))
}

/// Download the node for `system` (`serpis_node::deploy::PROBE`) published with release `tag` under `releases`
/// into `store`, and keep it only if it matches the checksum built into serpis.
async fn download_node(releases: &str, tag: &str, nodes: &str, store: &std::path::Path, system: &str) -> anyhow::Result<serpis_node::deploy::NodeBinary> {
    use anyhow::{Context, bail};
    let triple = serpis_node::deploy::triple_for(system).with_context(|| format!("serpis has no release for {system}"))?;
    let triple = triple.split(".2.").next().unwrap_or(triple);
    let wanted = released_sha256(nodes, triple).with_context(|| format!("this release has no serpis-node for {system}"))?;
    let url = format!("{}/{tag}/serpis-node-{triple}", releases.trim_end_matches('/'));
    let http = reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(30)).timeout(std::time::Duration::from_secs(600)).build()?;
    let bytes = http.get(&url).send().await.and_then(reqwest::Response::error_for_status).with_context(|| format!("downloading {url}"))?.bytes().await.with_context(|| format!("downloading {url}"))?;
    let dir = store.join(triple);
    tokio::fs::create_dir_all(&dir).await?;
    // Beside its final name and moved, so nothing half-written or wrong is ever found there.
    let part = dir.join(format!(".serpis-node.{}.part", uuid::Uuid::new_v4()));
    tokio::fs::write(&part, &bytes).await?;
    let got = serpis_node::deploy::sha256_file(&part)?;
    if got != wanted {
        let _ = tokio::fs::remove_file(&part).await;
        bail!("{url} isn't the node this serpis was released with (its checksum is {got}, not {wanted})");
    }
    tokio::fs::rename(&part, dir.join("serpis-node")).await?;
    serpis_node::deploy::scan(store).into_iter().find(|n| n.system == system).with_context(|| format!("the build for {system} isn't in {}", store.display()))
}

/// Build the node for `system` (`serpis_node::deploy::PROBE`) with scripts/build-node.sh into `store`.
async fn build_node(root: &std::path::Path, store: &std::path::Path, system: &str) -> anyhow::Result<serpis_node::deploy::NodeBinary> {
    use anyhow::{Context, bail};
    let triple = serpis_node::deploy::triple_for(system).with_context(|| format!("serpis can't build for {system}"))?;
    let here = serpis_node::deploy::this_system();
    let mut cmd = tokio::process::Command::new(root.join("scripts/build-node.sh"));
    cmd.arg("--store").env("SERPIS_NODE_STORE", store);
    // A Mac's own node needs nothing else. Anything else is cross-compiled with zig, a Linux
    // machine's own node too: linked here it would need this machine's glibc, not 2.28, and may
    // be copied to an older one.
    if here != system || !system.starts_with("Darwin") {
        // That needs these two.
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
    serpis_node::deploy::scan(store).into_iter().find(|n| n.system == system).with_context(|| format!("the build for {system} isn't in {}", store.display()))
}

/// How an ssh target logs in: `user@host`, or the host.
pub fn ssh_login(t: &Target) -> Option<String> {
    match t {
        Target::Ssh { host, .. } => Some(host.clone()),
        _ => None,
    }
}

/// The host an ssh target reaches, without its user.
fn ssh_host(t: &Target) -> Option<String> {
    match t {
        Target::Ssh { host, .. } => Some(host.rsplit('@').next().unwrap_or(host).to_string()),
        _ => None,
    }
}

pub(crate) fn ssh_target(ed: &mut Editor, host: &str, command: Option<String>) -> Target {
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
    // The host keys serpis trusts (machine-add asks), and ssh's, read only. ssh keeps an option's
    // first value: these come after remote-ssh-args, which can say otherwise.
    let mut files = vec![crate::machines::known_hosts(ed)];
    if ed.var("remote-ssh-known-hosts").and_then(Value::as_bool).unwrap_or(true) {
        files.push(crate::expand_home("~/.ssh/known_hosts"));
    }
    let files: Vec<String> = files.iter().map(|f| format!("\"{}\"", f.display())).collect();
    ssh_args.extend(["-o".into(), format!("UserKnownHostsFile={}", files.join(" ")), "-o".into(), "StrictHostKeyChecking=yes".into()]);
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

/// This machine's node: its socket and the `serpis-node` to start it from, found or built.
async fn local_target(root: Option<std::path::PathBuf>, store: std::path::PathBuf, configured: Option<String>, stage: impl Fn(serpis_node::client::Stage)) -> anyhow::Result<Target> {
    let socket = serpis_node::socket_path();
    let exe = match configured {
        Some(c) => crate::expand_home(&c),
        None => own_node(root, store, stage).await?,
    };
    anyhow::ensure!(exe.is_file(), "no serpis-node at {}", exe.display());
    Ok(Target::Own { socket, exe })
}

/// The `serpis-node` built from these sources: beside serpis (how it is installed, and where
/// cargo builds it), else in the node store, else built there now from serpis's source.
async fn own_node(root: Option<std::path::PathBuf>, store: std::path::PathBuf, stage: impl Fn(serpis_node::client::Stage)) -> anyhow::Result<std::path::PathBuf> {
    // A node of other sources (a stale build) speaks another protocol: its stamp says so.
    async fn ours(exe: &std::path::Path) -> bool {
        let out = tokio::process::Command::new(exe).arg("stamp").stdin(std::process::Stdio::null()).output().await;
        out.is_ok_and(|o| String::from_utf8_lossy(&o.stdout).trim() == serpis_node::STAMP)
    }
    if let Some(beside) = std::env::current_exe().and_then(|e| e.canonicalize()).ok().and_then(|e| Some(e.parent()?.join("serpis-node"))) {
        if beside.is_file() && ours(&beside).await {
            return Ok(beside);
        }
    }
    let here = serpis_node::deploy::this_system();
    if let Some(n) = serpis_node::deploy::scan(&store).into_iter().find(|n| n.system == here) {
        return Ok(n.path);
    }
    let Some(root) = root else { anyhow::bail!("serpis-node isn't installed beside serpis, and serpis has no source to build it from") };
    stage(serpis_node::client::Stage::Building(here.clone()));
    Ok(build_node(&root, &store, &here).await?.path)
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
    let local = machine == LOCAL;
    // This machine's node answers at once; the card is for building it, should it come to that.
    if !local {
        ed.busy_start(&key, &format!("Connecting to {machine}…"));
    }
    tracing::info!("connecting to {machine}");
    let target = if local { None } else { Some(target_of(ed, machine)) };
    let local_parts = (source_root(), crate::expand_home(&ed.var_str("remote-node-builds").unwrap_or_else(|| "~/.serpis/nodes".into())).join(serpis_node::STAMP), ed.var_str("local-node-binary").filter(|s| !s.is_empty()));
    let obtain = obtain(ed);
    let wires = ed.remote.wires.clone();
    let (main, name) = (ed.main_sender(), machine.to_string());
    ed.rt.spawn(async move {
        // The first connect to a machine installs its node there, which takes a few seconds, and
        // the first to a new kind of system builds the node for it first, which takes minutes.
        let stage = {
            let (main, name, key) = (main.clone(), name.clone(), key.clone());
            move |s: serpis_node::client::Stage| {
                let label = match s {
                    serpis_node::client::Stage::Building(system) => format!("{} serpis-node for {system} (once)…", if RELEASED_NODES.is_some() { "Downloading" } else { "Building" }),
                    serpis_node::client::Stage::Installing => format!("Installing serpis-node on {name}…"),
                    serpis_node::client::Stage::Upgrading => format!("Updating serpis-node on {name}…"),
                };
                let key = key.clone();
                main.send(move |ed| ed.busy_start(&key, &label));
            }
        };
        let target = match target {
            Some(t) => Ok(t),
            None => {
                let (root, store, configured) = local_parts;
                local_target(root, store, configured, &stage).await
            }
        };
        let result = match target {
            Ok(target) => {
                let (t, n) = (target.clone(), name.clone());
                main.send(move |ed| {
                    ed.remote.targets.insert(n, t);
                });
                Client::connect_with(&target, "serpis", obtain, stage).await
            }
            Err(e) => Err(e),
        };
        let (client, events) = match result {
            Ok((c, e)) => (Ok(Arc::new(c)), Some(e)),
            Err(e) => (Err(format!("{e:#}")), None),
        };
        let pump_main = main.clone();
        let pump_name = name.clone();
        if let Ok(c) = &client {
            wires.connected(&name, c.clone());
        }
        main.send(move |ed| connected(ed, &name, client));
        // Events go to the chats that show them, in order; the end of the stream is a drop. A
        // terminal's bytes go straight to its buffer, whatever the window's thread is doing.
        if let Some(mut events) = events {
            while let Some(ev) = events.recv().await {
                if wires.deliver(&pump_name, &ev) {
                    pump_main.repaint();
                    continue;
                }
                let n = pump_name.clone();
                pump_main.send(move |ed| route(ed, &n, ev));
            }
            wires.dropped(&pump_name);
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
    let quiet = ed.remote.quiet.remove(machine);
    if let Err(e) = &client
        && e.contains("Permission denied")
        && !quiet
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
            lend(ed, c);
            if machine == LOCAL {
                // This machine's node runs the same config: it gets this window's settings, and
                // whatever changes them from now on.
                let vars: serde_json::Map<String, Value> = ed.vars.iter().map(|(k, v)| (k.clone(), v.value.clone())).collect();
                ed.remote.vars_changed.clear();
                c.send(Request::SetVars { vars });
                // From here too: the node may have been started by something other than this app
                // (an older version still mid-turn, a build run from source), and has no icon.
                serpis_node::indicator::set_enabled(ed.var("status-indicator").and_then(Value::as_bool).unwrap_or(true));
                // A node that was already running may have read them before they changed.
                c.send(Request::ReloadModels);
                if ed.remote.config_loads > 1 {
                    reload_node_config(ed);
                }
            } else if c.hello.setup.providers.is_empty() {
                // A machine with no models or keys can't run a chat: offer to copy this one's.
                offer_setup(ed, machine, false);
            }
            for note in &c.hello.notes {
                ed.warn(format!("{}: {note}", if machine == LOCAL { "this machine's node" } else { machine }));
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

/// Whether `remote-hosts` or `remote-machines` names the account for `machine` itself
/// (`user@host`).
fn account_named(ed: &Editor, machine: &str) -> bool {
    if crate::machines::added(ed).iter().any(|(n, h)| n == machine && h.contains('@')) {
        return true;
    }
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
                            if c.node.state.model.is_none() {
                                c.set_model_spec(&model);
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

/// Close this window's connection to `machine` (its node keeps running, and anyone else's
/// connection with it), unless chats or terminals of it are shown here: false then.
pub fn disconnect(ed: &mut Editor, machine: &str) -> bool {
    if !chats_on(ed, machine).is_empty() || crate::terminals::shown_on(ed, machine) {
        return false;
    }
    ed.remote.wires.dropped(machine);
    ed.remote.conns.remove(machine);
    ed.remote.targets.remove(machine);
    true
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
    if chats_on(ed, machine).is_empty() && !crate::terminals::shown_on(ed, machine) {
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
                if let Some(chat) = ed.with_buffer_as::<ChatBuffer, _>(id, |b, _| b.chat_id()) {
                    c.send(Request::Attach { chat });
                }
            }
            crate::terminals::reattach(ed, &name);
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

/// How `machine` is reached now: the way its connection went, with the node builds there are
/// now (a connect can build one for a system serpis had none for).
pub fn current_target(ed: &mut Editor, machine: &str) -> Option<Target> {
    let mut t = ed.remote.targets.get(machine).cloned()?;
    if let Target::Ssh { deploy, .. } = &mut t {
        *deploy = node_builds(ed);
    }
    Some(t)
}

/// Whether `machine`'s node is connected now.
pub fn is_up(ed: &Editor, machine: &str) -> bool {
    matches!(ed.remote.conns.get(machine), Some(Conn::Up(c)) if c.is_open())
}

/// The machines connected now.
pub fn connected_machines(ed: &Editor) -> Vec<String> {
    ed.remote.conns.keys().filter(|m| is_up(ed, m)).cloned().collect()
}

/// Send `request` to `machine`'s node; `done` gets the answer on the UI thread.
pub fn request(ed: &mut Editor, machine: &str, request: Request, done: impl FnOnce(&mut Editor, Result<Response, String>) + 'static) {
    with_client(ed, machine, move |ed, c| match c {
        Err(e) => done(ed, Err(e)),
        Ok(c) => {
            let older = c.hello.host.clone();
            // Sent now: requests reach the node in the order they are made.
            let answer = c.call(request);
            answer_with(ed, async move { answer.await.map_err(|e| older_node(&older, format!("{e:#}"))) }, done);
        }
    });
}

/// Await `answer` off the UI thread and give it to `done` on it (`done` needn't be Send).
pub fn answer_with(ed: &mut Editor, answer: impl Future<Output = Result<Response, String>> + Send + 'static, done: impl FnOnce(&mut Editor, Result<Response, String>) + 'static) {
    let token = ed.remote.next_answer;
    ed.remote.next_answer += 1;
    ed.remote.answers.insert(token, Box::new(done));
    let main = ed.main_sender();
    ed.rt.spawn(async move {
        let r = answer.await;
        // (Not a log's own records: logged, they would be read back and logged again.)
        if !matches!(r, Ok(Response::Logs { .. })) {
            tracing::debug!("answer {token}: {r:?}");
        }
        main.send(move |ed| {
            if let Some(done) = ed.remote.answers.remove(&token) {
                done(ed, r);
            }
        });
    });
}

/// What an older node's "can't read that call" means for whoever asked.
fn older_node(host: &str, error: String) -> String {
    if error.contains("can't read that call") {
        format!("{host} runs an older serpis-node that can't do this yet: it updates once its chats are idle (or stop them there)")
    } else {
        error
    }
}

/// Send `request` to `machine`'s node and report a failure (for actions on a chat).
pub fn send(ed: &mut Editor, machine: &str, request: Request) {
    let name = machine.to_string();
    self::request(ed, machine, request, move |ed, r| {
        if let Err(e) = r {
            ed.error(if name == LOCAL { e } else { format!("{name}: {e}") });
        }
    });
}

/// Buffers showing chats of `machine`.
pub fn chats_on(ed: &Editor, machine: &str) -> Vec<BufferId> {
    ed.buffers
        .iter()
        .filter(|(_, s)| s.buf.as_ref().and_then(|b| b.as_any().downcast_ref::<ChatBuffer>()).is_some_and(|c| c.node.machine == machine))
        .map(|(id, _)| id)
        .collect()
}

/// The buffer showing `chat` of `machine`, if one does.
pub fn chat_buffer(ed: &Editor, machine: &str, chat: &str) -> Option<BufferId> {
    ed.buffers.iter().find_map(|(id, s)| {
        let r = &s.buf.as_ref()?.as_any().downcast_ref::<ChatBuffer>()?.node;
        (r.machine == machine && r.chat == chat).then_some(id)
    })
}

fn route(ed: &mut Editor, machine: &str, ev: Event) {
    let chat = match &ev {
        Event::Splice { chat, .. } | Event::State { chat, .. } | Event::Closed { chat } | Event::Notice { chat, .. } => chat.clone(),
        Event::Message { level, text } => return say(ed, machine, *level, text),
        Event::ToolCall { chat, call, name, args } => return run_lent(ed, machine, chat.clone(), *call, name.clone(), args.clone()),
        // (Delivered on the way in: see `with_client`.)
        Event::TerminalOutput { .. } | Event::TerminalExited { .. } => return,
        Event::PairRequested { pairing, key, label } => return crate::pairing::requested(ed, machine, pairing.clone(), key.clone(), label.clone()),
    };
    let Some(id) = chat_buffer(ed, machine, &chat) else { return };
    let ev = Some(ev);
    let mut ev = ev;
    if ed.with_buffer_as::<ChatBuffer, _>(id, |c, ed| c.node_event(ed, ev.take().unwrap())).is_none() {
        // The buffer is busy (its own hook is running): try again next tick.
        if let Some(ev) = ev {
            let m = machine.to_string();
            ed.defer(move |ed| route(ed, &m, ev));
        }
    }
}

/// What a machine's node said, outside any chat.
fn say(ed: &mut Editor, machine: &str, level: serpis_proto::wire::NoticeLevel, text: &str) {
    use serpis_proto::wire::NoticeLevel;
    let text = if machine == LOCAL { text.to_string() } else { format!("{machine}: {text}") };
    match level {
        NoticeLevel::Info => ed.message(text),
        NoticeLevel::Warn => ed.warn(text),
        NoticeLevel::Error => ed.error(text),
    }
}

/// Open a buffer showing `chat` on `machine` (the one that already does, if any).
pub fn open_chat(ed: &mut Editor, machine: &str, chat: &str, show: bool) -> BufferId {
    let id = match chat_buffer(ed, machine, chat) {
        Some(id) => id,
        None => {
            let buf = ChatBuffer::new(ed, &json!({"machine": machine, "chat": chat}));
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

/// Start chat `chat` (an id this window chose) in `cwd` on `machine`, and show it in the buffer
/// that has it. Messages for it can go meanwhile: the node takes them in order.
pub fn start_chat(ed: &mut Editor, machine: &str, chat: &str, cwd: &str, model: Option<String>, thinking: Option<String>) {
    let (m, c) = (machine.to_string(), chat.to_string());
    request(ed, machine, Request::New { cwd: cwd.to_string(), model, thinking, chat: Some(chat.to_string()) }, move |ed, r| match r {
        Ok(_) => {}
        Err(e) => {
            ed.error(format!("{}: {e}", if m == LOCAL { "Starting the chat" } else { &m }));
            if let Some(id) = chat_buffer(ed, &m, &c) {
                ed.with_buffer_as::<ChatBuffer, _>(id, |b, _| b.set_online(false));
            }
        }
    });
    // Right behind it, so this window shows it, and lends it its tools, from its first message.
    send(ed, machine, Request::Attach { chat: chat.to_string() });
}

/// Continue saved session `path` of `machine` in the buffer that shows chat `chat` (the id the
/// session has, as far as this window knows).
pub fn resume_into(ed: &mut Editor, machine: &str, path: &str, chat: &str) {
    let (m, expected) = (machine.to_string(), chat.to_string());
    request(ed, machine, Request::Resume { session: path.to_string() }, move |ed, r| match r {
        // Mostly the id the session has, and attached right behind; else the one the node gave.
        Ok(Response::Chat { chat }) if chat == expected => {}
        Ok(Response::Chat { chat }) => {
            if let Some(id) = chat_buffer(ed, &m, &expected) {
                ed.with_buffer_as::<ChatBuffer, _>(id, |b, ed| b.become_remote(ed, &m, &chat));
            }
            send(ed, &m, Request::Attach { chat });
        }
        Ok(other) => ed.error(format!("{m}: unexpected answer {other:?}")),
        Err(e) => ed.error(format!("{}: {e}", if m == LOCAL { "Resuming the chat" } else { &m })),
    });
    // A session without an id resumes under another, and that one is attached above instead.
    if !chat.is_empty() {
        let (m, c) = (machine.to_string(), chat.to_string());
        request(ed, machine, Request::Attach { chat: chat.to_string() }, move |ed, r| {
            if r.is_err() && chat_buffer(ed, &m, &c).is_some() {
                tracing::debug!("{m}: {c} resumed under another id");
            }
        });
    }
}

/// Start a chat in `dir` on `machine` and show it.
pub fn new_chat(ed: &mut Editor, machine: &str, dir: &str, model: Option<String>) {
    let buf = ChatBuffer::new(ed, &json!({"machine": machine, "cwd": dir, "model": model}));
    let id = ed.add_buffer("*chat*", Box::new(buf));
    ed.show_buffer(id);
}

/// Turn chat buffer `id`, a chat with no messages yet, into a new chat in `dir` on `machine`.
fn move_empty_chat(ed: &mut Editor, id: BufferId, machine: &str, dir: &str) {
    let m = machine.to_string();
    let chat = uuid::Uuid::now_v7().to_string();
    request(ed, machine, Request::New { cwd: dir.to_string(), model: None, thinking: None, chat: Some(chat.clone()) }, move |ed, r| match r {
        Ok(_) => {
            // The empty chat it showed is closed where it was.
            if let Some((was, old)) = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| (c.node.machine.clone(), c.chat_id())) {
                send(ed, &was, Request::Detach { chat: old, close: true });
            }
            ed.with_buffer_as::<ChatBuffer, _>(id, |c, ed| c.become_remote(ed, &m, &chat));
            send(ed, &m, Request::Attach { chat });
        }
        Err(e) => ed.error(format!("{}: {e}", if m == LOCAL { "this machine" } else { &m })),
    });
}

/// A fresh conversation in chat buffer `id`: a new chat where it runs, in its folder, with its
/// model and thinking level (chat-clear). The one it showed is closed there, its session kept.
pub fn fresh_chat(ed: &mut Editor, id: BufferId) {
    let Some((machine, old, state)) = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| (c.node.machine.clone(), c.chat_id(), c.state_now())) else { return };
    let chat = uuid::Uuid::now_v7().to_string();
    send(ed, &machine, Request::Detach { chat: old, close: true });
    ed.with_buffer_as::<ChatBuffer, _>(id, |c, ed| {
        c.become_remote(ed, &machine, &chat);
        c.node.state.model = state.model.clone();
        c.node.state.thinking = state.thinking.clone();
    });
    start_chat(ed, &machine, &chat, &state.cwd, state.model, Some(state.thinking));
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
        Candidate::new(format!("Bring {name} with it")).detail(format!("to its place on {there}: where it was before, beside a clone of it, or serpis's folder (remote-carry-directory)")).value("bring"),
        Candidate::new(format!("Use a folder that is on {there}")).value("pick"),
    ];
    ed.completing_read("Its folder: ", cands, true, move |ed, c, _| match c.map(|c| c.value).as_deref() {
        Some("bring") => {
            let (f2, t2) = (from.clone(), to.clone());
            carry(ed, id, from, to, dir, move |ed, r| match r {
                Ok((cwd, note)) => {
                    if let Some(n) = note {
                        ed.remote.move_notes.insert(format!("{id:?}"), n);
                    }
                    fetch_session(ed, id, f2, t2, cwd)
                }
                Err(e) => moved_failed(ed, id, format!("bringing its folder: {e}")),
            });
        }
        Some(_) => pick(ed, Box::new(move |ed, dir| fetch_session(ed, id, from, to, dir))),
        None => {}
    });
}

/// Machines whose saved sessions are listed with this machine's: those of remote-hosts, and the
/// rayfish peers serpis can log in to (connected, or with a known account): listing never asks.
pub fn listable(ed: &mut Editor) -> Vec<String> {
    let users = ed.var("remote-users").cloned().unwrap_or(Value::Null);
    machines(ed)
        .into_iter()
        .filter(|m| m.source != "rayfish" || users.get(&m.name).is_some() || matches!(ed.remote.conns.get(&m.name), Some(Conn::Up(_))))
        .map(|m| m.name)
        .collect()
}

/// Ask every listable machine for its saved sessions, quietly; `on_sessions` runs as they come.
/// This machine's node is asked too: which of its sessions are open or running.
pub fn fetch_sessions(ed: &mut Editor) {
    fetch_local_sessions(ed);
    let list = listable(ed);
    ed.remote.sessions_pending = list.len();
    for m in list {
        if !matches!(ed.remote.conns.get(&m), Some(Conn::Up(_))) {
            ed.remote.quiet.insert(m.clone());
        }
        let name = m.clone();
        request(ed, &m, Request::Sessions { limit: Some(200) }, move |ed, r| {
            match r {
                Ok(Response::Sessions { sessions }) => {
                    ed.remote.sessions.insert(name, sessions);
                }
                _ => {
                    ed.remote.sessions.remove(&name);
                }
            }
            ed.remote.sessions_pending = ed.remote.sessions_pending.saturating_sub(1);
            if let Some(f) = ed.remote.on_sessions.take() {
                f(ed);
                ed.remote.on_sessions.get_or_insert(f);
            }
        });
    }
}

/// Ask this machine's node which of its sessions are open or running (another device may be
/// running one); `on_sessions` runs when it answers.
pub fn fetch_local_sessions(ed: &mut Editor) {
    request(ed, LOCAL, Request::Sessions { limit: Some(500) }, |ed, r| {
        if let Ok(Response::Sessions { sessions }) = r {
            ed.remote.sessions.insert(LOCAL.into(), sessions);
            if let Some(f) = ed.remote.on_sessions.take() {
                f(ed);
                ed.remote.on_sessions.get_or_insert(f);
            }
        }
    });
}

/// Resume session `path` (a file on `machine`) there, and show it.
pub fn resume_on(ed: &mut Editor, machine: &str, path: &str) {
    let m = machine.to_string();
    request(ed, machine, Request::Resume { session: path.to_string() }, move |ed, r| match r {
        Ok(Response::Chat { chat }) => {
            open_chat(ed, &m, &chat, true);
        }
        Ok(other) => ed.error(format!("{m}: unexpected answer {other:?}")),
        Err(e) => ed.error(format!("{m}: {e}")),
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

/// Carry folder `dir` (on `from`, None: here) to its place on `to` (None: here; see
/// serpis_node::workspace), over the nodes' own connections; `done` gets the chat's folder
/// there, and what was kept aside on the way.
fn carry(ed: &mut Editor, id: BufferId, from: Option<String>, to: Option<String>, dir: String, done: impl FnOnce(&mut Editor, Result<(String, Option<String>), String>) + 'static) {
    let places = serpis_node::workspace::Places {
        repo: ed.var_str("remote-carry-directory").filter(|s| !s.is_empty()).unwrap_or_else(|| serpis_node::workspace::Places::default().repo),
        plain: ed.var_str("remote-carry-directory-plain").filter(|s| !s.is_empty()).unwrap_or_else(|| serpis_node::workspace::Places::default().plain),
    };
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
            Response::Unpacked { cwd, note } => Ok((cwd, note)),
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
            let r = carry_folder(src, dst, dir, places, progress).await.map(|(cwd, note)| Response::Unpacked { cwd, note }).map_err(|e| format!("{e:#}"));
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
async fn carry_folder(src: Option<Arc<Client>>, dst: Option<Arc<Client>>, dir: String, places: serpis_node::workspace::Places, progress: impl Fn(String)) -> anyhow::Result<(String, Option<String>)> {
    use anyhow::{Context, bail};
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD as B64;
    let mb = |n: u64| format!("{:.1}", n as f64 / (1u64 << 20) as f64);
    let home = dirs::home_dir().unwrap_or_default();
    // Packed: here (a local file), or there (a name on the source node).
    let (local, remote, size, packed) = match &src {
        None => {
            let h = home.clone();
            let (file, packed) = tokio::task::spawn_blocking(move || serpis_node::workspace::pack(std::path::Path::new(&dir), &transfer_dir(), &h)).await??;
            let size = std::fs::metadata(&file)?.len();
            (Some(file), None, size, serde_json::to_value(packed)?)
        }
        Some(s) => match s.call(Request::Pack { dir }).await.map_err(|e| anyhow::anyhow!(older_node(&s.hello.host, format!("{e:#}"))))? {
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
            match t.call(Request::Unpack { name, packed, under: String::new(), places: Some(serde_json::to_value(&places)?) }).await? {
                Response::Unpacked { cwd, note } => Ok((cwd, note)),
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
            let r = tokio::task::spawn_blocking(move || serpis_node::workspace::place(&f2, &packed, &places, &home)).await.context("unpacking")?;
            let _ = std::fs::remove_file(&file);
            r.map(|p| (p.cwd.to_string_lossy().into_owned(), p.note))
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

/// The chat's session text, exported by its node (`from`; None: this machine's).
fn fetch_session(ed: &mut Editor, id: BufferId, from: Option<String>, to: Option<String>, dir: String) {
    let key = format!("remote-move:{id:?}");
    ed.busy_start(&key, &format!("Moving the chat to {}…", to.as_deref().unwrap_or("this machine")));
    let node = from.clone().unwrap_or_else(|| LOCAL.into());
    let chat = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.chat_id()).unwrap_or_default();
    request(ed, &node, Request::Export { chat }, move |ed, r| match r {
        Ok(Response::Session { text }) => deliver(ed, id, from, to, dir, text),
        Ok(other) => moved_failed(ed, id, format!("unexpected answer {other:?}")),
        Err(e) => moved_failed(ed, id, e),
    });
}

/// Continue the session on `to` (None: here), then let the machine it left know.
fn deliver(ed: &mut Editor, id: BufferId, from: Option<String>, to: Option<String>, dir: String, text: String) {
    let to_name = to.clone().unwrap_or_else(|| LOCAL.into());
    let (from_node, to_node) = (from.unwrap_or_else(|| LOCAL.into()), to.clone().unwrap_or_else(|| LOCAL.into()));
    let shown = to.clone().unwrap_or_else(|| "this machine".into());
    request(ed, &to_node.clone(), Request::Import { session: text, cwd: dir.clone() }, move |ed, r| match r {
        Ok(Response::Chat { chat }) => {
            // Marked as moved, and closed where it was.
            let old = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.chat_id()).unwrap_or_default();
            send(ed, &from_node, Request::Moved { chat: old, to: to_name });
            ed.with_buffer_as::<ChatBuffer, _>(id, |c, ed| c.become_remote(ed, &to_node, &chat));
            send(ed, &to_node, Request::Attach { chat });
            moved(ed, id, &shown, &dir);
        }
        Ok(other) => moved_failed(ed, id, format!("unexpected answer {other:?}")),
        Err(e) => moved_failed(ed, id, e),
    });
}

fn moved(ed: &mut Editor, id: BufferId, to: &str, cwd: &str) {
    ed.busy_stop(&format!("remote-move:{id:?}"));
    let note = ed.remote.move_notes.remove(&format!("{id:?}")).map(|n| format!(" ({n})")).unwrap_or_default();
    ed.message(format!("The chat continues on {to}, in {cwd}{note}"));
}

fn moved_failed(ed: &mut Editor, id: BufferId, error: String) {
    ed.busy_stop(&format!("remote-move:{id:?}"));
    ed.remote.move_notes.remove(&format!("{id:?}"));
    ed.error(format!("The chat couldn't move: {error}"));
}

/// Pick a machine (`local` first), then call `f` with its name ("local" for this one).
pub fn pick_machine(ed: &mut Editor, prompt: &str, with_local: bool, f: impl FnOnce(&mut Editor, String) + 'static) {
    let cands = machine_candidates(ed, with_local);
    let loading = ed.remote.rayfish_loading;
    // None yet: add one, and go on with it.
    if cands.len() == 1 && !loading {
        return crate::machines::add(ed, None, None, Some(Box::new(f)));
    }
    ed.remote.picker = Some((prompt.to_string(), with_local));
    ed.completing_read(prompt, cands, false, move |ed, c, typed| {
        ed.remote.picker = None;
        let name = c.map(|c| c.value).unwrap_or(typed);
        if name == crate::machines::ADD {
            crate::machines::add(ed, None, None, Some(Box::new(f)));
        } else if !name.trim().is_empty() {
            f(ed, name.trim().to_string());
        }
    });
    // rayfish peers are still being asked for: they join the list when they arrive.
    ed.minibuffer_loading(loading);
}

/// The open machine picker, again with what is known now (rayfish answered).
fn refresh_picker(ed: &mut Editor) {
    crate::machines::refresh(ed);
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
    cands.push(Candidate::new("Add a machine…").value(crate::machines::ADD).detail("ssh config, Tailscale, rayfish, this network, or any ssh destination"));
    cands
}

/// Pick a folder on `machine` (its home's folders, or a typed path), then call `f` with it.
pub fn pick_dir(ed: &mut Editor, machine: &str, f: impl FnOnce(&mut Editor, String) + 'static) {
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

/// The chats that are open, to show again next time (`session-restore-on-start`): where each
/// runs, and its session, to resume should its node have closed it meanwhile.
pub fn open_chats(ed: &Editor) -> Value {
    let list: Vec<Value> = ed
        .buffers
        .values()
        .filter_map(|s| {
            let r = &s.buf.as_ref()?.as_any().downcast_ref::<ChatBuffer>()?.node;
            Some(json!({"machine": r.machine, "chat": r.chat, "session": r.state.session}))
        })
        .collect();
    Value::Array(list)
}

pub fn reopen(ed: &mut Editor, list: &Value) {
    for v in list.as_array().into_iter().flatten() {
        let (Some(m), Some(c)) = (v["machine"].as_str(), v["chat"].as_str()) else { continue };
        if chat_buffer(ed, m, c).is_some() {
            continue;
        }
        let buf = ChatBuffer::new(ed, &json!({"machine": m, "chat": c}));
        ed.add_buffer("*chat*", Box::new(buf));
        let (m2, c2, session) = (m.to_string(), c.to_string(), v["session"].as_str().map(str::to_string));
        request(ed, m, Request::Attach { chat: c.to_string() }, move |ed, r| {
            // Closed there meanwhile (its node stopped, or it was idle and nobody showed it):
            // it continues from its session.
            if r.is_err()
                && let Some(path) = session
            {
                resume_into(ed, &m2, &path, &c2);
            }
        });
    }
}

/// The tools this window runs for the chats it shows, wherever they run: `serpis_lua` and
/// `serpis_api`, and the ones its Lua defines (a node uses its own of the same name).
fn lent_specs(ed: &Editor) -> Vec<serpis_proto::wire::ToolSpec> {
    ed.agent_tools.iter().map(|(_, t)| serpis_proto::wire::ToolSpec { name: t.name().to_string(), description: t.description(), parameters: t.parameters() }).collect()
}

fn lend(ed: &Editor, c: &Arc<Client>) {
    c.send(Request::Lend { tools: lent_specs(ed) });
}

/// Run tool `name` this window lent, for `chat` on `machine`, and answer its node.
fn run_lent(ed: &mut Editor, machine: &str, chat: String, call: u64, name: String, args: Value) {
    let tool = ed.agent_tools.iter().find(|(_, t)| t.name() == name).map(|(_, t)| t.clone());
    let Some(tool) = tool else {
        return send(ed, machine, Request::ToolDone { call, text: format!("{name} isn't defined in this window any more"), is_error: true });
    };
    let mut ctx = serpis_agent::ToolContext::new(ed.cwd.clone());
    // How serpis_lua finds the chat it runs for.
    ctx.env.insert("SERPIS_SESSION_ID".into(), chat);
    let (main, m) = (ed.main_sender(), machine.to_string());
    ed.rt.spawn(async move {
        let (text, is_error) = match tool.execute(format!("lent-{call}"), args, ctx).await {
            Ok(r) => (r.text_content(), r.is_error),
            Err(e) => (format!("{e:#}"), true),
        };
        main.send(move |ed| send(ed, &m, Request::ToolDone { call, text, is_error }));
    });
}

/// Once a frame: what changed for the nodes goes to them (tools lent, this machine's settings).
pub fn tick(ed: &mut Editor) {
    crate::machines::tick(ed);
    crate::pairing::tick(ed);
    let up = |ed: &Editor, m: &str| match ed.remote.conns.get(m) {
        Some(Conn::Up(c)) if c.is_open() => Some(c.clone()),
        _ => None,
    };
    if std::mem::take(&mut ed.remote.tools_changed) {
        let conns: Vec<Arc<Client>> = ed.remote.conns.values().filter_map(|c| if let Conn::Up(c) = c { Some(c.clone()) } else { None }).collect();
        for c in conns {
            lend(ed, &c);
        }
    }
    if !ed.remote.vars_changed.is_empty()
        && let Some(c) = up(ed, LOCAL)
    {
        c.send(Request::SetVars { vars: std::mem::take(&mut ed.remote.vars_changed) });
    }
}

/// The window loaded its config again: so does this machine's node, when it runs.
pub fn config_reloaded(ed: &mut Editor) {
    ed.remote.config_loads += 1;
    if ed.remote.config_loads > 1 && matches!(ed.remote.conns.get(LOCAL), Some(Conn::Up(_))) {
        reload_node_config(ed);
    }
}

fn reload_node_config(ed: &mut Editor) {
    request(ed, LOCAL, Request::Reload, |ed, r| match r {
        Ok(Response::Reloaded { errors }) => {
            for e in errors {
                ed.warn(format!("this machine's node: {e}"));
            }
        }
        Ok(_) => {}
        Err(e) => ed.error(format!("this machine's node: {e}")),
    });
}

/// models.json or auth.json changed here: this machine's node reads them again.
pub fn models_changed(ed: &mut Editor) {
    if matches!(ed.remote.conns.get(LOCAL), Some(Conn::Up(_))) {
        send(ed, LOCAL, Request::ReloadModels);
    }
}

/// A setting changed: this machine's node gets it at the next tick.
pub fn var_changed(ed: &mut Editor, name: &str, value: &Value) {
    ed.remote.vars_changed.insert(name.to_string(), value.clone());
}

pub fn install(ed: &mut Editor) {
    let o = BUILTIN;
    ed.defvar("remote-hosts", json!([]), "Machines chats can run on: ssh destinations (\"gpu-box\", \"me@lab\", a rayfish name like \"box.home.ray\"), or {name, host, env = {VAR = value}, node_command} / {name, command = {argv}} tables. serpis installs its node on each the first time (remote-deploy).");
    ed.defvar("remote-rayfish", json!(true), "Also offer the peers of your rayfish networks (`ray status --json`) as machines, as hostname.network.ray.");
    ed.defvar("remote-rayfish-command", json!(""), "The ray command for remote-rayfish (\"\": ray on PATH, else the one in /Applications/Rayfish.app).");
    ed.defvar("remote-node-command", json!(""), "What runs on a machine to reach its node over ssh, instead of the node serpis installs there (\"\": serpis's own, else serpis-node from PATH, ~/.local/bin or ~/.cargo/bin).");
    ed.defvar("remote-deploy", json!(true), "Install serpis-node on a machine the first time serpis connects to it (into ~/.serpis/bin there, named and checked by its SHA-256), from remote-node-builds.");
    ed.defvar("remote-node-builds", json!("~/.serpis/nodes"), "Where serpis keeps the node builds it installs on machines: <dir>/<node source stamp>/<target triple>/serpis-node. A build is made the first time a machine of that system needs one (a release downloads it from remote-node-releases, a checkout builds it from its source), or by scripts/build-node.sh --store.");
    ed.defvar("remote-node-releases", json!("https://github.com/flashmind-labs/serpis/releases/download"), "Where a released serpis downloads node builds from: <this>/<tag>/serpis-node-<target triple>, checked against the checksums built into serpis, so a mirror can't serve anything else (\"\": never download). A serpis built from source doesn't use it.");
    ed.defvar("remote-users", Editor::persisted_state("remote-users").unwrap_or_else(|| json!({})), "The account to log in as on each machine named without one (a rayfish peer, a bare host): {[\"box.net.ray\"] = \"alice\"}. serpis asks when a machine refuses your account here, and keeps the answer.");
    ed.defvar("remote-carry-directory", json!("~/.serpis/worktrees/{project}/{branch}"), "Where a repository brought along with a moving chat goes on a machine it hasn't been on: {project}, {branch}, and {path} (where it was, relative to the home there: \"~/{path}\" keeps the same layout). Where it was before, or where it came from, it goes back to; beside a clone of it at the same place it becomes a worktree of that clone. Nothing there is overwritten: changes made there are stashed, commits kept on a branch.");
    ed.defvar("remote-carry-directory-plain", json!("~/.serpis/carried/{project}"), "Where a folder that isn't a repository goes when brought along ({project}, {path}); one changed there since is moved aside first.");
    ed.defvar("remote-offer-setup", json!(true), "When a machine has no models or API keys, offer to copy this machine's there (remote-setup does it any time).");
    ed.defvar("remote-setup-declined", Editor::persisted_state("remote-setup-declined").unwrap_or_else(|| json!([])), "Machines not to offer setting up again.");
    ed.defvar("local-node-binary", json!(""), "The serpis-node this machine's chats run in (\"\": the one beside serpis, else one serpis builds into remote-node-builds). It keeps running when serpis quits; serpis restarts it on a newer build once its chats are idle.");
    ed.defvar("status-indicator", json!(true), "macOS: show an icon in the menu bar while this machine's node or terminals run, which stays when serpis quits, with what they are doing and a way to stop them.");
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
            if m == LOCAL {
                if ed.buffer_machine(id).is_some() {
                    let here = ed.cwd.to_string_lossy().into_owned();
                    move_empty_chat(ed, id, LOCAL, &here);
                }
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
    ed.define_command("remote-close-chat", "Stop this chat where it runs and close it there (its session stays, to resume). Killing its buffer only stops showing it (and closes it when it is idle and nothing else shows it).", o, |ed, _| {
        let id = crate::commands::current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
        let Some((m, chat)) = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| (c.node.machine.clone(), c.chat_id())) else {
            anyhow::bail!("Not a chat buffer");
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
    ed.define_command("remote-install-service", "Start a machine's serpis-node when it boots, and again when it crashes (a systemd user service, or a launchd agent on a Mac), so chats cut off by either keep going without anyone connecting. Asks first (params: machine, or \"local\"; true to skip asking).", o, |ed, a| {
        let yes = a.params.get(1).and_then(Value::as_bool).unwrap_or(false);
        let go = move |ed: &mut Editor, m: String| {
            let install = move |ed: &mut Editor, m: String| {
                let mm = m.clone();
                request(ed, &m, Request::InstallService, move |ed, r| match r {
                    Ok(Response::Service { manager, file, note, .. }) => {
                        let place = if mm == LOCAL { "this machine".to_string() } else { mm.clone() };
                        let more = note.map(|n| format!("; {n}")).unwrap_or_default();
                        ed.message(format!("serpis-node on {place} now starts at boot and after crashes ({manager}: {file}){more}"));
                    }
                    Ok(other) => ed.error(format!("{mm}: unexpected answer {other:?}")),
                    Err(e) => ed.error(format!("{mm}: {e}")),
                });
            };
            if yes {
                return install(ed, m);
            }
            let place = if m == LOCAL { "this machine".to_string() } else { m.clone() };
            ed.y_or_n(&format!("Install serpis-node as a service on {place}, started at boot and after crashes?"), move |ed, ok| {
                if ok {
                    install(ed, m);
                }
            });
        };
        match a.str(0) {
            Some(m) => go(ed, m.to_string()),
            None => pick_machine(ed, "Start at boot on: ", true, go),
        }
        Ok(())
    });
    ed.define_command("remote-uninstall-service", "Stop starting a machine's serpis-node at boot (remote-install-service); the node running keeps running (param: machine).", o, |ed, a| {
        let go = |ed: &mut Editor, m: String| {
            let mm = m.clone();
            request(ed, &m, Request::UninstallService, move |ed, r| match r {
                Ok(Response::Service { file, .. }) if file.is_empty() => ed.message(format!("serpis-node on {mm} wasn't installed as a service")),
                Ok(Response::Service { file, .. }) => ed.message(format!("serpis-node on {mm} no longer starts at boot (removed {file})")),
                Ok(other) => ed.error(format!("{mm}: unexpected answer {other:?}")),
                Err(e) => ed.error(format!("{mm}: {e}")),
            });
        };
        match a.str(0) {
            Some(m) => go(ed, m.to_string()),
            None => pick_machine(ed, "Stop starting at boot on: ", true, go),
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

    /// Serves `body` to every request, and answers 404 for any path but `path`.
    fn serve(path: &'static str, body: Vec<u8>) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{Read, Write};
            for stream in listener.incoming().flatten() {
                let mut stream = stream;
                let mut request = [0u8; 2048];
                let n = stream.read(&mut request).unwrap_or(0);
                let found = String::from_utf8_lossy(&request[..n]).starts_with(&format!("GET {path} "));
                let (status, body) = if found { ("200 OK", &body[..]) } else { ("404 Not Found", &b""[..]) };
                let _ = write!(stream, "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len());
                let _ = stream.write_all(body);
            }
        });
        format!("http://{addr}/")
    }

    #[test]
    fn a_published_checksum_is_found_by_its_exact_triple() {
        let nodes = "x86_64-unknown-linux-musl=aaa, aarch64-apple-darwin=bbb";
        assert_eq!(released_sha256(nodes, "x86_64-unknown-linux-musl"), Some("aaa"));
        assert_eq!(released_sha256(nodes, "aarch64-apple-darwin"), Some("bbb"));
        assert_eq!(released_sha256(nodes, "x86_64-unknown"), None);
    }

    #[tokio::test]
    async fn a_downloaded_node_is_kept_only_if_its_checksum_matches() {
        let store = std::env::temp_dir().join(format!("serpis-download-test-{}", uuid::Uuid::new_v4()));
        let triple = "x86_64-unknown-linux-musl";
        let installed = store.join(triple).join("serpis-node");
        let leftovers = || std::fs::read_dir(store.join(triple)).unwrap().flatten().filter(|e| e.file_name() != "serpis-node").count();
        let system = "Linux x86_64 musl";
        let good = store.with_extension("good");
        std::fs::write(&good, b"a node").unwrap();
        let sha = serpis_node::deploy::sha256_file(&good).unwrap();
        std::fs::remove_file(&good).unwrap();
        let nodes = format!("{triple}={sha}");
        let path = "/v1.2.3/serpis-node-x86_64-unknown-linux-musl";

        // What was served isn't what was released: nothing is kept.
        let tampered = serve(path, b"not that node".to_vec());
        let err = download_node(&tampered, "v1.2.3", &nodes, &store, system).await.unwrap_err();
        assert!(format!("{err:#}").contains("isn't the node this serpis was released with"), "{err:#}");
        assert!(!installed.exists());
        assert_eq!(leftovers(), 0);

        // A release without it, or without this system, fails and says what is missing.
        let right = serve(path, b"a node".to_vec());
        let err = download_node(&right, "v9.9.9", &nodes, &store, system).await.unwrap_err();
        assert!(format!("{err:#}").contains("/v9.9.9/serpis-node-x86_64-unknown-linux-musl"), "{err:#}");
        let err = download_node(&right, "v1.2.3", &nodes, &store, "Linux aarch64 musl").await.unwrap_err();
        assert!(format!("{err:#}").contains("no serpis-node for Linux aarch64 musl"), "{err:#}");
        let err = download_node(&right, "v1.2.3", &nodes, &store, "Plan9 mips").await.unwrap_err();
        assert!(format!("{err:#}").contains("no release for Plan9 mips"), "{err:#}");

        // The real one lands where machines' nodes are looked for.
        let node = download_node(&right, "v1.2.3", &nodes, &store, system).await.unwrap();
        assert_eq!((node.system.as_str(), node.sha256.as_str(), node.path.as_path()), (system, sha.as_str(), installed.as_path()));
        assert_eq!(std::fs::read(&installed).unwrap(), b"a node");
        assert_eq!(serpis_node::deploy::scan(&store), [node.clone()]);
        assert_eq!(leftovers(), 0);

        // The same machine type with glibc gets its own file, without zigbuild's version suffix.
        let gnu = serve("/v1.2.3/serpis-node-x86_64-unknown-linux-gnu", b"a node".to_vec());
        let nodes = format!("{nodes},x86_64-unknown-linux-gnu={sha}");
        let other = download_node(&gnu, "v1.2.3", &nodes, &store, "Linux x86_64 gnu").await.unwrap();
        assert_eq!(other.path, store.join("x86_64-unknown-linux-gnu/serpis-node"));
        assert_eq!(serpis_node::deploy::scan(&store), [other, node]);
        std::fs::remove_dir_all(&store).unwrap();
    }

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
