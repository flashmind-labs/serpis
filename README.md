<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/serpis-logo-dark.svg">
    <img alt="serpis" src="assets/serpis-logo-light.svg" width="380">
  </picture>
</p>

An agent harness for power users: pi's agent and tools inside an Emacs-shaped GUI (Rust, egui).
Everything is a buffer (chats, terminals, text), every key goes through keymaps, and the whole
core is open to extension from Lua or Rust plugins that link the same library.

## Install (macOS)

[Releases](https://github.com/flashmind-labs/serpis/releases) have `Serpis-macos-arm64.zip`. Unzip
it and move `Serpis.app` to `~/Applications`. The app isn't notarized yet, so macOS refuses to open
a downloaded copy until you clear the quarantine flag:

```sh
xattr -dr com.apple.quarantine ~/Applications/Serpis.app
```

Or build it from source (needs Rust and zig 0.16):

```sh
scripts/install-macos.sh      # builds Serpis.app into ~/Applications, and `serpis [dir]` into ~/.local/bin
```

Run it again to update. The bundle carries serpis' core library and Rust's standard library, so
it survives `cargo clean` and toolchain updates. Started from the Dock or Finder, serpis reads your
login shell's environment once (PATH, exported API keys), like VS Code; from a terminal it keeps
the terminal's (`SERPIS_NO_SHELL_ENV=1` turns the import off). Rust plugins must be rebuilt
against the installed version. The icon is drawn by `scripts/make-icon.py`.

## Install (iPhone)

```sh
scripts/install-ios.sh [iPhone]   # builds the app and installs it on your iPhone (by cable, or paired over the network)
```

It needs Xcode, `xcodegen` (`brew install xcodegen`) and Rust (the iOS targets are added with
rustup). It signs with your own Apple developer team, found from the Apple Development
certificate in your keychain (`SERPIS_IOS_TEAM` chooses one), and keeps it in the untracked
`ios/Local.xcconfig`. A free Apple ID works (Xcode > Settings > Accounts); its apps run for 7
days, then run the script again. `--build-only` builds without a phone. The first time, the
phone needs Developer Mode (Settings > Privacy & Security) and to trust your developer profile
(Settings > General > VPN & Device Management). Then run `phone-pair` in serpis on the desktop
and scan its code with the Camera.

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
| `s-N` / `s-T` | new chat in a new git worktree (pick the branch it starts from) / new terminal |
| `s-b` / `s-e` | toggle the sidebar / search it |
| `s-r` / `C-c C-r` | resume a saved chat session (`chat-resume`) |
| `s-\` / `s-\|` | split right / below |
| `s-]` `s-[` `s-1..9` | move between windows |
| `s-w` / `s-W` | close window (the buffer stays; the last leaves the workspace empty) / kill buffer (and its windows) |
| `RET` / `S-RET` / `M-RET` | send / newline / queue follow-up |
| `ESC` or `C-c C-c` | abort the agent |
| `TAB` | fold/unfold the tool output at point |
| `s-D` | compare and review the current folder's changes (`diff-view`) |
| `s-,` | open `init.lua` (`M-x reload-config` applies changes; `config-auto-reload` on applies them on save) |
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

With the mouse, in text, a chat's transcript and its prompt: drag to select, double-click for
a word, triple-click for its line, and click four times for everything.

`set-keybinding-scheme` switches to `emacs` or `vim` (or call `serpis.keys.scheme("vim")` in
`init.lua`). Vim works everywhere: in the chat transcript and prompt, in text buffers, and in
terminals, where normal state navigates the scrollback and edits the command line after the prompt
(`dw`, `ciw`, `A`, `p`, ...): edits are replayed to the program as keys, so they work in zsh, bash or
Claude Code's prompt. zsh gets OSC 133 prompt marks automatically; other shells fall back to
`terminal-prompt-regexp`. `<leader>` (`leader-key`, default `SPC`) starts vim normal-state
bindings and `C-w` is the window prefix. Page Up and Page Down scroll half a page in normal and
visual state, as `C-u` and `C-d` do (`vim-page-keys` = `"full"`: a page, as `C-b` and `C-f`).

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

## The agent's instructions

Serpis's agent builds its system prompt when a chat starts, from:

- `SYSTEM.md` in the config directory (`~/.config/serpis`), in place of serpis's own prompt;
- `APPEND_SYSTEM.md` there, added to it, then the `agent-append-system-prompt` setting;
- `AGENTS.md` (or `CLAUDE.md`) in `~/.serpis/agent`, then in each folder from the root down to the
  chat's, as the project's instructions.

`agent-system-prompt-files` off ignores the two config-directory files. A chat keeps the prompt
it started with, so its prompt cache stays warm: `chat-refresh-context` picks up changes once, and
`agent-live-context` rebuilds the prompt for every message. Claude Code and Codex chats use their
own prompts and project-instruction files.

## Skills

Chats on serpis's own agent have skills the way pi has them (the Agent Skills format,
agentskills.io): a folder with a `SKILL.md` whose frontmatter names it and says when it applies.
The system prompt lists each skill's name, description and file, and the agent reads the file
when a task matches; `disable-model-invocation: true` keeps one out of that list. serpis's own
live in `~/.serpis/agent/skills`; pi's (`~/.pi/agent/skills`, and the paths its `skills` setting
adds) come next, so skills you made for pi work as they are (`agent-skills-pi` = `false` leaves
them out). Then the project's `.pi/skills`, `~/.agents/skills` and `.agents/skills` up to the
repository's root, and `agent-skill-paths`; the first of a name wins. `agent-skills-claude` adds
Claude Code's (`~/.claude/skills`), `agent-skills` = `false` turns them off.

`/skill:name what to do` sends a skill's instructions with the message, for when it should apply
for sure (the transcript shows what you typed); `C-c /` (`chat-select-skill`) picks one and starts
the message with it.

Claude Code chats have both: Claude Code's own skills (`~/.claude/skills`, its plugins), which
`claude` loads itself, and serpis's, listed in its system prompt the same way for it to load when
a task matches (`claude-code-skills`; those it already has under the same name are left out). The
list is read when a chat's `claude` starts: a running one keeps it, so a change to your skills
costs no restart (`claude-code-skills-restart` = `true` restarts it, resuming, to see them). In
them, `/skill:name` sends one of serpis's, and Claude Code's own you call as it does, `/name`.
serpis's own agent doesn't see Claude Code's skills (`agent-skills-claude` adds them).

Codex chats use the skills Codex finds itself and also receive Serpis's skill list in their
thread instructions (`codex-skills`; `codex-skills-restart` restarts the app-server and resumes
the thread when that list changes). `/skill:name` still expands a Serpis skill explicitly.

## Notifications

When something you'd want to know about happens out of sight, serpis sends a desktop
notification; a click on it brings you back to that chat or terminal:

- a chat's turn ends, on serpis's own agent or Claude Code, here or on another machine ("Done in
  2m 14s: …" with the start of its reply), if it ran at least `notify-after` seconds (15);
- an agent waits for you: a permission, its questions, a plan to approve (at once);
- a run fails;
- a terminal's command finishes (after `notify-after`; `notify-terminals` = `false` stops it).

Always, by default; `notify-when` = `"unseen"` sends them only while serpis isn't the focused app
(or its window is hidden) or the buffer isn't on screen, `"away"` only while serpis isn't the
focused app. Some window managers (AeroSpace) hide windows without telling the app it lost focus,
which those two can't see. `notify-events`
picks among `"done"`, `"waiting"` and `"failed"`; `notify` = `false` turns them off,
`notify-sound` the sound. On macOS they come from Serpis.app when it is installed
(`scripts/install-macos.sh`), even when serpis runs from a terminal, and a click brings you back;
without it they show through AppleScript, and a click doesn't. Scripts send their own
with `serpis.notify(title, body, {buffer = ...})`, and the `notify` hook changes any (return
`{title = ..., body = ...}`) or drops it (`false`).

## Diffs and review

`s-D` (`diff-view`) compares the repository of the current buffer's folder, on whichever machine
it is. On a branch of its own it opens against the main branch, from where they forked, uncommitted
work and untracked files included: what a pull request would contain, plus what isn't committed
yet. On the main branch it opens on uncommitted changes. `d` picks anything else: uncommitted,
staged, unstaged, the last commit, any branch, another worktree, or a typed `a..b`, `a...b` or
`commit:rev`. A project's or worktree's right-click menu in the sidebar has it too (`Review changes`).

It compares tokens instead of lines, as difftastic does, with the grammars serpis highlights with:

- A renamed variable marks the name, not the line.
- Re-indenting a block or re-wrapping a call isn't a change: those lines dim, and `n` and `p` skip
  them. Spacing still counts inside strings, and for indentation where it is syntax (Python, YAML,
  Makefiles). `w` cycles `diff-whitespace`: `smart`, `hide` (they look unchanged), `exact`.
- As in JetBrains' viewer, lines that are alike sit across from each other, gaps are hatched, and
  lines that share little are shaded whole instead of marked word by word.
- An inserted function reads as the whole function, not the end of one and the start of the next.
- Blocks of `diff-move-min-lines` or more cut here and pasted there show as moved, in purple.

It works on tokens, not on a syntax tree as difftastic does: a change of nesting reads as tokens
added around unchanged ones. In return it knows every language with a grammar and never fails on
code that doesn't parse.

| Keys | Action |
|---|---|
| `n` `p` | next / previous change, on into the next file |
| `]` `[` `f` | next / previous file, or pick one |
| `j` `k` `J` `K` | move, extend the selection (the mouse: drag) |
| `h` `l` | the other side |
| `TAB` `x` | open the fold at the cursor, open or close them all |
| `s` `z` `b` | side by side or unified, wrap, the file list |
| `v` | mark viewed and go on (changing the file clears it) |
| `/` `y` `o` | search this file, copy the selection, open the file |
| `c` `a` | comment on the selected lines, or ask about them (or click or drag line numbers) |
| `e` `D` `R` | edit, delete, resolve the comment below the line |
| `C` | delete all the comments, after asking |
| `S` | send the review to the agent |

In the composer, `RET` is a new line, `s-RET` or `C-c C-c` keeps the comment, `C-c C-s` sends it on its
own right away, `M-k` switches between fix and question, and `ESC` drops it.

A review goes to the chat the page was opened from, or one you pick (`diff-select-target`), or a
new chat, or an agent in a terminal, where it is pasted at the prompt (`diff-review-terminal-submit`
presses Return too). Fixes and questions are sent apart, each with its file, line and code, and the
chat shows beside the page. While its agent works the review waits its turn
(`diff-review-while-running`). The page checks the repository every `diff-refresh-seconds`, so what
the agent changes shows up, and comments follow their code: when it is gone they move above the
file, marked outdated. The file list, the file's header and closed folders show how many comments
they hold.

A review waits until it is sent. Comments not sent yet, a comment still being written, what was
compared, the file showing and viewed marks are kept per worktree under `reviews/` in the config
folder: quit, come back, and `s-D` carries on where you stopped. Sent and resolved comments show
until the page closes and aren't kept.

The other settings are `diff-view-style`, `diff-split-min-width`, `diff-wrap`,
`diff-context-lines`, `diff-file-tree`, `diff-tree-width`, `diff-default-source`,
`diff-base-branch`, `diff-open-in`, `diff-review-fix-instructions`,
`diff-review-question-instructions`, `diff-review-quote-lines` and `diff-review-focus`. Scripts have
`serpis.diff.open/info/files/comments/comment`, and the hooks `diff-comment-added`,
`diff-review-send` (return a string to replace what is sent, `false` to not send it) and
`diff-review-sent`. For example, a house rule on every review:

```lua
serpis.hook.add("diff-review-send", function(p)
  return p.text .. "\nCite the file and line in every answer."
end)
```

## Log

`log-view` opens the `*Log*` page: what serpis logs (also printed to the terminal it was started
from, as before), and the log of each connected machine's serpis-node, where its chats and their
agents run (Claude Code's included). A row a record, with when, how severe, where from and what;
a click opens one in full. `l` cycles the level shown (or the Errors · Warnings · Info · Debug
control), `/` searches, `f` follows the newest (scrolling up pauses, `G` follows again), `s` shows
the next machine's log, `c` clears, `q` closes. `log-level` says what is kept (`"debug"` for
more); `log-view-level` what shows first.

Extensions log with `serpis.log.info(text, fields)` (`error`, `warn`, `debug`, `trace` too):
`serpis.log.info("synced", {files = 3})` shows `files=3` beside it. In a node's config, they go to
that node's log. `serpis.log.records(after)` reads the log back.

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
closes it: the others take its space, and closing the last leaves the workspace empty (it offers
what to open, and what you open fills it). Nothing is shown in its place, and the buffer isn't
killed: hidden buffers stay in `switch-to-hidden-buffer`. Killing is separate
(`kill-current-buffer`) and closes the windows that showed the buffer, in any workspace, as an
app's do in i3 (`kill-buffer-closes-window` = `false` shows what they showed before instead). It
asks first when a terminal runs a program other than its idle shell (`confirm-kill-processes` /
`confirm-quit-processes`: `"running"`, `"always"`, `"never"`; the `kill-buffer-query` hook can
veto or approve). serpis starts on an empty workspace; `workspace-initial-buffer` names a buffer
new workspaces show instead.

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

## Chats on any machine

Every chat runs in a node (`serpis-node`, one file, no GUI): one per machine and account, which
runs the agent, its tools, its LLM calls and its session file, with that machine's files and API
keys. The window only shows chats and sends what you type, chats on this machine included: their
node is started when serpis needs it (the `serpis-node` beside serpis, `local-node-binary`) and
keeps running when serpis quits, so a chat keeps going, and serpis on any other machine (or the
phone) that reaches this one can show it and drive it, live, while you watch it here.
`chat-resume` marks the chats that are open or running, wherever they run; picking one shows it.
Closing a chat's window leaves it running if it is working; an idle one nobody else is showing is
closed, its session kept. A rebuilt serpis restarts this machine's node on the new build once its
chats are idle.

The node runs the same `init.lua` and extensions as the window, for the chat's side of them: chat
hooks (`before-prompt`, `before-tool-call`, `assistant-message`, ...), agent tools written in Lua,
and settings; keymaps, commands and the UI do nothing there (`serpis.where` is `"node"` there,
`"window"` here). Settings this window changes go to this machine's node, and `reload-config`
reloads it too. Tools that need the window (`serpis_lua`, `serpis_api`, and Lua tools only the
window defines) are lent by each window to the chats it shows, wherever they run.

`machine-add` (or "Add a machine…" wherever serpis asks for a machine) adds one: it suggests
the hosts in your ssh config, your Tailscale machines, rayfish peers, machines on this network
that offer ssh and the ones used before, or takes any ssh destination (`me@10.0.0.5`,
`ssh://me@box:2222`, an EC2 name). serpis then logs in, and walks you through what stands in
the way: a host key it hasn't seen is shown by its fingerprint and, once you trust it, kept in
serpis's own `~/.serpis/known_hosts` (`remote-known-hosts`; ssh's own is read, never written,
and every login checks host keys strictly); a machine that doesn't take your key gets it with
`ssh-copy-id`, in a terminal where you type the password once (or the key goes on the clipboard,
or you log in as another account). Named, it is kept (`remote-machines`) and serpis installs its
node there. `machine-rename`, `machine-remove` and `machine-forget-host-key` (after a machine
was reinstalled) do the rest; each source of suggestions can be turned off (`machine-suggest-*`),
and the `machine-added` hook runs once one is added. `machine-remove` only takes a machine off
this desktop's list: nothing on the machine changes, and its node keeps running for anyone
else using it.

`phone-pair` pairs the iPhone app: it shows a code the phone's camera scans (the app opens). The
code carries the machines you share (as ssh reaches them) with the host keys trusted here, so the
phone trusts exactly those, and a pairing key each machine lets in for `phone-pair-minutes`, only
to run `serpis-node pair` (`restrict`, a forced command and an `expiry-time` in its
`authorized_keys` line). With it the phone offers its own key; the machine's node tells this
window, which asks you (showing the key's fingerprint, as the phone does), and once you say yes
the phone's key is added on every machine shared and the pairing key removed everywhere. The
phone never adds or configures machines itself.

"This machine" (`phone-pair local`) shares the desktop serpis runs on, so the phone drives the
chats this window shows. It needs ssh on here (Remote Login on macOS: System Settings › General ›
Sharing; the code says so when nothing answers) and an address the phone can use, which serpis
asks for once, offering this machine's Tailscale name and its name and address on this network
(`phone-pair-local-address` keeps it; set it to `""` to be asked again). Its host keys are read
from `phone-pair-local-host-keys`, its port from `phone-pair-sshd-config`.

Machines are ssh destinations: those added, `remote-hosts` (`serpis.var.set("remote-hosts", {"gpu-box",
"me@lab"})`, or `{name, host, env = {...}}` tables), plus the peers of your
[rayfish](https://github.com/rayfish/rayfish) networks (`remote-rayfish`), named
`host.network.ray`. With rayfish, `ray ssh on` and `ray ssh allow <network> <this machine> --user
<account>` on the other machine let serpis in by mesh identity, no keys needed; rayfish logs in
as the account the client asks for, so when a machine refuses yours serpis asks for the username
there, and keeps it (`remote-users`) once a login with it works; `"alice@box.net.ray"` in
`remote-hosts`, or `User` in `~/.ssh/config`, names it up front. serpis keeps no
credentials of its own: sshd, or rayfish, decides who gets in.

The composer's machine pill (`chat-select-machine`) moves a chat to a machine and a folder there,
messages and all: here to a machine, back, or from one machine to another. It continues there as
the same chat (its id keeps provider caches warm), with that folder's instructions, and the machine
it left keeps its session, marked as moved. Only an idle chat moves. Its folder can come along ("Bring
<folder> with it"): a git repository as its current branch plus its uncommitted work (changes,
deletions, untracked files; ignored ones stay), made without touching its index, working tree or
refs, and checked out there with the work uncommitted again; any other folder as its files. A
carried folder keeps its identity: each machine remembers where it put it (`~/.serpis/carried.json`),
so it goes back to the folder it was in before, or came from, brought up to date in place. On a
machine it hasn't been on, beside a clone of the same repository at the same place in the home it
becomes a worktree of that clone; otherwise it goes to `remote-carry-directory`
(`~/.serpis/worktrees/{project}/{branch}`; `"~/{path}"` keeps the home's layout) or, for other
folders, `remote-carry-directory-plain` (`~/.serpis/carried/{project}`). Nothing there is lost:
changes made there since are stashed (`git stash list`), commits kept on a `serpis/kept/...`
branch, a changed plain folder moved aside, and the message says so. Or it continues in a folder already there. `chat-resume`'s
"every machine" scope (TAB to it, or `chat-resume-scope` = `"everywhere"`) lists the saved
sessions of this machine and of every other one together, newest first, each saying where it is;
one picked there resumes there. Sessions whose chat moved on aren't listed
(`chat-resume-show-moved`). Besides the pill, `remote-new-chat`, `remote-attach-chat` (chats running there), `remote-resume-chat`
(sessions saved there) and `remote-close-chat` do the rest, and `chat-select-model` lists that
machine's models. Closing such a chat's window only stops showing it. The sidebar groups them as
`folder · machine`, and they are shown again when serpis starts.

serpis installs its node (`serpis-node`, one file, no GUI) on a machine the first time it
connects: into `~/.serpis/bin` there, named by its SHA-256 and checked before every start. The
first machine of each system (Linux x86-64 or arm64, with glibc or musl; macOS) gets it then, once per build of its
sources, into `~/.serpis/nodes`. serpis asks the machine what it is first, so Alpine gets a musl
build and Debian a glibc one (`serpis-node system` shows the answer); a glibc older than 2.28, or
a machine that can't say, gets the musl build too, since it is static and starts anywhere. serpis
doesn't support 32-bit systems: connecting to one stops with an error saying so, on a 64-bit kernel
too, and nothing is downloaded or installed there. A released serpis downloads the node published with it
(`remote-node-releases`) and checks it against the checksums built into serpis. serpis run from
its source builds it with `scripts/build-node.sh` (anything but this Mac needs `cargo install
cargo-zigbuild` and `rustup target add <target>`, and says so). When it can't get one, a `serpis-node` already
installed there is used. `remote-deploy` turns installing off, and `remote-node-command` runs something else there. On the machine, the
node keeps its settings in `~/.serpis/node/settings.json` (the variables above, by name) and uses
models and keys from `~/.serpis/agent` or `~/.pi/agent` there. When it has none, serpis offers to
copy this machine's API keys, models.json and default model there (over the ssh connection, into
files only that account can read), adding only what it lacks; OAuth logins aren't copied, since
their tokens belong to one machine. `remote-setup` offers it any time, `remote-offer-setup` turns
the offer off. A machine's chats run the Lua config that machine has (`~/.config/serpis` there);
it isn't copied yet. `scripts/ssh-check.sh <host>`
runs a whole chat over real ssh in a throwaway directory there, with a mock model.

A chat keeps going when its machine's node stops mid-turn (a crash, a reboot, a kill): when the
node starts again, the turn continues from what was saved, and the chat says so. A tool that was
cut off isn't run again (it may have done its work): its result says it was cut off, and the
model carries on from there. A turn cut off more than `chat-keep-going-limit` times in a row (2)
is parked instead, in case it is what stops the node. `chat-keep-going` turns this off (the
chat then waits for you), and each chat can choose for itself: the composer's `↻ Keeps going`
pill (`chat-toggle-keep-going`). Scripts hear of it through the `turn-cut-off` hook.

A node that isn't running is started by whatever connects to it next. For a chat to continue
after a reboot or a crash with nobody connecting (an agent working through the night),
`remote-install-service` installs the machine's node as a service (asking first): a systemd
user unit on Linux, a launchd agent on a Mac, which start it at boot (Linux: once `loginctl
enable-linger <user>`, with sudo where it asks, lets the user's services run without a login, which serpis says) and again
after a crash. `remote-uninstall-service` removes it.

## Codex

With the `codex` CLI installed and signed in, its models appear in `chat-select-model` as
`codex/gpt-...`. Serpis asks `codex app-server` for the current model catalog, including each
model's reasoning levels. A chat on one of those models runs through Codex's own harness, login,
tools, MCP servers, rules, sandbox and compaction. The result is still an ordinary Serpis chat:
the transcript, tool cards, diffs, session file, hooks, remote machines and phone all work the
same way.

Each chat keeps one app-server process and one Codex thread. The Codex thread ID is saved with
assistant messages, so reopening a Serpis session resumes Codex's stored thread. Switching an
existing chat to Codex sends the earlier conversation with the first turn. Text, images,
reasoning summaries, commands, file changes, MCP calls and web searches stream into the usual
Serpis entries. The context meter uses Codex's token count, and `chat-compact` asks Codex to
compact its thread.

Command, file-change and extra-permission requests wait on the tool card. Allow, Always and Deny
use the same keys and phone controls as Claude Code permissions. `before-tool-call` can block a
request first, and `tool-ask` can answer it. `codex-permission-prompts` can answer every request
with `ask`, `allow` or `deny`. `codex-approval-policy` defaults to `on-request`; set it to an empty
string to inherit Codex's config. `codex-sandbox` can override the sandbox, while `codex-args`,
`codex-env` and `codex-path` control how app-server starts. Each remote machine uses its own Codex
installation and login.

Messages sent while Codex works use `turn/steer`; follow-ups start after the current turn.
Stopping sends `turn/interrupt`. Reasoning levels include `ultra` when the selected model offers
it.

`codex-status` shows the signed-in account, ChatGPT plan-limit windows and reset times, available
credits and account token activity reported by `codex app-server`. It asks the current chat's
machine, or this machine when there is no current remote chat. The page's `g` refreshes it and `q`
closes it. API-key and cloud-provider accounts still show their authentication type; status fields
that only ChatGPT-backed accounts provide are marked unavailable.

## Claude Code

With the `claude` CLI installed and logged in, its models show up in `chat-select-model` as
`claude-code/opus`, `claude-code/sonnet`, ... (the list your account offers, asked from `claude`
itself). A chat on one of them runs its turns through Claude Code, with its harness, tools,
permission rules, compaction, MCP servers and login, so a Claude subscription pays for it. It is
still an ordinary serpis chat: the same transcript, tool cards and diffs, session files, resume,
hooks, sidebar, other machines (each with its own `claude`) and the phone. One `claude` process
serves a chat, and its session id is the chat's, so resuming the chat resumes Claude Code's
session. Switching a chat to Claude Code midway hands it the conversation so far. Its tools are
shown under serpis's names where they match (`Bash` is `bash`, `Edit` is `edit` with its diff);
its todo list and plans get cards of their own. Thinking levels are its `--effort` levels, and
`chat-compact` sends `/compact`.

When it asks permission (an edit, a command its rules don't allow), the tool's card shows what
it wants to do and waits, and a bar above the composer offers Allow (`C-c C-y`), Always
(`C-c C-a`: what Claude Code suggests, such as "edits in this session") and Deny (`C-c C-d`;
`chat-deny` with a reason tells it why). The chat keeps waiting on its machine until someone
answers, from any window. `before-tool-call` handlers can block it first, and `tool-ask` handlers
can answer for you (return `"allow"`, `"always"`, `"deny"` or `{deny = "why"}`);
`claude-code-permission-prompts` = `allow` or `deny` answers every prompt the same way.

Its multiple-choice questions (AskUserQuestion) wait on their card too, listing each question's
options. `C-c C-y` (or Answer) asks them one by one in the minibuffer: pick an option, or type
an answer of your own; a question that takes several takes picks until Done. `C-c C-d` (Skip)
lets it go on without. On the phone they are a form. Scripts answer with
`chat-answer-questions` given `{["Which color?"] = "Blue"}`, or from a `tool-ask` handler
returning `{answers = {...}}`. When prompts aren't asked (`allow`, `deny`), it is told to ask in
its reply instead.
`claude-code-permission-mode`, `claude-code-args` and `claude-code-path` set the rest.
`cargo run -p serpis-chat --example claude_code_smoke -- <dir> <prompt>` tries it headless.

While it thinks, its reply says so (`Thinking… · ~1.2k tokens`, its own estimate); its thinking
itself comes summarized (`claude-code-thinking-display`) and shows as any chat's does. A message
sent while it works goes to it at once and is taken in at its next step, in the same turn, as
in its own prompt (shown queued until then); `chat-send-follow-up` still waits for the turn to
end. A plan it asks to carry out (plan mode) waits on its card: `C-c C-y` approves it (it goes
on asking before edits), `C-c C-a` approves it accepting edits, `chat-approve-plan` picks any
mode, and `C-c C-d` (Keep planning) asks what should change. A resumed chat keeps its mode, its
subagents' steps, how its background work ended, and why it went on by itself.

Its permission mode shows in the composer the way its own prompt line shows it
(`⏵⏵ auto mode on`, `⏸ plan mode on`, `⏵⏵ accept edits on`), on the phone too. `S-TAB` cycles
through `claude-code-permission-mode-cycle` (default, accept edits, plan, auto) and a click on it
picks any mode (`chat-select-permission-mode`); the switch is immediate, with no restart, and
when Claude Code changes mode itself (edits allowed for the session, a plan approved) the chat
follows. New chats start in `claude-code-permission-mode`.

Work it runs on the side shows as it goes, as many at once as it starts. A background shell
(Bash with `run_in_background`), a monitor or an agent keeps a live status on the card that
started it (`● in background`, then how it ended), and the composer counts what is running by
kind, as Claude Code does (`· 1 shell · 1 monitor`). A click on that count (`chat-show-task`)
opens the work's card in the transcript, with what it has printed; `C-c C-k` (`chat-stop-task`)
stops one. Both ask which when several run. When such work finishes after the turn, Claude Code takes a turn of its own to act on
it, and the chat runs that turn like any other: shown with what started it, recorded, stoppable.
A subagent's steps (its commands, edits and answer) show inside its Agent card, live, even
when it runs in the background. Stopping or closing a chat ends everything its `claude`
started: what it runs is sent SIGTERM (so scripts clean up), then SIGKILL after a moment.

A command (or subagent) it waits on can move to the background, as Ctrl+B does in its TUI:
`C-c C-b` (`chat-background-task`; its card says so) returns its call and the turn goes on while
it runs, as any background shell. The phone has a Background button for it.

What its commands print shows on their cards as they run, background ones and those it waits
for: the last `chat-task-output-lines` lines (8; open the card for the last 40), as plain text,
with how much it printed in all. serpis reads it every `claude-code-task-output-interval`
milliseconds (1000; 0 turns it off), as Claude Code's own `/tasks` view does, and once more when
the command ends; the phone shows the same.

The composer's context meter (all chats) shows how full the context is, for a Claude Code chat as
Claude Code counts it (its window, its system prompt and tools), with a tick where it compacts by
itself; it turns yellow as it gets close, and its tooltip says by how much. Claude Code
compacting (by itself, or with `/compact`) shows as compacting, then a note in the transcript
where it happened (`Compacted by itself: 155.0k → 21.0k tokens`), kept when the chat is resumed.

The commands an agent runs, serpis's own or Claude Code's (and `claude` itself), have
`SERPIS_AGENT=1` in their environment, beside `SERPIS_SESSION_ID`; your own terminals don't. A
shell config can tell them apart, e.g. in `~/.zshrc`:

```zsh
if [[ -n $SERPIS_AGENT || -n $CLAUDECODE ]]; then
  unsetopt nomatch equals          # globs and "=word" as bash reads them
else
  alias rm='rm -i' cp='cp -i' mv='mv -i'
fi
```

`claude-code-usage` shows your subscription's usage as Claude Code's `/usage` reports it, in a
page of its own: the current five-hour session and the week (and any per-model weeks), each with
a bar that turns yellow and then red as it fills and when it resets, extra usage, where the week
went, and Claude Code's advice from the machine's last week of transcripts
(`claude-code-usage-advice` = `false` skips that scan). `g` asks again, `q` closes it. It asks the current chat's
machine, else this one (or the machine you name), and nothing reaches a model. Scripts get the
same answer from `serpis.agent.claude_code_usage(function(usage, err) ... end, {machine = ...})`.

Usage limits show where you work. When Claude Code warns that the account is close to one, the
composer says how close (`⚠ 85% of session limit`; a click opens `claude-code-usage`). When a
turn is refused at one, a bar above the composer says which limit, when it resets, and that the
chat goes on by itself then: its machine's node sends `claude-code-limit-continue-prompt`
(`claude-code-limit-continue-delay` seconds after the reset), so this machine can sleep, and a
node restarted meanwhile still does it. The bar's button, or `chat-toggle-continue-after-limit`,
makes it wait for you instead (`claude-code-continue-after-limit` = `false` makes that the
default); anything you send before the reset goes at once. The phone shows the same. Scripts
see it in `serpis.chat.info()` (`limit`, `continues_at`) and the `usage-limit` hook.

## Terminals on any machine

A terminal on this machine is a child of the window, on a PTY of its own: the quickest it can
be. Terminals on other machines run in that machine's terminal host (`serpis-node terminals`), a
process of its own beside the node, so a new serpis build never takes a shell with it: they keep
running when serpis quits, and the next serpis shows the ones that were shown
(`session-restore-on-start`), with what they printed and their screen as it is; killing a buffer
ends its terminal. `terminal-persistent` runs this machine's terminals that way too.

`remote-new-terminal` opens one on another machine (pick the machine, then a folder there),
`new-terminal` in a chat or terminal on another machine opens it there, in its folder
(`terminal-follow-machine`), and `terminal-attach` shows one no buffer shows. Everything about a
terminal goes over the connection the window has to its machine's node anyway, bytes included,
and a dropped one is attached again with the machine. What runs in it (for the sidebar's
activity) is asked of its machine every `terminal-poll-seconds`. A terminal runs with the home
and shell its node has, and its zsh gets serpis's prompt marks (`terminal-shell-integration`).

## Images

- **Chats**: paste an image (Cmd-V with a screenshot on the clipboard) or drop image files on a chat:
  they wait in the composer as thumbnails (× removes one) and go with the next message, if the model
  takes images. Images show in your messages, in tool results (`read` on an image) and in resumed
  sessions. Other dropped files go into the prompt as paths. Lua: `serpis.chat.attach(buf, path)`.
- **Terminals**: programs can show images with the Kitty graphics protocol (`kitten icat`, yazi's
  previews, anything using Unicode placeholders), drawn from libghostty's image store. Pasting an
  image saves it as a PNG under `$TMPDIR/serpis-pastes` and pastes its path, which Claude Code and
  others attach; dropped files paste their paths. Ctrl-V still reaches the program (Claude Code reads
  the clipboard itself). `terminal-image-storage-mb` sets the memory per terminal (0 turns it off).

## Extend

- **Lua** (LuaJIT): `~/.config/serpis/init.lua` (created on first run with examples), plus one file per
  extension in `~/.config/serpis/extensions/` (`name.lua` or `name/init.lua`), loaded after it.
  `reload-config` reloads them all, as does saving any of them with `config-auto-reload` on. Commands, keymaps, hooks, settings, buffers, windows, chats (model,
  usage, abort, session entries) and agent tools; the reference is `crates/serpis/src/extend.rs`. See
  `examples/extensions/` for ports of pi's max-money and model-presets extensions. One rule keeps
  prompt caching working: hooks and tools may block or add, never rewrite what a chat already sent
  (earlier messages, the system prompt, tool definitions), or every turn rewrites the cache.
  `serpis.chat.cache` and the `cache-miss` hook show what an extension does to it.
- **From a chat**: agents can extend serpis while it runs, like pi's agents write pi extensions.
  `serpis_api` gives them the guide and the reference (plus the commands, settings and hooks defined
  right now); `serpis_lua` runs Lua in the editor to load, test and inspect what they wrote. Hide
  them with `agent-disabled-tools`.
- **Rust**: a `dylib` crate depending on `serpis`; see `examples/plugins/hello`. Put the built library in
  `~/.config/serpis/plugins/` and it is loaded at start and on `reload-config` (on rebuild too, with
  `config-auto-reload` on). Plugins get `&mut Editor`:
  they can define buffer kinds, agent tools, interceptors, anything the built-ins do. They must be
  rebuilt against the serpis they run in (stale builds are refused, not crashed).

## Layout

```
crates/serpis-ai      pi-ai port: model catalog, auth, API adapters, streaming
crates/serpis-agent   agent loop + tools (read, bash, edit, write, grep, find, ls, web_search)
crates/serpis         editor core (dylib): buffers, windows, keymaps, Lua, plugins, UI, packages
crates/serpis-bin     the `serpis` binary
crates/serpis-chat    a chat without a window: conversation, agent, session, cache bookkeeping
crates/serpis-proto   what nodes and their clients say to each other
crates/serpis-node    the node: runs a machine's chats and their Lua; `serpis-node agent` for ssh
tests/ui              scripted UI tests (scripts/ui-test.sh)
docs/PLAN.md          design and roadmap
```

## Release

Set the new version in `Cargo.toml`, commit it, then push a tag naming it:

```sh
git tag v0.2.0 && git push origin v0.2.0
```

`.github/workflows/release.yml` refuses a tag that doesn't match `Cargo.toml`. It builds `serpis-node`
for Linux x86-64 and arm64 with glibc and with musl (each run on Ubuntu, and on Debian 10 or Alpine,
before anything is published) and for macOS arm64 and x86-64, builds `Serpis.app` with those nodes'
checksums inside it, and publishes all of them with a `SHA256SUMS` file as a GitHub release.

## Test

```sh
cargo test                 # unit tests
scripts/ui-test.sh         # drives a real window: vim grammar, emacs keys, terminal, layout, workspaces, sidebar, sessions
scripts/ui-test.sh --narrow  # the same at 760x560, however big the window is
scripts/promo/build.sh     # the promo video, from a scripted run (scripts/promo/README.md)
```

Every UI test also checks that no controls overlap or spill out of their place. These are
the composer's pills and labels and a window header's title, badges and buttons, checked in
every frame of the test. Your window manager decides the window's size, so a normal run
covers whatever size you happen to have, and `--narrow` always covers a cramped one.
