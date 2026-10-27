//! serpis-agent: pi's agent loop and built-in coding tools, in Rust.

pub mod agent;
pub mod claude_code;
pub mod compaction;
pub mod system_prompt;
pub mod tools;

pub use agent::{AGENT_ENV, AgentConfig, AgentEvent, AgentHandle, Answer, AskReply, EmitFn, ToolGate, ToolHooks, complete, run, supports_native_web_search};
pub use tools::{AgentTool, ExecutionMode, ToolContext, ToolRef, ToolResult, builtin_tools};
