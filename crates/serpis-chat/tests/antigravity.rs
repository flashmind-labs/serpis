//! Chats on Antigravity, against serpis-testkit's stand-in `agy acp` binary.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::{Value, json};
use serpis_ai::{AssistantContent, ThinkingLevel};
use serpis_chat::{Chat, ChatInit, Host, Level};
use serpis_proto::{Decision, Entry, Tone};

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
        let model = serpis_ai::antigravity::fallback_models().remove(0);
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
        panic!("Antigravity turn did not finish: {:#?}", chat.entries());
    }

    fn wait_for_ask(&mut self, chat: &mut Chat) -> String {
        for _ in 0..400 {
            let _ = self.woken.recv_timeout(Duration::from_millis(25));
            chat.poll(&mut self.host);
            if let Some(id) = chat.asking().first() {
                return id.clone();
            }
        }
        panic!("Antigravity did not ask: {:#?}", chat.entries());
    }
}

fn setup() -> World {
    let home = std::env::temp_dir().join(format!("serpis-agy-{}", uuid::Uuid::now_v7()));
    let work = home.join("project");
    let agent = home.join("agent");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&agent).unwrap();
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("SERPIS_ANTIGRAVITY_PATH", serpis_testkit::fake_agy());
        std::env::set_var("SERPIS_NODE_EXE", serpis_testkit::serpis_node());
    }
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let host = TestHost {
        vars: HashMap::from([
            ("session-directory".into(), json!(home.join("sessions"))),
            ("antigravity-permission-prompts".into(), json!("ask")),
            ("antigravity-mode".into(), json!("default")),
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
fn chats_run_on_antigravity_and_answer_approvals() {
    let mut world = setup();
    assert!(
        world
            .host
            .registry
            .models()
            .iter()
            .any(|model| model.provider == "antigravity")
    );
    let mut chat = world.chat();

    chat.submit(&mut world.host, "hello".into(), vec![], false, None)
        .unwrap();
    world.finish(&mut chat);
    assert!(matches!(&chat.entries()[0], Entry::User { text, .. } if text == "hello"));
    assert_eq!(last_text(&chat), "echo: hello");
    assert!(chat.entries().iter().any(|entry| matches!(entry, Entry::Assistant { blocks, .. } if blocks.iter().any(|block| matches!(block, AssistantContent::Thinking { thinking, .. } if thinking == "brief thought")))));
    assert_eq!(chat.context(), (Some(42), 1_048_576));

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

    // Inline diff rendering on ACP edit tool calls.
    chat.submit(&mut world.host, "edit a file".into(), vec![], false, None)
        .unwrap();
    world.finish(&mut chat);
    let edit = serpis_proto::find_tool(chat.entries(), "edit-1").unwrap();
    assert!(
        matches!(edit, Entry::Tool { name, result: Some(result), .. } if name == "edit" && result.details["diff"].as_str().is_some_and(|d| d.contains("-1 before") && d.contains("+1 after")))
    );

    // Live permission-mode switching on Antigravity chats.
    chat.set_permission_mode(&mut world.host, "accept-edits").unwrap();
    assert_eq!(chat.state().permission_mode.as_deref(), Some("accept-edits"));

    // Auto mode approves permission requests automatically without waiting for ToolAsk.
    chat.set_permission_mode(&mut world.host, "auto").unwrap();
    assert_eq!(chat.state().permission_mode.as_deref(), Some("auto"));
    chat.submit(&mut world.host, "run a command".into(), vec![], false, None)
        .unwrap();
    world.finish(&mut chat);
    assert!(chat.asking().is_empty());
    assert_eq!(last_text(&chat), "Command finished.");

    // Interleaved text -> thinking -> text never drops the trailing report text.
    chat.submit(&mut world.host, "interleaved report".into(), vec![], false, None)
        .unwrap();
    world.finish(&mut chat);
    assert_eq!(last_text(&chat), "Done: final report delivered.");
}

#[test]
fn discovers_models_and_reads_usage_from_fake_agy() {
    let binary = serpis_testkit::fake_agy();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let models = rt.block_on(serpis_ai::antigravity::discover(&binary)).unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].id, "gemini-4-neon");
    assert_eq!(models[1].id, "gemini-3.8-flash");

    let usage = rt.block_on(serpis_ai::antigravity::usage(&binary)).unwrap();
    assert_eq!(usage["groups"][0]["name"], "Gemini Models");
    assert_eq!(usage["groups"][0]["buckets"][0]["window"], "weekly");
}

impl World {
    /// Poll until `done` holds and no turn is running.
    fn until(&mut self, chat: &mut Chat, what: &str, done: impl Fn(&Chat) -> bool) {
        for _ in 0..400 {
            let _ = self.woken.recv_timeout(Duration::from_millis(25));
            chat.poll(&mut self.host);
            if !chat.running() && done(chat) {
                return;
            }
        }
        panic!("{what}: {:#?}", chat.entries());
    }
}

fn tool_cards(chat: &Chat, id: &str) -> usize {
    chat.entries().iter().filter(|e| matches!(e, Entry::Tool { id: i, .. } if i == id)).count()
}

#[test]
fn stdout_noise_does_not_fail_a_turn() {
    let mut world = setup();
    let mut chat = world.chat();
    chat.submit(&mut world.host, "noisy hello".into(), vec![], false, None).unwrap();
    world.finish(&mut chat);
    assert_eq!(chat.last_error, None);
    assert!(
        !chat.entries().iter().any(|e| matches!(e, Entry::Info { text, .. } if text.contains("went on by itself"))),
        "the reply belongs to the turn, not one agy started: {:#?}",
        chat.entries()
    );
    assert_eq!(last_text(&chat), "echo: noisy hello");
}

#[test]
fn asks_between_runs_are_answered_once() {
    // Antigravity asks after its turn ended: on its own (between runs), or in a turn it
    // started by itself.
    for prompt in ["background ask", "background turn ask"] {
        let mut world = setup();
        let mut chat = world.chat();
        chat.submit(&mut world.host, prompt.into(), vec![], false, None).unwrap();
        let id = world.wait_for_ask(&mut chat);
        assert_eq!(id, "bg-1", "{prompt}");
        assert!(chat.answer(&id, Decision::Allow));
        world.until(&mut chat, prompt, |c| last_text(c) == "Command finished.");
        assert_eq!(tool_cards(&chat, "bg-1"), 1, "{prompt}: one card for the tool: {:#?}", chat.entries());
        // Where it went on by itself is marked as a run of its own (a report before it stays one).
        assert!(chat.entries().iter().any(|e| matches!(e, Entry::Info { tone: Tone::OwnTurn, .. })), "{prompt}: {:#?}", chat.entries());
        assert!(
            matches!(serpis_proto::find_tool(chat.entries(), "bg-1"), Some(Entry::Tool { result: Some(r), .. }) if !r.is_error),
            "{prompt}"
        );
    }
}

#[test]
fn clearing_a_chat_with_a_pending_ask_starts_agy_over() {
    let mut world = setup();
    let mut chat = world.chat();
    chat.submit(&mut world.host, "background ask".into(), vec![], false, None).unwrap();
    world.wait_for_ask(&mut chat);
    chat.clear();
    chat.submit(&mut world.host, "hello".into(), vec![], false, None).unwrap();
    world.finish(&mut chat);
    assert_eq!(last_text(&chat), "echo: hello");
}

#[test]
fn modes_restore_only_on_their_harness() {
    let mut world = setup();
    let mut chat = world.chat();
    chat.submit(&mut world.host, "hello".into(), vec![], false, None).unwrap();
    world.finish(&mut chat);
    assert!(chat.set_permission_mode(&mut world.host, "bypassPermissions").is_err());
    assert!(chat.set_permission_mode(&mut world.host, "acceptEdits").is_err(), "Claude Code's spelling");
    chat.set_permission_mode(&mut world.host, "accept-edits").unwrap();
    let path = chat.session.as_ref().map(|s| s.path.clone()).expect("its session");

    let mut back = world.chat();
    back.restore(&mut world.host, &serpis_chat::session::load(&path).unwrap());
    assert_eq!(back.state().permission_mode.as_deref(), Some("accept-edits"));

    // The same mode, last chosen on Claude Code: not Antigravity's.
    let mut loaded = serpis_chat::session::load(&path).unwrap();
    for (k, d) in &mut loaded.entries {
        if k == serpis_chat::CLAUDE_MODE {
            d["harness"] = json!(serpis_ai::claude_code::PROVIDER);
        }
    }
    let mut other = world.chat();
    other.restore(&mut world.host, &loaded);
    assert_eq!(other.state().permission_mode.as_deref(), Some("default"));
}

#[test]
fn a_challenger_gets_its_tools_over_mcp() {
    // Its session's serpis-challenge entry: the challenge tools as an MCP server of its ACP
    // session, a call to them allowed without asking (the person's prompts are "ask"), run on
    // serpis-node mcp-challenge, which checks it against the report. Antigravity calls it
    // through a tool of its own: the call is still told apart by what it wraps.
    let mut world = setup();
    let mut chat = world.chat();
    let report = "Done: it works.\n\n## Changed\n\n- Retries back off.\n";
    chat.append_entry(&mut world.host, serpis_agent::challenge::ENTRY, json!({"report": report}));
    let finding = json!({"claim": "Changed 1", "status": "plausible", "quote": "Retries back off.", "finding": "They don't.", "evidence": "Read it."});
    chat.submit(&mut world.host, format!("challenge {finding}"), vec![], false, None).unwrap();
    world.finish(&mut chat);
    assert!(chat.asking().is_empty());
    let tool = serpis_proto::find_tool(chat.entries(), "mcp-1").unwrap();
    let Entry::Tool { name, args, result: Some(result), .. } = tool else { panic!("{tool:#?}") };
    assert_eq!(serpis_agent::challenge::call_of(name, args), Some((serpis_agent::challenge::FINDING, finding)));
    assert!(!result.is_error && result.content.iter().any(|part| matches!(part, serpis_ai::UserContent::Text { text } if text == "Recorded: Changed 1, plausible. It goes on the report as a draft for the person to review.")), "{result:#?}");
    assert_eq!(last_text(&chat), "Finding completed.");

    // Any other chat's sessions get no MCP server: the stand-in says so when asked for one.
    let mut other = world.chat();
    other.submit(&mut world.host, format!("challenge {}", json!({"claim": "x"})), vec![], false, None).unwrap();
    let id = world.wait_for_ask(&mut other);
    assert!(other.answer(&id, Decision::Allow));
    world.finish(&mut other);
    assert_eq!(last_text(&other), "Finding failed.");
}
