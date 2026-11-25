//! Antigravity as a chat agent, driven through `agy acp` (ACP JSON-RPC 2.0 over stdio).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use serpis_ai::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdout};
use tokio::sync::mpsc;

use crate::agent::{AgentEvent, AgentHandle, Answer, AskReply, EmitFn, ToolGate, ToolHooks};
use crate::tools::ToolResult;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Launch {
    pub binary: PathBuf,
    pub cwd: PathBuf,
    /// More arguments after `agy acp`.
    pub args: Vec<String>,
    /// Extra system instructions, used to offer Serpis skills Antigravity does not already see.
    pub instructions: Option<String>,
    pub instructions_restart: bool,
    pub env: Vec<(String, String)>,
}

pub struct RunConfig {
    pub launch: Launch,
    pub model: Model,
    pub effort: Option<String>,
    pub mode: Option<String>,
    pub hooks: Option<Arc<dyn ToolHooks>>,
    pub prompts: Prompts,
    /// MCP servers for its sessions (`session/new`'s `mcpServers`): a challenger's tools.
    pub mcp_servers: Vec<Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Prompts {
    #[default]
    Ask,
    Allow,
    Deny,
}

impl Prompts {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "ask" => Some(Self::Ask),
            "allow" => Some(Self::Allow),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

#[derive(Clone, Default)]
pub struct Antigravity {
    process: Arc<tokio::sync::Mutex<Option<Process>>>,
    watching: Arc<Mutex<Option<CancellationToken>>>,
    /// Live writer and active session id, so `set_mode` can send without locking `process`.
    writer: Arc<Mutex<Option<(mpsc::UnboundedSender<String>, String)>>>,
    /// The latest mode requested by the user (updated immediately by `set_mode`, even mid-turn).
    live_mode: Arc<Mutex<Option<String>>>,
    next_id: Arc<AtomicU64>,
    stop: Arc<Mutex<CancellationToken>>,
    pid: Arc<Mutex<Option<u32>>>,
    /// Stopped while locked: the next run starts it again.
    stale: Arc<AtomicBool>,
}

/// How long a turn Antigravity started by itself may stay quiet before it counts as done
/// (ACP marks no end for one).
const QUIET: Duration = Duration::from_secs(2);

pub fn is_reply(message: &Message) -> bool {
    matches!(message, Message::Assistant(a) if a.provider == serpis_ai::antigravity::PROVIDER)
}

impl Antigravity {
    pub fn run(
        &self,
        history: Vec<Message>,
        prompts: Vec<Message>,
        cfg: RunConfig,
        emit: EmitFn,
    ) -> AgentHandle {
        let handle = AgentHandle::new();
        if let Some(w) = self.watching.lock().unwrap().take() {
            w.cancel();
        }
        let cfg = Arc::new(cfg);
        let (me, h) = (self.clone(), handle.clone());
        tokio::spawn(async move {
            emit(AgentEvent::AgentStart);
            let error = me
                .drive(history, prompts, &cfg, &h, &emit)
                .await
                .err()
                .map(|e| format!("{e:#}"));
            me.watch(cfg, emit.clone());
            emit(AgentEvent::AgentEnd { error });
        });
        handle
    }

    /// Switch Antigravity's permission mode live (between runs or during one).
    /// Returns false when no session is open yet (the next turn applies it on start).
    pub fn set_mode(&self, mode: &str) -> bool {
        *self.live_mode.lock().unwrap() = Some(mode.to_string());
        let guard = self.writer.lock().unwrap();
        let Some((tx, session_id)) = guard.as_ref() else {
            return false;
        };
        let id = alloc_id(&self.next_id);
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "session/set_mode",
            "params": {"sessionId": session_id, "modeId": mode}
        });
        tx.send(format!("{msg}\n")).is_ok()
    }

    pub fn stop(&self) {
        if let Some(w) = self.watching.lock().unwrap().take() {
            w.cancel();
        }
        *self.writer.lock().unwrap() = None;
        self.stop.lock().unwrap().cancel();
        match self.process.try_lock() {
            Ok(mut process) => {
                if let Some(process) = process.as_mut() {
                    let _ = process.child.start_kill();
                }
                *process = None;
            }
            Err(_) => {
                if let Some(pid) = self.pid.lock().unwrap().take() {
                    crate::claude_code::end_tree(pid, Duration::from_secs(3));
                }
                self.stale.store(true, Ordering::SeqCst);
            }
        }
    }

    /// Watch `agy acp` stdout between runs so background tool updates, permission requests,
    /// mode/usage updates, or autonomous follow-up turns are surfaced immediately.
    fn watch(&self, cfg: Arc<RunConfig>, emit: EmitFn) {
        let token = CancellationToken::new();
        *self.watching.lock().unwrap() = Some(token.clone());
        let me = self.clone();
        tokio::spawn(async move {
            let mut guard = me.process.lock().await;
            let mut state: Option<Turn<'_>> = None;
            loop {
                let Some(p) = guard.as_mut().filter(|_| !token.is_cancelled()) else {
                    return;
                };
                let value = match p.pending.pop_front() {
                    Some(ahead) => ahead,
                    None => tokio::select! {
                        line = p.lines.next_line() => match line {
                            Ok(Some(l)) => match serde_json::from_str::<Value>(&l) {
                                Ok(v) => v,
                                Err(_) => continue,
                            },
                            _ => return,
                        },
                        _ = token.cancelled() => return,
                    },
                };
                let Some(sid) = p.session_id.clone() else { continue };
                if is_request(&value) {
                    let turn = state.get_or_insert_with(|| Turn::new(&cfg.model, &sid, &emit));
                    if p.handle_request(&value, turn, &cfg, &AgentHandle::new(), &token, &emit)
                        .await
                        .is_err()
                    {
                        return;
                    }
                    continue;
                }
                if value["method"].as_str() != Some("session/update") {
                    continue;
                }
                let update = &value["params"]["update"];
                p.track_mode(update);
                if !matches!(
                    update["sessionUpdate"].as_str(),
                    Some("agent_message_chunk" | "agent_thought_chunk")
                ) {
                    state
                        .get_or_insert_with(|| Turn::new(&cfg.model, &sid, &emit))
                        .session_update(update);
                    continue;
                }
                // Autonomous turn (e.g. a scheduled wake or background task finishing).
                let h = AgentHandle::new();
                emit(AgentEvent::Adopted {
                    handle: h.clone(),
                    why: None,
                });
                emit(AgentEvent::AgentStart);
                let mut turn = state
                    .take()
                    .unwrap_or_else(|| Turn::new(&cfg.model, &sid, &emit));
                turn.session_update(update);
                let stop = p.follow(&mut turn, &cfg, &h, &token, &emit).await;
                turn.finish(stop, None);
                emit(AgentEvent::AgentEnd { error: None });
            }
        });
    }

    async fn drive(
        &self,
        history: Vec<Message>,
        prompts: Vec<Message>,
        cfg: &RunConfig,
        h: &AgentHandle,
        emit: &EmitFn,
    ) -> Result<()> {
        let mut guard = self.process.lock().await;
        let process_stop = {
            let mut stop = self.stop.lock().unwrap();
            if stop.is_cancelled() {
                *stop = CancellationToken::new();
            }
            stop.clone()
        };
        let stale = self.stale.swap(false, Ordering::SeqCst);
        let exited = guard
            .as_mut()
            .is_some_and(|p| p.child.try_wait().ok().flatten().is_some());
        let restart = stale
            || exited
            || guard.as_ref().is_none_or(|p| {
                p.launch.binary != cfg.launch.binary
                    || p.launch.cwd != cfg.launch.cwd
                    || p.launch.args != cfg.launch.args
                    || p.launch.env != cfg.launch.env
                    || (cfg.launch.instructions_restart
                        && p.launch.instructions != cfg.launch.instructions)
            });
        if restart {
            *self.writer.lock().unwrap() = None;
            if let Some(old) = guard.as_mut() {
                let _ = old.child.start_kill();
            }
            let process = Process::start(cfg.launch.clone(), self.next_id.clone(), self.live_mode.clone()).await?;
            *self.pid.lock().unwrap() = process.child.id();
            *guard = Some(process);
        }
        let process = guard.as_mut().expect("process was started");
        let saved_session = history.iter().rev().find_map(|m| match m {
            Message::Assistant(a) if a.provider == serpis_ai::antigravity::PROVIDER => {
                a.response_id.clone()
            }
            _ => None,
        });
        if process.session_id.is_none() {
            process.open_session(saved_session.as_deref(), cfg).await?;
        }
        if let Some(sid) = &process.session_id {
            *self.writer.lock().unwrap() = Some((process.tx.clone(), sid.clone()));
        }

        // When a chat switches to Antigravity, give it the conversation its own session has not seen.
        let mut replay = (saved_session.is_none() && !history.is_empty())
            .then(|| replay_history(&history))
            .filter(|s| !s.is_empty());
        let mut continuing = prompts.is_empty();
        let mut pending: VecDeque<Message> = prompts.into();
        loop {
            let mut input = Vec::new();
            if let Some(context) = replay.take() {
                input.push(json!({"type": "text", "text": context}));
            }
            if continuing && pending.is_empty() {
                input.push(json!({"type": "text", "text": "Continue."}));
                continuing = false;
            }
            while let Some(message) = pending.pop_front() {
                emit(AgentEvent::MessageStart(message.clone()));
                emit(AgentEvent::MessageEnd(message.clone()));
                input.extend(prompt_blocks(&message));
            }
            if input.is_empty() {
                return Ok(());
            }
            process.turn(input, cfg, h, &process_stop, emit).await?;
            let (steering, follow_ups) = h.take_queued();
            pending.extend(steering);
            pending.extend(follow_ups);
            if pending.is_empty() || h.cancel.is_cancelled() {
                return Ok(());
            }
            emit(AgentEvent::TurnStart);
        }
    }
}

struct Process {
    child: Child,
    tx: mpsc::UnboundedSender<String>,
    lines: Lines<BufReader<ChildStdout>>,
    pending: VecDeque<Value>,
    next_id: Arc<AtomicU64>,
    session_id: Option<String>,
    supports_load: bool,
    supports_steering: bool,
    supports_system_prompt_append: bool,
    active_model: Option<String>,
    active_effort: Option<String>,
    active_mode: Option<String>,
    live_mode: Arc<Mutex<Option<String>>>,
    launch: Launch,
}

impl Process {
    async fn start(
        launch: Launch,
        next_id: Arc<AtomicU64>,
        live_mode: Arc<Mutex<Option<String>>>,
    ) -> Result<Self> {
        let mut command = tokio::process::Command::new(&launch.binary);
        command
            .arg("acp")
            .args(&launch.args)
            .current_dir(&launch.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for k in crate::agent::HIDDEN_ENV {
            command.env_remove(k);
        }
        for (name, value) in &launch.env {
            command.env(name, value);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("starting {} acp", launch.binary.display()))?;
        let mut stdin = child.stdin.take().context("agy acp has no stdin")?;
        let (tx, mut rx) = mpsc::unbounded_channel::<String>();
        tokio::spawn(async move {
            while let Some(line) = rx.recv().await {
                if stdin.write_all(line.as_bytes()).await.is_err() || stdin.flush().await.is_err() {
                    break;
                }
            }
        });
        let lines = BufReader::new(child.stdout.take().context("agy acp has no stdout")?).lines();
        let mut process = Process {
            child,
            tx,
            lines,
            pending: VecDeque::new(),
            next_id,
            session_id: None,
            supports_load: false,
            supports_steering: false,
            supports_system_prompt_append: false,
            active_model: None,
            active_effort: None,
            active_mode: None,
            live_mode,
            launch,
        };
        let init = process
            .call(
                "initialize",
                json!({
                    "protocolVersion": 1,
                    "clientCapabilities": {},
                    "clientInfo": {"name": "serpis", "title": "Serpis", "version": env!("CARGO_PKG_VERSION")}
                }),
            )
            .await?;
        let caps = &init["result"]["agentCapabilities"];
        process.supports_load = caps["loadSession"].as_bool().unwrap_or(false)
            || caps["load_session"].as_bool().unwrap_or(false);
        process.supports_system_prompt_append = caps
            .pointer("/_meta/antigravity/systemPrompt/append")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        process.supports_steering = init
            .pointer("/result/agentInfo/version")
            .and_then(Value::as_str)
            .and_then(parse_semver)
            .is_some_and(|v| v >= (1, 5, 0));
        Ok(process)
    }

    async fn open_session(&mut self, saved: Option<&str>, cfg: &RunConfig) -> Result<()> {
        let mut meta = None;
        if self.supports_system_prompt_append
            && let Some(instructions) = cfg.launch.instructions.as_deref().filter(|s| !s.is_empty())
        {
            meta = Some(json!({"antigravity": {"systemPrompt": {"append": instructions}}}));
        }
        if let Some(session_id) = saved.filter(|_| self.supports_load) {
            let mut params = json!({"sessionId": session_id, "cwd": cfg.launch.cwd, "mcpServers": cfg.mcp_servers});
            if let Some(m) = &meta {
                params["_meta"] = m.clone();
            }
            let loaded = self.call("session/load", params).await;
            if let Ok(response) = loaded {
                self.session_id = Some(session_id.to_string());
                self.record_config(&response["result"]);
                return Ok(());
            }
        }
        let mut params = json!({"cwd": cfg.launch.cwd, "mcpServers": cfg.mcp_servers});
        if let Some(m) = meta {
            params["_meta"] = m;
        }
        let response = self.call("session/new", params).await?;
        let id = response
            .pointer("/result/sessionId")
            .and_then(Value::as_str)
            .context("Antigravity did not return a sessionId")?;
        self.session_id = Some(id.to_string());
        self.record_config(&response["result"]);
        Ok(())
    }

    fn record_config(&mut self, result: &Value) {
        if let Some(mode) = result.pointer("/modes/currentModeId").and_then(Value::as_str) {
            self.active_mode = Some(mode.to_string());
        }
        for opt in result["configOptions"].as_array().into_iter().flatten() {
            let value = opt["currentValue"].as_str().map(str::to_string);
            match opt["id"].as_str() {
                Some("model") => self.active_model = value,
                Some("thought_level") => self.active_effort = value,
                Some("mode") => self.active_mode = value,
                _ => {}
            }
        }
    }

    async fn apply_config(&mut self, cfg: &RunConfig) -> Result<()> {
        let session_id = self
            .session_id
            .clone()
            .context("Antigravity session is not open")?;
        if self.active_model.as_deref() != Some(cfg.model.id.as_str()) {
            let response = self
                .call(
                    "session/set_config_option",
                    json!({"sessionId": session_id, "configId": "model", "value": cfg.model.id}),
                )
                .await;
            if let Ok(r) = response {
                self.record_config(&r["result"]);
                self.active_model = Some(cfg.model.id.clone());
            }
        }
        if let Some(effort) = cfg.effort.as_deref()
            && self.active_effort.as_deref() != Some(effort)
        {
            let response = self
                .call(
                    "session/set_config_option",
                    json!({"sessionId": session_id, "configId": "thought_level", "value": effort}),
                )
                .await;
            if let Ok(r) = response {
                self.record_config(&r["result"]);
                self.active_effort = Some(effort.to_string());
            }
        }
        if let Some(mode) = cfg.mode.as_deref() {
            *self.live_mode.lock().unwrap() = Some(mode.to_string());
            if self.active_mode.as_deref() != Some(mode) {
                let response = self
                    .call(
                        "session/set_mode",
                        json!({"sessionId": session_id, "modeId": mode}),
                    )
                    .await;
                if response.is_ok() {
                    self.active_mode = Some(mode.to_string());
                } else if let Ok(r) = self
                    .call(
                        "session/set_config_option",
                        json!({"sessionId": session_id, "configId": "mode", "value": mode}),
                    )
                    .await
                {
                    self.record_config(&r["result"]);
                    self.active_mode = Some(mode.to_string());
                }
            }
        }
        Ok(())
    }

    fn effective_mode(&self, cfg: &RunConfig) -> Option<String> {
        self.live_mode
            .lock()
            .unwrap()
            .clone()
            .or_else(|| cfg.mode.clone())
            .or_else(|| self.active_mode.clone())
    }

    fn alloc_id(&self) -> u64 {
        alloc_id(&self.next_id)
    }

    /// Remember a mode Antigravity reports switching to.
    fn track_mode(&mut self, update: &Value) {
        if update["sessionUpdate"].as_str() == Some("current_mode_update")
            && let Some(mode) = update["currentModeId"].as_str()
        {
            self.active_mode = Some(mode.to_string());
        }
    }

    /// Follow a turn Antigravity started by itself until it goes quiet, answering what it asks.
    async fn follow(
        &mut self,
        turn: &mut Turn<'_>,
        cfg: &RunConfig,
        h: &AgentHandle,
        stop: &CancellationToken,
        emit: &EmitFn,
    ) -> StopReason {
        loop {
            let value = match self.pending.pop_front() {
                Some(ahead) => ahead,
                None => tokio::select! {
                    line = tokio::time::timeout(QUIET, self.lines.next_line()) => match line {
                        Ok(Ok(Some(l))) => match serde_json::from_str::<Value>(&l) {
                            Ok(v) => v,
                            Err(_) => continue,
                        },
                        _ => return StopReason::Stop,
                    },
                    _ = h.cancel.cancelled() => {
                        let session_id = turn.session_id.clone();
                        let _ = self.send(&json!({"jsonrpc": "2.0", "method": "session/cancel", "params": {"sessionId": session_id}}));
                        return StopReason::Aborted;
                    }
                    _ = stop.cancelled() => return StopReason::Aborted,
                },
            };
            if is_request(&value) {
                if self.handle_request(&value, turn, cfg, h, stop, emit).await.is_err() {
                    return StopReason::Error;
                }
            } else if value["method"].as_str() == Some("session/update") {
                let update = &value["params"]["update"];
                self.track_mode(update);
                turn.session_update(update);
            }
        }
    }

    async fn turn(
        &mut self,
        prompt: Vec<Value>,
        cfg: &RunConfig,
        h: &AgentHandle,
        process_stop: &CancellationToken,
        emit: &EmitFn,
    ) -> Result<()> {
        self.apply_config(cfg).await?;
        let session_id = self
            .session_id
            .clone()
            .context("Antigravity session is not open")?;
        let request_id = self.alloc_id();
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": request_id,
            "method": "session/prompt",
            "params": {"sessionId": session_id, "prompt": prompt}
        }))?;
        let mut state = Turn::new(&cfg.model, &session_id, emit);
        // Open a streaming assistant entry right away so Serpis shows `Thinking…` while
        // Antigravity thinks silently (Gemini models often omit thought chunks).
        state.ensure_reply();
        let mut in_flight: HashSet<u64> = HashSet::from([request_id]);
        let mut cancelled = false;
        loop {
            let ahead = self.pending.pop_front();
            let value = if let Some(value) = ahead {
                Some(value)
            } else {
                tokio::select! {
                    line = self.lines.next_line() => match line? {
                        Some(l) => match serde_json::from_str::<Value>(&l) {
                            Ok(v) => Some(v),
                            Err(_) => continue,
                        },
                        None => None,
                    },
                    _ = h.steered(), if !cancelled && self.supports_steering => {
                        for message in h.take_steering() {
                            let blocks = prompt_blocks(&message);
                            if !blocks.is_empty() {
                                let steer_id = self.alloc_id();
                                in_flight.insert(steer_id);
                                self.send(&json!({
                                    "jsonrpc": "2.0",
                                    "id": steer_id,
                                    "method": "session/prompt",
                                    "params": {"sessionId": session_id, "prompt": blocks}
                                }))?;
                                emit(AgentEvent::MessageStart(message.clone()));
                                emit(AgentEvent::MessageEnd(message));
                            }
                        }
                        continue;
                    }
                    _ = h.cancel.cancelled(), if !cancelled => {
                        cancelled = true;
                        let _ = self.send(&json!({
                            "jsonrpc": "2.0",
                            "method": "session/cancel",
                            "params": {"sessionId": session_id}
                        }));
                        continue;
                    }
                    _ = process_stop.cancelled() => {
                        let _ = self.child.start_kill();
                        state.finish(StopReason::Aborted, None);
                        return Ok(());
                    }
                }
            };
            let Some(value) = value else {
                state.finish(
                    if cancelled {
                        StopReason::Aborted
                    } else {
                        StopReason::Error
                    },
                    Some("Antigravity ACP stopped".into()),
                );
                bail!("Antigravity ACP stopped mid-turn");
            };
            if let Some(id) = value["id"].as_u64()
                && in_flight.remove(&id)
            {
                if let Some(error) = value.get("error") {
                    state.finish(
                        StopReason::Error,
                        Some(
                            error["message"]
                                .as_str()
                                .unwrap_or("Antigravity refused the turn")
                                .into(),
                        ),
                    );
                    return Ok(());
                }
                let result = &value["result"];
                state.apply_prompt_usage(&result["usage"]);
                if !in_flight.is_empty() {
                    // A steered prompt is still running; stay in this turn until it completes.
                    continue;
                }
                let stop = match result["stopReason"].as_str().unwrap_or("end_turn") {
                    "end_turn" | "max_tokens" | "max_turn_requests" => StopReason::Stop,
                    "cancelled" => StopReason::Aborted,
                    _ => StopReason::Error,
                };
                state.finish(stop, None);
                return Ok(());
            }
            if is_request(&value) {
                self.handle_request(&value, &mut state, cfg, h, process_stop, emit).await?;
                continue;
            }
            if value["method"].as_str() == Some("session/update") {
                let update = &value["params"]["update"];
                self.track_mode(update);
                state.session_update(update);
            }
        }
    }

    async fn handle_request(
        &mut self,
        request: &Value,
        state: &mut Turn<'_>,
        cfg: &RunConfig,
        h: &AgentHandle,
        stop: &CancellationToken,
        emit: &EmitFn,
    ) -> Result<()> {
        let method = request["method"].as_str().unwrap_or("");
        if method != "session/request_permission" {
            self.respond_error(&request["id"], "Serpis does not handle this ACP request")?;
            return Ok(());
        }
        let params = &request["params"];
        let tool_call = &params["toolCall"];
        let id = tool_call["toolCallId"].as_str().unwrap_or("").to_string();
        let options = params["options"].as_array().cloned().unwrap_or_default();
        let is_question = id.starts_with("interaction_");
        let (name, args, always) = if is_question {
            let question = tool_call["title"].as_str().unwrap_or("Choose an option.");
            let choices: Vec<Value> = options
                .iter()
                .map(|o| {
                    json!({
                        "label": o["name"].as_str().or_else(|| o["optionId"].as_str()).unwrap_or(""),
                        "description": o["description"].as_str().unwrap_or("")
                    })
                })
                .collect();
            (
                "AskUserQuestion".to_string(),
                json!({"questions": [{"id": id, "header": "Question", "question": question, "options": choices, "multiSelect": false}]}),
                None,
            )
        } else {
            let (name, args) = normalize_tool(tool_call);
            let always = options
                .iter()
                .find(|o| o["kind"].as_str() == Some("allow_always"))
                .and_then(|o| o["name"].as_str())
                .map(str::to_string);
            (name, args, always)
        };

        if !state.tools.contains_key(&id) {
            state.start_tool(id.clone(), name.clone(), args.clone());
        }

        // A challenger's own tools (crate::challenge) only record what they are given: allowed,
        // in a session it gave them to.
        let challenge = !is_question && !cfg.mcp_servers.is_empty() && crate::challenge::call_of(&name, &args).is_some();
        let mode = self.effective_mode(cfg);
        let auto_allow = !is_question
            && match mode.as_deref() {
                Some("auto") => true,
                Some("accept-edits") => matches!(name.as_str(), "edit" | "read" | "grep" | "web_search"),
                _ => false,
            };
        let gate = match &cfg.hooks {
            Some(hooks) => hooks.before_tool_call(&name, args).await,
            None => ToolGate::Allow(Value::Null),
        };
        let answer = match gate {
            ToolGate::Block(reason) => Answer::Deny(Some(reason)),
            ToolGate::Allow(_) => match cfg.prompts {
                _ if challenge => Answer::Allow,
                Prompts::Deny => {
                    Answer::Deny(Some("Refused by antigravity-permission-prompts".into()))
                }
                Prompts::Allow if !is_question => Answer::Allow,
                _ if auto_allow => Answer::Allow,
                _ => {
                    let (reply, answer) = AskReply::new();
                    emit(AgentEvent::ToolAsk {
                        id: id.clone(),
                        always,
                        reply,
                    });
                    tokio::select! {
                        answer = answer => answer.unwrap_or(Answer::Deny(None)),
                        _ = h.cancel.cancelled() => Answer::Deny(Some("Stopped by the user".into())),
                        _ = stop.cancelled() => Answer::Deny(Some("Stopped by the user".into())),
                    }
                }
            },
        };

        let outcome = select_permission_outcome(&options, &answer, is_question);
        self.send(&json!({"jsonrpc": "2.0", "id": request["id"], "result": {"outcome": outcome}}))
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.alloc_id();
        self.send(&json!({"jsonrpc": "2.0", "method": method, "id": id, "params": params}))?;
        loop {
            let line = self
                .lines
                .next_line()
                .await?
                .context("Antigravity ACP stopped")?;
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value["id"].as_u64() == Some(id) {
                if let Some(error) = value.get("error") {
                    bail!(
                        "{}",
                        error["message"]
                            .as_str()
                            .unwrap_or("Antigravity ACP request failed")
                    );
                }
                return Ok(value);
            }
            self.pending.push_back(value);
        }
    }

    fn respond_error(&self, id: &Value, message: &str) -> Result<()> {
        self.send(&json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": message}}))
    }

    fn send(&self, value: &Value) -> Result<()> {
        self.tx
            .send(format!("{value}\n"))
            .map_err(|_| anyhow!("Antigravity ACP stdin closed"))
    }
}

struct Turn<'a> {
    model: &'a Model,
    session_id: String,
    emit: &'a EmitFn,
    reply: Option<AssistantMessage>,
    text: Option<usize>,
    thinking: Option<usize>,
    tools: HashMap<String, String>,
    usage: Usage,
}

impl<'a> Turn<'a> {
    fn new(model: &'a Model, session_id: &str, emit: &'a EmitFn) -> Self {
        Self {
            model,
            session_id: session_id.to_string(),
            emit,
            reply: None,
            text: None,
            thinking: None,
            tools: HashMap::new(),
            usage: Usage::default(),
        }
    }

    fn ensure_reply(&mut self) {
        if self.reply.is_none() {
            (self.emit)(AgentEvent::MessageStart(Message::Assistant(
                AssistantMessage::empty(self.model),
            )));
            self.reply = Some(AssistantMessage::empty(self.model));
        }
    }

    fn text(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.ensure_reply();
        // Close any open thinking block first so blocks stay strictly sequential for streaming.
        if let Some(index) = self.thinking.take() {
            (self.emit)(AgentEvent::MessageDelta(StreamEvent::ThinkingEnd { index }));
        }
        let reply = self.reply.as_mut().unwrap();
        let index = *self.text.get_or_insert_with(|| {
            let index = reply.content.len();
            reply.content.push(AssistantContent::Text {
                text: String::new(),
                text_signature: None,
            });
            (self.emit)(AgentEvent::MessageDelta(StreamEvent::TextStart { index }));
            index
        });
        if let Some(AssistantContent::Text { text, .. }) = reply.content.get_mut(index) {
            text.push_str(delta);
        }
        (self.emit)(AgentEvent::MessageDelta(StreamEvent::TextDelta {
            index,
            delta: delta.to_string(),
        }));
    }

    fn thinking(&mut self, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.ensure_reply();
        // Close any open text block first so subsequent text opens a fresh block after thinking.
        if let Some(index) = self.text.take() {
            (self.emit)(AgentEvent::MessageDelta(StreamEvent::TextEnd { index }));
        }
        let reply = self.reply.as_mut().unwrap();
        let index = *self.thinking.get_or_insert_with(|| {
            let index = reply.content.len();
            reply.content.push(AssistantContent::Thinking {
                thinking: String::new(),
                thinking_signature: None,
                redacted: false,
            });
            (self.emit)(AgentEvent::MessageDelta(StreamEvent::ThinkingStart {
                index,
            }));
            index
        });
        if let Some(AssistantContent::Thinking { thinking, .. }) = reply.content.get_mut(index) {
            thinking.push_str(delta);
        }
        (self.emit)(AgentEvent::MessageDelta(StreamEvent::ThinkingDelta {
            index,
            delta: delta.to_string(),
        }));
    }

    fn start_tool(&mut self, id: String, name: String, args: Value) {
        self.ensure_reply();
        if let Some(reply) = self.reply.as_mut() {
            reply.content.push(AssistantContent::ToolCall(ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: args.clone(),
                thought_signature: None,
                arguments_error: None,
            }));
        }
        self.close_reply(StopReason::ToolUse, None);
        (self.emit)(AgentEvent::ToolStart {
            id: id.clone(),
            name: name.clone(),
            args,
        });
        self.tools.insert(id, name);
    }

    fn session_update(&mut self, update: &Value) {
        match update["sessionUpdate"].as_str().unwrap_or("") {
            "agent_message_chunk" => {
                if let Some(text) = update.pointer("/content/text").and_then(Value::as_str) {
                    self.text(text);
                }
            }
            "agent_thought_chunk" => {
                if let Some(text) = update.pointer("/content/text").and_then(Value::as_str) {
                    self.thinking(text);
                }
            }
            "tool_call" => {
                let Some(id) = update["toolCallId"].as_str() else {
                    return;
                };
                if !self.tools.contains_key(id) {
                    let (name, args) = normalize_tool(update);
                    self.start_tool(id.to_string(), name, args);
                }
                if matches!(update["status"].as_str(), Some("completed" | "failed" | "cancelled" | "error")) {
                    self.finish_tool(update);
                }
            }
            "tool_call_update" => {
                let Some(id) = update["toolCallId"].as_str() else {
                    return;
                };
                if !self.tools.contains_key(id) {
                    let (name, args) = normalize_tool(update);
                    self.start_tool(id.to_string(), name, args);
                }
                match update["status"].as_str() {
                    Some("completed" | "failed" | "cancelled" | "error") => self.finish_tool(update),
                    _ => {
                        let text = tool_output_text(update);
                        if !text.is_empty() {
                            (self.emit)(AgentEvent::ToolUpdate {
                                id: id.to_string(),
                                partial: ToolResult::text(text),
                            });
                        }
                    }
                }
            }
            "usage_update" => {
                let used = update["used"].as_u64().unwrap_or(0);
                let window = update["size"]
                    .as_u64()
                    .unwrap_or(self.model.context_window);
                if window > 0 {
                    (self.emit)(AgentEvent::Context {
                        used,
                        window,
                        compacts_at: None,
                    });
                }
            }
            "current_mode_update" => {
                if let Some(mode) = update["currentModeId"].as_str() {
                    (self.emit)(AgentEvent::PermissionMode(mode.to_string()));
                }
            }
            _ => {}
        }
    }

    fn finish_tool(&mut self, update: &Value) {
        let id = update["toolCallId"].as_str().unwrap_or("").to_string();
        let name = self
            .tools
            .remove(&id)
            .unwrap_or_else(|| normalize_tool(update).0);
        let text = tool_output_text(update);
        let details = tool_output_details(update);
        let is_error = update["status"].as_str() == Some("failed");
        let result = ToolResultMessage {
            tool_call_id: id.clone(),
            tool_name: name.clone(),
            content: vec![UserContent::Text { text }],
            is_error,
            details,
            timestamp: now_ms(),
        };
        (self.emit)(AgentEvent::ToolEnd {
            id,
            name,
            result: result.clone(),
        });
        let message = Message::ToolResult(result);
        (self.emit)(AgentEvent::MessageStart(message.clone()));
        (self.emit)(AgentEvent::MessageEnd(message));
        if self.tools.is_empty() {
            // Show `Thinking…` again while Antigravity processes the tool result.
            self.ensure_reply();
        }
    }

    fn apply_prompt_usage(&mut self, usage: &Value) {
        if !usage.is_object() {
            return;
        }
        self.usage.input = usage["inputTokens"].as_u64().unwrap_or(self.usage.input);
        self.usage.output = usage["outputTokens"].as_u64().unwrap_or(self.usage.output);
        self.usage.cache_read = usage["cachedReadTokens"]
            .as_u64()
            .unwrap_or(self.usage.cache_read);
        self.usage.cache_write = usage["cachedWriteTokens"]
            .as_u64()
            .unwrap_or(self.usage.cache_write);
        self.usage.reasoning = usage["thoughtTokens"].as_u64().filter(|n| *n > 0);
        self.usage.total_tokens = usage["totalTokens"]
            .as_u64()
            .unwrap_or(self.usage.input + self.usage.output);
    }

    fn close_reply(&mut self, stop: StopReason, error: Option<String>) {
        let Some(mut reply) = self.reply.take() else {
            return;
        };
        if let Some(index) = self.text.take() {
            (self.emit)(AgentEvent::MessageDelta(StreamEvent::TextEnd { index }));
        }
        if let Some(index) = self.thinking.take() {
            (self.emit)(AgentEvent::MessageDelta(StreamEvent::ThinkingEnd { index }));
        }
        reply.response_id = Some(self.session_id.to_string());
        reply.usage = self.usage.clone();
        reply.stop_reason = stop;
        reply.error_message = error;
        reply.timestamp = now_ms();
        (self.emit)(AgentEvent::MessageEnd(Message::Assistant(reply)));
    }

    fn finish(&mut self, stop: StopReason, error: Option<String>) {
        if self.reply.is_none() && (stop == StopReason::Error || error.is_some()) {
            self.ensure_reply();
        }
        self.close_reply(stop, error);
        (self.emit)(AgentEvent::TurnEnd);
    }
}

fn alloc_id(next: &AtomicU64) -> u64 {
    next.fetch_add(1, Ordering::SeqCst) + 1
}

/// A JSON-RPC request from `agy acp` (it has a method and an id to answer).
fn is_request(value: &Value) -> bool {
    value.get("id").is_some() && value["method"].is_string()
}

fn parse_semver(version: &str) -> Option<(u64, u64, u64)> {
    let mut parts = version.trim().split(['.', '-', '+']);
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
    Some((major, minor, patch))
}

fn normalize_tool(tool: &Value) -> (String, Value) {
    let kind = tool["kind"].as_str().unwrap_or("");
    let raw = &tool["rawInput"];
    match kind {
        "execute" => {
            let command = raw["command"]
                .as_str()
                .or_else(|| raw["CommandLine"].as_str())
                .or_else(|| raw["command_line"].as_str())
                .or_else(|| raw["commandLine"].as_str())
                .or_else(|| tool["title"].as_str())
                .unwrap_or("");
            let cwd = raw["cwd"]
                .as_str()
                .or_else(|| raw["Cwd"].as_str())
                .or_else(|| raw["WorkingDirectory"].as_str())
                .unwrap_or("");
            let mut args = json!({"command": command});
            if !cwd.is_empty() {
                args["cwd"] = json!(cwd);
            }
            ("bash".into(), args)
        }
        "read" => {
            let path = raw["path"]
                .as_str()
                .or_else(|| raw["Path"].as_str())
                .or_else(|| raw["AbsolutePath"].as_str())
                .or_else(|| tool.pointer("/locations/0/path").and_then(Value::as_str))
                .unwrap_or("");
            ("read".into(), json!({"path": path}))
        }
        "edit" => {
            let path = raw["path"]
                .as_str()
                .or_else(|| raw["Path"].as_str())
                .or_else(|| raw["TargetFile"].as_str())
                .or_else(|| tool.pointer("/locations/0/path").and_then(Value::as_str))
                .unwrap_or("");
            let mut args = if raw.is_object() {
                raw.clone()
            } else {
                json!({})
            };
            if !path.is_empty() && args.get("path").is_none() {
                args["path"] = json!(path);
            }
            ("edit".into(), args)
        }
        "search" => (
            "grep".into(),
            if raw.is_object() {
                raw.clone()
            } else {
                json!({"title": tool["title"]})
            },
        ),
        "fetch" => (
            "web_search".into(),
            if raw.is_object() {
                raw.clone()
            } else {
                json!({"title": tool["title"]})
            },
        ),
        _ => {
            let name = tool["title"]
                .as_str()
                .filter(|s| !s.is_empty())
                .unwrap_or(if kind.is_empty() { "tool" } else { kind })
                .to_string();
            let args = if raw.is_null() {
                json!({})
            } else {
                raw.clone()
            };
            (name, args)
        }
    }
}

fn tool_output_text(update: &Value) -> String {
    let mut parts = Vec::new();
    if let Some(content) = update["content"].as_array() {
        for item in content {
            if item["type"].as_str() == Some("diff") {
                if let Some(path) = item["path"].as_str() {
                    parts.push(format!("edited {path}"));
                }
            } else if let Some(text) = item.pointer("/content/text").and_then(Value::as_str) {
                parts.push(text.to_string());
            } else if let Some(text) = item["text"].as_str() {
                parts.push(text.to_string());
            }
        }
    }
    if !parts.is_empty() {
        return parts.join("\n");
    }
    let raw = &update["rawOutput"];
    if let Some(s) = raw.as_str() {
        return s.to_string();
    }
    if let Some(s) = raw["output"]
        .as_str()
        .or_else(|| raw["combinedOutput"].as_str())
        .or_else(|| raw["combined_output"].as_str())
        .or_else(|| raw["stdout"].as_str())
    {
        return s.to_string();
    }
    if raw.is_object() || raw.is_array() {
        return raw.to_string();
    }
    String::new()
}

/// Extract inline diff details from ACP `content` diff blocks (or `rawOutput.diff`) so
/// Serpis's edit tool cards render diffs just like built-in, Claude Code, and Codex edits.
fn tool_output_details(update: &Value) -> Value {
    let mut diffs = Vec::new();
    let mut first_changed: Option<usize> = None;
    if let Some(content) = update["content"].as_array() {
        for item in content {
            if item["type"].as_str() == Some("diff") {
                let old = item["oldText"].as_str().unwrap_or("");
                let new = item["newText"].as_str().unwrap_or("");
                let rendered = crate::tools::edit_diff::generate_diff_string(old, new, 3);
                if first_changed.is_none() {
                    first_changed = rendered.first_changed_line;
                }
                if !rendered.diff.is_empty() {
                    diffs.push(rendered.diff);
                }
            }
        }
    }
    if !diffs.is_empty() {
        let mut obj = json!({"diff": diffs.join("\n")});
        if let Some(line) = first_changed {
            obj["firstChangedLine"] = json!(line);
        }
        return obj;
    }
    if let Some(diff) = update.pointer("/rawOutput/diff").and_then(Value::as_str)
        && !diff.is_empty()
    {
        return json!({"diff": diff});
    }
    Value::Null
}

fn select_permission_outcome(options: &[Value], answer: &Answer, is_question: bool) -> Value {
    let by_kind = |kind: &str| {
        options
            .iter()
            .find(|o| o["kind"].as_str() == Some(kind))
            .and_then(|o| o["optionId"].as_str())
    };
    let selected = |id: &str| json!({"outcome": "selected", "optionId": id});
    if is_question {
        if let Answer::Answers(answers) = answer
            && let Some(chosen) = answers.values().next()
        {
            if let Some(opt) = options.iter().find(|o| {
                o["name"].as_str() == Some(chosen.as_str())
                    || o["optionId"].as_str() == Some(chosen.as_str())
            }) && let Some(id) = opt["optionId"].as_str()
            {
                return selected(id);
            }
        }
        return json!({"outcome": "cancelled"});
    }
    match answer {
        Answer::Allow => by_kind("allow_once")
            .or_else(|| by_kind("allow_always"))
            .map(selected)
            .unwrap_or_else(|| json!({"outcome": "cancelled"})),
        Answer::Always => by_kind("allow_always")
            .or_else(|| by_kind("allow_once"))
            .map(selected)
            .unwrap_or_else(|| json!({"outcome": "cancelled"})),
        Answer::Deny(_) | Answer::Answers(_) | Answer::Plan(_) => by_kind("reject_once")
            .or_else(|| by_kind("reject_always"))
            .map(selected)
            .unwrap_or_else(|| json!({"outcome": "cancelled"})),
    }
}

fn prompt_blocks(message: &Message) -> Vec<Value> {
    match message {
        Message::User { content, .. } => content
            .iter()
            .map(|part| match part {
                UserContent::Text { text } => json!({"type": "text", "text": text}),
                UserContent::Image { data, mime_type } => {
                    json!({"type": "image", "data": data, "mimeType": mime_type})
                }
            })
            .collect(),
        _ => vec![json!({"type": "text", "text": message_text(message)})],
    }
}

fn message_text(message: &Message) -> String {
    match message {
        Message::User { content, .. } => content
            .iter()
            .filter_map(|p| match p {
                UserContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Message::Assistant(a) => a.text(),
        Message::ToolResult(r) => r
            .content
            .iter()
            .filter_map(|p| match p {
                UserContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn replay_history(history: &[Message]) -> String {
    history
        .iter()
        .filter_map(|message| {
            let text = message_text(message);
            if text.is_empty() {
                return None;
            }
            let role = match message {
                Message::User { .. } => "user",
                Message::Assistant(_) => "assistant",
                Message::ToolResult(_) => "tool",
            };
            Some(format!("[{role}]\n{text}"))
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_acp_tools_to_serpis_names() {
        let (name, args) = normalize_tool(&json!({
            "toolCallId": "tc-1",
            "kind": "execute",
            "title": "Run command",
            "rawInput": {"CommandLine": "git status", "Cwd": "/tmp"}
        }));
        assert_eq!(name, "bash");
        assert_eq!(args["command"], "git status");
        assert_eq!(args["cwd"], "/tmp");

        let (name, args) = normalize_tool(&json!({
            "toolCallId": "tc-2",
            "kind": "read",
            "rawInput": {"Path": "/tmp/a.rs"}
        }));
        assert_eq!(name, "read");
        assert_eq!(args["path"], "/tmp/a.rs");
    }

    #[test]
    fn renders_acp_diff_content_into_tool_details() {
        let update = json!({
            "toolCallId": "e-1",
            "status": "completed",
            "content": [{"type": "diff", "path": "notes.txt", "oldText": "hello\n", "newText": "world\n"}]
        });
        let details = tool_output_details(&update);
        let diff = details["diff"].as_str().unwrap();
        assert!(diff.contains("-1 hello"));
        assert!(diff.contains("+1 world"));
        assert_eq!(details["firstChangedLine"], 1);
        assert_eq!(tool_output_text(&update), "edited notes.txt");
    }

    #[test]
    fn parses_semver_for_steering_capability() {
        assert_eq!(parse_semver("1.5.15"), Some((1, 5, 15)));
        assert_eq!(parse_semver("1.5.0-beta.1"), Some((1, 5, 0)));
        assert!(parse_semver("1.5.0").unwrap() >= (1, 5, 0));
        assert!(parse_semver("1.4.9").unwrap() < (1, 5, 0));
    }

    #[test]
    fn maps_permission_answers_to_acp_outcomes() {
        let options = vec![
            json!({"optionId": "once", "name": "Allow once", "kind": "allow_once"}),
            json!({"optionId": "always", "name": "Allow always", "kind": "allow_always"}),
            json!({"optionId": "deny", "name": "Deny", "kind": "reject_once"}),
        ];
        assert_eq!(
            select_permission_outcome(&options, &Answer::Allow, false),
            json!({"outcome": "selected", "optionId": "once"})
        );
        assert_eq!(
            select_permission_outcome(&options, &Answer::Always, false),
            json!({"outcome": "selected", "optionId": "always"})
        );
        assert_eq!(
            select_permission_outcome(&options, &Answer::Deny(None), false),
            json!({"outcome": "selected", "optionId": "deny"})
        );
    }

    #[test]
    fn encodes_images_as_acp_image_blocks() {
        let message = Message::User {
            content: vec![UserContent::Image {
                data: "AA==".into(),
                mime_type: "image/png".into(),
            }],
            timestamp: 0,
        };
        assert_eq!(
            prompt_blocks(&message)[0],
            json!({"type": "image", "data": "AA==", "mimeType": "image/png"})
        );
    }
}
