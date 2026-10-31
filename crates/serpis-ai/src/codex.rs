//! Codex as a provider. Chats on a `codex/*` model run through `codex app-server`,
//! using the CLI's login, agent harness, tools, permissions, threads and compaction.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::types::Model;

pub const PROVIDER: &str = "codex";

/// `SERPIS_CODEX_PATH`, then `codex` on PATH or in its usual install locations. Codex is
/// offered only after it has a config directory, so test homes do not start a real CLI.
pub fn find_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SERPIS_CODEX_PATH") {
        return Some(PathBuf::from(p)).filter(|p| !p.as_os_str().is_empty() && is_executable(p));
    }
    let home = dirs::home_dir()?;
    if !home.join(".codex").is_dir() {
        return None;
    }
    let on_path = std::env::var_os("PATH")
        .into_iter()
        .flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .map(|d| d.join("codex"));
    let installed = [
        home.join(".local/bin/codex"),
        home.join(".npm-global/bin/codex"),
    ];
    on_path.chain(installed).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

pub fn fallback_models() -> Vec<Model> {
    vec![model(
        "gpt-6.1-sol",
        "GPT-6.1 Sol",
        &["low", "medium", "high", "xhigh", "max", "ultra"],
    )]
}

fn model(id: &str, name: &str, efforts: &[&str]) -> Model {
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
        "name": format!("Codex · {name}"),
        "api": PROVIDER,
        "provider": PROVIDER,
        "reasoning": !efforts.is_empty(),
        "thinkingLevelMap": map,
        "input": ["text", "image"],
        "contextWindow": 200000,
        "maxTokens": 128000
    }))
    .expect("a valid model")
}

pub fn model_from_list(value: &Value) -> Option<Model> {
    if value["hidden"].as_bool().unwrap_or(false) {
        return None;
    }
    let id = value["id"].as_str().or_else(|| value["model"].as_str())?;
    let name = value["displayName"].as_str().unwrap_or(id);
    let efforts: Vec<&str> = value["supportedReasoningEfforts"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|e| e["reasoningEffort"].as_str())
        .collect();
    let mut model = model(id, name, &efforts);
    if let Some(input) = value["inputModalities"].as_array() {
        model.input = input
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect();
    }
    Some(model)
}

/// Ask a fresh app-server for the model catalog. This does not start a model turn.
pub async fn discover(binary: &Path) -> Result<Vec<Model>> {
    let (mut child, mut input, mut lines) = start(binary).await?;
    initialize(&mut input, &mut lines).await?;
    let request = json!({"method":"model/list","id":2,"params":{"limit":100}});
    input.write_all(format!("{request}\n").as_bytes()).await?;
    let response = wait_response(&mut lines, 2).await?;
    let _ = child.kill().await;
    Ok(response
        .pointer("/result/data")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(model_from_list)
        .collect())
}

/// Read account details, ChatGPT quota windows and account activity for a Codex status page.
/// Errors from a surface unavailable to this auth mode are kept beside the other data.
pub async fn status(binary: &Path) -> Result<Value> {
    let (mut child, mut input, mut lines) = start(binary).await?;
    initialize(&mut input, &mut lines).await?;
    let account = request(&mut input, &mut lines, 2, "account/read", Some(json!({"refreshToken":false}))).await?;
    let (rate_limits, rate_limits_error) = optional_request(&mut input, &mut lines, 3, "account/rateLimits/read").await?;
    let (usage, usage_error) = optional_request(&mut input, &mut lines, 4, "account/usage/read").await?;
    let _ = child.kill().await;
    Ok(json!({
        "account": account,
        "rateLimits": rate_limits,
        "rateLimitsError": rate_limits_error,
        "usage": usage,
        "usageError": usage_error,
    }))
}

async fn start(binary: &Path) -> Result<(tokio::process::Child, tokio::process::ChildStdin, tokio::io::Lines<BufReader<tokio::process::ChildStdout>>)> {
    let mut child = tokio::process::Command::new(binary)
        .arg("app-server")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {} app-server", binary.display()))?;
    let input = child.stdin.take().context("codex app-server has no stdin")?;
    let lines = BufReader::new(child.stdout.take().context("codex app-server has no stdout")?).lines();
    Ok((child, input, lines))
}

async fn initialize(input: &mut tokio::process::ChildStdin, lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>) -> Result<()> {
    let init = json!({"method":"initialize","id":1,"params":{"clientInfo":{"name":"serpis","title":"Serpis","version":env!("CARGO_PKG_VERSION")}}});
    input.write_all(format!("{init}\n").as_bytes()).await?;
    wait_response(lines, 1).await?;
    input.write_all(b"{\"method\":\"initialized\",\"params\":{}}\n").await?;
    Ok(())
}

async fn request(
    input: &mut tokio::process::ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
    method: &str,
    params: Option<Value>,
) -> Result<Value> {
    let mut message = json!({"method":method,"id":id});
    if let Some(params) = params {
        message["params"] = params;
    }
    input.write_all(format!("{message}\n").as_bytes()).await?;
    let response = wait_response(lines, id).await?;
    Ok(response["result"].clone())
}

async fn optional_request(
    input: &mut tokio::process::ChildStdin,
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
    method: &str,
) -> Result<(Value, Value)> {
    let message = json!({"method":method,"id":id});
    input.write_all(format!("{message}\n").as_bytes()).await?;
    let response = wait_answer(lines, id).await?;
    match response.get("error") {
        Some(error) => Ok((Value::Null, json!(error["message"].as_str().unwrap_or("Codex did not return this status")))),
        None => Ok((response["result"].clone(), Value::Null)),
    }
}

async fn wait_response(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
) -> Result<Value> {
    let value = wait_answer(lines, id).await?;
    if let Some(error) = value.get("error") {
        bail!("{}", error["message"].as_str().unwrap_or("Codex app-server request failed"));
    }
    Ok(value)
}

async fn wait_answer(
    lines: &mut tokio::io::Lines<BufReader<tokio::process::ChildStdout>>,
    id: u64,
) -> Result<Value> {
    tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(line) = lines.next_line().await? {
            let Ok(value) = serde_json::from_str::<Value>(&line) else {
                continue;
            };
            if value["id"].as_u64() == Some(id) {
                return Ok(value);
            }
        }
        bail!("codex app-server exited before answering")
    })
    .await
    .context("codex app-server did not answer in a minute")?
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ThinkingLevel, supported_thinking_levels};

    #[test]
    fn model_catalog_maps_reasoning_and_hides_hidden_models() {
        let m = model_from_list(&json!({
            "id":"gpt-test", "displayName":"GPT Test", "hidden":false,
            "supportedReasoningEfforts":[{"reasoningEffort":"low"},{"reasoningEffort":"high"}],
            "inputModalities":["text"]
        }))
        .unwrap();
        assert_eq!(m.provider, PROVIDER);
        assert_eq!(m.input, ["text"]);
        assert_eq!(
            supported_thinking_levels(&m),
            vec![ThinkingLevel::Low, ThinkingLevel::High]
        );
        assert!(model_from_list(&json!({"id":"old", "hidden":true})).is_none());
    }
}
