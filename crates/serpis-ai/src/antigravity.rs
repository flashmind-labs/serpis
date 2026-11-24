//! Antigravity as a provider. Chats on an `antigravity/*` model run through `agy acp`,
//! using the CLI's login, agent harness, tools, permissions, sessions and compaction.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::types::Model;

pub const PROVIDER: &str = "antigravity";

/// `SERPIS_ANTIGRAVITY_PATH`, then `agy` on PATH or in its usual install locations. Antigravity
/// is offered only after it has a config directory, so test homes do not start a real CLI.
pub fn find_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SERPIS_ANTIGRAVITY_PATH") {
        return Some(PathBuf::from(p)).filter(|p| !p.as_os_str().is_empty() && is_executable(p));
    }
    let home = dirs::home_dir()?;
    if !home.join(".gemini").join("antigravity-cli").is_dir() {
        return None;
    }
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join("agy"));
    let installed = [home.join(".local/bin/agy")];
    on_path.chain(installed).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Models offered before `agy acp` was asked for the account's live catalog.
pub fn fallback_models() -> Vec<Model> {
    vec![
        model("gemini-4-neon", "Gemini 4 Neon", &["low", "medium", "high", "xhigh"]),
        model("gemini-4-argon", "Gemini 4 Argon", &["low", "medium", "high"]),
        model("gemini-3.8-flash", "Gemini 3.8 Flash", &["low", "medium", "high"]),
    ]
}

fn context_window(id: &str) -> u64 {
    if id.starts_with("gemini-") {
        1_048_576
    } else if id.starts_with("claude-") && !id.contains("-4-") && !id.contains("-3-") {
        1_000_000
    } else {
        200_000
    }
}

pub fn model(id: &str, name: &str, efforts: &[&str]) -> Model {
    let mut map = serde_json::Map::new();
    map.insert("off".into(), Value::Null);
    map.insert("minimal".into(), Value::Null);
    for level in ["low", "medium", "high", "xhigh", "max", "ultra"] {
        map.insert(
            level.into(),
            if efforts.contains(&level) {
                json!(level)
            } else {
                Value::Null
            },
        );
    }
    serde_json::from_value(json!({
        "id": id,
        "name": format!("Antigravity · {name}"),
        "api": PROVIDER,
        "provider": PROVIDER,
        "reasoning": !efforts.is_empty(),
        "thinkingLevelMap": map,
        "input": ["text", "image"],
        "contextWindow": context_window(id),
        "maxTokens": 65_536
    }))
    .expect("a valid model")
}

const KNOWN_LEVELS: &[&str] = &["low", "medium", "high", "xhigh", "max", "ultra"];

/// Build models from an `agy acp` `session/new` result (`configOptions` and `models.availableModels`).
pub fn models_from_session(result: &Value) -> Vec<Model> {
    let available: Vec<&str> = result
        .pointer("/models/availableModels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m["modelId"].as_str())
        .collect();

    let default_levels: Vec<&str> = result["configOptions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|opt| opt["id"].as_str() == Some("thought_level") || opt["category"].as_str() == Some("thought_level"))
        .and_then(|opt| opt["options"].as_array())
        .into_iter()
        .flatten()
        .filter_map(|o| o["value"].as_str())
        .collect();

    let model_options = result["configOptions"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|opt| opt["id"].as_str() == Some("model") || opt["category"].as_str() == Some("model"))
        .and_then(|opt| opt["options"].as_array());

    if let Some(options) = model_options {
        let mut out = Vec::new();
        for option in options {
            let Some(id) = option["value"].as_str() else {
                continue;
            };
            let name = option["name"].as_str().unwrap_or(id);
            let mut efforts: Vec<&str> = Vec::new();
            for &level in KNOWN_LEVELS {
                let suffixed = format!("{id}-{level}");
                if available.iter().any(|a| *a == suffixed) {
                    efforts.push(level);
                }
            }
            // If availableModels wasn't provided at all, fall back to the session's thought_level options.
            if efforts.is_empty() && available.is_empty() {
                efforts.extend(default_levels.iter().copied());
            }
            out.push(model(id, name, &efforts));
        }
        if !out.is_empty() {
            return out;
        }
    }

    // Fallback: older runtimes without configOptions list only models.availableModels.
    let mut seen = BTreeSet::new();
    result
        .pointer("/models/availableModels")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|entry| {
            let id = entry["modelId"].as_str()?;
            if !seen.insert(id.to_string()) {
                return None;
            }
            let name = entry["name"].as_str().unwrap_or(id);
            Some(model(id, name, &[]))
        })
        .collect()
}

/// Ask a fresh `agy acp` process for the account's model catalog. This does not start a model turn.
pub async fn discover(binary: &Path) -> Result<Vec<Model>> {
    let mut child = tokio::process::Command::new(binary)
        .arg("acp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {} acp", binary.display()))?;
    let mut input = child.stdin.take().context("agy acp has no stdin")?;
    let mut lines = BufReader::new(child.stdout.take().context("agy acp has no stdout")?).lines();

    let init = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": 1,
            "clientCapabilities": {},
            "clientInfo": {"name": "serpis", "title": "Serpis", "version": env!("CARGO_PKG_VERSION")}
        }
    });
    input.write_all(format!("{init}\n").as_bytes()).await?;
    input.flush().await?;
    wait_response(&mut lines, 1).await?;

    let cwd = std::env::temp_dir();
    let new_session = json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "session/new",
        "params": {"cwd": cwd, "mcpServers": []}
    });
    input.write_all(format!("{new_session}\n").as_bytes()).await?;
    input.flush().await?;
    let response = wait_response(&mut lines, 2).await?;
    let _ = child.kill().await;
    Ok(models_from_session(&response["result"]))
}

/// Read Antigravity's plan usage via `agy -p /usage --output-format stream-json`.
/// Answers from the quota service without a model turn or saved conversation.
pub async fn usage(binary: &Path) -> Result<Value> {
    let mut child = tokio::process::Command::new(binary)
        .args(["-p", "/usage", "--output-format", "stream-json"])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {} -p /usage", binary.display()))?;
    let mut lines = BufReader::new(child.stdout.take().context("agy has no stdout")?).lines();
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let mut found = None;
    while let Ok(Ok(Some(line))) = tokio::time::timeout_at(deadline, lines.next_line()).await {
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value["event"].as_str() == Some("command_result")
            && value.pointer("/command/name").and_then(Value::as_str) == Some("usage")
        {
            found = Some(value["command"]["data"].clone());
            break;
        }
        if value["event"].as_str() == Some("result")
            && value.pointer("/result/command/name").and_then(Value::as_str) == Some("usage")
        {
            found = Some(value["result"]["command"]["data"].clone());
            break;
        }
    }
    let _ = child.kill().await;
    found.context("Antigravity did not return /usage data")
}

async fn wait_response(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
) -> Result<Value> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        let line = tokio::time::timeout_at(deadline, lines.next_line())
            .await
            .context("Antigravity ACP timed out")??
            .context("Antigravity ACP stopped")?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if value["id"].as_u64() == Some(id) {
            if let Some(error) = value.get("error") {
                bail!(
                    "{}",
                    error["message"]
                        .as_str()
                        .unwrap_or("Antigravity ACP request failed")
                );
            }
            return Ok(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_session_config_and_available_models() {
        let result = json!({
            "sessionId": "s-1",
            "models": {
                "availableModels": [
                    {"modelId": "gemini-4-neon-low", "name": "Gemini 4 Neon (Low)"},
                    {"modelId": "gemini-4-neon-medium", "name": "Gemini 4 Neon (Medium)"},
                    {"modelId": "gemini-4-neon-high", "name": "Gemini 4 Neon (High)"},
                    {"modelId": "gemini-4-neon-xhigh", "name": "Gemini 4 Neon (XHigh)"},
                    {"modelId": "gemini-pro-orchestrator-neon", "name": "Orchestrator (Neon)"}
                ]
            },
            "configOptions": [
                {
                    "id": "model",
                    "category": "model",
                    "type": "select",
                    "currentValue": "gemini-4-neon",
                    "options": [
                        {"value": "gemini-4-neon", "name": "Gemini 4 Neon"},
                        {"value": "gemini-pro-orchestrator-neon", "name": "Orchestrator (Neon)"}
                    ]
                },
                {
                    "id": "thought_level",
                    "category": "thought_level",
                    "type": "select",
                    "currentValue": "high",
                    "options": [
                        {"value": "low", "name": "Low"},
                        {"value": "medium", "name": "Medium"},
                        {"value": "high", "name": "High"},
                        {"value": "xhigh", "name": "Extra"}
                    ]
                }
            ]
        });
        let models = models_from_session(&result);
        assert_eq!(models.len(), 2);
        assert_eq!(models[0].id, "gemini-4-neon");
        assert_eq!(models[0].name, "Antigravity · Gemini 4 Neon");
        assert_eq!(models[0].context_window, 1_048_576);
        assert_eq!(models[0].thinking_map(crate::ThinkingLevel::Xhigh), Some(Some("xhigh")));
        assert_eq!(models[0].thinking_map(crate::ThinkingLevel::Max), Some(None));
        assert_eq!(models[1].id, "gemini-pro-orchestrator-neon");
        assert!(!models[1].reasoning);
    }
}
