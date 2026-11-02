//! Installing the node on first connect, through a stand-in for ssh that runs the command
//! here (so no network and no sshd): it is installed once, checked on every start, replaced when
//! it was altered, and a machine serpis has no build for falls back to one it has itself.

use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use serpis_node::client::{Client, Target};
use serpis_node::deploy::{NodeBinary, sha256_file};
use serpis_proto::wire::Request;

fn uname() -> String {
    serpis_node::deploy::this_system()
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
    let node = NodeBinary { system: uname(), path: exe.to_path_buf(), sha256: sha256_file(exe).unwrap() };
    let installed = home.join(".serpis/bin").join(format!("serpis-node-{}", node.sha256));
    let target = |deploy: Vec<NodeBinary>| Target::Ssh {
        host: "box".into(),
        ssh: vec![fake_ssh.to_string_lossy().into_owned()],
        ssh_args: vec!["-T".into()],
        env: vec![("HOME".into(), home.to_string_lossy().into_owned()), ("SERPIS_NODE_SOCKET".into(), root.join("n.sock").to_string_lossy().into_owned())],
        command: None,
        deploy,
        ask: false,
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

        // No builds at all: the machine says its system, a build is made for it (here: this
        // one, standing in for scripts/build-node.sh), installed, and run.
        std::fs::remove_dir_all(home.join(".serpis")).unwrap();
        let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
        let (a, n) = (asked.clone(), node.clone());
        let obtain: serpis_node::client::Obtain = std::sync::Arc::new(move |system: String| {
            a.lock().unwrap().push(system);
            let n = n.clone();
            Box::pin(async move { Ok(n) })
        });
        let stages = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let st = stages.clone();
        let (c, _) = Client::connect_with(&target(vec![]), "deploy-test", Some(obtain), move |s| st.lock().unwrap().push(s)).await.unwrap();
        assert_eq!(*asked.lock().unwrap(), [uname()], "a build was asked for once, for this system");
        assert_eq!(*stages.lock().unwrap(), [serpis_node::client::Stage::Building(uname()), serpis_node::client::Stage::Installing]);
        assert_eq!(sha256_file(&installed).unwrap(), node.sha256);
        c.call(Request::Stop).await.unwrap();
        drop(c);

        // No build to be had: a serpis-node installed there is used instead...
        std::fs::remove_dir_all(home.join(".serpis")).unwrap();
        let failing: serpis_node::client::Obtain = std::sync::Arc::new(|_| Box::pin(async { anyhow::bail!("no toolchain here") }));
        std::fs::create_dir_all(home.join(".local/bin")).unwrap();
        std::os::unix::fs::symlink(exe, home.join(".local/bin/serpis-node")).unwrap();
        let (c, _) = Client::connect_with(&target(vec![]), "deploy-test", Some(failing.clone()), |_| {}).await.unwrap();
        c.call(Request::Stop).await.unwrap();
        drop(c);
        // ...and without one, the error says both why there's no build and that none is there.
        std::fs::remove_file(home.join(".local/bin/serpis-node")).unwrap();
        let err = Client::connect_with(&target(vec![]), "deploy-test", Some(failing), |_| {}).await.err().expect("nothing to run");
        let err = format!("{err:#}");
        assert!(err.contains("no toolchain here") && err.contains("serpis-node is not installed"), "{err}");

        // No build for this machine's system, and none installed there: said so.
        let other = NodeBinary { system: "Plan9 mips".into(), ..node.clone() };
        let err = Client::connect(&target(vec![other]), "deploy-test").await.err().expect("nothing to run");
        assert!(format!("{err:#}").contains("serpis-node is not installed"), "{err:#}");
    });
    let _ = std::fs::remove_dir_all(&root);
}

/// A stand-in machine reached through a stand-in ssh: `uname -sm` says `uname`, getconf does
/// `getconf`, and the only build to hand is a glibc one. Returns its root, its home, and the target.
fn machine(uname: &str, getconf: &str) -> (std::path::PathBuf, std::path::PathBuf, Target) {
    let root = std::env::temp_dir().join(format!("sd-{}", &uuid::Uuid::now_v7().simple().to_string()[24..]));
    let (home, bin) = (root.join("home"), root.join("bin"));
    std::fs::create_dir_all(&home).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let script = |path: &Path, body: &str| {
        std::fs::write(path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    };
    script(&bin.join("uname"), &format!("echo '{uname}'"));
    script(&bin.join("getconf"), getconf);
    let fake_ssh = root.join("ssh");
    script(&fake_ssh, "while [ $# -gt 0 ] && [ \"$1\" != \"--\" ]; do shift; done; shift\nexec sh -c \"$*\"");
    let exe = Path::new(env!("CARGO_BIN_EXE_serpis-node"));
    let glibc = NodeBinary { system: "Linux aarch64 gnu".into(), path: exe.to_path_buf(), sha256: sha256_file(exe).unwrap() };
    let target = Target::Ssh {
        host: "box".into(),
        ssh: vec![fake_ssh.to_string_lossy().into_owned()],
        ssh_args: vec![],
        env: vec![("HOME".into(), home.to_string_lossy().into_owned()), ("PATH".into(), format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()))],
        command: None,
        deploy: vec![glibc],
        ask: false,
    };
    (root, home, target)
}

/// Connects, recording which systems a build is asked for (none can be had), and returns those and the error.
fn connect_failing(target: &Target) -> (Vec<String>, String) {
    let asked = std::sync::Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let a = asked.clone();
    let obtain: serpis_node::client::Obtain = std::sync::Arc::new(move |system: String| {
        a.lock().unwrap().push(system);
        Box::pin(async { anyhow::bail!("no build here") })
    });
    let rt = tokio::runtime::Runtime::new().unwrap();
    let err = rt.block_on(Client::connect_with(target, "deploy-test", Some(obtain), |_| {})).err().expect("nothing it can run");
    let asked = asked.lock().unwrap().clone();
    (asked, format!("{err:#}"))
}

/// An Alpine machine: the glibc build must not be sent, and serpis is asked for the build the
/// machine says it needs.
#[test]
fn a_machine_with_musl_is_asked_about_and_never_sent_a_glibc_build() {
    let (root, home, target) = machine("Linux aarch64", "[ \"$1\" = LONG_BIT ] && echo 64 || exit 1");
    let (asked, err) = connect_failing(&target);
    assert_eq!(asked, ["Linux aarch64 musl"]);
    assert!(err.contains("serpis has no serpis-node for Linux aarch64 musl"), "{err}");
    assert!(!home.join(".serpis/bin").exists(), "nothing was installed there");
    let _ = std::fs::remove_dir_all(&root);
}

/// A 32-bit system on a 64-bit kernel: nothing is asked for, sent or run, not even a serpis-node
/// it already has, and the error says why.
#[test]
fn a_32_bit_system_is_refused_before_anything_is_fetched() {
    let (root, home, target) = machine("Linux aarch64", "[ \"$1\" = LONG_BIT ] && echo 32 || echo 'glibc 2.36'");
    std::fs::create_dir_all(home.join(".local/bin")).unwrap();
    std::os::unix::fs::symlink(env!("CARGO_BIN_EXE_serpis-node"), home.join(".local/bin/serpis-node")).unwrap();
    let (asked, err) = connect_failing(&target);
    assert_eq!(asked, Vec::<String>::new(), "no build was asked for");
    assert!(err.contains("needs a 64-bit system") && err.contains("Linux aarch64 32-bit"), "{err}");
    assert!(!home.join(".serpis/bin").exists(), "nothing was installed there");
    let _ = std::fs::remove_dir_all(&root);
}
