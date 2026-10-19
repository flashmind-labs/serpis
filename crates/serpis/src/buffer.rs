//! Buffers: the unit of content. A chat, a terminal, a text file, a help page — all buffers,
//! all shown in windows, all driven by the same keymaps and commands.

use std::any::Any;
use std::collections::HashMap;

use egui::Rect;
use serde_json::Value;

use crate::editor::Editor;
use crate::keys::KeyChord;
use crate::text::{CursorShape, TextSurface};

slotmap::new_key_type! {
    pub struct BufferId;
    pub struct WindowId;
}

/// Editor-owned buffer state, available even while the buffer object is borrowed out.
#[derive(Debug, Clone, Default)]
pub struct BufferMeta {
    pub name: String,
    /// Major mode, e.g. `chat`, `terminal`, `text`. Selects the `<major>` keymap.
    pub major: String,
    /// Minor modes in priority order, e.g. `vim-normal`. Each selects `<minor>` and `<minor>:<major>` keymaps.
    pub minor_modes: Vec<String>,
    /// Buffer-local variables.
    pub vars: HashMap<String, Value>,
}

impl BufferMeta {
    pub fn has_mode(&self, m: &str) -> bool {
        self.minor_modes.iter().any(|x| x == m)
    }
}

/// What a buffer shows in its window header and the mode line.
#[derive(Debug, Clone, Default)]
pub struct Modeline {
    pub title: String,
    /// Short right-aligned status entries (model, cwd, process state, ...).
    pub status: Vec<String>,
    pub busy: bool,
}

/// A buffer implementation. Every method receives the whole editor: buffers are taken out of
/// the editor while called, so they can freely use any editor API (Emacs style).
pub trait Buffer: Any {
    /// Major mode name.
    fn kind(&self) -> &str;

    /// Paint the buffer into `rect`.
    fn ui(&mut self, ui: &mut egui::Ui, rect: Rect, focused: bool, ed: &mut Editor);

    /// A key chord no keymap or interceptor claimed. Return true if consumed.
    fn handle_key(&mut self, _chord: &KeyChord, _ed: &mut Editor) -> bool {
        false
    }

    /// Typed text that no binding claimed (self-insert).
    fn handle_text(&mut self, _text: &str, _ed: &mut Editor) -> bool {
        false
    }

    /// Clipboard paste.
    fn handle_paste(&mut self, text: &str, ed: &mut Editor) -> bool {
        self.handle_text(text, ed)
    }

    /// Per-frame work: drain channels, advance state.
    fn poll(&mut self, _ed: &mut Editor) {}

    fn surface(&mut self) -> Option<&mut dyn TextSurface> {
        None
    }

    fn surface_ref(&self) -> Option<&dyn TextSurface> {
        None
    }

    fn modeline(&self) -> Modeline {
        Modeline::default()
    }

    /// Cursor style override (vim sets it per state through `Editor::cursor_shape`).
    fn default_cursor(&self) -> CursorShape {
        CursorShape::Bar
    }

    fn is_modified(&self) -> bool {
        false
    }

    /// The directory this buffer works in, used for project detection.
    fn directory(&self) -> Option<std::path::PathBuf> {
        None
    }

    /// Called before the buffer is destroyed.
    fn on_kill(&mut self, _ed: &mut Editor) {}

    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}

/// Implements `as_any`/`as_any_mut` for a buffer type.
#[macro_export]
macro_rules! impl_any {
    () => {
        fn as_any(&self) -> &dyn std::any::Any {
            self
        }
        fn as_any_mut(&mut self) -> &mut dyn std::any::Any {
            self
        }
    };
}

pub struct BufferSlot {
    pub meta: BufferMeta,
    pub buf: Option<Box<dyn Buffer>>,
}
