//! A stand-in for the `claude` CLI in stream-json mode, for tests: the handshake, permission
//! prompts, interrupts and sessions as Claude Code does them, with scripted replies and no
//! model. Every start and every message it's sent are logged to `$FAKE_CLAUDE_LOG` (JSON
//! lines); sessions are files in `$FAKE_CLAUDE_DIR`.
//!
//! Replies by what the prompt says: "edit" asks to edit `notes.txt` (a permission prompt, then a
//! diff), "question" asks two questions, "slow" streams until interrupted, "fail" ends in an
//! API error, "subagent" runs an agent whose steps come during the turn, "agent in the
//! background" one whose steps come after it, "background" a command in the background (a
//! real `sleep`, in a process group of its own, logged as `{"spawned": pid}`) that finishes
//! after the turn ("forever": never), "monitor" a Monitor that runs until stopped; work
//! finished after a turn gets a turn of its own, as
//! Claude Code does. Anything else is echoed after a thought.

use std::io::{BufRead, Write};
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

fn out(v: Value) {
    let mut o = std::io::stdout().lock();
    let _ = writeln!(o, "{v}");
    let _ = o.flush();
}

fn log(v: Value) {
    if let Some(p) = std::env::var_os("FAKE_CLAUDE_LOG")
        && let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(p) {
            let _ = writeln!(f, "{v}");
        }
}

fn ev(e: Value) {
    out(json!({"type": "stream_event", "event": e, "parent_tool_use_id": null}));
}

fn start(id: &str) {
    ev(json!({"type": "message_start", "message": {"id": id, "model": "claude-fake-5", "usage": {"input_tokens": 10, "output_tokens": 1, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 20}}}));
}

fn stop(reason: &str) {
    ev(json!({"type": "message_delta", "delta": {"stop_reason": reason}, "usage": {"output_tokens": 7}}));
    ev(json!({"type": "message_stop"}));
}

fn text_block(index: u64, text: &str) {
    ev(json!({"type": "content_block_start", "index": index, "content_block": {"type": "text", "text": ""}}));
    ev(json!({"type": "content_block_delta", "index": index, "delta": {"type": "text_delta", "text": text}}));
    ev(json!({"type": "content_block_stop", "index": index}));
}

fn result(session: &str, error: Option<&str>) {
    out(json!({"type": "result", "subtype": if error.is_some() { "error_during_execution" } else { "success" }, "is_error": error.is_some(), "result": error.unwrap_or("ok"), "session_id": session, "total_cost_usd": 0.01}));
}

fn system(sub: &str, v: Value) {
    let mut line = json!({"type": "system", "subtype": sub, "session_id": "s"});
    line.as_object_mut().unwrap().extend(v.as_object().cloned().unwrap_or_default());
    out(line);
}

/// A tool call, streamed.
fn call(index: u64, id: &str, name: &str, input: &Value) {
    ev(json!({"type": "content_block_start", "index": index, "content_block": {"type": "tool_use", "id": id, "name": name, "input": {}}}));
    ev(json!({"type": "content_block_delta", "index": index, "delta": {"type": "input_json_delta", "partial_json": input.to_string()}}));
    ev(json!({"type": "content_block_stop", "index": index}));
}

fn tool_result(id: &str, text: &str, parent: Option<&str>) {
    out(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": text}]}, "parent_tool_use_id": parent}));
}

/// A subagent's steps (under tool call `parent`): a command, its output, its answer.
fn subagent_steps(parent: &str, n: u64) {
    let bash = format!("sub{n}");
    out(json!({"type": "assistant", "message": {"id": format!("s{n}a"), "role": "assistant", "model": "claude-fake-5", "content": [{"type": "tool_use", "id": bash, "name": "Bash", "input": {"command": "echo from-sub"}}]}, "parent_tool_use_id": parent}));
    system("task_progress", json!({"task_id": format!("a{n}"), "tool_use_id": parent, "description": "Running echo from-sub", "last_tool_name": "Bash", "usage": {"total_tokens": 900, "tool_uses": 1, "duration_ms": 400}}));
    tool_result(&bash, "from-sub", Some(parent));
    out(json!({"type": "assistant", "message": {"id": format!("s{n}b"), "role": "assistant", "model": "claude-fake-5", "content": [{"type": "text", "text": "It printed from-sub."}]}, "parent_tool_use_id": parent}));
}

/// Its own turn, after work it left running finished.
fn own_turn(session: &str, n: u64, text: &str) {
    system("init", json!({}));
    start(&format!("own{n}"));
    text_block(0, text);
    stop("end_turn");
    result(session, None);
}

/// What finishes after a turn.
enum Later {
    Command { n: u64 },
    Agent { n: u64 },
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    log(json!({"start": args, "cwd": cwd, "agent_env": std::env::var("SERPIS_AGENT").ok()}));
    let after = |flag: &str| args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).cloned();
    let dir = PathBuf::from(std::env::var_os("FAKE_CLAUDE_DIR").unwrap_or_else(|| std::env::temp_dir().into_os_string()));
    let session = match (after("--resume"), after("--session-id")) {
        (Some(id), _) => {
            if !dir.join(&id).exists() {
                eprintln!("No conversation found with session ID: {id}");
                result(&id, Some(&format!("No conversation found with session ID: {id}")));
                return;
            }
            id
        }
        (None, Some(id)) => {
            let _ = std::fs::write(dir.join(&id), "");
            id
        }
        _ => "no-session".into(),
    };
    // Stdin is read on one thread, so a reply can watch for an interrupt while it streams.
    let (tx, rx) = std::sync::mpsc::channel::<String>();
    std::thread::spawn(move || {
        for l in std::io::stdin().lock().lines().map_while(Result::ok) {
            if tx.send(l).is_err() {
                break;
            }
        }
    });
    let mut lines = rx.iter();
    let mut n = 0;
    let mut later: Vec<(Instant, Later)> = Vec::new();
    // Running in the background: task id to its command (stop_task ends it).
    let mut running: Vec<(String, String, std::process::Child)> = Vec::new();
    loop {
        if let Some(i) = later.iter().position(|(at, _)| Instant::now() >= *at) {
            match later.remove(i).1 {
                Later::Command { n } => {
                    let task = format!("b{n}");
                    if let Some(i) = running.iter().position(|(t, ..)| *t == task) {
                        let (.., mut child) = running.remove(i);
                        let _ = child.kill();
                        let _ = child.wait();
                    }
                    system("task_updated", json!({"task_id": task, "patch": {"status": "completed"}}));
                    system("task_notification", json!({"task_id": task, "tool_use_id": format!("t{n}"), "status": "completed", "summary": "Background command \"sleep then echo bg-done\" completed (exit code 0)", "output_file": "/tmp/out"}));
                    own_turn(&session, n, "The background command finished: bg-done.");
                }
                Later::Agent { n } => {
                    let parent = format!("t{n}");
                    subagent_steps(&parent, n);
                    system("task_updated", json!({"task_id": format!("a{n}"), "patch": {"status": "completed"}}));
                    system("task_notification", json!({"task_id": format!("a{n}"), "tool_use_id": parent, "status": "completed", "summary": "It printed from-sub.", "output_file": "/tmp/out", "usage": {"total_tokens": 1200, "tool_uses": 1, "duration_ms": 900}}));
                    own_turn(&session, n, "The agent reported: from-sub.");
                }
            }
            continue;
        }
        let line = match rx.recv_timeout(Duration::from_millis(30)) {
            Ok(l) => l,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(_) => break,
        };
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        log(json!({"got": msg}));
        if msg["type"] == "control_request" && msg["request"]["subtype"] == "initialize" {
            let models = [json!({"value": "default", "displayName": "Default"}), json!({"value": "fake", "displayName": "Fake 5", "supportedEffortLevels": ["low", "high"], "supportsThinkingOff": true})];
            out(json!({"type": "control_response", "response": {"subtype": "success", "request_id": msg["request_id"], "response": {"models": models, "account": {"email": "test@example.com", "subscriptionType": "Claude Test"}}}}));
            out(json!({"type": "system", "subtype": "init", "session_id": session, "permissionMode": after("--permission-mode").unwrap_or_else(|| "default".into())}));
            continue;
        }
        if msg["type"] == "control_request" && msg["request"]["subtype"] == "set_permission_mode" {
            out(json!({"type": "control_response", "response": {"subtype": "success", "request_id": msg["request_id"], "response": {"mode": msg["request"]["mode"]}}}));
            system("status", json!({"status": null, "permissionMode": msg["request"]["mode"]}));
            continue;
        }
        if msg["type"] == "control_request" && msg["request"]["subtype"] == "get_usage" {
            let window = |pct: u32, at: &str| json!({"utilization": pct, "resets_at": at});
            let behaviors = (msg["request"]["skip_behaviors"] != true).then(|| {
                let tip = json!({"key": "long_context", "pct": 90, "title": "Start fresh when you switch tasks", "detail": "run /clear"});
                json!({"day": {"request_count": 12, "session_count": 2, "recommendations": [tip]}, "week": {"request_count": 80, "session_count": 5, "recommendations": [tip]}})
            });
            let usage = json!({
                "subscription_type": "max",
                "rate_limits_available": true,
                "rate_limits": {
                    "five_hour": window(42, "2030-01-01T05:00:00Z"),
                    "seven_day": window(7, "2030-01-04T00:00:00Z"),
                    "seven_day_opus": null,
                    "model_scoped": [{"display_name": "Fake", "utilization": 61, "resets_at": "2030-01-04T00:00:00Z"}],
                    "extra_usage": {"is_enabled": true, "monthly_limit": 5000, "used_credits": 1250, "utilization": 25, "currency": "USD"},
                    "seven_day_breakdown": {"rows": [{"display_name": "Claude Code", "percent": 80}, {"display_name": "Chats", "percent": 20}]},
                },
                "behaviors": behaviors,
            });
            out(json!({"type": "control_response", "response": {"subtype": "success", "request_id": msg["request_id"], "response": usage}}));
            continue;
        }
        if msg["type"] == "control_request" && msg["request"]["subtype"] == "stop_task" {
            let task = msg["request"]["task_id"].as_str().unwrap_or("").to_string();
            if let Some(i) = running.iter().position(|(t, ..)| *t == task) {
                let (task, tool, mut child) = running.remove(i);
                let _ = child.kill();
                let _ = child.wait();
                later.retain(|(_, l)| !matches!(l, Later::Command { n } if format!("b{n}") == task));
                system("task_updated", json!({"task_id": task, "patch": {"status": "killed"}}));
                system("task_notification", json!({"task_id": task, "tool_use_id": tool, "status": "stopped", "summary": "Stopped", "output_file": "/tmp/out"}));
            }
            out(json!({"type": "control_response", "response": {"subtype": "success", "request_id": msg["request_id"], "response": {}}}));
            continue;
        }
        if msg["type"] != "user" {
            continue;
        }
        // A message sent with an id is said to be taken in, as Claude Code does.
        if !msg["uuid"].is_null() {
            out(json!({"type": "command_lifecycle", "command_uuid": msg["uuid"], "state": "started"}));
        }
        let text = msg["message"]["content"].as_array().and_then(|b| b.iter().rev().find_map(|b| b["text"].as_str())).unwrap_or("").to_string();
        n += 1;
        if text.contains("make a plan") {
            // A plan to approve (ExitPlanMode): approved, it goes on in the mode it is given;
            // refused, it says what it heard.
            let id = format!("t{n}");
            let input = json!({"plan": "# Plan\n1. Create hello.txt\n2. Write hi in it"});
            start(&format!("m{n}a"));
            call(0, &id, "ExitPlanMode", &input);
            stop("tool_use");
            out(json!({"type": "control_request", "request_id": format!("perm{n}"), "request": {"subtype": "can_use_tool", "tool_name": "ExitPlanMode", "input": input, "tool_use_id": id, "permission_suggestions": null}}));
            let answer = loop {
                let Some(l) = lines.next() else { return };
                let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
                log(json!({"got": v}));
                if v["type"] == "control_response" {
                    break v;
                }
            };
            let r = &answer["response"]["response"];
            let reply = if r["behavior"] == "allow" {
                let mode = r["updatedPermissions"][0]["mode"].as_str().unwrap_or("default").to_string();
                system("status", json!({"status": null, "permissionMode": mode}));
                tool_result(&id, "User has approved your plan. You can now start coding.", None);
                format!("Plan approved; on in {mode}.")
            } else {
                let why = r["message"].as_str().unwrap_or("").to_string();
                out(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": id, "content": why, "is_error": true}]}, "parent_tool_use_id": null}));
                format!("Planning on. Heard: {why}")
            };
            start(&format!("m{n}b"));
            text_block(0, &reply);
            stop("end_turn");
            result(&session, None);
        } else if text.contains("long task") {
            // A command that takes a while; messages sent meanwhile (priority next) are taken
            // in at the next step, and its answer says what they said, as Claude Code does.
            let id = format!("t{n}");
            start(&format!("m{n}a"));
            call(0, &id, "Bash", &json!({"command": "sleep 1; echo long-done"}));
            stop("tool_use");
            let mut told: Vec<(Value, String)> = Vec::new();
            for _ in 0..15 {
                if let Ok(l) = rx.recv_timeout(Duration::from_millis(100)) {
                    let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
                    log(json!({"got": v}));
                    if v["type"] == "user" && v["priority"] == "next" {
                        out(json!({"type": "command_lifecycle", "command_uuid": v["uuid"], "state": "queued"}));
                        told.push((v["uuid"].clone(), v["message"]["content"][0]["text"].as_str().unwrap_or("").to_string()));
                    }
                }
            }
            tool_result(&id, "long-done", None);
            // Taken in at the next step.
            for (uuid, _) in &told {
                out(json!({"type": "command_lifecycle", "command_uuid": uuid, "state": "started"}));
            }
            let said: Vec<&str> = told.iter().map(|(_, t)| t.as_str()).collect();
            start(&format!("m{n}b"));
            text_block(0, &if said.is_empty() { "Done.".to_string() } else { format!("Done. You said: {}.", said.join("; ")) });
            stop("end_turn");
            for (uuid, _) in &told {
                out(json!({"type": "command_lifecycle", "command_uuid": uuid, "state": "completed"}));
            }
            result(&session, None);
        } else if text.contains("think") {
            // A while thinking: its estimate grows; the thinking itself is sent only when
            // asked for (--thinking-display summarized), as Claude Code does.
            let summarized = after("--thinking-display").as_deref() == Some("summarized");
            start(&format!("m{n}"));
            ev(json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}));
            let frames = if text.contains("a while") { 40 } else { 12 };
            for i in 1..=frames {
                system("thinking_tokens", json!({"estimated_tokens": i * 150, "estimated_tokens_delta": 150}));
                if summarized && i == 1 {
                    ev(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "Weighing it up."}}));
                }
                std::thread::sleep(Duration::from_millis(180));
            }
            ev(json!({"type": "content_block_stop", "index": 0}));
            text_block(1, "Thought it through.");
            stop("end_turn");
            result(&session, None);
        } else if text.contains("monitor") {
            // A monitor: a shell task (its tool, Monitor, says so) that runs until stopped.
            let id = format!("t{n}");
            start(&format!("m{n}a"));
            call(0, &id, "Monitor", &json!({"command": "tail -f app.log", "description": "watch the log"}));
            stop("tool_use");
            let child = std::process::Command::new("sleep").arg("300").process_group(0).spawn().expect("sleep");
            log(json!({"spawned": child.id()}));
            let task = format!("b{n}");
            system("task_started", json!({"task_id": task, "tool_use_id": id, "description": "watch the log", "task_type": "local_bash", "is_backgrounded": true}));
            tool_result(&id, &format!("Monitor started with ID: {task}"), None);
            start(&format!("m{n}b"));
            text_block(0, "Watching the log.");
            stop("end_turn");
            result(&session, None);
            running.push((task, id, child));
        } else if text.contains("background") && !text.contains("agent") {
            let id = format!("t{n}");
            start(&format!("m{n}a"));
            call(0, &id, "Bash", &json!({"command": "sleep 300; echo bg-done", "description": "sleep then echo bg-done", "run_in_background": true}));
            stop("tool_use");
            let child = std::process::Command::new("sleep").arg("300").process_group(0).spawn().expect("sleep");
            log(json!({"spawned": child.id()}));
            let task = format!("b{n}");
            system("task_started", json!({"task_id": task, "tool_use_id": id, "description": "sleep then echo bg-done", "is_backgrounded": true}));
            tool_result(&id, &format!("Command running in background with ID: {task}. Output is being written to: /tmp/out"), None);
            start(&format!("m{n}b"));
            text_block(0, "Started it in the background.");
            stop("end_turn");
            result(&session, None);
            running.push((task, id, child));
            if !text.contains("forever") {
                later.push((Instant::now() + Duration::from_millis(700), Later::Command { n }));
            }
        } else if text.contains("agent") {
            // An agent: its steps come under its tool call, during the turn or after it.
            let id = format!("t{n}");
            let behind = text.contains("background");
            start(&format!("m{n}a"));
            call(0, &id, "Agent", &json!({"description": "Echo from-sub", "prompt": "Run echo from-sub and say what it printed.", "subagent_type": "general-purpose", "run_in_background": behind}));
            stop("tool_use");
            system("task_started", json!({"task_id": format!("a{n}"), "tool_use_id": id, "description": "Echo from-sub", "subagent_type": "general-purpose", "is_backgrounded": behind}));
            if behind {
                tool_result(&id, "Async agent launched successfully.", None);
                start(&format!("m{n}b"));
                text_block(0, "Launched it.");
                stop("end_turn");
                result(&session, None);
                later.push((Instant::now() + Duration::from_millis(700), Later::Agent { n }));
            } else {
                subagent_steps(&id, n);
                system("task_notification", json!({"task_id": format!("a{n}"), "tool_use_id": id, "status": "completed", "summary": "It printed from-sub.", "output_file": "/tmp/out", "usage": {"total_tokens": 1200, "tool_uses": 1, "duration_ms": 900}}));
                tool_result(&id, "It printed from-sub.", None);
                start(&format!("m{n}b"));
                text_block(0, "The subagent said from-sub.");
                stop("end_turn");
                result(&session, None);
            }
        } else if text.contains("question") {
            // Two questions, the second multiple-choice; it says what it was told.
            let input = json!({"questions": [
                {"question": "Which color?", "header": "Color", "multiSelect": false, "options": [{"label": "Red", "description": "warm"}, {"label": "Blue", "description": "cool"}]},
                {"question": "Which sizes?", "header": "Sizes", "multiSelect": true, "options": [{"label": "S", "description": "small"}, {"label": "M", "description": "medium"}, {"label": "L", "description": "large"}]},
            ]});
            start(&format!("m{n}a"));
            ev(json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": format!("t{n}"), "name": "AskUserQuestion", "input": {}}}));
            ev(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": input.to_string()}}));
            ev(json!({"type": "content_block_stop", "index": 0}));
            stop("tool_use");
            out(json!({"type": "control_request", "request_id": format!("perm{n}"), "request": {"subtype": "can_use_tool", "tool_name": "AskUserQuestion", "input": input, "tool_use_id": format!("t{n}")}}));
            let answer = loop {
                let Some(l) = lines.next() else { return };
                let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
                log(json!({"got": v}));
                if v["type"] == "control_response" {
                    break v;
                }
            };
            let r = &answer["response"]["response"];
            let reply = if r["behavior"] == "allow" {
                let a = &r["updatedInput"]["answers"];
                let said = format!("\"Which color?\"=\"{}\". \"Which sizes?\"=\"{}\"", a["Which color?"].as_str().unwrap_or(""), a["Which sizes?"].as_str().unwrap_or(""));
                out(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": format!("t{n}"), "content": format!("User has answered your questions: {said}.")}]}, "parent_tool_use_id": null}));
                format!("You chose {} in {}.", a["Which color?"].as_str().unwrap_or("?"), a["Which sizes?"].as_str().unwrap_or("?"))
            } else {
                let why = r["message"].as_str().unwrap_or("denied").to_string();
                out(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": format!("t{n}"), "content": why, "is_error": true}]}, "parent_tool_use_id": null}));
                "No answers.".to_string()
            };
            start(&format!("m{n}b"));
            text_block(0, &reply);
            stop("end_turn");
            result(&session, None);
        } else if text.contains("edit") {
            start(&format!("m{n}a"));
            ev(json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": format!("t{n}"), "name": "Edit", "input": {}}}));
            ev(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"file_path\": \"notes.txt\", "}}));
            ev(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "\"old_string\": \"two\", \"new_string\": \"TWO\"}"}}));
            ev(json!({"type": "content_block_stop", "index": 0}));
            stop("tool_use");
            let input = json!({"file_path": "notes.txt", "old_string": "two", "new_string": "TWO"});
            out(json!({"type": "control_request", "request_id": format!("perm{n}"), "request": {"subtype": "can_use_tool", "tool_name": "Edit", "input": input, "tool_use_id": format!("t{n}"), "permission_suggestions": [{"type": "setMode", "mode": "acceptEdits", "destination": "session"}]}}));
            let answer = loop {
                let Some(l) = lines.next() else { return };
                let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
                log(json!({"got": v}));
                if v["type"] == "control_response" {
                    break v;
                }
            };
            let behavior = &answer["response"]["response"]["behavior"];
            // Allowing edits for the session switches its mode.
            if let Some(mode) = answer["response"]["response"]["updatedPermissions"][0]["mode"].as_str() {
                system("status", json!({"status": null, "permissionMode": mode}));
            }
            if behavior == "allow" {
                let patch = json!([{"oldStart": 1, "oldLines": 3, "newStart": 1, "newLines": 3, "lines": [" one", "-two", "+TWO", " three"]}]);
                out(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": format!("t{n}"), "content": "The file notes.txt has been updated."}]}, "parent_tool_use_id": null, "tool_use_result": {"filePath": "notes.txt", "structuredPatch": patch}}));
            } else {
                let why = answer["response"]["response"]["message"].as_str().unwrap_or("denied").to_string();
                out(json!({"type": "user", "message": {"role": "user", "content": [{"type": "tool_result", "tool_use_id": format!("t{n}"), "content": why, "is_error": true}]}, "parent_tool_use_id": null}));
            }
            start(&format!("m{n}b"));
            text_block(0, if behavior == "allow" { "Edited." } else { "Not allowed." });
            stop("end_turn");
            result(&session, None);
        } else if text.contains("slow") {
            start(&format!("m{n}"));
            ev(json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}));
            // Words until asked to stop.
            loop {
                ev(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "word "}}));
                if let Ok(l) = rx.recv_timeout(Duration::from_millis(20)) {
                    let v: Value = serde_json::from_str(&l).unwrap_or_default();
                    log(json!({"got": v}));
                    if v["request"]["subtype"] == "interrupt" {
                        out(json!({"type": "control_response", "response": {"subtype": "success", "request_id": v["request_id"]}}));
                        result(&session, Some("interrupted"));
                        break;
                    }
                }
            }
        } else if text.contains("fail") {
            out(json!({"type": "assistant", "message": {"id": format!("m{n}"), "model": "<synthetic>", "role": "assistant", "content": [{"type": "text", "text": "API Error: overloaded"}]}, "parent_tool_use_id": null, "error": "unknown"}));
            result(&session, Some("API Error: overloaded"));
        } else {
            start(&format!("m{n}"));
            ev(json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}));
            ev(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "hmm"}}));
            ev(json!({"type": "content_block_stop", "index": 0}));
            text_block(1, &format!("echo: {text}"));
            stop("end_turn");
            result(&session, None);
        }
    }
}
