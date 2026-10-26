//! Terminals on a machine: started through its chat node, held by its terminal node (manymux's,
//! `serpis-node terminals`), and reached as bytes through `serpis-node agent` the way ssh runs
//! it. They outlive the client that started them. Nothing of the user's is touched: the home
//! and both sockets are throwaway.

use std::path::Path;
use std::time::Duration;

use manymux::client::{Stream, Update};
use manymux::proto::Size;
use serpis_node::client::{Client, Target, terminal_pipe};
use serpis_proto::wire::{Request, Response};

/// Read the session until its screen holds `text`.
async fn until(reader: &mut manymux::client::SessionReader, seen: &mut String, text: &str) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    while !seen.contains(text) {
        match tokio::time::timeout_at(deadline, reader.next()).await {
            Ok(Ok(Update::Output(b) | Update::Screen(b) | Update::History(b))) => seen.push_str(&String::from_utf8_lossy(&b)),
            Ok(Ok(Update::Exited(_) | Update::Disconnected)) => panic!("the terminal went; it said: {seen}"),
            Ok(Ok(_)) => {}
            Ok(Err(e)) => panic!("{e:#}; it said: {seen}"),
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
    let terminals = root.join("t/mm.sock");
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
        let (c, _) = Client::connect(&agent, "test").await.unwrap();
        assert!(names(c.call(Request::Terminals).await.unwrap()).is_empty(), "no terminal node yet: none");
        let err = c.call(Request::NewTerminal { cwd: "~/nowhere".into(), cols: 80, rows: 24, integration: false }).await.unwrap_err();
        assert!(err.to_string().contains("isn't a folder"), "{err}");

        let Response::Terminal { name } = c.call(Request::NewTerminal { cwd: "~/project".into(), cols: 80, rows: 24, integration: true }).await.unwrap() else { panic!() };
        // The bytes go straight to the terminal node, as `ssh <host> serpis-node agent` takes them.
        let pipe = terminal_pipe(&agent).await.unwrap();
        let attached = Stream::from_halves(pipe.read, pipe.write).attach(&name, Size::new(80, 24), 0, false).await.unwrap();
        let mut halves = attached.split();
        halves.writer.send_input(b"pwd; echo made-$((6*7)) \"home=$HOME;\"\r").await.unwrap();
        let mut seen = String::new();
        until(&mut halves.reader, &mut seen, "made-42").await;
        assert!(seen.contains(&*home.join("project").to_string_lossy()), "it starts in the folder asked for: {seen}");
        // The node's home, not the account's: nothing of the user's is read or written.
        until(&mut halves.reader, &mut seen, &format!("home={};", home.display())).await;

        // What it runs shows in the list; at the prompt nothing does.
        halves.writer.send_input(b"sleep 30\r").await.unwrap();
        let mut running = Vec::new();
        for _ in 0..100 {
            running = names(c.call(Request::Terminals).await.unwrap());
            if running.iter().any(|(_, f)| f.first().is_some_and(|p| p.ends_with("sleep"))) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(running.iter().any(|(n, f)| *n == name && f.first().is_some_and(|p| p.ends_with("sleep"))), "{running:?}");
        halves.writer.send_input(b"\x03").await.unwrap();

        // Gone, the client and its chat node: the terminal keeps running, and comes back.
        drop(halves);
        drop(pipe.child);
        c.call(Request::Stop).await.unwrap();
        drop(c);
        tokio::time::sleep(Duration::from_millis(300)).await;
        let (c, _) = Client::connect(&agent, "test-again").await.unwrap();
        assert_eq!(names(c.call(Request::Terminals).await.unwrap()).into_iter().map(|(n, _)| n).collect::<Vec<_>>(), [name.clone()]);
        let pipe = terminal_pipe(&agent).await.unwrap();
        let mut halves = Stream::from_halves(pipe.read, pipe.write).attach(&name, Size::new(80, 24), 100, false).await.unwrap().split();
        let mut seen = String::new();
        until(&mut halves.reader, &mut seen, "made-42").await;

        // Ended: the list says so.
        c.call(Request::KillTerminal { name: name.clone() }).await.unwrap();
        let mut left = vec![(String::new(), Vec::new())];
        for _ in 0..100 {
            left = names(c.call(Request::Terminals).await.unwrap());
            if left.is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(left.is_empty(), "{left:?}");
        c.call(Request::Stop).await.unwrap();
        serpis_node::terminals::stop(&terminals).await.unwrap();
    });
    assert!(!Path::new(&terminals).exists() || std::os::unix::net::UnixStream::connect(&terminals).is_err(), "the terminal node stopped");
    let _ = std::fs::remove_dir_all(&root);
}
