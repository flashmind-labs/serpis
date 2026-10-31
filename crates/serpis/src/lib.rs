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
pub use serpis_chat::cache_watch;
pub mod chat_api;
pub mod claude_usage;
pub mod codex_status;
pub mod log_view;
pub mod notify;
pub mod extend;
pub mod frecency;
pub mod highlight;
pub mod images;
pub mod launch;
pub mod chrome;
pub mod clipboard;
pub mod commands;
pub mod editor;
pub mod fonts;
pub mod git;
pub mod jump;
pub mod keymap;
pub mod layout;
pub mod machines;
pub mod pairing;
pub mod workspace;
pub mod wm;
pub mod keys;
pub mod lua;
pub mod minibuffer;
pub mod motion;
pub mod mouse;
pub mod packages;
pub mod pickers;
pub mod remote;
pub mod terminals;
pub mod plugins;
pub mod project;
pub use serpis_chat::session;
pub mod session_ui;
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

pub use serpis_chat::config::config_dir;

/// `s` as one shell word: unchanged when safe, else single-quoted.
pub fn shell_quote(s: &str) -> String {
    if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "/._-+,:@%=~".contains(c)) {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
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
    ed.defvar("config-auto-reload", json!(false), "Reload init.lua, the Lua extensions and Rust plugins when their files change (watching the config folder). Off: they reload on reload-config or reload-extensions only.");
    ed.defvar("perf-slow-frame-ms", json!(100.0), "Frames slower than this many milliseconds are logged with where their time went (serpis.debug.slow_frames()).");
    ed.defvar("busy-indicator", json!(true), "Show work in progress (connecting to a machine, scripts' serpis.busy.start) as a card with a spinner above the echo area.");
    ed.defvar("minibuffer-style", json!("palette"), "Where prompts appear: \"palette\" (a floating card near the top, like a command palette) or \"bottom\" (the echo area, Emacs style).");
    ed.defvar("chat-show-thinking", json!(true), "Show the model's thinking in chats; when false only its header (\"Thought · 12 lines\") is shown. chat-toggle-thinking flips it.");
    ed.defvar("chat-show-usage", json!(true), "Show the session's usage in the chat's composer, like pi's footer: ↑input ↓output Rcache-read Wcache-write $cost context%/window.");
    ed.defvar("scroll-animation-time", json!(0.2), "Seconds views take to glide where keyboard motions scroll them (j/k past the edge, page keys). Holding a key glides at a steady speed; 0 jumps.");
    ed.add_hook("variable-changed", editor::BUILTIN, |ed, p| {
        if p.get("name").and_then(Value::as_str) == Some("scroll-animation-time") {
            text::set_scroll_time(ed.var("scroll-animation-time").and_then(Value::as_f64).unwrap_or(0.2) as f32);
        }
        None
    });
    ed.defvar("minibuffer-frecency", json!(true), "Rank prompt choices (M-x, buffers, Lua pickers with `frecency`) by how often and how recently you picked them.");
    ed.defvar("minibuffer-frecency-half-life", json!(7.0), "Days after which a pick counts half as much for frecency.");
    ed.defvar("theme-accent", json!("theme"), "Accent color: \"theme\" (the theme's own), one of its palette colors (blue, purple, cyan, green, yellow, orange, red), so it suits any theme, or \"#rrggbb\". select-accent picks one with a live preview.");
    ed.add_hook("variable-changed", editor::BUILTIN, |ed, p| {
        if p.get("name").and_then(Value::as_str) == Some("theme-accent") {
            let name = ed.theme.name.clone();
            if let Err(e) = ed.load_theme(&name) {
                ed.warn(format!("{e:#}"));
            }
        }
        None
    });
    ed.defvar("chat-max-width", json!(860.0), "Width of a chat's reading column in points at the default font size; wider windows center it.");
    ed.defvar("agent-model", Value::Null, "Default model for new chats, as provider/model-id.");
    ed.defvar("agent-thinking", Value::Null, "Default thinking level: off, minimal, low, medium, high, xhigh, max.");
    ed.defvar("agent-native-web-search", json!(true), "Use the provider's own web search when the model supports it.");
    ed.defvar("agent-disabled-tools", json!([]), "Tool names hidden from the agent.");
    ed.defvar("agent-guidelines", json!([]), "Extra rule bullets for the system prompt.");
    ed.defvar("agent-cache-retention", json!("long"), "How long providers keep a chat's prompt cached: \"long\" (1 hour on Anthropic-style caches, 24 hours where OpenAI-style caches allow it), \"short\" (5 minutes) or \"none\". Or per provider: {\"default\": \"long\", \"openrouter\": \"short\"}. 1-hour writes cost 2x input instead of 1.25x, but only for new tokens; a short cache expires during pauses and the next message rewrites the whole conversation.");
    ed.defvar("chat-cache-indicator", json!(true), "Show how each response used the prompt cache (\"cache 96%\", \"cache miss\", \"cache new\") after its token counts.");
    ed.defvar("chat-cache-status", json!(true), "Show the chat's cache hit rate and whether its cache is warm or cold in its window header; clicking it runs chat-cache-report.");
    ed.defvar("chat-cache-miss-notice", json!("inline"), "Announce a prompt cache miss and its cause: \"inline\" (a line in the chat), \"message\" (the echo area) or \"off\". The cache-miss hook runs either way.");
    ed.defvar("cache-warn", json!("notice"), "Before an action that throws a warm prompt cache away (see cache-warn-actions): \"confirm\" asks first, \"notice\" says what it costs after, \"off\" stays quiet. Sending after the cache expired and tool changes only ever get a notice.");
    ed.defvar("cache-warn-threshold", json!(0.10), "Warn (cache-warn) only when re-caching would cost at least this many dollars.");
    ed.defvar("cache-warn-actions", json!(commands::CACHE_WARN_ACTIONS), "Actions cache-warn warns about: model, thinking, tools, refresh (chat-refresh-context), expired (sending after the cache expired).");
    ed.defvar("chat-resume-system-prompt", json!("warm"), "What a resumed or restored chat sends as its system prompt: \"warm\" keeps the one it was sent with while its prompt cache can still be alive (otherwise it is rebuilt from the current AGENTS.md / CLAUDE.md and settings, as keeping it would save nothing), \"always\" keeps it, \"never\" rebuilds it.");
    ed.defvar("chat-resume-system-prompt-notice", json!(true), "Say so when a resumed chat keeps its system prompt although the project's instructions or prompt settings changed since (chat-refresh-context picks them up).");
    ed.defvar("chat-compact-auto", json!(true), "Compact a chat when its context nears the model's window (leaving chat-compact-reserve-tokens free), and after a context-overflow error (then retrying the turn once). The summary request reads the prompt cache.");
    ed.defvar("chat-compact-reserve-tokens", json!(16384), "Tokens of the context window compaction keeps free for the next response; the summary may use up to 80% of them.");
    ed.defvar("chat-compact-keep-tokens", json!(20000), "Recent tokens a compaction keeps as they are (the rest becomes the summary).");
    ed.defvar("chat-compact-instructions", Value::Null, "What compaction summaries should focus on, besides pi's checkpoint format (goal, progress, decisions, next steps).");
    ed.defvar("agent-live-context", json!(false), "Rebuild the system prompt (AGENTS.md / CLAUDE.md, SYSTEM.md / APPEND_SYSTEM.md, agent-guidelines, agent-append-system-prompt) for every message instead of keeping the one a chat started with. Any change then rewrites the chat's whole prompt cache; chat-refresh-context picks up changes once.");
    ed.defvar("agent-append-system-prompt", Value::Null, "Text appended to the system prompt (after APPEND_SYSTEM.md).");
    ed.defvar("agent-system-prompt-files", json!(true), "Read SYSTEM.md (used in place of serpis's own system prompt) and APPEND_SYSTEM.md (added to it) from the config directory. Project instructions (AGENTS.md / CLAUDE.md) still follow either.");
    ed.defvar("claude-code-path", Value::Null, "The claude CLI that chats on claude-code/* models run (their turns go through Claude Code: its harness, tools, permissions and login, a Claude subscription included). Unset: claude on PATH, ~/.local/bin/claude or ~/.claude/local/claude. Each machine uses its own.");
    ed.defvar("claude-code-permission-mode", json!("default"), "The permission mode new Claude Code chats start in: \"default\", \"acceptEdits\", \"plan\", \"auto\", \"dontAsk\" or \"bypassPermissions\" (each chat switches its own: S-TAB, chat-select-permission-mode). Its own settings' allow and deny rules apply in every mode.");
    ed.defvar("claude-code-permission-prompts", json!("ask"), "Who answers Claude Code's permission prompts that before-tool-call handlers (and tool-ask handlers) let through: \"ask\" (you: the tool's card and a bar above the composer wait for Allow, Always or Deny, from any window or the phone; C-c C-y, C-c C-a, C-c C-d), \"allow\" (like serpis's own agent, which never asks) or \"deny\". Tools its rules allow, such as Read, never prompt.");
    ed.defvar("claude-code-continue-after-limit", json!(true), "A Claude Code chat stopped at a usage limit (its session or weekly limit) goes on by itself when the limit resets, run by its machine's serpis-node (so this one can sleep), with claude-code-limit-continue-prompt. Off, it waits for you. Each stop can choose for itself (the bar above the composer, chat-toggle-continue-after-limit).");
    ed.defvar("claude-code-limit-continue-delay", json!(60), "Seconds after a usage limit's reset that a chat stopped at it goes on (claude-code-continue-after-limit), as the reset can lag.");
    ed.defvar("claude-code-limit-continue-prompt", json!(serpis_chat::LIMIT_CONTINUE_PROMPT), "What a Claude Code chat stopped at a usage limit is sent when it goes on after the reset (claude-code-continue-after-limit).");
    ed.defvar("agent-skills", json!(true), "Chats have skills, as pi has them (agentskills.io): each one's name and description is in the system prompt, and the agent reads its SKILL.md when a task matches; /skill:name sends one with the message (chat-select-skill picks one). Found in serpis's agent directory's skills (~/.serpis/agent/skills), then pi's (~/.pi/agent/skills and its skills setting: agent-skills-pi), the project's .pi/skills and .agents/skills, ~/.agents/skills and agent-skill-paths.");
    ed.defvar("agent-skills-pi", json!(true), "Chats also have pi's skills (~/.pi/agent/skills, and those its settings.json adds), after serpis's own when two share a name.");
    ed.defvar("agent-skill-paths", json!([]), "More skill folders or files for chats (beside pi's: agent-skills).");
    ed.defvar("claude-code-skills", json!(true), "Claude Code chats are offered serpis's skills (pi's: agent-skills) too, beside Claude Code's own, to load when a task matches (listed in its system prompt; those it has already, in ~/.claude/skills, left out). Read when a chat's claude starts: one that runs keeps the list it started with (claude-code-skills-restart).");
    ed.defvar("claude-code-skills-restart", json!(false), "When serpis's skills change, a Claude Code chat's claude restarts with its next message (resuming its session, which re-caches its prompt) to see them. Off, it sees them the next time it starts.");
    ed.defvar("agent-skills-claude", json!(false), "Chats on serpis's own agent also have Claude Code's skills (~/.claude/skills, the project's .claude/skills), after pi's when two share a name. Off by default: those synced from the Claude apps are written for tools serpis's agent doesn't have.");
    ed.defvar("claude-code-task-output-interval", json!(1000), "Milliseconds between reads of what a Claude Code chat's commands print while they run (shown on their cards, live); 0 doesn't read it. Taken up when its claude starts.");
    ed.defvar("chat-task-output-lines", json!(8), "Lines of a running command's output its card shows (opened, the card shows all that is kept: its last 40).");
    ed.defvar("claude-code-thinking-display", json!("summarized"), "How Claude Code chats send their thinking: \"summarized\" (a summary of it, shown like any chat's thinking: chat-show-thinking), \"omitted\" (none: a Thinking… line with its token count shows instead) or \"highlights\"; \"\" leaves the flag out, for a claude too old to know it.");
    ed.defvar("claude-code-permission-mode-cycle", json!(["default", "acceptEdits", "plan", "auto"]), "The permission modes chat-cycle-permission-mode (S-TAB in a Claude Code chat) goes through, in order. Add \"bypassPermissions\" or \"dontAsk\" to reach those too.");
    ed.defvar("claude-code-args", json!([]), "More arguments for the claude CLI of Claude Code chats, e.g. [\"--append-system-prompt\", \"Be brief.\"] or [\"--mcp-config\", \"~/mcp.json\"].");
    ed.defvar("claude-code-usage-advice", json!(true), "claude-code-usage also shows Claude Code's advice on what spends the subscription, from a scan of the machine's last week of Claude Code transcripts (a moment longer).");
    ed.defvar("codex-path", Value::Null, "The codex CLI that chats on codex/* models run through. Unset: codex on PATH or its usual install locations. Each machine uses its own CLI and login.");
    ed.defvar("codex-permission-prompts", json!("ask"), "Who answers Codex command, file and permission requests that before-tool-call and tool-ask handlers let through: \"ask\", \"allow\" or \"deny\".");
    ed.defvar("codex-approval-policy", json!("on-request"), "Codex's approval policy for codex/* chats: \"untrusted\", \"on-request\" or \"never\". An empty string inherits Codex's config.");
    ed.defvar("codex-sandbox", Value::Null, "Override Codex's sandbox for codex/* chats: \"read-only\", \"workspace-write\" or \"danger-full-access\". Unset inherits Codex's config.");
    ed.defvar("codex-args", json!([]), "More arguments for codex app-server, such as [\"-c\", \"web_search=live\"].");
    ed.defvar("codex-env", json!({}), "Extra environment variables for the codex app-server process of codex/* chats.");
    ed.defvar("codex-skills", json!(true), "Offer serpis's skills to Codex beside the skills Codex finds itself. The list is added to the thread's developer instructions when it starts.");
    ed.defvar("codex-skills-restart", json!(false), "Restart a Codex chat's app-server before its next turn when the offered Serpis skills changed. Its saved thread resumes in the new process.");
    ed.defvar("plugin-paths", json!([]), "Extra Rust plugin library paths.");
    ed.defvar("extensions-disabled", json!([]), "Lua extensions in <config>/extensions not to load, by name (\"max-money\" for max-money.lua).");
    ed.defvar("terminal-image-storage-mb", json!(320), "Memory for images programs show in a terminal (Kitty graphics protocol: yazi previews, kitten icat...), per terminal; 0 turns terminal images off.");
    ed.defvar("terminal-shell-integration", json!(true), "Load serpis's zsh integration in new terminals (OSC 133 prompt marks).");
    ed.defvar("terminal-prompt-regexp", json!(buffers::terminal::DEFAULT_PROMPT_REGEXP), "Prompt prefix regexp used when the shell doesn't mark prompts (OSC 133).");

    minibuffer::install(ed);
    commands::install(ed);
    log_view::install(ed);
    notify::install(ed);
    wm::install(ed);
    workspace::install(ed);
    mouse::install(ed);
    sidebar::install(ed);
    session_ui::install(ed);
    remote::install(ed);
    terminals::install(ed);
    machines::install(ed);
    pairing::install(ed);
    pickers::install(ed);
    jump::install(ed);
    extend::install(ed);
    highlight::preload();
    ed.define_buffer_kind("chat", o, |ed, p| Ok(Box::new(buffers::chat::ChatBuffer::new(ed, p))));
    ed.define_buffer_kind("terminal", o, |ed, p| Ok(Box::new(buffers::terminal::TerminalBuffer::new(ed, p)?)));
    ed.define_buffer_kind("text", o, |_, p| {
        Ok(Box::new(buffers::text::TextBuffer::new(p.get("text").and_then(Value::as_str).unwrap_or(""))))
    });
    packages::install_base(ed);
    packages::set_scheme(ed, "regular");
    fonts::install(ed);
    // Last interactively chosen theme and accent; init.lua may override them.
    if let Some(a) = Editor::persisted_state("theme-accent") {
        ed.set_var("theme-accent", a);
    }
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
    session_ui::restore_open(ed);
    if let Some(script) = std::env::var_os("SERPIS_TEST_SCRIPT") {
        run_test_script_when_settled(ed, PathBuf::from(script), std::time::Instant::now(), None);
    }
}

/// Run a test script once the native window has kept its size for a moment: the OS may resize
/// a window just after it opens, and tests read window positions from their first step.
fn run_test_script_when_settled(ed: &mut Editor, script: PathBuf, started: std::time::Instant, last: Option<(egui::Vec2, std::time::Instant)>) {
    let size = ed.ctx.input(|i| i.viewport().inner_rect).map(|r| r.size());
    let now = std::time::Instant::now();
    let steady = match (size, last) {
        (Some(s), Some((prev, since))) if s == prev => Some((prev, since)),
        (Some(s), _) => Some((s, now)),
        (None, _) => None,
    };
    let settled = steady.is_some_and(|(_, since)| now - since >= std::time::Duration::from_millis(400) && ed.frames_drawn >= 3);
    if settled || now - started > std::time::Duration::from_secs(5) {
        lua::run_file(ed, &script);
    } else {
        ed.defer(move |ed| run_test_script_when_settled(ed, script, started, steady));
    }
}

/// Run serpis.
pub fn run() -> anyhow::Result<()> {
    // Before any thread starts or anything reads the environment.
    launch::prepare();
    // Printed to the terminal as before, and kept for the *Log* page.
    serpis_node::logs::init("warn,serpis=info,message=info,lua=info");
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
