//! Shared visual vocabulary for serpis's own chrome (sidebar, window headers, the chat
//! composer, the status bar): icons drawn as shapes, pills, keycaps, ghost buttons and the
//! spinner, so every surface uses the same sizes, radii and colors.

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, StrokeKind, Vec2};

use crate::theme::Theme;

/// A buffer name as people read it: `*terminal*<2>` → `Terminal 2`, `*chat*` → `Chat`.
pub fn pretty_name(name: &str) -> String {
    let (base, n) = match name.rsplit_once('<') {
        Some((b, rest)) if rest.ends_with('>') => (b, Some(&rest[..rest.len() - 1])),
        _ => (name, None),
    };
    let Some(inner) = base.strip_prefix('*').and_then(|b| b.strip_suffix('*')) else { return name.to_string() };
    let mut out: String = inner.chars().take(1).flat_map(char::to_uppercase).chain(inner.chars().skip(1)).collect();
    if let Some(n) = n {
        out.push(' ');
        out.push_str(n);
    }
    out
}

/// A translucent version of `c` (for tinted backgrounds).
pub fn tint(c: Color32, alpha: f32) -> Color32 {
    let [r, g, b, _] = c.to_array();
    Color32::from_rgba_unmultiplied(r, g, b, (alpha * 255.0) as u8)
}

/// Text or icon color that reads on `fill` (an accent button, the current workspace chip): the
/// theme's darkest or lightest end, whichever contrasts more.
pub fn on(fill: Color32, t: &Theme) -> Color32 {
    fn lum(c: Color32) -> f32 {
        let f = |v: u8| {
            let v = v as f32 / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
    }
    let contrast = |a: Color32, b: Color32| {
        let (x, y) = (lum(a), lum(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    };
    let (dark, light) = if t.dark { (t.bg_alt, t.fg) } else { (t.fg, t.bg_alt) };
    if contrast(fill, dark) >= contrast(fill, light) { dark } else { light }
}

/// Hover/selection washes that work on any background.
pub fn hover_bg(t: &Theme) -> Color32 {
    tint(t.fg, if t.dark { 0.06 } else { 0.05 })
}

pub fn selected_bg(t: &Theme) -> Color32 {
    tint(t.accent, if t.dark { 0.16 } else { 0.12 })
}

/// Text laid out on one line and cut with an ellipsis to fit `max_w`; returns its width.
pub fn text(p: &egui::Painter, pos: Pos2, s: &str, font: FontId, color: Color32, max_w: f32) -> f32 {
    let mut g = p.layout_no_wrap(s.to_string(), font.clone(), color);
    if g.size().x > max_w && max_w > 10.0 {
        let chars: Vec<char> = s.chars().collect();
        let (mut lo, mut hi) = (0, chars.len());
        while lo < hi {
            let mid = (lo + hi).div_ceil(2);
            let w = p.layout_no_wrap(format!("{}…", chars[..mid].iter().collect::<String>()), font.clone(), color).size().x;
            if w <= max_w { lo = mid } else { hi = mid - 1 }
        }
        g = p.layout_no_wrap(format!("{}…", chars[..lo].iter().collect::<String>().trim_end()), font, color);
    }
    let w = g.size().x;
    p.galley(egui::pos2(pos.x, pos.y - g.size().y / 2.0), g, color);
    w
}

pub fn text_width(p: &egui::Painter, s: &str, font: FontId) -> f32 {
    p.layout_no_wrap(s.to_string(), font, Color32::WHITE).size().x
}

/// A rounded label: `fg` on `bg`, vertically centered on `cy`, starting at `x` (or ending
/// there with `right_aligned`). Returns its rectangle.
pub fn pill(p: &egui::Painter, x: f32, cy: f32, label: &str, font: FontId, fg: Color32, bg: Color32, right_aligned: bool) -> Rect {
    let g = p.layout_no_wrap(label.to_string(), font.clone(), fg);
    let h = (font.size + 8.0).round();
    let w = g.size().x + h * 0.8;
    let left = if right_aligned { x - w } else { x };
    let r = Rect::from_min_size(egui::pos2(left, cy - h / 2.0), egui::vec2(w, h));
    p.rect_filled(r, h / 2.0, bg);
    p.galley(r.center() - g.size() / 2.0, g, fg);
    r
}

/// A keyboard shortcut chip (`⌘E`).
pub fn keycap(p: &egui::Painter, right: f32, cy: f32, keys: &str, t: &Theme) -> Rect {
    let font = FontId::proportional(t.font_size - 3.0);
    let g = p.layout_no_wrap(pretty_keys(keys), font, t.fg_faint);
    let h = (t.font_size + 3.0).round();
    let r = Rect::from_min_size(egui::pos2(right - g.size().x - 10.0, cy - h / 2.0), egui::vec2(g.size().x + 10.0, h));
    p.rect_stroke(r, 4.0, Stroke::new(1.0, t.border), StrokeKind::Inside);
    p.galley(r.center() - g.size() / 2.0, g, t.fg_faint);
    r
}

/// `s-e` → `⌘E`, `C-c C-m` → `⌃C ⌃M` (on macOS; other platforms spell modifiers out).
pub fn pretty_keys(keys: &str) -> String {
    keys.split(' ')
        .map(|chord| {
            let mut out = String::new();
            let mut rest = chord;
            while rest.len() > 2 && rest.as_bytes()[1] == b'-' {
                let m = &rest[..1];
                out.push_str(match (m, cfg!(target_os = "macos")) {
                    ("s", true) => "⌘",
                    ("C", true) => "⌃",
                    ("M", true) => "⌥",
                    ("S", true) => "⇧",
                    ("s", false) => "Super+",
                    ("C", false) => "Ctrl+",
                    ("M", false) => "Alt+",
                    _ => "Shift+",
                });
                rest = &rest[2..];
            }
            let key = match rest {
                "RET" => "↩".to_string(),
                "SPC" => "Space".to_string(),
                "ESC" => "Esc".to_string(),
                "TAB" => "⇥".to_string(),
                k if k.chars().count() == 1 && k.chars().all(char::is_lowercase) => k.to_uppercase(),
                k if k.chars().count() == 1 && k.chars().all(char::is_uppercase) => format!("⇧{k}"),
                k => k.to_string(),
            };
            out + &key
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// A square icon button that shows a wash on hover. `paint` draws the icon into a rect.
pub fn ghost_button(ui: &mut egui::Ui, rect: Rect, id: egui::Id, t: &Theme, tip: &str, paint: impl FnOnce(&egui::Painter, Rect, Color32)) -> egui::Response {
    let resp = ui.interact(rect, id, Sense::click()).on_hover_text(tip);
    let p = ui.painter();
    if resp.hovered() {
        p.rect_filled(rect, 6.0, hover_bg(t));
    }
    let inner = Rect::from_center_size(rect.center(), Vec2::splat(rect.height() * 0.46));
    paint(p, inner, if resp.hovered() { t.fg } else { t.fg_faint });
    resp
}

// ------------------------------------------------------------------ icons

pub fn spinner(p: &egui::Painter, c: Pos2, r: f32, time: f64, color: Color32) {
    p.circle_stroke(c, r, Stroke::new(1.5, tint(color, 0.22)));
    let a0 = (time * 5.0) as f32;
    let pts: Vec<Pos2> = (0..=12).map(|i| a0 + i as f32 * 0.2).map(|a| c + Vec2::angled(a) * r).collect();
    p.add(egui::Shape::line(pts, Stroke::new(1.6, color)));
}

pub fn branch(p: &egui::Painter, r: Rect, color: Color32) {
    let s = Stroke::new(1.2, color);
    let a = egui::pos2(r.left() + r.width() * 0.25, r.top() + 1.5);
    let b = egui::pos2(a.x, r.bottom() - 1.5);
    let c = egui::pos2(r.right() - r.width() * 0.2, r.top() + r.height() * 0.3);
    p.line_segment([a, b], s);
    p.add(egui::Shape::line(vec![c, egui::pos2(c.x, r.center().y), egui::pos2(a.x, r.center().y + r.height() * 0.25)], s));
    for q in [a, b, c] {
        p.circle_filled(q, 1.6, color);
    }
}

pub fn chevron(p: &egui::Painter, c: Pos2, open: bool, color: Color32) {
    let s = Stroke::new(1.3, color);
    let pts = if open {
        vec![c + egui::vec2(-3.0, -1.5), c + egui::vec2(0.0, 1.5), c + egui::vec2(3.0, -1.5)]
    } else {
        vec![c + egui::vec2(-1.5, -3.0), c + egui::vec2(1.5, 0.0), c + egui::vec2(-1.5, 3.0)]
    };
    p.add(egui::Shape::line(pts, s));
}

pub fn plus(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.4, c);
    p.line_segment([r.center_top(), r.center_bottom()], s);
    p.line_segment([r.left_center(), r.right_center()], s);
}

pub fn cross(p: &egui::Painter, r: Rect, c: Color32) {
    let r = r.shrink(r.width() * 0.08);
    let s = Stroke::new(1.3, c);
    p.line_segment([r.left_top(), r.right_bottom()], s);
    p.line_segment([r.right_top(), r.left_bottom()], s);
}

pub fn prompt(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.4, c);
    p.add(egui::Shape::line(vec![r.left_top() + egui::vec2(0.0, 1.0), egui::pos2(r.center().x - 1.0, r.center().y), r.left_bottom() - egui::vec2(0.0, 1.0)], s));
    p.line_segment([egui::pos2(r.center().x + 1.0, r.bottom() - 1.0), egui::pos2(r.right(), r.bottom() - 1.0)], s);
}

pub fn diamond(p: &egui::Painter, c: Pos2, r: f32, color: Color32) {
    p.add(egui::Shape::convex_polygon(vec![c + egui::vec2(0.0, -r), c + egui::vec2(r, 0.0), c + egui::vec2(0.0, r), c + egui::vec2(-r, 0.0)], color, Stroke::NONE));
}

pub fn lines(p: &egui::Painter, r: Rect, c: Color32) {
    for k in 0..3 {
        let y = r.top() + 1.0 + k as f32 * (r.height() - 2.0) / 2.0;
        p.line_segment([egui::pos2(r.left(), y), egui::pos2(if k == 2 { r.center().x + 1.0 } else { r.right() }, y)], Stroke::new(1.3, c));
    }
}

pub fn folder(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.3, c);
    let top = r.top() + r.height() * 0.18;
    let tab_w = r.width() * 0.4;
    p.add(egui::Shape::closed_line(
        vec![
            egui::pos2(r.left(), top),
            egui::pos2(r.left() + tab_w, top),
            egui::pos2(r.left() + tab_w + r.width() * 0.1, top + r.height() * 0.14),
            egui::pos2(r.right(), top + r.height() * 0.14),
            egui::pos2(r.right(), r.bottom() - r.height() * 0.08),
            egui::pos2(r.left(), r.bottom() - r.height() * 0.08),
        ],
        s,
    ));
}

/// A window with a sidebar panel on its left (filled when the sidebar shows).
pub fn sidebar_icon(p: &egui::Painter, r: Rect, c: Color32, shown: bool) {
    let s = Stroke::new(1.2, c);
    p.rect_stroke(r, 2.5, s, StrokeKind::Inside);
    let split = r.left() + r.width() * 0.36;
    p.line_segment([egui::pos2(split, r.top()), egui::pos2(split, r.bottom())], s);
    if shown {
        p.rect_filled(Rect::from_min_max(r.min, egui::pos2(split, r.bottom())).shrink(1.5), 1.0, tint(c, 0.6));
    }
}

/// "label (⌘K)" when `cmd` has a key binding, else just "label".
pub fn with_key(label: &str, keys: &std::collections::HashMap<String, String>, cmd: &str) -> String {
    match keys.get(cmd) {
        Some(k) => format!("{label}  ({})", pretty_keys(k)),
        None => label.to_string(),
    }
}

pub fn magnifier(p: &egui::Painter, c: Pos2, r: f32, color: Color32) {
    p.circle_stroke(c, r, Stroke::new(1.3, color));
    p.line_segment([c + egui::vec2(r * 0.72, r * 0.72), c + egui::vec2(r * 1.5, r * 1.5)], Stroke::new(1.4, color));
}

pub fn clock(p: &egui::Painter, c: Pos2, r: f32, color: Color32) {
    p.circle_stroke(c, r, Stroke::new(1.1, color));
    p.line_segment([c, c + egui::vec2(0.0, -r * 0.6)], Stroke::new(1.1, color));
    p.line_segment([c, c + egui::vec2(r * 0.5, 0.0)], Stroke::new(1.1, color));
}

pub fn arrow_up(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.8, c);
    p.line_segment([r.center_bottom(), r.center_top()], s);
    p.add(egui::Shape::line(vec![egui::pos2(r.left(), r.center().y - r.height() * 0.1), r.center_top(), egui::pos2(r.right(), r.center().y - r.height() * 0.1)], s));
}

pub fn split_right(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.2, c);
    p.rect_stroke(r, 2.0, s, StrokeKind::Inside);
    p.line_segment([r.center_top(), r.center_bottom()], s);
}

pub fn split_below(p: &egui::Painter, r: Rect, c: Color32) {
    let s = Stroke::new(1.2, c);
    p.rect_stroke(r, 2.0, s, StrokeKind::Inside);
    p.line_segment([r.left_center(), r.right_center()], s);
}

/// The icon of a buffer kind: a diamond for chats, a prompt for terminals, lines otherwise.
pub fn kind_icon(p: &egui::Painter, c: Pos2, size: f32, kind: &str, color: Color32) {
    let r = Rect::from_center_size(c, Vec2::splat(size));
    match kind {
        "chat" => diamond(p, c, size * 0.48, color),
        "terminal" => prompt(p, r.shrink(size * 0.06), color),
        "help" => {
            p.text(c, Align2::CENTER_CENTER, "?", FontId::proportional(size * 1.2), color);
        }
        _ => lines(p, r.shrink(size * 0.08), color),
    }
}

/// A rounded square with a project's initial, tinted with its color.
pub fn avatar(p: &egui::Painter, c: Pos2, size: f32, name: &str, color: Color32) {
    let r = Rect::from_center_size(c, Vec2::splat(size));
    p.rect_filled(r, size * 0.28, tint(color, 0.22));
    let initial = name.chars().next().map(|c| c.to_uppercase().to_string()).unwrap_or_default();
    p.text(c, Align2::CENTER_CENTER, initial, FontId::proportional(size * 0.62), color);
}

#[cfg(test)]
mod tests {
    #[test]
    fn names_read_like_titles() {
        assert_eq!(super::pretty_name("*terminal*<2>"), "Terminal 2");
        assert_eq!(super::pretty_name("*chat*"), "Chat");
        assert_eq!(super::pretty_name("init.lua"), "init.lua");
    }

    #[test]
    fn keys_read_like_menus() {
        if cfg!(target_os = "macos") {
            assert_eq!(super::pretty_keys("s-e"), "⌘E");
            assert_eq!(super::pretty_keys("s-N"), "⌘⇧N");
            assert_eq!(super::pretty_keys("C-c C-m"), "⌃C ⌃M");
            assert_eq!(super::pretty_keys("s-RET"), "⌘↩");
        }
    }
}
