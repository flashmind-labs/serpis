//! A small `codex app-server` stand-in for integration tests.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn send(value: Value) {
    let mut out = std::io::stdout().lock();
    writeln!(out, "{value}").unwrap();
    out.flush().unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.get(1).map(String::as_str) != Some("app-server") {
        return;
    }
    let stdin = std::io::stdin();
    let mut pending_command: Option<(Value, String, String)> = None;
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some((approval_id, thread, turn)) = pending_command.take() {
            if message["id"] == approval_id {
                let accepted = message
                    .pointer("/result/decision")
                    .and_then(Value::as_str)
                    .is_some_and(|d| d.starts_with("accept"));
                let status = if accepted { "completed" } else { "declined" };
                send(
                    json!({"method":"item/commandExecution/outputDelta","params":{"threadId":thread,"turnId":turn,"itemId":"cmd-1","delta":if accepted {"ok\n"} else {""}}}),
                );
                send(
                    json!({"method":"item/completed","params":{"threadId":thread,"turnId":turn,"completedAtMs":3,"item":{"type":"commandExecution","id":"cmd-1","command":"printf ok","cwd":"/tmp","commandActions":[],"status":status,"aggregatedOutput":if accepted {"ok\n"} else {""},"exitCode":if accepted {Value::from(0)} else {Value::Null}}}}),
                );
                send(
                    json!({"method":"item/agentMessage/delta","params":{"threadId":thread,"turnId":turn,"itemId":"answer-1","delta":if accepted {"Command finished."} else {"Command was denied."}}}),
                );
                finish_turn(&thread, &turn, status != "declined");
                continue;
            }
            pending_command = Some((approval_id, thread, turn));
        }
        let id = message["id"].clone();
        match message["method"].as_str() {
            Some("initialize") => send(
                json!({"id":id,"result":{"userAgent":"fake-codex","platformFamily":"unix","platformOs":"test"}}),
            ),
            Some("model/list") => {
                send(json!({"id":id,"result":{"data":[model()],"nextCursor":null}}))
            }
            Some("thread/start") => send(json!({"id":id,"result":{"thread":{"id":"thr-fake"}}})),
            Some("thread/resume") => send(
                json!({"id":id,"result":{"thread":{"id":message.pointer("/params/threadId").and_then(Value::as_str).unwrap_or("thr-fake")}}}),
            ),
            Some("thread/compact/start") => send(json!({"id":id,"result":{}})),
            Some("turn/start") => {
                let thread = message
                    .pointer("/params/threadId")
                    .and_then(Value::as_str)
                    .unwrap_or("thr-fake")
                    .to_string();
                let turn = format!("turn-{}", id.as_u64().unwrap_or(1));
                send(
                    json!({"id":id,"result":{"turn":{"id":turn,"items":[],"status":"inProgress"}}}),
                );
                send(
                    json!({"method":"turn/started","params":{"threadId":thread,"turn":{"id":turn,"items":[],"status":"inProgress"}}}),
                );
                let text = message
                    .pointer("/params/input")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v["text"].as_str())
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.contains("run a command") {
                    send(
                        json!({"method":"item/started","params":{"threadId":thread,"turnId":turn,"startedAtMs":1,"item":{"type":"commandExecution","id":"cmd-1","command":"printf ok","cwd":"/tmp","commandActions":[],"status":"inProgress"}}}),
                    );
                    let approval = json!(9001);
                    send(
                        json!({"id":approval,"method":"item/commandExecution/requestApproval","params":{"threadId":thread,"turnId":turn,"itemId":"cmd-1","startedAtMs":2,"command":"printf ok","cwd":"/tmp","reason":"test"}}),
                    );
                    pending_command = Some((approval, thread, turn));
                } else {
                    send(
                        json!({"method":"item/reasoning/summaryTextDelta","params":{"threadId":thread,"turnId":turn,"itemId":"reason-1","summaryIndex":0,"delta":"brief thought"}}),
                    );
                    send(
                        json!({"method":"item/agentMessage/delta","params":{"threadId":thread,"turnId":turn,"itemId":"answer-1","delta":format!("echo: {text}")}}),
                    );
                    finish_turn(&thread, &turn, true);
                }
            }
            Some("turn/steer" | "turn/interrupt") => send(json!({"id":id,"result":{}})),
            _ => {}
        }
    }
}

fn finish_turn(thread: &str, turn: &str, success: bool) {
    send(
        json!({"method":"thread/tokenUsage/updated","params":{"threadId":thread,"turnId":turn,"tokenUsage":{"last":{"inputTokens":12,"cachedInputTokens":3,"cacheWriteInputTokens":0,"outputTokens":4,"reasoningOutputTokens":1,"totalTokens":19},"total":{"inputTokens":12,"cachedInputTokens":3,"cacheWriteInputTokens":0,"outputTokens":4,"reasoningOutputTokens":1,"totalTokens":19},"modelContextWindow":200000}}}),
    );
    send(
        json!({"method":"turn/completed","params":{"threadId":thread,"turn":{"id":turn,"items":[],"status":if success {"completed"} else {"failed"}}}}),
    );
}

fn model() -> Value {
    json!({
        "id":"gpt-fake", "model":"gpt-fake", "displayName":"GPT Fake", "description":"test",
        "hidden":false, "isDefault":true, "defaultReasoningEffort":"medium",
        "supportedReasoningEfforts":[{"reasoningEffort":"low","description":""},{"reasoningEffort":"high","description":""}],
        "inputModalities":["text","image"]
    })
}
