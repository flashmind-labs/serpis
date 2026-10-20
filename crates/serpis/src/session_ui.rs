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
            let mut projects: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
            for i in &mut list {
                i.project = projects
                    .entry(i.cwd.clone())
                    .or_insert_with(|| crate::git::repo(&i.cwd).map(|r| r.main).or_else(|| crate::project::root(&i.cwd)))
                    .clone();
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
        });
    });
}

/// The chat showing session `path`, if one does.
pub fn chat_with_session(ed: &Editor, path: &Path) -> Option<BufferId> {
    ed.buffers.iter().find_map(|(id, s)| {
        let c = s.buf.as_ref()?.as_any().downcast_ref::<ChatBuffer>()?;
        (c.session.as_ref()?.path == path).then_some(id)
    })
}

/// The session file a chat records to, once it has one.
pub fn session_of(ed: &Editor, id: BufferId) -> Option<PathBuf> {
    let c = ed.buffers.get(id)?.buf.as_ref()?.as_any().downcast_ref::<ChatBuffer>()?;
    c.session.as_ref().filter(|w| w.exists()).map(|w| w.path.clone())
}

/// Load session `path` into a chat without showing it: the chat that already has it, else the
/// current chat when it is empty (`reuse`), else a new one.
pub fn restore(ed: &mut Editor, path: &Path, reuse: bool) -> anyhow::Result<BufferId> {
    if let Some(id) = chat_with_session(ed, path) {
        return Ok(id);
    }
    let loaded = session::load(path)?;
    let cur = ed.current_buffer();
    let empty = ed.buffers.get(cur).and_then(|s| s.buf.as_ref()).and_then(|b| b.as_any().downcast_ref::<ChatBuffer>()).is_some_and(|c| c.history.is_empty() && !c.running());
    let id = if reuse && empty {
        cur
    } else {
        let cwd = if loaded.cwd.is_dir() { loaded.cwd.clone() } else { ed.cwd.clone() };
        ed.create_buffer("chat", Some("*chat*"), &json!({"cwd": cwd.to_string_lossy()}))?
    };
    ed.with_buffer_as::<ChatBuffer, _>(id, |c, ed| c.restore(ed, &loaded));
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

fn pick(ed: &mut Editor, all: bool) {
    let project = current_project(ed);
    let mut list: Vec<Info> = ed.sessions.list.clone();
    let mut scope = "all projects".to_string();
    if let (Some(root), false) = (&project, all) {
        let mine: Vec<Info> = list.iter().filter(|i| in_project(i, root)).cloned().collect();
        if mine.is_empty() {
            ed.message(format!("No sessions in {}; showing all", root.display()));
        } else {
            list = mine;
            scope = root.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
        }
    }
    if list.is_empty() {
        ed.message("No saved sessions yet");
        return;
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
            let label = if all || project.is_none() { format!("{title}  ·  {proj}") } else { title };
            let mut detail = format!("{} · {} msgs", age(i), i.messages);
            if let Some(m) = &i.model {
                detail.push_str(&format!(" · {}", m.rsplit('/').next().unwrap_or(m)));
            }
            detail.push_str(&format!(" · {}", source(ed, i)));
            if open.contains(&i.path) {
                detail.push_str(" · open");
            }
            Candidate::new(label).detail(detail).value(i.path.to_string_lossy().into_owned())
        })
        .collect();
    ed.completing_read(&format!("Resume ({scope}): "), cands, true, |ed, c, _| {
        if let Some(c) = c {
            if let Err(e) = resume(ed, Path::new(&c.value)) {
                ed.error(format!("{e:#}"));
            }
        }
    });
    if let Some(mb) = &mut ed.minibuffer {
        mb.keep_order = true;
    }
}

/// Remember which chats are open, for `session-restore-on-start`.
pub fn save_open(ed: &mut Editor) {
    let ids: Vec<BufferId> = ed.buffers.keys().collect();
    let open: Vec<String> = ids.into_iter().filter_map(|id| session_of(ed, id)).map(|p| p.to_string_lossy().into_owned()).collect();
    ed.persist_state("open-sessions", json!(open));
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
}

pub fn install(ed: &mut Editor) {
    let o = BUILTIN;
    ed.defvar("session-save", json!(true), "Record chats to session files (pi's format) so they can be resumed.");
    ed.defvar("session-directory", Value::Null, "Where chats are recorded (default: <agent dir>/sessions, e.g. ~/.serpis/agent/sessions).");
    ed.defvar("session-directories", json!(["~/.pi/agent/sessions"]), "More session directories chat-resume reads (pi's, by default), besides session-directory.");
    ed.defvar("session-restore-on-start", json!(true), "Reopen the chats that were open when serpis quit.");
    ed.defvar("session-scan-interval", json!(30.0), "Seconds a read session list stays fresh.");
    ed.defvar("project-remember", json!(true), "Pin a project to the sidebar (the projects variable) when a chat in it saves its first session.");
    ed.defvar("sidebar-recent-sessions", json!(3), "Closed sessions listed under each sidebar project (0 hides them).");

    ed.define_command("chat-resume", "Resume a saved chat session of this project (all projects with C-u or param \"all\").", o, |ed, a| {
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
            if let Some(id) = chat_with_session(ed, &path) {
                ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.session = None);
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
