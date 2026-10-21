//! Images shown in serpis (chat attachments, images in transcripts): decoded once, kept by a
//! content id, uploaded as a texture when first drawn. Lines and widgets refer to them by id.

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

use base64::Engine;

/// An image known to the store: its id and size in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ImageRef {
    pub id: u64,
    pub w: u32,
    pub h: u32,
}

impl ImageRef {
    /// Its size in points when shown at most `max_w` x `max_h` (never enlarged).
    pub fn fit(&self, max_w: f32, max_h: f32) -> egui::Vec2 {
        let (w, h) = (self.w.max(1) as f32, self.h.max(1) as f32);
        let s = (max_w / w).min(max_h / h).min(1.0);
        egui::vec2((w * s).max(1.0), (h * s).max(1.0))
    }
}

/// Image bytes as a model receives them: base64 and a mime type.
#[derive(Debug, Clone, PartialEq)]
pub struct Encoded {
    pub mime: String,
    pub data: Arc<str>,
}

struct Stored {
    w: u32,
    h: u32,
    pixels: Option<egui::ColorImage>,
    texture: Option<egui::TextureHandle>,
}

fn store() -> &'static Mutex<HashMap<u64, Stored>> {
    static S: OnceLock<Mutex<HashMap<u64, Stored>>> = OnceLock::new();
    S.get_or_init(Default::default)
}

fn id_of(bytes: &[u8]) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    bytes.hash(&mut h);
    h.finish()
}

/// Decode image file bytes (png, jpeg, gif, webp, bmp) into the store.
pub fn from_bytes(bytes: &[u8]) -> Option<ImageRef> {
    let id = id_of(bytes);
    if let Some(e) = store().lock().unwrap().get(&id) {
        return Some(ImageRef { id, w: e.w, h: e.h });
    }
    let decoded = image::load_from_memory(bytes).ok()?.to_rgba8();
    let (w, h) = decoded.dimensions();
    store().lock().unwrap().entry(id).or_insert_with(|| Stored {
        w,
        h,
        pixels: Some(egui::ColorImage::from_rgba_unmultiplied([w as usize, h as usize], decoded.as_raw())),
        texture: None,
    });
    Some(ImageRef { id, w, h })
}

/// An image a model sent or received (base64), into the store. Transcripts rebuild often:
/// known base64 skips decoding.
pub fn from_base64(data: &str) -> Option<ImageRef> {
    static SEEN: OnceLock<Mutex<HashMap<u64, ImageRef>>> = OnceLock::new();
    let key = id_of(data.as_bytes());
    let seen = SEEN.get_or_init(Default::default);
    if let Some(r) = seen.lock().unwrap().get(&key) {
        return Some(*r);
    }
    let r = from_bytes(&base64::engine::general_purpose::STANDARD.decode(data.trim()).ok()?)?;
    seen.lock().unwrap().insert(key, r);
    Some(r)
}

/// Encode RGBA pixels (a clipboard image) as PNG.
pub fn png_from_rgba(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    let img = image::RgbaImage::from_raw(w, h, rgba.to_vec())?;
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// The mime type of image file bytes, if they are an image models take.
pub fn mime_of(bytes: &[u8]) -> Option<&'static str> {
    match image::guess_format(bytes).ok()? {
        image::ImageFormat::Png => Some("image/png"),
        image::ImageFormat::Jpeg => Some("image/jpeg"),
        image::ImageFormat::Gif => Some("image/gif"),
        image::ImageFormat::WebP => Some("image/webp"),
        _ => None,
    }
}

/// Read an image file: (store ref, base64 for models). Formats models don't take (bmp) are
/// converted to PNG.
pub fn load_file(path: &std::path::Path) -> Option<(ImageRef, Encoded)> {
    let bytes = std::fs::read(path).ok()?;
    let r = from_bytes(&bytes)?;
    let (mime, bytes) = match mime_of(&bytes) {
        Some(m) => (m, bytes),
        None => {
            let img = image::load_from_memory(&bytes).ok()?.to_rgba8();
            ("image/png", png_from_rgba(img.width(), img.height(), img.as_raw())?)
        }
    };
    Some((r, Encoded { mime: mime.into(), data: base64::engine::general_purpose::STANDARD.encode(bytes).into() }))
}

/// Encode PNG bytes for a model.
pub fn encode_png(bytes: &[u8]) -> Encoded {
    Encoded { mime: "image/png".into(), data: base64::engine::general_purpose::STANDARD.encode(bytes).into() }
}

/// Whether a path looks like an image file.
pub fn is_image_path(path: &std::path::Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp"))
}

/// Paint image `id` filling `rect` with rounded corners (a placeholder until it loads).
pub fn paint(painter: &egui::Painter, id: u64, rect: egui::Rect, radius: f32, placeholder: egui::Color32) {
    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
    match texture(painter.ctx(), id) {
        Some(tex) => {
            painter.add(egui::epaint::RectShape::filled(rect, radius, egui::Color32::WHITE).with_texture(tex, uv));
        }
        None => {
            painter.rect_filled(rect, radius, placeholder);
        }
    }
}

/// The texture for image `id` (uploaded on first use).
pub fn texture(ctx: &egui::Context, id: u64) -> Option<egui::TextureId> {
    let mut s = store().lock().unwrap();
    let e = s.get_mut(&id)?;
    if e.texture.is_none() {
        let px = e.pixels.take()?;
        e.texture = Some(ctx.load_texture(format!("image-{id:x}"), px, egui::TextureOptions::LINEAR));
    }
    e.texture.as_ref().map(|t| t.id())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_fits() {
        let png = png_from_rgba(2, 1, &[255, 0, 0, 255, 0, 0, 255, 255]).unwrap();
        assert_eq!(mime_of(&png), Some("image/png"));
        let r = from_bytes(&png).unwrap();
        assert_eq!((r.w, r.h), (2, 1));
        let e = encode_png(&png);
        assert_eq!(from_base64(&e.data), Some(r));
        let big = ImageRef { id: 0, w: 2000, h: 1000 };
        assert_eq!(big.fit(400.0, 300.0), egui::vec2(400.0, 200.0));
        assert_eq!(r.fit(400.0, 300.0), egui::vec2(2.0, 1.0));
    }
}
