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
    // A skill of the user's (pi's folder) and one of the project's.
    for (dir, text) in [
        (agent.join("skills/eng/review"), "---\nname: review\ndescription: Review a change. Use when asked to review.\n---\nRead the whole diff first.\n"),
        (work.join(".pi/skills/deploy"), "---\ndescription: Deploy the project.\n---\nRun the deploy script.\n"),
        // pi's own folder (~/.pi/agent/skills): serpis reads it too.
        (home.join(".pi/agent/skills/grill"), "---\ndescription: Grill the plan.\n---\nAsk hard questions.\n"),
        // Claude Code's own: not for serpis's agent.
        (home.join(".claude/skills/pdf"), "---\ndescription: Work with PDFs.\n---\nUse the pdf tools.\n"),
    ] {
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("SKILL.md"), text).unwrap();
    }
    // The user's additions to the system prompt, in the config directory (~/.config/serpis).
    let config = home.join(".config/serpis");
    std::fs::create_dir_all(&config).unwrap();
    std::fs::write(config.join("APPEND_SYSTEM.md"), "Always answer in British English.\n").unwrap();
    std::fs::write(work.join("AGENTS.md"), "Run the tests before saying it works.\n").unwrap();
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
    let mut chat = Chat::new(ChatInit { cwd: work.clone(), model: Some(model.clone()), thinking: serpis_ai::ThinkingLevel::Medium }, Arc::new(move || {
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

    // Its commands know they run for an agent (a shell config can tell its own terminals
    // from them): SERPIS_AGENT, beside the session's id.
    chat.submit(&mut host, r#"CALL bash {"command": "echo agent=$SERPIS_AGENT"}"#.into(), vec![], false, None).unwrap();
    finish(&mut chat, &mut host, &woken, &mut mirror);
    let out = chat.entries().iter().rev().find_map(|e| if let Entry::Tool { name, result: Some(r), .. } = e { (name == "bash").then(|| format!("{:?}", r.content)) } else { None }).expect("the command ran");
    assert!(out.contains("agent=1"), "{out}");

    // Skills, as pi has them: named and described in the system prompt, read when they apply.
    let system = mock.log()[0].body["messages"][0]["content"].to_string();
    assert!(system.contains("<available_skills>") && system.contains("<name>review</name>") && system.contains("<name>deploy</name>"), "{system}");
    assert!(system.contains("Use the read tool to load a skill's file"), "{system}");
    assert!(!system.contains("<name>pdf</name>"), "Claude Code's skills stay Claude Code's");
    // APPEND_SYSTEM.md is added to serpis's prompt; the project's AGENTS.md is in it too.
    assert!(system.contains("You are an expert coding assistant") && system.contains("Always answer in British English.") && system.contains("Run the tests before saying it works."), "{system}");
    assert_eq!(chat.skills(&host).iter().map(|s| (s.name.as_str(), s.source.as_str())).collect::<Vec<_>>(), [("review", "user"), ("grill", "pi"), ("deploy", "project")]);
    // /skill:name sends its instructions with what follows; the transcript shows it as typed.
    chat.submit(&mut host, "/skill:review the last commit".into(), vec![], false, None).unwrap();
    finish(&mut chat, &mut host, &woken, &mut mirror);
    let sent = serpis_testkit::mock::last_user_text(mock.log().last().unwrap().wire, &mock.log().last().unwrap().body).unwrap_or_default();
    assert!(sent.starts_with("<skill name=\"review\"") && sent.contains("Read the whole diff first.") && sent.ends_with("</skill>\n\nthe last commit"), "{sent}");
    assert!(chat.entries().iter().any(|e| matches!(e, Entry::User { text, .. } if text == "/skill:review the last commit")), "{:#?}", chat.entries());
    assert!(chat.submit(&mut host, "/skill:nope".into(), vec![], false, None).is_err(), "an unknown skill is refused, not sent");
    host.notices.clear();
    // Resumed, it still reads as typed.
    let path = chat.session.as_ref().map(|w| w.path.clone()).unwrap();
    let mut back = Chat::new(ChatInit { cwd: home.join("project"), model: None, thinking: serpis_ai::ThinkingLevel::Medium }, Arc::new(|| {}));
    back.restore(&mut host, &serpis_chat::session::load(&path).unwrap());
    assert!(back.entries().iter().any(|e| matches!(e, Entry::User { text, .. } if text == "/skill:review the last commit")));

    // SYSTEM.md takes the place of serpis's own prompt; APPEND_SYSTEM.md and the project's
    // instructions still follow it.
    std::fs::write(config.join("SYSTEM.md"), "You are Reviewer, a careful code reviewer.\n").unwrap();
    let system_of = |host: &mut TestHost| {
        let mut c = Chat::new(ChatInit { cwd: home.join("project"), model: Some(model.clone()), thinking: serpis_ai::ThinkingLevel::Medium }, Arc::new(|| {}));
        c.submit(host, "hello".into(), vec![], false, None).unwrap();
        let (tx, rx) = mpsc::channel::<()>();
        drop(tx);
        finish(&mut c, host, &rx, &mut Mirror::default());
        // (A string, or blocks marked for the cache.)
        let content = mock.log().last().unwrap().body["messages"][0]["content"].clone();
        content.as_str().map(str::to_string).unwrap_or_else(|| content[0]["text"].as_str().unwrap_or_default().to_string())
    };
    let system = system_of(&mut host);
    assert!(system.starts_with("You are Reviewer, a careful code reviewer."), "{system}");
    assert!(!system.contains("You are an expert coding assistant"), "{system}");
    assert!(system.contains("Always answer in British English.") && system.contains("Run the tests before saying it works."), "{system}");
    // agent-system-prompt-files off: neither file.
    host.vars.insert("agent-system-prompt-files".into(), json!(false));
    let system = system_of(&mut host);
    assert!(system.contains("You are an expert coding assistant") && !system.contains("Reviewer") && !system.contains("British English"), "{system}");
    assert!(system.contains("Run the tests before saying it works."), "project instructions stay: {system}");
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
