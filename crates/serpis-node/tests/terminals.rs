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
    if seen.contains(text) {
        return;
    }
    loop {
        match tokio::time::timeout_at(deadline, events.recv()).await {
            Ok(Some(Event::TerminalOutput { name: n, data })) if n == name => {
                // Only the new text (and what it may finish) is searched: output can be megabytes.
                let from = seen.len().saturating_sub(text.len());
                seen.push_str(&String::from_utf8_lossy(&data));
                if seen.as_bytes()[from..].windows(text.len()).any(|w| w == text.as_bytes()) {
                    return;
                }
            }
            Ok(Some(Event::TerminalExited { name: n, .. })) if n == name => panic!("the terminal ended; it said: {seen}"),
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
    // Its nodes stopped and its folder gone however the test ends (a failed one too).
    let _cleanup = Cleanup(root.clone());
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

        // Typing is quick: a key's echo comes back in a few milliseconds, not a node's tick.
        let mut slowest = Duration::ZERO;
        for i in 0..50 {
            let marker = format!("k{i}x");
            let at = std::time::Instant::now();
            c.send_terminal(&name, marker.as_bytes());
            until(&mut events, &mut seen, &name, &marker).await;
            slowest = slowest.max(at.elapsed());
            // (The line cleared again, so no marker wraps.)
            c.send_terminal(&name, b"\x15");
        }
        c.send_terminal(&name, b"\x15");
        eprintln!("slowest echo of 50 keys: {slowest:?}");
        assert!(slowest < Duration::from_millis(200), "the slowest echo took {slowest:?}");

        // A terminal printing flat out holds nothing else up: another starts, and is typed into,
        // while it prints; and what it printed arrives fast.
        c.send_terminal(&name, b"head -c 20000000 /dev/zero | tr '\\0' 'x'; echo; echo printed-all\r");
        let at = std::time::Instant::now();
        let Response::Terminal { name: other } = c.call(Request::NewTerminal { cwd: "~".into(), cols: 80, rows: 24, integration: false }).await.unwrap() else { panic!() };
        c.call(Request::AttachTerminal { name: other.clone(), cols: 80, rows: 24, history: false }).await.unwrap();
        c.send_terminal(&other, b"echo second-$((1+1))\r");
        let mut second = String::new();
        until(&mut events, &mut second, &other, "second-2").await;
        eprintln!("a second terminal started and echoed while the first printed: {:?}", at.elapsed());
        assert!(at.elapsed() < Duration::from_secs(3), "the second terminal took {:?} while the first printed", at.elapsed());
        until(&mut events, &mut seen, &name, "printed-all").await;
        eprintln!("20 MB printed and received: {:?}", at.elapsed());
        assert!(at.elapsed() < Duration::from_secs(5), "20 MB took {:?}", at.elapsed());
        c.call(Request::KillTerminal { name: other.clone() }).await.unwrap();

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
        let mut left = Vec::new();
        for _ in 0..50 {
            left = names(c.call(Request::Terminals).await.unwrap()).into_iter().map(|(n, _)| n).collect::<Vec<_>>();
            if left.len() == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert_eq!(left, [name.clone()]);
        c.call(Request::AttachTerminal { name: name.clone(), cols: 80, rows: 24, history: true }).await.unwrap();
        // (What it printed last: the 20 MB pushed older output out of what is kept.)
        let mut seen = String::new();
        until(&mut events, &mut seen, &name, "printed-all").await;
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

/// Stops the test's chat node and terminal host (with the test's environment) and removes its
/// folder, when dropped.
struct Cleanup(std::path::PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for what in ["stop", "stop-terminals"] {
            let _ = std::process::Command::new(env!("CARGO_BIN_EXE_serpis-node")).arg(what).status();
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
