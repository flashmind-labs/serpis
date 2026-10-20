//! What people see when picking things: buffers by their display names (a chat's title,
//! `Terminal 2`), and folders, from the projects serpis knows, a typed path or the system's
//! folder dialog.
//!
//! Commands that need a folder take it as a parameter when scripted; run interactively they
//! ask with [`Editor::read_directory`]: `project-open` (`s-O`), `C-u new-chat`,
//! `C-u new-terminal`, `project-add`.

use std::path::{Path, PathBuf};

use crate::buffer::BufferId;
use crate::editor::{BUILTIN, Editor, buffer_id_to_u64};
use crate::minibuffer::Candidate;

/// Candidate value standing for "open the system folder dialog".
const BROWSE: &str = "\u{1}browse";

/// `/Users/me/x` → `~/x`.
pub fn tilde(p: &Path) -> String {
    match dirs::home_dir().and_then(|h| p.strip_prefix(&h).ok().map(Path::to_path_buf)) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => p.display().to_string(),
    }
}

impl Editor {
    /// The name people see for a buffer: its title (a chat's first prompt, a title set with
    /// `buffer-set-title`, a terminal's title), else a readable form of its name (`Terminal 2`).
    pub fn buffer_display_name(&self, id: BufferId) -> String {
        let title = self.buffer_title(id);
        if !title.trim().is_empty() {
            return title;
        }
        self.meta(id).map(|m| crate::chrome::pretty_name(&m.name)).unwrap_or_default()
    }

    /// A picker entry for a buffer: its display name, and what tells it apart (kind, project,
    /// branch, what it is doing).
    pub fn buffer_candidate(&mut self, id: BufferId) -> Candidate {
        let label = self.buffer_display_name(id);
        let kind = self.meta(id).map(|m| crate::chrome::pretty_name(&format!("*{}*", m.major))).unwrap_or_default();
        let mut detail = vec![kind];
        if let Some(d) = self.buffer_directory(id) {
            match self.repo_of(&d) {
                Some(r) => {
                    detail.push(r.main.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default());
                    detail.push(r.head_label());
                }
                None => detail.push(tilde(&d)),
            }
        }
        let a = self.buffer_activity(id);
        if a.state != crate::ActivityState::Idle {
            detail.push(a.state.as_str().into());
        }
        Candidate::new(label).detail(detail.into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(" · ")).value(buffer_id_to_u64(id).to_string())
    }

    /// Folders worth offering: projects with open buffers, pinned ones, then those of recent
    /// sessions, then where serpis started.
    pub fn known_projects(&mut self) -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = Vec::new();
        let mut add = |p: PathBuf, check: bool| {
            if !out.contains(&p) && (!check || p.is_dir()) {
                out.push(p);
            }
        };
        for it in crate::sidebar::items(self) {
            if let Some(p) = it.project {
                add(p, false);
            }
        }
        for p in self.pinned_projects() {
            add(p, true);
        }
        // Session directories were checked by the background scan; never stat them here.
        for i in self.sessions.list.iter().filter(|i| i.exists) {
            add(i.project.clone().unwrap_or_else(|| i.cwd.clone()), false);
        }
        if let Some(root) = self.project_root_of(&self.cwd.clone()) {
            add(root, false);
        }
        add(self.cwd.clone(), false);
        out
    }

    /// Ask for a folder: a known project, a typed path (with completion), or "Browse…" for the
    /// system dialog. `f` gets the chosen directory.
    pub fn read_directory(&mut self, prompt: &str, f: impl FnOnce(&mut Editor, PathBuf) + 'static) {
        let known: Vec<Candidate> = self
            .known_projects()
            .into_iter()
            .map(|p| {
                let name = p.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| p.display().to_string());
                Candidate::new(name).detail(tilde(&p)).value(p.to_string_lossy().into_owned())
            })
            .collect();
        let source = move |input: &str| -> Vec<Candidate> {
            if !input.contains('/') {
                // Known projects first, then the folders in your home folder.
                let mut out = vec![Candidate::new("Browse…").detail("Choose a folder in the system dialog").value(BROWSE)];
                out.extend(known.iter().cloned());
                let home = dirs::home_dir().unwrap_or_default();
                out.extend(subfolders(&home, false).into_iter().filter(|c| !known.iter().any(|k| k.value == c.value)).map(|c| {
                    let detail = tilde(Path::new(&c.value));
                    c.detail(detail)
                }));
                return out;
            }
            // Folders under what has been typed (relative paths count from home); the folder
            // itself first.
            let input = &home_relative(input);
            let path = crate::expand_home(input);
            let (dir, _) = match input.rfind('/') {
                Some(i) => (crate::expand_home(&input[..=i]), &input[i + 1..]),
                None => (path.clone(), ""),
            };
            let mut out = Vec::new();
            if dir.is_dir() {
                out.push(Candidate::new("Use this folder").detail(tilde(&dir)).value(dir.to_string_lossy().into_owned()));
            }
            out.extend(subfolders(&dir, input.rsplit('/').next().is_some_and(|q| q.starts_with('.'))));
            out
        };
        self.completing_read_dynamic(prompt, "", source, move |ed, c, input| {
            let choice = c.map(|c| c.value).unwrap_or(input);
            if choice == BROWSE {
                browse_folder(ed, f);
                return;
            }
            let dir = crate::expand_home(&home_relative(choice.trim()));
            if dir.is_dir() {
                f(ed, dir.canonicalize().unwrap_or(dir));
            } else {
                ed.error(format!("Not a folder: {}", tilde(&dir)));
            }
        });
        if let Some(mb) = &mut self.minibuffer {
            mb.prefer_input = false;
            mb.keep_order = true;
        }
    }
}

/// A typed path as the picker reads it: `/`, `~` and `.` paths as they are, anything else
/// relative to the home folder.
fn home_relative(input: &str) -> String {
    if input.starts_with('/') || input.starts_with('~') || input.starts_with('.') || input.is_empty() {
        input.to_string()
    } else {
        format!("~/{input}")
    }
}

/// The folders in `dir` as candidates (`name/`, valued by full path), hidden ones on request.
fn subfolders(dir: &Path, hidden: bool) -> Vec<Candidate> {
    let mut subs: Vec<Candidate> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| hidden || !n.starts_with('.'))
        .map(|n| Candidate::new(format!("{n}/")).value(dir.join(&n).to_string_lossy().into_owned()))
        .collect();
    subs.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
    subs
}

/// The system's folder chooser (Finder on macOS, zenity or kdialog elsewhere), in the
/// background; `f` runs with the choice.
pub fn browse_folder(ed: &mut Editor, f: impl FnOnce(&mut Editor, PathBuf) + 'static) {
    let start = ed.new_buffer_directory();
    let main = ed.main_sender();
    // The callback stays on the UI thread; the dialog's answer travels to it.
    let slot = std::rc::Rc::new(std::cell::RefCell::new(Some(Box::new(f) as Box<dyn FnOnce(&mut Editor, PathBuf)>)));
    let key = PENDING.with(|p| {
        let mut p = p.borrow_mut();
        p.1 += 1;
        let key = p.1;
        p.0.insert(key, slot);
        key
    });
    ed.rt.spawn_blocking(move || {
        let chosen = run_dialog(&start);
        main.send(move |ed| {
            let Some(slot) = PENDING.with(|p| p.borrow_mut().0.remove(&key)) else { return };
            let Some(f) = slot.borrow_mut().take() else { return };
            match chosen {
                Ok(Some(dir)) => f(ed, dir),
                Ok(None) => ed.message("Cancelled"),
                Err(e) => ed.error(format!("Folder dialog: {e}")),
            }
        });
    });
}

type Pending = std::rc::Rc<std::cell::RefCell<Option<Box<dyn FnOnce(&mut Editor, PathBuf)>>>>;

thread_local! {
    static PENDING: std::cell::RefCell<(std::collections::HashMap<u64, Pending>, u64)> = std::cell::RefCell::new((Default::default(), 0));
}

fn run_dialog(start: &Path) -> Result<Option<PathBuf>, String> {
    use std::process::Command;
    let output = if cfg!(target_os = "macos") {
        let script = format!(
            "POSIX path of (choose folder with prompt \"Choose a project folder\" default location (POSIX file \"{}\"))",
            start.display().to_string().replace('"', "\\\"")
        );
        Command::new("osascript").args(["-e", &script]).output()
    } else {
        Command::new("zenity")
            .args(["--file-selection", "--directory", "--title=Choose a project folder", &format!("--filename={}/", start.display())])
            .output()
            .or_else(|_| Command::new("kdialog").args(["--getexistingdirectory", &start.display().to_string()]).output())
    }
    .map_err(|e| e.to_string())?;
    if !output.status.success() {
        // Cancelling the dialog exits non-zero.
        return Ok(None);
    }
    let path = String::from_utf8_lossy(&output.stdout).trim().trim_end_matches('/').to_string();
    Ok((!path.is_empty()).then(|| PathBuf::from(path)))
}

/// Open a folder as a project: pin it and start a chat there.
pub fn open_project(ed: &mut Editor, dir: PathBuf) {
    crate::sidebar::add_project(ed, &dir);
    match ed.new_buffer_in("chat", &dir) {
        Ok(_) => ed.message(format!("Opened {}", tilde(&dir))),
        Err(e) => ed.error(format!("{e:#}")),
    }
}

pub fn install(ed: &mut Editor) {
    let o = BUILTIN;
    ed.define_command("project-open", "Open a folder as a project and start a chat in it. Interactively, pick a known project, type a path, or Browse… (param: directory).", o, |ed, a| {
        if let Some(d) = a.str(0) {
            open_project(ed, crate::expand_home(d));
            return Ok(());
        }
        // Open at once with the projects known now; session history refreshes in the background.
        ed.read_directory("Open project: ", open_project);
        crate::session_ui::scan_then(ed, false, |_| {});
        Ok(())
    });
    ed.define_command("browse-folder", "Choose a folder in the system dialog and start a chat in it.", o, |ed, _| {
        browse_folder(ed, open_project);
        Ok(())
    });
    for kind in ["chat", "terminal"] {
        ed.define_command(&format!("new-{kind}-in-folder"), &format!("Open a new {kind} in a folder you pick (a known project, a typed path, or Browse…)."), o, move |ed, _| {
            ed.read_directory(&format!("New {kind} in: "), move |ed, dir| {
                if let Err(e) = ed.new_buffer_in(kind, &dir) {
                    ed.error(format!("{e:#}"));
                }
            });
            crate::session_ui::scan_then(ed, false, |_| {});
            Ok(())
        });
    }
    ed.bind("global", "s-O", "project-open", o);
}

/// Run `then` with the folder from parameter 0, else (with a prefix argument) one asked for,
/// else the default (`new-buffer-directory`).
pub fn with_directory(ed: &mut Editor, a: &crate::editor::Args, prompt: &str, then: impl FnOnce(&mut Editor, PathBuf) + 'static) {
    match a.str(0) {
        Some(d) => then(ed, crate::expand_home(d)),
        None if a.prefix.is_some() => ed.read_directory(prompt, then),
        None => {
            let d = ed.new_buffer_directory();
            then(ed, d)
        }
    }
}
