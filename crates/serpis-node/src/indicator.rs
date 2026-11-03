//! The menu bar indicator's side of the node (crates/serpis-status is the indicator): the node
//! and the terminal host outlive serpis's window, so whichever starts first starts it, as does
//! the window when it connects to a node already running, and it asks `serpis-node status` what
//! is running.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::client::{Client, Target};
use serde::Serialize;
use serpis_proto::wire::{self, Call, Reply, Request, Response, tag};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;

/// How long the node or the terminal host get to answer.
const ANSWER: Duration = Duration::from_secs(2);

/// What runs on this machine, as `serpis-node status` prints it.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Status {
    pub node: bool,
    pub terminal_host: bool,
    /// Chats open on the node.
    pub chats: usize,
    /// Those working on a turn now.
    pub working: usize,
    pub terminals: usize,
    /// The `status-indicator` setting.
    pub indicator: bool,
}

/// Present when `status-indicator` was turned off, so the setting holds when no window is there
/// to say so.
fn off_marker() -> PathBuf {
    crate::node_dir().join("status-indicator-off")
}

/// Whether the indicator should show: `status-indicator`, from serpis's window or settings.json.
pub fn enabled() -> bool {
    let settings = std::fs::read_to_string(crate::node_dir().join("settings.json")).ok().and_then(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    !off_marker().exists() && settings.and_then(|s| s.get("status-indicator")?.as_bool()) != Some(false)
}

/// The `status-indicator` setting changed (it is also sent whenever a window connects).
pub fn set_enabled(on: bool) {
    if on {
        let _ = std::fs::remove_file(off_marker());
        start();
    } else if let Err(e) = std::fs::write(off_marker(), "") {
        tracing::warn!("{}: {e}", off_marker().display());
    }
}

/// The indicator beside a program that runs from an app bundle (the node or the window). Nothing
/// else starts it: nodes installed on other machines, and development and test builds, have no
/// menu bar of the user's to show in.
fn indicator_beside(exe: &Path) -> Option<PathBuf> {
    let dir = exe.parent()?;
    if !dir.ends_with("Contents/MacOS") || !dir.parent()?.parent()?.extension().is_some_and(|e| e == "app") {
        return None;
    }
    Some(dir.parent()?.join("Helpers/SerpisStatus.app/Contents/MacOS/serpis-status")).filter(|p| p.is_file())
}

/// Start the indicator if this node should have one. It keeps to one copy itself, so this is
/// safe to call from each process that outlives the window.
pub fn start() {
    use std::os::unix::process::CommandExt;
    if !enabled() {
        return;
    }
    let exe = std::env::current_exe().and_then(|e| e.canonicalize());
    let Some(indicator) = exe.ok().as_deref().and_then(indicator_beside) else { return };
    let mut cmd = std::process::Command::new(indicator);
    cmd.stdin(std::process::Stdio::null()).stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
    // SAFETY: setsid is async-signal-safe, and nothing else runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid().map_err(std::io::Error::from)?;
            Ok(())
        });
    }
    match cmd.spawn() {
        // Waited for, or it would stay in the process table once it leaves. A copy started while
        // one already runs leaves at once, and each window that connects starts one.
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => tracing::warn!("starting the menu bar indicator: {e}"),
    }
}

/// How many terminals the host at `socket` holds; None when it doesn't answer in time.
async fn terminals_held(stream: UnixStream) -> Option<usize> {
    let exchange = async {
        let (mut rd, mut wr) = stream.into_split();
        wr.write_all(&wire::encode(tag::CALL, &Call { id: 1, request: Request::Terminals })).await.ok()?;
        let mut decoder = wire::Decoder::default();
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = rd.read(&mut buf).await.ok()?;
            if n == 0 {
                return None;
            }
            decoder.push(&buf[..n]);
            while let Some((kind, body)) = decoder.next().ok()? {
                if kind == tag::REPLY
                    && let Ok(Reply { id: 1, response: Response::Terminals { terminals } }) = serde_json::from_slice(&body)
                {
                    return Some(terminals.len());
                }
            }
        }
    };
    tokio::time::timeout(ANSWER, exchange).await.ok().flatten()
}

/// What runs here. Only looks: nothing that isn't running gets started.
pub async fn status() -> Status {
    let mut s = Status { indicator: enabled(), ..Default::default() };
    let node_socket = crate::socket_path();
    let client = tokio::time::timeout(ANSWER, Client::connect(&Target::Local(node_socket.clone()), "serpis-status")).await;
    if let Ok(Ok((client, _events))) = client {
        s.node = true;
        if let Ok(Ok(Response::Chats { chats })) = tokio::time::timeout(ANSWER, client.call(Request::List)).await {
            s.chats = chats.len();
            s.working = chats.iter().filter(|c| c.state.running || c.state.compacting).count();
        }
    } else {
        // Not one this build can talk to (an older node, or a stuck one) is still running.
        s.node = UnixStream::connect(&node_socket).await.is_ok();
    }
    if let Ok(stream) = UnixStream::connect(crate::terminals::socket()).await {
        s.terminal_host = true;
        s.terminals = terminals_held(stream).await.unwrap_or(0);
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_node_in_an_app_bundle_has_an_indicator() {
        let root = std::env::temp_dir().join(format!("serpis-indicator-{}", std::process::id()));
        let macos = root.join("Serpis.app/Contents/MacOS");
        std::fs::create_dir_all(&macos).unwrap();
        let node = macos.join("serpis-node");
        assert_eq!(indicator_beside(&node), None, "the bundle has none");
        let helper = root.join("Serpis.app/Contents/Helpers/SerpisStatus.app/Contents/MacOS");
        std::fs::create_dir_all(&helper).unwrap();
        std::fs::write(helper.join("serpis-status"), "").unwrap();
        assert_eq!(indicator_beside(&node), Some(helper.join("serpis-status")));
        // A development build, with one built beside it.
        let dev = root.join("target/debug");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join("serpis-status"), "").unwrap();
        assert_eq!(indicator_beside(&dev.join("serpis-node")), None);
        // A node installed on a machine.
        assert_eq!(indicator_beside(&root.join(".serpis/bin/serpis-node-ab12")), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
