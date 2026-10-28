//! Built-in commands. Keybinding schemes only bind these; they add no behavior of their own
//! (except vim's operator grammar).

use serde_json::Value;

use crate::buffer::{Buffer, BufferId};
use crate::buffers::chat::{ChatBuffer, Focus};
use crate::buffers::terminal::TerminalBuffer;
use crate::buffers::text::TextBuffer;
use crate::editor::{Args, BUILTIN, Editor};
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

pub fn current_chat(ed: &mut Editor) -> Option<BufferId> {
    let id = ed.current_buffer();
    (ed.meta(id)?.major == "chat").then_some(id)
}

/// Run `f` on the focused chat buffer.
/// Answer the tool call waiting for the person in the current chat.
fn answer(ed: &mut Editor, d: serpis_proto::Decision) -> anyhow::Result<()> {
    if !with_chat(ed, |c, _| c.answer(d))? {
        ed.message("Nothing is waiting for an answer");
    }
    Ok(())
}

/// Ask `questions[i]` in the minibuffer (`picked` so far, for a multiple-choice one), then the
/// next; the answers go to chat `id` after the last.
fn ask_question(ed: &mut Editor, id: BufferId, questions: Vec<Value>, i: usize, mut answers: std::collections::BTreeMap<String, String>, picked: Vec<String>) {
    let Some(q) = questions.get(i).cloned() else {
        let sent = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.answer(serpis_proto::Decision::Answers { answers })).unwrap_or(false);
        if !sent {
            ed.message("Nothing is waiting for an answer");
        }
        return;
    };
    let text = q["question"].as_str().unwrap_or("").to_string();
    let multi = q["multiSelect"].as_bool().unwrap_or(false);
    let header = q["header"].as_str().filter(|h| !h.is_empty()).map(|h| format!("{h} · ")).unwrap_or_default();
    let count = if questions.len() > 1 { format!("({}/{}) ", i + 1, questions.len()) } else { String::new() };
    let so_far = if picked.is_empty() { String::new() } else { format!(" [{}]", picked.join(", ")) };
    let prompt = format!("{count}{header}{text}{so_far}{} ", if multi { " (pick each, then Done)" } else { "" });
    const DONE: &str = "Done";
    let mut cands: Vec<Candidate> = Vec::new();
    if multi && !picked.is_empty() {
        cands.push(Candidate::new(DONE).detail(format!("answer {}", picked.join(", "))));
    }
    for o in q["options"].as_array().into_iter().flatten() {
        let label = o["label"].as_str().unwrap_or("").to_string();
        if !picked.contains(&label) {
            cands.push(Candidate::new(label).detail(o["description"].as_str().unwrap_or("").to_string()));
        }
    }
    ed.completing_read(&prompt, cands, false, move |ed, c, input| {
        // An option, or the person's own words (matching none).
        let choice = c.map(|c| c.value).unwrap_or_else(|| input.trim().to_string());
        let mut picked = picked;
        if multi && choice != DONE {
            if !choice.is_empty() {
                picked.push(choice);
            }
            return ask_question(ed, id, questions, i, answers, picked);
        }
        answers.insert(text, if multi { picked.join(", ") } else { choice });
        ask_question(ed, id, questions, i + 1, answers, vec![])
    });
}

pub fn with_chat<R>(ed: &mut Editor, f: impl FnOnce(&mut ChatBuffer, &mut Editor) -> R) -> anyhow::Result<R> {
    let id = current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
    ed.with_buffer_as::<ChatBuffer, R>(id, f).ok_or_else(|| anyhow::anyhow!("Not a chat buffer"))
}

pub fn with_terminal<R>(ed: &mut Editor, f: impl FnOnce(&mut TerminalBuffer, &mut Editor) -> R) -> anyhow::Result<R> {
    let id = ed.current_buffer();
    ed.with_buffer_as::<TerminalBuffer, R>(id, f).ok_or_else(|| anyhow::anyhow!("Not in a terminal buffer"))
}

pub const CACHE_WARN_ACTIONS: [&str; 5] = ["model", "thinking", "tools", "refresh", "expired"];

/// Run `act` on the current chat, warning first if it throws a warm prompt cache away (above
/// cache-warn-threshold, for actions in cache-warn-actions). cache-warn "confirm" asks before,
/// "notice" says so after; a before-cache-invalidate handler can veto (false) or reword it.
pub fn guard_cache(ed: &mut Editor, action: &'static str, what: &str, act: impl FnOnce(&mut Editor) + 'static) {
    let id = current_chat(ed);
    let est = id.and_then(|id| ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.cache_invalidation())).flatten();
    let Some((tokens, cost)) = est.filter(|(_, cost)| crate::buffers::chat::cache_warning_applies(ed, action, *cost)) else {
        return act(ed);
    };
    let mut msg = format!("{what} re-caches {} tokens on the next message ({})", crate::buffers::chat::fmt_tokens(tokens), crate::cache_watch::fmt_cost(cost));
    let payload = serde_json::json!({"buffer": id.map(crate::editor::buffer_id_to_u64), "action": action, "tokens": tokens, "cost": cost, "message": msg});
    for r in ed.run_hook("before-cache-invalidate", &payload) {
        match r {
            Value::Bool(false) => {
                ed.message(format!("Kept the prompt cache: a before-cache-invalidate hook stopped the {action} change"));
                return;
            }
            Value::String(m) => msg = m,
            _ => {}
        }
    }
    if ed.var_str("cache-warn").as_deref() != Some("confirm") {
        act(ed);
        ed.message(msg);
        return;
    }
    let cands = vec![Candidate::new("yes"), Candidate::new("no"), Candidate::new("always").detail(format!("yes, and stop warning about {action} changes"))];
    ed.completing_read(&format!("{msg}. Go ahead? "), cands, true, move |ed, c, _| match c.map(|c| c.value).as_deref() {
        Some("yes") => act(ed),
        Some("always") => {
            let actions: Vec<Value> = ed
                .var("cache-warn-actions")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_else(|| CACHE_WARN_ACTIONS.iter().map(|a| Value::from(*a)).collect())
                .into_iter()
                .filter(|a| a.as_str() != Some(action))
                .collect();
            ed.set_var("cache-warn-actions", Value::Array(actions));
            act(ed);
        }
        _ => ed.message("Kept the prompt cache"),
    });
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
        // The commands you use most come first, even before typing.
        ed.minibuffer_frecency("command", true);
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
    ed.define_command("reload-config", "Reload init.lua, the Lua extensions and Rust plugins (config-auto-reload does it whenever their files change).", o, |ed, _| {
        crate::lua::reload(ed);
        crate::plugins::reload_all(ed);
        Ok(())
    });
    ed.define_command("reload-extensions", "Reload the extensions: the Lua ones in <config>/extensions and Rust plugins. Lua extensions share init.lua's Lua state, so init.lua runs again with them.", o, |ed, _| {
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
    ed.define_command("select-accent", "Choose the accent color with live preview: the theme's own, one of its palette colors, or a typed #rrggbb (remembered across restarts).", o, |ed, a| {
        let choose = |ed: &mut Editor, v: &str| {
            ed.set_var("theme-accent", serde_json::json!(v));
            ed.persist_state("theme-accent", serde_json::json!(v));
        };
        if let Some(v) = a.str(0) {
            choose(ed, v);
            return Ok(());
        }
        let original = ed.var_str("theme-accent").unwrap_or_else(|| "theme".into());
        // The theme as it is without an override, to show what each choice looks like.
        ed.set_var("theme-accent", serde_json::json!("theme"));
        let own = crate::themes::hex(ed.theme.accent);
        let mut cands = vec![Candidate::new("theme").detail(format!("{own}   the theme's own"))];
        for role in crate::themes::ACCENT_ROLES {
            if let Some(c) = crate::themes::accent_color(&ed.theme, role) {
                cands.push(Candidate::new(role).detail(crate::themes::hex(c)));
            }
        }
        ed.set_var("theme-accent", serde_json::json!(original.clone()));
        ed.completing_read("Accent (or #rrggbb): ", cands, false, move |ed, c, input| {
            let v = c.map(|c| c.value).unwrap_or(input);
            if v.is_empty() {
                return;
            }
            choose(ed, &v);
            ed.message(format!("Accent: {v}"));
        });
        let restore = original.clone();
        ed.minibuffer_preview(
            Some(std::rc::Rc::new(|ed: &mut Editor, c: Option<&Candidate>| {
                if let Some(c) = c {
                    ed.set_var("theme-accent", serde_json::json!(c.value));
                }
            })),
            Some(Box::new(move |ed: &mut Editor| {
                ed.set_var("theme-accent", serde_json::json!(restore));
            })),
            true,
        );
        if let Some(mb) = &mut ed.minibuffer
            && let Some(i) = mb.filtered.iter().position(|(ci, _)| mb.candidates[*ci].value == original)
        {
            mb.selected = i;
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
        let mut ids: Vec<BufferId> = ed.listed_buffers();
        // Most recently shown first, current last.
        let hist = ed.windows[ed.focused].history.clone();
        ids.sort_by_key(|id| std::cmp::Reverse(hist.iter().position(|h| h == id).map(|p| p + 1).unwrap_or(0)));
        ids.retain(|id| *id != cur);
        if !ed.is_empty_view(cur) {
            ids.push(cur);
        }
        let cands = ids.into_iter().map(|id| ed.buffer_candidate(id)).collect();
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
        // Typing ranks buffers you go to often higher; with nothing typed the most recent
        // stays first (RET returns to it).
        ed.minibuffer_frecency("buffer", false);
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
    ed.define_command("new-chat", "Open a new agent chat in the current project. With C-u, choose the folder (scripts pass it as a parameter).", o, |ed, a| {
        crate::pickers::with_directory(ed, a, "New chat in: ", |ed, dir| {
            if let Err(e) = ed.new_buffer_in("chat", &dir) {
                ed.error(format!("{e:#}"));
            }
        });
        Ok(())
    });
    ed.define_command("new-terminal", "Open a new terminal in the current project. With C-u, choose the folder (scripts pass it as a parameter). In a chat or terminal on another machine, it opens there, in its folder (terminal-follow-machine).", o, |ed, a| {
        let here = ed.current_buffer();
        if a.str(0).is_none()
            && a.prefix.is_none()
            && ed.var("terminal-follow-machine").and_then(Value::as_bool).unwrap_or(true)
            && let Some(machine) = ed.buffer_machine(here)
        {
            let dir = ed.buffer_directory(here).map(|d| d.to_string_lossy().into_owned()).unwrap_or_else(|| "~".into());
            crate::terminals::new_terminal(ed, &machine, &dir)?;
            return Ok(());
        }
        crate::pickers::with_directory(ed, a, "New terminal in: ", |ed, dir| {
            if let Err(e) = ed.new_buffer_in("terminal", &dir) {
                ed.error(format!("{e:#}"));
            }
        });
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
    ed.define_command("yank", "Paste from the clipboard: text, files copied in a file manager, or an image.", o, |ed, _| {
        if let Some(files) = crate::clipboard::files() {
            ed.handle_files(files);
        } else if let Some(t) = crate::clipboard::get() {
            ed.handle_paste(&t);
        } else if let Some(png) = crate::clipboard::image_png() {
            ed.paste_image(&png);
        } else if let Some(t) = ed.clipboard_text() {
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
    ed.define_command("chat-allow", "Allow the tool call waiting for you (Claude Code's permission prompts), once; questions it asks are answered (chat-answer-questions).", o, |ed, a| {
        if with_chat(ed, |c, _| c.waiting().is_some_and(|w| w.1 == "AskUserQuestion"))? {
            return ed.run_command("chat-answer-questions", a);
        }
        answer(ed, serpis_proto::Decision::Allow)
    });
    ed.define_command(
        "chat-approve-plan",
        "Approve the plan Claude Code asks to carry out, choosing the permission mode it goes on in (default: asks before edits; acceptEdits; auto; ...). Param: the mode.",
        o,
        |ed, a| {
            let id = current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
            if !with_chat(ed, |c, _| c.waiting().is_some_and(|w| w.1 == "ExitPlanMode"))? {
                ed.message("No plan is waiting for approval");
                return Ok(());
            }
            let approve = move |ed: &mut Editor, mode: String| {
                ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.answer(serpis_proto::Decision::Plan { mode }));
            };
            if let Some(m) = a.str(0) {
                approve(ed, m.to_string());
                return Ok(());
            }
            let modes = [("default", "asks before edits and commands"), ("acceptEdits", "edits without asking"), ("auto", "decides itself what is safe"), ("bypassPermissions", "never asks")];
            let cands = modes.iter().map(|(m, d)| Candidate::new(*m).detail(*d)).collect();
            ed.completing_read("Approve the plan, going on in: ", cands, true, move |ed, c, _| {
                if let Some(c) = c {
                    approve(ed, c.value);
                }
            });
            Ok(())
        },
    );
    ed.define_command("chat-keep-planning", "Don't approve the plan Claude Code asks to carry out: say what should change, and it plans on. Param: what should change.", o, |ed, a| {
        let id = current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
        let send = move |ed: &mut Editor, what: String| {
            let message = Some(what).filter(|w| !w.trim().is_empty());
            ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.answer(serpis_proto::Decision::Deny { message }));
        };
        if let Some(w) = a.str(0) {
            send(ed, w.to_string());
            return Ok(());
        }
        ed.read_string("What should change? ", "", move |ed, what| send(ed, what));
        Ok(())
    });
    ed.define_command(
        "chat-answer-questions",
        "Answer the questions waiting for you (Claude Code's AskUserQuestion), one by one: pick an option or type your own answer; multiple-choice ones take picks until Done. Param: the answers, {question = answer}.",
        o,
        |ed, a| {
            let id = current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
            let Some((_, name, args, _)) = with_chat(ed, |c, _| c.waiting())? else {
                ed.message("Nothing is waiting for an answer");
                return Ok(());
            };
            if name != "AskUserQuestion" {
                ed.message("What waits isn't a question: chat-allow or chat-deny");
                return Ok(());
            }
            if let Some(given) = a.params.first().and_then(Value::as_object) {
                let answers = given.iter().map(|(q, v)| (q.clone(), v.as_str().map(str::to_string).unwrap_or_else(|| v.to_string()))).collect();
                return answer(ed, serpis_proto::Decision::Answers { answers });
            }
            let questions = args["questions"].as_array().cloned().unwrap_or_default();
            ask_question(ed, id, questions, 0, Default::default(), vec![]);
            Ok(())
        },
    );
    ed.define_command("chat-allow-always", "Allow the tool call waiting for you, and what its prompt offers from now on (\"edits in this session\", a rule for this project).", o, |ed, _| {
        answer(ed, serpis_proto::Decision::Always)
    });
    ed.define_command(
        "chat-cycle-permission-mode",
        "Switch this Claude Code chat to the next permission mode of claude-code-permission-mode-cycle (default, accept edits, plan, auto), as shift+tab does in Claude Code.",
        o,
        |ed, _| {
            let Some(mode) = with_chat(ed, |c, _| c.state_now().permission_mode)? else {
                ed.message("Only chats on Claude Code have permission modes");
                return Ok(());
            };
            let cycle: Vec<String> = ed.var("claude-code-permission-mode-cycle").and_then(|v| serde_json::from_value(v.clone()).ok()).unwrap_or_default();
            let Some(next) = cycle.iter().position(|m| *m == mode).map(|i| cycle[(i + 1) % cycle.len()].clone()).or_else(|| cycle.first().cloned()) else {
                ed.message("claude-code-permission-mode-cycle is empty");
                return Ok(());
            };
            with_chat(ed, |c, _| c.set_permission_mode(&next))?;
            ed.message(crate::buffers::chat::permission_mode_label(&next, &ed.theme).0);
            Ok(())
        },
    );
    ed.define_command(
        "chat-select-permission-mode",
        "Choose this Claude Code chat's permission mode: when it asks you before acting. Param: default, acceptEdits, plan, auto, dontAsk or bypassPermissions.",
        o,
        |ed, a| {
            let id = current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
            if with_chat(ed, |c, _| c.state_now().permission_mode)?.is_none() {
                ed.message("Only chats on Claude Code have permission modes");
                return Ok(());
            }
            let set = move |ed: &mut Editor, mode: String| {
                ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.set_permission_mode(&mode));
                ed.message(crate::buffers::chat::permission_mode_label(&mode, &ed.theme).0);
            };
            if let Some(m) = a.str(0) {
                if !serpis_chat::PERMISSION_MODES.contains(&m) {
                    anyhow::bail!("{m} isn't a permission mode (one of {})", serpis_chat::PERMISSION_MODES.join(", "));
                }
                set(ed, m.to_string());
                return Ok(());
            }
            let about = [
                ("default", "asks before edits and commands its rules don't allow"),
                ("acceptEdits", "edits without asking; asks for commands"),
                ("plan", "reads and plans, changes nothing until you approve"),
                ("auto", "decides itself what is safe, asks only for what isn't"),
                ("dontAsk", "never asks: what its rules don't allow is refused"),
                ("bypassPermissions", "never asks and allows everything"),
            ];
            let cands = about.iter().map(|(m, d)| Candidate::new(*m).detail(*d)).collect();
            ed.completing_read("Permission mode: ", cands, true, move |ed, c, _| {
                if let Some(c) = c {
                    set(ed, c.value);
                }
            });
            Ok(())
        },
    );
    ed.define_command(
        "chat-select-skill",
        "Pick one of the chat's skills (where it runs: pi's skill folders, the project's, Claude Code's) and start a message with it: /skill:name, then what you type. Param: a skill's name.",
        o,
        |ed, a| {
            let id = current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
            let (machine, chat) = with_chat(ed, |c, _| (c.node.machine.clone(), c.node.chat.clone()))?;
            let given = a.str(0).map(str::to_string);
            let start = move |ed: &mut Editor, name: String| {
                ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| {
                    c.prompt.set_text(&format!("/skill:{name} "));
                    c.prompt.move_to_end();
                });
                // Once the minibuffer has handed focus back (to the part it was taken from).
                ed.defer(move |ed| {
                    ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.focus_prompt());
                });
            };
            crate::remote::request(ed, &machine, serpis_proto::wire::Request::Skills { chat }, move |ed, r| match r {
                Ok(serpis_proto::wire::Response::Skills { skills }) if skills.is_empty() => {
                    ed.message("This chat has no skills (pi's skill folders, the project's .pi/skills or .agents/skills, agent-skill-paths)");
                }
                Ok(serpis_proto::wire::Response::Skills { skills }) => match given {
                    Some(n) if skills.iter().any(|s| s.name == n) => start(ed, n),
                    Some(n) => ed.error(format!("No skill named {n}")),
                    None => {
                        let cands = skills
                            .iter()
                            .map(|s| {
                                let from = match s.source.as_str() {
                                    "project" => " (project)",
                                    "claude" => " (Claude Code's)",
                                    _ => "",
                                };
                                Candidate::new(&s.name).detail(format!("{}{from}", s.description))
                            })
                            .collect();
                        ed.completing_read("Skill: ", cands, true, move |ed, c, _| {
                            if let Some(c) = c {
                                start(ed, c.value);
                            }
                        });
                    }
                },
                Ok(_) => ed.error("Its machine's serpis-node is older and doesn't list skills: update it"),
                Err(e) => ed.error(e),
            });
            Ok(())
        },
    );
    ed.define_command(
        "chat-background-task",
        "Move what Claude Code waits on (a command, a subagent) to the background, as Ctrl+B in its TUI: the call returns and the turn goes on while it runs. The only one, or the one you pick. Param: its tool call's id.",
        o,
        |ed, a| {
            let id = current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
            let running = with_chat(ed, |c, _| c.foreground_tasks())?;
            let send = move |ed: &mut Editor, tool: String| {
                ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.background_task(&tool));
                ed.message("Moved to the background");
            };
            if let Some(t) = a.str(0) {
                send(ed, t.to_string());
                return Ok(());
            }
            match running.len() {
                0 => ed.message("Nothing runs in the foreground (only Claude Code chats move commands to the background)"),
                1 => send(ed, running[0].0.clone()),
                _ => {
                    let cands = running.iter().map(|(tool, t)| Candidate::new(&t.description).value(tool.clone()).detail(t.kind.clone())).collect();
                    ed.completing_read("Move to the background: ", cands, true, move |ed, c, _| {
                        if let Some(c) = c {
                            send(ed, c.value);
                        }
                    });
                }
            }
            Ok(())
        },
    );
    ed.define_command(
        "chat-stop-task",
        "Stop work the agent left running in the background (Claude Code's background commands and agents): the only one, or the one you pick. Param: its task id.",
        o,
        |ed, a| {
            let id = current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
            let tasks = with_chat(ed, |c, _| c.background_tasks())?;
            let stop = move |ed: &mut Editor, task: String| {
                ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.stop_task(&task));
                ed.message("Asked it to stop");
            };
            if let Some(task) = a.str(0) {
                stop(ed, task.to_string());
                return Ok(());
            }
            match tasks.len() {
                0 => ed.message("Nothing is running in the background"),
                1 => stop(ed, tasks[0].id.clone()),
                _ => {
                    let cands = tasks.iter().map(|t| Candidate::new(t.description.clone()).detail(format!("{} · {}", if t.kind == "agent" { "agent" } else { "command" }, t.id))).collect();
                    ed.completing_read("Stop: ", cands, true, move |ed, c, _| {
                        if let Some(t) = c.and_then(|c| tasks.into_iter().find(|t| t.description == c.value)) {
                            stop(ed, t.id);
                        }
                    });
                }
            }
            Ok(())
        },
    );
    ed.define_command("chat-deny", "Refuse the tool call waiting for you. Param: what to tell the agent instead.", o, |ed, a| {
        let message = a.str(0).filter(|m| !m.trim().is_empty()).map(str::to_string);
        answer(ed, serpis_proto::Decision::Deny { message })
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
    ed.define_command("chat-clear", "Start a fresh conversation in this chat (where it runs, in its folder; the one it had stays in its session).", o, |ed, _| {
        let id = current_chat(ed).ok_or_else(|| anyhow::anyhow!("Not in a chat buffer"))?;
        crate::remote::fresh_chat(ed, id);
        Ok(())
    });
    ed.define_command(
        "chat-refresh-context",
        "Re-read AGENTS.md / CLAUDE.md and the system prompt vars with the next message. A chat keeps the system prompt it started with, so the provider can reuse its cache; refreshing rewrites the cache once.",
        o,
        |ed, _| {
            with_chat(ed, |_, _| ())?;
            guard_cache(ed, "refresh", "Refreshing the context", |ed| {
                let _ = with_chat(ed, |c, _| c.refresh_context());
                ed.message("The next message re-reads the project's instructions (rewrites the prompt cache once)");
            });
            Ok(())
        },
    );
    ed.define_command(
        "chat-compact",
        "Replace this chat's earlier messages with a summary, keeping the recent ones (chat-compact-keep-tokens). Param: what the summary should focus on. Chats also compact on their own near the context window (chat-compact-auto).",
        o,
        |ed, a| {
            let instructions = a.str(0).map(str::to_string);
            with_chat(ed, |c, _| c.compact(instructions))
        },
    );
    ed.define_command("chat-cache-report", "Show this chat's prompt cache: hit rate, whether it is warm, and its recent misses with their causes.", o, |ed, _| {
        let cache = with_chat(ed, |c, _| c.cache_json())?;
        let misses: Vec<String> = cache["misses"]
            .as_array()
            .into_iter()
            .flatten()
            .rev()
            .map(|m| format!("- cache miss: {}; rewrote {} tokens ({})", m["text"].as_str().unwrap_or(""), crate::buffers::chat::fmt_tokens(m["tokens"].as_u64().unwrap_or(0)), crate::cache_watch::fmt_cost(m["cost"].as_f64().unwrap_or(0.0))))
            .collect();
        let mut text = String::from("Prompt cache\n\n");
        text.push_str(&match cache["hit_rate"].as_f64() {
            Some(r) => format!("{:.1}% of this chat's prompt tokens were read from the cache.\n", r * 100.0),
            None => "Nothing sent yet.\n".into(),
        });
        text.push_str(&match cache["state"].as_str() {
            Some("warm") => format!("Warm: it expires in {} unless the next message reads it first.\n", crate::cache_watch::fmt_duration(std::time::Duration::from_secs(cache["left"].as_u64().unwrap_or(0)))),
            Some(_) => "Cold: the next message rewrites it.\n".into(),
            None => String::new(),
        });
        text.push_str("\nRecent misses (newest first):\n");
        text.push_str(&if misses.is_empty() { "- none".to_string() } else { misses.join("\n") });
        text.push_str("\n\nSettings: chat-cache-indicator, chat-cache-status, chat-cache-miss-notice, cache-warn, cache-warn-threshold, cache-warn-actions, agent-cache-retention.\n");
        show_page(ed, "*cache*", &text);
        Ok(())
    });
    ed.define_command(
        "claude-code-usage",
        "Show your Claude subscription's usage as Claude Code reports it (its /usage): the session and weekly limits and when they reset, extra usage, and what spends it (claude-code-usage-advice), in a page of its own (g asks again). Asked of the current chat's machine, else this one. Param: a machine.",
        o,
        |ed, a| {
            let machine = a.str(0).map(str::to_string).or_else(|| current_chat(ed).and_then(|id| ed.buffer_machine(id)));
            crate::claude_usage::show(ed, machine);
            Ok(())
        },
    );
    ed.define_command("claude-code-usage-refresh", "Ask Claude Code for its usage again, in the usage page shown.", o, |ed, _| {
        let id = ed.current_buffer();
        let Some(machine) = ed.with_buffer_as::<crate::claude_usage::UsageBuffer, _>(id, |b, _| b.machine.clone()) else {
            anyhow::bail!("Not in a Claude Code usage page");
        };
        crate::claude_usage::show(ed, machine);
        Ok(())
    });
    // Toggling tools throws the cache away (tools come first in every request): say what it costs.
    ed.add_hook("variable-changed", o, |ed, p| {
        let name = p.get("name").and_then(Value::as_str).unwrap_or_default();
        if name != "agent-disabled-tools" && name != "agent-native-web-search" {
            return None;
        }
        let id = current_chat(ed)?;
        let (tokens, cost) = ed.with_buffer_as::<ChatBuffer, _>(id, |c, _| c.cache_invalidation()).flatten()?;
        if crate::buffers::chat::cache_warning_applies(ed, "tools", cost) {
            ed.message(format!("Changing the tools re-caches {} tokens on the next message ({})", crate::buffers::chat::fmt_tokens(tokens), crate::cache_watch::fmt_cost(cost)));
        }
        None
    });
    ed.define_command("chat-toggle-thinking", "Show or hide the model's thinking in chats (chat-show-thinking).", o, |ed, _| {
        let show = !ed.var_bool("chat-show-thinking");
        ed.set_var("chat-show-thinking", serde_json::json!(show));
        ed.message(if show { "Showing thinking" } else { "Hiding thinking" });
        Ok(())
    });
    ed.define_command("chat-cycle-thinking", "Cycle the thinking level.", o, |ed, _| {
        with_chat(ed, |_, _| ())?;
        guard_cache(ed, "thinking", "Changing the thinking level", |ed| {
            if let Ok(lvl) = with_chat(ed, |c, ed| c.cycle_thinking(ed)) {
                ed.message(format!("Thinking: {}", lvl.as_str()));
            }
        });
        Ok(())
    });
    ed.define_command("chat-toggle-keep-going", "Whether this chat's turn continues by itself when its machine's serpis-node stops mid-turn (a crash, a reboot) and starts again, or waits for you (param: true or false; default: chat-keep-going).", o, |ed, a| {
        let on = a.params.first().and_then(Value::as_bool);
        let now = with_chat(ed, |c, _| {
            let on = on.unwrap_or(!c.keep_going());
            c.set_keep_going(on);
            on
        })?;
        ed.message(if now { "This chat keeps going after its node restarts" } else { "This chat waits for you after its node restarts" });
        Ok(())
    });
    ed.define_command("chat-toggle-continue-after-limit", "Whether this chat, stopped at a Claude Code usage limit, goes on by itself when the limit resets, or waits for you (param: true or false; default: claude-code-continue-after-limit).", o, |ed, a| {
        let on = a.params.first().and_then(Value::as_bool);
        let now = with_chat(ed, |c, _| {
            c.held_at_limit().map(|(_, continues)| {
                let on = on.unwrap_or(continues.is_none());
                c.set_continue_after_limit(on);
                on
            })
        })?;
        match now {
            Some(true) => ed.message("This chat goes on by itself when the limit resets"),
            Some(false) => ed.message("This chat waits for you after the limit resets"),
            None => anyhow::bail!("This chat isn't stopped at a usage limit"),
        }
        Ok(())
    });
    ed.define_command("chat-select-thinking", "Choose the thinking level from a picker (param: level, e.g. \"high\").", o, |ed, a| {
        if let Some(level) = a.str(0).map(str::to_string) {
            let id = ed.chat_id(None)?;
            serpis_ai::ThinkingLevel::parse(&level).ok_or_else(|| anyhow::anyhow!("Unknown thinking level {level}"))?;
            guard_cache(ed, "thinking", "Changing the thinking level", move |ed| {
                if let Err(e) = ed.set_chat_thinking(id, &level) {
                    ed.error(format!("{e:#}"));
                }
            });
            return Ok(());
        }
        let (levels, current) = with_chat(ed, |c, ed| (c.thinking_levels(ed), c.thinking_level()))?;
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
            if l == current {
                return;
            }
            guard_cache(ed, "thinking", "Changing the thinking level", move |ed| {
                let Ok(l) = with_chat(ed, |chat, _| {
                    chat.set_thinking(l);
                    chat.thinking_level()
                }) else { return };
                ed.message(format!("Thinking: {}", l.as_str()));
            });
        });
        Ok(())
    });
    ed.define_command("chat-toggle-fold", "Expand or collapse the tool output or thinking at point (TAB, or click it).", o, |ed, _| {
        with_chat(ed, |c, _| {
            c.toggle_fold_at_cursor();
        })
    });
    ed.define_command("chat-focus-prompt", "Move to the prompt (motions and edits act on it).", o, |ed, _| {
        with_chat(ed, |c, _| {
            c.focus = Focus::Prompt;
        })
    });
    ed.define_command("chat-focus-transcript", "Move to the transcript (to read, search and copy; i/a/o return to the prompt).", o, |ed, _| {
        with_chat(ed, |c, _| {
            c.focus = Focus::Transcript;
            c.view.reveal_cursor();
        })
    });
    ed.define_command("chat-toggle-focus", "Switch between the prompt and the transcript.", o, |ed, _| {
        with_chat(ed, |c, _| {
            c.focus = if c.focus == Focus::Prompt { Focus::Transcript } else { Focus::Prompt };
        })
    });
    ed.define_command("chat-history-previous", "Recall the previous prompt.", o, |ed, _| with_chat(ed, |c, _| c.history_prev()));
    ed.define_command("chat-history-next", "Recall the next prompt.", o, |ed, _| with_chat(ed, |c, _| c.history_next()));
    ed.define_command("chat-select-model", "Choose the model for this chat (param: provider/model-id).", o, |ed, a| {
        if let Some(spec) = a.str(0).map(str::to_string) {
            let id = ed.chat_id(None)?;
            guard_cache(ed, "model", "Switching the model", move |ed| {
                if let Err(e) = ed.set_chat_model(id, &spec) {
                    ed.error(format!("{e:#}"));
                }
            });
            return Ok(());
        }
        // A chat on another machine uses that machine's models and keys.
        if let Some(id) = current_chat(ed)
            && let Some(machine) = ed.buffer_machine(id)
        {
            let m = machine.clone();
            crate::remote::request(ed, &machine, serpis_proto::wire::Request::Models, move |ed, r| match r {
                Ok(serpis_proto::wire::Response::Models { models }) => {
                    let cands = models.into_iter().map(|i| Candidate::new(i.model).detail(format!("{}{}", i.name, if i.reasoning { "  ◆" } else { "" }))).collect();
                    ed.completing_read(&format!("Model on {m}: "), cands, true, move |ed, c, _| {
                        if let Some(c) = c {
                            ed.with_buffer_as::<ChatBuffer, _>(id, |chat, _| chat.set_model_spec(&c.value));
                        }
                    });
                }
                Ok(other) => ed.error(format!("{m}: unexpected answer {other:?}")),
                Err(e) => ed.error(format!("{m}: {e}")),
            });
            return Ok(());
        }
        let configured = ed.registry.configured_providers();
        let mut models: Vec<serpis_ai::Model> = ed
            .registry
            .models()
            .into_iter()
            .filter(|m| configured.contains(&m.provider))
            .filter(|m| serpis_ai::api::chat_api(&m.api))
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
            let same = with_chat(ed, |chat, _| chat.state_now().model.as_deref() == Some(c.value.as_str())).unwrap_or(false);
            let change = move |ed: &mut Editor| {
                ed.set_var("agent-model", Value::String(c.value.clone()));
                let _ = with_chat(ed, |chat, _| chat.set_model(m));
                ed.message(format!("Model: {}", c.value));
            };
            if same { change(ed) } else { guard_cache(ed, "model", "Switching the model", change) }
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
                    // This machine's chats use it too.
                    crate::remote::send(ed, crate::remote::LOCAL, serpis_proto::wire::Request::ReloadModels);
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
    let ids: Vec<BufferId> = ed.listed_buffers();
    if ids.is_empty() {
        return;
    }
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
