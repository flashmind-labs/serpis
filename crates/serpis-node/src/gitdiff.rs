//! What a diff reads from a repository: which files differ between two states of it, and both
//! versions of each. It runs where the repository is, in a machine's node for a chat there and
//! on a background thread of the window for one here, and leaves comparing the text to the
//! client, so comparing works the same for any machine.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};
use serpis_proto::wire::{DiffBranch, DiffEntry, DiffList, DiffRefs, DiffSide, DiffSpec, DiffText, Request, Response};

/// A version larger than this isn't read: nobody reviews it line by line.
pub const MAX_FILE: usize = 8 << 20;
/// git's id of the empty tree: what an unborn branch is compared from.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// Answer a diff request.
pub fn answer(request: &Request) -> anyhow::Result<Response> {
    Ok(match request {
        Request::DiffRefs { dir } => Response::DiffRefs { refs: refs(Path::new(dir))? },
        Request::DiffList { dir, spec } => Response::DiffList { list: list(Path::new(dir), spec)? },
        Request::DiffFile { dir, spec, path, old_path } => {
            let (old, new) = file(Path::new(dir), spec, path, old_path.as_deref())?;
            Response::DiffFile { old, new }
        }
        _ => bail!("not a diff request"),
    })
}

fn raw(dir: &Path, args: &[&str]) -> anyhow::Result<std::process::Output> {
    // No locks: this runs every few seconds beside whatever an agent does in the repository.
    Command::new("git").arg("-C").arg(dir).env("GIT_OPTIONAL_LOCKS", "0").args(args).output().context("running git")
}

fn git(dir: &Path, args: &[&str]) -> anyhow::Result<String> {
    let out = raw(dir, args)?;
    if !out.status.success() {
        bail!("{}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn root_of(dir: &Path) -> anyhow::Result<PathBuf> {
    match git(dir, &["rev-parse", "--show-toplevel"]) {
        Ok(r) => Ok(PathBuf::from(r.trim_end_matches('\n'))),
        Err(_) => bail!("{} is not in a git repository", dir.display()),
    }
}

fn check_rev(rev: &str) -> anyhow::Result<()> {
    if rev.is_empty() || rev.starts_with('-') {
        bail!("{rev:?} isn't a branch or commit");
    }
    Ok(())
}

pub fn refs(dir: &Path) -> anyhow::Result<DiffRefs> {
    let root = root_of(dir)?;
    let (head, detached) = match git(&root, &["symbolic-ref", "--quiet", "--short", "HEAD"]) {
        Ok(b) => (b.trim().to_string(), false),
        Err(_) => (git(&root, &["rev-parse", "--short", "HEAD"]).unwrap_or_default().trim().to_string(), true),
    };
    let format = "--format=%(refname)%00%(refname:short)%00%(HEAD)%00%(subject)";
    let text = git(&root, &["for-each-ref", "--sort=-committerdate", format, "refs/heads", "refs/remotes"])?;
    let mut branches: Vec<DiffBranch> = text
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\0').collect();
            let [refname, short, current, subject] = f[..] else { return None };
            (!refname.ends_with("/HEAD")).then(|| DiffBranch { name: short.to_string(), remote: refname.starts_with("refs/remotes/"), current: current == "*", subject: subject.to_string() })
        })
        .collect();
    branches.sort_by_key(|b| b.remote);
    let local = |n: &str| branches.iter().any(|b| b.name == n);
    let default_branch = git(&root, &["symbolic-ref", "--quiet", "--short", "refs/remotes/origin/HEAD"])
        .ok()
        .map(|o| o.trim().to_string())
        .map(|o| o.strip_prefix("origin/").filter(|b| local(b)).map_or(o.clone(), str::to_string))
        .or_else(|| ["main", "master", "trunk", "develop"].into_iter().find(|b| local(b)).map(str::to_string));
    let mut worktrees = Vec::new();
    let porcelain = git(&root, &["worktree", "list", "--porcelain"]).unwrap_or_default();
    for block in porcelain.split("\n\n") {
        let (mut path, mut name) = (None, String::new());
        for line in block.lines() {
            match line.split_once(' ') {
                Some(("worktree", p)) => path = Some(p.to_string()),
                Some(("branch", b)) => name = b.strip_prefix("refs/heads/").unwrap_or(b).to_string(),
                Some(("HEAD", h)) if name.is_empty() => name = h.chars().take(7).collect(),
                _ => {}
            }
        }
        if let Some(p) = path {
            worktrees.push((p, name));
        }
    }
    Ok(DiffRefs { root: root.to_string_lossy().into_owned(), head, detached, default_branch, branches, worktrees })
}

/// The commit a side of a comparison stands on, for finding where it forked.
fn commit_of(side: &DiffSide) -> &str {
    match side {
        DiffSide::Rev(r) => r,
        _ => "HEAD",
    }
}

/// `spec`'s left side after finding the merge base: (what to compare from, the merge base).
fn resolve_base(root: &Path, spec: &DiffSpec) -> anyhow::Result<(DiffSide, Option<String>)> {
    if let DiffSide::Rev(r) = &spec.base {
        check_rev(r)?;
    }
    if let DiffSide::Rev(r) = &spec.target {
        check_rev(r)?;
    }
    let exists = |rev: &str| raw(root, &["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")]).is_ok_and(|o| o.status.success());
    match &spec.base {
        DiffSide::Rev(r) if spec.merge_base => {
            if !exists(r) {
                bail!("There is no branch or commit {r}");
            }
            // Unrelated histories have none: compare with the branch itself.
            match git(root, &["merge-base", r, commit_of(&spec.target)]) {
                Ok(m) => {
                    let m = m.trim().to_string();
                    Ok((DiffSide::Rev(m.clone()), Some(m)))
                }
                Err(_) => Ok((spec.base.clone(), None)),
            }
        }
        // A new repository has no HEAD yet: everything in it is new.
        DiffSide::Rev(r) if r == "HEAD" && !exists("HEAD") => Ok((DiffSide::Rev(EMPTY_TREE.to_string()), None)),
        DiffSide::Rev(r) if !exists(r) => bail!("There is no branch or commit {r}"),
        other => Ok((other.clone(), None)),
    }
}

/// git's arguments naming a comparison.
fn compare_args(base: &DiffSide, target: &DiffSide) -> anyhow::Result<Vec<String>> {
    Ok(match (base, target) {
        (DiffSide::Rev(a), DiffSide::Rev(b)) => vec![a.clone(), b.clone()],
        (DiffSide::Rev(a), DiffSide::Index) => vec!["--cached".into(), a.clone()],
        (DiffSide::Rev(a), DiffSide::Worktree) => vec![a.clone()],
        (DiffSide::Index, DiffSide::Worktree) => vec![],
        _ => bail!("Can't compare those: the left side must be older than the right"),
    })
}

/// `git diff --numstat -z` → path to (added, removed); None for a binary file.
pub fn parse_numstat(text: &str) -> HashMap<String, Option<(u64, u64)>> {
    let mut out = HashMap::new();
    let mut parts = text.split('\0');
    while let Some(head) = parts.next() {
        let mut f = head.splitn(3, '\t');
        let (Some(a), Some(d), Some(path)) = (f.next(), f.next(), f.next()) else { continue };
        // A rename: an empty path here, then the old path and the new one.
        let path = if path.is_empty() {
            let _old = parts.next();
            parts.next().unwrap_or_default()
        } else {
            path
        };
        out.insert(path.to_string(), a.parse().ok().zip(d.parse().ok()));
    }
    out
}

/// `git diff --raw -z --no-abbrev` → the files, with their blob ids as their signature.
pub fn parse_raw(text: &str) -> Vec<DiffEntry> {
    let mut out = Vec::new();
    let mut parts = text.split('\0');
    while let Some(head) = parts.next() {
        let Some(meta) = head.strip_prefix(':') else { continue };
        let f: Vec<&str> = meta.split(' ').collect();
        let [mode_a, mode_b, old, new, status] = f[..] else { continue };
        let first = parts.next().unwrap_or_default().to_string();
        let kind = status.chars().next().unwrap_or('M');
        let (path, old_path) = if matches!(kind, 'R' | 'C') { (parts.next().unwrap_or_default().to_string(), Some(first)) } else { (first, None) };
        out.push(DiffEntry { path, old_path, status: kind.to_string(), signature: format!("{mode_a}{mode_b}{old}{new}"), ..Default::default() });
    }
    out
}

/// A file on disk, as a signature: git reports no id for uncommitted content.
fn stamp(path: &Path) -> String {
    match std::fs::symlink_metadata(path) {
        Ok(m) => {
            let at = m.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map_or(0, |d| d.as_nanos());
            format!("{at}:{}", m.len())
        }
        Err(_) => "gone".into(),
    }
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(8192)].contains(&0)
}

pub fn list(dir: &Path, spec: &DiffSpec) -> anyhow::Result<DiffList> {
    let root = root_of(dir)?;
    let (base, merge_base) = resolve_base(&root, spec)?;
    let compare = compare_args(&base, &spec.target)?;
    let run = |extra: &[&str]| -> anyhow::Result<String> {
        let mut args: Vec<&str> = vec!["diff", "--no-ext-diff", "--no-color", "-M", "-z"];
        args.extend_from_slice(extra);
        args.extend(compare.iter().map(String::as_str));
        args.push("--");
        git(&root, &args)
    };
    let mut files = parse_raw(&run(&["--raw", "--no-abbrev"])?);
    let stats = parse_numstat(&run(&["--numstat"])?);
    let on_disk = spec.target == DiffSide::Worktree;
    for f in &mut files {
        match stats.get(&f.path) {
            Some(Some((a, r))) => (f.added, f.removed) = (*a, *r),
            Some(None) => f.binary = true,
            None => {}
        }
        if on_disk {
            f.signature.push_str(&stamp(&root.join(&f.path)));
        }
    }
    if on_disk {
        let others = git(&root, &["ls-files", "--others", "--exclude-standard", "-z"])?;
        for path in others.split('\0').filter(|p| !p.is_empty()) {
            let full = root.join(path);
            let bytes = if std::fs::metadata(&full).is_ok_and(|m| m.len() as usize <= MAX_FILE) { std::fs::read(&full).unwrap_or_default() } else { Vec::new() };
            let binary = is_binary(&bytes);
            let added = if binary { 0 } else { bytes.iter().filter(|b| **b == b'\n').count() as u64 + u64::from(bytes.last().is_some_and(|b| *b != b'\n')) };
            files.push(DiffEntry { path: path.to_string(), status: "A".into(), untracked: true, binary, added, signature: stamp(&full), ..Default::default() });
        }
    }
    files.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(DiffList { root: root.to_string_lossy().into_owned(), merge_base, files })
}

fn text_of(bytes: Vec<u8>) -> DiffText {
    if bytes.len() > MAX_FILE {
        return DiffText { too_large: true, ..Default::default() };
    }
    if is_binary(&bytes) {
        return DiffText { binary: true, ..Default::default() };
    }
    DiffText { text: String::from_utf8_lossy(&bytes).into_owned(), ..Default::default() }
}

fn version(root: &Path, side: &DiffSide, path: &str) -> anyhow::Result<DiffText> {
    let spec = match side {
        DiffSide::Worktree => {
            let full = root.join(path);
            let Ok(meta) = std::fs::symlink_metadata(&full) else { return Ok(DiffText { missing: true, ..Default::default() }) };
            if meta.is_symlink() {
                return Ok(DiffText { text: std::fs::read_link(&full)?.to_string_lossy().into_owned(), ..Default::default() });
            }
            if meta.is_dir() {
                // A submodule: its commit is what could be compared, and git reported that.
                return Ok(DiffText { binary: true, ..Default::default() });
            }
            if meta.len() as usize > MAX_FILE {
                return Ok(DiffText { too_large: true, ..Default::default() });
            }
            return Ok(text_of(std::fs::read(&full)?));
        }
        DiffSide::Index => format!(":{path}"),
        DiffSide::Rev(r) => format!("{r}:{path}"),
    };
    let out = raw(root, &["cat-file", "blob", &spec])?;
    if !out.status.success() {
        return Ok(DiffText { missing: true, ..Default::default() });
    }
    Ok(text_of(out.stdout))
}

pub fn file(dir: &Path, spec: &DiffSpec, path: &str, old_path: Option<&str>) -> anyhow::Result<(DiffText, DiffText)> {
    let root = root_of(dir)?;
    let (base, _) = resolve_base(&root, spec)?;
    Ok((version(&root, &base, old_path.unwrap_or(path))?, version(&root, &spec.target, path)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, args: &[&str]) {
        let ok = Command::new("git").arg("-C").arg(dir).args(["-c", "user.email=t@t", "-c", "user.name=t"]).args(args).output().unwrap();
        assert!(ok.status.success(), "{args:?}: {}", String::from_utf8_lossy(&ok.stderr));
    }

    fn repo(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("serpis-gitdiff-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        sh(&dir, &["init", "-q", "-b", "main"]);
        dir
    }

    fn rev(r: &str) -> DiffSide {
        DiffSide::Rev(r.to_string())
    }

    #[test]
    fn parses_raw_and_numstat_with_renames() {
        let raw = ":100644 100644 aaa bbb M\0src/a.rs\0:100644 100644 ccc ddd R097\0old name.rs\0new name.rs\0:000000 100644 000 eee A\0n.rs\0";
        let f = parse_raw(raw);
        assert_eq!(f.len(), 3);
        assert_eq!((f[0].path.as_str(), f[0].status.as_str()), ("src/a.rs", "M"));
        assert_eq!((f[1].path.as_str(), f[1].old_path.as_deref(), f[1].status.as_str()), ("new name.rs", Some("old name.rs"), "R"));
        assert_eq!(f[2].status, "A");
        let n = parse_numstat("3\t1\tsrc/a.rs\x002\t0\t\0old name.rs\0new name.rs\0-\t-\tlogo.png\0");
        assert_eq!(n["src/a.rs"], Some((3, 1)));
        assert_eq!(n["new name.rs"], Some((2, 0)));
        assert_eq!(n["logo.png"], None);
    }

    #[test]
    fn every_kind_of_comparison_reads_the_right_versions() {
        let dir = repo("kinds");
        std::fs::write(dir.join("a.txt"), "one\n").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-qm", "init"]);
        sh(&dir, &["checkout", "-qb", "work"]);
        std::fs::write(dir.join("a.txt"), "committed\n").unwrap();
        sh(&dir, &["commit", "-qam", "work"]);
        std::fs::write(dir.join("a.txt"), "staged\n").unwrap();
        sh(&dir, &["add", "."]);
        std::fs::write(dir.join("a.txt"), "disk\n").unwrap();
        std::fs::write(dir.join("new.txt"), "fresh\nfile\n").unwrap();
        // main moves on: a pull request's diff doesn't show this.
        sh(&dir, &["stash", "-q", "-u"]);
        sh(&dir, &["checkout", "-q", "main"]);
        std::fs::write(dir.join("later.txt"), "main only\n").unwrap();
        sh(&dir, &["add", "."]);
        sh(&dir, &["commit", "-qm", "later"]);
        sh(&dir, &["checkout", "-q", "work"]);
        sh(&dir, &["stash", "pop", "-q", "--index"]);

        let texts = |spec: DiffSpec| {
            let (o, n) = file(&dir, &spec, "a.txt", None).unwrap();
            (o.text, n.text)
        };
        let s = |base, target, merge_base| DiffSpec { base, target, merge_base };
        assert_eq!(texts(s(rev("HEAD"), DiffSide::Worktree, false)), ("committed\n".into(), "disk\n".into()));
        assert_eq!(texts(s(rev("HEAD"), DiffSide::Index, false)), ("committed\n".into(), "staged\n".into()));
        assert_eq!(texts(s(DiffSide::Index, DiffSide::Worktree, false)), ("staged\n".into(), "disk\n".into()));
        assert_eq!(texts(s(rev("main"), rev("work"), true)), ("one\n".into(), "committed\n".into()));

        let names = |spec: DiffSpec| list(&dir, &spec).unwrap().files.into_iter().map(|f| f.path).collect::<Vec<_>>();
        // Untracked files count as uncommitted work; what main added since forking doesn't show.
        assert_eq!(names(s(rev("main"), DiffSide::Worktree, true)), ["a.txt", "new.txt"]);
        assert_eq!(names(s(rev("main"), DiffSide::Worktree, false)), ["a.txt", "later.txt", "new.txt"]);
        assert_eq!(names(s(rev("HEAD"), DiffSide::Index, false)), ["a.txt"]);
        let l = list(&dir, &s(rev("main"), DiffSide::Worktree, true)).unwrap();
        assert!(l.merge_base.is_some());
        let new = l.files.iter().find(|f| f.path == "new.txt").unwrap();
        assert!(new.untracked);
        assert_eq!((new.status.as_str(), new.added), ("A", 2));

        // Writing a file changes its signature, without git knowing.
        let before = list(&dir, &s(rev("HEAD"), DiffSide::Worktree, false)).unwrap().files[0].signature.clone();
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(dir.join("a.txt"), "disk again\n").unwrap();
        assert_ne!(before, list(&dir, &s(rev("HEAD"), DiffSide::Worktree, false)).unwrap().files[0].signature);

        let r = refs(&dir).unwrap();
        assert_eq!((r.head.as_str(), r.default_branch.as_deref()), ("work", Some("main")));
        assert!(r.branches.iter().any(|b| b.name == "main" && !b.current));
        assert!(!r.worktrees.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_new_repository_and_odd_files() {
        let dir = repo("new");
        std::fs::write(dir.join("a.txt"), "hello\n").unwrap();
        std::fs::write(dir.join("b.bin"), b"\0\x01\x02").unwrap();
        let spec = DiffSpec { base: rev("HEAD"), target: DiffSide::Worktree, merge_base: false };
        let l = list(&dir, &spec).unwrap();
        assert_eq!(l.files.len(), 2);
        assert!(l.files[1].binary);
        let (o, n) = file(&dir, &spec, "a.txt", None).unwrap();
        assert!(o.missing);
        assert_eq!(n.text, "hello\n");
        assert!(file(&dir, &spec, "b.bin", None).unwrap().1.binary);
        assert!(list(&dir, &DiffSpec { base: rev("--help"), target: DiffSide::Worktree, merge_base: false }).is_err());
        assert!(list(&dir, &DiffSpec { base: rev("nope"), target: DiffSide::Worktree, merge_base: true }).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
