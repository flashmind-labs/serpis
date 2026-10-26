//! A real machine rebooted mid-turn: its node, a service there (`remote-install-service`), starts
//! at boot and continues the chat. Opt-in, in two runs with a reboot between them, since the
//! reboot is a person's to make:
//!
//!   SERPIS_TEST_REBOOT_HOST=me@box SERPIS_TEST_REBOOT_NODE='~/.serpis/bin/serpis-node-<sha>' \
//!   SERPIS_TEST_REBOOT_MODEL=provider/model SERPIS_TEST_REBOOT_PHASE=before|after \
//!   SERPIS_TEST_REBOOT_STATE=/some/file cargo test -p serpis-node --test reboot -- --nocapture
//!
//! `before` starts a chat on the model given (a mock, for no cost) and leaves it running a long
//! tool; `after` (once the machine is back, and before anything else connects) checks the chat
//! continued, then closes it and deletes its session. Without the variables, nothing runs.

use std::time::Duration;

use serpis_node::client::{Client, Target};
use serpis_proto::Entry;
use serpis_proto::wire::{Event, Request, Response};

fn var(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|v| !v.is_empty())
}

async fn until(events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>, entries: &mut Vec<Entry>, secs: u64, done: impl Fn(&[Entry]) -> bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while !done(entries) {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(Event::Splice { splice, .. })) => splice.apply(entries),
            Ok(Some(_)) => {}
            Ok(None) => panic!("the connection closed; entries: {entries:#?}"),
            Err(_) => panic!("timed out; entries: {entries:#?}"),
        }
    }
}

#[test]
fn a_reboot_mid_turn() {
    let (Some(host), Some(node), Some(phase), Some(state)) = (var("SERPIS_TEST_REBOOT_HOST"), var("SERPIS_TEST_REBOOT_NODE"), var("SERPIS_TEST_REBOOT_PHASE"), var("SERPIS_TEST_REBOOT_STATE")) else {
        eprintln!("skipped: SERPIS_TEST_REBOOT_HOST, _NODE, _PHASE and _STATE aren't all set");
        return;
    };
    let mut target = Target::ssh(&host);
    if let Target::Ssh { command, .. } = &mut target {
        *command = Some(format!("{node} agent"));
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (c, mut events) = Client::connect(&target, "reboot-test").await.unwrap();
        let mut entries = Vec::new();
        match phase.as_str() {
            "before" => {
                let model = var("SERPIS_TEST_REBOOT_MODEL").expect("SERPIS_TEST_REBOOT_MODEL");
                c.call(Request::ReloadModels).await.unwrap();
                let Response::Chat { chat } = c.call(Request::New { cwd: "~".into(), model: Some(model), thinking: None, chat: None }).await.unwrap() else { panic!() };
                c.call(Request::Attach { chat: chat.clone() }).await.unwrap();
                c.call(Request::Prompt { chat: chat.clone(), text: r#"CALL bash {"command": "sleep 600"}"#.into(), images: vec![], follow_up: false, request_id: None }).await.unwrap();
                until(&mut events, &mut entries, 60, |es| es.iter().any(|e| matches!(e, Entry::Tool { result: None, .. }))).await;
                // Noted mid-turn before it can be cut off.
                tokio::time::sleep(Duration::from_secs(2)).await;
                std::fs::write(&state, &chat).unwrap();
                println!("chat {chat} is mid-tool on {host}: reboot it now");
            }
            "after" => {
                let chat = std::fs::read_to_string(&state).unwrap();
                c.call(Request::Attach { chat: chat.clone() }).await.unwrap();
                let says = |es: &[Entry], t: &str| {
                    es.iter().any(|e| match e {
                        Entry::Info { text, .. } => text.contains(t),
                        Entry::Tool { result: Some(r), .. } => serde_json::to_string(r).unwrap().contains(t),
                        _ => false,
                    })
                };
                until(&mut events, &mut entries, 60, |es| says(es, "Continued") && says(es, "Not run to the end")).await;
                for e in &entries {
                    if let Entry::Info { text, .. } = e {
                        println!("note: {text}");
                    }
                }
                let Response::Chats { chats } = c.call(Request::List).await.unwrap() else { panic!() };
                let session = chats.iter().find(|s| s.chat == chat).and_then(|s| s.state.session.clone());
                c.call(Request::Close { chat: chat.clone() }).await.unwrap();
                println!("continued after the reboot; session: {}", session.unwrap_or_default());
            }
            other => panic!("SERPIS_TEST_REBOOT_PHASE is before or after, not {other}"),
        }
    });
}
