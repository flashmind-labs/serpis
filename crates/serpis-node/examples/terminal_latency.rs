//! How long a key takes to come back from a terminal on a machine: `terminal_latency <command
//! that runs serpis-node agent there>...`. Each key gets two writes back a little apart (as an
//! editor redraws), and the time to the second is what counts.

use std::time::{Duration, Instant};

use serpis_node::client::{Client, Target};
use serpis_proto::wire::{Event, Request, Response};

#[tokio::main]
async fn main() {
    let cmd: Vec<String> = std::env::args().skip(1).collect();
    let (c, mut events) = Client::connect(&Target::Command(cmd), "latency").await.unwrap();
    let Response::Terminal { name } = c.call(Request::NewTerminal { cwd: "~".into(), cols: 80, rows: 24, integration: false }).await.unwrap() else { panic!() };
    c.call(Request::AttachTerminal { name: name.clone(), cols: 80, rows: 24, history: false }).await.unwrap();
    let wait = async |events: &mut tokio::sync::mpsc::UnboundedReceiver<Event>, text: &str, seen: &mut String| {
        while !seen.contains(text) {
            match tokio::time::timeout(Duration::from_secs(10), events.recv()).await {
                Ok(Some(Event::TerminalOutput { data, .. })) => seen.push_str(&String::from_utf8_lossy(&data)),
                Ok(Some(_)) => {}
                other => panic!("{other:?}; seen {seen:?}"),
            }
        }
    };
    let mut seen = String::new();
    c.send_terminal(&name, b"stty raw -echo; python3 -u -c \"import os,time\nwhile True:\n  b=os.read(0,1)\n  os.write(1,b'<'+b+b'>'); time.sleep(0.002); os.write(1,b'{'+b+b'}')\"\r");
    // (The prompt and the command line printed first.)
    tokio::time::sleep(Duration::from_secs(2)).await;
    while let Ok(Some(_)) = tokio::time::timeout(Duration::from_millis(200), events.recv()).await {}
    let (mut first, mut second) = (Vec::new(), Vec::new());
    for i in 0..60u8 {
        let k = (b'a' + i % 26) as char;
        seen.clear();
        let at = Instant::now();
        c.send_terminal(&name, k.to_string().as_bytes());
        wait(&mut events, &format!("<{k}>"), &mut seen).await;
        first.push(at.elapsed().as_secs_f64() * 1000.0);
        wait(&mut events, &format!("{{{k}}}"), &mut seen).await;
        second.push(at.elapsed().as_secs_f64() * 1000.0);
        tokio::time::sleep(Duration::from_millis(std::env::var("GAP").ok().and_then(|g| g.parse().ok()).unwrap_or(30))).await;
    }
    for (what, mut v) in [("first write", first), ("second write", second)] {
        v.sort_by(f64::total_cmp);
        println!("{what:12}: median {:.1} ms, p90 {:.1} ms, worst {:.1} ms", v[v.len() / 2], v[v.len() * 9 / 10], v[v.len() - 1]);
    }
    c.call(Request::KillTerminal { name }).await.unwrap();
    c.call(Request::Stop).await.ok();
}
