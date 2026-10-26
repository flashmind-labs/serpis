//! Terminals on this machine: manymux's node holds them, so they outlive every window and any
//! client that reaches the machine can attach to one. It runs as a process of its own
//! (`serpis-node terminals`), not inside the chat node, so a chat node restarting on a new build
//! never takes a shell with it.
//!
//! It listens at manymux's own socket unless `$SERPIS_TERMINAL_SOCKET` says otherwise, so an
//! `mm` on the machine and serpis share their terminals: whichever starts first holds them,
//! and the other is a client of it. Terminal bytes go straight to it (a client elsewhere runs
//! `serpis-node agent` with `SERPIS_BRIDGE=terminals`); the chat node only lists, starts and
//! ends terminals for clients, over the connection they already have.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use manymux::client::Stream;
use manymux::proto::{Request, Response, Size, SpawnSpec};
use serpis_proto::wire::TerminalInfo;

/// Where the terminal node listens: `$SERPIS_TERMINAL_SOCKET`, else manymux's socket.
pub fn socket() -> PathBuf {
    std::env::var_os("SERPIS_TERMINAL_SOCKET").map(PathBuf::from).unwrap_or_else(manymux::config::socket)
}

/// Run the terminal node at `socket` until it is stopped. At manymux's socket it is the
/// machine's manymux node, and does what `mm daemon` does (watches `mm`'s machines, raises
/// notifications); anywhere else it holds terminals and nothing more.
pub async fn serve(socket: &Path) -> Result<()> {
    let shared = socket == manymux::config::socket();
    let config = if shared {
        let hosts = manymux::hosts::Hosts::load().map(|h| h.names()).unwrap_or_default();
        manymux::node::Config { peers: hosts, hosts_file: Some(manymux::hosts::Hosts::path()), notifications: true }
    } else {
        manymux::node::Config { peers: Vec::new(), hosts_file: None, notifications: false }
    };
    let node = manymux::node::Node::start(config).await;
    let stopping = node.clone();
    tokio::spawn(async move {
        use tokio::signal::unix::{SignalKind, signal};
        let (Ok(mut term), Ok(mut int)) = (signal(SignalKind::terminate()), signal(SignalKind::interrupt())) else { return };
        tokio::select! {
            _ = term.recv() => {}
            _ = int.recv() => {}
        }
        // Hung up gently: shells write their history.
        stopping.shutdown(manymux::node::GRACE).await;
        std::process::exit(0);
    });
    node.serve(socket).await
}

/// Make sure a terminal node listens at `socket`, starting `exe terminals` (in a session of its
/// own, so it outlives whatever started it) when none does.
pub async fn ensure(exe: &Path, socket: &Path) -> Result<()> {
    if tokio::net::UnixStream::connect(socket).await.is_ok() {
        return Ok(());
    }
    use std::os::unix::process::CommandExt;
    // Its log goes beside the chat node's (manymux's socket directory is manymux's).
    let dir = crate::node_dir();
    std::fs::create_dir_all(&dir)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("terminals.log"))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("terminals").env("SERPIS_TERMINAL_SOCKET", socket).stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    // SAFETY: setsid is async-signal-safe, and nothing else runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid().map_err(std::io::Error::from)?;
            Ok(())
        });
    }
    cmd.spawn().context("starting the terminal node")?;
    crate::bridge::wait_for(socket).await.map(|_| ())
}

/// One request to the terminal node at `socket`, starting it first when `start` is given.
async fn call(socket: &Path, start: Option<&Path>, request: Request) -> Result<Response> {
    if let Some(exe) = start {
        ensure(exe, socket).await?;
    }
    let mut stream = Stream::local(socket).await?;
    match stream.call(&request).await? {
        Response::Error(e) => bail!(e),
        r => Ok(r),
    }
}

/// The terminals at `socket`, with what each is doing (none when no terminal node runs).
pub async fn list(socket: &Path) -> Result<Vec<TerminalInfo>> {
    if tokio::net::UnixStream::connect(socket).await.is_err() {
        return Ok(Vec::new());
    }
    let Response::Sessions(sessions) = call(socket, None, Request::List).await? else { bail!("the terminal node answered List with something else") };
    // Older manymux nodes can't say: the list is still worth having.
    let doing = match call(socket, None, Request::Snapshot).await {
        Ok(Response::Snapshot(d)) => d,
        _ => Vec::new(),
    };
    Ok(sessions
        .into_iter()
        .map(|s| {
            let d = doing.iter().find(|d| d.name == s.name && d.pid == s.pid);
            TerminalInfo {
                foreground: d.map(|d| d.foreground.clone()).filter(|f| !at_prompt(f)).unwrap_or_default(),
                cwd: d.and_then(|d| d.cwd.clone()),
                started: s.started.duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0),
                name: s.name,
                title: s.title,
                command: s.command,
                attached: s.attached,
                idle: s.idle,
            }
        })
        .collect())
}

/// Whether a terminal's foreground is a shell (at its prompt) rather than a program it runs.
fn at_prompt(argv: &[String]) -> bool {
    let Some(first) = argv.first() else { return true };
    let name = first.rsplit('/').next().unwrap_or(first).trim_start_matches('-');
    matches!(name, "sh" | "bash" | "zsh" | "fish" | "dash" | "ksh" | "mksh" | "tcsh" | "csh" | "nu" | "elvish" | "xonsh")
}

/// The scripts serpis's shell integration is made of (zsh: prompt marks, the directory).
pub const SHELL_FILES: [(&str, &str); 2] = [("zsh/.zshenv", include_str!("../shell/zsh/.zshenv")), ("serpis.zsh", include_str!("../shell/serpis.zsh"))];

/// Write the shell integration to a directory of this user's, and return it.
pub fn install_shell_integration() -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("serpis-shell-{}", nix::unistd::getuid()));
    std::fs::create_dir_all(dir.join("zsh")).ok()?;
    for (path, body) in SHELL_FILES {
        let p = dir.join(path);
        if std::fs::read_to_string(&p).ok().as_deref() != Some(body) {
            std::fs::write(&p, body).ok()?;
        }
    }
    Some(dir)
}

/// The user's shell: `$SHELL` (sshd sets it to the login shell), else the account's.
fn login_shell() -> String {
    std::env::var("SHELL")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| nix::unistd::User::from_uid(nix::unistd::getuid()).ok().flatten().map(|u| u.shell.to_string_lossy().into_owned()))
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/bin/sh".into())
}

/// Start a login shell in `cwd` at `socket` (starting the terminal node from `exe` first), and
/// return the terminal's name.
pub async fn spawn(exe: &Path, socket: &Path, cwd: &Path, size: (u16, u16), integration: bool) -> Result<String> {
    if !cwd.is_dir() {
        bail!("{} isn't a folder here", cwd.display());
    }
    let shell = login_shell();
    let base = shell.rsplit('/').next().unwrap_or(&shell).to_string();
    // What a terminal of serpis's says about itself, and zsh's prompt marks. The home is the
    // one this node runs with: manymux takes the account's, which is the same thing unless
    // someone (a test, a container) gave the node another.
    let mut command = vec!["env".to_string(), "COLORTERM=truecolor".into(), "TERM_PROGRAM=serpis".into()];
    if let Some(home) = std::env::var_os("HOME") {
        command.push(format!("HOME={}", home.to_string_lossy()));
    }
    command.push(format!("SHELL={shell}"));
    if let Some(dir) = (integration && base == "zsh").then(install_shell_integration).flatten() {
        if let Some(z) = std::env::var_os("ZDOTDIR") {
            command.push(format!("SERPIS_ZDOTDIR={}", z.to_string_lossy()));
        }
        command.push(format!("ZDOTDIR={}", dir.join("zsh").display()));
        command.push(format!("SERPIS_SHELL_DIR={}", dir.display()));
    }
    command.extend([shell.clone(), "-l".into()]);
    let taken: Vec<String> = match call(socket, Some(exe), Request::List).await? {
        Response::Sessions(s) => s.into_iter().map(|s| s.name).collect(),
        _ => Vec::new(),
    };
    let name = (1..).map(|n| if n == 1 { base.clone() } else { format!("{base}-{n}") }).find(|n| !taken.contains(n)).unwrap_or_default();
    let spec = SpawnSpec { name: Some(name), command, cwd: Some(cwd.to_string_lossy().into_owned()), size: Size::new(size.0, size.1), label: Some(base) };
    match call(socket, None, Request::Spawn(spec)).await? {
        Response::Spawned { name } => Ok(name),
        other => bail!("the terminal node answered a spawn with {other:?}"),
    }
}

/// End the terminal `name` at `socket`.
pub async fn kill(socket: &Path, name: &str) -> Result<()> {
    call(socket, None, Request::Kill { name: name.to_string() }).await.map(|_| ())
}

/// Stop the terminal node at `socket`, if one runs: every terminal in it ends.
pub async fn stop(socket: &Path) -> Result<()> {
    if tokio::net::UnixStream::connect(socket).await.is_err() {
        return Ok(());
    }
    manymux::node::stop(socket).await
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_shell_in_front_is_the_prompt() {
        for (argv, prompt) in [(vec!["-zsh"], true), (vec!["/bin/bash", "-l"], true), (vec!["vim", "x"], false), (vec![], true)] {
            let argv: Vec<String> = argv.into_iter().map(String::from).collect();
            assert_eq!(super::at_prompt(&argv), prompt, "{argv:?}");
        }
    }
}
