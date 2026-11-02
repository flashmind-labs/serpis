//! Built-in packages. They use only the public editor API, exactly like user packages.
//!
//! - `base`: bindings every scheme shares (arrows, Return, clipboard on Cmd, mouse-friendly keys).
//! - `regular`, `emacs`, `vim`: keybinding schemes. Exactly one is active; switching removes
//!   the old scheme's bindings (owner `scheme`) and installs the new one.

pub mod emacs;
pub mod regular;
pub mod vim;

use serde_json::Value;

use crate::editor::Editor;
use crate::minibuffer::Candidate;

pub const SCHEME_OWNER: &str = "scheme";
pub const BASE_OWNER: &str = "base";
pub const SCHEMES: [&str; 3] = ["regular", "emacs", "vim"];

/// Maps whose buffers edit text with the shared editing commands.
pub const TEXT_MAPS: [&str; 4] = ["text", "chat", "help", crate::diff::view::EDITING];

pub fn install_base(ed: &mut Editor) {
    let o = BASE_OWNER;
    for m in TEXT_MAPS {
        for (k, c) in [
            ("<left>", "backward-char"),
            ("<right>", "forward-char"),
            ("<up>", "previous-line"),
            ("<down>", "next-line"),
            ("<home>", "beginning-of-line"),
            ("<end>", "end-of-line"),
            ("<pageup>", "scroll-down-command"),
            ("<pagedown>", "scroll-up-command"),
            ("S-<left>", "select-backward-char"),
            ("S-<right>", "select-forward-char"),
            ("S-<up>", "select-previous-line"),
            ("S-<down>", "select-next-line"),
            ("S-<home>", "select-beginning-of-line"),
            ("S-<end>", "select-end-of-line"),
            ("M-<left>", "backward-word"),
            ("M-<right>", "forward-word"),
            ("M-S-<left>", "select-backward-word"),
            ("M-S-<right>", "select-forward-word"),
            ("DEL", "delete-backward-char"),
            ("<delete>", "delete-char"),
            ("M-DEL", "backward-kill-word"),
            ("RET", "newline"),
        ] {
            ed.bind(m, k, c, o);
        }
    }
    // Chat: Return sends, Shift/Meta-Return for newline / follow-up.
    for (k, c) in [
        ("RET", "chat-send"),
        ("S-RET", "chat-newline"),
        ("M-RET", "chat-send-follow-up"),
        ("TAB", "chat-toggle-fold"),
        ("C-c C-c", "chat-abort"),
        ("C-c C-m", "chat-select-model"),
        ("C-c C-t", "chat-cycle-thinking"),
        ("C-c C-s", "chat-select-thinking"),
        ("C-c C-l", "chat-clear"),
        ("C-c C-o", "chat-toggle-focus"),
        ("C-c C-p", "chat-history-previous"),
        ("C-c C-n", "chat-history-next"),
        ("C-c C-y", "chat-allow"),
        ("C-c C-a", "chat-allow-always"),
        ("C-c C-d", "chat-deny"),
        ("C-c C-k", "chat-stop-task"),
        ("C-c C-b", "chat-background-task"),
        ("C-c /", "chat-select-skill"),
        ("S-TAB", "chat-cycle-permission-mode"),
    ] {
        ed.bind("chat", k, c, o);
    }
    for (k, c) in [("g", "claude-code-usage-refresh"), ("q", "kill-current-buffer")] {
        ed.bind("claude-usage", k, c, o);
    }
    for (k, c) in [("g", "codex-status-refresh"), ("q", "kill-current-buffer")] {
        ed.bind("codex-status", k, c, o);
    }
    for (k, c) in [("C-c C-c", "terminal-send-ctrl-c"), ("C-c C-k", "terminal-copy-mode"), ("C-c C-j", "terminal-exit-copy-mode"), ("C-c ESC", "terminal-send-escape")] {
        ed.bind("terminal", k, c, o);
    }
    // Super (Cmd) bindings never reach terminal programs, so they are safe everywhere.
    for (k, c) in [
        ("s-c", "copy-region-as-kill"),
        ("s-x", "kill-region"),
        ("s-v", "yank"),
        ("s-a", "select-all"),
        ("s-z", "undo"),
        ("s-Z", "redo"),
        ("s-P", "execute-extended-command"),
        ("s-p", "switch-to-buffer"),
        ("s-n", "new-chat"),
        ("s-t", "new-terminal"),
        ("s-o", "find-file"),
        ("s-s", "save-buffer"),
        ("s-w", "close-window"),
        ("s-W", "kill-current-buffer"),
        ("s-\\", "split-window-right"),
        ("s-|", "split-window-below"),
        ("s-]", "other-window"),
        ("s-[", "previous-window"),
        ("s-f", "search-forward"),
        ("s-G", "search-previous"),
        ("s-=", "text-scale-increase"),
        ("s-+", "text-scale-increase"),
        ("s--", "text-scale-decrease"),
        ("s-0", "text-scale-reset"),
        ("s-,", "open-config"),
        ("s-q", "quit"),
        ("s-R", "reload-config"),
        ("<f1>", "describe-key"),
    ] {
        ed.bind("global", k, c, o);
    }
    for n in 1..=9 {
        ed.bind("global", &format!("s-{n}"), &format!("select-window-{n}"), o);
        ed.define_command(&format!("select-window-{n}"), &format!("Focus window {n}."), o, move |ed, _| {
            if let Some(w) = ed.layout.visible_leaves().get(n - 1).copied() {
                ed.focus_window(w);
            }
            Ok(())
        });
    }
    // The terminal gets Cmd-v as a paste and Cmd-c as copy-mode copy.
    ed.bind("terminal", "s-c", "terminal-copy", o);
    ed.define_command("set-keybinding-scheme", "Switch between the regular, emacs and vim schemes.", crate::editor::BUILTIN, |ed, a| {
        match a.str(0) {
            Some(s) => set_scheme(ed, s),
            None => {
                let cur = ed.var_str("keybinding-scheme").unwrap_or_default();
                let cands = SCHEMES.iter().map(|s| Candidate::new(*s).detail(if *s == cur { "active" } else { "" })).collect();
                ed.completing_read("Keybinding scheme: ", cands, true, |ed, c, _| {
                    if let Some(c) = c {
                        set_scheme(ed, &c.value);
                    }
                });
            }
        }
        Ok(())
    });
}

pub fn set_scheme(ed: &mut Editor, scheme: &str) {
    ed.remove_owner(SCHEME_OWNER);
    vim::disable(ed);
    match scheme {
        "emacs" => emacs::install(ed),
        "vim" => {
            emacs::install_global_only(ed);
            vim::enable(ed);
        }
        _ => regular::install(ed),
    }
    ed.set_var("keybinding-scheme", Value::String(scheme.to_string()));
    ed.message(format!("Keybinding scheme: {scheme}"));
}
