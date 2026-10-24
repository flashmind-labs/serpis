//! `serpis-node agent`: what a client elsewhere runs here over ssh. It joins its stdin and
//! stdout to this machine's node socket, starting the node first if none is running, and adds
//! nothing of its own: the bytes are the protocol.

use std::path::Path;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tokio::net::UnixStream;

/// Join stdin and stdout to the node at `socket` until either side closes.
pub async fn agent(socket: &Path) -> Result<()> {
    let stream = match UnixStream::connect(socket).await {
        Ok(s) => s,
        Err(_) => {
            start_node(&std::env::current_exe().context("finding serpis-node")?, socket)?;
            wait_for(socket).await?
        }
    };
    let (mut from_node, mut to_node) = stream.into_split();
    let up = tokio::spawn(async move {
        let _ = tokio::io::copy(&mut tokio::io::stdin(), &mut to_node).await;
        // The client is done sending: the node answers what it has, then closes its side.
        let _ = tokio::io::AsyncWriteExt::shutdown(&mut to_node).await;
    });
    let _ = tokio::io::copy(&mut from_node, &mut tokio::io::stdout()).await;
    up.abort();
    Ok(())
}

/// Start `exe serve` in the background, in a session of its own, so it outlives whatever
/// started it (an ssh connection, a window); its output goes to `node.log` next to the socket.
pub fn start_node(exe: &Path, socket: &Path) -> Result<()> {
    use std::os::unix::process::CommandExt;
    let dir = socket.parent().context("the socket path has no directory")?;
    std::fs::create_dir_all(dir)?;
    let log = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("node.log"))?;
    let mut cmd = std::process::Command::new(exe);
    cmd.arg("serve").env("SERPIS_NODE_SOCKET", socket).stdin(std::process::Stdio::null()).stdout(log.try_clone()?).stderr(log);
    // SAFETY: setsid is async-signal-safe, and nothing else runs between fork and exec.
    unsafe {
        cmd.pre_exec(|| {
            nix::unistd::setsid().map_err(std::io::Error::from)?;
            Ok(())
        });
    }
    cmd.spawn().context("starting the node")?;
    Ok(())
}

pub async fn wait_for(socket: &Path) -> Result<UnixStream> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Ok(s) = UnixStream::connect(socket).await {
            return Ok(s);
        }
        if Instant::now() > deadline {
            bail!("started a node, but nothing listens on {} (see node.log there)", socket.display());
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
