# Serpis — plan

Serpis is an agent harness for power users: pi's agent and tools, inside an
Emacs-shaped GUI. A small Rust core owns buffers, windows, keymaps, commands
and hooks. Everything else (vim keys, the chat UI, the terminal, user config)
is a package written against the same public API, and Rust and Lua
extensions can reach the same internals that the built-in packages use.

## Decisions (agreed)

| Topic | Decision |
|---|---|
| GUI | egui / eframe 0.37 |
| Rust extensions | `dylib` crates that link the shared `serpis` dylib and get `&mut Editor`. Hot reload = unregister everything the plugin owns, unload, load a fresh copy. Plugins must be built with the same rustc and serpis version (verified by a version-stamp symbol). |
| Lua extensions | mlua (Lua 5.4, vendored). `~/.config/serpis/init.lua` plus `lua/` modules. Hot reload by re-running the file after dropping what it registered. |
| Providers | Port pi-ai: same `Model` records, the same `models.json` / `auth.json` formats, the pi.dev model catalog, and API adapters per wire protocol. |
| Web search | Both: provider-native search where the model supports it, otherwise a client-side backend trait (Brave, Exa, Tavily, DuckDuckGo). |
| Terminal | libghostty-vt via `libghostty-rs` (built with zig 0.16). |

## Architecture

```
crates/
  serpis-ai      rlib   pi-ai port: types, model registry/catalog, auth, API adapters, SSE
  serpis-agent   rlib   pi agent loop + tools (read/bash/edit/write/grep/find/ls/web_search), system prompt
  serpis         dylib  the editor core ("the guts") + built-in packages; re-exports egui, mlua, ai, agent
  serpis-bin     bin    `serpis` = serpis::run(); sets rpath for the shared libstd
examples/plugins/hello  a Rust plugin (dylib) showing commands, keys, a custom buffer and an agent tool
```

`-C prefer-dynamic` is set workspace-wide so the host, `libserpis.dylib` and
plugins share one copy of std and of every crate `serpis` re-exports. A
prototype confirmed this works: statics are shared, `TypeId`s match, trait
objects cross the boundary, and unloading succeeds.

### Core primitives (`serpis` crate)

- **Editor**: the single mutable world, passed as `&mut Editor` to every command, hook, Lua call and plugin.
- **Buffer** (`trait Buffer`): a named object with a major mode, a list of minor modes, buffer-local vars, `ui()` and `handle_key()`. Built-in kinds: `chat`, `terminal`, `text` (scratch/file), `messages`, `help`/describe.
- **TextSurface** (`trait TextSurface`): the shared text view every buffer can expose: lines, cursor, selection, visible range, `screen_rect(pos)`, and insert/delete when the buffer is writable. Vim motions, search, yank and avy/flash-style jumping are written once against this trait, so they work in terminals, chats and text buffers alike.
- **Windows**: a binary split tree. Each window shows a buffer and has its own point/scroll. Splits can be resized by dragging dividers or with commands; windows can be split, closed, maximized (`delete-other-windows`), swapped and cycled. Every window header has mouse controls (split, close, buffer menu).
- **Keymaps**: Emacs `kbd` notation (`C-x C-f`, `M-x`, `s-p`, `<escape>`). Lookup order for the focused buffer: overriding map (minibuffer/palette) → for each minor mode M: `M:<major>` then `M` → `<major>` → `global`. Prefix keys show a which-key popup. Unbound keys go to the key interceptors (vim's operator parser), then to the buffer's self-insert or terminal passthrough.
- **Commands**: named, documented functions `Fn(&mut Editor, &Args)` with a prefix argument and repeat count. All of them are reachable from `M-x` / the command palette.
- **Hooks**: named events (`buffer-created`, `window-focused`, `agent-event`, `before-tool-call`, `after-tool-call`, `config-reloaded`, …) with ordered handlers.
- **Ownership**: every registration (command, binding, hook, tool, buffer kind) records its owner (`builtin`, `lua:init.lua`, `plugin:hello`). Reloading an owner removes exactly what it registered.

### Keybinding schemes

`regular` (default; CUA/macOS-like), `emacs` and `vim` are built-in packages.
The vim package is evil-style: per-buffer states (normal/insert/visual/
visual-line), with an operator/count/motion/text-object parser over
`TextSurface`, and it is enabled out of the box. In a terminal, insert
state sends keys to the shell and normal state navigates the scrollback, as
in ghostel + evil.

### Agent

The `serpis-ai` and `serpis-agent` crates run on a tokio runtime and stream
events to the UI over channels. The tool implementations follow pi's byte
for byte where it matters: truncation limits and messages, fuzzy edit
matching, BOM/CRLF handling, the per-file mutation queue, bash output
spooling to a temp file, ripgrep/fd flags, and the system prompt structure.
Lua and Rust extensions can register tools, which run on the UI thread
through a request channel.

### Chat UI

The transcript is styled text with block regions (like Emacs buffers with
faces and overlays): a user/assistant prose font, monospace code, foldable
tool-call cards, a unified diff view with line numbers and +/- faces for
`edit`, streaming thinking, usage/cost in the mode line, and a prompt
editor at the bottom. Because it is text, vim, search and avy work on the
transcript too.

## Phases

1. **Foundation (this pass)**: workspace, dylib plugin loading and hot reload, Lua config and hot reload, buffers/windows/keymaps/commands/hooks, palette + minibuffer, which-key, the regular/emacs/vim schemes, the text buffer, the libghostty terminal, and the chat buffer with the agent. Agent coverage: the full pi tool set plus `web_search`, the `openai-completions`, `anthropic-messages`, `openai-responses` and `google-generative-ai` adapters, the pi.dev catalog, `models.json`, `auth.json` and environment-variable API keys.
2. **Provider parity**: OAuth logins (Anthropic, OpenAI Codex, GitHub Copilot, …), Bedrock/Vertex/Azure/Mistral/Codex adapters, retries, overflow handling, compaction, sessions (JSONL), session tree, and `/` commands.
3. **Navigation packages**: avy/flash port on `TextSurface`, consult-style pickers, a file buffer with syntax highlighting, and project search.
4. **Polish**: themes, fonts and ligatures, kitty graphics in the terminal, a web buffer (a wry webview or servo, to be decided), and packaging (bundling libstd next to the binary).

## Status (end of phase 1)

Done and tested (unit tests + `scripts/ui-test.sh` against a real window):

- Core: buffers, window tree (keyboard + mouse: split/close buttons, draggable dividers), keymaps with
  prefix keys and which-key, commands with prefix args, hooks, variables, ownership-exact reloads.
- Schemes: regular, emacs, vim (operators/counts/motions/text objects/visual/dot-repeat/ex commands,
  per-state keymaps, works in chat transcript+prompt, text buffers and terminal scrollback).
- Lua config with live reload; Rust dylib plugins with live reload and build-id ABI checks.
- Agent: pi's loop and tools; chat UI with streaming, thinking, foldable tool cards, diffs, usage/cost.
- Providers: openai-completions and anthropic-messages verified live (OpenRouter); openai-responses and
  google-generative-ai implemented but not yet exercised against live keys. Catalog, models.json and
  auth.json compatible with pi.
- Terminal: libghostty-vt with PTY, key encoding, colors, scrollback copy mode.
- Web search: provider-native (Anthropic, OpenAI Responses, Gemini) or client-side Brave/Exa/Tavily/DuckDuckGo.

Known gaps carried into phase 2: OAuth logins and the remaining adapters (Bedrock, Vertex, Azure,
Mistral, Codex), session persistence and compaction, terminal mouse reporting and mouse selection,
kitty images, syntax highlighting, and the avy/flash package.
