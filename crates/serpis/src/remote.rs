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
}

/// The machines chats can run on: `remote-hosts`, then rayfish peers.
pub fn machines(ed: &mut Editor) -> Vec<Machine> {
    let mut out = configured(ed);
    if ed.var("remote-rayfish").and_then(Value::as_bool).unwrap_or(true) {
        refresh_rayfish(ed);
        for m in &ed.remote.rayfish {
            if !out.iter().any(|o| o.name == m.name) {
                out.push(m.clone());
            }
        }
    }
    out
}

fn node_command(ed: &Editor) -> String {
    ed.var_str("remote-node-command").unwrap_or_else(|| "serpis-node agent".into())
}

fn ssh_target(ed: &Editor, host: &str, command: Option<String>) -> Target {
    let Target::Ssh { mut ssh_args, .. } = Target::ssh(host) else { unreachable!() };
    if let Some(Value::Array(extra)) = ed.var("remote-ssh-args") {
        ssh_args.extend(extra.iter().filter_map(Value::as_str).map(str::to_string));
    }
    Target::Ssh { host: host.to_string(), command: command.unwrap_or_else(|| node_command(ed)), ssh_args }
}

/// `remote-hosts`: `"box"`, or `{name, host, node_command, command}` (`command`: a program that
/// speaks the protocol on stdio instead of ssh, e.g. for a container).
fn configured(ed: &Editor) -> Vec<Machine> {
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
                        _ => ssh_target(ed, host?, o.get("node_command").and_then(Value::as_str).map(str::to_string)),
                    };
                    (name, target)
                }
                _ => return None,
            };
            Some(Machine { name, target, source: "remote-hosts", detail: String::new() })
        })
        .collect()
}

/// Ask rayfish for its peers, at most every 30 seconds, in the background.
fn refresh_rayfish(ed: &mut Editor) {
    if ed.remote.rayfish_checked.is_some_and(|t| t.elapsed() < Duration::from_secs(30)) {
        return;
    }
    ed.remote.rayfish_checked = Some(Instant::now());
    let main = ed.main_sender();
    ed.rt.spawn(async move {
        let out = tokio::process::Command::new("ray").args(["--json", "status"]).stdin(std::process::Stdio::null()).stderr(std::process::Stdio::null()).output().await;
        let Ok(out) = out else { return };
        let Ok(status) = serde_json::from_slice::<Value>(&out.stdout) else { return };
        let peers = rayfish_peers(&status);
        main.send(move |ed| {
            let targets: Vec<Machine> = peers.into_iter().map(|(host, state)| Machine { target: ssh_target(ed, &host, None), name: host, source: "rayfish", detail: state }).collect();
            ed.remote.rayfish = targets;
        });
    });
}

/// `hostname.network.ray` and its state, for every peer in `ray --json status`.
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
    machines(ed).into_iter().find(|m| m.name == name).map(|m| m.target).unwrap_or_else(|| ssh_target(ed, name, None))
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
    let target = target_of(ed, machine);
    let (main, name) = (ed.main_sender(), machine.to_string());
    ed.rt.spawn(async move {
        let result = Client::connect(&target, "serpis").await;
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

fn connected(ed: &mut Editor, machine: &str, client: Result<Arc<Client>, String>) {
    let waiting = match ed.remote.conns.remove(machine) {
        Some(Conn::Connecting(w)) => w,
        _ => Vec::new(),
    };
    match &client {
        Ok(c) => {
            ed.remote.conns.insert(machine.to_string(), Conn::Up(c.clone()));
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

/// Pick a machine (`local` first), then call `f` with its name ("local" for this one).
fn pick_machine(ed: &mut Editor, prompt: &str, with_local: bool, f: impl FnOnce(&mut Editor, String) + 'static) {
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
        cands.push(Candidate::new(m.name.clone()).detail(detail));
    }
    if cands.is_empty() {
        ed.message("No machines: add ssh hosts to remote-hosts (or install rayfish), e.g. serpis.var.set(\"remote-hosts\", {\"gpu-box\"})");
        return;
    }
    ed.completing_read(prompt, cands, false, move |ed, c, typed| {
        let name = c.map(|c| c.value).unwrap_or(typed);
        if !name.trim().is_empty() {
            f(ed, name.trim().to_string());
        }
    });
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
    ed.defvar("remote-hosts", json!([]), "Machines chats can run on: ssh destinations (\"gpu-box\", \"me@lab\", a rayfish name like \"box.home.ray\"), or {name, host, node_command} / {name, command = {argv}} tables. Each needs serpis-node on its PATH (scripts/build-node.sh).");
    ed.defvar("remote-rayfish", json!(true), "Also offer the peers of your rayfish networks (`ray --json status`) as machines, as hostname.network.ray.");
    ed.defvar("remote-node-command", json!("serpis-node agent"), "What runs on a machine to reach its node over ssh.");
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
    ed.define_command("chat-select-machine", "Choose where this chat runs. A chat with no messages yet moves to the machine you pick (then a folder there); `local` runs it in serpis.", o, |ed, _| {
        let id = crate::commands::current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
        let empty = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.is_empty()).unwrap_or(false);
        if !empty {
            ed.message("This chat already has messages: start a new chat on the other machine (remote-new-chat); moving chats between machines comes next");
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
