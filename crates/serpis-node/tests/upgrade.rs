//! A machine running an older node: connecting with a newer build restarts it on that build,
//! but not while a chat there is mid-turn, and its open chats come back. Through a stand-in for
//! ssh; the "older" node is this build under another build id (SERPIS_NODE_BUILD).

use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::time::Duration;

use serde_json::json;
use serpis_node::client::{Client, Stage, Target};
use serpis_node::deploy::{NodeBinary, sha256_file};
use serpis_proto::wire::{Event, Request, Response};

/// Until the chat's state says it is (true) or isn't (false, after answering) running.
async fn wait(events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>, running: bool) {
    loop {
        match tokio::time::timeout(Duration::from_secs(20), events.recv()).await.unwrap() {
            Some(Event::State { state, .. }) if state.running == running && (running || state.usage.total_tokens > 0) => return,
            Some(_) => {}
            None => panic!("closed"),
        }
    }
}

#[test]
fn an_older_node_is_restarted_on_the_new_build_when_idle() {
    let root = std::env::temp_dir().join(format!("sg-{}", &uuid::Uuid::now_v7().simple().to_string()[24..]));
    let home = root.join("home");
    let (agent, work) = (home.join(".serpis/agent"), home.join("proj"));
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::create_dir_all(&work).unwrap();
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mock = rt.block_on(serpis_testkit::Mock::start(serpis_testkit::commands()));
    let models = json!({"providers": {"mock": {"baseUrl": format!("{}/api/v1", mock.base), "api": "openai-completions", "apiKey": "x", "compat": {"openRouter": true}, "models": [{"id": "anthropic/claude-sonnet-4.5"}]}}});
    std::fs::write(agent.join("models.json"), models.to_string()).unwrap();
    let fake_ssh = root.join("ssh");
    std::fs::write(&fake_ssh, "#!/bin/sh\nwhile [ $# -gt 0 ] && [ \"$1\" != \"--\" ]; do shift; done; shift\nexec sh -c \"$*\"\n").unwrap();
    std::fs::set_permissions(&fake_ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let exe = Path::new(env!("CARGO_BIN_EXE_serpis-node"));
    let uname = String::from_utf8(std::process::Command::new("uname").arg("-sm").output().unwrap().stdout).unwrap().trim().to_string();
    let node = NodeBinary { os_arch: uname, path: exe.to_path_buf(), sha256: sha256_file(exe).unwrap() };
    let socket = root.join("n.sock");
    let env = vec![("HOME".to_string(), home.to_string_lossy().into_owned()), ("SERPIS_NODE_SOCKET".to_string(), socket.to_string_lossy().into_owned())];
    let target = Target::Ssh { host: "box".into(), ssh: vec![fake_ssh.to_string_lossy().into_owned()], ssh_args: vec![], env, command: None, deploy: vec![node.clone()], ask: false };

    // The older node, running.
    let mut old = std::process::Command::new(exe)
        .arg("serve")
        .env("HOME", &home)
        .env("SERPIS_NODE_SOCKET", &socket)
        .env("SERPIS_NODE_BUILD", "an-older-build")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    while !socket.exists() {
        std::thread::sleep(Duration::from_millis(20));
    }
    rt.block_on(async {
        // A chat there, mid-turn (its tool sleeps for a while).
        let (c, mut events) = Client::connect(&Target::Local(socket.clone()), "upgrade-test").await.unwrap();
        assert_eq!(c.hello.build, "an-older-build");
        let Response::Chat { chat } = c.call(Request::New { cwd: work.to_string_lossy().into_owned(), model: Some("mock/anthropic/claude-sonnet-4.5".into()), thinking: None }).await.unwrap() else { panic!() };
        c.call(Request::Attach { chat: chat.clone() }).await.unwrap();
        c.call(Request::Prompt { chat: chat.clone(), text: "SLEEP".into(), images: vec![], follow_up: false, request_id: None }).await.unwrap();
        wait(&mut events, true).await;

        // Busy: the newer client keeps it, and knows it's outdated.
        let (busy, _) = Client::connect_with(&target, "upgrade-test", None, |_| {}).await.unwrap();
        assert!(busy.hello.outdated, "a node with a chat mid-turn isn't restarted");
        assert_eq!(busy.hello.build, "an-older-build");
        drop(busy);

        // Idle: restarted on the new build, and the chat is there again.
        c.call(Request::Abort { chat: chat.clone() }).await.unwrap();
        wait(&mut events, false).await;
        drop((c, events));
        let stages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let st = stages.clone();
        let (new, _) = Client::connect_with(&target, "upgrade-test", None, move |s| st.lock().unwrap().push(s)).await.unwrap();
        assert_eq!(new.hello.build, node.sha256, "now the new build runs");
        assert!(!new.hello.outdated);
        assert!(stages.lock().unwrap().contains(&Stage::Upgrading));
        let Response::Chats { chats } = new.call(Request::List).await.unwrap() else { panic!() };
        assert_eq!(chats.iter().map(|c| c.chat.clone()).collect::<Vec<_>>(), [chat], "its open chat came back");
        new.call(Request::Stop).await.unwrap();
    });
    assert!(old.wait().unwrap().success(), "the older node exited by itself");
    let _ = std::fs::remove_dir_all(&root);
}
