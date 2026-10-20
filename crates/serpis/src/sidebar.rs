//! The sidebar: every chat and terminal at a glance, grouped by project and git worktree.
//!
//! Each buffer reports an [`Activity`] (working, waiting for you, error, ...). The sidebar
//! tracks state changes for all buffers, even while hidden: a buffer that stops working out
//! of sight gets *attention* (a dot) until you look at it, and every change runs the
//! `buffer-activity-changed` hook ({buffer, name, kind, state, previous, detail, agent,
//! attention}), so Lua can notify, log or chain agents.
//!
//! Projects are the main checkouts of git repositories (or `project.el` roots); a project's
//! linked worktrees are listed under it with their branch and changes, and new chats can get
//! a worktree of their own (`worktree-new-chat`). Typing in the sidebar searches all buffers
//! by name, title, project, branch, state and (from three characters) content.
//!
//! Settings: `sidebar-visible`, `sidebar-width`, `sidebar-kinds`, `sidebar-sort`,
//! `sidebar-show-other-buffers`, `sidebar-show-empty-worktrees`, `sidebar-git-refresh`,
//! `sidebar-jump-to-workspace`, `sidebar-buffer-menu`, `sidebar-project-menu`,
//! `sidebar-worktree-menu`, `projects`, `agent-programs`, `new-buffer-directory`,
//! `worktree-directory`, `worktree-branch-prefix`, `worktree-delete-branch`,
//! `terminal-busy-title-regexp`, `terminal-quiet-seconds`. Hooks: `buffer-activity-changed`,
//! `worktree-created`, `worktree-removed`.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui::{Color32, FontId, Rect, Sense, Stroke, StrokeKind, Vec2};
use serde_json::{Value, json};

use crate::buffer::{Activity, ActivityState, BufferId};
use crate::editor::{Args, BUILTIN, Editor, buffer_id_from_u64, buffer_id_to_u64};
use crate::git;
use crate::keys::KeyChord;
use crate::minibuffer::Candidate;

/// Per-buffer state the sidebar remembers between frames.
#[derive(Debug, Clone)]
pub struct Tracked {
    pub state: ActivityState,
    /// When `state` began.
    pub since: Instant,
    /// Finished, failed or started waiting while you weren't looking.
    pub attention: bool,
    /// Creation order.
    pub seq: u64,
    /// Last moment the buffer was working (for `sidebar-sort` = "recent").
    pub last_active: Instant,
}

/// Worktrees and their changes for one repository, from the last background refresh.
#[derive(Debug, Clone, Default)]
pub struct GitInfo {
    pub worktrees: Vec<git::Worktree>,
    pub status: HashMap<PathBuf, git::Status>,
}

/// A selectable sidebar row (keyboard navigation and commands act on the selected one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowRef {
    Project(PathBuf),
    Worktree(PathBuf),
    Item(BufferId),
    /// A saved session no chat has open.
    Session(PathBuf),
}

#[derive(Default)]
pub struct Sidebar {
    /// Keys go to the sidebar (search and navigation) instead of the focused window.
    pub focused: bool,
    pub query: String,
    pub selected: usize,
    /// Collapsed project roots and worktree paths.
    pub collapsed: HashSet<PathBuf>,
    pub tracked: HashMap<BufferId, Tracked>,
    seq: u64,
    /// `git::repo` per directory, re-read every few seconds.
    repos: HashMap<PathBuf, (Instant, Option<git::Repo>)>,
    pub git: HashMap<PathBuf, GitInfo>,
    git_at: HashMap<PathBuf, Instant>,
    refreshing: HashSet<PathBuf>,
    /// Selectable rows as last laid out, and where they were drawn.
    pub rows: Vec<RowRef>,
    pub row_rects: Vec<Rect>,
    /// Scroll the selected row into view on the next frame.
    reveal: bool,
    content: Option<(String, Instant, HashMap<BufferId, String>)>,
}

/// A buffer as the sidebar shows it.
#[derive(Debug, Clone)]
pub struct Item {
    pub buffer: BufferId,
    pub name: String,
    pub title: String,
    pub kind: String,
    pub dir: Option<PathBuf>,
    /// Project root (a repository's main checkout).
    pub project: Option<PathBuf>,
    pub worktree: Option<PathBuf>,
    pub branch: Option<String>,
    pub activity: Activity,
    pub since: Instant,
    pub agent: bool,
    pub attention: bool,
    /// Shown in a window of the current workspace.
    pub visible: bool,
    pub current: bool,
    pub seq: u64,
    pub last_active: Instant,
    /// Matching content line, when found by a content search.
    pub snippet: Option<String>,
}

impl Item {
    /// The display name (see `Editor::buffer_display_name`).
    pub fn label(&self) -> String {
        if self.title.trim().is_empty() { crate::chrome::pretty_name(&self.name) } else { self.title.clone() }
    }

    pub fn to_json(&self) -> Value {
        json!({
            "buffer": buffer_id_to_u64(self.buffer),
            "name": self.name,
            "title": self.title,
            "kind": self.kind,
            "dir": self.dir.as_ref().map(|p| p.to_string_lossy()),
            "project": self.project.as_ref().map(|p| p.to_string_lossy()),
            "worktree": self.worktree.as_ref().map(|p| p.to_string_lossy()),
            "branch": self.branch,
            "state": self.activity.state.as_str(),
            "detail": self.activity.detail,
            "program": self.activity.program,
            "seconds": self.since.elapsed().as_secs(),
            "agent": self.agent,
            "attention": self.attention,
            "visible": self.visible,
            "current": self.current,
        })
    }
}

#[derive(Debug, Clone)]
pub struct WorktreeGroup {
    pub path: PathBuf,
    pub branch: String,
    pub main: bool,
    pub status: Option<git::Status>,
    pub items: Vec<Item>,
}

#[derive(Debug, Clone)]
pub struct Project {
    /// `None` for buffers outside any project.
    pub root: Option<PathBuf>,
    pub name: String,
    pub git: bool,
    pub worktrees: Vec<WorktreeGroup>,
    /// Recent sessions of this project that no chat has open.
    pub sessions: Vec<crate::session::Info>,
}

impl Project {
    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.worktrees.iter().flat_map(|w| w.items.iter())
    }

    pub fn to_json(&self) -> Value {
        json!({
            "root": self.root.as_ref().map(|p| p.to_string_lossy()),
            "name": self.name,
            "git": self.git,
            "worktrees": self.worktrees.iter().map(|w| json!({
                "path": w.path.to_string_lossy(),
                "branch": w.branch,
                "main": w.main,
                "status": w.status.as_ref().map(|s| json!({
                    "changed": s.changed, "untracked": s.untracked, "ahead": s.ahead, "behind": s.behind,
                    "added": s.added, "removed": s.removed, "upstream": s.upstream,
                })),
                "buffers": w.items.iter().map(|i| buffer_id_to_u64(i.buffer)).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        })
    }
}

pub fn is_agent_program(ed: &Editor, program: &str) -> bool {
    let p = program.rsplit('/').next().unwrap_or(program).to_lowercase();
    ed.var("agent-programs").and_then(Value::as_array).is_some_and(|a| a.iter().filter_map(Value::as_str).any(|x| x.eq_ignore_ascii_case(&p)))
}

fn var_list(ed: &Editor, name: &str) -> Vec<String> {
    ed.var(name).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect()).unwrap_or_default()
}

impl Editor {
    /// A buffer's activity: its own report, unless a buffer-local `activity` overrides it.
    pub fn buffer_activity(&self, id: BufferId) -> Activity {
        let mut a = self.buffers.get(id).and_then(|s| s.buf.as_ref()).map(|b| b.activity()).unwrap_or_default();
        if let Some(o) = self.meta(id).and_then(|m| m.vars.get("activity")).filter(|v| v.is_object()) {
            if let Some(s) = o.get("state").and_then(Value::as_str).and_then(ActivityState::parse) {
                a.state = s;
            }
            if let Some(d) = o.get("detail").and_then(Value::as_str) {
                a.detail = d.to_string();
            }
        }
        a
    }

    /// A buffer's title: buffer-local `title`, else what the buffer reports.
    pub fn buffer_title(&self, id: BufferId) -> String {
        if let Some(t) = self.meta(id).and_then(|m| m.vars.get("title")).and_then(Value::as_str) {
            return t.to_string();
        }
        self.buffers.get(id).and_then(|s| s.buf.as_ref()).map(|b| b.title()).unwrap_or_default()
    }

    /// Set (or with `None`, clear) a buffer's title; a chat also names its session.
    pub fn set_buffer_title(&mut self, id: BufferId, title: Option<String>) {
        let title = title.filter(|t| !t.is_empty());
        if let Some(m) = self.meta_mut(id) {
            match &title {
                Some(t) => m.vars.insert("title".into(), json!(t)),
                None => m.vars.remove("title"),
            };
        }
        if let Some(t) = title {
            self.with_buffer_as::<crate::buffers::chat::ChatBuffer, _>(id, |c, ed| c.set_session_name(ed, &t));
        }
    }

    /// The repository around `dir` (cached for a few seconds).
    pub fn repo_of(&mut self, dir: &Path) -> Option<git::Repo> {
        if let Some((t, r)) = self.sidebar.repos.get(dir) {
            if t.elapsed() < Duration::from_secs(3) {
                return r.clone();
            }
        }
        let r = git::repo(dir);
        self.sidebar.repos.insert(dir.to_path_buf(), (Instant::now(), r.clone()));
        r
    }

    /// The project root of a directory: the main checkout of its repository, else the
    /// nearest `project.el`-style root.
    pub fn project_root_of(&mut self, dir: &Path) -> Option<PathBuf> {
        self.repo_of(dir).map(|r| r.main).or_else(|| crate::project::root(dir))
    }

    /// The window showing `id` in any workspace, current first.
    fn window_showing(&self, id: BufferId) -> Option<(String, crate::WindowId)> {
        if self.windows[self.focused].buffer == id {
            return Some((self.workspace.clone(), self.focused));
        }
        if let Some(w) = self.layout.visible_leaves().into_iter().chain(self.layout.leaves()).find(|w| self.windows[*w].buffer == id) {
            return Some((self.workspace.clone(), w));
        }
        self.workspaces.iter().find_map(|ws| ws.layout.leaves().into_iter().find(|w| self.windows[*w].buffer == id).map(|w| (ws.name.clone(), w)))
    }

    /// Bring a buffer up: focus a window already showing it (on another workspace too, per
    /// `sidebar-jump-to-workspace`), else show it in the focused window.
    pub fn open_buffer(&mut self, id: BufferId) {
        if !self.buffers.contains_key(id) {
            return;
        }
        let jump = self.var("sidebar-jump-to-workspace").and_then(Value::as_bool).unwrap_or(true);
        match self.window_showing(id) {
            Some((ws, w)) if ws == self.workspace => self.focus_window(w),
            Some((ws, w)) if jump => {
                self.switch_workspace(&ws);
                self.focus_window(w);
            }
            _ => self.show_buffer(id),
        }
        self.sidebar.focused = false;
    }

    /// The directory new chats and terminals start in (`new-buffer-directory`): the current
    /// buffer's worktree root ("project"), its own directory ("buffer"), or where serpis
    /// started ("editor").
    pub fn new_buffer_directory(&mut self) -> PathBuf {
        let mode = self.var_str("new-buffer-directory").unwrap_or_else(|| "project".into());
        let dir = self.buffer_directory(self.current_buffer());
        match (mode.as_str(), dir) {
            ("editor", _) | (_, None) => self.cwd.clone(),
            ("buffer", Some(d)) => d,
            (_, Some(d)) => self.repo_of(&d).map(|r| r.worktree).or_else(|| crate::project::root(&d)).unwrap_or(d),
        }
    }

    /// Create a chat (or terminal) in `dir` and show it.
    pub fn new_buffer_in(&mut self, kind: &str, dir: &Path) -> anyhow::Result<BufferId> {
        let id = self.create_buffer(kind, Some(&format!("*{kind}*")), &json!({"cwd": dir.to_string_lossy()}))?;
        self.show_buffer(id);
        self.sidebar.focused = false;
        Ok(id)
    }

    /// Projects pinned in the `projects` variable.
    pub fn pinned_projects(&self) -> Vec<PathBuf> {
        var_list(self, "projects").iter().map(|p| crate::expand_home(p)).collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Tracking
// ---------------------------------------------------------------------------------------------

/// Note activity changes for every buffer (called once per frame).
pub fn track(ed: &mut Editor) {
    let now = Instant::now();
    let current = ed.current_buffer();
    let visible: HashSet<BufferId> = ed.layout.visible_leaves().iter().map(|w| ed.windows[*w].buffer).collect();
    let ids: Vec<BufferId> = ed.buffers.keys().collect();
    let mut changes = Vec::new();
    let mut agent_busy = false;
    for id in ids {
        let a = ed.buffer_activity(id);
        let agent = ed.meta(id).is_some_and(|m| m.major == "chat") || a.program.as_deref().is_some_and(|p| is_agent_program(ed, p));
        if a.state == ActivityState::Working && agent {
            agent_busy = true;
        }
        let seen = id == current && !ed.sidebar.focused;
        let sb = &mut ed.sidebar;
        let t = sb.tracked.entry(id).or_insert_with(|| {
            sb.seq += 1;
            Tracked { state: a.state, since: now, attention: false, seq: sb.seq, last_active: now }
        });
        if a.state == ActivityState::Working {
            t.last_active = now;
        }
        if t.state != a.state {
            let prev = t.state;
            t.state = a.state;
            t.since = now;
            // Something you'd want to look at happened out of sight.
            let notable = prev == ActivityState::Working || matches!(a.state, ActivityState::Waiting | ActivityState::Error);
            if notable && a.state != ActivityState::Working && !visible.contains(&id) {
                t.attention = true;
            }
            changes.push((id, prev, a.clone(), agent, t.attention));
        }
        if seen {
            t.attention = false;
        }
    }
    ed.sidebar.tracked.retain(|id, _| ed.buffers.contains_key(*id));
    // Quiet agents turn "waiting" without any event; look again soon.
    if agent_busy {
        ed.ctx.request_repaint_after(Duration::from_millis(500));
    }
    for (id, prev, a, agent, attention) in changes {
        let (name, kind) = ed.meta(id).map(|m| (m.name.clone(), m.major.clone())).unwrap_or_default();
        let payload = json!({
            "buffer": buffer_id_to_u64(id), "name": name, "kind": kind,
            "state": a.state.as_str(), "previous": prev.as_str(), "detail": a.detail,
            "agent": agent, "attention": attention,
        });
        ed.run_hook("buffer-activity-changed", &payload);
    }
}

// ---------------------------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------------------------

/// Every buffer as a sidebar item.
pub fn items(ed: &mut Editor) -> Vec<Item> {
    let current = ed.current_buffer();
    let visible: HashSet<BufferId> = ed.layout.visible_leaves().iter().map(|w| ed.windows[*w].buffer).collect();
    let ids: Vec<BufferId> = ed.buffers.keys().collect();
    let mut out = Vec::new();
    for id in ids {
        let Some(meta) = ed.meta(id).cloned() else { continue };
        let dir = ed.buffer_directory(id);
        let repo = dir.as_ref().and_then(|d| ed.repo_of(d));
        let project = match &repo {
            Some(r) => Some(r.main.clone()),
            None => dir.as_ref().and_then(|d| crate::project::root(d)),
        };
        let activity = ed.buffer_activity(id);
        let agent = meta.major == "chat" || activity.program.as_deref().is_some_and(|p| is_agent_program(ed, p));
        let t = ed.sidebar.tracked.get(&id).cloned();
        let now = Instant::now();
        out.push(Item {
            buffer: id,
            title: ed.buffer_title(id),
            kind: meta.major.clone(),
            name: meta.name,
            worktree: repo.as_ref().map(|r| r.worktree.clone()).or_else(|| project.clone()),
            branch: repo.as_ref().map(git::Repo::head_label),
            project,
            dir,
            since: activity.since.or(t.as_ref().map(|t| t.since)).unwrap_or(now),
            activity,
            agent,
            attention: t.as_ref().is_some_and(|t| t.attention),
            visible: visible.contains(&id),
            current: id == current,
            seq: t.as_ref().map(|t| t.seq).unwrap_or(u64::MAX),
            last_active: t.map(|t| t.last_active).unwrap_or(now),
            snippet: None,
        });
    }
    out
}

fn sort_items(ed: &Editor, items: &mut [Item]) {
    if ed.var_str("sidebar-sort").as_deref() == Some("recent") {
        items.sort_by(|a, b| b.last_active.cmp(&a.last_active).then(a.seq.cmp(&b.seq)));
    } else {
        items.sort_by_key(|i| i.seq);
    }
}

fn dir_name(p: &Path) -> String {
    p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// Buffers grouped by project and worktree, with pinned projects and known worktrees
/// included even when empty. The last group (root `None`) holds buffers outside projects.
pub fn projects(ed: &mut Editor) -> Vec<Project> {
    let kinds = var_list(ed, "sidebar-kinds");
    let show_other = ed.var_bool("sidebar-show-other-buffers");
    let show_empty = ed.var("sidebar-show-empty-worktrees").and_then(Value::as_bool).unwrap_or(true);
    let mut all = items(ed);
    sort_items(ed, &mut all);
    let mut roots: Vec<PathBuf> = Vec::new();
    for p in ed.pinned_projects() {
        let root = ed.project_root_of(&p).unwrap_or(p);
        if !roots.contains(&root) {
            roots.push(root);
        }
    }
    let pinned = roots.len();
    let mut others = Vec::new();
    let recent = ed.var("sidebar-recent-sessions").and_then(Value::as_u64).unwrap_or(3) as usize;
    let open: HashSet<PathBuf> = ed.buffers.keys().collect::<Vec<_>>().into_iter().filter_map(|id| crate::session_ui::session_of(ed, id)).collect();
    for it in &all {
        if !kinds.is_empty() && !kinds.contains(&it.kind) {
            continue;
        }
        match &it.project {
            Some(r) if !roots.contains(r) => roots.push(r.clone()),
            _ => {}
        }
    }
    roots[pinned..].sort_by_key(|r| dir_name(r).to_lowercase());
    let mut out = Vec::new();
    for root in roots {
        let is_git = ed.repo_of(&root).is_some();
        let info = ed.sidebar.git.get(&root).cloned();
        let mut groups: Vec<WorktreeGroup> = Vec::new();
        if let Some(info) = &info {
            for w in &info.worktrees {
                groups.push(WorktreeGroup {
                    path: w.path.clone(),
                    branch: w.branch.clone().unwrap_or_else(|| w.head.clone()),
                    main: w.main,
                    status: info.status.get(&w.path).cloned(),
                    items: Vec::new(),
                });
            }
        }
        if groups.is_empty() {
            let branch = ed.repo_of(&root).map(|r| r.head_label()).unwrap_or_default();
            groups.push(WorktreeGroup { path: root.clone(), branch, main: true, status: None, items: Vec::new() });
        }
        for it in all.iter().filter(|i| i.project.as_ref() == Some(&root) && (kinds.is_empty() || kinds.contains(&i.kind))) {
            let wt = it.worktree.clone().unwrap_or_else(|| root.clone());
            match groups.iter_mut().find(|g| g.path == wt) {
                Some(g) => g.items.push(it.clone()),
                None => groups.push(WorktreeGroup { path: wt, branch: it.branch.clone().unwrap_or_default(), main: false, status: None, items: vec![it.clone()] }),
            }
        }
        if !show_empty {
            groups.retain(|g| g.main || !g.items.is_empty());
        }
        let sessions = ed.sessions.list.iter().filter(|i| i.project.as_ref() == Some(&root) && !open.contains(&i.path)).take(recent).cloned().collect();
        out.push(Project { name: dir_name(&root), git: is_git, root: Some(root), worktrees: groups, sessions });
    }
    for it in all {
        let shown = it.project.is_some() && (kinds.is_empty() || kinds.contains(&it.kind));
        if !shown && (show_other || (!kinds.is_empty() && kinds.contains(&it.kind))) {
            others.push(it);
        }
    }
    if !others.is_empty() {
        out.push(Project {
            root: None,
            name: "Other buffers".into(),
            git: false,
            worktrees: vec![WorktreeGroup { path: PathBuf::new(), branch: String::new(), main: true, status: None, items: others }],
            sessions: Vec::new(),
        });
    }
    out
}

/// Text a query is matched against.
fn haystack(it: &Item) -> String {
    let project = it.project.as_deref().map(dir_name).unwrap_or_default();
    format!("{} {} {} {} {} {} {}", it.label(), it.name, project, it.branch.as_deref().unwrap_or(""), it.kind, it.activity.state.as_str(), it.activity.detail)
}

/// Buffers whose content contains `query` (case-insensitive), with the first matching line.
fn content_matches(ed: &mut Editor, query: &str) -> HashMap<BufferId, String> {
    if let Some((q, at, hits)) = &ed.sidebar.content {
        if q == query && at.elapsed() < Duration::from_secs(2) {
            return hits.clone();
        }
    }
    let needle = query.to_lowercase();
    let mut hits = HashMap::new();
    for id in ed.buffers.keys().collect::<Vec<_>>() {
        let line = ed.with_buffer(id, |b, _| {
            let text = b.full_text()?;
            let lines: Vec<&str> = text.lines().collect();
            // Bounded: the most recent 4000 lines.
            lines.iter().rev().take(4000).find(|l| l.to_lowercase().contains(&needle)).map(|l| l.to_string())
        });
        if let Some(Some(l)) = line {
            hits.insert(id, l.trim().chars().take(120).collect());
        }
    }
    ed.sidebar.content = Some((query.to_string(), Instant::now(), hits.clone()));
    hits
}

/// All buffers matching `query`, best first.
pub fn search(ed: &mut Editor, query: &str) -> Vec<Item> {
    let mut scored: Vec<(i64, Item)> = items(ed).into_iter().filter_map(|it| crate::minibuffer::fuzzy(query, &haystack(&it)).map(|(s, _)| (s, it))).collect();
    if query.trim().chars().count() >= 3 {
        let hits = content_matches(ed, query.trim());
        for it in items(ed) {
            if let Some(line) = hits.get(&it.buffer) {
                match scored.iter_mut().find(|(_, i)| i.buffer == it.buffer) {
                    Some((_, i)) => i.snippet = Some(line.clone()),
                    None => scored.push((-1000, Item { snippet: Some(line.clone()), ..it })),
                }
            }
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.seq.cmp(&b.1.seq)));
    scored.into_iter().map(|(_, i)| i).collect()
}

/// Refresh worktrees and status of `roots` in the background, at most every
/// `sidebar-git-refresh` seconds each.
pub fn refresh_git(ed: &mut Editor, roots: &[PathBuf], force: bool) {
    let every = Duration::from_secs_f64(ed.var("sidebar-git-refresh").and_then(Value::as_f64).unwrap_or(5.0).max(0.5));
    for root in roots {
        if ed.sidebar.refreshing.contains(root) || (!force && ed.sidebar.git_at.get(root).is_some_and(|t| t.elapsed() < every)) {
            continue;
        }
        ed.sidebar.refreshing.insert(root.clone());
        ed.sidebar.git_at.insert(root.clone(), Instant::now());
        let main = ed.main_sender();
        let root = root.clone();
        ed.rt.spawn_blocking(move || {
            let worktrees = git::worktrees(&root).unwrap_or_default();
            let status = worktrees.iter().filter(|w| w.path.is_dir()).filter_map(|w| git::status(&w.path).ok().map(|s| (w.path.clone(), s))).collect();
            main.send(move |ed| {
                ed.sidebar.refreshing.remove(&root);
                ed.sidebar.git.insert(root, GitInfo { worktrees, status });
            });
        });
    }
}

// ---------------------------------------------------------------------------------------------
// Commands
// ---------------------------------------------------------------------------------------------

/// The selected row, if the sidebar has one.
fn selected_row(ed: &Editor) -> Option<RowRef> {
    ed.sidebar.rows.get(ed.sidebar.selected).cloned()
}

/// The directory a project/worktree command targets: a parameter, the selected sidebar row
/// (while the sidebar has focus), or the current buffer's.
fn target_dir(ed: &mut Editor, a: &Args, worktree: bool) -> PathBuf {
    if let Some(d) = a.str(0).filter(|s| !s.is_empty()) {
        return crate::expand_home(d);
    }
    if ed.sidebar.focused {
        match selected_row(ed) {
            Some(RowRef::Project(p) | RowRef::Worktree(p)) if !p.as_os_str().is_empty() => return p,
            Some(RowRef::Item(id)) => {
                if let Some(d) = ed.buffer_directory(id) {
                    let r = ed.repo_of(&d);
                    return match r {
                        Some(r) if worktree => r.worktree,
                        Some(r) => r.main,
                        None => d,
                    };
                }
            }
            _ => {}
        }
    }
    ed.new_buffer_directory()
}

fn target_buffer(ed: &Editor, a: &Args) -> Option<BufferId> {
    if let Some(v) = a.params.first() {
        let n = v.as_u64().or_else(|| v.as_str().and_then(|s| s.parse().ok()))?;
        return Some(buffer_id_from_u64(n)).filter(|id| ed.buffers.contains_key(*id));
    }
    match selected_row(ed) {
        Some(RowRef::Item(id)) if ed.sidebar.focused => Some(id),
        _ => Some(ed.current_buffer()),
    }
}

fn move_selection(ed: &mut Editor, d: isize) {
    let n = ed.sidebar.rows.len() as isize;
    if n > 0 {
        ed.sidebar.selected = (ed.sidebar.selected as isize + d).clamp(0, n - 1) as usize;
        ed.sidebar.reveal = true;
    }
}

pub fn set_visible(ed: &mut Editor, on: bool) {
    ed.set_var("sidebar-visible", json!(on));
    if !on {
        ed.sidebar.focused = false;
    }
    ed.persist_state("sidebar-visible", json!(on));
}

pub fn focus(ed: &mut Editor) {
    if !ed.var_bool("sidebar-visible") {
        ed.set_var("sidebar-visible", json!(true));
    }
    ed.sidebar.focused = true;
    // Start on the current buffer's row.
    let cur = ed.current_buffer();
    if let Some(i) = ed.sidebar.rows.iter().position(|r| *r == RowRef::Item(cur)) {
        ed.sidebar.selected = i;
    }
    ed.sidebar.reveal = true;
}

fn set_query(ed: &mut Editor, q: String) {
    ed.sidebar.query = q;
    ed.sidebar.selected = 0;
    ed.sidebar.reveal = true;
}

/// Typed text while the sidebar has focus edits the search.
pub fn insert_text(ed: &mut Editor, text: &str) {
    let q = format!("{}{}", ed.sidebar.query, text.replace('\n', " "));
    set_query(ed, q);
}

pub fn self_insert(ed: &mut Editor, chord: &KeyChord) {
    if let Some(c) = chord.printable() {
        insert_text(ed, &c.to_string());
    }
}

fn activate(ed: &mut Editor, split: bool) {
    match selected_row(ed) {
        Some(RowRef::Item(id)) => {
            if split {
                ed.sidebar.focused = false;
                ed.split_window(false);
                ed.show_buffer(id);
            } else {
                ed.open_buffer(id);
            }
        }
        Some(RowRef::Project(p) | RowRef::Worktree(p)) => toggle_collapsed(ed, p),
        Some(RowRef::Session(p)) => {
            let r = if split {
                crate::session_ui::restore(ed, &p, false).map(|id| {
                    ed.sidebar.focused = false;
                    ed.split_window(false);
                    ed.show_buffer(id);
                })
            } else {
                crate::session_ui::resume(ed, &p).map(|_| ())
            };
            if let Err(e) = r {
                ed.error(format!("{e:#}"));
            }
        }
        None => {}
    }
}

/// Closed sessions matching `query`, best first.
pub fn search_sessions(ed: &Editor, query: &str) -> Vec<crate::session::Info> {
    let open: Vec<PathBuf> = ed.buffers.keys().filter_map(|id| crate::session_ui::session_of(ed, id)).collect();
    let mut scored: Vec<(i64, &crate::session::Info)> = ed
        .sessions
        .list
        .iter()
        .filter(|i| !open.contains(&i.path))
        .filter_map(|i| {
            let project = i.project.as_deref().map(dir_name).unwrap_or_default();
            let hay = format!("{} {} {project}", i.title(), i.last_prompt.chars().take(200).collect::<String>());
            crate::minibuffer::fuzzy(query, &hay).map(|(s, _)| (s, i))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then(b.1.modified.cmp(&a.1.modified)));
    scored.into_iter().take(30).map(|(_, i)| i.clone()).collect()
}

fn toggle_collapsed(ed: &mut Editor, p: PathBuf) {
    if !ed.sidebar.collapsed.remove(&p) {
        ed.sidebar.collapsed.insert(p);
    }
}

/// A suggested branch name for a new worktree.
fn suggest_branch(ed: &Editor) -> String {
    let prefix = ed.var_str("worktree-branch-prefix").unwrap_or_default();
    let id = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}{}", &id[..6])
}

/// Where a worktree for `branch` of project `main` goes (`worktree-directory`).
pub fn worktree_path(ed: &Editor, main: &Path, branch: &str) -> PathBuf {
    let base = crate::expand_home(&ed.var_str("worktree-directory").unwrap_or_else(|| "~/.serpis/worktrees".into()));
    base.join(dir_name(main)).join(git::slug(branch).replace('/', "-"))
}

/// Create a worktree on `branch` from the checkout at `from`, in the background, then run
/// `then` with its path.
pub fn create_worktree(ed: &mut Editor, from: PathBuf, branch: String, then: impl FnOnce(&mut Editor, PathBuf) + Send + 'static) {
    let Some(repo) = ed.repo_of(&from) else {
        ed.error(format!("{} is not in a git repository", from.display()));
        return;
    };
    let branch = branch.trim().to_string();
    if branch.is_empty() {
        return;
    }
    let path = worktree_path(ed, &repo.main, &branch);
    ed.message(format!("Creating worktree {branch}…"));
    let main = ed.main_sender();
    let root = repo.main.clone();
    ed.rt.spawn_blocking(move || {
        let r = git::add_worktree(&from, &path, &branch, None);
        main.send(move |ed| match r {
            Ok(()) => {
                let path = path.canonicalize().unwrap_or(path);
                ed.sidebar.repos.clear();
                refresh_git(ed, std::slice::from_ref(&root), true);
                ed.message(format!("Worktree {branch} at {}", path.display()));
                ed.run_hook("worktree-created", &json!({"project": root.to_string_lossy(), "path": path.to_string_lossy(), "branch": branch}));
                then(ed, path);
            }
            Err(e) => ed.error(format!("{e:#}")),
        });
    });
}

/// Remove a linked worktree after confirming; its buffers are killed first.
pub fn remove_worktree(ed: &mut Editor, path: PathBuf) {
    let Some(repo) = ed.repo_of(&path) else {
        ed.error(format!("{} is not a git worktree", path.display()));
        return;
    };
    if !repo.is_linked() {
        ed.error("The main checkout can't be removed");
        return;
    }
    let wt = repo.worktree.clone();
    let buffers: Vec<BufferId> = items(ed).into_iter().filter(|i| i.worktree.as_ref() == Some(&wt)).map(|i| i.buffer).collect();
    let status = git::status(&wt).unwrap_or_default();
    let mut what = vec![format!("Remove worktree {}", repo.head_label())];
    if !buffers.is_empty() {
        what.push(format!("{} buffer(s) will be killed", buffers.len()));
    }
    if status.dirty() {
        what.push(format!("{} uncommitted change(s) will be lost", status.changed + status.untracked));
    }
    let prompt = format!("{}?", what.join("; "));
    ed.y_or_n(&prompt, move |ed, yes| {
        if !yes {
            return;
        }
        for id in buffers {
            ed.kill_buffer(id);
        }
        match git::remove_worktree(&repo.main, &wt, status.dirty()) {
            Ok(()) => {
                let policy = ed.var_str("worktree-delete-branch").unwrap_or_else(|| "never".into());
                if let (Some(b), true) = (&repo.branch, policy != "never") {
                    if let Err(e) = git::delete_branch(&repo.main, b, policy == "always") {
                        ed.warn(format!("Branch {b} kept: {e:#}"));
                    }
                }
                ed.sidebar.repos.clear();
                ed.sidebar.collapsed.remove(&wt);
                refresh_git(ed, std::slice::from_ref(&repo.main), true);
                ed.message(format!("Removed worktree {}", wt.display()));
                ed.run_hook("worktree-removed", &json!({"project": repo.main.to_string_lossy(), "path": wt.to_string_lossy(), "branch": repo.branch}));
            }
            Err(e) => ed.error(format!("{e:#}")),
        }
    });
}

pub fn add_project(ed: &mut Editor, dir: &Path) {
    let root = ed.project_root_of(dir).unwrap_or_else(|| dir.to_path_buf());
    let mut list = var_list(ed, "projects");
    let s = root.to_string_lossy().into_owned();
    if !list.contains(&s) {
        list.push(s.clone());
        ed.set_var("projects", json!(list));
        ed.persist_state("projects", json!(list));
    }
    ed.sidebar.collapsed.remove(&root);
}

pub fn remove_project(ed: &mut Editor, root: &Path) {
    let s = root.to_string_lossy().into_owned();
    let list: Vec<String> = var_list(ed, "projects").into_iter().filter(|p| *crate::expand_home(p) != *root && *p != s).collect();
    ed.set_var("projects", json!(list));
    ed.persist_state("projects", json!(list));
}

pub fn install(ed: &mut Editor) {
    let o = BUILTIN;
    ed.defvar("sidebar-visible", json!(true), "Show the sidebar (sidebar-toggle). Interactive toggles are remembered.");
    ed.defvar("sidebar-width", json!(270.0), "Sidebar width in points (drag its edge).");
    ed.defvar("sidebar-kinds", json!(["chat", "terminal"]), "Buffer kinds the sidebar lists under projects ([] lists all). Search always covers every buffer.");
    ed.defvar("sidebar-sort", json!("created"), "Order of buffers in a worktree: \"created\" or \"recent\" (last active first).");
    ed.defvar("sidebar-show-other-buffers", json!(false), "List buffers outside projects (or of other kinds) in an \"Other buffers\" group.");
    ed.defvar("sidebar-show-empty-worktrees", json!(true), "List a project's worktrees even when no buffer works in them.");
    ed.defvar("sidebar-git-refresh", json!(5.0), "Seconds between background refreshes of worktrees and git status while the sidebar shows.");
    ed.defvar("sidebar-jump-to-workspace", json!(true), "Opening a buffer shown on another workspace switches there instead of showing it here.");
    ed.defvar("projects", json!(Editor::persisted_state("projects").unwrap_or(json!([]))), "Project directories always listed in the sidebar (project-add / project-remove).");
    ed.defvar("agent-programs", json!(["claude", "codex", "pi", "aider", "opencode", "gemini", "amp", "goose", "crush", "cursor-agent", "qwen", "droid"]), "Programs that count as coding agents when they run in a terminal.");
    ed.defvar("terminal-busy-title-regexp", json!("^[\u{2800}-\u{28FF}]"), "A terminal title matching this means its agent is working (Claude Code spins a braille glyph). \"\" disables.");
    ed.defvar("terminal-quiet-seconds", json!(1.5), "Seconds without output after which an agent in a terminal counts as waiting for you.");
    ed.defvar("new-buffer-directory", json!("project"), "Where new-chat / new-terminal start: \"project\" (the current buffer's worktree root), \"buffer\" (its directory) or \"editor\" (where serpis started).");
    ed.defvar("worktree-directory", json!("~/.serpis/worktrees"), "Where new worktrees go: <dir>/<project>/<branch>.");
    ed.defvar("worktree-branch-prefix", json!("serpis/"), "Prefix of suggested branch names for new worktrees.");
    ed.defvar("worktree-delete-branch", json!("never"), "After worktree-remove, delete its branch: \"never\", \"merged\" (git branch -d) or \"always\" (-D).");
    ed.defvar(
        "sidebar-buffer-menu",
        json!([
            {"label": "Open", "command": "sidebar-open"},
            {"label": "Open in split", "command": "sidebar-open-split"},
            {"label": "Rename…", "command": "buffer-set-title"},
            "-",
            {"label": "New chat here", "command": "project-new-chat"},
            {"label": "New terminal here", "command": "project-new-terminal"},
            "-",
            {"label": "Kill buffer", "command": "sidebar-kill"},
        ]),
        "Right-click menu of sidebar buffers. Items get the buffer id as their last argument.",
    );
    ed.defvar(
        "sidebar-project-menu",
        json!([
            {"label": "New chat", "command": "project-new-chat"},
            {"label": "New chat in a new worktree…", "command": "worktree-new-chat"},
            {"label": "New terminal", "command": "project-new-terminal"},
            "-",
            {"label": "Refresh git status", "command": "sidebar-refresh"},
            {"label": "Remove from sidebar", "command": "project-remove"},
        ]),
        "Right-click menu of sidebar projects. Items get the project root as their last argument.",
    );
    ed.defvar(
        "sidebar-worktree-menu",
        json!([
            {"label": "New chat", "command": "project-new-chat"},
            {"label": "New terminal", "command": "project-new-terminal"},
            {"label": "New chat in a new worktree from here…", "command": "worktree-new-chat"},
            "-",
            {"label": "Remove worktree…", "command": "worktree-remove"},
        ]),
        "Right-click menu of sidebar worktrees. Items get the worktree path as their last argument.",
    );
    if let Some(v) = Editor::persisted_state("sidebar-visible") {
        ed.set_var("sidebar-visible", v);
    }
    if let Some(v) = Editor::persisted_state("sidebar-width") {
        ed.set_var("sidebar-width", v);
    }

    ed.define_command("sidebar-toggle", "Show or hide the sidebar.", o, |ed, _| {
        let on = !ed.var_bool("sidebar-visible");
        set_visible(ed, on);
        Ok(())
    });
    ed.define_command("sidebar-focus", "Move keys to the sidebar: type to search, arrows to pick, RET to open.", o, |ed, _| {
        if ed.sidebar.focused {
            ed.sidebar.focused = false;
        } else {
            focus(ed);
        }
        Ok(())
    });
    ed.define_command("sidebar-unfocus", "Give keys back to the focused window.", o, |ed, _| {
        ed.sidebar.focused = false;
        Ok(())
    });
    ed.define_command("sidebar-escape", "Clear the sidebar search, or leave the sidebar.", o, |ed, _| {
        if ed.sidebar.query.is_empty() {
            ed.sidebar.focused = false;
        } else {
            set_query(ed, String::new());
        }
        Ok(())
    });
    ed.define_command("sidebar-next", "Select the next sidebar row.", o, |ed, a| {
        move_selection(ed, a.count() as isize);
        Ok(())
    });
    ed.define_command("sidebar-previous", "Select the previous sidebar row.", o, |ed, a| {
        move_selection(ed, -(a.count() as isize));
        Ok(())
    });
    ed.define_command("sidebar-activate", "Open the selected buffer, or fold the selected project or worktree.", o, |ed, _| {
        activate(ed, false);
        Ok(())
    });
    ed.define_command("sidebar-toggle-collapse", "Fold or unfold the selected project or worktree (or the one holding the selected buffer).", o, |ed, _| {
        match selected_row(ed) {
            Some(RowRef::Project(p) | RowRef::Worktree(p)) => toggle_collapsed(ed, p),
            Some(RowRef::Item(id)) => {
                if let Some(w) = items(ed).into_iter().find(|i| i.buffer == id).and_then(|i| i.worktree) {
                    toggle_collapsed(ed, w);
                }
            }
            Some(RowRef::Session(_)) | None => {}
        }
        Ok(())
    });
    ed.define_command("sidebar-delete-backward", "Delete the last character of the sidebar search.", o, |ed, _| {
        let mut q = ed.sidebar.query.clone();
        q.pop();
        set_query(ed, q);
        Ok(())
    });
    ed.define_command("sidebar-kill-word", "Delete the last word of the sidebar search.", o, |ed, _| {
        let q = ed.sidebar.query.trim_end();
        let cut = q.rfind(' ').map(|i| i + 1).unwrap_or(0);
        let q = q[..cut].to_string();
        set_query(ed, q);
        Ok(())
    });
    ed.define_command("sidebar-clear", "Clear the sidebar search.", o, |ed, _| {
        set_query(ed, String::new());
        Ok(())
    });
    ed.define_command("sidebar-search", "Search all buffers from the sidebar (param: query).", o, |ed, a| {
        focus(ed);
        set_query(ed, a.str(0).unwrap_or("").to_string());
        Ok(())
    });
    ed.define_command("sidebar-open", "Open a buffer (param: buffer id; default: the selected one).", o, |ed, a| {
        if let Some(id) = target_buffer(ed, a) {
            ed.open_buffer(id);
        }
        Ok(())
    });
    ed.define_command("sidebar-open-split", "Open a buffer in a new window to the right (param: buffer id).", o, |ed, a| {
        if let Some(id) = target_buffer(ed, a) {
            ed.sidebar.focused = false;
            ed.split_window(false);
            ed.show_buffer(id);
        }
        Ok(())
    });
    ed.define_command("sidebar-kill", "Kill a buffer from the sidebar (param: buffer id; asks first if a program runs).", o, |ed, a| {
        if let Some(id) = target_buffer(ed, a) {
            ed.kill_buffer_interactive(id);
        }
        Ok(())
    });
    ed.define_command("sidebar-refresh", "Re-read worktrees and git status now.", o, |ed, _| {
        ed.sidebar.repos.clear();
        let roots: Vec<PathBuf> = projects(ed).into_iter().filter(|p| p.git).filter_map(|p| p.root).collect();
        refresh_git(ed, &roots, true);
        Ok(())
    });
    ed.define_command("buffer-set-title", "Set a buffer's title (params: buffer id, title; prompts without a title).", o, |ed, a| {
        let Some(id) = target_buffer(ed, a) else { return Ok(()) };
        let set = move |ed: &mut Editor, t: String| ed.set_buffer_title(id, Some(t));
        match a.str(1) {
            Some(t) => set(ed, t.to_string()),
            None => {
                let cur = ed.buffer_title(id);
                ed.read_string("Title: ", &cur, set);
            }
        }
        Ok(())
    });
    ed.define_command("project-new-chat", "New chat in a project or worktree (param: directory; default: the selected or current one).", o, |ed, a| {
        let dir = target_dir(ed, a, true);
        ed.new_buffer_in("chat", &dir)?;
        Ok(())
    });
    ed.define_command("project-new-terminal", "New terminal in a project or worktree (param: directory).", o, |ed, a| {
        let dir = target_dir(ed, a, true);
        ed.new_buffer_in("terminal", &dir)?;
        Ok(())
    });
    ed.define_command("worktree-new-chat", "New chat in a new git worktree (params: directory, branch; prompts for the branch).", o, |ed, a| {
        let from = target_dir(ed, a, true);
        if ed.repo_of(&from).is_none() {
            anyhow::bail!("{} is not in a git repository", from.display());
        }
        let go = move |ed: &mut Editor, branch: String| {
            create_worktree(ed, from, branch, |ed, path| {
                if let Err(e) = ed.new_buffer_in("chat", &path) {
                    ed.error(format!("{e:#}"));
                }
            });
        };
        match a.str(1) {
            Some(b) => go(ed, b.to_string()),
            None => {
                let suggestion = suggest_branch(ed);
                ed.read_string("New worktree on branch: ", &suggestion, go);
            }
        }
        Ok(())
    });
    ed.define_command("worktree-new-terminal", "New terminal in a new git worktree (params: directory, branch).", o, |ed, a| {
        let from = target_dir(ed, a, true);
        let go = move |ed: &mut Editor, branch: String| {
            create_worktree(ed, from, branch, |ed, path| {
                if let Err(e) = ed.new_buffer_in("terminal", &path) {
                    ed.error(format!("{e:#}"));
                }
            });
        };
        match a.str(1) {
            Some(b) => go(ed, b.to_string()),
            None => {
                let suggestion = suggest_branch(ed);
                ed.read_string("New worktree on branch: ", &suggestion, go);
            }
        }
        Ok(())
    });
    ed.define_command("worktree-remove", "Remove a linked git worktree and kill its buffers, after confirming (param: path).", o, |ed, a| {
        let dir = target_dir(ed, a, true);
        remove_worktree(ed, dir);
        Ok(())
    });
    ed.define_command("worktree-switch", "Open a buffer in another worktree of this project (or start a chat there).", o, |ed, _| {
        let dir = ed.new_buffer_directory();
        let Some(repo) = ed.repo_of(&dir) else { anyhow::bail!("Not in a git repository") };
        let wts = git::worktrees(&repo.main)?;
        let cands = wts
            .iter()
            .map(|w| Candidate::new(w.branch.clone().unwrap_or_else(|| w.head.clone())).detail(w.path.to_string_lossy().into_owned()).value(w.path.to_string_lossy().into_owned()))
            .collect();
        ed.completing_read("Worktree: ", cands, true, |ed, c, _| {
            let Some(c) = c else { return };
            let path = PathBuf::from(c.value);
            let found = items(ed).into_iter().filter(|i| i.worktree.as_ref() == Some(&path)).max_by_key(|i| (i.kind == "chat", i.last_active));
            match found {
                Some(i) => ed.open_buffer(i.buffer),
                None => {
                    if let Err(e) = ed.new_buffer_in("chat", &path) {
                        ed.error(format!("{e:#}"));
                    }
                }
            }
        });
        Ok(())
    });
    ed.define_command("project-add", "Always list a project in the sidebar (param: directory; prompts otherwise).", o, |ed, a| {
        if let Some(d) = a.str(0) {
            add_project(ed, &crate::expand_home(d));
            ed.message(format!("Project {d} added"));
            return Ok(());
        }
        ed.read_directory("Add project: ", |ed, path| {
            add_project(ed, &path);
            ed.message(format!("Project {} added", crate::pickers::tilde(&path)));
        });
        Ok(())
    });
    ed.define_command("project-remove", "Stop listing a project in the sidebar (its buffers stay; param: root).", o, |ed, a| {
        let root = match a.str(0) {
            Some(d) => crate::expand_home(d),
            None => {
                let d = target_dir(ed, a, false);
                ed.project_root_of(&d).unwrap_or(d)
            }
        };
        remove_project(ed, &root);
        Ok(())
    });

    for (k, c) in [
        ("RET", "sidebar-activate"),
        ("C-m", "sidebar-activate"),
        ("s-RET", "sidebar-open-split"),
        ("M-RET", "sidebar-open-split"),
        ("<down>", "sidebar-next"),
        ("C-n", "sidebar-next"),
        ("C-j", "sidebar-next"),
        ("<up>", "sidebar-previous"),
        ("C-p", "sidebar-previous"),
        ("C-k", "sidebar-previous"),
        ("TAB", "sidebar-toggle-collapse"),
        ("ESC", "sidebar-escape"),
        ("C-g", "sidebar-unfocus"),
        ("DEL", "sidebar-delete-backward"),
        ("C-h", "sidebar-delete-backward"),
        ("M-DEL", "sidebar-kill-word"),
        ("C-w", "sidebar-kill-word"),
        ("C-u", "sidebar-clear"),
        ("C-c C-n", "project-new-chat"),
        ("C-c C-t", "project-new-terminal"),
        ("C-c C-w", "worktree-new-chat"),
        ("C-c C-k", "sidebar-kill"),
        ("C-c C-r", "buffer-set-title"),
    ] {
        ed.bind("sidebar", k, c, o);
    }
    for (k, c) in [("s-b", "sidebar-toggle"), ("s-e", "sidebar-focus"), ("s-N", "worktree-new-chat"), ("s-T", "project-new-terminal")] {
        ed.bind("global", k, c, o);
    }
}

// ---------------------------------------------------------------------------------------------
// Drawing
// ---------------------------------------------------------------------------------------------

/// Something a row asked for this frame (applied after drawing).
enum Act {
    Open(BufferId),
    Select(usize),
    Toggle(PathBuf),
    Resume(PathBuf),
    Run(String, Vec<Value>),
    Focus,
}

fn state_color(t: &crate::theme::Theme, s: ActivityState) -> Color32 {
    match s {
        ActivityState::Idle => t.fg_faint,
        ActivityState::Working => t.accent,
        ActivityState::Waiting => t.yellow,
        ActivityState::Error => t.red,
        ActivityState::Exited => t.fg_faint,
    }
}

/// A stable color per project name.
fn project_color(t: &crate::theme::Theme, name: &str) -> Color32 {
    let palette = [t.accent, t.green, t.yellow, t.purple, t.cyan, t.blue, t.accent2, t.red];
    let h = name.bytes().fold(5381u32, |h, b| h.wrapping_mul(33) ^ b as u32);
    palette[h as usize % palette.len()]
}

pub fn fmt_age(d: Duration) -> String {
    let s = d.as_secs();
    match s {
        0..=59 => "now".into(),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

/// The line under an item's title: what it is doing, or what it is.
fn status_line(it: &Item) -> String {
    match (it.activity.state, it.agent) {
        (ActivityState::Working, _) => it.activity.detail.clone(),
        (ActivityState::Waiting, true) => format!("Waiting for you · {}", it.activity.detail),
        (ActivityState::Waiting, false) => format!("Waiting · {}", it.activity.detail),
        (ActivityState::Error, _) => it.activity.detail.clone(),
        (ActivityState::Exited, _) => "Exited".into(),
        (ActivityState::Idle, _) if !it.activity.detail.is_empty() => it.activity.detail.clone(),
        (ActivityState::Idle, _) => if it.kind == "terminal" { "Shell".into() } else { it.kind.clone() },
    }
}

/// Paint the sidebar into `rect`: a flush panel on the app background.
pub fn draw(ui: &mut egui::Ui, ed: &mut Editor, rect: Rect) {
    use crate::chrome;
    let theme = ed.theme.clone();
    let t = &theme;
    let k = t.ui_scale();
    let time = ui.input(|i| i.time);
    let mut acts: Vec<Act> = Vec::new();
    let pad = 10.0 * k;
    let inner = rect.shrink2(egui::vec2(pad, 0.0));
    let title_font = FontId::proportional(t.font_size - 1.0);
    let small = FontId::proportional(t.font_size - 2.5);
    let tiny = FontId::proportional(t.font_size - 3.5);
    // Shortcuts as currently bound (a rebound or unbound command shows its real key, or none).
    let keys = crate::commands::all_bindings(ed);

    // Search.
    let search_r = Rect::from_min_size(egui::pos2(inner.left(), rect.top() + 10.0 * k), egui::vec2(inner.width(), (32.0 * k).round()));
    let sresp = ui.interact(search_r, ui.scope_id().with("sidebar-search"), Sense::click());
    if sresp.clicked() {
        acts.push(Act::Focus);
    }
    let p = ui.painter();
    let focused = ed.sidebar.focused;
    p.rect_filled(search_r, 8.0 * k, if focused { t.bg_elevated } else { chrome::hover_bg(t) });
    if focused {
        p.rect_stroke(search_r, 8.0 * k, Stroke::new(1.0, chrome::tint(t.accent, 0.6)), StrokeKind::Inside);
    }
    let cy = search_r.center().y;
    chrome::magnifier(p, egui::pos2(search_r.left() + 14.0 * k, cy - 1.0), 4.5 * k, t.fg_faint);
    let tx = search_r.left() + 28.0 * k;
    let query_font = FontId::proportional(t.font_size - 1.0);
    if ed.sidebar.query.is_empty() {
        chrome::text(p, egui::pos2(tx, cy), if focused { "Search everything…" } else { "Search" }, query_font.clone(), t.fg_faint, search_r.width() - 70.0 * k);
        if let Some(key) = keys.get("sidebar-focus").filter(|_| !focused) {
            chrome::keycap(p, search_r.right() - 8.0 * k, cy, key, t);
        }
    } else {
        let w = chrome::text(p, egui::pos2(tx, cy), &ed.sidebar.query, query_font, t.fg, search_r.width() - 40.0 * k);
        if focused && (time * 2.0) as i64 % 2 == 0 {
            p.line_segment([egui::pos2(tx + w + 1.5, cy - 8.0 * k), egui::pos2(tx + w + 1.5, cy + 8.0 * k)], Stroke::new(1.5, t.cursor));
        }
    }
    if focused {
        ui.ctx().request_repaint_after(Duration::from_millis(500));
    }

    // Actions: a "New chat" button, then terminal and worktree as icon buttons.
    let act_y = search_r.bottom() + 8.0 * k;
    let bh = (32.0 * k).round();
    let new_r = Rect::from_min_size(egui::pos2(inner.left(), act_y), egui::vec2(inner.width() - 3.0 * (bh + 4.0 * k), bh));
    let nresp = ui.interact(new_r, ui.scope_id().with("sidebar-new"), Sense::click()).on_hover_text("New chat in this project");
    let p = ui.painter();
    p.rect_filled(new_r, 8.0 * k, if nresp.hovered() { chrome::tint(t.accent, 0.22) } else { chrome::tint(t.accent, 0.13) });
    chrome::plus(p, Rect::from_center_size(egui::pos2(new_r.left() + 15.0 * k, new_r.center().y), Vec2::splat(9.0 * k)), t.accent);
    chrome::text(p, egui::pos2(new_r.left() + 28.0 * k, new_r.center().y), "New chat", FontId::proportional(t.font_size - 1.0), t.accent, new_r.width() - 70.0 * k);
    if let Some(key) = keys.get("new-chat") {
        chrome::keycap(p, new_r.right() - 8.0 * k, new_r.center().y, key, t);
    }
    if nresp.clicked() {
        acts.push(Act::Run("project-new-chat".into(), Vec::new()));
    }
    let term_r = Rect::from_min_size(egui::pos2(new_r.right() + 4.0 * k, act_y), Vec2::splat(bh));
    if chrome::ghost_button(ui, term_r, ui.scope_id().with("sidebar-term"), t, &chrome::with_key("New terminal", &keys, "project-new-terminal"), chrome::prompt).clicked() {
        acts.push(Act::Run("project-new-terminal".into(), Vec::new()));
    }
    let wt_r = Rect::from_min_size(egui::pos2(term_r.right() + 4.0 * k, act_y), Vec2::splat(bh));
    if chrome::ghost_button(ui, wt_r, ui.scope_id().with("sidebar-wt"), t, &chrome::with_key("New chat in a new worktree", &keys, "worktree-new-chat"), |p, r, c| chrome::branch(p, r.expand(1.0), c)).clicked() {
        acts.push(Act::Run("worktree-new-chat".into(), Vec::new()));
    }
    let open_r = Rect::from_min_size(egui::pos2(wt_r.right() + 4.0 * k, act_y), Vec2::splat(bh));
    if chrome::ghost_button(ui, open_r, ui.scope_id().with("sidebar-open"), t, &chrome::with_key("Open a folder as a project", &keys, "project-open"), |p, r, c| chrome::folder(p, r.expand(1.5), c)).clicked() {
        acts.push(Act::Run("project-open".into(), Vec::new()));
    }

    // The list.
    let list_rect = Rect::from_min_max(egui::pos2(rect.left(), act_y + bh + 10.0 * k), egui::pos2(rect.right(), rect.bottom() - 6.0 * k));
    let query = ed.sidebar.query.clone();
    let mut rows: Vec<RowRef> = Vec::new();
    let rects = std::cell::RefCell::new(Vec::new());
    let selected = ed.sidebar.selected;
    let reveal = std::mem::take(&mut ed.sidebar.reveal);
    let mut any_working = false;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list_rect).id_salt("sidebar-list"));
    child.set_clip_rect(list_rect);
    let projects = if query.is_empty() { projects(ed) } else { Vec::new() };
    let found = if query.is_empty() { Vec::new() } else { search(ed, &query) };
    let found_sessions = if query.is_empty() { Vec::new() } else { search_sessions(ed, &query) };
    let roots: Vec<PathBuf> = projects.iter().filter(|p| p.git).filter_map(|p| p.root.clone()).collect();
    refresh_git(ed, &roots, false);
    crate::session_ui::scan_then(ed, false, |_| {});
    let bmenu = ed.var("sidebar-buffer-menu").cloned().unwrap_or(Value::Null);
    let pmenu = ed.var("sidebar-project-menu").cloned().unwrap_or(Value::Null);
    let wmenu = ed.var("sidebar-worktree-menu").cloned().unwrap_or(Value::Null);
    let sb_focused = ed.sidebar.focused;
    let ed_ref: &Editor = ed;
    egui::ScrollArea::vertical().id_salt("sidebar-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 0.0);
        let w = ui.available_width();
        let x0 = rect.left() + pad;
        let x1 = rect.left() + w - pad;
        // Registers a row for keyboard navigation and tests; returns (index, rect, selected).
        let mut row = |ui: &mut egui::Ui, r: RowRef, h: f32| -> (usize, Rect, bool) {
            let idx = rows.len();
            rows.push(r);
            let (rr, _) = ui.allocate_exact_size(egui::vec2(w, h), Sense::hover());
            rects.borrow_mut().push(rr);
            let is_sel = sb_focused && idx == selected;
            if is_sel && reveal {
                ui.scroll_to_rect(rr, None);
            }
            (idx, rr, is_sel)
        };
        let section = |ui: &mut egui::Ui, label: &str, indent: f32| {
            let (r, _) = ui.allocate_exact_size(egui::vec2(w, 24.0 * k), Sense::hover());
            chrome::text(ui.painter(), egui::pos2(x0 + indent, r.center().y + 3.0 * k), &label.to_uppercase(), tiny.clone(), t.fg_faint, x1 - x0);
        };

        let item_row = |ui: &mut egui::Ui, row: &mut dyn FnMut(&mut egui::Ui, RowRef, f32) -> (usize, Rect, bool), it: &Item, indent: f32, acts: &mut Vec<Act>, any_working: &mut bool, subtitle: Option<String>| {
            let (idx, r, is_sel) = row(ui, RowRef::Item(it.buffer), 44.0 * k);
            let card = Rect::from_min_max(egui::pos2(x0 + indent, r.top() + 1.0), egui::pos2(x1, r.bottom() - 1.0));
            let hovered = ui.rect_contains_pointer(card);
            let id = ui.scope_id().with(("sb-item", buffer_id_to_u64(it.buffer)));
            let kill_r = Rect::from_center_size(egui::pos2(card.right() - 14.0 * k, card.center().y), Vec2::splat(22.0 * k));
            if hovered && chrome::ghost_button(ui, kill_r, id.with("kill"), t, "Kill buffer", chrome::cross).clicked() {
                acts.push(Act::Run("sidebar-kill".into(), vec![json!(buffer_id_to_u64(it.buffer))]));
            }
            let main_r = if hovered { Rect::from_min_max(card.min, egui::pos2(kill_r.left() - 2.0, card.bottom())) } else { card };
            let resp = ui.interact(main_r, id, Sense::click());
            let p = ui.painter();
            let bg = if it.current { chrome::selected_bg(t) } else if resp.hovered() || is_sel { chrome::hover_bg(t) } else { Color32::TRANSPARENT };
            p.rect_filled(card, 8.0 * k, bg);
            if is_sel {
                p.rect_stroke(card, 8.0 * k, Stroke::new(1.0, chrome::tint(t.accent, 0.5)), StrokeKind::Inside);
            }
            let st = it.activity.state;
            let col = state_color(t, st);
            let y1 = card.top() + 14.0 * k;
            let y2 = card.bottom() - 13.0 * k;
            // Leading status: a spinner while working, a dot while waiting, else the kind.
            let ic = egui::pos2(card.left() + 15.0 * k, y1);
            match st {
                ActivityState::Working => {
                    *any_working = true;
                    chrome::spinner(p, ic, 5.0 * k, time, col);
                }
                ActivityState::Waiting => {
                    let a = 0.65 + 0.35 * ((time * 2.5).sin() as f32);
                    p.circle_filled(ic, 4.5 * k, chrome::tint(col, 0.25 * a));
                    p.circle_filled(ic, 3.0 * k, col.gamma_multiply(a));
                }
                ActivityState::Error => chrome::cross(p, Rect::from_center_size(ic, Vec2::splat(9.0 * k)), col),
                _ => chrome::kind_icon(p, ic, 11.0 * k, &it.kind, if it.current { t.accent } else { t.fg_faint }),
            }
            let tx = card.left() + 30.0 * k;
            let mut rx = card.right() - 10.0 * k;
            if !hovered {
                let age = fmt_age(it.since.elapsed());
                let aw = chrome::text_width(p, &age, tiny.clone());
                chrome::text(p, egui::pos2(rx - aw, y1), &age, tiny.clone(), t.fg_faint, aw + 1.0);
                rx -= aw + 8.0 * k;
            } else {
                rx = kill_r.left() - 4.0 * k;
            }
            if it.attention {
                p.circle_filled(egui::pos2(rx - 3.0 * k, y1), 3.5 * k, if st == ActivityState::Error { t.red } else { t.accent });
                rx -= 12.0 * k;
            }
            let title_col = if it.current || it.attention { t.fg } else { chrome::tint(t.fg, 0.82) };
            chrome::text(p, egui::pos2(tx, y1), &it.label(), title_font.clone(), title_col, rx - tx);
            let line2 = match (&it.snippet, subtitle) {
                (Some(s), _) => format!("“{s}”"),
                (None, Some(s)) => format!("{s} · {}", status_line(it)),
                (None, None) => status_line(it),
            };
            let lcol = if matches!(st, ActivityState::Idle | ActivityState::Exited) { t.fg_faint } else { col };
            chrome::text(p, egui::pos2(tx, y2), &line2, small.clone(), lcol, card.right() - 10.0 * k - tx);
            if resp.clicked() {
                acts.push(Act::Select(idx));
                acts.push(Act::Open(it.buffer));
            }
            if resp.middle_clicked() {
                acts.push(Act::Run("sidebar-kill".into(), vec![json!(buffer_id_to_u64(it.buffer))]));
            }
            let mut chosen = None;
            resp.context_menu(|ui| crate::mouse::menu_ui(ui, ed_ref, &bmenu, &mut chosen));
            if let Some((cmd, mut args)) = chosen {
                args.push(json!(buffer_id_to_u64(it.buffer)));
                if cmd.starts_with("project-") || cmd.starts_with("worktree-") {
                    args = vec![json!(it.worktree.as_ref().or(it.dir.as_ref()).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default())];
                }
                acts.push(Act::Run(cmd, args));
            }
        };

        let session_row = |ui: &mut egui::Ui, row: &mut dyn FnMut(&mut egui::Ui, RowRef, f32) -> (usize, Rect, bool), i: &crate::session::Info, indent: f32, sub: bool, acts: &mut Vec<Act>| {
            let (idx, r, is_sel) = row(ui, RowRef::Session(i.path.clone()), if sub { 44.0 } else { 30.0 } * k);
            let card = Rect::from_min_max(egui::pos2(x0 + indent, r.top() + 1.0), egui::pos2(x1, r.bottom() - 1.0));
            let resp = ui.interact(card, ui.scope_id().with(("sb-session", i.path.to_string_lossy().into_owned())), Sense::click());
            let p = ui.painter();
            if resp.hovered() || is_sel {
                p.rect_filled(card, 8.0 * k, chrome::hover_bg(t));
            }
            if is_sel {
                p.rect_stroke(card, 8.0 * k, Stroke::new(1.0, chrome::tint(t.accent, 0.5)), StrokeKind::Inside);
            }
            let y1 = if sub { card.top() + 14.0 * k } else { card.center().y };
            chrome::clock(p, egui::pos2(card.left() + 15.0 * k, y1), 4.5 * k, t.fg_faint);
            let age = i.modified.and_then(|tm| tm.elapsed().ok()).map(fmt_age).unwrap_or_default();
            let aw = chrome::text_width(p, &age, tiny.clone());
            chrome::text(p, egui::pos2(card.right() - 10.0 * k - aw, y1), &age, tiny.clone(), t.fg_faint, aw + 1.0);
            let tx = card.left() + 30.0 * k;
            let font = if sub { title_font.clone() } else { small.clone() };
            chrome::text(p, egui::pos2(tx, y1), &i.title(), font, if resp.hovered() { t.fg } else { t.fg_dim }, card.right() - 20.0 * k - aw - tx);
            if sub {
                let project = i.project.as_deref().map(dir_name).unwrap_or_else(|| dir_name(&i.cwd));
                chrome::text(p, egui::pos2(tx, card.bottom() - 13.0 * k), &format!("{project} · {} messages · saved", i.messages), small.clone(), t.fg_faint, card.right() - 10.0 * k - tx);
            }
            let resp = resp.on_hover_text(format!("Resume · {}", i.path.display()));
            if resp.clicked() {
                acts.push(Act::Select(idx));
                acts.push(Act::Resume(i.path.clone()));
            }
            let path = i.path.to_string_lossy().into_owned();
            resp.context_menu(|ui| {
                if ui.button("Resume").clicked() {
                    acts.push(Act::Resume(i.path.clone()));
                    ui.close();
                }
                if ui.button("Delete session…").clicked() {
                    acts.push(Act::Run("session-delete".into(), vec![json!(path)]));
                    ui.close();
                }
            });
        };

        if !query.is_empty() {
            let n = found.len();
            section(ui, &format!("{n} buffer{}", if n == 1 { "" } else { "s" }), 4.0 * k);
            for it in &found {
                let project = it.project.as_deref().map(dir_name).unwrap_or_else(|| "no project".into());
                let sub = match &it.branch {
                    Some(b) if !b.is_empty() => format!("{project} · {b}"),
                    _ => project,
                };
                item_row(ui, &mut row, it, 0.0, &mut acts, &mut any_working, Some(sub));
            }
            if !found_sessions.is_empty() {
                ui.add_space(6.0 * k);
                section(ui, &format!("Saved sessions · {}", found_sessions.len()), 4.0 * k);
                for i in &found_sessions {
                    session_row(ui, &mut row, i, 0.0, true, &mut acts);
                }
            }
            return;
        }

        for pr in &projects {
            let key = pr.root.clone().unwrap_or_default();
            let collapsed = ed_ref.sidebar.collapsed.contains(&key);
            let (idx, r, is_sel) = row(ui, RowRef::Project(key.clone()), 32.0 * k);
            let hovered = ui.rect_contains_pointer(r);
            let id = ui.scope_id().with(("sb-project", key.to_string_lossy().into_owned()));
            let bsz = (24.0 * k).round();
            let mut buttons_left = x1;
            if hovered && pr.root.is_some() {
                let dir = json!(key.to_string_lossy());
                let mut bx = x1 - bsz;
                for (tip, cmd, kind) in [("New chat", "project-new-chat", 0), ("New terminal", "project-new-terminal", 1), ("New chat in a new worktree", "worktree-new-chat", 2)] {
                    if kind == 2 && !pr.git {
                        continue;
                    }
                    let br = Rect::from_min_size(egui::pos2(bx, r.center().y - bsz / 2.0), Vec2::splat(bsz));
                    let resp = chrome::ghost_button(ui, br, id.with(cmd), t, tip, match kind {
                        0 => chrome::plus,
                        1 => chrome::prompt,
                        _ => |p: &egui::Painter, r: Rect, c: Color32| chrome::branch(p, r.expand(1.0), c),
                    });
                    if resp.clicked() {
                        acts.push(Act::Run(cmd.into(), vec![dir.clone()]));
                    }
                    buttons_left = br.left();
                    bx -= bsz;
                }
            }
            let card = Rect::from_min_max(egui::pos2(x0, r.top() + 1.0), egui::pos2(x1, r.bottom() - 1.0));
            let resp = ui.interact(Rect::from_min_max(card.min, egui::pos2(buttons_left - 2.0, card.bottom())), id, Sense::click());
            let p = ui.painter();
            if is_sel {
                p.rect_filled(card, 8.0 * k, chrome::hover_bg(t));
                p.rect_stroke(card, 8.0 * k, Stroke::new(1.0, chrome::tint(t.accent, 0.5)), StrokeKind::Inside);
            }
            let cy = r.center().y;
            let pc = project_color(t, &pr.name);
            if pr.root.is_some() {
                chrome::avatar(p, egui::pos2(x0 + 15.0 * k, cy), 20.0 * k, &pr.name, pc);
            } else {
                chrome::lines(p, Rect::from_center_size(egui::pos2(x0 + 15.0 * k, cy), Vec2::splat(11.0 * k)), t.fg_faint);
            }
            // Counts: working agents and buffers that want you, as pills.
            let working = pr.items().filter(|i| i.activity.state == ActivityState::Working).count();
            let waiting = pr.items().filter(|i| i.activity.state == ActivityState::Waiting || i.attention).count();
            let mut rx = buttons_left - 4.0 * k;
            if !hovered {
                chrome::chevron(p, egui::pos2(rx - 6.0 * k, cy), !collapsed, t.fg_faint);
                rx -= 18.0 * k;
                for (n, c) in [(waiting, t.yellow), (working, t.accent)] {
                    if n > 0 {
                        let pr = chrome::pill(p, rx, cy, &n.to_string(), tiny.clone(), c, chrome::tint(c, 0.16), true);
                        rx = pr.left() - 4.0 * k;
                    }
                }
            }
            let name_font = FontId::proportional(t.font_size - 0.5);
            chrome::text(p, egui::pos2(x0 + 32.0 * k, cy), &pr.name, name_font, t.fg, rx - x0 - 36.0 * k);
            if resp.clicked() {
                acts.push(Act::Select(idx));
                acts.push(Act::Toggle(key.clone()));
            }
            if let Some(root) = &pr.root {
                let mut chosen = None;
                let resp = resp.on_hover_text(root.to_string_lossy());
                resp.context_menu(|ui| crate::mouse::menu_ui(ui, ed_ref, &pmenu, &mut chosen));
                if let Some((cmd, mut args)) = chosen {
                    args.push(json!(root.to_string_lossy()));
                    acts.push(Act::Run(cmd, args));
                }
            }
            if collapsed {
                ui.add_space(4.0 * k);
                continue;
            }
            let show_wt = pr.git;
            if pr.items().next().is_none() && pr.sessions.is_empty() {
                let (er, _) = ui.allocate_exact_size(egui::vec2(w, 26.0 * k), Sense::hover());
                chrome::text(ui.painter(), egui::pos2(x0 + 32.0 * k, er.center().y), "No chats yet", small.clone(), t.fg_faint, w);
            }
            for wt in &pr.worktrees {
                let wcollapsed = ed_ref.sidebar.collapsed.contains(&wt.path);
                if show_wt {
                    let (idx, r, is_sel) = row(ui, RowRef::Worktree(wt.path.clone()), 26.0 * k);
                    let hovered = ui.rect_contains_pointer(r);
                    let id = ui.scope_id().with(("sb-wt", wt.path.to_string_lossy().into_owned()));
                    let mut buttons_left = x1;
                    if hovered {
                        let dir = json!(wt.path.to_string_lossy());
                        let bsz = (22.0 * k).round();
                        let mut bx = x1 - bsz;
                        let mut btns: Vec<(&str, &str, fn(&egui::Painter, Rect, Color32))> = vec![("New chat here", "project-new-chat", chrome::plus)];
                        if !wt.main {
                            btns.insert(0, ("Remove worktree", "worktree-remove", chrome::cross));
                        }
                        for (tip, cmd, paint) in btns {
                            let br = Rect::from_min_size(egui::pos2(bx, r.center().y - bsz / 2.0), Vec2::splat(bsz));
                            if chrome::ghost_button(ui, br, id.with(cmd), t, tip, paint).clicked() {
                                acts.push(Act::Run(cmd.into(), vec![dir.clone()]));
                            }
                            buttons_left = br.left();
                            bx -= bsz;
                        }
                    }
                    let card = Rect::from_min_max(egui::pos2(x0 + 18.0 * k, r.top()), egui::pos2(x1, r.bottom()));
                    let resp = ui.interact(Rect::from_min_max(card.min, egui::pos2(buttons_left - 2.0, card.bottom())), id, Sense::click());
                    let p = ui.painter();
                    if is_sel || resp.hovered() {
                        p.rect_filled(card, 6.0 * k, chrome::hover_bg(t));
                    }
                    let cy = r.center().y;
                    let bcol = if wt.main { t.fg_faint } else { t.purple };
                    chrome::branch(p, Rect::from_center_size(egui::pos2(card.left() + 12.0 * k, cy), egui::vec2(10.0 * k, 12.0 * k)), bcol);
                    let mut rx = buttons_left - 8.0 * k;
                    if let Some(s) = &wt.status {
                        let mut parts: Vec<(String, Color32)> = Vec::new();
                        if s.added > 0 {
                            parts.push((format!("+{}", s.added), t.green));
                        }
                        if s.removed > 0 {
                            parts.push((format!("−{}", s.removed), t.red));
                        }
                        if s.untracked > 0 {
                            parts.push((format!("{} new", s.untracked), t.fg_faint));
                        }
                        if s.ahead > 0 {
                            parts.push((format!("↑{}", s.ahead), t.cyan));
                        }
                        if s.behind > 0 {
                            parts.push((format!("↓{}", s.behind), t.yellow));
                        }
                        for (txt, c) in parts.into_iter().rev() {
                            let tw = chrome::text_width(p, &txt, tiny.clone());
                            rx -= tw;
                            chrome::text(p, egui::pos2(rx, cy), &txt, tiny.clone(), c, tw + 1.0);
                            rx -= 6.0 * k;
                        }
                    }
                    let label = if wt.branch.is_empty() { dir_name(&wt.path) } else { wt.branch.clone() };
                    let lcol = if wt.items.is_empty() { t.fg_faint } else { t.fg_dim };
                    chrome::text(p, egui::pos2(card.left() + 24.0 * k, cy), &label, small.clone(), lcol, rx - card.left() - 28.0 * k);
                    if resp.clicked() {
                        acts.push(Act::Select(idx));
                        acts.push(Act::Toggle(wt.path.clone()));
                    }
                    let mut chosen = None;
                    let path = wt.path.clone();
                    let resp = resp.on_hover_text(path.to_string_lossy());
                    resp.context_menu(|ui| crate::mouse::menu_ui(ui, ed_ref, &wmenu, &mut chosen));
                    if let Some((cmd, mut args)) = chosen {
                        args.push(json!(path.to_string_lossy()));
                        acts.push(Act::Run(cmd, args));
                    }
                    if wcollapsed {
                        continue;
                    }
                }
                for it in &wt.items {
                    item_row(ui, &mut row, it, if show_wt { 18.0 * k } else { 0.0 }, &mut acts, &mut any_working, None);
                }
            }
            if !pr.sessions.is_empty() {
                section(ui, "Recent", 22.0 * k);
                for i in &pr.sessions {
                    session_row(ui, &mut row, i, 18.0 * k, false, &mut acts);
                }
            }
            ui.add_space(10.0 * k);
        }
        if projects.is_empty() {
            let (er, _) = ui.allocate_exact_size(egui::vec2(w, 40.0 * k), Sense::hover());
            let hint = match keys.get("project-open") {
                Some(k) => format!("No projects yet. Open a folder with {}.", chrome::pretty_keys(k)),
                None => "No projects yet. Open a folder with project-open.".to_string(),
            };
            chrome::text(ui.painter(), egui::pos2(x0 + 4.0, er.center().y), &hint, small.clone(), t.fg_faint, w - 20.0);
        }
    });
    if any_working {
        ui.ctx().request_repaint_after(Duration::from_millis(60));
    }
    ed.sidebar.rows = rows;
    ed.sidebar.row_rects = rects.into_inner();
    if ed.sidebar.selected >= ed.sidebar.rows.len() {
        ed.sidebar.selected = ed.sidebar.rows.len().saturating_sub(1);
    }

    for a in acts {
        match a {
            Act::Open(id) => ed.open_buffer(id),
            Act::Select(i) => ed.sidebar.selected = i,
            Act::Toggle(p) => toggle_collapsed(ed, p),
            Act::Resume(p) => {
                if let Err(e) = crate::session_ui::resume(ed, &p) {
                    ed.error(format!("{e:#}"));
                }
            }
            Act::Focus => focus(ed),
            Act::Run(cmd, params) => crate::mouse::run_action(ed, (cmd, params)),
        }
    }
}

/// The draggable edge between the sidebar and the windows.
pub fn draw_handle(ui: &mut egui::Ui, ed: &mut Editor, rect: Rect) {
    let resp = ui.interact(rect, ui.scope_id().with("sidebar-handle"), Sense::click_and_drag());
    if resp.hovered() || resp.dragged() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeHorizontal);
        ui.painter().rect_filled(rect.shrink2(egui::vec2(2.0, 0.0)), 2.0, ed.theme.border_focused.gamma_multiply(0.6));
    }
    if resp.dragged() {
        if let Some(p) = resp.interact_pointer_pos() {
            let w = (p.x - ui.max_rect().left()).clamp(180.0, 900.0) / ed.theme.ui_scale();
            ed.set_var("sidebar-width", json!(w.round()));
        }
    }
    if resp.drag_stopped() {
        let w = ed.var("sidebar-width").cloned().unwrap_or(json!(270.0));
        ed.persist_state("sidebar-width", w);
    }
    if resp.double_clicked() {
        set_visible(ed, false);
    }
}
