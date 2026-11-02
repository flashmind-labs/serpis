//! A node over real ssh (opt-in: `scripts/ssh-check.sh [host]`, default localhost).
//!
//! Everything on the other side is made over ssh in a throwaway directory under /tmp there: a
//! home with a project, an agent directory whose only model is a mock provider, and the node's
//! directory. So the node never sees the account's real sessions, settings or keys, and no
//! real provider is called. Host keys go to a known-hosts file of the test's own.
//!
//! The node is installed there the way serpis installs it, from the builds in
//! `SERPIS_SSH_TEST_NODE_BUILDS` (a `<target>/serpis-node` store; this build alone for
//! localhost), into the throwaway home. The mock runs where the node does: in this process on
//! localhost, or on the other machine (`SERPIS_SSH_TEST_MOCK`: serpis-mock-provider's path there).
//! Needs ssh that works without a prompt: a key in authorized_keys, or rayfish's mesh SSH.

use std::time::Duration;

use serde_json::json;
use serpis_node::client::{Client, Target};
use serpis_proto::Entry;
use serpis_proto::wire::{Event, Request, Response};

/// Run `script` with sh on `host`, through ssh with `args`; its stdout.
fn far(host: &str, args: &[String], script: &str) -> String {
    let out = std::process::Command::new("ssh").args(args).arg(host).arg("--").arg(format!("sh -c {}", quote(script))).output().expect("running ssh");
    assert!(out.status.success(), "on {host}: {script}\n{}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

#[test]
#[ignore = "uses real ssh: scripts/ssh-check.sh"]
fn a_node_over_real_ssh() {
    let host = std::env::var("SERPIS_SSH_TEST_HOST").unwrap_or_else(|_| "localhost".into());
    let local = matches!(host.as_str(), "localhost" | "127.0.0.1" | "::1");
    let builds = match std::env::var("SERPIS_SSH_TEST_NODE_BUILDS") {
        Ok(dir) => serpis_node::deploy::scan(std::path::Path::new(&dir)),
        Err(_) => {
            let exe = std::path::Path::new(env!("CARGO_BIN_EXE_serpis-node"));
            let system = serpis_node::deploy::this_system();
            vec![serpis_node::deploy::NodeBinary { system, path: exe.into(), sha256: serpis_node::deploy::sha256_file(exe).unwrap() }]
        }
    };
    assert!(!builds.is_empty(), "no node builds to install");
    let id = &uuid::Uuid::now_v7().simple().to_string()[24..];
    let dir = format!("/tmp/snssh-{id}");
    let near = std::env::temp_dir().join(format!("snssh-{id}"));
    std::fs::create_dir_all(&near).unwrap();
    let Target::Ssh { mut ssh_args, .. } = Target::ssh(&host) else { unreachable!() };
    ssh_args.extend(["-o".into(), format!("UserKnownHostsFile={}", near.join("known_hosts").display()), "-o".into(), "StrictHostKeyChecking=accept-new".into()]);
    let rt = tokio::runtime::Runtime::new().unwrap();

    // The mock provider, next to the node.
    far(&host, &ssh_args, &format!("mkdir -p {dir}/home/proj {dir}/agent {dir}/node && printf 'zero over ssh\\n' > {dir}/home/proj/f0.txt && printf 'one over ssh\\n' > {dir}/home/proj/f1.txt"));
    let (_mock, base, mock_pid) = if local {
        let m = rt.block_on(serpis_testkit::Mock::start(serpis_testkit::commands()));
        let base = m.base.clone();
        (Some(m), base, None)
    } else {
        let mock = std::env::var("SERPIS_SSH_TEST_MOCK").expect("SERPIS_SSH_TEST_MOCK: serpis-mock-provider's path on the other machine");
        let started = far(&host, &ssh_args, &format!("nohup {mock} {dir}/mock.port {dir}/mock.log >/dev/null 2>&1 & echo $!; for i in $(seq 50); do [ -s {dir}/mock.port ] && break; sleep 0.1; done; cat {dir}/mock.port"));
        let mut lines = started.lines();
        let pid = lines.next().unwrap().trim().to_string();
        let port = lines.next().expect("the mock wrote its port").trim().to_string();
        (None, format!("http://127.0.0.1:{port}"), Some(pid))
    };
    let models = json!({"providers": {"mock": {"baseUrl": format!("{base}/api/v1"), "api": "openai-completions", "apiKey": "x", "compat": {"openRouter": true}, "models": [{"id": "anthropic/claude-sonnet-4.5"}]}}});
    let settings = json!({"session-directory": format!("{dir}/sessions")});
    far(&host, &ssh_args, &format!("printf '%s' {} > {dir}/agent/models.json && printf '{{}}' > {dir}/agent/auth.json && printf '%s' {} > {dir}/node/settings.json", quote(&models.to_string()), quote(&settings.to_string())));

    let env = [("HOME", "home"), ("SERPIS_NODE_DIR", "node"), ("SERPIS_NODE_SOCKET", "node/node.sock"), ("SERPIS_AGENT_DIR", "agent")].map(|(k, v)| (k.to_string(), format!("{dir}/{v}"))).to_vec();
    let target = Target::Ssh { host: host.clone(), ssh: vec!["ssh".into()], ssh_args: ssh_args.clone(), env, command: None, deploy: builds, ask: false };
    rt.block_on(async {
        let started = std::time::Instant::now();
        let (c, mut events) = Client::connect(&target, "ssh-check").await.unwrap_or_else(|e| panic!("connecting over ssh to {host}: {e:#}"));
        println!("node on {host}: {} (serpis-node {}), connected in {:?}", c.hello.host, c.hello.serpis, started.elapsed());
        assert_eq!(c.hello.home, format!("{dir}/home"), "the node runs in the throwaway home");
        assert!(far(&host, &ssh_args, &format!("ls {dir}/home/.serpis/bin")).contains("serpis-node-"), "serpis installed its node there");
        let Response::Chat { chat } = c.call(Request::New { cwd: "~/proj".into(), model: None, thinking: None, chat: None }).await.unwrap() else { panic!() };
        c.call(Request::Attach { chat: chat.clone() }).await.unwrap();
        let t = std::time::Instant::now();
        c.call(Request::Prompt { chat: chat.clone(), text: "READ 2".into(), images: vec![], follow_up: false, request_id: Some("ssh-1".into()) }).await.unwrap();
        let mut entries: Vec<Entry> = Vec::new();
        let mut idle = false;
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        while !(idle && entries.len() >= 5) {
            match tokio::time::timeout_at(deadline, events.recv()).await.expect("the chat finished over ssh") {
                Some(Event::Splice { splice, .. }) => splice.apply(&mut entries),
                Some(Event::State { state, .. }) => idle = !state.running && state.usage.total_tokens > 0,
                Some(_) => {}
                None => panic!("the ssh connection closed"),
            }
        }
        let tool = serde_json::to_string(&entries[2]).unwrap();
        assert!(tool.contains("zero over ssh"), "the tool ran on the far side: {tool}");
        println!("a chat with two tool calls ran on {host} over ssh in {:?}: {} entries", t.elapsed(), entries.len());
        let Response::Sessions { sessions } = c.call(Request::Sessions { limit: None }).await.unwrap() else { panic!() };
        assert_eq!(sessions.len(), 1, "the session was saved there");
        c.call(Request::Stop).await.unwrap();
    });
    std::thread::sleep(Duration::from_millis(500));
    let left = far(&host, &ssh_args, &format!("ls {dir}/node"));
    assert!(!left.contains("node.sock"), "the node stopped: {left}");
    let kill = mock_pid.map(|p| format!("kill {p}; ")).unwrap_or_default();
    far(&host, &ssh_args, &format!("{kill}rm -rf {dir}"));
    let _ = std::fs::remove_dir_all(&near);
}
