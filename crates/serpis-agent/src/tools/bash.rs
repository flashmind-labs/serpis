//! `bash` tool (pi `bash.ts`): streaming output with tail truncation, temp-file spooling,
//! timeout and abort that kill the whole process group.

use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use tokio::io::AsyncReadExt;
use tokio::process::Command;

use super::output::OutputAccumulator;
use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, TruncatedBy, format_size};
use super::*;

pub const BASH_UPDATE_THROTTLE: Duration = Duration::from_millis(100);
const MAX_TIMEOUT_SECONDS: f64 = 2_147_483_647.0 / 1000.0;
const STRUCTURED_OUTPUT_MAX_BYTES: usize = 1024 * 1024;

#[derive(Default)]
pub struct BashTool {
    /// Prepended to every command (e.g. shell setup).
    pub command_prefix: Option<String>,
    /// Explicit shell; default `/bin/bash`, then `bash` on PATH, then `sh`.
    pub shell_path: Option<String>,
}

fn shell(custom: Option<&str>) -> anyhow::Result<String> {
    if let Some(p) = custom {
        if std::path::Path::new(p).exists() {
            return Ok(p.to_string());
        }
        anyhow::bail!("Custom shell path not found: {p}");
    }
    if std::path::Path::new("/bin/bash").exists() {
        return Ok("/bin/bash".into());
    }
    Ok(find_binary(&["bash"]).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|| "sh".into()))
}

#[cfg(unix)]
fn kill_process_group(pid: u32) {
    use nix::sys::signal::{Signal, killpg};
    use nix::unistd::Pid;
    let _ = killpg(Pid::from_raw(pid as i32), Signal::SIGKILL);
}

impl AgentTool for BashTool {
    fn name(&self) -> &str {
        "bash"
    }
    fn description(&self) -> String {
        format!(
            "Execute a bash command in the current working directory. Returns stdout and stderr. Output is truncated to last {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). If truncated, full output is saved to a temp file. Optionally provide a timeout in seconds.",
            DEFAULT_MAX_BYTES / 1024
        )
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "command": {"type": "string", "description": "Shell command to execute"},
                "description": {"type": "string", "description": "What the command is for, in a few words, for the person following along: \"Run the test suite\", \"Find where cards are drawn\". Give one with every command."},
                "timeout": {"type": "number", "description": "Timeout in seconds (optional, no default timeout)"}
            },
            "required": ["command"]
        })
    }
    fn prompt_snippet(&self) -> Option<String> {
        Some("Execute bash commands (ls, grep, find, etc.)".into())
    }
    fn prompt_guidelines(&self) -> Vec<String> {
        vec!["You can inspect SERPIS_* environment variables for current model and session details.".into()]
    }

    fn execute(&self, _id: String, args: Value, ctx: ToolContext) -> BoxFuture<'static, anyhow::Result<ToolResult>> {
        let prefix = self.command_prefix.clone();
        let shell_path = self.shell_path.clone();
        async move {
            let command = arg_str(&args, "command").unwrap_or("").to_string();
            let timeout = args.get("timeout").and_then(Value::as_f64);
            if let Some(t) = timeout {
                if !t.is_finite() || t <= 0.0 {
                    anyhow::bail!("Invalid timeout: must be a finite number of seconds");
                }
                if t > MAX_TIMEOUT_SECONDS {
                    anyhow::bail!("Invalid timeout: maximum is {MAX_TIMEOUT_SECONDS} seconds");
                }
            }
            let command = match prefix {
                Some(p) => format!("{p}\n{command}"),
                None => command,
            };
            if !ctx.cwd.exists() {
                anyhow::bail!("Working directory does not exist: {}\nCannot execute bash commands.", ctx.cwd.display());
            }
            ctx.check_cancelled()?;
            let shell = shell(shell_path.as_deref())?;
            let mut cmd = Command::new(&shell);
            cmd.arg("-c")
                .arg(&command)
                .current_dir(&ctx.cwd)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .envs(&ctx.env)
                .kill_on_drop(true);
            #[cfg(unix)]
            cmd.process_group(0);
            let mut child = cmd.spawn()?;
            let pid = child.id().unwrap_or(0);

            let acc = Arc::new(Mutex::new(OutputAccumulator::new("serpis-bash")));
            let last_update = Arc::new(Mutex::new(Instant::now() - BASH_UPDATE_THROTTLE));
            ctx.update(ToolResult::default());
            let emit = {
                let acc = acc.clone();
                let ctx = ctx.clone();
                let last_update = last_update.clone();
                move |force: bool| {
                    let mut last = last_update.lock().unwrap();
                    if !force && last.elapsed() < BASH_UPDATE_THROTTLE {
                        return;
                    }
                    *last = Instant::now();
                    let snap = acc.lock().unwrap().snapshot(true);
                    ctx.update(ToolResult::text(snap.content).with_details(json!({
                        "truncation": snap.truncation.truncated.then_some(&snap.truncation),
                        "fullOutputPath": snap.full_output_path,
                    })));
                }
            };
            let pump = |mut reader: Box<dyn tokio::io::AsyncRead + Unpin + Send>| {
                let acc = acc.clone();
                let emit = emit.clone();
                async move {
                    let mut buf = vec![0u8; 8192];
                    loop {
                        match reader.read(&mut buf).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                acc.lock().unwrap().append(&buf[..n]);
                                emit(false);
                            }
                        }
                    }
                }
            };
            let out_task = tokio::spawn(pump(Box::new(child.stdout.take().unwrap())));
            let err_task = tokio::spawn(pump(Box::new(child.stderr.take().unwrap())));
            let started = Instant::now();

            enum End {
                Exit(Option<i32>),
                Aborted,
                Timeout,
            }
            let sleep = async {
                match timeout {
                    Some(t) => tokio::time::sleep(Duration::from_secs_f64(t)).await,
                    None => futures::future::pending().await,
                }
            };
            let end = tokio::select! {
                status = child.wait() => End::Exit(status.ok().and_then(|s| {
                    #[cfg(unix)]
                    { use std::os::unix::process::ExitStatusExt; s.code().or_else(|| s.signal().map(|sig| 128 + sig)) }
                    #[cfg(not(unix))]
                    { s.code() }
                })),
                _ = ctx.cancel.cancelled() => End::Aborted,
                _ = sleep => End::Timeout,
            };
            if !matches!(end, End::Exit(_)) {
                #[cfg(unix)]
                kill_process_group(pid);
                let _ = child.kill().await;
            }
            // Detached descendants may keep the pipes open; do not hang on them.
            let _ = tokio::time::timeout(Duration::from_millis(500), async {
                let _ = out_task.await;
                let _ = err_task.await;
            })
            .await;

            let (snapshot, last_line_bytes, full) = {
                let mut a = acc.lock().unwrap();
                a.finish();
                let snap = a.snapshot(true);
                let full = a.read_full_output(STRUCTURED_OUTPUT_MAX_BYTES);
                (snap, a.last_line_bytes(), full)
            };
            emit(true);
            let format_output = |empty: &str| -> (String, Value) {
                let t = &snapshot.truncation;
                let mut text = if snapshot.content.is_empty() { empty.to_string() } else { snapshot.content.clone() };
                let mut details = Value::Null;
                if t.truncated {
                    let path = snapshot.full_output_path.as_ref().map(|p| p.display().to_string()).unwrap_or_default();
                    details = json!({"truncation": t, "fullOutputPath": path});
                    let start = t.total_lines - t.output_lines + 1;
                    let end = t.total_lines;
                    if t.last_line_partial {
                        text.push_str(&format!(
                            "\n\n[Showing last {} of line {end} (line is {}). Full output: {path}]",
                            format_size(t.output_bytes),
                            format_size(last_line_bytes)
                        ));
                    } else if t.truncated_by == Some(TruncatedBy::Lines) {
                        text.push_str(&format!("\n\n[Showing lines {start}-{end} of {}. Full output: {path}]", t.total_lines));
                    } else {
                        text.push_str(&format!(
                            "\n\n[Showing lines {start}-{end} of {} ({} limit). Full output: {path}]",
                            t.total_lines,
                            format_size(DEFAULT_MAX_BYTES)
                        ));
                    }
                }
                (text, details)
            };
            let append_status = |text: String, status: &str| if text.is_empty() { status.to_string() } else { format!("{text}\n\n{status}") };
            match end {
                End::Aborted => anyhow::bail!("{}", append_status(format_output("").0, "Command aborted")),
                End::Timeout => anyhow::bail!(
                    "{}",
                    append_status(format_output("").0, &format!("Command timed out after {} seconds", timeout.unwrap_or(0.0)))
                ),
                End::Exit(None) => anyhow::bail!("{}", append_status(format_output("(no output)").0, "Command terminated without an exit code")),
                End::Exit(Some(code)) => {
                    let (text, mut details) = format_output("(no output)");
                    let structured = json!({
                        "output": full.0,
                        "truncated": full.1,
                        "exit_code": code,
                        "wall_time_seconds": (started.elapsed().as_secs_f64() * 10.0).round() / 10.0,
                    });
                    if details.is_null() {
                        details = json!({});
                    }
                    details["structured"] = structured;
                    if code != 0 {
                        return Ok(ToolResult {
                            is_error: true,
                            ..ToolResult::text(append_status(text, &format!("Command exited with code {code}"))).with_details(details)
                        });
                    }
                    Ok(ToolResult::text(text).with_details(details))
                }
            }
        }
        .boxed()
    }
}
