//! Putting serpis-node on a machine: serpis carries the node for each system it can reach, and
//! installs the right one the first time it connects, as pi-env does. Nothing then
//! depends on the machine's PATH or packages, and a machine gets a new node with a new serpis.
//!
//! The node lives in `~/.serpis/bin/serpis-node-<sha256>`, named by its checksum, so versions
//! never overwrite each other. Every connect runs a short `sh` script there (see
//! [`agent_command`]) that picks the build for `uname -sm`, checks its checksum, and runs it; a
//! missing or altered one is reported with [`MARKER`] and exit status 125, and the client then
//! uploads it over the same ssh login ([`upload`]) and connects again. A machine whose system
//! serpis has no build for falls back to a serpis-node it finds there (PATH, `~/.local/bin`,
//! `~/.cargo/bin`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// What the far side writes to stderr when it needs the node with this checksum.
pub const MARKER: &str = "SERPIS-DEPLOY ";

/// What the far side writes to stderr when serpis has no build for its system (`uname -sm`).
pub const SYSTEM_MARKER: &str = "SERPIS-SYSTEM ";

/// A node build serpis can install.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeBinary {
    /// `uname -sm` of the systems it runs on: `Linux x86_64`, `Darwin arm64`.
    pub os_arch: String,
    pub path: PathBuf,
    pub sha256: String,
}

/// The target triple to build for a system (`uname -sm`): Linux builds against glibc 2.28, so
/// they run on distributions from 2018 on.
pub fn triple_for(os_arch: &str) -> Option<&'static str> {
    Some(match os_arch {
        "Linux x86_64" => "x86_64-unknown-linux-gnu.2.28",
        "Linux aarch64" => "aarch64-unknown-linux-gnu.2.28",
        "Darwin arm64" => "aarch64-apple-darwin",
        "Darwin x86_64" => "x86_64-apple-darwin",
        _ => return None,
    })
}

/// `uname -sm` for a Rust target triple (a glibc version suffix is ignored).
pub fn os_arch_of(triple: &str) -> Option<&'static str> {
    let triple = triple.split(".2.").next().unwrap_or(triple);
    Some(match triple {
        "x86_64-unknown-linux-gnu" | "x86_64-unknown-linux-musl" => "Linux x86_64",
        "aarch64-unknown-linux-gnu" | "aarch64-unknown-linux-musl" => "Linux aarch64",
        "aarch64-apple-darwin" => "Darwin arm64",
        "x86_64-apple-darwin" => "Darwin x86_64",
        _ => return None,
    })
}

pub fn sha256_file(path: &Path) -> std::io::Result<String> {
    let mut h = Sha256::new();
    let mut f = std::fs::File::open(path)?;
    std::io::copy(&mut f, &mut h)?;
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

/// The node builds in a store: `<dir>/<target triple>/serpis-node` (what scripts/build-node.sh
/// and the install script put there).
pub fn scan(dir: &Path) -> Vec<NodeBinary> {
    let mut out = Vec::new();
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let (Some(os_arch), path) = (os_arch_of(&name), e.path().join("serpis-node")) else { continue };
        if out.iter().any(|b: &NodeBinary| b.os_arch == os_arch) {
            continue;
        }
        match sha256_file(&path) {
            Ok(sha256) => out.push(NodeBinary { os_arch: os_arch.into(), path, sha256 }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("{}: {e}", path.display()),
        }
    }
    out.sort_by(|a, b| a.os_arch.cmp(&b.os_arch));
    out
}

/// A serpis-node already on the machine: PATH, then where user installs put it. A
/// non-interactive ssh session often has a short PATH (no `~/.local/bin`).
const LOOKUP: &str = r#"for p in serpis-node "$HOME/.local/bin/serpis-node" "$HOME/.cargo/bin/serpis-node"; do command -v "$p" >/dev/null 2>&1 && exec "$p" agent; done; echo "serpis-node is not installed on $(hostname) (not on PATH, nor in ~/.local/bin or ~/.cargo/bin), and serpis has no build for $(uname -sm)" >&2; exit 127"#;

/// What a client that can't install nodes (the phone app) runs on a machine to reach its node:
/// the build that last started a node there (`~/.serpis/bin/serpis-node-current`), else a
/// serpis-node on PATH or in the usual places. Exits 127, saying so, when there is none.
pub const FOUND_COMMAND: &str = r#"sh -c 'for p in "$HOME/.serpis/bin/serpis-node-current" serpis-node "$HOME/.local/bin/serpis-node" "$HOME/.cargo/bin/serpis-node"; do command -v "$p" >/dev/null 2>&1 && exec "$p" agent; done; echo "serpis-node is not set up on $(hostname): open a chat there once from serpis on a computer" >&2; exit 127'"#;

/// What runs on a machine to reach its node: the build of `nodes` for its system, checked. For
/// another system it says which ([`SYSTEM_MARKER`]) when `ask` (serpis may get a build for it),
/// else runs a serpis-node found there. One `sh -c`, so it works under any login shell (fish
/// too). Nothing in it may contain a single quote.
pub fn agent_command(nodes: &[NodeBinary], ask: bool) -> String {
    let otherwise = if ask { format!("echo \"{SYSTEM_MARKER}$(uname -sm)\" >&2; exit 125") } else { LOOKUP.to_string() };
    if nodes.is_empty() {
        return format!("sh -c '{otherwise}'");
    }
    let mut cases = String::new();
    for n in nodes {
        cases.push_str(&format!("\"{}\") h={};; ", n.os_arch, n.sha256));
    }
    format!(
        concat!(
            "sh -c 'h=; case \"$(uname -sm)\" in {cases}esac; ",
            "if [ -n \"$h\" ]; then b=\"$HOME/.serpis/bin/serpis-node-$h\"; ",
            "if [ -x \"$b\" ]; then s=$( (sha256sum \"$b\" || shasum -a 256 \"$b\") 2>/dev/null | cut -d\" \" -f1); ",
            "[ \"$s\" = \"$h\" ] && exec \"$b\" agent; ",
            "[ -n \"$s\" ] || {{ echo \"serpis: neither sha256sum nor shasum is installed on $(hostname), so $b cant be checked\" >&2; exit 126; }}; ",
            "rm -f \"$b\"; fi; echo \"{marker}$h\" >&2; exit 125; fi; {otherwise}'"
        ),
        cases = cases,
        marker = MARKER,
        otherwise = otherwise
    )
}

/// The command that writes the node with checksum `sha256` from stdin into place there.
pub fn install_command(sha256: &str) -> String {
    // Written beside its final name and moved, so a cut connection never leaves a partial node
    // under the name that is run; the directory and file are the user's alone.
    format!(r#"sh -c 'umask 077; d="$HOME/.serpis/bin"; t="$d/.serpis-node-{sha256}.part"; mkdir -p "$d" && cat > "$t" && chmod 700 "$t" && mv -f "$t" "$d/serpis-node-{sha256}"'"#)
}

/// The checksum a failed connect asked for, from what the far side wrote to stderr.
pub fn wanted(stderr: &str) -> Option<&str> {
    stderr.lines().find_map(|l| l.trim().strip_prefix(MARKER)).map(str::trim)
}

/// The system a failed connect said serpis has no build for.
pub fn wanted_system(stderr: &str) -> Option<&str> {
    stderr.lines().find_map(|l| l.trim().strip_prefix(SYSTEM_MARKER)).map(str::trim)
}

/// Upload `node` through `ssh` (program and fixed arguments), `ssh_args`, to `host`, run with
/// `env` in front, as [`install_command`] puts it in place.
pub async fn upload(ssh: &[String], ssh_args: &[String], host: &str, env: &str, node: &NodeBinary) -> Result<()> {
    let (program, fixed) = ssh.split_first().context("no ssh program")?;
    let bytes = tokio::fs::read(&node.path).await.with_context(|| format!("reading {}", node.path.display()))?;
    let mut child = tokio::process::Command::new(program)
        .args(fixed)
        .args(ssh_args)
        .arg(host)
        .arg("--")
        .arg(format!("{env}{}", install_command(&node.sha256)))
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .with_context(|| format!("starting {program}"))?;
    let mut stdin = child.stdin.take().unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut stdin, &bytes).await.context("sending the node")?;
    drop(stdin);
    let out = child.wait_with_output().await?;
    if !out.status.success() {
        bail!("installing serpis-node on {host} failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    tracing::info!("installed serpis-node {} on {host} ({} MB)", &node.sha256[..12], bytes.len() / (1 << 20));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targets_name_their_systems_as_uname_does() {
        assert_eq!(os_arch_of("x86_64-unknown-linux-gnu.2.28"), Some("Linux x86_64"));
        assert_eq!(os_arch_of("aarch64-apple-darwin"), Some("Darwin arm64"));
        assert_eq!(os_arch_of("riscv64gc-unknown-linux-gnu"), None);
    }

    #[test]
    fn the_far_side_script_is_one_quoted_word() {
        let n = NodeBinary { os_arch: "Linux x86_64".into(), path: "x".into(), sha256: "ab12".into() };
        for cmd in [agent_command(&[n.clone()], true), agent_command(&[n], false), agent_command(&[], true), agent_command(&[], false), install_command("ab12")] {
            assert!(cmd.starts_with("sh -c '") && cmd.ends_with('\''), "{cmd}");
            assert_eq!(cmd.matches('\'').count(), 2, "nothing inside may close the quote: {cmd}");
        }
        assert_eq!(wanted("Warning: added host key\nSERPIS-DEPLOY ab12\n"), Some("ab12"));
        assert_eq!(wanted("Permission denied"), None);
        assert_eq!(wanted_system("SERPIS-SYSTEM Linux x86_64\n"), Some("Linux x86_64"));
        assert_eq!(triple_for("Linux aarch64").and_then(os_arch_of), Some("Linux aarch64"));
    }
}
