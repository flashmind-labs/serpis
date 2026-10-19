//! `grep` tool (pi `grep.ts`): ripgrep JSON output, match limit, context lines, long-line truncation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
use tokio::process::Command;

use super::path_utils::resolve_to_cwd;
use super::truncate::{DEFAULT_MAX_BYTES, GREP_MAX_LINE_LENGTH, Limits, format_size, truncate_head, truncate_line};
use super::*;

const DEFAULT_LIMIT: u64 = 100;

pub struct GrepTool;

impl AgentTool for GrepTool {
    fn name(&self) -> &str {
        "grep"
    }
    fn description(&self) -> String {
        format!(
            "Search file contents for a pattern. Returns matching lines with file paths and line numbers. Respects .gitignore. Output is truncated to {DEFAULT_LIMIT} matches or {}KB (whichever is hit first). Long lines are truncated to {GREP_MAX_LINE_LENGTH} chars.",
            DEFAULT_MAX_BYTES / 1024
        )
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "pattern": {"type": "string", "description": "Search pattern (regex or literal string)"},
                "path": {"type": "string", "description": "Directory or file to search (default: current directory)"},
                "glob": {"type": "string", "description": "Filter files by glob pattern, e.g. '*.ts' or '**/*.spec.ts'"},
                "ignoreCase": {"type": "boolean", "description": "Case-insensitive search (default: false)"},
                "literal": {"type": "boolean", "description": "Treat pattern as literal string instead of regex (default: false)"},
                "context": {"type": "number", "description": "Number of lines to show before and after each match (default: 0)"},
                "limit": {"type": "number", "description": "Maximum number of matches to return (default: 100)"}
            },
            "required": ["pattern"]
        })
    }
    fn prompt_snippet(&self) -> Option<String> {
        Some("Search file contents for patterns (respects .gitignore)".into())
    }

    fn execute(&self, _id: String, args: Value, ctx: ToolContext) -> BoxFuture<'static, anyhow::Result<ToolResult>> {
        async move {
            ctx.check_cancelled()?;
            let rg = find_binary(&["rg"]).ok_or_else(|| anyhow::anyhow!("ripgrep (rg) is not available and could not be downloaded"))?;
            let pattern = arg_str(&args, "pattern").unwrap_or("").to_string();
            let search_path = resolve_to_cwd(arg_str(&args, "path").unwrap_or("."), &ctx.cwd);
            let is_dir = match tokio::fs::metadata(&search_path).await {
                Ok(m) => m.is_dir(),
                Err(_) => anyhow::bail!("Path not found: {}", search_path.display()),
            };
            let context = arg_u64(&args, "context").unwrap_or(0) as usize;
            let limit = arg_u64(&args, "limit").unwrap_or(DEFAULT_LIMIT).max(1) as usize;

            let mut cmd = Command::new(rg);
            cmd.args(["--json", "--line-number", "--color=never", "--hidden"]);
            if args.get("ignoreCase").and_then(Value::as_bool) == Some(true) {
                cmd.arg("--ignore-case");
            }
            if args.get("literal").and_then(Value::as_bool) == Some(true) {
                cmd.arg("--fixed-strings");
            }
            if let Some(g) = arg_str(&args, "glob") {
                cmd.args(["--glob", g]);
            }
            cmd.arg("--").arg(&pattern).arg(&search_path);
            cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
            let mut child = cmd.spawn().map_err(|e| anyhow::anyhow!("Failed to run ripgrep: {e}"))?;
            let mut stderr = child.stderr.take().unwrap();
            let stderr_task = tokio::spawn(async move {
                let mut s = String::new();
                let _ = stderr.read_to_string(&mut s).await;
                s
            });
            let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
            let mut matches: Vec<(PathBuf, usize, Option<String>)> = Vec::new();
            let mut limit_reached = false;
            loop {
                let line = tokio::select! {
                    _ = ctx.cancel.cancelled() => { let _ = child.kill().await; anyhow::bail!("Operation aborted") }
                    l = lines.next_line() => l?,
                };
                let Some(line) = line else { break };
                let Ok(ev) = serde_json::from_str::<Value>(&line) else { continue };
                if ev["type"] == "match" {
                    if let (Some(p), Some(n)) = (ev.pointer("/data/path/text").and_then(Value::as_str), ev.pointer("/data/line_number").and_then(Value::as_u64)) {
                        matches.push((PathBuf::from(p), n as usize, ev.pointer("/data/lines/text").and_then(Value::as_str).map(str::to_string)));
                    }
                    if matches.len() >= limit {
                        limit_reached = true;
                        let _ = child.kill().await;
                        break;
                    }
                }
            }
            let status = child.wait().await?;
            let stderr = stderr_task.await.unwrap_or_default();
            if !limit_reached && !matches!(status.code(), Some(0) | Some(1)) {
                let msg = stderr.trim();
                anyhow::bail!("{}", if msg.is_empty() { format!("ripgrep exited with code {:?}", status.code()) } else { msg.to_string() });
            }
            if matches.is_empty() {
                return Ok(ToolResult::text("No matches found"));
            }

            let format_path = |p: &Path| -> String {
                if is_dir {
                    if let Ok(rel) = p.strip_prefix(&search_path) {
                        let s = rel.to_string_lossy().replace('\\', "/");
                        if !s.is_empty() && !s.starts_with("..") {
                            return s;
                        }
                    }
                }
                p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default()
            };
            let mut cache: HashMap<PathBuf, Vec<String>> = HashMap::new();
            let mut out: Vec<String> = Vec::new();
            let mut lines_truncated = false;
            for (path, n, text) in &matches {
                let rel = format_path(path);
                if context == 0 {
                    if let Some(t) = text {
                        let clean = t.replace("\r\n", "\n").replace('\r', "");
                        let clean = clean.strip_suffix('\n').unwrap_or(&clean);
                        let (t, tr) = truncate_line(clean, GREP_MAX_LINE_LENGTH);
                        lines_truncated |= tr;
                        out.push(format!("{rel}:{n}: {t}"));
                        continue;
                    }
                }
                let file_lines = cache.entry(path.clone()).or_insert_with(|| {
                    std::fs::read_to_string(path)
                        .map(|c| c.replace("\r\n", "\n").replace('\r', "\n").split('\n').map(str::to_string).collect())
                        .unwrap_or_default()
                });
                if file_lines.is_empty() {
                    out.push(format!("{rel}:{n}: (unable to read file)"));
                    continue;
                }
                let start = if context > 0 { n.saturating_sub(context).max(1) } else { *n };
                let end = if context > 0 { (n + context).min(file_lines.len()) } else { *n };
                for cur in start..=end {
                    let lt = file_lines.get(cur - 1).map(String::as_str).unwrap_or("").replace('\r', "");
                    let (t, tr) = truncate_line(&lt, GREP_MAX_LINE_LENGTH);
                    lines_truncated |= tr;
                    if cur == *n {
                        out.push(format!("{rel}:{cur}: {t}"));
                    } else {
                        out.push(format!("{rel}-{cur}- {t}"));
                    }
                }
            }
            let t = truncate_head(&out.join("\n"), Limits::bytes_only());
            let mut output = t.content.clone();
            let mut notices = Vec::new();
            let mut details = serde_json::Map::new();
            if limit_reached {
                notices.push(format!("{limit} matches limit reached. Use limit={} for more, or refine pattern", limit * 2));
                details.insert("matchLimitReached".into(), json!(limit));
            }
            if t.truncated {
                notices.push(format!("{} limit reached", format_size(DEFAULT_MAX_BYTES)));
                details.insert("truncation".into(), json!(t));
            }
            if lines_truncated {
                notices.push(format!("Some lines truncated to {GREP_MAX_LINE_LENGTH} chars. Use read tool to see full lines"));
                details.insert("linesTruncated".into(), json!(true));
            }
            if !notices.is_empty() {
                output.push_str(&format!("\n\n[{}]", notices.join(". ")));
            }
            Ok(ToolResult::text(output).with_details(if details.is_empty() { Value::Null } else { Value::Object(details) }))
        }
        .boxed()
    }
}
