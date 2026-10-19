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

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Layout {
    Leaf(WindowId),
    Split { vertical: bool, ratio: f32, a: usize, b: usize },
}

/// Window layout as an arena-backed binary tree (`vertical` = stacked top/bottom).
#[derive(Debug, Clone)]
pub struct LayoutTree {
    pub nodes: Vec<Layout>,
    pub root: usize,
}

impl LayoutTree {
    fn new(w: WindowId) -> Self {
        LayoutTree {
            nodes: vec![Layout::Leaf(w)],
            root: 0,
        }
    }
    fn find_leaf(&self, w: WindowId) -> Option<usize> {
        self.nodes.iter().position(|n| *n == Layout::Leaf(w))
    }
    fn parent(&self, idx: usize) -> Option<usize> {
        self.nodes.iter().position(|n| matches!(n, Layout::Split { a, b, .. } if *a == idx || *b == idx))
    }
    pub fn leaves(&self) -> Vec<WindowId> {
        let mut out = Vec::new();
        self.collect(self.root, &mut out);
        out
    }
    fn collect(&self, i: usize, out: &mut Vec<WindowId>) {
        match self.nodes[i] {
            Layout::Leaf(w) => out.push(w),
            Layout::Split { a, b, .. } => {
                self.collect(a, out);
                self.collect(b, out);
            }
        }
    }
    fn split(&mut self, target: WindowId, new: WindowId, vertical: bool) {
        let Some(i) = self.find_leaf(target) else { return };
        let a = self.nodes.len();
        self.nodes.push(Layout::Leaf(target));
        let b = self.nodes.len();
        self.nodes.push(Layout::Leaf(new));
        self.nodes[i] = Layout::Split { vertical, ratio: 0.5, a, b };
    }
    fn remove(&mut self, target: WindowId) -> bool {
        let Some(i) = self.find_leaf(target) else { return false };
        let Some(p) = self.parent(i) else { return false };
        let Layout::Split { a, b, .. } = self.nodes[p] else { return false };
        let sibling = if a == i { b } else { a };
        self.nodes[p] = self.nodes[sibling];
        self.compact();
        true
    }
    /// Rebuild the arena with only nodes reachable from the root.
    fn compact(&mut self) {
        fn copy(src: &[Layout], i: usize, out: &mut Vec<Layout>) -> usize {
            let idx = out.len();
            out.push(src[i]);
            if let Layout::Split { vertical, ratio, a, b } = src[i] {
                let na = copy(src, a, out);
                let nb = copy(src, b, out);
                out[idx] = Layout::Split { vertical, ratio, a: na, b: nb };
            }
            idx
        }
        let mut out = Vec::new();
        copy(&self.nodes, self.root, &mut out);
        self.nodes = out;
        self.root = 0;
    }
    /// Resize the split containing `w` along `vertical` by `delta` (fraction).
    pub fn resize(&mut self, w: WindowId, vertical: bool, delta: f32) {
        let Some(mut i) = self.find_leaf(w) else { return };
        while let Some(p) = self.parent(i) {
            if let Layout::Split { vertical: v, ratio, a, .. } = &mut self.nodes[p] {
                if *v == vertical {
                    let d = if *a == i { delta } else { -delta };
                    *ratio = (*ratio + d).clamp(0.1, 0.9);
                    return;
                }
            }
            i = p;
        }
    }
    pub fn balance(&mut self) {
        for n in &mut self.nodes {
            if let Layout::Split { ratio, .. } = n {
                *ratio = 0.5;
            }
        }
    }
}

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
    pub layout: LayoutTree,
    pub focused: WindowId,
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
    last_layout: Option<Value>,
    /// Fading highlights drawn over buffers.
    pub flashes: Vec<Flash>,
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
        tracing::info!("{text}");
        self.messages.push(text.clone());
        self.echo = Some((text, Level::Info, Instant::now()));
    }

    pub fn warn(&mut self, text: impl Into<String>) {
        let text = text.into();
        tracing::warn!("{text}");
        self.messages.push(format!("warning: {text}"));
        self.echo = Some((text, Level::Warn, Instant::now()));
    }

    pub fn error(&mut self, text: impl Into<String>) {
        let text = text.into();
        tracing::error!("{text}");
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
        self.agent_tools.retain(|(o, _)| o != owner);
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
        let r = f(b.as_mut(), self);
        match self.buffers.get_mut(id) {
            Some(slot) => slot.buf = Some(b),
            None => b.on_kill(self),
        }
        Some(r)
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
        if let Some((w, _)) = self.windows.iter().find(|(_, w)| w.buffer == id) {
            self.focused = w;
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

    pub fn split_window(&mut self, below: bool) -> WindowId {
        let buffer = self.current_buffer();
        let w = self.windows.insert(Window {
            buffer,
            rect: Rect::NOTHING,
            history: Vec::new(),
        });
        self.layout.split(self.focused, w, below);
        self.focused = w;
        w
    }

    pub fn delete_window(&mut self, w: WindowId) -> bool {
        if self.layout.leaves().len() <= 1 {
            return false;
        }
        if !self.layout.remove(w) {
            return false;
        }
        self.windows.remove(w);
        if self.focused == w {
            self.focused = self.layout.leaves()[0];
        }
        true
    }

    pub fn delete_other_windows(&mut self) {
        let keep = self.focused;
        for w in self.layout.leaves() {
            if w != keep {
                self.windows.remove(w);
            }
        }
        self.layout = LayoutTree::new(keep);
    }

    pub fn other_window(&mut self, delta: isize) {
        let leaves = self.layout.leaves();
        let i = leaves.iter().position(|w| *w == self.focused).unwrap_or(0) as isize;
        let n = leaves.len() as isize;
        self.focused = leaves[((i + delta) % n + n) as usize % n as usize];
    }

    /// Focus the nearest window in a direction (`left`, `right`, `up`, `down`).
    pub fn windmove(&mut self, dir: &str) -> bool {
        let cur = self.windows[self.focused].rect;
        let c = cur.center();
        let best = self
            .layout
            .leaves()
            .into_iter()
            .filter(|w| *w != self.focused)
            .filter_map(|w| {
                let r = self.windows[w].rect;
                let ok = match dir {
                    "left" => r.right() <= cur.left() + 1.0 && r.top() < cur.bottom() && r.bottom() > cur.top(),
                    "right" => r.left() >= cur.right() - 1.0 && r.top() < cur.bottom() && r.bottom() > cur.top(),
                    "up" => r.bottom() <= cur.top() + 1.0 && r.left() < cur.right() && r.right() > cur.left(),
                    _ => r.top() >= cur.bottom() - 1.0 && r.left() < cur.right() && r.right() > cur.left(),
                };
                ok.then(|| (w, r.center().distance(c)))
            })
            .min_by(|a, b| a.1.total_cmp(&b.1));
        if let Some((w, _)) = best {
            self.focused = w;
            true
        } else {
            false
        }
    }

    // ------------------------------------------------------------------
    // Window configurations
    // ------------------------------------------------------------------

    /// Snapshot of the window layout (tree, ratios, buffers, focus) as plain data, so it can
    /// be kept in variables, survive config reloads and be restored later.
    pub fn window_configuration(&self) -> Value {
        let leaves = self.layout.leaves();
        fn node(ed: &Editor, i: usize) -> Value {
            match ed.layout.nodes[i] {
                Layout::Leaf(w) => serde_json::json!({"buffer": buffer_id_to_u64(ed.windows[w].buffer)}),
                Layout::Split { vertical, ratio, a, b } => serde_json::json!({
                    "split": if vertical { "below" } else { "right" },
                    "ratio": ratio,
                    "a": node(ed, a),
                    "b": node(ed, b),
                }),
            }
        }
        serde_json::json!({
            "tree": node(self, self.layout.root),
            "focused": leaves.iter().position(|w| *w == self.focused).unwrap_or(0),
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
        fn build(ed: &mut Editor, v: &Value, nodes: &mut Vec<Layout>, fallback: BufferId) -> anyhow::Result<usize> {
            if let Some(b) = v.get("buffer").and_then(Value::as_u64) {
                let id = buffer_id_from_u64(b);
                let buffer = if ed.buffers.contains_key(id) { id } else { fallback };
                let w = ed.windows.insert(Window { buffer, rect: Rect::NOTHING, history: Vec::new() });
                nodes.push(Layout::Leaf(w));
                return Ok(nodes.len() - 1);
            }
            let vertical = v.get("split").and_then(Value::as_str) == Some("below");
            let ratio = v.get("ratio").and_then(Value::as_f64).unwrap_or(0.5) as f32;
            let idx = nodes.len();
            nodes.push(Layout::Split { vertical, ratio, a: 0, b: 0 });
            let a = build(ed, v.get("a").ok_or_else(|| anyhow::anyhow!("split without a"))?, nodes, fallback)?;
            let b = build(ed, v.get("b").ok_or_else(|| anyhow::anyhow!("split without b"))?, nodes, fallback)?;
            nodes[idx] = Layout::Split { vertical, ratio, a, b };
            Ok(idx)
        }
        let old: Vec<WindowId> = self.layout.leaves();
        let mut nodes = Vec::new();
        if let Err(e) = build(self, tree, &mut nodes, scratch) {
            for n in &nodes {
                if let Layout::Leaf(w) = n {
                    self.windows.remove(*w);
                }
            }
            return Err(e);
        }
        // Keep each window's buffer history when the same buffer stays visible.
        for w in &old {
            self.windows.remove(*w);
        }
        self.layout = LayoutTree { nodes, root: 0 };
        let leaves = self.layout.leaves();
        let f = conf.get("focused").and_then(Value::as_u64).unwrap_or(0) as usize;
        self.focused = leaves[f.min(leaves.len() - 1)];
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
                match map.lookup(seq) {
                    Lookup::None => continue,
                    other => return other,
                }
            }
        }
        Lookup::None
    }

    /// Bindings available after the pending prefix (for which-key).
    pub fn pending_bindings(&self) -> Vec<(String, String)> {
        let mut seen = std::collections::BTreeMap::new();
        for name in self.active_keymaps().into_iter().rev() {
            if let Some(sub) = self.keymaps.get(&name).and_then(|m| m.submap(&self.pending_keys)) {
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

    /// Typed text that reached the buffer.
    pub fn self_insert(&mut self, text: &str) {
        if self.minibuffer.is_some() {
            crate::minibuffer::insert_text(self, text);
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
        let id = self.current_buffer();
        self.with_buffer(id, |b, ed| b.handle_paste(text, ed));
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
        while let Ok(task) = self.main_rx.try_recv() {
            task(self);
        }
        let ids: Vec<BufferId> = self.buffers.keys().collect();
        for id in ids {
            self.with_buffer(id, |b, ed| b.poll(ed));
        }
        crate::lua::poll_reload(self);
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

pub fn buffer_id_from_u64(v: u64) -> BufferId {
    slotmap::KeyData::from_ffi(v).into()
}
