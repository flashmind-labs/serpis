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

use egui::{Align2, Color32, FontId, Rect, Sense, Stroke, StrokeKind, Vec2};
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
    pub fn label(&self) -> &str {
        if self.title.is_empty() { &self.name } else { &self.title }
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
            let s = b.surface()?;
            let n = s.line_count();
            // Bounded: the most recent 4000 lines.
            (n.saturating_sub(4000)..n).rev().map(|i| s.line(i)).find(|l| l.to_lowercase().contains(&needle))
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
        let start = format!("{}/", ed.cwd.parent().unwrap_or(&ed.cwd).display());
        ed.completing_read_dynamic("Add project: ", &start, crate::commands::file_candidates, |ed, c, input| {
            let path = crate::expand_home(&c.map(|c| c.value).unwrap_or(input));
            if path.is_dir() {
                add_project(ed, &path);
                ed.message(format!("Project {} added", path.display()));
            } else {
                ed.error(format!("Not a directory: {}", path.display()));
            }
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

/// Row heights at the default font size; they grow with `Theme::ui_scale`.
const ROW_PROJECT: f32 = 32.0;
const ROW_WORKTREE: f32 = 24.0;
const ROW_ITEM: f32 = 42.0;

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
        0..=59 => format!("{s}s"),
        60..=3599 => format!("{}m", s / 60),
        3600..=86_399 => format!("{}h", s / 3600),
        _ => format!("{}d", s / 86_400),
    }
}

fn paint_spinner(p: &egui::Painter, c: egui::Pos2, r: f32, t: f64, color: Color32) {
    let a0 = (t * 5.0) as f32;
    let pts: Vec<egui::Pos2> = (0..=14).map(|i| a0 + i as f32 * 0.32).map(|a| c + Vec2::angled(a) * r).collect();
    p.add(egui::Shape::line(pts, Stroke::new(1.6, color)));
}

fn paint_branch(p: &egui::Painter, r: Rect, color: Color32) {
    let s = Stroke::new(1.2, color);
    let a = egui::pos2(r.left() + 2.5, r.top() + 2.0);
    let b = egui::pos2(r.left() + 2.5, r.bottom() - 2.0);
    let c = egui::pos2(r.right() - 2.5, r.top() + 3.5);
    p.line_segment([a, b], s);
    p.add(egui::Shape::line(vec![c, egui::pos2(c.x, r.center().y), egui::pos2(a.x, r.center().y + 3.0)], s));
    for q in [a, b, c] {
        p.circle_filled(q, 1.8, color);
    }
}

fn paint_chevron(p: &egui::Painter, c: egui::Pos2, open: bool, color: Color32) {
    let pts = if open {
        vec![c + egui::vec2(-3.5, -1.5), c + egui::vec2(3.5, -1.5), c + egui::vec2(0.0, 2.5)]
    } else {
        vec![c + egui::vec2(-1.5, -3.5), c + egui::vec2(-1.5, 3.5), c + egui::vec2(2.5, 0.0)]
    };
    p.add(egui::Shape::convex_polygon(pts, color, Stroke::NONE));
}

/// A small icon button that only shows while its row is hovered.
fn row_button(ui: &mut egui::Ui, rect: Rect, id: egui::Id, theme: &crate::theme::Theme, tip: &str, paint: impl FnOnce(&egui::Painter, Rect, Color32)) -> bool {
    let resp = ui.interact(rect, id, Sense::click()).on_hover_text(tip);
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 4, theme.border);
    }
    paint(p, rect.shrink(5.0), if resp.hovered() { theme.fg } else { theme.fg_dim });
    resp.clicked()
}

fn plus(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.4, c);
    p.line_segment([r.center_top(), r.center_bottom()], s);
    p.line_segment([r.left_center(), r.right_center()], s);
}

fn prompt_icon(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.4, c);
    p.add(egui::Shape::line(vec![r.left_top() + egui::vec2(0.0, 1.0), egui::pos2(r.center().x - 1.0, r.center().y), r.left_bottom() - egui::vec2(0.0, 1.0)], s));
    p.line_segment([egui::pos2(r.center().x + 1.0, r.bottom() - 1.0), egui::pos2(r.right(), r.bottom() - 1.0)], s);
}

fn cross(p: &egui::Painter, r: Rect, c: Color32) {
    let r = r.shrink(1.0);
    let s = Stroke::new(1.3, c);
    p.line_segment([r.left_top(), r.right_bottom()], s);
    p.line_segment([r.right_top(), r.left_bottom()], s);
}

fn text(p: &egui::Painter, pos: egui::Pos2, s: &str, font: FontId, color: Color32, max_w: f32) -> f32 {
    let mut g = p.layout_no_wrap(s.to_string(), font.clone(), color);
    if g.size().x > max_w && max_w > 10.0 {
        let mut n = s.chars().count();
        while n > 1 && g.size().x > max_w {
            n = n * 9 / 10;
            g = p.layout_no_wrap(format!("{}…", s.chars().take(n).collect::<String>()), font.clone(), color);
        }
    }
    let w = g.size().x;
    p.galley(egui::pos2(pos.x, pos.y - g.size().y / 2.0), g, color);
    w
}

/// Paint the sidebar into `rect`.
pub fn draw(ui: &mut egui::Ui, ed: &mut Editor, rect: Rect) {
    let theme = ed.theme.clone();
    let p = ui.painter();
    let r = theme.radius + 2;
    p.rect_filled(rect, r, theme.bg_alt);
    p.rect_stroke(rect, r, Stroke::new(1.0, if ed.sidebar.focused { theme.border_focused } else { theme.border }), StrokeKind::Inside);
    let time = ui.input(|i| i.time);
    let mut acts: Vec<Act> = Vec::new();
    let k = theme.ui_scale();

    // Search box.
    let search_r = Rect::from_min_size(rect.min + egui::vec2(10.0, 10.0), egui::vec2(rect.width() - 20.0, (30.0 * k).round()));
    let sresp = ui.interact(search_r, ui.scope_id().with("sidebar-search"), Sense::click());
    if sresp.clicked() {
        acts.push(Act::Focus);
    }
    let p = ui.painter();
    p.rect_filled(search_r, 6, theme.bg_elevated);
    p.rect_stroke(search_r, 6, Stroke::new(1.0, if ed.sidebar.focused { theme.border_focused } else { theme.border }), StrokeKind::Inside);
    let mag = egui::pos2(search_r.left() + 15.0, search_r.center().y - 1.0);
    p.circle_stroke(mag, 4.5, Stroke::new(1.3, theme.fg_faint));
    p.line_segment([mag + egui::vec2(3.2, 3.2), mag + egui::vec2(6.5, 6.5)], Stroke::new(1.3, theme.fg_faint));
    let small = FontId::proportional(theme.font_size - 2.0);
    let font = FontId::proportional(theme.font_size - 0.5);
    let tx = search_r.left() + 28.0;
    if ed.sidebar.query.is_empty() {
        let hint = if ed.sidebar.focused { "Search chats, terminals, projects…" } else { "Search  (s-e)" };
        text(p, egui::pos2(tx, search_r.center().y), hint, font.clone(), theme.fg_faint, search_r.width() - 36.0);
    } else {
        let w = text(p, egui::pos2(tx, search_r.center().y), &ed.sidebar.query, font.clone(), theme.fg, search_r.width() - 36.0);
        if ed.sidebar.focused && (time * 2.0) as i64 % 2 == 0 {
            p.line_segment([egui::pos2(tx + w + 1.5, search_r.top() + 8.0), egui::pos2(tx + w + 1.5, search_r.bottom() - 8.0)], Stroke::new(1.5, theme.cursor));
        }
    }
    if ed.sidebar.focused {
        ui.ctx().request_repaint_after(Duration::from_millis(500));
    }

    // Footer.
    let footer_h = (34.0 * k).round();
    let footer = Rect::from_min_max(egui::pos2(rect.left() + 8.0, rect.bottom() - footer_h), egui::pos2(rect.right() - 8.0, rect.bottom() - 6.0));
    let list_rect = Rect::from_min_max(egui::pos2(rect.left() + 4.0, search_r.bottom() + 8.0), egui::pos2(rect.right() - 4.0, footer.top() - 4.0));
    ui.painter().line_segment([egui::pos2(rect.left() + 8.0, footer.top() - 2.0), egui::pos2(rect.right() - 8.0, footer.top() - 2.0)], Stroke::new(1.0, theme.border));
    let buttons = [("New chat", "project-new-chat", "s-n"), ("Terminal", "project-new-terminal", "s-T"), ("Worktree", "worktree-new-chat", "s-N")];
    let bw = (footer.width() - 8.0) / 3.0;
    for (i, (label, cmd, key)) in buttons.iter().enumerate() {
        let br = Rect::from_min_size(egui::pos2(footer.left() + i as f32 * (bw + 4.0), footer.top() + 2.0), egui::vec2(bw, footer.height() - 4.0));
        let resp = ui.interact(br, ui.scope_id().with(("sidebar-footer", i)), Sense::click()).on_hover_text(format!("{cmd}  ({key})"));
        let p = ui.painter();
        p.rect_filled(br, 6, if resp.hovered() { theme.border } else { theme.bg_elevated });
        let ic = Rect::from_center_size(egui::pos2(br.left() + 13.0, br.center().y), Vec2::splat(9.0));
        match i {
            0 => plus(p, ic, theme.accent),
            1 => prompt_icon(p, ic, theme.green),
            _ => paint_branch(p, ic.expand(1.5), theme.purple),
        }
        text(p, egui::pos2(br.left() + 23.0, br.center().y), label, small.clone(), theme.fg_dim, br.width() - 26.0);
        if resp.clicked() {
            acts.push(Act::Run(cmd.to_string(), Vec::new()));
        }
    }

    // List.
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
        let item_row = |ui: &mut egui::Ui, it: &Item, indent: f32, rows: &mut Vec<RowRef>, acts: &mut Vec<Act>, any_working: &mut bool, subtitle: Option<String>| {
            let idx = rows.len();
            rows.push(RowRef::Item(it.buffer));
            let (r, _) = ui.allocate_exact_size(egui::vec2(w, ROW_ITEM * k), Sense::hover());
            rects.borrow_mut().push(r);
            let is_sel = sb_focused && idx == selected;
            if is_sel && reveal {
                ui.scroll_to_rect(r, None);
            }
            let hovered = ui.rect_contains_pointer(r);
            let kill_r = Rect::from_center_size(egui::pos2(r.right() - 16.0 * k, r.top() + 13.0 * k), Vec2::splat(20.0 * k));
            let main_r = if hovered { Rect::from_min_max(r.min, egui::pos2(kill_r.left() - 2.0, r.bottom())) } else { r };
            let id = ui.scope_id().with(("sb-item", buffer_id_to_u64(it.buffer)));
            if hovered && row_button(ui, kill_r, id.with("kill"), &theme, "Kill buffer", cross) {
                acts.push(Act::Run("sidebar-kill".into(), vec![json!(buffer_id_to_u64(it.buffer))]));
            }
            let resp = ui.interact(main_r, id, Sense::click());
            let p = ui.painter();
            let card = r.shrink2(egui::vec2(4.0, 2.0));
            let bg = if it.current { theme.selection.gamma_multiply(0.55) } else if is_sel || resp.hovered() { theme.border } else { Color32::TRANSPARENT };
            p.rect_filled(card, 6, bg);
            if is_sel {
                p.rect_stroke(card, 6, Stroke::new(1.0, theme.border_focused), StrokeKind::Inside);
            }
            let st = it.activity.state;
            let col = state_color(&theme, st);
            // Working rows pulse a bar on their left edge.
            if st == ActivityState::Working {
                *any_working = true;
                let a = 0.55 + 0.45 * ((time * 3.0).sin() as f32);
                p.rect_filled(Rect::from_min_size(egui::pos2(card.left(), card.top() + 6.0), egui::vec2(2.5, card.height() - 12.0)), 1.5, col.gamma_multiply(a));
            } else if it.current {
                p.rect_filled(Rect::from_min_size(egui::pos2(card.left(), card.top() + 6.0), egui::vec2(2.5, card.height() - 12.0)), 1.5, theme.accent);
            }
            let x = card.left() + 10.0 + indent;
            let y1 = card.top() + 12.0 * k;
            let y2 = card.bottom() - 11.0 * k;
            // Kind icon, tinted by state.
            let icon_r = Rect::from_center_size(egui::pos2(x + 5.0, y1), Vec2::splat(10.0));
            let icol = if st == ActivityState::Idle { if it.current { theme.accent } else { theme.fg_dim } } else { col };
            if it.kind == "terminal" {
                prompt_icon(p, icon_r.shrink(0.5), icol);
            } else if it.kind == "chat" {
                let c = icon_r.center();
                p.add(egui::Shape::convex_polygon(vec![c + egui::vec2(0.0, -5.0), c + egui::vec2(5.0, 0.0), c + egui::vec2(0.0, 5.0), c + egui::vec2(-5.0, 0.0)], icol, Stroke::NONE));
            } else {
                for k in 0..3 {
                    let yy = icon_r.top() + 2.0 + k as f32 * 3.0;
                    p.line_segment([egui::pos2(icon_r.left(), yy), egui::pos2(icon_r.right(), yy)], Stroke::new(1.2, icol));
                }
            }
            // Right side: age, attention dot, in-view marker.
            let mut rx = card.right() - 8.0 - if hovered { 22.0 } else { 0.0 };
            if it.attention {
                p.circle_filled(egui::pos2(rx - 4.0, y1), 4.0, if st == ActivityState::Error { theme.red } else { theme.accent });
                rx -= 14.0;
            }
            if !hovered {
                let age = fmt_age(it.since.elapsed());
                let g = p.layout_no_wrap(age, small.clone(), theme.fg_faint);
                rx -= g.size().x;
                p.galley(egui::pos2(rx, y1 - g.size().y / 2.0), g, theme.fg_faint);
                rx -= 6.0;
            }
            if it.visible && !it.current {
                let vr = Rect::from_center_size(egui::pos2(rx - 5.0, y1), egui::vec2(10.0, 8.0));
                p.rect_stroke(vr, 2, Stroke::new(1.1, theme.fg_faint), StrokeKind::Inside);
                p.line_segment([egui::pos2(vr.center().x, vr.top()), egui::pos2(vr.center().x, vr.bottom())], Stroke::new(1.1, theme.fg_faint));
                rx -= 14.0;
            }
            let name_font = if it.attention { FontId::proportional(theme.font_size - 0.5) } else { font.clone() };
            let name_col = if it.current || it.attention { theme.fg } else { theme.fg_dim };
            text(p, egui::pos2(x + 16.0, y1), it.label(), name_font, name_col, rx - x - 22.0);
            // Second line: state, or the search subtitle.
            let mut sx = x + 16.0;
            match st {
                ActivityState::Working => {
                    paint_spinner(p, egui::pos2(sx + 4.0, y2), 4.0, time, col);
                    sx += 13.0;
                }
                ActivityState::Waiting => {
                    let a = 0.6 + 0.4 * ((time * 2.0).sin() as f32);
                    p.circle_filled(egui::pos2(sx + 4.0, y2), 3.5, col.gamma_multiply(a));
                    sx += 13.0;
                }
                ActivityState::Error => {
                    cross(p, Rect::from_center_size(egui::pos2(sx + 4.0, y2), Vec2::splat(8.0)), col);
                    sx += 13.0;
                }
                _ => {}
            }
            let status = match (st, it.agent) {
                (ActivityState::Working, _) => it.activity.detail.clone(),
                (ActivityState::Waiting, true) => format!("{} · waiting for you", it.activity.detail),
                (ActivityState::Waiting, false) => format!("{} · waiting", it.activity.detail),
                (ActivityState::Error, _) => it.activity.detail.clone(),
                (ActivityState::Exited, _) => "exited".into(),
                (ActivityState::Idle, _) if !it.activity.detail.is_empty() => it.activity.detail.clone(),
                (ActivityState::Idle, _) => if it.title.is_empty() || it.title == it.name { it.kind.clone() } else { it.name.clone() },
            };
            let line2 = match (&it.snippet, subtitle) {
                (Some(s), _) => format!("“{s}”"),
                (None, Some(s)) => format!("{s} · {status}"),
                (None, None) => status,
            };
            let lcol = if matches!(st, ActivityState::Idle | ActivityState::Exited) { theme.fg_faint } else { col };
            text(p, egui::pos2(sx, y2), &line2, small.clone(), lcol, card.right() - sx - 10.0);
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
                // Commands that take a directory get the buffer's worktree instead.
                if cmd.starts_with("project-") || cmd.starts_with("worktree-") {
                    args = vec![json!(it.worktree.as_ref().or(it.dir.as_ref()).map(|p| p.to_string_lossy().into_owned()).unwrap_or_default())];
                }
                acts.push(Act::Run(cmd, args));
            }
        };

        // A closed session: one line, resumes on click.
        let session_row = |ui: &mut egui::Ui, i: &crate::session::Info, indent: f32, sub: bool, rows: &mut Vec<RowRef>, acts: &mut Vec<Act>| {
            let idx = rows.len();
            rows.push(RowRef::Session(i.path.clone()));
            let h = if sub { ROW_ITEM } else { 26.0 } * k;
            let (r, _) = ui.allocate_exact_size(egui::vec2(w, h), Sense::hover());
            rects.borrow_mut().push(r);
            let is_sel = sb_focused && idx == selected;
            if is_sel && reveal {
                ui.scroll_to_rect(r, None);
            }
            let resp = ui.interact(r, ui.scope_id().with(("sb-session", i.path.to_string_lossy().into_owned())), Sense::click());
            let p = ui.painter();
            let card = r.shrink2(egui::vec2(4.0, 1.0));
            if is_sel || resp.hovered() {
                p.rect_filled(card, 6, theme.border);
            }
            if is_sel {
                p.rect_stroke(card, 6, Stroke::new(1.0, theme.border_focused), StrokeKind::Inside);
            }
            let x = card.left() + 10.0 + indent;
            let y1 = if sub { card.top() + 12.0 * k } else { card.center().y };
            // A clock face.
            let c = egui::pos2(x + 5.0, y1);
            p.circle_stroke(c, 4.5, Stroke::new(1.1, theme.fg_faint));
            p.line_segment([c, c + egui::vec2(0.0, -2.8)], Stroke::new(1.1, theme.fg_faint));
            p.line_segment([c, c + egui::vec2(2.2, 0.0)], Stroke::new(1.1, theme.fg_faint));
            let age = i.modified.and_then(|t| t.elapsed().ok()).map(fmt_age).unwrap_or_default();
            let g = p.layout_no_wrap(age, small.clone(), theme.fg_faint);
            let rx = card.right() - 8.0 - g.size().x;
            p.galley(egui::pos2(rx, y1 - g.size().y / 2.0), g, theme.fg_faint);
            text(p, egui::pos2(x + 16.0, y1), &i.title(), small.clone(), theme.fg_dim, rx - x - 22.0);
            if sub {
                let project = i.project.as_deref().map(dir_name).unwrap_or_else(|| dir_name(&i.cwd));
                text(p, egui::pos2(x + 16.0, card.bottom() - 11.0 * k), &format!("{project} · {} msgs · saved session", i.messages), small.clone(), theme.fg_faint, card.right() - x - 26.0);
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
            let (hr, _) = ui.allocate_exact_size(egui::vec2(w, 22.0 * k), Sense::hover());
            let n = found.len();
            text(ui.painter(), egui::pos2(hr.left() + 10.0, hr.center().y), &format!("{n} match{}", if n == 1 { "" } else { "es" }), small.clone(), theme.fg_faint, w);
            for it in &found {
                let project = it.project.as_deref().map(dir_name).unwrap_or_else(|| "no project".into());
                let sub = match &it.branch {
                    Some(b) if !b.is_empty() => format!("{project} · {b}"),
                    _ => project,
                };
                item_row(ui, it, 0.0, &mut rows, &mut acts, &mut any_working, Some(sub));
            }
            if !found_sessions.is_empty() {
                let (hr, _) = ui.allocate_exact_size(egui::vec2(w, 26.0 * k), Sense::hover());
                text(ui.painter(), egui::pos2(hr.left() + 10.0, hr.center().y + 2.0), &format!("Saved sessions ({})", found_sessions.len()), small.clone(), theme.fg_faint, w);
                for i in &found_sessions {
                    session_row(ui, i, 0.0, true, &mut rows, &mut acts);
                }
            }
            return;
        }

        for pr in &projects {
            let key = pr.root.clone().unwrap_or_default();
            let collapsed = ed_ref.sidebar.collapsed.contains(&key);
            let idx = rows.len();
            rows.push(RowRef::Project(key.clone()));
            let (r, _) = ui.allocate_exact_size(egui::vec2(w, ROW_PROJECT * k), Sense::hover());
            rects.borrow_mut().push(r);
            let is_sel = sb_focused && idx == selected;
            if is_sel && reveal {
                ui.scroll_to_rect(r, None);
            }
            let hovered = ui.rect_contains_pointer(r);
            let id = ui.scope_id().with(("sb-project", key.to_string_lossy().into_owned()));
            let bsz = (22.0 * k).round();
            let mut bx = r.right() - 6.0 - bsz;
            let mut buttons_left = r.right();
            if hovered && pr.root.is_some() {
                let dir = json!(key.to_string_lossy());
                for (tip, cmd, kind) in [("New chat", "project-new-chat", 0), ("New terminal", "project-new-terminal", 1), ("New chat in a new worktree", "worktree-new-chat", 2)] {
                    if kind == 2 && !pr.git {
                        continue;
                    }
                    let br = Rect::from_min_size(egui::pos2(bx, r.center().y - bsz / 2.0), Vec2::splat(bsz));
                    let clicked = row_button(ui, br, id.with(cmd), &theme, tip, |p, rr, c| match kind {
                        0 => plus(p, rr, c),
                        1 => prompt_icon(p, rr, c),
                        _ => paint_branch(p, rr.expand(1.0), c),
                    });
                    if clicked {
                        acts.push(Act::Run(cmd.into(), vec![dir.clone()]));
                    }
                    buttons_left = br.left();
                    bx -= bsz + 2.0;
                }
            }
            let main_r = Rect::from_min_max(r.min, egui::pos2(buttons_left - 2.0, r.bottom()));
            let resp = ui.interact(main_r, id, Sense::click());
            let p = ui.painter();
            if is_sel || resp.hovered() {
                p.rect_filled(r.shrink2(egui::vec2(4.0, 2.0)), 6, theme.border);
            }
            let x = r.left() + 10.0;
            paint_chevron(p, egui::pos2(x + 3.0, r.center().y), !collapsed, theme.fg_faint);
            let pc = project_color(&theme, &pr.name);
            let badge = Rect::from_center_size(egui::pos2(x + 20.0, r.center().y), Vec2::splat(18.0));
            if pr.root.is_some() {
                p.rect_filled(badge, 5, pc.gamma_multiply(0.25));
                let initial = pr.name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
                p.text(badge.center(), Align2::CENTER_CENTER, initial, FontId::proportional(theme.font_size - 3.0), pc);
            } else {
                p.rect_stroke(badge, 5, Stroke::new(1.0, theme.fg_faint), StrokeKind::Inside);
            }
            // Counts: working agents and buffers that want you.
            let working = pr.items().filter(|i| i.activity.state == ActivityState::Working).count();
            let waiting = pr.items().filter(|i| i.activity.state == ActivityState::Waiting || i.attention).count();
            let mut rx = buttons_left - 6.0;
            if !hovered {
                for (n, c, spin) in [(waiting, theme.yellow, false), (working, theme.accent, true)] {
                    if n == 0 {
                        continue;
                    }
                    let g = p.layout_no_wrap(n.to_string(), small.clone(), c);
                    rx -= g.size().x;
                    p.galley(egui::pos2(rx, r.center().y - g.size().y / 2.0), g, c);
                    rx -= 11.0;
                    if spin {
                        paint_spinner(p, egui::pos2(rx + 4.5, r.center().y), 4.0, time, c);
                    } else {
                        p.circle_filled(egui::pos2(rx + 4.5, r.center().y), 3.5, c);
                    }
                    rx -= 8.0;
                }
            }
            let name_font = FontId::proportional(theme.font_size);
            text(p, egui::pos2(badge.right() + 8.0, r.center().y), &pr.name, name_font, theme.fg, rx - badge.right() - 12.0);
            if resp.clicked() {
                acts.push(Act::Select(idx));
                acts.push(Act::Toggle(key.clone()));
            }
            if let Some(root) = &pr.root {
                let mut chosen = None;
                resp.context_menu(|ui| crate::mouse::menu_ui(ui, ed_ref, &pmenu, &mut chosen));
                if let Some((cmd, mut args)) = chosen {
                    args.push(json!(root.to_string_lossy()));
                    acts.push(Act::Run(cmd, args));
                }
                if let Some(path) = &pr.root {
                    resp.on_hover_text(path.to_string_lossy());
                }
            }
            if collapsed {
                continue;
            }
            // Worktree headers appear for git projects; plain projects list buffers directly.
            let show_wt = pr.git;
            if pr.items().next().is_none() && !show_wt {
                let (er, _) = ui.allocate_exact_size(egui::vec2(w, 22.0 * k), Sense::hover());
                text(ui.painter(), egui::pos2(er.left() + 40.0, er.center().y), "No chats yet", small.clone(), theme.fg_faint, w);
            }
            for wt in &pr.worktrees {
                let wcollapsed = ed_ref.sidebar.collapsed.contains(&wt.path);
                if show_wt {
                    let idx = rows.len();
                    rows.push(RowRef::Worktree(wt.path.clone()));
                    let (r, _) = ui.allocate_exact_size(egui::vec2(w, ROW_WORKTREE * k), Sense::hover());
                    rects.borrow_mut().push(r);
                    let is_sel = sb_focused && idx == selected;
                    if is_sel && reveal {
                        ui.scroll_to_rect(r, None);
                    }
                    let hovered = ui.rect_contains_pointer(r);
                    let id = ui.scope_id().with(("sb-wt", wt.path.to_string_lossy().into_owned()));
                    let mut buttons_left = r.right();
                    if hovered {
                        let dir = json!(wt.path.to_string_lossy());
                        let mut bx = r.right() - 6.0 - 20.0 * k;
                        let mut btns = vec![("New chat here", "project-new-chat", 0)];
                        if !wt.main {
                            btns.insert(0, ("Remove worktree", "worktree-remove", 1));
                        }
                        for (tip, cmd, kind) in btns {
                            let br = Rect::from_min_size(egui::pos2(bx, r.center().y - 10.0 * k), Vec2::splat(20.0 * k));
                            if row_button(ui, br, id.with(cmd), &theme, tip, |p, rr, c| if kind == 0 { plus(p, rr, c) } else { cross(p, rr, c) }) {
                                acts.push(Act::Run(cmd.into(), vec![dir.clone()]));
                            }
                            buttons_left = br.left();
                            bx -= 22.0 * k;
                        }
                    }
                    let resp = ui.interact(Rect::from_min_max(r.min, egui::pos2(buttons_left - 2.0, r.bottom())), id, Sense::click());
                    let p = ui.painter();
                    if is_sel || resp.hovered() {
                        p.rect_filled(r.shrink2(egui::vec2(4.0, 1.0)), 5, theme.border);
                    }
                    let x = r.left() + 30.0;
                    let bcol = if wt.main { theme.fg_faint } else { theme.purple };
                    paint_branch(p, Rect::from_center_size(egui::pos2(x + 5.0, r.center().y), egui::vec2(10.0, 12.0)), bcol);
                    let mut rx = buttons_left - 8.0;
                    if let Some(s) = &wt.status {
                        let mut parts: Vec<(String, Color32)> = Vec::new();
                        if s.added > 0 {
                            parts.push((format!("+{}", s.added), theme.green));
                        }
                        if s.removed > 0 {
                            parts.push((format!("−{}", s.removed), theme.red));
                        }
                        if s.untracked > 0 {
                            parts.push((format!("?{}", s.untracked), theme.fg_faint));
                        }
                        if s.ahead > 0 {
                            parts.push((format!("↑{}", s.ahead), theme.cyan));
                        }
                        if s.behind > 0 {
                            parts.push((format!("↓{}", s.behind), theme.yellow));
                        }
                        for (t, c) in parts.into_iter().rev() {
                            let g = p.layout_no_wrap(t, small.clone(), c);
                            rx -= g.size().x;
                            p.galley(egui::pos2(rx, r.center().y - g.size().y / 2.0), g, c);
                            rx -= 5.0;
                        }
                    }
                    let label = if wt.branch.is_empty() { dir_name(&wt.path) } else { wt.branch.clone() };
                    let lcol = if wt.items.is_empty() { theme.fg_faint } else { theme.fg_dim };
                    text(p, egui::pos2(x + 15.0, r.center().y), &label, small.clone(), lcol, rx - x - 20.0);
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
                    item_row(ui, it, if show_wt { 22.0 } else { 12.0 }, &mut rows, &mut acts, &mut any_working, None);
                }
            }
            if !pr.sessions.is_empty() {
                let (hr, _) = ui.allocate_exact_size(egui::vec2(w, 20.0 * k), Sense::hover());
                text(ui.painter(), egui::pos2(hr.left() + 32.0, hr.center().y + 2.0), "Recent sessions", small.clone(), theme.fg_faint, w);
                for i in &pr.sessions {
                    session_row(ui, i, 22.0, false, &mut rows, &mut acts);
                }
            }
            ui.add_space(6.0);
        }
        if projects.is_empty() {
            let (er, _) = ui.allocate_exact_size(egui::vec2(w, 40.0 * k), Sense::hover());
            text(ui.painter(), egui::pos2(er.left() + 12.0, er.center().y), "No projects yet: start a chat or run project-add", small.clone(), theme.fg_faint, w - 20.0);
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
