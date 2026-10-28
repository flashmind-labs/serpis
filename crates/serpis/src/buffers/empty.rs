//! What an empty workspace shows: its only window when everything in it was closed, as i3 shows
//! the wallpaper. Not a buffer of the user's: no buffer list, switcher or sidebar shows it, and
//! opening anything in its window takes its place. It offers what to open, with the keys bound
//! to each.

use egui::{FontId, Rect};

use crate::buffer::{Buffer, Modeline};
use crate::editor::Editor;
use crate::impl_any;

/// Its name and major mode (the `empty` keymap).
pub const NAME: &str = "*empty*";
pub const KIND: &str = "empty";

/// What it offers: (command, label).
const ACTIONS: [(&str, &str); 5] = [
    ("new-chat", "New chat"),
    ("new-terminal", "New terminal"),
    ("find-file", "Open a file"),
    ("switch-to-buffer", "Switch to a buffer"),
    ("chat-resume", "Resume a chat"),
];

#[derive(Default)]
pub struct EmptyBuffer;

impl Buffer for EmptyBuffer {
    fn kind(&self) -> &str {
        KIND
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, _focused: bool, ed: &mut Editor) {
        let t = ed.theme.clone();
        let k = t.ui_scale();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, t.bg);
        let keys = crate::commands::all_bindings(ed);
        let row_h = 34.0 * k;
        let block_h = 64.0 * k + row_h * ACTIONS.len() as f32;
        let c = rect.center();
        let mut y = (c.y - block_h / 2.0).max(rect.top() + 24.0 * k);
        let title = if ed.workspace.is_empty() { "Empty workspace".to_string() } else { format!("Workspace {} is empty", ed.workspace) };
        painter.text(egui::pos2(c.x, y), egui::Align2::CENTER_TOP, title, FontId::proportional(t.font_size + 4.0), t.fg_dim);
        y += 46.0 * k;
        let width = (rect.width() - 48.0 * k).clamp(160.0, 340.0 * k);
        let left = c.x - width / 2.0;
        let font = FontId::proportional(t.font_size - 1.0);
        let mut run = None;
        for (cmd, label) in ACTIONS {
            let r = Rect::from_min_size(egui::pos2(left, y), egui::vec2(width, row_h - 4.0 * k));
            let resp = ui.interact(r, ui.scope_id().with(("empty", cmd)), egui::Sense::click());
            if resp.hovered() {
                painter.rect_filled(r, 8.0 * k, crate::chrome::hover_bg(&t));
            }
            crate::chrome::text(&painter, egui::pos2(r.left() + 12.0 * k, r.center().y), label, font.clone(), if resp.hovered() { t.fg } else { t.fg_dim }, width * 0.6);
            if let Some(key) = keys.get(cmd) {
                crate::chrome::keycap(&painter, r.right() - 10.0 * k, r.center().y, key, &t);
            }
            if resp.clicked() {
                run = Some(cmd);
            }
            y += row_h;
        }
        if let Some(cmd) = run {
            ed.main_sender().send(move |ed| ed.call(cmd));
        }
    }

    fn full_text(&mut self) -> Option<String> {
        Some(String::new())
    }

    fn modeline(&self) -> Modeline {
        Modeline { title: "Empty".into(), ..Default::default() }
    }

    impl_any!();
}
