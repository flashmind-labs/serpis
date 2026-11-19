//! Glyphs a terminal draws itself rather than taking from a font, as Ghostty does: powerline
//! separators are shapes filling the cell exactly (a font's version never matches the line
//! height, so rounded caps come out clipped), and Nerd Font icons are scaled to the cell by
//! their ink, not their nominal size, and may spread into a following blank cell.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use egui::{Color32, FontId, Mesh, Pos2, Rect, Shape, Stroke, Vec2, pos2};

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

/// Draw a block element (U+2580..259F) or box-drawing character (U+2500..257F) in `cell`,
/// as Ghostty does (`sprite/draw/block.zig`, `box.zig`): shapes on whole device pixels, so
/// neighbouring cells meet exactly. A font's glyphs, placed at fractional cell positions,
/// leave seams where two blocks meet. False for other chars.
pub fn block_or_box(c: char, cell: Rect, ppp: f32, color: Color32, out: &mut Vec<Shape>) -> bool {
    let g = Px::new(cell, ppp, color);
    match c as u32 {
        0x2580..=0x259F => g.block(c, out),
        0x2500..=0x257F => g.box_drawing(c, out),
        _ => false,
    }
}

/// A cell on the device pixel grid: edges rounded the same way for every cell, so a cell's
/// right edge is its neighbour's left edge.
struct Px {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    ppp: f32,
    color: Color32,
}

impl Px {
    fn new(cell: Rect, ppp: f32, color: Color32) -> Self {
        let r = |v: f32| (v * ppp).round();
        Px { x0: r(cell.left()), y0: r(cell.top()), x1: r(cell.right()), y1: r(cell.bottom()), ppp, color }
    }
    fn w(&self) -> f32 {
        self.x1 - self.x0
    }
    fn h(&self) -> f32 {
        self.y1 - self.y0
    }
    /// A filled rectangle between device pixel coordinates: a plain mesh, not a rect shape,
    /// which egui feathers (and draws as a line segment when thin, moved to a pixel centre).
    fn fill(&self, x0: f32, y0: f32, x1: f32, y1: f32, color: Color32, out: &mut Vec<Shape>) {
        if x1 > x0 && y1 > y0 {
            let s = 1.0 / self.ppp;
            let mut mesh = Mesh::default();
            mesh.add_colored_rect(Rect::from_min_max(pos2(x0 * s, y0 * s), pos2(x1 * s, y1 * s)), color);
            out.push(Shape::mesh(mesh));
        }
    }
    /// The cell split at `f` of its width (rounded to a pixel).
    fn fx(&self, f: f32) -> f32 {
        self.x0 + (self.w() * f).round()
    }
    fn fy(&self, f: f32) -> f32 {
        self.y0 + (self.h() * f).round()
    }

    fn block(&self, c: char, out: &mut Vec<Shape>) -> bool {
        let (x0, y0, x1, y1, col) = (self.x0, self.y0, self.x1, self.y1, self.color);
        let u = c as u32;
        match u {
            0x2580 => self.fill(x0, y0, x1, self.fy(0.5), col, out),
            // Lower one eighth .. full block.
            0x2581..=0x2588 => self.fill(x0, self.fy(1.0 - (u - 0x2580) as f32 / 8.0), x1, y1, col, out),
            // Left seven eighths .. left one eighth.
            0x2589..=0x258F => self.fill(x0, y0, self.fx((0x2590 - u) as f32 / 8.0), y1, col, out),
            0x2590 => self.fill(self.fx(0.5), y0, x1, y1, col, out),
            0x2591..=0x2593 => self.fill(x0, y0, x1, y1, col.gamma_multiply((u - 0x2590) as f32 / 4.0), out),
            0x2594 => self.fill(x0, y0, x1, self.fy(1.0 / 8.0), col, out),
            0x2595 => self.fill(self.fx(7.0 / 8.0), y0, x1, y1, col, out),
            _ => {
                // Quadrants: upper left 1, upper right 2, lower left 4, lower right 8.
                const Q: [u8; 10] = [4, 8, 1, 1 | 4 | 8, 1 | 8, 1 | 2 | 4, 1 | 2 | 8, 2, 2 | 4, 2 | 4 | 8];
                let q = Q[(u - 0x2596) as usize];
                let (mx, my) = (self.fx(0.5), self.fy(0.5));
                for (bit, (a, b, c, d)) in [(1, (x0, y0, mx, my)), (2, (mx, y0, x1, my)), (4, (x0, my, mx, y1)), (8, (mx, my, x1, y1))] {
                    if q & bit != 0 {
                        self.fill(a, b, c, d, col, out);
                    }
                }
            }
        }
        true
    }

    /// Light line thickness in pixels; heavy lines are twice that.
    fn light(&self) -> f32 {
        (self.h() / 17.0).round().max(1.0)
    }

    fn box_drawing(&self, c: char, out: &mut Vec<Shape>) -> bool {
        let u = c as u32;
        match u {
            0x2504..=0x250B => {
                let i = u - 0x2504;
                return self.dashes(if i < 4 { 3 } else { 4 }, i % 2 == 1, (i / 2) % 2 == 1, out);
            }
            0x254C..=0x254F => {
                let i = u - 0x254C;
                return self.dashes(2, i % 2 == 1, i >= 2, out);
            }
            0x256D..=0x2570 => return self.arc(u - 0x256D, out),
            0x2571..=0x2573 => return self.diagonals(u, out),
            _ => {}
        }
        let Some(w) = box_weights(u) else { return false };
        self.lines(w, out);
        true
    }

    /// Lines from the centre to the cell's edges, `w` (up, right, down, left) each 0 none,
    /// 1 light, 2 heavy, 3 double. They join as Ghostty's do: an arm reaches over the lines
    /// across it, and double lines leave their inner corners open.
    fn lines(&self, [up, right, down, left]: [u8; 4], out: &mut Vec<Shape>) {
        let (x0, y0, x1, y1, col) = (self.x0, self.y0, self.x1, self.y1, self.color);
        let t = self.light();
        let thick = |w: u8| if w == 2 { 2.0 * t } else { t };
        // Where a line of thickness `th` starts across the cell (centred, on a pixel).
        let hpos = |th: f32| y0 + ((self.h() - th) / 2.0).floor();
        let vpos = |th: f32| x0 + ((self.w() - th) / 2.0).floor();
        let (cx, cy) = (self.fx(0.5), self.fy(0.5));
        // A double line's two strokes: the first at a, the second at a + 2t.
        let (ha, va) = (hpos(3.0 * t), vpos(3.0 * t));
        let vdouble = up == 3 || down == 3;
        let hdouble = left == 3 || right == 3;
        let vmax = [up, down].into_iter().filter(|&w| w != 3).map(|w| if w == 0 { 0.0 } else { thick(w) }).fold(0.0, f32::max);
        let hmax = [left, right].into_iter().filter(|&w| w != 3).map(|w| if w == 0 { 0.0 } else { thick(w) }).fold(0.0, f32::max);
        // Horizontal arms: (from, to) along x for the left arm and the right arm, per stroke.
        for (w, is_left) in [(left, true), (right, false)] {
            if w == 0 {
                continue;
            }
            // How far into the centre the arm reaches: (left arm's end, right arm's start).
            let reach = |stroke_top: bool| -> (f32, f32) {
                if vdouble {
                    if w == 3 {
                        // Double meets double: each stroke stops at the vertical stroke on its side,
                        // or runs across where no arm comes from that side.
                        let near = if stroke_top { up == 3 } else { down == 3 };
                        if near { (va + t, va + 2.0 * t) } else { (va + 3.0 * t, va) }
                    } else if up == 3 && down == 3 {
                        (va + t, va + 2.0 * t)
                    } else {
                        (va + 3.0 * t, va)
                    }
                } else if vmax > 0.0 {
                    let v = vpos(vmax);
                    (v + vmax, v)
                } else {
                    (cx, cx)
                }
            };
            let strokes: Vec<(f32, f32, bool)> =
                if w == 3 { vec![(ha, ha + t, true), (ha + 2.0 * t, ha + 3.0 * t, false)] } else { vec![(hpos(thick(w)), hpos(thick(w)) + thick(w), true)] };
            for (a, b, top) in strokes {
                let (end, start) = reach(top);
                if is_left { self.fill(x0, a, end, b, col, out) } else { self.fill(start, a, x1, b, col, out) }
            }
        }
        for (w, is_up) in [(up, true), (down, false)] {
            if w == 0 {
                continue;
            }
            let reach = |stroke_left: bool| -> (f32, f32) {
                if hdouble {
                    if w == 3 {
                        let near = if stroke_left { left == 3 } else { right == 3 };
                        if near { (ha + t, ha + 2.0 * t) } else { (ha + 3.0 * t, ha) }
                    } else if left == 3 && right == 3 {
                        (ha + t, ha + 2.0 * t)
                    } else {
                        (ha + 3.0 * t, ha)
                    }
                } else if hmax > 0.0 {
                    let h = hpos(hmax);
                    (h + hmax, h)
                } else {
                    (cy, cy)
                }
            };
            let strokes: Vec<(f32, f32, bool)> =
                if w == 3 { vec![(va, va + t, true), (va + 2.0 * t, va + 3.0 * t, false)] } else { vec![(vpos(thick(w)), vpos(thick(w)) + thick(w), true)] };
            for (a, b, l) in strokes {
                let (end, start) = reach(l);
                if is_up { self.fill(a, y0, b, end, col, out) } else { self.fill(a, start, b, y1, col, out) }
            }
        }
    }

    /// `n` dashes across the cell (vertical when `vertical`), each gap split over both ends so
    /// dashes in neighbouring cells are evenly spaced.
    fn dashes(&self, n: u32, heavy: bool, vertical: bool, out: &mut Vec<Shape>) -> bool {
        let th = if heavy { 2.0 * self.light() } else { self.light() };
        let (len, start) = if vertical { (self.h(), self.y0) } else { (self.w(), self.x0) };
        let gap = (len / (2.0 * n as f32)).round().max(1.0);
        for i in 0..n {
            let a = start + (len * i as f32 / n as f32).round() + (gap / 2.0).floor();
            let b = start + (len * (i + 1) as f32 / n as f32).round() - (gap / 2.0).ceil();
            if vertical {
                let x = self.x0 + ((self.w() - th) / 2.0).floor();
                self.fill(x, a, x + th, b, self.color, out);
            } else {
                let y = self.y0 + ((self.h() - th) / 2.0).floor();
                self.fill(a, y, b, y + th, self.color, out);
            }
        }
        true
    }

    /// Rounded corners ╭ ╮ ╯ ╰ (i = 0..3): a quarter circle between the middles of two edges,
    /// on the same centre lines as straight light lines.
    fn arc(&self, i: u32, out: &mut Vec<Shape>) -> bool {
        let t = self.light();
        let s = 1.0 / self.ppp;
        let cx = self.x0 + ((self.w() - t) / 2.0).floor() + t / 2.0;
        let cy = self.y0 + ((self.h() - t) / 2.0).floor() + t / 2.0;
        let r = (self.w() / 2.0).min(self.h() / 2.0);
        // Towards the horizontal edge (sx) and the vertical edge (sy) the arm runs to.
        let (sx, sy) = match i {
            0 => (1.0, 1.0),
            1 => (-1.0, 1.0),
            2 => (-1.0, -1.0),
            _ => (1.0, -1.0),
        };
        let ex = if sx > 0.0 { self.x1 } else { self.x0 };
        let ey = if sy > 0.0 { self.y1 } else { self.y0 };
        let mut pts = vec![pos2(ex, cy)];
        // The circle's centre, r from the corner point along both arms.
        let (ox, oy) = (cx + sx * r, cy + sy * r);
        const N: usize = 16;
        for k in 0..=N {
            let a = std::f32::consts::FRAC_PI_2 * k as f32 / N as f32;
            // From (cx + sx r, cy) on the horizontal arm round to (cx, cy + sy r) on the vertical.
            pts.push(pos2(ox - sx * r * a.sin(), oy - sy * r * a.cos()));
        }
        pts.push(pos2(cx, ey));
        let pts = pts.into_iter().map(|p| pos2(p.x * s, p.y * s)).collect();
        out.push(Shape::line(pts, Stroke::new(t * s, self.color)));
        true
    }

    fn diagonals(&self, u: u32, out: &mut Vec<Shape>) -> bool {
        let s = 1.0 / self.ppp;
        let st = Stroke::new(self.light() * s, self.color);
        let (a, b, c, d) = (pos2(self.x0 * s, self.y0 * s), pos2(self.x1 * s, self.y0 * s), pos2(self.x0 * s, self.y1 * s), pos2(self.x1 * s, self.y1 * s));
        if u != 0x2572 {
            out.push(Shape::line_segment([c, b], st));
        }
        if u != 0x2571 {
            out.push(Shape::line_segment([a, d], st));
        }
        true
    }
}

/// (up, right, down, left) for the straight box-drawing lines: 0 none, 1 light, 2 heavy, 3 double.
fn box_weights(u: u32) -> Option<[u8; 4]> {
    // U+2500..254B, then U+2550..256C, then U+2574..257F; digits are up, right, down, left.
    const A: [&str; 0x4C] = [
        "0101", "0202", "1010", "2020", "", "", "", "", "", "", "", "", // ─ ━ │ ┃ dashes
        "0110", "0210", "0120", "0220", "0011", "0012", "0021", "0022", // ┌ ┍ ┎ ┏ ┐ ┑ ┒ ┓
        "1100", "1200", "2100", "2200", "1001", "1002", "2001", "2002", // └ ┕ ┖ ┗ ┘ ┙ ┚ ┛
        "1110", "1210", "2110", "1120", "2120", "2210", "1220", "2220", // ├ ┝ ┞ ┟ ┠ ┡ ┢ ┣
        "1011", "1012", "2011", "1021", "2021", "2012", "1022", "2022", // ┤ ┥ ┦ ┧ ┨ ┩ ┪ ┫
        "0111", "0112", "0211", "0212", "0121", "0122", "0221", "0222", // ┬ ┭ ┮ ┯ ┰ ┱ ┲ ┳
        "1101", "1102", "1201", "1202", "2101", "2102", "2201", "2202", // ┴ ┵ ┶ ┷ ┸ ┹ ┺ ┻
        "1111", "1112", "1211", "1212", "2111", "1121", "2121", "2112", // ┼ ┽ ┾ ┿ ╀ ╁ ╂ ╃
        "2211", "1122", "1221", "2212", "1222", "2122", "2221", "2222", // ╄ ╅ ╆ ╇ ╈ ╉ ╊ ╋
    ];
    const B: [&str; 0x1D] = [
        "0303", "3030", "0310", "0130", "0330", "0013", "0031", "0033", // ═ ║ ╒ ╓ ╔ ╕ ╖ ╗
        "1300", "3100", "3300", "1003", "3001", "3003", "1310", "3130", // ╘ ╙ ╚ ╛ ╜ ╝ ╞ ╟
        "3330", "1013", "3031", "3033", "0313", "0131", "0333", "1303", // ╠ ╡ ╢ ╣ ╤ ╥ ╦ ╧
        "3101", "3303", "1313", "3131", "3333", // ╨ ╩ ╪ ╫ ╬
    ];
    const C: [&str; 12] = ["0001", "1000", "0100", "0010", "0002", "2000", "0200", "0020", "0201", "1020", "0102", "2010"];
    let s = match u {
        0x2500..=0x254B => A[(u - 0x2500) as usize],
        0x2550..=0x256C => B[(u - 0x2550) as usize],
        0x2574..=0x257F => C[(u - 0x2574) as usize],
        _ => return None,
    };
    let b = s.as_bytes();
    (b.len() == 4).then(|| [b[0] - b'0', b[1] - b'0', b[2] - b'0', b[3] - b'0'])
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

    /// The device pixel rectangles a glyph fills (from its meshes), at `ppp`.
    fn rects(c: char, cell: Rect, ppp: f32) -> Vec<[f32; 4]> {
        let mut out = Vec::new();
        assert!(block_or_box(c, cell, ppp, Color32::WHITE, &mut out), "{c} not drawn");
        out.iter()
            .map(|s| match s {
                Shape::Mesh(m) => {
                    let r = m.calc_bounds();
                    [r.left() * ppp, r.top() * ppp, r.right() * ppp, r.bottom() * ppp]
                }
                _ => panic!("{c}: not a mesh"),
            })
            .collect()
    }

    #[test]
    fn blocks_meet_on_whole_pixels() {
        // A fractional cell (a font's advance), at 2x: neighbours share their edge exactly.
        let ppp = 2.0;
        let cell = |col: f32, row: f32| Rect::from_min_size(pos2(3.3 + col * 7.83, 5.1 + row * 17.6), egui::vec2(7.83, 17.6));
        let mut right_edges = Vec::new();
        for col in 0..6 {
            let r = rects('█', cell(col as f32, 0.0), ppp);
            for v in r[0] {
                assert_eq!(v, v.round(), "{v} not on a pixel");
            }
            if col > 0 {
                assert_eq!(r[0][0], right_edges[col - 1], "seam before column {col}");
            }
            right_edges.push(r[0][2]);
        }
        // ▛ is three quadrants, leaving the lower right open; ▐ the right half.
        let full = rects('█', cell(0.0, 0.0), ppp)[0];
        let q = rects('▛', cell(0.0, 0.0), ppp);
        assert_eq!(q.len(), 3);
        let (mx, my) = (q[0][2], q[0][3]);
        assert!(q.iter().all(|r| !(r[0] >= mx && r[1] >= my)));
        let half = rects('▐', cell(0.0, 0.0), ppp)[0];
        assert_eq!((half[0], half[2], half[1], half[3]), (mx, full[2], full[1], full[3]));
        // Every block element and straight box line is drawn.
        for u in (0x2580..=0x259F).chain(0x2500..=0x257F) {
            let c = char::from_u32(u).unwrap();
            let mut out = Vec::new();
            assert!(block_or_box(c, cell(0.0, 0.0), ppp, Color32::WHITE, &mut out) && !out.is_empty(), "U+{u:04X}");
        }
    }

    #[test]
    fn box_lines_join() {
        let ppp = 1.0;
        let cell = Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(9.0, 18.0));
        // ─ spans the cell, centred, one pixel thick at this size; ━ twice that.
        let h: Vec<_> = rects('─', cell, ppp);
        assert_eq!((h.iter().map(|r| r[0]).fold(f32::MAX, f32::min), h.iter().map(|r| r[2]).fold(0.0, f32::max)), (0.0, 9.0));
        assert_eq!(h[0][3] - h[0][1], 1.0);
        assert_eq!(rects('━', cell, ppp)[0][3] - rects('━', cell, ppp)[0][1], 2.0);
        // ┌'s arms overlap at the corner (no notch): the right arm starts at the vertical's left.
        let c = rects('┌', cell, ppp);
        assert_eq!(c[0][0], c[1][0]);
        assert_eq!(c[1][1], c[0][1]);
        // ╔ has an outer and an inner corner: four strokes.
        assert_eq!(rects('╔', cell, ppp).len(), 4);
        // Rounded corners are a stroked path from edge to edge.
        let mut out = Vec::new();
        assert!(block_or_box('╭', cell, ppp, Color32::WHITE, &mut out));
        assert!(matches!(&out[0], Shape::Path(p) if p.points.first().unwrap().x == 9.0 && p.points.last().unwrap().y == 18.0));
    }
}
