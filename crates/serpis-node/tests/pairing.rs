//! Pairing a phone, on a machine: the desktop lets a pairing key in (only to run `serpis-node
//! pair`), the phone's offer reaches the desktop through the node, and the desktop's answer
//! reaches the phone; approved, the phone's key is let in and the pairing key isn't any more.
//! Nothing of the user's is touched: home, node and authorized_keys are throwaway.

use std::io::Write;
use std::time::Duration;

use serpis_node::client::{Client, Target};
use serpis_proto::wire::{Event, Request};

const PAIRING_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIGq3vDqLX3Yv1n0OQ9wq3zvJ6S7xq8hj1yYb2m8Y3cXx";
const PHONE_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAINm9mnRQLkdBuv6CDOCflHSvvxq6w1MpcbvPddiMhtV1";

/// `serpis-node pair <id>` with `offer` on stdin, as the phone's login runs it: what it printed.
fn pair(id: &str, offer: &str) -> std::thread::JoinHandle<serde_json::Value> {
    let (id, offer) = (id.to_string(), offer.to_string());
    std::thread::spawn(move || {
        let mut child = std::process::Command::new(env!("CARGO_BIN_EXE_serpis-node")).args(["pair", &id]).stdin(std::process::Stdio::piped()).stdout(std::process::Stdio::piped()).spawn().unwrap();
        writeln!(child.stdin.take().unwrap(), "{offer}").unwrap();
        let out = child.wait_with_output().unwrap();
        serde_json::from_slice(&out.stdout).unwrap_or_else(|_| serde_json::json!({ "raw": String::from_utf8_lossy(&out.stdout) }))
    })
}

#[test]
fn a_phone_pairs_once_the_desktop_says_so() {
    let root = std::env::temp_dir().join(format!("sp-{}", &uuid::Uuid::now_v7().simple().to_string()[24..]));
    let (home, node_dir) = (root.join("home"), root.join("node"));
    std::fs::create_dir_all(home.join(".ssh")).unwrap();
    std::fs::create_dir_all(&node_dir).unwrap();
    let keys = home.join(".ssh/authorized_keys");
    std::fs::write(&keys, "ssh-rsa AAAAB3Nza me@laptop\n").unwrap();
    // SAFETY: the only test in this binary, before any thread of ours starts.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("SERPIS_NODE_DIR", &node_dir);
        std::env::set_var("SERPIS_NODE_SOCKET", node_dir.join("node.sock"));
        std::env::set_var("SERPIS_TERMINAL_SOCKET", root.join("t.sock"));
    }
    let _cleanup = Cleanup(root.clone());
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let agent = Target::Command(vec![env!("CARGO_BIN_EXE_serpis-node").into(), "agent".into()]);
        let (desktop, mut events) = Client::connect(&agent, "desktop").await.unwrap();

        // A code nobody showed gets nowhere.
        let wrong = pair("nosuchcode", &serde_json::json!({"key": PHONE_KEY, "label": "iPhone"}).to_string());
        let said = tokio::task::spawn_blocking(move || wrong.join().unwrap()).await.unwrap();
        assert!(said["error"].as_str().unwrap_or("").contains("scan a new code"), "{said}");

        desktop.call(Request::StartPairing { pairing: "code1".into(), key: PAIRING_KEY.into(), minutes: 5 }).await.unwrap();
        let text = std::fs::read_to_string(&keys).unwrap();
        let line = text.lines().find(|l| l.contains("serpis-pairing code1")).expect(&text);
        assert!(line.starts_with("restrict,command=\"'") && line.contains("' pair code1\",expiry-time=\""), "only to pair, for a while: {line}");
        assert!(text.starts_with("ssh-rsa AAAAB3Nza me@laptop\n"), "what was there stays: {text}");

        // The phone offers its key; the desktop hears of it, and says yes.
        let phone = pair("code1", &serde_json::json!({"key": format!("{PHONE_KEY} phone"), "label": "Ivan’s iPhone\n"}).to_string());
        let asked = loop {
            match tokio::time::timeout(Duration::from_secs(10), events.recv()).await.expect("asked").expect("open") {
                Event::PairRequested { pairing, key, label } => break (pairing, key, label),
                _ => continue,
            }
        };
        assert_eq!(asked, ("code1".to_string(), PHONE_KEY.to_string(), "Ivan’s iPhone".to_string()));
        desktop.call(Request::AuthorizeKey { key: PHONE_KEY.into(), comment: "serpis phone Ivan’s iPhone".into() }).await.unwrap();
        desktop.call(Request::PairAnswer { pairing: "code1".into(), approved: true }).await.unwrap();
        desktop.call(Request::EndPairing { pairing: "code1".into() }).await.unwrap();
        let said = tokio::task::spawn_blocking(move || phone.join().unwrap()).await.unwrap();
        assert_eq!(said, serde_json::json!({"approved": true}));
        let text = std::fs::read_to_string(&keys).unwrap();
        assert_eq!(text, format!("ssh-rsa AAAAB3Nza me@laptop\n{PHONE_KEY} serpis phone Ivan’s iPhone\n"));

        // Ended, the code is no use; a pairing the desktop declines says no.
        let again = pair("code1", &serde_json::json!({"key": PHONE_KEY}).to_string());
        let said = tokio::task::spawn_blocking(move || again.join().unwrap()).await.unwrap();
        assert!(said["error"].is_string(), "{said}");
        desktop.call(Request::StartPairing { pairing: "code2".into(), key: PAIRING_KEY.into(), minutes: 5 }).await.unwrap();
        let declined = pair("code2", &serde_json::json!({"key": PHONE_KEY}).to_string());
        loop {
            if let Event::PairRequested { .. } = tokio::time::timeout(Duration::from_secs(10), events.recv()).await.expect("asked").expect("open") {
                break;
            }
        }
        desktop.call(Request::PairAnswer { pairing: "code2".into(), approved: false }).await.unwrap();
        let said = tokio::task::spawn_blocking(move || declined.join().unwrap()).await.unwrap();
        assert_eq!(said, serde_json::json!({"approved": false}));
        desktop.call(Request::EndPairing { pairing: "code2".into() }).await.unwrap();
        assert!(!std::fs::read_to_string(&keys).unwrap().contains("serpis-pairing"));
        desktop.call(Request::Stop).await.unwrap();
    });
    let _ = std::fs::remove_dir_all(&root);
}

/// Stops the test's node and removes its folder, when dropped.
struct Cleanup(std::path::PathBuf);

impl Drop for Cleanup {
    fn drop(&mut self) {
        for what in ["stop", "stop-terminals"] {
            let _ = std::process::Command::new(env!("CARGO_BIN_EXE_serpis-node")).arg(what).status();
        }
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
