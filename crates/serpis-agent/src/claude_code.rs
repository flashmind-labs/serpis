//! Claude Code as a chat's agent: the `claude` CLI runs the turn (its own harness, tools,
//! permissions, compaction and login), and its output comes back as the same [`AgentEvent`]s
//! serpis's agent loop emits, so a chat shows, records and resumes it like any other.
//!
//! One `claude -p --input-format stream-json --output-format stream-json` process serves a
//! chat across turns, as Anthropic's Agent SDKs run it. Its session id is the chat's, so a
//! resumed chat resumes Claude Code's own session (`--resume`). Tools are shown under serpis's
//! names where one matches (`Bash` → `bash`, `Read` → `read` with `path`, ...), so tool cards,
//! diffs, hooks and scripts treat them alike. Permission prompts go to `before-tool-call`
//! handlers, then to `claude-code-permission-prompts`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use serpis_ai::api::anthropic_messages::EventReader;
use serpis_ai::stream::Emitter;
use serpis_ai::*;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::Child;
use tokio::sync::mpsc;

use crate::agent::{AgentEvent, AgentHandle, Answer, AskReply, EmitFn, ToolGate, ToolHooks};

/// How the `claude` process is started. A run with a different launch restarts it (resuming
/// its session), e.g. after the chat's model or thinking level changed.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Launch {
    pub binary: PathBuf,
    pub cwd: PathBuf,
    /// What `--model` takes (an alias or a model name).
    pub model: String,
    /// `--effort`; None leaves it to Claude Code.
    pub effort: Option<String>,
    /// `--permission-mode`: default, acceptEdits, plan, auto, dontAsk, bypassPermissions.
    pub permission_mode: String,
    /// `--thinking-display`: summarized (its thinking is sent as a summary), omitted or
    /// highlights; None leaves it to Claude Code (which leaves thinking out of the stream).
    pub thinking_display: Option<String>,
    /// More arguments (`claude-code-args`).
    pub args: Vec<String>,
    /// What its system prompt is told of serpis's skills (appended with `--append-system-prompt`,
    /// after any of `args`). Read when it starts: a running `claude` keeps the list it started
    /// with, unless `skills_restart`.
    pub skills: Option<String>,
    /// A changed `skills` restarts it (resuming its session) with the next run.
    pub skills_restart: bool,
    pub env: Vec<(String, String)>,
}

pub struct RunConfig {
    pub launch: Launch,
    /// The chat's id, also Claude Code's session id.
    pub session_id: String,
    /// The chat's model (what replies are recorded as).
    pub model: Model,
    /// Asked first about each permission prompt (`before-tool-call`).
    pub hooks: Option<Arc<dyn ToolHooks>>,
    /// Who answers a permission prompt no hook blocked.
    pub prompts: Prompts,
    /// How often what its commands print is read while they run (None: not read).
    pub task_output_every: Option<Duration>,
}

/// Who answers Claude Code's permission prompts (`claude-code-permission-prompts`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Prompts {
    /// The person, from any client showing the chat (a [`AgentEvent::ToolAsk`]).
    #[default]
    Ask,
    Allow,
    Deny,
}

impl Prompts {
    pub fn parse(s: &str) -> Option<Prompts> {
        match s {
            "ask" => Some(Prompts::Ask),
            "allow" => Some(Prompts::Allow),
            "deny" => Some(Prompts::Deny),
            _ => None,
        }
    }
}

/// A chat's Claude Code: the process, started with the first run and kept between runs.
/// Between runs it is still read: its background work reports in, and when that work is done
/// Claude Code takes a turn of its own ([`AgentEvent::Adopted`]).
#[derive(Clone, Default)]
pub struct ClaudeCode {
    proc: Arc<tokio::sync::Mutex<Option<Process>>>,
    /// The watch between runs, ended when a run starts.
    watching: Arc<Mutex<Option<CancellationToken>>>,
    /// Lines to write to it, without its lock (asking it to stop a background task).
    writer: Arc<Mutex<Option<mpsc::UnboundedSender<String>>>>,
    /// Its pid, to stop it without its lock (a run or the watch holds that).
    pid: Arc<Mutex<Option<u32>>>,
    /// Stopped while locked: the next run starts it again.
    stale: Arc<std::sync::atomic::AtomicBool>,
}

/// Whether `m` is a reply Claude Code wrote (so its session holds what came before it).
pub fn is_reply(m: &Message) -> bool {
    matches!(m, Message::Assistant(a) if a.provider == serpis_ai::claude_code::PROVIDER)
}

impl ClaudeCode {
    /// Run a turn for `prompts` (none: continue a cut-off turn). `history` is the chat so far:
    /// whatever Claude Code's session hasn't seen of it (turns another model answered) goes
    /// along with the first prompt.
    pub fn run(&self, history: Vec<Message>, prompts: Vec<Message>, cfg: RunConfig, emit: EmitFn) -> AgentHandle {
        let handle = AgentHandle::new();
        if let Some(w) = self.watching.lock().unwrap().take() {
            w.cancel();
        }
        let (h, me) = (handle.clone(), self.clone());
        tokio::spawn(async move {
            emit(AgentEvent::AgentStart);
            let cfg = Arc::new(cfg);
            let error = me.drive(history, prompts, &cfg, &h, &emit).await.err().map(|e| format!("{e:#}"));
            // Watching before the chat hears the run ended (and may start the next).
            me.watch(cfg, emit.clone());
            emit(AgentEvent::AgentEnd { error });
        });
        handle
    }

    /// Switch its permission mode now (between runs or in one). False when there is no
    /// process: the next run starts in the mode its launch says.
    pub fn set_permission_mode(&self, mode: &str) -> bool {
        let line = json!({"type": "control_request", "request_id": format!("serpis-mode-{mode}"), "request": {"subtype": "set_permission_mode", "mode": mode}});
        self.writer.lock().unwrap().as_ref().is_some_and(|w| w.send(format!("{line}\n")).is_ok())
    }

    /// Ask Claude Code to stop background task `task` (a command, a subagent). False when
    /// there is no process to ask.
    pub fn stop_task(&self, task: &str) -> bool {
        let line = json!({"type": "control_request", "request_id": format!("serpis-stop-{task}"), "request": {"subtype": "stop_task", "task_id": task}});
        self.writer.lock().unwrap().as_ref().is_some_and(|w| w.send(format!("{line}\n")).is_ok())
    }

    /// Read what it says between runs: its tasks' progress, background subagents' steps,
    /// their permission prompts, and the turns it takes of its own accord, which are run as
    /// runs the chat adopts. Ends when a run starts or the process does.
    fn watch(&self, cfg: Arc<RunConfig>, emit: EmitFn) {
        let token = CancellationToken::new();
        *self.watching.lock().unwrap() = Some(token.clone());
        let me = self.clone();
        tokio::spawn(async move {
            let mut guard = me.proc.lock().await;
            loop {
                let Some(p) = guard.as_mut().filter(|_| !token.is_cancelled()) else { return };
                let v = tokio::select! {
                    v = p.next_line() => v,
                    _ = token.cancelled() => return,
                };
                let Some(v) = v else { return };
                if p.aside(&v, &cfg.model, &emit) {
                    continue;
                }
                match v["type"].as_str().unwrap_or("") {
                    "control_request" => {
                        if p.respond(&v, &cfg, &AgentHandle::new(), &emit, false).await.is_err() {
                            return;
                        }
                    }
                    // A turn it started: as a run, which the chat adopts.
                    "stream_event" | "assistant" => {
                        let h = AgentHandle::new();
                        emit(AgentEvent::Adopted { handle: h.clone(), why: p.finished.take() });
                        emit(AgentEvent::AgentStart);
                        p.pending.push_front(v);
                        let error = turns(p, VecDeque::new(), None, true, &cfg, &h, &emit).await.err().map(|e| format!("{e:#}"));
                        emit(AgentEvent::AgentEnd { error });
                    }
                    _ => {}
                }
            }
        });
    }

    /// Stop the process (the chat closed, or starts over).
    pub fn stop(&self) {
        if let Some(w) = self.watching.lock().unwrap().take() {
            w.cancel();
        }
        *self.writer.lock().unwrap() = None;
        match self.proc.try_lock() {
            Ok(mut p) => {
                if let Some(p) = p.as_mut() {
                    p.end(Duration::from_secs(3));
                }
                *p = None;
            }
            Err(_) => {
                if let Some(pid) = self.pid.lock().unwrap().take() {
                    end_tree(pid, Duration::from_secs(3));
                }
                self.stale.store(true, std::sync::atomic::Ordering::SeqCst);
            }
        }
    }

    async fn drive(&self, history: Vec<Message>, prompts: Vec<Message>, cfg: &RunConfig, h: &AgentHandle, emit: &EmitFn) -> Result<()> {
        let mut guard = self.proc.lock().await;
        let resume = history.iter().any(is_reply);
        let mut fresh = !resume;
        let stale = self.stale.swap(false, std::sync::atomic::Ordering::SeqCst);
        // A new permission mode is switched to, not restarted for.
        // Its mode is switched in place; serpis's skills are read once, when it starts (unless
        // told to restart for them).
        let same = |l: &Launch| {
            let skills = if cfg.launch.skills_restart { l.skills.clone() } else { cfg.launch.skills.clone() };
            Launch { permission_mode: cfg.launch.permission_mode.clone(), skills, ..l.clone() } == cfg.launch
        };
        if stale || guard.as_mut().is_none_or(|p| !same(&p.launch) || p.exited()) {
            if let Some(old) = guard.take() {
                *self.writer.lock().unwrap() = None;
                old.stop().await;
            }
            *guard = Some(match Process::start(cfg, resume).await {
                Ok(p) => p,
                // Its session isn't on this machine (the chat moved here): a new one, told
                // what was said.
                Err(e) if resume && format!("{e:#}").contains("No conversation found") => {
                    fresh = true;
                    Process::start(cfg, false).await?
                }
                Err(e) => return Err(e),
            });
        }
        let p = guard.as_mut().expect("started");
        if p.mode != cfg.launch.permission_mode {
            p.request(json!({"subtype": "set_permission_mode", "mode": cfg.launch.permission_mode})).await?;
            p.mode = cfg.launch.permission_mode.clone();
        }
        *self.writer.lock().unwrap() = Some(p.out.clone());
        *self.pid.lock().unwrap() = p.child.id();
        let earlier = if fresh { &history[..] } else { unseen(&history) };
        let context = replay(earlier);
        let mut queue: VecDeque<Message> = prompts.into();
        if queue.is_empty() {
            queue.push_back(Message::User { content: vec![UserContent::Text { text: CONTINUE.into() }], timestamp: now_ms() });
        }
        turns(p, queue, context, false, cfg, h, emit).await
    }
}

/// Turns, one per message of `queue` (the first sent with `context`), and then one per
/// message queued while they ran: Claude Code takes one message at a time. `going`: a turn
/// it started itself is under way, read first.
async fn turns(p: &mut Process, mut queue: VecDeque<Message>, mut context: Option<String>, mut going: bool, cfg: &RunConfig, h: &AgentHandle, emit: &EmitFn) -> Result<()> {
    let mut late = false;
    loop {
        if !going {
            let Some(msg) = queue.pop_front() else { return Ok(()) };
            emit(AgentEvent::MessageStart(msg.clone()));
            emit(AgentEvent::MessageEnd(msg.clone()));
            p.send(&user_line(&msg, context.take())).await?;
        }
        going = false;
        let stopped = p.turn(cfg, h, emit).await?;
        emit(AgentEvent::TurnEnd);
        if stopped {
            return Ok(());
        }
        // Sent while it worked but too late for that turn: it takes them as its next one.
        if !p.steers.is_empty() && !late {
            late = true;
            going = true;
            continue;
        }
        // (Never said taken in: kept as sent, rather than waited on.)
        for (_, msg) in p.steers.drain(..) {
            emit(AgentEvent::MessageStart(msg.clone()));
            emit(AgentEvent::MessageEnd(msg));
        }
        late = false;
        let (steer, follow) = h.take_queued();
        queue.extend(steer.into_iter().chain(follow));
    }
}

/// Stop process `pid` and its descendants: SIGTERM to each now (so a script's cleanup runs),
/// SIGKILL after `grace` to those still there. Descendants are found by parent, not process
/// group: the commands `claude` runs may be in groups of their own. A process is killed only
/// if it is still the one found (same start time), never one that reused its pid.
fn end_tree(pid: u32, grace: Duration) {
    end_processes(process_tree(pid), grace);
}

/// SIGTERM to each of `found` still running, SIGKILL after `grace` to those left.
fn end_processes(found: Vec<(i32, String)>, grace: Duration) {
    #[cfg(unix)]
    {
        use nix::sys::signal::{Signal, kill};
        use nix::unistd::Pid;
        let now: HashMap<i32, String> = processes().into_iter().map(|(p, _, start)| (p, start)).collect();
        let found: Vec<(i32, String)> = found.into_iter().filter(|(p, start)| now.get(p) == Some(start)).collect();
        if found.is_empty() {
            return;
        }
        for (p, _) in &found {
            let _ = kill(Pid::from_raw(*p), Signal::SIGTERM);
        }
        std::thread::spawn(move || {
            std::thread::sleep(grace);
            let now: HashMap<i32, String> = processes().into_iter().map(|(p, _, start)| (p, start)).collect();
            for (p, start) in found {
                if now.get(&p) == Some(&start) {
                    let _ = kill(Pid::from_raw(p), Signal::SIGKILL);
                }
            }
        });
    }
}

/// Every process: (pid, parent, start time), from `ps`.
fn processes() -> Vec<(i32, i32, String)> {
    let Ok(out) = std::process::Command::new("ps").args(["-A", "-o", "pid=", "-o", "ppid=", "-o", "lstart="]).output() else { return vec![] };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| {
            let mut it = l.split_whitespace();
            let (pid, ppid) = (it.next()?.parse().ok()?, it.next()?.parse().ok()?);
            Some((pid, ppid, it.collect::<Vec<_>>().join(" ")))
        })
        .collect()
}

/// `pid` and its descendants, with their start times.
fn process_tree(pid: u32) -> Vec<(i32, String)> {
    let all = processes();
    let mut found: Vec<(i32, String)> = all.iter().filter(|(p, ..)| *p == pid as i32).map(|(p, _, s)| (*p, s.clone())).collect();
    let mut i = 0;
    while i < found.len() {
        let parent = found[i].0;
        found.extend(all.iter().filter(|(_, pp, _)| *pp == parent).map(|(p, _, s)| (*p, s.clone())));
        i += 1;
    }
    found
}

/// What a continued turn is sent (its node stopped while Claude Code worked).
pub const CONTINUE: &str = "Continue where you left off: serpis stopped while you were working, so the last step may not have finished. Check before redoing it.";

/// The history after Claude Code's last reply (and that reply's tool results): what other
/// models said since, which its session lacks.
fn unseen(history: &[Message]) -> &[Message] {
    let Some(last) = history.iter().rposition(is_reply) else { return history };
    let rest = &history[last + 1..];
    let results = rest.iter().take_while(|m| matches!(m, Message::ToolResult(_))).count();
    &rest[results..]
}

/// Earlier turns as text, for a Claude Code session that didn't see them.
fn replay(messages: &[Message]) -> Option<String> {
    const CAP: usize = 2000;
    let cut = |s: &str| if s.len() > CAP { format!("{}… [{} more bytes]", &s[..s.floor_char_boundary(CAP)], s.len() - CAP) } else { s.to_string() };
    let mut out = Vec::new();
    for m in messages {
        match m {
            Message::User { content, .. } => {
                let text: Vec<&str> = content.iter().filter_map(|c| if let UserContent::Text { text } = c { Some(text.as_str()) } else { None }).collect();
                out.push(format!("[user]\n{}", text.join("\n")));
            }
            Message::Assistant(a) => {
                let text = a.text();
                if !text.trim().is_empty() {
                    out.push(format!("[assistant]\n{text}"));
                }
                for c in a.tool_calls() {
                    out.push(format!("[tool call: {}]\n{}", c.name, cut(&c.arguments.to_string())));
                }
            }
            Message::ToolResult(r) => {
                let text: Vec<&str> = r.content.iter().filter_map(|c| if let UserContent::Text { text } = c { Some(text.as_str()) } else { None }).collect();
                out.push(format!("[tool result{}]\n{}", if r.is_error { ", error" } else { "" }, cut(&text.join("\n"))));
            }
        }
    }
    (!out.is_empty()).then(|| {
        format!(
            "This conversation began before you joined it: in serpis, with another model and its own tools. What was said so far:\n\n<conversation>\n{}\n</conversation>\n\nContinue from there; the new message follows.",
            out.join("\n\n")
        )
    })
}

/// A user message as a stream-json input line, `context` before its own text.
fn user_line(msg: &Message, context: Option<String>) -> Value {
    let mut blocks: Vec<Value> = context.into_iter().map(|c| json!({"type": "text", "text": c})).collect();
    if let Message::User { content, .. } = msg {
        for c in content {
            blocks.push(match c {
                UserContent::Text { text } => json!({"type": "text", "text": text}),
                UserContent::Image { data, mime_type } => json!({"type": "image", "source": {"type": "base64", "media_type": mime_type, "data": data}}),
            });
        }
    }
    json!({"type": "user", "message": {"role": "user", "content": blocks}, "parent_tool_use_id": null})
}

/// A Claude Code tool call under serpis's name and argument names, where serpis has the
/// same tool (cards, diffs and hooks know those); others keep Claude Code's.
pub fn serpis_tool(name: &str, args: &Value) -> (String, Value) {
    let renamed = |to: &str, path: bool| {
        let mut a = args.clone();
        if let (true, Some(o)) = (path, a.as_object_mut())
            && let Some(p) = o.remove("file_path") {
                o.insert("path".into(), p);
            }
        (to.to_string(), a)
    };
    match name {
        "Bash" => renamed("bash", false),
        "Read" => renamed("read", true),
        "Write" => renamed("write", true),
        "Edit" | "MultiEdit" => renamed("edit", true),
        "Grep" => renamed("grep", false),
        "Glob" => renamed("find", false),
        "LS" => renamed("ls", false),
        "WebSearch" => renamed("web_search", false),
        _ => (name.to_string(), args.clone()),
    }
}

/// [`serpis_tool`]'s arguments back as Claude Code's tool `name` takes them.
pub fn claude_input(name: &str, args: Value) -> Value {
    let mut args = args;
    if matches!(name, "Read" | "Write" | "Edit" | "MultiEdit")
        && let Some(o) = args.as_object_mut()
            && let Some(p) = o.remove("path") {
                o.insert("file_path".into(), p);
            }
    args
}

/// An edit's `structuredPatch` hunks as serpis's diff (`+12 code`, `-3 code`, ` 7 code`).
pub fn patch_diff(hunks: &[Value]) -> String {
    let lines = |h: &Value| h["lines"].as_array().cloned().unwrap_or_default();
    let max = hunks.iter().map(|h| h["oldStart"].as_u64().unwrap_or(1).max(h["newStart"].as_u64().unwrap_or(1)) as usize + lines(h).len()).max().unwrap_or(1);
    let w = max.to_string().len();
    let mut out = Vec::new();
    for (i, h) in hunks.iter().enumerate() {
        if i > 0 {
            out.push(format!(" {} ...", " ".repeat(w)));
        }
        let (mut old, mut new) = (h["oldStart"].as_u64().unwrap_or(1) as usize, h["newStart"].as_u64().unwrap_or(1) as usize);
        for l in lines(h) {
            let l = l.as_str().unwrap_or("");
            let (sign, text) = l.split_at(l.chars().next().map_or(0, char::len_utf8));
            match sign {
                "+" => {
                    out.push(format!("+{new:>w$} {text}"));
                    new += 1;
                }
                "-" => {
                    out.push(format!("-{old:>w$} {text}"));
                    old += 1;
                }
                "\\" => {}
                _ => {
                    out.push(format!(" {old:>w$} {text}"));
                    old += 1;
                    new += 1;
                }
            }
        }
    }
    out.join("\n")
}

struct Process {
    child: Child,
    /// Lines for its stdin (written in order by a task of their own).
    out: mpsc::UnboundedSender<String>,
    lines: mpsc::UnboundedReceiver<Value>,
    /// Lines read ahead, to be read again first.
    pending: VecDeque<Value>,
    /// Its tasks (background commands, subagents): the tool call that started each, and how
    /// it is going.
    tasks: HashMap<String, (String, serpis_proto::Task)>,
    /// What the last task to end did ("Background command finished: ..."): why it may take
    /// a turn of its own next.
    finished: Option<String>,
    /// Subagents' tool calls by id: serpis's name for each.
    nested_calls: HashMap<String, String>,
    /// Its permission mode, as it last said.
    mode: String,
    /// Messages sent while it worked (`priority: next`), by the uuid they went with, until
    /// it says it took them in (`command_lifecycle` started).
    steers: Vec<(String, Message)>,
    launch: Launch,
    /// It said it is compacting its context.
    compacting: bool,
    /// Its commands whose output is read (by task id), and whether each has ended (read once
    /// more, then dropped).
    watched: Arc<Mutex<HashMap<String, bool>>>,
    /// The end of what it wrote to stderr, for when it dies.
    stderr: Arc<Mutex<String>>,
    next_request: u64,
}

impl Process {
    async fn start(cfg: &RunConfig, resume: bool) -> Result<Process> {
        let l = &cfg.launch;
        let mut cmd = tokio::process::Command::new(&l.binary);
        cmd.args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--include-partial-messages"])
            .args(["--permission-prompt-tool", "stdio", "--model", &l.model, "--permission-mode", &l.permission_mode])
            .args([if resume { "--resume" } else { "--session-id" }, &cfg.session_id]);
        if let Some(e) = &l.effort {
            cmd.args(["--effort", e]);
        }
        if let Some(d) = &l.thinking_display {
            cmd.args(["--thinking-display", d]);
        }
        let mut args = l.args.clone();
        if let Some(text) = l.skills.as_ref().filter(|t| !t.is_empty()) {
            // Said once: with what `args` appends, if anything.
            match args.iter().position(|a| a == "--append-system-prompt").filter(|i| *i + 1 < args.len()) {
                Some(i) => args[i + 1] = format!("{}\n\n{text}", args[i + 1]),
                None => args.extend(["--append-system-prompt".to_string(), text.clone()]),
            }
        }
        cmd.args(&args).envs(l.env.iter().cloned()).current_dir(&l.cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn().with_context(|| format!("starting Claude Code ({})", l.binary.display()))?;
        let mut stdin = child.stdin.take().context("no stdin")?;
        let (writer, mut to_write) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(line) = to_write.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() {
                    break;
                }
            }
        });
        let (tx, lines) = mpsc::unbounded_channel();
        let mut out = BufReader::new(child.stdout.take().context("no stdout")?).lines();
        tokio::spawn(async move {
            while let Ok(Some(line)) = out.next_line().await {
                if let Ok(v) = serde_json::from_str::<Value>(&line)
                    && tx.send(v).is_err() {
                        break;
                    }
            }
        });
        let stderr = Arc::new(Mutex::new(String::new()));
        let (mut err, keep) = (child.stderr.take().context("no stderr")?, stderr.clone());
        tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok(n @ 1..) = err.read(&mut buf).await {
                let mut s = keep.lock().unwrap();
                s.push_str(&String::from_utf8_lossy(&buf[..n]));
                if s.len() > 8192 {
                    let cut = s.ceil_char_boundary(s.len() - 4096);
                    s.drain(..cut);
                }
            }
        });
        // What its commands print, read as they run (as its own /tasks view does); the answers
        // come back among its lines (`aside`).
        let watched: Arc<Mutex<HashMap<String, bool>>> = Default::default();
        if let Some(every) = cfg.task_output_every {
            let (watched, out) = (watched.clone(), writer.clone());
            tokio::spawn(async move {
                loop {
                    tokio::time::sleep(every).await;
                    let ids: Vec<String> = {
                        let mut w = watched.lock().unwrap();
                        let ids = w.keys().cloned().collect();
                        w.retain(|_, ended| !*ended);
                        ids
                    };
                    for id in ids {
                        let line = json!({"type": "control_request", "request_id": format!("{TASK_OUTPUT}{id}"), "request": {"subtype": "get_task_output", "task_id": id}});
                        if out.send(format!("{line}\n")).is_err() {
                            return;
                        }
                    }
                }
            });
        }
        let mut p = Process { child, out: writer, lines, pending: VecDeque::new(), tasks: HashMap::new(), finished: None, nested_calls: HashMap::new(), mode: l.permission_mode.clone(), steers: Vec::new(), launch: l.clone(), compacting: false, watched, stderr, next_request: 0 };
        p.initialize().await?;
        Ok(p)
    }

    /// The SDK handshake; fails when `claude` stops first (a session to resume that isn't
    /// there, not logged in, ...), with what it said.
    async fn initialize(&mut self) -> Result<()> {
        let id = self.request(json!({"subtype": "initialize"})).await?;
        let wait = async {
            while let Some(v) = self.lines.recv().await {
                match v["type"].as_str() {
                    Some("control_response") if v.pointer("/response/request_id").and_then(Value::as_str) == Some(id.as_str()) => {
                        return match v.pointer("/response/subtype").and_then(Value::as_str) {
                            Some("error") => Err(anyhow!("{}", v.pointer("/response/error").and_then(Value::as_str).unwrap_or("Claude Code refused to start"))),
                            _ => Ok(()),
                        };
                    }
                    Some("result") => return Err(anyhow!("{}", result_error(&v))),
                    _ => {}
                }
            }
            Err(anyhow!("Claude Code stopped"))
        };
        let r = tokio::time::timeout(Duration::from_secs(60), wait).await.unwrap_or_else(|_| Err(anyhow!("Claude Code didn't start within 60s")));
        r.map_err(|e| self.with_stderr(e))
    }

    fn with_stderr(&self, e: anyhow::Error) -> anyhow::Error {
        let tail = self.stderr.lock().unwrap().trim().to_string();
        if tail.is_empty() { e } else { anyhow!("{e:#}: {tail}") }
    }

    fn exited(&mut self) -> bool {
        !matches!(self.child.try_wait(), Ok(None))
    }

    async fn send(&mut self, v: &Value) -> Result<()> {
        self.out.send(format!("{v}\n")).map_err(|_| self.with_stderr(anyhow!("writing to Claude Code: it stopped")))
    }

    /// The next line it wrote (one read ahead first); None once it has stopped.
    async fn next_line(&mut self) -> Option<Value> {
        match self.pending.pop_front() {
            Some(v) => Some(v),
            None => self.lines.recv().await,
        }
    }

    /// A line beside the turn, handled here: a subagent's step, or how a task is going. False
    /// for the turn's own lines.
    fn aside(&mut self, v: &Value, model: &Model, emit: &EmitFn) -> bool {
        if let Some(parent) = v["parent_tool_use_id"].as_str() {
            self.nested(parent, v, model, emit);
            return true;
        }
        let sub = v["subtype"].as_str().unwrap_or("");
        if v["type"] == "system" && sub.starts_with("task_") {
            self.task(sub, v, emit);
            return true;
        }
        // How much it thinks, as it does (its thinking itself may not be sent).
        if v["type"] == "system" && sub == "thinking_tokens" {
            if let Some(d) = v["estimated_tokens_delta"].as_u64() {
                emit(AgentEvent::ThinkingTokens(d));
            }
            return true;
        }
        // How full its context is, as asked (`get_context_usage`).
        if v["type"] == "control_response" && v.pointer("/response/request_id").and_then(Value::as_str) == Some(CONTEXT) {
            if let Some(r) = v.pointer("/response/response").filter(|_| v.pointer("/response/subtype").and_then(Value::as_str) == Some("success"))
                && let (Some(used), Some(window)) = (r["totalTokens"].as_u64(), r["maxTokens"].as_u64())
            {
                let compacts_at = r["autoCompactThreshold"].as_u64().filter(|_| r["isAutoCompactEnabled"] != false);
                emit(AgentEvent::Context { used, window, compacts_at });
            }
            return true;
        }
        // It compacted its context (by itself, or asked with /compact): how full it is now.
        if v["type"] == "system" && sub == "compact_boundary" {
            let m = &v["compact_metadata"];
            emit(AgentEvent::Compacting(false));
            emit(AgentEvent::Compacted { auto: m["trigger"] != "manual", before: m["pre_tokens"].as_u64().unwrap_or(0), after: m["post_tokens"].as_u64() });
            self.ask_context();
            return true;
        }
        // What a command printed, as asked (`get_task_output`).
        if v["type"] == "control_response"
            && let Some(task) = v.pointer("/response/request_id").and_then(Value::as_str).and_then(|r| r.strip_prefix(TASK_OUTPUT))
        {
            let r = v.pointer("/response/response").filter(|_| v.pointer("/response/subtype").and_then(Value::as_str) == Some("success"));
            if let (Some(r), Some((tool, _))) = (r, self.tasks.get(task))
                && !tool.is_empty()
            {
                let output = plain_tail(r["output"].as_str().unwrap_or(""), r["truncated"].as_bool().unwrap_or(false));
                emit(AgentEvent::TaskOutput { tool: tool.clone(), output, bytes: r["total_bytes"].as_u64().unwrap_or(0) });
            }
            return true;
        }
        // Where the account stands against its usage limits, when that changes.
        if v["type"] == "rate_limit_event" {
            emit(AgentEvent::Limit(limit_of(&v["rate_limit_info"])));
            return true;
        }
        // Compacting by itself, and done.
        if v["type"] == "system" && sub == "status" {
            let now = v["status"] == "compacting";
            if now != self.compacting {
                self.compacting = now;
                emit(AgentEvent::Compacting(now));
            }
        }
        // It says what mode it is in when it starts, and when that changes.
        if v["type"] == "system"
            && let Some(m) = v["permissionMode"].as_str()
            && m != self.mode
        {
            self.mode = m.to_string();
            emit(AgentEvent::PermissionMode(m.to_string()));
        }
        false
    }

    /// A subagent's step: its reply (and the tool calls in it) or its tools' results, as
    /// events nested under the tool call that started it.
    fn nested(&mut self, parent: &str, v: &Value, model: &Model, emit: &EmitFn) {
        let under = |event: AgentEvent| emit(AgentEvent::Nested { parent: parent.to_string(), event: Box::new(event) });
        match v["type"].as_str() {
            Some("assistant") => {
                let m = whole_message(v, model, &mut self.nested_calls);
                let calls: Vec<ToolCall> = m.tool_calls().cloned().collect();
                under(AgentEvent::MessageEnd(Message::Assistant(m)));
                for c in calls {
                    under(AgentEvent::ToolStart { id: c.id, name: c.name, args: c.arguments });
                }
            }
            Some("user") => {
                for r in tool_results(v, &self.nested_calls) {
                    under(AgentEvent::ToolEnd { id: r.tool_call_id.clone(), name: r.tool_name.clone(), result: r });
                }
            }
            _ => {}
        }
    }

    /// How task `v["task_id"]` is going (`task_started`, `task_progress`, `task_updated`,
    /// `task_notification`), merged into what is known of it and passed on whole.
    fn task(&mut self, sub: &str, v: &Value, emit: &EmitFn) {
        let Some(id) = v["task_id"].as_str() else { return };
        if sub != "task_started" && !self.tasks.contains_key(id) && v["tool_use_id"].is_null() {
            return;
        }
        let (tool, t) = self.tasks.entry(id.to_string()).or_insert_with(|| (String::new(), serpis_proto::Task { id: id.to_string(), kind: "bash".into(), status: "running".into(), ..Default::default() }));
        if let Some(by) = v["tool_use_id"].as_str() {
            *tool = by.to_string();
        }
        let usage = |t: &mut serpis_proto::Task, u: &Value| {
            if u.is_object() {
                t.tool_uses = u["tool_uses"].as_u64().unwrap_or(t.tool_uses);
                t.tokens = u["total_tokens"].as_u64().unwrap_or(t.tokens);
                t.duration_ms = u["duration_ms"].as_u64().unwrap_or(t.duration_ms);
            }
        };
        let status = |s: &str| match s {
            "completed" | "failed" => s.to_string(),
            "killed" | "stopped" => "stopped".to_string(),
            _ => "running".to_string(),
        };
        match sub {
            "task_started" => {
                // Housekeeping of its own, which it says not to show.
                if v["ambient"] == true || v["skip_transcript"] == true {
                    let id = id.to_string();
                    self.tasks.remove(&id);
                    return;
                }
                t.description = v["description"].as_str().unwrap_or("").to_string();
                t.kind = match (v["task_type"].as_str(), v["subagent_type"].is_string()) {
                    (Some("local_agent"), _) | (None, true) => "agent".into(),
                    (Some("local_bash") | None, _) => "bash".into(),
                    (Some(other), _) => other.to_string(),
                };
                t.background = v["is_backgrounded"].as_bool().unwrap_or(false);
            }
            "task_progress" => {
                t.summary = v["summary"].as_str().or(v["description"].as_str()).map(str::to_string);
                t.last_tool = v["last_tool_name"].as_str().map(|n| serpis_tool(n, &Value::Null).0);
                usage(t, &v["usage"]);
            }
            "task_updated" => {
                let patch = &v["patch"];
                if let Some(s) = patch["status"].as_str() {
                    t.status = status(s);
                }
                if let Some(b) = patch["is_backgrounded"].as_bool() {
                    t.background = b;
                }
                if let Some(e) = patch["error"].as_str() {
                    t.summary = Some(e.to_string());
                }
            }
            "task_notification" => {
                t.status = status(v["status"].as_str().unwrap_or("completed"));
                if let Some(s) = v["summary"].as_str().filter(|s| !s.is_empty()) {
                    t.summary = Some(s.to_string());
                }
                t.last_tool = None;
                usage(t, &v["usage"]);
                let what = if t.kind == "agent" { "Background agent" } else { "Background command" };
                let how = match t.status.as_str() {
                    "failed" => "failed",
                    "stopped" => "was stopped",
                    _ => "finished",
                };
                self.finished = Some(if t.description.is_empty() { format!("{what} {how}") } else { format!("{what} {how}: {}", t.description) });
            }
            _ => return,
        }
        // A command's output is read while it runs, and once more when it ends.
        if t.kind == "bash" {
            self.watched.lock().unwrap().insert(id.to_string(), !t.running());
        }
        if !tool.is_empty() {
            emit(AgentEvent::Task { tool: tool.clone(), task: t.clone() });
        }
    }

    /// Answer a control request it sent (a permission prompt).
    async fn respond(&mut self, v: &Value, cfg: &RunConfig, h: &AgentHandle, emit: &EmitFn, interrupted: bool) -> Result<()> {
        let answer = self.answer(&v["request"], cfg, h, emit, interrupted).await;
        let id = v["request_id"].clone();
        let response = match answer {
            Ok(r) => json!({"subtype": "success", "request_id": id, "response": r}),
            Err(e) => json!({"subtype": "error", "request_id": id, "error": format!("{e:#}")}),
        };
        self.send(&json!({"type": "control_response", "response": response})).await
    }

    /// Send a control request; its id.
    /// Ask how full its context is (answered among its lines: `aside`), cheaply: from its last
    /// response's usage, not counted anew.
    fn ask_context(&self) {
        let line = json!({"type": "control_request", "request_id": CONTEXT, "request": {"subtype": "get_context_usage", "detail": "summary"}});
        let _ = self.out.send(format!("{line}\n"));
    }

    async fn request(&mut self, request: Value) -> Result<String> {
        self.next_request += 1;
        let id = format!("serpis-{}", self.next_request);
        self.send(&json!({"type": "control_request", "request_id": id, "request": request})).await?;
        Ok(id)
    }

    async fn stop(self) {
        let Process { mut child, out, .. } = self;
        // Found first: once it exits, what it started belongs to no one.
        let tree = child.id().map(process_tree).unwrap_or_default();
        drop(out);
        let _ = tokio::time::timeout(Duration::from_secs(2), child.wait()).await;
        end_processes(tree, Duration::from_secs(3));
    }

    /// End it and everything it started (the commands it runs, its background tasks, a
    /// window a test opened): see [`end_tree`].
    fn end(&mut self, grace: Duration) {
        match self.child.id() {
            Some(pid) => end_tree(pid, grace),
            None => {
                let _ = self.child.start_kill();
            }
        }
    }

    /// Read one turn, up to its `result`. True when it stopped short (aborted, failed).
    async fn turn(&mut self, cfg: &RunConfig, h: &AgentHandle, emit: &EmitFn) -> Result<bool> {
        let mut t = Turn { emit, model: &cfg.model, current: None, streamed: HashSet::new(), held: None, calls: HashMap::new() };
        let mut interrupted = false;
        let grace = tokio::time::sleep(Duration::from_secs(86_400));
        tokio::pin!(grace);
        loop {
            let ahead = self.pending.pop_front();
            let v = if ahead.is_some() { ahead } else { tokio::select! {
                v = self.lines.recv() => v,
                // Sent while it works: it takes them in at its next step, as its own prompt does.
                _ = h.steered(), if !interrupted => {
                    for msg in h.take_steering() {
                        let uuid = uuid::Uuid::new_v4().to_string();
                        let mut line = user_line(&msg, None);
                        line["priority"] = json!("next");
                        line["uuid"] = json!(uuid);
                        self.send(&line).await?;
                        self.steers.push((uuid, msg));
                    }
                    continue;
                }
                _ = h.cancel.cancelled(), if !interrupted => {
                    interrupted = true;
                    self.request(json!({"subtype": "interrupt"})).await?;
                    grace.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(10));
                    continue;
                }
                // It didn't stop when asked: end it (the next run resumes its session).
                _ = &mut grace => {
                    self.end(Duration::from_secs(3));
                    t.close_open(StopReason::Aborted, None);
                    return Ok(true);
                }
            } };
            let Some(v) = v else {
                t.close_open(if interrupted { StopReason::Aborted } else { StopReason::Error }, Some("Claude Code stopped".into()));
                if interrupted {
                    return Ok(true);
                }
                return Err(self.with_stderr(anyhow!("Claude Code stopped mid-turn")));
            };
            if self.aside(&v, &cfg.model, emit) {
                continue;
            }
            match v["type"].as_str().unwrap_or("") {
                // A message sent while it worked, taken in now: it is the chat's from here.
                "command_lifecycle" if v["state"] == "started" => {
                    if let Some(i) = self.steers.iter().position(|(u, _)| v["command_uuid"].as_str() == Some(u.as_str())) {
                        let (_, msg) = self.steers.remove(i);
                        t.close_open(StopReason::Stop, None);
                        emit(AgentEvent::MessageStart(msg.clone()));
                        emit(AgentEvent::MessageEnd(msg));
                    }
                }
                "stream_event" => t.event(&v["event"]),
                "assistant" => {
                    // Refused at a usage limit: the reply says so, with the limit it hit.
                    if let Some(info) = v.pointer("/api_error_params/rate_limit_info").filter(|i| i.is_object()) {
                        emit(AgentEvent::Limit(limit_of(info)));
                    }
                    t.whole(&v)
                }
                "user" => t.results(&v),
                "control_request" => self.respond(&v, cfg, h, emit, interrupted).await?,
                "result" => {
                    // How full its context is now (answered after the turn, to the watcher).
                    self.ask_context();
                    let aborted = interrupted || h.cancel.is_cancelled();
                    t.close_open(if aborted { StopReason::Aborted } else { StopReason::Error }, None);
                    let cost = v["total_cost_usd"].as_f64().unwrap_or(0.0);
                    let failed = v["is_error"].as_bool().unwrap_or(false) && !aborted;
                    if let Some(mut m) = t.held.take() {
                        m.usage.cost.total += cost;
                        if failed && m.stop_reason != StopReason::Error {
                            m.stop_reason = StopReason::Error;
                            m.error_message = Some(result_error(&v));
                        }
                        emit(AgentEvent::MessageEnd(Message::Assistant(m)));
                    } else if failed {
                        bail!("{}", result_error(&v));
                    }
                    return Ok(aborted || failed);
                }
                _ => {}
            }
        }
    }

    /// Answer a request from `claude`: permission prompts; anything else isn't served.
    async fn answer(&self, req: &Value, cfg: &RunConfig, h: &AgentHandle, emit: &EmitFn, interrupted: bool) -> Result<Value> {
        match req["subtype"].as_str() {
            Some("can_use_tool") => {
                let tool = req["tool_name"].as_str().unwrap_or("");
                let (name, args) = serpis_tool(tool, &req["input"]);
                if interrupted {
                    return Ok(json!({"behavior": "deny", "message": "Stopped by the user", "interrupt": true}));
                }
                // Its multiple-choice questions are for the person: with nobody asked, it asks in
                // its reply instead.
                let question = tool == "AskUserQuestion";
                if question && !matches!(cfg.prompts, Prompts::Ask) {
                    return Ok(json!({"behavior": "deny", "message": PLAIN_QUESTIONS}));
                }
                let gate = match &cfg.hooks {
                    Some(hooks) => hooks.before_tool_call(&name, args).await,
                    None => ToolGate::Allow(args),
                };
                let args = match gate {
                    ToolGate::Block(reason) => return Ok(json!({"behavior": "deny", "message": reason})),
                    ToolGate::Allow(args) => args,
                };
                let suggestions = req.get("permission_suggestions").filter(|s| s.as_array().is_some_and(|a| !a.is_empty()));
                // A plan to approve: "always" goes on accepting edits, as its own prompt offers.
                let plan = tool == "ExitPlanMode";
                let answer = match cfg.prompts {
                    Prompts::Allow => Answer::Allow,
                    Prompts::Deny => Answer::Deny(Some(
                        "Refused: serpis answers Claude Code's permission prompts with no (claude-code-permission-prompts). Ask the user to allow it.".into(),
                    )),
                    Prompts::Ask => {
                        let (reply, rx) = AskReply::new();
                        let id = req["tool_use_id"].as_str().unwrap_or("").to_string();
                        let always = if plan { Some("accepting edits from here".to_string()) } else { suggestions.and_then(always_label) };
                        emit(AgentEvent::ToolAsk { id, always, reply });
                        tokio::select! {
                            a = rx => a.unwrap_or(Answer::Deny(None)),
                            _ = h.cancel.cancelled() => return Ok(json!({"behavior": "deny", "message": "Stopped by the user", "interrupt": true})),
                        }
                    }
                };
                // A plan approved goes on in the mode chosen (allowing it: as it was before
                // planning, asking first; always: accepting edits).
                let answer = match answer {
                    Answer::Allow if plan => Answer::Plan("default".into()),
                    Answer::Always if plan => Answer::Plan("acceptEdits".into()),
                    a => a,
                };
                Ok(match answer {
                    Answer::Plan(mode) => json!({
                        "behavior": "allow",
                        "updatedInput": claude_input(tool, args),
                        "updatedPermissions": [{"type": "setMode", "mode": mode, "destination": "session"}],
                    }),
                    Answer::Deny(why) if plan => json!({"behavior": "deny", "message": match why {
                        Some(w) => format!("The user wants to keep planning. What should change: {w}"),
                        None => "The user wants to keep planning: ask them what should change.".to_string(),
                    }}),
                    Answer::Answers(answers) => {
                        let mut input = claude_input(tool, args);
                        input["answers"] = json!(answers);
                        json!({"behavior": "allow", "updatedInput": input})
                    }
                    // Questions are answered, not allowed.
                    Answer::Allow | Answer::Always if question => json!({"behavior": "deny", "message": PLAIN_QUESTIONS}),
                    Answer::Deny(None) if question => json!({"behavior": "deny", "message": "The user chose not to answer these questions. Go on without them, or ask in plain text."}),
                    Answer::Allow => json!({"behavior": "allow", "updatedInput": claude_input(tool, args)}),
                    Answer::Always => match suggestions {
                        Some(s) => json!({"behavior": "allow", "updatedInput": claude_input(tool, args), "updatedPermissions": s}),
                        None => json!({"behavior": "allow", "updatedInput": claude_input(tool, args)}),
                    },
                    Answer::Deny(why) => json!({"behavior": "deny", "message": why.unwrap_or_else(|| "The user refused this. Ask them how they want to go on.".into())}),
                })
            }
            other => bail!("serpis doesn't handle {}", other.unwrap_or("this request")),
        }
    }
}

const PLAIN_QUESTIONS: &str = "Nobody can answer a form right now: ask these questions in your reply as plain text instead, and wait for the answer.";

/// What Claude Code's `permission_suggestions` would allow, in words: "edits in this
/// session", "Bash(git status:*) in this project".
fn always_label(suggestions: &Value) -> Option<String> {
    let place = |d: &Value| match d.as_str() {
        Some("session") => " in this session",
        Some("localSettings" | "projectSettings") => " in this project",
        Some("userSettings") => " everywhere",
        _ => "",
    };
    let parts: Vec<String> = suggestions
        .as_array()?
        .iter()
        .filter_map(|s| match s["type"].as_str()? {
            "setMode" => Some(match s["mode"].as_str()? {
                "acceptEdits" => format!("edits{}", place(&s["destination"])),
                m => format!("{m} mode{}", place(&s["destination"])),
            }),
            "addRules" => {
                let rules: Vec<String> = s["rules"]
                    .as_array()?
                    .iter()
                    .map(|r| match r["ruleContent"].as_str() {
                        Some(c) => format!("{}({c})", r["toolName"].as_str().unwrap_or("")),
                        None => r["toolName"].as_str().unwrap_or("").to_string(),
                    })
                    .collect();
                Some(format!("{}{}", rules.join(", "), place(&s["destination"])))
            }
            "addDirectories" => Some(format!("access to {}{}", s["directories"].as_array()?.iter().filter_map(Value::as_str).collect::<Vec<_>>().join(", "), place(&s["destination"]))),
            _ => None,
        })
        .collect();
    (!parts.is_empty()).then(|| parts.join(", "))
}

/// The request id of `get_context_usage` requests (answers to one are all alike).
const CONTEXT: &str = "serpis-context";

/// The request id prefix of `get_task_output` requests (then the task's id).
const TASK_OUTPUT: &str = "serpis-task-output:";

/// Lines of a command's output kept to show.
const OUTPUT_LINES: usize = 40;

/// What a command printed, as it would read on a terminal, plain: escape sequences (colors,
/// cursor moves) dropped, a line rewritten with `\r` (a progress bar) as it ended up, and its
/// last [`OUTPUT_LINES`] lines. `truncated`: the text starts mid-line (the first is dropped).
pub fn plain_tail(raw: &str, truncated: bool) -> String {
    let mut text = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.next() {
                // CSI: parameters, then a final byte.
                Some('[') => {
                    while let Some(&n) = chars.peek() {
                        chars.next();
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                // OSC: up to BEL or ST.
                Some(']') => {
                    while let Some(n) = chars.next() {
                        if n == '\u{7}' || (n == '\u{1b}' && chars.peek() == Some(&'\\')) {
                            if n == '\u{1b}' {
                                chars.next();
                            }
                            break;
                        }
                    }
                }
                _ => {}
            },
            '\t' | '\n' | '\r' => text.push(c),
            c if c.is_control() => {}
            c => text.push(c),
        }
    }
    let mut lines: Vec<&str> = text
        .split('\n')
        .map(|l| {
            // A line rewritten in place reads as its last version.
            let l = l.trim_end_matches('\r');
            l.rsplit('\r').next().unwrap_or(l)
        })
        .collect();
    if truncated && lines.len() > 1 {
        lines.remove(0);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
    let from = lines.len().saturating_sub(OUTPUT_LINES);
    lines[from..].join("\n")
}

/// A `rate_limit_info`: how the account stands against its usage limits.
fn limit_of(info: &Value) -> serpis_proto::Limit {
    serpis_proto::Limit {
        status: info["status"].as_str().unwrap_or("allowed").to_string(),
        resets_at: info["resetsAt"].as_i64(),
        kind: info["rateLimitType"].as_str().map(str::to_string),
        utilization: info["utilization"].as_f64(),
    }
}

/// What a failed `result` says.
fn result_error(v: &Value) -> String {
    if let Some(e) = v["errors"].as_array().filter(|a| !a.is_empty()) {
        return e.iter().map(|e| e.as_str().map(str::to_string).unwrap_or_else(|| e.to_string())).collect::<Vec<_>>().join("; ");
    }
    match v["result"].as_str() {
        Some(r) if !r.is_empty() => r.to_string(),
        _ => format!("Claude Code failed ({})", v["subtype"].as_str().unwrap_or("error")),
    }
}

/// One turn's replies as they stream.
struct Turn<'a> {
    emit: &'a EmitFn,
    model: &'a Model,
    /// The reply streaming now, its events, and Claude Code's tool names by content index.
    current: Option<Current>,
    /// Replies already built from their stream (their whole `assistant` copies are skipped).
    streamed: HashSet<String>,
    /// The turn's last reply, which waits for the `result` to get the turn's cost.
    held: Option<AssistantMessage>,
    /// Tool calls by id: serpis's name for each.
    calls: HashMap<String, String>,
}

struct Current {
    em: Emitter,
    rx: EventStream,
    reader: EventReader,
    tools: HashMap<usize, String>,
}

impl Turn<'_> {
    fn event(&mut self, ev: &Value) {
        let ty = ev["type"].as_str().unwrap_or("");
        if ty == "message_start" {
            self.close_open(StopReason::Error, Some("cut off".into()));
            let (em, rx) = Emitter::new(self.model);
            if let Some(id) = ev.pointer("/message/id").and_then(Value::as_str) {
                self.streamed.insert(id.to_string());
            }
            self.current = Some(Current { em, rx, reader: EventReader::new(self.model, None), tools: HashMap::new() });
            (self.emit)(AgentEvent::MessageStart(Message::Assistant(AssistantMessage::empty(self.model))));
        }
        let Some(c) = self.current.as_mut() else { return };
        if let Err(e) = c.reader.handle(ty, ev, &mut c.em) {
            c.em.output.stop_reason = StopReason::Error;
            c.em.output.error_message = Some(format!("{e:#}"));
        }
        while let Ok(se) = c.rx.try_recv() {
            let se = match se {
                StreamEvent::ToolCallStart { index, id, name } => {
                    let (shown, _) = serpis_tool(&name, &Value::Null);
                    c.tools.insert(index, name);
                    self.calls.insert(id.clone(), shown.clone());
                    StreamEvent::ToolCallStart { index, id, name: shown }
                }
                StreamEvent::ToolCallDelta { index, delta, arguments } => {
                    let name = c.tools.get(&index).map(String::as_str).unwrap_or("");
                    StreamEvent::ToolCallDelta { index, delta, arguments: serpis_tool(name, &arguments).1 }
                }
                StreamEvent::ToolCallEnd { index, mut tool_call } => {
                    (tool_call.name, tool_call.arguments) = serpis_tool(c.tools.get(&index).map(String::as_str).unwrap_or(""), &tool_call.arguments);
                    StreamEvent::ToolCallEnd { index, tool_call }
                }
                other => other,
            };
            (self.emit)(AgentEvent::MessageDelta(se));
        }
        if ty == "message_stop" {
            self.close_open(StopReason::Stop, None);
        }
    }

    /// End the reply streaming now (at its end, or cut off with `stop`).
    fn close_open(&mut self, stop: StopReason, error: Option<String>) {
        let Some(c) = self.current.take() else { return };
        let mut m = c.em.output;
        if m.stop_reason == StopReason::Pending {
            m.stop_reason = stop;
            m.error_message = error.filter(|_| stop != StopReason::Stop);
        }
        for (i, block) in m.content.iter_mut().enumerate() {
            if let AssistantContent::ToolCall(tc) = block {
                (tc.name, tc.arguments) = serpis_tool(c.tools.get(&i).map(String::as_str).unwrap_or(&tc.name), &tc.arguments);
            }
        }
        self.reply(m);
    }

    /// A finished reply: a step calling tools goes out now, the turn's last one at its result.
    fn reply(&mut self, mut m: AssistantMessage) {
        m.usage.recompute_total();
        m.timestamp = now_ms();
        if let Some(prev) = self.held.take() {
            (self.emit)(AgentEvent::MessageEnd(Message::Assistant(prev)));
        }
        if m.stop_reason == StopReason::ToolUse {
            let calls: Vec<ToolCall> = m.tool_calls().cloned().collect();
            (self.emit)(AgentEvent::MessageEnd(Message::Assistant(m)));
            for c in calls {
                (self.emit)(AgentEvent::ToolStart { id: c.id, name: c.name, args: c.arguments });
            }
        } else {
            self.held = Some(m);
        }
    }

    /// A whole reply that didn't stream: Claude Code's own messages (an API error, a
    /// command's output). Streamed ones were already built from their events.
    fn whole(&mut self, line: &Value) {
        if line["message"]["id"].as_str().is_some_and(|id| self.streamed.contains(id)) {
            return;
        }
        let m = whole_message(line, self.model, &mut self.calls);
        (self.emit)(AgentEvent::MessageStart(Message::Assistant(AssistantMessage::empty(self.model))));
        self.reply(m);
    }

    /// Tool results (a `user` line).
    fn results(&mut self, v: &Value) {
        for r in tool_results(v, &self.calls) {
            (self.emit)(AgentEvent::ToolEnd { id: r.tool_call_id.clone(), name: r.tool_name.clone(), result: r.clone() });
            (self.emit)(AgentEvent::MessageStart(Message::ToolResult(r.clone())));
            (self.emit)(AgentEvent::MessageEnd(Message::ToolResult(r)));
        }
    }
}

/// A whole `assistant` line as a reply (its tool calls under serpis's names, noted in `calls`).
fn whole_message(line: &Value, model: &Model, calls: &mut HashMap<String, String>) -> AssistantMessage {
    let msg = &line["message"];
    let mut m = AssistantMessage::empty(model);
    for b in msg["content"].as_array().into_iter().flatten() {
        match b["type"].as_str() {
            Some("text") => m.content.push(AssistantContent::Text { text: b["text"].as_str().unwrap_or("").into(), text_signature: None }),
            Some("thinking") => m.content.push(AssistantContent::Thinking { thinking: b["thinking"].as_str().unwrap_or("").into(), thinking_signature: None, redacted: false }),
            Some("tool_use") => {
                let (name, arguments) = serpis_tool(b["name"].as_str().unwrap_or(""), &b["input"]);
                let id = b["id"].as_str().unwrap_or("").to_string();
                calls.insert(id.clone(), name.clone());
                m.content.push(AssistantContent::ToolCall(ToolCall { id, name, arguments, thought_signature: None, arguments_error: None }));
            }
            _ => {}
        }
    }
    m.stop_reason = if m.tool_calls().next().is_some() { StopReason::ToolUse } else { StopReason::Stop };
    if msg["model"] == "<synthetic>" && !line["error"].is_null() {
        m.stop_reason = StopReason::Error;
        m.error_message = Some(m.text());
    }
    m.usage.recompute_total();
    m.timestamp = now_ms();
    m
}

/// The tool results of a `user` line (an edit's with its diff).
fn tool_results(v: &Value, calls: &HashMap<String, String>) -> Vec<ToolResultMessage> {
    let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else { return vec![] };
    let results: Vec<&Value> = blocks.iter().filter(|b| b["type"] == "tool_result").collect();
    let only = results.len() == 1;
    results
        .into_iter()
        .map(|b| {
            let id = b["tool_use_id"].as_str().unwrap_or("").to_string();
            let content = match &b["content"] {
                Value::String(s) => vec![UserContent::Text { text: s.clone() }],
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|p| match p["type"].as_str() {
                        Some("text") => Some(UserContent::Text { text: p["text"].as_str().unwrap_or("").into() }),
                        Some("image") => Some(UserContent::Image { data: p.pointer("/source/data")?.as_str()?.into(), mime_type: p.pointer("/source/media_type")?.as_str()?.into() }),
                        _ => None,
                    })
                    .collect(),
                _ => Vec::new(),
            };
            let details = match v["tool_use_result"]["structuredPatch"].as_array() {
                Some(hunks) if only && !hunks.is_empty() => json!({"diff": patch_diff(hunks)}),
                _ => Value::Null,
            };
            let name = calls.get(&id).cloned().unwrap_or_default();
            ToolResultMessage { tool_call_id: id, tool_name: name, content, is_error: b["is_error"].as_bool().unwrap_or(false), details, timestamp: now_ms() }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_reads_as_plain_text() {
        // Colors and a title (OSC) go; a progress line shows as it ended up.
        assert_eq!(plain_tail("\u{1b}[1;32mok\u{1b}[0m\n\u{1b}]0;title\u{7}50%\r100%\n\n", false), "ok\n100%");
        // Cut off mid-line: that line goes.
        assert_eq!(plain_tail("ial line\nnext\n", true), "next");
        // Only the last lines.
        let many: String = (1..=100).map(|i| format!("{i}\n")).collect();
        let tail = plain_tail(&many, false);
        assert_eq!((tail.lines().count(), tail.lines().next()), (OUTPUT_LINES, Some("61")));
    }

    #[test]
    fn tools_take_serpis_names_and_back() {
        let (name, args) = serpis_tool("Edit", &json!({"file_path": "a.rs", "old_string": "x", "new_string": "y"}));
        assert_eq!(name, "edit");
        assert_eq!(args, json!({"path": "a.rs", "old_string": "x", "new_string": "y"}));
        assert_eq!(claude_input("Edit", args), json!({"file_path": "a.rs", "old_string": "x", "new_string": "y"}));
        assert_eq!(serpis_tool("Bash", &json!({"command": "ls"})), ("bash".into(), json!({"command": "ls"})));
        assert_eq!(serpis_tool("TodoWrite", &json!({})).0, "TodoWrite");
    }

    #[test]
    fn always_says_what_it_allows() {
        assert_eq!(always_label(&json!([{"type": "setMode", "mode": "acceptEdits", "destination": "session"}])).as_deref(), Some("edits in this session"));
        let rules = json!([{"type": "addRules", "rules": [{"toolName": "Bash", "ruleContent": "git status:*"}], "behavior": "allow", "destination": "localSettings"}]);
        assert_eq!(always_label(&rules).as_deref(), Some("Bash(git status:*) in this project"));
        assert_eq!(always_label(&json!([])), None);
    }

    #[test]
    fn patches_read_like_serpis_diffs() {
        let hunks = vec![
            json!({"oldStart": 1, "oldLines": 3, "newStart": 1, "newLines": 3, "lines": [" one", "-two", "+TWO", " three"]}),
            json!({"oldStart": 9, "oldLines": 2, "newStart": 9, "newLines": 1, "lines": [" nine", "-ten"]}),
        ];
        assert_eq!(patch_diff(&hunks), "  1 one\n- 2 two\n+ 2 TWO\n  3 three\n    ...\n  9 nine\n-10 ten");
    }

    #[test]
    fn only_turns_claude_code_missed_are_replayed() {
        let user = |t: &str| Message::User { content: vec![UserContent::Text { text: t.into() }], timestamp: 0 };
        let reply = |provider: &str| {
            let mut m = AssistantMessage::empty(&serpis_ai::claude_code::fallback_models()[0]);
            m.provider = provider.into();
            m.content.push(AssistantContent::Text { text: "ok".into(), text_signature: None });
            Message::Assistant(m)
        };
        let h = vec![user("a"), reply("claude-code"), user("b"), reply("openrouter")];
        assert_eq!(unseen(&h).len(), 2);
        assert!(replay(unseen(&h)).unwrap().contains("[user]\nb\n\n[assistant]\nok"));
        assert!(replay(unseen(&h[..2])).is_none());
    }
}
