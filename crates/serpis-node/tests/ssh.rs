//! A node over real ssh (opt-in: `scripts/ssh-check.sh [host]`, default localhost).
//!
//! The node on the other side runs in a throwaway home with throwaway node and agent
//! directories (set on its command line), so it never sees the account's real sessions,
//! settings or keys; host keys go to a known-hosts file of the test's own. On localhost the
//! chat runs for real against the mock provider; another host can't reach the mock, so there
//! the test stops after the node answered and opened a chat.
//!
//! Needs ssh access that works without a prompt (a key in authorized_keys, or rayfish's mesh
//! SSH), and on another host `SERPIS_SSH_TEST_NODE`: where serpis-node is there.

use std::time::Duration;

use serde_json::json;
use serpis_node::client::{Client, Target};
use serpis_proto::Entry;
use serpis_proto::wire::{Event, Request, Response};

#[test]
#[ignore = "uses real ssh: scripts/ssh-check.sh"]
fn a_node_over_real_ssh() {
    let host = std::env::var("SERPIS_SSH_TEST_HOST").unwrap_or_else(|_| "localhost".into());
    let local = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
    let node = std::env::var("SERPIS_SSH_TEST_NODE").unwrap_or_else(|_| env!("CARGO_BIN_EXE_serpis-node").to_string());
    // On the far side: /tmp there (it exists everywhere and keeps the socket path short).
    let id = &uuid::Uuid::now_v7().simple().to_string()[24..];
    let far = format!("/tmp/snssh-{id}");
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mock = rt.block_on(serpis_testkit::Mock::start(serpis_testkit::commands()));
    // This side: the known-hosts file. On localhost, the far side's files are made from here too.
    let near = std::env::temp_dir().join(format!("snssh-{id}"));
    std::fs::create_dir_all(&near).unwrap();
    if local {
        let models = json!({"providers": {"mock": {"baseUrl": format!("{}/api/v1", mock.base), "api": "openai-completions", "apiKey": "x", "compat": {"openRouter": true}, "models": [{"id": "anthropic/claude-sonnet-4.5"}]}}});
        for d in ["home/proj", "agent", "node"] {
            std::fs::create_dir_all(format!("{far}/{d}")).unwrap();
        }
        std::fs::write(format!("{far}/agent/models.json"), models.to_string()).unwrap();
        std::fs::write(format!("{far}/agent/auth.json"), "{}").unwrap();
        std::fs::write(format!("{far}/node/settings.json"), json!({"session-directory": format!("{far}/sessions")}).to_string()).unwrap();
        std::fs::write(format!("{far}/home/proj/f0.txt"), "zero over ssh\n").unwrap();
        std::fs::write(format!("{far}/home/proj/f1.txt"), "one over ssh\n").unwrap();
    }
    let prepare = if local { String::new() } else { format!("mkdir -p {far}/home/proj {far}/agent {far}/node && ") };
    let command = format!("{prepare}env HOME={far}/home SERPIS_NODE_DIR={far}/node SERPIS_NODE_SOCKET={far}/node/node.sock SERPIS_AGENT_DIR={far}/agent {node} agent");
    let Target::Ssh { mut ssh_args, .. } = Target::ssh(&host) else { unreachable!() };
    ssh_args.extend(["-o".into(), format!("UserKnownHostsFile={}", near.join("known_hosts").display()), "-o".into(), "StrictHostKeyChecking=accept-new".into()]);
    let target = Target::Ssh { host: host.clone(), command, ssh_args };

    rt.block_on(async {
        let (c, mut events) = Client::connect(&target, "ssh-check").await.unwrap_or_else(|e| panic!("connecting over ssh to {host}: {e:#}"));
        println!("node on {host}: {} (serpis-node {})", c.hello.host, c.hello.serpis);
        assert_eq!(c.hello.home, format!("{far}/home"), "the node runs in the throwaway home");
        let Response::Chat { chat } = c.call(Request::New { cwd: "~/proj".into(), model: None, thinking: None }).await.unwrap() else { panic!() };
        c.call(Request::Attach { chat: chat.clone() }).await.unwrap();
        if local {
            c.call(Request::Prompt { chat: chat.clone(), text: "READ 2".into(), images: vec![], follow_up: false, request_id: Some("ssh-1".into()) }).await.unwrap();
            let mut entries: Vec<Entry> = Vec::new();
            let mut idle = false;
            let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
            while !(idle && entries.len() >= 5) {
                match tokio::time::timeout_at(deadline, events.recv()).await.expect("the chat finished over ssh") {
                    Some(Event::Splice { splice, .. }) => splice.apply(&mut entries),
                    Some(Event::State { state, .. }) => idle = !state.running && state.usage.total_tokens > 0,
                    Some(_) => {}
                    None => panic!("the ssh connection closed"),
                }
            }
            let tool = serde_json::to_string(&entries[2]).unwrap();
            assert!(tool.contains("zero over ssh"), "the tool ran on the far side: {tool}");
            println!("a chat ran over ssh: {} entries", entries.len());
        }
        c.call(Request::Stop).await.unwrap();
    });
    if local {
        std::thread::sleep(Duration::from_millis(300));
        assert!(!std::path::Path::new(&format!("{far}/node/node.sock")).exists(), "the node stopped");
        let _ = std::fs::remove_dir_all(&far);
    }
    let _ = std::fs::remove_dir_all(&near);
}
