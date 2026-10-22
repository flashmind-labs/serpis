//! Carrying a chat's folder to another machine.
//!
//! A git repository goes as a bundle of its current branch plus a snapshot commit of the
//! uncommitted work (changes, deletions and untracked files; ignored ones stay behind). The
//! snapshot is made through a temporary index and a temporary ref, so the repository's index,
//! working tree and refs are untouched. On the other side the branch is checked out at the
//! same commit with the work back as uncommitted changes. Any other folder goes as a tar.gz.
//! Either way the chat's place inside it (a subfolder) is kept.
//!
//! Both ends use `git` and `tar` from PATH.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

/// How a folder was packed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Packed {
    /// A git bundle: `branch` (None when HEAD was detached) at `head`, and `snapshot`, the
    /// working state on top of it (None when there was nothing uncommitted).
    Git { branch: Option<String>, head: String, snapshot: Option<String>, subdir: String },
    /// A tar.gz of the folder.
    Tar { subdir: String },
}

impl Packed {
    /// The folder's name, for where to put it there.
    pub fn subdir(&self) -> &str {
        match self {
            Packed::Git { subdir, .. } | Packed::Tar { subdir } => subdir,
        }
    }
}

fn run(cmd: &mut Command) -> Result<String> {
    let out = cmd.output().with_context(|| format!("running {:?}", cmd.get_program()))?;
    if !out.status.success() {
        bail!("{:?} failed: {}", cmd.get_program(), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir);
    c
}

/// Pack `dir` (a chat's folder) into a file in `into`: its repository when it is in one, else
/// the folder itself. Returns the file and how it was packed.
pub fn pack(dir: &Path, into: &Path) -> Result<(PathBuf, Packed)> {
    std::fs::create_dir_all(into)?;
    let name = format!("pack-{}", uuid::Uuid::now_v7().simple());
    let top = run(git(dir).args(["rev-parse", "--show-toplevel"])).ok().map(PathBuf::from);
    let Some(top) = top else {
        let file = into.join(format!("{name}.tar.gz"));
        let parent = dir.parent().context("a folder at the root can't be carried")?;
        let base = dir.file_name().context("no folder name")?;
        run(Command::new("tar").arg("-czf").arg(&file).arg("-C").arg(parent).arg(base))?;
        return Ok((file, Packed::Tar { subdir: base.to_string_lossy().into_owned() }));
    };
    let dir = dir.canonicalize().unwrap_or(dir.to_path_buf());
    let top = top.canonicalize().unwrap_or(top);
    let rel = dir.strip_prefix(&top).unwrap_or(Path::new(""));
    let repo_name = top.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into());
    let subdir = Path::new(&repo_name).join(rel).to_string_lossy().into_owned();
    let head = run(git(&top).args(["rev-parse", "HEAD"])).context("the repository has no commit yet")?;
    let branch = run(git(&top).args(["symbolic-ref", "--quiet", "--short", "HEAD"])).ok();
    // The working state as a commit, made with an index of its own.
    let index = into.join(format!("{name}.index"));
    // (A linked worktree's .git is a file: git says where its index is.)
    if let Ok(i) = run(git(&top).args(["rev-parse", "--path-format=absolute", "--git-path", "index"])) {
        std::fs::copy(i, &index).ok();
    }
    let with_index = |args: &[&str]| -> Result<String> { run(git(&top).env("GIT_INDEX_FILE", &index).args(args)) };
    with_index(&["add", "-A"])?;
    let tree = with_index(&["write-tree"])?;
    let _ = std::fs::remove_file(&index);
    let head_tree = run(git(&top).args(["rev-parse", "HEAD^{tree}"]))?;
    let snapshot = if tree == head_tree {
        None
    } else {
        Some(run(git(&top).args(["commit-tree", &tree, "-p", &head, "-m", "serpis: uncommitted work, carried with a chat"]))?)
    };
    // A ref of our own for the bundle to name, gone right after.
    let tmp_ref = format!("refs/serpis/carry/{name}");
    run(git(&top).args(["update-ref", &tmp_ref, snapshot.as_deref().unwrap_or(&head)]))?;
    let file = into.join(format!("{name}.bundle"));
    let mut args = vec!["bundle".to_string(), "create".into(), file.to_string_lossy().into_owned(), tmp_ref.clone()];
    if let Some(b) = &branch {
        args.push(b.clone());
    }
    let bundled = run(git(&top).args(&args));
    let _ = run(git(&top).args(["update-ref", "-d", &tmp_ref]));
    bundled?;
    Ok((file, Packed::Git { branch, head, snapshot, subdir }))
}

/// Unpack `file` into a new folder under `under`, named as the packed one (`name-2`, `name-3`,
/// ... when that is taken: nothing there is ever overwritten), and return the chat's folder in it.
pub fn unpack(file: &Path, packed: &Packed, under: &Path) -> Result<PathBuf> {
    std::fs::create_dir_all(under)?;
    let mut parts = Path::new(packed.subdir()).components();
    let top_name = parts.next().context("nothing packed")?.as_os_str().to_string_lossy().into_owned();
    let rest = parts.as_path().to_path_buf();
    let top = (1..).map(|i| if i == 1 { under.join(&top_name) } else { under.join(format!("{top_name}-{i}")) }).find(|p| !p.exists()).unwrap();
    match packed {
        Packed::Tar { .. } => {
            // Unpacked beside, then given its free name.
            let tmp = under.join(format!(".serpis-unpack-{}", uuid::Uuid::now_v7().simple()));
            std::fs::create_dir_all(&tmp)?;
            let r = run(Command::new("tar").arg("-xzf").arg(file).arg("-C").arg(&tmp)).and_then(|_| Ok(std::fs::rename(tmp.join(&top_name), &top)?));
            let _ = std::fs::remove_dir_all(&tmp);
            r?;
        }
        Packed::Git { branch, head, snapshot, .. } => {
            run(Command::new("git").arg("clone").arg("--quiet").arg("--no-checkout").arg(file).arg(&top))?;
            // The bundle is where it came from, not a remote worth keeping.
            let _ = run(git(&top).args(["remote", "remove", "origin"]));
            let at = snapshot.as_deref().unwrap_or(head);
            match branch {
                Some(b) => run(git(&top).args(["checkout", "--quiet", "-B", b, at]))?,
                None => run(git(&top).args(["checkout", "--quiet", "--detach", at]))?,
            };
            if snapshot.is_some() {
                // Back to the branch's commit, the work left uncommitted, as it was.
                run(git(&top).args(["reset", "--quiet", "--mixed", head]))?;
            }
        }
    }
    // (Joining an empty rest would leave a trailing slash.)
    Ok(if rest.as_os_str().is_empty() { top } else { top.join(rest) })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, script: &str) {
        let out = Command::new("sh").arg("-c").arg(script).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "{script}: {}", String::from_utf8_lossy(&out.stderr));
    }

    #[test]
    fn a_repository_arrives_with_its_branch_and_uncommitted_work() {
        let root = std::env::temp_dir().join(format!("ws-{}", uuid::Uuid::now_v7().simple()));
        let repo = root.join("here/app");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        sh(&repo, "git init -q -b main && git config user.email t@t && git config user.name t \
            && printf 'a\\n' > kept.txt && printf 'b\\n' > gone.txt && printf 'x\\n' > src/lib.rs && printf 'target/\\n' > .gitignore \
            && git add -A && git commit -qm first && git checkout -qb feature \
            && printf 'a2\\n' > kept.txt && rm gone.txt && printf 'new\\n' > src/new.rs && git add src/new.rs \
            && printf 'untracked\\n' > notes.md && mkdir target && printf 'big\\n' > target/out");
        let before = std::fs::read(repo.join(".git/index")).unwrap();
        let (file, packed) = pack(&repo.join("src"), &root.join("transfer")).unwrap();
        assert_eq!(std::fs::read(repo.join(".git/index")).unwrap(), before, "the repository's index is untouched");
        let refs = Command::new("git").arg("-C").arg(&repo).args(["for-each-ref", "refs/serpis"]).output().unwrap();
        assert!(refs.stdout.is_empty(), "no refs left behind");
        let Packed::Git { branch, snapshot, subdir, .. } = &packed else { panic!("{packed:?}") };
        assert_eq!((branch.as_deref(), subdir.as_str()), (Some("feature"), "app/src"));
        assert!(snapshot.is_some());

        let cwd = unpack(&file, &packed, &root.join("there")).unwrap();
        assert_eq!(cwd, root.join("there/app/src"), "the chat's subfolder");
        let there = root.join("there/app");
        let read = |p: &str| std::fs::read_to_string(there.join(p)).ok();
        assert_eq!(read("kept.txt").as_deref(), Some("a2\n"));
        assert_eq!(read("gone.txt"), None, "deleted stays deleted");
        assert_eq!(read("src/new.rs").as_deref(), Some("new\n"));
        assert_eq!(read("notes.md").as_deref(), Some("untracked\n"));
        assert_eq!(read("target/out"), None, "ignored files stay behind");
        let status = Command::new("git").arg("-C").arg(&there).args(["status", "--porcelain"]).output().unwrap();
        let status = String::from_utf8_lossy(&status.stdout);
        for line in [" M kept.txt", " D gone.txt", "?? notes.md", "?? src/new.rs"] {
            assert!(status.lines().any(|l| l == line), "the work is uncommitted again ({line}): {status}");
        }
        let branch_now = Command::new("git").arg("-C").arg(&there).args(["branch", "--show-current"]).output().unwrap();
        assert_eq!(String::from_utf8_lossy(&branch_now.stdout).trim(), "feature");
        let again = unpack(&file, &packed, &root.join("there")).unwrap();
        assert_eq!(again, root.join("there/app-2/src"), "never into an existing folder: beside it");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn any_other_folder_arrives_as_it_is() {
        let root = std::env::temp_dir().join(format!("ws-{}", uuid::Uuid::now_v7().simple()));
        let dir = root.join("here/notes");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("sub/a.txt"), "hi").unwrap();
        let (file, packed) = pack(&dir, &root.join("transfer")).unwrap();
        assert_eq!(packed, Packed::Tar { subdir: "notes".into() });
        let cwd = unpack(&file, &packed, &root.join("there")).unwrap();
        assert_eq!(std::fs::read_to_string(cwd.join("sub/a.txt")).unwrap(), "hi");
        let _ = std::fs::remove_dir_all(&root);
    }
}
