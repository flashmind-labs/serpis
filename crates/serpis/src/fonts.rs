//! Fonts: the monospace and UI fonts and their sizes.
//!
//! `font-mono` and `font-sans` name an installed family ("Intel One Mono") or a font file;
//! unset, `~/.config/serpis/fonts/{mono,sans}.ttf` or a platform default is used.
//! `font-size` and `font-mono-size` are the base sizes that `text-scale-reset` returns to.
//!
//! Variable fonts are set up the way browsers do it: the optical size axis follows the text size
//! (SF's Text cut rather than its Display default), weights come from `font-sans-weight` /
//! `font-mono-weight`, and Apple's tracking table (`trak`) spaces body text as CoreText would
//! (`font-tracking`). Bold and headings use `font-bold-weight`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use fontique::{Collection, CollectionOptions, FontStyle, FontWeight, FontWidth, SourceCache};
use serde_json::{Value, json};

use crate::editor::{BUILTIN, Editor};
use crate::theme::Theme;

const DEFAULT_MONO: &[&str] = &[
    "/System/Library/Fonts/SFNSMono.ttf",
    "/System/Library/Fonts/Menlo.ttc",
    "/usr/share/fonts/truetype/jetbrains-mono/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/TTF/JetBrainsMono-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf",
];
const DEFAULT_SANS: &[&str] = &[
    "/System/Library/Fonts/SFNS.ttf",
    "/System/Library/Fonts/Helvetica.ttc",
    "/usr/share/fonts/truetype/inter/Inter-Regular.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
];

struct System {
    collection: Collection,
    cache: SourceCache,
}

/// The installed fonts, enumerated on first use.
fn system() -> &'static Mutex<System> {
    static SYSTEM: OnceLock<Mutex<System>> = OnceLock::new();
    SYSTEM.get_or_init(|| {
        Mutex::new(System {
            collection: Collection::new(CollectionOptions { shared: false, system_fonts: true }),
            cache: SourceCache::default(),
        })
    })
}

/// Names of the installed font families, sorted.
pub fn families() -> Vec<String> {
    let mut sys = system().lock().unwrap();
    let mut names: Vec<String> = sys.collection.family_names().map(str::to_string).collect();
    names.sort_by_key(|n| n.to_lowercase());
    names.dedup();
    names
}

/// The regular face of an installed family.
fn from_family(name: &str) -> Option<egui::FontData> {
    let mut sys = system().lock().unwrap();
    let System { collection, cache } = &mut *sys;
    let family = collection.family_by_name(name)?;
    let font = family.match_font(FontWidth::NORMAL, FontStyle::Normal, FontWeight::NORMAL, false)?;
    let (blob, _) = font.load(Some(cache))?.into_raw_parts();
    Some(egui::FontData::from_blob(blob, font.index()))
}

fn from_file(path: &Path) -> Option<egui::FontData> {
    std::fs::read(path).ok().map(egui::FontData::from_owned)
}

/// Resolve a `font-*` value: a file path, else a family name.
fn resolve(spec: &str) -> Option<egui::FontData> {
    let path = crate::expand_home(spec);
    if spec.contains('/') || path.extension().is_some_and(|e| matches!(e.to_str(), Some("ttf" | "otf" | "ttc"))) {
        return from_file(&path);
    }
    from_family(spec)
}

/// A table of an sfnt font (`index` picks the face of a collection).
fn sfnt_table<'a>(data: &'a [u8], index: u32, tag: &[u8; 4]) -> Option<&'a [u8]> {
    let u16_at = |o: usize| data.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    let u32_at = |o: usize| data.get(o..o + 4).map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]));
    let base = if data.get(..4) == Some(b"ttcf") { u32_at(12 + 4 * index as usize)? as usize } else { 0 };
    let n = u16_at(base + 4)? as usize;
    (0..n).find_map(|i| {
        let rec = base + 12 + 16 * i;
        (data.get(rec..rec + 4)? == tag).then(|| {
            let (off, len) = (u32_at(rec + 8)? as usize, u32_at(rec + 12)? as usize);
            data.get(off..off + len)
        })?
    })
}

/// Apple's tracking table for normal text: (point size, tracking in em), ascending sizes.
pub fn tracking_table(data: &[u8], index: u32) -> Vec<(f32, f32)> {
    let parse = || -> Option<Vec<(f32, f32)>> {
        let head = sfnt_table(data, index, b"head")?;
        let upm = u16::from_be_bytes([*head.get(18)?, *head.get(19)?]) as f32;
        let t = sfnt_table(data, index, b"trak")?;
        let u16_at = |o: usize| t.get(o..o + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
        let i32_at = |o: usize| t.get(o..o + 4).map(|b| i32::from_be_bytes([b[0], b[1], b[2], b[3]]));
        let horiz = u16_at(8)? as usize;
        if horiz == 0 {
            return None;
        }
        let (n_tracks, n_sizes, size_off) = (u16_at(horiz)? as usize, u16_at(horiz + 2)? as usize, i32_at(horiz + 4)? as usize);
        let sizes: Vec<f32> = (0..n_sizes).map(|j| i32_at(size_off + 4 * j).map(|v| v as f32 / 65536.0)).collect::<Option<_>>()?;
        (0..n_tracks).find_map(|k| {
            let rec = horiz + 8 + 8 * k;
            (i32_at(rec)? == 0).then_some(())?;
            let vals = u16_at(rec + 6)? as usize;
            sizes.iter().enumerate().map(|(j, s)| u16_at(vals + 2 * j).map(|v| (*s, v as i16 as f32 / upm))).collect()
        })
    };
    parse().unwrap_or_default()
}

/// Set a variable font's axes: optical size for `size`, and `weight`.
fn tune(data: egui::FontData, size: f32, weight: Option<f32>, optical: &Value) -> egui::FontData {
    let mut coords = egui::epaint::text::VariationCoords::default();
    for axis in data.variation_axes() {
        let tag = axis.tag.to_be_bytes();
        let value = match &tag {
            b"opsz" => match optical {
                Value::Number(n) => n.as_f64().map(|n| n as f32),
                Value::Bool(false) => None,
                _ => Some(size),
            },
            b"wght" => weight,
            _ => None,
        };
        if let Some(v) = value {
            coords.push(tag, v.clamp(axis.range.min, axis.range.max));
        }
    }
    let tweak = egui::FontTweak { coords, ..Default::default() };
    data.tweak(tweak)
}

/// Install the configured fonts; reports a font that can't be found and falls back.
pub fn apply(ed: &mut Editor) {
    let mut defs = egui::FontDefinitions::default();
    let user = crate::config_dir().join("fonts");
    let (ui_size, mono_size) = (ed.theme.font_size, ed.theme.mono_size);
    let optical = ed.var("font-optical-size").cloned().unwrap_or(json!("auto"));
    let mut tracking = Vec::new();
    for (var, file, defaults, key, family) in [
        ("font-mono", "mono.ttf", DEFAULT_MONO, "serpis-mono", egui::FontFamily::Monospace),
        ("font-sans", "sans.ttf", DEFAULT_SANS, "serpis-sans", egui::FontFamily::Proportional),
    ] {
        let spec = ed.var_str(var).filter(|s| !s.is_empty());
        let chosen = spec.as_deref().and_then(resolve);
        if let (Some(s), None) = (&spec, &chosen) {
            ed.warn(format!("{var}: font not found: {s}"));
        }
        let data = chosen.or_else(|| std::iter::once(user.join(file)).chain(defaults.iter().map(PathBuf::from)).find_map(|p| from_file(&p)));
        if let Some(data) = data {
            let mono = family == egui::FontFamily::Monospace;
            let weight = ed.var(&format!("{var}-weight")).and_then(Value::as_f64).map(|w| w as f32);
            if !mono {
                tracking = tracking_table(data.bytes(), data.index);
            }
            let data = tune(data, if mono { mono_size } else { ui_size }, weight, &optical);
            defs.font_data.insert(key.into(), Arc::new(data));
            defs.families.entry(family).or_default().insert(0, key.into());
        }
    }
    ed.ctx.set_fonts(defs);
    ed.theme.tracking = match ed.var("font-tracking") {
        Some(Value::Number(n)) => vec![(0.0, n.as_f64().unwrap_or(0.0) as f32)],
        Some(Value::Bool(false)) | Some(Value::Null) => Vec::new(),
        _ => tracking,
    };
    ed.theme.bold_weight = ed.var("font-bold-weight").and_then(Value::as_f64).unwrap_or(600.0) as f32;
    ed.theme.font_epoch += 1;
    ed.ctx.request_repaint();
}

/// The base sizes: `font-size` / `font-mono-size`, else the theme defaults.
pub fn base_sizes(ed: &Editor) -> (f32, f32) {
    let d = Theme::default();
    let get = |name: &str, def: f32| ed.var(name).and_then(Value::as_f64).map_or(def, |v| v as f32);
    (get("font-size", d.font_size), get("font-mono-size", d.mono_size))
}

pub fn reset_sizes(ed: &mut Editor) {
    let (ui, mono) = base_sizes(ed);
    ed.theme.font_size = ui;
    ed.theme.mono_size = mono;
    // Optical sizes follow the text size.
    apply(ed);
}

/// Change both sizes by `delta` points.
pub fn scale(ed: &mut Editor, delta: f32) {
    ed.theme.font_size = (ed.theme.font_size + delta).clamp(8.0, 72.0);
    ed.theme.mono_size = (ed.theme.mono_size + delta).clamp(8.0, 72.0);
    apply(ed);
}

pub fn install(ed: &mut Editor) {
    ed.defvar("font-mono", Value::Null, "Monospace font: an installed family name or a font file path.");
    ed.defvar("font-sans", Value::Null, "UI font: an installed family name or a font file path.");
    ed.defvar("font-size", Value::Null, "Base UI font size (text-scale-reset returns to it).");
    ed.defvar("font-mono-size", Value::Null, "Base monospace font size (text-scale-reset returns to it).");
    ed.defvar("font-sans-weight", json!(400.0), "Weight of a variable UI font (wght axis; SF's default instance is 400).");
    ed.defvar("font-mono-weight", json!(400.0), "Weight of a variable monospace font (SF Mono's default instance is a light 295).");
    ed.defvar("font-bold-weight", json!(600.0), "Weight of bold text and headings in variable fonts.");
    ed.defvar("font-optical-size", json!("auto"), "Optical size of variable fonts: \"auto\" (the text size, as browsers do), a number, or false for the font's default.");
    ed.defvar("font-tracking", json!("auto"), "Letter spacing of UI text: \"auto\" (the font's own tracking table, as macOS applies it), a number in em, or false.");
    // Fonts picked with select-font; init.lua may override them.
    for var in ["font-mono", "font-sans"] {
        if let Some(v) = Editor::persisted_state(var) {
            ed.set_var(var, v);
        }
    }
    ed.add_hook("variable-changed", BUILTIN, |ed, p| {
        match p.get("name").and_then(Value::as_str) {
            Some("font-mono" | "font-sans" | "font-sans-weight" | "font-mono-weight" | "font-bold-weight" | "font-optical-size" | "font-tracking") => apply(ed),
            Some("font-size" | "font-mono-size") => reset_sizes(ed),
            _ => {}
        }
        None
    });
    for (cmd, var, label) in [
        ("select-font", "font-mono", "Monospace font: "),
        ("select-ui-font", "font-sans", "UI font: "),
    ] {
        let doc = format!("Choose the {} from the installed fonts (remembered across restarts).", if var == "font-mono" { "monospace font" } else { "UI font" });
        ed.define_command(cmd, &doc, BUILTIN, move |ed, a| {
            if let Some(name) = a.str(0) {
                choose(ed, var, name);
                return Ok(());
            }
            let current = ed.var_str(var).unwrap_or_default();
            let cands = families()
                .into_iter()
                .map(|n| {
                    let detail = if n == current { "current" } else { "" };
                    crate::minibuffer::Candidate::new(n).detail(detail)
                })
                .collect();
            ed.completing_read(label, cands, true, move |ed, c, _| {
                if let Some(c) = c {
                    choose(ed, var, &c.value);
                }
            });
            Ok(())
        });
    }
}

fn choose(ed: &mut Editor, var: &str, name: &str) {
    ed.set_var(var, json!(name));
    ed.persist_state(var, json!(name));
    ed.message(format!("{var}: {name}"));
}
