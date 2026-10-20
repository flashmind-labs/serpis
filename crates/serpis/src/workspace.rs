//! Workspaces, i3-style: named layouts of windows, one on screen at a time.
//!
//! The current workspace lives in `Editor::layout`/`focused`; the others wait in
//! `Editor::workspaces` with their windows intact, so switching restores views exactly
//! (scroll, history, tabs). Buffers are global: any workspace can show any buffer.
//!
//! Settings: `workspace-initial-buffer`, `workspace-keep-empty`,
//! `workspace-auto-back-and-forth`, `workspace-move-follows`, `workspace-display-names`,
//! `workspace-show-in-status`. Hooks: `workspace-switched` ({from, to}) and
//! `workspace-node-moved` ({to, follow}).

use serde_json::{Value, json};

use crate::buffer::{BufferId, WindowId};
use crate::editor::{Args, BUILTIN, Editor, LayoutTree, Node, NodeRef};
use crate::minibuffer::Candidate;

pub struct Workspace {
    pub name: String,
    pub layout: LayoutTree,
    pub focused: WindowId,
}

/// Numbers sort numerically and before names, like i3's workspace order.
fn order_key(name: &str) -> (u8, u64, String) {
    match name.parse::<u64>() {
        Ok(n) => (0, n, String::new()),
        Err(_) => (1, 0, name.to_string()),
    }
}

impl Editor {
    /// The buffer a new or emptied workspace shows (`workspace-initial-buffer`).
    pub fn workspace_initial_buffer(&mut self) -> BufferId {
        let name = self.var_str("workspace-initial-buffer").unwrap_or_else(|| "*scratch*".into());
        match self.buffer_by_name(&name) {
            Some(id) => id,
            None => self.scratch_buffer(),
        }
    }

    /// A workspace is empty when it is a single window on the initial buffer.
    fn layout_is_empty(&self, layout: &LayoutTree) -> bool {
        let leaves = layout.leaves();
        let initial = self.var_str("workspace-initial-buffer").unwrap_or_else(|| "*scratch*".into());
        leaves.len() == 1 && self.meta(self.windows[leaves[0]].buffer).is_some_and(|m| m.name == initial)
    }

    pub fn workspace_is_empty(&self, name: &str) -> bool {
        if name == self.workspace {
            return self.layout_is_empty(&self.layout);
        }
        self.workspaces.iter().find(|w| w.name == name).is_none_or(|w| self.layout_is_empty(&w.layout))
    }

    /// Workspace names, current included, in i3 order.
    pub fn workspace_names(&self) -> Vec<String> {
        let mut names: Vec<String> = std::iter::once(self.workspace.clone()).chain(self.workspaces.iter().map(|w| w.name.clone())).collect();
        names.sort_by_key(|n| order_key(n));
        names
    }

    /// The label shown for a workspace (`workspace-display-names`, else its name).
    pub fn workspace_display(&self, name: &str) -> String {
        self.var("workspace-display-names").and_then(|m| m.get(name)).and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| name.to_string())
    }

    /// Buffers shown on a workspace, in layout order.
    pub fn workspace_buffers(&self, name: &str) -> Vec<BufferId> {
        let leaves = if name == self.workspace {
            self.layout.leaves()
        } else {
            self.workspaces.iter().find(|w| w.name == name).map(|w| w.layout.leaves()).unwrap_or_default()
        };
        leaves.into_iter().map(|w| self.windows[w].buffer).collect()
    }

    fn fresh_layout(&mut self) -> (LayoutTree, WindowId) {
        let b = self.workspace_initial_buffer();
        let w = self.new_window(b);
        (LayoutTree::new(w), w)
    }

    /// Show workspace `name`, creating it if needed. The one left behind is dropped if it is
    /// empty, unless `workspace-keep-empty`.
    pub fn switch_workspace(&mut self, name: &str) {
        let mut name = name.to_string();
        if name == self.workspace {
            match self.prev_workspace.clone() {
                Some(p) if self.var_bool("workspace-auto-back-and-forth") => name = p,
                _ => return,
            }
        }
        let (layout, focused) = match self.workspaces.iter().position(|w| w.name == name) {
            Some(i) => {
                let w = self.workspaces.remove(i);
                (w.layout, w.focused)
            }
            None => self.fresh_layout(),
        };
        let old = Workspace {
            name: std::mem::replace(&mut self.workspace, name.clone()),
            layout: std::mem::replace(&mut self.layout, layout),
            focused: std::mem::replace(&mut self.focused, focused),
        };
        let from = old.name.clone();
        if self.layout_is_empty(&old.layout) && !self.var_bool("workspace-keep-empty") {
            for w in old.layout.leaves() {
                self.windows.remove(w);
            }
        } else {
            self.workspaces.push(old);
        }
        self.prev_workspace = Some(from.clone());
        self.selected = None;
        self.layout.note_focus(self.focused);
        // A workspace switch is not a layout edit (winner-style undo shouldn't see it).
        self.last_layout = Some(self.window_configuration());
        self.run_hook("workspace-switched", &json!({"from": from, "to": name}));
    }

    /// Move a window or container to workspace `name` (created if needed), placing it next
    /// to that workspace's focused window. With `follow`, switch there too.
    pub fn move_to_workspace(&mut self, r: NodeRef, name: &str, follow: bool) -> bool {
        if name == self.workspace {
            return false;
        }
        let Some(node) = self.layout.node(r).cloned() else { return false };
        let mut moved = Vec::new();
        node.leaves(&mut moved);
        if moved.len() >= self.layout.leaves().len() {
            // Everything goes: this workspace keeps a fresh window.
            let (layout, w) = self.fresh_layout();
            self.layout = layout;
            self.focused = w;
        } else {
            let parent = self.layout.parent_of(r);
            self.layout.remove(r);
            if !self.layout.contains(self.focused) {
                let next = parent
                    .and_then(|p| self.layout.node(NodeRef::Container(p)))
                    .and_then(Node::focus_leaf)
                    .unwrap_or_else(|| self.layout.leaves()[0]);
                self.focused = next;
            }
        }
        self.selected = None;
        self.layout.note_focus(self.focused);
        let land = node.focus_leaf().unwrap_or(moved[0]);
        let target_empty = self.workspace_is_empty(name);
        match self.workspaces.iter().position(|w| w.name == name) {
            Some(i) if !target_empty => {
                let ws = &mut self.workspaces[i];
                ws.layout.fullscreen = None;
                ws.layout.insert_beside(NodeRef::Window(ws.focused), node);
                ws.focused = land;
                ws.layout.note_focus(land);
            }
            found => {
                if let Some(i) = found {
                    let old = self.workspaces.remove(i);
                    for w in old.layout.leaves() {
                        self.windows.remove(w);
                    }
                }
                let mut layout = LayoutTree::from_node(node);
                layout.note_focus(land);
                self.workspaces.push(Workspace { name: name.to_string(), layout, focused: land });
            }
        }
        self.run_hook("workspace-node-moved", &json!({"to": name, "follow": follow}));
        if follow {
            self.switch_workspace(name);
        }
        true
    }

    pub fn rename_workspace(&mut self, from: &str, to: &str) -> anyhow::Result<()> {
        if to.is_empty() || self.workspace_names().iter().any(|n| n == to) {
            anyhow::bail!("Workspace {to} already exists");
        }
        if self.workspace == from {
            self.workspace = to.to_string();
        } else if let Some(w) = self.workspaces.iter_mut().find(|w| w.name == from) {
            w.name = to.to_string();
        } else {
            anyhow::bail!("No workspace {from}");
        }
        if self.prev_workspace.as_deref() == Some(from) {
            self.prev_workspace = Some(to.to_string());
        }
        Ok(())
    }

    /// One line describing a workspace for pickers: project names, else buffer names.
    pub fn workspace_summary(&self, name: &str) -> String {
        let mut seen = Vec::new();
        let bufs = self.workspace_buffers(name);
        for id in &bufs {
            let label = self.buffer_directory(*id).and_then(|d| crate::project::name(&d)).unwrap_or_else(|| self.buffer_display_name(*id));
            if !seen.contains(&label) {
                seen.push(label);
            }
        }
        seen.join(", ")
    }
}

impl LayoutTree {
    /// A layout made of one moved subtree.
    pub fn from_node(node: Node) -> LayoutTree {
        match node {
            Node::Window(w) => LayoutTree::new(w),
            Node::Container(c) => {
                let mut t = LayoutTree { root: c, fullscreen: None };
                t.normalize();
                t
            }
        }
    }
}

fn param_or_prompt(ed: &mut Editor, a: &Args, prompt: &str, f: impl FnOnce(&mut Editor, String) + 'static) {
    if let Some(name) = a.str(0).map(str::to_string).or_else(|| a.params.first().and_then(Value::as_u64).map(|n| n.to_string())) {
        f(ed, name);
        return;
    }
    let cands = candidates(ed, true);
    ed.completing_read(prompt, cands, false, move |ed, c, input| {
        let name = c.map(|c| c.value).unwrap_or(input);
        if !name.is_empty() {
            f(ed, name);
        }
    });
}

/// Workspaces for pickers: non-empty ones plus the current one.
fn candidates(ed: &Editor, all: bool) -> Vec<Candidate> {
    ed.workspace_names()
        .into_iter()
        .filter(|n| all || *n == ed.workspace || !ed.workspace_is_empty(n))
        .map(|n| {
            let summary = ed.workspace_summary(&n);
            let label = format!("{}  {}", ed.workspace_display(&n), if summary.is_empty() { "empty".into() } else { summary });
            Candidate::new(label).value(n.clone()).detail(if n == ed.workspace { "current" } else { "" })
        })
        .collect()
}

pub fn install(ed: &mut Editor) {
    let o = BUILTIN;
    ed.defvar("workspace-initial-buffer", json!("*scratch*"), "Buffer shown by a new or emptied workspace.");
    ed.defvar("workspace-keep-empty", json!(false), "Keep empty workspaces when switching away (i3 drops them).");
    ed.defvar("workspace-auto-back-and-forth", json!(false), "Switching to the current workspace goes back to the previous one (i3 workspace_auto_back_and_forth).");
    ed.defvar("workspace-move-follows", json!(false), "Moving a window to a workspace also switches to it.");
    ed.defvar("workspace-display-names", json!({}), "Labels for workspaces, by name: {\"1\": \"一\", ...}.");
    ed.defvar("workspace-show-in-status", json!(true), "Show the current workspace in the status bar.");

    ed.define_command("workspace", "Switch to a workspace (name as parameter, else pick one with live preview).", o, |ed, a| {
        if let Some(name) = a.str(0).map(str::to_string).or_else(|| a.params.first().and_then(Value::as_u64).map(|n| n.to_string())) {
            ed.switch_workspace(&name);
            return Ok(());
        }
        let original = ed.workspace.clone();
        let prev = ed.prev_workspace.clone();
        let cands = candidates(ed, false);
        let accept_from = original.clone();
        ed.completing_read("Workspace: ", cands, false, move |ed, c, input| {
            let original = accept_from;
            let name = c.map(|c| c.value).unwrap_or(input);
            // Previews moved around; land on the choice with `original` as the previous one.
            ed.switch_workspace(&original);
            ed.prev_workspace = prev;
            if !name.is_empty() && name != original {
                ed.switch_workspace(&name);
            }
        });
        let back = original.clone();
        let cancel_to = original.clone();
        ed.minibuffer_preview(
            Some(std::rc::Rc::new(move |ed: &mut Editor, c: Option<&Candidate>| {
                let to = c.map(|c| c.value.clone()).unwrap_or_else(|| back.clone());
                if to != ed.workspace {
                    ed.switch_workspace(&to);
                }
            })),
            Some(Box::new(move |ed: &mut Editor| {
                ed.switch_workspace(&cancel_to);
            })),
            true,
        );
        Ok(())
    });
    for i in 1..=9 {
        ed.define_command(&format!("workspace-{i}"), &format!("Switch to workspace {i}."), o, move |ed, _| {
            ed.switch_workspace(&i.to_string());
            Ok(())
        });
        ed.define_command(&format!("workspace-move-window-{i}"), &format!("Move the focused window (or selected container) to workspace {i}."), o, move |ed, _| {
            let r = ed.target_node();
            let follow = ed.var_bool("workspace-move-follows");
            if ed.move_to_workspace(r, &i.to_string(), follow) {
                ed.message(format!("Moved to workspace {}", ed.workspace_display(&i.to_string())));
            }
            Ok(())
        });
    }
    ed.define_command("workspace-move-window", "Move the focused window (or selected container) to a workspace.", o, |ed, a| {
        param_or_prompt(ed, a, "Move to workspace: ", |ed, name| {
            let r = ed.target_node();
            let follow = ed.var_bool("workspace-move-follows");
            ed.move_to_workspace(r, &name, follow);
        });
        Ok(())
    });
    ed.define_command("workspace-back-and-forth", "Switch to the previous workspace.", o, |ed, _| {
        match ed.prev_workspace.clone() {
            Some(p) => ed.switch_workspace(&p),
            None => ed.message("No previous workspace"),
        }
        Ok(())
    });
    ed.define_command("workspace-rename", "Rename the current workspace.", o, |ed, a| {
        let from = ed.workspace.clone();
        if let Some(to) = a.str(0) {
            return ed.rename_workspace(&from, to);
        }
        ed.read_string("Rename workspace to: ", &from.clone(), move |ed, to| {
            if let Err(e) = ed.rename_workspace(&from, to.trim()) {
                ed.error(format!("{e:#}"));
            }
        });
        Ok(())
    });
}
