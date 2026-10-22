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
| Lua extensions | mlua (LuaJIT, vendored). `~/.config/serpis/init.lua` plus `lua/` modules and `extensions/`. Hot reload by re-running the files after dropping what they registered. Agents extend it from chats with the `serpis_api` and `serpis_lua` tools. |
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

## Cost and caching

Goal: pay as little as possible for the same work, and show the user why. **OpenRouter is the
venue we optimize for first**: every item is built and tested on its paths (chat completions with
`anthropic/*`, `google/*` and `openai/*` models, and Anthropic messages on an openrouter.ai base
URL) before the direct providers, and OpenRouter is detected by provider id *or* base URL. Every
behavior is a var, and every UI element can be turned off.

A cache read needs four things, and each has one cheap fix:

| Condition | Fix |
|---|---|
| Same bytes: the prefix is unchanged | Everything already sent is immutable (items 1–2) |
| Correct marks: on explicit-cache APIs a mark sits within 20 blocks of the last cached entry | Two rolling marks (item 3) |
| Not expired | 1h retention by default (item 4) |
| Same place: same upstream / routing key | Keep the session id; record the OpenRouter upstream (item 5) |

The invariant behind all of it, for serpis and its extensions alike: block or add, never rewrite
what a chat already sent (earlier messages, the system prompt, tool definitions). The extension
guide (`serpis_api` "guide") and the README say so, with how to check it.

With the prefix immutable by construction, a miss has only three possible causes, and serpis knows
each: a setting changed (model, thinking, tools), the entry expired, or OpenRouter moved the chat
to another upstream. Any other miss is a bug, and the test suite (item 1) exists to catch it before
it ships, so there is no runtime hashing of prompts.

1. **Cache test suite** (`crates/serpis-testkit`, dev-only). One model of provider caching, used at
   every level:
   - *Simulator*: Anthropic-style rules (prefix keyed by model and thinking settings, at most 4
     marks, a 20-block lookback per mark, TTL reset on each read, minimum cacheable size) and
     automatic prefix caching (OpenAI-style, keyed by routing key). OpenRouter is modelled as one
     cache per upstream, keyed by `x-session-id`.
   - *Mock provider*: a local HTTP server speaking chat completions (OpenRouter format included)
     and Anthropic messages. It answers with the simulator's numbers in real usage fields and
     logs each request's read, write and miss cause.
   - *Adapter tests*: every API path through `serpis_ai::api::build_request` into the simulator.
   - *Agent scenarios*: real agent runs against the mock: multi-turn, 10 parallel tool calls,
     steering, follow-ups, abort, an errored turn, truncated tool calls, images. Each scenario
     asserts the hit rate and that every miss has an expected cause.
   - *App scenarios* (`scripts/ui-test.sh` runs the mock as a binary): resume, model and thinking
     switches, CLAUDE.md edits, tool toggles, and the cache UI from item 6.
   - A test that sends the same conversation twice and requires identical bytes (determinism).

   *Done*: the simulator and mock (`crates/serpis-testkit`, with tests of
   each caching rule), adapter fixtures (`crates/serpis-ai/tests/requests.rs`) and 8 agent
   scenarios on 5 venues (`crates/serpis-agent/tests/cache.rs`). The suite found two leaks, both
   fixed: on OpenRouter Claude models only the marked message went out as content parts, so the
   previous request's last message changed bytes each turn; and a step with 10 parallel tool calls
   missed through the lookback. Re-introducing either leak, or dropping the OpenRouter session
   header, fails the suite. The shared prefix measures about 2K tokens: above the 1,024-token
   minimum, below 4,096. App scenarios: `tests/ui/cache.lua` runs the real app against
   `serpis-mock-provider` (CLAUDE.md edit, `chat-refresh-context`, thinking change, resume).
   Re-introducing a per-message prompt rebuild or a new id on resume fails it. Property tests
   (`crates/serpis-ai/tests/cache_props.rs`, proptest) generate agent-shaped transcripts on six
   venues: append-only always hits, bytes are deterministic, and each injected change is its own
   miss cause and recovers. Seeds of past failures are kept next to them.
2. **Frozen prefix** (*done*): the system prompt is kept per chat, and saved in its session
   (`serpis-system-prompt` entries). A resumed or restored chat keeps it while its cache can still
   be warm (`chat-resume-system-prompt`: warm / always / never), says so when the instructions
   changed since (`chat-resume-system-prompt-notice`), and carries its cache state on from the
   last response; an older one is rebuilt, since keeping it would save nothing.
   `tests/ui/cache_restore.lua` restarts serpis with CLAUDE.md changed and requires a cache hit.
   Tools are compared by their full declarations, so a changed extension tool is a "tools" miss. `chat-refresh-context` rebuilds
   it with the next message, and `agent-live-context` rebuilds it for every message. The tool list
   stays live: a kept tool from a hot-reloaded Rust plugin would point into unloaded code, and
   changing tools is an explicit action (a stated miss cause, warned about in item 6). When the
   tools change, the prompt is rebuilt with them, since that request misses anyway.
3. **Marks** (*done*): the system prompt, the last tool, the previous request's end (the last user
   or tool message before the newest assistant message) and this request's end. The OAuth
   identity block is no longer marked (the system mark covers it). OpenRouter gets marks for
   `google/*` too, and is detected by base URL.
4. **Retention** (*done*): `agent-cache-retention` (`none` / `short` / `long`, overridable per provider),
   default `long`. 1h writes cost 2× instead of 1.25×, but only on new tokens, while every pause
   over 5 minutes on a short TTL rewrites the whole context.
5. **Routing** (*done*): a resumed chat reuses the saved session id (session files are created
   with the chat's id). `compat.openRouter` marks a gateway in front of OpenRouter. Each
   response's OpenRouter upstream is kept on the message (`upstream`, saved in the session) and
   passed to the `assistant-message` hook.
6. **Cache UI** (*done*: `crates/serpis/src/cache_watch.rs` works out hits, misses and their
   causes from what each request was sent with and what the provider reported; `tests/ui/cache.lua`
   checks every part below, and every switch, against the mock), each part with its own var:
   - `chat-cache-indicator`: a hit / partial / miss chip on each response (cache reads ÷ prompt).
   - `chat-cache-status`: the session hit rate and the cache state ("warm 41m", "cold") in the
     header; clicking lists recent misses with their causes.
   - `chat-cache-miss-notice` (`inline` / `message` / `off`): e.g. "cache miss: thinking level
     changed, rewrote 84K tokens (~$0.32)".
   - `cache-warn` (`confirm` / `notice` / `off`), `cache-warn-threshold` (default $0.10) and
     `cache-warn-actions`: a warning before an action that invalidates the cache (model,
     thinking, tool or web-search toggle, `chat-refresh-context`, sending after the TTL expired),
     with the estimate (last prompt tokens × write price). "Don't warn again" updates
     `cache-warn-actions`, and the `before-cache-invalidate` hook can veto or rewrite it.

7. **Live check against real providers** (*done*): `scripts/live-cache-check.sh` runs
   `crates/serpis-ai/tests/live_cache.rs` with the user's `OPENROUTER_API_KEY` (a few cents). A
   short conversation per model family (Claude, Gemini, OpenAI, DeepSeek; first, next turn,
   12 parallel tool calls, another turn) goes through serpis' adapters exactly as a chat sends it,
   and through the simulator alongside. A missing read the simulator expects fails the run for
   explicit caches and is reported for automatic ones; rejected requests fail. Each surprise
   becomes a simulator rule and a test. The test is ignored, needs SERPIS_LIVE_TEST=1, and reads
   the key only from the environment: the test suites never touch real providers. A per-request
   debug log was dropped: sessions already keep each response's cache usage and upstream, and the
   chat flags unexplained misses itself.
   First run (2026-10-22): Claude Haiku 5.5 (via Vertex) read everything the previous request
   cached on every request, the 12-parallel-call step included; GPT-5 mini (via Azure) and
   DeepSeek v3.1 accept `prompt_cache_retention: "24h"`; OpenAI reads come in 128-token blocks
   (the chat allows for it); OpenRouter moved DeepSeek between upstreams despite the session id
   and it still read the cache (the simulator would have predicted a miss: re-check before
   changing its rule). Gemini was not checked: the account's zero-data-retention setting excludes
   Google's endpoints (now reported as skipped).
   Second run: OpenRouter again moved DeepSeek from CoreWeave to SiliconFlow, at the same request
   (the 12-parallel-call step), and this time the move cost a full rewrite: upstreams do have
   separate caches, as the simulator assumes. OpenRouter documents sticky routing keyed on our
   session id, with fallback when the sticky upstream errors or is unavailable, and expiry after
   10 minutes idle. Pinned to CoreWeave without fallbacks, that request failed with "Failed to
   apply prompt template: ... undefined and string": its DeepSeek template can't take an assistant
   message with `content: null` next to tool calls. Serpis now sends `""` there for OpenRouter
   models other than `anthropic/*` and `google/*` (`compat.emptyAssistantContentAsString`), and
   with that the conversation stays on CoreWeave and reads the cache on every request: OpenRouter's
   sticky routing works. The live check now warns whenever the upstream changes mid-conversation.
   The 10-minute expiry matters more: after a longer pause a multi-upstream model can land on an
   upstream whose cache is cold while the 1h entry is still alive elsewhere. If that shows up, an
   opt-in, off-by-default preference for the chat's last upstream (`provider.order`, fallbacks
   allowed; it replaces OpenRouter's stickiness) is the fix.

8. **Compaction** (*done*; serpis had none, so long chats failed at the context window): pi's
   triggers (`chat-compact-auto`: near the window, leaving `chat-compact-reserve-tokens`; after a
   context-overflow error, then the turn is retried once) and `chat-compact`, keeping
   `chat-compact-keep-tokens` of recent messages; pi-format `compaction` session entries. Unlike
   pi, which sends the old conversation re-serialized as text under its own system prompt (the
   whole history at full input price), serpis asks for the summary in the conversation itself,
   through `serpis_agent::complete`, the agent's own request path, with the chat's own retention
   (`none` would send no cache markers, and nothing could be read). Normally the request is the
   whole conversation plus one instruction naming the kept messages: an extension of the last
   request, which every cache serves. After an overflow the whole conversation doesn't fit, so it
   is the messages before the cut plus the instruction: a prefix of what was sent, read by caches
   that keep earlier entries. The live check showed why both: Claude (Vertex) and GPT-5 mini
   (Azure) read the prefix request, but DeepInfra's DeepSeek keeps only a conversation's latest
   whole prompt and read 1 token of it (now the simulator's `latest_prompt_only`, with a test).
   A later run on DeepSeek via CoreWeave missed twice mid-conversation, then read an earlier
   prefix in full: CoreWeave keeps earlier prefixes but serves them unreliably (likely several
   machines behind one endpoint), and run to run differs, so it is noise, not a rule. In chats,
   a miss with no known cause on such automatic caches is now reported as the provider's best
   effort ("provider"), and "unexplained" is kept for explicit caches, where a miss with nothing
   changed would point at serpis. The next
   request misses once, named "compacted". Hooks: `before-compact` (cancel, or supply a summary),
   `chat-compacted`. Tests: `serpis_agent::compaction` units, an agent scenario on every venue,
   `tests/ui/compact.lua` (manual, automatic, overflow and retry, resume after compaction), and a
   side-request step in the live check. The simulator learned that a request no longer than the
   previous one that branches off the shared prefix is not a miss.

Deferred until usage data asks for them:

- **Cache warmer** (pi 1.3's `cache-warmer.js`: refresh at 90% of the TTL when the expected saving
  is at least $0.05). With 1h retention it only helps tool runs or pauses longer than an hour. It
  is the next step if the usage data shows expiries.
- Sharing the stable prefix across chats: worth about $0.02 per new chat for a ~5K-token prefix,
  because serpis chats are long, unlike the short CI jobs in Cloudflare's post.
- Staggered parallel writes (needs sub-agents first), tool-usage stats, per-effort cost reports.

Sources: serpis' request path (2026-10-21), pi 1.3.0's `cache-warmer.js`, and Cloudflare's
"obsessive inference optimization" post.

## Chats and terminals on other machines, and the iOS app

Goal: start chats and terminals on your laptop or on dedicated boxes, pick the machine from a
list, move a chat (with its folder) between machines and resume it anywhere, and use a
chat-only iOS app on the same chats, the way t3code does. Agreed 2026-10-22.

| Topic | Decision |
|---|---|
| Where a chat runs | All of it runs on its machine: agent loop, LLM calls, tools, session file. Laptop sleeps, phone attaches: the chat keeps going. |
| Engine | `serpis-chat`: a chat without a window (no egui; its dependency tree has no GUI crate). The desktop runs it in-process; boxes run it in `serpis-node`. |
| What clients see | `serpis-proto`: the transcript as `Entry`s, sent as splices ("from entry N on, it is now these") plus a `ChatState`. The desktop chat already renders only from splices, local chats included, so one path is tested by every UI test. |
| Daemon on boxes | `serpis-node`, headless and egui-free, built the way manymux builds its node: per user, owner-only Unix socket and nothing else, reached by `ssh <host> serpis-node agent` (stdio bridge), installed as a per-user service. |
| Security | serpis authenticates nothing itself. sshd decides who gets in: rayfish mesh SSH (keyless, `ssh_allow`) or your SSH keys. API keys stay on each box (its own auth.json); none cross the wire. Optional strict mode per host, after pi-env: pinned host key, no forwarding. |
| rayfish | The network and identity layer: mesh names, NAT traversal, keyless SSH, `ray status` for the machine list. No code dependency, so plain SSH works too (rayfish is pre-1.0 and unaudited). |
| manymux | Remote terminals: its client library feeds the libghostty terminal; sessions survive disconnects. Chats are not manymux sessions: its sessions are PTYs repainted through an emulator, which doesn't fit structured chat sync. |
| From pi-durable | Request ids on prompts (a retried send never runs twice; *done*), a snapshot instead of a backlog for a client that falls behind, running turns that survive a node restart, and parking a turn that keeps crashing. |
| Moving a chat | The session file (pi JSONL) moves as is; the folder by git first (the branch plus a snapshot commit of uncommitted changes, into a worktree on the target), rsync over SSH otherwise; environment variables only from an allowlist setting. |
| iOS | SwiftUI, with a Rust core over UniFFI (protocol client and SSH, its key in the Keychain / Secure Enclave). Reaches boxes over the Rayfish VPN or any SSH-reachable host. After the node works. |

Steps:

1. *Done*: the engine out of the chat buffer (`crates/serpis-chat`, with `session` and
   `cache_watch`), the wire types and framing (`crates/serpis-proto`), and the chat buffer
   rendering from splices. `crates/serpis-chat/tests/chat.rs` runs a real chat against the mock
   provider with no window and checks that a client fed JSON splices shows exactly the same
   transcript, that a retried request id doesn't run twice, and that the saved session resumes.
2. *Done*: `serpis-node` (`crates/serpis-node`): one thread owns the chats; an owner-only
   socket (0700 directory, peer uid checked); `serpis-node agent` bridges stdio for ssh and
   starts a node in its own session when none runs; open chats come back when it restarts;
   settings from `~/.serpis/node/settings.json` (the desktop's variable names); request ids, and
   a lagging client gets a fresh snapshot instead of a backlog. `scripts/build-node.sh` builds
   the one-file binary for a box (std linked statically). In the GUI (`crates/serpis/src/remote.rs`):
   `remote-hosts` and rayfish peers (`ray status --json`) as machines, one connection per
   machine with reconnects, the composer's machine pill (`chat-select-machine`) for a new chat,
   `remote-new-chat`, `remote-attach-chat`, `remote-resume-chat`, `remote-close-chat`,
   `remote-stop-node`, the machine's own models in `chat-select-model`, sidebar groups
   `folder · machine`, and remote chats reopened on start. Closing a remote chat's window only
   stops showing it. Tests: `crates/serpis-node/tests/node.rs` (socket, bridge, auto-start,
   late joiners, retries, resume, restart) and `tests/ui/remote.lua` (the whole flow with real
   input against a real node in a throwaway home, through `serpis-node agent` instead of ssh).
   Checked over real ssh and rayfish (2026-10-22): a Linux x86-64 box reached by its mesh name
   with keyless mesh SSH, `tests/ui/remote.lua` with `SERPIS_TEST_REMOTE_HOST` passes there.
   serpis installs its node on first connect (`serpis_node::deploy`): the build for `uname -sm`
   from `~/.serpis/nodes`, into `~/.serpis/bin/serpis-node-<sha256>` there, checked before every
   start and replaced when altered; a system without a build falls back to a serpis-node found
   there. The first connect to a fresh box over the mesh took 6.5 s (10.6 MB), later ones 0.2 s.
   Two wrong assumptions found on the way: rayfish logs in as the account the client names (its
   peers carry none, so `user@host` in `remote-hosts` replaces the peer), and non-interactive
   ssh has a short PATH (hence installing rather than looking for one). A newer serpis talking
   to an older running node isn't handled yet beyond a version check in hello.
   Next here: the node's own Lua config (hooks and tools on boxes); running turns that survive
   a node restart; parking a turn that keeps crashing it.
3. Remote terminals through manymux.
4. Moving and syncing chats; sessions of every machine in one list.
5. The iOS app.
