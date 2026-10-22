//! The agent loop (pi `agent-loop.ts`): stream a response, run its tool calls, feed the
//! results back, repeat until the model stops. Steering messages queued during a run are
//! injected before the next model call; follow-ups restart the loop after it would stop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures::future::{BoxFuture, join_all};
use serde_json::Value;
use serpis_ai::*;

use crate::tools::{ExecutionMode, ToolContext, ToolRef, ToolResult, validate_arguments};

#[derive(Debug, Clone)]
pub enum AgentEvent {
    AgentStart,
    TurnStart,
    /// A complete message entered the transcript start (user, tool result) or an assistant
    /// message began streaming (empty content).
    MessageStart(Message),
    /// Streaming delta for the assistant message currently in flight.
    MessageDelta(StreamEvent),
    MessageEnd(Message),
    ToolStart { id: String, name: String, args: Value },
    ToolUpdate { id: String, partial: ToolResult },
    ToolEnd { id: String, name: String, result: ToolResultMessage },
    TurnEnd,
    AgentEnd { error: Option<String> },
}

pub type EmitFn = Arc<dyn Fn(AgentEvent) + Send + Sync>;

/// Decision of a `before_tool_call` hook.
pub enum ToolGate {
    Allow(Value),
    Block(String),
}

/// Extension points around tool execution, implemented by the editor (Lua/Rust hooks).
pub trait ToolHooks: Send + Sync {
    fn before_tool_call(&self, _name: &str, args: Value) -> BoxFuture<'static, ToolGate> {
        Box::pin(async move { ToolGate::Allow(args) })
    }
    fn after_tool_call(&self, _name: &str, result: ToolResult) -> BoxFuture<'static, ToolResult> {
        Box::pin(async move { result })
    }
}

#[derive(Clone)]
pub struct AgentConfig {
    pub model: Model,
    pub reasoning: ThinkingLevel,
    pub tools: Vec<ToolRef>,
    pub system_prompt: String,
    pub api_key: Option<String>,
    pub headers: HashMap<String, String>,
    pub session_id: String,
    pub cwd: PathBuf,
    /// Ask the provider to search instead of offering the client-side `web_search` tool.
    pub native_web_search: bool,
    pub max_tokens: Option<u64>,
    /// How long providers should keep this chat's prompt cached.
    pub cache_retention: CacheRetention,
    pub hooks: Option<Arc<dyn ToolHooks>>,
    pub sequential_tools: bool,
}

/// Whether a model's API can search the web server-side.
pub fn supports_native_web_search(model: &Model) -> bool {
    match model.api.as_str() {
        "anthropic-messages" => !model.is_openrouter(),
        "openai-responses" => model.provider == "openai",
        "google-generative-ai" => true,
        // OpenRouter's "web" plugin searches on every request rather than letting the model
        // decide, so chat-completions models use the client-side tool instead.
        _ => false,
    }
}

/// Control handle for a running agent.
#[derive(Clone)]
pub struct AgentHandle {
    pub cancel: CancellationToken,
    steering: Arc<Mutex<Vec<Message>>>,
    follow_ups: Arc<Mutex<Vec<Message>>>,
}

impl AgentHandle {
    /// Inject a message before the next model call (interrupting the current plan).
    pub fn steer(&self, msg: Message) {
        self.steering.lock().unwrap().push(msg);
    }
    /// Queue a message to run once the agent would otherwise stop.
    pub fn follow_up(&self, msg: Message) {
        self.follow_ups.lock().unwrap().push(msg);
    }
    pub fn abort(&self) {
        self.cancel.cancel();
    }
}

/// Start an agent run over `history` with new `prompts`. Events go to `emit`; the transcript
/// owner appends each `MessageEnd` message.
pub fn run(history: Vec<Message>, prompts: Vec<Message>, cfg: AgentConfig, emit: EmitFn) -> AgentHandle {
    let handle = AgentHandle {
        cancel: CancellationToken::new(),
        steering: Default::default(),
        follow_ups: Default::default(),
    };
    let h = handle.clone();
    tokio::spawn(async move {
        emit(AgentEvent::AgentStart);
        let error = run_loop(history, prompts, &cfg, &h, &emit).await.err().map(|e| format!("{e:#}"));
        emit(AgentEvent::AgentEnd { error });
    });
    handle
}

async fn run_loop(
    mut messages: Vec<Message>,
    prompts: Vec<Message>,
    cfg: &AgentConfig,
    h: &AgentHandle,
    emit: &EmitFn,
) -> anyhow::Result<()> {
    let (native, tools) = offered_tools(cfg);
    let mut pending = prompts;
    pending.extend(std::mem::take(&mut *h.steering.lock().unwrap()));
    let mut first = true;
    loop {
        let mut has_more_tool_calls = true;
        while has_more_tool_calls || !pending.is_empty() {
            if !first {
                emit(AgentEvent::TurnStart);
            }
            first = false;
            for m in pending.drain(..) {
                emit(AgentEvent::MessageStart(m.clone()));
                emit(AgentEvent::MessageEnd(m.clone()));
                messages.push(m);
            }
            let (ctx, opts) = request(cfg, native, &tools, messages.clone(), h.cancel.child_token());
            emit(AgentEvent::MessageStart(Message::Assistant(AssistantMessage::empty(&cfg.model))));
            let mut rx = serpis_ai::stream(&cfg.model, &ctx, opts);
            let mut final_msg = None;
            while let Some(ev) = rx.recv().await {
                match ev {
                    StreamEvent::Done(m) | StreamEvent::Error(m) => {
                        final_msg = Some(m);
                        break;
                    }
                    other => emit(AgentEvent::MessageDelta(other)),
                }
            }
            let assistant = final_msg.unwrap_or_else(|| {
                let mut m = AssistantMessage::empty(&cfg.model);
                m.stop_reason = StopReason::Error;
                m.error_message = Some("stream closed unexpectedly".into());
                m
            });
            emit(AgentEvent::MessageEnd(Message::Assistant(assistant.clone())));
            messages.push(Message::Assistant(assistant.clone()));
            if matches!(assistant.stop_reason, StopReason::Error | StopReason::Aborted) {
                emit(AgentEvent::TurnEnd);
                return Ok(());
            }
            let calls: Vec<ToolCall> = assistant.tool_calls().cloned().collect();
            has_more_tool_calls = !calls.is_empty();
            if has_more_tool_calls {
                let results = if assistant.stop_reason == StopReason::Length {
                    fail_truncated(&calls, emit)
                } else {
                    execute_tool_calls(&calls, &tools, cfg, h, emit).await
                };
                for r in results {
                    let m = Message::ToolResult(r);
                    emit(AgentEvent::MessageStart(m.clone()));
                    emit(AgentEvent::MessageEnd(m.clone()));
                    messages.push(m);
                }
            }
            emit(AgentEvent::TurnEnd);
            if h.cancel.is_cancelled() {
                return Ok(());
            }
            pending.extend(std::mem::take(&mut *h.steering.lock().unwrap()));
        }
        let follow = std::mem::take(&mut *h.follow_ups.lock().unwrap());
        if follow.is_empty() {
            return Ok(());
        }
        pending = follow;
    }
}

/// Whether the provider searches the web itself, and the tools offered to the model.
fn offered_tools(cfg: &AgentConfig) -> (bool, Vec<ToolRef>) {
    let native = cfg.native_web_search && supports_native_web_search(&cfg.model);
    let tools = cfg.tools.iter().filter(|t| !(native && t.name() == "web_search")).cloned().collect();
    (native, tools)
}

/// A request exactly as the agent loop sends it. Anything else sent for the same chat
/// (`complete`) goes through here, so it shares the chat's prompt cache.
fn request(cfg: &AgentConfig, native: bool, tools: &[ToolRef], messages: Vec<Message>, cancel: CancellationToken) -> (Context, StreamOptions) {
    let ctx = Context {
        system_prompt: cfg.system_prompt.clone(),
        messages,
        tools: tools.iter().map(|t| t.declaration()).collect(),
    };
    let opts = StreamOptions {
        api_key: cfg.api_key.clone(),
        headers: cfg.headers.clone(),
        reasoning: Some(cfg.reasoning),
        max_tokens: cfg.max_tokens,
        session_id: Some(cfg.session_id.clone()),
        cache_retention: cfg.cache_retention,
        cancel,
        native_web_search: native,
        ..Default::default()
    };
    (ctx, opts)
}

/// One model response to `messages`, sent as the agent loop would send it (same system prompt,
/// tools and options) but without running tools: a side request that still reads the chat's
/// prompt cache, such as a compaction summary.
pub async fn complete(cfg: &AgentConfig, messages: Vec<Message>, cancel: CancellationToken) -> AssistantMessage {
    let (native, tools) = offered_tools(cfg);
    let (ctx, opts) = request(cfg, native, &tools, messages, cancel);
    let mut rx = serpis_ai::stream(&cfg.model, &ctx, opts);
    while let Some(ev) = rx.recv().await {
        if let StreamEvent::Done(m) | StreamEvent::Error(m) = ev {
            return m;
        }
    }
    let mut m = AssistantMessage::empty(&cfg.model);
    m.stop_reason = StopReason::Error;
    m.error_message = Some("stream closed unexpectedly".into());
    m
}

fn error_result(call: &ToolCall, text: String) -> ToolResultMessage {
    ToolResultMessage {
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        content: vec![UserContent::Text { text }],
        is_error: true,
        details: Value::Null,
        timestamp: now_ms(),
    }
}

fn fail_truncated(calls: &[ToolCall], emit: &EmitFn) -> Vec<ToolResultMessage> {
    calls
        .iter()
        .map(|c| {
            emit(AgentEvent::ToolStart {
                id: c.id.clone(),
                name: c.name.clone(),
                args: c.arguments.clone(),
            });
            let r = error_result(
                c,
                format!(
                    "Tool call \"{}\" was not executed: the response hit the output token limit, so its arguments may be truncated. Re-issue the tool call with complete arguments.",
                    c.name
                ),
            );
            emit(AgentEvent::ToolEnd {
                id: c.id.clone(),
                name: c.name.clone(),
                result: r.clone(),
            });
            r
        })
        .collect()
}

enum Prepared {
    Immediate(ToolResultMessage),
    Run(ToolRef, Value),
}

async fn prepare(call: &ToolCall, tools: &[ToolRef], cfg: &AgentConfig) -> Prepared {
    let Some(tool) = tools.iter().find(|t| t.name() == call.name).cloned() else {
        return Prepared::Immediate(error_result(call, format!("Tool {} not found", call.name)));
    };
    if let Some(err) = &call.arguments_error {
        return Prepared::Immediate(error_result(call, err.clone()));
    }
    let args = tool.prepare_arguments(call.arguments.clone());
    let args = match validate_arguments(&call.name, &tool.parameters(), &args) {
        Ok(a) => a,
        Err(e) => return Prepared::Immediate(error_result(call, e)),
    };
    let args = match &cfg.hooks {
        Some(hooks) => match hooks.before_tool_call(&call.name, args).await {
            ToolGate::Allow(a) => a,
            ToolGate::Block(reason) => return Prepared::Immediate(error_result(call, reason)),
        },
        None => args,
    };
    Prepared::Run(tool, args)
}

fn session_env(cfg: &AgentConfig) -> HashMap<String, String> {
    HashMap::from([
        ("SERPIS_SESSION_ID".into(), cfg.session_id.clone()),
        ("SERPIS_PROVIDER".into(), cfg.model.provider.clone()),
        ("SERPIS_MODEL".into(), cfg.model.id.clone()),
        ("SERPIS_REASONING_LEVEL".into(), cfg.reasoning.as_str().into()),
    ])
}

async fn execute_one(call: ToolCall, tool: ToolRef, args: Value, cfg: AgentConfig, h: AgentHandle, emit: EmitFn) -> ToolResultMessage {
    if h.cancel.is_cancelled() {
        return error_result(&call, "Operation aborted".into());
    }
    let id = call.id.clone();
    let emit_u = emit.clone();
    let ctx = ToolContext {
        cwd: cfg.cwd.clone(),
        cancel: h.cancel.child_token(),
        on_update: Some(Arc::new(move |partial| {
            emit_u(AgentEvent::ToolUpdate {
                id: id.clone(),
                partial,
            })
        })),
        model: Some(cfg.model.clone()),
        env: session_env(&cfg),
    };
    let result = match tool.execute(call.id.clone(), args, ctx).await {
        Ok(r) => r,
        Err(e) => ToolResult::error(format!("{e:#}")),
    };
    let result = match &cfg.hooks {
        Some(hooks) => hooks.after_tool_call(&call.name, result).await,
        None => result,
    };
    ToolResultMessage {
        tool_call_id: call.id.clone(),
        tool_name: call.name.clone(),
        content: result.content,
        is_error: result.is_error,
        details: result.details,
        timestamp: now_ms(),
    }
}

async fn execute_tool_calls(
    calls: &[ToolCall],
    tools: &[ToolRef],
    cfg: &AgentConfig,
    h: &AgentHandle,
    emit: &EmitFn,
) -> Vec<ToolResultMessage> {
    let sequential = cfg.sequential_tools
        || calls
            .iter()
            .any(|c| tools.iter().any(|t| t.name() == c.name && t.execution_mode() == ExecutionMode::Sequential));
    let mut out = Vec::new();
    let mut parallel: Vec<BoxFuture<'static, ToolResultMessage>> = Vec::new();
    for call in calls {
        emit(AgentEvent::ToolStart {
            id: call.id.clone(),
            name: call.name.clone(),
            args: call.arguments.clone(),
        });
        let finished = |r: ToolResultMessage| {
            emit(AgentEvent::ToolEnd {
                id: r.tool_call_id.clone(),
                name: r.tool_name.clone(),
                result: r.clone(),
            });
            r
        };
        match prepare(call, tools, cfg).await {
            Prepared::Immediate(r) => {
                if sequential {
                    out.push(finished(r));
                } else {
                    let r = finished(r);
                    parallel.push(Box::pin(async move { r }));
                }
            }
            Prepared::Run(tool, args) => {
                let fut = execute_one(call.clone(), tool, args, cfg.clone(), h.clone(), emit.clone());
                if sequential {
                    let r = fut.await;
                    out.push(finished(r));
                    if h.cancel.is_cancelled() {
                        break;
                    }
                } else {
                    let emit = emit.clone();
                    parallel.push(Box::pin(async move {
                        let r = fut.await;
                        emit(AgentEvent::ToolEnd {
                            id: r.tool_call_id.clone(),
                            name: r.tool_name.clone(),
                            result: r.clone(),
                        });
                        r
                    }));
                }
            }
        }
    }
    if !sequential {
        out = join_all(parallel).await;
    }
    out
}
