//! Setting a machine up from another: what it reports having, and what `Setup` adds. A machine
//! with pi's config keeps it (serpis reads pi's files when it has none of its own), only what it
//! lacks is added, and files with keys are its user's alone.

use std::os::unix::fs::PermissionsExt;

use serde_json::{Value, json};
use serpis_node::client::{Client, Target};
use serpis_proto::wire::{Request, Response};

#[test]
fn a_machine_gets_what_it_lacks_and_keeps_what_it_has() {
    let root = std::env::temp_dir().join(format!("su-{}", &uuid::Uuid::now_v7().simple().to_string()[24..]));
    let (home, node_dir) = (root.join("home"), root.join("node"));
    let pi = home.join(".pi/agent");
    std::fs::create_dir_all(&pi).unwrap();
    std::fs::create_dir_all(&node_dir).unwrap();
    // The machine has pi set up with a provider of its own.
    std::fs::write(pi.join("models.json"), json!({"providers": {"theirs": {"baseUrl": "http://127.0.0.1:9/v1", "api": "openai-completions", "apiKey": "t", "models": [{"id": "m"}]}}}).to_string()).unwrap();
    std::fs::write(pi.join("auth.json"), json!({"openai": {"type": "api_key", "key": "their-key"}}).to_string()).unwrap();
    // SAFETY: the only test in this binary, before any thread of ours starts.
    unsafe { std::env::set_var("HOME", &home) };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let socket = node_dir.join("n.sock");
    let node = serpis_node::server::start(serpis_node::server::Config { socket: socket.clone(), dir: node_dir.clone(), agent_dir: Some(home.join(".serpis/agent")) }, rt.handle().clone()).unwrap();
    rt.block_on(async {
        let (c, _) = Client::connect(&Target::Local(socket.clone()), "setup-test").await.unwrap();
        let mut providers = c.hello.setup.providers.clone();
        providers.sort();
        assert_eq!(providers, ["openai", "theirs"], "pi's providers count");
        assert_eq!(c.hello.setup.sources, ["~/.pi/agent"]);

        let mine = |key: &str| json!({"type": "api_key", "key": key});
        let auth = serde_json::Map::from_iter([("openrouter".to_string(), mine("my-key")), ("openai".to_string(), mine("not-theirs"))]);
        let models = json!({"providers": {"mine": {"baseUrl": "http://127.0.0.1:9/v1", "api": "openai-completions", "models": [{"id": "x"}]}, "theirs": {"baseUrl": "http://elsewhere"}}});
        let settings = serde_json::Map::from_iter([("agent-model".to_string(), json!("openrouter/some/model"))]);
        let Response::Setup { mut added } = c.call(Request::Setup { auth: auth.clone(), models: Some(models.clone()), settings: settings.clone() }).await.unwrap() else { panic!() };
        added.sort();
        assert_eq!(added, ["auth:openrouter", "models:mine", "settings:agent-model"], "only what it lacked");

        let own = home.join(".serpis/agent");
        let read = |f: &str| serde_json::from_str::<Value>(&std::fs::read_to_string(own.join(f)).unwrap()).unwrap();
        let auth_now = read("auth.json");
        assert_eq!(auth_now["openai"]["key"], "their-key", "its own key stays");
        assert_eq!(auth_now["openrouter"]["key"], "my-key");
        assert!(read("models.json")["providers"]["theirs"]["apiKey"] == "t", "pi's providers carried over, not replaced");
        let mode = |p: std::path::PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(own.join("auth.json")), 0o600, "keys are its user's alone");
        assert_eq!(mode(own.clone()), 0o700);
        let settings_now: Value = serde_json::from_str(&std::fs::read_to_string(node_dir.join("settings.json")).unwrap()).unwrap();
        assert_eq!(settings_now["agent-model"], "openrouter/some/model");

        // Now it has them: nothing more to add, and it says so.
        let Response::Setup { added } = c.call(Request::Setup { auth, models: Some(models), settings }).await.unwrap() else { panic!() };
        assert!(added.is_empty(), "{added:?}");
        let (c2, _) = Client::connect(&Target::Local(socket.clone()), "setup-test").await.unwrap();
        assert!(c2.hello.setup.providers.contains(&"openrouter".to_string()), "{:?}", c2.hello.setup);
        assert_eq!(c2.hello.setup.sources, ["~/.serpis/agent"]);
    });
    node.stop();
    let _ = std::fs::remove_dir_all(&root);
}
