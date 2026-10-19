//! Vim emulation (evil-style), built as a package on the public API.
//!
//! States are buffer minor modes (`vim-normal`, `vim-insert`, `vim-visual`, `vim-visual-line`),
//! so every state has keymaps (`vim-normal`, `vim-normal:chat`, ...) that users can extend.
//! The operator/count/motion/text-object grammar runs as a key interceptor over the buffer's
//! [`TextSurface`], so it works identically in text buffers, chat (prompt and transcript) and
//! terminal scrollback.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::{Value, json};

use crate::buffer::BufferId;
use crate::commands::{goal_col, remember_goal, repeat_search, set_last_search};
use crate::editor::{Args, Editor, KeyResult, buffer_id_from_u64};
use crate::keys::{Key, KeyChord};
use crate::motion::{self, text_object};
use crate::text::{Pos, char_len, ordered};

pub const OWNER: &str = "vim";
const STATES: [&str; 4] = ["vim-normal", "vim-insert", "vim-visual", "vim-visual-line"];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Normal,
    Insert,
    Visual,
    VisualLine,
}

impl State {
    fn mode(self) -> &'static str {
        match self {
            State::Normal => "vim-normal",
            State::Insert => "vim-insert",
            State::Visual => "vim-visual",
            State::VisualLine => "vim-visual-line",
        }
    }
    fn cursor(self) -> &'static str {
        match self {
            State::Insert => "bar",
            _ => "block",
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            State::Normal => "NORMAL",
            State::Insert => "INSERT",
            State::Visual => "VISUAL",
            State::VisualLine => "V-LINE",
        }
    }
}

#[derive(Default)]
struct Pending {
    count: Option<usize>,
    op_count: Option<usize>,
    op: Option<char>,
    /// `g`, `z`, `f/F/t/T`, `r`, `i`/`a` (text object), `Z`.
    prefix: Option<char>,
    keys: Vec<KeyChord>,
}

struct Vim {
    pending: Pending,
    last_find: Option<(char, bool, bool)>,
    last_change: Vec<KeyChord>,
    replaying: bool,
    yank_linewise: bool,
}

thread_local! {
    static VIM: RefCell<Option<Rc<RefCell<Vim>>>> = const { RefCell::new(None) };
}

fn vim() -> Option<Rc<RefCell<Vim>>> {
    VIM.with(|v| v.borrow().clone())
}

pub fn state_of(ed: &Editor, id: BufferId) -> Option<State> {
    let meta = ed.meta(id)?;
    [State::Normal, State::Insert, State::Visual, State::VisualLine].into_iter().find(|s| meta.has_mode(s.mode()))
}

pub fn set_state(ed: &mut Editor, id: BufferId, st: State) {
    let Some(meta) = ed.meta_mut(id) else { return };
    meta.minor_modes.retain(|m| !STATES.contains(&m.as_str()));
    meta.minor_modes.insert(0, st.mode().to_string());
    meta.vars.insert("cursor-shape".into(), json!(st.cursor()));
    let major = meta.major.clone();
    // Terminals: normal/visual states navigate scrollback, insert sends keys.
    if major == "terminal" {
        let _ = crate::commands::with_terminal(ed, |t, _| match st {
            State::Insert => t.exit_copy_mode(),
            _ => t.enter_copy_mode(),
        });
    }
    if st == State::Normal || st == State::Insert {
        ed.with_buffer(id, |b, _| {
            if let Some(s) = b.surface() {
                s.set_anchor(None);
            }
        });
    }
    ed.run_hook("vim-state-changed", &json!({"state": st.label()}));
}

fn initial_state(ed: &Editor, major: &str) -> State {
    let key = format!("vim-initial-state-{major}");
    match ed.var_str(&key).as_deref() {
        Some("insert") => State::Insert,
        Some("normal") => State::Normal,
        _ => match major {
            "chat" | "terminal" => State::Insert,
            _ => State::Normal,
        },
    }
}

pub fn enable(ed: &mut Editor) {
    VIM.with(|v| {
        *v.borrow_mut() = Some(Rc::new(RefCell::new(Vim {
            pending: Pending::default(),
            last_find: None,
            last_change: Vec::new(),
            replaying: false,
            yank_linewise: false,
        })))
    });
    let ids: Vec<BufferId> = ed.buffers.keys().collect();
    for id in ids {
        let major = ed.meta(id).map(|m| m.major.clone()).unwrap_or_default();
        let st = initial_state(ed, &major);
        set_state(ed, id, st);
    }
    ed.add_hook("buffer-created", OWNER, |ed, p| {
        if let Some(id) = p.get("buffer").and_then(Value::as_u64).map(buffer_id_from_u64) {
            let major = ed.meta(id).map(|m| m.major.clone()).unwrap_or_default();
            let st = initial_state(ed, &major);
            set_state(ed, id, st);
        }
        None
    });
    ed.add_interceptor(OWNER, intercept);
    install_commands(ed);
    install_keymaps(ed);
}

pub fn disable(ed: &mut Editor) {
    VIM.with(|v| *v.borrow_mut() = None);
    ed.remove_owner(OWNER);
    let ids: Vec<BufferId> = ed.buffers.keys().collect();
    for id in ids {
        if let Some(m) = ed.meta_mut(id) {
            m.minor_modes.retain(|x| !STATES.contains(&x.as_str()));
            m.vars.remove("cursor-shape");
        }
    }
}

fn install_keymaps(ed: &mut Editor) {
    let o = OWNER;
    for (k, c) in [
        ("ESC", "vim-normal-state"),
        ("C-[", "vim-normal-state"),
        ("C-w", "backward-kill-word"),
        ("C-u", "vim-delete-to-line-start"),
        ("C-r", "yank"),
    ] {
        ed.bind("vim-insert", k, c, o);
    }
    // In terminals, Escape leaves insert state; C-c ESC sends a real escape.
    ed.bind("vim-insert:terminal", "C-w", "vim-terminal-passthrough", o);
    ed.bind("vim-insert:terminal", "C-u", "vim-terminal-passthrough", o);
    ed.bind("vim-insert:terminal", "C-r", "vim-terminal-passthrough", o);
    for (k, c) in [
        (":", "vim-ex"),
        ("C-w h", "windmove-left"),
        ("C-w j", "windmove-down"),
        ("C-w k", "windmove-up"),
        ("C-w l", "windmove-right"),
        ("C-w <left>", "windmove-left"),
        ("C-w <down>", "windmove-down"),
        ("C-w <up>", "windmove-up"),
        ("C-w <right>", "windmove-right"),
        ("C-w w", "other-window"),
        ("C-w C-w", "other-window"),
        ("C-w W", "previous-window"),
        ("C-w s", "split-window-below"),
        ("C-w v", "split-window-right"),
        ("C-w c", "delete-window"),
        ("C-w q", "delete-window"),
        ("C-w o", "delete-other-windows"),
        ("C-w =", "balance-windows"),
        ("C-w >", "enlarge-window-horizontally"),
        ("C-w <", "shrink-window-horizontally"),
        ("C-w +", "enlarge-window"),
        ("C-w -", "shrink-window"),
        ("SPC SPC", "execute-extended-command"),
        ("SPC :", "execute-extended-command"),
        ("SPC b b", "switch-to-buffer"),
        ("SPC b d", "kill-current-buffer"),
        ("SPC b n", "next-buffer"),
        ("SPC b p", "previous-buffer"),
        ("SPC f f", "find-file"),
        ("SPC f s", "save-buffer"),
        ("SPC f p", "open-config"),
        ("SPC w v", "split-window-right"),
        ("SPC w s", "split-window-below"),
        ("SPC w d", "delete-window"),
        ("SPC w m", "delete-other-windows"),
        ("SPC w w", "other-window"),
        ("SPC a c", "new-chat"),
        ("SPC a m", "chat-select-model"),
        ("SPC a t", "chat-cycle-thinking"),
        ("SPC o t", "new-terminal"),
        ("SPC h k", "describe-key"),
        ("SPC h f", "describe-command"),
        ("SPC h v", "describe-variable"),
        ("SPC h m", "describe-mode"),
        ("SPC h b", "describe-keymap"),
        ("SPC h e", "view-messages"),
        ("SPC t s", "set-keybinding-scheme"),
        ("SPC t t", "toggle-theme"),
        ("SPC q r", "reload-config"),
        ("SPC q q", "quit"),
        ("C-r", "redo"),
    ] {
        ed.bind("vim-normal", k, c, o);
    }
    for (k, c) in [("ESC", "vim-normal-state"), ("C-[", "vim-normal-state"), (":", "vim-ex")] {
        ed.bind("vim-visual", k, c, o);
        ed.bind("vim-visual-line", k, c, o);
    }
    for (k, c) in [("RET", "chat-send"), ("TAB", "chat-toggle-fold"), ("C-c C-c", "chat-abort")] {
        ed.bind("vim-normal:chat", k, c, o);
    }
    ed.bind("vim-normal:terminal", "p", "yank", o);
}

fn install_commands(ed: &mut Editor) {
    let o = OWNER;
    ed.define_command("vim-normal-state", "Enter vim normal state.", o, |ed, _| {
        let id = ed.current_buffer();
        let was_insert = state_of(ed, id) == Some(State::Insert);
        set_state(ed, id, State::Normal);
        if was_insert {
            // Like vim, leaving insert moves the cursor back one column.
            ed.with_surface(|s, _| {
                let c = s.cursor();
                if c.col > 0 && s.editable_at(c) {
                    s.set_cursor(Pos::new(c.line, c.col - 1));
                }
            });
        }
        if let Some(v) = vim() {
            v.borrow_mut().pending = Pending::default();
        }
        Ok(())
    });
    ed.define_command("vim-insert-state", "Enter vim insert state.", o, |ed, _| {
        enter_insert(ed, None);
        Ok(())
    });
    ed.define_command("vim-delete-to-line-start", "Delete from line start to point.", o, |ed, _| {
        if let Some((a, b)) = ed.with_surface(|s, _| (Pos::new(s.cursor().line, 0), s.cursor())) {
            crate::commands::kill(ed, a, b, false);
        }
        Ok(())
    });
    ed.define_command("vim-terminal-passthrough", "Send the key to the terminal unchanged.", o, |ed, a: &Args| {
        if let Some(k) = a.keys.last().cloned() {
            let id = ed.current_buffer();
            ed.with_buffer(id, |b, ed| b.handle_key(&k, ed));
        }
        Ok(())
    });
    ed.define_command("vim-ex", "Run an ex command (:w, :q, :e FILE, :sp, :vs, :NUM, or any command name).", o, |ed, _| {
        ed.completing_read(":", ex_candidates(ed), false, |ed, c, input| {
            let line = if input.trim().is_empty() { c.map(|c| c.value).unwrap_or_default() } else { input };
            ex(ed, line.trim());
        });
        Ok(())
    });
}

fn ex_candidates(ed: &Editor) -> Vec<crate::minibuffer::Candidate> {
    let mut names: Vec<String> = ed.commands.keys().cloned().collect();
    names.sort();
    names.into_iter().map(crate::minibuffer::Candidate::new).collect()
}

fn ex(ed: &mut Editor, line: &str) {
    let (cmd, arg) = line.split_once(' ').map(|(a, b)| (a, b.trim())).unwrap_or((line, ""));
    if let Ok(n) = cmd.parse::<usize>() {
        ed.with_surface(|s, _| {
            let p = motion::line_first_nonblank(s, n.saturating_sub(1).min(s.line_count().saturating_sub(1)));
            s.set_cursor(p);
        });
        return;
    }
    let run = |ed: &mut Editor, c: &str| ed.call(c);
    match cmd {
        "w" | "write" => run(ed, "save-buffer"),
        "q" | "quit" | "clo" | "close" => {
            if !ed.delete_window(ed.focused) {
                run(ed, "kill-current-buffer");
            }
        }
        "wq" | "x" => {
            run(ed, "save-buffer");
            if !ed.delete_window(ed.focused) {
                run(ed, "kill-current-buffer");
            }
        }
        "qa" | "qall" | "qa!" => run(ed, "quit"),
        "e" | "edit" if !arg.is_empty() => {
            let path = crate::expand_home(arg);
            let path = if path.is_absolute() { path } else { ed.cwd.join(path) };
            if let Err(e) = crate::commands::open_file(ed, path) {
                ed.error(format!("{e:#}"));
            }
        }
        "e" | "edit" => run(ed, "find-file"),
        "sp" | "split" => run(ed, "split-window-below"),
        "vs" | "vsp" | "vsplit" => run(ed, "split-window-right"),
        "bd" | "bdelete" => run(ed, "kill-current-buffer"),
        "bn" | "bnext" => run(ed, "next-buffer"),
        "bp" | "bprevious" => run(ed, "previous-buffer"),
        "b" | "buffer" | "ls" => run(ed, "switch-to-buffer"),
        "noh" | "nohlsearch" => {}
        "term" | "terminal" => run(ed, "new-terminal"),
        "chat" => run(ed, "new-chat"),
        "model" => run(ed, "chat-select-model"),
        "set" => {
            if let Some((k, v)) = arg.split_once('=') {
                let val = serde_json::from_str(v).unwrap_or(Value::String(v.to_string()));
                ed.set_var(k.trim(), val);
            }
        }
        "s" | "%s" => ed.message("substitute is not implemented yet"),
        other => {
            let mut args = Args::default();
            for a in arg.split_whitespace() {
                args.params.push(Value::String(a.to_string()));
            }
            if let Err(e) = ed.run_command(other, &args) {
                ed.error(format!("{e:#}"));
            }
        }
    }
}

fn enter_insert(ed: &mut Editor, at: Option<Pos>) {
    let id = ed.current_buffer();
    ed.with_surface(|s, _| {
        if let Some(p) = at {
            s.set_cursor(p);
        }
        // Read-only positions (chat transcript): jump to the editable end.
        let c = s.cursor();
        if !s.editable_at(c) {
            let end = motion::doc_end(s);
            if s.editable_at(end) {
                s.set_cursor(end);
            }
        }
    });
    set_state(ed, id, State::Insert);
}

fn digit(c: &KeyChord) -> Option<usize> {
    match c.key {
        Key::Char(d @ '0'..='9') if c.mods.is_empty() => d.to_digit(10).map(|d| d as usize),
        _ => None,
    }
}

/// The interceptor: runs for chords that no keymap claimed.
fn intercept(ed: &mut Editor, chord: &KeyChord) -> KeyResult {
    let id = ed.current_buffer();
    let Some(st) = state_of(ed, id) else { return KeyResult::Pass };
    if st == State::Insert {
        return KeyResult::Pass;
    }
    let Some(v) = vim() else { return KeyResult::Pass };
    // Prime surfaces that lazily load text (terminal scrollback).
    ed.with_surface(|s, _| s.undo_boundary());
    let done = {
        let mut vm = v.borrow_mut();
        vm.pending.keys.push(chord.clone());
        handle(ed, &mut vm, st, chord)
    };
    let mut vm = v.borrow_mut();
    match done {
        Step::Pending => {}
        Step::Done { change } => {
            let keys = std::mem::take(&mut vm.pending.keys);
            if change && !vm.replaying {
                vm.last_change = keys;
            }
            vm.pending = Pending::default();
        }
        Step::Invalid => vm.pending = Pending::default(),
    }
    KeyResult::Handled
}

enum Step {
    Pending,
    Done { change: bool },
    Invalid,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Exclusive,
    Inclusive,
    Line,
}

/// Resolve a motion key to a target position.
fn motion_target(ed: &mut Editor, vm: &mut Vim, c: char, count: usize, has_count: bool) -> Option<(Pos, Kind)> {
    let last = ed.last_command.clone();
    ed.with_surface(|s, _| {
        let p = s.cursor();
        let rep = |f: &dyn Fn(Pos) -> Pos| (0..count).fold(p, |q, _| f(q));
        Some(match c {
            'h' => (Pos::new(p.line, p.col.saturating_sub(count)), Kind::Exclusive),
            'l' => (Pos::new(p.line, (p.col + count).min(s.line_len(p.line))), Kind::Exclusive),
            'j' | 'k' => {
                let goal = goal_col(p, last.as_deref());
                remember_goal(goal);
                let d = if c == 'j' { count as isize } else { -(count as isize) };
                (motion::vertical(s, p, d, goal), Kind::Line)
            }
            'w' => (rep(&|q| motion::word_forward(s, q, false)), Kind::Exclusive),
            'W' => (rep(&|q| motion::word_forward(s, q, true)), Kind::Exclusive),
            'b' => (rep(&|q| motion::word_backward(s, q, false)), Kind::Exclusive),
            'B' => (rep(&|q| motion::word_backward(s, q, true)), Kind::Exclusive),
            'e' => (rep(&|q| motion::word_end(s, q, false)), Kind::Inclusive),
            'E' => (rep(&|q| motion::word_end(s, q, true)), Kind::Inclusive),
            '0' => (Pos::new(p.line, 0), Kind::Exclusive),
            '^' | '_' => (motion::line_first_nonblank(s, p.line), Kind::Exclusive),
            '$' => {
                let l = (p.line + count - 1).min(s.line_count().saturating_sub(1));
                (Pos::new(l, s.line_len(l).saturating_sub(1)), Kind::Inclusive)
            }
            'G' => {
                let l = if has_count { count.saturating_sub(1) } else { s.line_count().saturating_sub(1) };
                (motion::line_first_nonblank(s, l.min(s.line_count().saturating_sub(1))), Kind::Line)
            }
            '}' => (rep(&|q| motion::paragraph(s, q, true)), Kind::Exclusive),
            '{' => (rep(&|q| motion::paragraph(s, q, false)), Kind::Exclusive),
            '%' => (motion::matching_bracket(s, p)?, Kind::Inclusive),
            'H' => (Pos::new(s.visible_lines().start, 0), Kind::Line),
            'L' => (Pos::new(s.visible_lines().end.saturating_sub(1).min(s.line_count().saturating_sub(1)), 0), Kind::Line),
            'M' => {
                let r = s.visible_lines();
                (Pos::new(((r.start + r.end) / 2).min(s.line_count().saturating_sub(1)), 0), Kind::Line)
            }
            '+' | '\n' => (motion::line_first_nonblank(s, (p.line + count).min(s.line_count().saturating_sub(1))), Kind::Line),
            '-' => (motion::line_first_nonblank(s, p.line.saturating_sub(count)), Kind::Line),
            ';' | ',' => {
                let (ch, fwd, till) = vm.last_find?;
                let fwd = if c == ',' { !fwd } else { fwd };
                (motion::find_in_line(s, p, ch, fwd, till, count)?, if fwd { Kind::Inclusive } else { Kind::Exclusive })
            }
            _ => return None,
        })
    })
    .flatten()
}

fn is_motion_key(c: char) -> bool {
    "hjklwWbBeE0^_$G}{%HLM+-;,".contains(c)
}

fn apply_operator(ed: &mut Editor, vm: &mut Vim, op: char, a: Pos, b: Pos, kind: Kind) -> bool {
    let (start, end, linewise) = ed
        .with_surface(|s, _| {
            let (a, b) = ordered(a, b);
            match kind {
                Kind::Line => {
                    let last = s.line_count().saturating_sub(1);
                    let end = if b.line < last { Pos::new(b.line + 1, 0) } else { motion::line_end(s, b.line) };
                    // Deleting the last line also removes the newline before it.
                    let start = if b.line >= last && a.line > 0 && matches!(op, 'd') { motion::line_end(s, a.line - 1) } else { Pos::new(a.line, 0) };
                    (start, end, true)
                }
                Kind::Inclusive => (a, motion::next_pos(s, b).unwrap_or(Pos::new(b.line, b.col + 1)), false),
                Kind::Exclusive => (a, b, false),
            }
        })
        .unwrap_or((a, b, false));
    let text = ed.with_surface(|s, _| s.text_range(start, end)).unwrap_or_default();
    match op {
        'y' => {
            let t = if linewise && !text.ends_with('\n') { format!("{text}\n") } else { text };
            ed.copy_to_clipboard(&t);
            vm.yank_linewise = linewise;
            ed.with_surface(|s, _| {
                s.set_anchor(None);
                s.set_cursor(if linewise { Pos::new(start.line.min(a.line.min(b.line)), s.cursor().col.min(start.col)) } else { start });
            });
            false
        }
        'd' | 'c' => {
            let editable = ed.with_surface(|s, _| s.editable_at(start)).unwrap_or(false);
            if !editable {
                return false;
            }
            let t = if linewise && !text.ends_with('\n') { format!("{text}\n") } else { text };
            ed.copy_to_clipboard(&t);
            vm.yank_linewise = linewise;
            if op == 'c' && linewise {
                // `cc` keeps an empty line to type on.
                let (s0, e0) = ed
                    .with_surface(|s, _| {
                        let (a, b) = ordered(a, b);
                        (motion::line_first_nonblank(s, a.line), motion::line_end(s, b.line))
                    })
                    .unwrap();
                crate::commands::kill(ed, s0, e0, false);
            } else {
                crate::commands::kill(ed, start, end, false);
            }
            if op == 'c' {
                enter_insert(ed, None);
            } else if linewise {
                ed.with_surface(|s, _| {
                    let l = start.line.min(s.line_count().saturating_sub(1));
                    let p = motion::line_first_nonblank(s, if start.col > 0 { (l + 1).min(s.line_count() - 1) } else { l });
                    s.set_cursor(p);
                });
            }
            true
        }
        '>' | '<' => {
            ed.with_surface(|s, _| {
                let (a, b) = ordered(a, b);
                for l in a.line..=b.line {
                    if !s.editable_at(Pos::new(l, 0)) {
                        continue;
                    }
                    if op == '>' {
                        if !s.line(l).is_empty() {
                            s.insert(Pos::new(l, 0), "    ");
                        }
                    } else {
                        let n = s.line(l).chars().take(4).take_while(|c| *c == ' ').count();
                        s.delete(Pos::new(l, 0), Pos::new(l, n));
                    }
                }
                let p = motion::line_first_nonblank(s, a.line);
                s.set_cursor(p);
            });
            true
        }
        '~' | 'u' | 'U' => {
            let mapped: String = text
                .chars()
                .map(|c| match op {
                    'u' => c.to_lowercase().next().unwrap_or(c),
                    'U' => c.to_uppercase().next().unwrap_or(c),
                    _ if c.is_uppercase() => c.to_lowercase().next().unwrap_or(c),
                    _ => c.to_uppercase().next().unwrap_or(c),
                })
                .collect();
            ed.with_surface(|s, _| {
                if s.editable_at(start) {
                    s.delete(start, end);
                    s.insert(start, &mapped);
                    s.set_cursor(start);
                }
            });
            true
        }
        _ => false,
    }
}

fn paste(ed: &mut Editor, vm: &Vim, after: bool, count: usize) -> bool {
    let Some(text) = ed.clipboard_text() else { return false };
    let id = ed.current_buffer();
    if ed.meta(id).is_some_and(|m| m.major == "terminal") {
        ed.handle_paste(&text);
        return false;
    }
    let linewise = vm.yank_linewise || text.ends_with('\n');
    ed.with_surface(|s, _| {
        let c = s.cursor();
        if !s.editable_at(c) {
            return false;
        }
        let body = text.repeat(count);
        if linewise {
            let body = body.trim_end_matches('\n').to_string();
            if after {
                let at = motion::line_end(s, c.line);
                s.insert(at, &format!("\n{body}"));
                let p = motion::line_first_nonblank(s, c.line + 1);
                s.set_cursor(p);
            } else {
                s.insert(Pos::new(c.line, 0), &format!("{body}\n"));
                let p = motion::line_first_nonblank(s, c.line);
                s.set_cursor(p);
            }
        } else {
            let at = if after && s.line_len(c.line) > 0 { Pos::new(c.line, c.col + 1) } else { c };
            let end = s.insert(at, &body);
            s.set_cursor(Pos::new(end.line, end.col.saturating_sub(1)));
        }
        true
    })
    .unwrap_or(false)
}

fn handle(ed: &mut Editor, vm: &mut Vim, st: State, chord: &KeyChord) -> Step {
    let id = ed.current_buffer();
    // Counts.
    if let Some(d) = digit(chord) {
        if vm.pending.prefix.is_none() && (d != 0 || vm.pending.count.is_some()) {
            vm.pending.count = Some(vm.pending.count.unwrap_or(0) * 10 + d);
            return Step::Pending;
        }
    }
    let count_given = vm.pending.count.is_some() || vm.pending.op_count.is_some();
    let count = vm.pending.count.unwrap_or(1) * vm.pending.op_count.unwrap_or(1);
    let visual = matches!(st, State::Visual | State::VisualLine);

    let ch = match (&chord.key, chord.mods.is_command()) {
        (Key::Char(c), false) => Some(*c),
        (Key::Named(n), false) if n == "return" => Some('\n'),
        (Key::Named(n), false) if n == "space" => Some(' '),
        _ => None,
    };

    // Control keys in normal/visual state.
    if ch.is_none() {
        let page = ed.with_surface(|s, _| s.page_lines()).unwrap_or(20);
        let delta: Option<isize> = match (&chord.key, chord.mods.ctrl) {
            (Key::Char('d'), true) => Some((page / 2) as isize),
            (Key::Char('u'), true) => Some(-((page / 2) as isize)),
            (Key::Char('f'), true) => Some(page as isize),
            (Key::Char('b'), true) => Some(-(page as isize)),
            (Key::Char('e'), true) => Some(1),
            (Key::Char('y'), true) => Some(-1),
            _ => None,
        };
        if let Some(d) = delta {
            ed.with_surface(|s, _| {
                let c = s.cursor();
                let p = motion::vertical(s, c, d, c.col);
                s.set_cursor(p);
            });
            return Step::Done { change: false };
        }
        if chord.mods.ctrl && chord.key == Key::Char('v') {
            return Step::Done { change: false };
        }
        // Arrow keys act as motions.
        let mapped = match &chord.key {
            Key::Named(n) if n == "left" => Some('h'),
            Key::Named(n) if n == "right" => Some('l'),
            Key::Named(n) if n == "up" => Some('k'),
            Key::Named(n) if n == "down" => Some('j'),
            _ => None,
        };
        let Some(m) = mapped else { return Step::Invalid };
        return do_motion(ed, vm, st, m, count, count_given);
    }
    let c = ch.unwrap();

    // Prefix continuations.
    if let Some(prefix) = vm.pending.prefix.take() {
        match prefix {
            'f' | 'F' | 't' | 'T' => {
                let fwd = prefix == 'f' || prefix == 't';
                let till = prefix == 't' || prefix == 'T';
                vm.last_find = Some((c, fwd, till));
                if let Some(op) = vm.pending.op {
                    // `dt)`, `cf,`: the find is the operator's motion.
                    let start = ed.with_surface(|s, _| s.cursor()).unwrap_or_default();
                    let Some((t, kind)) = motion_target(ed, vm, ';', count, count_given) else { return Step::Invalid };
                    let change = apply_operator(ed, vm, op, start, t, kind);
                    return Step::Done { change };
                }
                return do_motion(ed, vm, st, ';', count, count_given);
            }
            'r' => {
                let ok = ed
                    .with_surface(|s, _| {
                        let p = s.cursor();
                        if !s.editable_at(p) || p.col >= s.line_len(p.line) {
                            return false;
                        }
                        let n = count.min(s.line_len(p.line) - p.col);
                        s.delete(p, Pos::new(p.line, p.col + n));
                        s.insert(p, &c.to_string().repeat(n));
                        s.set_cursor(Pos::new(p.line, p.col + n - 1));
                        true
                    })
                    .unwrap_or(false);
                return Step::Done { change: ok };
            }
            'g' => {
                return match c {
                    'g' => {
                        let line = if count_given { count - 1 } else { 0 };
                        let target = ed.with_surface(|s, _| motion::line_first_nonblank(s, line.min(s.line_count().saturating_sub(1))));
                        match target {
                            Some(t) => finish_motion(ed, vm, st, t, Kind::Line),
                            None => Step::Invalid,
                        }
                    }
                    'e' | 'E' => {
                        let big = c == 'E';
                        let t = ed.with_surface(|s, _| {
                            let mut p = s.cursor();
                            for _ in 0..count {
                                // Previous word end: back to word start, then one char before.
                                let start = motion::word_backward(s, p, big);
                                p = motion::prev_pos(s, start).unwrap_or(start);
                            }
                            p
                        });
                        match t {
                            Some(t) => finish_motion(ed, vm, st, t, Kind::Inclusive),
                            None => Step::Invalid,
                        }
                    }
                    '~' | 'u' | 'U' if vm.pending.op.is_none() && !visual => {
                        vm.pending.op = Some(c);
                        vm.pending.op_count = vm.pending.count.take();
                        Step::Pending
                    }
                    'u' | 'U' | '~' if visual => {
                        let sel = ed.with_surface(|s, _| s.anchor().map(|a| (a, s.cursor()))).flatten();
                        if let Some((a, b)) = sel {
                            let kind = if st == State::VisualLine { Kind::Line } else { Kind::Inclusive };
                            apply_operator(ed, vm, c, a, b, kind);
                            set_state(ed, id, State::Normal);
                        }
                        Step::Done { change: true }
                    }
                    't' => {
                        ed.call("next-buffer");
                        Step::Done { change: false }
                    }
                    'T' => {
                        ed.call("previous-buffer");
                        Step::Done { change: false }
                    }
                    'i' => {
                        enter_insert(ed, None);
                        Step::Done { change: true }
                    }
                    _ => Step::Invalid,
                };
            }
            'z' => {
                // zz/zt/zb: only reveal the cursor (views center on reveal margins).
                ed.with_surface(|s, _| s.reveal_cursor());
                return Step::Done { change: false };
            }
            'i' | 'a' => {
                let Some(op) = vm.pending.op else { return Step::Invalid };
                let obj = ed.with_surface(|s, _| text_object(s, s.cursor(), prefix == 'i', c)).flatten();
                let Some((a, b)) = obj else { return Step::Invalid };
                let kind = if c == 'p' { Kind::Line } else { Kind::Exclusive };
                let b = if kind == Kind::Line { Pos::new(b.line.saturating_sub(usize::from(b.col == 0 && b.line > a.line)), 0) } else { b };
                let change = apply_operator(ed, vm, op, a, b, kind);
                return Step::Done { change };
            }
            'v' => {
                // Visual text objects: viw, va", ...
                let obj = ed.with_surface(|s, _| text_object(s, s.cursor(), vm.pending.keys.len() >= 2 && vm.pending.keys[vm.pending.keys.len() - 2].key == Key::Char('i'), c)).flatten();
                if let Some((a, b)) = obj {
                    ed.with_surface(|s, _| {
                        s.set_anchor(Some(a));
                        let e = motion::prev_pos(s, b).unwrap_or(b);
                        s.set_cursor(e);
                    });
                }
                return Step::Done { change: false };
            }
            _ => return Step::Invalid,
        }
    }

    // Operator-pending.
    if let Some(op) = vm.pending.op {
        if c == op || (op == 'c' && c == 'c') || (matches!(op, '~' | 'u' | 'U') && (c == op || c == 'u' && op == 'u' || c == 'U' && op == 'U')) {
            // Linewise double: dd, yy, cc, >>, <<.
            let (a, b) = ed.with_surface(|s, _| {
                let p = s.cursor();
                (p, Pos::new((p.line + count - 1).min(s.line_count().saturating_sub(1)), 0))
            }).unwrap_or_default();
            let change = apply_operator(ed, vm, op, a, b, Kind::Line);
            return Step::Done { change };
        }
        match c {
            'i' | 'a' | 'f' | 'F' | 't' | 'T' | 'g' => {
                vm.pending.prefix = Some(c);
                return Step::Pending;
            }
            _ if is_motion_key(c) => {
                let start = ed.with_surface(|s, _| s.cursor()).unwrap_or_default();
                // `cw` behaves like `ce`.
                let m = if op == 'c' && c == 'w' { 'e' } else if op == 'c' && c == 'W' { 'E' } else { c };
                let Some((t, kind)) = motion_target(ed, vm, m, count, count_given) else { return Step::Invalid };
                let kind = if op == 'c' && (c == 'w' || c == 'W') { Kind::Inclusive } else { kind };
                let change = apply_operator(ed, vm, op, start, t, kind);
                return Step::Done { change };
            }
            _ => return Step::Invalid,
        }
    }

    // Visual state commands.
    if visual {
        let kind = if st == State::VisualLine { Kind::Line } else { Kind::Inclusive };
        let sel = ed.with_surface(|s, _| s.anchor().map(|a| (a, s.cursor()))).flatten();
        match c {
            'd' | 'x' | 'y' | 'c' | '>' | '<' | '~' | 'u' | 'U' | 's' => {
                let op = match c {
                    'x' => 'd',
                    's' => 'c',
                    other => other,
                };
                if let Some((a, b)) = sel {
                    let change = apply_operator(ed, vm, op, a, b, kind);
                    if op != 'c' {
                        set_state(ed, id, State::Normal);
                    }
                    return Step::Done { change };
                }
                return Step::Invalid;
            }
            'o' => {
                ed.with_surface(|s, _| {
                    if let Some(a) = s.anchor() {
                        let c = s.cursor();
                        s.set_anchor(Some(c));
                        s.set_cursor(a);
                    }
                });
                return Step::Done { change: false };
            }
            'v' => {
                set_state(ed, id, if st == State::Visual { State::Normal } else { State::Visual });
                if st != State::Visual {
                    keep_visual_anchor(ed, State::Visual);
                }
                return Step::Done { change: false };
            }
            'V' => {
                set_state(ed, id, if st == State::VisualLine { State::Normal } else { State::VisualLine });
                if st != State::VisualLine {
                    keep_visual_anchor(ed, State::VisualLine);
                }
                return Step::Done { change: false };
            }
            'i' | 'a' => {
                vm.pending.prefix = Some('v');
                return Step::Pending;
            }
            'p' | 'P' => {
                if let Some((a, b)) = sel {
                    apply_operator(ed, vm, 'd', a, b, kind);
                    paste(ed, vm, false, 1);
                    set_state(ed, id, State::Normal);
                }
                return Step::Done { change: true };
            }
            'J' => {
                if let Some((a, b)) = sel {
                    let (a, b) = ordered(a, b);
                    join_lines(ed, a.line, (b.line - a.line).max(1));
                    set_state(ed, id, State::Normal);
                }
                return Step::Done { change: true };
            }
            _ => {}
        }
    }

    match c {
        _ if is_motion_key(c) => return do_motion(ed, vm, st, c, count, count_given),
        'f' | 'F' | 't' | 'T' | 'r' | 'g' | 'z' => {
            vm.pending.prefix = Some(c);
            return Step::Pending;
        }
        'd' | 'y' | 'c' | '>' | '<' => {
            vm.pending.op = Some(c);
            vm.pending.op_count = vm.pending.count.take();
            return Step::Pending;
        }
        _ => {}
    }

    // Normal-state commands.
    let change = match c {
        'i' => {
            enter_insert(ed, None);
            true
        }
        'a' => {
            let p = ed.with_surface(|s, _| {
                let c = s.cursor();
                Pos::new(c.line, (c.col + 1).min(s.line_len(c.line)))
            });
            enter_insert(ed, p);
            true
        }
        'I' => {
            let p = ed.with_surface(|s, _| motion::line_first_nonblank(s, s.cursor().line));
            enter_insert(ed, p);
            true
        }
        'A' => {
            let p = ed.with_surface(|s, _| motion::line_end(s, s.cursor().line));
            enter_insert(ed, p);
            true
        }
        'o' | 'O' => {
            let ok = ed
                .with_surface(|s, _| {
                    let l = s.cursor().line;
                    if !s.editable_at(Pos::new(l, 0)) {
                        return false;
                    }
                    let indent: String = s.line(l).chars().take_while(|c| c.is_whitespace()).collect();
                    if c == 'o' {
                        let at = motion::line_end(s, l);
                        let e = s.insert(at, &format!("\n{indent}"));
                        s.set_cursor(e);
                    } else {
                        s.insert(Pos::new(l, 0), &format!("{indent}\n"));
                        s.set_cursor(Pos::new(l, char_len(&indent)));
                    }
                    true
                })
                .unwrap_or(false);
            enter_insert(ed, None);
            ok
        }
        'x' | 'X' => {
            let r = ed.with_surface(|s, _| {
                let p = s.cursor();
                if c == 'x' {
                    (p, Pos::new(p.line, (p.col + count).min(s.line_len(p.line))))
                } else {
                    (Pos::new(p.line, p.col.saturating_sub(count)), p)
                }
            });
            if let Some((a, b)) = r {
                apply_operator(ed, vm, 'd', a, b, Kind::Exclusive)
            } else {
                false
            }
        }
        's' => {
            let r = ed.with_surface(|s, _| {
                let p = s.cursor();
                (p, Pos::new(p.line, (p.col + count).min(s.line_len(p.line))))
            });
            if let Some((a, b)) = r {
                apply_operator(ed, vm, 'c', a, b, Kind::Exclusive)
            } else {
                false
            }
        }
        'S' => {
            let r = ed.with_surface(|s, _| s.cursor());
            if let Some(p) = r {
                apply_operator(ed, vm, 'c', p, p, Kind::Line)
            } else {
                false
            }
        }
        'D' | 'C' | 'Y' => {
            let r = ed.with_surface(|s, _| {
                let p = s.cursor();
                let l = (p.line + count - 1).min(s.line_count().saturating_sub(1));
                (p, motion::line_end(s, l))
            });
            match (c, r) {
                ('Y', Some((p, _))) => apply_operator(ed, vm, 'y', p, p, Kind::Line),
                (_, Some((a, b))) => apply_operator(ed, vm, if c == 'D' { 'd' } else { 'c' }, a, b, Kind::Exclusive),
                _ => false,
            }
        }
        'p' | 'P' => paste(ed, vm, c == 'p', count),
        'u' => {
            for _ in 0..count {
                ed.with_surface(|s, _| s.undo());
            }
            false
        }
        'J' => {
            let l = ed.with_surface(|s, _| s.cursor().line).unwrap_or(0);
            join_lines(ed, l, count.max(1));
            true
        }
        '~' => {
            let r = ed.with_surface(|s, _| {
                let p = s.cursor();
                (p, Pos::new(p.line, (p.col + count).min(s.line_len(p.line))))
            });
            if let Some((a, b)) = r {
                let ch = apply_operator(ed, vm, '~', a, b, Kind::Exclusive);
                ed.with_surface(|s, _| s.set_cursor(Pos::new(b.line, b.col.min(s.line_len(b.line).saturating_sub(1)))));
                ch
            } else {
                false
            }
        }
        'v' => {
            set_state(ed, id, State::Visual);
            keep_visual_anchor(ed, State::Visual);
            false
        }
        'V' => {
            set_state(ed, id, State::VisualLine);
            keep_visual_anchor(ed, State::VisualLine);
            false
        }
        '/' | '?' => {
            let fwd = c == '/';
            ed.read_string(if fwd { "/" } else { "?" }, "", move |ed, pat| {
                if !pat.is_empty() {
                    set_last_search(&pat, fwd);
                    repeat_search(ed, false);
                }
            });
            false
        }
        'n' => {
            repeat_search(ed, false);
            false
        }
        'N' => {
            repeat_search(ed, true);
            false
        }
        '*' | '#' => {
            let word = ed.with_surface(|s, _| text_object(s, s.cursor(), true, 'w').map(|(a, b)| s.text_range(a, b))).flatten();
            if let Some(w) = word.filter(|w| !w.trim().is_empty()) {
                set_last_search(&format!(r"\b{}\b", regex::escape(&w)), c == '*');
                repeat_search(ed, false);
            }
            false
        }
        '.' => {
            // Replayed by `flush_replay` once the interceptor has released its state.
            let keys = vm.last_change.clone();
            if !keys.is_empty() {
                vm.replaying = true;
                REPLAY.with(|r| *r.borrow_mut() = keys);
            }
            false
        }
        'q' if ed.meta(id).is_some_and(|m| m.major == "help") => {
            ed.call("kill-current-buffer");
            false
        }
        _ => return Step::Invalid,
    };
    Step::Done { change }
}

thread_local! {
    static REPLAY: RefCell<Vec<KeyChord>> = const { RefCell::new(Vec::new()) };
}

/// Replay a recorded change (dot repeat) after the interceptor released its state.
pub fn flush_replay(ed: &mut Editor) {
    let keys = REPLAY.with(|r| std::mem::take(&mut *r.borrow_mut()));
    if keys.is_empty() {
        return;
    }
    for k in keys {
        ed.handle_chord(k);
    }
    if let Some(v) = vim() {
        v.borrow_mut().replaying = false;
    }
}

fn keep_visual_anchor(ed: &mut Editor, st: State) {
    ed.with_surface(|s, _| {
        let c = s.cursor();
        if st == State::VisualLine {
            s.set_anchor(Some(Pos::new(c.line, 0)));
            let e = motion::line_end(s, c.line);
            s.set_cursor(Pos::new(c.line, e.col.max(c.col)));
        } else {
            s.set_anchor(Some(c));
        }
    });
}

fn join_lines(ed: &mut Editor, line: usize, count: usize) {
    ed.with_surface(|s, _| {
        for _ in 0..count {
            if line + 1 >= s.line_count() || !s.editable_at(Pos::new(line, 0)) {
                break;
            }
            let end = motion::line_end(s, line);
            let next_ws = s.line(line + 1).chars().take_while(|c| c.is_whitespace()).count();
            s.delete(end, Pos::new(line + 1, next_ws));
            if !s.line(line).is_empty() {
                s.insert(end, " ");
            }
            s.set_cursor(end);
        }
    });
}

fn do_motion(ed: &mut Editor, vm: &mut Vim, st: State, c: char, count: usize, has_count: bool) -> Step {
    let Some((t, kind)) = motion_target(ed, vm, c, count, has_count) else { return Step::Invalid };
    finish_motion(ed, vm, st, t, kind)
}

fn finish_motion(ed: &mut Editor, _vm: &mut Vim, st: State, t: Pos, _kind: Kind) -> Step {
    ed.with_surface(|s, _| {
        if st == State::VisualLine {
            // Keep whole lines selected: anchor at line start (or end), cursor at the far edge.
            let a = s.anchor().unwrap_or(t);
            let down = t.line >= a.line;
            let anchor = if down { Pos::new(a.line, 0) } else { motion::line_end(s, a.line) };
            s.set_anchor(Some(anchor));
            let e = if down { motion::line_end(s, t.line) } else { Pos::new(t.line, 0) };
            s.set_cursor(e);
        } else {
            let t = if st == State::Normal && s.editable_at(t) {
                // Normal state cannot rest past the last character.
                Pos::new(t.line, t.col.min(s.line_len(t.line).saturating_sub(1)))
            } else {
                t
            };
            s.set_cursor(t);
        }
    });
    Step::Done { change: false }
}
