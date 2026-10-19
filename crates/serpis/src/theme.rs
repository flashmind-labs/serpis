//! Colors, fonts and faces. Everything is a field so Lua/Rust config can restyle it.

use egui::{Color32, FontFamily, FontId};

/// Semantic text faces, resolved to colors and fonts by the theme (like Emacs faces).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Face {
    #[default]
    Default,
    Code,
    Dim,
    Bold,
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
}

#[derive(Debug, Clone)]
pub struct Theme {
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
    pub user_bg: Color32,
    pub tool_bg: Color32,
    pub font_size: f32,
    pub mono_size: f32,
    pub radius: u8,
}

impl Default for Theme {
    fn default() -> Self {
        Self::dark()
    }
}

impl Theme {
    pub fn dark() -> Self {
        Theme {
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
            user_bg: Color32::from_rgb(0x22, 0x24, 0x33),
            tool_bg: Color32::from_rgb(0x1b, 0x1c, 0x24),
            font_size: 14.5,
            mono_size: 13.5,
            radius: 6,
        }
    }

    pub fn light() -> Self {
        Theme {
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
            user_bg: Color32::from_rgb(0xec, 0xee, 0xf8),
            tool_bg: Color32::from_rgb(0xf4, 0xf5, 0xf8),
            font_size: 14.5,
            mono_size: 13.5,
            radius: 6,
        }
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
            Face::Dim => (self.prop(), self.fg_dim, t),
            Face::Bold => (FontId::new(self.font_size, FontFamily::Proportional), self.fg, t),
            Face::Accent => (self.prop(), self.accent, t),
            Face::UserLabel => (self.prop(), self.accent, t),
            Face::UserText => (self.prop(), self.fg, t),
            Face::AssistantText => (self.prop(), self.fg, t),
            Face::Thinking => (FontId::new(self.font_size - 1.0, FontFamily::Proportional), self.fg_faint, t),
            Face::ToolHeader => (self.mono(), self.accent2, t),
            Face::ToolArgs => (self.mono(), self.fg_dim, t),
            Face::ToolOutput => (self.mono(), self.fg_dim, t),
            Face::ToolError => (self.mono(), self.red, t),
            Face::ToolOk => (self.mono(), self.green, t),
            Face::DiffAdd => (self.mono(), self.green, self.diff_add_bg),
            Face::DiffDel => (self.mono(), self.red, self.diff_del_bg),
            Face::DiffContext => (self.mono(), self.fg_faint, t),
            Face::DiffHunk => (self.mono(), self.purple, t),
            Face::Error => (self.prop(), self.red, t),
            Face::Warning => (self.prop(), self.yellow, t),
            Face::Heading => (FontId::new(self.font_size + 2.0, FontFamily::Proportional), self.fg, t),
            Face::InlineCode => (self.mono(), self.cyan, t),
            Face::Link => (self.prop(), self.blue, t),
            Face::PromptMarker => (self.mono(), self.accent, t),
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
