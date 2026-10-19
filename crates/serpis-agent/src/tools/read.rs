//! `read` tool (pi `read.ts`): text with offset/limit and truncation notices, images as attachments.

use std::io::Cursor;

use base64::Engine;
use futures::FutureExt;
use futures::future::BoxFuture;
use serde_json::{Value, json};
use serpis_ai::UserContent;

use super::path_utils::resolve_read_path;
use super::truncate::{DEFAULT_MAX_BYTES, DEFAULT_MAX_LINES, Limits, format_size, truncate_head};
use super::*;

pub struct ReadTool {
    pub auto_resize_images: bool,
}

impl Default for ReadTool {
    fn default() -> Self {
        ReadTool { auto_resize_images: true }
    }
}

/// Detect a supported image type from magic bytes (pi `mime.ts`).
pub fn detect_image_mime(buf: &[u8]) -> Option<&'static str> {
    if buf.len() >= 3 && buf[0] == 0xff && buf[1] == 0xd8 && buf[2] == 0xff {
        return (buf.get(3) != Some(&0xf7)).then_some("image/jpeg");
    }
    if buf.starts_with(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]) {
        // Animated PNGs (acTL chunk before IDAT) are not supported inline.
        let is_apng = buf
            .windows(4)
            .take(4096)
            .position(|w| w == b"acTL")
            .is_some_and(|a| buf.windows(4).position(|w| w == b"IDAT").is_none_or(|i| a < i));
        return (!is_apng).then_some("image/png");
    }
    if buf.starts_with(b"GIF87a") || buf.starts_with(b"GIF89a") {
        return Some("image/gif");
    }
    if buf.len() >= 12 && &buf[0..4] == b"RIFF" && &buf[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    if buf.starts_with(b"BM") {
        return Some("image/bmp");
    }
    None
}

const MAX_DIM: u32 = 2000;
const MAX_ENCODED_BYTES: usize = 4_718_592; // 4.5MB of base64

struct Processed {
    data: String,
    mime: String,
    hints: Vec<String>,
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// pi `processImage`: normalize to png/jpeg/gif/webp, then fit inside 2000x2000 and 4.5MB,
/// trying PNG and JPEG at falling qualities and shrinking by 25% steps.
fn process_image(bytes: &[u8], mime: &str, auto_resize: bool) -> Result<Processed, String> {
    let mut hints = Vec::new();
    let (bytes, mime): (Vec<u8>, String) = if mime == "image/bmp" {
        let img = image::load_from_memory(bytes)
            .map_err(|_| "[Image omitted: could not be converted to a supported inline image format.]".to_string())?;
        let mut out = Vec::new();
        img.write_to(&mut Cursor::new(&mut out), image::ImageFormat::Png)
            .map_err(|_| "[Image omitted: could not be converted to a supported inline image format.]".to_string())?;
        hints.push("[Image converted from image/bmp to image/png.]".to_string());
        (out, "image/png".into())
    } else {
        (bytes.to_vec(), mime.to_string())
    };
    if !auto_resize {
        return Ok(Processed { data: b64(&bytes), mime, hints });
    }
    let input_b64_size = bytes.len().div_ceil(3) * 4;
    let Ok(img) = image::load_from_memory(&bytes) else {
        return Ok(Processed { data: b64(&bytes), mime, hints });
    };
    let (ow, oh) = (img.width(), img.height());
    if ow <= MAX_DIM && oh <= MAX_DIM && input_b64_size < MAX_ENCODED_BYTES {
        return Ok(Processed { data: b64(&bytes), mime, hints });
    }
    let (mut w, mut h) = (ow, oh);
    if w > MAX_DIM {
        h = ((h as f64 * MAX_DIM as f64) / w as f64).round() as u32;
        w = MAX_DIM;
    }
    if h > MAX_DIM {
        w = ((w as f64 * MAX_DIM as f64) / h as f64).round() as u32;
        h = MAX_DIM;
    }
    loop {
        let resized = img.resize_exact(w.max(1), h.max(1), image::imageops::FilterType::Lanczos3);
        let mut candidates: Vec<(Vec<u8>, &str)> = Vec::new();
        let mut png = Vec::new();
        if resized.write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png).is_ok() {
            candidates.push((png, "image/png"));
        }
        let rgb = resized.to_rgb8();
        for q in [80u8, 85, 70, 55, 40] {
            let mut jpg = Vec::new();
            let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, q);
            if rgb.write_with_encoder(enc).is_ok() {
                candidates.push((jpg, "image/jpeg"));
            }
        }
        for (data, m) in candidates {
            let encoded = b64(&data);
            if encoded.len() < MAX_ENCODED_BYTES {
                hints.push(format!(
                    "[Image: original {ow}x{oh}, displayed at {w}x{h}. Multiply coordinates by {:.2} to map to original image.]",
                    ow as f64 / w as f64
                ));
                return Ok(Processed { data: encoded, mime: m.into(), hints });
            }
        }
        if w == 1 && h == 1 {
            break;
        }
        let (nw, nh) = (((w as f64) * 0.75).floor().max(1.0) as u32, ((h as f64) * 0.75).floor().max(1.0) as u32);
        if (nw, nh) == (w, h) {
            break;
        }
        (w, h) = (nw, nh);
    }
    Err("[Image omitted: could not be resized below the inline image size limit.]".into())
}

impl AgentTool for ReadTool {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> String {
        format!(
            "Read the contents of a file. Supports text files and images (jpg, png, gif, webp, bmp). Images are sent as attachments. For text files, output is truncated to {DEFAULT_MAX_LINES} lines or {}KB (whichever is hit first). Use offset/limit for large files. When you need the full file, continue with offset until complete.",
            DEFAULT_MAX_BYTES / 1024
        )
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Path to the file to read (relative or absolute)"},
                "offset": {"type": "number", "description": "Line number to start reading from (1-indexed)"},
                "limit": {"type": "number", "description": "Maximum number of lines to read"}
            },
            "required": ["path"]
        })
    }
    fn prompt_snippet(&self) -> Option<String> {
        Some("Read file contents".into())
    }
    fn prompt_guidelines(&self) -> Vec<String> {
        vec!["Use read to examine files instead of cat or sed.".into()]
    }

    fn execute(&self, _id: String, args: Value, ctx: ToolContext) -> BoxFuture<'static, anyhow::Result<ToolResult>> {
        let auto_resize = self.auto_resize_images;
        async move {
            ctx.check_cancelled()?;
            let path = arg_str(&args, "path").unwrap_or("").to_string();
            let offset = arg_u64(&args, "offset");
            let limit = arg_u64(&args, "limit");
            let abs = resolve_read_path(&path, &ctx.cwd);
            let buffer = tokio::fs::read(&abs).await.map_err(|e| anyhow::anyhow!("{e}: {}", abs.display()))?;
            ctx.check_cancelled()?;
            let non_vision = ctx
                .model
                .as_ref()
                .is_some_and(|m| !m.supports_images())
                .then_some("[Current model does not support images. The image will be omitted from this request.]");

            if let Some(mime) = detect_image_mime(&buffer) {
                let processed = tokio::task::spawn_blocking(move || process_image(&buffer, mime, auto_resize)).await?;
                return Ok(match processed {
                    Err(message) => {
                        let mut note = format!("Read image file [{mime}]\n{message}");
                        if let Some(n) = non_vision {
                            note.push('\n');
                            note.push_str(n);
                        }
                        ToolResult::text(note)
                    }
                    Ok(p) => {
                        let mut note = format!("Read image file [{}]", p.mime);
                        if !p.hints.is_empty() {
                            note.push('\n');
                            note.push_str(&p.hints.join("\n"));
                        }
                        if let Some(n) = non_vision {
                            note.push('\n');
                            note.push_str(n);
                        }
                        ToolResult {
                            content: vec![
                                UserContent::Text { text: note },
                                UserContent::Image {
                                    data: p.data,
                                    mime_type: p.mime,
                                },
                            ],
                            details: Value::Null,
                            is_error: false,
                        }
                    }
                });
            }

            let text = String::from_utf8_lossy(&buffer);
            let all: Vec<&str> = text.split('\n').collect();
            let total = all.len();
            let start = offset.map(|o| o.saturating_sub(1) as usize).unwrap_or(0);
            let start_display = start + 1;
            if start >= total {
                anyhow::bail!("Offset {} is beyond end of file ({} lines total)", offset.unwrap_or(0), total);
            }
            let (selected, user_limited) = match limit {
                Some(l) => {
                    let end = (start + l as usize).min(total);
                    (all[start..end].join("\n"), Some(end - start))
                }
                None => (all[start..].join("\n"), None),
            };
            let t = truncate_head(&selected, Limits::default());
            let mut details = Value::Null;
            let out = if t.first_line_exceeds_limit {
                details = json!({"truncation": t});
                format!(
                    "[Line {start_display} is {}, exceeds {} limit. Use bash: sed -n '{start_display}p' {path} | head -c {DEFAULT_MAX_BYTES}]",
                    format_size(all[start].len()),
                    format_size(DEFAULT_MAX_BYTES)
                )
            } else if t.truncated {
                let end_display = start_display + t.output_lines - 1;
                let next = end_display + 1;
                let mut s = t.content.clone();
                if t.truncated_by == Some(super::truncate::TruncatedBy::Lines) {
                    s.push_str(&format!("\n\n[Showing lines {start_display}-{end_display} of {total}. Use offset={next} to continue.]"));
                } else {
                    s.push_str(&format!(
                        "\n\n[Showing lines {start_display}-{end_display} of {total} ({} limit). Use offset={next} to continue.]",
                        format_size(DEFAULT_MAX_BYTES)
                    ));
                }
                details = json!({"truncation": t});
                s
            } else if let Some(n) = user_limited.filter(|n| start + n < total) {
                let remaining = total - (start + n);
                format!("{}\n\n[{remaining} more lines in file. Use offset={} to continue.]", t.content, start + n + 1)
            } else {
                t.content
            };
            Ok(ToolResult::text(out).with_details(details))
        }
        .boxed()
    }
}
