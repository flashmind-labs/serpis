//! `serpis-node status`, what the menu bar indicator shows: it reports a real node and terminal
//! host, and looking never starts either.

use std::path::Path;
use std::process::{Child, Command};

use serpis_node::client::{Client, Target};
use serpis_proto::wire::Request;

fn node(root: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_serpis-node"));
    c.env("HOME", root.join("home")).env("SERPIS_NODE_DIR", root.join("n")).env("SERPIS_NODE_SOCKET", root.join("n/n.sock")).env("SERPIS_TERMINAL_SOCKET", root.join("n/t.sock"));
    c
}

fn status(root: &Path) -> serde_json::Value {
    let out = node(root).arg("status").output().unwrap();
    serde_json::from_slice(&out.stdout).unwrap()
}

fn wait_for(what: impl Fn() -> bool) {
    for _ in 0..200 {
        if what() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("it didn't happen in time");
}

#[test]
fn status_reports_what_runs_and_starts_nothing() {
    let root = std::env::temp_dir().join(format!("ss-{}", &uuid::Uuid::now_v7().simple().to_string()[24..]));
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("n")).unwrap();

    let s = status(&root);
    assert_eq!((s["node"].as_bool(), s["terminal_host"].as_bool(), s["indicator"].as_bool()), (Some(false), Some(false), Some(true)));
    assert!(!root.join("n/n.sock").exists() && !root.join("n/t.sock").exists(), "looking started nothing");

    let mut serve: Child = node(&root).arg("serve").spawn().unwrap();
    wait_for(|| status(&root)["node"] == true);
    let s = status(&root);
    assert_eq!((s["chats"].as_u64(), s["working"].as_u64(), s["terminal_host"].as_bool()), (Some(0), Some(0), Some(false)), "{s}");

    let mut terminals: Child = node(&root).arg("terminals").spawn().unwrap();
    wait_for(|| status(&root)["terminal_host"] == true);
    assert_eq!(status(&root)["terminals"].as_u64(), Some(0));

    // A real chat and a real terminal are counted.
    let home = root.join("home").to_string_lossy().into_owned();
    let target = Target::Local(root.join("n/n.sock"));
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (c, _events) = Client::connect(&target, "status-test").await.unwrap();
        c.call(Request::New { cwd: home.clone(), model: None, thinking: None, chat: None }).await.unwrap();
        c.call(Request::NewTerminal { cwd: home.clone(), cols: 80, rows: 24, integration: false }).await.unwrap();
    });
    let s = status(&root);
    assert_eq!((s["chats"].as_u64(), s["working"].as_u64(), s["terminals"].as_u64()), (Some(1), Some(0), Some(1)), "{s}");

    // Off in settings.json, the setting holds with no window to say so.
    std::fs::write(root.join("n/settings.json"), r#"{"status-indicator": false}"#).unwrap();
    assert_eq!(status(&root)["indicator"].as_bool(), Some(false));
    std::fs::remove_file(root.join("n/settings.json")).unwrap();

    assert!(node(&root).arg("stop").status().unwrap().success());
    assert!(node(&root).arg("stop-terminals").status().unwrap().success());
    serve.wait().unwrap();
    terminals.wait().unwrap();
    let s = status(&root);
    assert_eq!((s["node"].as_bool(), s["terminal_host"].as_bool()), (Some(false), Some(false)), "{s}");
    let _ = std::fs::remove_dir_all(&root);
}

/// A client that leaves must take its connection with it: this node has few file handles, sees
/// more clients come and go than it has, and must still serve the next one.
#[test]
fn connections_are_freed_when_clients_leave() {
    let root = std::env::temp_dir().join(format!("ss-{}", &uuid::Uuid::now_v7().simple().to_string()[24..]));
    std::fs::create_dir_all(root.join("home")).unwrap();
    std::fs::create_dir_all(root.join("n")).unwrap();
    let exe = env!("CARGO_BIN_EXE_serpis-node");
    let mut serve = node(&root);
    serve.args(["serve"]);
    let mut limited = Command::new("sh");
    limited.args(["-c", &format!("ulimit -n 64; exec '{exe}' serve")]);
    for (k, v) in serve.get_envs() {
        limited.env(k, v.unwrap());
    }
    let mut server: Child = limited.stderr(std::process::Stdio::null()).stdout(std::process::Stdio::null()).spawn().unwrap();
    wait_for(|| status(&root)["node"] == true);
    let handles = |pid: u32| Command::new("lsof").args(["-n", "-p", &pid.to_string()]).output().map(|o| o.stdout.iter().filter(|b| **b == b'\n').count()).unwrap_or(0);
    let before = handles(server.id());
    for _ in 0..80 {
        status(&root);
    }
    std::thread::sleep(std::time::Duration::from_millis(300));
    let after = handles(server.id());

    let home = root.join("home").to_string_lossy().into_owned();
    let target = Target::Local(root.join("n/n.sock"));
    let rt = tokio::runtime::Runtime::new().unwrap();
    let opened = rt.block_on(async {
        let (c, _events) = tokio::time::timeout(std::time::Duration::from_secs(5), Client::connect(&target, "status-test")).await.map_err(|_| anyhow::anyhow!("connecting timed out"))??;
        tokio::time::timeout(std::time::Duration::from_secs(5), c.call(Request::New { cwd: home.clone(), model: None, thinking: None, chat: None })).await.map_err(|_| anyhow::anyhow!("opening a chat timed out"))?
    });
    assert!(node(&root).arg("stop").status().unwrap().success());
    let _ = server.kill();
    let _ = server.wait();
    let _ = std::fs::remove_dir_all(&root);
    assert!(after <= before + 2, "connections piled up: {before} open files before, {after} after");
    opened.expect("a new client is served");
}
