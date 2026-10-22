//! Projects: a directory tree rooted at a version-control root (like Emacs `project.el`).

use std::path::{Path, PathBuf};

const MARKERS: [&str; 4] = [".git", ".jj", ".hg", ".project"];

static LOOKUPS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// How many times [`root`] has looked at the disk (tests check nothing does it every frame).
pub fn lookups() -> u64 {
    LOOKUPS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Nearest ancestor of `dir` (inclusive) containing a VCS or `.project` marker.
pub fn root(dir: &Path) -> Option<PathBuf> {
    LOOKUPS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    dir.ancestors().find(|d| MARKERS.iter().any(|m| d.join(m).exists())).map(Path::to_path_buf)
}

/// Project name: the root directory's name.
pub fn name(dir: &Path) -> Option<String> {
    root(dir).and_then(|r| r.file_name().map(|f| f.to_string_lossy().into_owned()))
}

#[cfg(test)]
mod tests {
    #[test]
    fn finds_repo_root() {
        let here = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let root = super::root(&here).expect("serpis lives in a git repo");
        assert!(root.join(".git").exists());
        assert!(super::name(&here).is_some());
    }
}
