//! A small `codex app-server` stand-in for integration tests.
//!
//! "challenge {args}" calls `challenge_finding` with those arguments through the MCP server its
//! `-c mcp_servers.serpis.*` overrides name (asking first, as Codex does, with an MCP
//! elicitation), and reports it as an `mcpToolCall` item.

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
    let server = mcp_server(&args);
    let stdin = std::io::stdin();
    let mut pending_command: Option<(Value, String, String)> = None;
    // (elicitation id, thread, turn, the call's arguments)
    let mut pending_mcp: Option<(Value, String, String, Value)> = None;
    for line in stdin.lock().lines().map_while(Result::ok) {
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if let Some((ask_id, thread, turn, arguments)) = pending_mcp.take() {
            if message["id"] == ask_id {
                let accepted = message.pointer("/result/action").and_then(Value::as_str) == Some("accept");
                let item = |status: &str, result: Value| json!({"type":"mcpToolCall","id":"mcp-1","server":"serpis","tool":"challenge_finding","arguments":arguments,"status":status,"result":result});
                let (status, result) = match (&server, accepted) {
                    (Some((command, args, env)), true) => {
                        let r = serpis_testkit::mcp_call(command, args, env, "challenge_finding", &arguments);
                        (if r["isError"] == true { "failed" } else { "completed" }, r)
                    }
                    _ => ("declined", Value::Null),
                };
                send(json!({"method":"item/completed","params":{"threadId":thread,"turnId":turn,"completedAtMs":3,"item":item(status, result)}}));
                send(json!({"method":"item/agentMessage/delta","params":{"threadId":thread,"turnId":turn,"itemId":"answer-1","delta":format!("Finding {status}.")}}));
                finish_turn(&thread, &turn, true);
                continue;
            }
            pending_mcp = Some((ask_id, thread, turn, arguments));
        }
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
            Some("account/read") => send(json!({"id":id,"result":{
                "account":{"type":"chatgpt","email":"codex@example.test","planType":"pro","workspaceName":null},
                "requiresOpenaiAuth":true
            }})),
            Some("account/rateLimits/read") => send(json!({"id":id,"result":{
                "rateLimits":{"limitId":"codex","limitName":null,"planType":"pro","primary":{"usedPercent":25,"windowDurationMins":300,"resetsAt":2000000000},"secondary":{"usedPercent":40,"windowDurationMins":10080,"resetsAt":2000500000},"credits":{"hasCredits":true,"unlimited":false,"balance":"12.5"}},
                "rateLimitsByLimitId":null,
                "rateLimitResetCredits":{"availableCount":1,"credits":null}
            }})),
            Some("account/usage/read") => send(json!({"id":id,"result":{
                "summary":{"lifetimeTokens":1234567,"peakDailyTokens":45678,"longestRunningTurnSec":540,"currentStreakDays":8,"longestStreakDays":14},
                "dailyUsageBuckets":null,
                "threadUsage":null
            }})),
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
                if let Some(arguments) = text.strip_prefix("challenge ").and_then(|a| serde_json::from_str::<Value>(a).ok()) {
                    send(
                        json!({"method":"item/started","params":{"threadId":thread,"turnId":turn,"startedAtMs":1,"item":{"type":"mcpToolCall","id":"mcp-1","server":"serpis","tool":"challenge_finding","arguments":arguments,"status":"inProgress"}}}),
                    );
                    let ask = json!(9002);
                    send(
                        json!({"id":ask,"method":"mcpServer/elicitation/request","params":{"serverName":"serpis","threadId":thread,"turnId":turn,"mode":"form","message":"Allow the serpis MCP server to run tool \"challenge_finding\"?","requestedSchema":{"type":"object","properties":{}},"_meta":{"codex_approval_kind":"mcp_tool_call","tool_name":"challenge_finding"}}}),
                    );
                    pending_mcp = Some((ask, thread, turn, arguments));
                } else if text.contains("run a command") {
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

/// The MCP server `-c mcp_servers.serpis.command=..`, `.args=[..]` and `.env={K=".."}` name:
/// (command, args, env). Their values are TOML; these are also JSON, but for the env table.
type Server = (String, Vec<String>, Vec<(String, String)>);

fn mcp_server(args: &[String]) -> Option<Server> {
    let value = |key: &str| args.iter().find_map(|a| a.strip_prefix(&format!("mcp_servers.serpis.{key}=")));
    let command: String = serde_json::from_str(value("command")?).ok()?;
    let list: Vec<String> = value("args").and_then(|v| serde_json::from_str(v).ok()).unwrap_or_default();
    let env = value("env")
        .and_then(|t| t.strip_prefix('{')?.strip_suffix('}'))
        .and_then(|t| t.split_once('='))
        .and_then(|(k, v)| Some((k.trim().to_string(), serde_json::from_str::<String>(v.trim()).ok()?)))
        .into_iter()
        .collect();
    Some((command, list, env))
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
