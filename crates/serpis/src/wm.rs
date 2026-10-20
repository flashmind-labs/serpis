//! Window management commands, i3-style, plus buffer lifetime: closing a window only closes
//! a view, killing a buffer is separate and asks first when a program is running.
//!
//! Commands act on the focused window, or on the container selected with `focus-parent`.
//! Settings: `confirm-kill-processes` and `confirm-quit-processes` (`"running"`, `"always"`
//! or `"never"`). Hook: `kill-buffer-query` ({buffer, name, process, running}); a handler
//! returning `false` keeps the buffer, `true` kills it without asking.

use serde_json::{Value, json};

use crate::buffer::{BufferId, ProcessStatus};
use crate::editor::{BUILTIN, Dir, Editor, Kind, NodeRef, buffer_id_from_u64, buffer_id_to_u64};
use crate::minibuffer::Candidate;

impl Editor {
    pub fn process_status(&self, id: BufferId) -> ProcessStatus {
        self.buffers.get(id).and_then(|s| s.buf.as_ref()).map(|b| b.process_status()).unwrap_or(ProcessStatus::None)
    }

    /// Whether `confirm-*` policy `var` asks about `status`.
    fn should_confirm(&self, var: &str, status: &ProcessStatus) -> bool {
        match (self.var_str(var).as_deref().unwrap_or("running"), status) {
            ("never", _) | (_, ProcessStatus::None) => false,
            ("always", _) => true,
            (_, ProcessStatus::Running(_)) => true,
            _ => false,
        }
    }

    /// Kill a buffer as a user action: confirm per `confirm-kill-processes` and the
    /// `kill-buffer-query` hook, then kill.
    pub fn kill_buffer_interactive(&mut self, id: BufferId) {
        if self.meta(id).is_none() {
            return;
        }
        let name = self.buffer_display_name(id);
        let status = self.process_status(id);
        let mut ask = self.should_confirm("confirm-kill-processes", &status);
        let process = match &status {
            ProcessStatus::Running(p) => Some(p.clone()),
            _ => None,
        };
        let answers = self.run_hook("kill-buffer-query", &json!({"buffer": buffer_id_to_u64(id), "name": name, "process": process, "running": process.is_some()}));
        if answers.iter().any(|v| v == &Value::Bool(false)) {
            return;
        }
        if answers.iter().any(|v| v == &Value::Bool(true)) {
            ask = false;
        }
        if !ask {
            self.kill_buffer(id);
            return;
        }
        let what = match process {
            Some(p) => format!("{name} is running {p}; kill it?"),
            None => format!("Kill {name}?"),
        };
        self.y_or_n(&what, move |ed, yes| {
            if yes {
                ed.kill_buffer(id);
            }
        });
    }

    /// Quit, confirming per `confirm-quit-processes` when buffers run programs.
    pub fn quit_interactive(&mut self) {
        let mut running = Vec::new();
        for id in self.buffers.keys().collect::<Vec<_>>() {
            let status = self.process_status(id);
            if self.should_confirm("confirm-quit-processes", &status) {
                let name = self.buffer_display_name(id);
                running.push(match status {
                    ProcessStatus::Running(p) => format!("{name} ({p})"),
                    _ => name,
                });
            }
        }
        if running.is_empty() {
            self.quit_requested = true;
            return;
        }
        self.y_or_n(&format!("Running: {}. Quit anyway?", running.join(", ")), |ed, yes| {
            ed.quit_requested = yes;
        });
    }

    /// Buffers not shown in any window of any workspace, most recently shown first.
    pub fn hidden_buffers(&self) -> Vec<BufferId> {
        let shown: Vec<BufferId> = self.windows.values().map(|w| w.buffer).collect();
        let mut out: Vec<BufferId> = Vec::new();
        for id in self.windows[self.focused].history.iter().rev() {
            if !shown.contains(id) && !out.contains(id) && self.buffers.contains_key(*id) {
                out.push(*id);
            }
        }
        for id in self.buffers.keys() {
            if !shown.contains(&id) && !out.contains(&id) {
                out.push(id);
            }
        }
        out
    }

    /// Show something else in the focused window: the last buffer it showed that is now
    /// hidden, else any hidden buffer, else the initial buffer.
    pub fn bury_buffer(&mut self) {
        let cur = self.current_buffer();
        let hidden = self.hidden_buffers();
        let next = hidden.into_iter().find(|b| *b != cur).unwrap_or_else(|| self.workspace_initial_buffer());
        if next != cur {
            self.show_buffer(next);
        }
    }

    /// Close the focused window or selected container; the last window buries its buffer
    /// instead. Buffers are never killed.
    pub fn close_window(&mut self) {
        let r = self.target_node();
        if !self.delete_node(r) {
            self.bury_buffer();
        }
    }

    /// The container layout commands act on: the selected container, else the focused
    /// window's parent.
    fn layout_target(&self) -> Option<u64> {
        match self.target_node() {
            NodeRef::Container(id) => Some(id),
            r @ NodeRef::Window(_) => self.layout.parent_of(r),
        }
    }

    pub fn set_layout_kind(&mut self, kind: Kind) {
        let Some(cid) = self.layout_target() else { return };
        let Some(path) = self.layout.path(NodeRef::Container(cid)) else { return };
        if let Some(c) = self.layout.container_mut(&path) {
            c.kind = kind;
        }
        self.layout.note_focus(self.focused);
    }

    pub fn layout_kind(&self) -> Option<Kind> {
        let cid = self.layout_target()?;
        let path = self.layout.path(NodeRef::Container(cid))?;
        self.layout.container(&path).map(|c| c.kind)
    }

    pub fn move_window(&mut self, dir: Dir) -> bool {
        let r = self.target_node();
        self.layout.fullscreen = None;
        let ok = self.layout.move_node(r, dir);
        self.layout.note_focus(self.focused);
        ok
    }

    /// Select the container around the current selection (i3 `focus parent`).
    pub fn focus_parent(&mut self) -> bool {
        let r = self.target_node();
        let Some(p) = self.layout.parent_of(r) else { return false };
        if NodeRef::Container(p) == r {
            return false;
        }
        self.selected = Some(NodeRef::Container(p));
        true
    }

    /// Narrow the selection back toward the focused window (i3 `focus child`).
    pub fn focus_child(&mut self) -> bool {
        let Some(NodeRef::Container(cid)) = self.selected else { return false };
        let Some(path) = self.layout.path(NodeRef::Window(self.focused)) else { return false };
        let Some(cpath) = self.layout.path(NodeRef::Container(cid)) else { return false };
        // The next node down the focused window's path.
        let child = self.layout.container(&cpath).and_then(|c| c.children.get(path[cpath.len()])).map(|n| n.as_ref());
        self.selected = match child {
            Some(NodeRef::Container(id)) => Some(NodeRef::Container(id)),
            _ => None,
        };
        true
    }
}

fn dir_arg(a: &crate::editor::Args) -> Option<Dir> {
    a.str(0).and_then(Dir::parse)
}

pub fn install(ed: &mut Editor) {
    let o = BUILTIN;
    ed.defvar("confirm-kill-processes", json!("running"), "Confirm killing a process buffer: \"running\" (a program other than the idle shell), \"always\" or \"never\".");
    ed.defvar("fullscreen-indicator", json!("FULLSCREEN"), "Badge on a fullscreen window's header (\"\" hides it); shows how many windows it hides, and clicking it restores the layout.");
    ed.defvar("confirm-quit-processes", json!("running"), "Confirm quitting with process buffers: \"running\", \"always\" or \"never\".");

    // ------------------------------------------------------------ closing vs killing
    ed.define_command("close-window", "Close the focused window or selected container (a view: buffers live on). The last window shows another buffer instead.", o, |ed, _| {
        ed.close_window();
        Ok(())
    });
    ed.define_command("delete-window", "Close the focused window or selected container.", o, |ed, _| {
        let r = ed.target_node();
        if !ed.delete_node(r) {
            ed.message("Attempt to delete the sole window");
        }
        Ok(())
    });
    ed.define_command("bury-buffer", "Show another buffer in this window (the buffer stays alive).", o, |ed, _| {
        ed.bury_buffer();
        Ok(())
    });
    ed.define_command("kill-current-buffer", "Kill the focused buffer (asks first if a program runs in it; see confirm-kill-processes).", o, |ed, _| {
        let id = ed.current_buffer();
        ed.kill_buffer_interactive(id);
        Ok(())
    });
    ed.define_command("kill-buffer", "Kill a buffer chosen by name (asks first if a program runs in it).", o, |ed, a| {
        if let Some(n) = a.str(0) {
            if let Some(id) = ed.buffer_by_name(n) {
                ed.kill_buffer_interactive(id);
            }
            return Ok(());
        }
        let ids: Vec<BufferId> = ed.buffers.keys().collect();
        let cands = ids.into_iter().map(|id| ed.buffer_candidate(id)).collect();
        ed.completing_read("Kill buffer: ", cands, true, |ed, c, _| {
            if let Some(c) = c {
                ed.kill_buffer_interactive(buffer_id_from_u64(c.value.parse().unwrap_or(0)));
            }
        });
        Ok(())
    });
    ed.define_command("quit", "Quit serpis (asks first if programs run in terminals; see confirm-quit-processes).", o, |ed, _| {
        ed.quit_interactive();
        Ok(())
    });
    ed.define_command("switch-to-hidden-buffer", "Show a buffer that no window shows (like i3's scratchpad).", o, |ed, _| {
        let cands: Vec<Candidate> = ed.hidden_buffers().into_iter().map(|id| ed.buffer_candidate(id)).collect();
        if cands.is_empty() {
            ed.message("No hidden buffers");
            return Ok(());
        }
        ed.completing_read("Hidden buffer: ", cands, true, |ed, c, _| {
            if let Some(c) = c {
                ed.show_buffer(buffer_id_from_u64(c.value.parse().unwrap_or(0)));
            }
        });
        if let Some(mb) = &mut ed.minibuffer {
            mb.keep_order = true;
        }
        Ok(())
    });

    // ------------------------------------------------------------ focus and move
    for (d, name) in [(Dir::Left, "left"), (Dir::Right, "right"), (Dir::Up, "up"), (Dir::Down, "down")] {
        ed.define_command(&format!("windmove-{name}"), &format!("Focus the window to the {name}."), o, move |ed, _| {
            ed.windmove(name);
            Ok(())
        });
        ed.define_command(&format!("window-move-{name}"), &format!("Move the focused window (or selected container) {name}: swap with a neighbor, enter or leave containers, i3-style."), o, move |ed, _| {
            ed.move_window(d);
            Ok(())
        });
    }
    ed.define_command("window-move", "Move the focused window or selected container in a direction (left/right/up/down).", o, |ed, a| {
        match dir_arg(a) {
            Some(d) => {
                ed.move_window(d);
                Ok(())
            }
            None => anyhow::bail!("window-move needs a direction"),
        }
    });
    ed.define_command("focus-parent", "Select the container around the focused window (window commands then act on all of it).", o, |ed, _| {
        if !ed.focus_parent() {
            ed.message("Already at the top");
        }
        Ok(())
    });
    ed.define_command("focus-child", "Narrow a container selection back toward the focused window.", o, |ed, _| {
        ed.focus_child();
        Ok(())
    });

    // ------------------------------------------------------------ layouts
    for (cmd, kind, doc) in [
        ("layout-splith", Kind::SplitH, "Lay out the focused container side by side."),
        ("layout-splitv", Kind::SplitV, "Lay out the focused container stacked top to bottom."),
        ("layout-tabbed", Kind::Tabbed, "Show the focused container as tabs."),
        ("layout-stacked", Kind::Stacked, "Show the focused container as a stack of title bars."),
    ] {
        ed.define_command(cmd, doc, o, move |ed, _| {
            ed.set_layout_kind(kind);
            Ok(())
        });
    }
    ed.define_command("layout-toggle-split", "Switch the focused container between side by side and stacked.", o, |ed, _| {
        let next = match ed.layout_kind() {
            Some(Kind::SplitH) => Kind::SplitV,
            _ => Kind::SplitH,
        };
        ed.set_layout_kind(next);
        Ok(())
    });
    ed.define_command("layout-toggle-all", "Cycle the focused container: splith, splitv, stacked, tabbed.", o, |ed, _| {
        let next = match ed.layout_kind() {
            Some(Kind::SplitH) => Kind::SplitV,
            Some(Kind::SplitV) => Kind::Stacked,
            Some(Kind::Stacked) => Kind::Tabbed,
            _ => Kind::SplitH,
        };
        ed.set_layout_kind(next);
        Ok(())
    });
    ed.define_command("layout", "Set the focused container's layout (splith, splitv, tabbed, stacked).", o, |ed, a| {
        match a.str(0).and_then(Kind::parse) {
            Some(k) => {
                ed.set_layout_kind(k);
                Ok(())
            }
            None => anyhow::bail!("layout needs splith, splitv, tabbed or stacked"),
        }
    });
    ed.define_command("window-fullscreen-toggle", "Show the focused window alone, or restore the layout (i3 fullscreen).", o, |ed, _| {
        ed.layout.fullscreen = match ed.layout.fullscreen {
            Some(_) => None,
            None => Some(ed.focused),
        };
        Ok(())
    });

    // ------------------------------------------------------------ sizes
    for (cmd, horizontal, sign, doc) in [
        ("enlarge-window-horizontally", true, 1.0, "Widen the focused window."),
        ("shrink-window-horizontally", true, -1.0, "Narrow the focused window."),
        ("enlarge-window", false, 1.0, "Make the focused window taller."),
        ("shrink-window", false, -1.0, "Make the focused window shorter."),
    ] {
        ed.define_command(cmd, doc, o, move |ed, a| {
            let r = ed.target_node();
            ed.layout.resize(r, horizontal, sign * 0.05 * a.count() as f32);
            Ok(())
        });
    }
    ed.define_command("balance-windows", "Make all windows in every container the same size.", o, |ed, _| {
        ed.layout.balance();
        Ok(())
    });
    ed.define_command("select-window-by-number", "Focus window N (prefix or param).", o, |ed, a| {
        let n = a.params.first().and_then(Value::as_u64).map(|v| v as usize).or(a.prefix.map(|p| p as usize)).unwrap_or(1);
        if let Some(w) = ed.layout.visible_leaves().get(n.saturating_sub(1)) {
            ed.focus_window(*w);
        }
        Ok(())
    });
}
