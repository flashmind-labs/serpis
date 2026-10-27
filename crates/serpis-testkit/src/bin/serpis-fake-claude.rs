//! A stand-in for the `claude` CLI in stream-json mode, for tests: the handshake, permission
//! prompts, interrupts and sessions as Claude Code does them, with scripted replies and no
//! model. Every start and every message it's sent are logged to `$FAKE_CLAUDE_LOG` (JSON
//! lines); sessions are files in `$FAKE_CLAUDE_DIR`.
//!
//! Replies by what the prompt says: "edit" asks to edit `notes.txt` (a permission prompt, then a
//! diff), "slow" streams until interrupted, "fail" ends in an API error, anything else is
//! echoed after a thought.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::Duration;

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

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let cwd = std::env::current_dir().unwrap_or_default();
    log(json!({"start": args, "cwd": cwd}));
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
    while let Some(line) = lines.next() {
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        log(json!({"got": msg}));
        if msg["type"] == "control_request" && msg["request"]["subtype"] == "initialize" {
            let models = [json!({"value": "default", "displayName": "Default"}), json!({"value": "fake", "displayName": "Fake 5", "supportedEffortLevels": ["low", "high"], "supportsThinkingOff": true})];
            out(json!({"type": "control_response", "response": {"subtype": "success", "request_id": msg["request_id"], "response": {"models": models, "account": {"email": "test@example.com", "subscriptionType": "Claude Test"}}}}));
            out(json!({"type": "system", "subtype": "init", "session_id": session}));
            continue;
        }
        if msg["type"] != "user" {
            continue;
        }
        let text = msg["message"]["content"].as_array().and_then(|b| b.iter().rev().find_map(|b| b["text"].as_str())).unwrap_or("").to_string();
        n += 1;
        if text.contains("edit") {
            start(&format!("m{n}a"));
            ev(json!({"type": "content_block_start", "index": 0, "content_block": {"type": "tool_use", "id": format!("t{n}"), "name": "Edit", "input": {}}}));
            ev(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "{\"file_path\": \"notes.txt\", "}}));
            ev(json!({"type": "content_block_delta", "index": 0, "delta": {"type": "input_json_delta", "partial_json": "\"old_string\": \"two\", \"new_string\": \"TWO\"}"}}));
            ev(json!({"type": "content_block_stop", "index": 0}));
            stop("tool_use");
            let input = json!({"file_path": "notes.txt", "old_string": "two", "new_string": "TWO"});
            out(json!({"type": "control_request", "request_id": format!("perm{n}"), "request": {"subtype": "can_use_tool", "tool_name": "Edit", "input": input, "tool_use_id": format!("t{n}")}}));
            let answer = loop {
                let Some(l) = lines.next() else { return };
                let Ok(v) = serde_json::from_str::<Value>(&l) else { continue };
                log(json!({"got": v}));
                if v["type"] == "control_response" {
                    break v;
                }
            };
            let behavior = &answer["response"]["response"]["behavior"];
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
