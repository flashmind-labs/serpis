//! Saving, finding and resuming chat sessions (`chat-resume`, like pi's and Claude Code's
//! `/resume`).
//!
//! Chats record themselves to `session-directory` as they go, in pi's format; `chat-resume`
//! picks from the sessions of the current project (all of them with a prefix argument or the
//! `all` parameter) in every `session-directories` entry, so pi's sessions resume here and
//! serpis's in pi. `chat-continue` resumes the latest one. Open chats come back after a restart
//! with `session-restore-on-start`. The list is read in the background and cached.
//!
//! Settings: `session-save`, `session-directory`, `session-directories`,
//! `session-restore-on-start`, `session-scan-interval`, `sidebar-recent-sessions`. Hook:
//! `session-resumed` ({buffer, path, id, messages}).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::buffer::BufferId;
use crate::buffers::chat::ChatBuffer;
use crate::editor::{BUILTIN, Editor, buffer_id_to_u64};
use crate::minibuffer::Candidate;
use crate::session::{self, Info};

/// The session list, read in the background.
#[derive(Default)]
pub struct Sessions {
    pub list: Vec<Info>,
    cache: HashMap<PathBuf, Info>,
    pub scanned: Option<Instant>,
    scanning: bool,
    /// A rescan was asked for while one ran: another follows it.
    again: bool,
    /// Run once the scan in progress finishes.
    waiting: Vec<Box<dyn FnOnce(&mut Editor)>>,
}

/// Where new sessions are written.
pub fn session_root(ed: &Editor) -> PathBuf {
    match ed.var_str("session-directory") {
        Some(d) if !d.is_empty() => crate::expand_home(&d),
        _ => ed.registry.paths().agent_dir.join("sessions"),
    }
}

/// Every directory sessions are read from (the writing one first).
pub fn session_roots(ed: &Editor) -> Vec<PathBuf> {
    let mut out = vec![session_root(ed)];
    for d in ed.var("session-directories").and_then(Value::as_array).into_iter().flatten().filter_map(Value::as_str) {
        let p = crate::expand_home(d);
        let same = |a: &Path, b: &Path| a == b || a.canonicalize().ok().zip(b.canonicalize().ok()).is_some_and(|(x, y)| x == y);
        if !out.iter().any(|o| same(o, &p)) {
            out.push(p);
        }
    }
    out
}

/// Read the session list again in the background.
pub fn rescan(ed: &mut Editor) {
    scan_then(ed, true, |_| {});
}

/// Run `f` with a fresh session list: right away if it was read within
/// `session-scan-interval` seconds (and `force` is off), else after a background scan.
pub fn scan_then(ed: &mut Editor, force: bool, f: impl FnOnce(&mut Editor) + 'static) {
    let every = Duration::from_secs_f64(ed.var("session-scan-interval").and_then(Value::as_f64).unwrap_or(30.0));
    let fresh = ed.sessions.scanned.is_some_and(|t| t.elapsed() < every);
    if fresh && !force && !ed.sessions.scanning {
        f(ed);
        return;
    }
    ed.sessions.waiting.push(Box::new(f));
    if ed.sessions.scanning {
        // The scan under way may have started before what this is for: once more after it.
        ed.sessions.again |= force;
        return;
    }
    ed.sessions.scanning = true;
    let roots = session_roots(ed);
    let mut cache = std::mem::take(&mut ed.sessions.cache);
    let main = ed.main_sender();
    ed.rt.spawn_blocking(move || {
        // A bad file must not leave the scan pending forever.
        let scanned = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut list = session::list(&roots, &mut cache);
            // Each distinct directory is looked at once.
            let mut projects: HashMap<PathBuf, (Option<PathBuf>, bool)> = HashMap::new();
            for i in &mut list {
                let (project, exists) = projects
                    .entry(i.cwd.clone())
                    .or_insert_with(|| {
                        if !i.cwd.is_dir() {
                            return (None, false);
                        }
                        let project = crate::git::repo(&i.cwd).map(|r| r.main).or_else(|| crate::project::root(&i.cwd));
                        (project, true)
                    })
                    .clone();
                i.project = project;
                i.exists = exists;
            }
            (list, cache)
        }));
        let failed = scanned.is_err();
        let (list, cache) = scanned.unwrap_or_default();
        main.send(move |ed| {
            if failed {
                ed.error("Reading sessions failed (see the log)");
            }
            ed.sessions.list = list;
            ed.sessions.cache = cache;
            ed.sessions.scanned = Some(Instant::now());
            ed.sessions.scanning = false;
            for f in std::mem::take(&mut ed.sessions.waiting) {
                f(ed);
            }
            // An open session picker shows what the scan found.
            if let Some(f) = ed.remote.on_sessions.take() {
                f(ed);
                ed.remote.on_sessions.get_or_insert(f);
            }
            if std::mem::take(&mut ed.sessions.again) {
                rescan(ed);
            }
        });
    });
}

/// The chat showing session `path` (a file on this machine), if one does.
pub fn chat_with_session(ed: &Editor, path: &Path) -> Option<BufferId> {
    ed.buffers.iter().find_map(|(id, s)| {
        let c = s.buf.as_ref()?.as_any().downcast_ref::<ChatBuffer>()?;
        (c.node.local() && c.session_path()? == path).then_some(id)
    })
}

/// The session file a chat of this machine records to, once it has one.
pub fn session_of(ed: &Editor, id: BufferId) -> Option<PathBuf> {
    let c = ed.buffers.get(id)?.buf.as_ref()?.as_any().downcast_ref::<ChatBuffer>()?;
    c.node.local().then(|| c.session_path()).flatten()
}

/// Continue session `path` in a chat without showing it: the chat that already has it, else the
/// current chat when it is empty (`reuse`), else a new one. This machine's node continues it
/// (or is already running it, for another window).
pub fn restore(ed: &mut Editor, path: &Path, reuse: bool) -> anyhow::Result<BufferId> {
    if let Some(id) = chat_with_session(ed, path) {
        return Ok(id);
    }
    let loaded = session::load(path)?;
    let cur = ed.current_buffer();
    let empty = ed.buffers.get(cur).and_then(|s| s.buf.as_ref()).and_then(|b| b.as_any().downcast_ref::<ChatBuffer>()).is_some_and(|c| c.is_empty());
    let chat = if loaded.id.is_empty() { uuid::Uuid::now_v7().to_string() } else { loaded.id.clone() };
    let p = path.to_string_lossy().into_owned();
    let cwd = if loaded.cwd.is_dir() { loaded.cwd.clone() } else { ed.cwd.clone() };
    let id = if reuse && empty {
        let old = ed.with_buffer_as::<ChatBuffer, _>(cur, |c, _| (c.node.machine.clone(), c.chat_id()));
        if let Some((machine, old)) = old {
            crate::remote::send(ed, &machine, serpis_proto::wire::Request::Detach { chat: old, close: true });
        }
        ed.with_buffer_as::<ChatBuffer, _>(cur, |c, ed| {
            c.become_remote(ed, crate::remote::LOCAL, &chat);
            c.node.state.cwd = cwd.to_string_lossy().into_owned();
            c.node.state.session = Some(p.clone());
        });
        crate::remote::resume_into(ed, crate::remote::LOCAL, &p, &chat);
        cur
    } else {
        ed.create_buffer("chat", Some("*chat*"), &json!({"cwd": cwd.to_string_lossy(), "session": p, "chat": chat}))?
    };
    ed.with_buffer_as::<ChatBuffer, _>(id, |c, ed| {
        // Scripts see the restored chat (serpis.chat.*) at once.
        c.publish(ed, id);
    });
    ed.run_hook("session-resumed", &json!({"buffer": buffer_id_to_u64(id), "path": path.to_string_lossy(), "id": loaded.id, "messages": loaded.messages.len()}));
    Ok(id)
}

/// Resume session `path` and bring its chat up.
pub fn resume(ed: &mut Editor, path: &Path) -> anyhow::Result<BufferId> {
    let id = restore(ed, path, true)?;
    ed.open_buffer(id);
    Ok(id)
}

/// The project the current buffer belongs to.
fn current_project(ed: &mut Editor) -> Option<PathBuf> {
    let dir = ed.buffer_directory(ed.current_buffer()).unwrap_or_else(|| ed.cwd.clone());
    ed.project_root_of(&dir)
}

/// Whether session `i` belongs to project `root`.
fn in_project(i: &Info, root: &Path) -> bool {
    i.project.as_deref() == Some(root) || i.cwd.starts_with(root)
}

fn age(i: &Info) -> String {
    i.modified.and_then(|t| t.elapsed().ok()).map(crate::sidebar::fmt_age).unwrap_or_default()
}

/// Which tool wrote a session, judged by its directory.
fn source(ed: &Editor, i: &Info) -> &'static str {
    if i.path.starts_with(session_root(ed)) { "serpis" } else if i.path.to_string_lossy().contains("/.pi/") { "pi" } else { "other" }
}

/// Which sessions chat-resume lists; TAB cycles through the ones that apply.
#[derive(Debug, Clone, PartialEq)]
enum Scope {
    /// The current project, all its worktrees.
    Project(PathBuf),
    All,
    /// The current folder (a worktree or a subdirectory of the project).
    Folder(PathBuf),
    /// This machine's sessions and every other machine's (remote-hosts, rayfish).
    Everywhere,
}

impl Scope {
    fn label(&self) -> String {
        let name = |p: &Path| p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
        match self {
            Scope::Project(p) => format!("project {}", name(p)),
            Scope::All => "all projects".into(),
            Scope::Folder(p) => format!("folder {}", name(p)),
            Scope::Everywhere => "every machine".into(),
        }
    }
    /// Whether this machine's session `i` is listed (a session that moved away continues
    /// elsewhere: hidden unless chat-resume-show-moved).
    fn holds(&self, i: &Info) -> bool {
        match self {
            Scope::Project(root) => in_project(i, root),
            Scope::All | Scope::Everywhere => true,
            Scope::Folder(dir) => i.cwd.starts_with(dir),
        }
    }
}

/// The scopes to cycle through, in order: this project, all projects, this folder (when it
/// isn't the project itself).
fn scopes(ed: &mut Editor) -> Vec<Scope> {
    let dir = ed.local_directory(ed.current_buffer()).unwrap_or_else(|| ed.cwd.clone());
    let project = current_project(ed);
    let mut out = Vec::new();
    if let Some(p) = &project {
        out.push(Scope::Project(p.clone()));
    }
    out.push(Scope::All);
    if project.as_deref() != Some(dir.as_path()) && dir.is_dir() {
        out.push(Scope::Folder(dir));
    }
    if !crate::remote::listable(ed).is_empty() {
        out.push(Scope::Everywhere);
    }
    out
}

/// Whether this machine's node has session `path` open, and running, as it last said (it can
/// be another device's chat): (open, running).
fn node_state(ed: &Editor, path: &Path) -> (bool, bool) {
    ed.remote.sessions.get(crate::remote::LOCAL).and_then(|l| l.iter().find(|s| Path::new(&s.path) == path)).map_or((false, false), |s| (s.open, s.running))
}

/// The prompt and candidates for `scope` (with what TAB switches to).
fn listing(ed: &mut Editor, scopes: &[Scope], at: usize) -> (String, Vec<Candidate>) {
    let scope = &scopes[at];
    let show_moved = ed.var("chat-resume-show-moved").and_then(Value::as_bool).unwrap_or(false);
    let list: Vec<Info> = ed.sessions.list.iter().filter(|i| scope.holds(i) && (show_moved || i.moved_to.is_none())).cloned().collect();
    if *scope == Scope::Everywhere {
        return everywhere(ed, scopes, at, list);
    }
    let open: Vec<PathBuf> = ed.buffers.keys().collect::<Vec<_>>().into_iter().filter_map(|id| session_of(ed, id)).collect();
    let cands = list
        .iter()
        .map(|i| {
            let proj = i.project.as_deref().or(Some(i.cwd.as_path())).and_then(Path::file_name).map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            let mut title = i.title();
            if title.chars().count() > 70 {
                title = title.chars().take(69).collect::<String>() + "…";
            }
            let label = if *scope == Scope::All { format!("{title}  ·  {proj}") } else { title };
            let mut detail = format!("{} · {} msgs", age(i), i.messages);
            if let Some(m) = &i.model {
                detail.push_str(&format!(" · {}", m.rsplit('/').next().unwrap_or(m)));
            }
            detail.push_str(&format!(" · {}", source(ed, i)));
            match node_state(ed, &i.path) {
                (_, true) => detail.push_str(" · running"),
                (true, _) => detail.push_str(" · open"),
                _ if open.contains(&i.path) => detail.push_str(" · open"),
                _ => {}
            }
            Candidate::new(label).detail(detail).value(i.path.to_string_lossy().into_owned())
        })
        .collect::<Vec<_>>();
    let next = if scopes.len() > 1 { format!(" · TAB: {}", scopes[(at + 1) % scopes.len()].label()) } else { String::new() };
    let empty = if cands.is_empty() { " · none here" } else { "" };
    (format!("Resume ({}{empty}{next}): ", scope.label()), cands)
}

/// Every machine's sessions, newest first, each saying where it is (a picked one resumes there).
fn everywhere(ed: &mut Editor, scopes: &[Scope], at: usize, here: Vec<Info>) -> (String, Vec<Candidate>) {
    let show_moved = ed.var("chat-resume-show-moved").and_then(Value::as_bool).unwrap_or(false);
    let ms = |t: Option<std::time::SystemTime>| t.and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_millis() as u64).unwrap_or(0);
    // (modified, title, where, detail, value)
    let mark = |open: bool, running: bool| if running { " · running" } else if open { " · open" } else { "" };
    let mut rows: Vec<(u64, String, String, String, String)> = here
        .iter()
        .map(|i| {
            let (open, running) = node_state(ed, &i.path);
            (ms(i.modified), i.title(), "here".to_string(), format!("{} · {} msgs{}", age(i), i.messages, mark(open, running)), i.path.to_string_lossy().into_owned())
        })
        .collect();
    // This machine's own are listed from its files, above.
    for (m, list) in ed.remote.sessions.iter().filter(|(m, _)| *m != crate::remote::LOCAL) {
        for s in list.iter().filter(|s| show_moved || s.moved_to.is_none()) {
            let ago = crate::sidebar::fmt_age(std::time::Duration::from_millis(serpis_ai::now_ms().saturating_sub(s.modified)));
            rows.push((s.modified, s.title.clone(), format!("on {m}"), format!("{ago} · {} msgs{}", s.messages, mark(s.open, s.running)), format!("@{m}\u{1}{}", s.path)));
        }
    }
    rows.sort_by(|a, b| b.0.cmp(&a.0));
    let cands = rows
        .into_iter()
        .map(|(_, title, place, detail, value)| {
            let mut title = if title.is_empty() { "(untitled)".to_string() } else { title };
            if title.chars().count() > 70 {
                title = title.chars().take(69).collect::<String>() + "…";
            }
            Candidate::new(format!("{title}  ·  {place}")).detail(detail).value(value)
        })
        .collect::<Vec<_>>();
    let waiting = ed.remote.sessions_pending;
    let asking = if waiting > 0 { format!(" · asking {waiting} machine{}", if waiting == 1 { "" } else { "s" }) } else { String::new() };
    let next = if scopes.len() > 1 { format!(" · TAB: {}", scopes[(at + 1) % scopes.len()].label()) } else { String::new() };
    (format!("Resume (every machine{asking}{next}): "), cands)
}

fn pick(ed: &mut Editor, all: bool) {
    if ed.sessions.list.is_empty() && crate::remote::listable(ed).is_empty() {
        ed.message("No saved sessions yet");
        return;
    }
    let scopes = scopes(ed);
    let wanted = if all { "all".to_string() } else { ed.var_str("chat-resume-scope").unwrap_or_else(|| "project".into()) };
    let mut at = scopes
        .iter()
        .position(|s| matches!((s, wanted.as_str()), (Scope::All, "all") | (Scope::Project(_), "project") | (Scope::Folder(_), "folder") | (Scope::Everywhere, "everywhere")))
        .unwrap_or(0);
    // A scope with nothing in it opens on the next one that has something.
    for _ in 0..scopes.len() {
        if ed.sessions.list.iter().any(|i| scopes[at].holds(i)) {
            break;
        }
        at = (at + 1) % scopes.len();
    }
    // Other machines' sessions are asked for when that scope is shown, and join it as they come.
    let current = std::rc::Rc::new(std::cell::Cell::new(at));
    let show = {
        let (scopes, current) = (scopes.clone(), current.clone());
        std::rc::Rc::new(move |ed: &mut Editor, fetch: bool| {
            let at = current.get();
            if scopes[at] == Scope::Everywhere && fetch {
                crate::remote::fetch_sessions(ed);
            }
            let (prompt, cands) = listing(ed, &scopes, at);
            let loading = scopes[at] == Scope::Everywhere && ed.remote.sessions_pending > 0;
            if let Some(mb) = &mut ed.minibuffer {
                mb.replace_candidates(prompt, cands);
            }
            ed.minibuffer_loading(loading);
        })
    };
    let (prompt, cands) = listing(ed, &scopes, at);
    ed.completing_read(&prompt, cands, true, |ed, c, _| {
        ed.remote.on_sessions = None;
        if let Some(c) = c {
            if let Some((machine, path)) = c.value.strip_prefix('@').and_then(|v| v.split_once('\u{1}')) {
                return crate::remote::resume_on(ed, machine, path);
            }
            if let Err(e) = resume(ed, Path::new(&c.value)) {
                ed.error(format!("{e:#}"));
            }
        }
    });
    let s2 = show.clone();
    ed.remote.on_sessions = Some(Box::new(move |ed: &mut Editor| {
        if ed.minibuffer.as_ref().is_some_and(|m| m.prompt.starts_with("Resume (")) {
            s2(ed, false);
        }
    }));
    if scopes[at] == Scope::Everywhere {
        show(ed, true);
    } else {
        // Which of this machine's are running (here or for another device) shows as it answers.
        crate::remote::fetch_local_sessions(ed);
    }
    let cycle: crate::minibuffer::CycleFn = std::rc::Rc::new(move |ed: &mut Editor, dir: i32| {
        let n = scopes.len();
        current.set(((current.get() as i32 + dir).rem_euclid(n as i32)) as usize);
        show(ed, true);
    });
    if let Some(mb) = &mut ed.minibuffer {
        mb.keep_order = true;
        mb.cycle = Some(cycle);
    }
}

/// Remember which chats are open, for `session-restore-on-start`.
pub fn save_open(ed: &mut Editor) {
    let ids: Vec<BufferId> = ed.buffers.keys().collect();
    let open: Vec<String> = ids.into_iter().filter_map(|id| session_of(ed, id)).map(|p| p.to_string_lossy().into_owned()).collect();
    ed.persist_state("open-sessions", json!(open));
    ed.persist_state("open-remote-chats", crate::remote::open_chats(ed));
    ed.persist_state("open-terminals", crate::terminals::open_terminals(ed));
}

/// Reopen the chats that were open last time (in the background of the sidebar, unshown).
pub fn restore_open(ed: &mut Editor) {
    if !ed.var("session-restore-on-start").and_then(Value::as_bool).unwrap_or(true) {
        return;
    }
    let paths: Vec<String> = Editor::persisted_state("open-sessions").and_then(|v| serde_json::from_value(v).ok()).unwrap_or_default();
    for p in paths {
        let p = PathBuf::from(p);
        if p.exists() {
            if let Err(e) = restore(ed, &p, false) {
                ed.warn(format!("restoring {}: {e:#}", p.display()));
            }
        }
    }
    // Chats on other machines kept running there: show them again.
    if let Some(list) = Editor::persisted_state("open-remote-chats") {
        crate::remote::reopen(ed, &list);
    }
    // So did terminals that terminal nodes hold.
    if let Some(list) = Editor::persisted_state("open-terminals") {
        crate::terminals::reopen(ed, &list);
    }
}

pub fn install(ed: &mut Editor) {
    let o = BUILTIN;
    ed.defvar("session-save", json!(true), "Record chats to session files (pi's format) so they can be resumed.");
    ed.defvar("chat-keep-going", json!(true), "A chat whose machine's serpis-node stopped mid-turn (a crash, a reboot, a kill) continues the turn when the node starts again: from what was saved, a tool cut off mid-run not run again. Off, it waits for you. Each chat can choose for itself (the composer's ↻ pill, chat-toggle-keep-going). On another machine, its node's own settings say.");
    ed.defvar("chat-keep-going-limit", json!(2), "Times in a row a chat's turn is continued after its node stopped mid-turn; cut off once more, it is parked (waits for you), in case the turn is what stops the node.");
    ed.defvar("session-directory", Value::Null, "Where chats are recorded (default: <agent dir>/sessions, e.g. ~/.serpis/agent/sessions).");
    ed.defvar("session-directories", json!(["~/.pi/agent/sessions"]), "More session directories chat-resume reads (pi's, by default), besides session-directory.");
    ed.defvar("session-restore-on-start", json!(true), "Reopen the chats that were open when serpis quit, and show the terminals still running that were shown.");
    ed.defvar("session-scan-interval", json!(30.0), "Seconds a read session list stays fresh.");
    ed.defvar("project-remember", json!(true), "Pin a project to the sidebar (the projects variable) when a chat in it saves its first session.");
    ed.defvar("chat-resume-scope", json!("project"), "Sessions chat-resume starts with: \"project\" (this project, all its worktrees), \"all\", \"folder\" (the current folder) or \"everywhere\" (every machine's: remote-hosts and rayfish peers). TAB / S-TAB in the picker cycle through them.");
    ed.defvar("chat-resume-show-moved", json!(false), "List sessions whose chat moved to another machine (they continue there) in chat-resume.");
    ed.defvar("sidebar-recent-sessions", json!(3), "Closed sessions listed under each sidebar project (0 hides them).");

    ed.define_command("chat-resume", "Resume a saved chat session: this project's first (chat-resume-scope); TAB / S-TAB cycle to all projects and this folder. C-u or param \"all\" starts with all.", o, |ed, a| {
        let all = a.prefix.is_some() || a.str(0) == Some("all");
        if ed.sessions.scanned.is_none() {
            ed.message("Reading sessions…");
        }
        scan_then(ed, false, move |ed| pick(ed, all));
        Ok(())
    });
    ed.define_command("chat-continue", "Resume the most recent session of this project.", o, |ed, _| {
        scan_then(ed, true, |ed| {
            let project = current_project(ed);
            let latest = ed.sessions.list.iter().find(|i| project.as_deref().is_none_or(|r| in_project(i, r))).map(|i| i.path.clone());
            match latest {
                Some(p) => {
                    if let Err(e) = resume(ed, &p) {
                        ed.error(format!("{e:#}"));
                    }
                }
                None => ed.message("No session to continue in this project"),
            }
        });
        Ok(())
    });
    ed.define_command("session-open", "Resume the session file given as parameter.", o, |ed, a| {
        let p = a.str(0).ok_or_else(|| anyhow::anyhow!("session-open needs a path"))?;
        resume(ed, &crate::expand_home(p))?;
        Ok(())
    });
    ed.define_command("session-delete", "Delete a session file (param: path; default: the current chat's), after confirming.", o, |ed, a| {
        let path = match a.str(0) {
            Some(p) => crate::expand_home(p),
            None => session_of(ed, ed.current_buffer()).ok_or_else(|| anyhow::anyhow!("This chat has no session file"))?,
        };
        let name = session::info(&path).map(|i| i.title()).unwrap_or_else(|| path.display().to_string());
        ed.y_or_n(&format!("Delete session \"{name}\"?"), move |ed, yes| {
            if !yes {
                return;
            }
            // A chat showing it stops: it would write the file again.
            if let Some(id) = chat_with_session(ed, &path) {
                if let Some(chat) = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.chat_id()) {
                    crate::remote::send(ed, crate::remote::LOCAL, serpis_proto::wire::Request::Close { chat });
                }
                ed.kill_buffer(id);
            }
            match std::fs::remove_file(&path) {
                Ok(()) => {
                    ed.message(format!("Deleted {}", path.display()));
                    rescan(ed);
                }
                Err(e) => ed.error(format!("{}: {e}", path.display())),
            }
        });
        Ok(())
    });
    ed.define_command("session-rescan", "Read the session list again.", o, |ed, _| {
        rescan(ed);
        Ok(())
    });
    for (k, c) in [("C-c C-r", "chat-resume")] {
        ed.bind("chat", k, c, o);
    }
    ed.bind("global", "s-r", "chat-resume", o);
}
