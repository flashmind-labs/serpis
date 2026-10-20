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

    /// Whether the buffer runs a process, and what it is doing (confirms kills and quits).
    fn process_status(&self) -> ProcessStatus {
        ProcessStatus::None
    }

    /// What the buffer is doing, for the sidebar and `buffer-activity-changed`. The default
    /// derives it from [`Buffer::process_status`]; a buffer-local `activity` variable
    /// (`serpis.buffer.set_activity`) overrides it.
    fn activity(&self) -> Activity {
        match self.process_status() {
            ProcessStatus::Running(p) => Activity::new(ActivityState::Working, p),
            _ => Activity::default(),
        }
    }

    /// A short human title (a chat's first prompt, a terminal's title), shown by the sidebar.
    fn title(&self) -> String {
        self.modeline().title
    }

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

/// What a buffer's process is doing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessStatus {
    /// Not a process buffer.
    None,
    /// A shell waiting at its prompt.
    Idle,
    /// A program in the foreground, by name.
    Running(String),
}

/// Coarse state of a buffer's work, from the user's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub enum ActivityState {
    /// Nothing happening.
    #[default]
    Idle,
    /// Producing output: an agent turn, a running command.
    Working,
    /// An agent (or program) is waiting for the user.
    Waiting,
    /// The last run failed.
    Error,
    /// The process is gone.
    Exited,
}

impl ActivityState {
    pub fn as_str(self) -> &'static str {
        match self {
            ActivityState::Idle => "idle",
            ActivityState::Working => "working",
            ActivityState::Waiting => "waiting",
            ActivityState::Error => "error",
            ActivityState::Exited => "exited",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "idle" => ActivityState::Idle,
            "working" => ActivityState::Working,
            "waiting" => ActivityState::Waiting,
            "error" => ActivityState::Error,
            "exited" => ActivityState::Exited,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Activity {
    pub state: ActivityState,
    /// What exactly: `bash cargo test`, `thinking`, `claude`, ...
    pub detail: String,
    /// When the current state began (elapsed timers).
    pub since: Option<std::time::Instant>,
    /// The program in the foreground, for process buffers.
    pub program: Option<String>,
}

impl Activity {
    pub fn new(state: ActivityState, detail: impl Into<String>) -> Self {
        Activity { state, detail: detail.into(), ..Default::default() }
    }
}

pub struct BufferSlot {
    pub meta: BufferMeta,
    pub buf: Option<Box<dyn Buffer>>,
}
