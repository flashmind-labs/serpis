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
use serpis_proto::{Decision, Entry};

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

    /// Poll until a tool call waits for the person; its id.
    fn ask(&mut self, chat: &mut Chat) -> String {
        for _ in 0..400 {
            let _ = self.woken.recv_timeout(Duration::from_millis(25));
            chat.poll(&mut self.host);
            if let Some(id) = chat.asking().first() {
                return id.clone();
            }
        }
        panic!("nothing asked: {:#?}", chat.entries());
    }

    /// Poll until `done` says so.
    fn until(&mut self, chat: &mut Chat, done: impl Fn(&Chat) -> bool) {
        for _ in 0..400 {
            let _ = self.woken.recv_timeout(Duration::from_millis(25));
            chat.poll(&mut self.host);
            if done(chat) {
                return;
            }
        }
        panic!("it didn't happen: {:#?}", chat.entries());
    }

    /// Everything the stand-in logged, a line each.
    fn log_lines(&self) -> Vec<Value> {
        let text = std::fs::read_to_string(&self.log).unwrap_or_default();
        text.lines().filter_map(|l| serde_json::from_str(l).ok()).collect()
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
    assert!(w.log_lines().iter().any(|l| l["agent_env"] == "1"), "claude runs with SERPIS_AGENT, as its commands then do");
    assert_eq!(after(&starts[0], "--session-id"), Some(chat.session_id.as_str()));
    assert_eq!(after(&starts[0], "--model"), Some("sonnet"));
    assert_eq!(after(&starts[0], "--effort"), Some("high"));
    assert_eq!(after(&starts[0], "--permission-mode"), Some("default"));

    // The next turn goes to the same process.
    w.send(&mut chat, "again");
    assert_eq!(last_text(chat.entries()), "echo: again");
    assert_eq!(w.log().0.len(), 1, "one claude process per chat");

    // An edit asks the person: its card says so (not running meanwhile), and so does the
    // chat's step. Allowing it always passes Claude Code's suggestion back.
    chat.submit(&mut w.host, "please edit".into(), vec![], false, None).unwrap();
    let asked = w.ask(&mut chat);
    assert!(matches!(chat.entries().iter().find(|e| matches!(e, Entry::Tool { id, .. } if *id == asked)), Some(Entry::Tool { ask: Some(a), running_ms: None, .. }) if a.always.as_deref() == Some("edits in this session")));
    assert_eq!(chat.state().step.as_deref(), Some("waiting for you: edit notes.txt"));
    assert!(!chat.answer("nope", Decision::Allow), "only a waiting call can be answered");
    assert!(chat.answer(&asked, Decision::Always));
    assert!(!chat.answer(&asked, Decision::Allow), "answered once");
    w.finish(&mut chat);
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
    assert_eq!(answer["response"]["response"]["updatedPermissions"][0]["mode"], "acceptEdits");
    assert!(chat.entries().iter().all(|e| !matches!(e, Entry::Tool { ask: Some(_), .. })));
    // That switched Claude Code to accepting edits, and the chat says so.
    assert_eq!(chat.state().permission_mode.as_deref(), Some("acceptEdits"));

    // Its mode is switched at once, without starting it again.
    let starts = w.log().0.len();
    chat.set_permission_mode("auto").unwrap();
    assert_eq!(chat.state().permission_mode.as_deref(), Some("auto"));
    w.send(&mut chat, "in auto mode");
    assert_eq!(w.log().0.len(), starts, "no restart for a new mode");
    assert!(w.log().1.iter().any(|g| g["request"]["subtype"] == "set_permission_mode" && g["request"]["mode"] == "auto"));
    assert!(chat.set_permission_mode("yolo").is_err());
    chat.set_permission_mode("default").unwrap();

    // The person refuses one, saying why.
    chat.submit(&mut w.host, "edit no".into(), vec![], false, None).unwrap();
    let asked = w.ask(&mut chat);
    chat.answer(&asked, Decision::Deny { message: Some("not that file".into()) });
    w.finish(&mut chat);
    let refusal = chat.entries().iter().rev().find_map(|e| if let Entry::Tool { result: Some(r), .. } = e { Some(serde_json::to_string(&r.content).unwrap()) } else { None }).unwrap();
    assert!(refusal.contains("not that file"), "{refusal}");

    // Stopping while it waits ends the turn and the question.
    chat.submit(&mut w.host, "edit, then stop".into(), vec![], false, None).unwrap();
    w.ask(&mut chat);
    chat.abort();
    w.finish(&mut chat);
    assert!(chat.asking().is_empty() && chat.entries().iter().all(|e| !matches!(e, Entry::Tool { ask: Some(_), .. })));

    // Its questions wait for answers, which go back with the questions.
    chat.submit(&mut w.host, "a question".into(), vec![], false, None).unwrap();
    let asked = w.ask(&mut chat);
    assert_eq!(chat.state().step.as_deref(), Some("waiting for your answer: Which color? (+1 more)"));
    let answers = [("Which color?", "Blue"), ("Which sizes?", "S, L")].into_iter().map(|(q, a)| (q.to_string(), a.to_string())).collect();
    assert!(chat.answer(&asked, Decision::Answers { answers }));
    w.finish(&mut chat);
    assert_eq!(last_text(chat.entries()), "You chose Blue in S, L.");
    let (_, got) = w.log();
    let answer = &got.iter().rev().find(|g| g["type"] == "control_response").unwrap()["response"]["response"];
    assert_eq!((&answer["behavior"], &answer["updatedInput"]["questions"][1]["header"]), (&json!("allow"), &json!("Sizes")), "{answer}");

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
    // With nobody asked, questions are put in the reply instead.
    w.send(&mut chat, "one question more");
    assert!(matches!(refused(&chat), (true, t) if t.contains("plain text")));
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

    // Work on the side. A subagent: its steps go in its tool call's entry, with how it went.
    let mut bg = w.chat();
    w.send(&mut bg, "use a subagent");
    let tool = |chat: &Chat, name: &str| chat.entries().iter().rev().find(|e| matches!(e, Entry::Tool { name: n, .. } if n == name)).cloned().unwrap_or_else(|| panic!("no {name}: {:#?}", chat.entries()));
    let Entry::Tool { steps, task: Some(task), result: Some(_), .. } = tool(&bg, "Agent") else { panic!("{:#?}", bg.entries()) };
    assert_eq!((task.kind.as_str(), task.status.as_str(), task.background, task.tool_uses), ("agent", "completed", false, 1), "{task:?}");
    assert!(matches!(&steps[0], Entry::Tool { name, result: Some(r), .. } if name == "bash" && !r.is_error), "{steps:#?}");
    assert_eq!(last_text(&steps), "It printed from-sub.");
    assert!(bg.entries().iter().all(|e| !matches!(e, Entry::Tool { name, .. } if name == "bash")), "its steps aren't the chat's own");
    assert_eq!(last_text(bg.entries()), "The subagent said from-sub.");

    // A command in the background: the turn ends while it runs; when it finishes, Claude Code
    // takes a turn of its own, which the chat runs (and records) as any.
    w.send(&mut bg, "run it in the background");
    assert_eq!(last_text(bg.entries()), "Started it in the background.");
    let Entry::Tool { task: Some(task), .. } = tool(&bg, "bash") else { panic!() };
    assert!(task.running() && task.background, "{task:?}");
    w.until(&mut bg, |c| c.running());
    w.finish(&mut bg);
    assert_eq!(last_text(bg.entries()), "The background command finished: bg-done.");
    assert!(bg.entries().iter().any(|e| matches!(e, Entry::Info { text, .. } if text == "Background command finished: sleep then echo bg-done")), "{:#?}", bg.entries());
    let Entry::Tool { task: Some(task), .. } = tool(&bg, "bash") else { panic!() };
    assert_eq!(task.status, "completed");
    assert!(bg.history.last().is_some_and(|m| matches!(m, serpis_ai::Message::Assistant(a) if a.text() == "The background command finished: bg-done.")), "its own turn is recorded");

    // An agent in the background: its steps come after the turn, then its own turn.
    w.send(&mut bg, "an agent in the background");
    assert_eq!(last_text(bg.entries()), "Launched it.");
    w.until(&mut bg, |c| c.running());
    w.finish(&mut bg);
    assert_eq!(last_text(bg.entries()), "The agent reported: from-sub.");
    let Entry::Tool { steps, task: Some(task), .. } = tool(&bg, "Agent") else { panic!() };
    assert_eq!((steps.len(), task.status.as_str(), task.background), (2, "completed", true), "{steps:#?}");

    // Stopping one: Claude Code is asked to.
    w.send(&mut bg, "background forever");
    let Entry::Tool { task: Some(task), .. } = tool(&bg, "bash") else { panic!() };
    assert!(bg.stop_task(&task.id));
    w.until(&mut bg, |c| matches!(c.entries().iter().rev().find(|e| matches!(e, Entry::Tool { name, .. } if name == "bash")), Some(Entry::Tool { task: Some(t), .. }) if t.status == "stopped"));
    assert!(!bg.running(), "a stopped task takes no turn");

    // A monitor beside a shell: both run, each its own task, and a monitor is told apart.
    w.send(&mut bg, "background forever too");
    w.send(&mut bg, "start a monitor");
    let Entry::Tool { task: Some(monitor), .. } = tool(&bg, "Monitor") else { panic!("{:#?}", bg.entries()) };
    let Entry::Tool { task: Some(shell), .. } = tool(&bg, "bash") else { panic!() };
    assert_eq!((monitor.kind.as_str(), shell.kind.as_str()), ("monitor", "bash"));
    assert!(monitor.running() && shell.running());
    for t in [&monitor.id, &shell.id] {
        assert!(bg.stop_task(t));
    }
    w.until(&mut bg, |c| c.entries().iter().all(|e| !matches!(e, Entry::Tool { task: Some(t), .. } if t.running())));

    // What it leaves running goes with it: closing the chat's Claude Code ends what it started.
    w.send(&mut bg, "background forever, again");
    let pid = w.log_lines().iter().rev().find_map(|l| l["spawned"].as_u64()).expect("the stand-in's command");
    let alive = |pid: u64| std::process::Command::new("kill").args(["-0", &pid.to_string()]).stderr(std::process::Stdio::null()).status().is_ok_and(|s| s.success());
    assert!(alive(pid));
    bg.clear();
    for _ in 0..100 {
        if !alive(pid) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(!alive(pid), "the command claude started, in a process group of its own, ended with it");

    assert!(w.host.notices.is_empty(), "{:?}", w.host.notices);
    let _ = std::fs::remove_dir_all(&w.home);
}

