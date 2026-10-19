//! Built-in tools and the tool contract.

pub mod bash;
pub mod edit;
pub mod edit_diff;
pub mod find;
pub mod grep;
pub mod ls;
pub mod output;
pub mod path_utils;
pub mod read;
pub mod truncate;
pub mod web_search;
pub mod write;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use futures::future::BoxFuture;
use serde_json::{Value, json};
use serpis_ai::{CancellationToken, Model, Tool, UserContent};

/// What a tool returns. `details` is for renderers (diffs, truncation info) and never reaches the model.
#[derive(Debug, Clone, Default)]
pub struct ToolResult {
    pub content: Vec<UserContent>,
    pub details: Value,
    pub is_error: bool,
}

impl ToolResult {
    pub fn text(text: impl Into<String>) -> Self {
        ToolResult {
            content: vec![UserContent::Text { text: text.into() }],
            details: Value::Null,
            is_error: false,
        }
    }

    pub fn error(text: impl Into<String>) -> Self {
        ToolResult {
            is_error: true,
            ..Self::text(text)
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = details;
        self
    }

    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|c| match c {
                UserContent::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
}

pub type UpdateFn = Arc<dyn Fn(ToolResult) + Send + Sync>;

/// Per-call execution context.
#[derive(Clone)]
pub struct ToolContext {
    pub cwd: PathBuf,
    pub cancel: CancellationToken,
    pub on_update: Option<UpdateFn>,
    pub model: Option<Model>,
    /// Extra environment for spawned processes (session metadata).
    pub env: HashMap<String, String>,
}

impl ToolContext {
    pub fn new(cwd: impl Into<PathBuf>) -> Self {
        ToolContext {
            cwd: cwd.into(),
            cancel: CancellationToken::new(),
            on_update: None,
            model: None,
            env: HashMap::new(),
        }
    }

    pub fn update(&self, r: ToolResult) {
        if let Some(f) = &self.on_update {
            f(r);
        }
    }

    pub fn check_cancelled(&self) -> anyhow::Result<()> {
        if self.cancel.is_cancelled() {
            anyhow::bail!("Operation aborted");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecutionMode {
    Parallel,
    Sequential,
}

/// A tool the agent can call. Implemented by built-ins, Rust plugins and (via an adapter) Lua.
pub trait AgentTool: Send + Sync {
    fn name(&self) -> &str;
    fn label(&self) -> &str {
        self.name()
    }
    fn description(&self) -> String;
    /// JSON schema for the arguments object.
    fn parameters(&self) -> Value;
    /// One-line summary for the system prompt tool list.
    fn prompt_snippet(&self) -> Option<String> {
        None
    }
    /// Rule bullets this tool contributes to the system prompt.
    fn prompt_guidelines(&self) -> Vec<String> {
        Vec::new()
    }
    fn execution_mode(&self) -> ExecutionMode {
        ExecutionMode::Parallel
    }
    /// Repair common model mistakes in arguments before validation.
    fn prepare_arguments(&self, args: Value) -> Value {
        args
    }
    fn execute(&self, call_id: String, args: Value, ctx: ToolContext) -> BoxFuture<'static, anyhow::Result<ToolResult>>;

    fn declaration(&self) -> Tool {
        Tool {
            name: self.name().to_string(),
            description: self.description(),
            parameters: self.parameters(),
        }
    }
}

pub type ToolRef = Arc<dyn AgentTool>;

/// Return type of [`AgentTool::execute`].
pub type BoxFutureResult = BoxFuture<'static, anyhow::Result<ToolResult>>;

/// The default coding tool set, in pi's order (`read, bash, edit, write`) plus the read-only
/// helpers and `web_search`.
pub fn builtin_tools() -> Vec<ToolRef> {
    vec![
        Arc::new(read::ReadTool::default()),
        Arc::new(bash::BashTool::default()),
        Arc::new(edit::EditTool),
        Arc::new(write::WriteTool),
        Arc::new(grep::GrepTool),
        Arc::new(find::FindTool),
        Arc::new(ls::LsTool),
        Arc::new(web_search::WebSearchTool::default()),
    ]
}

/// Validate arguments against the subset of JSON schema the built-in tools use:
/// object type, required properties, and primitive property types (with light coercion).
pub fn validate_arguments(tool_name: &str, schema: &Value, args: &Value) -> Result<Value, String> {
    let mut args = args.clone();
    if !args.is_object() {
        args = json!({});
    }
    let mut errors = Vec::new();
    let props = schema.get("properties").and_then(Value::as_object);
    if let Some(required) = schema.get("required").and_then(Value::as_array) {
        for r in required.iter().filter_map(Value::as_str) {
            if args.get(r).is_none_or(Value::is_null) {
                errors.push(format!("  - /{r}: must have required property '{r}'"));
            }
        }
    }
    if let (Some(props), Some(obj)) = (props, args.as_object_mut()) {
        // Optional nulls are treated as absent.
        obj.retain(|_, v| !v.is_null());
        for (k, v) in obj.iter_mut() {
            let Some(ty) = props.get(k).and_then(|p| p.get("type")).and_then(Value::as_str) else { continue };
            let ok = match ty {
                "string" => v.is_string(),
                "number" | "integer" => {
                    if let Some(n) = v.as_str().and_then(|s| s.trim().parse::<f64>().ok()) {
                        *v = json!(n);
                    }
                    v.is_number()
                }
                "boolean" => {
                    if let Some(b) = v.as_str().and_then(|s| s.parse::<bool>().ok()) {
                        *v = json!(b);
                    }
                    v.is_boolean()
                }
                "array" => v.is_array(),
                "object" => v.is_object(),
                _ => true,
            };
            if !ok {
                errors.push(format!("  - /{k}: must be {ty}"));
            }
        }
    }
    if errors.is_empty() {
        Ok(args)
    } else {
        Err(format!(
            "Validation failed for tool \"{tool_name}\":\n{}\n\nReceived arguments:\n{}",
            errors.join("\n"),
            serde_json::to_string_pretty(&args).unwrap_or_default()
        ))
    }
}

/// Serialize mutations of the same file (pi `withFileMutationQueue`). Different files run in parallel.
pub async fn with_file_mutation_queue<T>(path: &Path, f: impl std::future::Future<Output = T>) -> T {
    static QUEUES: OnceLock<Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>> = OnceLock::new();
    let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let lock = QUEUES
        .get_or_init(Default::default)
        .lock()
        .unwrap()
        .entry(key.clone())
        .or_default()
        .clone();
    let guard = lock.lock().await;
    let out = f.await;
    drop(guard);
    let mut map = QUEUES.get().unwrap().lock().unwrap();
    if map.get(&key).is_some_and(|l| Arc::strong_count(l) == 1) {
        map.remove(&key);
    }
    out
}

pub(crate) fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str)
}

pub(crate) fn arg_u64(args: &Value, key: &str) -> Option<u64> {
    args.get(key).and_then(|v| v.as_u64().or_else(|| v.as_f64().map(|f| f.max(0.0) as u64)))
}

/// Locate a helper binary (`rg`, `fd`) on PATH or in serpis/pi tool dirs.
pub fn find_binary(names: &[&str]) -> Option<PathBuf> {
    let home = dirs::home_dir().unwrap_or_default();
    let extra = [home.join(".serpis/agent/bin"), home.join(".pi/agent/bin")];
    let path = std::env::var_os("PATH").unwrap_or_default();
    for dir in std::env::split_paths(&path).chain(extra) {
        for n in names {
            let p = dir.join(n);
            if p.is_file() {
                return Some(p);
            }
        }
    }
    None
}
