//! Built-in commands. Keybinding schemes only bind these; they add no behavior of their own
//! (except vim's operator grammar).

use serde_json::Value;

use crate::buffer::{Buffer, BufferId};
use crate::buffers::chat::{ChatBuffer, Focus};
use crate::buffers::terminal::TerminalBuffer;
use crate::buffers::text::TextBuffer;
use crate::editor::{Args, BUILTIN, Editor, buffer_id_to_u64};
use crate::minibuffer::Candidate;
use crate::motion;
use crate::text::{Pos, TextArea, TextSurface, char_len, ordered};

/// Insert text at the cursor, replacing the selection.
pub fn insert_replacing_selection(area: &mut TextArea, text: &str) {
    if let Some((a, b)) = area.selection() {
        area.delete(a, b);
        area.set_cursor(a);
        area.set_anchor(None);
    }
    let c = area.cursor();
    let end = area.insert(c, text);
    area.set_cursor(end);
}

/// Move the cursor, extending the selection when `extend` is set (shift-motions).
pub fn move_to(s: &mut dyn TextSurface, p: Pos, extend: bool) {
    if extend && s.anchor().is_none() {
        s.set_anchor(Some(s.cursor()));
    }
    if !extend && s.anchor().is_some() && !keep_mark() {
        s.set_anchor(None);
    }
    s.set_cursor(p);
}

thread_local! {
    /// Emacs transient mark: motions extend an active mark set with C-SPC.
    static MARK_ACTIVE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

pub fn set_mark_active(on: bool) {
    MARK_ACTIVE.with(|m| m.set(on));
}

fn keep_mark() -> bool {
    MARK_ACTIVE.with(|m| m.get())
}

fn motion_cmd(ed: &mut Editor, name: &str, doc: &str, extend: bool, f: fn(&dyn TextSurface, Pos, usize) -> Pos) {
    ed.define_command(name, doc, BUILTIN, move |ed, a| {
        let n = a.count();
        ed.with_surface(|s, _| {
            let p = f(s, s.cursor(), n);
            move_to(s, p, extend);
        });
        Ok(())
    });
}

fn repeat(n: usize, mut p: Pos, f: impl Fn(Pos) -> Pos) -> Pos {
    for _ in 0..n {
        p = f(p);
    }
    p
}

/// Delete `from..to` on the focused surface, saving the text to the kill ring.
/// Delete `from..to` and return the text actually removed (surfaces may clamp the range).
pub fn kill(ed: &mut Editor, from: Pos, to: Pos, save: bool) -> Option<String> {
    let text = ed
        .with_surface(|s, _| {
            let (a, b) = ordered(from, to);
            if !s.editable_at(a) {
                return None;
            }
            let t = s.delete(a, b);
            s.set_cursor(a);
            s.set_anchor(None);
            Some(t)
        })
        .flatten();
    if let Some(t) = text.as_ref().filter(|t| !t.is_empty() && save) {
        ed.copy_to_clipboard(t);
    }
    text
}

fn current_chat(ed: &mut Editor) -> Option<BufferId> {
    let id = ed.current_buffer();
    (ed.meta(id)?.major == "chat").then_some(id)
}

/// Run `f` on the focused chat buffer.
pub fn with_chat<R>(ed: &mut Editor, f: impl FnOnce(&mut ChatBuffer, &mut Editor) -> R) -> anyhow::Result<R> {
    let id = current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
    ed.with_buffer_as::<ChatBuffer, R>(id, f).ok_or_else(|| anyhow::anyhow!("Not a chat buffer"))
}

pub fn with_terminal<R>(ed: &mut Editor, f: impl FnOnce(&mut TerminalBuffer, &mut Editor) -> R) -> anyhow::Result<R> {
    let id = ed.current_buffer();
    ed.with_buffer_as::<TerminalBuffer, R>(id, f).ok_or_else(|| anyhow::anyhow!("Not in a terminal buffer"))
}

fn show_page(ed: &mut Editor, name: &str, text: &str) {
    let id = match ed.buffer_by_name(name) {
        Some(id) => {
            ed.with_buffer_as::<TextBuffer, ()>(id, |b, _| {
                b.area.read_only = false;
                b.area.set_text(text);
                b.area.read_only = true;
                b.area.view.cursor = Pos::ZERO;
            });
            id
        }
        None => ed.add_buffer(name, Box::new(TextBuffer::page(text, "help"))),
    };
    ed.pop_to_buffer(id);
}

pub fn install(ed: &mut Editor) {
    let o = BUILTIN;

    // ---------------------------------------------------------------- core
    ed.define_command("execute-extended-command", "Run a command by name (M-x).", o, |ed, a| {
        let prefix = a.prefix;
        let mut names: Vec<(&String, &crate::editor::Command)> = ed.commands.iter().collect();
        names.sort_by(|a, b| a.0.cmp(b.0));
        let bindings = all_bindings(ed);
        let cands = names
            .into_iter()
            .map(|(n, c)| {
                let key = bindings.get(n.as_str()).cloned().unwrap_or_default();
                let doc = c.doc.lines().next().unwrap_or("");
                Candidate::new(n.clone()).detail(if key.is_empty() { doc.to_string() } else { format!("{key}   {doc}") })
            })
            .collect();
        ed.completing_read("M-x ", cands, true, move |ed, c, _| {
            if let Some(c) = c {
                let args = Args { prefix, ..Default::default() };
                if let Err(e) = ed.run_command(&c.value, &args) {
                    ed.error(format!("{}: {e:#}", c.value));
                }
            }
        });
        Ok(())
    });
    ed.define_command("keyboard-quit", "Cancel the current prompt, selection or key sequence.", o, |ed, _| {
        set_mark_active(false);
        ed.keyboard_quit();
        Ok(())
    });
    ed.define_command("universal-argument", "Begin a numeric prefix argument (C-u = 4).", o, |ed, _| {
        ed.prefix_arg = Some(ed.prefix_arg.map(|p| p * 4).unwrap_or(4));
        ed.message(format!("C-u {}", ed.prefix_arg.unwrap()));
        Ok(())
    });
    ed.define_command("reload-config", "Reload init.lua and Rust plugins.", o, |ed, _| {
        crate::lua::reload(ed);
        crate::plugins::reload_all(ed);
        Ok(())
    });
    ed.define_command("open-config", "Visit ~/.config/serpis/init.lua.", o, |ed, _| {
        let path = crate::config_dir().join("init.lua");
        if !path.exists() {
            let _ = std::fs::create_dir_all(crate::config_dir());
            let _ = std::fs::write(&path, crate::lua::DEFAULT_INIT);
        }
        open_file(ed, path)
    });
    ed.define_command("toggle-theme", "Switch to the light/dark counterpart of the current theme.", o, |ed, _| {
        let target = crate::themes::counterpart(&ed.theme.name);
        let target = if ed.theme_names().contains(&target) { target } else if ed.theme.dark { "serpis-light".into() } else { "serpis-dark".into() };
        ed.load_theme(&target)?;
        ed.persist_theme(&target);
        ed.message(format!("Theme: {target}"));
        Ok(())
    });
    ed.define_command("select-theme", "Choose a theme with live preview (remembered across restarts).", o, |ed, a| {
        if let Some(name) = a.str(0) {
            ed.load_theme(name)?;
            ed.persist_theme(name);
            return Ok(());
        }
        let original = ed.theme.name.clone();
        let cands = ed
            .theme_names()
            .into_iter()
            .map(|n| {
                let detail = if n == original { "current" } else { "" };
                Candidate::new(n).detail(detail)
            })
            .collect();
        ed.completing_read("Theme: ", cands, true, |ed, c, _| {
            if let Some(c) = c {
                if let Err(e) = ed.load_theme(&c.value) {
                    ed.error(format!("{e:#}"));
                    return;
                }
                ed.persist_theme(&c.value);
                ed.message(format!("Theme: {}", c.value));
            }
        });
        let restore = original.clone();
        ed.minibuffer_preview(
            Some(std::rc::Rc::new(|ed: &mut Editor, c: Option<&Candidate>| {
                if let Some(c) = c {
                    let _ = ed.load_theme(&c.value);
                }
            })),
            Some(Box::new(move |ed: &mut Editor| {
                let _ = ed.load_theme(&restore);
            })),
            true,
        );
        // Start on the current theme rather than the first entry.
        if let Some(mb) = &mut ed.minibuffer {
            if let Some(i) = mb.filtered.iter().position(|(ci, _)| mb.candidates[*ci].value == original) {
                mb.selected = i;
            }
        }
        Ok(())
    });
    ed.define_command("text-scale-increase", "Increase font size.", o, |ed, _| {
        crate::fonts::scale(ed, 1.0);
        Ok(())
    });
    ed.define_command("text-scale-reset", "Reset font sizes to font-size / font-mono-size.", o, |ed, _| {
        crate::fonts::reset_sizes(ed);
        Ok(())
    });
    ed.define_command("send-key-to-buffer", "Give the invoking key to the buffer itself (e.g. a terminal), bypassing keymaps.", o, |ed, a| {
        if let Some(k) = a.keys.last().cloned() {
            let id = ed.current_buffer();
            ed.with_buffer(id, |b, ed| b.handle_key(&k, ed));
        }
        Ok(())
    });
    ed.define_command("text-scale-decrease", "Decrease font size.", o, |ed, _| {
        crate::fonts::scale(ed, -1.0);
        Ok(())
    });

    // ---------------------------------------------------------------- windows
    ed.define_command("split-window-right", "Split the window side by side.", o, |ed, _| {
        ed.split_window(false);
        Ok(())
    });
    ed.define_command("split-window-below", "Split the window top/bottom.", o, |ed, _| {
        ed.split_window(true);
        Ok(())
    });
    ed.define_command("delete-other-windows", "Make the focused window fill the frame.", o, |ed, _| {
        ed.delete_other_windows();
        Ok(())
    });
    ed.define_command("other-window", "Focus the next window.", o, |ed, a| {
        ed.other_window(a.count() as isize);
        Ok(())
    });
    ed.define_command("previous-window", "Focus the previous window.", o, |ed, a| {
        ed.other_window(-(a.count() as isize));
        Ok(())
    });

    // ---------------------------------------------------------------- buffers
    ed.define_command("switch-to-buffer", "Show a buffer in the focused window.", o, |ed, _| {
        let cur = ed.current_buffer();
        let mut ids: Vec<BufferId> = ed.buffers.keys().collect();
        // Most recently shown first, current last.
        let hist = ed.windows[ed.focused].history.clone();
        ids.sort_by_key(|id| std::cmp::Reverse(hist.iter().position(|h| h == id).map(|p| p + 1).unwrap_or(0)));
        ids.retain(|id| *id != cur);
        ids.push(cur);
        let cands = ids
            .into_iter()
            .map(|id| {
                let m = &ed.buffers[id].meta;
                Candidate::new(m.name.clone()).detail(m.major.clone()).value(buffer_id_to_u64(id).to_string())
            })
            .collect();
        ed.completing_read("Switch to buffer: ", cands, false, |ed, c, input| match c {
            Some(c) => {
                let id = crate::editor::buffer_id_from_u64(c.value.parse().unwrap_or(0));
                ed.show_buffer(id);
            }
            None if !input.is_empty() => {
                let id = ed.add_buffer(&input, Box::new(TextBuffer::new("")));
                ed.show_buffer(id);
            }
            None => {}
        });
        Ok(())
    });
    ed.define_command("next-buffer", "Cycle to the next buffer.", o, |ed, _| {
        cycle_buffer(ed, 1);
        Ok(())
    });
    ed.define_command("previous-buffer", "Cycle to the previous buffer.", o, |ed, _| {
        cycle_buffer(ed, -1);
        Ok(())
    });
    ed.define_command("new-buffer", "Create a buffer of a registered kind (param: kind).", o, |ed, a| {
        match a.str(0) {
            Some(kind) => {
                let id = ed.create_buffer(kind, None, a.params.get(1).unwrap_or(&Value::Null))?;
                ed.show_buffer(id);
            }
            None => {
                let mut kinds: Vec<String> = ed.buffer_kinds.keys().cloned().collect();
                kinds.sort();
                ed.completing_read("Buffer kind: ", kinds.into_iter().map(Candidate::new).collect(), true, |ed, c, _| {
                    if let Some(c) = c {
                        match ed.create_buffer(&c.value, None, &Value::Null) {
                            Ok(id) => ed.show_buffer(id),
                            Err(e) => ed.error(format!("{e:#}")),
                        }
                    }
                });
            }
        }
        Ok(())
    });
    ed.define_command("new-chat", "Open a new agent chat (param: directory; default per new-buffer-directory).", o, |ed, a| {
        let dir = a.str(0).map(crate::expand_home).unwrap_or_else(|| ed.new_buffer_directory());
        ed.new_buffer_in("chat", &dir)?;
        Ok(())
    });
    ed.define_command("new-terminal", "Open a new terminal (param: directory; default per new-buffer-directory).", o, |ed, a| {
        let dir = a.str(0).map(crate::expand_home).unwrap_or_else(|| ed.new_buffer_directory());
        ed.new_buffer_in("terminal", &dir)?;
        Ok(())
    });
    ed.define_command("new-scratch", "Open a new scratch buffer.", o, |ed, _| {
        let id = ed.add_buffer("*scratch*", Box::new(TextBuffer::new("")));
        ed.show_buffer(id);
        Ok(())
    });
    ed.define_command("find-file", "Visit a file.", o, |ed, _| {
        let start = format!("{}/", ed.cwd.display());
        ed.completing_read_dynamic("Find file: ", &start, file_candidates, |ed, c, input| {
            let path = c.map(|c| c.value).unwrap_or(input);
            let path = crate::expand_home(&path);
            if path.is_dir() {
                ed.cwd = path.clone();
                ed.message(format!("cwd: {}", path.display()));
            } else if let Err(e) = open_file(ed, path) {
                ed.error(format!("{e:#}"));
            }
        });
        Ok(())
    });
    ed.define_command("save-buffer", "Save the focused file buffer.", o, |ed, _| {
        let id = ed.current_buffer();
        match ed.with_buffer_as::<TextBuffer, _>(id, |b, _| b.save()) {
            Some(Ok(p)) => ed.message(format!("Wrote {p}")),
            Some(Err(e)) => ed.error(format!("{e:#}")),
            None => ed.message("Nothing to save"),
        }
        Ok(())
    });
    ed.define_command("view-messages", "Show the *Messages* log.", o, |ed, _| {
        let text = ed.messages.join("\n");
        show_page(ed, "*Messages*", &text);
        Ok(())
    });

    // ---------------------------------------------------------------- help
    ed.define_command("describe-key", "Show the command bound to the next key sequence.", o, |ed, _| {
        ed.describe_key_pending = true;
        ed.message("Describe key: ");
        Ok(())
    });
    ed.define_command("describe-command", "Show a command's documentation.", o, |ed, _| {
        let mut names: Vec<String> = ed.commands.keys().cloned().collect();
        names.sort();
        ed.completing_read("Describe command: ", names.into_iter().map(Candidate::new).collect(), true, |ed, c, _| {
            if let Some(c) = c {
                let cmd = ed.commands.get(&c.value).cloned();
                let keys = all_bindings(ed).get(c.value.as_str()).cloned().unwrap_or_else(|| "(unbound)".into());
                if let Some(cmd) = cmd {
                    let text = format!("{}\n\n  defined by: {}\n  keys: {keys}\n\n{}", cmd.name, cmd.owner, cmd.doc);
                    show_page(ed, "*Help*", &text);
                }
            }
        });
        Ok(())
    });
    ed.define_command("describe-keymap", "List the bindings of a keymap.", o, |ed, _| {
        let mut names: Vec<String> = ed.keymaps.maps.keys().cloned().collect();
        names.sort();
        let active = ed.active_keymaps();
        let cands = names
            .into_iter()
            .map(|n| {
                let d = if active.contains(&n) { "active" } else { "" };
                Candidate::new(n).detail(d)
            })
            .collect();
        ed.completing_read("Describe keymap: ", cands, true, |ed, c, _| {
            if let Some(c) = c {
                let mut text = format!("Keymap `{}`\n\n", c.value);
                if let Some(m) = ed.keymap(&c.value) {
                    for (k, cmd) in m.flatten() {
                        text.push_str(&format!("{k:<24} {cmd}\n"));
                    }
                }
                show_page(ed, "*Help*", &text);
            }
        });
        Ok(())
    });
    ed.define_command("describe-variable", "Show a variable's value.", o, |ed, _| {
        let mut names: Vec<String> = ed.vars.keys().cloned().collect();
        names.sort();
        let cands = names.into_iter().map(|n| {
            let v = ed.vars[&n].value.to_string();
            Candidate::new(n).detail(v)
        }).collect();
        ed.completing_read("Describe variable: ", cands, true, |ed, c, _| {
            if let Some(c) = c {
                if let Some(v) = ed.vars.get(&c.value).cloned() {
                    show_page(ed, "*Help*", &format!("{}\n\n  value: {}\n\n{}", c.value, serde_json::to_string_pretty(&v.value).unwrap_or_default(), v.doc));
                }
            }
        });
        Ok(())
    });
    ed.define_command("describe-mode", "Show the active modes and keymaps of the focused buffer.", o, |ed, _| {
        let id = ed.current_buffer();
        let meta = ed.meta(id).cloned().unwrap_or_default();
        let text = format!(
            "Buffer: {}\nMajor mode: {}\nMinor modes: {}\n\nKeymaps (highest priority first):\n  {}\n",
            meta.name,
            meta.major,
            meta.minor_modes.join(", "),
            ed.active_keymaps().join("\n  ")
        );
        show_page(ed, "*Help*", &text);
        Ok(())
    });

    // ---------------------------------------------------------------- motion
    motion_cmd(ed, "forward-char", "Move right.", false, |s, p, n| repeat(n, p, |p| motion::next_pos(s, p).unwrap_or(p)));
    motion_cmd(ed, "backward-char", "Move left.", false, |s, p, n| repeat(n, p, |p| motion::prev_pos(s, p).unwrap_or(p)));
    motion_cmd(ed, "forward-word", "Move to the end of the next word.", false, |s, p, n| repeat(n, p, |p| motion::emacs_forward_word(s, p)));
    motion_cmd(ed, "backward-word", "Move to the start of the previous word.", false, |s, p, n| repeat(n, p, |p| motion::emacs_backward_word(s, p)));
    motion_cmd(ed, "beginning-of-line", "Move to line start (first non-blank on repeat).", false, |s, p, _| {
        let fnb = motion::line_first_nonblank(s, p.line);
        if p.col == fnb.col { Pos::new(p.line, 0) } else if p.col == 0 { fnb } else { Pos::new(p.line, 0) }
    });
    motion_cmd(ed, "end-of-line", "Move to line end.", false, |s, p, _| motion::line_end(s, p.line));
    motion_cmd(ed, "beginning-of-buffer", "Move to the start of the buffer.", false, |_, _, _| Pos::ZERO);
    motion_cmd(ed, "end-of-buffer", "Move to the end of the buffer.", false, |s, _, _| motion::doc_end(s));
    motion_cmd(ed, "forward-paragraph", "Move to the next blank line.", false, |s, p, n| repeat(n, p, |p| motion::paragraph(s, p, true)));
    motion_cmd(ed, "backward-paragraph", "Move to the previous blank line.", false, |s, p, n| repeat(n, p, |p| motion::paragraph(s, p, false)));
    motion_cmd(ed, "select-forward-char", "Extend the selection right.", true, |s, p, n| repeat(n, p, |p| motion::next_pos(s, p).unwrap_or(p)));
    motion_cmd(ed, "select-backward-char", "Extend the selection left.", true, |s, p, n| repeat(n, p, |p| motion::prev_pos(s, p).unwrap_or(p)));
    motion_cmd(ed, "select-forward-word", "Extend the selection by a word.", true, |s, p, n| repeat(n, p, |p| motion::emacs_forward_word(s, p)));
    motion_cmd(ed, "select-backward-word", "Extend the selection back a word.", true, |s, p, n| repeat(n, p, |p| motion::emacs_backward_word(s, p)));
    motion_cmd(ed, "select-end-of-line", "Extend the selection to line end.", true, |s, p, _| motion::line_end(s, p.line));
    motion_cmd(ed, "select-beginning-of-line", "Extend the selection to line start.", true, |_, p, _| Pos::new(p.line, 0));
    for (name, delta, extend) in [
        ("next-line", 1isize, false),
        ("previous-line", -1, false),
        ("select-next-line", 1, true),
        ("select-previous-line", -1, true),
    ] {
        ed.define_command(name, "Move vertically, keeping the column.", o, move |ed, a| {
            let n = a.count() as isize;
            let last = ed.last_command.clone();
            ed.with_surface(|s, _| {
                let c = s.cursor();
                let goal = goal_col(c, last.as_deref());
                let p = motion::vertical(s, c, delta * n, goal);
                move_to(s, p, extend);
                remember_goal(goal);
            });
            Ok(())
        });
    }
    for (name, dir) in [("scroll-up-command", 1isize), ("scroll-down-command", -1)] {
        ed.define_command(name, "Move by a page.", o, move |ed, _| {
            ed.with_surface(|s, _| {
                let c = s.cursor();
                let page = s.page_lines().saturating_sub(2).max(1) as isize;
                let p = motion::vertical(s, c, dir * page, c.col);
                move_to(s, p, false);
            });
            Ok(())
        });
    }

    // ---------------------------------------------------------------- editing
    ed.define_command("newline", "Insert a line break.", o, |ed, _| {
        ed.self_insert("\n");
        Ok(())
    });
    ed.define_command("delete-backward-char", "Delete the character (or selection) before point.", o, |ed, a| {
        let n = a.count();
        let sel = ed.with_surface(|s, _| s.selection()).flatten();
        if let Some((x, y)) = sel {
            kill(ed, x, y, false);
            return Ok(());
        }
        ed.with_surface(|s, _| {
            let c = s.cursor();
            let from = repeat(n, c, |p| motion::prev_pos(s, p).unwrap_or(p));
            if s.editable_at(from) {
                s.delete(from, c);
                s.set_cursor(from);
            }
        });
        Ok(())
    });
    ed.define_command("delete-char", "Delete the character (or selection) at point.", o, |ed, a| {
        let n = a.count();
        let sel = ed.with_surface(|s, _| s.selection()).flatten();
        if let Some((x, y)) = sel {
            kill(ed, x, y, false);
            return Ok(());
        }
        ed.with_surface(|s, _| {
            let c = s.cursor();
            let to = repeat(n, c, |p| motion::next_pos(s, p).unwrap_or(p));
            if s.editable_at(c) {
                s.delete(c, to);
            }
        });
        Ok(())
    });
    ed.define_command("kill-line", "Kill to end of line (or the newline at its end).", o, |ed, _| {
        let r = ed.with_surface(|s, _| {
            let c = s.cursor();
            let end = motion::line_end(s, c.line);
            if c == end { motion::next_pos(s, c).map(|n| (c, n)) } else { Some((c, end)) }
        });
        if let Some(Some((a, b))) = r {
            kill(ed, a, b, true);
        }
        Ok(())
    });
    ed.define_command("kill-word", "Kill to the end of the word.", o, |ed, a| {
        let n = a.count();
        if let Some((a, b)) = ed.with_surface(|s, _| (s.cursor(), repeat(n, s.cursor(), |p| motion::emacs_forward_word(s, p)))) {
            kill(ed, a, b, true);
        }
        Ok(())
    });
    ed.define_command("backward-kill-word", "Kill back to the start of the word.", o, |ed, a| {
        let n = a.count();
        if let Some((a, b)) = ed.with_surface(|s, _| (repeat(n, s.cursor(), |p| motion::emacs_backward_word(s, p)), s.cursor())) {
            kill(ed, a, b, true);
        }
        Ok(())
    });
    ed.define_command("set-mark-command", "Set the mark (start a selection).", o, |ed, _| {
        ed.with_surface(|s, _| {
            let c = s.cursor();
            s.set_anchor(Some(c));
        });
        set_mark_active(true);
        ed.message("Mark set");
        Ok(())
    });
    ed.define_command("kill-region", "Cut the selection.", o, |ed, _| {
        set_mark_active(false);
        if let Some(Some((a, b))) = ed.with_surface(|s, _| s.selection()) {
            kill(ed, a, b, true);
        }
        Ok(())
    });
    ed.define_command("copy-region-as-kill", "Copy the selection.", o, |ed, _| {
        set_mark_active(false);
        let t = ed.with_surface(|s, _| {
            let t = s.selection().map(|(a, b)| (s.text_range(a, b), a, b));
            s.set_anchor(None);
            t
        });
        if let Some(Some((t, a, b))) = t {
            ed.copy_to_clipboard(&t);
            ed.notify_yank(a, b, false);
            ed.message("Copied");
        }
        Ok(())
    });
    ed.define_command("paste", "Paste from the clipboard (alias of yank).", o, |ed, a| ed.run_command("yank", a));
    ed.define_command("yank", "Paste from the clipboard.", o, |ed, _| {
        if let Some(t) = ed.clipboard_text() {
            ed.handle_paste(&t);
        }
        Ok(())
    });
    ed.define_command("undo", "Undo.", o, |ed, _| {
        ed.with_surface(|s, _| s.undo());
        Ok(())
    });
    ed.define_command("redo", "Redo.", o, |ed, _| {
        ed.with_surface(|s, _| s.redo());
        Ok(())
    });
    ed.define_command("select-all", "Select the whole buffer.", o, |ed, _| {
        ed.with_surface(|s, _| {
            s.set_anchor(Some(Pos::ZERO));
            let e = motion::doc_end(s);
            s.set_cursor(e);
        });
        Ok(())
    });
    ed.define_command("search-forward", "Search forward for a regex.", o, |ed, _| {
        search_prompt(ed, true);
        Ok(())
    });
    ed.define_command("search-backward", "Search backward for a regex.", o, |ed, _| {
        search_prompt(ed, false);
        Ok(())
    });
    ed.define_command("search-next", "Repeat the last search.", o, |ed, _| {
        repeat_search(ed, false);
        Ok(())
    });
    ed.define_command("search-previous", "Repeat the last search in the other direction.", o, |ed, _| {
        repeat_search(ed, true);
        Ok(())
    });

    // ---------------------------------------------------------------- chat
    ed.define_command("chat-send", "Send the prompt (or steer the running agent).", o, |ed, _| {
        with_chat(ed, |c, ed| {
            if c.focus == Focus::Transcript {
                if !c.toggle_fold_at_cursor() {
                    c.focus = Focus::Prompt;
                }
                return;
            }
            c.send(ed, false)
        })
    });
    ed.define_command("chat-send-follow-up", "Queue the prompt to run after the agent finishes.", o, |ed, _| with_chat(ed, |c, ed| c.send(ed, true)));
    ed.define_command("chat-newline", "Insert a newline in the prompt.", o, |ed, _| {
        with_chat(ed, |c, _| {
            c.focus = Focus::Prompt;
            insert_replacing_selection(&mut c.prompt, "\n");
        })
    });
    ed.define_command("chat-abort", "Abort the running agent.", o, |ed, _| {
        with_chat(ed, |c, ed| {
            if c.running() {
                c.abort();
                ed.message("Aborted");
            }
        })
    });
    ed.define_command("chat-abort-or-quit", "Abort the agent if running, else keyboard-quit.", o, |ed, _| {
        let running = with_chat(ed, |c, _| c.running()).unwrap_or(false);
        if running {
            with_chat(ed, |c, _| c.abort())?;
            ed.message("Aborted");
        } else {
            ed.keyboard_quit();
        }
        Ok(())
    });
    ed.define_command("chat-clear", "Start a fresh conversation in this chat.", o, |ed, _| with_chat(ed, |c, _| c.clear()));
    ed.define_command("chat-cycle-thinking", "Cycle the thinking level.", o, |ed, _| {
        let lvl = with_chat(ed, |c, _| {
            c.cycle_thinking();
            c.thinking
        })?;
        ed.message(format!("Thinking: {}", lvl.as_str()));
        Ok(())
    });
    ed.define_command("chat-select-thinking", "Choose the thinking level from a picker.", o, |ed, _| {
        let (levels, current) = with_chat(ed, |c, _| {
            let levels = c.model.as_ref().map(serpis_ai::supported_thinking_levels).unwrap_or_else(|| serpis_ai::ThinkingLevel::ALL.to_vec());
            (levels, c.thinking)
        })?;
        let cands = levels
            .iter()
            .map(|l| {
                let c = Candidate::new(l.as_str());
                if *l == current { c.detail("current") } else { c }
            })
            .collect();
        ed.completing_read("Thinking: ", cands, true, move |ed, c, _| {
            let Some(c) = c else { return };
            let Some(l) = serpis_ai::ThinkingLevel::parse(&c.value) else { return };
            let Ok(l) = with_chat(ed, |chat, _| {
                chat.set_thinking(l);
                l
            }) else { return };
            ed.message(format!("Thinking: {}", l.as_str()));
        });
        Ok(())
    });
    ed.define_command("chat-toggle-fold", "Expand or collapse the tool output at point.", o, |ed, _| {
        with_chat(ed, |c, _| {
            c.toggle_fold_at_cursor();
        })
    });
    ed.define_command("chat-focus-prompt", "Move to the prompt.", o, |ed, _| {
        with_chat(ed, |c, _| {
            c.focus = Focus::Prompt;
        })
    });
    ed.define_command("chat-history-previous", "Recall the previous prompt.", o, |ed, _| with_chat(ed, |c, _| c.history_prev()));
    ed.define_command("chat-history-next", "Recall the next prompt.", o, |ed, _| with_chat(ed, |c, _| c.history_next()));
    ed.define_command("chat-select-model", "Choose the model for this chat.", o, |ed, _| {
        let configured = ed.registry.configured_providers();
        let mut models: Vec<serpis_ai::Model> = ed
            .registry
            .models()
            .into_iter()
            .filter(|m| configured.contains(&m.provider))
            .filter(|m| serpis_ai::api::SUPPORTED_APIS.contains(&m.api.as_str()))
            .collect();
        models.sort_by(|a, b| (a.provider.as_str(), a.id.as_str()).cmp(&(b.provider.as_str(), b.id.as_str())));
        let cands = models
            .iter()
            .map(|m| {
                let ctx = crate::buffers::chat::fmt_tokens(m.context_window);
                let cost = if m.cost.input > 0.0 { format!("  ${}/{}", m.cost.input, m.cost.output) } else { String::new() };
                Candidate::new(format!("{}/{}", m.provider, m.id)).detail(format!("{}  {ctx}{cost}{}", m.display_name(), if m.reasoning { "  ◆" } else { "" }))
            })
            .collect();
        ed.completing_read("Model: ", cands, true, move |ed, c, _| {
            let Some(c) = c else { return };
            let Some(m) = models.into_iter().find(|m| format!("{}/{}", m.provider, m.id) == c.value) else { return };
            ed.set_var("agent-model", Value::String(c.value.clone()));
            let _ = with_chat(ed, |chat, _| chat.set_model(m));
            ed.message(format!("Model: {}", c.value));
        });
        Ok(())
    });
    ed.define_command("chat-refresh-models", "Refetch model catalogs.", o, |ed, _| {
        let reg = ed.registry.clone();
        let main = ed.main_sender();
        ed.rt.spawn(async move {
            let errs = reg.refresh_catalogs(true).await;
            main.send(move |ed| {
                if errs.is_empty() {
                    ed.message(format!("Models refreshed ({} available)", ed.registry.models().len()));
                } else {
                    ed.warn(format!("Model refresh: {}", errs.join("; ")));
                }
            });
        });
        Ok(())
    });
    ed.define_command("chat-set-api-key", "Store an API key for a provider in auth.json.", o, |ed, _| {
        let cands = serpis_ai::registry::BUILTIN_PROVIDERS.iter().map(|p| Candidate::new(p.id).detail(p.name)).collect();
        ed.completing_read("Provider: ", cands, false, |ed, c, input| {
            let provider = c.map(|c| c.value).unwrap_or(input);
            ed.read_string(&format!("API key for {provider}: "), "", move |ed, key| {
                if let Err(e) = ed.registry.set_api_key(&provider, key.trim()) {
                    ed.error(format!("{e:#}"));
                } else {
                    ed.message(format!("Saved key for {provider}"));
                    ed.call("chat-refresh-models");
                }
            });
        });
        Ok(())
    });

    // ---------------------------------------------------------------- terminal
    ed.define_command("terminal-send-escape", "Send ESC to the terminal program.", o, |ed, _| with_terminal(ed, |t, _| t.write(b"\x1b")));
    ed.define_command("terminal-send-ctrl-c", "Send C-c to the terminal program.", o, |ed, _| with_terminal(ed, |t, _| t.write(b"\x03")));
    ed.define_command("terminal-copy-mode", "Navigate the scrollback with motions.", o, |ed, _| with_terminal(ed, |t, _| t.enter_copy_mode()));
    ed.define_command("terminal-exit-copy-mode", "Return to live terminal input.", o, |ed, _| with_terminal(ed, |t, _| t.exit_copy_mode()));
    ed.define_command("terminal-copy", "Copy the copy-mode selection.", o, |ed, _| {
        let t = with_terminal(ed, |t, _| t.selection().map(|(a, b)| (t.text_range(a, b), a, b)))?;
        if let Some((t, a, b)) = t {
            ed.copy_to_clipboard(&t);
            ed.notify_yank(a, b, false);
            ed.message("Copied");
        }
        Ok(())
    });
}

thread_local! {
    static GOAL: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    static LAST_SEARCH: std::cell::RefCell<(String, bool)> = const { std::cell::RefCell::new((String::new(), true)) };
}

/// Keep the goal column across consecutive vertical moves.
pub fn goal_col(c: Pos, last: Option<&str>) -> usize {
    let vertical = last.is_some_and(|l| l.ends_with("next-line") || l.ends_with("previous-line") || l.starts_with("vim-"));
    if vertical { GOAL.with(|g| g.get().max(c.col)) } else { c.col }
}

pub fn remember_goal(g: usize) {
    GOAL.with(|x| x.set(g));
}

fn search_prompt(ed: &mut Editor, forward: bool) {
    let prompt = if forward { "Search: " } else { "Search backward: " };
    ed.read_string(prompt, "", move |ed, pat| {
        if pat.is_empty() {
            return;
        }
        LAST_SEARCH.with(|l| *l.borrow_mut() = (pat, forward));
        repeat_search(ed, false);
    });
}

pub fn set_last_search(pat: &str, forward: bool) {
    LAST_SEARCH.with(|l| *l.borrow_mut() = (pat.to_string(), forward));
}

pub fn repeat_search(ed: &mut Editor, reverse: bool) {
    let (pat, fwd) = LAST_SEARCH.with(|l| l.borrow().clone());
    if pat.is_empty() {
        ed.message("No previous search");
        return;
    }
    let forward = fwd != reverse;
    let found = ed.with_surface(|s, _| {
        let c = s.cursor();
        motion::search(s, c, &pat, forward).map(|(a, _)| {
            s.set_anchor(None);
            s.set_cursor(a);
        })
    });
    if !matches!(found, Some(Some(()))) {
        ed.message(format!("Search failed: {pat}"));
    }
}

fn cycle_buffer(ed: &mut Editor, d: isize) {
    let ids: Vec<BufferId> = ed.buffers.keys().collect();
    let cur = ed.current_buffer();
    let i = ids.iter().position(|b| *b == cur).unwrap_or(0) as isize;
    let n = ids.len() as isize;
    ed.show_buffer(ids[((i + d) % n + n) as usize % ids.len()]);
}

/// Command → first key sequence bound to it in any keymap (for annotations).
pub fn all_bindings(ed: &Editor) -> std::collections::HashMap<String, String> {
    let mut out = std::collections::HashMap::new();
    let active = ed.active_keymaps();
    let mut names: Vec<&String> = ed.keymaps.maps.keys().collect();
    names.sort_by_key(|n| active.iter().position(|a| a == *n).unwrap_or(usize::MAX));
    for n in names {
        for (k, c) in ed.keymaps.maps[n].flatten() {
            out.entry(c).or_insert(k);
        }
    }
    out
}

pub fn open_file(ed: &mut Editor, path: std::path::PathBuf) -> anyhow::Result<()> {
    if let Some((id, _)) = ed.buffers.iter().find(|(id, _)| {
        ed.buffers[*id]
            .buf
            .as_ref()
            .and_then(|b| b.as_any().downcast_ref::<TextBuffer>())
            .and_then(|t| t.path.as_ref())
            .is_some_and(|p| *p == path)
    }) {
        ed.show_buffer(id);
        return Ok(());
    }
    let name = path.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
    let b = TextBuffer::open(path)?;
    let id = ed.add_buffer(&name, Box::new(b) as Box<dyn Buffer>);
    ed.show_buffer(id);
    Ok(())
}

pub(crate) fn file_candidates(input: &str) -> Vec<Candidate> {
    let expanded = crate::expand_home(input);
    let s = expanded.to_string_lossy().into_owned();
    let (dir, _) = match s.rfind('/') {
        Some(i) => (s[..=i].to_string(), s[i + 1..].to_string()),
        None => ("./".to_string(), s.clone()),
    };
    let mut out = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&dir) {
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let is_dir = e.file_type().is_ok_and(|t| t.is_dir());
            let label = if is_dir { format!("{name}/") } else { name.clone() };
            out.push(Candidate::new(label.clone()).value(format!("{dir}{label}")));
        }
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

pub fn line_text_len(s: &dyn TextSurface, l: usize) -> usize {
    char_len(&s.line(l))
}
