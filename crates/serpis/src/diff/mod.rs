//! The diff viewer: any two states of a repository compared token by token (`engine`), read from
//! whichever machine it is on (`serpis_node::gitdiff`), shown side by side with review comments
//! between the lines (`view`), and sent to an agent to fix or answer (`review`).
//!
//! `diff-view` opens it for the current buffer's folder; everything it does is a command, a
//! setting, or `serpis.diff.*` in Lua.

pub mod engine;
pub mod review;
pub mod view;

use serde_json::{Value, json};
use serpis_proto::wire::DiffSpec;

use crate::buffer::BufferId;
use crate::editor::{BUILTIN, Editor};
use crate::minibuffer::Candidate;
use engine::Whitespace;
use review::{Ask, Framing, State, Which};
use view::{DiffBuffer, EDITING, KIND};

/// Open the diff page for `dir` (on `machine`): the page it already has, or a new one.
pub fn open(ed: &mut Editor, machine: Option<String>, dir: String, spec: Option<DiffSpec>, target: Option<BufferId>) -> BufferId {
    // One page per folder and machine.
    let found = ed.buffers.iter().find(|(_, s)| s.buf.as_ref().and_then(|b| b.as_any().downcast_ref::<DiffBuffer>()).is_some_and(|b| b.machine == machine && (b.dir == dir || b.root == dir))).map(|(id, _)| id);
    let id = match found {
        Some(id) => {
            ed.with_buffer_as::<DiffBuffer, _>(id, |b, ed| {
                if target.is_some() {
                    b.target = target;
                }
                match spec {
                    Some(s) => b.set_spec(ed, s),
                    None => b.refresh(ed),
                }
            });
            id
        }
        None => {
            let name = dir.trim_end_matches('/').rsplit('/').next().unwrap_or("diff");
            let b = DiffBuffer::new(ed, machine, dir.clone(), spec, target);
            let id = ed.add_buffer(&format!("*diff {name}*"), Box::new(b));
            ed.with_buffer_as::<DiffBuffer, _>(id, |b, ed| {
                b.id = Some(id);
                b.start(ed);
            });
            id
        }
    };
    if ed.var_str("diff-open-in").as_deref() == Some("split") {
        ed.pop_to_buffer(id);
    } else {
        ed.show_buffer(id);
    }
    id
}

/// The page to act on: `id`, else the current buffer.
pub fn page(ed: &Editor, id: Option<BufferId>) -> anyhow::Result<BufferId> {
    let id = id.unwrap_or_else(|| ed.current_buffer());
    let is_it = ed.buffers.get(id).and_then(|s| s.buf.as_ref()).is_some_and(|b| b.as_any().is::<DiffBuffer>());
    // While it is drawn or running a command it is taken out of its slot: its mode still says.
    let busy = ed.meta(id).is_some_and(|m| m.major == KIND || m.major == EDITING);
    if is_it || busy {
        Ok(id)
    } else {
        anyhow::bail!("Not in a diff page (diff-view)")
    }
}

fn with_page<R>(ed: &mut Editor, f: impl FnOnce(&mut DiffBuffer, &mut Editor) -> R) -> anyhow::Result<R> {
    let id = page(ed, None)?;
    ed.with_buffer_as::<DiffBuffer, _>(id, f).ok_or_else(|| anyhow::anyhow!("Not in a diff page (diff-view)"))
}

/// What a page should compare, from a command's or script's text.
fn spec_from(s: &str) -> anyhow::Result<DiffSpec> {
    view::parse_source(s).ok_or_else(|| anyhow::anyhow!("{s:?} isn't something to compare"))
}

/// Ask what to compare.
fn select_source(ed: &mut Editor) -> anyhow::Result<()> {
    let id = page(ed, None)?;
    let Some((refs, spec)) = ed.with_buffer_as::<DiffBuffer, _>(id, |b, _| (b.refs.clone(), b.spec.clone())) else { return Ok(()) };
    let refs = refs.unwrap_or_default();
    let now = spec.as_ref().map(view::describe);
    let note = |name: &str, doc: &str| if now.as_deref() == Some(name) { format!("showing · {doc}") } else { doc.to_string() };
    let mut list = vec![
        Candidate::new("Uncommitted changes").detail(note("Uncommitted changes", "everything not committed, untracked files too")).value("uncommitted"),
        Candidate::new("Staged changes").detail(note("Staged changes", "what the next commit would contain")).value("staged"),
        Candidate::new("Unstaged changes").detail(note("Unstaged changes", "edits not staged yet")).value("unstaged"),
        Candidate::new("Last commit").detail("HEAD against its parent").value("commit:HEAD"),
    ];
    for b in refs.branches.iter().filter(|b| !b.current) {
        list.push(Candidate::new(format!("vs {}", b.name)).detail(format!("from where they forked, uncommitted work too · {}", b.subject)).value(b.name.clone()));
    }
    for (path, branch) in refs.worktrees.iter().filter(|(p, _)| *p != refs.root) {
        list.push(Candidate::new(format!("Worktree {branch}")).detail(path.clone()).value(format!("worktree:{path}")));
    }
    ed.completing_read("Compare (or type a..b, a...b, commit:rev, a branch): ", list, false, move |ed, c, input| {
        let choice = c.map(|c| c.value).unwrap_or(input);
        if let Some(path) = choice.strip_prefix("worktree:") {
            let machine = ed.with_buffer_as::<DiffBuffer, _>(id, |b, _| b.machine.clone()).flatten();
            open(ed, machine, path.to_string(), None, None);
            return;
        }
        match spec_from(&choice) {
            Ok(s) => {
                ed.with_buffer_as::<DiffBuffer, _>(id, |b, ed| b.set_spec(ed, s));
            }
            Err(e) => ed.error(format!("{e:#}")),
        }
    });
    Ok(())
}

/// Chats and agents' terminals a review could go to: those of this machine and worktree first.
fn targets(ed: &mut Editor, root: &str, machine: &Option<String>) -> Vec<(BufferId, String, String)> {
    let mut out: Vec<(bool, BufferId, String, String)> = Vec::new();
    for id in ed.listed_buffers() {
        let Some(major) = ed.meta(id).map(|m| m.major.clone()) else { continue };
        if major != "chat" && major != "terminal" {
            continue;
        }
        if ed.buffer_machine(id) != *machine {
            continue;
        }
        let Some((title, program)) = ed.with_buffer(id, |b, _| (b.title(), b.activity().program)) else { continue };
        if major == "terminal" && !program.as_deref().is_some_and(|p| crate::sidebar::is_agent_program(ed, p)) {
            continue;
        }
        let dir = ed.buffer_directory(id).map(|d| d.to_string_lossy().into_owned()).unwrap_or_default();
        let here = dir == root || dir.starts_with(&format!("{root}/"));
        out.push((here, id, if title.is_empty() { major.clone() } else { title }, format!("{major} · {dir}")));
    }
    out.sort_by_key(|x| !x.0);
    out.into_iter().map(|(_, id, t, d)| (id, t, d)).collect()
}

/// The value meaning "a new chat" among a review's targets.
const NEW_CHAT: &str = "new";

/// Ask which chat or terminal a review goes to, then `then`.
fn select_target(ed: &mut Editor, id: BufferId, then: impl FnOnce(&mut Editor) + 'static) {
    let Some((root, machine)) = ed.with_buffer_as::<DiffBuffer, _>(id, |b, _| (b.root.clone(), b.machine.clone())) else { return };
    let mut list = Vec::new();
    for (buf, title, detail) in targets(ed, &root, &machine) {
        list.push(Candidate::new(title).detail(detail).value(crate::editor::buffer_id_to_u64(buf).to_string()));
    }
    list.push(Candidate::new("New chat here").detail(root).value(NEW_CHAT));
    ed.completing_read("Send the review to: ", list, true, move |ed, c, _| {
        let Some(c) = c else { return };
        let target = choose_target(ed, id, &c.value);
        ed.with_buffer_as::<DiffBuffer, _>(id, |b, _| b.target = target);
        then(ed);
    });
}

/// A target from a picked value: a buffer's id or [`NEW_CHAT`], which starts a chat.
fn choose_target(ed: &mut Editor, id: BufferId, value: &str) -> Option<BufferId> {
    if value == NEW_CHAT {
        let (root, machine) = ed.with_buffer_as::<DiffBuffer, _>(id, |b, _| (b.root.clone(), b.machine.clone()))?;
        let m = machine.unwrap_or_else(|| crate::remote::LOCAL.to_string());
        let chat = crate::buffers::chat::ChatBuffer::new(ed, &json!({"machine": m, "cwd": root}));
        return Some(ed.add_buffer("*chat*", Box::new(chat)));
    }
    value.parse::<u64>().ok().map(crate::editor::buffer_id_from_u64).filter(|b| ed.buffers.contains_key(*b))
}

/// Send comments to the page's target, asking for one if it has none. `only`: just this comment;
/// otherwise every draft.
pub fn send(ed: &mut Editor, id: BufferId, only: Option<u64>) -> anyhow::Result<()> {
    let Some((target, root, compared, comments)) = ed.with_buffer_as::<DiffBuffer, _>(id, |b, _| {
        let comments: Vec<review::Comment> = b.drafts().into_iter().filter(|c| only.is_none_or(|o| c.id == o)).cloned().collect();
        (b.target.filter(|t| *t != id), b.root.clone(), b.spec.as_ref().map(view::describe).unwrap_or_default(), comments)
    }) else {
        anyhow::bail!("Not in a diff page (diff-view)")
    };
    if comments.is_empty() {
        ed.message("No comments to send: c comments on the line at the cursor");
        return Ok(());
    }
    let target = target.filter(|t| ed.buffers.contains_key(*t));
    let Some(target) = target else {
        select_target(ed, id, move |ed| {
            if ed.with_buffer_as::<DiffBuffer, _>(id, |b, _| b.target).flatten().is_some()
                && let Err(e) = send(ed, id, only)
            {
                ed.error(format!("{e:#}"));
            }
        });
        return Ok(());
    };
    let framing = Framing {
        root,
        compared: compared.clone(),
        fix: ed.var_str("diff-review-fix-instructions").unwrap_or_default(),
        question: ed.var_str("diff-review-question-instructions").unwrap_or_default(),
        quote_lines: ed.var("diff-review-quote-lines").and_then(Value::as_u64).unwrap_or(20) as usize,
    };
    let refs: Vec<&review::Comment> = comments.iter().collect();
    let mut text = review::prompt(&refs, &framing);
    // A handler can rewrite the message, or refuse it by returning false.
    let payload = json!({"text": text, "comments": comments.iter().map(review::to_json).collect::<Vec<_>>(), "target": crate::editor::buffer_id_to_u64(target), "compared": compared});
    for r in ed.run_hook("diff-review-send", &payload) {
        match r {
            Value::String(s) => text = s,
            Value::Bool(false) => return Ok(()),
            _ => {}
        }
    }
    let major = ed.meta(target).map(|m| m.major.clone()).unwrap_or_default();
    if major == "chat" {
        let queue = ed.var_str("diff-review-while-running").as_deref() != Some("steer");
        ed.with_chat_soon(target, move |c, e| {
            let follow_up = queue && c.running();
            c.submit(e, text, follow_up);
        });
    } else {
        // A terminal: pasted, for the program there to take when its user says so.
        let paste = text.clone();
        ed.with_buffer(target, |b, ed| b.handle_paste(&paste, ed));
        if ed.var_bool("diff-review-terminal-submit") {
            ed.with_buffer(target, |b, ed| b.handle_paste("\r", ed));
        }
    }
    let ids: Vec<u64> = comments.iter().map(|c| c.id).collect();
    ed.with_buffer_as::<DiffBuffer, _>(id, |b, ed| {
        for c in b.review.comments.iter_mut().filter(|c| ids.contains(&c.id)) {
            c.state = State::Sent;
        }
        b.persist(ed);
    });
    ed.run_hook("diff-review-sent", &json!({"comments": ids, "target": crate::editor::buffer_id_to_u64(target)}));
    // Beside the page, so the answer and the code can be read together; the keys stay here
    // unless asked otherwise.
    let was = ed.focused;
    ed.pop_to_buffer(target);
    if ed.var_str("diff-review-focus").as_deref() != Some("agent") {
        ed.focus_window(was);
    }
    ed.message(format!("Sent {} {}", ids.len(), if ids.len() == 1 { "comment" } else { "comments" }));
    Ok(())
}

pub fn install(ed: &mut Editor) {
    let o = BUILTIN;
    ed.defvar("diff-view-style", json!("split"), "How the diff page shows a file: \"split\" (side by side, as long as it fits: diff-split-min-width) or \"unified\".");
    ed.defvar("diff-split-min-width", json!(600), "Below this width (in points, before UI scale) the diff page shows the unified view instead of side by side.");
    ed.defvar("diff-whitespace", json!("smart"), "What spacing means on the diff page. \"smart\": spacing and line breaks between tokens aren't changes; those lines show dimmed and n and p skip them (inside strings, and for indentation where it is syntax, spacing still counts). \"hide\": as smart, but those lines look unchanged. \"exact\": every byte counts.");
    ed.defvar("diff-wrap", json!(true), "Wrap long lines on the diff page; off, they scroll sideways.");
    ed.defvar("diff-context-lines", json!(3), "Unchanged lines shown around each change on the diff page; the rest fold.");
    ed.defvar("diff-move-min-lines", json!(3), "Blocks of at least this many lines cut in one place and pasted in another show as moved on the diff page; 0: never.");
    ed.defvar("diff-file-tree", json!(true), "Show the list of files on the diff page, when there is room.");
    ed.defvar("diff-tree-width", json!(250), "Width of the diff page's file list, in points.");
    ed.defvar("diff-refresh-seconds", json!(2), "How often a diff page on screen checks the repository for changes, so what an agent writes shows up; 0: only when asked (diff-refresh).");
    ed.defvar("diff-default-source", json!("auto"), "What a new diff page compares. \"auto\": on a branch of its own, what the branch would bring (against diff-base-branch); on the main branch, uncommitted changes. Or any of \"uncommitted\", \"staged\", \"unstaged\", \"a..b\", \"a...b\", \"commit:rev\", or a branch.");
    ed.defvar("diff-base-branch", Value::Null, "The branch \"vs\" compares with on the diff page. Unset: origin's default branch, else main or master.");
    ed.defvar("diff-open-in", json!("current"), "Where diff-view opens the page: \"current\" (the focused window) or \"split\" (beside it).");
    ed.defvar("diff-review-fix-instructions", json!("Make these changes. When one is unclear or wrong, say so instead of guessing."), "What a review sent to an agent says about comments asking for a change.");
    ed.defvar("diff-review-question-instructions", json!("Answer these. Don't change code for them."), "What a review sent to an agent says about questions.");
    ed.defvar("diff-review-quote-lines", json!(20), "Lines of code a review quotes per comment when sent to an agent; 0: only their file and line.");
    ed.defvar("diff-review-while-running", json!("follow-up"), "A review sent to a chat while its agent is working: \"follow-up\" (it runs when the agent finishes) or \"steer\" (it interrupts).");
    ed.defvar("diff-review-terminal-submit", json!(false), "A review sent to an agent in a terminal is pasted at its prompt; on, Return is sent too.");
    ed.defvar("diff-review-focus", json!("diff"), "Where the keys go after sending a review: \"diff\" (stay on the page, the agent beside it) or \"agent\".");

    ed.define_command("diff-view", "Compare the current folder's repository: uncommitted changes, branches, commits (params: what to compare, as in diff-select-source; a folder).", o, |ed, a| {
        let id = ed.current_buffer();
        let target = ed.meta(id).is_some_and(|m| m.major == "chat" || m.major == "terminal").then_some(id);
        let machine = ed.buffer_machine(id);
        let dir = a.str(1).map(str::to_string).or_else(|| ed.buffer_directory(id).map(|d| d.to_string_lossy().into_owned())).unwrap_or_else(|| ed.cwd.to_string_lossy().into_owned());
        let spec = a.str(0).filter(|s| !s.is_empty()).map(spec_from).transpose()?;
        open(ed, machine, dir, spec, target);
        Ok(())
    });
    // From the sidebar: its menus pass a project or worktree's folder as the last argument.
    ed.define_command("diff-view-folder", "Compare the repository of a folder, as a new diff page starts (param: the folder).", o, |ed, a| {
        let dir = a.str(0).map(str::to_string).ok_or_else(|| anyhow::anyhow!("Which folder?"))?;
        // A folder on another machine is known from what runs in it there.
        let of = ed.listed_buffers().into_iter().find(|b| ed.buffer_directory(*b).is_some_and(|d| d.starts_with(&dir)));
        let machine = of.and_then(|b| ed.buffer_machine(b));
        open(ed, machine, dir, None, None);
        Ok(())
    });
    ed.define_command("diff-select-source", "Choose what the diff page compares (param: \"uncommitted\", \"staged\", \"unstaged\", \"a..b\", \"a...b\", \"commit:rev\", or a branch).", o, |ed, a| {
        if let Some(s) = a.str(0) {
            let s = spec_from(s)?;
            return with_page(ed, |b, ed| b.set_spec(ed, s));
        }
        select_source(ed)
    });
    ed.define_command("diff-refresh", "Read the repository again now: its branches too, and whatever a file looks like.", o, |ed, _| with_page(ed, |b, ed| b.start(ed)));
    ed.define_command("diff-next-change", "Go to the next change, on into the next file.", o, |ed, _| {
        let m = with_page(ed, |b, ed| b.step_change(ed, 1))?;
        m.into_iter().for_each(|m| ed.message(m));
        Ok(())
    });
    ed.define_command("diff-previous-change", "Go to the previous change, back into the previous file.", o, |ed, _| {
        let m = with_page(ed, |b, ed| b.step_change(ed, -1))?;
        m.into_iter().for_each(|m| ed.message(m));
        Ok(())
    });
    ed.define_command("diff-next-file", "Show the next file.", o, |ed, _| with_page(ed, |b, ed| b.step_file(ed, 1, false)).map(|_| ()));
    ed.define_command("diff-previous-file", "Show the previous file.", o, |ed, _| with_page(ed, |b, ed| b.step_file(ed, -1, false)).map(|_| ()));
    ed.define_command("diff-select-file", "Show a file (param: its path; asks without one).", o, |ed, a| {
        if let Some(p) = a.str(0).map(str::to_string) {
            return with_page(ed, |b, ed| b.select_file(ed, &p));
        }
        let id = page(ed, None)?;
        let files = ed.with_buffer_as::<DiffBuffer, _>(id, |b, _| b.files.clone()).unwrap_or_default();
        let list = files.into_iter().map(|f| Candidate::new(f.path.clone()).detail(format!("{} +{} −{}", f.status, f.added, f.removed)).value(f.path)).collect();
        ed.completing_read("File: ", list, true, move |ed, c, _| {
            if let Some(c) = c {
                ed.with_buffer_as::<DiffBuffer, _>(id, |b, ed| b.select_file(ed, &c.value));
            }
        });
        Ok(())
    });
    ed.define_command("diff-mark-viewed", "Mark the file as viewed and go to the next (or unmark it). Changing the file clears it.", o, |ed, _| {
        with_page(ed, |b, ed| {
            if b.toggle_viewed(ed) {
                b.step_file(ed, 1, false);
            }
        })
    });
    ed.define_command("diff-toggle-view", "Side by side or unified.", o, |ed, _| {
        with_page(ed, |b, _| {
            let s = !b.split;
            b.set_split(s);
        })
    });
    ed.define_command("diff-cycle-whitespace", "What spacing means: smart, hide, exact (param: one of them).", o, |ed, a| {
        let set = a.str(0).map(str::to_string);
        with_page(ed, |b, ed| {
            let w = set.as_deref().and_then(Whitespace::parse).unwrap_or(match b.whitespace {
                Whitespace::Smart => Whitespace::Hide,
                Whitespace::Hide => Whitespace::Exact,
                Whitespace::Exact => Whitespace::Smart,
            });
            b.set_whitespace(ed, w);
            ed.message(format!("Spacing: {}", w.name()));
        })
    });
    ed.define_command("diff-toggle-wrap", "Wrap long lines, or scroll them sideways.", o, |ed, _| {
        with_page(ed, |b, _| {
            b.wrap = !b.wrap;
            b.set_split(b.split);
        })
    });
    ed.define_command("diff-toggle-tree", "Show or hide the list of files.", o, |ed, _| {
        with_page(ed, |b, _| b.show_tree = !b.show_tree)
    });
    ed.define_command("diff-toggle-expand", "Show every unchanged line, or fold them again.", o, |ed, _| with_page(ed, |b, _| b.toggle_expand_all()));
    ed.define_command("diff-toggle-fold", "Open the folded lines at the cursor, else fold everything back.", o, |ed, _| {
        with_page(ed, |b, _| {
            if !b.open_fold_at_cursor() {
                b.toggle_expand_all();
            }
        })
    });
    for (name, doc, delta, extend) in [
        ("diff-next-line", "Move down a line.", 1isize, false),
        ("diff-previous-line", "Move up a line.", -1, false),
        ("diff-select-next-line", "Extend the selection down a line.", 1, true),
        ("diff-select-previous-line", "Extend the selection up a line.", -1, true),
        ("diff-page-down", "Move down half a page.", 20, false),
        ("diff-page-up", "Move up half a page.", -20, false),
    ] {
        ed.define_command(name, doc, o, move |ed, _| with_page(ed, |b, _| b.move_cursor(delta, extend)));
    }
    ed.define_command("diff-top", "Go to the first line.", o, |ed, _| with_page(ed, |b, _| b.cursor_to(false)));
    ed.define_command("diff-bottom", "Go to the last line.", o, |ed, _| with_page(ed, |b, _| b.cursor_to(true)));
    ed.define_command("diff-other-side", "Switch the cursor between the old and new side.", o, |ed, _| {
        with_page(ed, |b, _| {
            b.side = if b.side == Which::New { Which::Old } else { Which::New };
        })
    });
    ed.define_command("diff-search", "Go to the next line of this file with some text (param: the text).", o, |ed, a| {
        if let Some(s) = a.str(0).map(str::to_string) {
            let found = with_page(ed, |b, _| {
                b.search = s.clone();
                b.search_next(&s)
            })?;
            if !found {
                ed.message(format!("No {s} in this file"));
            }
            return Ok(());
        }
        let id = page(ed, None)?;
        let now = ed.with_buffer_as::<DiffBuffer, _>(id, |b, _| b.search.clone()).unwrap_or_default();
        ed.read_string("Search this file: ", &now, move |ed, s| {
            let found = ed.with_buffer_as::<DiffBuffer, _>(id, |b, _| {
                b.search = s.clone();
                b.search_next(&s)
            });
            if found == Some(false) {
                ed.message(format!("No {s} in this file"));
            }
        });
        Ok(())
    });
    ed.define_command("diff-copy", "Copy the selected lines (or the cursor's) of the side the cursor is on.", o, |ed, _| {
        let text = with_page(ed, |b, _| {
            let (side, start, end) = b.selected_lines()?;
            let d = b.diff.as_deref()?;
            let s = if side == Which::New { &d.new } else { &d.old };
            Some(s.text[start as usize - 1..end as usize].join("\n"))
        })?;
        if let Some(t) = text {
            ed.copy_to_clipboard(&t);
            ed.message("Copied");
        }
        Ok(())
    });
    ed.define_command("diff-visit-file", "Open the file at the cursor's line (on this machine).", o, |ed, _| {
        let Some((root, path, line, remote)) = with_page(ed, |b, _| {
            let line = b.line_at_cursor_new();
            Some((b.root.clone(), b.current.clone()?, line, b.machine.is_some()))
        })?
        else {
            return Ok(());
        };
        if remote {
            anyhow::bail!("This file is on another machine");
        }
        crate::commands::open_file(ed, std::path::Path::new(&root).join(path))?;
        if let Some(l) = line {
            ed.with_surface(|s, _| s.set_cursor(crate::text::Pos::new(l as usize - 1, 0)));
        }
        Ok(())
    });

    // Comments.
    ed.define_command("diff-comment", "Comment on the selected lines, or the cursor's.", o, |ed, _| {
        with_page(ed, |b, ed| {
            if let Some((side, start, end)) = b.selected_lines() {
                b.begin_comment(ed, side, start, end, Ask::Fix, "");
            }
        })
    });
    ed.define_command("diff-ask", "Ask a question about the selected lines, or the cursor's.", o, |ed, _| {
        with_page(ed, |b, ed| {
            if let Some((side, start, end)) = b.selected_lines() {
                b.begin_comment(ed, side, start, end, Ask::Question, "");
            }
        })
    });
    ed.define_command("diff-comment-save", "Keep the comment being written, to send with the review.", o, |ed, _| with_page(ed, |b, ed| b.commit_comment(ed)).map(|_| ()));
    ed.define_command("diff-comment-send", "Keep the comment being written and send it to the agent now, on its own.", o, |ed, _| {
        let id = page(ed, None)?;
        if let Some(Some(c)) = ed.with_buffer_as::<DiffBuffer, _>(id, |b, ed| b.commit_comment(ed)) {
            send(ed, id, Some(c))?;
        }
        Ok(())
    });
    ed.define_command("diff-comment-cancel", "Drop the comment being written.", o, |ed, _| with_page(ed, |b, ed| b.leave_editing(ed)));
    ed.define_command("diff-comment-escape", "In vim's insert state, back to normal; otherwise drop the comment being written.", o, |ed, _| {
        let id = page(ed, None)?;
        if crate::packages::vim::state_of(ed, id).is_some_and(|s| s == crate::packages::vim::State::Insert) {
            crate::packages::vim::set_state(ed, id, crate::packages::vim::State::Normal);
            return Ok(());
        }
        with_page(ed, |b, ed| b.leave_editing(ed))
    });
    ed.define_command("diff-comment-cycle-kind", "Whether the comment being written asks for a fix or a question (param: \"fix\" or \"question\").", o, |ed, a| {
        let set = a.str(0).and_then(Ask::parse);
        with_page(ed, |b, _| {
            if let Some(e) = b.editor.as_mut() {
                e.ask = set.unwrap_or(if e.ask == Ask::Fix { Ask::Question } else { Ask::Fix });
            }
        })
    });
    // These act on a comment by id (a click) or the one under the cursor's line.
    let which = |ed: &mut Editor, a: &crate::editor::Args| -> anyhow::Result<Option<u64>> {
        if let Some(id) = a.params.first().and_then(Value::as_u64) {
            return Ok(Some(id));
        }
        with_page(ed, |b, _| b.comment_at_cursor())
    };
    ed.define_command("diff-comment-edit", "Change a comment (param: its id; without one, the one below the cursor's line).", o, move |ed, a| {
        let Some(id) = which(ed, a)? else {
            ed.message("No comment on this line");
            return Ok(());
        };
        with_page(ed, |b, ed| b.begin_edit(ed, id))
    });
    ed.define_command("diff-comment-delete", "Delete a comment (param: its id; without one, the one below the cursor's line).", o, move |ed, a| {
        let Some(id) = which(ed, a)? else {
            ed.message("No comment on this line");
            return Ok(());
        };
        with_page(ed, |b, ed| b.remove_comment(ed, id))
    });
    ed.define_command("diff-comment-resolve", "Resolve a comment, or open it again (param: its id; without one, the one below the cursor's line).", o, move |ed, a| {
        let Some(id) = which(ed, a)? else {
            ed.message("No comment on this line");
            return Ok(());
        };
        with_page(ed, |b, ed| b.toggle_resolved(ed, id))
    });
    ed.define_command("diff-comments-clear", "Delete every comment of this review, sent or not, after asking (param: true doesn't ask).", o, |ed, a| {
        let id = page(ed, None)?;
        let n = ed.with_buffer_as::<DiffBuffer, _>(id, |b, _| b.review.comments.len()).unwrap_or(0);
        if n == 0 {
            ed.message("No comments to clear");
            return Ok(());
        }
        let clear = move |ed: &mut Editor| {
            ed.with_buffer_as::<DiffBuffer, _>(id, |b, ed| b.clear_comments(ed));
            ed.message(format!("Deleted {n} {}", if n == 1 { "comment" } else { "comments" }));
        };
        if a.params.first().and_then(Value::as_bool) == Some(true) {
            clear(ed);
            return Ok(());
        }
        let question = if n == 1 { "Delete the comment?".to_string() } else { format!("Delete all {n} comments?") };
        ed.y_or_n(&question, move |ed, yes| {
            if yes {
                clear(ed);
            }
        });
        Ok(())
    });
    ed.define_command("diff-send-review", "Send every comment not sent yet to the agent (param: the buffer id of a chat or terminal, or \"new\").", o, |ed, a| {
        let id = page(ed, None)?;
        let given = a.params.first().map(|v| v.as_u64().map_or_else(|| v.as_str().unwrap_or_default().to_string(), |n| n.to_string()));
        if let Some(v) = given {
            let t = choose_target(ed, id, &v).ok_or_else(|| anyhow::anyhow!("No such buffer"))?;
            ed.with_buffer_as::<DiffBuffer, _>(id, |b, _| b.target = Some(t));
        }
        send(ed, id, None)
    });
    ed.define_command("diff-select-target", "Choose which chat or terminal a review goes to.", o, |ed, _| {
        let id = page(ed, None)?;
        select_target(ed, id, |_| {});
        Ok(())
    });

    for (k, c) in [
        ("j", "diff-next-line"),
        ("k", "diff-previous-line"),
        ("<down>", "diff-next-line"),
        ("<up>", "diff-previous-line"),
        ("C-n", "diff-next-line"),
        ("C-p", "diff-previous-line"),
        ("J", "diff-select-next-line"),
        ("K", "diff-select-previous-line"),
        ("S-<down>", "diff-select-next-line"),
        ("S-<up>", "diff-select-previous-line"),
        ("<pagedown>", "diff-page-down"),
        ("<pageup>", "diff-page-up"),
        ("C-d", "diff-page-down"),
        ("C-u", "diff-page-up"),
        ("g g", "diff-top"),
        ("G", "diff-bottom"),
        ("h", "diff-other-side"),
        ("l", "diff-other-side"),
        ("<left>", "diff-other-side"),
        ("<right>", "diff-other-side"),
        ("n", "diff-next-change"),
        ("p", "diff-previous-change"),
        ("]", "diff-next-file"),
        ("[", "diff-previous-file"),
        ("f", "diff-select-file"),
        ("v", "diff-mark-viewed"),
        ("d", "diff-select-source"),
        ("s", "diff-toggle-view"),
        ("w", "diff-cycle-whitespace"),
        ("z", "diff-toggle-wrap"),
        ("x", "diff-toggle-expand"),
        ("TAB", "diff-toggle-fold"),
        ("b", "diff-toggle-tree"),
        ("r", "diff-refresh"),
        ("/", "diff-search"),
        ("y", "diff-copy"),
        ("o", "diff-visit-file"),
        ("c", "diff-comment"),
        ("a", "diff-ask"),
        ("e", "diff-comment-edit"),
        ("D", "diff-comment-delete"),
        ("R", "diff-comment-resolve"),
        ("C", "diff-comments-clear"),
        ("S", "diff-send-review"),
        ("q", "kill-current-buffer"),
    ] {
        ed.bind(KIND, k, c, o);
    }
    for (k, c) in [
        ("C-c C-c", "diff-comment-save"),
        ("C-RET", "diff-comment-save"),
        ("s-RET", "diff-comment-save"),
        ("C-c C-s", "diff-comment-send"),
        ("C-c C-k", "diff-comment-cancel"),
        ("ESC", "diff-comment-escape"),
        ("M-k", "diff-comment-cycle-kind"),
    ] {
        ed.bind(EDITING, k, c, o);
    }
    ed.bind("global", "s-D", "diff-view", o);
}

impl DiffBuffer {
    /// The new side's line at the cursor, 1-based, or the nearest one above.
    fn line_at_cursor_new(&self) -> Option<u32> {
        (0..=self.cursor).rev().find_map(|r| self.rows.get(r).and_then(|x| x.new)).map(|l| l + 1)
    }
}

/// A page as scripts see it.
pub fn info(b: &mut DiffBuffer) -> Value {
    let (at, of) = b.change_position();
    let selected = b.selected_lines();
    json!({
        "root": b.root,
        "machine": b.machine,
        "compared": b.spec.as_ref().map(view::describe),
        "loaded": b.loaded,
        "loading": b.loading,
        "error": b.error,
        "file": b.current,
        "view": if b.split_now { "split" } else { "unified" },
        "whitespace": b.whitespace.name(),
        "wrap": b.wrap,
        "change": at,
        "changes": of,
        "rows": b.rows.len(),
        "cursor": b.cursor + 1,
        "side": b.side.name(),
        "selection": selected.map(|(s, a, e)| json!({"side": s.name(), "line": a, "end_line": e})),
        "writing": b.editor.as_ref().map(|e| json!({"text": e.area.text(), "kind": e.ask.name(), "line": e.start, "end_line": e.end, "side": e.side.name()})),
        "target": b.target.map(crate::editor::buffer_id_to_u64),
        "drafts": b.drafts().len(),
    })
}

/// The files a page lists, as scripts see them.
pub fn files_json(b: &DiffBuffer) -> Value {
    Value::Array(b.files.iter().map(|f| json!({"path": f.path, "old_path": f.old_path, "status": f.status, "untracked": f.untracked, "binary": f.binary, "added": f.added, "removed": f.removed, "viewed": b.review.viewed.contains_key(&f.path), "comments": b.comments_on(&f.path)})).collect())
}

/// `serpis.diff.comment`: add a comment from a script. On the current file's line, unless
/// another path is given: then it anchors when that file is shown.
pub fn add_comment(ed: &mut Editor, id: BufferId, spec: &Value) -> anyhow::Result<u64> {
    let text = spec.get("text").and_then(Value::as_str).ok_or_else(|| anyhow::anyhow!("A comment needs its text"))?.to_string();
    let line = spec.get("line").and_then(Value::as_u64).ok_or_else(|| anyhow::anyhow!("A comment needs its line"))? as u32;
    if line == 0 {
        anyhow::bail!("Lines count from 1");
    }
    let end = spec.get("end_line").and_then(Value::as_u64).map_or(line, |e| e as u32).max(line);
    let side = if spec.get("side").and_then(Value::as_str) == Some("old") { Which::Old } else { Which::New };
    let ask = spec.get("kind").and_then(Value::as_str).and_then(Ask::parse).unwrap_or_default();
    let path = spec.get("path").and_then(Value::as_str).map(str::to_string);
    ed.with_buffer_as::<DiffBuffer, _>(id, |b, ed| {
        let path = path.or_else(|| b.current.clone()).ok_or_else(|| anyhow::anyhow!("No file is showing: give a path"))?;
        let c = b.add_comment(path, side, line, end, text, ask);
        b.persist(ed);
        Ok(c)
    })
    .ok_or_else(|| anyhow::anyhow!("Not a diff page"))?
}
