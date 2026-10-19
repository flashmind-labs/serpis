//! The default scheme: conventional desktop editing (Ctrl on Linux/Windows mirrors Cmd on macOS).

use super::{SCHEME_OWNER, TEXT_MAPS};
use crate::editor::Editor;

pub fn install(ed: &mut Editor) {
    let o = SCHEME_OWNER;
    for (k, c) in [
        ("ESC", "keyboard-quit"),
        ("C-S-p", "execute-extended-command"),
        ("C-p", "switch-to-buffer"),
        ("C-n", "new-chat"),
        ("C-S-t", "new-terminal"),
        ("C-o", "find-file"),
        ("C-s", "save-buffer"),
        ("C-\\", "split-window-right"),
        ("C-|", "split-window-below"),
        ("C-<tab>", "next-buffer"),
        ("C-S-<tab>", "previous-buffer"),
        ("C-q", "quit"),
        ("M-x", "execute-extended-command"),
    ] {
        ed.bind("global", k, c, o);
    }
    // On macOS, Cmd carries these (see `base`) and Ctrl stays free for terminals and `C-c` prefixes.
    let ctrl_clipboard: &[(&str, &str)] = if cfg!(target_os = "macos") {
        &[]
    } else {
        &[("C-z", "undo"), ("C-y", "redo"), ("C-a", "select-all"), ("C-c", "copy-region-as-kill"), ("C-x", "kill-region"), ("C-v", "yank"), ("C-f", "search-forward")]
    };
    for m in TEXT_MAPS {
        for (k, c) in ctrl_clipboard {
            ed.bind(m, k, c, o);
        }
        for (k, c) in [
            ("C-<left>", "backward-word"),
            ("C-<right>", "forward-word"),
            ("C-DEL", "backward-kill-word"),
            ("s-<left>", "beginning-of-line"),
            ("s-<right>", "end-of-line"),
            ("s-<up>", "beginning-of-buffer"),
            ("s-<down>", "end-of-buffer"),
            ("s-DEL", "kill-line"),
        ] {
            ed.bind(m, k, c, o);
        }
    }
    // Chat: Escape aborts a running agent, otherwise cancels.
    ed.bind("chat", "ESC", "chat-abort-or-quit", o);
}
