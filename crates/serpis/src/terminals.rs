//! Terminals a machine's terminal node holds (manymux's: `serpis-node terminals`, or an `mm`
//! running there), shown in terminal buffers. The machine's chat node starts, lists and ends
//! them over the connection the window already has; the bytes of each go over a stream of its
//! own, straight to the terminal node (`serpis-node agent` with `SERPIS_BRIDGE=terminals`, over
//! the same ssh as the chats). A terminal outlives the window and the connection: one that
//! drops is attached again, and the next serpis to start shows it again.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use manymux::client::{Stream, Update};
use manymux::proto::Size;
use serde_json::{Value, json};
use serpis_proto::wire::{Request, Response, TerminalInfo};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};

use crate::buffer::BufferId;
use crate::buffers::terminal::{PtyMsg, TerminalBuffer};
use crate::editor::Editor;

/// What a terminal buffer sends its terminal.
pub enum Out {
    Input(Vec<u8>),
    Resize(u16, u16),
    /// Stop showing it (it keeps running there).
    Detach,
}

/// A terminal buffer's terminal on a machine.
pub struct Link {
    pub machine: String,
    /// Its name there, once it has been started (set as the machine answers, so a buffer killed
    /// at any moment after that ends it).
    name: Arc<std::sync::Mutex<Option<String>>>,
    pub out: UnboundedSender<Out>,
    /// Attached now (false while connecting, and while a dropped connection is retried).
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
    /// A terminal running there already, with this many lines of what it printed before.
    Attach { name: String, history: u32 },
}

impl Link {
    /// Start showing a terminal on `machine` at `size` (cols, rows); what it prints goes to `to`.
    pub fn open(ed: &mut Editor, machine: &str, open: Open, size: (u16, u16), to: mpsc::Sender<PtyMsg>) -> Link {
        let (out, rx) = unbounded_channel();
        let killed = Arc::new(AtomicBool::new(false));
        let named: Arc<std::sync::Mutex<Option<String>>> = Default::default();
        let link = Link { machine: machine.to_string(), name: named.clone(), out, online: false, info: None, killed: killed.clone() };
        let m = machine.to_string();
        match open {
            Open::Attach { name, history } => {
                *named.lock().unwrap() = Some(name.clone());
                attach(ed, &m, name, history, size, rx, to);
            }
            Open::New { cwd, integration } => {
                let request = Request::NewTerminal { cwd, cols: size.0, rows: size.1, integration };
                crate::remote::request(ed, machine, request, move |ed, r| match r {
                    Ok(Response::Terminal { name }) if killed.load(Ordering::Relaxed) => crate::remote::send(ed, &m, Request::KillTerminal { name }),
                    Ok(Response::Terminal { name }) => {
                        *named.lock().unwrap() = Some(name.clone());
                        attach(ed, &m, name, 0, size, rx, to);
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

    pub fn send(&self, out: Out) {
        let _ = self.out.send(out);
    }

    /// The buffer is going: end the terminal there.
    pub fn kill(&self, ed: &mut Editor) {
        self.killed.store(true, Ordering::Relaxed);
        if let Some(name) = self.name() {
            crate::remote::send(ed, &self.machine, Request::KillTerminal { name });
        }
        self.send(Out::Detach);
    }
}

/// Why a terminal couldn't be shown, in the terminal itself.
fn fail(to: &mpsc::Sender<PtyMsg>, why: &str) {
    let _ = to.send(PtyMsg::Data(format!("\r\n\x1b[31m{}\x1b[0m\r\n", why.replace('\n', "\r\n")).into_bytes()));
    let _ = to.send(PtyMsg::Exit);
}

/// Attach to `name` on `machine` once its node is reached, and keep it attached.
fn attach(ed: &mut Editor, machine: &str, name: String, history: u32, size: (u16, u16), rx: UnboundedReceiver<Out>, to: mpsc::Sender<PtyMsg>) {
    let m = machine.to_string();
    crate::remote::with_client(ed, machine, move |ed, c| {
        let target = match c {
            Ok(_) => crate::remote::current_target(ed, &m),
            Err(e) => return fail(&to, &e),
        };
        let Some(target) = target else { return fail(&to, &format!("no way to reach {m}'s terminals")) };
        let main = ed.main_sender();
        ed.rt.spawn(pump(target, name, history, size, rx, to, main));
    });
}

/// Keep `name` attached until it ends or the buffer lets go of it, attaching again after drops.
async fn pump(target: serpis_node::client::Target, name: String, mut history: u32, mut size: (u16, u16), mut rx: UnboundedReceiver<Out>, to: mpsc::Sender<PtyMsg>, main: crate::editor::MainSender) {
    let tell = |m: PtyMsg| {
        let _ = to.send(m);
        main.repaint();
    };
    let mut failures = 0u32;
    loop {
        let attempt = async {
            let pipe = serpis_node::client::terminal_pipe(&target).await?;
            let (stderr, done) = (pipe.stderr.clone(), pipe.stderr_done);
            let attached = Stream::from_halves(pipe.read, pipe.write).attach(&name, Size::new(size.0, size.1), history, false).await;
            match attached {
                Ok(a) => Ok((a, pipe.child)),
                // What the far side said is the reason, when it said anything.
                Err(e) => Err(serpis_node::client::Pipe::why(stderr, done).await.map(|w| anyhow::anyhow!("{w}")).unwrap_or(e)),
            }
        };
        let (attached, _child) = match attempt.await {
            Ok(a) => a,
            Err(e) => {
                let why = format!("{e:#}");
                // Ended there meanwhile.
                if why.contains("no session named") {
                    return tell(PtyMsg::Exit);
                }
                tracing::warn!("terminal {name}: {why}");
                failures += 1;
                let pause = Duration::from_millis(500 * 2u64.pow(failures.min(5))).min(Duration::from_secs(10));
                // Waiting, the buffer may still resize or let go.
                let until = tokio::time::Instant::now() + pause;
                loop {
                    tokio::select! {
                        _ = tokio::time::sleep_until(until) => break,
                        o = rx.recv() => match o {
                            Some(Out::Resize(c, r)) => size = (c, r),
                            Some(Out::Input(_)) => {}
                            None | Some(Out::Detach) => return,
                        },
                    }
                }
                continue;
            }
        };
        failures = 0;
        history = 0;
        tell(PtyMsg::Online(true));
        let mut h = attached.split();
        loop {
            tokio::select! {
                u = h.reader.next() => match u {
                    Ok(Update::Output(b) | Update::Screen(b) | Update::History(b)) => tell(PtyMsg::Data(b)),
                    Ok(Update::Ping) => {
                        if h.writer.pong().await.is_err() {
                            break;
                        }
                    }
                    Ok(Update::Exited(_)) => return tell(PtyMsg::Exit),
                    Ok(Update::Disconnected) | Err(_) => break,
                    Ok(_) => {}
                },
                o = rx.recv() => match o {
                    Some(Out::Input(b)) => {
                        if h.writer.send_input(&b).await.is_err() {
                            break;
                        }
                    }
                    Some(Out::Resize(c, r)) => {
                        size = (c, r);
                        let _ = h.writer.resize(Size::new(c, r)).await;
                    }
                    None | Some(Out::Detach) => {
                        let _ = h.writer.detach().await;
                        return;
                    }
                },
            }
        }
        tell(PtyMsg::Online(false));
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
    ed.defvar("terminal-persistent", json!(true), "Terminals on this machine run in its terminal node (manymux's, shared with mm), so they outlive the window and other devices reach them; off, a terminal is a child of the window and ends with it. Terminals on other machines always run in theirs.");
    ed.defvar("terminal-poll-seconds", json!(1.0), "How often machines are asked what their shown terminals are doing (the program in front, for activity).");
    ed.defvar("terminal-history-lines", json!(5000), "Lines of what a terminal printed before it was attached, for its scrollback.");
    ed.defvar("terminal-follow-machine", json!(true), "new-terminal opens on the machine of the buffer you are in (a chat or terminal on another machine), in its folder there.");
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
