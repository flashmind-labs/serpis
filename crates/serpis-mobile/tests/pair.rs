//! Pairing, the phone's side: a desktop shares a machine (its node runs here, reached by a real
//! ssh server in the test); the phone reads the link, trusts the host key it carries, logs in
//! with the pairing key, offers its own key, and is told yes once the desktop says so. Nothing
//! of the user's is touched: home, node and phone data are throwaway.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine;
use russh::keys::ssh_key::rand_core::UnwrapErr;
use russh::keys::{Algorithm, PrivateKey};
use serde_json::{Value, json};
use serpis_mobile::{Core, Listener, MobileError};
use serpis_node::client::{Client, Target};
use serpis_proto::wire::{Event, Request};

struct Quiet;
impl Listener for Quiet {
    fn event(&self, _machine: String, _event: String) {}
    fn closed(&self, _machine: String, _reason: String) {}
}

/// The link a desktop shows, for one machine.
fn link(id: &str, key: &PrivateKey, until: u64, host: &str, port: u16, host_key: &str) -> String {
    let pem = key.to_openssh(russh::keys::ssh_key::LineEnding::LF).unwrap();
    let body: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
    let v = json!({"v": 1, "p": id, "k": body, "x": until, "m": [{"n": "box", "h": host, "p": port, "u": "me", "k": [host_key]}]});
    format!("serpis://pair#{}", base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(v.to_string()))
}

#[test]
fn a_phone_pairs_through_a_machine_the_desktop_shares() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    assert!(std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "serpis-node"]).current_dir(&root).status().unwrap().success());
    let node = root.join("target/debug/serpis-node").canonicalize().unwrap();
    let dir = std::env::temp_dir().join(format!("smp-{}", std::process::id()));
    let home = dir.join("home");
    std::fs::create_dir_all(home.join(".serpis/bin")).unwrap();
    std::fs::create_dir_all(dir.join("node")).unwrap();
    std::os::unix::fs::symlink(&node, home.join(".serpis/bin/serpis-node-current")).unwrap();
    // SAFETY: the only test in this binary, before the server or the core start a thread.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("SERPIS_NODE_DIR", dir.join("node"));
        std::env::set_var("SERPIS_NODE_SOCKET", dir.join("node/node.sock"));
        std::env::set_var("SERPIS_TERMINAL_SOCKET", dir.join("t.sock"));
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let (desktop, mut events) = rt.block_on(Client::connect(&Target::Command(vec![node.to_string_lossy().into(), "agent".into()]), "desktop")).unwrap();

    // The desktop's pairing key, let in on the machine (by the test's ssh server alone; the
    // node writes it to authorized_keys as a real sshd would read it).
    let pairing = PrivateKey::random(&mut UnwrapErr(getrandom::SysRng), Algorithm::Ed25519).unwrap();
    let sshd = rt.block_on(serpis_testkit::sshd::start(pairing.public_key().clone())).unwrap();
    let public = pairing.public_key().to_openssh().unwrap();
    rt.block_on(desktop.call(Request::StartPairing { pairing: "code1".into(), key: public, minutes: 5 })).unwrap();
    let host_key = sshd.host_key.to_openssh().unwrap();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();

    let phone = Core::new(dir.join("phone").to_string_lossy().into(), None, Arc::new(Quiet)).unwrap();
    let phone_key = phone.public_key();

    // A code past its time isn't tried.
    let old = link("code1", &pairing, now - 10, "127.0.0.1", sshd.port, &host_key);
    let Err(MobileError::Failed { message }) = futures::executor::block_on(phone.pair(old, "iPhone".into())) else { panic!("expired") };
    assert!(message.contains("expired"), "{message}");

    // The phone pairs; the desktop hears it and says yes.
    let code = link("code1", &pairing, now + 300, "127.0.0.1", sshd.port, &host_key);
    let phone = Arc::new(phone);
    let p = phone.clone();
    let pairing_done = std::thread::spawn(move || futures::executor::block_on(p.pair(code, "iPhone".into())));
    let (key, label) = rt.block_on(async {
        loop {
            match tokio::time::timeout(Duration::from_secs(20), events.recv()).await.expect("the desktop is asked").expect("open") {
                Event::PairRequested { pairing, key, label } if pairing == "code1" => break (key, label),
                _ => continue,
            }
        }
    });
    assert_eq!((key.as_str(), label.as_str()), (phone_key.split_whitespace().take(2).collect::<Vec<_>>().join(" ").as_str(), "iPhone"));
    rt.block_on(async {
        desktop.call(Request::AuthorizeKey { key: key.clone(), comment: "serpis phone iPhone".into() }).await.unwrap();
        desktop.call(Request::PairAnswer { pairing: "code1".into(), approved: true }).await.unwrap();
        desktop.call(Request::EndPairing { pairing: "code1".into() }).await.unwrap();
    });
    let machines: Value = serde_json::from_str(&pairing_done.join().unwrap().unwrap()).unwrap();
    let address = format!("me@127.0.0.1:{}", sshd.port);
    assert_eq!(machines, json!([{"name": "box", "address": address}]));
    let keys = std::fs::read_to_string(home.join(".ssh/authorized_keys")).unwrap();
    assert!(keys.contains(&format!("{key} serpis phone iPhone")) && !keys.contains("serpis-pairing"), "{keys}");

    // The machine's host key came with the code: the phone trusts it without asking (its own
    // key isn't let in by this test's server, which is the next thing a connection meets).
    assert!(matches!(futures::executor::block_on(phone.connect("box".into(), address)), Err(MobileError::KeyRefused { .. })));

    rt.block_on(desktop.call(Request::Stop)).unwrap();
    let _ = std::process::Command::new(&node).arg("stop-terminals").status();
    drop((phone, desktop));
    let _ = std::fs::remove_dir_all(&dir);
}
