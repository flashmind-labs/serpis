//! Claude Code as a provider: chats on a `claude-code/*` model run through the `claude` CLI
//! (its harness, tools and login: a Claude subscription works) instead of serpis's own agent
//! loop. This module finds the CLI and lists its models; `serpis_agent::claude_code` runs it.
//!
//! The CLI is driven the way Anthropic's Agent SDKs drive it: `claude -p --input-format
//! stream-json --output-format stream-json`, JSON lines both ways, with `control_request`s for
//! the handshake, permission prompts and interrupts.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context as _, Result, bail};
use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::types::Model;

/// The provider id (and `Model::api`) of Claude Code models.
pub const PROVIDER: &str = "claude-code";

/// Where the `claude` CLI is: `SERPIS_CLAUDE_CODE_PATH` (empty: nowhere, Claude Code is
/// off), else `claude` on PATH or where its installers put it, once it has been set up for
/// this user (its config directory exists; a throwaway home, as in tests, never starts it).
pub fn find_binary() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("SERPIS_CLAUDE_CODE_PATH") {
        return Some(PathBuf::from(p)).filter(|p| !p.as_os_str().is_empty() && p.is_file());
    }
    let config = std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from).or_else(|| dirs::home_dir().map(|h| h.join(".claude")));
    if !config.is_some_and(|c| c.is_dir()) {
        return None;
    }
    let on_path = std::env::var_os("PATH").into_iter().flat_map(|p| std::env::split_paths(&p).collect::<Vec<_>>()).map(|d| d.join("claude"));
    let home = dirs::home_dir().unwrap_or_default();
    let installed = [home.join(".local/bin/claude"), home.join(".claude/local/claude")];
    on_path.chain(installed).find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// The models offered before the CLI was asked for its own list: its aliases, which follow
/// the latest version of each family.
pub fn fallback_models() -> Vec<Model> {
    [("opus", "Opus"), ("sonnet", "Sonnet"), ("haiku", "Haiku")]
        .iter()
        .map(|(id, name)| model(id, name, &["low", "medium", "high", "xhigh", "max"], false))
        .collect()
}

/// A Claude Code model: `id` is what `--model` takes (an alias or a full model name).
/// Thinking levels map onto `--effort`; `off` only where the model can think not at all.
pub fn model(id: &str, name: &str, efforts: &[&str], thinking_off: bool) -> Model {
    let mut map = serde_json::Map::new();
    map.insert("off".into(), if thinking_off { json!("off") } else { Value::Null });
    map.insert("minimal".into(), Value::Null);
    for level in ["low", "medium", "high", "xhigh", "max", "ultra"] {
        map.insert(level.into(), if efforts.contains(&level) { json!(level) } else { Value::Null });
    }
    // Claude Code reports a 1M window for its 5.x models; older ones have 200K.
    let window = if id.contains("-4-") || id.contains("-3-") { 200_000 } else { 1_000_000 };
    let raw = json!({
        "id": id,
        "name": format!("Claude Code · {name}"),
        "api": PROVIDER,
        "provider": PROVIDER,
        "reasoning": !efforts.is_empty(),
        "thinkingLevelMap": map,
        "input": ["text", "image"],
        "contextWindow": window,
        "maxTokens": 128_000,
    });
    serde_json::from_value(raw).expect("a valid model")
}

/// A model from an entry of the CLI's `initialize` answer (`{value, displayName,
/// supportedEffortLevels, supportsThinkingOff, ...}`). `default` is left out: it is one of
/// the others under another name.
pub fn model_from_init(m: &Value) -> Option<Model> {
    let id = m["value"].as_str().filter(|v| *v != "default")?;
    let name = m["displayName"].as_str().unwrap_or(id);
    let efforts: Vec<&str> = m["supportedEffortLevels"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
    Some(model(id, name, &efforts, m["supportsThinkingOff"].as_bool().unwrap_or(false)))
}

/// What `claude` says about itself when started: its models, and the account it is logged in
/// with (`{email, subscriptionType, apiProvider, ...}`).
pub struct Discovered {
    pub models: Vec<Model>,
    pub account: Map<String, Value>,
}

/// Start `claude`, ask it for its models and account (the `initialize` handshake, no prompt:
/// nothing is sent to a model), and stop it.
pub async fn discover(binary: &Path) -> Result<Discovered> {
    let r = control(binary, None).await?;
    let models = r["models"].as_array().map(|a| a.iter().filter_map(model_from_init).collect()).unwrap_or_default();
    let account = r["account"].as_object().cloned().unwrap_or_default();
    Ok(Discovered { models, account })
}

/// Start `claude`, ask it for its `/usage` (the `get_usage` request: the subscription, its
/// rate-limit windows, what extra usage it has, and, with `advice`, what the last week's
/// transcripts say spends it), and stop it. Nothing is sent to a model. The answer is the
/// CLI's own (`{subscription_type, rate_limits_available, rate_limits: {five_hour,
/// seven_day, ...}, behaviors}`); its shape is experimental there and may change.
pub async fn usage(binary: &Path, advice: bool) -> Result<Value> {
    control(binary, Some(json!({"subtype": "get_usage", "skip_behaviors": !advice}))).await
}

/// The `initialize` handshake with a fresh `claude`, then `request` if any: the answer to the
/// last one sent.
async fn control(binary: &Path, request: Option<Value>) -> Result<Value> {
    let mut child = tokio::process::Command::new(binary)
        .args(["-p", "--input-format", "stream-json", "--output-format", "stream-json", "--verbose", "--no-session-persistence"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {}", binary.display()))?;
    let mut stdin = child.stdin.take().context("no stdin")?;
    let mut lines = BufReader::new(child.stdout.take().context("no stdout")?).lines();
    let mut sent = vec![json!({"subtype": "initialize"})];
    sent.extend(request);
    let last = format!("serpis-{}", sent.len());
    for (i, r) in sent.iter().enumerate() {
        let line = json!({"type": "control_request", "request_id": format!("serpis-{}", i + 1), "request": r});
        stdin.write_all(format!("{line}\n").as_bytes()).await?;
    }
    let answer = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(line) = lines.next_line().await? {
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if v["type"] == "control_response" && v.pointer("/response/request_id").and_then(Value::as_str) == Some(last.as_str()) {
                return Ok(v);
            }
        }
        bail!("claude exited before answering")
    })
    .await
    .context("claude didn't answer in a minute")??;
    let _ = child.kill().await;
    if answer.pointer("/response/subtype").and_then(Value::as_str) == Some("error") {
        bail!("{}", answer.pointer("/response/error").and_then(Value::as_str).unwrap_or("claude refused"));
    }
    Ok(answer.pointer("/response/response").cloned().unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ThinkingLevel, clamp_thinking_level, supported_thinking_levels};

    #[test]
    fn thinking_levels_follow_the_models_efforts() {
        let m = model_from_init(&json!({"value": "claude-opus-4-6", "displayName": "Opus 4.6", "supportedEffortLevels": ["low", "medium", "high", "max"], "supportsThinkingOff": true})).unwrap();
        assert_eq!(m.provider, PROVIDER);
        assert_eq!(m.context_window, 200_000);
        assert_eq!(supported_thinking_levels(&m), vec![ThinkingLevel::Off, ThinkingLevel::Low, ThinkingLevel::Medium, ThinkingLevel::High, ThinkingLevel::Max]);
        let opus = &fallback_models()[0];
        assert_eq!(clamp_thinking_level(opus, ThinkingLevel::Off), ThinkingLevel::Low);
        assert_eq!(clamp_thinking_level(opus, ThinkingLevel::Xhigh), ThinkingLevel::Xhigh);
        assert!(model_from_init(&json!({"value": "default"})).is_none());
    }
}
