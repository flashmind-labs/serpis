//! Theme registry: bundled palettes (Modus, GitHub), user-defined palettes from Lua/Rust,
//! and the mapping from a palette onto serpis UI roles.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use egui::Color32;
use serde::Deserialize;

use crate::editor::Editor;
use crate::theme::Theme;

/// A palette. Only `bg`, `fg` and the accent/status colors are required; the rest is
/// derived when absent, so user palettes can be small.
#[derive(Debug, Clone, Deserialize)]
pub struct ThemeSpec {
    #[serde(default)]
    pub light: bool,
    pub bg: String,
    pub fg: String,
    /// Darker/dimmer background: frame around windows, tool cards.
    pub bg_dim: Option<String>,
    /// Raised surfaces: window headers, prompt box, popups.
    #[serde(alias = "bg_alt")]
    pub bg_inactive: Option<String>,
    pub fg_dim: Option<String>,
    pub fg_faint: Option<String>,
    pub border: Option<String>,
    pub accent: Option<String>,
    pub blue: Option<String>,
    pub orange: Option<String>,
    pub selection: Option<String>,
    pub cursor: Option<String>,
    pub red: Option<String>,
    pub green: Option<String>,
    pub yellow: Option<String>,
    pub purple: Option<String>,
    pub cyan: Option<String>,
    pub diff_add: Option<String>,
    pub diff_del: Option<String>,
    pub diff_add_fg: Option<String>,
    pub diff_del_fg: Option<String>,
    pub accent_subtle: Option<String>,
    pub ansi: Option<Vec<String>>,
}

pub fn parse_hex(s: &str) -> Option<Color32> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    Some(Color32::from_rgb((v >> 16) as u8, (v >> 8) as u8, v as u8))
}

/// Linear blend from `a` to `b`.
/// Perceived lightness (OKLab L, 0..1).
fn ok_l(c: Color32) -> f32 {
    let lin = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    let (r, g, b) = (lin(c.r()), lin(c.g()), lin(c.b()));
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_99 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s
}

/// `c`, moved toward `body` until their lightness differs by at most `max` (hue kept).
fn within(body: Color32, c: Color32, max: f32) -> Color32 {
    if (ok_l(c) - ok_l(body)).abs() <= max {
        return c;
    }
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..20 {
        let t = (lo + hi) / 2.0;
        if (ok_l(mix(body, c, t)) - ok_l(body)).abs() > max { hi = t } else { lo = t }
    }
    mix(body, c, lo)
}

fn mix(a: Color32, b: Color32, t: f32) -> Color32 {
    let l = |x: u8, y: u8| (x as f32 + (y as f32 - x as f32) * t).round() as u8;
    Color32::from_rgb(l(a.r(), b.r()), l(a.g(), b.g()), l(a.b(), b.b()))
}

impl ThemeSpec {
    /// Map the palette onto UI roles. `base` supplies fonts and geometry.
    pub fn to_theme(&self, name: &str, base: &Theme) -> anyhow::Result<Theme> {
        let req = |s: &str, what: &str| parse_hex(s).ok_or_else(|| anyhow::anyhow!("theme {name}: invalid {what} color {s:?}"));
        let opt = |s: &Option<String>| s.as_deref().and_then(parse_hex);
        let body = req(&self.bg, "bg")?;
        let fg = req(&self.fg, "fg")?;
        let dark = !self.light;
        // Frame and tool cards sit slightly away from the body color. Palettes made for other
        // roles (Modus' bg-dim is a distinct region in Emacs) can be far from it: keep surfaces
        // within a light tint of the body, so the frame doesn't read as grey next to it.
        let near = |c: Color32, f: f32| within(body, c, (ok_l(mix(body, fg, f)) - ok_l(body)).abs());
        let dim = near(opt(&self.bg_dim).unwrap_or_else(|| mix(body, if dark { Color32::BLACK } else { fg }, 0.06)), 0.06);
        let elevated = near(opt(&self.bg_inactive).unwrap_or_else(|| mix(body, fg, 0.08)), 0.11);
        let fg_dim = opt(&self.fg_dim).unwrap_or_else(|| mix(fg, body, 0.35));
        let fg_faint = opt(&self.fg_faint).unwrap_or_else(|| mix(fg_dim, body, 0.35));
        let blue = opt(&self.blue).or(opt(&self.accent)).unwrap_or(fg);
        // The theme's own accent when it has one (Modus' tinted variants are red or purple).
        let accent = opt(&self.accent).unwrap_or(blue);
        let red = opt(&self.red).unwrap_or(base.red);
        let green = opt(&self.green).unwrap_or(base.green);
        let yellow = opt(&self.yellow).unwrap_or(base.yellow);
        let purple = opt(&self.purple).unwrap_or(base.purple);
        let cyan = opt(&self.cyan).unwrap_or(base.cyan);
        let ansi = match &self.ansi {
            Some(v) if v.len() == 16 => {
                let mut a = [Color32::BLACK; 16];
                for (i, h) in v.iter().enumerate() {
                    a[i] = parse_hex(h).ok_or_else(|| anyhow::anyhow!("theme {name}: invalid ansi color {h:?}"))?;
                }
                a
            }
            _ => {
                let normal = [body, red, green, yellow, blue, purple, cyan, fg_dim];
                let mut a = [Color32::BLACK; 16];
                for i in 0..8 {
                    a[i] = normal[i];
                    a[i + 8] = mix(normal[i], fg, 0.25);
                }
                a
            }
        };
        Ok(Theme {
            name: name.to_string(),
            dark,
            bg: dim,
            bg_alt: body,
            bg_elevated: elevated,
            fg,
            fg_dim,
            fg_faint,
            accent,
            accent2: opt(&self.orange).unwrap_or(yellow),
            border: opt(&self.border).unwrap_or_else(|| mix(body, fg, 0.18)),
            border_focused: accent,
            selection: opt(&self.selection).unwrap_or_else(|| mix(body, accent, 0.3)),
            cursor: opt(&self.cursor).unwrap_or(fg),
            green,
            red,
            yellow,
            blue,
            purple,
            cyan,
            diff_add_bg: opt(&self.diff_add).unwrap_or_else(|| mix(body, green, 0.15)),
            diff_del_bg: opt(&self.diff_del).unwrap_or_else(|| mix(body, red, 0.15)),
            diff_add_fg: opt(&self.diff_add_fg).unwrap_or(green),
            diff_del_fg: opt(&self.diff_del_fg).unwrap_or(red),
            ansi,
            user_bg: opt(&self.accent_subtle).unwrap_or_else(|| mix(body, accent, 0.12)),
            tool_bg: dim,
            font_size: base.font_size,
            mono_size: base.mono_size,
            radius: base.radius,
            tracking: base.tracking.clone(),
            bold_weight: base.bold_weight,
            font_epoch: base.font_epoch,
        })
    }
}

/// Bundled palettes, by name.
pub fn bundled() -> &'static BTreeMap<String, ThemeSpec> {
    static B: OnceLock<BTreeMap<String, ThemeSpec>> = OnceLock::new();
    B.get_or_init(|| {
        let mut m = BTreeMap::new();
        for src in [include_str!("../themes/modus.json"), include_str!("../themes/github.json"), include_str!("../themes/rayfish.json")] {
            let parsed: BTreeMap<String, ThemeSpec> = serde_json::from_str(src).expect("bundled theme JSON");
            m.extend(parsed);
        }
        m
    })
}

/// Name of the light/dark counterpart (modus-operandi ↔ modus-vivendi, github-light ↔ github-dark).
pub fn counterpart(name: &str) -> String {
    let swap = |s: &str, a: &str, b: &str| -> Option<String> {
        if s.contains(a) {
            Some(s.replacen(a, b, 1))
        } else if s.contains(b) {
            Some(s.replacen(b, a, 1))
        } else {
            None
        }
    };
    swap(name, "operandi", "vivendi")
        .or_else(|| swap(name, "light", "dark"))
        .unwrap_or_else(|| "serpis-light".into())
}

/// Accent choices besides the theme's own: its palette colors, so they suit any theme.
pub const ACCENT_ROLES: [&str; 7] = ["blue", "purple", "cyan", "green", "yellow", "orange", "red"];

/// The color `spec` names in theme `t`: a palette role ("purple") or "#rrggbb"; None for
/// "theme" (its own accent) and anything unknown.
pub fn accent_color(t: &Theme, spec: &str) -> Option<Color32> {
    match spec.trim() {
        "blue" => Some(t.blue),
        "purple" => Some(t.purple),
        "cyan" => Some(t.cyan),
        "green" => Some(t.green),
        "yellow" => Some(t.yellow),
        "orange" => Some(t.accent2),
        "red" => Some(t.red),
        s if s.starts_with('#') => parse_hex(s),
        _ => None,
    }
}

/// Use `c` as the accent, with what is derived from it (focus borders, your message bubbles).
pub fn set_accent(t: &mut Theme, c: Color32) {
    t.accent = c;
    t.border_focused = c;
    t.user_bg = mix(t.bg_alt, c, if t.dark { 0.16 } else { 0.12 });
}

pub fn hex(c: Color32) -> String {
    format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b())
}

fn state_path() -> std::path::PathBuf {
    crate::config_dir().join("state.json")
}

impl Editor {
    /// All theme names: built-ins, bundled palettes, then user-defined ones.
    pub fn theme_names(&self) -> Vec<String> {
        let mut v = vec!["serpis-dark".to_string(), "serpis-light".to_string()];
        v.extend(bundled().keys().cloned());
        for (n, _) in &self.custom_themes {
            if !v.contains(n) {
                v.push(n.clone());
            }
        }
        v
    }

    /// Register a palette under `name` (Lua `serpis.theme.define`).
    pub fn define_theme(&mut self, name: &str, spec: ThemeSpec) {
        self.custom_themes.retain(|(n, _)| n != name);
        self.custom_themes.push((name.to_string(), spec));
    }

    fn resolve_theme(&self, name: &str) -> anyhow::Result<Theme> {
        let base = &self.theme;
        let mut t = match name {
            "serpis-dark" => Theme::dark(),
            "serpis-light" => Theme::light(),
            _ => {
                let spec = self
                    .custom_themes
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, s)| s)
                    .or_else(|| bundled().get(name))
                    .ok_or_else(|| anyhow::anyhow!("Unknown theme: {name}"))?;
                spec.to_theme(name, base)?
            }
        };
        t.font_size = base.font_size;
        t.mono_size = base.mono_size;
        t.tracking = base.tracking.clone();
        t.bold_weight = base.bold_weight;
        t.font_epoch = base.font_epoch;
        if let Some(spec) = self.var_str("theme-accent") {
            if let Some(c) = accent_color(&t, &spec) {
                set_accent(&mut t, c);
            } else if spec != "theme" && !spec.is_empty() {
                anyhow::bail!("theme-accent: not a palette color or #rrggbb: {spec}");
            }
        }
        Ok(t)
    }

    /// Switch theme (also used for live preview). Terminals pick up the new ANSI colors.
    pub fn load_theme(&mut self, name: &str) -> anyhow::Result<()> {
        let t = self.resolve_theme(name)?;
        self.theme = t;
        self.theme.apply(&self.ctx);
        self.theme_generation += 1;
        self.ctx.request_repaint();
        self.run_hook("theme-changed", &serde_json::json!({"name": name}));
        Ok(())
    }

    /// Remember `name` as the theme to start with.
    pub fn persist_theme(&self, name: &str) {
        self.persist_state("theme", serde_json::Value::String(name.to_string()));
    }

    /// Theme remembered by `persist_theme`, if any.
    pub fn persisted_theme() -> Option<String> {
        Self::persisted_state("theme")?.as_str().map(str::to_string)
    }

    /// Remember an interactive choice across restarts (`state.json`).
    pub fn persist_state(&self, key: &str, value: serde_json::Value) {
        let path = state_path();
        let mut state: serde_json::Value = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str(&s).ok()).unwrap_or_else(|| serde_json::json!({}));
        state[key] = value;
        let _ = std::fs::create_dir_all(crate::config_dir());
        let _ = std::fs::write(path, serde_json::to_string_pretty(&state).unwrap_or_default());
    }

    pub fn persisted_state(key: &str) -> Option<serde_json::Value> {
        let state: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(state_path()).ok()?).ok()?;
        state.get(key).cloned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_themes_map() {
        let base = Theme::dark();
        assert!(bundled().len() >= 19);
        for (name, spec) in bundled() {
            let t = spec.to_theme(name, &base).unwrap();
            assert_eq!(t.dark, !spec.light, "{name}");
        }
        // Themes keep their own accents (Modus Operandi Tinted's is red, not the palette's blue).
        let tinted = bundled()["modus-operandi-tinted"].to_theme("modus-operandi-tinted", &base).unwrap();
        assert_eq!(hex(tinted.accent), "#a60000");
        assert_eq!(accent_color(&tinted, "purple"), Some(tinted.purple));
        assert_eq!(accent_color(&tinted, "#ff8800"), Some(Color32::from_rgb(0xff, 0x88, 0x00)));
        assert_eq!(accent_color(&tinted, "theme"), None);
        let v = bundled()["modus-vivendi"].to_theme("modus-vivendi", &base).unwrap();
        // Its frame and cards stay near the black body instead of Modus' #1e1e1e / #303030.
        assert!(ok_l(v.bg) < ok_l(Color32::from_rgb(0x14, 0x14, 0x14)), "{}", hex(v.bg));
        assert!(ok_l(v.bg_elevated) < ok_l(Color32::from_rgb(0x24, 0x24, 0x24)), "{}", hex(v.bg_elevated));
        // GitHub's own (subtle) surfaces are kept as they are.
        let g = bundled()["github-dark-default"].to_theme("github-dark-default", &base).unwrap();
        assert_eq!(hex(g.bg), "#04070d");
        assert_eq!(v.bg_alt, Color32::BLACK);
        assert_eq!(counterpart("modus-vivendi-tinted"), "modus-operandi-tinted");
        assert_eq!(counterpart("github-light-high-contrast"), "github-dark-high-contrast");
    }
}
