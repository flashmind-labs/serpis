# serpis

![serpis](assets/serpis-logo-light.svg)

An agent harness for power users: pi's agent and tools inside an Emacs-shaped GUI, written in Rust
with egui. Everything is a buffer (chats, terminals, text), every key goes through keymaps, and the
whole core is open to extension from Lua or Rust plugins that link the same library.

Chats run on any machine you can ssh to, keep running when you quit, and resume in pi's session
format. With the `claude` CLI installed, a chat can run on Claude Code's harness instead, so a
Claude subscription pays for it.

Start with [the tour](#the-tour). The reference for every setting, command and edge case is in
[MANUAL.md](MANUAL.md).

## Install (macOS)

```sh
scripts/install-macos.sh      # builds Serpis.app into ~/Applications and `serpis [dir]` into ~/.local/bin
```

Run it again to update. The bundle carries the core library and Rust's standard library, so it
survives `cargo clean` and toolchain updates. Started from the Dock or Finder, serpis reads your
login shell's environment once (PATH, exported API keys), like VS Code; from a terminal it keeps
the terminal's (`SERPIS_NO_SHELL_ENV=1` turns that off). Rust plugins must be rebuilt against the
installed version. The icon is drawn by `scripts/make-icon.py`.

## Install (iPhone)

```sh
scripts/install-ios.sh [iPhone]   # by cable, or paired over the network
```

It needs Xcode, `xcodegen` (`brew install xcodegen`) and Rust (the iOS targets are added with
rustup). It signs with your own Apple developer team, found from the Apple Development certificate
in your keychain (`SERPIS_IOS_TEAM` chooses one), and keeps it in the untracked
`ios/Local.xcconfig`. A free Apple ID works (Xcode > Settings > Accounts); its apps run for 7 days,
then run the script again. `--build-only` builds without a phone. The first time, the phone needs
Developer Mode (Settings > Privacy & Security) and trust in your developer profile (Settings >
General > VPN & Device Management). Then run `phone-pair` in serpis on the desktop and scan its code
with the Camera.

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
| `s-w` / `s-W` | close window (the buffer stays) / kill buffer (and its windows) |
| `RET` / `S-RET` / `M-RET` | send / newline / queue follow-up |
| `ESC` or `C-c C-c` | abort the agent |
| `TAB` | fold/unfold the tool output at point |
| `s-,` | open `init.lua` (`M-x reload-config` applies changes; `config-auto-reload` on applies them on save) |
| `s-=` / `s--` / `s-0` | bigger / smaller / reset font size |

## The tour

**Chats and terminals.** Every project can hold any number of chats. `s-N` asks for a branch, makes
a worktree and opens a chat there. The sidebar groups everything by project and worktree, shows what
each buffer is doing (running `bash cargo test`, thinking, waiting for you, failed), and counts the
agents working per project. `s-e` searches all of it, contents included.

**Machines.** Each machine and account runs one node, a single binary that holds the chats, their
tools and their API keys. The window only displays them, so a chat keeps working when you quit, and
you can attach to it from another desktop or the phone. `machine-add` walks you through ssh, host
keys and installing the node. The pill above the composer moves a chat, folder included, to another
machine and back. A turn cut off by a crash or reboot picks up where it stopped.

**Claude Code.** With `claude` installed and logged in, `claude-code/opus` and friends appear in
`chat-select-model`. Those chats run on Claude Code's harness, tools and permissions, billed to your
subscription, while staying ordinary serpis chats: same transcript, cards, diffs, sessions and
resume. Permission prompts, its multiple-choice questions and its plans wait on the tool card for
Allow, Always or Deny. Background shells, monitors and subagents show live on their cards.
`claude-code-usage` shows your plan's limits.

**Sessions.** Chats save as they go in pi's JSONL format, so serpis resumes pi's sessions and pi
resumes serpis's. `s-r` picks one from the current project; sessions from every machine can be
listed together and resumed where they are.

**Windows.** Layout works like i3/sway: a tree of containers per workspace, laid out split, tabbed or
stacked, rearranged by keys or by dragging headers onto edges. Closing a window is not killing the
buffer.

**Extend.** Lua in `~/.config/serpis/init.lua` and `extensions/` gets commands, keymaps, hooks,
settings, buffers, windows, chats and agent tools. Rust `dylib` plugins get `&mut Editor` and can
define anything the built-ins do. Agents can extend serpis while it runs, through `serpis_api` and
`serpis_lua`.

**Also.** Desktop notifications when work finishes or an agent waits. Logs from every machine in one
page (`log-view`). avy-style jumping and ninja-style copying (`M-g w`, `s-g l`). Images pasted into
chats, and the Kitty graphics protocol in terminals. Skills in the Agent Skills format, shared with
pi and optionally Claude Code.

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

## Test

```sh
cargo test                   # unit tests
scripts/ui-test.sh           # drives a real window: vim grammar, emacs keys, terminal, layout, sidebar, sessions
scripts/ui-test.sh --narrow  # the same at 760x560, however big the window is
scripts/promo/build.sh       # the promo video, from a scripted run (scripts/promo/README.md)
```

Every UI test also checks that no control overlaps or spills out of its place: the composer's pills
and labels and a window header's title, badges and buttons, checked in every frame. Your window
manager decides the window's size, so a normal run covers whatever size you have, and `--narrow`
always covers a cramped one.

The reference for every setting, command and edge case is in [MANUAL.md](MANUAL.md).

