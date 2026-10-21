//! System clipboard access (reads; writes go through egui so they work everywhere).

pub fn get() -> Option<String> {
    arboard::Clipboard::new().ok()?.get_text().ok().filter(|t| !t.is_empty())
}

/// Files copied in a file manager (Finder), if that is what the clipboard holds.
pub fn files() -> Option<Vec<std::path::PathBuf>> {
    arboard::Clipboard::new().ok()?.get().file_list().ok().filter(|f| !f.is_empty())
}

/// An image on the clipboard (a screenshot, an image copied from a browser) as PNG bytes.
pub fn image_png() -> Option<Vec<u8>> {
    let img = arboard::Clipboard::new().ok()?.get_image().ok()?;
    crate::images::png_from_rgba(img.width as u32, img.height as u32, &img.bytes)
}

pub fn set(text: &str) {
    if let Ok(mut c) = arboard::Clipboard::new() {
        let _ = c.set_text(text.to_string());
    }
}
