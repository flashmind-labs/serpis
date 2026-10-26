//! Terminals on a machine, over the one connection a client has to its node (here through
//! `serpis-node agent`, the way ssh runs it): started, attached, typed into and read; held by
//! the terminal host, so they outlive the client and the chat node, and come back, screen and
//! all. Nothing of the user's is touched: the home and both sockets are throwaway.

use std::time::Duration;

use serpis_node::client::{Client, Target};
use serpis_proto::wire::{Event, Request, Response};
use tokio::sync::mpsc::UnboundedReceiver;

/// What terminal `name` printed, read until it holds `text`.
async fn until(events: &mut UnboundedReceiver<Event>, seen: &mut String, name: &str, text: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !seen.contains(text) {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(Event::TerminalOutput { name: n, data })) if n == name => seen.push_str(&String::from_utf8_lossy(&data)),
            Ok(Some(Event::TerminalExited { .. })) => panic!("the terminal ended; it said: {seen}"),
            Ok(Some(_)) => {}
            Ok(None) => panic!("the connection closed; it said: {seen}"),
            Err(_) => panic!("no {text:?} in: {seen}"),
        }
    }
}

fn names(r: Response) -> Vec<(String, Vec<String>)> {
    match r {
        Response::Terminals { terminals } => terminals.into_iter().map(|t| (t.name, t.foreground)).collect(),
        other => panic!("{other:?}"),
    }
}

#[test]
fn terminals_outlive_their_clients() {
    let root = std::env::temp_dir().join(format!("st-{}", &uuid::Uuid::now_v7().simple().to_string()[24..]));
    let (home, node_dir) = (root.join("home"), root.join("node"));
    std::fs::create_dir_all(home.join("project")).unwrap();
    std::fs::create_dir_all(&node_dir).unwrap();
    let terminals = root.join("t/terminals.sock");
    // SAFETY: the only test in this binary, before any thread of ours starts.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("SERPIS_NODE_DIR", &node_dir);
        std::env::set_var("SERPIS_TERMINAL_SOCKET", &terminals);
        // A shell that reads nothing of the user's.
        std::env::set_var("SHELL", "/bin/sh");
    }
    let exe = env!("CARGO_BIN_EXE_serpis-node");
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let agent = Target::Command(vec![exe.into(), "agent".into()]);
        let (c, mut events) = Client::connect(&agent, "test").await.unwrap();
        assert!(names(c.call(Request::Terminals).await.unwrap()).is_empty(), "none yet");
        let err = c.call(Request::NewTerminal { cwd: "~/nowhere".into(), cols: 80, rows: 24, integration: false }).await.unwrap_err();
        assert!(err.to_string().contains("isn't a folder"), "{err}");

        let Response::Terminal { name } = c.call(Request::NewTerminal { cwd: "~/project".into(), cols: 80, rows: 24, integration: true }).await.unwrap() else { panic!() };
        c.call(Request::AttachTerminal { name: name.clone(), cols: 80, rows: 24, history: false }).await.unwrap();
        c.send_terminal(&name, b"pwd; echo made-$((6*7)) \"home=$HOME;\"\r");
        let mut seen = String::new();
        until(&mut events, &mut seen, &name, "made-42").await;
        assert!(seen.contains(&*home.join("project").to_string_lossy()), "it starts in the folder asked for: {seen}");
        // The node's home, not the account's: nothing of the user's is read or written.
        until(&mut events, &mut seen, &name, &format!("home={};", home.display())).await;

        // What runs in it shows in the list; at the prompt nothing does.
        c.send_terminal(&name, b"sleep 30\r");
        let mut running = Vec::new();
        for _ in 0..100 {
            running = names(c.call(Request::Terminals).await.unwrap());
            if running.iter().any(|(_, f)| f.first().is_some_and(|p| p.ends_with("sleep"))) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(running.iter().any(|(n, f)| *n == name && f.first().is_some_and(|p| p.ends_with("sleep"))), "{running:?}");
        c.send_terminal(&name, b"\x03clear; echo on-the-screen\r");
        until(&mut events, &mut seen, &name, "on-the-screen\r").await;

        // The client and its chat node gone: the terminal keeps running, and comes back with what
        // it printed and its screen.
        c.call(Request::Stop).await.unwrap();
        drop((c, events));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let (c, mut events) = Client::connect(&agent, "test-again").await.unwrap();
        assert_eq!(names(c.call(Request::Terminals).await.unwrap()).into_iter().map(|(n, _)| n).collect::<Vec<_>>(), [name.clone()]);
        c.call(Request::AttachTerminal { name: name.clone(), cols: 80, rows: 24, history: true }).await.unwrap();
        let mut seen = String::new();
        until(&mut events, &mut seen, &name, "made-42").await;
        until(&mut events, &mut seen, &name, "on-the-screen").await;

        // Ended: it says so, and the list does.
        c.call(Request::KillTerminal { name: name.clone() }).await.unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        loop {
            match tokio::time::timeout_at(deadline, events.recv()).await.expect("an end") {
                Some(Event::TerminalExited { name: n, .. }) if n == name => break,
                Some(_) => {}
                None => panic!("closed"),
            }
        }
        assert!(names(c.call(Request::Terminals).await.unwrap()).is_empty());
        c.call(Request::Stop).await.unwrap();
        serpis_node::terminals::stop(&terminals).await.unwrap();
    });
    assert!(std::os::unix::net::UnixStream::connect(&terminals).is_err(), "the terminal host stopped");
    let _ = std::fs::remove_dir_all(&root);
}
