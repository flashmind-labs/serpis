//! Codex as a chat agent, driven through `codex app-server`'s JSONL protocol.

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::{Context as _, Result, anyhow, bail};
use serde_json::{Value, json};
use serpis_ai::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, Lines};
use tokio::process::{Child, ChildStdin, ChildStdout};

use crate::agent::{AgentEvent, AgentHandle, Answer, AskReply, EmitFn, ToolGate, ToolHooks};
use crate::tools::ToolResult;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Launch {
    pub binary: PathBuf,
    pub cwd: PathBuf,
    /// More arguments after `codex app-server`.
    pub args: Vec<String>,
    /// Extra developer instructions, used to offer Serpis skills Codex does not already see.
    pub instructions: Option<String>,
    pub instructions_restart: bool,
    pub env: Vec<(String, String)>,
}

pub struct RunConfig {
    pub launch: Launch,
    pub model: Model,
    pub effort: Option<String>,
    pub approval_policy: Option<String>,
    pub sandbox: Option<String>,
    pub hooks: Option<Arc<dyn ToolHooks>>,
    pub prompts: Prompts,
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
pub struct Codex {
    process: Arc<tokio::sync::Mutex<Option<Process>>>,
    stop: Arc<std::sync::Mutex<CancellationToken>>,
}

pub fn is_reply(message: &Message) -> bool {
    matches!(message, Message::Assistant(a) if a.provider == serpis_ai::codex::PROVIDER)
}

impl Codex {
    pub fn run(
        &self,
        history: Vec<Message>,
        prompts: Vec<Message>,
        cfg: RunConfig,
        emit: EmitFn,
    ) -> AgentHandle {
        let handle = AgentHandle::new();
        let (me, h) = (self.clone(), handle.clone());
        tokio::spawn(async move {
            emit(AgentEvent::AgentStart);
            let error = me
                .drive(history, prompts, &cfg, &h, &emit)
                .await
                .err()
                .map(|e| format!("{e:#}"));
            emit(AgentEvent::AgentEnd { error });
        });
        handle
    }

    pub fn stop(&self) {
        self.stop.lock().unwrap().cancel();
        if let Ok(mut process) = self.process.try_lock() {
            if let Some(process) = process.as_mut() {
                let _ = process.child.start_kill();
            }
            *process = None;
        }
    }

    /// Ask Codex to compact its current thread. False when it is busy or has not started.
    pub fn compact(&self) -> bool {
        let Ok(mut process) = self.process.clone().try_lock_owned() else {
            return false;
        };
        let Some(thread_id) = process.as_ref().and_then(|p| p.thread_id.clone()) else {
            return false;
        };
        tokio::spawn(async move {
            if let Some(process) = process.as_mut() {
                let _ = process
                    .call("thread/compact/start", json!({"threadId":thread_id}))
                    .await;
            }
        });
        true
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
        let exited = guard
            .as_mut()
            .is_some_and(|p| p.child.try_wait().ok().flatten().is_some());
        let restart = exited
            || guard.as_ref().is_none_or(|p| {
                p.launch.binary != cfg.launch.binary
                    || p.launch.cwd != cfg.launch.cwd
                    || p.launch.args != cfg.launch.args
                    || p.launch.env != cfg.launch.env
                    || (cfg.launch.instructions_restart
                        && p.launch.instructions != cfg.launch.instructions)
            });
        if restart {
            if let Some(old) = guard.as_mut() {
                let _ = old.child.start_kill();
            }
            *guard = Some(Process::start(cfg.launch.clone()).await?);
        }
        let process = guard.as_mut().expect("process was started");
        let saved_thread = history.iter().rev().find_map(|m| match m {
            Message::Assistant(a) if a.provider == serpis_ai::codex::PROVIDER => {
                a.response_id.clone()
            }
            _ => None,
        });
        if process.thread_id.is_none() {
            process.open_thread(saved_thread.as_deref(), cfg).await?;
        }

        // When a chat switches to Codex, give it the conversation its own thread has not seen.
        // It is protocol context, not another visible user message.
        let mut replay = (saved_thread.is_none() && !history.is_empty())
            .then(|| replay_history(&history))
            .filter(|s| !s.is_empty());
        let mut continuing = prompts.is_empty();
        let mut pending: VecDeque<Message> = prompts.into();
        loop {
            let mut input = Vec::new();
            if let Some(context) = replay.take() {
                input.push(json!({"type":"text","text":context}));
            }
            if continuing && pending.is_empty() {
                input.push(json!({"type":"text","text":"Continue."}));
                continuing = false;
            }
            while let Some(message) = pending.pop_front() {
                emit(AgentEvent::MessageStart(message.clone()));
                emit(AgentEvent::MessageEnd(message.clone()));
                input.extend(input_of(&message));
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
    input: ChildStdin,
    lines: Lines<BufReader<ChildStdout>>,
    pending: VecDeque<Value>,
    next_id: u64,
    thread_id: Option<String>,
    launch: Launch,
}

impl Process {
    async fn start(launch: Launch) -> Result<Self> {
        let mut command = tokio::process::Command::new(&launch.binary);
        command
            .arg("app-server")
            .args(&launch.args)
            .current_dir(&launch.cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true);
        for (name, value) in &launch.env {
            command.env(name, value);
        }
        let mut child = command
            .spawn()
            .with_context(|| format!("starting {} app-server", launch.binary.display()))?;
        let input = child
            .stdin
            .take()
            .context("codex app-server has no stdin")?;
        let lines = BufReader::new(
            child
                .stdout
                .take()
                .context("codex app-server has no stdout")?,
        )
        .lines();
        let mut process = Process {
            child,
            input,
            lines,
            pending: VecDeque::new(),
            next_id: 0,
            thread_id: None,
            launch,
        };
        process
            .call(
                "initialize",
                json!({"clientInfo":{"name":"serpis","title":"Serpis","version":env!("CARGO_PKG_VERSION")}}),
            )
            .await?;
        process
            .send(&json!({"method":"initialized","params":{}}))
            .await?;
        Ok(process)
    }

    async fn open_thread(&mut self, saved: Option<&str>, cfg: &RunConfig) -> Result<()> {
        let mut params = json!({
            "cwd": cfg.launch.cwd,
            "model": cfg.model.id,
        });
        if let Some(policy) = &cfg.approval_policy {
            params["approvalPolicy"] = json!(policy);
        }
        if let Some(sandbox) = &cfg.sandbox {
            params["sandbox"] = json!(sandbox);
        }
        if let Some(instructions) = cfg.launch.instructions.as_deref().filter(|s| !s.is_empty()) {
            params["developerInstructions"] = json!(instructions);
        }
        let response = if let Some(thread_id) = saved {
            params["threadId"] = json!(thread_id);
            self.call("thread/resume", params).await
        } else {
            self.call("thread/start", params).await
        }?;
        let id = response
            .pointer("/result/thread/id")
            .and_then(Value::as_str)
            .context("Codex did not return a thread id")?;
        self.thread_id = Some(id.to_string());
        Ok(())
    }

    async fn turn(
        &mut self,
        input: Vec<Value>,
        cfg: &RunConfig,
        h: &AgentHandle,
        process_stop: &CancellationToken,
        emit: &EmitFn,
    ) -> Result<()> {
        let thread = self.thread_id.clone().context("Codex thread is not open")?;
        let mut params = json!({"threadId": thread, "input": input, "model": cfg.model.id});
        if let Some(effort) = &cfg.effort {
            params["effort"] = json!(effort);
        }
        if let Some(policy) = &cfg.approval_policy {
            params["approvalPolicy"] = json!(policy);
        }
        if let Some(sandbox) = &cfg.sandbox {
            params["sandboxPolicy"] = sandbox_policy(sandbox);
        }
        self.next_id += 1;
        let request_id = self.next_id;
        self.send(&json!({"method":"turn/start","id":request_id,"params":params}))
            .await?;
        let mut state = Turn::new(&cfg.model, &thread, emit);
        let mut interrupted = false;
        let mut turn_id: Option<String> = None;
        loop {
            let ahead = self.pending.pop_front();
            let value = if let Some(value) = ahead {
                Some(value)
            } else {
                tokio::select! {
                    line = self.lines.next_line() => self.parse_line(line?).transpose()?,
                    _ = h.steered(), if !interrupted && turn_id.is_some() => {
                        for message in h.take_steering() {
                            let params = json!({"threadId":thread,"expectedTurnId":turn_id,"input":input_of(&message)});
                            self.notify_call("turn/steer", params).await?;
                            emit(AgentEvent::MessageStart(message.clone()));
                            emit(AgentEvent::MessageEnd(message));
                        }
                        continue;
                    }
                    _ = h.cancel.cancelled(), if !interrupted && turn_id.is_some() => {
                        interrupted = true;
                        self.notify_call("turn/interrupt", json!({"threadId":thread,"turnId":turn_id})).await?;
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
                    if interrupted {
                        StopReason::Aborted
                    } else {
                        StopReason::Error
                    },
                    Some("Codex app-server stopped".into()),
                );
                bail!("Codex app-server stopped mid-turn");
            };
            if value["id"].as_u64() == Some(request_id) {
                if let Some(error) = value.get("error") {
                    state.finish(
                        StopReason::Error,
                        Some(
                            error["message"]
                                .as_str()
                                .unwrap_or("Codex refused the turn")
                                .into(),
                        ),
                    );
                    return Ok(());
                }
                turn_id = value
                    .pointer("/result/turn/id")
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .or(turn_id);
                continue;
            }
            if value.get("id").is_some() && value["method"].is_string() {
                self.approval(&value, cfg, h, emit).await?;
                continue;
            }
            let method = value["method"].as_str().unwrap_or("");
            let params = &value["params"];
            match method {
                "turn/started" => {
                    turn_id = params
                        .pointer("/turn/id")
                        .and_then(Value::as_str)
                        .map(str::to_string)
                        .or(turn_id)
                }
                "item/agentMessage/delta" => state.text(params["delta"].as_str().unwrap_or("")),
                "item/reasoning/summaryTextDelta" | "item/reasoning/textDelta" => {
                    state.thinking(params["delta"].as_str().unwrap_or(""))
                }
                "item/started" => state.item_started(&params["item"]),
                "item/completed" => state.item_completed(&params["item"]),
                "item/commandExecution/outputDelta" | "command/exec/outputDelta" => {
                    state.tool_update(params)
                }
                "thread/tokenUsage/updated" => state.usage(&params["tokenUsage"]),
                "thread/compacted" => emit(AgentEvent::Compacted {
                    auto: true,
                    before: state.context_used,
                    after: None,
                }),
                "turn/completed" => {
                    let status = params
                        .pointer("/turn/status")
                        .and_then(Value::as_str)
                        .unwrap_or("failed");
                    let stop = match status {
                        "completed" => StopReason::Stop,
                        "interrupted" => StopReason::Aborted,
                        _ => StopReason::Error,
                    };
                    let error = params
                        .pointer("/turn/error/message")
                        .and_then(Value::as_str)
                        .map(str::to_string);
                    state.finish(stop, error);
                    return Ok(());
                }
                _ => {}
            }
        }
    }

    async fn approval(
        &mut self,
        request: &Value,
        cfg: &RunConfig,
        h: &AgentHandle,
        emit: &EmitFn,
    ) -> Result<()> {
        let method = request["method"].as_str().unwrap_or("");
        let params = &request["params"];
        let id = params["itemId"]
            .as_str()
            .or_else(|| params["callId"].as_str())
            .unwrap_or("")
            .to_string();
        let (name, args, always) = match method {
            "item/commandExecution/requestApproval" | "execCommandApproval" => (
                "bash",
                json!({"command":params["command"],"cwd":params["cwd"]}),
                Some("similar commands in this session".to_string()),
            ),
            "item/fileChange/requestApproval" | "applyPatchApproval" => (
                "edit",
                params
                    .get("fileChanges")
                    .cloned()
                    .unwrap_or_else(|| params.clone()),
                Some("these files in this session".to_string()),
            ),
            "item/permissions/requestApproval" => (
                "permissions",
                params["permissions"].clone(),
                Some("these permissions for this session".to_string()),
            ),
            "item/tool/requestUserInput" => (
                "AskUserQuestion",
                json!({"questions":params["questions"]}),
                None,
            ),
            _ => {
                self.respond_error(&request["id"], "Serpis does not handle this Codex request")
                    .await?;
                return Ok(());
            }
        };
        emit(AgentEvent::ToolStart {
            id: id.clone(),
            name: name.to_string(),
            args: args.clone(),
        });
        let gate = match &cfg.hooks {
            Some(hooks) => hooks.before_tool_call(name, args).await,
            None => ToolGate::Allow(Value::Null),
        };
        let answer = match gate {
            ToolGate::Block(reason) => Answer::Deny(Some(reason)),
            ToolGate::Allow(_) => match cfg.prompts {
                Prompts::Allow => Answer::Allow,
                Prompts::Deny => Answer::Deny(Some("Refused by codex-permission-prompts".into())),
                Prompts::Ask => {
                    let (reply, answer) = AskReply::new();
                    emit(AgentEvent::ToolAsk { id, always, reply });
                    tokio::select! {
                        answer = answer => answer.unwrap_or(Answer::Deny(None)),
                        _ = h.cancel.cancelled() => Answer::Deny(Some("Stopped by the user".into())),
                    }
                }
            },
        };
        let result = if method == "item/tool/requestUserInput" {
            let given = match answer {
                Answer::Answers(answers) => answers,
                _ => Default::default(),
            };
            let answers: serde_json::Map<String, Value> = params["questions"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|question| {
                    let id = question["id"].as_str()?;
                    let text = question["question"].as_str().unwrap_or(id);
                    let answer = given.get(text).or_else(|| given.get(id)).cloned().unwrap_or_default();
                    Some((id.to_string(), json!({"answers":if answer.is_empty() { Vec::<String>::new() } else { vec![answer] }})))
                })
                .collect();
            json!({"answers":answers})
        } else if matches!(method, "execCommandApproval" | "applyPatchApproval") {
            let decision = match answer {
                Answer::Allow => json!("approved"),
                Answer::Always => json!("approved_for_session"),
                Answer::Deny(reason) => {
                    json!({"denied":{"rejection":reason.unwrap_or_else(|| "Denied by the user".into())}})
                }
                _ => json!({"denied":{"rejection":"Denied by the user"}}),
            };
            json!({"decision":decision})
        } else if method == "item/permissions/requestApproval" {
            match answer {
                Answer::Allow => json!({"permissions":params["permissions"],"scope":"turn"}),
                Answer::Always => json!({"permissions":params["permissions"],"scope":"session"}),
                _ => json!({"permissions":{}}),
            }
        } else {
            let decision = match answer {
                Answer::Allow => "accept",
                Answer::Always => "acceptForSession",
                _ => "decline",
            };
            json!({"decision":decision})
        };
        self.send(&json!({"id":request["id"],"result":result}))
            .await
    }

    async fn call(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        self.send(&json!({"method":method,"id":id,"params":params}))
            .await?;
        loop {
            let line = self
                .lines
                .next_line()
                .await?
                .context("Codex app-server stopped")?;
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value["id"].as_u64() == Some(id) {
                if let Some(error) = value.get("error") {
                    bail!(
                        "{}",
                        error["message"].as_str().unwrap_or("Codex request failed")
                    );
                }
                return Ok(value);
            }
            self.pending.push_back(value);
        }
    }

    async fn notify_call(&mut self, method: &str, params: Value) -> Result<()> {
        self.next_id += 1;
        self.send(&json!({"method":method,"id":self.next_id,"params":params}))
            .await
    }

    async fn respond_error(&mut self, id: &Value, message: &str) -> Result<()> {
        self.send(&json!({"id":id,"error":{"code":-32601,"message":message}}))
            .await
    }

    async fn send(&mut self, value: &Value) -> Result<()> {
        self.input
            .write_all(format!("{value}\n").as_bytes())
            .await
            .context("writing to Codex app-server")
    }

    fn parse_line(&self, line: Option<String>) -> Option<Result<Value>> {
        line.map(|line| serde_json::from_str(&line).map_err(|e| anyhow!(e)))
    }
}

struct Turn<'a> {
    model: &'a Model,
    thread_id: &'a str,
    emit: &'a EmitFn,
    reply: Option<AssistantMessage>,
    text: Option<usize>,
    thinking: Option<usize>,
    tools: HashMap<String, String>,
    output: HashMap<String, String>,
    usage: Usage,
    context_used: u64,
}

impl<'a> Turn<'a> {
    fn new(model: &'a Model, thread_id: &'a str, emit: &'a EmitFn) -> Self {
        Self {
            model,
            thread_id,
            emit,
            reply: None,
            text: None,
            thinking: None,
            tools: HashMap::new(),
            output: HashMap::new(),
            usage: Usage::default(),
            context_used: 0,
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
        self.ensure_reply();
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
        self.ensure_reply();
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

    fn item_started(&mut self, item: &Value) {
        let Some((id, name, args)) = tool_of(item) else {
            return;
        };
        self.close_reply(StopReason::ToolUse, None);
        let mut message = AssistantMessage::empty(self.model);
        message.response_id = Some(self.thread_id.to_string());
        message.stop_reason = StopReason::ToolUse;
        message.content.push(AssistantContent::ToolCall(ToolCall {
            id: id.clone(),
            name: name.clone(),
            arguments: args.clone(),
            thought_signature: None,
            arguments_error: None,
        }));
        (self.emit)(AgentEvent::MessageStart(Message::Assistant(
            AssistantMessage::empty(self.model),
        )));
        (self.emit)(AgentEvent::MessageEnd(Message::Assistant(message)));
        (self.emit)(AgentEvent::ToolStart {
            id: id.clone(),
            name: name.clone(),
            args,
        });
        self.tools.insert(id, name);
    }

    fn item_completed(&mut self, item: &Value) {
        match item["type"].as_str() {
            Some("agentMessage") if self.reply.is_none() => {
                self.text(item["text"].as_str().unwrap_or(""))
            }
            Some(
                "commandExecution"
                | "fileChange"
                | "mcpToolCall"
                | "dynamicToolCall"
                | "webSearch"
                | "imageView"
                | "collabAgentToolCall",
            ) => {
                let id = item["id"].as_str().unwrap_or("").to_string();
                let name = self.tools.remove(&id).unwrap_or_else(|| {
                    tool_of(item)
                        .map(|(_, n, _)| n)
                        .unwrap_or_else(|| "tool".into())
                });
                let result = result_of(item, &name, self.output.remove(&id));
                (self.emit)(AgentEvent::ToolEnd {
                    id: id.clone(),
                    name,
                    result: result.clone(),
                });
                let message = Message::ToolResult(result);
                (self.emit)(AgentEvent::MessageStart(message.clone()));
                (self.emit)(AgentEvent::MessageEnd(message));
            }
            _ => {}
        }
    }

    fn tool_update(&mut self, params: &Value) {
        let id = params["itemId"].as_str().unwrap_or("").to_string();
        let output = self.output.entry(id.clone()).or_default();
        output.push_str(params["delta"].as_str().unwrap_or(""));
        (self.emit)(AgentEvent::ToolUpdate {
            id,
            partial: ToolResult::text(output.clone()),
        });
    }

    fn usage(&mut self, token: &Value) {
        let last = &token["last"];
        self.usage.input = last["inputTokens"].as_u64().unwrap_or(0);
        self.usage.output = last["outputTokens"].as_u64().unwrap_or(0);
        self.usage.cache_read = last["cachedInputTokens"].as_u64().unwrap_or(0);
        self.usage.cache_write = last["cacheWriteInputTokens"].as_u64().unwrap_or(0);
        self.usage.reasoning = last["reasoningOutputTokens"].as_u64().filter(|n| *n > 0);
        self.usage.total_tokens = last["totalTokens"]
            .as_u64()
            .unwrap_or(self.usage.input + self.usage.output);
        self.context_used = token
            .pointer("/total/totalTokens")
            .and_then(Value::as_u64)
            .unwrap_or(self.context_used);
        if let Some(window) = token["modelContextWindow"].as_u64() {
            (self.emit)(AgentEvent::Context {
                used: self.context_used,
                window,
                compacts_at: None,
            });
        }
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
        reply.response_id = Some(self.thread_id.to_string());
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

fn input_of(message: &Message) -> Vec<Value> {
    match message {
        Message::User { content, .. } => content
            .iter()
            .map(|part| match part {
                UserContent::Text { text } => json!({"type":"text","text":text}),
                UserContent::Image { data, mime_type } => {
                    json!({"type":"image","url":format!("data:{mime_type};base64,{data}")})
                }
            })
            .collect(),
        _ => vec![json!({"type":"text","text":message_text(message)})],
    }
}

fn message_text(message: &Message) -> String {
    match message {
        Message::User { content, .. } => content
            .iter()
            .filter_map(|p| {
                if let UserContent::Text { text } = p {
                    Some(text.as_str())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Message::Assistant(a) => a.text(),
        Message::ToolResult(r) => r
            .content
            .iter()
            .filter_map(|p| {
                if let UserContent::Text { text } = p {
                    Some(text.as_str())
                } else {
                    None
                }
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

fn sandbox_policy(mode: &str) -> Value {
    match mode {
        "read-only" => json!({"type":"readOnly"}),
        "danger-full-access" => json!({"type":"dangerFullAccess"}),
        _ => {
            json!({"type":"workspaceWrite","writableRoots":[],"networkAccess":false,"excludeTmpdirEnvVar":false,"excludeSlashTmp":false})
        }
    }
}

fn tool_of(item: &Value) -> Option<(String, String, Value)> {
    let id = item["id"].as_str()?.to_string();
    let pair = match item["type"].as_str()? {
        "commandExecution" => (
            "bash".into(),
            json!({"command":item["command"],"cwd":item["cwd"]}),
        ),
        "fileChange" => ("edit".into(), json!({"changes":item["changes"]})),
        "mcpToolCall" => (
            format!(
                "{}.{}",
                item["server"].as_str().unwrap_or("mcp"),
                item["tool"].as_str().unwrap_or("tool")
            ),
            item["arguments"].clone(),
        ),
        "dynamicToolCall" => (
            item["tool"].as_str().unwrap_or("tool").to_string(),
            item["arguments"].clone(),
        ),
        "webSearch" => ("web_search".into(), json!({"query":item["query"]})),
        "imageView" => ("read".into(), json!({"path":item["path"]})),
        "collabAgentToolCall" => (
            item["tool"].as_str().unwrap_or("agent").to_string(),
            json!({"prompt":item["prompt"],"model":item["model"]}),
        ),
        _ => return None,
    };
    Some((id, pair.0, pair.1))
}

fn result_of(item: &Value, name: &str, streamed: Option<String>) -> ToolResultMessage {
    let status = item["status"].as_str().unwrap_or("completed");
    let (text, details) = match item["type"].as_str() {
        Some("commandExecution") => (
            streamed
                .or_else(|| item["aggregatedOutput"].as_str().map(str::to_string))
                .unwrap_or_default(),
            Value::Null,
        ),
        Some("fileChange") => {
            let changes = item["changes"].as_array().cloned().unwrap_or_default();
            let text = changes
                .iter()
                .map(|c| {
                    format!(
                        "{} {}",
                        c["kind"].as_str().unwrap_or("update"),
                        c["path"].as_str().unwrap_or("")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let diff = changes
                .iter()
                .filter_map(|c| c["diff"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            (text, json!({"diff":diff}))
        }
        Some("mcpToolCall") => (
            item.pointer("/result/content")
                .map(Value::to_string)
                .unwrap_or_default(),
            item["result"].clone(),
        ),
        Some("webSearch") => (item["results"].to_string(), Value::Null),
        _ => (streamed.unwrap_or_else(|| item.to_string()), Value::Null),
    };
    ToolResultMessage {
        tool_call_id: item["id"].as_str().unwrap_or("").to_string(),
        tool_name: name.to_string(),
        content: vec![UserContent::Text { text }],
        is_error: matches!(status, "failed" | "declined"),
        details,
        timestamp: now_ms(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_codex_items_to_serpis_tools() {
        let (id, name, args) = tool_of(
            &json!({"type":"commandExecution","id":"x","command":"git status","cwd":"/tmp"}),
        )
        .unwrap();
        assert_eq!((id.as_str(), name.as_str()), ("x", "bash"));
        assert_eq!(args["command"], "git status");
        let result = result_of(
            &json!({"type":"fileChange","id":"e","status":"completed","changes":[{"kind":"update","path":"a.rs","diff":"@@ -1 +1 @@"}]}),
            "edit",
            None,
        );
        assert_eq!(result.details["diff"], "@@ -1 +1 @@");
    }

    #[test]
    fn images_are_data_urls_for_app_server() {
        let message = Message::User {
            content: vec![UserContent::Image {
                data: "AA==".into(),
                mime_type: "image/png".into(),
            }],
            timestamp: 0,
        };
        assert_eq!(input_of(&message)[0]["url"], "data:image/png;base64,AA==");
    }
}
