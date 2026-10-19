//! serpis-agent: pi's agent loop and built-in coding tools, in Rust.

pub mod agent;
pub mod system_prompt;
pub mod tools;

pub use agent::{AgentConfig, AgentEvent, AgentHandle, EmitFn, ToolGate, ToolHooks, run, supports_native_web_search};
pub use tools::{AgentTool, ExecutionMode, ToolContext, ToolRef, ToolResult, builtin_tools};
