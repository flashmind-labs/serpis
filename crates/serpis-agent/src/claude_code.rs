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
use tokio::process::{Child, ChildStdin};
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
    /// More arguments (`claude-code-args`).
    pub args: Vec<String>,
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
#[derive(Clone, Default)]
pub struct ClaudeCode {
    proc: Arc<tokio::sync::Mutex<Option<Process>>>,
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
        let (h, me) = (handle.clone(), self.clone());
        tokio::spawn(async move {
            emit(AgentEvent::AgentStart);
            let error = me.drive(history, prompts, &cfg, &h, &emit).await.err().map(|e| format!("{e:#}"));
            emit(AgentEvent::AgentEnd { error });
        });
        handle
    }

    /// Stop the process (the chat closed, or starts over).
    pub fn stop(&self) {
        if let Ok(mut p) = self.proc.try_lock() {
            if let Some(p) = p.as_mut() {
                let _ = p.child.start_kill();
            }
            *p = None;
        }
    }

    async fn drive(&self, history: Vec<Message>, prompts: Vec<Message>, cfg: &RunConfig, h: &AgentHandle, emit: &EmitFn) -> Result<()> {
        let mut guard = self.proc.lock().await;
        let resume = history.iter().any(is_reply);
        let mut fresh = !resume;
        if guard.as_mut().is_none_or(|p| p.launch != cfg.launch || p.exited()) {
            if let Some(old) = guard.take() {
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
        let earlier = if fresh { &history[..] } else { unseen(&history) };
        let mut context = replay(earlier);
        let mut queue: VecDeque<Message> = prompts.into();
        if queue.is_empty() {
            queue.push_back(Message::User { content: vec![UserContent::Text { text: CONTINUE.into() }], timestamp: now_ms() });
        }
        while let Some(msg) = queue.pop_front() {
            emit(AgentEvent::MessageStart(msg.clone()));
            emit(AgentEvent::MessageEnd(msg.clone()));
            p.send(&user_line(&msg, context.take())).await?;
            let stopped = p.turn(cfg, h, emit).await?;
            emit(AgentEvent::TurnEnd);
            if stopped {
                break;
            }
            // Queued while it ran: Claude Code takes one message at a time.
            let (steer, follow) = h.take_queued();
            queue.extend(steer.into_iter().chain(follow));
        }
        Ok(())
    }
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
    stdin: ChildStdin,
    lines: mpsc::UnboundedReceiver<Value>,
    launch: Launch,
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
        cmd.args(&l.args).envs(l.env.iter().cloned()).current_dir(&l.cwd).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        let mut child = cmd.spawn().with_context(|| format!("starting Claude Code ({})", l.binary.display()))?;
        let stdin = child.stdin.take().context("no stdin")?;
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
        let mut p = Process { child, stdin, lines, launch: l.clone(), stderr, next_request: 0 };
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
        let line = format!("{v}\n");
        let r = self.stdin.write_all(line.as_bytes()).await;
        r.map_err(|e| self.with_stderr(anyhow!("writing to Claude Code: {e}")))
    }

    /// Send a control request; its id.
    async fn request(&mut self, request: Value) -> Result<String> {
        self.next_request += 1;
        let id = format!("serpis-{}", self.next_request);
        self.send(&json!({"type": "control_request", "request_id": id, "request": request})).await?;
        Ok(id)
    }

    async fn stop(mut self) {
        drop(self.stdin);
        if tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await.is_err() {
            let _ = self.child.kill().await;
        }
    }

    /// Read one turn, up to its `result`. True when it stopped short (aborted, failed).
    async fn turn(&mut self, cfg: &RunConfig, h: &AgentHandle, emit: &EmitFn) -> Result<bool> {
        let mut t = Turn { emit, model: &cfg.model, current: None, streamed: HashSet::new(), held: None, calls: HashMap::new() };
        let mut interrupted = false;
        let grace = tokio::time::sleep(Duration::from_secs(86_400));
        tokio::pin!(grace);
        loop {
            let v = tokio::select! {
                v = self.lines.recv() => v,
                _ = h.cancel.cancelled(), if !interrupted => {
                    interrupted = true;
                    self.request(json!({"subtype": "interrupt"})).await?;
                    grace.as_mut().reset(tokio::time::Instant::now() + Duration::from_secs(10));
                    continue;
                }
                // It didn't stop when asked: end it (the next run resumes its session).
                _ = &mut grace => {
                    let _ = self.child.start_kill();
                    t.close_open(StopReason::Aborted, None);
                    return Ok(true);
                }
            };
            let Some(v) = v else {
                t.close_open(if interrupted { StopReason::Aborted } else { StopReason::Error }, Some("Claude Code stopped".into()));
                if interrupted {
                    return Ok(true);
                }
                return Err(self.with_stderr(anyhow!("Claude Code stopped mid-turn")));
            };
            // Subagents' own steps: their Task tool's card shows the outcome.
            if !v["parent_tool_use_id"].is_null() {
                continue;
            }
            match v["type"].as_str().unwrap_or("") {
                "stream_event" => t.event(&v["event"]),
                "assistant" => t.whole(&v),
                "user" => t.results(&v),
                "control_request" => {
                    let answer = self.answer(&v["request"], cfg, h, emit, interrupted).await;
                    let id = v["request_id"].clone();
                    let response = match answer {
                        Ok(r) => json!({"subtype": "success", "request_id": id, "response": r}),
                        Err(e) => json!({"subtype": "error", "request_id": id, "error": format!("{e:#}")}),
                    };
                    self.send(&json!({"type": "control_response", "response": response})).await?;
                }
                "result" => {
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
                let answer = match cfg.prompts {
                    Prompts::Allow => Answer::Allow,
                    Prompts::Deny => Answer::Deny(Some(
                        "Refused: serpis answers Claude Code's permission prompts with no (claude-code-permission-prompts). Ask the user to allow it.".into(),
                    )),
                    Prompts::Ask => {
                        let (reply, rx) = AskReply::new();
                        let id = req["tool_use_id"].as_str().unwrap_or("").to_string();
                        emit(AgentEvent::ToolAsk { id, always: suggestions.and_then(always_label), reply });
                        tokio::select! {
                            a = rx => a.unwrap_or(Answer::Deny(None)),
                            _ = h.cancel.cancelled() => return Ok(json!({"behavior": "deny", "message": "Stopped by the user", "interrupt": true})),
                        }
                    }
                };
                Ok(match answer {
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
        let msg = &line["message"];
        if msg["id"].as_str().is_some_and(|id| self.streamed.contains(id)) {
            return;
        }
        let mut m = AssistantMessage::empty(self.model);
        for b in msg["content"].as_array().into_iter().flatten() {
            match b["type"].as_str() {
                Some("text") => m.content.push(AssistantContent::Text { text: b["text"].as_str().unwrap_or("").into(), text_signature: None }),
                Some("thinking") => m.content.push(AssistantContent::Thinking { thinking: b["thinking"].as_str().unwrap_or("").into(), thinking_signature: None, redacted: false }),
                Some("tool_use") => {
                    let (name, arguments) = serpis_tool(b["name"].as_str().unwrap_or(""), &b["input"]);
                    let id = b["id"].as_str().unwrap_or("").to_string();
                    self.calls.insert(id.clone(), name.clone());
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
        (self.emit)(AgentEvent::MessageStart(Message::Assistant(AssistantMessage::empty(self.model))));
        self.reply(m);
    }

    /// Tool results (a `user` line).
    fn results(&mut self, v: &Value) {
        let Some(blocks) = v.pointer("/message/content").and_then(Value::as_array) else { return };
        let results: Vec<&Value> = blocks.iter().filter(|b| b["type"] == "tool_result").collect();
        let only = results.len() == 1;
        for b in results {
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
            let name = self.calls.get(&id).cloned().unwrap_or_default();
            let r = ToolResultMessage { tool_call_id: id.clone(), tool_name: name.clone(), content, is_error: b["is_error"].as_bool().unwrap_or(false), details, timestamp: now_ms() };
            (self.emit)(AgentEvent::ToolEnd { id, name, result: r.clone() });
            (self.emit)(AgentEvent::MessageStart(Message::ToolResult(r.clone())));
            (self.emit)(AgentEvent::MessageEnd(Message::ToolResult(r)));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
