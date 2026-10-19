//! `find` tool (pi `find.ts`): fd glob search honoring .gitignore.

use std::process::Stdio;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::path_utils::resolve_to_cwd;
use super::truncate::{DEFAULT_MAX_BYTES, Limits, format_size, truncate_head};
use super::*;

const DEFAULT_LIMIT: u64 = 1000;

pub struct FindTool;

impl AgentTool for FindTool {
    fn name(&self) -> &str {
        "find"
    }
    fn description(&self) -> String {
        format!(
            "Search for files by glob pattern. Returns matching file paths relative to the search directory. Respects .gitignore. Output is truncated to {DEFAULT_LIMIT} results or {}KB (whichever is hit first).",
            DEFAULT_MAX_BYTES / 1024
        )
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Glob pattern to match files, e.g. '*.ts', '**/*.json', or 'src/**/*.spec.ts'"},
                "path": {"type": "string", "description": "Directory to search in (default: current directory)"},
                "limit": {"type": "number", "description": "Maximum number of results (default: 1000)"}
            },
            "required": ["pattern"]
        })
    }
    fn prompt_snippet(&self) -> Option<String> {
        Some("Find files by glob pattern (respects .gitignore)".into())
    }

    fn execute(&self, _id: String, args: Value, ctx: ToolContext) -> BoxFuture<'static, anyhow::Result<ToolResult>> {
        async move {
            ctx.check_cancelled()?;
            let pattern = arg_str(&args, "pattern").unwrap_or("").to_string();
            let search_path = resolve_to_cwd(arg_str(&args, "path").unwrap_or("."), &ctx.cwd);
            let limit = arg_u64(&args, "limit").unwrap_or(DEFAULT_LIMIT) as usize;
            let fd = find_binary(&["fd", "fdfind"]).ok_or_else(|| anyhow::anyhow!("fd is not available and could not be downloaded"))?;
            let mut cmd = Command::new(fd);
            cmd.args(["--glob", "--color=never", "--hidden"]);
            // fd ignores .gitignore outside git repos unless told otherwise.
            let inside_git = search_path.ancestors().any(|p| p.join(".git").exists());
            if !inside_git {
                cmd.arg("--no-require-git");
            }
            cmd.args(["--max-results", &limit.to_string()]);
            let mut effective = pattern.clone();
            if pattern.contains('/') {
                cmd.arg("--full-path");
                if !pattern.starts_with('/') && !pattern.starts_with("**/") && pattern != "**" {
                    effective = format!("**/{pattern}");
                }
            }
            cmd.arg("--").arg(&effective).arg(&search_path);
            cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
            let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("Failed to run fd: {e}"))?;
            let (mut so, mut se) = (child.stdout.take().unwrap(), child.stderr.take().unwrap());
            let (mut out, mut err) = (String::new(), String::new());
            let status = tokio::select! {
                _ = ctx.cancel.cancelled() => { let _ = child.kill().await; anyhow::bail!("Operation aborted") }
                r = async { let _ = so.read_to_string(&mut out).await; let _ = se.read_to_string(&mut err).await; child.wait().await } => r?,
            };
            if !status.success() && out.trim().is_empty() {
                let msg = err.trim();
                anyhow::bail!("{}", if msg.is_empty() { format!("fd exited with code {:?}", status.code()) } else { msg.to_string() });
            }
            let results: Vec<String> = out
                .lines()
                .map(|l| l.trim_end_matches('\r').trim())
                .filter(|l| !l.is_empty())
                .map(|l| {
                    let trailing = l.ends_with('/');
                    let p = std::path::Path::new(l);
                    let rel = p.strip_prefix(&search_path).unwrap_or(p).to_string_lossy().replace('\\', "/");
                    if trailing && !rel.ends_with('/') { format!("{rel}/") } else { rel }
                })
                .collect();
            if results.is_empty() {
                return Ok(ToolResult::text("No files found matching pattern"));
            }
            let limit_reached = results.len() >= limit;
            let t = truncate_head(&results.join("\n"), Limits::bytes_only());
            let mut output = t.content.clone();
            let mut notices = Vec::new();
            let mut details = serde_json::Map::new();
            if limit_reached {
                notices.push(format!("{limit} results limit reached. Use limit={} for more, or refine pattern", limit * 2));
                details.insert("resultLimitReached".into(), json!(limit));
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
