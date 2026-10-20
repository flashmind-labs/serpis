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
| `s-n` / `s-t` | new chat / new terminal (in the current buffer's project) |
| `s-O` | open a folder as a project (pick a known one, type a path, or Browse… for the system dialog) |
| `s-N` / `s-T` | new chat in a new git worktree / new terminal |
| `s-b` / `s-e` | toggle the sidebar / search it |
| `s-r` / `C-c C-r` | resume a saved chat session (`chat-resume`) |
| `s-\` / `s-\|` | split right / below |
| `s-]` `s-[` `s-1..9` | move between windows |
| `s-w` / `s-W` | close window (the buffer stays) / kill buffer |
| `RET` / `S-RET` / `M-RET` | send / newline / queue follow-up |
| `ESC` or `C-c C-c` | abort the agent |
| `TAB` | fold/unfold the tool output at point |
| `s-,` | open `init.lua` (saved changes reload live) |
| `s-=` / `s--` / `s-0` | bigger / smaller / reset font size |

Fonts are set in `init.lua` with an installed family name or a font file, e.g.
`serpis.font.set({ mono = "Intel One Mono", sans = "Inter", mono_size = 14 })`, or picked with
`select-font` / `select-ui-font`. The defaults match t3code's: the system UI font (San Francisco on
macOS) and SF Mono. Variable fonts are set up the way browsers set them up: the optical size
follows the text size (`font-optical-size`), weights come from `font-sans-weight` /
`font-mono-weight`, bold and headings use `font-bold-weight`, and the font's own tracking table
spaces UI text (`font-tracking`). Markdown tables in chats render as aligned grids. Prompts (`M-x`, pickers) open as a floating command palette;
`minibuffer-style` = `"bottom"` puts them in the echo area, Emacs style. `chat-max-width` sets the
width of a chat's reading column.

In a chat, the prompt and the transcript are separate: selections, edits, `gg` and `G` act on
the one that has focus. In vim normal state the prompt sits below the transcript: `k` on the
prompt's first line moves up into the transcript and `j` on its last line comes back down. Clicks,
jumps (`gw`), `C-k` / `C-j` and `C-c C-o` (`chat-toggle-focus`) switch too; `i`, `a`, `o` in the
transcript go back to the prompt.

`set-keybinding-scheme` switches to `emacs` or `vim` (or call `serpis.keys.scheme("vim")` in
`init.lua`). Vim works everywhere: in the chat transcript and prompt, in text buffers, and in
terminals, where normal state navigates the scrollback and edits the command line after the prompt
(`dw`, `ciw`, `A`, `p`, ...): edits are replayed to the program as keys, so they work in zsh, bash or
Claude Code's prompt. zsh gets OSC 133 prompt marks automatically; other shells fall back to
`terminal-prompt-regexp`. `<leader>` (`leader-key`, default `SPC`) starts vim normal-state
bindings and `C-w` is the window prefix.

In terminals `C-c` is a prefix, as in Emacs: `C-c C-c` sends a real Ctrl-C, and a `C-c` followed by
an unbound key goes to the program as both keys (`prefix-passthrough-modes`,
`prefix-passthrough-timeout`). In vim insert state `ESC` follows `terminal-escape`: `"auto"` sends
it to full-screen programs (neovim, less, Claude Code) and leaves insert state at a shell prompt,
`"terminal"` always sends it, `"serpis"` always leaves insert state. `C-c C-z` cycles it for the
current terminal, `C-c ESC` does whichever ESC doesn't, and the header shows `ESC → program`
while ESC goes to the program.
Windows can also be split, closed and resized with the mouse (header buttons, draggable dividers).

## Sidebar: projects, worktrees and agents

The sidebar (`s-b`, or the button at the bottom left) lists every chat and terminal grouped by project (a repository's main
checkout) and by git worktree, each worktree with its branch and changes (`+added −removed`,
untracked, ahead/behind). Each buffer shows what it is doing: a spinner and the current step
while it works (`bash cargo test`, `thinking`), a yellow dot when an agent waits for you, a red
cross on errors. A buffer that finishes or starts waiting while you aren't looking gets a dot
until you open it. Project headers count working and waiting agents.

Chats work, and so do agents in terminals: programs in `agent-programs` (`claude`, `codex`, `pi`,
...) count as working while output flows or their title spins (`terminal-busy-title-regexp`),
and as waiting for you after `terminal-quiet-seconds` of quiet. Other commands work until they
exit.

`s-e` focuses the search: type to filter all buffers by name, title, project, branch, state
and (from three characters) content; arrows / `C-n` `C-p` pick, `RET` opens (focusing the window
that already shows it, on any workspace), `s-RET` opens in a split, `TAB` folds, `ESC` clears and
leaves. Hover buttons and right-click menus (`sidebar-buffer-menu`, `sidebar-project-menu`,
`sidebar-worktree-menu`) start chats and terminals in a project or worktree, create and remove
worktrees and kill buffers.

Every project can have any number of chats. `worktree-new-chat` (`s-N`) asks for a branch,
runs `git worktree add` under `worktree-directory` (default `~/.serpis/worktrees/<project>/<branch>`)
and opens a chat there; `worktree-remove` kills the worktree's buffers and removes it after
confirming (`worktree-delete-branch` decides about the branch). `project-add` pins a project so
it is listed with no buffers open. Chats are titled by their first prompt (`buffer-set-title`
renames).

From Lua: `serpis.sidebar.toggle/focus/query/items/selected/open`,
`serpis.project.list/add/remove/repo/worktrees/new_worktree`,
`serpis.buffer.activity/set_activity/title/set_title`, and the hooks `buffer-activity-changed`
({buffer, name, kind, state, previous, detail, agent, attention}), `worktree-created` and
`worktree-removed`. For example, a desktop notification when an agent needs you:

```lua
serpis.hook.add("buffer-activity-changed", function(e)
  if e.attention then
    os.execute(string.format([[osascript -e 'display notification "%s" with title "serpis"']], e.name .. ": " .. e.state))
  end
end)
```

Buffers show the same name everywhere (sidebar, switcher, tabs, headers, the window title): a
chat's title, else a readable name like `Chat 2` or `Terminal`. Internal names (`*chat*<2>`) stay
as stable identifiers for scripts.

Commands that need a folder take it as a parameter when scripted (`serpis.command.run("new-chat",
"~/src/app")`) and ask when run by hand: `project-open` (`s-O`, also the folder button in the
sidebar), `new-chat-in-folder`, `new-terminal-in-folder`, `project-add`, or `C-u` before `new-chat`
/ `new-terminal` in the emacs and vim schemes. The folder picker lists projects you have worked in and
the folders in your home folder; typed paths start from home (`src/app`, no `~/` needed) unless they
begin with `/`, `~` or `.`; and Browse… opens the system's folder dialog.

## Jumping and copying (avy, hop.nvim, ninja)

`jump-word` (`M-g w`, vim `gw`), `jump-line` (`M-g l`), `jump-char` (`M-g c`), `jump-char-2`
(`M-g C`) and `jump-pattern` (`M-g /`) label targets in every visible window (text buffers, chat
transcripts and prompts, terminals) and jump to the one whose label you type; the nearest targets
get single keys. ninja copies instead of jumping, without moving the cursor: `s-g l` a line,
`s-g w` a word, `s-g h` a hash, `s-g u` a URL, `s-g q` a quoted string. Settings: `jump-keys`
(hop's by default; `"abcdefghijklmnopqrstuvwxyz"` for avy's), `jump-all-windows`, `jump-dim`,
`jump-label-colors`, `jump-single-candidate`, and `vim-g-commands` for vim's `g` keys. Lua:
`serpis.jump.run{regex = "...", group = 1, action = "goto" | "copy" | function(t) ... end}`.

## Sessions

Chats save themselves as they go, in pi's session format (JSONL, `~/.serpis/agent/sessions/`),
so serpis resumes pi's sessions and pi resumes serpis's. `chat-resume` (`s-r`, like `/resume`)
picks from the current project's sessions in `session-directory` and `session-directories` (pi's
`~/.pi/agent/sessions` by default), newest first, and continues the chosen one in place: new
messages append to the same file. `C-u s-r` lists every project. `chat-continue` resumes the latest
session of the project, `session-delete` removes one, and `chat-clear` starts a new one.

Chats left open come back after a restart (`session-restore-on-start`), and the sidebar lists each
project's latest closed sessions under it (`sidebar-recent-sessions`, click to resume). Its search
covers saved sessions too. A project you chat in stays listed (`project-remember`).
Branches and compactions written by pi are followed: the active branch is resumed, with the
compaction summary in place of what it replaced. `session-save` turns recording off.

From Lua: `serpis.session.list(fn)`, `serpis.session.resume(path, {show})`, `serpis.session.of(buffer)`,
`serpis.session.directory()`, and the hook `session-resumed` ({buffer, path, id, messages}).

## Windows and workspaces

Layout works like i3/sway. A window is a view onto a buffer, and closing it (`close-window`, `:q`)
never kills the buffer: hidden buffers stay in `switch-to-hidden-buffer`. Killing is separate
(`kill-current-buffer`) and asks first when a terminal runs a program other than its idle shell
(`confirm-kill-processes` / `confirm-quit-processes`: `"running"`, `"always"`, `"never"`; the
`kill-buffer-query` hook can veto or approve).

Each workspace is a tree of containers holding any number of windows, laid out `splith`, `splitv`,
`tabbed` or `stacked`:

- `windmove-*` focuses along the tree (tabs with left/right), `window-move-*` moves the window:
  it swaps with a neighbor, enters the container next to it, or leaves its container, as in i3
  (vim: `C-w H/J/K/L`).
- `focus-parent` / `focus-child` select a whole container, so moves, closes and workspace moves
  apply to all of it.
- `layout-toggle-split`, `layout-tabbed`, `layout-stacked`, `layout-toggle-all`,
  `window-fullscreen-toggle`; dividers between any two windows drag, double-click evens them.
- Workspaces keep their live layouts: `workspace-1..9`, `workspace` (picker with preview),
  `workspace-move-window-1..9`, `workspace-back-and-forth`, `workspace-rename`. Settings:
  `workspace-display-names`, `workspace-auto-back-and-forth`, `workspace-move-follows`,
  `workspace-keep-empty`, `workspace-initial-buffer`.

With the mouse: drag a window header, a tab or a group's grip (`Alt`-drag a header for its whole
container) onto a window's edge to split there, onto its middle to group as tabs, onto a tab bar to
insert or reorder, or onto a workspace chip in the status bar (`+` makes a new one); `ESC` cancels.
Middle-click closes a header or tab, double-click a header for fullscreen, the wheel over a tab bar
switches tabs, and dividers drag. Right-click menus come from `window-menu` and `workspace-menu`,
lists of `{label, command, args}` entries you can change (`mouse-middle-click-closes`,
`mouse-double-click-fullscreen` and `mouse-drop-edge` tune the rest).

From Lua: `serpis.window.focus/move/layout/focus_parent/fullscreen/close/configuration`,
`serpis.window.list/place/place_tab` (what a drop does),
`serpis.workspace.switch/move_window/list/rename`, and the hooks `workspace-switched` and
`window-layout-changed`.

## Extend

- **Lua** (LuaJIT): `~/.config/serpis/init.lua` (created on first run with examples), plus one file per
  extension in `~/.config/serpis/extensions/` (`name.lua` or `name/init.lua`), loaded after it. Saving
  any of them reloads them all. Commands, keymaps, hooks, settings, buffers, windows, chats (model,
  usage, abort, session entries) and agent tools; the reference is `crates/serpis/src/extend.rs`. See
  `examples/extensions/` for ports of pi's max-money and model-presets extensions.
- **From a chat**: agents can extend serpis while it runs, like pi's agents write pi extensions.
  `serpis_api` gives them the guide and the reference (plus the commands, settings and hooks defined
  right now); `serpis_lua` runs Lua in the editor to load, test and inspect what they wrote. Hide
  them with `agent-disabled-tools`.
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
scripts/ui-test.sh         # drives a real window: vim grammar, emacs keys, terminal, layout, workspaces, sidebar, sessions
```
