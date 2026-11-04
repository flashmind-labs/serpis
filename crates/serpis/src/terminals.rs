//! Terminals a machine holds (its terminal host: `serpis-node terminals`), shown in terminal
//! buffers. Everything about them goes over the connection the window already has to the
//! machine's node: starting, listing and ending them, and their bytes both ways. A terminal
//! outlives the window and the connection: one that drops is attached again with the machine,
//! and the next serpis to start shows it again.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use serpis_proto::wire::{Request, Response, TerminalInfo};

use crate::buffer::BufferId;
use crate::buffers::terminal::{PtyMsg, TerminalBuffer};
use crate::editor::Editor;

/// Each machine's connection, and the buffers showing its terminals: what a terminal prints
/// goes to its buffer as it arrives, and what is typed goes out as it is typed, neither waiting
/// for the window's thread (editing a command line waits on it for the echo).
#[derive(Clone, Default)]
pub struct Wires(Arc<Mutex<WiresInner>>);

#[derive(Default)]
struct WiresInner {
    clients: HashMap<String, Arc<serpis_node::client::Client>>,
    views: HashMap<(String, String), mpsc::Sender<PtyMsg>>,
}

impl Wires {
    pub fn connected(&self, machine: &str, client: Arc<serpis_node::client::Client>) {
        self.0.lock().unwrap().clients.insert(machine.to_string(), client);
    }

    /// `machine`'s connection ended: its terminals say so until it is back.
    pub fn dropped(&self, machine: &str) {
        let mut w = self.0.lock().unwrap();
        w.clients.remove(machine);
        for ((m, _), to) in &w.views {
            if m == machine {
                let _ = to.send(PtyMsg::Online(false));
            }
        }
    }

    /// Take a terminal's event to its buffer; false for any other event.
    pub fn deliver(&self, machine: &str, ev: &serpis_proto::wire::Event) -> bool {
        use serpis_proto::wire::Event;
        let mut w = self.0.lock().unwrap();
        match ev {
            Event::TerminalOutput { name, data } => {
                if let Some(to) = w.views.get(&(machine.to_string(), name.clone())) {
                    let _ = to.send(PtyMsg::Data(data.clone()));
                }
                true
            }
            Event::TerminalExited { name, .. } => {
                if let Some(to) = w.views.remove(&(machine.to_string(), name.clone())) {
                    let _ = to.send(PtyMsg::Exit);
                }
                true
            }
            _ => false,
        }
    }

    fn show(&self, machine: &str, name: &str, to: mpsc::Sender<PtyMsg>) {
        self.0.lock().unwrap().views.insert((machine.to_string(), name.to_string()), to);
    }

    fn hide(&self, machine: &str, name: &str) {
        self.0.lock().unwrap().views.remove(&(machine.to_string(), name.to_string()));
    }

    fn input(&self, machine: &str, name: &str, bytes: &[u8]) {
        if let Some(c) = self.0.lock().unwrap().clients.get(machine) {
            c.send_terminal(name, bytes);
        }
    }

    fn request(&self, machine: &str, request: Request) {
        if let Some(c) = self.0.lock().unwrap().clients.get(machine) {
            c.send(request);
        }
    }
}

/// Where a terminal buffer's bytes go (its keys, and its VT engine's answers): straight to its
/// terminal, from whatever thread writes them.
#[derive(Clone)]
pub struct Input {
    wires: Wires,
    machine: String,
    name: Arc<Mutex<Option<String>>>,
}

impl Input {
    pub fn write(&self, bytes: &[u8]) {
        if let Some(n) = self.name.lock().unwrap().as_deref() {
            self.wires.input(&self.machine, n, bytes);
        }
    }
}

/// A terminal buffer's terminal on a machine.
pub struct Link {
    pub machine: String,
    /// Its name there, once it has been started (set as the machine answers, so a buffer killed
    /// at any moment after that ends it).
    name: Arc<Mutex<Option<String>>>,
    wires: Wires,
    /// Where what it prints goes.
    to: mpsc::Sender<PtyMsg>,
    /// The size it is shown at now (shared with its attach, which may go out later: the first
    /// layout happens before the machine has said it started the terminal).
    size: Arc<Mutex<(u16, u16)>>,
    /// Attached now: what it prints arrives (false while starting, connecting, after a drop).
    pub online: bool,
    /// What the machine last said about it.
    pub info: Option<TerminalInfo>,
    /// The buffer was killed: a terminal still being started is ended once it is.
    killed: Arc<AtomicBool>,
}

/// What to show in a terminal buffer.
pub enum Open {
    /// A new login shell in `cwd` there.
    New { cwd: String, integration: bool },
    /// A terminal running there already.
    Attach { name: String },
}

impl Link {
    /// Start showing a terminal on `machine` at `size` (cols, rows); what it prints goes to `to`.
    pub fn open(ed: &mut Editor, machine: &str, open: Open, size: (u16, u16), to: mpsc::Sender<PtyMsg>) -> Link {
        let killed = Arc::new(AtomicBool::new(false));
        let named: Arc<Mutex<Option<String>>> = Default::default();
        let wires = ed.remote.wires.clone();
        let shown = Arc::new(Mutex::new(size));
        let link = Link { machine: machine.to_string(), name: named.clone(), wires: wires.clone(), to: to.clone(), size: shown.clone(), online: false, info: None, killed: killed.clone() };
        let m = machine.to_string();
        match open {
            Open::Attach { name } => {
                *named.lock().unwrap() = Some(name.clone());
                wires.show(&m, &name, to.clone());
                attach(ed, &m, &name, shown, true, to);
            }
            Open::New { cwd, integration } => {
                let request = Request::NewTerminal { cwd, cols: size.0, rows: size.1, integration };
                crate::remote::request(ed, machine, request, move |ed, r| match r {
                    Ok(Response::Terminal { name }) if killed.load(Ordering::Relaxed) => crate::remote::send(ed, &m, Request::KillTerminal { name }),
                    Ok(Response::Terminal { name }) => {
                        *named.lock().unwrap() = Some(name.clone());
                        wires.show(&m, &name, to.clone());
                        attach(ed, &m, &name, shown, false, to);
                    }
                    Ok(other) => fail(&to, &format!("{m} answered {other:?}")),
                    Err(e) => fail(&to, &e),
                });
            }
        }
        link
    }

    /// Its name there, once it has been started.
    pub fn name(&self) -> Option<String> {
        self.name.lock().unwrap().clone()
    }

    /// Where its bytes go.
    pub fn input(&self) -> Input {
        Input { wires: self.wires.clone(), machine: self.machine.clone(), name: self.name.clone() }
    }

    /// Shown at another size.
    pub fn resize(&mut self, cols: u16, rows: u16) {
        *self.size.lock().unwrap() = (cols, rows);
        if let Some(name) = self.name() {
            self.wires.request(&self.machine, Request::ResizeTerminal { name, cols, rows });
        }
    }

    /// The buffer is going: end the terminal there, or (quitting) just stop showing it.
    pub fn close(&self, ed: &mut Editor, end: bool) {
        self.killed.store(end, Ordering::Relaxed);
        if let Some(name) = self.name() {
            self.wires.hide(&self.machine, &name);
            crate::remote::send(ed, &self.machine, if end { Request::KillTerminal { name } } else { Request::DetachTerminal { name } });
        }
    }
}

/// Why a terminal couldn't be shown, in the terminal itself.
fn fail(to: &mpsc::Sender<PtyMsg>, why: &str) {
    let _ = to.send(PtyMsg::Data(format!("\r\n\x1b[31m{}\x1b[0m\r\n", why.replace('\n', "\r\n")).into_bytes()));
    let _ = to.send(PtyMsg::Exit);
}

/// Show `name` of `machine`: what it prints arrives from now on, starting with its screen (and,
/// for a fresh view, what it printed before).
fn attach(ed: &mut Editor, machine: &str, name: &str, size: Arc<Mutex<(u16, u16)>>, history: bool, to: mpsc::Sender<PtyMsg>) {
    // The size it is shown at by now.
    let (cols, rows) = *size.lock().unwrap();
    let request = Request::AttachTerminal { name: name.to_string(), cols, rows, history };
    crate::remote::request(ed, machine, request, move |ed, r| match r {
        Ok(_) => {
            let _ = to.send(PtyMsg::Online(true));
            ed.main_sender().repaint();
        }
        // Ended there meanwhile.
        Err(e) if e.contains("no terminal") => {
            let _ = to.send(PtyMsg::Exit);
        }
        Err(e) => fail(&to, &e),
    });
}

/// Whether buffers show terminals of `machine` (it is reconnected for them, as for chats).
pub fn shown_on(ed: &Editor, machine: &str) -> bool {
    linked(ed).iter().any(|(_, m, _)| m == machine)
}

/// `machine` is back: its terminals are attached again (their screens come again; what they
/// printed meanwhile is on them).
pub fn reattach(ed: &mut Editor, machine: &str) {
    let mut again = Vec::new();
    for s in ed.buffers.values() {
        if let Some(link) = s.buf.as_ref().and_then(|b| b.as_any().downcast_ref::<TerminalBuffer>()).and_then(TerminalBuffer::link)
            && link.machine == machine
            && let Some(name) = link.name()
        {
            again.push((name, link.size.clone(), link.to.clone()));
        }
    }
    for (name, size, to) in again {
        attach(ed, machine, &name, size, false, to);
    }
}

/// What each machine's terminals are doing, asked for while buffers show them.
#[derive(Default)]
pub struct Polls {
    machines: HashMap<String, Poll>,
}

#[derive(Default)]
struct Poll {
    asked: Option<Instant>,
    waiting: bool,
    terminals: Vec<TerminalInfo>,
}

/// The terminal buffers that show terminals of machines: (buffer, machine, name).
fn linked(ed: &Editor) -> Vec<(BufferId, String, Option<String>)> {
    ed.buffers
        .iter()
        .filter_map(|(id, s)| {
            let link = s.buf.as_ref()?.as_any().downcast_ref::<TerminalBuffer>()?.link()?;
            Some((id, link.machine.clone(), link.name()))
        })
        .collect()
}

/// Ask the machines whose terminals buffers show what those are doing, every
/// `terminal-poll-seconds`.
pub fn tick(ed: &mut Editor) {
    let every = Duration::from_secs_f64(ed.var("terminal-poll-seconds").and_then(Value::as_f64).unwrap_or(1.0).max(0.2));
    let mut machines: Vec<String> = linked(ed).into_iter().map(|(_, m, _)| m).collect();
    machines.sort();
    machines.dedup();
    for m in machines {
        // A machine that can't be reached is tried again now and then, not every second.
        let pause = if crate::remote::is_up(ed, &m) { every } else { Duration::from_secs(10) };
        let p = ed.remote.terminals.machines.entry(m.clone()).or_default();
        if p.waiting || p.asked.is_some_and(|t| t.elapsed() < pause) {
            continue;
        }
        let p = ed.remote.terminals.machines.get_mut(&m).unwrap();
        p.waiting = true;
        p.asked = Some(Instant::now());
        let m2 = m.clone();
        crate::remote::request(ed, &m, Request::Terminals, move |ed, r| {
            let p = ed.remote.terminals.machines.entry(m2).or_default();
            p.waiting = false;
            if let Ok(Response::Terminals { terminals }) = r {
                p.terminals = terminals;
            }
        });
    }
}

/// What `machine` last said about its terminal `name`.
pub fn info(ed: &Editor, machine: &str, name: &str) -> Option<TerminalInfo> {
    ed.remote.terminals.machines.get(machine)?.terminals.iter().find(|t| t.name == name).cloned()
}

/// The terminals shown now, to show again next time (`session-restore-on-start`).
pub fn open_terminals(ed: &Editor) -> Value {
    let list: Vec<Value> = linked(ed).into_iter().filter_map(|(_, m, n)| Some(json!({"machine": m, "name": n?}))).collect();
    Value::Array(list)
}

/// Show the terminals that were shown last time, if they still run (each machine is asked).
pub fn reopen(ed: &mut Editor, list: &Value) {
    let mut wanted: HashMap<String, Vec<String>> = HashMap::new();
    for v in list.as_array().into_iter().flatten() {
        if let (Some(m), Some(n)) = (v["machine"].as_str(), v["name"].as_str()) {
            wanted.entry(m.to_string()).or_default().push(n.to_string());
        }
    }
    for (m, names) in wanted {
        let m2 = m.clone();
        crate::remote::request(ed, &m, Request::Terminals, move |ed, r| {
            let Ok(Response::Terminals { terminals }) = r else { return };
            let shown = linked(ed);
            for n in names.iter().filter(|n| terminals.iter().any(|t| &t.name == *n)) {
                if shown.iter().any(|(_, sm, sn)| *sm == m2 && sn.as_deref() == Some(n.as_str())) {
                    continue;
                }
                if let Err(e) = attach_buffer(ed, &m2, n, false) {
                    tracing::warn!("reopening terminal {m2}/{n}: {e:#}");
                }
            }
        });
    }
}

/// A buffer showing the terminal `name` of `machine` (the one that does, if any).
pub fn attach_buffer(ed: &mut Editor, machine: &str, name: &str, show: bool) -> anyhow::Result<BufferId> {
    let existing = linked(ed).into_iter().find(|(_, m, n)| m == machine && n.as_deref() == Some(name)).map(|(id, ..)| id);
    let id = match existing {
        Some(id) => id,
        None => ed.create_buffer("terminal", Some("*terminal*"), &json!({"machine": machine, "terminal": name}))?,
    };
    if show {
        ed.show_buffer(id);
    }
    Ok(id)
}

/// A new terminal on `machine` in `dir` there, shown.
pub fn new_terminal(ed: &mut Editor, machine: &str, dir: &str) -> anyhow::Result<BufferId> {
    let id = ed.create_buffer("terminal", Some("*terminal*"), &json!({"machine": machine, "cwd": dir}))?;
    ed.show_buffer(id);
    ed.sidebar.focused = false;
    Ok(id)
}

/// Pick one of the terminals running on this machine and the ones connected (`terminal-attach`).
fn pick(ed: &mut Editor, machines: Vec<String>) {
    let shown = linked(ed);
    let pending = Arc::new(std::sync::Mutex::new(machines.len()));
    let found: Arc<std::sync::Mutex<Vec<(String, TerminalInfo)>>> = Default::default();
    for m in machines {
        let (pending, found, shown) = (pending.clone(), found.clone(), shown.clone());
        let m2 = m.clone();
        crate::remote::request(ed, &m, Request::Terminals, move |ed, r| {
            if let Ok(Response::Terminals { terminals }) = r {
                let fresh = terminals.into_iter().filter(|t| !shown.iter().any(|(_, sm, sn)| *sm == m2 && sn.as_deref() == Some(&t.name)));
                found.lock().unwrap().extend(fresh.map(|t| (m2.clone(), t)));
            }
            let mut left = pending.lock().unwrap();
            *left -= 1;
            if *left > 0 {
                return;
            }
            let list = std::mem::take(&mut *found.lock().unwrap());
            if list.is_empty() {
                return ed.message("No other terminals are running");
            }
            let cands = list
                .iter()
                .map(|(m, t)| {
                    let here = if m == crate::remote::LOCAL { "here".to_string() } else { format!("on {m}") };
                    let doing = t.foreground.first().map(|p| p.rsplit('/').next().unwrap_or(p).to_string()).unwrap_or_else(|| "shell".into());
                    let label = format!("{} {here}", t.name);
                    let detail = format!("{doing} · {} · {}", t.title, t.cwd.clone().unwrap_or_default());
                    crate::minibuffer::Candidate::new(label).detail(detail).value(format!("{m}\n{}", t.name))
                })
                .collect();
            ed.completing_read("Attach to terminal: ", cands, true, |ed, c, _| {
                let Some(c) = c else { return };
                let Some((m, n)) = c.value.split_once('\n') else { return };
                if let Err(e) = attach_buffer(ed, m, n, true) {
                    ed.error(format!("{e:#}"));
                }
            });
        });
    }
}

pub fn install(ed: &mut Editor) {
    let o = crate::editor::BUILTIN;
    ed.defvar("terminal-persistent", json!(false), "Terminals on this machine run in its terminal host (serpis-node terminals), so they outlive the window and other devices reach them; off (the default), a terminal is a child of the window, which is quickest, and ends with it. Terminals on other machines always run in theirs.");
    ed.defvar("terminal-poll-seconds", json!(1.0), "How often machines are asked what their shown terminals are doing (the program in front, for activity).");
    ed.defvar("terminal-follow-machine", json!(true), "new-terminal opens on the machine of the buffer you are in (a chat or terminal on another machine), in its folder there.");
    ed.defvar("chat-follow-machine", json!(true), "new-chat opens on the machine of the buffer you are in (a chat or terminal on another machine), in its folder there.");
    ed.define_command("remote-new-terminal", "Open a terminal on another machine: pick the machine, then a folder there (params: machine, folder).", o, |ed, a| {
        let (machine, dir) = (a.str(0).map(str::to_string), a.str(1).map(str::to_string));
        let open = |ed: &mut Editor, m: &str, d: &str| {
            if let Err(e) = new_terminal(ed, m, d) {
                ed.error(format!("{e:#}"));
            }
        };
        match (machine, dir) {
            (Some(m), Some(d)) => open(ed, &m, &d),
            (Some(m), None) => crate::remote::pick_dir(ed, &m.clone(), move |ed, d| open(ed, &m, &d)),
            _ => crate::remote::pick_machine(ed, "Machine: ", false, move |ed, m| crate::remote::pick_dir(ed, &m.clone(), move |ed, d| open(ed, &m, &d))),
        }
        Ok(())
    });
    ed.define_command("terminal-attach", "Show a terminal that runs on this machine or a connected one but no buffer shows (started elsewhere, or with mm). With C-u, pick the machine first (params: machine, name).", o, |ed, a| {
        if let (Some(m), Some(n)) = (a.str(0), a.str(1)) {
            attach_buffer(ed, m, n, true)?;
            return Ok(());
        }
        if a.prefix.is_some() {
            crate::remote::pick_machine(ed, "Terminals of: ", true, |ed, m| pick(ed, vec![m]));
            return Ok(());
        }
        let mut machines = vec![crate::remote::LOCAL.to_string()];
        machines.extend(crate::remote::connected_machines(ed).into_iter().filter(|m| m != crate::remote::LOCAL));
        pick(ed, machines);
        Ok(())
    });
}
