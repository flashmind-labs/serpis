//! Carrying a chat's folder to another machine, and back.
//!
//! A git repository goes as a bundle of its current branch plus a snapshot commit of the
//! uncommitted work (changes, deletions and untracked files; ignored ones stay behind), made
//! through a temporary index and a temporary ref, so the repository itself is untouched. Any other
//! folder goes as a tar.gz. Either way the chat's place inside it (a subfolder) is kept.
//!
//! A carried folder keeps its identity. Each machine has a registry, `~/.serpis/carried.json`,
//! of where each carried repository (by its first commit) and branch lives there and of the state
//! serpis last left it in; leaving a machine records the folder it left too. So arriving goes:
//! 1. to the folder the registry names (where it was before, or where it came from), updated in
//!    place;
//! 2. else, when a clone of the same repository is at the same place in the home as where it
//!    came from, into a worktree of that clone (sharing its history; the clone's own working tree
//!    is not touched);
//! 3. else to a new folder, `~/.serpis/worktrees/{project}/{branch}` (`~/.serpis/carried/{project}`
//!    for other folders), or wherever [`Places`] says.
//!
//! Nothing there is lost: uncommitted changes made there since serpis last left the folder are
//! stashed first (`git stash list`), commits made there since are kept on a branch of their own,
//! and a plain folder changed there is moved aside. What happened is in [`Placed::note`].
//!
//! Both ends use `git` and `tar` from PATH.

use std::collections::BTreeMap;
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
    Git {
        branch: Option<String>,
        head: String,
        snapshot: Option<String>,
        /// The repository's name, then the chat's folder inside it.
        subdir: String,
        /// The repository's first commit: the same repository wherever it is cloned.
        #[serde(default)]
        repo_id: String,
        /// Where it was, relative to the home there (None outside the home).
        #[serde(default)]
        home_path: Option<String>,
    },
    /// A tar.gz of the folder.
    Tar {
        subdir: String,
        #[serde(default)]
        home_path: Option<String>,
        /// Its key in the registries: the one it arrived with, so it is the same folder
        /// wherever it went (a plain folder has nothing else to tell it by).
        #[serde(default)]
        key: String,
    },
}

impl Packed {
    /// The folder's name, then the chat's folder inside it.
    pub fn subdir(&self) -> &str {
        match self {
            Packed::Git { subdir, .. } | Packed::Tar { subdir, .. } => subdir,
        }
    }

    fn home_path(&self) -> Option<&str> {
        match self {
            Packed::Git { home_path, .. } | Packed::Tar { home_path, .. } => home_path.as_deref(),
        }
    }

    /// Its key in the registry: the repository and branch, or the folder.
    fn key(&self) -> String {
        match self {
            Packed::Git { repo_id, branch, .. } => format!("git:{repo_id}:{}", branch.as_deref().unwrap_or("(detached)")),
            Packed::Tar { key, .. } if !key.is_empty() => key.clone(),
            Packed::Tar { subdir, home_path, .. } => format!("dir:{}", home_path.as_deref().unwrap_or(subdir)),
        }
    }
}

/// Where carried folders go on a machine.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Places {
    /// For repositories: `{project}`, `{branch}` and `{path}` (where it was, relative to the home
    /// there) are filled in; `~` is the home.
    pub repo: String,
    /// For other folders (`{project}`, `{path}`).
    pub plain: String,
}

impl Default for Places {
    fn default() -> Self {
        Places { repo: "~/.serpis/worktrees/{project}/{branch}".into(), plain: "~/.serpis/carried/{project}".into() }
    }
}

/// Where a carried folder went, and anything kept aside on the way.
#[derive(Debug, Clone, PartialEq)]
pub struct Placed {
    pub cwd: PathBuf,
    pub note: Option<String>,
}

/// The registry of carried folders on this machine (`~/.serpis/carried.json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub path: PathBuf,
    /// The state serpis last left it in: the git tree of the working state, or a hash of the files.
    pub state: String,
    /// The commit it was on (repositories).
    #[serde(default)]
    pub head: String,
    /// Its project's name: what it was called where it came from, wherever it is now.
    #[serde(default)]
    pub project: String,
}

fn registry_path(home: &Path) -> PathBuf {
    home.join(".serpis/carried.json")
}

fn load_registry(home: &Path) -> BTreeMap<String, Entry> {
    std::fs::read_to_string(registry_path(home)).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn record(home: &Path, key: &str, entry: Entry) -> Result<()> {
    let mut reg = load_registry(home);
    reg.insert(key.to_string(), entry);
    let path = registry_path(home);
    std::fs::create_dir_all(path.parent().unwrap())?;
    let tmp = path.with_extension("json.part");
    std::fs::write(&tmp, serde_json::to_string_pretty(&reg)?)?;
    std::fs::rename(tmp, path)?;
    Ok(())
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

/// The working state of repository `top` as a tree (tracked and untracked files, not ignored
/// ones), without touching its index.
fn state_tree(top: &Path) -> Result<String> {
    let index = std::env::temp_dir().join(format!("serpis-index-{}", uuid::Uuid::now_v7().simple()));
    // (A linked worktree's .git is a file: git says where its index is.)
    if let Ok(i) = run(git(top).args(["rev-parse", "--path-format=absolute", "--git-path", "index"])) {
        std::fs::copy(i, &index).ok();
    }
    let with_index = |args: &[&str]| -> Result<String> { run(git(top).env("GIT_INDEX_FILE", &index).args(args)) };
    let tree = with_index(&["add", "-A"]).and_then(|_| with_index(&["write-tree"]));
    let _ = std::fs::remove_file(&index);
    tree
}

fn repo_id(top: &Path) -> Result<String> {
    Ok(run(git(top).args(["rev-list", "--max-parents=0", "HEAD"]))?.lines().last().unwrap_or_default().to_string())
}

fn repo_top(dir: &Path) -> Option<PathBuf> {
    run(git(dir).args(["rev-parse", "--show-toplevel"])).ok().map(PathBuf::from).map(|p| p.canonicalize().unwrap_or(p))
}

/// A hash of every file under `dir` (paths and contents): whether a plain folder changed.
fn content_hash(dir: &Path) -> String {
    fn walk(base: &Path, d: &Path, h: &mut u64) {
        let mut entries: Vec<PathBuf> = std::fs::read_dir(d).into_iter().flatten().flatten().map(|e| e.path()).collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(base, &p, h);
            } else {
                for b in p.strip_prefix(base).unwrap_or(&p).to_string_lossy().bytes().chain(std::fs::read(&p).unwrap_or_default()) {
                    *h ^= b as u64;
                    *h = h.wrapping_mul(0x100000001b3);
                }
            }
        }
    }
    let mut h = 0xcbf29ce484222325;
    walk(dir, dir, &mut h);
    format!("{h:016x}")
}

fn home_rel(path: &Path, home: &Path) -> Option<String> {
    let home = home.canonicalize().unwrap_or(home.to_path_buf());
    path.strip_prefix(&home).ok().map(|r| r.to_string_lossy().into_owned()).filter(|r| !r.is_empty())
}

/// Pack `dir` (a chat's folder) into a file in `into`: its repository when it is in one, else
/// the folder itself. Leaving, the folder is recorded in this machine's registry (under `home`),
/// so the chat finds it again when it comes back.
pub fn pack(dir: &Path, into: &Path, home: &Path) -> Result<(PathBuf, Packed)> {
    std::fs::create_dir_all(into)?;
    let name = format!("pack-{}", uuid::Uuid::now_v7().simple());
    let dir = dir.canonicalize().unwrap_or(dir.to_path_buf());
    let Some(top) = repo_top(&dir) else {
        let file = into.join(format!("{name}.tar.gz"));
        let parent = dir.parent().context("a folder at the root can't be carried")?;
        let base = dir.file_name().context("no folder name")?;
        run(Command::new("tar").arg("-czf").arg(&file).arg("-C").arg(parent).arg(base))?;
        let home_path = home_rel(&dir, home);
        // A folder that came here leaves under the key it came with.
        let key = load_registry(home).into_iter().find(|(_, e)| e.path == dir).map(|(k, _)| k).unwrap_or_else(|| format!("dir:{}", home_path.as_deref().unwrap_or(&base.to_string_lossy())));
        let packed = Packed::Tar { subdir: base.to_string_lossy().into_owned(), home_path, key };
        record(home, &packed.key(), Entry { path: dir.clone(), state: content_hash(&dir), head: String::new(), project: base.to_string_lossy().into_owned() })?;
        return Ok((file, packed));
    };
    let rel = dir.strip_prefix(&top).unwrap_or(Path::new(""));
    // A folder that came here keeps the name it came with (here it may be `.../app/feature`).
    let known = load_registry(home).into_values().find(|e| e.path == top && !e.project.is_empty());
    let repo_name = known.map(|e| e.project).unwrap_or_else(|| top.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "project".into()));
    let subdir = Path::new(&repo_name).join(rel).to_string_lossy().into_owned();
    let head = run(git(&top).args(["rev-parse", "HEAD"])).context("the repository has no commit yet")?;
    let branch = run(git(&top).args(["symbolic-ref", "--quiet", "--short", "HEAD"])).ok();
    let tree = state_tree(&top)?;
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
    let packed = Packed::Git { branch, head: head.clone(), snapshot, subdir, repo_id: repo_id(&top)?, home_path: home_rel(&top, home) };
    record(home, &packed.key(), Entry { path: top, state: tree, head, project: repo_name })?;
    Ok((file, packed))
}

/// `template` with its placeholders filled in (`~` is `home`).
fn expand(template: &str, home: &Path, project: &str, branch: &str, path: &str) -> PathBuf {
    let t = template.replace("{project}", project).replace("{branch}", branch).replace("{path}", path);
    match t.strip_prefix("~/").or_else(|| (t == "~").then_some("")) {
        Some(rest) => home.join(rest),
        None => PathBuf::from(t),
    }
}

/// A free folder at `want` (`want-2`, `want-3`, ... when something unrelated is there).
fn free(want: PathBuf) -> PathBuf {
    if !want.exists() {
        return want;
    }
    (2..).map(|i| PathBuf::from(format!("{}-{i}", want.display()))).find(|p| !p.exists()).unwrap()
}

/// Put what `file` carries on this machine (its home: `home`), as the module says; returns the
/// chat's folder there.
pub fn place(file: &Path, packed: &Packed, places: &Places, home: &Path) -> Result<Placed> {
    let mut parts = Path::new(packed.subdir()).components();
    let project = parts.next().context("nothing packed")?.as_os_str().to_string_lossy().into_owned();
    let rest = parts.as_path().to_path_buf();
    let reg = load_registry(home);
    let known = reg.get(&packed.key()).cloned();
    let (top, note) = match packed {
        Packed::Git { branch, repo_id, .. } => {
            let same_repo = |d: &Path| repo_top(d).is_some_and(|t| t == d.canonicalize().unwrap_or(d.to_path_buf())) && repo_id_of(d) == *repo_id;
            let slug = branch.as_deref().map(|b| b.replace('/', "-")).unwrap_or_else(|| "detached".into());
            let wanted = expand(&places.repo, home, &project, &slug, packed.home_path().unwrap_or(&project));
            match known.filter(|e| same_repo(&e.path)) {
                // 1. Where it was before, or where it came from.
                Some(e) => {
                    let note = update(&e.path, file, packed, Some(&e))?;
                    (e.path, note)
                }
                None => {
                    let clone = packed.home_path().map(|p| home.join(p)).filter(|c| same_repo(c));
                    match clone {
                        // 2. Beside a clone of it there: a worktree of that clone.
                        Some(clone) if !wanted.exists() => {
                            let note = worktree(&clone, &wanted, file, packed)?;
                            (wanted, note)
                        }
                        _ if wanted.exists() && same_repo(&wanted) => {
                            let note = update(&wanted, file, packed, None)?;
                            (wanted, note)
                        }
                        // 3. A new folder.
                        _ => {
                            let dest = free(wanted);
                            fresh(&dest, file, packed)?;
                            (dest, None)
                        }
                    }
                }
            }
        }
        Packed::Tar { .. } => {
            let wanted = expand(&places.plain, home, &project, "", packed.home_path().unwrap_or(&project));
            let (dest, expected) = match known.filter(|e| e.path.is_dir()) {
                Some(e) => (e.path, Some(e.state)),
                None => (wanted, None),
            };
            let mut note = None;
            if dest.exists() && expected.as_deref() != Some(content_hash(&dest).as_str()) {
                // Changed there since serpis left it (or not serpis's at all): kept aside.
                let aside = free(PathBuf::from(format!("{}.serpis-old-{}", dest.display(), stamp())));
                std::fs::rename(&dest, &aside)?;
                note = Some(format!("what was in {} was moved to {}", dest.display(), aside.display()));
            } else if dest.exists() {
                std::fs::remove_dir_all(&dest)?;
            }
            let tmp = dest.with_file_name(format!(".serpis-unpack-{}", uuid::Uuid::now_v7().simple()));
            std::fs::create_dir_all(&tmp)?;
            // (The archive holds the folder under the name it had there.)
            let r = run(Command::new("tar").arg("-xzf").arg(file).arg("-C").arg(&tmp)).and_then(|_| {
                let inner = std::fs::read_dir(&tmp)?.flatten().next().context("an empty archive")?.path();
                std::fs::create_dir_all(dest.parent().context("no parent")?)?;
                Ok(std::fs::rename(inner, &dest)?)
            });
            let _ = std::fs::remove_dir_all(&tmp);
            r?;
            (dest, note)
        }
    };
    // Recorded as serpis leaves it now.
    let entry = match packed {
        Packed::Git { .. } => Entry { path: top.clone(), state: state_tree(&top)?, head: run(git(&top).args(["rev-parse", "HEAD"]))?, project: project.clone() },
        Packed::Tar { .. } => Entry { path: top.clone(), state: content_hash(&top), head: String::new(), project: project.clone() },
    };
    record(home, &packed.key(), entry)?;
    let cwd = if rest.as_os_str().is_empty() { top } else { top.join(rest) };
    Ok(Placed { cwd, note })
}

fn repo_id_of(dir: &Path) -> String {
    repo_id(dir).unwrap_or_default()
}

fn stamp() -> String {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0).to_string()
}

/// The commit to check out and, when there was uncommitted work, the commit to leave HEAD on.
fn targets(packed: &Packed) -> (&str, &str, Option<&str>) {
    let Packed::Git { head, snapshot, branch, .. } = packed else { unreachable!() };
    (snapshot.as_deref().unwrap_or(head), head, branch.as_deref())
}

/// Check `at` out in `top` on `branch` (detached when the branch is checked out elsewhere), then
/// put HEAD back on `head` with the rest as uncommitted work.
fn check_out(top: &Path, at: &str, head: &str, branch: Option<&str>) -> Result<Option<String>> {
    let mut note = None;
    match branch {
        Some(b) => {
            if run(git(top).args(["checkout", "-q", "-f", "-B", b, at])).is_err() {
                run(git(top).args(["checkout", "-q", "-f", "--detach", at]))?;
                note = Some(format!("{b} is checked out in another worktree there, so this one is detached at it"));
            }
        }
        None => {
            run(git(top).args(["checkout", "-q", "-f", "--detach", at]))?;
        }
    }
    // Untracked files of the state it replaced (anything worth keeping was stashed).
    run(git(top).args(["clean", "-q", "-fd"]))?;
    if at != head {
        run(git(top).args(["reset", "-q", "--mixed", head]))?;
    }
    Ok(note)
}

fn fresh(dest: &Path, file: &Path, packed: &Packed) -> Result<()> {
    std::fs::create_dir_all(dest.parent().context("no parent")?)?;
    run(Command::new("git").arg("clone").arg("--quiet").arg("--no-checkout").arg(file).arg(dest))?;
    // The bundle is where it came from, not a remote worth keeping.
    let _ = run(git(dest).args(["remote", "remove", "origin"]));
    let (at, head, branch) = targets(packed);
    check_out(dest, at, head, branch)?;
    Ok(())
}

/// Fetch what `file` carries into repository `top` (refs under refs/serpis/in, removed after).
fn fetch(top: &Path, file: &Path) -> Result<()> {
    run(git(top).arg("fetch").arg("-q").arg(file).arg("+refs/*:refs/serpis/in/*"))?;
    Ok(())
}

fn drop_fetched(top: &Path) {
    if let Ok(refs) = run(git(top).args(["for-each-ref", "--format=%(refname)", "refs/serpis/in"])) {
        for r in refs.lines() {
            let _ = run(git(top).args(["update-ref", "-d", r]));
        }
    }
}

/// Bring repository `top` (a worktree or clone of it) up to what `file` carries. Changes made
/// there since serpis left it (`known`; None: it never did) are kept: uncommitted ones stashed,
/// commits on a branch of their own.
fn update(top: &Path, file: &Path, packed: &Packed, known: Option<&Entry>) -> Result<Option<String>> {
    let (at, head, branch) = targets(packed);
    fetch(top, file)?;
    let mut notes = Vec::new();
    let here_head = run(git(top).args(["rev-parse", "HEAD"])).unwrap_or_default();
    let dirty = !run(git(top).args(["status", "--porcelain"]))?.is_empty();
    let changed = known.is_none_or(|e| state_tree(top).ok().as_deref() != Some(e.state.as_str()));
    if dirty && changed {
        let what = branch.unwrap_or("it");
        run(git(top).args(["stash", "push", "-q", "-u", "-m", &format!("serpis: changes made here before {what} came back from another machine")]))?;
        notes.push("changes made there were stashed first (git stash list)".to_string());
    }
    let moved_on = known.is_none_or(|e| e.head != here_head);
    let behind = run(git(top).args(["merge-base", "--is-ancestor", &here_head, head])).is_ok();
    if moved_on && !behind && !here_head.is_empty() {
        let keep = format!("serpis/kept/{}-{}", branch.unwrap_or("detached").replace('/', "-"), stamp());
        run(git(top).args(["branch", &keep, &here_head]))?;
        notes.push(format!("its commits made there were kept on {keep}"));
    }
    let r = check_out(top, at, head, branch);
    drop_fetched(top);
    notes.extend(r?);
    Ok((!notes.is_empty()).then(|| notes.join("; ")))
}

/// A worktree of `clone` at `dest` with what `file` carries.
fn worktree(clone: &Path, dest: &Path, file: &Path, packed: &Packed) -> Result<Option<String>> {
    let (at, head, branch) = targets(packed);
    fetch(clone, file)?;
    std::fs::create_dir_all(dest.parent().context("no parent")?)?;
    let r = run(git(clone).arg("worktree").arg("add").arg("-q").arg("--detach").arg(dest).arg(at)).and_then(|_| check_out(dest, at, head, branch));
    drop_fetched(clone);
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(dir: &Path, script: &str) -> String {
        let out = Command::new("sh").arg("-c").arg(script).current_dir(dir).output().unwrap();
        assert!(out.status.success(), "{script}: {}", String::from_utf8_lossy(&out.stderr));
        String::from_utf8_lossy(&out.stdout).trim_end().to_string()
    }

    /// Two machines: homes `a` and `b`, and a repository in a's home.
    fn setup() -> (PathBuf, PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("ws-{}", uuid::Uuid::now_v7().simple())).canonicalize_or_self();
        let (a, b) = (root.join("a"), root.join("b"));
        let repo = a.join("code/app");
        std::fs::create_dir_all(repo.join("src")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        sh(&repo, "git init -q -b main && git config user.email t@t && git config user.name t \
            && printf 'a\\n' > kept.txt && printf 'b\\n' > gone.txt && printf 'x\\n' > src/lib.rs && printf 'target/\\n' > .gitignore \
            && git add -A && git commit -qm first && git checkout -qb feature \
            && printf 'a2\\n' > kept.txt && rm gone.txt && printf 'new\\n' > src/new.rs && git add src/new.rs \
            && printf 'untracked\\n' > notes.md && mkdir target && printf 'big\\n' > target/out");
        (root, a, b, repo)
    }

    trait Canon {
        fn canonicalize_or_self(self) -> PathBuf;
    }
    impl Canon for PathBuf {
        fn canonicalize_or_self(self) -> PathBuf {
            std::fs::create_dir_all(&self).unwrap();
            self.canonicalize().unwrap()
        }
    }

    fn carry(from: &Path, from_home: &Path, to_home: &Path) -> Placed {
        let (file, packed) = pack(from, &from_home.join(".transfer"), from_home).unwrap();
        place(&file, &packed, &Places::default(), to_home).unwrap()
    }

    fn status(dir: &Path) -> String {
        sh(dir, "git status --porcelain")
    }

    #[test]
    fn a_repository_arrives_in_its_place_with_its_branch_and_uncommitted_work() {
        let (root, a, b, repo) = setup();
        let before = std::fs::read(repo.join(".git/index")).unwrap();
        let placed = carry(&repo.join("src"), &a, &b);
        assert_eq!(std::fs::read(repo.join(".git/index")).unwrap(), before, "the repository's index is untouched");
        assert!(sh(&repo, "git for-each-ref refs/serpis").is_empty(), "no refs left behind");
        let there = b.join(".serpis/worktrees/app/feature");
        assert_eq!(placed, Placed { cwd: there.join("src"), note: None }, "in serpis's folder there, the chat's subfolder kept");
        assert_eq!(std::fs::read_to_string(there.join("kept.txt")).unwrap(), "a2\n");
        assert!(!there.join("gone.txt").exists() && !there.join("target").exists(), "deleted stays deleted, ignored stays behind");
        let st = status(&there);
        for line in [" M kept.txt", " D gone.txt", "?? notes.md", "?? src/new.rs"] {
            assert!(st.lines().any(|l| l == line), "the work is uncommitted again ({line}): {st}");
        }
        assert_eq!(sh(&there, "git branch --show-current"), "feature");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn it_goes_back_and_forth_in_the_same_two_folders() {
        let (root, a, b, repo) = setup();
        let there = carry(&repo, &a, &b).cwd;
        // Worked on there, then back home: the original folder, brought up to date.
        sh(&there, "git add -A && git commit -qm 'done there' && printf 'more\\n' > more.txt");
        let back = carry(&there, &b, &a);
        assert_eq!(back, Placed { cwd: repo.clone(), note: None }, "home again, untouched since: updated in place");
        assert_eq!(sh(&repo, "git log -1 --format=%s"), "done there");
        assert_eq!(status(&repo), "?? more.txt");
        // Machine to machine, it keeps its name (there it lives in .../app/feature).
        let c = a.parent().unwrap().join("c");
        std::fs::create_dir_all(&c).unwrap();
        assert_eq!(carry(&there, &b, &c).cwd, c.join(".serpis/worktrees/app/feature"));
        // And there again: the same folder, not a copy.
        sh(&repo, "printf 'again\\n' >> more.txt");
        assert_eq!(carry(&repo, &a, &b).cwd, there);
        assert_eq!(std::fs::read_to_string(there.join("more.txt")).unwrap(), "more\nagain\n");
        assert!(!b.join(".serpis/worktrees/app/feature-2").exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn what_was_done_there_meanwhile_is_kept() {
        let (root, a, b, repo) = setup();
        let there = carry(&repo, &a, &b).cwd;
        // Back home, then changed there without serpis (an edit and a commit), and carried again.
        carry(&there, &b, &a);
        sh(&there, "printf 'mine\\n' > mine.txt && git add mine.txt && git commit -qm 'made there' && printf 'dirty\\n' > kept.txt");
        let placed = carry(&repo, &a, &b);
        let note = placed.note.expect("it says what it kept");
        assert!(note.contains("stashed") && note.contains("serpis/kept/feature-"), "{note}");
        assert!(sh(&there, "git stash list").contains("serpis: changes made here"), "the edit is in the stash");
        let kept = sh(&there, "git branch --list 'serpis/kept/*' --format='%(refname:short)'");
        assert_eq!(sh(&there, &format!("git log -1 --format=%s {kept}")), "made there", "the commit is on a branch of its own");
        assert_eq!(std::fs::read_to_string(there.join("kept.txt")).unwrap(), "a2\n", "and the folder is what came");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn beside_a_clone_there_it_becomes_a_worktree_of_it() {
        let (root, a, b, repo) = setup();
        // b has its own clone at the same place in its home, on main, with work of its own.
        sh(&b, &format!("mkdir -p code && git clone -q '{}' code/app && cd code/app && git checkout -q main && printf 'theirs\\n' > theirs.txt", repo.display()));
        let clone = b.join("code/app");
        let placed = carry(&repo, &a, &b);
        let there = b.join(".serpis/worktrees/app/feature");
        assert_eq!(placed.cwd, there);
        assert!(sh(&clone, "git worktree list").contains(".serpis/worktrees/app/feature"), "a worktree of the clone");
        assert_eq!(status(&clone), "?? theirs.txt", "the clone's own work untouched");
        assert_eq!(sh(&there, "git branch --show-current"), "feature");
        assert!(sh(&clone, "git for-each-ref refs/serpis").is_empty());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn any_other_folder_arrives_in_its_place_and_keeps_what_changed_there() {
        let root = std::env::temp_dir().join(format!("ws-{}", uuid::Uuid::now_v7().simple())).canonicalize_or_self();
        let (a, b) = (root.join("a"), root.join("b"));
        let dir = a.join("notes");
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        std::fs::write(dir.join("sub/a.txt"), "hi").unwrap();
        let there = carry(&dir, &a, &b).cwd;
        assert_eq!(there, b.join(".serpis/carried/notes"));
        assert_eq!(std::fs::read_to_string(there.join("sub/a.txt")).unwrap(), "hi");
        // Back home unchanged there: in place.
        assert_eq!(carry(&there, &b, &a), Placed { cwd: dir.clone(), note: None });
        // Changed there meanwhile, then carried again: what was there is moved aside.
        std::fs::write(there.join("sub/a.txt"), "changed there").unwrap();
        let placed = carry(&dir, &a, &b);
        assert_eq!(placed.cwd, there);
        assert!(placed.note.as_deref().is_some_and(|n| n.contains("serpis-old")), "{placed:?}");
        assert_eq!(std::fs::read_to_string(there.join("sub/a.txt")).unwrap(), "hi");
        let _ = std::fs::remove_dir_all(&root);
    }
}
