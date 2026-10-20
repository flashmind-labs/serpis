# Serpis

An agent harness for power users: pi's agent and tools inside an Emacs-shaped GUI (Rust, egui).
Everything is a buffer (chats, terminals, text), every key goes through keymaps, and the whole
core is open to extension from Lua or Rust plugins that link the same library.

## Run

```sh
cargo run -p serpis-bin            # needs zig 0.16 on PATH (libghostty-vt is built from source)
```

Models, keys and defaults come from the same files as pi: `~/.serpis/agent/{models.json,auth.json}`,
falling back to `~/.pi/agent/`, plus the usual `*_API_KEY` variables. Your pi default model and
thinking level are picked up automatically. `chat-select-model` (`C-c C-m`) switches models.

## Keys (regular scheme, macOS)

| Keys | Action |
|---|---|
| `s-P` / `M-x` | run any command |
| `s-p` | switch buffer |
| `s-n` / `s-t` | new chat / new terminal |
| `s-\` / `s-\|` | split right / below |
| `s-]` `s-[` `s-1..9` | move between windows |
| `s-w` | close window |
| `RET` / `S-RET` / `M-RET` | send / newline / queue follow-up |
| `ESC` or `C-c C-c` | abort the agent |
| `TAB` | fold/unfold the tool output at point |
| `s-,` | open `init.lua` (saved changes reload live) |
| `s-=` / `s--` / `s-0` | bigger / smaller / reset font size |

Fonts are set in `init.lua` with an installed family name or a font file, e.g.
`serpis.font.set({ mono = "Intel One Mono", sans = "Inter", mono_size = 14 })`, or picked with
`select-font` / `select-ui-font`.

`set-keybinding-scheme` switches to `emacs` or `vim` (or call `serpis.keys.scheme("vim")` in
`init.lua`). Vim works everywhere: in the chat transcript and prompt, in text buffers, and in
terminals, where normal state navigates the scrollback and edits the command line after the prompt
(`dw`, `ciw`, `A`, `p`, ...): edits are replayed to the program as keys, so they work in zsh, bash or
Claude Code's prompt. zsh gets OSC 133 prompt marks automatically; other shells fall back to
`terminal-prompt-regexp`. `SPC` is the leader and `C-w` the window prefix.
Windows can also be split, closed and resized with the mouse (header buttons, draggable dividers).

## Extend

- **Lua**: `~/.config/serpis/init.lua` (created on first run with examples). Commands, keymaps, hooks,
  variables, buffers, windows and agent tools: see the `serpis` table in `crates/serpis/src/lua.rs`.
- **Rust**: a `dylib` crate depending on `serpis`; see `examples/plugins/hello`. Put the built library in
  `~/.config/serpis/plugins/` and it is loaded and hot-reloaded on rebuild. Plugins get `&mut Editor`:
  they can define buffer kinds, agent tools, interceptors, anything the built-ins do. They must be
  rebuilt against the serpis they run in (stale builds are refused, not crashed).

## Layout

```
crates/serpis-ai      pi-ai port: model catalog, auth, API adapters, streaming
crates/serpis-agent   agent loop + tools (read, bash, edit, write, grep, find, ls, web_search)
crates/serpis         editor core (dylib): buffers, windows, keymaps, Lua, plugins, UI, packages
crates/serpis-bin     the `serpis` binary
tests/ui              scripted UI tests (scripts/ui-test.sh)
docs/PLAN.md          design and roadmap
```

## Test

```sh
cargo test                 # unit tests
scripts/ui-test.sh         # drives a real window: vim grammar, emacs keys, terminal copy mode
```
