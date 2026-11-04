//! Colors, fonts and faces. Everything is a field so Lua/Rust config can restyle it.

use egui::{Color32, FontFamily, FontId};

/// Semantic text faces, resolved to colors and fonts by the theme (like Emacs faces).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Face {
    #[default]
    Default,
    Code,
    /// Bold and dim monospace (table headers and rules).
    CodeBold,
    CodeDim,
    Dim,
    Bold,
    /// Emphasis in reading text: *italic*, ***bold italic***, ~~struck through~~.
    Italic,
    BoldItalic,
    Strike,
    Accent,
    UserLabel,
    UserText,
    AssistantText,
    Thinking,
    ToolHeader,
    ToolArgs,
    ToolOutput,
    ToolError,
    ToolOk,
    DiffAdd,
    DiffDel,
    DiffContext,
    DiffHunk,
    Error,
    Warning,
    Heading,
    InlineCode,
    Link,
    PromptMarker,
    /// Small secondary text: usage, timestamps, notices.
    Meta,
    /// Meta text that reports something good (a cache hit) or worth a look (a cache miss).
    MetaGood,
    MetaWarn,
    /// Small medium-weight labels (who is speaking, card titles).
    Label,
    /// Text that is there to be copied, not read.
    Unseen,
    /// Syntax highlighting (monospace, colored from the theme's palette).
    SynKeyword,
    SynString,
    SynComment,
    SynFunction,
    SynType,
    SynConstant,
    SynAttribute,
}

#[derive(Debug, Clone)]
pub struct Theme {
    pub name: String,
    pub dark: bool,
    pub bg: Color32,
    pub bg_alt: Color32,
    pub bg_elevated: Color32,
    pub fg: Color32,
    pub fg_dim: Color32,
    pub fg_faint: Color32,
    pub accent: Color32,
    pub accent2: Color32,
    pub border: Color32,
    pub border_focused: Color32,
    pub selection: Color32,
    pub cursor: Color32,
    pub green: Color32,
    pub red: Color32,
    pub yellow: Color32,
    pub blue: Color32,
    pub purple: Color32,
    pub cyan: Color32,
    pub diff_add_bg: Color32,
    pub diff_del_bg: Color32,
    pub diff_add_fg: Color32,
    pub diff_del_fg: Color32,
    /// Terminal ANSI colors 0-15.
    pub ansi: [Color32; 16],
    pub user_bg: Color32,
    pub tool_bg: Color32,
    pub font_size: f32,
    pub mono_size: f32,
    pub radius: u8,
    /// Letter spacing of proportional text by size: (points, em), ascending (`font-tracking`).
    pub tracking: Vec<(f32, f32)>,
    /// `wght` of bold faces in variable fonts.
    pub bold_weight: f32,
    /// Bumped whenever the font files or their settings change.
    pub font_epoch: u64,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    pub fn dark() -> Self {
        Theme {
            name: "serpis-dark".into(),
            dark: true,
            bg: Color32::from_rgb(0x16, 0x17, 0x1d),
            bg_alt: Color32::from_rgb(0x1c, 0x1d, 0x25),
            bg_elevated: Color32::from_rgb(0x24, 0x26, 0x30),
            fg: Color32::from_rgb(0xd9, 0xdc, 0xe6),
            fg_dim: Color32::from_rgb(0x9a, 0x9f, 0xb2),
            fg_faint: Color32::from_rgb(0x5f, 0x64, 0x78),
            accent: Color32::from_rgb(0x8b, 0x9c, 0xff),
            accent2: Color32::from_rgb(0xf0, 0x9e, 0x6f),
            border: Color32::from_rgb(0x2c, 0x2e, 0x3a),
            border_focused: Color32::from_rgb(0x5b, 0x67, 0xc9),
            selection: Color32::from_rgba_unmultiplied(0x8b, 0x9c, 0xff, 0x48),
            cursor: Color32::from_rgb(0xff, 0xcc, 0x66),
            green: Color32::from_rgb(0x9e, 0xce, 0x6a),
            red: Color32::from_rgb(0xf7, 0x76, 0x8e),
            yellow: Color32::from_rgb(0xe0, 0xaf, 0x68),
            blue: Color32::from_rgb(0x7a, 0xa2, 0xf7),
            purple: Color32::from_rgb(0xbb, 0x9a, 0xf7),
            cyan: Color32::from_rgb(0x7d, 0xcf, 0xff),
            diff_add_bg: Color32::from_rgba_unmultiplied(0x9e, 0xce, 0x6a, 0x22),
            diff_del_bg: Color32::from_rgba_unmultiplied(0xf7, 0x76, 0x8e, 0x22),
            diff_add_fg: Color32::from_rgb(0x9e, 0xce, 0x6a),
            diff_del_fg: Color32::from_rgb(0xf7, 0x76, 0x8e),
            ansi: [Color32::from_rgb(0x15, 0x16, 0x1e), Color32::from_rgb(0xf7, 0x76, 0x8e), Color32::from_rgb(0x9e, 0xce, 0x6a), Color32::from_rgb(0xe0, 0xaf, 0x68), Color32::from_rgb(0x7a, 0xa2, 0xf7), Color32::from_rgb(0xbb, 0x9a, 0xf7), Color32::from_rgb(0x7d, 0xcf, 0xff), Color32::from_rgb(0xa9, 0xb1, 0xd6), Color32::from_rgb(0x41, 0x48, 0x68), Color32::from_rgb(0xff, 0x89, 0x9d), Color32::from_rgb(0x9f, 0xe0, 0x44), Color32::from_rgb(0xfa, 0xba, 0x4a), Color32::from_rgb(0x8d, 0xb0, 0xff), Color32::from_rgb(0xc7, 0xa9, 0xff), Color32::from_rgb(0xa4, 0xda, 0xff), Color32::from_rgb(0xc0, 0xca, 0xf5)],
            user_bg: Color32::from_rgb(0x22, 0x24, 0x33),
            tool_bg: Color32::from_rgb(0x1b, 0x1c, 0x24),
            font_size: 14.5,
            mono_size: 13.5,
            radius: 6,
            tracking: Vec::new(),
            bold_weight: 600.0,
            font_epoch: 0,
        }
    }

    pub fn light() -> Self {
        Theme {
            name: "serpis-light".into(),
            dark: false,
            bg: Color32::from_rgb(0xfa, 0xfa, 0xfb),
            bg_alt: Color32::from_rgb(0xf1, 0xf2, 0xf5),
            bg_elevated: Color32::WHITE,
            fg: Color32::from_rgb(0x24, 0x27, 0x33),
            fg_dim: Color32::from_rgb(0x5c, 0x61, 0x73),
            fg_faint: Color32::from_rgb(0xa0, 0xa4, 0xb2),
            accent: Color32::from_rgb(0x4c, 0x5f, 0xd5),
            accent2: Color32::from_rgb(0xc8, 0x6a, 0x2c),
            border: Color32::from_rgb(0xe1, 0xe3, 0xea),
            border_focused: Color32::from_rgb(0x4c, 0x5f, 0xd5),
            selection: Color32::from_rgba_unmultiplied(0x4c, 0x5f, 0xd5, 0x38),
            cursor: Color32::from_rgb(0x4c, 0x5f, 0xd5),
            green: Color32::from_rgb(0x38, 0x7a, 0x26),
            red: Color32::from_rgb(0xc4, 0x32, 0x4e),
            yellow: Color32::from_rgb(0x9a, 0x6a, 0x00),
            blue: Color32::from_rgb(0x2f, 0x5f, 0xc4),
            purple: Color32::from_rgb(0x7a, 0x4c, 0xc4),
            cyan: Color32::from_rgb(0x0b, 0x7a, 0x9a),
            diff_add_bg: Color32::from_rgba_unmultiplied(0x38, 0x9a, 0x26, 0x24),
            diff_del_bg: Color32::from_rgba_unmultiplied(0xc4, 0x32, 0x4e, 0x22),
            diff_add_fg: Color32::from_rgb(0x38, 0x7a, 0x26),
            diff_del_fg: Color32::from_rgb(0xc4, 0x32, 0x4e),
            ansi: [Color32::from_rgb(0xe9, 0xe9, 0xed), Color32::from_rgb(0xc4, 0x32, 0x4e), Color32::from_rgb(0x38, 0x7a, 0x26), Color32::from_rgb(0x9a, 0x6a, 0x00), Color32::from_rgb(0x2f, 0x5f, 0xc4), Color32::from_rgb(0x7a, 0x4c, 0xc4), Color32::from_rgb(0x0b, 0x7a, 0x9a), Color32::from_rgb(0x5c, 0x61, 0x73), Color32::from_rgb(0xa0, 0xa4, 0xb2), Color32::from_rgb(0xd6, 0x45, 0x5f), Color32::from_rgb(0x4a, 0x8f, 0x36), Color32::from_rgb(0xb0, 0x7d, 0x0a), Color32::from_rgb(0x41, 0x70, 0xd6), Color32::from_rgb(0x8c, 0x5e, 0xd6), Color32::from_rgb(0x1a, 0x8c, 0xad), Color32::from_rgb(0x24, 0x27, 0x33)],
            user_bg: Color32::from_rgb(0xec, 0xee, 0xf8),
            tool_bg: Color32::from_rgb(0xf4, 0xf5, 0xf8),
            font_size: 14.5,
            mono_size: 13.5,
            radius: 6,
            tracking: Vec::new(),
            bold_weight: 600.0,
            font_epoch: 0,
        }
    }

    /// How much bigger than the default size the UI text is; fixed chrome (headers, rows,
    /// tabs) scales by it.
    pub fn ui_scale(&self) -> f32 {
        (self.font_size / Theme::dark().font_size).max(0.5)
    }

    /// Changes whenever laid-out text would change size (cached heights key on it).
    pub fn layout_key(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (self.font_size.to_bits(), self.mono_size.to_bits(), self.bold_weight.to_bits(), self.font_epoch).hash(&mut h);
        for (s, v) in &self.tracking {
            (s.to_bits(), v.to_bits()).hash(&mut h);
        }
        h.finish()
    }

    /// Letter spacing in points for proportional text of `size`, interpolated from `tracking`.
    pub fn letter_spacing(&self, size: f32) -> f32 {
        let t = &self.tracking;
        let em = match t.iter().position(|(s, _)| *s >= size) {
            _ if t.is_empty() => return 0.0,
            Some(0) => t[0].1,
            None => t[t.len() - 1].1,
            Some(i) => {
                let ((s0, v0), (s1, v1)) = (t[i - 1], t[i]);
                v0 + (v1 - v0) * (size - s0) / (s1 - s0).max(f32::EPSILON)
            }
        };
        em * size
    }

    /// The full text format of a face: font, colors, weight and spacing.
    pub fn format(&self, face: Face) -> egui::TextFormat {
        let (font_id, color, background) = self.face(face);
        let weight = match face {
            Face::Bold | Face::BoldItalic | Face::Heading | Face::CodeBold => Some(self.bold_weight),
            Face::UserLabel | Face::ToolHeader | Face::Label => Some((self.bold_weight - 100.0).max(400.0)),
            _ => None,
        };
        let mut coords = egui::epaint::text::VariationCoords::default();
        if let Some(w) = weight {
            coords.push(b"wght", w);
        }
        let proportional = font_id.family == FontFamily::Proportional;
        // Reading text gets air between lines (CSS line-height), code a little less.
        let line_height = match face {
            Face::AssistantText | Face::UserText | Face::Thinking | Face::Error | Face::Warning => Some((font_id.size * 1.55).round()),
            Face::Code | Face::CodeBold | Face::CodeDim | Face::ToolOutput | Face::ToolError | Face::ToolArgs | Face::DiffAdd | Face::DiffDel | Face::DiffContext | Face::DiffHunk => Some((font_id.size * 1.5).round()),
            Face::Heading => Some((font_id.size * 1.35).round()),
            _ => None,
        };
        egui::TextFormat {
            line_height,
            expand_bg: if face == Face::InlineCode { 0.0 } else { 1.0 },
            extra_letter_spacing: if proportional { self.letter_spacing(font_id.size) } else { 0.0 },
            font_id,
            color,
            background,
            coords,
            italics: matches!(face, Face::Thinking | Face::SynComment | Face::Italic | Face::BoldItalic),
            strikethrough: if face == Face::Strike { egui::Stroke::new(1.0, self.fg_dim) } else { egui::Stroke::NONE },
            ..Default::default()
        }
    }

    /// Background of inline code chips.
    pub fn chip(&self) -> Color32 {
        let [r, g, b, _] = self.fg.to_array();
        Color32::from_rgba_unmultiplied(r, g, b, if self.dark { 20 } else { 16 })
    }

    pub fn prop(&self) -> FontId {
        FontId::new(self.font_size, FontFamily::Proportional)
    }

    pub fn mono(&self) -> FontId {
        FontId::new(self.mono_size, FontFamily::Monospace)
    }

    /// (font, foreground, background) for a face.
    pub fn face(&self, face: Face) -> (FontId, Color32, Color32) {
        let t = Color32::TRANSPARENT;
        match face {
            Face::Default => (self.prop(), self.fg, t),
            Face::Code => (self.mono(), self.fg, t),
            Face::CodeBold => (self.mono(), self.fg, t),
            Face::CodeDim => (self.mono(), self.fg_faint, t),
            Face::Dim => (self.prop(), self.fg_dim, t),
            Face::Bold => (FontId::new(self.font_size, FontFamily::Proportional), self.fg, t),
            Face::Italic | Face::BoldItalic => (self.prop(), self.fg, t),
            Face::Strike => (self.prop(), self.fg_dim, t),
            Face::Accent => (self.prop(), self.accent, t),
            Face::UserLabel => (self.prop(), self.accent, t),
            Face::UserText => (self.prop(), self.fg, t),
            Face::AssistantText => (self.prop(), self.fg, t),
            Face::Thinking => (FontId::new(self.font_size - 1.0, FontFamily::Proportional), self.fg_faint, t),
            Face::ToolHeader => (FontId::new(self.font_size - 1.0, FontFamily::Proportional), self.fg, t),
            Face::ToolArgs => (self.mono(), self.fg_dim, t),
            Face::ToolOutput => (self.mono(), self.fg_dim, t),
            Face::ToolError => (self.mono(), self.red, t),
            Face::ToolOk => (self.mono(), self.green, t),
            // Diff lines tint their whole row (the card behind them), not just the glyphs.
            Face::DiffAdd => (self.mono(), self.diff_add_fg, t),
            Face::DiffDel => (self.mono(), self.diff_del_fg, t),
            Face::DiffContext => (self.mono(), self.fg_faint, t),
            Face::DiffHunk => (self.mono(), self.purple, t),
            Face::Error => (self.prop(), self.red, t),
            Face::Warning => (self.prop(), self.yellow, t),
            Face::Heading => (FontId::new(self.font_size + 2.0, FontFamily::Proportional), self.fg, t),
            Face::InlineCode => (FontId::new(self.mono_size - 1.0, FontFamily::Monospace), self.cyan, self.chip()),
            Face::Link => (self.prop(), self.blue, t),
            Face::PromptMarker => (self.mono(), self.accent, t),
            Face::Meta => (FontId::new(self.font_size - 2.5, FontFamily::Proportional), self.fg_faint, t),
            Face::MetaGood => (FontId::new(self.font_size - 2.5, FontFamily::Proportional), self.green, t),
            Face::MetaWarn => (FontId::new(self.font_size - 2.5, FontFamily::Proportional), self.yellow, t),
            Face::Label => (FontId::new(self.font_size - 1.5, FontFamily::Proportional), self.fg_dim, t),
            Face::Unseen => (self.mono(), Color32::TRANSPARENT, t),
            Face::SynKeyword => (self.mono(), self.purple, t),
            Face::SynString => (self.mono(), self.green, t),
            Face::SynComment => (self.mono(), self.fg_faint, t),
            Face::SynFunction => (self.mono(), self.blue, t),
            Face::SynType => (self.mono(), self.yellow, t),
            Face::SynConstant => (self.mono(), self.cyan, t),
            Face::SynAttribute => (self.mono(), self.cyan, t),
        }
    }

    pub fn apply(&self, ctx: &egui::Context) {
        let mut visuals = if self.dark { egui::Visuals::dark() } else { egui::Visuals::light() };
        visuals.panel_fill = self.bg;
        visuals.window_fill = self.bg_elevated;
        visuals.extreme_bg_color = self.bg_alt;
        visuals.faint_bg_color = self.bg_alt;
        visuals.override_text_color = Some(self.fg);
        visuals.selection.bg_fill = self.selection;
        visuals.window_stroke = egui::Stroke::new(1.0, self.border);
        visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, self.border);
        visuals.window_corner_radius = egui::CornerRadius::same(self.radius + 4);
        visuals.menu_corner_radius = egui::CornerRadius::same(self.radius + 2);
        ctx.set_visuals(visuals);
        ctx.all_styles_mut(|s| {
            s.spacing.item_spacing = egui::vec2(6.0, 4.0);
            s.spacing.button_padding = egui::vec2(6.0, 2.0);
        });
    }
}
