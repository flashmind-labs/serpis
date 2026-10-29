//! The credentials page: on a first start, when no provider has a key anywhere, serpis asks
//! for one here instead of opening a chat that can't run. The key goes to
//! `~/.serpis/agent/auth.json` (`chat-set-api-key`), and the page turns into a way to start as
//! soon as one is found. `setup-credentials` shows it again at any time.

use egui::{FontId, Rect};
use serde_json::{Value, json};

use crate::buffer::{Buffer, Modeline};
use crate::editor::{BUILTIN, Editor};
use crate::impl_any;

/// Its name and major mode.
pub const NAME: &str = "*Welcome*";
pub const KIND: &str = "welcome";

pub struct WelcomeBuffer;

/// Whether to ask for credentials on this start: the config folder is new (so this is a first
/// start), no provider has a key anywhere, and the page was never finished or skipped.
pub fn should_ask(ed: &Editor, fresh_config: bool) -> bool {
    fresh_config
        && ed.var_bool("first-run-setup")
        && Editor::persisted_state("setup-done").is_none()
        && ed.registry.configured_providers().is_empty()
}

/// Show the page (making its buffer if it isn't there).
pub fn open(ed: &mut Editor) {
    let id = match ed.buffers.iter().find(|(_, s)| s.meta.major == KIND) {
        Some((id, _)) => id,
        None => match ed.create_buffer(KIND, Some(NAME), &Value::Null) {
            Ok(id) => id,
            Err(e) => {
                ed.error(format!("{e:#}"));
                return;
            }
        },
    };
    ed.show_buffer(id);
}

/// Done asking: remember it, close the page and show a chat.
fn finish(ed: &mut Editor) {
    ed.persist_state("setup-done", json!(true));
    match ed
        .buffers
        .iter()
        .find(|(_, s)| s.meta.major == "chat")
        .map(|(id, _)| id)
    {
        Some(id) => ed.show_buffer(id),
        None => {
            if let Ok(id) = ed.create_buffer("chat", Some("*chat*"), &Value::Null) {
                ed.show_buffer(id);
            }
        }
    }
    for id in ed
        .buffers
        .iter()
        .filter(|(_, s)| s.meta.major == KIND)
        .map(|(id, _)| id)
        .collect::<Vec<_>>()
    {
        ed.kill_buffer(id);
    }
}

impl Buffer for WelcomeBuffer {
    fn kind(&self) -> &str {
        KIND
    }

    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, _focused: bool, ed: &mut Editor) {
        let t = ed.theme.clone();
        let k = t.ui_scale();
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 0.0, t.bg);
        // A key in auth.json, in models.json or in the environment, found meanwhile.
        let found = ed.registry.configured_providers();
        let width = (rect.width() - 48.0 * k).clamp(240.0, 620.0 * k);
        let left = rect.center().x - width / 2.0;
        let body = FontId::proportional(t.font_size - 1.0);
        let line_h = (t.font_size + 8.0) * k;
        let rows: [(&'static str, &'static str); 2] = if found.is_empty() {
            [
                ("chat-set-api-key", "Add an API key"),
                ("setup-credentials-finish", "Skip, for now"),
            ]
        } else {
            [
                ("setup-credentials-finish", "Start a chat"),
                ("chat-select-model", "Choose a model"),
            ]
        };
        let text: Vec<String> = if found.is_empty() {
            vec![
                "Chats run on a model provider's API, so serpis needs a key from one.".into(),
                "A key added here goes to ~/.serpis/agent/auth.json, readable by you only".into(),
                "(pi's ~/.pi/agent/auth.json is read too). Keys in the environment, such as".into(),
                "OPENROUTER_API_KEY or ANTHROPIC_API_KEY, work without this page.".into(),
            ]
        } else {
            vec![
                format!("Keys found for {}.", found.join(", ")),
                "The model of a chat is chosen with chat-select-model (C-c C-m).".into(),
            ]
        };
        let row_h = 34.0 * k;
        let block_h = 46.0 * k + text.len() as f32 * line_h + 18.0 * k + rows.len() as f32 * row_h;
        let mut y = (rect.center().y - block_h / 2.0).max(rect.top() + 24.0 * k);
        painter.text(
            egui::pos2(rect.center().x, y),
            egui::Align2::CENTER_TOP,
            "Welcome to serpis",
            FontId::proportional(t.font_size + 5.0),
            t.fg_dim,
        );
        y += 46.0 * k;
        for l in &text {
            crate::chrome::text(
                &painter,
                egui::pos2(left, y + line_h / 2.0),
                l,
                body.clone(),
                t.fg_dim,
                width,
            );
            y += line_h;
        }
        y += 18.0 * k;
        let keys = crate::commands::all_bindings(ed);
        let mut run = None;
        for (cmd, label) in rows {
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
                width * 0.6,
            );
            if let Some(key) = keys.get(cmd) {
                crate::chrome::keycap(&painter, r.right() - 10.0 * k, r.center().y, key, &t);
            }
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

    fn full_text(&mut self) -> Option<String> {
        Some(String::new())
    }

    fn modeline(&self) -> Modeline {
        Modeline {
            title: "Welcome".into(),
            ..Default::default()
        }
    }

    impl_any!();
}

pub fn install(ed: &mut Editor) {
    // A test runs from an empty home and would land here before it has written a models.json:
    // it asks for the page only when it says so (SERPIS_TEST_FIRST_RUN=1).
    let first_run = std::env::var_os("SERPIS_TEST_SCRIPT").is_none()
        || std::env::var_os("SERPIS_TEST_FIRST_RUN").is_some_and(|v| v == "1");
    ed.defvar("first-run-setup", json!(first_run), "On a start whose config folder is new (~/.config/serpis/init.lua absent), where no provider has a key anywhere, show this page instead of a chat, to add one (chat-set-api-key). setup-credentials shows it any time; finishing or skipping it records setup-done in state.json.");
    ed.define_command(
        "setup-credentials",
        "Set up model credentials: add an API key, then start chatting.",
        BUILTIN,
        |ed, _| {
            open(ed);
            Ok(())
        },
    );
    ed.define_command(
        "setup-credentials-finish",
        "Done with setup-credentials: remember it and show a chat.",
        BUILTIN,
        |ed, _| {
            finish(ed);
            Ok(())
        },
    );
}
