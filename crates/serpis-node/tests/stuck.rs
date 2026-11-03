//! This machine's node holds its socket but no longer answers (out of file handles, wedged):
//! connecting replaces it, or the window could never reach a node again. Only ever a real node.

use std::os::unix::net::UnixListener;
use std::process::{Command, Stdio};
use std::time::Duration;

use serpis_node::client::{Client, Target};

fn alive(pid: u32) -> bool {
    Command::new("ps").args(["-p", &pid.to_string(), "-o", "stat="]).output().is_ok_and(|o| {
        let stat = String::from_utf8_lossy(&o.stdout).trim().to_string();
        !stat.is_empty() && !stat.starts_with('Z')
    })
}

#[test]
fn a_stuck_node_is_replaced_and_nothing_else_is_touched() {
    let root = std::env::temp_dir().join(format!("sk-{}", &uuid::Uuid::now_v7().simple().to_string()[24..]));
    let (home, dir) = (root.join("home"), root.join("n"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&dir).unwrap();
    // What the replacement inherits: this test's own home, never the real one.
    unsafe {
        std::env::set_var("HOME", &home);
        std::env::set_var("SERPIS_NODE_DIR", &dir);
        std::env::remove_var("XDG_CONFIG_HOME");
    }
    let socket = dir.join("n.sock");
    let exe = std::path::PathBuf::from(env!("CARGO_BIN_EXE_serpis-node"));
    let target = Target::Own { socket: socket.clone(), exe: exe.clone() };
    let rt = tokio::runtime::Runtime::new().unwrap();

    // Something else holds the socket and hangs up, and node.pid names another program: it is
    // left alone, and the connection fails.
    let mut other = Command::new("sleep").arg("120").spawn().unwrap();
    std::fs::write(dir.join("node.pid"), other.id().to_string()).unwrap();
    let listener = UnixListener::bind(&socket).unwrap();
    let hangs_up = std::thread::spawn(move || {
        for stream in listener.incoming() {
            match stream {
                Ok(s) => drop(s),
                Err(_) => break,
            }
        }
    });
    let failed = rt.block_on(Client::connect_with(&target, "stuck-test", None, |_| {}));
    assert!(failed.is_err(), "nothing there speaks the protocol");
    assert!(alive(other.id()), "a process that isn't a node is never stopped");
    let _ = other.kill();
    let _ = other.wait();
    drop(hangs_up);
    std::fs::remove_file(&socket).unwrap();
    std::fs::remove_file(dir.join("node.pid")).unwrap();

    // A real node, frozen: it takes connections and never answers, not even to SIGTERM.
    let mut stuck = Command::new(&exe).arg("serve").env("SERPIS_NODE_SOCKET", &socket).stdout(Stdio::null()).stderr(Stdio::null()).spawn().unwrap();
    while !dir.join("node.pid").exists() {
        std::thread::sleep(Duration::from_millis(20));
    }
    let first = stuck.id();
    assert!(Command::new("kill").args(["-STOP", &first.to_string()]).status().unwrap().success());

    let (client, _events) = rt.block_on(Client::connect_with(&target, "stuck-test", None, |_| {})).expect("a new node took over");
    let _ = stuck.wait();
    let now: u32 = std::fs::read_to_string(dir.join("node.pid")).unwrap().trim().parse().unwrap();
    assert_ne!(now, first, "another process runs the node now");
    assert!(alive(now));
    rt.block_on(client.call(serpis_proto::wire::Request::Stop)).unwrap();
    let _ = std::fs::remove_dir_all(&root);
}
