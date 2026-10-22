//! A chat with no window, as a node runs it: real agent runs against the mock provider, shown
//! somewhere else only through the splices it hands out, sent as JSON the way a node sends them.

use std::collections::HashMap;
use std::sync::{Arc, mpsc};
use std::time::Duration;

use serde_json::{Value, json};
use serpis_chat::{Chat, ChatInit, Host, Level};
use serpis_proto::{Entry, Splice};

struct TestHost {
    vars: HashMap<String, Value>,
    registry: serpis_ai::Registry,
    rt: tokio::runtime::Handle,
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
        self.notices.push(text);
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
        self.rt.clone()
    }
    fn chat_ref(&self) -> Value {
        json!("test")
    }
}

/// A client's copy of the transcript, kept up to date from JSON splices.
#[derive(Default)]
struct Mirror {
    entries: Vec<Entry>,
    splices: Vec<usize>,
}

impl Mirror {
    fn take(&mut self, chat: &mut Chat) {
        if let Some(s) = chat.take_splice() {
            let wire = serde_json::to_string(&s).unwrap();
            self.splices.push(s.from);
            serde_json::from_str::<Splice>(&wire).unwrap().apply(&mut self.entries);
        }
    }
}

/// Poll the chat whenever it wakes, mirroring it, until its run is over.
fn finish(chat: &mut Chat, host: &mut TestHost, woken: &mpsc::Receiver<()>, mirror: &mut Mirror) {
    loop {
        let _ = woken.recv_timeout(Duration::from_millis(50));
        chat.poll(host);
        mirror.take(chat);
        if !chat.running() {
            return;
        }
    }
}

fn kinds(entries: &[Entry]) -> Vec<&'static str> {
    entries
        .iter()
        .map(|e| match e {
            Entry::User { .. } => "user",
            Entry::Assistant { .. } => "assistant",
            Entry::Tool { .. } => "tool",
            Entry::Info { .. } => "info",
        })
        .collect()
}

#[test]
fn a_chat_runs_without_a_window_and_shows_elsewhere() {
    // Nothing of the user's: an empty home and agent directory.
    let home = std::env::temp_dir().join(format!("serpis-chat-{}", uuid::Uuid::now_v7()));
    let (work, agent) = (home.join("project"), home.join("agent"));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(work.join("f0.txt"), "zero\n").unwrap();
    std::fs::write(work.join("f1.txt"), "one\n").unwrap();
    // SAFETY: the only test in this binary, before any thread of ours starts.
    unsafe { std::env::set_var("HOME", &home) };

    let rt = tokio::runtime::Runtime::new().unwrap();
    let mock = rt.block_on(serpis_testkit::Mock::start(serpis_testkit::commands()));
    let model: serpis_ai::Model = serde_json::from_value(json!({
        "id": "anthropic/claude-sonnet-4.5", "api": "openai-completions", "provider": "openrouter",
        "baseUrl": format!("{}/api/v1", mock.base), "reasoning": true, "input": ["text", "image"],
    }))
    .unwrap();
    let mut host = TestHost {
        vars: HashMap::from([("session-directory".to_string(), json!(home.join("sessions")))]),
        registry: serpis_ai::Registry::new(serpis_ai::Paths { agent_dir: agent }),
        rt: rt.handle().clone(),
        notices: Vec::new(),
    };
    let (wake, woken) = mpsc::channel();
    let wake = Arc::new(std::sync::Mutex::new(wake));
    let mut chat = Chat::new(ChatInit { cwd: work.clone(), model: Some(model), thinking: serpis_ai::ThinkingLevel::Medium }, Arc::new(move || {
        let _ = wake.lock().unwrap().send(());
    }));
    let mut mirror = Mirror::default();
    mirror.take(&mut chat);

    chat.submit(&mut host, "hello".into(), vec![], false, Some("req-1")).unwrap();
    finish(&mut chat, &mut host, &woken, &mut mirror);
    // A client retrying a send whose answer it lost doesn't send it twice.
    chat.submit(&mut host, "hello".into(), vec![], false, Some("req-1")).unwrap();
    assert!(!chat.running(), "the retried request started another run");
    chat.submit(&mut host, "READ 2".into(), vec![], false, Some("req-2")).unwrap();
    finish(&mut chat, &mut host, &woken, &mut mirror);

    let all = chat.full_splice();
    assert_eq!(mirror.entries, all.entries, "the client shows exactly what the chat shows");
    assert_eq!(kinds(&all.entries), ["user", "assistant", "user", "assistant", "tool", "tool", "assistant"], "{:#?}", all.entries);
    let Entry::Tool { result: Some(r), elapsed_ms: Some(_), running_ms: None, .. } = &all.entries[4] else { panic!("the first read finished: {:#?}", all.entries[4]) };
    assert!(r.content.iter().any(|c| matches!(c, serpis_ai::UserContent::Text { text } if text.contains("zero"))), "{r:?}");
    // After the first, splices start where things changed: nobody is sent the whole
    // transcript again.
    assert!(mirror.splices.len() > 4, "{:?}", mirror.splices);
    assert!(mirror.splices.iter().skip(1).all(|&f| f > 0), "{:?}", mirror.splices);

    let state = chat.state();
    assert_eq!(state.model.as_deref(), Some("openrouter/anthropic/claude-sonnet-4.5"));
    assert_eq!(state.title, "hello");
    assert!(!state.running && state.step.is_none());
    assert!(state.usage.total_tokens > 0, "{state:?}");
    // The conversation was saved, and resumes into the same transcript.
    let path = chat.session.as_ref().map(|w| w.path.clone()).unwrap();
    let loaded = serpis_chat::session::load(&path).unwrap();
    let mut again = Chat::new(ChatInit { cwd: work, model: None, thinking: serpis_ai::ThinkingLevel::Medium }, Arc::new(|| {}));
    again.restore(&mut host, &loaded);
    let resumed = again.full_splice().entries;
    let same = |es: &[Entry]| es.iter().map(comparable).collect::<Vec<_>>();
    assert_eq!(same(&resumed[1..resumed.len() - 1]), same(&all.entries), "resumed sessions show the same conversation");
    assert!(host.notices.is_empty(), "{:?}", host.notices);
    let _ = std::fs::remove_dir_all(&home);
}

/// What a live and a resumed transcript both say: timings and cache verdicts are only known
/// live, and tool calls only show as their tool entries.
fn comparable(e: &Entry) -> Entry {
    let mut e = e.clone();
    match &mut e {
        Entry::Tool { elapsed_ms, .. } => *elapsed_ms = None,
        Entry::Assistant { cache, blocks, .. } => {
            *cache = None;
            blocks.retain(|b| !matches!(b, serpis_ai::AssistantContent::ToolCall(_)));
        }
        _ => {}
    }
    e
}
