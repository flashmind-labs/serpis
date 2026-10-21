//! Images in terminals: the Kitty graphics protocol. libghostty-vt parses the protocol, decodes
//! and stores the images and tracks their placements; this draws them, below or above the text
//! as each placement asks, from textures cached by image generation.

use std::collections::HashMap;

use egui::{Color32, Pos2, Rect, Vec2};
use libghostty_vt::Terminal;
use libghostty_vt::kitty::graphics::{self, ImageFormat, Layer, PlacementIterator};
use libghostty_vt::style::StyleColor;

/// Install the PNG decoder (once per thread; terminals live on the UI thread).
pub fn init() {
    thread_local!(static DONE: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
    if !DONE.get() {
        let _ = graphics::set_png_decoder(Some(Box::new(graphics::RustPngDecoder::new())));
        DONE.set(true);
    }
}

/// The Unicode placeholder character (Kitty graphics "virtual placements", used by yazi and
/// by programs running in tmux): cells of it, colored with the image id, show the image.
pub const PLACEHOLDER: char = '\u{10EEEE}';

/// One cell of a placeholder, as the terminal paints it: viewport position, the image id from
/// its foreground color, the placement id from its underline color, and the row, column and
/// high id byte its diacritics give (any may be missing).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaceholderCell {
    pub x: u32,
    pub y: i32,
    pub id_low: u32,
    pub placement: u32,
    pub row: Option<u32>,
    pub col: Option<u32>,
    pub id_high: Option<u32>,
}

impl PlaceholderCell {
    /// Parse a cell: its grapheme (placeholder first, then diacritics) and raw colors.
    pub fn parse(x: u32, y: i32, grapheme: &str, fg: &StyleColor, underline: &StyleColor) -> Option<Self> {
        let mut cs = grapheme.chars();
        if cs.next()? != PLACEHOLDER {
            return None;
        }
        let mut idx = cs.map(|c| DIACRITICS.binary_search(&(c as u32)).ok().map(|i| i as u32));
        Some(PlaceholderCell {
            x,
            y,
            id_low: color_id(fg),
            placement: color_id(underline),
            row: idx.next().flatten(),
            col: idx.next().flatten(),
            id_high: idx.next().flatten().filter(|h| *h < 256),
        })
    }
}

/// A run of placeholder cells on one row showing consecutive columns of one image's row.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaceholderRun {
    pub x: u32,
    pub y: i32,
    pub image: u32,
    pub placement: u32,
    pub row: u32,
    pub col: u32,
    pub width: u32,
}

/// Group a row's placeholder cells (in order) into runs; Kitty's continuation rules (as Ghostty
/// ports them): a cell without diacritics continues the run on its left.
pub fn runs(cells: &[PlaceholderCell]) -> Vec<PlaceholderRun> {
    let mut out = Vec::new();
    let mut cur: Option<(PlaceholderCell, u32)> = None;
    let complete = |(c, w): (PlaceholderCell, u32)| PlaceholderRun {
        x: c.x,
        y: c.y,
        image: c.id_low | (c.id_high.unwrap_or(0) << 24),
        placement: c.placement,
        row: c.row.unwrap_or(0),
        col: c.col.unwrap_or(0),
        width: w,
    };
    for c in cells {
        if let Some((p, w)) = cur.as_mut() {
            let adjacent = c.x == p.x + *w;
            let fits = adjacent
                && c.id_low == p.id_low
                && c.placement == p.placement
                && c.row.is_none_or(|r| Some(r) == p.row)
                && c.col.is_none_or(|k| Some(k) == p.col.map(|pc| pc + *w))
                && c.id_high.is_none_or(|h| Some(h) == p.id_high);
            if fits {
                *w += 1;
                continue;
            }
            out.push(complete((*p, *w)));
        }
        let mut start = *c;
        start.row.get_or_insert(0);
        start.col.get_or_insert(0);
        cur = Some((start, 1));
    }
    if let Some(c) = cur {
        out.push(complete(c));
    }
    out
}

/// An image id as a color encodes it: a palette index, or the 24 bits of an RGB color.
fn color_id(c: &StyleColor) -> u32 {
    match c {
        StyleColor::None => 0,
        StyleColor::Palette(p) => p.0 as u32,
        StyleColor::Rgb(c) => (c.r as u32) << 16 | (c.g as u32) << 8 | c.b as u32,
    }
}

/// Row/column diacritics of Kitty's Unicode placeholders, by value (kitty's
/// rowcolumn-diacritics.txt, as Ghostty has it). Sorted, for binary search.
const DIACRITICS: [u32; 297] = [
    0x0305, 0x030D, 0x030E, 0x0310, 0x0312, 0x033D, 0x033E, 0x033F, 0x0346, 0x034A,
    0x034B, 0x034C, 0x0350, 0x0351, 0x0352, 0x0357, 0x035B, 0x0363, 0x0364, 0x0365,
    0x0366, 0x0367, 0x0368, 0x0369, 0x036A, 0x036B, 0x036C, 0x036D, 0x036E, 0x036F,
    0x0483, 0x0484, 0x0485, 0x0486, 0x0487, 0x0592, 0x0593, 0x0594, 0x0595, 0x0597,
    0x0598, 0x0599, 0x059C, 0x059D, 0x059E, 0x059F, 0x05A0, 0x05A1, 0x05A8, 0x05A9,
    0x05AB, 0x05AC, 0x05AF, 0x05C4, 0x0610, 0x0611, 0x0612, 0x0613, 0x0614, 0x0615,
    0x0616, 0x0617, 0x0657, 0x0658, 0x0659, 0x065A, 0x065B, 0x065D, 0x065E, 0x06D6,
    0x06D7, 0x06D8, 0x06D9, 0x06DA, 0x06DB, 0x06DC, 0x06DF, 0x06E0, 0x06E1, 0x06E2,
    0x06E4, 0x06E7, 0x06E8, 0x06EB, 0x06EC, 0x0730, 0x0732, 0x0733, 0x0735, 0x0736,
    0x073A, 0x073D, 0x073F, 0x0740, 0x0741, 0x0743, 0x0745, 0x0747, 0x0749, 0x074A,
    0x07EB, 0x07EC, 0x07ED, 0x07EE, 0x07EF, 0x07F0, 0x07F1, 0x07F3, 0x0816, 0x0817,
    0x0818, 0x0819, 0x081B, 0x081C, 0x081D, 0x081E, 0x081F, 0x0820, 0x0821, 0x0822,
    0x0823, 0x0825, 0x0826, 0x0827, 0x0829, 0x082A, 0x082B, 0x082C, 0x082D, 0x0951,
    0x0953, 0x0954, 0x0F82, 0x0F83, 0x0F86, 0x0F87, 0x135D, 0x135E, 0x135F, 0x17DD,
    0x193A, 0x1A17, 0x1A75, 0x1A76, 0x1A77, 0x1A78, 0x1A79, 0x1A7A, 0x1A7B, 0x1A7C,
    0x1B6B, 0x1B6D, 0x1B6E, 0x1B6F, 0x1B70, 0x1B71, 0x1B72, 0x1B73, 0x1CD0, 0x1CD1,
    0x1CD2, 0x1CDA, 0x1CDB, 0x1CE0, 0x1DC0, 0x1DC1, 0x1DC3, 0x1DC4, 0x1DC5, 0x1DC6,
    0x1DC7, 0x1DC8, 0x1DC9, 0x1DCB, 0x1DCC, 0x1DD1, 0x1DD2, 0x1DD3, 0x1DD4, 0x1DD5,
    0x1DD6, 0x1DD7, 0x1DD8, 0x1DD9, 0x1DDA, 0x1DDB, 0x1DDC, 0x1DDD, 0x1DDE, 0x1DDF,
    0x1DE0, 0x1DE1, 0x1DE2, 0x1DE3, 0x1DE4, 0x1DE5, 0x1DE6, 0x1DFE, 0x20D0, 0x20D1,
    0x20D4, 0x20D5, 0x20D6, 0x20D7, 0x20DB, 0x20DC, 0x20E1, 0x20E7, 0x20E9, 0x20F0,
    0x2CEF, 0x2CF0, 0x2CF1, 0x2DE0, 0x2DE1, 0x2DE2, 0x2DE3, 0x2DE4, 0x2DE5, 0x2DE6,
    0x2DE7, 0x2DE8, 0x2DE9, 0x2DEA, 0x2DEB, 0x2DEC, 0x2DED, 0x2DEE, 0x2DEF, 0x2DF0,
    0x2DF1, 0x2DF2, 0x2DF3, 0x2DF4, 0x2DF5, 0x2DF6, 0x2DF7, 0x2DF8, 0x2DF9, 0x2DFA,
    0x2DFB, 0x2DFC, 0x2DFD, 0x2DFE, 0x2DFF, 0xA66F, 0xA67C, 0xA67D, 0xA6F0, 0xA6F1,
    0xA8E0, 0xA8E1, 0xA8E2, 0xA8E3, 0xA8E4, 0xA8E5, 0xA8E6, 0xA8E7, 0xA8E8, 0xA8E9,
    0xA8EA, 0xA8EB, 0xA8EC, 0xA8ED, 0xA8EE, 0xA8EF, 0xA8F0, 0xA8F1, 0xAAB0, 0xAAB2,
    0xAAB3, 0xAAB7, 0xAAB8, 0xAABE, 0xAABF, 0xAAC1, 0xFE20, 0xFE21, 0xFE22, 0xFE23,
    0xFE24, 0xFE25, 0xFE26, 0x10A0F, 0x10A38, 0x1D185, 0x1D186, 0x1D187, 0x1D188, 0x1D189,
    0x1D1AA, 0x1D1AB, 0x1D1AC, 0x1D1AD, 0x1D242, 0x1D243, 0x1D244,
];

/// Where one image was drawn last frame (for tests): image id and rect.
pub type Drawn = (u32, Rect);

pub struct Images {
    iter: Option<PlacementIterator<'static>>,
    /// image id -> (image generation, texture).
    textures: HashMap<u32, (u64, egui::TextureHandle)>,
    pub drawn: Vec<Drawn>,
}

impl Default for Images {
    fn default() -> Self {
        Images { iter: PlacementIterator::new().ok(), textures: HashMap::new(), drawn: Vec::new() }
    }
}

/// How cells map to the screen: top-left of the grid, a cell's size in points and in the
/// pixels the terminal was told about.
pub struct Grid {
    pub origin: Pos2,
    pub cell: Vec2,
    pub cell_px: (u32, u32),
    pub clip: Rect,
}

impl Images {
    /// Draw the placements of one layer. Call `begin` first each frame.
    pub fn paint(&mut self, term: &Terminal, layer: Layer, painter: &egui::Painter, grid: &Grid) {
        let Some(iter) = self.iter.as_mut() else { return };
        let Ok(gfx) = term.kitty_graphics() else { return };
        if gfx.generation().unwrap_or(0) == 0 {
            return;
        }
        let Ok(mut placements) = iter.update(&gfx) else { return };
        let _ = placements.set_layer(layer);
        let painter = painter.with_clip_rect(grid.clip);
        let (sx, sy) = (grid.cell.x / grid.cell_px.0.max(1) as f32, grid.cell.y / grid.cell_px.1.max(1) as f32);
        while let Some(p) = placements.next() {
            let Ok(id) = p.image_id() else { continue };
            let Some(image) = gfx.image(id) else { continue };
            let Ok(info) = p.placement_render_info(&image, term) else { continue };
            if !info.viewport_visible || info.pixel_width == 0 || info.pixel_height == 0 {
                continue;
            }
            let Some(tex) = texture(&mut self.textures, painter.ctx(), id, &image) else { continue };
            let (xo, yo) = (p.x_offset().unwrap_or(0) as f32, p.y_offset().unwrap_or(0) as f32);
            let min = grid.origin + egui::vec2(info.viewport_col as f32 * grid.cell.x + xo * sx, info.viewport_row as f32 * grid.cell.y + yo * sy);
            let rect = Rect::from_min_size(min, egui::vec2(info.pixel_width as f32 * sx, info.pixel_height as f32 * sy));
            let (w, h) = (image.width().unwrap_or(1).max(1) as f32, image.height().unwrap_or(1).max(1) as f32);
            let uv = Rect::from_min_size(
                egui::pos2(info.source_x as f32 / w, info.source_y as f32 / h),
                egui::vec2(info.source_width as f32 / w, info.source_height as f32 / h),
            );
            painter.image(tex, rect, uv, Color32::WHITE);
            self.drawn.push((id, rect));
        }
    }

    /// Draw placeholder runs (virtual placements): each shows its slice of the image, fitted
    /// into the placement's cell box with its aspect ratio kept and centered (Ghostty's math).
    pub fn paint_virtual(&mut self, term: &Terminal, runs: &[PlaceholderRun], painter: &egui::Painter, grid: &Grid) {
        if runs.is_empty() {
            return;
        }
        let Some(iter) = self.iter.as_mut() else { return };
        let Ok(gfx) = term.kitty_graphics() else { return };
        // The virtual placements: (image, placement) -> requested (columns, rows).
        let mut boxes: Vec<(u32, u32, u32, u32)> = Vec::new();
        if let Ok(mut ps) = iter.update(&gfx) {
            // The iterator keeps the last layer filter.
            let _ = ps.set_layer(Layer::All);
            while let Some(p) = ps.next() {
                if p.is_virtual().unwrap_or(false) {
                    boxes.push((p.image_id().unwrap_or(0), p.placement_id().unwrap_or(0), p.columns().unwrap_or(0), p.rows().unwrap_or(0)));
                }
            }
        }
        let painter = painter.with_clip_rect(grid.clip);
        let (cw, ch) = (grid.cell_px.0.max(1) as f64, grid.cell_px.1.max(1) as f64);
        let (sx, sy) = (grid.cell.x / cw as f32, grid.cell.y / ch as f32);
        for r in runs {
            let Some(image) = gfx.image(r.image) else { continue };
            let Some(&(_, _, cols, rows)) = boxes.iter().find(|b| b.0 == r.image && (r.placement == 0 || b.1 == r.placement)) else { continue };
            let (iw, ih) = (image.width().unwrap_or(0) as f64, image.height().unwrap_or(0) as f64);
            if iw == 0.0 || ih == 0.0 {
                continue;
            }
            let rows = if rows == 0 { (ih / ch).ceil() } else { rows as f64 };
            let cols = if cols == 0 { (iw / cw).ceil() } else { cols as f64 };
            let Some((src, dst)) = virtual_slice(iw, ih, cols, rows, cw, ch, r) else { continue };
            let Some(tex) = texture(&mut self.textures, painter.ctx(), r.image, &image) else { continue };
            let min = grid.origin + egui::vec2(r.x as f32 * grid.cell.x + dst.0 as f32 * sx, r.y as f32 * grid.cell.y + dst.1 as f32 * sy);
            let rect = Rect::from_min_size(min, egui::vec2(dst.2 as f32 * sx, dst.3 as f32 * sy));
            let uv = Rect::from_min_size(
                egui::pos2((src.0 / iw) as f32, (src.1 / ih) as f32),
                egui::vec2((src.2 / iw) as f32, (src.3 / ih) as f32),
            );
            painter.image(tex, rect, uv, Color32::WHITE);
            self.drawn.push((r.image, rect));
        }
    }

    /// Start a frame: forget what was drawn, and drop textures of images that are gone.
    pub fn begin(&mut self, term: &Terminal) {
        self.drawn.clear();
        if self.textures.is_empty() {
            return;
        }
        let Ok(gfx) = term.kitty_graphics() else { return };
        self.textures.retain(|id, _| gfx.image(*id).is_some());
    }
}

/// The part of an image a placeholder run shows: (source x, y, w, h) in image pixels and
/// (destination x offset, y offset, w, h) in pixels from the run's top-left cell. The image is
/// fitted into `cols` x `rows` cells keeping its aspect ratio, centered (a port of Ghostty's
/// `renderPlacement`). None when the run falls in the padding around the image.
#[allow(clippy::type_complexity)]
fn virtual_slice(iw: f64, ih: f64, cols: f64, rows: f64, cw: f64, ch: f64, r: &PlaceholderRun) -> Option<((f64, f64, f64, f64), (f64, f64, f64, f64))> {
    let (gw, gh) = (cols * cw, rows * ch);
    // Scale to fit, and the padding (in image pixels) that centers it.
    let scale = if iw * gh > ih * gw { gw / iw } else { gh / ih };
    let (xo, yo) = ((gw - iw * scale) / 2.0 / scale, (gh - ih * scale) / 2.0 / scale);
    let (sw, sh) = (iw + 2.0 * xo, ih + 2.0 * yo);
    let mut src = (sw * (r.col as f64 / cols), sh * (r.row as f64 / rows), sw * (r.width as f64 / cols), sh * (1.0 / rows));
    let mut dst = (0.0, 0.0, r.width as f64 * cw, ch);
    if src.1 < yo {
        let off = yo - src.1;
        src.3 -= off;
        dst.1 = off * scale;
        dst.3 -= off * scale;
        src.1 = 0.0;
        if src.3 > ih {
            src.3 = ih;
            dst.3 = ih * scale;
        }
    } else if src.1 + src.3 > sh - yo {
        src.1 -= yo;
        src.3 = sh - yo - src.1 - yo;
        dst.3 = src.3 * scale;
    } else {
        src.1 -= yo;
    }
    if src.0 < xo {
        let off = xo - src.0;
        src.2 -= off;
        dst.0 = off * scale;
        dst.2 -= off * scale;
        src.0 = 0.0;
        if src.2 > iw {
            src.2 = iw;
            dst.2 = iw * scale;
        }
    } else if src.0 + src.2 > sw - xo {
        src.0 -= xo;
        src.2 = sw - xo - src.0 - xo;
        dst.2 = src.2 * scale;
    } else {
        src.0 -= xo;
    }
    (src.2 > 0.0 && src.3 > 0.0).then_some((src, dst))
}

/// The texture for image `id`, uploaded again when its generation changed.
fn texture(cache: &mut HashMap<u32, (u64, egui::TextureHandle)>, ctx: &egui::Context, id: u32, image: &graphics::Image<'_>) -> Option<egui::TextureId> {
    let generation = image.generation().ok()?;
    if let Some((g, t)) = cache.get(&id)
        && *g == generation
    {
        return Some(t.id());
    }
    let (w, h) = (image.width().ok()? as usize, image.height().ok()? as usize);
    let data = image.data().ok()??;
    let rgba: Vec<u8> = match image.format().ok()? {
        ImageFormat::Rgba => data.to_vec(),
        ImageFormat::Rgb => data.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
        ImageFormat::Gray => data.iter().flat_map(|&g| [g, g, g, 255]).collect(),
        ImageFormat::GrayAlpha => data.as_chunks::<2>().0.iter().flat_map(|p| [p[0], p[0], p[0], p[1]]).collect(),
        _ => return None,
    };
    if rgba.len() != w * h * 4 {
        return None;
    }
    let img = egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba);
    let t = ctx.load_texture(format!("kitty-{id}-{generation}"), img, egui::TextureOptions::LINEAR);
    let tid = t.id();
    cache.insert(id, (generation, t));
    Some(tid)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(x: u32, row: Option<u32>, col: Option<u32>) -> PlaceholderCell {
        PlaceholderCell { x, y: 0, id_low: 7, placement: 0, row, col, id_high: None }
    }

    #[test]
    fn diacritics_decode() {
        let g: String = [PLACEHOLDER, '\u{0305}', '\u{030D}', '\u{030E}'].iter().collect();
        let c = PlaceholderCell::parse(3, 1, &g, &StyleColor::Rgb(libghostty_vt::style::RgbColor { r: 0, g: 1, b: 2 }), &StyleColor::None).unwrap();
        assert_eq!((c.row, c.col, c.id_high, c.id_low), (Some(0), Some(1), Some(2), 258));
        assert!(PlaceholderCell::parse(0, 0, "a", &StyleColor::None, &StyleColor::None).is_none());
    }

    #[test]
    fn cells_without_diacritics_continue_runs() {
        let r = runs(&[cell(0, Some(2), Some(0)), cell(1, None, None), cell(2, None, None), cell(4, None, None)]);
        assert_eq!(r.len(), 2);
        assert_eq!((r[0].row, r[0].col, r[0].width), (2, 0, 3));
        assert_eq!((r[1].x, r[1].row, r[1].col, r[1].width), (4, 0, 0, 1));
        // An explicit column that doesn't follow starts a new run.
        let r = runs(&[cell(0, Some(0), Some(0)), cell(1, Some(0), Some(5))]);
        assert_eq!(r.len(), 2);
    }

    #[test]
    fn slices_fit_and_center() {
        // A square image in a 4x2-cell box of 10x20 px cells (40x40): fills it exactly.
        let run = |row, col, width| PlaceholderRun { x: 0, y: 0, image: 1, placement: 0, row, col, width };
        let (src, dst) = virtual_slice(100.0, 100.0, 4.0, 2.0, 10.0, 20.0, &run(1, 0, 4)).unwrap();
        assert_eq!((src.1.round(), src.3.round(), dst.3.round()), (50.0, 50.0, 20.0));
        // A wide image in a square box: padded above and below, so the top row starts lower.
        let (_, dst) = virtual_slice(200.0, 100.0, 2.0, 2.0, 20.0, 20.0, &run(0, 0, 2)).unwrap();
        assert_eq!(dst.1.round(), 10.0);
    }
}
