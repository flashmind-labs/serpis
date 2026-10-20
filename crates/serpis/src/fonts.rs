//! Fonts: the monospace and UI fonts and their sizes.
//!
//! `font-mono` and `font-sans` name an installed family ("Intel One Mono") or a font file;
//! unset, `~/.config/serpis/fonts/{mono,sans}.ttf` or a platform default is used.
//! `font-size` and `font-mono-size` are the base sizes that `text-scale-reset` returns to.

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

/// Install the configured fonts; reports a font that can't be found and falls back.
pub fn apply(ed: &mut Editor) {
    let mut defs = egui::FontDefinitions::default();
    let user = crate::config_dir().join("fonts");
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
            defs.font_data.insert(key.into(), Arc::new(data));
            defs.families.entry(family).or_default().insert(0, key.into());
        }
    }
    ed.ctx.set_fonts(defs);
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
    ed.ctx.request_repaint();
}

/// Change both sizes by `delta` points.
pub fn scale(ed: &mut Editor, delta: f32) {
    ed.theme.font_size = (ed.theme.font_size + delta).clamp(8.0, 72.0);
    ed.theme.mono_size = (ed.theme.mono_size + delta).clamp(8.0, 72.0);
    ed.ctx.request_repaint();
}

pub fn install(ed: &mut Editor) {
    ed.defvar("font-mono", Value::Null, "Monospace font: an installed family name or a font file path.");
    ed.defvar("font-sans", Value::Null, "UI font: an installed family name or a font file path.");
    ed.defvar("font-size", Value::Null, "Base UI font size (text-scale-reset returns to it).");
    ed.defvar("font-mono-size", Value::Null, "Base monospace font size (text-scale-reset returns to it).");
    // Fonts picked with select-font; init.lua may override them.
    for var in ["font-mono", "font-sans"] {
        if let Some(v) = Editor::persisted_state(var) {
            ed.set_var(var, v);
        }
    }
    ed.add_hook("variable-changed", BUILTIN, |ed, p| {
        match p.get("name").and_then(Value::as_str) {
            Some("font-mono" | "font-sans") => apply(ed),
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
