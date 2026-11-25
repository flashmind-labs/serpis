//! A small `agy` stand-in (`agy acp` and `agy -p /usage`) for integration tests.
//!
//! "challenge {args}" calls `challenge_finding` with those arguments on the `serpis` MCP server
//! its session was given (`mcpServers`), through a tool of its own for MCP calls, asking first.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn send(value: Value) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{value}").unwrap();
    out.flush().unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) == Some("-p")
        && args.get(2).map(String::as_str) == Some("/usage")
    {
        send(json!({
            "event": "command_result",
            "command": {
                "name": "usage",
                "data": {
                    "description": "Within each group, models share a weekly limit and a 5-hour limit.",
                    "groups": [
                        {
                            "name": "Gemini Models",
                            "description": "Models within this group: Gemini 4 Neon, Gemini 3.8 Flash",
                            "buckets": [
                                {"id": "gemini-weekly", "name": "Weekly Limit Remaining", "window": "weekly", "remaining_fraction": 0.85, "reset_time": "2026-11-30T20:00:00Z"},
                                {"id": "gemini-5h", "name": "Five Hour Limit Remaining", "window": "5h", "remaining_fraction": 0.90, "reset_time": "2026-11-24T01:00:00Z"}
                            ]
                        }
                    ]
                }
            }
        }));
        return;
    }
    if args.get(1).map(String::as_str) != Some("acp") {
        return;
    }

    let stdin = std::io::stdin();
    // (permission request id, prompt to finish once answered, session, tool call)
    let mut pending_permission: Option<(Value, Option<Value>, String, String)> = None;
    let mut current_model = "gemini-4-neon".to_string();
    let mut current_thought = "high".to_string();
    let mut current_mode = "default".to_string();
    // The MCP servers its session was given, and a call to one waiting for permission:
    // (request id, prompt, session, arguments).
    let mut mcp_servers: Vec<Value> = Vec::new();
    let mut pending_mcp: Option<(Value, Value, String, Value)> = None;

    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some((perm_id, prompt_id, session_id, arguments)) = pending_mcp.take() {
            if message["id"] == perm_id {
                let option = message.pointer("/result/outcome/optionId").and_then(Value::as_str);
                let server = mcp_servers.iter().find(|s| s["name"] == "serpis");
                let (status, text) = match (server, matches!(option, Some("once" | "always"))) {
                    (Some(s), true) => {
                        let strings = |v: &Value| v.as_array().into_iter().flatten().filter_map(Value::as_str).map(str::to_string).collect::<Vec<_>>();
                        let env: Vec<(String, String)> = s["env"].as_array().into_iter().flatten().map(|e| (e["name"].as_str().unwrap_or("").to_string(), e["value"].as_str().unwrap_or("").to_string())).collect();
                        let r = serpis_testkit::mcp_call(s["command"].as_str().unwrap_or(""), &strings(&s["args"]), &env, "challenge_finding", &arguments);
                        (if r["isError"] == true { "failed" } else { "completed" }, r["content"][0]["text"].as_str().unwrap_or("").to_string())
                    }
                    (None, _) => ("failed", "no serpis MCP server".to_string()),
                    _ => ("failed", "denied".to_string()),
                };
                send(json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session_id, "update": {
                    "sessionUpdate": "tool_call_update", "toolCallId": "mcp-1", "status": status,
                    "content": [{"type": "content", "content": {"type": "text", "text": text}}]
                }}}));
                send(json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session_id, "update": {
                    "sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": format!("Finding {status}.")}
                }}}));
                finish_prompt(prompt_id, &session_id);
                continue;
            }
            pending_mcp = Some((perm_id, prompt_id, session_id, arguments));
        }
        if let Some((perm_id, prompt_id, session_id, tool_id)) = pending_permission.take() {
            if message["id"] == perm_id {
                let outcome = message.pointer("/result/outcome/outcome").and_then(Value::as_str);
                let option = message.pointer("/result/outcome/optionId").and_then(Value::as_str);
                let accepted = outcome == Some("selected") && matches!(option, Some("once" | "always"));
                let status = if accepted { "completed" } else { "failed" };
                let output = if accepted { "ok\n" } else { "denied" };
                send(json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": session_id,
                        "update": {
                            "sessionUpdate": "tool_call_update",
                            "toolCallId": tool_id,
                            "status": status,
                            "content": [{"type": "content", "content": {"type": "text", "text": output}}]
                        }
                    }
                }));
                send(json!({
                    "jsonrpc": "2.0",
                    "method": "session/update",
                    "params": {
                        "sessionId": session_id,
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": {"type": "text", "text": if accepted { "Command finished." } else { "Command was denied." }}
                        }
                    }
                }));
                if let Some(prompt_id) = prompt_id {
                    finish_prompt(prompt_id, &session_id);
                }
                continue;
            }
            pending_permission = Some((perm_id, prompt_id, session_id, tool_id));
        }

        let id = message["id"].clone();
        match message["method"].as_str() {
            Some("initialize") => send(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": {
                    "protocolVersion": 1,
                    "agentCapabilities": {
                        "loadSession": true,
                        "promptCapabilities": {"image": true, "audio": false, "embeddedContext": true},
                        "_meta": {"antigravity": {"tier": true, "systemPrompt": {"append": true}}}
                    },
                    "agentInfo": {"name": "Fake Antigravity ACP", "version": "1.5.15"}
                }
            })),
            Some("session/new" | "session/load") => {
                mcp_servers = message.pointer("/params/mcpServers").and_then(Value::as_array).cloned().unwrap_or_default();
                let sid = message
                    .pointer("/params/sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("agy-fake-session")
                    .to_string();
                send(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {
                        "sessionId": sid,
                        "models": {
                            "availableModels": [
                                {"modelId": "gemini-4-neon-low", "name": "Gemini 4 Neon (Low)"},
                                {"modelId": "gemini-4-neon-high", "name": "Gemini 4 Neon (High)"},
                                {"modelId": "gemini-3.8-flash-low", "name": "Gemini 3.8 Flash (Low)"},
                                {"modelId": "gemini-3.8-flash-high", "name": "Gemini 3.8 Flash (High)"}
                            ],
                            "currentModelId": "gemini-4-neon-high"
                        },
                        "configOptions": config_options(&current_model, &current_thought, &current_mode)
                    }
                }));
            }
            Some("session/set_mode") => {
                let sid = message.pointer("/params/sessionId").and_then(Value::as_str).unwrap_or("agy-fake-session");
                if let Some(mode) = message.pointer("/params/modeId").and_then(Value::as_str) {
                    current_mode = mode.to_string();
                    send(json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": sid,
                            "update": {"sessionUpdate": "current_mode_update", "currentModeId": current_mode}
                        }
                    }));
                }
                send(json!({"jsonrpc": "2.0", "id": id, "result": {}}));
            }
            Some("session/set_config_option") => {
                let config_id = message.pointer("/params/configId").and_then(Value::as_str).unwrap_or("");
                let value = message.pointer("/params/value").and_then(Value::as_str).unwrap_or("");
                let sid = message.pointer("/params/sessionId").and_then(Value::as_str).unwrap_or("agy-fake-session");
                match config_id {
                    "model" => current_model = value.to_string(),
                    "thought_level" => current_thought = value.to_string(),
                    "mode" => {
                        current_mode = value.to_string();
                        send(json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": sid,
                                "update": {"sessionUpdate": "current_mode_update", "currentModeId": current_mode}
                            }
                        }));
                    }
                    _ => {}
                }
                send(json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": {"configOptions": config_options(&current_model, &current_thought, &current_mode)}
                }));
            }
            Some("session/prompt") => {
                let session_id = message
                    .pointer("/params/sessionId")
                    .and_then(Value::as_str)
                    .unwrap_or("agy-fake-session")
                    .to_string();
                let text = message
                    .pointer("/params/prompt")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if let Some(arguments) = text.strip_prefix("challenge ").and_then(|a| serde_json::from_str::<Value>(a).ok()) {
                    let tool_call = json!({
                        "toolCallId": "mcp-1", "title": "Call MCP tool", "kind": "other",
                        "rawInput": {"server_name": "serpis", "tool_name": "challenge_finding", "arguments": arguments}
                    });
                    let mut update = tool_call.clone();
                    update["sessionUpdate"] = json!("tool_call");
                    update["status"] = json!("pending");
                    send(json!({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": session_id, "update": update}}));
                    send(json!({"jsonrpc": "2.0", "id": 9003, "method": "session/request_permission", "params": {
                        "sessionId": session_id, "toolCall": tool_call,
                        "options": [
                            {"optionId": "once", "name": "Allow once", "kind": "allow_once"},
                            {"optionId": "always", "name": "Allow always", "kind": "allow_always"},
                            {"optionId": "deny", "name": "Deny", "kind": "reject_once"}
                        ]
                    }}));
                    pending_mcp = Some((json!(9003), id, session_id, arguments));
                } else if text.contains("edit a file") {
                    send(json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "tool_call",
                                "toolCallId": "edit-1",
                                "title": "Edit notes.txt",
                                "kind": "edit",
                                "status": "in_progress",
                                "rawInput": {"Path": "notes.txt"}
                            }
                        }
                    }));
                    send(json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "tool_call_update",
                                "toolCallId": "edit-1",
                                "status": "completed",
                                "content": [{"type": "diff", "path": "notes.txt", "oldText": "before\n", "newText": "after\n"}]
                            }
                        }
                    }));
                    send(json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": "Edited notes.txt."}
                            }
                        }
                    }));
                    finish_prompt(id, &session_id);
                } else if text.contains("interleaved report") {
                    send(json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": "Waiting for tests to finish."}
                            }
                        }
                    }));
                    send(json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "agent_thought_chunk",
                                "content": {"type": "text", "text": "Tests passed, writing the final report."}
                            }
                        }
                    }));
                    send(json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": "Done: final report delivered."}
                            }
                        }
                    }));
                    finish_prompt(id, &session_id);
                } else if text.contains("run a command") {
                    send(json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "tool_call",
                                "toolCallId": "cmd-1",
                                "title": "Run printf ok",
                                "kind": "execute",
                                "status": "pending",
                                "rawInput": {"CommandLine": "printf ok", "Cwd": "/tmp"}
                            }
                        }
                    }));
                    ask_permission(9001, &session_id, "cmd-1");
                    pending_permission = Some((json!(9001), Some(id), session_id, "cmd-1".into()));
                } else if text.contains("background") {
                    // The prompt ends, then Antigravity goes on by itself and asks to run a
                    // command: after starting to talk ("background turn"), or before.
                    finish_prompt(id, &session_id);
                    if text.contains("background turn") {
                        send(json!({
                            "jsonrpc": "2.0",
                            "method": "session/update",
                            "params": {
                                "sessionId": session_id,
                                "update": {
                                    "sessionUpdate": "agent_message_chunk",
                                    "content": {"type": "text", "text": "Background job started."}
                                }
                            }
                        }));
                    }
                    send(json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "tool_call",
                                "toolCallId": "bg-1",
                                "title": "Run printf ok",
                                "kind": "execute",
                                "status": "pending",
                                "rawInput": {"CommandLine": "printf ok", "Cwd": "/tmp"}
                            }
                        }
                    }));
                    ask_permission(9002, &session_id, "bg-1");
                    pending_permission = Some((json!(9002), None, session_id, "bg-1".into()));
                } else {
                    if text.contains("noisy") {
                        // A log line on stdout that isn't JSON-RPC.
                        println!("agy: warming up");
                    }
                    send(json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "agent_thought_chunk",
                                "content": {"type": "text", "text": "brief thought"}
                            }
                        }
                    }));
                    send(json!({
                        "jsonrpc": "2.0",
                        "method": "session/update",
                        "params": {
                            "sessionId": session_id,
                            "update": {
                                "sessionUpdate": "agent_message_chunk",
                                "content": {"type": "text", "text": format!("echo: {text}")}
                            }
                        }
                    }));
                    finish_prompt(id, &session_id);
                }
            }
            _ => {}
        }
    }
}

fn ask_permission(id: u64, session_id: &str, tool_id: &str) {
    send(json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "session/request_permission",
        "params": {
            "sessionId": session_id,
            "toolCall": {
                "toolCallId": tool_id,
                "title": "Run printf ok",
                "kind": "execute",
                "rawInput": {"CommandLine": "printf ok", "Cwd": "/tmp"}
            },
            "options": [
                {"optionId": "once", "name": "Allow once", "kind": "allow_once"},
                {"optionId": "always", "name": "Allow always", "kind": "allow_always"},
                {"optionId": "deny", "name": "Deny", "kind": "reject_once"}
            ]
        }
    }));
}

fn finish_prompt(id: Value, session_id: &str) {
    send(json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
            "sessionId": session_id,
            "update": {"sessionUpdate": "usage_update", "used": 42, "size": 1_048_576}
        }
    }));
    send(json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": {
            "stopReason": "end_turn",
            "usage": {
                "inputTokens": 30,
                "outputTokens": 12,
                "cachedReadTokens": 0,
                "cachedWriteTokens": 0,
                "thoughtTokens": 5,
                "totalTokens": 42
            }
        }
    }));
}

fn config_options(model: &str, thought: &str, mode: &str) -> Value {
    json!([
        {
            "id": "model",
            "category": "model",
            "type": "select",
            "currentValue": model,
            "options": [
                {"value": "gemini-4-neon", "name": "Gemini 4 Neon"},
                {"value": "gemini-3.8-flash", "name": "Gemini 3.8 Flash"}
            ]
        },
        {
            "id": "thought_level",
            "category": "thought_level",
            "type": "select",
            "currentValue": thought,
            "options": [
                {"value": "low", "name": "Low"},
                {"value": "high", "name": "High"}
            ]
        },
        {
            "id": "mode",
            "category": "mode",
            "type": "select",
            "currentValue": mode,
            "options": [
                {"value": "default", "name": "Default"},
                {"value": "accept-edits", "name": "Accept edits"},
                {"value": "auto", "name": "Auto"},
                {"value": "plan", "name": "Plan"}
            ]
        }
    ])
}
