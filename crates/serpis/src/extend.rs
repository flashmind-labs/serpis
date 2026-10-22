//! Serpis extending itself: agent tools that run Lua in the live editor (`serpis_lua`) and
//! read its API (`serpis_api`), so an agent in a chat can write, load and test extensions the
//! way pi's agents write pi extensions. Also the reference for the Lua API (`LUA_API`, checked
//! against the real `serpis` table by a test) and for hooks.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use futures::FutureExt;
use mlua::{Function, LuaSerdeExt, MultiValue, Value as LuaValue};
use serde_json::{Value, json};

use crate::editor::{BUILTIN, Editor, MainSender, buffer_id_to_u64};

/// Every function (and constant) in the `serpis` table except `serpis.debug.*` (for tests):
/// (name, signature and what it does).
pub const LUA_API: &[(&str, &str)] = &[
    // Core.
    ("serpis.version", "string: serpis's version."),
    ("serpis.config_dir", "string: the config directory (init.lua, lua/, extensions/)."),
    ("serpis.message", "(text): show a message in the echo area (and *Messages*)."),
    ("serpis.error", "(text): show an error."),
    ("serpis.reload", "(): reload the whole Lua config now (init.lua and extensions); errors become messages."),
    ("serpis.defer", "(ms, fn): run fn once after ms milliseconds on the UI thread."),
    ("serpis.schedule", "(fn): run fn at the start of the next tick."),
    // Commands.
    ("serpis.command.define", "(name, fn(args), doc?): define an M-x command; args.params holds the parameters it was run with, args.prefix the C-u argument."),
    ("serpis.command.run", "(name, ...params): run a command; errors are raised."),
    ("serpis.command.list", "(): every command's name, sorted (serpis_api topic \"commands\" has their docs)."),
    // Keymaps.
    ("serpis.keymap.set", "(map, keys, cmd | fn): bind keys (\"C-c a\", \"s-k\", \"<leader> b\") in a keymap: \"global\", a major mode (\"chat\", \"terminal\", \"text\"), a minor mode (\"vim-normal\", \"vim-insert\") or \"minor:major\"."),
    ("serpis.keymap.unset", "(map, keys): remove a binding."),
    ("serpis.keymap.list", "(map): {keys = command} in a keymap."),
    // Hooks.
    ("serpis.hook.add", "(name, fn(payload)): run fn on an event (see the hooks topic); some hooks act on what fn returns."),
    ("serpis.hook.run", "(name, payload?): run a hook (your own events too); returns the handlers' non-nil results."),
    // Settings.
    ("serpis.var.get", "(name): a setting's value."),
    ("serpis.var.set", "(name, value): change a setting (runs variable-changed)."),
    ("serpis.var.define", "(name, default, doc?): define a setting; keeps a value that was already set."),
    // Buffers (ids are numbers; nil means the current buffer).
    ("serpis.buffer.current", "(): the current buffer's id."),
    ("serpis.buffer.list", "(): every buffer as {id, name, kind}."),
    ("serpis.buffer.name", "(id?): its name."),
    ("serpis.buffer.title", "(id?): its display title (a chat's first prompt, a terminal's program)."),
    ("serpis.buffer.set_title", "(id?, title | nil): override the title (nil restores the default)."),
    ("serpis.buffer.kind", "(id?): its major mode: \"chat\", \"terminal\", \"text\"..."),
    ("serpis.buffer.modes", "(id?): its active minor modes."),
    ("serpis.buffer.add_mode", "(mode, id?): enable a minor mode (its keymap applies)."),
    ("serpis.buffer.remove_mode", "(mode, id?): disable a minor mode."),
    ("serpis.buffer.create", "(kind, name?, params?): create a buffer (\"chat\" takes {cwd}, \"terminal\" {cwd, command}, \"text\" {text}); returns its id."),
    ("serpis.buffer.show", "(id): show it in the focused window."),
    ("serpis.buffer.kill", "(id?, {confirm = true}?): kill it; with confirm, ask like kill-buffer does."),
    ("serpis.buffer.exists", "(id): whether it is alive."),
    ("serpis.buffer.by_name", "(name): the id of the buffer called name, or nil."),
    ("serpis.buffer.hidden", "(): buffers no window shows."),
    ("serpis.buffer.scratch", "(): the *scratch* buffer's id (created if needed)."),
    ("serpis.buffer.directory", "(id?): its working directory."),
    ("serpis.buffer.process", "(id?): nil, or {running = false} or {running = true, program} for terminals and chats."),
    ("serpis.buffer.activity", "(id?): what the sidebar shows: {state, detail, program, attention, seconds}."),
    ("serpis.buffer.set_activity", "(id?, {state, detail} | nil): override that (state: \"idle\", \"working\", \"waiting\", \"error\", \"done\")."),
    ("serpis.buffer.text", "(): the focused buffer's text (for a chat, the focused part: prompt or transcript)."),
    ("serpis.buffer.line", "(n): line n (1-based) of the focused buffer."),
    ("serpis.buffer.cursor", "(): line, col (1-based) of the focused buffer's cursor."),
    ("serpis.buffer.set_cursor", "(line, col): move it."),
    ("serpis.buffer.insert", "(text): insert at the cursor, as if typed."),
    ("serpis.buffer.visible", "(): first and last line on screen (1-based)."),
    ("serpis.buffer.highlight", "({line, col}, {line, col}, {duration = ms, face = name}?): flash a range (end exclusive)."),
    ("serpis.buffer.set_local", "(key, value): a buffer-local setting for the current buffer."),
    // Chats (buf: a chat's buffer id; nil = the current buffer; in serpis_lua, CHAT is your own chat).
    ("serpis.chat.list", "(): every chat as serpis.chat.info returns it."),
    ("serpis.chat.info", "(buf?): {buffer, model = \"provider/id\", thinking, running, compacting, usage, context = {used, window, percent}, title, cwd, session = path | nil, session_id (the chat's id on its machine), machine = name | nil (where it runs, when not here)}."),
    ("serpis.chat.usage", "(buf?): the whole session's {input, output, cache_read, cache_write, total_tokens, cost} (cost in dollars, every message so far)."),
    ("serpis.chat.cache", "(buf?): the prompt cache: {hit_rate (share of prompt tokens read from the cache), state (\"warm\", \"cold\" or nil before anything was cached), left (seconds until it expires), misses = {{cause, text, tokens, cost}}} (newest last)."),
    ("serpis.chat.running", "(buf?): whether its agent is working."),
    ("serpis.chat.abort", "(buf?): stop its agent now (also from the chat's own hooks: no further model request or tool runs)."),
    ("serpis.chat.set_model", "(buf?, \"provider/model-id\"): switch its model; raises when unknown or the provider has no credentials."),
    ("serpis.chat.set_thinking", "(buf?, level): off, minimal, low, medium, high, xhigh, max (clamped to the model)."),
    ("serpis.chat.attach", "(buf?, path): attach an image file (png, jpeg, gif, webp, bmp) to the chat's next message; raises when it isn't an image."),
    ("serpis.chat.attachments", "(buf?): images waiting for the next message: {name, width, height, mime}."),
    ("serpis.chat.transcript", "(buf?): the transcript's text as shown (as of the last frame that drew it)."),
    ("serpis.chat.prompt", "(buf?, text): send text as if typed (a running agent is steered)."),
    ("serpis.chat.append", "(buf?, type, data): add a custom entry to its session file (pi's `custom` entries); kept when the session is resumed."),
    ("serpis.chat.entries", "(buf?, type?): its custom entries' data (of one type), oldest first; restored on resume, so use them for per-chat state."),
    // Agent.
    ("serpis.agent.tool", "({name, description, parameters = JSON schema, execute = fn(args) -> string | {text, is_error}}): give agents a tool (runs on the UI thread; keep it quick)."),
    ("serpis.agent.prompt", "(text): send text to the current chat."),
    ("serpis.agent.model", "(\"provider/model-id\"): the default model for new chats (agent-model)."),
    ("serpis.agent.models", "(): every known model as {spec, provider, id, name, reasoning, configured}."),
    ("serpis.agent.find_model", "(\"provider/model-id\"): that model (as in models()) or nil."),
    ("serpis.agent.reload_models", "(): re-read models.json and auth.json."),
    // Sessions.
    ("serpis.session.list", "(fn(list)): saved sessions, newest first ({path, id, cwd, project, name, title, first_prompt, last_prompt, messages, model, seconds}); fn runs when the background scan is done."),
    ("serpis.session.resume", "(path, {show = false}?): load a session into a chat; returns its buffer id."),
    ("serpis.session.of", "(buf?): the session file a chat records to, or nil."),
    ("serpis.session.directory", "(): where new sessions are saved."),
    // UI.
    ("serpis.ui.status", "(name, fn(buf) -> string | nil): a segment in the status bar (buf: the focused buffer)."),
    ("serpis.ui.header", "(name, fn(buf) -> string | nil): a segment in every window's header (buf: that window's buffer)."),
    ("serpis.minibuffer.read", "({prompt, candidates = {string | {label, value?, detail?}}, initial?, require_match?, sort?, frecency = category?, on_accept = fn({value, input}), on_select?, on_cancel?}): ask with completion; with `frecency`, picks are remembered and rank higher next time."),
    ("serpis.minibuffer.y_or_n", "(prompt, fn(yes)): a one-key yes/no question."),
    ("serpis.minibuffer.loading", "(on?): show (true) or hide (false) the open prompt's loading bar while more candidates are fetched; without an argument, whether it shows."),
    ("serpis.minibuffer.set_candidates", "(list): replace the open prompt's candidates (strings or {label, value, detail}), keeping the selection."),
    ("serpis.busy.start", "(key, label): show work in progress, a card with a spinner (busy-indicator), until stopped; again with the same key changes its label."),
    ("serpis.busy.stop", "(key): it's done."),
    ("serpis.busy.list", "(): what is in progress: {key, label, seconds}, oldest first."),
    ("serpis.highlight.on_yank", "(payload, {duration = ms}?): flash what was yanked (call from text-yanked)."),
    ("serpis.keys.scheme", "(name?): the keybinding scheme (\"regular\", \"emacs\", \"vim\"); with a name, switch."),
    ("serpis.keys.feed", "(\"C-x b\"): run keys as if typed (bindings only; not text input)."),
    // Windows.
    ("serpis.window.list", "(): windows of this workspace: {id, buffer, focused, visible, rect = {x, y, w, h}}."),
    ("serpis.window.count", "(): how many windows this workspace has."),
    ("serpis.window.buffers", "(): the buffers windows show."),
    ("serpis.window.split", "(\"below\"?): split the focused window (side by side unless \"below\")."),
    ("serpis.window.delete", "(): delete the focused window."),
    ("serpis.window.close", "(): close the view (the last window buries its buffer); never kills buffers."),
    ("serpis.window.only", "(): delete every other window."),
    ("serpis.window.other", "(n?): focus the nth next window."),
    ("serpis.window.focus", "(\"left\" | \"right\" | \"up\" | \"down\"): directional focus."),
    ("serpis.window.focus_id", "(id): focus a window."),
    ("serpis.window.focus_parent", "(): select the enclosing container (i3)."),
    ("serpis.window.focus_child", "(): back down."),
    ("serpis.window.move", "(dir): move the focused window or selected container (i3)."),
    ("serpis.window.resize", "(\"wider\" | \"narrower\" | \"taller\" | \"shorter\", amount?): resize by a fraction (0.05)."),
    ("serpis.window.layout", "(kind?): the focused container's layout; with kind (\"splith\", \"splitv\", \"tabbed\", \"stacked\"), set it."),
    ("serpis.window.fullscreen", "(on?): toggle or set fullscreen for the focused window."),
    ("serpis.window.place", "(win, target, zone): what a mouse drop does; zone \"left\", \"right\", \"up\", \"down\" or \"center\" (tabs)."),
    ("serpis.window.place_tab", "(win, tab_of, index): into the tab group holding tab_of."),
    ("serpis.window.configuration", "(): the layout as a table (to restore later)."),
    ("serpis.window.set_configuration", "(conf): restore one."),
    // Workspaces.
    ("serpis.workspace.current", "(): the current workspace's name."),
    ("serpis.workspace.previous", "(): the previous one's."),
    ("serpis.workspace.list", "(): {name, display, current, empty, buffers}."),
    ("serpis.workspace.switch", "(name): switch (created on demand)."),
    ("serpis.workspace.move_window", "(name, {follow = bool}?): move the focused window or container there."),
    ("serpis.workspace.rename", "(from, to): rename."),
    ("serpis.workspace.back_and_forth", "(): to the previous workspace."),
    // Projects and the sidebar.
    ("serpis.project.root", "(dir): the project root around dir (git or markers), or nil."),
    ("serpis.project.name", "(dir): its display name."),
    ("serpis.project.of_buffer", "(buf?): the project directory a buffer belongs to."),
    ("serpis.project.repo", "(dir): {worktree, main, branch, linked} for the git repository around dir."),
    ("serpis.project.worktrees", "(dir): {path, branch, head, main, locked} (runs git)."),
    ("serpis.project.new_worktree", "(dir, branch, {base = \"origin/main\", kind = \"chat\" | \"terminal\" | false, on_done = fn(path)}?): create a git worktree on a new branch from base (default: the current checkout), or on branch when it exists (and open a buffer in it)."),
    ("serpis.project.add", "(dir): pin a project to the sidebar."),
    ("serpis.project.remove", "(dir): unpin it."),
    ("serpis.project.list", "(): the sidebar's groups: {root, name, git, worktrees = {{path, branch, main, status, buffers}}}."),
    ("serpis.sidebar.toggle", "(on?): show/hide it."),
    ("serpis.sidebar.visible", "(): whether it is shown."),
    ("serpis.sidebar.focus", "(on?): move focus to (or away from) its search."),
    ("serpis.sidebar.focused", "(): whether it has focus."),
    ("serpis.sidebar.query", "(q?): its search text; with q, set it."),
    ("serpis.sidebar.items", "(query?): every buffer as {buffer, name, title, kind, project, worktree, branch, state, detail, program, seconds, agent, attention, visible, current}."),
    ("serpis.sidebar.selected", "(): the selected row as {kind = \"buffer\" | \"project\" | \"worktree\", id | path}."),
    ("serpis.sidebar.open", "(buf): open a buffer as clicking its row does."),
    // Jumps.
    ("serpis.jump.run", "({regex, group = n | \"first\", action = \"goto\" | \"copy\" | fn({buffer, line, col, end_col, text}), highlight?, all_windows?, inline_code?}): avy/hop labels on the matches; inline_code also targets code a chat shows without its backticks."),
    ("serpis.jump.labels", "(): the labels on screen {label, buffer, line, col, text}."),
    ("serpis.jump.cancel", "(): remove them."),
    // Looks.
    ("serpis.font.set", "({mono, sans, size, mono_size}): fonts (family names or file paths)."),
    ("serpis.font.get", "(): the current fonts and sizes."),
    ("serpis.font.list", "(): installed families."),
    ("serpis.theme.current", "(): the theme's name."),
    ("serpis.theme.list", "(): theme names."),
    ("serpis.theme.colors", "(): the current colors as \"#rrggbb\": {accent, bg, fg, fg_dim, border, selection, blue, purple, cyan, green, yellow, orange, red}."),
    ("serpis.theme.load", "(name): switch theme."),
    ("serpis.theme.toggle", "(): light/dark."),
    ("serpis.theme.define", "(name, {light, bg, fg, bg_dim, fg_dim, border, blue, selection, cursor, red, green, yellow, purple, cyan, ...}): a new theme."),
    ("serpis.theme.set", "({font_size, mono_size, accent, bg, cursor}): tweak the current theme."),
];

/// Hooks serpis runs: (name, payload and what a handler may return).
pub const HOOKS: &[(&str, &str)] = &[
    ("before-prompt", "{buffer, text, model, running}: a chat is about to send text. Return {block = \"reason\"} to refuse it (the text stays in the composer)."),
    ("before-compact", "{buffer, reason, tokens, messages}: a chat is about to replace its first `messages` messages with a summary (reason: manual, threshold or overflow). Return false to cancel, or {summary = \"...\"} to supply the summary yourself."),
    ("chat-compacted", "{buffer, summary, tokens, messages, cache_read}: a chat replaced its first `messages` messages with `summary`; cache_read is what the summary request read from the prompt cache."),
    ("cache-miss", "{buffer, cause, text, tokens, cost}: a response read much less from the prompt cache than the previous one left; cause is compacted, model, thinking, tools, prompt, expired, upstream, provider (a best-effort automatic cache didn't reuse it) or unexplained (an explicit cache missed with nothing changed), tokens what was rewritten and cost what reading it would have saved."),
    ("before-cache-invalidate", "{buffer, action, tokens, cost, message}: an action (model, thinking, refresh) is about to throw a warm prompt cache away. Return false to keep the cache (the action doesn't happen) or a string to replace the warning."),
    ("assistant-message", "{buffer, text, stopReason, model, upstream, usage, total}: a model response finished; upstream is who served it behind a router (OpenRouter's provider, e.g. \"Anthropic\"), usage is this response's, total the session's ({input, output, cache_read, cache_write, total_tokens, cost})."),
    ("before-tool-call", "{buffer, name, args}: the agent wants to run a tool. Return {block = \"reason\"} to refuse, or {args = {...}} to change its arguments."),
    ("after-tool-call", "{buffer, name, text, isError}: a tool finished. Return {text = \"...\"} to replace what the model sees."),
    ("tool-finished", "{buffer, name, isError}: a tool's result reached the chat."),
    ("agent-finished", "{buffer, total}: a chat's agent stopped (done, failed or aborted)."),
    ("remote-connected", "{machine, host, serpis}: serpis reached the node on another machine (host: what it calls itself; serpis: its version)."),
    ("remote-disconnected", "{machine, error}: the node on another machine couldn't be reached, or its connection closed; it is retried while one of its chats is shown."),
    ("session-resumed", "{buffer, path, id, messages}: a saved session was loaded into a chat."),
    ("buffer-created", "{buffer, kind}."),
    ("buffer-shown", "{buffer}: a buffer was shown in a window."),
    ("buffer-killed", "{buffer}."),
    ("kill-buffer-query", "{buffer, name, process, running}: before killing. Return false to keep the buffer, true to kill without asking."),
    ("buffer-activity-changed", "{buffer, name, kind, state, previous, detail, agent, attention}: a buffer's sidebar state changed (working, waiting, done...)."),
    ("terminal-exited", "nil: a terminal's program exited."),
    ("variable-changed", "{name, value}: a setting changed."),
    ("config-loaded", "nil: init.lua and the extensions ran."),
    ("text-yanked", "{buffer, start = {line, col}, end = {line, col}, linewise}: text was copied (1-based, end exclusive; see serpis.highlight.on_yank)."),
    ("jump-copied", "{text}: a ninja copy (s-g ...) copied text."),
    ("keyboard-quit", "nil: C-g / ESC cancelled something."),
    ("vim-state-changed", "{state}: normal, insert, visual..."),
    ("theme-changed", "{name}."),
    ("window-configuration-changed", "nil: windows were split, closed or moved."),
    ("window-layout-changed", "{previous, current}: a container's layout changed."),
    ("workspace-switched", "{from, to}."),
    ("workspace-node-moved", "{to, follow}: a window or container moved to another workspace."),
    ("worktree-created", "{project, path, branch}."),
    ("worktree-removed", "{project, path, branch}."),
];

const GUIDE: &str = r#"# Extending serpis (the editor you are running in)

Serpis is extended with Lua (LuaJIT: Lua 5.1 plus `bit`, `ffi` and `goto`; there is no `//`
or `utf8` library). Everything a user sees is reachable: commands, keys, hooks, settings,
buffers, chats, agent tools, UI segments.

## Where code lives
- {config}/init.lua: the user's config. Don't put extensions here.
- {config}/extensions/NAME.lua (or NAME/init.lua for several files): one extension per file,
  loaded after init.lua in name order. This is where new extensions go. `require` finds
  modules in {config}/lua/ and {config}/extensions/.
- The setting `extensions-disabled` lists extensions not to load, by name.
- Loaded now: {loaded}.

## Loading
Saving any .lua file under {config} reloads the whole Lua config about 150 ms later: what Lua
registered (commands, bindings, hooks, tools, segments, settings' definitions) is removed and
every file runs again in a fresh Lua state. So:
- top-level code runs on every reload; define things, don't accumulate them;
- local variables reset on reload. Keep per-chat state in the session with
  serpis.chat.append / serpis.chat.entries (it also survives resume), and settings with
  serpis.var.define (the user can set them in init.lua).

## Working on an extension
1. Read the API: serpis_api with topic "lua", "hooks", "commands" or "variables" (filter
   narrows them).
2. Write {config}/extensions/NAME.lua with your file tools.
3. Load it and check: serpis_lua { code = "serpis.reload()" } reports errors from any file.
4. Exercise it with serpis_lua: run its commands (serpis.command.run), read state back
   (serpis.chat.info(CHAT), serpis.chat.entries(CHAT, "type")), call hooks
   (serpis.hook.run). In serpis_lua, CHAT is your own chat's buffer id.
5. Tell the user what you added and how to use it (command names, keys, settings).

## Conventions
- Commands are kebab-case and show up in M-x; give each a doc string.
- Make behavior configurable with serpis.var.define("my-ext-setting", default, "doc").
- A chat's model is "provider/model-id" (see serpis.agent.models()).
- Hooks receive one payload table; return nil unless the hook acts on results.
- Errors raised in hooks and commands are shown to the user; prefer serpis.message for
  feedback, serpis.error for failures.

## Keep the prompt cache warm
Providers reuse what a chat already sent only while those bytes stay exactly the same. An
extension that changes anything already sent makes the next request rewrite the whole
conversation, about ten times the price of reading it from the cache, on every turn.
- Hooks may block or add, never rewrite what was already sent. Add with serpis.chat.prompt,
  block with before-prompt; don't edit earlier messages, the system prompt or tool definitions
  from one request to the next.
- Agent tools: keep the name, description and parameters fixed. No dates, counts or state in a
  description; changing data goes in the tool's result. Registering, removing or reordering
  tools in the middle of a chat throws its cache away.
- No dates, times or ids in agent-append-system-prompt or agent-guidelines.
- Session entries (serpis.chat.append) are never sent to the model: use them freely.
- Check your extension: serpis.chat.cache(CHAT) shows the hit rate and recent misses with their
  causes, and the cache-miss hook fires on each one. A miss with cause "unexplained" after your
  change means it altered something already sent.

## Example: a command that shows what this chat has spent
serpis.command.define("chat-cost", function()
  local u = serpis.chat.usage()
  serpis.message(string.format("$%.4f, %d tokens", u.cost, u.total_tokens))
end, "Show what the current chat has spent.")
"#;

pub fn install(ed: &mut Editor) {
    let main = ed.main_sender();
    ed.register_tool(BUILTIN, std::sync::Arc::new(LuaEval { main: main.clone() }));
    ed.register_tool(BUILTIN, std::sync::Arc::new(ApiDocs { main }));
}

fn config_dir() -> String {
    let d = crate::config_dir();
    match dirs::home_dir().and_then(|h| d.strip_prefix(&h).ok().map(|r| format!("~/{}", r.display()))) {
        Some(s) => s,
        None => d.display().to_string(),
    }
}

/// The reference text for `topic`, filtered to lines (entries) containing `filter`.
pub fn reference(ed: &Editor, topic: &str, filter: Option<&str>) -> String {
    let keep = |s: &str| filter.is_none_or(|f| s.to_lowercase().contains(&f.to_lowercase()));
    let lines: Vec<String> = match topic {
        "lua" => LUA_API.iter().map(|(n, d)| format!("{n}{}{d}", if d.starts_with('(') { "" } else { ": " })).collect(),
        "hooks" => HOOKS.iter().map(|(n, d)| format!("{n}: {d}")).collect(),
        "commands" => {
            let mut v: Vec<String> = ed.commands.values().map(|c| format!("{}: {}", c.name, c.doc)).collect();
            v.sort();
            v
        }
        "variables" => {
            let mut v: Vec<String> = ed.vars.iter().map(|(n, v)| format!("{n} = {}: {}", v.value, v.doc)).collect();
            v.sort();
            v
        }
        _ => {
            let loaded = if ed.loaded_extensions.is_empty() { "none".to_string() } else { ed.loaded_extensions.join(", ") };
            return GUIDE.replace("{config}", &config_dir()).replace("{loaded}", &loaded);
        }
    };
    let out: Vec<String> = lines.into_iter().filter(|l| keep(l)).collect();
    if out.is_empty() { format!("Nothing in {topic} matches.") } else { out.join("\n") }
}

/// Tool: the API reference.
struct ApiDocs {
    main: MainSender,
}

impl serpis_agent::AgentTool for ApiDocs {
    fn name(&self) -> &str {
        "serpis_api"
    }
    fn description(&self) -> String {
        "Read how to extend serpis, the editor this chat runs in, with Lua: topic \"guide\" (start here: where extensions live, how they load, how to test them), \"lua\" (the serpis.* API), \"hooks\" (events and payloads), \"commands\" or \"variables\" (everything defined right now). `filter` keeps matching entries.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "topic": {"type": "string", "enum": ["guide", "lua", "hooks", "commands", "variables"]},
                "filter": {"type": "string", "description": "Only entries containing this text (case-insensitive)."}
            }
        })
    }
    fn prompt_snippet(&self) -> Option<String> {
        Some("Read the API for extending serpis (the editor you run in) with Lua".into())
    }
    fn execute(&self, _id: String, args: Value, _ctx: serpis_agent::ToolContext) -> futures::future::BoxFuture<'static, anyhow::Result<serpis_agent::ToolResult>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        self.main.send(move |ed| {
            let topic = args.get("topic").and_then(Value::as_str).unwrap_or("guide");
            let _ = tx.send(reference(ed, topic, args.get("filter").and_then(Value::as_str).filter(|f| !f.is_empty())));
        });
        async move { Ok(serpis_agent::ToolResult::text(rx.await.map_err(|_| anyhow::anyhow!("serpis closed"))?)) }.boxed()
    }
}

/// Tool: run Lua in the live editor.
struct LuaEval {
    main: MainSender,
}

/// How long one serpis_lua call may run before it is stopped (it blocks the UI).
const EVAL_LIMIT: Duration = Duration::from_secs(5);

impl serpis_agent::AgentTool for LuaEval {
    fn name(&self) -> &str {
        "serpis_lua"
    }
    fn description(&self) -> String {
        "Run Lua inside serpis, the editor this chat runs in, with the full serpis.* API (see serpis_api). Use it to load and test extensions (serpis.reload()), inspect the editor, or act on it. An expression's value is returned (\"serpis.chat.info(CHAT)\"); otherwise use `return`. CHAT is this chat's buffer id. The result lists returned values, print() output, and messages and errors serpis showed meanwhile. Runs on the UI thread, stopped after 5 s.".into()
    }
    fn parameters(&self) -> Value {
        json!({
            "type": "object",
            "properties": {"code": {"type": "string", "description": "Lua code (LuaJIT)."}},
            "required": ["code"]
        })
    }
    fn prompt_snippet(&self) -> Option<String> {
        Some("Run Lua inside serpis (the editor you run in) to load, test or inspect extensions".into())
    }
    fn execute(&self, _id: String, args: Value, ctx: serpis_agent::ToolContext) -> futures::future::BoxFuture<'static, anyhow::Result<serpis_agent::ToolResult>> {
        let (tx, rx) = tokio::sync::oneshot::channel();
        let code = args.get("code").and_then(Value::as_str).unwrap_or("").to_string();
        let sid = ctx.env.get("SERPIS_SESSION_ID").cloned();
        self.main.send(move |ed| {
            let chat = sid.and_then(|s| ed.chat_by_session_id(&s));
            let _ = tx.send(eval(ed, &code, chat.map(buffer_id_to_u64)));
        });
        async move {
            let (text, is_error) = rx.await.map_err(|_| anyhow::anyhow!("serpis closed"))?;
            let mut r = serpis_agent::ToolResult::text(text);
            r.is_error = is_error;
            Ok(r)
        }
        .boxed()
    }
}

/// Run `code` in the editor's Lua state: (report, failed). Expressions are tried first, as in a
/// REPL; `chat` is exposed as the global `CHAT` meanwhile.
pub fn eval(ed: &mut Editor, code: &str, chat: Option<u64>) -> (String, bool) {
    let Some(lua) = ed.lua.as_ref().map(|l| l.lua.clone()) else {
        return ("Lua isn't running".into(), true);
    };
    let printed = Rc::new(RefCell::new(Vec::<String>::new()));
    let seen = ed.messages.len();
    let globals = lua.globals();
    let old_print: LuaValue = globals.get("print").unwrap_or(LuaValue::Nil);
    let old_chat: LuaValue = globals.get("CHAT").unwrap_or(LuaValue::Nil);
    let out = printed.clone();
    let print = lua.create_function(move |lua, args: MultiValue| {
        let tostring: Function = lua.globals().get("tostring")?;
        let parts: Vec<String> = args.into_iter().map(|v| tostring.call::<String>(v).unwrap_or_default()).collect();
        out.borrow_mut().push(parts.join("\t"));
        Ok(())
    });
    let _ = print.map(|p| globals.set("print", p));
    let _ = globals.set("CHAT", chat);
    // Compiled code doesn't run count hooks: interpret while this runs, so the limit holds.
    let jit: Option<mlua::Table> = globals.get("jit").ok();
    let jit_call = |name: &str| {
        if let Some(f) = jit.as_ref().and_then(|j| j.get::<Function>(name).ok()) {
            let _ = f.call::<()>(());
        }
    };
    jit_call("off");
    jit_call("flush");
    let start = Instant::now();
    let _ = lua.set_hook(mlua::HookTriggers::new().every_nth_instruction(10_000), move |_, _| {
        if start.elapsed() > EVAL_LIMIT {
            Err(mlua::Error::runtime(format!("stopped after {} s", EVAL_LIMIT.as_secs())))
        } else {
            Ok(mlua::VmState::Continue)
        }
    });
    let chunk = match lua.load(format!("return {code}")).set_name("serpis_lua").into_function() {
        Ok(f) => Ok(f),
        Err(_) => lua.load(code).set_name("serpis_lua").into_function(),
    };
    let result = chunk.and_then(|f| crate::lua::with_editor_ptr(ed, || f.call::<MultiValue>(())));
    lua.remove_hook();
    jit_call("on");
    // A reload in the code replaced the Lua state: restore nothing in the new one.
    if ed.lua.as_ref().is_some_and(|l| l.lua.globals().to_pointer() == globals.to_pointer()) {
        let _ = globals.set("print", old_print);
        let _ = globals.set("CHAT", old_chat);
    }

    let mut report = Vec::new();
    let failed = match &result {
        Ok(values) if values.is_empty() => {
            report.push("ok (no value)".to_string());
            false
        }
        Ok(values) => {
            for v in values {
                report.push(describe(&lua, v.clone()));
            }
            false
        }
        Err(e) => {
            report.push(format!("error: {e}"));
            true
        }
    };
    let printed = printed.borrow();
    if !printed.is_empty() {
        report.push(format!("printed:\n{}", printed.join("\n")));
    }
    let messages: Vec<&String> = ed.messages.iter().skip(seen).collect();
    if !messages.is_empty() {
        report.push(format!("messages:\n{}", messages.iter().map(|m| m.as_str()).collect::<Vec<_>>().join("\n")));
    }
    let errors = messages.iter().any(|m| m.starts_with("error:"));
    (report.join("\n\n"), failed || errors)
}

/// A Lua value as the model should see it: JSON when it converts, else `tostring`.
fn describe(lua: &mlua::Lua, v: LuaValue) -> String {
    match &v {
        LuaValue::String(s) => return format!("{:?}", s.to_string_lossy()),
        LuaValue::Function(_) | LuaValue::UserData(_) | LuaValue::Thread(_) => {
            return lua.globals().get::<Function>("tostring").and_then(|t| t.call::<String>(v)).unwrap_or_default();
        }
        _ => {}
    }
    let opts = mlua::DeserializeOptions::new().deny_unsupported_types(false).deny_recursive_tables(false);
    match lua.from_value_with::<Value>(v.clone(), opts) {
        Ok(j) => serde_json::to_string_pretty(&j).unwrap_or_default(),
        Err(_) => lua.globals().get::<Function>("tostring").and_then(|t| t.call::<String>(v)).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every function in the `serpis` table is documented, and nothing documented is gone.
    #[test]
    fn lua_api_reference_is_complete() {
        let lua = mlua::Lua::new();
        let api = crate::lua::api(&lua).unwrap();
        fn walk(t: &mlua::Table, prefix: &str, out: &mut Vec<String>) {
            for (k, v) in t.pairs::<String, LuaValue>().flatten() {
                let name = format!("{prefix}.{k}");
                match v {
                    LuaValue::Table(t) if name != "serpis.debug" => walk(&t, &name, out),
                    LuaValue::Table(_) => {}
                    _ => out.push(name),
                }
            }
        }
        let mut real = Vec::new();
        walk(&api, "serpis", &mut real);
        let documented: Vec<&str> = LUA_API.iter().map(|(n, _)| *n).collect();
        let missing: Vec<&String> = real.iter().filter(|n| !documented.contains(&n.as_str())).collect();
        let stale: Vec<&&str> = documented.iter().filter(|n| !real.iter().any(|r| r == **n)).collect();
        assert!(missing.is_empty(), "undocumented in extend::LUA_API: {missing:?}");
        assert!(stale.is_empty(), "documented but not in the API: {stale:?}");
    }

    /// Every hook the source runs is documented.
    #[test]
    fn hooks_reference_is_complete() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut sources = String::new();
        fn read(d: &std::path::Path, out: &mut String) {
            for e in std::fs::read_dir(d).unwrap().flatten() {
                let p = e.path();
                if p.is_dir() {
                    read(&p, out);
                } else if p.extension().is_some_and(|x| x == "rs") {
                    out.push_str(&std::fs::read_to_string(&p).unwrap());
                }
            }
        }
        read(&root, &mut sources);
        // Chats run their hooks from serpis-chat, through the editor's `Host`.
        read(&root.join("../../serpis-chat/src"), &mut sources);
        let documented: Vec<&str> = HOOKS.iter().map(|(n, _)| *n).collect();
        let mut missing = Vec::new();
        let calls = ["run_hook(\"", "host.hook(self, \""];
        for part in calls.iter().flat_map(|c| sources.split(c).skip(1)) {
            let name = &part[..part.find('"').unwrap()];
            if !documented.contains(&name) && !missing.contains(&name) {
                missing.push(name);
            }
        }
        assert!(missing.is_empty(), "hooks missing from extend::HOOKS: {missing:?}");
        for d in documented {
            assert!(calls.iter().any(|c| sources.contains(&format!("{c}{d}\""))), "documented hook {d} is never run");
        }
    }
}
