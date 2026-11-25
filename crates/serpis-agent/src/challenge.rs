//! A challenger's tools: how an agent challenging another's report (skills/challenge) says
//! what it found. `challenge_finding` records a claim it broke, `challenge_held` one that held,
//! `challenge_proof` its answer when asked to prove a finding. Each checks its arguments against
//! the report (the claim exists, the quote is the report's own words) and answers the model,
//! which fixes a bad call itself; serpis reads the calls that succeeded from the chat's
//! transcript. A chat gets them when its session has a [`ENTRY`] entry holding the report: as
//! tools of serpis's own agent, and through [`serve_mcp_stdio`] for the CLI harnesses.

use std::io::{BufRead, Write};
use std::sync::Arc;

use futures::FutureExt;
use serde_json::{Value, json};

use crate::tools::{AgentTool, BoxFutureResult, ToolContext, ToolRef, ToolResult};

/// The session entry that makes a chat a challenger: `{report}`, the report it challenges.
pub const ENTRY: &str = "serpis-challenge";
pub const FINDING: &str = "challenge_finding";
pub const HELD: &str = "challenge_held";
pub const PROOF: &str = "challenge_proof";
pub const NAMES: [&str; 3] = [FINDING, HELD, PROOF];
/// What the harnesses call the MCP server holding them (`mcp__serpis__challenge_finding`).
pub const MCP_SERVER: &str = "serpis";
/// Where `serpis-node mcp-challenge` finds the report.
pub const REPORT_ENV: &str = "SERPIS_CHALLENGE_REPORT";

/// The report a chat challenges, from its custom session entries (the last [`ENTRY`]).
pub fn report_of<'a>(entries: impl DoubleEndedIterator<Item = &'a (String, Value)>) -> Option<String> {
    entries.rev().find(|(k, _)| k == ENTRY).and_then(|(_, d)| d["report"].as_str()).map(str::to_string)
}

/// Which challenge tool a transcript's tool name is, however its harness spells it:
/// `challenge_finding`, `mcp__serpis__challenge_finding`, `serpis.challenge_finding`,
/// `serpis/challenge_finding (MCP)`: the name as a word of it.
pub fn tool_of(name: &str) -> Option<&'static str> {
    let word = |c: char| c.is_ascii_alphanumeric();
    NAMES.into_iter().find(|t| {
        name.match_indices(t).any(|(at, _)| !name[..at].ends_with(word) && !name[at + t.len()..].starts_with(word) && !name[at + t.len()..].starts_with('_'))
    })
}

/// A transcript's tool call as a challenge tool's: which, and its arguments. A harness that
/// calls MCP tools through a tool of its own names the tool and wraps the arguments in its
/// input (`{"tool_name": .., "arguments": {..}}`); both are looked for.
pub fn call_of(name: &str, args: &Value) -> Option<(&'static str, Value)> {
    let named = ["tool_name", "toolName", "tool", "name"].iter().find_map(|k| args.get(*k).and_then(Value::as_str).and_then(tool_of));
    let tool = tool_of(name).or(named)?;
    let inner = ["arguments", "args", "input", "params"].iter().find_map(|k| args.get(*k).filter(|v| v.is_object()));
    let args = match inner {
        Some(a) if args.get("claim").is_none() && args.get("outcome").is_none() => a.clone(),
        _ => args.clone(),
    };
    Some((tool, args))
}

/// The three tools, for the report `report`.
pub fn tools(report: &str) -> Vec<ToolRef> {
    let report: Arc<str> = Arc::from(report);
    NAMES.into_iter().map(|name| Arc::new(ChallengeTool { name, report: report.clone() }) as ToolRef).collect()
}

fn description(name: &str) -> &'static str {
    match name {
        FINDING => "Record a claim of the report you are challenging that you broke: false, or not backed by what its agent did. One call per finding, as soon as you have it; serpis places it on the report, under the words you quote, for the person to review. The answer says if it was recorded or what to fix.",
        HELD => "Record a claim of the report you are challenging that you checked and could not break.",
        PROOF => "Answer a request to prove one of your findings: confirmed, with what you ran and what it showed, or withdrawn, with why it doesn't hold.",
        _ => "",
    }
}

fn parameters(name: &str) -> Value {
    let claim = json!({"type": "string", "description": "The claim: `Headline` (the report's first sentence), or its section and item number, e.g. `Changed 2` (the second item under ## Changed), `Checked 1`."});
    match name {
        FINDING => json!({
            "type": "object",
            "properties": {
                "claim": claim,
                "status": {"type": "string", "enum": ["confirmed", "plausible"], "description": "confirmed: you ran something that shows it. plausible: you only read the code."},
                "quote": {"type": "string", "description": "The exact words of the claim you dispute, copied from the report: one sentence, not the whole item."},
                "finding": {"type": "string", "description": "What is wrong, in two sentences at most, with a concrete way it goes wrong."},
                "evidence": {"type": "string", "description": "What you ran and what it showed, or what you read (path:line). One line."}
            },
            "required": ["claim", "status", "quote", "finding", "evidence"]
        }),
        HELD => json!({
            "type": "object",
            "properties": {
                "claim": claim,
                "note": {"type": "string", "description": "How you checked it, in one line."}
            },
            "required": ["claim", "note"]
        }),
        _ => json!({
            "type": "object",
            "properties": {
                "outcome": {"type": "string", "enum": ["confirmed", "withdrawn"]},
                "evidence": {"type": "string", "description": "confirmed: what you ran and what it showed. withdrawn: why the finding doesn't hold."}
            },
            "required": ["outcome", "evidence"]
        }),
    }
}

/// Text as compared for quotes: markdown's inline marks out, whitespace as single spaces.
fn squash(s: &str) -> String {
    s.replace("**", "").replace('`', "").split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The claims of `report` a call may name: `Headline`, and each item of each `##` section
/// (`Changed 1`, `Checked 2`), in order.
pub fn claims(report: &str) -> Vec<String> {
    let mut out = vec!["Headline".to_string()];
    let mut section: Option<(String, usize)> = None;
    for line in report.lines() {
        if let Some(h) = line.strip_prefix("## ") {
            section = Some((h.trim().to_string(), 0));
            continue;
        }
        let Some((name, n)) = section.as_mut() else { continue };
        if line.starts_with(' ') || line.starts_with('\t') {
            continue;
        }
        let t = line.trim();
        let digits = t.chars().take_while(char::is_ascii_digit).count();
        let numbered = digits > 0 && (t[digits..].starts_with(". ") || t[digits..].starts_with(") "));
        if numbered || t.starts_with("- ") || t.starts_with("* ") {
            *n += 1;
            out.push(format!("{name} {n}"));
        }
    }
    out
}

/// `claim` as the report names it (case as there), or why it isn't one of its claims.
fn check_claim(report: &str, claim: &str) -> Result<String, String> {
    let all = claims(report);
    let want = claim.split_whitespace().collect::<Vec<_>>().join(" ");
    all.iter().find(|c| c.eq_ignore_ascii_case(&want)).cloned().ok_or_else(|| format!("The report has no claim `{claim}`. Its claims: {}.", all.join(", ")))
}

/// Check one call's arguments against the report: what to answer the model, or what to fix.
pub fn check(name: &str, report: &str, args: &Value) -> Result<String, String> {
    let s = |k: &str| args.get(k).and_then(Value::as_str).map(str::trim).unwrap_or("");
    let need = |k: &str| if s(k).is_empty() { Err(format!("`{k}` is empty.")) } else { Ok(()) };
    match name {
        FINDING => {
            let claim = check_claim(report, s("claim"))?;
            if !matches!(s("status"), "confirmed" | "plausible") {
                return Err("`status` is `confirmed` (you ran something that shows it) or `plausible` (you only read the code).".into());
            }
            for k in ["quote", "finding", "evidence"] {
                need(k)?;
            }
            let quote = squash(s("quote").trim_start_matches('>'));
            if !squash(report).contains(&quote) {
                return Err(format!("The quote isn't in the report. Copy the exact words of {claim} you dispute, one sentence of it."));
            }
            Ok(format!("Recorded: {claim}, {}. It goes on the report as a draft for the person to review.", s("status")))
        }
        HELD => {
            let claim = check_claim(report, s("claim"))?;
            need("note")?;
            Ok(format!("Noted: {claim} held."))
        }
        PROOF => {
            if !matches!(s("outcome"), "confirmed" | "withdrawn") {
                return Err("`outcome` is `confirmed` or `withdrawn`.".into());
            }
            need("evidence")?;
            Ok(format!("Recorded: {}.", s("outcome")))
        }
        _ => Err(format!("No tool {name}")),
    }
}

struct ChallengeTool {
    name: &'static str,
    report: Arc<str>,
}

impl AgentTool for ChallengeTool {
    fn name(&self) -> &str {
        self.name
    }
    fn description(&self) -> String {
        description(self.name).into()
    }
    fn parameters(&self) -> Value {
        parameters(self.name)
    }
    fn prompt_snippet(&self) -> Option<String> {
        Some(description(self.name).split(". ").next().unwrap_or("").trim_end_matches('.').to_string())
    }
    fn execute(&self, _id: String, args: Value, _ctx: ToolContext) -> BoxFutureResult {
        let r = match check(self.name, &self.report, &args) {
            Ok(t) => ToolResult::text(t),
            Err(e) => ToolResult::error(e),
        };
        async move { Ok(r) }.boxed()
    }
}

/// The harnesses' MCP server for the tools is `exe mcp-challenge` (serpis-node's), the report
/// in its environment. Claude Code's `--mcp-config` shape: `{"mcpServers": {"serpis": {...}}}`.
pub fn mcp_config(exe: &std::path::Path, report: &str) -> Value {
    json!({"mcpServers": {MCP_SERVER: {"command": exe.to_string_lossy(), "args": ["mcp-challenge"], "env": {REPORT_ENV: report}}}})
}

/// The same for Codex: `-c` overrides of its config (TOML values), its tools approved without
/// asking.
pub fn codex_args(exe: &std::path::Path, report: &str) -> Vec<String> {
    // A JSON string is a TOML basic string: the same escapes.
    let s = |v: &str| Value::from(v).to_string();
    let key = |k: &str| format!("mcp_servers.{MCP_SERVER}.{k}");
    [
        format!("{}={}", key("command"), s(&exe.to_string_lossy())),
        format!("{}=[\"mcp-challenge\"]", key("args")),
        format!("{}={{{REPORT_ENV}={}}}", key("env"), s(report)),
        format!("{}=\"approve\"", key("default_tools_approval_mode")),
    ]
    .into_iter()
    .flat_map(|kv| ["-c".to_string(), kv])
    .collect()
}

/// The same for an ACP agent (Antigravity): an entry of `session/new`'s `mcpServers`.
pub fn acp_server(exe: &std::path::Path, report: &str) -> Value {
    json!({"name": MCP_SERVER, "command": exe.to_string_lossy(), "args": ["mcp-challenge"], "env": [{"name": REPORT_ENV, "value": report}]})
}

/// Serve the tools over MCP on stdin and stdout (JSON-RPC, one message a line) for the report
/// `report`, until stdin closes.
pub fn serve_mcp_stdio(report: &str) -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut out = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(reply) = mcp_reply(report, &line) {
            writeln!(out, "{reply}")?;
            out.flush()?;
        }
    }
    Ok(())
}

/// The answer to one MCP message (None for a notification).
pub fn mcp_reply(report: &str, line: &str) -> Option<Value> {
    let msg: Value = match serde_json::from_str(line) {
        Ok(m) => m,
        Err(e) => return Some(json!({"jsonrpc": "2.0", "id": null, "error": {"code": -32700, "message": format!("{e}")}})),
    };
    let id = msg.get("id").cloned()?;
    let params = &msg["params"];
    let result = match msg["method"].as_str().unwrap_or("") {
        "initialize" => json!({
            "protocolVersion": params["protocolVersion"].as_str().unwrap_or("2025-06-18"),
            "capabilities": {"tools": {}},
            "serverInfo": {"name": MCP_SERVER, "version": env!("CARGO_PKG_VERSION")},
        }),
        "ping" => json!({}),
        // They only record what they are given: nothing to ask the person about.
        "tools/list" => json!({"tools": NAMES.iter().map(|n| json!({"name": n, "description": description(n), "inputSchema": parameters(n), "annotations": {"readOnlyHint": true, "destructiveHint": false, "openWorldHint": false}})).collect::<Vec<_>>()}),
        "tools/call" => {
            let name = params["name"].as_str().unwrap_or("");
            let (text, is_error) = match check(name, report, &params["arguments"]) {
                Ok(t) => (t, false),
                Err(e) => (e, true),
            };
            json!({"content": [{"type": "text", "text": text}], "isError": is_error})
        }
        m => return Some(json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("no method {m}")}})),
    };
    Some(json!({"jsonrpc": "2.0", "id": id, "result": result}))
}

#[cfg(test)]
mod tests {
    use super::*;

    const REPORT: &str = "Done: retries work. This is the **likely cause** of the flaky test.\n\n## Changed\n\n- Retries back off, `src/queue.rs:147`.\n  More words under it.\n- Jitter adds ±20%.\n\n## Checked\n\n1. **`cargo test`:** passes, two hundred times.\n";

    #[test]
    fn claims_are_the_headline_and_each_item() {
        assert_eq!(claims(REPORT), ["Headline", "Changed 1", "Changed 2", "Checked 1"]);
    }

    #[test]
    fn a_finding_is_checked_against_the_report() {
        let ok = json!({"claim": "changed 2", "status": "confirmed", "quote": "Jitter adds  ±20%.", "finding": "It adds 50%.", "evidence": "ran it"});
        assert_eq!(check(FINDING, REPORT, &ok), Ok("Recorded: Changed 2, confirmed. It goes on the report as a draft for the person to review.".into()));
        // Marks and line breaks don't matter; the words do.
        let marked = json!({"claim": "Headline", "status": "plausible", "quote": "> This is the likely cause\nof the flaky test.", "finding": "x", "evidence": "y"});
        assert!(check(FINDING, REPORT, &marked).is_ok());
        let missing = json!({"claim": "Changed 3", "status": "confirmed", "quote": "x", "finding": "x", "evidence": "y"});
        assert_eq!(check(FINDING, REPORT, &missing), Err("The report has no claim `Changed 3`. Its claims: Headline, Changed 1, Changed 2, Checked 1.".into()));
        let made_up = json!({"claim": "Changed 1", "status": "confirmed", "quote": "Retries never back off.", "finding": "x", "evidence": "y"});
        assert!(check(FINDING, REPORT, &made_up).unwrap_err().starts_with("The quote isn't in the report"));
        let status = json!({"claim": "Changed 1", "status": "sure", "quote": "x", "finding": "x", "evidence": "y"});
        assert!(check(FINDING, REPORT, &status).is_err());
        assert_eq!(check(HELD, REPORT, &json!({"claim": "Checked 1", "note": "reran it"})), Ok("Noted: Checked 1 held.".into()));
        assert!(check(PROOF, REPORT, &json!({"outcome": "withdrawn", "evidence": ""})).is_err());
    }

    #[test]
    fn harness_spellings_are_told_apart() {
        assert_eq!(tool_of("challenge_finding"), Some(FINDING));
        assert_eq!(tool_of("mcp__serpis__challenge_held"), Some(HELD));
        assert_eq!(tool_of("serpis.challenge_proof"), Some(PROOF));
        assert_eq!(tool_of("serpis/challenge_finding (MCP)"), Some(FINDING));
        assert_eq!(tool_of("mcp_serpis_challenge_finding"), Some(FINDING));
        assert_eq!(tool_of("challenge_findings"), None);
        assert_eq!(tool_of("challenge_finding_x"), None);
        assert_eq!(tool_of("read"), None);
        // Through a harness's own tool for MCP calls.
        let wrapped = json!({"server_name": "serpis", "tool_name": "challenge_held", "arguments": {"claim": "Changed 1", "note": "x"}});
        assert_eq!(call_of("call_mcp_tool", &wrapped), Some((HELD, json!({"claim": "Changed 1", "note": "x"}))));
        assert_eq!(call_of("challenge_held", &json!({"claim": "Changed 1"})), Some((HELD, json!({"claim": "Changed 1"}))));
        assert_eq!(call_of("call_mcp_tool", &json!({"tool_name": "other"})), None);
    }

    #[test]
    fn codex_is_given_the_server_as_config() {
        let args = codex_args(std::path::Path::new("/bin/serpis-node"), "Say \"hi\"\nnow");
        assert_eq!(
            args,
            [
                "-c",
                "mcp_servers.serpis.command=\"/bin/serpis-node\"",
                "-c",
                "mcp_servers.serpis.args=[\"mcp-challenge\"]",
                "-c",
                "mcp_servers.serpis.env={SERPIS_CHALLENGE_REPORT=\"Say \\\"hi\\\"\\nnow\"}",
                "-c",
                "mcp_servers.serpis.default_tools_approval_mode=\"approve\"",
            ]
        );
    }

    #[test]
    fn mcp_lists_and_calls_the_tools() {
        let init = mcp_reply(REPORT, r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26"}}"#).unwrap();
        assert_eq!(init["result"]["protocolVersion"], "2025-03-26");
        assert_eq!(mcp_reply(REPORT, r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#), None);
        let list = mcp_reply(REPORT, r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#).unwrap();
        assert_eq!(list["result"]["tools"].as_array().unwrap().len(), 3);
        let call = mcp_reply(REPORT, r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"challenge_held","arguments":{"claim":"Changed 1","note":"read it"}}}"#).unwrap();
        assert_eq!(call["result"], json!({"content": [{"type": "text", "text": "Noted: Changed 1 held."}], "isError": false}));
    }
}
