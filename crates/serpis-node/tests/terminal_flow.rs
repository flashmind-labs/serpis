//! A client slower than a terminal prints (a slow link): it is sent what fits, its chats are
//! never sent again whole for it, and the terminal's screen catches up once it reads again. The
//! slow client is a raw socket that stops reading while a terminal floods and its chat goes on.
//! Nothing of the user's is touched: home, node and agent are throwaway.

use std::time::Duration;

use serde_json::json;
use serpis_node::client::{Client, Target};
use serpis_proto::wire::{self, Call, Event, Reply, Request, Response, tag};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[test]
fn a_slow_client_gets_the_screen_not_a_backlog() {
    let root = std::env::temp_dir().join(format!("sf-{}", &uuid::Uuid::now_v7().simple().to_string()[24..]));
    let (home, node_dir, agent) = (root.join("home"), root.join("node"), root.join("agent"));
    for d in [home.join("project"), node_dir.clone(), agent.clone()] {
        std::fs::create_dir_all(d).unwrap();
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mock = rt.block_on(serpis_testkit::Mock::start(serpis_testkit::commands()));
    let models = json!({"providers": {"mock": {"baseUrl": format!("{}/api/v1", mock.base), "api": "openai-completions", "apiKey": "x", "compat": {"openRouter": true}, "models": [{"id": "m"}]}}});
    std::fs::write(agent.join("models.json"), models.to_string()).unwrap();
    std::fs::write(node_dir.join("settings.json"), json!({"session-directory": root.join("sessions")}).to_string()).unwrap();
    let socket = node_dir.join("node.sock");
    let terminals = root.join("t/terminals.sock");
    // SAFETY: the only test in this binary, before any thread of ours starts.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("SERPIS_NODE_DIR", &node_dir);
        std::env::set_var("SERPIS_NODE_SOCKET", &socket);
        std::env::set_var("SERPIS_AGENT_DIR", &agent);
        std::env::set_var("SERPIS_TERMINAL_SOCKET", &terminals);
        std::env::set_var("SHELL", "/bin/sh");
    }
    let exe = env!("CARGO_BIN_EXE_serpis-node");
    // Its nodes stopped and its folder gone however the test ends (a failed one too).
    let _cleanup = Cleanup(root.clone());
    rt.block_on(async {
        // A quick client: it starts the node, and drives the chat while the slow one doesn't read.
        let (quick, _) = Client::connect(&Target::Command(vec![exe.into(), "agent".into()]), "quick").await.unwrap();
        let Response::Chat { chat } = quick.call(Request::New { cwd: "~/project".into(), model: Some("mock/m".into()), thinking: None, chat: None }).await.unwrap() else { panic!() };

        // The slow one, frame by frame.
        let stream = tokio::net::UnixStream::connect(&socket).await.unwrap();
        let (mut rd, mut wr) = stream.into_split();
        let mut id = 0;
        let mut call = |r: Request| {
            id += 1;
            wire::encode(tag::CALL, &Call { id, request: r })
        };
        let mut out = Vec::new();
        out.extend(call(Request::Hello { version: wire::VERSION, client: "slow".into() }));
        out.extend(call(Request::Attach { chat: chat.clone() }));
        out.extend(call(Request::NewTerminal { cwd: "~/project".into(), cols: 80, rows: 24, integration: false }));
        wr.write_all(&out).await.unwrap();
        let mut decoder = wire::Decoder::default();
        let mut buf = vec![0u8; 64 * 1024];
        let mut frames: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut name = None;
        while name.is_none() {
            let n = rd.read(&mut buf).await.unwrap();
            decoder.push(&buf[..n]);
            while let Ok(Some((t, body))) = decoder.next() {
                if t == tag::REPLY
                    && let Ok(Reply { response: Response::Terminal { name: n }, .. }) = serde_json::from_slice::<Reply>(&body)
                {
                    name = Some(n);
                }
                frames.push((t, body));
            }
        }
        let name = name.unwrap();
        wr.write_all(&call(Request::AttachTerminal { name: name.clone(), cols: 80, rows: 24, history: false })).await.unwrap();
        wr.write_all(&wire::terminal_frame(&name, b"head -c 30000000 /dev/zero | tr '\\0' x; echo; echo printed-all\r")).await.unwrap();

        // It doesn't read while the terminal floods and the chat goes on.
        tokio::time::sleep(Duration::from_millis(300)).await;
        for i in 0..3 {
            quick.call(Request::Prompt { chat: chat.clone(), text: format!("hello {i}"), images: vec![], follow_up: true, request_id: None }).await.unwrap();
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        tokio::time::sleep(Duration::from_secs(3)).await;

        // It reads again: everything it is sent, until the terminal's screen shows the end.
        let (mut whole, mut said, mut screen) = (0, 0, String::new());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !screen.contains("printed-all") || said < 3 {
            let n = tokio::time::timeout_at(deadline, rd.read(&mut buf)).await.unwrap_or_else(|_| panic!("{said} splices; the screen ends: {:?}", &screen[screen.len().saturating_sub(200)..])).unwrap();
            assert!(n > 0, "the node hung up");
            decoder.push(&buf[..n]);
            while let Ok(Some((t, body))) = decoder.next() {
                match t {
                    tag::EVENT => {
                        if let Ok(Event::Splice { splice, .. }) = serde_json::from_slice::<Event>(&body) {
                            said += 1;
                            whole += usize::from(splice.from == 0);
                        }
                    }
                    tag::TERMINAL => {
                        let (_, bytes) = wire::parse_terminal(&body).unwrap();
                        screen.push_str(&String::from_utf8_lossy(&bytes[bytes.len().saturating_sub(4096)..]));
                        if screen.len() > 1 << 16 {
                            screen.drain(..screen.len() - (1 << 15));
                        }
                    }
                    _ => {}
                }
            }
        }
        // The chat's news came as it happened, never the whole chat again (only its first
        // splice, on attaching, is from the start).
        assert!(said >= 3, "the chat's news reached it: {said} splices");
        assert!(whole <= 1, "the chat was sent whole {whole} times");
        quick.call(Request::Stop).await.unwrap();
        serpis_node::terminals::stop(&terminals).await.unwrap();
    });
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
