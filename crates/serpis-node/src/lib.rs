//! The serpis node: one per machine and user, running that machine's chats for clients
//! elsewhere (the desktop, the iOS app).
//!
//! It never touches the network: it listens on a
//! Unix socket in an owner-only directory and checks that whoever connects is its own user.
//! Other machines reach it by running `serpis-node agent` over ssh, which bridges its stdin and
//! stdout to that socket, so sshd decides who gets in (your keys, or rayfish's mesh SSH and its
//! `ssh_allow`), and serpis keeps no keys or allowlist of its own. The account question answers
//! itself too: `ssh deploy@box` lands in deploy's node, because the socket lives in deploy's
//! home.
//!
//! Chats here are [`serpis_chat::Chat`]s, the same as on the desktop, and contain no GUI code.

#[cfg(feature = "node")]
pub mod bridge;
pub mod client;
pub mod deploy;
#[cfg(feature = "node")]
pub mod gitdiff;
#[cfg(feature = "node")]
pub mod keys;
#[cfg(feature = "node")]
pub mod logs;
#[cfg(feature = "node")]
pub mod lua;
#[cfg(feature = "node")]
pub mod workspace;
#[cfg(feature = "node")]
pub mod server;
#[cfg(feature = "node")]
pub mod service;
#[cfg(feature = "node")]
pub mod terminals;

use std::path::PathBuf;

/// The node's directory: its socket, settings, log and open chats. `$SERPIS_NODE_DIR`, else
/// `~/.serpis/node`.
pub fn node_dir() -> PathBuf {
    std::env::var_os("SERPIS_NODE_DIR").map(PathBuf::from).unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".serpis").join("node"))
}

/// Where the node listens: `$SERPIS_NODE_SOCKET`, else `node.sock` in [`node_dir`].
pub fn socket_path() -> PathBuf {
    std::env::var_os("SERPIS_NODE_SOCKET").map(PathBuf::from).unwrap_or_else(|| node_dir().join("node.sock"))
}

/// This build, as `0.1.0`.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The sources this node is built from, as `0.1.0-<hash>`: node builds are kept under it, so
/// one built from other sources is never taken for this one (build.rs).
pub const STAMP: &str = env!("SERPIS_NODE_STAMP");
