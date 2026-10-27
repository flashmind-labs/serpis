//! Chats on Claude Code, against a stand-in `claude` (serpis-testkit's serpis-fake-claude): what
//! the transcript shows, what `claude` is started with and sent, permission prompts, stopping,
//! resuming its session, and joining a conversation another model started.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::{Value, json};
use serpis_agent::ToolGate;
use serpis_ai::{AssistantContent, ThinkingLevel};
use serpis_chat::{Chat, ChatInit, Host, Level};
use serpis_proto::Entry;

struct TestHost {
    vars: HashMap<String, Value>,
    registry: serpis_ai::Registry,
    rt: tokio::runtime::Handle,
    notices: Vec<String>,
    hooks: Option<Arc<dyn serpis_agent::ToolHooks>>,
}

impl Host for TestHost {
    fn var(&self, name: &str) -> Option<Value> {
        self.vars.get(name).cloned()
    }
    fn hook(&mut self, _: &Chat, _: &str, _: &Value) -> Vec<Value> {
        Vec::new()
    }
    fn notify(&mut self, _: Level, text: String) {
        self.notices.push(text);
    }
    fn tools(&self) -> Vec<serpis_agent::ToolRef> {
        serpis_agent::builtin_tools()
    }
    fn tool_hooks(&self) -> Option<Arc<dyn serpis_agent::ToolHooks>> {
        self.hooks.clone()
    }
    fn registry(&self) -> &serpis_ai::Registry {
        &self.registry
    }
    fn runtime(&self) -> tokio::runtime::Handle {
        self.rt.clone()
    }
    fn chat_ref(&self) -> Value {
        json!("test")
    }
}

/// A `before-tool-call` handler refusing edits of `notes.txt`.
struct NoNotes;

impl serpis_agent::ToolHooks for NoNotes {
    fn before_tool_call(&self, name: &str, args: Value) -> futures::future::BoxFuture<'static, ToolGate> {
        let blocked = name == "edit" && args["path"] == "notes.txt";
        Box::pin(async move { if blocked { ToolGate::Block("notes.txt is off limits".into()) } else { ToolGate::Allow(args) } })
    }
}

struct World {
    home: PathBuf,
    work: PathBuf,
    log: PathBuf,
    host: TestHost,
    woken: mpsc::Receiver<()>,
    wake: serpis_chat::Wake,
    _rt: tokio::runtime::Runtime,
}

impl World {
    fn chat(&self) -> Chat {
        let sonnet = serpis_ai::claude_code::fallback_models().into_iter().find(|m| m.id == "sonnet").unwrap();
        Chat::new(ChatInit { cwd: self.work.clone(), model: Some(sonnet), thinking: ThinkingLevel::High }, self.wake.clone())
    }

    fn send(&mut self, chat: &mut Chat, text: &str) {
        chat.submit(&mut self.host, text.into(), vec![], false, None).unwrap();
        self.finish(chat);
    }

    fn finish(&mut self, chat: &mut Chat) {
        for _ in 0..400 {
            let _ = self.woken.recv_timeout(Duration::from_millis(25));
            chat.poll(&mut self.host);
            if !chat.running() {
                return;
            }
        }
        panic!("the turn didn't end: {:#?}", chat.entries());
    }

    /// What the stand-in logged: its starts' arguments, and the lines it was sent.
    fn log(&self) -> (Vec<Vec<String>>, Vec<Value>) {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        let lines: Vec<Value> = text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect();
        let starts = lines.iter().filter_map(|l| l["start"].as_array()).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).collect();
        let got = lines.iter().filter_map(|l| l.get("got").cloned()).collect();
        (starts, got)
    }
}

fn after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter().position(|a| a == flag).and_then(|i| args.get(i + 1)).map(String::as_str)
}

fn last_text(entries: &[Entry]) -> String {
    entries
        .iter()
        .rev()
        .find_map(|e| match e {
            Entry::Assistant { blocks, .. } => blocks.iter().rev().find_map(|b| if let AssistantContent::Text { text, .. } = b { Some(text.clone()) } else { None }),
            _ => None,
        })
        .unwrap_or_default()
}

fn setup() -> World {
    let home = std::env::temp_dir().join(format!("serpis-cc-{}", uuid::Uuid::now_v7()));
    let (work, agent, sessions) = (home.join("project"), home.join("agent"), home.join("claude-sessions"));
    for d in [&work, &agent, &sessions] {
        std::fs::create_dir_all(d).unwrap();
    }
    let log = home.join("claude.log");
    // SAFETY: set once, before any thread of ours starts; the only test in this binary.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("SERPIS_CLAUDE_CODE_PATH", serpis_testkit::fake_claude());
        std::env::set_var("FAKE_CLAUDE_LOG", &log);
        std::env::set_var("FAKE_CLAUDE_DIR", &sessions);
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let host = TestHost {
        vars: HashMap::from([("session-directory".to_string(), json!(home.join("sessions")))]),
        registry: serpis_ai::Registry::new(serpis_ai::Paths { agent_dir: agent }),
        rt: rt.handle().clone(),
        notices: Vec::new(),
        hooks: None,
    };
    let (tx, woken) = mpsc::channel();
    let tx = Arc::new(std::sync::Mutex::new(tx));
    let wake: serpis_chat::Wake = Arc::new(move || {
        let _ = tx.lock().unwrap().send(());
    });
    World { home, work, log, host, woken, wake, _rt: rt }
}

#[test]
fn chats_run_on_claude_code() {
    let mut w = setup();
    assert!(w.host.registry.models().iter().any(|m| m.provider == "claude-code" && m.id == "sonnet"), "Claude Code models are offered once claude is found");

    // A turn: Claude Code's thinking and text, its cost, under the chat's own session id.
    let mut chat = w.chat();
    w.send(&mut chat, "hello");
    let e = chat.entries().to_vec();
    assert!(matches!(&e[0], Entry::User { text, .. } if text == "hello"), "{e:#?}");
    let Entry::Assistant { blocks, usage: Some(u), error: None, streaming: false, .. } = &e[1] else { panic!("{e:#?}") };
    assert!(matches!(&blocks[0], AssistantContent::Thinking { thinking, .. } if thinking == "hmm"), "{blocks:?}");
    assert_eq!(last_text(&e), "echo: hello");
    assert_eq!((u.input, u.cache_read, u.output, u.cost.total), (10, 100, 7, 0.01));
    let (starts, _) = w.log();
    assert_eq!(starts.len(), 1);
    assert_eq!(after(&starts[0], "--session-id"), Some(chat.session_id.as_str()));
    assert_eq!(after(&starts[0], "--model"), Some("sonnet"));
    assert_eq!(after(&starts[0], "--effort"), Some("high"));
    assert_eq!(after(&starts[0], "--permission-mode"), Some("default"));

    // The next turn goes to the same process.
    w.send(&mut chat, "again");
    assert_eq!(last_text(chat.entries()), "echo: again");
    assert_eq!(w.log().0.len(), 1, "one claude process per chat");

    // An edit: asked permission for (allowed by default), shown as serpis's edit with its diff.
    w.send(&mut chat, "please edit");
    let tool = chat.entries().iter().find_map(|e| if let Entry::Tool { name, args, result, .. } = e { Some((name.clone(), args.clone(), result.clone())) } else { None }).expect("a tool entry");
    assert_eq!(tool.0, "edit");
    assert_eq!(tool.1["path"], "notes.txt");
    let r = tool.2.expect("its result");
    assert!(!r.is_error);
    assert_eq!(r.details["diff"], " 1 one\n-2 two\n+2 TWO\n 3 three");
    assert_eq!(last_text(chat.entries()), "Edited.");
    let (_, got) = w.log();
    let answer = got.iter().rev().find(|g| g["type"] == "control_response").unwrap();
    assert_eq!(answer["response"]["response"]["updatedInput"]["file_path"], "notes.txt", "arguments go back under Claude Code's names");

    // A before-tool-call handler refuses it; so does claude-code-permission-prompts = deny.
    w.host.hooks = Some(Arc::new(NoNotes));
    w.send(&mut chat, "edit again");
    let refused = |chat: &Chat| {
        chat.entries().iter().rev().find_map(|e| if let Entry::Tool { result: Some(r), .. } = e { Some((r.is_error, serde_json::to_string(&r.content).unwrap())) } else { None }).unwrap()
    };
    assert!(matches!(refused(&chat), (true, t) if t.contains("off limits")));
    w.host.hooks = None;
    w.host.vars.insert("claude-code-permission-prompts".into(), json!("deny"));
    w.send(&mut chat, "edit once more");
    assert!(matches!(refused(&chat), (true, t) if t.contains("claude-code-permission-prompts")));
    w.host.vars.remove("claude-code-permission-prompts");

    // Stopping interrupts it; the reply cut short says so, and the process carries on.
    chat.submit(&mut w.host, "slow please".into(), vec![], false, None).unwrap();
    for _ in 0..200 {
        let _ = w.woken.recv_timeout(Duration::from_millis(10));
        chat.poll(&mut w.host);
        if last_text(chat.entries()).contains("word word") {
            break;
        }
    }
    chat.abort();
    w.finish(&mut chat);
    assert!(matches!(chat.entries().iter().rev().find(|e| matches!(e, Entry::Assistant { .. })), Some(Entry::Assistant { error: Some(_), streaming: false, .. })), "{:#?}", chat.entries());
    w.send(&mut chat, "after");
    assert_eq!(last_text(chat.entries()), "echo: after");
    assert_eq!(w.log().0.len(), 1, "an interrupt keeps the process");

    // Changing the thinking level restarts it with the new effort, resuming.
    chat.set_thinking(ThinkingLevel::Max);
    w.send(&mut chat, "deeper");
    let (starts, _) = w.log();
    assert_eq!(starts.len(), 2);
    assert_eq!((after(&starts[1], "--effort"), after(&starts[1], "--resume")), (Some("max"), Some(chat.session_id.as_str())));

    // An API error ends the reply as an error.
    w.send(&mut chat, "fail now");
    assert!(matches!(chat.entries().iter().rev().find(|e| matches!(e, Entry::Assistant { .. })), Some(Entry::Assistant { error: Some(e), .. }) if e.contains("overloaded")));

    // Manual compaction is Claude Code's /compact.
    chat.compact(&mut w.host, serpis_chat::CompactReason::Manual, Some("keep the plan".into()));
    w.finish(&mut chat);
    assert!(w.log().1.iter().any(|g| g["message"]["content"][0]["text"] == "/compact keep the plan"));

    // Restored from its session file, the chat resumes Claude Code's session.
    let path = chat.session.as_ref().map(|s| s.path.clone()).unwrap();
    let id = chat.session_id.clone();
    drop(chat);
    let mut again = w.chat();
    again.restore(&mut w.host, &serpis_chat::session::load(&path).unwrap());
    assert_eq!(again.model.as_ref().map(|m| m.id.as_str()), Some("sonnet"));
    w.send(&mut again, "back");
    let (starts, got) = w.log();
    assert_eq!(after(starts.last().unwrap(), "--resume"), Some(id.as_str()));
    assert_eq!(got.last().unwrap()["message"]["content"].as_array().unwrap().len(), 1, "nothing to catch up on");

    // On a machine without that session (the chat moved), a new one is told the conversation.
    drop(again);
    std::fs::remove_file(w.home.join("claude-sessions").join(&id)).unwrap();
    let mut moved = w.chat();
    moved.restore(&mut w.host, &serpis_chat::session::load(&path).unwrap());
    w.send(&mut moved, "where were we");
    assert_eq!(last_text(moved.entries()), "echo: where were we");
    let (starts, got) = w.log();
    assert!(after(starts.last().unwrap(), "--session-id").is_some());
    let first = got.iter().rev().find(|g| g["type"] == "user").unwrap()["message"]["content"][0]["text"].as_str().unwrap().to_string();
    assert!(first.contains("<conversation>") && first.contains("echo: back"), "{first}");

    assert!(w.host.notices.is_empty(), "{:?}", w.host.notices);
    let _ = std::fs::remove_dir_all(&w.home);
}

