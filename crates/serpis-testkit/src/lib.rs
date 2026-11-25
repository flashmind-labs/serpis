//! Test support for serpis' prompt caching: a strict model of provider caches ([`sim`]) and a
//! local HTTP provider that answers with it ([`mock`]). Never talks to a real provider.

pub mod mock;
pub mod sim;
pub mod sshd;

pub use mock::{LogEntry, Mock, Reply, Script, commands};
pub use sim::{Config, Miss, Outcome, Simulator, Wire};

/// The running test's profile directory (target/<profile>): the one just under the target
/// directory (which holds `CACHEDIR.TAG`), wherever below it cargo put the test (`deps/<test>`,
/// or `build/<crate>/<hash>/out/<test>` since cargo 1.100).
fn profile_dir() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable");
    exe.ancestors().zip(exe.ancestors().skip(1)).find(|(_, up)| up.join("CACHEDIR.TAG").is_file()).map(|(d, _)| d.to_path_buf()).expect("target directory")
}

/// The stand-in `claude` CLI (`src/bin/serpis-fake-claude.rs`), built if needed, beside the
/// running test's own build.
pub fn fake_claude() -> std::path::PathBuf {
    static BUILT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let dir = profile_dir();
            let release = dir.file_name().is_some_and(|n| n == "release");
            let mut cargo = std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
            cargo.args(["build", "-q", "-p", "serpis-testkit", "--bin", "serpis-fake-claude"]);
            if release {
                cargo.arg("--release");
            }
            if let Some(target) = dir.parent() {
                cargo.env("CARGO_TARGET_DIR", target);
            }
            assert!(cargo.status().expect("running cargo").success(), "building serpis-fake-claude");
            dir.join("serpis-fake-claude")
        })
        .clone()
}

/// The stand-in `codex app-server` used by Codex integration tests.
pub fn fake_codex() -> std::path::PathBuf {
    static BUILT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let dir = profile_dir();
            let release = dir.file_name().is_some_and(|n| n == "release");
            let mut cargo = std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
            cargo.args(["build", "-q", "-p", "serpis-testkit", "--bin", "serpis-fake-codex"]);
            if release {
                cargo.arg("--release");
            }
            if let Some(target) = dir.parent() {
                cargo.env("CARGO_TARGET_DIR", target);
            }
            assert!(cargo.status().expect("running cargo").success(), "building serpis-fake-codex");
            dir.join("serpis-fake-codex")
        })
        .clone()
}

/// The stand-in `agy` CLI (`agy acp` and `agy -p /usage`) used by Antigravity integration tests.
pub fn fake_agy() -> std::path::PathBuf {
    static BUILT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let dir = profile_dir();
            let release = dir.file_name().is_some_and(|n| n == "release");
            let mut cargo = std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
            cargo.args(["build", "-q", "-p", "serpis-testkit", "--bin", "serpis-fake-agy"]);
            if release {
                cargo.arg("--release");
            }
            if let Some(target) = dir.parent() {
                cargo.env("CARGO_TARGET_DIR", target);
            }
            assert!(cargo.status().expect("running cargo").success(), "building serpis-fake-agy");
            dir.join("serpis-fake-agy")
        })
        .clone()
}

/// `serpis-node` (with its `node` feature), built if needed, beside the running test's own
/// build: what serves a challenger's tools over MCP (`serpis-node mcp-challenge`).
pub fn serpis_node() -> std::path::PathBuf {
    static BUILT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let dir = profile_dir();
            let release = dir.file_name().is_some_and(|n| n == "release");
            let mut cargo = std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
            cargo.args(["build", "-q", "-p", "serpis-node", "--features", "node", "--bin", "serpis-node"]);
            if release {
                cargo.arg("--release");
            }
            if let Some(target) = dir.parent() {
                cargo.env("CARGO_TARGET_DIR", target);
            }
            assert!(cargo.status().expect("running cargo").success(), "building serpis-node");
            dir.join("serpis-node")
        })
        .clone()
}

/// Call tool `tool` of the MCP server run by `command args` (stdio, with `env`), as the
/// stand-in harnesses do: its `tools/call` result (`{content, isError}`), or the error it gave.
pub fn mcp_call(command: &str, args: &[String], env: &[(String, String)], tool: &str, arguments: &serde_json::Value) -> serde_json::Value {
    use std::io::{BufRead, Write};
    use serde_json::json;
    let mut child = std::process::Command::new(command)
        .args(args)
        .envs(env.iter().cloned())
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("starting the MCP server");
    let mut input = child.stdin.take().unwrap();
    let mut output = std::io::BufReader::new(child.stdout.take().unwrap());
    let mut ask = |msg: serde_json::Value| -> serde_json::Value {
        writeln!(input, "{msg}").unwrap();
        input.flush().unwrap();
        if msg.get("id").is_none() {
            return serde_json::Value::Null;
        }
        let mut line = String::new();
        output.read_line(&mut line).unwrap();
        serde_json::from_str(&line).unwrap_or_default()
    };
    ask(json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "stand-in", "version": "0"}}}));
    ask(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}));
    let reply = ask(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": tool, "arguments": arguments}}));
    drop(ask);
    drop(input);
    let _ = child.wait();
    reply.get("result").cloned().unwrap_or_else(|| json!({"content": [{"type": "text", "text": reply["error"]["message"]}], "isError": true}))
}
