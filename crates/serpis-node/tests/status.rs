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
