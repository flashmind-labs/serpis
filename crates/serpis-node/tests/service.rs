//! The node as a service: installed when asked, it is what starts the node from then on (a
//! client finding none running starts it through the service, which a crash or a reboot would
//! too), on the build that asked, and a chat cut off meanwhile continues. The service manager is
//! a stand-in (`SERPIS_SERVICE_CTL`) that notes what it is asked and starts the node as the
//! real one would: no real systemd or launchd is touched, and the home is throwaway.

use std::path::Path;
use std::time::Duration;

use serpis_node::client::{Client, Target};
use serpis_proto::Entry;
use serpis_proto::wire::{Event, Request, Response};

#[test]
fn a_service_starts_the_node() {
    use std::os::unix::fs::PermissionsExt;
    let root = std::env::temp_dir().join(format!("sv-{}", &uuid::Uuid::now_v7().simple().to_string()[24..]));
    let (home, node, units) = (root.join("home"), root.join("node"), root.join("units"));
    for d in [home.join("project"), node.clone(), root.join("agent")] {
        std::fs::create_dir_all(d).unwrap();
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let mock = rt.block_on(serpis_testkit::Mock::start(serpis_testkit::commands()));
    let models = serde_json::json!({"providers": {"mock": {"baseUrl": format!("{}/api/v1", mock.base), "api": "openai-completions", "apiKey": "x", "compat": {"openRouter": true}, "models": [{"id": "m"}]}}});
    std::fs::write(root.join("agent/models.json"), models.to_string()).unwrap();
    std::fs::write(node.join("settings.json"), serde_json::json!({"session-directory": root.join("sessions")}).to_string()).unwrap();
    // The stand-in: notes its words; asked to (re)start the node, starts what the link points at.
    let ctl = root.join("ctl");
    std::fs::write(&ctl, format!(
        "#!/bin/sh\necho \"$*\" >> '{log}'\ncase \"$*\" in *restart*|*kickstart*) nohup \"$HOME/.serpis/bin/serpis-node-current\" serve >/dev/null 2>&1 & ;; esac\nexit 0\n",
        log = root.join("ctl.log").display()
    )).unwrap();
    std::fs::set_permissions(&ctl, std::fs::Permissions::from_mode(0o755)).unwrap();
    let env = |k: &str, v: &Path| format!("{k}={}", v.display());
    let exe = env!("CARGO_BIN_EXE_serpis-node");
    let target = Target::Command(vec![
        "env".into(),
        env("HOME", &home),
        env("SERPIS_NODE_DIR", &node),
        env("SERPIS_NODE_SOCKET", &node.join("node.sock")),
        env("SERPIS_AGENT_DIR", &root.join("agent")),
        env("SERPIS_CONFIG_DIR", &root.join("config")),
        env("SERPIS_TERMINAL_SOCKET", &root.join("t.sock")),
        env("SERPIS_SERVICE_DIR", &units),
        env("SERPIS_SERVICE_CTL", &ctl),
        exe.into(),
        "agent".into(),
    ]);
    let said = || std::fs::read_to_string(root.join("ctl.log")).unwrap_or_default();
    rt.block_on(async {
        let (c, mut events) = Client::connect(&target, "test").await.unwrap();
        let Response::Service { installed, file, .. } = c.call(Request::InstallService).await.unwrap() else { panic!() };
        assert!(installed && file.starts_with(&*units.to_string_lossy()), "{file}");
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("serpis-node-current") && text.contains(&*node.join("node.sock").to_string_lossy()), "{text}");
        assert_eq!(std::fs::read_link(home.join(".serpis/bin/serpis-node-current")).unwrap(), Path::new(exe));
        assert!(!said().contains("restart") && !said().contains("kickstart"), "installing doesn't start a second node: {}", said());

        // Mid-turn, the node is killed (a crash, a reboot).
        let Response::Chat { chat } = c.call(Request::New { cwd: "~/project".into(), model: Some("mock/m".into()), thinking: None, chat: None }).await.unwrap() else { panic!() };
        c.call(Request::Attach { chat: chat.clone() }).await.unwrap();
        c.call(Request::Prompt { chat: chat.clone(), text: r#"CALL bash {"command": "sleep 20"}"#.into(), images: vec![], follow_up: false, request_id: None }).await.unwrap();
        let mut entries: Vec<Entry> = Vec::new();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !entries.iter().any(|e| matches!(e, Entry::Tool { result: None, .. })) {
            match tokio::time::timeout_at(deadline, events.recv()).await.expect("the tool to start") {
                Some(Event::Splice { splice, .. }) => splice.apply(&mut entries),
                Some(_) => {}
                None => panic!("closed"),
            }
        }
        let pid = std::fs::read_to_string(node.join("node.pid")).unwrap();
        std::process::Command::new("kill").args(["-9", pid.trim()]).status().unwrap();
        drop((c, events));
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Started again through the service, and the chat continues.
        let (c, mut events) = Client::connect(&target, "test-again").await.unwrap();
        assert!(said().contains("restart") || said().contains("kickstart"), "{}", said());
        c.call(Request::Attach { chat: chat.clone() }).await.unwrap();
        let mut entries: Vec<Entry> = Vec::new();
        let says = |es: &[Entry], t: &str| es.iter().any(|e| matches!(e, Entry::Info { text, .. } if text.contains(t)));
        let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
        while !says(&entries, "Continued") {
            match tokio::time::timeout_at(deadline, events.recv()).await.unwrap_or_else(|_| panic!("not continued: {entries:#?}")) {
                Some(Event::Splice { splice, .. }) => splice.apply(&mut entries),
                Some(_) => {}
                None => panic!("closed"),
            }
        }

        let Response::Service { installed, .. } = c.call(Request::UninstallService).await.unwrap() else { panic!() };
        assert!(!installed && !Path::new(&file).exists());
        c.call(Request::Stop).await.unwrap();
    });
    let _ = std::fs::remove_dir_all(&root);
}
