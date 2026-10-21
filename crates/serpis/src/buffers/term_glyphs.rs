//! Glyphs a terminal draws itself rather than taking from a font, as Ghostty does: powerline
//! separators are shapes filling the cell exactly (a font's version never matches the line
//! height, so rounded caps come out clipped), and Nerd Font icons are scaled to the cell by
//! their ink, not their nominal size, and may spread into a following blank cell.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use egui::{Color32, FontId, Pos2, Rect, Shape, Stroke, Vec2, pos2};

/// Draw a powerline glyph (U+E0B0..E0BF, E0D2, E0D4) filling `cell`; false for other chars.
/// A port of Ghostty's `sprite/draw/powerline.zig`.
pub fn powerline(c: char, cell: Rect, color: Color32, out: &mut Vec<Shape>) -> bool {
    let (x0, y0, w, h) = (cell.left(), cell.top(), cell.width(), cell.height());
    let p = |x: f32, y: f32| pos2(x0 + x, y0 + y);
    // Mirror horizontally within the cell.
    let m = |pts: Vec<Pos2>| pts.into_iter().map(|q| pos2(x0 + w - (q.x - x0), q.y)).collect::<Vec<_>>();
    let thin = Stroke::new((h / 16.0).max(1.0), color);
    let fill = |pts: Vec<Pos2>| Shape::convex_polygon(pts, color, Stroke::NONE);
    match c {
        '\u{E0B0}' => out.push(fill(vec![p(0.0, 0.0), p(w, h / 2.0), p(0.0, h)])),
        '\u{E0B2}' => out.push(fill(vec![p(w, 0.0), p(0.0, h / 2.0), p(w, h)])),
        '\u{E0B1}' => out.push(Shape::line(vec![p(0.0, 0.0), p(w, h / 2.0), p(0.0, h)], thin)),
        '\u{E0B3}' => out.push(Shape::line(m(vec![p(0.0, 0.0), p(w, h / 2.0), p(0.0, h)]), thin)),
        '\u{E0B4}' => out.push(fill(round_cap(cell))),
        '\u{E0B6}' => out.push(fill(m(round_cap(cell)))),
        '\u{E0B5}' => out.push(Shape::line(round_cap(cell), thin)),
        '\u{E0B7}' => out.push(Shape::line(m(round_cap(cell)), thin)),
        '\u{E0B8}' => out.push(fill(vec![p(0.0, 0.0), p(w, h), p(0.0, h)])),
        '\u{E0BA}' => out.push(fill(vec![p(w, 0.0), p(w, h), p(0.0, h)])),
        '\u{E0BC}' => out.push(fill(vec![p(0.0, 0.0), p(w, 0.0), p(0.0, h)])),
        '\u{E0BE}' => out.push(fill(vec![p(0.0, 0.0), p(w, 0.0), p(w, h)])),
        '\u{E0B9}' | '\u{E0BF}' => out.push(Shape::line_segment([p(0.0, 0.0), p(w, h)], thin)),
        '\u{E0BB}' | '\u{E0BD}' => out.push(Shape::line_segment([p(w, 0.0), p(0.0, h)], thin)),
        '\u{E0D2}' | '\u{E0D4}' => {
            let t = thin.width;
            let top = vec![p(0.0, 0.0), p(w, 0.0), p(w / 2.0, h / 2.0 - t / 2.0), p(0.0, h / 2.0 - t / 2.0)];
            let bottom = vec![p(0.0, h), p(0.0, h / 2.0 + t / 2.0), p(w / 2.0, h / 2.0 + t / 2.0), p(w, h)];
            let (top, bottom) = if c == '\u{E0D4}' { (m(top), m(bottom)) } else { (top, bottom) };
            out.push(fill(top));
            out.push(fill(bottom));
        }
        _ => return false,
    }
    true
}

/// The right-facing rounded cap: from the top-left corner, a quarter circle down to the
/// middle, straight, and a quarter circle back to the bottom-left (radius min(w, h/2)).
fn round_cap(cell: Rect) -> Vec<Pos2> {
    let (x0, y0, w, h) = (cell.left(), cell.top(), cell.width(), cell.height());
    let r = w.min(h / 2.0);
    let mut pts = Vec::new();
    const N: usize = 12;
    // Upper quarter: centre (0, r), from angle -90° to 0°.
    for i in 0..=N {
        let a = -std::f32::consts::FRAC_PI_2 + std::f32::consts::FRAC_PI_2 * i as f32 / N as f32;
        pts.push(pos2(x0 + r * a.cos(), y0 + r + r * a.sin()));
    }
    // Lower quarter: centre (0, h - r), from 0° to 90°.
    for i in 0..=N {
        let a = std::f32::consts::FRAC_PI_2 * i as f32 / N as f32;
        pts.push(pos2(x0 + r * a.cos(), y0 + h - r + r * a.sin()));
    }
    pts
}

/// Icon height as a fraction of the row.
const ICON_HEIGHT: f32 = 0.68;
/// How far (in cells) an icon may spread into a following blank cell.
const MAX_SPREAD: f32 = 1.4;

/// Whether `c` is a Nerd Font icon (private use areas, outside the powerline shapes).
pub fn is_icon(c: char) -> bool {
    let u = c as u32;
    ((0xE000..=0xF8FF).contains(&u) && !(0xE0B0..=0xE0D7).contains(&u)) || (0xF0000..=0xFFFFD).contains(&u)
}

/// An icon's ink box at 100 pt: (left, top, width, height) relative to the galley origin.
type Ink = (f32, f32, f32, f32);

fn ink(ctx: &egui::Context, c: char) -> Option<Ink> {
    static CACHE: OnceLock<Mutex<HashMap<char, Option<Ink>>>> = OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(v) = cache.lock().unwrap().get(&c) {
        return *v;
    }
    let g = ctx.fonts_mut(|f| f.layout_no_wrap(c.to_string(), FontId::monospace(100.0), Color32::WHITE));
    let v = g.rows.first().and_then(|r| r.glyphs.first().map(|gl| (r.pos, gl))).filter(|(_, gl)| gl.uv_rect.size.x > 0.0).map(|(rp, gl)| {
        let o = rp.to_vec2() + gl.pos.to_vec2() + gl.uv_rect.offset;
        (o.x, o.y, gl.uv_rect.size.x, gl.uv_rect.size.y)
    });
    cache.lock().unwrap().insert(c, v);
    v
}

/// Draw Nerd Font icon `c` fitted to `cells` cells (1, or 2 when the next cell is blank) at
/// `origin`, centred, its ink about cap height (Ghostty's `fit_cover1`, centred).
pub fn icon(ctx: &egui::Context, c: char, origin: Pos2, cell: Vec2, cells: usize, color: Color32, out: &mut Vec<Shape>) -> bool {
    let Some((ix, iy, iw, ih)) = ink(ctx, c) else { return false };
    // About cap height (Ghostty's icon height), with room so neighbours don't touch.
    let (bw, bh) = (cell.x * if cells > 1 { MAX_SPREAD } else { 1.0 }, cell.y * ICON_HEIGHT);
    let s = (bw / iw).min(bh / ih);
    let size = 100.0 * s;
    let g = ctx.fonts_mut(|f| f.layout_no_wrap(c.to_string(), FontId::monospace(size), color));
    // Centred on the first cell when it fits there (center1), else starting at the cell, so the
    // rest of the blank cell stays a gap before the text; vertically centred on the row.
    let (w, h) = (iw * s, ih * s);
    let x = origin.x + ((cell.x - w) / 2.0).max(0.0) - ix * s;
    let y = origin.y + (cell.y - h) / 2.0 - iy * s;
    out.push(Shape::galley(pos2(x, y), g, color));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rounded_caps_span_the_cell() {
        let cell = Rect::from_min_size(pos2(10.0, 20.0), egui::vec2(8.0, 18.0));
        let pts = round_cap(cell);
        let ys: Vec<f32> = pts.iter().map(|p| p.y).collect();
        assert_eq!(ys.iter().cloned().fold(f32::MAX, f32::min), 20.0);
        assert!((ys.iter().cloned().fold(f32::MIN, f32::max) - 38.0).abs() < 0.01);
        // Reaches the cell's right edge (radius = min(w, h/2) = 8).
        assert!((pts.iter().map(|p| p.x).fold(f32::MIN, f32::max) - 18.0).abs() < 0.01);
        let mut out = Vec::new();
        assert!(powerline('\u{E0B6}', cell, Color32::WHITE, &mut out));
        assert!(!powerline('a', cell, Color32::WHITE, &mut out));
        assert!(is_icon('\u{F07B}') && !is_icon('\u{E0B4}') && !is_icon('a'));
    }
}
