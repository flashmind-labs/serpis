//! The editor: the single mutable world every command, hook, Lua function and Rust plugin
//! operates on. Nothing here is private to "the core": packages use the same API.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use egui::Rect;
use serde_json::Value;
use slotmap::SlotMap;

use crate::buffer::{Buffer, BufferId, BufferMeta, BufferSlot, WindowId};
use crate::keymap::{Keymap, Keymaps, Lookup};
use crate::keys::{KeyChord, format_seq, parse_seq};
use crate::minibuffer::Minibuffer;
use crate::text::{CursorShape, TextSurface};
use crate::theme::Theme;

pub const BUILTIN: &str = "builtin";

#[derive(Debug, Clone, Default)]
pub struct Args {
    /// Numeric prefix (`C-u 4`, vim count).
    pub prefix: Option<i64>,
    /// Key sequence that invoked the command.
    pub keys: Vec<KeyChord>,
    /// Arguments passed programmatically (Lua, M-x with params).
    pub params: Vec<Value>,
}

impl Args {
    pub fn count(&self) -> usize {
        self.prefix.map(|p| p.max(1) as usize).unwrap_or(1)
    }
    pub fn str(&self, i: usize) -> Option<&str> {
        self.params.get(i).and_then(Value::as_str)
    }
    pub fn with_param(mut self, v: impl Into<Value>) -> Self {
        self.params.push(v.into());
        self
    }
}

pub type CommandFn = Rc<dyn Fn(&mut Editor, &Args) -> anyhow::Result<()>>;

#[derive(Clone)]
pub struct Command {
    pub name: String,
    pub doc: String,
    pub owner: String,
    pub f: CommandFn,
}

/// Hook handlers may return a value (e.g. `{block = "reason"}` from `before-tool-call`).
pub type HookFn = Rc<dyn Fn(&mut Editor, &Value) -> Option<Value>>;

#[derive(Clone)]
pub struct Hook {
    pub owner: String,
    pub f: HookFn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyResult {
    Handled,
    Pass,
}

/// Sees chords that no keymap bound (vim's operator/motion parser lives here).
pub type InterceptorFn = Rc<dyn Fn(&mut Editor, &KeyChord) -> KeyResult>;

#[derive(Clone)]
pub struct Interceptor {
    pub owner: String,
    pub f: InterceptorFn,
}

/// Work `Editor::defer` runs at the start of the next tick.
type Deferred = Box<dyn FnOnce(&mut Editor)>;

#[derive(Debug, Clone)]
pub struct Var {
    pub value: Value,
    pub doc: String,
}

/// Constructor for a buffer kind (`new-buffer <kind>`).
pub type BufferCtor = Rc<dyn Fn(&mut Editor, &Value) -> anyhow::Result<Box<dyn Buffer>>>;

#[derive(Clone)]
pub struct BufferKind {
    pub owner: String,
    pub ctor: BufferCtor,
}

/// Text shown in the echo area (global) or in window headers (per buffer).
pub type SegmentFn = Rc<dyn Fn(&mut Editor, Option<BufferId>) -> Option<String>>;

#[derive(Clone)]
pub struct Segment {
    pub name: String,
    pub owner: String,
    pub f: SegmentFn,
}

/// A temporary highlight over a text range (yank flash, search hits, jump targets).
#[derive(Debug, Clone)]
pub struct Flash {
    pub buffer: BufferId,
    pub start: crate::text::Pos,
    /// Exclusive end.
    pub end: crate::text::Pos,
    pub born: Instant,
    pub duration: Duration,
    pub color: egui::Color32,
}

/// Something in progress, shown with a spinner until it stops (`Editor::busy_start`).
#[derive(Debug, Clone)]
pub struct Busy {
    pub key: String,
    pub label: String,
    pub since: Instant,
}

/// Work sent to the UI thread from anywhere.
pub type MainTask = Box<dyn FnOnce(&mut Editor) + Send>;

#[derive(Clone)]
pub struct MainSender {
    tx: mpsc::Sender<MainTask>,
    ctx: egui::Context,
}

impl MainSender {
    pub fn send(&self, f: impl FnOnce(&mut Editor) + Send + 'static) {
        let _ = self.tx.send(Box::new(f));
        self.ctx.request_repaint();
    }
    pub fn repaint(&self) {
        self.ctx.request_repaint();
    }
}

pub use crate::layout::{Container, Dir, Kind, LayoutTree, Node, NodeRef};

pub struct Window {
    pub buffer: BufferId,
    /// Last painted rectangle (for directional navigation and overlays).
    pub rect: Rect,
    /// Buffers previously shown here, most recent last.
    pub history: Vec<BufferId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Warn,
    Error,
}

/// A label painted over a buffer (jump hints, diagnostics). Cleared by its owner.
#[derive(Debug, Clone)]
pub struct Overlay {
    pub owner: String,
    pub rect: Rect,
    pub label: String,
    pub color: egui::Color32,
}

pub struct Editor {
    pub buffers: SlotMap<BufferId, BufferSlot>,
    pub windows: SlotMap<WindowId, Window>,
    /// The current workspace's layout; other workspaces are kept in `workspaces`.
    pub layout: LayoutTree,
    pub focused: WindowId,
    /// Inactive workspaces (the current one lives in `layout`/`focused`).
    pub workspaces: Vec<crate::workspace::Workspace>,
    /// Name of the current workspace.
    pub workspace: String,
    pub prev_workspace: Option<String>,
    /// A container selected with `focus-parent`; window commands act on it instead of the
    /// focused window.
    pub selected: Option<NodeRef>,
    /// A window, tab or group being dragged with the mouse.
    pub drag: Option<crate::mouse::Drag>,
    /// Drop targets registered while drawing this frame.
    pub drop_targets: Vec<crate::mouse::DropTarget>,
    pub keymaps: Keymaps,
    pub commands: HashMap<String, Command>,
    pub hooks: HashMap<String, Vec<Hook>>,
    pub vars: HashMap<String, Var>,
    pub interceptors: Vec<Interceptor>,
    pub buffer_kinds: HashMap<String, BufferKind>,
    pub agent_tools: Vec<(String, serpis_agent::ToolRef)>,
    pub pending_keys: Vec<KeyChord>,
    pub pending_since: Option<Instant>,
    pub prefix_arg: Option<i64>,
    pub last_command: Option<String>,
    pub this_command: Option<String>,
    pub messages: Vec<String>,
    pub echo: Option<(String, Level, Instant)>,
    pub minibuffer: Option<Minibuffer>,
    pub theme: Theme,
    pub ctx: egui::Context,
    pub rt: tokio::runtime::Handle,
    main_tx: mpsc::Sender<MainTask>,
    main_rx: mpsc::Receiver<MainTask>,
    pub kill_ring: Vec<String>,
    pub registry: serpis_ai::Registry,
    pub overlays: Vec<Overlay>,
    pub quit_requested: bool,
    pub lua: Option<crate::lua::LuaRuntime>,
    pub plugins: crate::plugins::PluginManager,
    /// Text events to skip because a meta chord already consumed their key press.
    pub(crate) skip_text: bool,
    pub cwd: std::path::PathBuf,
    /// Key sequence capture for `describe-key`.
    pub(crate) describe_key_pending: bool,
    /// Raw platform events queued for the next frame (tests and macros).
    pub injected_events: Vec<egui::Event>,
    /// Echo-area segments (left of the scheme name) and window-header segments.
    pub status_segments: Vec<Segment>,
    pub header_segments: Vec<Segment>,
    /// Status text computed this frame.
    pub status_cache: Vec<String>,
    /// Layout seen at the end of the previous frame (for `window-layout-changed`).
    pub(crate) last_layout: Option<Value>,
    /// Fading highlights drawn over buffers.
    pub flashes: Vec<Flash>,
    /// User-defined palettes (`serpis.theme.define`).
    pub custom_themes: Vec<(String, crate::themes::ThemeSpec)>,
    /// Bumped on every theme change so buffers can refresh derived colors.
    pub theme_generation: u64,
    /// The project/agent sidebar's state.
    pub sidebar: crate::sidebar::Sidebar,
    /// Saved chat sessions, as last read.
    pub sessions: crate::session_ui::Sessions,
    /// How long recent frames took (input, logic and drawing), in milliseconds, newest last.
    pub frame_times: std::collections::VecDeque<f32>,
    /// Where this frame's time went so far (`Editor::timed`): logged when it is slow.
    pub frame_parts: Vec<(String, f32)>,
    /// The breakdowns of recent slow frames (perf-slow-frame-ms), newest last.
    pub slow_frames: std::collections::VecDeque<(f32, Vec<(String, f32)>)>,
    /// Frames drawn so far (`serpis.debug.frames`: tests wait for input to be drawn).
    pub frames_drawn: u64,
    /// Where named controls were drawn last frame (`serpis.debug.rect`, for tests).
    pub debug_rects: HashMap<&'static str, Rect>,
    /// Jump labels on screen (avy/hop), and copies fading out.
    pub jump: crate::jump::JumpState,
    /// What each chat is doing (model, usage, session entries), readable even while the chat
    /// is busy (`serpis.chat`).
    pub chats: HashMap<BufferId, crate::chat_api::ChatInfo>,
    /// Buffers whose code is running (innermost last): `this_buffer`.
    buffer_stack: Vec<BufferId>,
    /// Work for the next tick that needs a busy buffer (`defer`).
    deferred: Vec<Deferred>,
    /// Lua extensions (`<config>/extensions`) that loaded without errors.
    pub loaded_extensions: Vec<String>,
    /// Connections to the nodes of other machines (remote chats).
    pub remote: crate::remote::Remotes,
    /// Work in progress the window shows (busy-indicator): connecting to a machine, ...
    pub busy: Vec<Busy>,
}

impl Editor {
    pub fn new(ctx: egui::Context, rt: tokio::runtime::Handle) -> Self {
        let (main_tx, main_rx) = mpsc::channel();
        let mut buffers = SlotMap::with_key();
        let scratch = buffers.insert(BufferSlot {
            meta: BufferMeta {
                name: "*scratch*".into(),
                major: "text".into(),
                ..Default::default()
            },
            buf: Some(Box::new(crate::buffers::text::TextBuffer::scratch()) as Box<dyn Buffer>),
        });
        let mut windows = SlotMap::with_key();
        let w = windows.insert(Window {
            buffer: scratch,
            rect: Rect::NOTHING,
            history: Vec::new(),
        });
        Editor {
            buffers,
            windows,
            layout: LayoutTree::new(w),
            focused: w,
            workspaces: Vec::new(),
            workspace: "1".into(),
            prev_workspace: None,
            selected: None,
            drag: None,
            drop_targets: Vec::new(),
            keymaps: Keymaps::default(),
            commands: HashMap::new(),
            hooks: HashMap::new(),
            vars: HashMap::new(),
            interceptors: Vec::new(),
            buffer_kinds: HashMap::new(),
            agent_tools: Vec::new(),
            pending_keys: Vec::new(),
            pending_since: None,
            prefix_arg: None,
            last_command: None,
            this_command: None,
            messages: Vec::new(),
            echo: None,
            minibuffer: None,
            theme: Theme::default(),
            ctx,
            rt,
            main_tx,
            main_rx,
            kill_ring: Vec::new(),
            registry: serpis_ai::Registry::new(serpis_ai::Paths::default_paths()),
            overlays: Vec::new(),
            quit_requested: false,
            lua: None,
            plugins: crate::plugins::PluginManager::default(),
            skip_text: false,
            cwd: std::env::current_dir().unwrap_or_default(),
            describe_key_pending: false,
            injected_events: Vec::new(),
            status_segments: Vec::new(),
            header_segments: Vec::new(),
            status_cache: Vec::new(),
            last_layout: None,
            flashes: Vec::new(),
            custom_themes: Vec::new(),
            theme_generation: 0,
            sidebar: Default::default(),
            sessions: Default::default(),
            frame_times: Default::default(),
            frames_drawn: 0,
            debug_rects: HashMap::new(),
            jump: Default::default(),
            chats: HashMap::new(),
            buffer_stack: Vec::new(),
            deferred: Vec::new(),
            loaded_extensions: Vec::new(),
            remote: Default::default(),
            busy: Vec::new(),
            frame_parts: Vec::new(),
            slow_frames: Default::default(),
        }
    }

    pub fn main_sender(&self) -> MainSender {
        MainSender {
            tx: self.main_tx.clone(),
            ctx: self.ctx.clone(),
        }
    }

    // ------------------------------------------------------------------
    // Messages
    // ------------------------------------------------------------------

    pub fn message(&mut self, text: impl Into<String>) {
        let text = text.into();
        tracing::info!(target: "message", "{text}");
        self.messages.push(text.clone());
        self.echo = Some((text, Level::Info, Instant::now()));
    }

    pub fn warn(&mut self, text: impl Into<String>) {
        let text = text.into();
        tracing::warn!(target: "message", "{text}");
        self.messages.push(format!("warning: {text}"));
        self.echo = Some((text, Level::Warn, Instant::now()));
    }

    pub fn error(&mut self, text: impl Into<String>) {
        let text = text.into();
        tracing::error!(target: "message", "{text}");
        self.messages.push(format!("error: {text}"));
        self.echo = Some((text, Level::Error, Instant::now()));
    }

    // ------------------------------------------------------------------
    // Commands, keymaps, hooks, vars
    // ------------------------------------------------------------------

    pub fn define_command(&mut self, name: &str, doc: &str, owner: &str, f: impl Fn(&mut Editor, &Args) -> anyhow::Result<()> + 'static) {
        self.commands.insert(
            name.to_string(),
            Command {
                name: name.to_string(),
                doc: doc.to_string(),
                owner: owner.to_string(),
                f: Rc::new(f),
            },
        );
    }

    pub fn run_command(&mut self, name: &str, args: &Args) -> anyhow::Result<()> {
        let cmd = self
            .commands
            .get(name)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("No such command: {name}"))?;
        let prev = self.this_command.replace(name.to_string());
        // Each command is one undo group.
        if let Some(s) = self.focused_surface_mut_raw() {
            s.undo_boundary();
        }
        let r = (cmd.f)(self, args);
        self.last_command = self.this_command.take();
        if prev.is_some() {
            self.this_command = prev;
        }
        r
    }

    /// Run a command and report errors in the echo area.
    pub fn call(&mut self, name: &str) {
        let args = Args {
            prefix: self.prefix_arg.take(),
            ..Default::default()
        };
        if let Err(e) = self.run_command(name, &args) {
            self.error(format!("{name}: {e:#}"));
        }
    }

    /// Bind `keys` (kbd notation) in keymap `map` to command `cmd`.
    pub fn bind(&mut self, map: &str, keys: &str, cmd: &str, owner: &str) {
        match parse_seq(keys) {
            Ok(seq) => self.keymaps.get_mut(map).bind(&seq, cmd, owner),
            Err(e) => self.error(format!("bind {map} {keys}: {e}")),
        }
    }

    pub fn unbind(&mut self, map: &str, keys: &str) {
        if let Ok(seq) = parse_seq(keys) {
            self.keymaps.get_mut(map).unbind(&seq);
        }
    }

    pub fn add_hook(&mut self, name: &str, owner: &str, f: impl Fn(&mut Editor, &Value) -> Option<Value> + 'static) {
        self.hooks.entry(name.to_string()).or_default().push(Hook {
            owner: owner.to_string(),
            f: Rc::new(f),
        });
    }

    /// Run every handler of hook `name`; returns their non-nil results.
    pub fn run_hook(&mut self, name: &str, payload: &Value) -> Vec<Value> {
        let hooks = self.hooks.get(name).cloned().unwrap_or_default();
        hooks.iter().filter_map(|h| (h.f)(self, payload)).collect()
    }

    pub fn add_interceptor(&mut self, owner: &str, f: impl Fn(&mut Editor, &KeyChord) -> KeyResult + 'static) {
        self.interceptors.push(Interceptor {
            owner: owner.to_string(),
            f: Rc::new(f),
        });
    }

    pub fn defvar(&mut self, name: &str, value: Value, doc: &str) {
        self.vars.entry(name.to_string()).or_insert(Var { value, doc: doc.to_string() });
    }

    pub fn set_var(&mut self, name: &str, value: Value) {
        let old = self.vars.get(name).map(|v| v.value.clone());
        match self.vars.get_mut(name) {
            Some(v) => v.value = value.clone(),
            None => {
                self.vars.insert(name.to_string(), Var { value: value.clone(), doc: String::new() });
            }
        }
        if old.as_ref() != Some(&value) {
            crate::remote::var_changed(self, name, &value);
            self.run_hook("variable-changed", &serde_json::json!({"name": name, "value": value}));
        }
    }

    pub fn var(&self, name: &str) -> Option<&Value> {
        self.vars.get(name).map(|v| &v.value)
    }

    pub fn var_str(&self, name: &str) -> Option<String> {
        self.var(name).and_then(Value::as_str).map(str::to_string)
    }

    pub fn var_bool(&self, name: &str) -> bool {
        self.var(name).and_then(Value::as_bool).unwrap_or(false)
    }

    pub fn define_buffer_kind(&mut self, kind: &str, owner: &str, ctor: impl Fn(&mut Editor, &Value) -> anyhow::Result<Box<dyn Buffer>> + 'static) {
        self.buffer_kinds.insert(
            kind.to_string(),
            BufferKind {
                owner: owner.to_string(),
                ctor: Rc::new(ctor),
            },
        );
    }

    pub fn register_tool(&mut self, owner: &str, tool: serpis_agent::ToolRef) {
        self.agent_tools.retain(|(_, t)| t.name() != tool.name());
        self.agent_tools.push((owner.to_string(), tool));
        self.remote.tools_changed = true;
    }

    /// Remove everything `owner` registered: commands, bindings, hooks, interceptors, buffer
    /// kinds and agent tools. This is what makes package reloads exact.
    pub fn remove_owner(&mut self, owner: &str) {
        self.commands.retain(|_, c| c.owner != owner);
        self.keymaps.remove_owner(owner);
        for hs in self.hooks.values_mut() {
            hs.retain(|h| h.owner != owner);
        }
        self.interceptors.retain(|i| i.owner != owner);
        self.buffer_kinds.retain(|_, k| k.owner != owner);
        let tools = self.agent_tools.len();
        self.agent_tools.retain(|(o, _)| o != owner);
        self.remote.tools_changed |= tools != self.agent_tools.len();
        self.overlays.retain(|o| o.owner != owner);
        self.status_segments.retain(|s| s.owner != owner);
        self.header_segments.retain(|s| s.owner != owner);
    }

    /// Add (or replace by name) a segment. `header` segments get the window's buffer.
    pub fn add_segment(&mut self, header: bool, name: &str, owner: &str, f: impl Fn(&mut Editor, Option<BufferId>) -> Option<String> + 'static) {
        let list = if header { &mut self.header_segments } else { &mut self.status_segments };
        list.retain(|s| s.name != name);
        list.push(Segment { name: name.to_string(), owner: owner.to_string(), f: Rc::new(f) });
    }

    /// Evaluate header segments for a buffer.
    pub fn header_text(&mut self, id: BufferId) -> Vec<String> {
        let segs = self.header_segments.clone();
        segs.iter().filter_map(|s| (s.f)(self, Some(id))).filter(|t| !t.is_empty()).collect()
    }

    // ------------------------------------------------------------------
    // Buffers
    // ------------------------------------------------------------------

    fn unique_name(&self, base: &str) -> String {
        let taken = |n: &str| self.buffers.values().any(|s| s.meta.name == n);
        if !taken(base) {
            return base.to_string();
        }
        (2..).map(|i| format!("{base}<{i}>")).find(|n| !taken(n)).unwrap()
    }

    pub fn add_buffer(&mut self, name: &str, buf: Box<dyn Buffer>) -> BufferId {
        let meta = BufferMeta {
            name: self.unique_name(name),
            major: buf.kind().to_string(),
            ..Default::default()
        };
        let id = self.buffers.insert(BufferSlot { meta, buf: Some(buf) });
        let payload = serde_json::json!({"buffer": buffer_id_to_u64(id), "kind": self.buffers[id].meta.major});
        self.run_hook("buffer-created", &payload);
        id
    }

    /// Create a buffer of a registered kind.
    pub fn create_buffer(&mut self, kind: &str, name: Option<&str>, params: &Value) -> anyhow::Result<BufferId> {
        let k = self
            .buffer_kinds
            .get(kind)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("Unknown buffer kind: {kind}"))?;
        let buf = (k.ctor)(self, params)?;
        let default_name = format!("*{kind}*");
        Ok(self.add_buffer(name.unwrap_or(&default_name), buf))
    }

    pub fn meta(&self, id: BufferId) -> Option<&BufferMeta> {
        self.buffers.get(id).map(|s| &s.meta)
    }

    pub fn meta_mut(&mut self, id: BufferId) -> Option<&mut BufferMeta> {
        self.buffers.get_mut(id).map(|s| &mut s.meta)
    }

    /// Borrow a buffer out of the editor and call `f` with both.
    pub fn with_buffer<R>(&mut self, id: BufferId, f: impl FnOnce(&mut dyn Buffer, &mut Editor) -> R) -> Option<R> {
        let mut b = self.buffers.get_mut(id)?.buf.take()?;
        self.buffer_stack.push(id);
        let r = f(b.as_mut(), self);
        self.buffer_stack.pop();
        match self.buffers.get_mut(id) {
            Some(slot) => slot.buf = Some(b),
            None => b.on_kill(self),
        }
        Some(r)
    }

    /// The buffer whose code is running right now (a chat firing a hook, a buffer drawing).
    pub fn this_buffer(&self) -> Option<BufferId> {
        self.buffer_stack.last().copied()
    }

    /// Run `f` at the start of the next tick (when buffers that are busy now are free again).
    pub fn defer(&mut self, f: impl FnOnce(&mut Editor) + 'static) {
        self.deferred.push(Box::new(f));
        self.ctx.request_repaint();
    }

    /// Downcast access to a buffer of a concrete type.
    pub fn with_buffer_as<T: Buffer, R>(&mut self, id: BufferId, f: impl FnOnce(&mut T, &mut Editor) -> R) -> Option<R> {
        self.with_buffer(id, |b, ed| b.as_any_mut().downcast_mut::<T>().map(|t| f(t, ed))).flatten()
    }

    pub fn current_buffer(&self) -> BufferId {
        self.windows[self.focused].buffer
    }

    pub fn buffer_by_name(&self, name: &str) -> Option<BufferId> {
        self.buffers.iter().find(|(_, s)| s.meta.name == name).map(|(id, _)| id)
    }

    pub fn show_buffer(&mut self, id: BufferId) {
        let w = self.focused;
        let win = &mut self.windows[w];
        if win.buffer != id {
            let prev = win.buffer;
            win.history.retain(|b| *b != prev && *b != id);
            win.history.push(prev);
            win.buffer = id;
        }
        let payload = serde_json::json!({"buffer": buffer_id_to_u64(id)});
        self.run_hook("buffer-shown", &payload);
    }

    /// Show `id` in another window, splitting if there is only one.
    pub fn pop_to_buffer(&mut self, id: BufferId) {
        if let Some(w) = self.layout.visible_leaves().into_iter().find(|w| self.windows[*w].buffer == id) {
            self.focus_window(w);
            return;
        }
        if self.layout.leaves().len() == 1 {
            self.split_window(false);
        } else {
            self.other_window(1);
        }
        self.show_buffer(id);
    }

    pub fn kill_buffer(&mut self, id: BufferId) {
        if !self.buffers.contains_key(id) {
            return;
        }
        let payload = serde_json::json!({"buffer": buffer_id_to_u64(id)});
        self.run_hook("buffer-killed", &payload);
        self.chats.remove(&id);
        if let Some(mut slot) = self.buffers.remove(id) {
            if let Some(b) = slot.buf.as_mut() {
                b.on_kill(self);
            }
        }
        if self.buffers.is_empty() {
            self.add_buffer("*scratch*", Box::new(crate::buffers::text::TextBuffer::scratch()));
        }
        let fallback = self.buffers.keys().next().unwrap();
        let ids: Vec<WindowId> = self.windows.keys().collect();
        for w in ids {
            let win = &mut self.windows[w];
            win.history.retain(|b| *b != id);
            if win.buffer == id {
                win.buffer = win.history.pop().unwrap_or(fallback);
            }
        }
    }

    // ------------------------------------------------------------------
    // Windows
    // ------------------------------------------------------------------

    pub fn new_window(&mut self, buffer: BufferId) -> WindowId {
        self.windows.insert(Window {
            buffer,
            rect: Rect::NOTHING,
            history: Vec::new(),
        })
    }

    /// Focus `w` (in the current workspace) and drop any container selection.
    pub fn focus_window(&mut self, w: WindowId) {
        if self.layout.contains(w) {
            self.sidebar.focused = false;
            self.focused = w;
            self.selected = None;
            self.layout.note_focus(w);
            if self.layout.fullscreen.is_some_and(|f| f != w) {
                self.layout.fullscreen = Some(w);
            }
        }
    }

    /// What window commands act on: the selected container, else the focused window.
    pub fn target_node(&self) -> NodeRef {
        self.selected.filter(|r| self.layout.path(*r).is_some()).unwrap_or(NodeRef::Window(self.focused))
    }

    /// Split the focused window; the new window shows the same buffer and takes focus.
    pub fn split_window(&mut self, below: bool) -> WindowId {
        let w = self.new_window(self.current_buffer());
        self.layout.fullscreen = None;
        self.layout.split(self.focused, w, if below { Kind::SplitV } else { Kind::SplitH });
        self.focus_window(w);
        w
    }

    /// Close a window (a view; its buffer lives on). The last window can't be closed.
    pub fn delete_window(&mut self, w: WindowId) -> bool {
        if self.layout.leaves().len() <= 1 || !self.layout.contains(w) {
            return false;
        }
        let parent = self.layout.parent_of(NodeRef::Window(w));
        self.layout.remove(NodeRef::Window(w));
        self.windows.remove(w);
        if self.focused == w {
            // Focus lands where the closed window was, like i3.
            let next = parent
                .and_then(|p| self.layout.node(NodeRef::Container(p)))
                .and_then(Node::focus_leaf)
                .or_else(|| self.layout.root.children.get(self.layout.root.focus).and_then(Node::focus_leaf))
                .unwrap_or_else(|| self.layout.leaves()[0]);
            self.focus_window(next);
        }
        true
    }

    /// Remove a subtree (window or container) and delete its windows.
    pub fn delete_node(&mut self, r: NodeRef) -> bool {
        match r {
            NodeRef::Window(w) => self.delete_window(w),
            NodeRef::Container(_) => {
                let Some(node) = self.layout.node(r) else { return false };
                let mut ws = Vec::new();
                node.leaves(&mut ws);
                if ws.len() >= self.layout.leaves().len() {
                    return false;
                }
                self.layout.remove(r);
                for w in ws {
                    self.windows.remove(w);
                }
                self.selected = None;
                if !self.layout.contains(self.focused) {
                    let next = self.layout.root.children.get(self.layout.root.focus).and_then(Node::focus_leaf).unwrap_or_else(|| self.layout.leaves()[0]);
                    self.focus_window(next);
                }
                true
            }
        }
    }

    pub fn delete_other_windows(&mut self) {
        let keep = self.focused;
        for w in self.layout.leaves() {
            if w != keep {
                self.windows.remove(w);
            }
        }
        self.layout = LayoutTree::new(keep);
        self.selected = None;
    }

    pub fn other_window(&mut self, delta: isize) {
        let leaves = self.layout.visible_leaves();
        let i = leaves.iter().position(|w| *w == self.focused).unwrap_or(0) as isize;
        let n = leaves.len() as isize;
        let w = leaves[((i + delta) % n + n) as usize % n as usize];
        self.focus_window(w);
    }

    /// Focus the window in a direction (`left`, `right`, `up`, `down`), following the tree
    /// like i3: tabs are reached with left/right, stacked entries with up/down.
    pub fn windmove(&mut self, dir: &str) -> bool {
        let Some(d) = Dir::parse(dir) else { return false };
        let from = match self.selected {
            Some(r) => self.layout.node(r).and_then(Node::focus_leaf).unwrap_or(self.focused),
            None => self.focused,
        };
        match self.layout.focus_dir(from, d) {
            Some(w) => {
                self.focus_window(w);
                true
            }
            None => false,
        }
    }

    // ------------------------------------------------------------------
    // Window configurations
    // ------------------------------------------------------------------

    /// Snapshot of the current workspace's layout (tree, sizes, buffers, focus) as plain
    /// data, so it can be kept in variables, survive config reloads and be restored later.
    pub fn window_configuration(&self) -> Value {
        let leaves = self.layout.leaves();
        let tree = self.layout.to_json(&|w| serde_json::json!({"buffer": buffer_id_to_u64(self.windows[w].buffer)}));
        serde_json::json!({
            "tree": tree,
            "focused": leaves.iter().position(|w| *w == self.focused).unwrap_or(0),
            "fullscreen": self.layout.fullscreen.is_some(),
        })
    }

    /// The buffer named `*scratch*`, created if needed.
    pub fn scratch_buffer(&mut self) -> BufferId {
        match self.buffer_by_name("*scratch*") {
            Some(id) => id,
            None => self.add_buffer("*scratch*", Box::new(crate::buffers::text::TextBuffer::scratch())),
        }
    }

    /// Restore a configuration from [`Self::window_configuration`]. Buffers that no longer
    /// exist are replaced by `*scratch*`.
    pub fn set_window_configuration(&mut self, conf: &Value) -> anyhow::Result<()> {
        let tree = conf.get("tree").ok_or_else(|| anyhow::anyhow!("window configuration has no tree"))?;
        let scratch = self.scratch_buffer();
        // Build the new tree first so a malformed configuration leaves the layout intact.
        let mut created = Vec::new();
        let built = LayoutTree::from_json(tree, &mut |v| {
            let id = v.get("buffer").and_then(Value::as_u64).map(buffer_id_from_u64).filter(|id| self.buffers.contains_key(*id)).unwrap_or(scratch);
            let w = self.new_window(id);
            created.push(w);
            w
        });
        let mut layout = match built {
            Ok(t) => t,
            Err(e) => {
                for w in created {
                    self.windows.remove(w);
                }
                return Err(e);
            }
        };
        for w in self.layout.leaves() {
            self.windows.remove(w);
        }
        let leaves = layout.leaves();
        let f = conf.get("focused").and_then(Value::as_u64).unwrap_or(0) as usize;
        let focused = leaves[f.min(leaves.len() - 1)];
        if conf.get("fullscreen").and_then(Value::as_bool) == Some(true) {
            layout.fullscreen = Some(focused);
        }
        self.layout = layout;
        self.selected = None;
        self.focus_window(focused);
        self.run_hook("window-configuration-changed", &Value::Null);
        Ok(())
    }

    /// Buffers shown in windows, in layout order.
    pub fn visible_buffers(&self) -> Vec<BufferId> {
        self.layout.leaves().iter().map(|w| self.windows[*w].buffer).collect()
    }

    /// The directory a buffer works in (file directory, chat cwd, terminal pwd).
    pub fn buffer_directory(&self, id: BufferId) -> Option<std::path::PathBuf> {
        self.buffers.get(id)?.buf.as_ref()?.directory()
    }

    /// Run `f`, noting how long it took as part `name` of this frame (slow frames say where their
    /// time went). Parts under half a millisecond aren't noted.
    pub fn timed<R>(&mut self, name: impl FnOnce(&Editor) -> String, f: impl FnOnce(&mut Editor) -> R) -> R {
        let t = Instant::now();
        let r = f(self);
        let ms = t.elapsed().as_secs_f32() * 1000.0;
        if ms >= 0.5 {
            let n = name(self);
            self.frame_parts.push((n, ms));
        }
        r
    }

    /// Show `label` with a spinner until `busy_stop(key)`; a running `key` gets the new label.
    pub fn busy_start(&mut self, key: &str, label: &str) {
        match self.busy.iter_mut().find(|b| b.key == key) {
            Some(b) => b.label = label.to_string(),
            None => self.busy.push(Busy { key: key.to_string(), label: label.to_string(), since: Instant::now() }),
        }
        self.ctx.request_repaint();
    }

    pub fn busy_stop(&mut self, key: &str) {
        self.busy.retain(|b| b.key != key);
        self.ctx.request_repaint();
    }

    /// The machine a buffer's work runs on, when it isn't this one.
    pub fn buffer_machine(&self, id: BufferId) -> Option<String> {
        self.buffers.get(id)?.buf.as_ref()?.machine()
    }

    /// The directory a buffer works in, when it is on this machine: the folder of a chat on
    /// another machine is a path there, and looking it up here is wrong and can be slow (an
    /// automounted `/home`, a network volume).
    pub fn local_directory(&self, id: BufferId) -> Option<std::path::PathBuf> {
        if self.buffer_machine(id).is_some() {
            return None;
        }
        self.buffer_directory(id)
    }

    // ------------------------------------------------------------------
    // Text surface helpers
    // ------------------------------------------------------------------

    fn focused_surface_mut_raw(&mut self) -> Option<&mut dyn TextSurface> {
        let id = self.current_buffer();
        let b = self.buffers.get_mut(id)?.buf.as_mut()?;
        b.surface()
    }

    /// Run `f` on the focused buffer's text surface.
    pub fn with_surface<R>(&mut self, f: impl FnOnce(&mut dyn TextSurface, &mut Editor) -> R) -> Option<R> {
        let id = self.current_buffer();
        self.with_buffer(id, |b, ed| b.surface().map(|s| f(s, ed))).flatten()
    }

    pub fn cursor_shape(&self, id: BufferId) -> Option<CursorShape> {
        match self.meta(id)?.vars.get("cursor-shape")?.as_str()? {
            "block" => Some(CursorShape::Block),
            "bar" => Some(CursorShape::Bar),
            "underline" => Some(CursorShape::Underline),
            "hollow" => Some(CursorShape::Hollow),
            _ => None,
        }
    }

    /// Highlight `start..end` in a buffer for `duration`, fading out (`color` defaults to the theme accent).
    pub fn flash_region(&mut self, buffer: BufferId, start: crate::text::Pos, end: crate::text::Pos, duration: Duration, color: Option<egui::Color32>) {
        let color = color.unwrap_or(self.theme.yellow.gamma_multiply(0.45));
        self.flashes.retain(|f| f.buffer != buffer);
        self.flashes.push(Flash { buffer, start, end, born: Instant::now(), duration, color });
        self.ctx.request_repaint();
    }

    /// Announce that text was yanked (copied) from the focused buffer: runs `text-yanked`
    /// with `{buffer, start = {line, col}, end = {line, col}, linewise}` (1-based, end exclusive).
    pub fn notify_yank(&mut self, start: crate::text::Pos, end: crate::text::Pos, linewise: bool) {
        let id = self.current_buffer();
        let payload = serde_json::json!({
            "buffer": buffer_id_to_u64(id),
            "start": [start.line + 1, start.col + 1],
            "end": [end.line + 1, end.col + 1],
            "linewise": linewise,
        });
        self.run_hook("text-yanked", &payload);
    }

    pub fn copy_to_clipboard(&mut self, text: &str) {
        self.kill_ring.push(text.to_string());
        if self.kill_ring.len() > 60 {
            self.kill_ring.remove(0);
        }
        // Write through immediately so a paste in the same frame sees it.
        crate::clipboard::set(text);
    }

    /// Latest clipboard text (system clipboard first, then the kill ring).
    pub fn clipboard_text(&self) -> Option<String> {
        crate::clipboard::get().or_else(|| self.kill_ring.last().cloned())
    }

    // ------------------------------------------------------------------
    // Key dispatch
    // ------------------------------------------------------------------

    /// Keymaps consulted for the focused buffer, highest priority first.
    pub fn active_keymaps(&self) -> Vec<String> {
        let mut maps = Vec::new();
        if self.minibuffer.is_some() {
            maps.push("minibuffer".to_string());
            maps.push("global".to_string());
            return maps;
        }
        if self.sidebar.focused {
            maps.push("sidebar".to_string());
            maps.push("global".to_string());
            return maps;
        }
        if let Some(meta) = self.meta(self.current_buffer()) {
            for m in &meta.minor_modes {
                maps.push(format!("{m}:{}", meta.major));
                maps.push(m.clone());
            }
            maps.push(meta.major.clone());
        }
        maps.push("global".to_string());
        maps
    }

    fn lookup(&self, seq: &[KeyChord]) -> Lookup {
        for name in self.active_keymaps() {
            if let Some(map) = self.keymaps.get(&name) {
                match self.lookup_in(map, seq) {
                    Lookup::None => continue,
                    other => return other,
                }
            }
        }
        Lookup::None
    }

    /// `seq` with the `leader-key` chord written as `<leader>`, when it contains it.
    pub fn leader_form(&self, seq: &[KeyChord]) -> Option<Vec<KeyChord>> {
        let leader: KeyChord = self.var_str("leader-key").filter(|s| !s.is_empty())?.parse().ok()?;
        seq.contains(&leader).then(|| seq.iter().map(|c| if *c == leader { KeyChord::named("leader") } else { c.clone() }).collect())
    }

    /// Look `seq` up in one keymap: a binding of the literal keys wins, then one written
    /// with `<leader>`.
    pub fn lookup_in(&self, map: &crate::keymap::Keymap, seq: &[KeyChord]) -> Lookup {
        match map.lookup(seq) {
            Lookup::None => self.leader_form(seq).map(|s| map.lookup(&s)).unwrap_or(Lookup::None),
            other => other,
        }
    }

    /// Bindings available after the pending prefix (for which-key).
    pub fn pending_bindings(&self) -> Vec<(String, String)> {
        let mut seen = std::collections::BTreeMap::new();
        for name in self.active_keymaps().into_iter().rev() {
            let leader = self.leader_form(&self.pending_keys);
            if let Some(sub) = self.keymaps.get(&name).and_then(|m| m.submap(&self.pending_keys).or_else(|| leader.as_ref().and_then(|l| m.submap(l)))) {
                for (chord, b) in &sub.bindings {
                    let desc = match b {
                        crate::keymap::Binding::Command { name, .. } => name.clone(),
                        crate::keymap::Binding::Prefix(_) => "+prefix".to_string(),
                    };
                    seen.insert(chord.clone(), desc);
                }
            }
        }
        seen.into_iter().map(|(k, v)| (k.to_string(), v)).collect()
    }

    pub fn handle_chord(&mut self, chord: KeyChord) {
        // Jump labels take keys first, in any buffer (terminals included).
        if crate::jump::key(self, &chord) {
            return;
        }
        if self.describe_key_pending {
            self.pending_keys.push(chord);
            match self.lookup(&self.pending_keys.clone()) {
                Lookup::Prefix => return,
                Lookup::Command(c) => {
                    let doc = self.commands.get(&c).map(|c| c.doc.clone()).unwrap_or_default();
                    self.message(format!("{} runs {c}: {doc}", format_seq(&self.pending_keys)));
                }
                Lookup::None => self.message(format!("{} is undefined", format_seq(&self.pending_keys))),
            }
            self.pending_keys.clear();
            self.describe_key_pending = false;
            return;
        }
        let mut seq = self.pending_keys.clone();
        seq.push(chord.clone());
        match self.lookup(&seq) {
            Lookup::Command(cmd) => {
                self.pending_keys.clear();
                self.pending_since = None;
                let args = Args {
                    prefix: self.prefix_arg.take(),
                    keys: seq,
                    params: Vec::new(),
                };
                if let Err(e) = self.run_command(&cmd, &args) {
                    self.error(format!("{cmd}: {e:#}"));
                }
                return;
            }
            Lookup::Prefix => {
                self.pending_keys = seq;
                self.pending_since = Some(Instant::now());
                return;
            }
            Lookup::None => {}
        }
        if !self.pending_keys.is_empty() {
            // In terminals a prefix like C-c means something to the program too: an unbound
            // continuation sends both keys there instead of being dropped.
            if self.prefix_passthrough() {
                self.flush_pending_to_buffer(Some(chord));
                return;
            }
            let s = format_seq(&seq);
            self.pending_keys.clear();
            self.pending_since = None;
            self.message(format!("{s} is undefined"));
            return;
        }
        if self.minibuffer.is_some() {
            crate::minibuffer::self_insert(self, &chord);
            return;
        }
        if self.sidebar.focused {
            crate::sidebar::self_insert(self, &chord);
            return;
        }
        for i in self.interceptors.clone() {
            if (i.f)(self, &chord) == KeyResult::Handled {
                crate::packages::vim::flush_replay(self);
                return;
            }
        }
        let id = self.current_buffer();
        let handled = self.with_buffer(id, |b, ed| b.handle_key(&chord, ed)).unwrap_or(false);
        if !handled {
            if let Some(c) = chord.printable() {
                self.self_insert(&c.to_string());
            }
        }
        crate::packages::vim::flush_replay(self);
    }

    /// Whether the focused buffer's major mode is in `prefix-passthrough-modes`.
    pub fn prefix_passthrough(&self) -> bool {
        let Some(m) = self.meta(self.current_buffer()) else { return false };
        self.minibuffer.is_none()
            && !self.sidebar.focused
            && self.var("prefix-passthrough-modes").and_then(Value::as_array).is_some_and(|a| a.iter().any(|v| v.as_str() == Some(m.major.as_str())))
    }

    /// Hand the pending prefix keys (and `then`) to the focused buffer as ordinary input.
    pub fn flush_pending_to_buffer(&mut self, then: Option<KeyChord>) {
        let mut keys = std::mem::take(&mut self.pending_keys);
        keys.extend(then);
        self.pending_since = None;
        let id = self.current_buffer();
        for k in keys {
            self.with_buffer(id, |b, ed| b.handle_key(&k, ed));
        }
    }

    /// Typed text that reached the buffer.
    pub fn self_insert(&mut self, text: &str) {
        if self.minibuffer.is_some() {
            crate::minibuffer::insert_text(self, text);
            return;
        }
        if self.sidebar.focused {
            crate::sidebar::insert_text(self, text);
            return;
        }
        let id = self.current_buffer();
        self.with_buffer(id, |b, ed| b.handle_text(text, ed));
    }

    /// Text input from the platform: each character is dispatched as a chord so keymaps
    /// (and vim's normal state) see it before it is inserted.
    pub fn handle_text_input(&mut self, text: &str) {
        for c in text.chars() {
            self.handle_chord(KeyChord::ch(c));
        }
    }

    pub fn handle_paste(&mut self, text: &str) {
        if self.minibuffer.is_some() {
            crate::minibuffer::insert_text(self, text);
            return;
        }
        if self.sidebar.focused {
            crate::sidebar::insert_text(self, text);
            return;
        }
        let id = self.current_buffer();
        self.with_buffer(id, |b, ed| b.handle_paste(text, ed));
    }

    /// Files dropped or pasted: the minibuffer gets their paths, a buffer decides (a chat
    /// attaches images, a terminal pastes paths).
    pub fn handle_files(&mut self, paths: Vec<std::path::PathBuf>) {
        if paths.is_empty() {
            return;
        }
        if self.minibuffer.is_some() || self.sidebar.focused {
            let text = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect::<Vec<_>>().join(" ");
            self.handle_paste(&text);
            return;
        }
        let id = self.current_buffer();
        self.with_buffer(id, |b, ed| b.handle_files(&paths, ed));
    }

    /// Paste an image (PNG bytes): saved under the temp dir, then handled as a file, so
    /// terminal programs (Claude Code, pi) get a path and chats an attachment.
    pub fn paste_image(&mut self, png: &[u8]) {
        let dir = std::env::temp_dir().join("serpis-pastes");
        let _ = std::fs::create_dir_all(&dir);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0);
        let path = dir.join(format!("pasted-{stamp}.png"));
        match std::fs::write(&path, png) {
            Ok(()) => self.handle_files(vec![path]),
            Err(e) => self.error(format!("Couldn't save the pasted image: {e}")),
        }
    }

    /// Files dropped on the window go to the window under the pointer.
    pub fn drop_files(&mut self, paths: Vec<std::path::PathBuf>, at: Option<egui::Pos2>) {
        if let Some(p) = at
            && let Some(w) = self.layout.leaves().into_iter().find(|w| self.windows.get(*w).is_some_and(|win| win.rect.contains(p)))
        {
            self.focus_window(w);
        }
        self.handle_files(paths);
    }

    pub fn keyboard_quit(&mut self) {
        self.pending_keys.clear();
        self.pending_since = None;
        self.prefix_arg = None;
        self.describe_key_pending = false;
        self.overlays.clear();
        if self.minibuffer.is_some() {
            crate::minibuffer::cancel(self);
            return;
        }
        self.with_surface(|s, _| s.set_anchor(None));
        self.run_hook("keyboard-quit", &Value::Null);
    }

    // ------------------------------------------------------------------
    // Frame
    // ------------------------------------------------------------------

    /// Per-frame work outside painting: queued tasks and buffer polling.
    pub fn tick(&mut self) {
        self.timed(|_| "queued work".into(), |ed| {
            while let Ok(task) = ed.main_rx.try_recv() {
                task(ed);
            }
        });
        self.timed(|_| "deferred work".into(), |ed| {
            for f in std::mem::take(&mut ed.deferred) {
                f(ed);
            }
        });
        let ids: Vec<BufferId> = self.buffers.keys().collect();
        for id in ids {
            self.timed(|ed| format!("poll {}", ed.buffer_display_name(id)), |ed| ed.with_buffer(id, |b, ed| b.poll(ed)));
        }
        // A lone prefix in a passthrough buffer (C-c in a terminal) reaches the program after
        // `prefix-passthrough-timeout` seconds.
        if let Some(since) = self.pending_since.filter(|_| !self.pending_keys.is_empty() && self.prefix_passthrough()) {
            let t = self.var("prefix-passthrough-timeout").and_then(Value::as_f64).unwrap_or(0.5);
            if t > 0.0 {
                let wait = Duration::from_secs_f64(t);
                match wait.checked_sub(since.elapsed()) {
                    Some(left) => self.ctx.request_repaint_after(left),
                    None => self.flush_pending_to_buffer(None),
                }
            }
        }
        self.timed(|_| "sidebar tracking".into(), crate::sidebar::track);
        self.timed(|_| "config reload check".into(), crate::lua::poll_reload);
        crate::minibuffer::notify_selection(self);
        // Report layout changes once per frame with the previous layout (winner-style undo).
        // Prompts preview layouts, so changes are only recorded outside the minibuffer.
        if self.minibuffer.is_none() {
            let now = self.window_configuration();
            match self.last_layout.replace(now.clone()) {
                Some(prev) if prev != now => {
                    self.run_hook("window-layout-changed", &serde_json::json!({"previous": prev, "current": now}));
                }
                _ => {}
            }
        }
        let segs = self.status_segments.clone();
        self.status_cache = segs.iter().filter_map(|s| (s.f)(self, None)).filter(|t| !t.is_empty()).collect();
        crate::plugins::poll_reload(self);
        crate::remote::tick(self);
        crate::terminals::tick(self);
        if let Some((_, _, t)) = &self.echo {
            if t.elapsed() > Duration::from_secs(8) && self.minibuffer.is_none() {
                self.echo = None;
            }
        }
    }

    pub fn keymap(&self, name: &str) -> Option<&Keymap> {
        self.keymaps.get(name)
    }
}

/// Stable numeric form of a buffer id for Lua and JSON payloads.
pub fn buffer_id_to_u64(id: BufferId) -> u64 {
    use slotmap::Key;
    id.data().as_ffi()
}

/// Stable numeric form of a window id for Lua and JSON payloads.
pub fn window_id_to_u64(id: WindowId) -> u64 {
    use slotmap::Key;
    id.data().as_ffi()
}

pub fn window_id_from_u64(v: u64) -> WindowId {
    slotmap::KeyData::from_ffi(v).into()
}

pub fn buffer_id_from_u64(v: u64) -> BufferId {
    slotmap::KeyData::from_ffi(v).into()
}
