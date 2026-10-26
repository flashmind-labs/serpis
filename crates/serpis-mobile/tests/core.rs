//! The phone's core end to end: ssh to a machine (a test server on this one, letting in the
//! device's key and running commands here), the node found there (`serpis-node-current`, as
//! the desktop leaves it), a chat run against the mock provider and followed through events.
//! Host keys are trusted only when accepted; a key the machine doesn't take, a changed host
//! key and a machine without serpis say so. Nothing of the user's is touched: the home is
//! throwaway, and the server listens on 127.0.0.1.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use serpis_mobile::{Core, Listener, MobileError};

#[derive(Default)]
struct Heard {
    events: Mutex<Vec<Value>>,
}

impl Listener for Heard {
    fn event(&self, _machine: String, event: String) {
        self.events.lock().unwrap().push(serde_json::from_str(&event).unwrap());
    }
    fn closed(&self, _machine: String, _reason: String) {}
}

impl Heard {
    /// The transcript as the events built it.
    fn entries(&self) -> Vec<serpis_proto::Entry> {
        let mut t = Vec::new();
        for e in self.events.lock().unwrap().iter() {
            if e["kind"] == "splice" {
                let s: serpis_proto::Splice = serde_json::from_value(e["splice"].clone()).unwrap();
                s.apply(&mut t);
            }
        }
        t
    }

    fn idle(&self) -> bool {
        self.events.lock().unwrap().iter().rev().find(|e| e["kind"] == "state").is_some_and(|e| e["state"]["running"] == false)
    }
}

fn until(what: &str, done: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(20);
    while !done() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// The serpis-node built from these sources, as a machine has it after the desktop was there.
fn node_binary() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let status = std::process::Command::new(env!("CARGO")).args(["build", "-q", "-p", "serpis-node"]).current_dir(&root).status().unwrap();
    assert!(status.success());
    root.join("target/debug/serpis-node").canonicalize().unwrap()
}

#[test]
fn a_phone_drives_a_chat_on_a_machine() {
    let node = node_binary();
    let root = std::env::temp_dir().join(format!("sm-{}", std::process::id()));
    let (home, data) = (root.join("home"), root.join("phone"));
    for d in [home.join("project"), home.join(".serpis/bin"), root.join("agent"), root.join("node")] {
        std::fs::create_dir_all(d).unwrap();
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mock = rt.block_on(serpis_testkit::Mock::start(serpis_testkit::commands()));
    let models = json!({"providers": {"mock": {"baseUrl": format!("{}/api/v1", mock.base), "api": "openai-completions", "apiKey": "x", "compat": {"openRouter": true}, "models": [{"id": "m"}]}}});
    std::fs::write(root.join("agent/models.json"), models.to_string()).unwrap();
    std::fs::write(root.join("node/settings.json"), json!({"session-directory": root.join("sessions")}).to_string()).unwrap();
    // What the server runs commands with: the machine's environment, a throwaway one.
    // SAFETY: the only test in this binary, before the server or the core start a thread.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("SERPIS_NODE_DIR", root.join("node"));
        std::env::set_var("SERPIS_NODE_SOCKET", root.join("node/node.sock"));
        std::env::set_var("SERPIS_AGENT_DIR", root.join("agent"));
        std::env::set_var("SERPIS_CONFIG_DIR", root.join("config"));
        std::env::set_var("SERPIS_TERMINAL_SOCKET", root.join("t.sock"));
    }

    let heard = Arc::new(Heard::default());
    let core = Core::new(data.to_string_lossy().into(), None, heard.clone()).unwrap();
    // The app keeps the key; given it back, the core is the same device.
    let again = Core::new(data.to_string_lossy().into(), Some(core.private_key()), Arc::new(Heard::default())).unwrap();
    assert_eq!(again.public_key(), core.public_key());
    let authorized = russh::keys::PublicKey::from_openssh(&core.public_key()).unwrap();
    let sshd = rt.block_on(serpis_testkit::sshd::start(authorized)).unwrap();
    let address = format!("me@127.0.0.1:{}", sshd.port);

    // A machine never seen: its key is shown, not trusted.
    let Err(MobileError::NewHost { fingerprint, .. }) = futures::executor::block_on(core.connect("box".into(), address.clone())) else { panic!("a new host is asked about") };
    assert_eq!(fingerprint, sshd.host_key.fingerprint(Default::default()).to_string());
    core.accept_host(address.clone(), fingerprint).unwrap();

    // Reached, but serpis was never there.
    match futures::executor::block_on(core.connect("box".into(), address.clone())) {
        Err(MobileError::NoNode { message }) => assert!(message.contains("not set up"), "{message}"),
        other => panic!("{other:?}"),
    }
    std::os::unix::fs::symlink(&node, home.join(".serpis/bin/serpis-node-current")).unwrap();

    let hello: Value = serde_json::from_str(&futures::executor::block_on(core.connect("box".into(), address.clone())).unwrap()).unwrap();
    assert_eq!(hello["home"], home.to_string_lossy().as_ref());
    assert!(core.is_connected("box".into()));

    // A chat there, followed through its events.
    let call = |r: Value| -> Result<Value, MobileError> { futures::executor::block_on(core.call("box".into(), r.to_string())).map(|a: String| serde_json::from_str(&a).unwrap()) };
    let chat = call(json!({"op": "new", "cwd": "~/project", "model": "mock/m"})).unwrap()["chat"].as_str().unwrap().to_string();
    call(json!({"op": "attach", "chat": chat})).unwrap();
    call(json!({"op": "prompt", "chat": chat, "text": "hello from the phone", "images": []})).unwrap();
    until("the answer", || heard.idle() && heard.entries().iter().any(|e| matches!(e, serpis_proto::Entry::Assistant { .. })));
    let t = heard.entries();
    assert!(matches!(&t[0], serpis_proto::Entry::User { text, .. } if text == "hello from the phone"), "{t:#?}");
    let sessions = call(json!({"op": "sessions"})).unwrap();
    assert_eq!(sessions["sessions"].as_array().unwrap().len(), 1, "{sessions}");
    // An answer the node refuses is an error with its reason.
    let Err(MobileError::Failed { message }) = call(json!({"op": "attach", "chat": "nope"})) else { panic!() };
    assert!(!message.is_empty());

    // Another device's key isn't let in.
    let stranger = Core::new(root.join("stranger").to_string_lossy().into(), None, Arc::new(Heard::default())).unwrap();
    stranger.accept_host(address.clone(), sshd.host_key.fingerprint(Default::default()).to_string()).unwrap();
    assert!(matches!(futures::executor::block_on(stranger.connect("box".into(), address.clone())), Err(MobileError::KeyRefused { .. })));

    // A machine whose host key isn't the one accepted is refused, saying so.
    let other = rt.block_on(serpis_testkit::sshd::start(russh::keys::PublicKey::from_openssh(&core.public_key()).unwrap())).unwrap();
    let moved = format!("me@127.0.0.1:{}", other.port);
    core.accept_host(moved.clone(), "SHA256:somebody-else".into()).unwrap();
    assert!(matches!(futures::executor::block_on(core.connect("box2".into(), moved)), Err(MobileError::HostKeyChanged { .. })));

    call(json!({"op": "stop"})).unwrap();
    core.disconnect("box".into());
    drop((core, again, stranger));
    let _ = std::fs::remove_dir_all(&root);
}
