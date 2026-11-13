//! Test support for serpis' prompt caching: a strict model of provider caches ([`sim`]) and a
//! local HTTP provider that answers with it ([`mock`]). Never talks to a real provider.

pub mod mock;
pub mod sim;
pub mod sshd;

pub use mock::{LogEntry, Mock, Reply, Script, commands};
pub use sim::{Config, Miss, Outcome, Simulator, Wire};

/// The running test's profile directory (target/<profile>): the one just under the target
/// directory (which holds `CACHEDIR.TAG`), wherever below it cargo put the test (`deps/<test>`,
/// or `build/<crate>/<hash>/out/<test>` since cargo 1.100).
fn profile_dir() -> std::path::PathBuf {
    let exe = std::env::current_exe().expect("test executable");
    exe.ancestors().zip(exe.ancestors().skip(1)).find(|(_, up)| up.join("CACHEDIR.TAG").is_file()).map(|(d, _)| d.to_path_buf()).expect("target directory")
}

/// The stand-in `claude` CLI (`src/bin/serpis-fake-claude.rs`), built if needed, beside the
/// running test's own build.
pub fn fake_claude() -> std::path::PathBuf {
    static BUILT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let dir = profile_dir();
            let release = dir.file_name().is_some_and(|n| n == "release");
            let mut cargo = std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
            cargo.args(["build", "-q", "-p", "serpis-testkit", "--bin", "serpis-fake-claude"]);
            if release {
                cargo.arg("--release");
            }
            if let Some(target) = dir.parent() {
                cargo.env("CARGO_TARGET_DIR", target);
            }
            assert!(cargo.status().expect("running cargo").success(), "building serpis-fake-claude");
            dir.join("serpis-fake-claude")
        })
        .clone()
}

/// The stand-in `codex app-server` used by Codex integration tests.
pub fn fake_codex() -> std::path::PathBuf {
    static BUILT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            let dir = profile_dir();
            let release = dir.file_name().is_some_and(|n| n == "release");
            let mut cargo = std::process::Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()));
            cargo.args(["build", "-q", "-p", "serpis-testkit", "--bin", "serpis-fake-codex"]);
            if release {
                cargo.arg("--release");
            }
            if let Some(target) = dir.parent() {
                cargo.env("CARGO_TARGET_DIR", target);
            }
            assert!(cargo.status().expect("running cargo").success(), "building serpis-fake-codex");
            dir.join("serpis-fake-codex")
        })
        .clone()
}
