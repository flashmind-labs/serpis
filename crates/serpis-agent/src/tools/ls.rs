//! `ls` tool (pi `ls.ts`).

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};

use super::path_utils::resolve_to_cwd;
use super::truncate::{DEFAULT_MAX_BYTES, Limits, format_size, truncate_head};
use super::*;

const DEFAULT_LIMIT: u64 = 500;

pub struct LsTool;

impl AgentTool for LsTool {
    fn name(&self) -> &str {
        "ls"
    }
    fn description(&self) -> String {
        format!(
            "List directory contents. Returns entries sorted alphabetically, with '/' suffix for directories. Includes dotfiles. Output is truncated to {DEFAULT_LIMIT} entries or {}KB (whichever is hit first).",
            DEFAULT_MAX_BYTES / 1024
        )
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory to list (default: current directory)"},
                "limit": {"type": "number", "description": "Maximum number of entries to return (default: 500)"}
            }
        })
    }
    fn prompt_snippet(&self) -> Option<String> {
        Some("List directory contents".into())
    }

    fn execute(&self, _id: String, args: Value, ctx: ToolContext) -> BoxFuture<'static, anyhow::Result<ToolResult>> {
        async move {
            ctx.check_cancelled()?;
            let dir = resolve_to_cwd(arg_str(&args, "path").unwrap_or("."), &ctx.cwd);
            let limit = arg_u64(&args, "limit").unwrap_or(DEFAULT_LIMIT) as usize;
            let meta = tokio::fs::metadata(&dir).await.map_err(|_| anyhow::anyhow!("Path not found: {}", dir.display()))?;
            if !meta.is_dir() {
                anyhow::bail!("Not a directory: {}", dir.display());
            }
            let mut rd = tokio::fs::read_dir(&dir).await.map_err(|e| anyhow::anyhow!("Cannot read directory: {e}"))?;
            let mut entries = Vec::new();
            while let Some(e) = rd.next_entry().await? {
                entries.push(e.file_name().to_string_lossy().into_owned());
            }
            entries.sort_by_key(|a| a.to_lowercase());
            let mut results = Vec::new();
            let mut limit_reached = false;
            for e in entries {
                if results.len() >= limit {
                    limit_reached = true;
                    break;
                }
                // Follow symlinks like Node's stat; skip entries that cannot be stat'ed.
                let Ok(m) = tokio::fs::metadata(dir.join(&e)).await else { continue };
                results.push(if m.is_dir() { format!("{e}/") } else { e });
            }
            if results.is_empty() {
                return Ok(ToolResult::text("(empty directory)"));
            }
            let t = truncate_head(&results.join("\n"), Limits::bytes_only());
            let mut output = t.content.clone();
            let mut notices = Vec::new();
            let mut details = serde_json::Map::new();
            if limit_reached {
                notices.push(format!("{limit} entries limit reached. Use limit={} for more", limit * 2));
                details.insert("entryLimitReached".into(), json!(limit));
            }
            if t.truncated {
                notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
                details.insert("truncation".into(), json!(t));
            }
            if !notices.is_empty() {
                output.push_str(&format!("\n\n[{}]", notices.join(". ")));
            }
            Ok(ToolResult::text(output).with_details(if details.is_empty() { Value::Null } else { Value::Object(details) }))
        }
        .boxed()
    }
}
