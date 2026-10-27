//! A chat on the real Claude Code, without a window: `cargo run -p serpis-chat --example
//! claude_code_smoke -- <dir> <prompt>...` runs each prompt as a turn in `<dir>` on
//! `claude-code/haiku` (your own claude login; a few cents of usage), then resumes the chat from
//! its session file for one more turn, and prints what the transcript shows.

use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::{Value, json};
use serpis_chat::{Chat, ChatInit, Host, Level};
use serpis_proto::Entry;

struct H {
    registry: serpis_ai::Registry,
    rt: tokio::runtime::Handle,
}

impl Host for H {
    fn var(&self, name: &str) -> Option<Value> {
        (name == "session-directory").then(|| json!(std::env::temp_dir().join("serpis-claude-code-smoke")))
    }
    fn hook(&mut self, _: &Chat, _: &str, _: &Value) -> Vec<Value> {
        Vec::new()
    }
    fn notify(&mut self, level: Level, text: String) {
        eprintln!("{level:?}: {text}");
    }
    fn tools(&self) -> Vec<serpis_agent::ToolRef> {
        Vec::new()
    }
    fn tool_hooks(&self) -> Option<Arc<dyn serpis_agent::ToolHooks>> {
        None
    }
    fn registry(&self) -> &serpis_ai::Registry {
        &self.registry
    }
    fn runtime(&self) -> tokio::runtime::Handle {
        self.rt.clone()
    }
    fn chat_ref(&self) -> Value {
        json!("smoke")
    }
}

fn turn(chat: &mut Chat, host: &mut H, woken: &mpsc::Receiver<()>, text: &str) {
    chat.submit(host, text.into(), vec![], false, None).unwrap();
    while chat.running() {
        let _ = woken.recv_timeout(Duration::from_millis(50));
        chat.poll(host);
    }
}

fn show(entries: &[Entry]) {
    for e in entries {
        match e {
            Entry::User { text, .. } => println!("user: {text}"),
            Entry::Assistant { blocks, usage, error, model, .. } => {
                let text: String = blocks.iter().filter_map(|b| if let serpis_ai::AssistantContent::Text { text, .. } = b { Some(text.as_str()) } else { None }).collect();
                let u = usage.as_ref().map(|u| format!(" [{} in, {} cached, {} out, ${:.4}]", u.input, u.cache_read, u.output, u.cost.total)).unwrap_or_default();
                println!("assistant ({model}): {}{u}{}", text.trim(), error.as_ref().map(|e| format!(" ERROR {e}")).unwrap_or_default());
            }
            Entry::Tool { name, args, result, .. } => {
                let diff = result.as_ref().and_then(|r| r.details["diff"].as_str()).map(|d| format!("\n{d}")).unwrap_or_default();
                println!("tool {name} {} -> {}{diff}", serpis_chat::tool_summary(name, args), result.as_ref().map(|r| if r.is_error { "error" } else { "ok" }).unwrap_or("none"));
            }
            Entry::Info { text, .. } => println!("info: {text}"),
        }
    }
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (dir, prompts) = args.split_first().expect("usage: claude_code_smoke <dir> <prompt>...");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mut host = H { registry: serpis_ai::Registry::new(serpis_ai::Paths::default_paths()), rt: rt.handle().clone() };
    let haiku = host.registry.find("claude-code", "haiku").expect("claude-code/haiku (is claude installed?)");
    let (tx, woken) = mpsc::channel();
    let tx = Arc::new(std::sync::Mutex::new(tx));
    let wake: serpis_chat::Wake = Arc::new(move || {
        let _ = tx.lock().unwrap().send(());
    });
    let mut chat = Chat::new(ChatInit { cwd: dir.into(), model: Some(haiku), thinking: serpis_ai::ThinkingLevel::Low }, wake.clone());
    for p in prompts {
        turn(&mut chat, &mut host, &woken, p);
    }
    let path = chat.session.as_ref().map(|s| s.path.clone()).expect("a session file");
    drop(chat);
    let mut again = Chat::new(ChatInit { cwd: dir.into(), model: None, thinking: serpis_ai::ThinkingLevel::Low }, wake);
    again.restore(&mut host, &serpis_chat::session::load(&path).unwrap());
    turn(&mut again, &mut host, &woken, "What was the first thing I asked you? One sentence.");
    show(again.entries());
}
