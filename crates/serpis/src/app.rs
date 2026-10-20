//! The eframe application: routes all keyboard input through the editor before egui sees it.

use egui::Event;

use crate::editor::Editor;
use crate::keys::{KeyChord, Mods, chord_from_egui};
use crate::packages::vim;

pub struct App {
    pub ed: Editor,
    _rt: Option<tokio::runtime::Runtime>,
}

impl App {
    pub fn new(ed: Editor, rt: tokio::runtime::Runtime) -> Self {
        App { ed, _rt: Some(rt) }
    }
}

fn command_chord(c: char) -> KeyChord {
    // Clipboard events stand for Cmd-<c> on macOS and Ctrl-<c> elsewhere.
    let mods = if cfg!(target_os = "macos") {
        Mods { sup: true, ..Mods::NONE }
    } else {
        Mods { ctrl: true, ..Mods::NONE }
    };
    KeyChord::ch(c).with(mods)
}

impl eframe::App for App {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw: &mut egui::RawInput) {
        // Context menus are egui popups: let them see ESC so it closes them.
        let popup_open = egui::Popup::is_any_open(ctx);
        let ed = &mut self.ed;
        let mut keep = Vec::with_capacity(raw.events.len());
        let mut events = std::mem::take(&mut raw.events);
        events.append(&mut ed.injected_events);
        // The text a key produces arrives in the same batch; never carry the skip over.
        ed.skip_text = false;
        for ev in events {
            if matches!(ev, Event::Key { pressed: true, .. } | Event::Text(_) | Event::Paste(_) | Event::Copy | Event::Cut) {
                tracing::debug!(target: "serpis::input", "{ev:?}");
            }
            match ev {
                // ESC drops a window drag without moving anything.
                Event::Key { key: egui::Key::Escape, pressed: true, .. } if ed.drag.is_some() => {
                    ed.drag = None;
                }
                ev @ Event::Key { key: egui::Key::Escape, .. } if popup_open => keep.push(ev),
                Event::Key { key, pressed: true, modifiers, .. } => {
                    let Some(chord) = chord_from_egui(key, modifiers) else { continue };
                    // Option+letter types characters on macOS unless the chord is bound
                    // (or the buffer is a terminal, where Meta is meaningful).
                    let meta_only = chord.mods.meta && !chord.mods.ctrl && !chord.mods.sup;
                    if meta_only && chord.printable().is_none() && matches!(chord.key, crate::keys::Key::Char(_)) {
                        let is_terminal = ed.meta(ed.current_buffer()).is_some_and(|m| m.major == "terminal");
                        if !ed.is_bound(&chord) && !is_terminal {
                            ed.skip_text = false;
                            continue;
                        }
                    }
                    // Option+key also emits its character (e.g. "å"); Ctrl/Cmd do not.
                    ed.skip_text = meta_only && matches!(chord.key, crate::keys::Key::Char(_));
                    ed.handle_chord(chord);
                }
                Event::Key { .. } => {}
                Event::Text(t) => {
                    if std::mem::take(&mut ed.skip_text) {
                        continue;
                    }
                    ed.handle_text_input(&t);
                }
                // Cmd/Ctrl-V arrives as a paste event: route it through the `s-v`/`C-v`
                // binding when there is one (the clipboard already holds `t`).
                Event::Paste(t) => {
                    let chord = command_chord('v');
                    if ed.is_bound(&chord) {
                        ed.handle_chord(chord);
                    } else {
                        ed.handle_paste(&t);
                    }
                }
                Event::Copy => ed.handle_chord(command_chord('c')),
                Event::Cut => ed.handle_chord(command_chord('x')),
                other => keep.push(other),
            }
            vim::flush_replay(ed);
        }
        raw.events = keep;
    }

    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.ed.tick();
        if self.ed.quit_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
        let title = {
            let id = self.ed.current_buffer();
            self.ed.meta(id).map(|m| format!("{} — serpis", m.name)).unwrap_or_else(|| "serpis".into())
        };
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        crate::ui::draw(ui, &mut self.ed);
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        self.ed.theme.bg.to_normalized_gamma_f32()
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        crate::session_ui::save_open(&mut self.ed);
        let ids: Vec<_> = self.ed.buffers.keys().collect();
        for id in ids {
            self.ed.kill_buffer(id);
        }
    }
}
