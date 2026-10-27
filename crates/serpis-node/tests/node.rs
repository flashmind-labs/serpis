//! A node end to end: clients on its socket and through `serpis-node agent` (what ssh runs),
//! chats against the mock provider, late joiners, retries, resume, and a node the agent has to
//! start itself. Nothing of the user's is read: home, agent and node directories are throwaway.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;
use serpis_node::client::{Client, Target};
use serpis_proto::wire::{Event, Request, Response};
use serpis_proto::{ChatState, Entry};
use tokio::sync::mpsc::UnboundedReceiver;

/// A client's view of one chat, kept from the events it is sent.
#[derive(Default)]
struct View {
    entries: Vec<Entry>,
    state: Option<ChatState>,
    notices: Vec<String>,
}

impl View {
    fn take(&mut self, e: Event) {
        match e {
            Event::Splice { splice, .. } => splice.apply(&mut self.entries),
            Event::State { state, .. } => self.state = Some(state),
            Event::Notice { text, .. } => self.notices.push(text),
            Event::Message { text, .. } => self.notices.push(text),
            Event::Closed { .. } | Event::ToolCall { .. } | Event::TerminalOutput { .. } | Event::TerminalExited { .. } | Event::PairRequested { .. } => {}
        }
    }

    /// Read events until `done` holds (or fail after a while).
    async fn until(&mut self, events: &mut UnboundedReceiver<Event>, done: impl Fn(&View) -> bool) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !done(self) {
            match tokio::time::timeout_at(deadline, events.recv()).await {
                Ok(Some(e)) => self.take(e),
                Ok(None) => panic!("the node closed the stream"),
                Err(_) => panic!("timed out; entries: {:#?}, state: {:#?}", self.entries, self.state),
            }
        }
    }

    fn idle(&self) -> bool {
        self.state.as_ref().is_some_and(|s| !s.running)
    }

    fn kinds(&self) -> Vec<&'static str> {
        self.entries
            .iter()
            .map(|e| match e {
                Entry::User { .. } => "user",
                Entry::Assistant { .. } => "assistant",
                Entry::Tool { .. } => "tool",
                Entry::Info { .. } => "info",
            })
            .collect()
    }
}

fn chat_of(r: Response) -> String {
    match r {
        Response::Chat { chat } => chat,
        other => panic!("not a chat: {other:?}"),
    }
}

/// A home with a project and an agent directory whose only model is the mock.
fn setup(root: &Path, mock: &str) -> (PathBuf, PathBuf) {
    let (work, agent) = (root.join("home/project"), root.join("agent"));
    std::fs::create_dir_all(&work).unwrap();
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(work.join("f0.txt"), "zero\n").unwrap();
    std::fs::write(work.join("f1.txt"), "one\n").unwrap();
    let models = json!({"providers": {"mock": {
        "baseUrl": format!("{mock}/api/v1"), "api": "openai-completions", "apiKey": "x", "compat": {"openRouter": true},
        "models": [{"id": "anthropic/claude-sonnet-4.5", "reasoning": true, "input": ["text", "image"]}],
    }}});
    std::fs::write(agent.join("models.json"), models.to_string()).unwrap();
    std::fs::write(agent.join("auth.json"), "{}").unwrap();
    (work, agent)
}

#[test]
fn a_node_runs_chats_for_clients_anywhere() {
    // Short: Unix socket paths are limited to about 100 bytes.
    let root = std::env::temp_dir().join(format!("sn-{}", &uuid::Uuid::now_v7().simple().to_string()[24..]));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mock = rt.block_on(serpis_testkit::Mock::start(serpis_testkit::commands()));
    let (work, agent) = setup(&root, &mock.base);
    // SAFETY: the only test in this binary, before any thread of ours starts.
    unsafe { std::env::set_var("HOME", root.join("home")) };
    let node_dir = root.join("node");
    std::fs::create_dir_all(&node_dir).unwrap();
    std::fs::write(node_dir.join("settings.json"), json!({"session-directory": root.join("sessions")}).to_string()).unwrap();
    let socket = node_dir.join("node.sock");
    let node = serpis_node::server::start(serpis_node::server::Config { socket: socket.clone(), dir: node_dir.clone(), agent_dir: Some(agent.clone()), config_dir: Some(node_dir.join("config")) }, rt.handle().clone()).unwrap();
    rt.block_on(async {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&node_dir).unwrap().permissions().mode() & 0o777, 0o700, "only its user can reach the socket");

        let (a, mut a_events) = Client::connect(&Target::Local(socket.clone()), "test-a").await.unwrap();
        assert!(!a.hello.serpis.is_empty());
        let Response::Models { models } = a.call(Request::Models).await.unwrap() else { panic!() };
        assert!(models.iter().any(|m| m.model == "mock/anthropic/claude-sonnet-4.5"), "{models:?}");

        // Mistakes are answered, not fatal.
        let err = a.call(Request::New { cwd: "/nowhere/at/all".into(), model: None, thinking: None, chat: None }).await.unwrap_err();
        assert!(err.to_string().contains("isn't a folder"), "{err}");
        assert!(a.call(Request::Abort { chat: "nope".into() }).await.is_err());

        // `~` is this machine's home.
        let chat = chat_of(a.call(Request::New { cwd: "~/project".into(), model: Some("mock/anthropic/claude-sonnet-4.5".into()), thinking: None, chat: None }).await.unwrap());
        a.call(Request::Attach { chat: chat.clone() }).await.unwrap();
        let mut va = View::default();
        va.until(&mut a_events, |v| v.state.is_some()).await;
        assert_eq!(va.state.as_ref().unwrap().cwd, work.to_string_lossy());

        let prompt = |text: &str, id: &str| Request::Prompt { chat: chat.clone(), text: text.into(), images: vec![], follow_up: false, request_id: Some(id.into()) };
        a.call(prompt("READ 2", "r1")).await.unwrap();
        va.until(&mut a_events, |v| v.kinds().len() >= 5 && v.idle()).await;
        assert_eq!(va.kinds(), ["user", "assistant", "tool", "tool", "assistant"]);
        let Entry::Tool { result: Some(r), .. } = &va.entries[2] else { panic!("{:#?}", va.entries) };
        assert!(serde_json::to_string(r).unwrap().contains("zero"));
        // A retry of a prompt whose reply was lost is answered without running it again.
        a.call(prompt("READ 2", "r1")).await.unwrap();

        // A second client joining late sees the same chat, and both follow what happens next.
        let (b, mut b_events) = Client::connect(&Target::Local(socket.clone()), "test-b").await.unwrap();
        let Response::Chats { chats } = b.call(Request::List).await.unwrap() else { panic!() };
        assert_eq!(chats.len(), 1);
        assert_eq!(chats[0].chat, chat);
        b.call(Request::Attach { chat: chat.clone() }).await.unwrap();
        let mut vb = View::default();
        vb.until(&mut b_events, |v| v.idle()).await;
        assert_eq!(vb.entries, va.entries, "a late joiner sees what the first client sees");
        b.call(prompt("hello", "r2")).await.unwrap();
        va.until(&mut a_events, |v| v.kinds().len() == 7 && v.idle()).await;
        vb.until(&mut b_events, |v| v.kinds().len() == 7 && v.idle()).await;
        assert_eq!(vb.entries, va.entries);
        assert_eq!(va.kinds()[5..], ["user", "assistant"], "the retried prompt didn't run again");

        // Settings come from the node's own file: the session went where it says.
        let Response::Sessions { sessions } = a.call(Request::Sessions { limit: None }).await.unwrap() else { panic!() };
        assert_eq!(sessions.len(), 1, "{sessions:?}");
        assert!(sessions[0].path.starts_with(&*root.join("sessions").to_string_lossy()));
        assert_eq!(sessions[0].title, "READ 2");
        // And those of the other directories the desktop lists (pi's): every device lists the same.
        let other = root.join("pi-sessions/--project--");
        std::fs::create_dir_all(&other).unwrap();
        let text = std::fs::read_to_string(&sessions[0].path).unwrap().replace(&sessions[0].id, "from-pi");
        std::fs::write(other.join("2026-01-01T00-00-00-000Z_from-pi.jsonl"), text).unwrap();
        std::fs::write(node_dir.join("settings.json"), json!({"session-directory": root.join("sessions"), "session-directories": [root.join("pi-sessions")]}).to_string()).unwrap();
        let mut listed = Vec::new();
        for _ in 0..60 {
            let Response::Sessions { sessions } = a.call(Request::Sessions { limit: None }).await.unwrap() else { panic!() };
            listed = sessions.into_iter().map(|s| s.id).collect();
            if listed.len() == 2 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(listed.contains(&"from-pi".to_string()), "{listed:?}");

        // Closing a chat keeps its session, which resumes as a chat again.
        a.call(Request::Close { chat: chat.clone() }).await.unwrap();
        let Response::Chats { chats } = a.call(Request::List).await.unwrap() else { panic!() };
        assert!(chats.is_empty());
        let resumed = chat_of(a.call(Request::Resume { session: sessions[0].path.clone() }).await.unwrap());
        assert_eq!(resumed, chat, "a resumed chat keeps its id");
        let (c, mut c_events) = Client::connect(&Target::Local(socket.clone()), "test-c").await.unwrap();
        c.call(Request::Attach { chat: chat.clone() }).await.unwrap();
        let mut vc = View::default();
        vc.until(&mut c_events, |v| v.idle()).await;
        assert_eq!(vc.kinds(), ["user", "assistant", "tool", "tool", "assistant", "user", "assistant", "info"]);

        // Through `serpis-node agent`, as `ssh <host> serpis-node agent` reaches it.
        let exe = env!("CARGO_BIN_EXE_serpis-node");
        let bridged = Target::Command(vec!["env".into(), format!("SERPIS_NODE_SOCKET={}", socket.display()), exe.into(), "agent".into()]);
        let (d, mut d_events) = Client::connect(&bridged, "test-d").await.unwrap();
        d.call(Request::Attach { chat: chat.clone() }).await.unwrap();
        let mut vd = View::default();
        vd.until(&mut d_events, |v| v.idle()).await;
        assert_eq!(vd.entries, vc.entries, "the bridge carries the protocol unchanged");
        d.call(Request::Prompt { chat: chat.clone(), text: "again".into(), images: vec![], follow_up: false, request_id: None }).await.unwrap();
        vd.until(&mut d_events, |v| v.kinds().len() == 10 && v.idle()).await;

        let Response::Dirs { dirs, .. } = d.call(Request::Dirs { path: "~".into() }).await.unwrap() else { panic!() };
        assert_eq!(dirs, ["project"]);
    });
    node.stop();
    // Reopened with the node: the chats that were open come back.
    let node = serpis_node::server::start(serpis_node::server::Config { socket: socket.clone(), dir: node_dir.clone(), agent_dir: Some(agent.clone()), config_dir: Some(node_dir.join("config")) }, rt.handle().clone()).unwrap();
    rt.block_on(async {
        let (a, _) = Client::connect(&Target::Local(socket.clone()), "test-e").await.unwrap();
        let Response::Chats { chats } = a.call(Request::List).await.unwrap() else { panic!() };
        assert_eq!(chats.len(), 1, "the open chat came back with the node");
    });
    node.stop();

    // No node running: the agent starts one, which keeps running after the client is gone.
    rt.block_on(async {
        let exe = env!("CARGO_BIN_EXE_serpis-node");
        let fresh = root.join("n2");
        let env = |k: &str, v: &Path| format!("{k}={}", v.display());
        let target = Target::Command(vec![
            "env".into(),
            env("HOME", &root.join("home")),
            env("SERPIS_NODE_DIR", &fresh),
            env("SERPIS_NODE_SOCKET", &fresh.join("node.sock")),
            env("SERPIS_AGENT_DIR", &agent),
            exe.into(),
            "agent".into(),
        ]);
        let (a, _) = Client::connect(&target, "test-f").await.unwrap();
        let chat = chat_of(a.call(Request::New { cwd: work.to_string_lossy().into_owned(), model: None, thinking: None, chat: None }).await.unwrap());
        drop(a);
        tokio::time::sleep(Duration::from_millis(200)).await;
        let (b, _) = Client::connect(&target, "test-g").await.unwrap();
        let Response::Chats { chats } = b.call(Request::List).await.unwrap() else { panic!() };
        assert_eq!(chats.iter().map(|c| c.chat.clone()).collect::<Vec<_>>(), [chat], "the node outlived the connection that started it");
        assert_eq!(chats[0].state.model.as_deref(), Some("mock/anthropic/claude-sonnet-4.5"), "its default model is the box's own");
        b.call(Request::Stop).await.unwrap();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(!fresh.join("node.sock").exists(), "a stopped node removes its socket");
    });
    let _ = std::fs::remove_dir_all(&root);
}
