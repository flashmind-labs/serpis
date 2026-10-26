//! A node killed mid-turn (as a crash or a reboot does it): its chat continues when it starts
//! again, without running the cut-off tool twice; a turn cut off too many times in a row is
//! parked; a chat that doesn't keep going waits. Each time the chat says what happened. The node
//! runs as its own process here (started by `serpis-node agent`), so it can be killed for real.

use std::path::Path;
use std::time::Duration;

use serde_json::json;
use serpis_node::client::{Client, Target};
use serpis_proto::Entry;
use serpis_proto::wire::{Event, Request, Response};
use tokio::sync::mpsc::UnboundedReceiver;

#[derive(Default)]
struct View {
    entries: Vec<Entry>,
    running: bool,
}

impl View {
    async fn until(&mut self, events: &mut UnboundedReceiver<Event>, done: impl Fn(&View) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !done(self) {
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Some(Event::Splice { splice, .. })) => splice.apply(&mut self.entries),
                Ok(Some(Event::State { state, .. })) => self.running = state.running,
                Ok(Some(_)) => {}
                Ok(None) => panic!("the node closed the stream; entries: {:#?}", self.entries),
                Err(_) => panic!("timed out; entries: {:#?}", self.entries),
            }
        }
    }

    fn says(&self, text: &str) -> bool {
        self.entries.iter().any(|e| match e {
            Entry::Info { text: t, .. } => t.contains(text),
            Entry::Tool { result: Some(r), .. } => serde_json::to_string(r).unwrap().contains(text),
            Entry::Assistant { blocks, .. } => serde_json::to_string(blocks).unwrap().contains(text),
            _ => false,
        })
    }

    /// A tool call that hasn't finished.
    fn tool_running(&self) -> bool {
        self.entries.iter().any(|e| matches!(e, Entry::Tool { result: None, .. }))
    }
}

fn settings(node: &Path, sessions: &Path, extra: serde_json::Value) {
    let mut s = json!({"session-directory": sessions});
    s.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    std::fs::write(node.join("settings.json"), s.to_string()).unwrap();
}

/// Kill the node as a crash would: no chance to note anything.
async fn crash(node: &Path) {
    let pid = std::fs::read_to_string(node.join("node.pid")).unwrap();
    let status = std::process::Command::new("kill").args(["-9", pid.trim()]).status().unwrap();
    assert!(status.success());
    for _ in 0..100 {
        if std::os::unix::net::UnixStream::connect(node.join("node.sock")).is_err() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("the node is still there");
}

#[test]
fn a_cut_off_turn_keeps_going() {
    let root = std::env::temp_dir().join(format!("sk-{}", &uuid::Uuid::now_v7().simple().to_string()[24..]));
    let (home, agent, node, sessions) = (root.join("home"), root.join("agent"), root.join("node"), root.join("sessions"));
    for d in [home.join("project"), agent.clone(), node.clone()] {
        std::fs::create_dir_all(d).unwrap();
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mock = rt.block_on(serpis_testkit::Mock::start(serpis_testkit::commands()));
    let models = json!({"providers": {"mock": {"baseUrl": format!("{}/api/v1", mock.base), "api": "openai-completions", "apiKey": "x", "compat": {"openRouter": true}, "models": [{"id": "m"}]}}});
    std::fs::write(agent.join("models.json"), models.to_string()).unwrap();
    settings(&node, &sessions, json!({}));
    let env = |k: &str, v: &Path| format!("{k}={}", v.display());
    let target = Target::Command(vec![
        "env".into(),
        env("HOME", &home),
        env("SERPIS_NODE_DIR", &node),
        env("SERPIS_NODE_SOCKET", &node.join("node.sock")),
        env("SERPIS_AGENT_DIR", &agent),
        env("SERPIS_CONFIG_DIR", &root.join("config")),
        env("SERPIS_TERMINAL_SOCKET", &root.join("t.sock")),
        env!("CARGO_BIN_EXE_serpis-node").into(),
        "agent".into(),
    ]);
    rt.block_on(async {
        let (c, mut events) = Client::connect(&target, "test").await.unwrap();
        let Response::Chat { chat } = c.call(Request::New { cwd: "~/project".into(), model: Some("mock/m".into()), thinking: None, chat: None }).await.unwrap() else { panic!() };
        c.call(Request::Attach { chat: chat.clone() }).await.unwrap();
        let prompt = |text: &str| Request::Prompt { chat: chat.clone(), text: text.into(), images: vec![], follow_up: false, request_id: None };
        let mut v = View::default();
        // A turn cut off mid-tool continues; the tool isn't run again.
        c.call(prompt(r#"CALL bash {"command": "sleep 30"}"#)).await.unwrap();
        v.until(&mut events, |v| v.tool_running()).await;
        drop((c, events));
        crash(&node).await;
        let (c, mut events) = Client::connect(&target, "test").await.unwrap();
        c.call(Request::Attach { chat: chat.clone() }).await.unwrap();
        let mut v = View::default();
        v.until(&mut events, |v| v.says("Continued") && v.says("done") && !v.running).await;
        // (Its result says it was cut off: the call wasn't made again.)
        assert!(v.says("Not run to the end"), "{:#?}", v.entries);

        // One cut off more times in a row than chat-keep-going-limit says is parked.
        settings(&node, &sessions, json!({"chat-keep-going-limit": 0}));
        c.call(prompt(r#"CALL bash {"command": "sleep 30"}"#)).await.unwrap();
        v.until(&mut events, |v| v.tool_running()).await;
        drop((c, events));
        crash(&node).await;
        let (c, mut events) = Client::connect(&target, "test").await.unwrap();
        c.call(Request::Attach { chat: chat.clone() }).await.unwrap();
        let mut v = View::default();
        v.until(&mut events, |v| v.says("Parked")).await;
        assert!(!v.running && v.tool_running(), "parked, not continued: {:#?}", v.entries);

        // Sent a message, it goes on (the cut-off call gets its result first).
        settings(&node, &sessions, json!({}));
        c.call(prompt("again")).await.unwrap();
        v.until(&mut events, |v| v.says("ok") && !v.running && !v.tool_running()).await;
        // A chat that doesn't keep going (its own choice) waits for you.
        c.call(Request::Append { chat: chat.clone(), kind: "keep-going".into(), data: json!({"on": false}) }).await.unwrap();
        c.call(prompt(r#"CALL bash {"command": "sleep 30"}"#)).await.unwrap();
        v.until(&mut events, |v| v.tool_running()).await;
        drop((c, events));
        crash(&node).await;
        let (c, mut events) = Client::connect(&target, "test").await.unwrap();
        c.call(Request::Attach { chat: chat.clone() }).await.unwrap();
        let mut v = View::default();
        v.until(&mut events, |v| v.says("doesn't keep going")).await;
        assert!(!v.running);
        c.call(Request::Stop).await.unwrap();
    });
    let _ = std::fs::remove_dir_all(&root);
}
