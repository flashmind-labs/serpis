//! Test support for serpis' prompt caching: a strict model of provider caches ([`sim`]) and a
//! local HTTP provider that answers with it ([`mock`]). Never talks to a real provider.

pub mod mock;
pub mod sim;
pub mod sshd;

pub use mock::{LogEntry, Mock, Reply, Script, commands};
pub use sim::{Config, Miss, Outcome, Simulator, Wire};

/// The stand-in `claude` CLI (`src/bin/serpis-fake-claude.rs`), built if needed, beside the
/// running test's own build.
pub fn fake_claude() -> std::path::PathBuf {
    static BUILT: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();
    BUILT
        .get_or_init(|| {
            // target/<profile>/deps/<test> → target/<profile>
            let exe = std::env::current_exe().expect("test executable");
            let dir = exe.parent().and_then(|d| d.parent()).expect("target directory").to_path_buf();
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
