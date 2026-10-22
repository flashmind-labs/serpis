//! Frame rendering: the window tree (with draggable dividers and mouse controls in every
//! window header), the echo area / minibuffer, and the which-key popup.

use std::time::Duration;

use serde_json::Value;

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::buffer::WindowId;
use crate::editor::{Container, Kind, Level, Node, NodeRef};
use crate::packages::vim;
use crate::text::{CursorShape, PlainLines, ViewOptions};
use crate::theme::Face;
use crate::Editor;

/// Chrome heights at the default font size; they grow with `Theme::ui_scale`.
const HEADER_H: f32 = 30.0;
const GAP: f32 = 6.0;
const ECHO_H: f32 = 30.0;
/// Height of a tab bar (tabbed) or of one title bar (stacked).
const TAB_H: f32 = 24.0;

pub fn draw(ui: &mut egui::Ui, ed: &mut Editor) {
    let full = ui.max_rect();
    ui.painter().rect_filled(full, 0.0, ed.theme.bg);
    let echo_h = (ECHO_H * ed.theme.ui_scale()).round();
    let mut body = Rect::from_min_max(full.min + Vec2::splat(GAP), egui::pos2(full.right() - GAP, full.bottom() - echo_h - GAP));
    ed.drop_targets.clear();
    if ed.var_bool("sidebar-visible") {
        // Set in points at the default font size; it grows with the text like the rest of the chrome.
        let w = ed.var("sidebar-width").and_then(Value::as_f64).unwrap_or(270.0) as f32 * ed.theme.ui_scale();
        let w = w.clamp(180.0, (body.width() - 200.0).max(180.0));
        // Flush with the window edge, on the app background like a native sidebar.
        let side = Rect::from_min_max(full.min, egui::pos2(full.left() + w, full.bottom() - echo_h));
        body.min.x = side.right() + GAP / 2.0;
        ed.timed(|_| "sidebar".into(), |ed| crate::sidebar::draw(ui, ed, side));
        crate::sidebar::draw_handle(ui, ed, Rect::from_min_max(egui::pos2(side.right(), side.top()), egui::pos2(body.left(), side.bottom())));
    } else {
        ed.sidebar.focused = false;
    }
    draw_layout(ui, ed, body);
    let echo = Rect::from_min_max(egui::pos2(full.left() + GAP, full.bottom() - echo_h), egui::pos2(full.right() - GAP, full.bottom()));
    let palette = ed.var_str("minibuffer-style").as_deref() != Some("bottom");
    draw_echo(ui, ed, echo, palette);
    if ed.minibuffer.is_some() {
        if palette {
            draw_palette(ui, ed, full);
        } else {
            draw_minibuffer_popup(ui, ed, echo);
        }
    } else if let Some(t) = ed.pending_since {
        if !ed.pending_keys.is_empty() {
            if t.elapsed() > Duration::from_millis(350) {
                draw_which_key(ui, ed, echo);
            } else {
                ui.ctx().request_repaint_after(Duration::from_millis(360));
            }
        }
    }
    if !ed.busy.is_empty() && ed.var("busy-indicator").and_then(Value::as_bool).unwrap_or(true) {
        draw_busy(ui, ed, Rect::from_min_max(full.min, egui::pos2(full.right(), echo.top())));
    }
    crate::mouse::update_drag(ui, ed);
    crate::jump::draw(ui, ed);
    for o in &ed.overlays {
        let p = ui.painter();
        p.rect_filled(o.rect, 3.0, o.color);
        p.text(o.rect.center(), Align2::CENTER_CENTER, &o.label, FontId::monospace(ed.theme.mono_size - 1.0), Color32::BLACK);
    }
}

/// Work in progress (`Editor::busy`): a small card per item, centered above the echo area, with a
/// spinner, its label and, after a few seconds, how long it has taken.
fn draw_busy(ui: &mut egui::Ui, ed: &mut Editor, above: Rect) {
    use crate::chrome;
    let t = &ed.theme;
    let k = t.ui_scale();
    let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("serpis-busy"));
    let painter = ui.ctx().layer_painter(layer);
    let font = FontId::proportional(t.font_size - 1.0);
    let time = ui.input(|i| i.time);
    let mut bottom = above.bottom() - 12.0 * k;
    let mut cards = Vec::new();
    for b in ed.busy.iter().rev() {
        let secs = b.since.elapsed().as_secs();
        let label = if secs >= 3 { format!("{} · {secs}s", b.label) } else { b.label.clone() };
        let tw = chrome::text_width(&painter, &label, font.clone());
        let (h, w) = ((34.0 * k).round(), tw + 52.0 * k);
        let card = Rect::from_min_size(egui::pos2(above.center().x - w / 2.0, bottom - h), egui::vec2(w, h));
        let shadow = egui::epaint::Shadow { offset: [0, 6], blur: 20, spread: 0, color: Color32::from_black_alpha(if t.dark { 110 } else { 36 }) };
        painter.add(shadow.as_shape(card, (h / 2.0) as u8));
        painter.rect_filled(card, h / 2.0, t.bg_elevated);
        painter.rect_stroke(card, h / 2.0, Stroke::new(1.0, t.border), StrokeKind::Inside);
        chrome::spinner(&painter, egui::pos2(card.left() + 20.0 * k, card.center().y), 6.0 * k, time, t.accent);
        chrome::text(&painter, egui::pos2(card.left() + 36.0 * k, card.center().y), &label, font.clone(), t.fg, tw + 1.0);
        cards.push(card);
        bottom = card.top() - 8.0 * k;
    }
    // Tests find the newest one.
    if let Some(c) = cards.first() {
        ed.debug_rects.insert("busy", *c);
    }
    ui.ctx().request_repaint_after(Duration::from_millis(50));
}

fn draw_layout(ui: &mut egui::Ui, ed: &mut Editor, body: Rect) {
    if !ed.layout.contains(ed.focused) {
        ed.focused = ed.layout.leaves()[0];
    }
    ed.layout.note_focus(ed.focused);
    // Windows hidden behind tabs keep no rectangle (overlays and jumps skip them).
    for w in ed.layout.leaves() {
        ed.windows[w].rect = Rect::NOTHING;
    }
    if let Some(f) = ed.layout.fullscreen.filter(|f| ed.layout.contains(*f)) {
        draw_window(ui, ed, f, body);
        return;
    }
    let root = ed.layout.root.clone();
    let mut rects = Vec::new();
    draw_container(ui, ed, &root, body, &mut rects);
    if let Some(NodeRef::Container(id)) = ed.selected {
        if let Some((_, r)) = rects.iter().find(|(c, _)| *c == id) {
            ui.painter().rect_stroke(r.expand(GAP / 2.0 - 1.0), ed.theme.radius + 3, Stroke::new(2.0, ed.theme.accent), StrokeKind::Inside);
        }
    }
}

/// A short title for a tab or stack entry: the buffer name, or an i3-style summary.
fn node_title(ed: &Editor, n: &Node) -> String {
    match n {
        Node::Window(w) => ed.buffer_display_name(ed.windows[*w].buffer),
        Node::Container(c) => {
            let k = match c.kind {
                Kind::SplitH => "H",
                Kind::SplitV => "V",
                Kind::Tabbed => "T",
                Kind::Stacked => "S",
            };
            format!("{k}[{}]", c.children.iter().map(|ch| node_title(ed, ch)).collect::<Vec<_>>().join(" "))
        }
    }
}

fn draw_child(ui: &mut egui::Ui, ed: &mut Editor, n: &Node, rect: Rect, rects: &mut Vec<(u64, Rect)>) {
    match n {
        Node::Window(w) => draw_window(ui, ed, *w, rect),
        Node::Container(c) => draw_container(ui, ed, c, rect, rects),
    }
}

/// Something a tab bar asked for this frame (applied after drawing it).
enum TabAct {
    Focus(usize),
    Close(usize),
    Drag(usize),
    Menu(usize, crate::mouse::MenuAction),
    SelectGroup,
    DragGroup,
}

/// Tabs side by side (tabbed) or one title bar per child (stacked), then the focused child.
/// Tabs click to focus, drag to move, close with × or the middle button, right-click for
/// `window-menu`; the grip at the end drags (or, clicked, selects) the whole group; the wheel
/// over the bar switches tabs.
fn draw_tabs(ui: &mut egui::Ui, ed: &mut Editor, c: &Container, rect: Rect, rects: &mut Vec<(u64, Rect)>) {
    let n = c.children.len();
    let tabbed = c.kind == Kind::Tabbed;
    let k = ed.theme.ui_scale();
    let tab_h = (TAB_H * k * if tabbed { 1.3 } else { 1.15 }).round();
    let bar_h = tab_h * if tabbed { 1.0 } else { n as f32 } + if tabbed { 0.0 } else { 4.0 * k };
    let bar = Rect::from_min_size(rect.min, egui::vec2(rect.width(), bar_h));
    let grip_w = 20.0;
    let theme = ed.theme.clone();
    let focused_child = c.focus.min(n - 1);
    let holds_focus = |ch: &Node| {
        let mut ws = Vec::new();
        ch.leaves(&mut ws);
        ws.contains(&ed.focused)
    };
    let items = ed.var("window-menu").cloned().unwrap_or(Value::Null);
    let mut acts = Vec::new();
    let mut tab_rects = Vec::new();
    let font = FontId::proportional(theme.font_size - 1.5);
    let mut active_tab = None;
    for (i, ch) in c.children.iter().enumerate() {
        let r = if tabbed {
            // Browser-style tabs: content-sized up to a cap, left-aligned.
            let w = ((rect.width() - grip_w) / n as f32).min(220.0 * k);
            Rect::from_min_size(egui::pos2(rect.left() + w * i as f32, rect.top()), egui::vec2(w - 2.0, tab_h))
        } else {
            Rect::from_min_size(egui::pos2(rect.left(), rect.top() + tab_h * i as f32), egui::vec2(rect.width() - grip_w - 2.0, tab_h - 2.0))
        };
        tab_rects.push(r);
        let active = i == focused_child;
        let resp = ui.interact(r, ui.scope_id().with(("tab", c.id, i)), Sense::click_and_drag());
        let p = ui.painter();
        let radius = (8.0 * k).round() as u8;
        if tabbed && active {
            // The active tab is part of the window below: same surface, joined outline.
            let cr = CornerRadius { nw: radius, ne: radius, sw: 0, se: 0 };
            p.rect_filled(r, cr, theme.bg_alt);
            let outline = if holds_focus(ch) { crate::chrome::tint(theme.accent, 0.55) } else { theme.border };
            p.rect_stroke(r, cr, Stroke::new(1.0, outline), StrokeKind::Inside);
            active_tab = Some((r, outline));
        } else if active {
            p.rect_filled(r, radius, if holds_focus(ch) { crate::chrome::selected_bg(&theme) } else { crate::chrome::hover_bg(&theme) });
        } else if resp.hovered() {
            p.rect_filled(r.shrink2(egui::vec2(0.0, 3.0 * k)), radius, crate::chrome::hover_bg(&theme));
        }
        let show_x = active || resp.hovered();
        let leaf = ch.focus_leaf().and_then(|w| ed.windows.get(w)).map(|w| w.buffer);
        let kind = leaf.and_then(|b| ed.meta(b)).map(|m| m.major.clone()).unwrap_or_default();
        let ic = egui::pos2(r.left() + 14.0 * k, r.center().y);
        let icol = if active && holds_focus(ch) { theme.accent } else { theme.fg_faint };
        crate::chrome::kind_icon(p, ic, 10.0 * k, if matches!(ch, Node::Container(_)) { "" } else { &kind }, icol);
        let text_right = r.right() - if show_x { 26.0 * k } else { 8.0 * k };
        crate::chrome::text(p, egui::pos2(r.left() + 26.0 * k, r.center().y), &node_title(ed, ch), font.clone(), if active { theme.fg } else { theme.fg_dim }, text_right - r.left() - 26.0 * k);
        if show_x {
            let xr = Rect::from_center_size(egui::pos2(r.right() - 14.0 * k, r.center().y), Vec2::splat(20.0 * k));
            let xresp = crate::chrome::ghost_button(ui, xr, ui.scope_id().with(("tab-x", c.id, i)), &theme, "Close", crate::chrome::cross);
            if xresp.clicked() {
                acts.push(TabAct::Close(i));
                continue;
            }
        }
        if resp.drag_started() {
            acts.push(TabAct::Drag(i));
        } else if resp.middle_clicked() && ed.var_bool("mouse-middle-click-closes") {
            acts.push(TabAct::Close(i));
        } else if resp.clicked() {
            acts.push(TabAct::Focus(i));
        }
        let mut chosen = None;
        resp.context_menu(|ui| crate::mouse::menu_ui(ui, ed, &items, &mut chosen));
        if let Some(a) = chosen {
            acts.push(TabAct::Menu(i, a));
        }
    }
    // The group grip.
    let grip = Rect::from_min_size(egui::pos2(rect.right() - grip_w, rect.top()), egui::vec2(grip_w, tab_h - 2.0));
    let gresp = ui.interact(grip, ui.scope_id().with(("tab-grip", c.id)), Sense::click_and_drag()).on_hover_text("Drag to move this group; click to select it");
    let selected = ed.selected == Some(NodeRef::Container(c.id));
    crate::mouse::paint_grip(ui.painter(), grip, if gresp.hovered() || selected { theme.accent } else { theme.fg_faint });
    if gresp.drag_started() {
        acts.push(TabAct::DragGroup);
    } else if gresp.clicked() {
        acts.push(TabAct::SelectGroup);
    }
    if gresp.hovered() && ed.drag.is_none() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grab);
    }
    // The wheel over the bar steps through tabs.
    if ui.rect_contains_pointer(bar) && ed.drag.is_none() {
        // One tab per wheel event, so a trackpad fling doesn't race through them.
        let step = ui.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::MouseWheel { delta, .. } => Some(if delta.y.abs() > delta.x.abs() { delta.y } else { delta.x }),
                _ => None,
            })
        }).unwrap_or(0.0);
        if step.abs() > 0.5 {
            let next = if step < 0.0 { (focused_child + 1).min(n - 1) } else { focused_child.saturating_sub(1) };
            if next != focused_child {
                acts.push(TabAct::Focus(next));
            }
        }
    }
    ed.drop_targets.push(crate::mouse::DropTarget::Tabs { cid: c.id, rect: bar, tabs: tab_rects, horizontal: tabbed });

    let body = Rect::from_min_max(egui::pos2(rect.left(), rect.top() + bar_h), rect.max);
    if let Some(ch) = c.children.get(focused_child) {
        draw_child(ui, ed, ch, body, rects);
    }
    // Open the window's top edge under the active tab so they read as one surface.
    if let Some((r, _)) = active_tab {
        let seam = Rect::from_min_max(egui::pos2(r.left() + 1.0, body.top() - 1.0), egui::pos2(r.right() - 1.0, body.top() + 1.5));
        ui.painter().rect_filled(seam, 0.0, theme.bg_alt);
    }

    for act in acts {
        match act {
            TabAct::Focus(i) | TabAct::Close(i) | TabAct::Drag(i) | TabAct::Menu(i, _) if i >= c.children.len() => {}
            TabAct::Focus(i) => {
                if let Some(w) = c.children[i].focus_leaf() {
                    ed.focus_window(w);
                }
            }
            TabAct::Close(i) => match c.children[i].as_ref() {
                NodeRef::Window(w) => {
                    ed.focus_window(w);
                    ed.close_window();
                }
                r => {
                    ed.delete_node(r);
                }
            },
            TabAct::Drag(i) => {
                let node = c.children[i].as_ref();
                ed.drag = Some(crate::mouse::Drag { node, label: ed.node_label(node) });
            }
            TabAct::Menu(i, a) => {
                let ch = &c.children[i];
                if let Some(w) = ch.focus_leaf() {
                    ed.focus_window(w);
                }
                if let NodeRef::Container(_) = ch.as_ref() {
                    ed.selected = Some(ch.as_ref());
                }
                crate::mouse::run_action(ed, a);
            }
            TabAct::SelectGroup => {
                if let Some(w) = c.children[focused_child].focus_leaf() {
                    ed.focus_window(w);
                }
                ed.selected = Some(NodeRef::Container(c.id));
            }
            TabAct::DragGroup => {
                let node = NodeRef::Container(c.id);
                ed.drag = Some(crate::mouse::Drag { node, label: ed.node_label(node) });
            }
        }
    }
}

fn draw_container(ui: &mut egui::Ui, ed: &mut Editor, c: &Container, rect: Rect, rects: &mut Vec<(u64, Rect)>) {
    rects.push((c.id, rect));
    if c.children.is_empty() {
        return;
    }
    if c.kind.single() {
        draw_tabs(ui, ed, c, rect, rects);
        return;
    }
    let horizontal = c.kind.horizontal();
    let n = c.children.len();
    let len = if horizontal { rect.width() } else { rect.height() } - GAP * (n - 1) as f32;
    let start = if horizontal { rect.left() } else { rect.top() };
    let mut pos = start;
    for (i, ch) in c.children.iter().enumerate() {
        let size = len * c.sizes.get(i).copied().unwrap_or(1.0 / n as f32);
        let r = if horizontal {
            Rect::from_min_max(egui::pos2(pos, rect.top()), egui::pos2(pos + size, rect.bottom()))
        } else {
            Rect::from_min_max(egui::pos2(rect.left(), pos), egui::pos2(rect.right(), pos + size))
        };
        draw_child(ui, ed, ch, r, rects);
        pos += size;
        if i + 1 < n {
            // Dividers resize the two children around them; double-click evens the container.
            let divider = if horizontal {
                Rect::from_min_max(egui::pos2(pos, rect.top()), egui::pos2(pos + GAP, rect.bottom()))
            } else {
                Rect::from_min_max(egui::pos2(rect.left(), pos), egui::pos2(rect.right(), pos + GAP))
            };
            let resp = ui.interact(divider, ui.scope_id().with(("divider", c.id, i)), Sense::click_and_drag());
            if resp.hovered() || resp.dragged() {
                ui.ctx().set_cursor_icon(if horizontal { egui::CursorIcon::ResizeHorizontal } else { egui::CursorIcon::ResizeVertical });
                ui.painter().rect_filled(divider.shrink2(if horizontal { egui::vec2(2.0, 0.0) } else { egui::vec2(0.0, 2.0) }), 2.0, ed.theme.border_focused.gamma_multiply(0.6));
            }
            if resp.dragged() {
                if let Some(p) = resp.interact_pointer_pos() {
                    // Fraction of the usable length up to the pointer, minus the gaps before it.
                    let along = if horizontal { p.x } else { p.y } - start - GAP * i as f32 - GAP / 2.0;
                    ed.layout.set_divider(c.id, i, along / len);
                }
            }
            if resp.double_clicked() {
                if let Some(path) = ed.layout.path(NodeRef::Container(c.id)) {
                    if let Some(cc) = ed.layout.container_mut(&path) {
                        let k = cc.sizes.len().max(1) as f32;
                        cc.sizes.iter_mut().for_each(|s| *s = 1.0 / k);
                    }
                }
            }
            pos += GAP;
        }
    }
}

fn draw_window(ui: &mut egui::Ui, ed: &mut Editor, w: WindowId, rect: Rect) {
    if !ed.windows.contains_key(w) {
        return;
    }
    ed.windows[w].rect = rect;
    ed.drop_targets.push(crate::mouse::DropTarget::Window(w, rect));
    let focused = ed.focused == w && ed.minibuffer.is_none() && !ed.sidebar.focused;
    let is_focus_win = ed.focused == w;
    let id = ed.windows[w].buffer;
    let theme = ed.theme.clone();
    let k = theme.ui_scale();
    let r = theme.radius + 4;
    let p = ui.painter();
    // The focused window gets a soft accent outline; the others a hairline.
    p.rect_filled(rect, r, theme.bg_alt);
    let outline = if is_focus_win { crate::chrome::tint(theme.accent, 0.55) } else { theme.border };
    p.rect_stroke(rect, r, Stroke::new(1.0, outline), StrokeKind::Inside);

    // Header: what the buffer is (a chat's title, else its name), where it works (project and
    // branch), its status, then ghost buttons.
    let header = Rect::from_min_size(rect.min, egui::vec2(rect.width(), (HEADER_H * 1.2 * k).round()));
    p.line_segment([egui::pos2(header.left() + 1.0, header.bottom()), egui::pos2(header.right() - 1.0, header.bottom())], Stroke::new(1.0, theme.border));
    let meta = ed.meta(id).cloned().unwrap_or_default();
    let header_extra = ed.header_text(id);
    let (modeline, modified) = ed
        .buffers
        .get(id)
        .and_then(|s| s.buf.as_ref())
        .map(|b| (b.modeline(), b.is_modified()))
        .unwrap_or_default();
    // The same name the sidebar and the buffer pickers use.
    let primary = ed.buffer_display_name(id);
    let secondary = String::new();
    let place = match ed.local_directory(id) {
        Some(d) => {
            let repo = ed.repo_of(&d);
            let root = repo.as_ref().map(|r| r.main.clone()).or_else(|| ed.marked_root_of(&d)).unwrap_or(d);
            let name = root.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
            Some((name, repo.map(|r| r.head_label())))
        }
        // A folder on another machine: its name, nothing looked up here.
        None => ed.buffer_directory(id).map(|d| (d.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default(), None)),
    };
    let p = ui.painter();
    let small = FontId::proportional(theme.font_size - 2.0);
    let cy = header.center().y;
    let mut x = header.left() + 14.0 * k;
    let icon_color = if modeline.busy { theme.yellow } else if is_focus_win { theme.accent } else { theme.fg_faint };
    crate::chrome::kind_icon(p, egui::pos2(x + 6.0 * k, cy), 12.0 * k, &meta.major, icon_color);
    x += 22.0 * k;

    // Right side first: buttons, badges, status pills; the title gets what is left.
    let bsz = (26.0 * k).round();
    let mut bx = header.right() - 6.0 * k - bsz;
    let wid = ui.scope_id().with(("win", format!("{w:?}")));
    let button = |ui: &mut egui::Ui, bx: f32, name: &str, tip: &str, paint: fn(&egui::Painter, Rect, Color32)| {
        crate::chrome::ghost_button(ui, Rect::from_min_size(egui::pos2(bx, cy - bsz / 2.0), Vec2::splat(bsz)), wid.with(name), &theme, tip, paint)
    };
    let close = button(ui, bx, "close", "Close window (the buffer stays open)", crate::chrome::cross);
    bx -= bsz;
    let below = button(ui, bx, "below", "Split below", crate::chrome::split_below);
    bx -= bsz;
    let right = button(ui, bx, "right", "Split right", crate::chrome::split_right);
    let buttons_left = bx - 2.0;
    bx -= 8.0 * k;
    let p = ui.painter();
    let badge_font = FontId::proportional(theme.font_size - 3.0);
    // Fullscreen (monocle): say so, with how many windows it hides; clicking restores.
    let mut fullscreen_badge = None;
    let indicator = ed.var_str("fullscreen-indicator").unwrap_or_default();
    if ed.layout.fullscreen == Some(w) && !indicator.is_empty() {
        let hidden = ed.layout.leaves().len().saturating_sub(1);
        let label = if hidden > 0 { format!("{indicator} +{hidden}") } else { indicator };
        let badge = crate::chrome::pill(p, bx, cy, &label, badge_font.clone(), theme.yellow, crate::chrome::tint(theme.yellow, 0.16), true);
        bx = badge.left() - 6.0 * k;
        fullscreen_badge = Some(badge);
    }
    if let Some(st) = vim::state_of(ed, id) {
        let color = match st {
            vim::State::Normal => theme.accent,
            vim::State::Insert => theme.green,
            _ => theme.purple,
        };
        let color = if is_focus_win { color } else { theme.fg_faint };
        let badge = crate::chrome::pill(p, bx, cy, st.label(), badge_font.clone(), color, crate::chrome::tint(color, 0.14), true);
        bx = badge.left() - 6.0 * k;
    }
    let name_font = FontId::proportional(theme.font_size - 0.5);
    let min_title = 80.0 * k;
    let mut status_left = bx;
    let entries: Vec<(&String, Option<&String>)> = header_extra
        .iter()
        .map(|s| (s, None))
        .chain(modeline.status.iter().enumerate().map(|(i, s)| (s, modeline.status_commands.get(i).and_then(Option::as_ref))))
        .collect();
    // Entries with a command become buttons once the header's own click area exists (they
    // must be on top of it to get clicks).
    let mut buttons: Vec<(Rect, String, usize)> = Vec::new();
    for (i, (st, cmd)) in entries.iter().enumerate().rev() {
        if crate::chrome::text_width(ui.painter(), st, small.clone()) + 20.0 > bx - (x + min_title) {
            break;
        }
        let pr = crate::chrome::pill(ui.painter(), bx, cy, st, small.clone(), theme.fg_dim, theme.chip(), true);
        if let Some(cmd) = cmd {
            buttons.push((pr, (*cmd).clone(), i));
        }
        bx = pr.left() - 6.0 * k;
        status_left = pr.left();
    }
    let p = ui.painter();
    // Where it works: project · branch, quietly.
    if let Some((proj, branch)) = place.filter(|(n, _)| !n.is_empty()) {
        let label = match branch.filter(|b| !b.is_empty()) {
            Some(b) => format!("{proj}  ·  {b}"),
            None => proj,
        };
        let w = crate::chrome::text_width(p, &label, small.clone());
        if w + 20.0 < status_left - (x + min_title) {
            let right_edge = status_left - 10.0 * k;
            crate::chrome::text(p, egui::pos2(right_edge - w, cy), &label, small.clone(), theme.fg_faint, w + 1.0);
            status_left = right_edge - w;
        }
    }
    let label = format!("{primary}{}", if modified { "  ●" } else { "" });
    let title_room = status_left - 16.0 * k - x;
    let name_w = crate::chrome::text(p, egui::pos2(x, cy), &label, name_font.clone(), if is_focus_win { theme.fg } else { theme.fg_dim }, title_room);
    let name_rect = Rect::from_min_size(egui::pos2(x, cy - 9.0), egui::vec2(name_w, 18.0));
    if !secondary.is_empty() && title_room - name_w > 40.0 {
        crate::chrome::text(p, egui::pos2(x + name_w + 10.0 * k, cy), &secondary, small.clone(), theme.fg_faint, title_room - name_w - 10.0 * k);
    }

    // Header interactions: click the name for the buffer switcher, drag to move the window
    // (Alt: its container), double-click for fullscreen, middle-click to close, right-click
    // for `window-menu`.
    let head_resp = ui.interact(Rect::from_min_max(header.min, egui::pos2(buttons_left, header.bottom())), wid.with("head"), Sense::click_and_drag());
    // Status entries with a command (a chat's model): a click runs it in this window.
    for (pr, cmd, i) in buttons {
        let resp = ui.interact(pr, wid.with(("status", i)), Sense::click()).on_hover_text(cmd.as_str()).on_hover_cursor(egui::CursorIcon::PointingHand);
        if resp.hovered() {
            ui.painter().rect_filled(pr, pr.height() / 2.0, crate::chrome::hover_bg(&theme));
        }
        ed.debug_rects.insert("header-status-button", pr);
        if resp.clicked() {
            ed.focus_window(w);
            ed.defer(move |ed| ed.call(&cmd));
        }
    }
    let on_name = head_resp.interact_pointer_pos().or(head_resp.hover_pos()).is_some_and(|p| p.x <= name_rect.right() + 8.0);
    if head_resp.drag_started() {
        let alt = ui.input(|i| i.modifiers.alt);
        let node = match ed.selected {
            Some(sel) if ed.layout.within(NodeRef::Window(w), sel) => sel,
            _ if alt => ed.layout.parent_of(NodeRef::Window(w)).map(NodeRef::Container).filter(|c| ed.layout.path(*c).is_some_and(|p| !p.is_empty())).unwrap_or(NodeRef::Window(w)),
            _ => NodeRef::Window(w),
        };
        ed.drag = Some(crate::mouse::Drag { node, label: ed.node_label(node) });
    }
    if head_resp.double_clicked() && ed.var_bool("mouse-double-click-fullscreen") {
        ed.focus_window(w);
        ed.call("window-fullscreen-toggle");
    } else if head_resp.clicked() {
        ed.focus_window(w);
        let on_badge = head_resp.interact_pointer_pos().zip(fullscreen_badge).is_some_and(|(p, b)| b.contains(p));
        if on_badge {
            ed.call("window-fullscreen-toggle");
        } else if on_name {
            ed.call("switch-to-buffer");
        }
    }
    if head_resp.middle_clicked() && ed.var_bool("mouse-middle-click-closes") {
        ed.focus_window(w);
        ed.close_window();
        return;
    }
    if head_resp.hovered() && ed.drag.is_none() {
        ui.ctx().set_cursor_icon(if on_name { egui::CursorIcon::PointingHand } else { egui::CursorIcon::Grab });
    }
    let items = ed.var("window-menu").cloned().unwrap_or(Value::Null);
    let mut chosen = None;
    head_resp.context_menu(|ui| crate::mouse::menu_ui(ui, ed, &items, &mut chosen));
    if let Some(a) = chosen {
        if !(ed.selected.is_some_and(|sel| ed.layout.within(NodeRef::Window(w), sel))) {
            ed.focus_window(w);
        }
        crate::mouse::run_action(ed, a);
        return;
    }
    if close.clicked() {
        ed.focus_window(w);
        ed.close_window();
        return;
    }
    if right.clicked() {
        ed.focus_window(w);
        ed.split_window(false);
        return;
    }
    if below.clicked() {
        ed.focus_window(w);
        ed.split_window(true);
        return;
    }

    // Body.
    let body = Rect::from_min_max(egui::pos2(rect.left() + 1.0, header.bottom() + 1.0), rect.max - Vec2::splat(1.0));
    if ui.input(|i| i.pointer.any_pressed()) && ui.rect_contains_pointer(body) && (ed.focused != w || ed.selected.is_some() || ed.sidebar.focused) {
        ed.focus_window(w);
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt(("body", format!("{w:?}"))));
    child.set_clip_rect(body);
    ed.timed(|ed| format!("draw {}", ed.buffer_display_name(id)), |ed| ed.with_buffer(id, |b, ed| b.ui(&mut child, body, focused, ed)));
    // The body is clipped square, so content backgrounds (a terminal's, a chat's) fill the
    // window's bottom corners: round them off again with what is behind the windows, and draw
    // the outline over the content.
    let p = ui.painter();
    let rr = r as f32;
    for (corner, dx) in [(rect.left_bottom(), 1.0f32), (rect.right_bottom(), -1.0)] {
        let c = egui::pos2(corner.x + dx * rr, corner.y - rr);
        let arc: Vec<Pos2> = (0..=10)
            .map(|i| {
                let a = i as f32 / 10.0 * std::f32::consts::FRAC_PI_2;
                egui::pos2(c.x - dx * rr * a.cos(), c.y + rr * a.sin())
            })
            .collect();
        for pair in arc.windows(2) {
            p.add(egui::Shape::convex_polygon(vec![corner, pair[0], pair[1]], theme.bg, Stroke::NONE));
        }
    }
    p.rect_stroke(rect, r, Stroke::new(1.0, outline), StrokeKind::Inside);
    draw_flashes(ui, ed, id, body);
}

/// Paint fading highlights for `id` on top of its window.
fn draw_flashes(ui: &mut egui::Ui, ed: &mut Editor, id: crate::BufferId, clip: Rect) {
    ed.flashes.retain(|f| f.born.elapsed() < f.duration);
    let flashes: Vec<_> = ed.flashes.iter().filter(|f| f.buffer == id).cloned().collect();
    if flashes.is_empty() {
        return;
    }
    let painter = ui.painter_at(clip);
    let rects: Vec<(Rect, Color32)> = ed
        .with_buffer(id, |b, _| {
            let mut out = Vec::new();
            let Some(s) = b.surface() else { return out };
            s.undo_boundary();
            let vis = s.visible_lines();
            for f in &flashes {
                // Fade out over the second half of the duration.
                let t = f.born.elapsed().as_secs_f32() / f.duration.as_secs_f32();
                let alpha = if t < 0.5 { 1.0 } else { (1.0 - t) * 2.0 };
                let color = f.color.gamma_multiply(alpha.clamp(0.0, 1.0));
                let first = f.start.line.max(vis.start);
                let last = f.end.line.min(vis.end.saturating_sub(1));
                for l in first..=last {
                    let s_col = if l == f.start.line { f.start.col } else { 0 };
                    let e_col = if l == f.end.line { f.end.col } else { s.line_len(l) };
                    if l == f.end.line && e_col == 0 && l > f.start.line {
                        continue; // exclusive end at a line start
                    }
                    let Some(a) = s.screen_rect(crate::Pos::new(l, s_col)) else { continue };
                    let r = if e_col > s_col {
                        s.screen_rect(crate::Pos::new(l, e_col - 1)).map(|b| a.union(b)).unwrap_or(a)
                    } else {
                        a
                    };
                    out.push((r, color));
                }
            }
            out
        })
        .unwrap_or_default();
    for (r, c) in rects {
        painter.rect_filled(r, 2.0, c);
    }
    ui.ctx().request_repaint();
}

fn draw_echo(ui: &mut egui::Ui, ed: &mut Editor, rect: Rect, palette: bool) {
    let theme = ed.theme.clone();
    let font = FontId::proportional(theme.font_size - 1.0);
    if let Some(mb) = ed.minibuffer.as_mut().filter(|_| !palette) {
        let p = ui.painter();
        p.rect_filled(rect, theme.radius, theme.bg_elevated);
        p.rect_stroke(rect, theme.radius, Stroke::new(1.0, theme.border_focused), StrokeKind::Inside);
        let pg = p.layout_no_wrap(mb.prompt.clone(), font.clone(), theme.accent);
        p.galley(egui::pos2(rect.left() + 12.0, rect.center().y - pg.size().y / 2.0), pg.clone(), theme.accent);
        let input_rect = Rect::from_min_max(egui::pos2(rect.left() + 12.0 + pg.size().x, rect.top()), rect.max);
        mb.input.view.padding = egui::vec2(4.0, (rect.height() - 18.0) / 2.0);
        mb.input.view.wrap = false;
        let src = PlainLines { lines: &mb.input.doc.lines, face: Face::Default };
        mb.input.view.show(ui, input_rect, &src, &theme, &ViewOptions { focused: true, cursor: CursorShape::Bar, show_cursor: true, selection: Default::default() });
        let count = format!("{}/{}", mb.filtered.len(), mb.candidates.len());
        if !mb.candidates.is_empty() || mb.dynamic.is_some() {
            ui.painter().text(rect.right_center() - egui::vec2(12.0, 0.0), Align2::RIGHT_CENTER, count, FontId::proportional(theme.font_size - 2.0), theme.fg_faint);
        }
        return;
    }
    // The sidebar toggle, always reachable at the bottom left.
    let k = theme.ui_scale();
    let bsz = (rect.height() - 4.0).min(26.0 * k);
    let toggle = Rect::from_min_size(egui::pos2(rect.left(), rect.center().y - bsz / 2.0), Vec2::splat(bsz));
    let shown = ed.var_bool("sidebar-visible");
    ed.debug_rects.insert("sidebar-toggle", toggle);
    let keys = crate::commands::all_bindings(ed);
    let tip = crate::chrome::with_key(if shown { "Hide the sidebar" } else { "Show the sidebar" }, &keys, "sidebar-toggle");
    if crate::chrome::ghost_button(ui, toggle, ui.scope_id().with("sidebar-toggle"), &theme, &tip, |p, r, c| crate::chrome::sidebar_icon(p, r.expand(2.0), c, shown)).clicked() {
        crate::sidebar::set_visible(ed, !shown);
    }
    let rect = Rect::from_min_max(egui::pos2(toggle.right() + 2.0, rect.top()), rect.max);
    let p = ui.painter();
    let mut right = String::new();
    if !ed.pending_keys.is_empty() {
        right.push_str(&format!("{}-   ", crate::keys::format_seq(&ed.pending_keys)));
    }
    if let Some(p) = ed.prefix_arg {
        right.push_str(&format!("C-u {p}   "));
    }
    for seg in &ed.status_cache {
        right.push_str(seg);
        right.push_str("   ");
    }
    right.push_str(&ed.var_str("keybinding-scheme").unwrap_or_else(|| "regular".into()));
    let r = p.text(rect.right_center() - egui::vec2(8.0, 0.0), Align2::RIGHT_CENTER, right, FontId::proportional(theme.font_size - 2.0), theme.fg_faint);
    let chips_left = crate::mouse::workspace_chips(ui, ed, r.left() - 12.0, rect.center().y);
    // The message gets the room left of the status and workspace chips.
    if let Some((msg, level, _)) = &ed.echo {
        let color = match level {
            Level::Info => theme.fg_dim,
            Level::Warn => theme.yellow,
            Level::Error => theme.red,
        };
        let line = msg.lines().next().unwrap_or("");
        let clip = Rect::from_min_max(rect.min, egui::pos2(chips_left - 12.0, rect.bottom()));
        ui.painter().with_clip_rect(clip.intersect(ui.clip_rect())).text(egui::pos2(rect.left() + 8.0, rect.center().y), Align2::LEFT_CENTER, line, font.clone(), color);
    }
}

/// The minibuffer as a floating command palette: a card near the top with the prompt and
/// input, then ranked candidates (matched characters highlighted, details on the right).
fn draw_palette(ui: &mut egui::Ui, ed: &mut Editor, full: Rect) {
    use crate::chrome;
    let theme = ed.theme.clone();
    let t = &theme;
    let k = t.ui_scale();
    let layer = egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("serpis-palette"));
    let mut pui = ui.new_child(egui::UiBuilder::new().layer_id(layer).max_rect(full));
    let painter = pui.painter().clone();
    // Dim what is behind, like a modal.
    painter.rect_filled(full, 0.0, Color32::from_black_alpha(if t.dark { 70 } else { 30 }));
    let Some(mb) = ed.minibuffer.as_mut() else { return };
    let w = (680.0 * k).min(full.width() - 40.0);
    let row_h = (34.0 * k).round();
    let input_h = (46.0 * k).round();
    let max_rows = 10usize;
    let n = mb.filtered.len().min(max_rows);
    let list_h = if n > 0 { n as f32 * row_h + 12.0 * k } else { 0.0 };
    let card = Rect::from_min_size(egui::pos2(full.center().x - w / 2.0, full.top() + (full.height() * 0.12).max(40.0)), egui::vec2(w, input_h + list_h));
    let radius = (12.0 * k).round() as u8;
    let shadow = egui::epaint::Shadow { offset: [0, 12], blur: 40, spread: 0, color: Color32::from_black_alpha(if t.dark { 140 } else { 50 }) };
    painter.add(shadow.as_shape(card, radius));
    painter.rect_filled(card, radius, t.bg_elevated);
    painter.rect_stroke(card, radius, Stroke::new(1.0, t.border), StrokeKind::Inside);

    // Input row: icon, prompt, text, count.
    let input = Rect::from_min_size(card.min, egui::vec2(w, input_h));
    let cy = input.center().y;
    chrome::magnifier(&painter, egui::pos2(input.left() + 20.0 * k, cy - 1.0), 5.5 * k, t.fg_faint);
    let prompt = mb.prompt.trim_end().trim_end_matches(':').trim_end().to_string();
    let small = FontId::proportional(t.font_size - 2.0);
    let mut x = input.left() + 38.0 * k;
    if !prompt.is_empty() {
        let pr = chrome::pill(&painter, x, cy, &prompt, small.clone(), t.accent, chrome::tint(t.accent, 0.14), false);
        x = pr.right() + 8.0 * k;
    }
    let count = if !mb.candidates.is_empty() || mb.dynamic.is_some() { format!("{} of {}", mb.filtered.len(), mb.candidates.len()) } else { String::new() };
    let count_w = chrome::text_width(&painter, &count, small.clone());
    let input_rect = Rect::from_min_max(egui::pos2(x, input.top()), egui::pos2(input.right() - count_w - 24.0 * k, input.bottom()));
    mb.input.view.padding = egui::vec2(2.0, (input_h - t.font_size * 1.4) / 2.0);
    mb.input.view.wrap = false;
    let src = PlainLines { lines: &mb.input.doc.lines, face: Face::Default };
    mb.input.view.show(&mut pui, input_rect, &src, t, &ViewOptions { focused: true, cursor: CursorShape::Bar, show_cursor: true, selection: Default::default() });
    if !count.is_empty() {
        chrome::text(&painter, egui::pos2(input.right() - 16.0 * k - count_w, cy), &count, small.clone(), t.fg_faint, count_w + 1.0);
    }

    // More candidates on their way: a moving bar under the input.
    if mb.loading {
        let bar = Rect::from_min_max(egui::pos2(card.left() + radius as f32, input.bottom() - 2.0 * k), egui::pos2(card.right() - radius as f32, input.bottom()));
        chrome::loading_bar(&painter, bar, pui.input(|i| i.time), &[t.accent, t.purple, t.cyan, t.green]);
        pui.ctx().request_repaint();
    }

    // Candidates.
    let mut clicked = None;
    if n > 0 {
        painter.hline(card.left()..=card.right(), input.bottom(), Stroke::new(1.0, t.border));
        let start = mb.selected.saturating_sub(max_rows - 1);
        let font = FontId::proportional(t.font_size - 0.5);
        for (i, (ci, positions)) in mb.filtered.iter().enumerate().skip(start).take(max_rows) {
            let c = &mb.candidates[*ci];
            let y = input.bottom() + 6.0 * k + (i - start) as f32 * row_h;
            let r = Rect::from_min_size(egui::pos2(card.left() + 6.0 * k, y), egui::vec2(w - 12.0 * k, row_h));
            let resp = pui.interact(r, egui::Id::unique(("palette-row", i)), Sense::click());
            if i == mb.selected {
                painter.rect_filled(r, 8.0 * k, chrome::selected_bg(t));
            } else if resp.hovered() {
                painter.rect_filled(r, 8.0 * k, chrome::hover_bg(t));
            }
            if resp.clicked() {
                clicked = Some(i);
            }
            let mut job = egui::text::LayoutJob::default();
            for (ix, ch) in c.label.chars().enumerate() {
                let mut f = t.format(if positions.contains(&ix) { Face::Bold } else { Face::Default });
                f.font_id = font.clone();
                f.color = if positions.contains(&ix) { t.accent } else { t.fg };
                job.append(&ch.to_string(), 0.0, f);
            }
            let g = painter.layout_job(job);
            let label_w = g.size().x;
            painter.galley(egui::pos2(r.left() + 12.0 * k, r.center().y - g.size().y / 2.0), g, t.fg);
            if !c.detail.is_empty() {
                // "s-| Split the window" → a keycap on the right, the description before it.
                let (keys, doc) = match c.detail.split_once("   ") {
                    Some((k, d)) if !k.is_empty() && !k.contains(' ') || k.split(' ').all(|t| t.len() <= 8 && t.contains('-')) => (Some(k.to_string()), d.to_string()),
                    _ => (None, c.detail.clone()),
                };
                let mut right = r.right() - 10.0 * k;
                if let Some(keys) = keys {
                    let kc = chrome::keycap(&painter, right, r.center().y, &keys, t);
                    right = kc.left() - 10.0 * k;
                }
                let max_w = right - (r.left() + 12.0 * k + label_w) - 24.0 * k;
                if max_w > 40.0 {
                    let dw = chrome::text_width(&painter, &doc, small.clone()).min(max_w);
                    chrome::text(&painter, egui::pos2(right - dw, r.center().y), &doc, small.clone(), t.fg_faint, max_w);
                }
            }
        }
    }
    // A click outside the palette dismisses it.
    let outside = pui.input(|i| i.pointer.any_pressed() && i.pointer.interact_pos().is_some_and(|p| !card.contains(p)));
    if let Some(i) = clicked {
        if let Some(mb) = &mut ed.minibuffer {
            mb.selected = i;
        }
        crate::minibuffer::accept(ed);
    } else if outside {
        crate::minibuffer::cancel(ed);
    }
}

fn draw_minibuffer_popup(ui: &mut egui::Ui, ed: &mut Editor, echo: Rect) {
    let theme = ed.theme.clone();
    let Some(mb) = &ed.minibuffer else { return };
    // More candidates on their way: a moving bar along the top of the echo area.
    if mb.loading {
        let bar = Rect::from_min_max(egui::pos2(echo.left(), echo.top()), egui::pos2(echo.right(), echo.top() + 2.0));
        crate::chrome::loading_bar(ui.painter(), bar, ui.input(|i| i.time), &[theme.accent, theme.purple, theme.cyan, theme.green]);
        ui.ctx().request_repaint();
    }
    if mb.filtered.is_empty() {
        return;
    }
    let row = theme.font_size + 10.0;
    let max_rows = 14usize;
    let n = mb.filtered.len().min(max_rows);
    let start = mb.selected.saturating_sub(max_rows - 1);
    let h = n as f32 * row + 12.0;
    let rect = Rect::from_min_max(egui::pos2(echo.left(), echo.top() - h - 6.0), egui::pos2(echo.right(), echo.top() - 6.0));
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("minibuffer-popup")));
    painter.rect_filled(rect.expand(1.0), theme.radius + 2, theme.border);
    painter.rect_filled(rect, theme.radius + 2, theme.bg_elevated);
    let font = FontId::proportional(theme.font_size);
    let small = FontId::proportional(theme.font_size - 2.0);
    let mut clicked = None;
    for (k, (ci, positions)) in mb.filtered.iter().enumerate().skip(start).take(max_rows) {
        let c = &mb.candidates[*ci];
        let y = rect.top() + 6.0 + (k - start) as f32 * row;
        let rrect = Rect::from_min_size(egui::pos2(rect.left() + 6.0, y), egui::vec2(rect.width() - 12.0, row));
        let resp = ui.interact(rrect, egui::Id::unique(("mb-row", k)), Sense::click());
        if k == mb.selected {
            painter.rect_filled(rrect, theme.radius, theme.selection);
        } else if resp.hovered() {
            painter.rect_filled(rrect, theme.radius, theme.border);
        }
        if resp.clicked() {
            clicked = Some(k);
        }
        // Highlight matched characters.
        let mut job = egui::text::LayoutJob::default();
        for (i, ch) in c.label.chars().enumerate() {
            let hit = positions.contains(&i);
            job.append(&ch.to_string(), 0.0, egui::TextFormat {
                font_id: font.clone(),
                color: if hit { theme.accent } else { theme.fg },
                ..Default::default()
            });
        }
        let g = painter.layout_job(job);
        let label_w = g.size().x;
        painter.galley(egui::pos2(rrect.left() + 10.0, rrect.center().y - g.size().y / 2.0), g, theme.fg);
        if !c.detail.is_empty() {
            let max_w = rrect.width() - label_w - 40.0;
            if max_w > 40.0 {
                let mut d = c.detail.clone();
                let dg = painter.layout_no_wrap(d.clone(), small.clone(), theme.fg_faint);
                if dg.size().x > max_w {
                    let keep = ((max_w / dg.size().x) * d.chars().count() as f32) as usize;
                    d = d.chars().take(keep.saturating_sub(1)).collect::<String>() + "…";
                }
                painter.text(rrect.right_center() - egui::vec2(10.0, 0.0), Align2::RIGHT_CENTER, d, small.clone(), theme.fg_faint);
            }
        }
    }
    if let Some(k) = clicked {
        if let Some(mb) = &mut ed.minibuffer {
            mb.selected = k;
        }
        crate::minibuffer::accept(ed);
    }
}

fn draw_which_key(ui: &mut egui::Ui, ed: &mut Editor, echo: Rect) {
    let theme = ed.theme.clone();
    let items = ed.pending_bindings();
    if items.is_empty() {
        return;
    }
    let font = FontId::monospace(theme.mono_size - 0.5);
    let col_w = 260.0;
    let cols = ((echo.width() - 24.0) / col_w).floor().max(1.0) as usize;
    let rows = items.len().div_ceil(cols);
    let row_h = theme.mono_size + 8.0;
    let h = rows as f32 * row_h + 34.0;
    let rect = Rect::from_min_max(egui::pos2(echo.left(), echo.top() - h - 6.0), egui::pos2(echo.right(), echo.top() - 6.0));
    let painter = ui.ctx().layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("which-key")));
    painter.rect_filled(rect.expand(1.0), theme.radius + 2, theme.border);
    painter.rect_filled(rect, theme.radius + 2, theme.bg_elevated);
    painter.text(rect.left_top() + egui::vec2(12.0, 8.0), Align2::LEFT_TOP, format!("{}-", crate::keys::format_seq(&ed.pending_keys)), font.clone(), theme.accent);
    for (i, (k, c)) in items.iter().enumerate() {
        let col = i / rows;
        let row = i % rows;
        let pos = Pos2::new(rect.left() + 12.0 + col as f32 * col_w, rect.top() + 28.0 + row as f32 * row_h);
        painter.text(pos, Align2::LEFT_TOP, k, font.clone(), theme.yellow);
        let color = if c == "+prefix" { theme.purple } else { theme.fg_dim };
        let mut label = c.clone();
        if label.chars().count() > 20 {
            label = label.chars().take(19).collect::<String>() + "…";
        }
        painter.text(pos + egui::vec2(70.0, 0.0), Align2::LEFT_TOP, label, font.clone(), color);
    }
}
