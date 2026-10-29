//! The credentials page: when no provider has a key anywhere, serpis asks for one here instead
//! of opening a chat that can't run. It closes onto a chat as soon as a key is found.

use egui::{FontId, Rect};
use serde_json::Value;

use crate::buffer::Buffer;
use crate::editor::{BUILTIN, Editor};
use crate::impl_any;

pub const KIND: &str = "welcome";

pub struct WelcomeBuffer;

/// Show the page on start when no provider has a key. A test asks for it with
/// SERPIS_TEST_FIRST_RUN=1, since it starts from an empty home.
pub fn open_if_needed(ed: &mut Editor) {
    let wanted = std::env::var_os("SERPIS_TEST_SCRIPT").is_none()
        || std::env::var_os("SERPIS_TEST_FIRST_RUN").is_some_and(|v| v == "1");
    if !wanted || !ed.registry.configured_providers().is_empty() {
        return;
    }
    match ed.create_buffer(KIND, Some("*Welcome*"), &Value::Null) {
        Ok(id) => ed.show_buffer(id),
        Err(e) => ed.error(format!("{e:#}")),
    }
}

/// Close the page and show a chat.
fn finish(ed: &mut Editor) {
    let chat = ed
        .buffers
        .iter()
        .find(|(_, s)| s.meta.major == "chat")
        .map(|(id, _)| id);
    if let Some(id) = chat.or_else(|| ed.create_buffer("chat", Some("*chat*"), &Value::Null).ok()) {
        ed.show_buffer(id);
    }
    let pages: Vec<_> = ed
        .buffers
        .iter()
        .filter(|(_, s)| s.meta.major == KIND)
        .map(|(id, _)| id)
        .collect();
    for id in pages {
        ed.kill_buffer(id);
    }
}

impl Buffer for WelcomeBuffer {
    fn kind(&self) -> &str {
        KIND
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, _focused: bool, ed: &mut Editor) {
        if !ed.registry.configured_providers().is_empty() {
            ed.main_sender().send(finish);
            return;
        }
        let t = ed.theme.clone();
        let k = t.ui_scale();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, t.bg);
        let width = (rect.width() - 48.0 * k).clamp(240.0, 620.0 * k);
        let left = rect.center().x - width / 2.0;
        let body = FontId::proportional(t.font_size - 1.0);
        let row_h = 34.0 * k;
        let mut y = rect.center().y - 60.0 * k;
        painter.text(
            egui::pos2(rect.center().x, y),
            egui::Align2::CENTER_TOP,
            "Chats need an API key from a model provider.",
            body.clone(),
            t.fg_dim,
        );
        y += 46.0 * k;
        let mut run = None;
        for (cmd, label) in [
            ("chat-set-api-key", "Add an API key"),
            ("setup-credentials-finish", "Skip, for now"),
        ] {
            let r = Rect::from_min_size(egui::pos2(left, y), egui::vec2(width, row_h - 4.0 * k));
            let resp = ui.interact(
                r,
                ui.scope_id().with(("welcome", cmd)),
                egui::Sense::click(),
            );
            if resp.hovered() {
                painter.rect_filled(r, 8.0 * k, crate::chrome::hover_bg(&t));
            }
            let color = if resp.hovered() { t.fg } else { t.fg_dim };
            crate::chrome::text(
                &painter,
                egui::pos2(r.left() + 12.0 * k, r.center().y),
                label,
                body.clone(),
                color,
                width,
            );
            ed.debug_rects.insert(cmd, r);
            if resp.clicked() {
                run = Some(cmd);
            }
            y += row_h;
        }
        if let Some(cmd) = run {
            ed.main_sender().send(move |ed| ed.call(cmd));
        }
    }

    impl_any!();
}

pub fn install(ed: &mut Editor) {
    ed.define_command(
        "setup-credentials-finish",
        "Close the credentials page and show a chat.",
        BUILTIN,
        |ed, _| {
            finish(ed);
            Ok(())
        },
    );
}
