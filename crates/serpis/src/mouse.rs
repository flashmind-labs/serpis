//! Mouse window management: drag windows, tabs and groups onto drop zones, close with the
//! middle button, and right-click menus.
//!
//! Dragging a window header, a tab or a group's grip picks it up (`Alt` on a header takes
//! the window's whole container; a container selected with `focus-parent` moves as a unit).
//! Dropping on a window's edge splits it there, on its center groups them as tabs, on a tab
//! bar inserts at that position, and on a workspace chip moves it to that workspace. `ESC`
//! cancels.
//!
//! Menus are data: `window-menu` (window headers and tabs) and `workspace-menu` (status-bar
//! workspace chips) are lists of `{label, command, args?}`, `{label, items}` submenus,
//! `{label, workspaces = command}` (one entry per workspace, plus a new one) and `"-"`
//! separators. Every item runs a command, so menus can be extended from Lua.
//!
//! Settings: `mouse-middle-click-closes`, `mouse-double-click-fullscreen`,
//! `mouse-drop-edge` (fraction of a window that counts as its edge).

use egui::{Color32, FontId, Pos2, Rect, Stroke, StrokeKind};
use serde_json::{Value, json};

use crate::buffer::WindowId;
use crate::editor::{Args, BUILTIN, Dir, Editor, NodeRef};
use crate::layout::Zone;

/// What is being dragged.
#[derive(Debug, Clone)]
pub struct Drag {
    pub node: NodeRef,
    pub label: String,
}

/// Places a drag can land, registered while drawing each frame.
#[derive(Debug, Clone)]
pub enum DropTarget {
    Window(WindowId, Rect),
    Tabs { cid: u64, rect: Rect, tabs: Vec<Rect>, horizontal: bool },
    Workspace(String, Rect),
}

enum Hit {
    Window(WindowId, Zone, Rect),
    Tab(u64, usize, Rect),
    Workspace(String, Rect),
}

impl Editor {
    /// Drop `r` on `target` in `zone`; the dropped node takes focus.
    pub fn drop_node(&mut self, r: NodeRef, target: NodeRef, zone: Zone) -> bool {
        self.layout.fullscreen = None;
        if !self.layout.place(r, target, zone) {
            return false;
        }
        self.focus_dropped(r);
        true
    }

    /// Put `r` at `index` in container `cid` (tab bars).
    pub fn drop_into(&mut self, r: NodeRef, cid: u64, index: usize) -> bool {
        self.layout.fullscreen = None;
        if !self.layout.place_in(r, cid, index) {
            return false;
        }
        self.focus_dropped(r);
        true
    }

    fn focus_dropped(&mut self, r: NodeRef) {
        let leaf = match r {
            NodeRef::Window(w) => Some(w),
            NodeRef::Container(_) => self.layout.node(r).and_then(|n| n.focus_leaf()),
        };
        if let Some(w) = leaf {
            self.focus_window(w);
        }
        if matches!(r, NodeRef::Container(_)) {
            self.selected = Some(r);
        }
    }

    /// A short label for a dragged node.
    pub fn node_label(&self, r: NodeRef) -> String {
        match r {
            NodeRef::Window(w) => self.windows.get(w).map(|w| self.buffer_display_name(w.buffer)).unwrap_or_default(),
            NodeRef::Container(_) => {
                let mut ws = Vec::new();
                if let Some(n) = self.layout.node(r) {
                    n.leaves(&mut ws);
                }
                format!("{} windows", ws.len())
            }
        }
    }

    /// The lowest unused numeric workspace name.
    pub fn next_free_workspace(&self) -> String {
        let names = self.workspace_names();
        (1..).map(|i: u64| i.to_string()).find(|n| !names.contains(n)).unwrap()
    }
}

pub fn install(ed: &mut Editor) {
    ed.defvar("mouse-middle-click-closes", json!(true), "Middle-clicking a window header or tab closes it.");
    ed.defvar("mouse-double-click-fullscreen", json!(true), "Double-clicking a window header toggles fullscreen.");
    ed.defvar("mouse-drop-edge", json!(0.28), "Fraction of a window, from each side, where a drop splits instead of making tabs.");
    ed.defvar(
        "window-menu",
        json!([
            {"label": "Close window", "command": "close-window"},
            {"label": "Kill buffer", "command": "kill-current-buffer"},
            "-",
            {"label": "Split right", "command": "split-window-right"},
            {"label": "Split below", "command": "split-window-below"},
            {"label": "Fullscreen", "command": "window-fullscreen-toggle"},
            {"label": "Layout", "items": [
                {"label": "Side by side", "command": "layout", "args": ["splith"]},
                {"label": "Stacked top to bottom", "command": "layout", "args": ["splitv"]},
                {"label": "Tabbed", "command": "layout", "args": ["tabbed"]},
                {"label": "Stacked titles", "command": "layout", "args": ["stacked"]},
            ]},
            {"label": "Move to workspace", "workspaces": "workspace-move-window"},
            "-",
            {"label": "Select container", "command": "focus-parent"},
            {"label": "Balance windows", "command": "balance-windows"},
            {"label": "Switch buffer…", "command": "switch-to-buffer"},
        ]),
        "Right-click menu of window headers and tabs: {label, command, args?}, {label, items}, {label, workspaces = command}, \"-\".",
    );
    ed.defvar(
        "workspace-menu",
        json!([
            {"label": "Rename…", "command": "workspace-rename"},
            {"label": "Back and forth", "command": "workspace-back-and-forth"},
        ]),
        "Right-click menu of the status bar workspace chips (runs on that workspace).",
    );
    ed.define_command("mouse-drag-cancel", "Cancel a window drag.", BUILTIN, |ed, _| {
        ed.drag = None;
        Ok(())
    });
}

/// A command to run from a menu item.
pub type MenuAction = (String, Vec<Value>);

/// Draw menu `items`; the chosen item lands in `action`.
pub fn menu_ui(ui: &mut egui::Ui, ed: &Editor, items: &Value, action: &mut Option<MenuAction>) {
    let keys = crate::commands::all_bindings(ed);
    let workspaces: Vec<(String, String)> = ed.workspace_names().into_iter().map(|n| (ed.workspace_display(&n), n)).collect();
    let next = ed.next_free_workspace();
    fn walk(ui: &mut egui::Ui, items: &Value, keys: &std::collections::HashMap<String, String>, wss: &[(String, String)], next: &str, action: &mut Option<MenuAction>) {
        for it in items.as_array().into_iter().flatten() {
            if it.as_str() == Some("-") {
                ui.separator();
                continue;
            }
            let label = it.get("label").and_then(Value::as_str).unwrap_or("?");
            if let Some(sub) = it.get("items") {
                ui.menu_button(label, |ui| walk(ui, sub, keys, wss, next, action));
            } else if let Some(cmd) = it.get("workspaces").and_then(Value::as_str) {
                ui.menu_button(label, |ui| {
                    for (display, name) in wss {
                        if ui.button(display).clicked() {
                            *action = Some((cmd.to_string(), vec![json!(name)]));
                            ui.close();
                        }
                    }
                    ui.separator();
                    if ui.button(format!("New ({next})")).clicked() {
                        *action = Some((cmd.to_string(), vec![json!(next)]));
                        ui.close();
                    }
                });
            } else if let Some(cmd) = it.get("command").and_then(Value::as_str) {
                let args: Vec<Value> = it.get("args").and_then(Value::as_array).cloned().unwrap_or_default();
                let mut b = egui::Button::new(label);
                if args.is_empty() {
                    if let Some(k) = keys.get(cmd) {
                        b = b.shortcut_text(k.as_str());
                    }
                }
                if ui.add(b).clicked() {
                    *action = Some((cmd.to_string(), args));
                    ui.close();
                }
            }
        }
    }
    walk(ui, items, &keys, &workspaces, &next, action);
}

pub fn run_action(ed: &mut Editor, (cmd, params): MenuAction) {
    let args = Args { params, ..Default::default() };
    if let Err(e) = ed.run_command(&cmd, &args) {
        ed.error(format!("{cmd}: {e:#}"));
    }
}

fn zone_for(ed: &Editor, r: Rect, p: Pos2) -> (Zone, Rect) {
    let t = ed.var("mouse-drop-edge").and_then(Value::as_f64).unwrap_or(0.28) as f32;
    let rel = (p - r.min) / r.size();
    let edges = [(rel.x, Dir::Left), (1.0 - rel.x, Dir::Right), (rel.y, Dir::Up), (1.0 - rel.y, Dir::Down)];
    let (d, dir) = edges.into_iter().min_by(|a, b| a.0.total_cmp(&b.0)).unwrap();
    if d >= t {
        return (Zone::Center, r.shrink(r.width().min(r.height()) * 0.12));
    }
    let preview = match dir {
        Dir::Left => Rect::from_min_max(r.min, egui::pos2(r.center().x, r.max.y)),
        Dir::Right => Rect::from_min_max(egui::pos2(r.center().x, r.min.y), r.max),
        Dir::Up => Rect::from_min_max(r.min, egui::pos2(r.max.x, r.center().y)),
        Dir::Down => Rect::from_min_max(egui::pos2(r.min.x, r.center().y), r.max),
    };
    (Zone::Edge(dir), preview)
}

fn hit(ed: &Editor, drag: &Drag, p: Pos2) -> Option<Hit> {
    let inside_drag = |r: NodeRef| ed.layout.within(r, drag.node);
    for t in &ed.drop_targets {
        if let DropTarget::Workspace(name, r) = t {
            if r.contains(p) && *name != ed.workspace {
                return Some(Hit::Workspace(name.clone(), *r));
            }
        }
    }
    for t in ed.drop_targets.iter().rev() {
        if let DropTarget::Tabs { cid, rect, tabs, horizontal } = t {
            if rect.contains(p) && !inside_drag(NodeRef::Container(*cid)) {
                let idx = tabs.iter().position(|r| if *horizontal { p.x < r.center().x } else { p.y < r.center().y }).unwrap_or(tabs.len());
                let line = match (tabs.get(idx), tabs.last()) {
                    (Some(r), _) if *horizontal => Rect::from_center_size(egui::pos2(r.left() - 1.0, r.center().y), egui::vec2(3.0, r.height())),
                    (Some(r), _) => Rect::from_center_size(egui::pos2(r.center().x, r.top() - 1.0), egui::vec2(r.width(), 3.0)),
                    (None, Some(r)) if *horizontal => Rect::from_center_size(egui::pos2(r.right() + 1.0, r.center().y), egui::vec2(3.0, r.height())),
                    (None, Some(r)) => Rect::from_center_size(egui::pos2(r.center().x, r.bottom() + 1.0), egui::vec2(r.width(), 3.0)),
                    _ => *rect,
                };
                return Some(Hit::Tab(*cid, idx, line));
            }
        }
    }
    for t in &ed.drop_targets {
        if let DropTarget::Window(w, r) = t {
            if r.contains(p) {
                if inside_drag(NodeRef::Window(*w)) {
                    return None;
                }
                let (zone, preview) = zone_for(ed, *r, p);
                return Some(Hit::Window(*w, zone, preview));
            }
        }
    }
    None
}

/// Follow an active drag: preview the drop under the pointer, and drop on release.
pub fn update_drag(ui: &mut egui::Ui, ed: &mut Editor) {
    let Some(drag) = ed.drag.clone() else { return };
    if ed.layout.path(drag.node).is_none() {
        ed.drag = None;
        return;
    }
    ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    let (pos, released) = ui.input(|i| (i.pointer.latest_pos(), i.pointer.primary_released() || !i.pointer.primary_down()));
    let Some(p) = pos else { return };
    let target = hit(ed, &drag, p);
    let theme = ed.theme.clone();
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("window-drag")));
    let accent = theme.accent;
    match &target {
        Some(Hit::Window(_, _, r)) | Some(Hit::Workspace(_, r)) => {
            painter.rect_filled(*r, theme.radius + 2, accent.gamma_multiply(0.22));
            painter.rect_stroke(*r, theme.radius + 2, Stroke::new(2.0, accent), StrokeKind::Inside);
        }
        Some(Hit::Tab(_, _, line)) => {
            painter.rect_filled(*line, 1.0, accent);
        }
        None => {}
    }
    let font = FontId::proportional(theme.font_size - 1.0);
    let g = painter.layout_no_wrap(drag.label.clone(), font, theme.fg);
    let r = Rect::from_min_size(p + egui::vec2(14.0, 10.0), g.size() + egui::vec2(16.0, 8.0));
    painter.rect_filled(r, 6, theme.bg_elevated);
    painter.rect_stroke(r, 6, Stroke::new(1.0, accent), StrokeKind::Inside);
    painter.galley(r.min + egui::vec2(8.0, 4.0), g, theme.fg);
    if released {
        ed.drag = None;
        match target {
            Some(Hit::Window(w, zone, _)) => {
                ed.drop_node(drag.node, NodeRef::Window(w), zone);
            }
            Some(Hit::Tab(cid, idx, _)) => {
                ed.drop_into(drag.node, cid, idx);
            }
            Some(Hit::Workspace(name, _)) => {
                if ed.move_to_workspace(drag.node, &name, false) {
                    let shown = ed.workspace_display(&name);
                    ed.message(format!("Moved to workspace {shown}"));
                }
            }
            None => {}
        }
    }
    ui.ctx().request_repaint();
}

/// Workspace chips for the status bar, right-aligned to end at `right`; returns their left
/// edge. Click switches, right-click opens `workspace-menu`, and during a drag they (plus a
/// "+" chip for a new workspace) accept drops.
pub fn workspace_chips(ui: &mut egui::Ui, ed: &mut Editor, right: f32, cy: f32) -> f32 {
    if !ed.var("workspace-show-in-status").and_then(Value::as_bool).unwrap_or(true) {
        return right;
    }
    let theme = ed.theme.clone();
    let mut names: Vec<String> = ed.workspace_names().into_iter().filter(|n| *n == ed.workspace || !ed.workspace_is_empty(n)).collect();
    let dragging = ed.drag.is_some();
    if dragging {
        names.push(ed.next_free_workspace());
    }
    let font = FontId::proportional(theme.font_size - 2.0);
    let mut x = right;
    let mut chips = Vec::new();
    for (i, n) in names.iter().enumerate().rev() {
        let is_new = dragging && i == names.len() - 1;
        let label = if is_new { "+".to_string() } else { ed.workspace_display(n) };
        let g = ui.painter().layout_no_wrap(label, font.clone(), theme.fg);
        let w = g.size().x + 14.0;
        let r = Rect::from_min_size(egui::pos2(x - w, cy - 10.0), egui::vec2(w, 20.0));
        x -= w + 4.0;
        chips.push((n.clone(), r, g));
    }
    let mut action: Option<(String, MenuAction)> = None;
    let mut switch = None;
    for (n, r, g) in chips {
        let current = n == ed.workspace;
        let resp = ui.interact(r, ui.scope_id().with(("ws-chip", n.clone())), egui::Sense::click());
        let bg = if current { theme.accent } else if resp.hovered() { theme.border } else { theme.bg_elevated };
        let fg = if current { theme.bg } else { theme.fg_dim };
        ui.painter().rect_filled(r, 10, bg);
        ui.painter().galley(r.center() - g.size() / 2.0, g, fg);
        if resp.clicked() {
            switch = Some(n.clone());
        }
        let items = ed.var("workspace-menu").cloned().unwrap_or(Value::Null);
        let mut chosen = None;
        resp.context_menu(|ui| menu_ui(ui, ed, &items, &mut chosen));
        if let Some(c) = chosen {
            action = Some((n.clone(), c));
        }
        ed.drop_targets.push(DropTarget::Workspace(n, r));
    }
    if let Some(n) = switch {
        ed.switch_workspace(&n);
    }
    if let Some((n, a)) = action {
        if n != ed.workspace {
            ed.switch_workspace(&n);
        }
        run_action(ed, a);
    }
    x
}

/// A small "drag me" grip.
pub fn paint_grip(p: &egui::Painter, r: Rect, color: Color32) {
    for row in 0..3 {
        for col in 0..2 {
            let c = egui::pos2(r.center().x - 2.5 + col as f32 * 5.0, r.center().y - 5.0 + row as f32 * 5.0);
            p.circle_filled(c, 1.3, color);
        }
    }
}
