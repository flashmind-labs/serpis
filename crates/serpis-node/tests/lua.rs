//! The config's chat side runs in the node: the same init.lua a window loads, whose hooks,
//! tools and settings shape the node's chats, and tools a window lends to the chats it shows.

use std::path::Path;
use std::time::Duration;

use serde_json::json;
use serpis_node::client::{Client, Target};
use serpis_proto::Entry;
use serpis_proto::wire::{Event, Request, Response, ToolSpec};
use tokio::sync::mpsc::UnboundedReceiver;

/// Events until `done` says so: what it was given, in order.
async fn until(events: &mut UnboundedReceiver<Event>, seen: &mut Vec<Event>, done: impl Fn(&[Event]) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !done(seen) {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(e)) => seen.push(e),
            Ok(None) => panic!("the node closed the stream"),
            Err(_) => panic!("timed out; events: {seen:#?}"),
        }
    }
}

fn transcript(seen: &[Event]) -> Vec<Entry> {
    let mut t = Vec::new();
    for e in seen {
        if let Event::Splice { splice, .. } = e {
            splice.clone().apply(&mut t);
        }
    }
    t
}

fn idle_after(seen: &[Event], n: usize) -> bool {
    let tools = transcript(seen).len() >= n;
    tools && matches!(seen.iter().rev().find_map(|e| if let Event::State { state, .. } = e { Some(state) } else { None }), Some(s) if !s.running)
}

fn tool_text(t: &[Entry], name: &str) -> String {
    t.iter()
        .find_map(|e| match e {
            Entry::Tool { name: n, result: Some(r), .. } if n == name => Some(serde_json::to_string(r).unwrap()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no {name} result in {t:#?}"))
}

fn setup(root: &Path, mock: &str) {
    for d in ["home/project", "agent", "node", "config/extensions"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    let models = json!({"providers": {"mock": {"baseUrl": format!("{mock}/api/v1"), "api": "openai-completions", "apiKey": "x", "compat": {"openRouter": true}, "models": [{"id": "m"}]}}});
    std::fs::write(root.join("agent/models.json"), models.to_string()).unwrap();
    std::fs::write(root.join("node/settings.json"), json!({"session-directory": root.join("sessions")}).to_string()).unwrap();
    // A window's config, as a user writes it: window parts and chat parts together.
    std::fs::write(
        root.join("config/init.lua"),
        r#"
        serpis.keymap.set("global", "s-k", function() serpis.message("window only") end)
        serpis.command.define("hello", function() end, "A window command.")
        serpis.agent.model("mock/m")
        serpis.hook.add("before-prompt", function(e)
          if e.text == "no" then return { block = "not on " .. serpis.where } end
        end)
        serpis.hook.add("before-tool-call", function(e)
          if e.name == "bash" then return { block = "no bash here" } end
        end)
        serpis.agent.tool({
          name = "stamp",
          description = "Stamp a value.",
          execute = function(a) return "stamp " .. a.v .. " in " .. serpis.chat.info().title end,
        })
        serpis.hook.add("agent-finished", function(e)
          serpis.chat.append(e.buffer, "seen", { n = #serpis.chat.entries(e.buffer, "seen") + 1 })
        end)
        "#,
    )
    .unwrap();
    std::fs::write(root.join("config/extensions/broken.lua"), "error('this one is broken')").unwrap();
}

#[test]
fn the_config_shapes_the_chats_where_they_run() {
    let root = std::env::temp_dir().join(format!("sl-{}", &uuid::Uuid::now_v7().simple().to_string()[24..]));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mock = rt.block_on(serpis_testkit::Mock::start(serpis_testkit::commands()));
    setup(&root, &mock.base);
    // SAFETY: the only test in this binary, before any thread of ours starts.
    unsafe { std::env::set_var("HOME", root.join("home")) };
    let socket = root.join("node/node.sock");
    let config = serpis_node::server::Config { socket: socket.clone(), dir: root.join("node"), agent_dir: Some(root.join("agent")), config_dir: Some(root.join("config")) };
    let node = serpis_node::server::start(config, rt.handle().clone()).unwrap();
    rt.block_on(async {
        let (a, mut a_events) = Client::connect(&Target::Local(socket.clone()), "test").await.unwrap();
        // A broken extension is reported, and the rest of the config still works.
        let notes = match a.call(Request::Hello { version: serpis_proto::wire::VERSION, client: "t".into() }).await.unwrap() {
            Response::Hello { notes, .. } => notes,
            other => panic!("{other:?}"),
        };
        assert!(notes.len() == 1 && notes[0].contains("this one is broken"), "{notes:?}");

        // The config's default model.
        let chat = match a.call(Request::New { cwd: "~/project".into(), model: None, thinking: None, chat: None }).await.unwrap() {
            Response::Chat { chat } => chat,
            other => panic!("{other:?}"),
        };
        a.call(Request::Attach { chat: chat.clone() }).await.unwrap();
        let mut seen = Vec::new();
        until(&mut a_events, &mut seen, |s| s.iter().any(|e| matches!(e, Event::State { state, .. } if state.model.as_deref() == Some("mock/m")))).await;

        // Its hooks decide.
        let prompt = |text: &str| Request::Prompt { chat: chat.clone(), text: text.into(), images: vec![], follow_up: false, request_id: None };
        let err = a.call(prompt("no")).await.unwrap_err();
        assert!(err.to_string().contains("not on node"), "{err}");

        // Its tools run here, reading the chat; tool hooks guard the built-in ones.
        a.call(prompt(r#"CALL stamp {"v": "7"}"#)).await.unwrap();
        until(&mut a_events, &mut seen, |s| idle_after(s, 4)).await;
        assert!(tool_text(&transcript(&seen), "stamp").contains(r#"stamp 7 in CALL stamp"#), "{:#?}", transcript(&seen));
        a.call(prompt(r#"CALL bash {"command": "echo hi"}"#)).await.unwrap();
        until(&mut a_events, &mut seen, |s| idle_after(s, 7)).await;
        assert!(tool_text(&transcript(&seen), "bash").contains("no bash here"), "{:#?}", transcript(&seen));

        // What hooks append is in the chat's state for clients.
        until(&mut a_events, &mut seen, |s| s.iter().any(|e| matches!(e, Event::State { state, .. } if state.custom.iter().filter(|(k, _)| k == "seen").count() == 2))).await;

        // A window lends a tool to the chats it shows: the agent calls it there.
        let spec = ToolSpec { name: "window_tool".into(), description: "Runs in the window.".into(), parameters: json!({"type": "object", "properties": {}}) };
        a.call(Request::Lend { tools: vec![spec] }).await.unwrap();
        a.call(prompt("CALL window_tool {}")).await.unwrap();
        until(&mut a_events, &mut seen, |s| s.iter().any(|e| matches!(e, Event::ToolCall { .. }))).await;
        let Some(Event::ToolCall { call, name, .. }) = seen.iter().find(|e| matches!(e, Event::ToolCall { .. })).cloned() else { unreachable!() };
        assert_eq!(name, "window_tool");
        a.call(Request::ToolDone { call, text: "from the window".into(), is_error: false }).await.unwrap();
        until(&mut a_events, &mut seen, |s| idle_after(s, 10)).await;
        assert!(tool_text(&transcript(&seen), "window_tool").contains("from the window"));

        // A chat working when its last view closes keeps working, and closes once it is done.
        a.call(prompt("CALL window_tool {}")).await.unwrap();
        until(&mut a_events, &mut seen, |s| s.iter().filter(|e| matches!(e, Event::ToolCall { .. })).count() == 2).await;
        let Some(Event::ToolCall { call, .. }) = seen.iter().rev().find(|e| matches!(e, Event::ToolCall { .. })).cloned() else { unreachable!() };
        a.call(Request::Detach { chat: chat.clone(), close: true }).await.unwrap();
        let Response::Chats { chats } = a.call(Request::List).await.unwrap() else { panic!() };
        assert_eq!(chats.len(), 1, "still working: still open");
        a.call(Request::ToolDone { call, text: "late".into(), is_error: false }).await.unwrap();
        let mut closed = false;
        for _ in 0..100 {
            let Response::Chats { chats } = a.call(Request::List).await.unwrap() else { panic!() };
            if chats.is_empty() {
                closed = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(closed, "done, and shown nowhere: closed");
        a.call(Request::Resume { session: root.join("sessions").to_string_lossy().into_owned() }).await.unwrap_err();
        let Response::Sessions { sessions } = a.call(Request::Sessions { limit: None }).await.unwrap() else { panic!() };
        let Response::Chat { chat: again } = a.call(Request::Resume { session: sessions[0].path.clone() }).await.unwrap() else { panic!() };
        assert_eq!(again, chat, "its session resumes it");
        a.call(Request::Attach { chat: chat.clone() }).await.unwrap();

        // The window's settings win over the config's while it runs; reloading reads the files again.
        a.call(Request::SetVars { vars: serde_json::Map::from_iter([("agent-model".to_string(), json!("mock/none"))]) }).await.unwrap();
        std::fs::remove_file(root.join("config/extensions/broken.lua")).unwrap();
        match a.call(Request::Reload).await.unwrap() {
            Response::Reloaded { errors } => assert!(errors.is_empty(), "{errors:?}"),
            other => panic!("{other:?}"),
        }
        // Closing the view of an idle chat nobody else shows closes it; its session stays.
        a.call(Request::Detach { chat: chat.clone(), close: true }).await.unwrap();
        let Response::Chats { chats } = a.call(Request::List).await.unwrap() else { panic!() };
        assert!(chats.is_empty(), "{chats:?}");
        let Response::Sessions { sessions } = a.call(Request::Sessions { limit: None }).await.unwrap() else { panic!() };
        assert!(sessions.len() == 1 && !sessions[0].open, "{sessions:?}");
    });
    node.stop();
    let _ = std::fs::remove_dir_all(&root);
}
