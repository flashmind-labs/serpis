//! Chats on Codex, against serpis-testkit's stand-in `codex app-server`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::{Value, json};
use serpis_ai::{AssistantContent, ThinkingLevel};
use serpis_chat::{Chat, ChatInit, Host, Level};
use serpis_proto::{Decision, Entry};

struct TestHost {
    vars: HashMap<String, Value>,
    registry: serpis_ai::Registry,
    runtime: tokio::runtime::Handle,
    notices: Vec<String>,
}

impl Host for TestHost {
    fn var(&self, name: &str) -> Option<Value> {
        self.vars.get(name).cloned()
    }
    fn hook(&mut self, _: &Chat, _: &str, _: &Value) -> Vec<Value> {
        Vec::new()
    }
    fn notify(&mut self, _: Level, text: String) {
        self.notices.push(text)
    }
    fn tools(&self) -> Vec<serpis_agent::ToolRef> {
        serpis_agent::builtin_tools()
    }
    fn tool_hooks(&self) -> Option<Arc<dyn serpis_agent::ToolHooks>> {
        None
    }
    fn registry(&self) -> &serpis_ai::Registry {
        &self.registry
    }
    fn runtime(&self) -> tokio::runtime::Handle {
        self.runtime.clone()
    }
    fn chat_ref(&self) -> Value {
        json!("test")
    }
}

struct World {
    host: TestHost,
    wake: serpis_chat::Wake,
    woken: mpsc::Receiver<()>,
    work: PathBuf,
    _runtime: tokio::runtime::Runtime,
}

impl World {
    fn chat(&self) -> Chat {
        let model = serpis_ai::codex::fallback_models().remove(0);
        Chat::new(
            ChatInit {
                cwd: self.work.clone(),
                model: Some(model),
                thinking: ThinkingLevel::High,
            },
            self.wake.clone(),
        )
    }

    fn finish(&mut self, chat: &mut Chat) {
        for _ in 0..400 {
            let _ = self.woken.recv_timeout(Duration::from_millis(25));
            chat.poll(&mut self.host);
            if !chat.running() {
                return;
            }
        }
        panic!("Codex turn did not finish: {:#?}", chat.entries());
    }

    fn wait_for_ask(&mut self, chat: &mut Chat) -> String {
        for _ in 0..400 {
            let _ = self.woken.recv_timeout(Duration::from_millis(25));
            chat.poll(&mut self.host);
            if let Some(id) = chat.asking().first() {
                return id.clone();
            }
        }
        panic!("Codex did not ask: {:#?}", chat.entries());
    }
}

fn setup() -> World {
    let home = std::env::temp_dir().join(format!("serpis-codex-{}", uuid::Uuid::now_v7()));
    let work = home.join("project");
    let agent = home.join("agent");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&agent).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("SERPIS_CODEX_PATH", serpis_testkit::fake_codex());
    }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let host = TestHost {
        vars: HashMap::from([
            ("session-directory".into(), json!(home.join("sessions"))),
            ("codex-permission-prompts".into(), json!("ask")),
            ("codex-approval-policy".into(), json!("on-request")),
        ]),
        registry: serpis_ai::Registry::new(serpis_ai::Paths { agent_dir: agent }),
        runtime: runtime.handle().clone(),
        notices: Vec::new(),
    };
    let (tx, woken) = mpsc::channel();
    let tx = Arc::new(std::sync::Mutex::new(tx));
    let wake = Arc::new(move || {
        let _ = tx.lock().unwrap().send(());
    });
    World {
        host,
        wake,
        woken,
        work,
        _runtime: runtime,
    }
}

fn last_text(chat: &Chat) -> String {
    chat.entries()
        .iter()
        .rev()
        .find_map(|entry| match entry {
            Entry::Assistant { blocks, .. } => blocks.iter().rev().find_map(|block| match block {
                AssistantContent::Text { text, .. } => Some(text.clone()),
                _ => None,
            }),
            _ => None,
        })
        .unwrap_or_default()
}

#[test]
fn chats_run_on_codex_and_answer_approvals() {
    let mut world = setup();
    assert!(
        world
            .host
            .registry
            .models()
            .iter()
            .any(|model| model.provider == "codex")
    );
    let mut chat = world.chat();

    chat.submit(&mut world.host, "hello".into(), vec![], false, None)
        .unwrap();
    world.finish(&mut chat);
    assert!(matches!(&chat.entries()[0], Entry::User { text, .. } if text == "hello"));
    assert_eq!(last_text(&chat), "echo: hello");
    assert!(chat.entries().iter().any(|entry| matches!(entry, Entry::Assistant { blocks, .. } if blocks.iter().any(|block| matches!(block, AssistantContent::Thinking { thinking, .. } if thinking == "brief thought")))));
    assert_eq!(chat.context(), (Some(19), 200_000));

    chat.submit(&mut world.host, "run a command".into(), vec![], false, None)
        .unwrap();
    let id = world.wait_for_ask(&mut chat);
    assert_eq!(id, "cmd-1");
    assert!(chat.answer(&id, Decision::Always));
    world.finish(&mut chat);
    let tool = serpis_proto::find_tool(chat.entries(), "cmd-1").unwrap();
    assert!(
        matches!(tool, Entry::Tool { name, result: Some(result), .. } if name == "bash" && !result.is_error && result.content.iter().any(|part| matches!(part, serpis_ai::UserContent::Text { text } if text == "ok\n")))
    );
    assert_eq!(last_text(&chat), "Command finished.");
}
