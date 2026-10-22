//! Git facts for projects and worktrees.
//!
//! [`repo`] reads the repository files directly (`.git`, `HEAD`, `commondir`), so it is cheap
//! enough to call while drawing. Listing worktrees, status and changes run `git` and belong
//! on a background thread.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Where a directory sits in a git repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Repo {
    /// The worktree containing the directory (the main checkout or a linked worktree).
    pub worktree: PathBuf,
    /// The main checkout: the project all worktrees belong to.
    pub main: PathBuf,
    /// Checked-out branch, or `None` when HEAD is detached.
    pub branch: Option<String>,
    /// Short commit id when HEAD is detached.
    pub detached: Option<String>,
}

impl Repo {
    pub fn is_linked(&self) -> bool {
        self.worktree != self.main
    }

    /// Branch name, or the detached commit.
    pub fn head_label(&self) -> String {
        self.branch.clone().or_else(|| self.detached.clone()).unwrap_or_default()
    }
}

fn read_trim(p: &Path) -> Option<String> {
    std::fs::read_to_string(p).ok().map(|s| s.trim().to_string())
}

/// The repository around `dir`, from its files alone (no `git` process).
pub fn repo(dir: &Path) -> Option<Repo> {
    let worktree = dir.ancestors().find(|d| d.join(".git").exists())?.to_path_buf();
    // `git worktree list` reports real paths; match them.
    let worktree = worktree.canonicalize().unwrap_or(worktree);
    let dotgit = worktree.join(".git");
    let (gitdir, main) = if dotgit.is_dir() {
        (dotgit, worktree.clone())
    } else {
        // A linked worktree: `.git` is a file pointing at `<common>/worktrees/<name>`.
        let line = read_trim(&dotgit)?;
        let gitdir = worktree.join(line.strip_prefix("gitdir:")?.trim());
        let common = match read_trim(&gitdir.join("commondir")) {
            Some(c) => gitdir.join(c),
            None => gitdir.parent()?.parent()?.to_path_buf(),
        };
        let common = common.canonicalize().unwrap_or(common);
        let main = if common.file_name().is_some_and(|f| f == ".git") { common.parent()?.to_path_buf() } else { common };
        (gitdir, main)
    };
    let head = read_trim(&gitdir.join("HEAD")).unwrap_or_default();
    let (branch, detached) = match head.strip_prefix("ref: refs/heads/") {
        Some(b) => (Some(b.to_string()), None),
        None => (None, Some(head.chars().take(7).collect())),
    };
    Some(Repo { worktree, main, branch, detached })
}

/// One entry of `git worktree list`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Worktree {
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    pub main: bool,
    pub locked: bool,
    pub prunable: bool,
}

/// Changes in a worktree, from `git status` and `git diff`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    /// Files with staged or unstaged changes (untracked excluded).
    pub changed: usize,
    pub untracked: usize,
    pub ahead: u32,
    pub behind: u32,
    /// Lines added and removed against HEAD.
    pub added: u64,
    pub removed: u64,
    pub upstream: Option<String>,
}

impl Status {
    pub fn dirty(&self) -> bool {
        self.changed + self.untracked > 0
    }
}

fn git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = Command::new("git").arg("-C").arg(dir).args(args).output()?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        anyhow::bail!("git {}: {}", args.join(" "), if err.is_empty() { format!("exit {}", out.status) } else { err });
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub fn parse_worktrees(porcelain: &str) -> Vec<Worktree> {
    let mut out: Vec<Worktree> = Vec::new();
    for block in porcelain.split("\n\n") {
        let mut w = Worktree::default();
        for line in block.lines() {
            let (k, v) = line.split_once(' ').unwrap_or((line, ""));
            match k {
                "worktree" => w.path = PathBuf::from(v),
                "HEAD" => w.head = v.chars().take(7).collect(),
                "branch" => w.branch = Some(v.strip_prefix("refs/heads/").unwrap_or(v).to_string()),
                "locked" => w.locked = true,
                "prunable" => w.prunable = true,
                _ => {}
            }
        }
        if !w.path.as_os_str().is_empty() {
            w.main = out.is_empty();
            out.push(w);
        }
    }
    out
}

/// Worktrees of the repository at `dir`, main checkout first.
pub fn worktrees(dir: &Path) -> anyhow::Result<Vec<Worktree>> {
    Ok(parse_worktrees(&git(dir, &["worktree", "list", "--porcelain"])?))
}

pub fn parse_status(porcelain: &str) -> Status {
    let mut s = Status::default();
    for line in porcelain.lines() {
        if let Some(head) = line.strip_prefix("## ") {
            // `main...origin/main [ahead 1, behind 2]`
            if let Some((_, rest)) = head.split_once("...") {
                let upstream = rest.split(' ').next().unwrap_or("");
                s.upstream = (!upstream.is_empty()).then(|| upstream.to_string());
            }
            if let Some(i) = head.find('[') {
                for part in head[i + 1..].trim_end_matches(']').split(", ") {
                    if let Some(n) = part.strip_prefix("ahead ") {
                        s.ahead = n.parse().unwrap_or(0);
                    } else if let Some(n) = part.strip_prefix("behind ") {
                        s.behind = n.parse().unwrap_or(0);
                    }
                }
            }
        } else if line.starts_with("??") {
            s.untracked += 1;
        } else if line.len() > 2 {
            s.changed += 1;
        }
    }
    s
}

/// `git diff --shortstat` → (added, removed).
pub fn parse_shortstat(text: &str) -> (u64, u64) {
    let mut added = 0;
    let mut removed = 0;
    for part in text.trim().split(", ") {
        let n = part.split(' ').next().and_then(|n| n.parse().ok()).unwrap_or(0);
        if part.contains("insertion") {
            added = n;
        } else if part.contains("deletion") {
            removed = n;
        }
    }
    (added, removed)
}

pub fn status(dir: &Path) -> anyhow::Result<Status> {
    let mut s = parse_status(&git(dir, &["status", "--porcelain=v1", "-b"])?);
    // An unborn branch has no HEAD to diff against.
    if let Ok(stat) = git(dir, &["diff", "--shortstat", "HEAD"]) {
        (s.added, s.removed) = parse_shortstat(&stat);
    }
    Ok(s)
}

/// A branch a new worktree can start from.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Branch {
    /// `main`, or `origin/main` for a remote branch.
    pub name: String,
    pub remote: bool,
    /// Checked out where the list was read.
    pub current: bool,
    /// Last commit, in seconds since the epoch.
    pub time: i64,
    pub subject: String,
    /// The worktree it is checked out in, if any.
    pub worktree: Option<PathBuf>,
}

impl Branch {
    /// A remote branch's name without its remote (`origin/fix` → `fix`).
    pub fn local_name(&self) -> &str {
        if self.remote { self.name.split_once('/').map_or(self.name.as_str(), |(_, n)| n) } else { &self.name }
    }
}

const BRANCH_FORMAT: &str = "--format=%(refname)%00%(refname:short)%00%(committerdate:unix)%00%(HEAD)%00%(worktreepath)%00%(subject)";

/// `git for-each-ref` lines in [`BRANCH_FORMAT`] → branches in the order given; remotes' HEAD
/// pointers are left out.
pub fn parse_branches(text: &str) -> Vec<Branch> {
    text.lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\0').collect();
            let [refname, short, time, head, worktree, subject] = f[..] else { return None };
            if refname.ends_with("/HEAD") {
                return None;
            }
            Some(Branch {
                name: short.to_string(),
                remote: refname.starts_with("refs/remotes/"),
                current: head == "*",
                time: time.parse().unwrap_or(0),
                subject: subject.to_string(),
                worktree: (!worktree.is_empty()).then(|| PathBuf::from(worktree)),
            })
        })
        .collect()
}

/// Local branches (the one checked out at `dir` first), then remote ones, newest first.
pub fn branches(dir: &Path) -> anyhow::Result<Vec<Branch>> {
    let mut list = parse_branches(&git(dir, &["for-each-ref", "--sort=-committerdate", BRANCH_FORMAT, "refs/heads", "refs/remotes"])?);
    list.sort_by_key(|b| (!b.current, b.remote));
    Ok(list)
}

/// Whether `rev` names a commit (a branch, tag, or hash).
pub fn is_commit(repo: &Path, rev: &str) -> bool {
    git(repo, &["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")]).is_ok()
}

pub fn branch_exists(repo: &Path, branch: &str) -> bool {
    git(repo, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{branch}")]).is_ok()
}

/// Add a worktree at `path`: on a new `branch` from `base` (default HEAD), or on `branch`
/// itself when it already exists.
pub fn add_worktree(repo: &Path, path: &Path, branch: &str, base: Option<&str>) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let p = path.to_string_lossy();
    if branch_exists(repo, branch) {
        git(repo, &["worktree", "add", &p, branch])?;
    } else {
        let mut args = vec!["worktree", "add", "-b", branch, &p];
        if let Some(b) = base {
            args.push(b);
        }
        git(repo, &args)?;
    }
    Ok(())
}

pub fn remove_worktree(repo: &Path, path: &Path, force: bool) -> anyhow::Result<()> {
    let p = path.to_string_lossy();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&p);
    git(repo, &args)?;
    Ok(())
}

pub fn delete_branch(repo: &Path, branch: &str, force: bool) -> anyhow::Result<()> {
    git(repo, &["branch", if force { "-D" } else { "-d" }, branch])?;
    Ok(())
}

/// A branch- and path-safe slug: `Fix the tabs!` → `fix-the-tabs`.
pub fn slug(text: &str) -> String {
    let mut s = String::new();
    for c in text.chars() {
        if c.is_ascii_alphanumeric() || c == '/' || c == '.' || c == '_' {
            s.push(c.to_ascii_lowercase());
        } else if !s.ends_with('-') {
            s.push('-');
        }
    }
    s.trim_matches(|c| c == '-' || c == '/' || c == '.').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_branches_and_skips_remote_head_pointers() {
        let text = [
            "refs/heads/main\0main\01700000300\0*\0/repo\0Fix tabs",
            "refs/heads/feature\0feature\01700000200\0 \0\0Add parser",
            "refs/remotes/origin/HEAD\0origin\01700000300\0 \0\0Fix tabs",
            "refs/remotes/origin/fix/x\0origin/fix/x\01700000100\0 \0\0Remote work",
        ]
        .join("\n");
        let b = parse_branches(&text);
        assert_eq!(b.len(), 3);
        assert_eq!((b[0].name.as_str(), b[0].current, b[0].worktree.as_deref()), ("main", true, Some(Path::new("/repo"))));
        assert_eq!((b[1].name.as_str(), b[1].current, b[1].worktree.as_deref(), b[1].time), ("feature", false, None, 1700000200));
        assert_eq!((b[2].name.as_str(), b[2].remote, b[2].local_name(), b[2].subject.as_str()), ("origin/fix/x", true, "fix/x", "Remote work"));
    }

    #[test]
    fn lists_real_branches_with_the_current_one_first() {
        let dir = std::env::temp_dir().join(format!("serpis-branches-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let sh = |args: &[&str]| assert!(Command::new("git").arg("-C").arg(&dir).args(args).output().unwrap().status.success(), "{args:?}");
        sh(&["init", "-q", "-b", "main"]);
        sh(&["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "init"]);
        sh(&["branch", "older"]);
        sh(&["checkout", "-q", "-b", "work"]);
        let b = branches(&dir).unwrap();
        assert_eq!(b[0].name, "work");
        assert!(b[0].current);
        assert!(b.iter().any(|x| x.name == "older" && !x.current));
        assert!(is_commit(&dir, "older") && !is_commit(&dir, "nope"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn parses_worktree_list() {
        let text = "worktree /r\nHEAD 0123456789\nbranch refs/heads/main\n\nworktree /w/a\nHEAD abcdef0123\nbranch refs/heads/serpis/a\nlocked\n\nworktree /w/b\nHEAD 999\ndetached\n";
        let w = parse_worktrees(text);
        assert_eq!(w.len(), 3);
        assert!(w[0].main && !w[1].main);
        assert_eq!(w[0].branch.as_deref(), Some("main"));
        assert_eq!(w[1].branch.as_deref(), Some("serpis/a"));
        assert!(w[1].locked);
        assert_eq!(w[2].branch, None);
        assert_eq!(w[0].head, "0123456");
    }

    #[test]
    fn parses_status() {
        let s = parse_status("## main...origin/main [ahead 2, behind 1]\n M a.rs\nA  b.rs\n?? c.rs\n");
        assert_eq!((s.changed, s.untracked, s.ahead, s.behind), (2, 1, 2, 1));
        assert_eq!(s.upstream.as_deref(), Some("origin/main"));
        let s = parse_status("## No commits yet on main\n");
        assert_eq!((s.changed, s.ahead), (0, 0));
        assert_eq!(parse_shortstat(" 3 files changed, 12 insertions(+), 4 deletions(-)\n"), (12, 4));
        assert_eq!(parse_shortstat(" 1 file changed, 1 deletion(-)"), (0, 1));
    }

    #[test]
    fn slugs() {
        assert_eq!(slug("Fix the tabs!"), "fix-the-tabs");
        assert_eq!(slug("serpis/New Thing"), "serpis/new-thing");
    }

    #[test]
    fn reads_repo_and_linked_worktrees() {
        let tmp = std::env::temp_dir().join(format!("serpis-git-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        let main = tmp.join("proj");
        std::fs::create_dir_all(main.join("sub")).unwrap();
        let run = |dir: &Path, args: &[&str]| assert!(Command::new("git").arg("-C").arg(dir).args(args).output().unwrap().status.success(), "{args:?}");
        run(&main, &["init", "-q", "-b", "main"]);
        run(&main, &["-c", "user.email=t@t", "-c", "user.name=t", "commit", "-q", "--allow-empty", "-m", "init"]);
        let main = main.canonicalize().unwrap();
        let r = repo(&main.join("sub")).unwrap();
        assert_eq!((r.worktree.clone(), r.main.clone(), r.branch.as_deref()), (main.clone(), main.clone(), Some("main")));
        let wt = tmp.join("wt").join("feature");
        add_worktree(&main, &wt, "feature", None).unwrap();
        let r = repo(&wt).unwrap();
        assert!(r.is_linked());
        assert_eq!(r.main, main);
        assert_eq!(r.branch.as_deref(), Some("feature"));
        let list = worktrees(&main).unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].branch.as_deref(), Some("feature"));
        std::fs::write(wt.join("x.txt"), "a\n").unwrap();
        assert_eq!(status(&wt).unwrap().untracked, 1);
        remove_worktree(&main, &wt, true).unwrap();
        assert_eq!(worktrees(&main).unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
