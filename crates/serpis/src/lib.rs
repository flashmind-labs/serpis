//! # Serpis
//!
//! An extensible agent harness shaped like Emacs. This crate is the whole editor core and is
//! built as a Rust `dylib`, so Rust plugins link the very same library and operate on the
//! live [`Editor`]. Lua configuration uses the same API through the `serpis` table.
//!
//! Re-exports (`egui`, `mlua`, `ai`, `agent`, ...) give plugins the exact crate instances the
//! editor uses.

pub mod app;
pub mod buffer;
pub mod buffers;
pub mod clipboard;
pub mod commands;
pub mod editor;
pub mod fonts;
pub mod git;
pub mod keymap;
pub mod layout;
pub mod workspace;
pub mod wm;
pub mod keys;
pub mod lua;
pub mod minibuffer;
pub mod motion;
pub mod mouse;
pub mod packages;
pub mod plugins;
pub mod project;
pub mod sidebar;
pub mod text;
pub mod theme;
pub mod themes;
pub mod ui;

pub use anyhow;
pub use buffer::{Activity, ActivityState, Buffer, BufferId, Modeline, ProcessStatus, WindowId};
pub use eframe;
pub use editor::{Args, Editor, KeyResult, MainSender};
pub use egui;
pub use keys::KeyChord;
pub use mlua;
pub use serde_json;
pub use serpis_agent as agent;
pub use serpis_ai as ai;
pub use text::{Pos, TextSurface};
pub use tokio;

use std::path::PathBuf;

/// `$SERPIS_CONFIG_DIR`, else `~/.config/serpis`.
pub fn config_dir() -> PathBuf {
    std::env::var_os("SERPIS_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| dirs::home_dir().unwrap_or_default().join(".config").join("serpis"))
}

pub fn expand_home(p: &str) -> PathBuf {
    match p.strip_prefix("~/") {
        Some(rest) => dirs::home_dir().unwrap_or_default().join(rest),
        None if p == "~" => dirs::home_dir().unwrap_or_default(),
        None => PathBuf::from(p),
    }
}

impl Editor {
    /// Whether `chord` (after any pending prefix) is bound in an active keymap.
    pub fn is_bound(&self, chord: &KeyChord) -> bool {
        let mut seq = self.pending_keys.clone();
        seq.push(chord.clone());
        self.active_keymaps().iter().any(|n| {
            self.keymaps
                .get(n)
                .is_some_and(|m| !matches!(self.lookup_in(m, &seq), keymap::Lookup::None))
        })
    }
}

/// Register built-in commands, buffer kinds and packages, then load the user's config.
pub fn bootstrap(ed: &mut Editor) {
    use serde_json::{Value, json};
    let o = editor::BUILTIN;
    ed.defvar("keybinding-scheme", json!("regular"), "Active keybinding scheme: regular, emacs or vim.");
    ed.defvar("prefix-passthrough-modes", json!(["terminal"]), "Major modes whose prefix keys also belong to the program: an unbound continuation (C-c q) is sent to it, and a lone prefix after prefix-passthrough-timeout.");
    ed.defvar("prefix-passthrough-timeout", json!(0.0), "Seconds before a lone prefix key in a passthrough buffer is sent to the program (0: wait for the next key, like Emacs; C-c C-c sends C-c).");
    ed.defvar("terminal-escape", json!("auto"), "Where ESC goes in a terminal in vim insert state: \"auto\" (the program when it is full-screen, else vim normal state), \"terminal\" or \"serpis\". C-c C-z cycles it per terminal.");
    ed.defvar("leader-key", json!("SPC"), "The key `<leader>` stands for in key bindings (vim normal state uses `<leader> b b`, ...). \"\" turns leader bindings off.");
    ed.defvar("agent-model", Value::Null, "Default model for new chats, as provider/model-id.");
    ed.defvar("agent-thinking", Value::Null, "Default thinking level: off, minimal, low, medium, high, xhigh, max.");
    ed.defvar("agent-native-web-search", json!(true), "Use the provider's own web search when the model supports it.");
    ed.defvar("agent-disabled-tools", json!([]), "Tool names hidden from the agent.");
    ed.defvar("agent-guidelines", json!([]), "Extra rule bullets for the system prompt.");
    ed.defvar("agent-append-system-prompt", Value::Null, "Text appended to the system prompt.");
    ed.defvar("plugin-paths", json!([]), "Extra Rust plugin library paths.");
    ed.defvar("terminal-shell-integration", json!(true), "Load serpis's zsh integration in new terminals (OSC 133 prompt marks).");
    ed.defvar("terminal-prompt-regexp", json!(buffers::terminal::DEFAULT_PROMPT_REGEXP), "Prompt prefix regexp used when the shell doesn't mark prompts (OSC 133).");

    minibuffer::install(ed);
    commands::install(ed);
    wm::install(ed);
    workspace::install(ed);
    mouse::install(ed);
    sidebar::install(ed);
    ed.define_buffer_kind("chat", o, |ed, p| Ok(Box::new(buffers::chat::ChatBuffer::new(ed, p))));
    ed.define_buffer_kind("terminal", o, |ed, p| Ok(Box::new(buffers::terminal::TerminalBuffer::new(ed, p)?)));
    ed.define_buffer_kind("text", o, |_, p| {
        Ok(Box::new(buffers::text::TextBuffer::new(p.get("text").and_then(Value::as_str).unwrap_or(""))))
    });
    packages::install_base(ed);
    packages::set_scheme(ed, "regular");
    fonts::install(ed);
    // Last interactively chosen theme; init.lua may override it.
    if let Some(t) = Editor::persisted_theme() {
        if let Err(e) = ed.load_theme(&t) {
            ed.warn(format!("{e:#}"));
        }
    }

    // User configuration (may switch the scheme, bind keys, define tools).
    lua::reload(ed);
    plugins::reload_all(ed);
    fonts::apply(ed);
    fonts::reset_sizes(ed);
    for e in ed.registry.load_errors() {
        ed.warn(e);
    }

    // Model catalogs refresh in the background.
    let reg = ed.registry.clone();
    let main = ed.main_sender();
    ed.rt.spawn(async move {
        let errs = reg.refresh_catalogs(false).await;
        main.send(move |ed| {
            if !errs.is_empty() {
                ed.warn(format!("model catalog: {}", errs.join("; ")));
            }
        });
    });

    // Start with a chat in the main window.
    if let Ok(id) = ed.create_buffer("chat", Some("*chat*"), &Value::Null) {
        ed.show_buffer(id);
    }
    if let Some(script) = std::env::var_os("SERPIS_TEST_SCRIPT") {
        lua::run_file(ed, std::path::Path::new(&script));
    }
}

/// Run serpis.
pub fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "warn,serpis=info".into()))
        .init();
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().thread_name("serpis-rt").build()?;
    let handle = rt.handle().clone();
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("serpis")
            .with_inner_size([1280.0, 820.0])
            .with_min_inner_size([480.0, 320.0]),
        // winit's default macOS menu claims Cmd-H (hide), Cmd-Alt-H and Cmd-Q before the
        // app sees them; without it every Cmd chord goes through serpis keymaps (`s-q` quits).
        #[cfg(target_os = "macos")]
        event_loop_builder: Some(Box::new(|b| {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            b.with_default_menu(false);
        })),
        ..Default::default()
    };
    eframe::run_native(
        "serpis",
        options,
        Box::new(move |cc| {
            let mut ed = Editor::new(cc.egui_ctx.clone(), handle);
            ed.theme.apply(&cc.egui_ctx);
            bootstrap(&mut ed);
            Ok(Box::new(app::App::new(ed, rt)))
        }),
    )
    .map_err(|e| anyhow::anyhow!("{e}"))
}
