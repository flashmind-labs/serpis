//! Emacs scheme. In terminals only the window/buffer prefixes (`C-x`, `M-x`, `C-h`, `C-g`)
//! are intercepted; everything else goes to the shell, as in vterm/ghostel semi-char mode.

use super::{SCHEME_OWNER, TEXT_MAPS};
use crate::editor::Editor;

/// Global prefixes shared by the emacs and vim schemes.
pub fn install_global_only(ed: &mut Editor) {
    let o = SCHEME_OWNER;
    for (k, c) in [
        ("M-x", "execute-extended-command"),
        ("C-g", "keyboard-quit"),
        ("C-u", "universal-argument"),
        ("C-x 0", "delete-window"),
        ("C-x 1", "delete-other-windows"),
        ("C-x 2", "split-window-below"),
        ("C-x 3", "split-window-right"),
        ("C-x o", "other-window"),
        ("C-x b", "switch-to-buffer"),
        ("C-x C-b", "switch-to-buffer"),
        ("C-x k", "kill-current-buffer"),
        ("C-x K", "kill-buffer"),
        ("C-x <left>", "previous-buffer"),
        ("C-x <right>", "next-buffer"),
        ("C-x C-f", "find-file"),
        ("C-x C-s", "save-buffer"),
        ("C-x C-c", "quit"),
        ("C-x ^", "enlarge-window"),
        ("C-x }", "enlarge-window-horizontally"),
        ("C-x {", "shrink-window-horizontally"),
        ("C-x +", "balance-windows"),
        ("C-x m", "new-chat"),
        ("C-x t", "new-terminal"),
        ("C-h k", "describe-key"),
        ("C-h f", "describe-command"),
        ("C-h x", "describe-command"),
        ("C-h v", "describe-variable"),
        ("C-h m", "describe-mode"),
        ("C-h b", "describe-keymap"),
        ("C-h e", "view-messages"),
    ] {
        ed.bind("global", k, c, o);
    }
}

pub fn install(ed: &mut Editor) {
    install_global_only(ed);
    let o = SCHEME_OWNER;
    for m in TEXT_MAPS {
        for (k, c) in [
            ("C-f", "forward-char"),
            ("C-b", "backward-char"),
            ("C-n", "next-line"),
            ("C-p", "previous-line"),
            ("C-a", "beginning-of-line"),
            ("C-e", "end-of-line"),
            ("M-f", "forward-word"),
            ("M-b", "backward-word"),
            ("M-<", "beginning-of-buffer"),
            ("M->", "end-of-buffer"),
            ("C-v", "scroll-up-command"),
            ("M-v", "scroll-down-command"),
            ("M-}", "forward-paragraph"),
            ("M-{", "backward-paragraph"),
            ("C-d", "delete-char"),
            ("C-k", "kill-line"),
            ("M-d", "kill-word"),
            ("C-SPC", "set-mark-command"),
            ("C-@", "set-mark-command"),
            ("C-w", "kill-region"),
            ("M-w", "copy-region-as-kill"),
            ("C-y", "yank"),
            ("C-/", "undo"),
            ("C-_", "undo"),
            ("C-x u", "undo"),
            ("C-?", "redo"),
            ("C-M-_", "redo"),
            ("C-s", "search-forward"),
            ("C-r", "search-backward"),
            ("C-x h", "select-all"),
            ("C-m", "newline"),
        ] {
            ed.bind(m, k, c, o);
        }
    }
    ed.bind("chat", "C-m", "chat-send", o);
    ed.bind("chat", "C-j", "chat-newline", o);
    ed.bind("chat", "M-p", "chat-history-previous", o);
    ed.bind("chat", "M-n", "chat-history-next", o);
    ed.bind("terminal", "C-y", "yank", o);
    ed.bind("terminal", "M-w", "terminal-copy", o);
}
