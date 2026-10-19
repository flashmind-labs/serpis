//! Projects: a directory tree rooted at a version-control root (like Emacs `project.el`).

use std::path::{Path, PathBuf};

const MARKERS: [&str; 4] = [".git", ".jj", ".hg", ".project"];

/// Nearest ancestor of `dir` (inclusive) containing a VCS or `.project` marker.
pub fn root(dir: &Path) -> Option<PathBuf> {
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
