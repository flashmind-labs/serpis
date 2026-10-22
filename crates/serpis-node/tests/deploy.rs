//! Installing the node on first connect, through a stand-in for ssh that runs the command
//! here (so no network and no sshd): it is installed once, checked on every start, replaced when
//! it was altered, and a machine serpis has no build for falls back to one it has itself.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serpis_node::client::{Client, Target};
use serpis_node::deploy::{NodeBinary, sha256_file};
use serpis_proto::wire::Request;

fn uname() -> String {
    String::from_utf8(std::process::Command::new("uname").arg("-sm").output().unwrap().stdout).unwrap().trim().to_string()
}

#[test]
fn the_node_is_installed_on_first_connect_and_checked_on_every_start() {
    let root = std::env::temp_dir().join(format!("sd-{}", &uuid::Uuid::now_v7().simple().to_string()[24..]));
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    // Takes ssh's arguments, drops everything up to `--`, and runs the rest with sh, as sshd
    // would run it in the login's shell.
    let fake_ssh = root.join("ssh");
    std::fs::write(&fake_ssh, "#!/bin/sh\nwhile [ $# -gt 0 ] && [ \"$1\" != \"--\" ]; do shift; done; shift\nexec sh -c \"$*\"\n").unwrap();
    std::fs::set_permissions(&fake_ssh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let exe = Path::new(env!("CARGO_BIN_EXE_serpis-node"));
    let node = NodeBinary { os_arch: uname(), path: exe.to_path_buf(), sha256: sha256_file(exe).unwrap() };
    let installed = home.join(".serpis/bin").join(format!("serpis-node-{}", node.sha256));
    let target = |deploy: Vec<NodeBinary>| Target::Ssh {
        host: "box".into(),
        ssh: vec![fake_ssh.to_string_lossy().into_owned()],
        ssh_args: vec!["-T".into()],
        env: vec![("HOME".into(), home.to_string_lossy().into_owned()), ("SERPIS_NODE_SOCKET".into(), root.join("n.sock").to_string_lossy().into_owned())],
        command: None,
        deploy,
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let (c, _) = Client::connect(&target(vec![node.clone()]), "deploy-test").await.unwrap();
        assert_eq!(c.hello.home, home.to_string_lossy(), "the installed node answered");
        assert_eq!(sha256_file(&installed).unwrap(), node.sha256, "installed under its checksum");
        let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&installed), 0o700);
        assert_eq!(mode(installed.parent().unwrap()), 0o700, "only its user can change it");
        let first = std::fs::metadata(&installed).unwrap().modified().unwrap();
        drop(c);

        // Once there, it is only checked.
        let (c, _) = Client::connect(&target(vec![node.clone()]), "deploy-test").await.unwrap();
        assert_eq!(std::fs::metadata(&installed).unwrap().modified().unwrap(), first, "not sent again");
        c.call(Request::Stop).await.unwrap();
        drop(c);

        // Altered there (a corrupted copy, or someone else's), it is replaced before it runs.
        std::fs::write(&installed, "#!/bin/sh\necho tampered\n").unwrap();
        let (c, _) = Client::connect(&target(vec![node.clone()]), "deploy-test").await.unwrap();
        assert_eq!(sha256_file(&installed).unwrap(), node.sha256, "replaced with the real one");
        c.call(Request::Stop).await.unwrap();
        drop(c);

        // No build for this machine's system, and none installed there: said so.
        let other = NodeBinary { os_arch: "Plan9 mips".into(), ..node.clone() };
        let err = Client::connect(&target(vec![other]), "deploy-test").await.err().expect("nothing to run");
        assert!(format!("{err:#}").contains("serpis-node is not installed"), "{err:#}");
    });
    let _ = std::fs::remove_dir_all(&root);
}
