//! Frame rendering: the window tree (with draggable dividers and mouse controls in every
//! window header), the echo area / minibuffer, and the which-key popup.

use std::time::Duration;

use egui::{Align2, Color32, CornerRadius, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::buffer::WindowId;
use crate::editor::{Container, Kind, Level, Node, NodeRef};
use crate::packages::vim;
use crate::text::{CursorShape, PlainLines, ViewOptions};
use crate::theme::Face;
use crate::Editor;

const HEADER_H: f32 = 30.0;
const GAP: f32 = 6.0;
const ECHO_H: f32 = 30.0;
/// Height of a tab bar (tabbed) or of one title bar (stacked).
const TAB_H: f32 = 24.0;

pub fn draw(ui: &mut egui::Ui, ed: &mut Editor) {
    let full = ui.max_rect();
    ui.painter().rect_filled(full, 0.0, ed.theme.bg);
    let body = Rect::from_min_max(full.min + Vec2::splat(GAP), egui::pos2(full.right() - GAP, full.bottom() - ECHO_H - GAP));
    draw_layout(ui, ed, body);
    let echo = Rect::from_min_max(egui::pos2(full.left() + GAP, full.bottom() - ECHO_H), egui::pos2(full.right() - GAP, full.bottom()));
    draw_echo(ui, ed, echo);
    if ed.minibuffer.is_some() {
        draw_minibuffer_popup(ui, ed, echo);
    } else if let Some(t) = ed.pending_since {
        if !ed.pending_keys.is_empty() {
            if t.elapsed() > Duration::from_millis(350) {
                draw_which_key(ui, ed, echo);
            } else {
                ui.ctx().request_repaint_after(Duration::from_millis(360));
            }
        }
    }
    for o in &ed.overlays {
        let p = ui.painter();
        p.rect_filled(o.rect, 3.0, o.color);
        p.text(o.rect.center(), Align2::CENTER_CENTER, &o.label, FontId::monospace(ed.theme.mono_size - 1.0), Color32::BLACK);
    }
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
        Node::Window(w) => ed.meta(ed.windows[*w].buffer).map(|m| m.name.clone()).unwrap_or_default(),
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

fn draw_container(ui: &mut egui::Ui, ed: &mut Editor, c: &Container, rect: Rect, rects: &mut Vec<(u64, Rect)>) {
    rects.push((c.id, rect));
    if c.children.is_empty() {
        return;
    }
    if c.kind.single() {
        // Tabs side by side, or one title bar per child stacked on top.
        let n = c.children.len();
        let bars = if c.kind == Kind::Tabbed { 1 } else { n };
        let bar_h = TAB_H * bars as f32;
        let theme = ed.theme.clone();
        let focused_child = c.focus.min(n - 1);
        let holds_focus = |ch: &Node| {
            let mut ws = Vec::new();
            ch.leaves(&mut ws);
            ws.contains(&ed.focused)
        };
        let mut clicked = None;
        for (i, ch) in c.children.iter().enumerate() {
            let r = if c.kind == Kind::Tabbed {
                let w = rect.width() / n as f32;
                Rect::from_min_size(egui::pos2(rect.left() + w * i as f32, rect.top()), egui::vec2(w - 2.0, TAB_H - 2.0))
            } else {
                Rect::from_min_size(egui::pos2(rect.left(), rect.top() + TAB_H * i as f32), egui::vec2(rect.width(), TAB_H - 2.0))
            };
            let active = i == focused_child;
            let resp = ui.interact(r, ui.scope_id().with(("tab", c.id, i)), Sense::click());
            let bg = if active && holds_focus(ch) {
                theme.accent.gamma_multiply(0.35)
            } else if active {
                theme.bg_elevated
            } else if resp.hovered() {
                theme.border
            } else {
                theme.bg_alt
            };
            let p = ui.painter();
            p.rect_filled(r, 4, bg);
            let title = node_title(ed, ch);
            let font = FontId::proportional(theme.font_size - 1.5);
            let g = p.layout_no_wrap(title, font, if active { theme.fg } else { theme.fg_dim });
            let clip = p.with_clip_rect(r.shrink(2.0));
            clip.galley(egui::pos2(r.left() + 8.0, r.center().y - g.size().y / 2.0), g, theme.fg);
            if resp.clicked() {
                clicked = ch.focus_leaf();
            }
        }
        if let Some(w) = clicked {
            ed.focus_window(w);
        }
        let body = Rect::from_min_max(egui::pos2(rect.left(), rect.top() + bar_h), rect.max);
        if let Some(ch) = c.children.get(focused_child) {
            draw_child(ui, ed, ch, body, rects);
        }
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

fn icon_button(ui: &mut egui::Ui, rect: Rect, id: egui::Id, color: Color32, hover_bg: Color32, paint: impl FnOnce(&egui::Painter, Rect, Stroke)) -> egui::Response {
    let resp = ui.interact(rect, id, Sense::click());
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 4.0, hover_bg);
    }
    paint(p, rect.shrink(6.0), Stroke::new(1.3, if resp.hovered() { color.gamma_multiply(1.4) } else { color }));
    resp
}

fn draw_window(ui: &mut egui::Ui, ed: &mut Editor, w: WindowId, rect: Rect) {
    if !ed.windows.contains_key(w) {
        return;
    }
    ed.windows[w].rect = rect;
    let focused = ed.focused == w && ed.minibuffer.is_none();
    let is_focus_win = ed.focused == w;
    let id = ed.windows[w].buffer;
    let theme = ed.theme.clone();
    let r = theme.radius + 2;
    let p = ui.painter();
    p.rect_filled(rect, r, theme.bg_alt);
    p.rect_stroke(rect, r, Stroke::new(1.0, if is_focus_win { theme.border_focused } else { theme.border }), StrokeKind::Inside);

    // Header.
    let header = Rect::from_min_size(rect.min, egui::vec2(rect.width(), HEADER_H));
    p.rect_filled(header.shrink(1.0), CornerRadius { nw: r, ne: r, sw: 0, se: 0 }, theme.bg_elevated);
    p.line_segment([egui::pos2(header.left() + 1.0, header.bottom()), egui::pos2(header.right() - 1.0, header.bottom())], Stroke::new(1.0, theme.border));
    let meta = ed.meta(id).cloned().unwrap_or_default();
    let header_extra = ed.header_text(id);
    let p = ui.painter();
    let (modeline, modified) = ed
        .buffers
        .get(id)
        .and_then(|s| s.buf.as_ref())
        .map(|b| (b.modeline(), b.is_modified()))
        .unwrap_or_default();
    let icon = match meta.major.as_str() {
        "chat" => "◆",
        "terminal" => "❯",
        "help" => "?",
        _ => "≡",
    };
    let small = FontId::proportional(theme.font_size - 1.5);
    let mut x = header.left() + 12.0;
    let cy = header.center().y;
    let icon_color = if modeline.busy { theme.yellow } else if is_focus_win { theme.accent } else { theme.fg_faint };
    let g = p.layout_no_wrap(icon.to_string(), FontId::proportional(theme.font_size), icon_color);
    p.galley(egui::pos2(x, cy - g.size().y / 2.0), g.clone(), icon_color);
    x += g.size().x + 8.0;
    let name = format!("{}{}", meta.name, if modified { " ●" } else { "" });
    let ng = p.layout_no_wrap(name, FontId::proportional(theme.font_size), if is_focus_win { theme.fg } else { theme.fg_dim });
    let name_rect = Rect::from_min_size(egui::pos2(x, cy - ng.size().y / 2.0), ng.size());
    p.galley(name_rect.min, ng.clone(), theme.fg);
    x += ng.size().x + 10.0;
    if !modeline.title.is_empty() {
        let tg = p.layout_no_wrap(modeline.title.clone(), small.clone(), theme.fg_faint);
        p.galley(egui::pos2(x, cy - tg.size().y / 2.0), tg, theme.fg_faint);
    }

    // Right side: buttons, then state badge and status entries.
    let bsz = 22.0;
    let mut bx = header.right() - 8.0 - bsz;
    let wid = ui.scope_id().with(("win", format!("{w:?}")));
    let hover = theme.border;
    let fg = theme.fg_faint;
    let close = icon_button(ui, Rect::from_min_size(egui::pos2(bx, cy - bsz / 2.0), Vec2::splat(bsz)), wid.with("close"), fg, hover, |p, r, s| {
        p.line_segment([r.left_top(), r.right_bottom()], s);
        p.line_segment([r.right_top(), r.left_bottom()], s);
    })
    .on_hover_text("Close window (the buffer stays open)");
    bx -= bsz + 2.0;
    let below = icon_button(ui, Rect::from_min_size(egui::pos2(bx, cy - bsz / 2.0), Vec2::splat(bsz)), wid.with("below"), fg, hover, |p, r, s| {
        p.rect_stroke(r, 2.0, s, StrokeKind::Inside);
        p.line_segment([r.left_center(), r.right_center()], s);
    })
    .on_hover_text("Split below");
    bx -= bsz + 2.0;
    let right = icon_button(ui, Rect::from_min_size(egui::pos2(bx, cy - bsz / 2.0), Vec2::splat(bsz)), wid.with("right"), fg, hover, |p, r, s| {
        p.rect_stroke(r, 2.0, s, StrokeKind::Inside);
        p.line_segment([r.center_top(), r.center_bottom()], s);
    })
    .on_hover_text("Split right");
    bx -= 10.0;
    let p = ui.painter();
    if let Some(st) = vim::state_of(ed, id) {
        let label = st.label();
        let color = match st {
            vim::State::Normal => theme.accent,
            vim::State::Insert => theme.green,
            _ => theme.purple,
        };
        let g = p.layout_no_wrap(label.to_string(), FontId::monospace(10.5), theme.bg);
        let badge = Rect::from_min_size(egui::pos2(bx - g.size().x - 12.0, cy - 9.0), egui::vec2(g.size().x + 12.0, 18.0));
        p.rect_filled(badge, 9.0, if is_focus_win { color } else { color.gamma_multiply(0.45) });
        p.galley(badge.center() - g.size() / 2.0, g, theme.bg);
        bx = badge.left() - 10.0;
    }
    for s in header_extra.iter().chain(modeline.status.iter()).rev() {
        let g = p.layout_no_wrap(s.clone(), small.clone(), theme.fg_faint);
        bx -= g.size().x;
        if bx < name_rect.right() + 20.0 {
            break;
        }
        p.galley(egui::pos2(bx, cy - g.size().y / 2.0), g, theme.fg_faint);
        bx -= 14.0;
    }

    // Header interactions: name opens the buffer switcher, double-click maximizes.
    let head_resp = ui.interact(Rect::from_min_max(header.min, egui::pos2(name_rect.right() + 8.0, header.bottom())), wid.with("head"), Sense::click());
    if head_resp.clicked() {
        ed.focus_window(w);
        ed.call("switch-to-buffer");
    }
    if head_resp.hovered() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
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
    if ui.input(|i| i.pointer.any_pressed()) && ui.rect_contains_pointer(body) && (ed.focused != w || ed.selected.is_some()) {
        ed.focus_window(w);
    }
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt(("body", format!("{w:?}"))));
    child.set_clip_rect(body);
    ed.with_buffer(id, |b, ed| b.ui(&mut child, body, focused, ed));
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

fn draw_echo(ui: &mut egui::Ui, ed: &mut Editor, rect: Rect) {
    let theme = ed.theme.clone();
    let font = FontId::proportional(theme.font_size - 0.5);
    if let Some(mb) = &mut ed.minibuffer {
        let p = ui.painter();
        p.rect_filled(rect, theme.radius, theme.bg_elevated);
        p.rect_stroke(rect, theme.radius, Stroke::new(1.0, theme.border_focused), StrokeKind::Inside);
        let pg = p.layout_no_wrap(mb.prompt.clone(), font.clone(), theme.accent);
        p.galley(egui::pos2(rect.left() + 12.0, rect.center().y - pg.size().y / 2.0), pg.clone(), theme.accent);
        let input_rect = Rect::from_min_max(egui::pos2(rect.left() + 12.0 + pg.size().x, rect.top()), rect.max);
        mb.input.view.padding = egui::vec2(4.0, (rect.height() - 18.0) / 2.0);
        mb.input.view.wrap = false;
        let src = PlainLines { lines: &mb.input.doc.lines, face: Face::Default };
        mb.input.view.show(ui, input_rect, &src, &theme, &ViewOptions { focused: true, cursor: CursorShape::Bar, show_cursor: true });
        let count = format!("{}/{}", mb.filtered.len(), mb.candidates.len());
        if !mb.candidates.is_empty() || mb.dynamic.is_some() {
            ui.painter().text(rect.right_center() - egui::vec2(12.0, 0.0), Align2::RIGHT_CENTER, count, FontId::proportional(theme.font_size - 2.0), theme.fg_faint);
        }
        return;
    }
    let p = ui.painter();
    if let Some((msg, level, _)) = &ed.echo {
        let color = match level {
            Level::Info => theme.fg_dim,
            Level::Warn => theme.yellow,
            Level::Error => theme.red,
        };
        let line = msg.lines().next().unwrap_or("");
        p.text(egui::pos2(rect.left() + 8.0, rect.center().y), Align2::LEFT_CENTER, line, font.clone(), color);
    }
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
    p.text(rect.right_center() - egui::vec2(8.0, 0.0), Align2::RIGHT_CENTER, right, FontId::proportional(theme.font_size - 2.0), theme.fg_faint);
}

fn draw_minibuffer_popup(ui: &mut egui::Ui, ed: &mut Editor, echo: Rect) {
    let theme = ed.theme.clone();
    let Some(mb) = &ed.minibuffer else { return };
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
