//! Putting serpis-node on a machine: serpis carries the node for each system it can reach, and
//! installs the right one the first time it connects, as pi-env does. Nothing then
//! depends on the machine's PATH or packages, and a machine gets a new node with a new serpis.
//!
//! The node lives in `~/.serpis/bin/serpis-node-<sha256>`, named by its checksum, so versions
//! never overwrite each other. Every connect runs a short `sh` script there (see
//! [`agent_command`]) that works out its system ([`PROBE`]), picks the build for it, checks its checksum, and runs it; a
//! missing or altered one is reported with [`MARKER`] and exit status 125, and the client then
//! uploads it over the same ssh login ([`upload`]) and connects again. A machine whose system
//! serpis has no build for falls back to a serpis-node it finds there (PATH, `~/.local/bin`,
//! `~/.cargo/bin`).

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use sha2::{Digest, Sha256};

/// What the far side writes to stderr when it needs the node with this checksum.
pub const MARKER: &str = "SERPIS-DEPLOY ";

/// What the far side writes to stderr when serpis has no build for its system ([`PROBE`]).
pub const SYSTEM_MARKER: &str = "SERPIS-SYSTEM ";

/// A node build serpis can install.
#[derive(Debug, Clone, PartialEq)]
pub struct NodeBinary {
    /// The systems it runs on ([`PROBE`]): `Linux x86_64 gnu`, `Linux aarch64 musl`, `Darwin arm64`.
    pub system: String,
    pub path: PathBuf,
    pub sha256: String,
}

/// Sets `$system` to what a machine is, in `sh`: `uname -sm`, and on Linux the C library its
/// programs link. glibc says so through getconf. Anything else gets the musl build, which is
/// static and so starts whatever is there: musl itself, a glibc older than the gnu build needs
/// (2.28), or a machine that can't say.
///
/// `uname -m` names the kernel, so a 32-bit system on a 64-bit kernel says `aarch64`: what its
/// programs are comes from getconf, else the class in the header of its /bin/sh (1 is 32-bit
/// ELF). Such a system is `Linux aarch64 32-bit`. Nothing in it may contain a single quote.
pub const PROBE: &str = r#"system="$(uname -sm)"; if [ "${system%% *}" = Linux ]; then b="$(getconf LONG_BIT 2>/dev/null)"; [ -n "$b" ] || { b="$(od -An -tu1 -j4 -N1 /bin/sh 2>/dev/null)"; [ "${b##* }" != 1 ] || b=32; }; if [ "$b" = 32 ]; then system="$system 32-bit"; else v="$(getconf GNU_LIBC_VERSION 2>/dev/null)"; v="${v#glibc 2.}"; v="${v%%.*}"; if [ "$v" -ge 28 ] 2>/dev/null; then system="$system gnu"; else system="$system musl"; fi; fi; fi; "#;

/// Stops on a 32-bit system, which serpis doesn't support: before it can ask for a build, so
/// nothing is downloaded, built or installed for it. Goes right after [`PROBE`].
const REFUSE_32_BIT: &str = r#"case "$system" in *32-bit) echo "serpis-node needs a 64-bit system, and $(hostname) is 32-bit ($system)" >&2; exit 127;; esac; "#;

/// What this machine is ([`PROBE`]); empty if `sh` can't say.
pub fn this_system() -> String {
    let out = std::process::Command::new("sh").arg("-c").arg(format!("{PROBE}echo \"$system\"")).stdin(std::process::Stdio::null()).output();
    out.map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default()
}

/// The target triple to build for a system ([`PROBE`]). gnu builds link glibc 2.28, so they run
/// on distributions from 2018 on; musl builds are static.
pub fn triple_for(system: &str) -> Option<&'static str> {
    Some(match system {
        "Linux x86_64 gnu" => "x86_64-unknown-linux-gnu.2.28",
        "Linux aarch64 gnu" => "aarch64-unknown-linux-gnu.2.28",
        "Linux x86_64 musl" => "x86_64-unknown-linux-musl",
        "Linux aarch64 musl" => "aarch64-unknown-linux-musl",
        "Darwin arm64" => "aarch64-apple-darwin",
        "Darwin x86_64" => "x86_64-apple-darwin",
        _ => return None,
    })
}

/// The system ([`PROBE`]) a target triple's builds run on (a glibc version suffix is ignored).
pub fn system_of(triple: &str) -> Option<&'static str> {
    let triple = triple.split(".2.").next().unwrap_or(triple);
    Some(match triple {
        "x86_64-unknown-linux-gnu" => "Linux x86_64 gnu",
        "aarch64-unknown-linux-gnu" => "Linux aarch64 gnu",
        "x86_64-unknown-linux-musl" => "Linux x86_64 musl",
        "aarch64-unknown-linux-musl" => "Linux aarch64 musl",
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
        let (Some(system), path) = (system_of(&name), e.path().join("serpis-node")) else { continue };
        if out.iter().any(|b: &NodeBinary| b.system == system) {
            continue;
        }
        match sha256_file(&path) {
            Ok(sha256) => out.push(NodeBinary { system: system.into(), path, sha256 }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => tracing::warn!("{}: {e}", path.display()),
        }
    }
    out.sort_by(|a, b| a.system.cmp(&b.system));
    out
}

/// A serpis-node already on the machine: PATH, then where user installs put it. A
/// non-interactive ssh session often has a short PATH (no `~/.local/bin`).
const LOOKUP: &str = r#"for p in serpis-node "$HOME/.local/bin/serpis-node" "$HOME/.cargo/bin/serpis-node"; do command -v "$p" >/dev/null 2>&1 && exec "$p" agent; done; echo "serpis-node is not installed on $(hostname) (not on PATH, nor in ~/.local/bin or ~/.cargo/bin), and serpis has no build for $system" >&2; exit 127"#;

/// What a client that can't install nodes (the phone app) runs on a machine to reach its node:
/// the build that last started a node there (`~/.serpis/bin/serpis-node-current`), else a
/// serpis-node on PATH or in the usual places. Exits 127, saying so, when there is none.
pub const FOUND_COMMAND: &str = r#"sh -c 'for p in "$HOME/.serpis/bin/serpis-node-current" serpis-node "$HOME/.local/bin/serpis-node" "$HOME/.cargo/bin/serpis-node"; do command -v "$p" >/dev/null 2>&1 && exec "$p" agent; done; echo "serpis-node is not set up on $(hostname): open a chat there once from serpis on a computer" >&2; exit 127'"#;

/// What runs on a machine to reach its node: the build of `nodes` for its system, checked. For
/// another system it says which ([`SYSTEM_MARKER`]) when `ask` (serpis may get a build for it),
/// else runs a serpis-node found there. One `sh -c`, so it works under any login shell (fish
/// too). Nothing in it may contain a single quote.
pub fn agent_command(nodes: &[NodeBinary], ask: bool) -> String {
    let otherwise = if ask { format!("echo \"{SYSTEM_MARKER}$system\" >&2; exit 125") } else { LOOKUP.to_string() };
    if nodes.is_empty() {
        return format!("sh -c '{PROBE}{REFUSE_32_BIT}{otherwise}'");
    }
    let mut cases = String::new();
    for n in nodes {
        cases.push_str(&format!("\"{}\") h={};; ", n.system, n.sha256));
    }
    format!(
        concat!(
            "sh -c '{probe}{refuse}h=; case \"$system\" in {cases}esac; ",
            "if [ -n \"$h\" ]; then b=\"$HOME/.serpis/bin/serpis-node-$h\"; ",
            "if [ -x \"$b\" ]; then s=$( (sha256sum \"$b\" || shasum -a 256 \"$b\") 2>/dev/null | cut -d\" \" -f1); ",
            "[ \"$s\" = \"$h\" ] && exec \"$b\" agent; ",
            "[ -n \"$s\" ] || {{ echo \"serpis: neither sha256sum nor shasum is installed on $(hostname), so $b cant be checked\" >&2; exit 126; }}; ",
            "rm -f \"$b\"; fi; echo \"{marker}$h\" >&2; exit 125; fi; {otherwise}'"
        ),
        probe = PROBE,
        refuse = REFUSE_32_BIT,
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
    fn targets_name_their_systems_as_the_probe_does() {
        assert_eq!(system_of("x86_64-unknown-linux-musl"), Some("Linux x86_64 musl"));
        assert_eq!(system_of("x86_64-unknown-linux-gnu"), Some("Linux x86_64 gnu"));
        assert_eq!(system_of("x86_64-unknown-linux-gnu.2.28"), Some("Linux x86_64 gnu"));
        assert_eq!(system_of("aarch64-apple-darwin"), Some("Darwin arm64"));
        assert_eq!(system_of("riscv64gc-unknown-linux-gnu"), None);
        assert_eq!(triple_for("Linux aarch64 32-bit"), None);
        for system in ["Linux x86_64 gnu", "Linux aarch64 gnu", "Linux x86_64 musl", "Linux aarch64 musl", "Darwin arm64", "Darwin x86_64"] {
            assert_eq!(triple_for(system).and_then(system_of), Some(system));
        }
    }

    /// What the probe says, on a stand-in machine: its `uname -sm`, what its getconf does, and
    /// what its od reads from /bin/sh.
    fn probe(uname: &str, getconf: &str) -> String {
        probe_od(uname, getconf, "echo '   2'")
    }

    fn probe_od(uname: &str, getconf: &str, od: &str) -> String {
        use std::os::unix::fs::PermissionsExt;
        let dir = std::env::temp_dir().join(format!("serpis-probe-{}", unique_name()));
        std::fs::create_dir_all(&dir).unwrap();
        for (name, body) in [("uname", format!("echo '{uname}'")), ("getconf", getconf.to_string()), ("od", od.to_string())] {
            let path = dir.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let path = format!("{}:{}", dir.display(), std::env::var("PATH").unwrap());
        let out = std::process::Command::new("sh").arg("-c").arg(format!("{PROBE}echo \"$system\"")).env("PATH", path).output().unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        String::from_utf8(out.stdout).unwrap().trim().to_string()
    }

    fn unique_name() -> String {
        static NEXT: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        format!("{}-{}", std::process::id(), NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed))
    }

    #[test]
    fn the_probe_tells_a_machines_c_library() {
        let glibc = |v: &str| format!("[ \"$1\" = LONG_BIT ] && echo 64 || echo 'glibc {v}'");
        // Debian and the like.
        assert_eq!(probe("Linux x86_64", &glibc("2.36")), "Linux x86_64 gnu");
        assert_eq!(probe("Linux aarch64", &glibc("2.28")), "Linux aarch64 gnu");
        assert_eq!(probe("Linux x86_64", &glibc("2.39.9000")), "Linux x86_64 gnu");
        // Alpine: its getconf doesn't know the name.
        assert_eq!(probe("Linux aarch64", "[ \"$1\" = LONG_BIT ] && echo 64 && exit; echo 'getconf: GNU_LIBC_VERSION: invalid' >&2; exit 1"), "Linux aarch64 musl");
        // Too old for the gnu build (compared as numbers), or it can't say: the static one starts anyway.
        assert_eq!(probe("Linux x86_64", &glibc("2.27")), "Linux x86_64 musl");
        assert_eq!(probe("Linux x86_64", &glibc("2.9")), "Linux x86_64 musl");
        assert_eq!(probe("Linux x86_64", "exit 127"), "Linux x86_64 musl");
        assert_eq!(probe("Linux x86_64", "[ \"$1\" = LONG_BIT ] && echo 64 || echo surprise"), "Linux x86_64 musl");
        // A 32-bit system says so, on a 64-bit kernel too, whichever C library it has.
        let bits32 = "[ \"$1\" = LONG_BIT ] && echo 32 || echo 'glibc 2.36'";
        assert_eq!(probe("Linux aarch64", bits32), "Linux aarch64 32-bit");
        assert_eq!(probe("Linux armv7l", bits32), "Linux armv7l 32-bit");
        assert_eq!(probe("Linux x86_64", "[ \"$1\" = LONG_BIT ] && echo 32 || exit 1"), "Linux x86_64 32-bit");
        // No getconf: /bin/sh's header tells.
        assert_eq!(probe_od("Linux aarch64", "exit 127", "echo '   1'"), "Linux aarch64 32-bit");
        assert_eq!(probe_od("Linux aarch64", "exit 127", "echo '   2'"), "Linux aarch64 musl");
        // Not Linux: no C library to choose.
        assert_eq!(probe("Darwin arm64", "exit 1"), "Darwin arm64");
    }

    #[test]
    fn the_far_side_script_is_one_quoted_word() {
        let n = NodeBinary { system: "Linux x86_64 gnu".into(), path: "x".into(), sha256: "ab12".into() };
        for cmd in [agent_command(&[n.clone()], true), agent_command(&[n], false), agent_command(&[], true), agent_command(&[], false), install_command("ab12")] {
            assert!(cmd.starts_with("sh -c '") && cmd.ends_with('\''), "{cmd}");
            assert_eq!(cmd.matches('\'').count(), 2, "nothing inside may close the quote: {cmd}");
        }
        assert_eq!(wanted("Warning: added host key\nSERPIS-DEPLOY ab12\n"), Some("ab12"));
        assert_eq!(wanted("Permission denied"), None);
        assert_eq!(wanted_system("SERPIS-SYSTEM Linux x86_64 musl\n"), Some("Linux x86_64 musl"));
    }
}
