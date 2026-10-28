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
- **Buffer** (`trait Buffer`): a named object with a major mode, a list of minor modes, buffer-local vars, `ui()` and `handle_key()`. Built-in kinds: `chat`, `terminal`, `text` (files, pages), `messages`, `help`/describe.
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

## Claude Code as a chat's agent

Goal: the same chats, on Claude Code's harness and the user's Claude subscription, the way t3code
wraps it. Started 2026-10-27.

| Topic | Decision |
|---|---|
| How | Speak the `claude` CLI's stream-json protocol directly (`-p --input-format stream-json --output-format stream-json`, `control_request`s for the handshake, permission prompts and interrupts), as the TS/Python Agent SDKs do internally. No SDK: no Node on machines, nothing but `claude` itself. |
| Where it plugs in | A provider (`claude-code`, `Model::api`) whose chats run `serpis_agent::claude_code` instead of the agent loop. It emits the same `AgentEvent`s, so the transcript, sessions, hooks, node, desktop and phone don't know the difference. Its stream events go through the Anthropic adapter's own `EventReader`. |
| Process | One per chat, kept between turns; restarted (with `--resume`) when the model, effort, permission mode or arguments change, or after it dies. Session id = the chat's id. |
| Context | Claude Code's session holds it. Turns another model answered (a chat switched to Claude Code, or one whose session isn't on this machine) go with the next prompt as text. |
| Tools | Shown under serpis's names and argument names where serpis has the tool; its other tools keep their names and get their own cards (TodoWrite, ExitPlanMode) and summaries (Task, WebFetch, Skill). |
| Permissions | `can_use_tool` → `before-tool-call` (can block) → `tool-ask` hooks (can answer) → `claude-code-permission-prompts`: `ask` (default) puts an `Ask` on the tool's entry and waits for an `Answer` request from any client (Allow, Always = Claude Code's `permission_suggestions` passed back, Deny with a reason); stopping refuses it. Its own allow/deny rules and `claude-code-permission-mode` apply first. AskUserQuestion is refused with a request to ask in plain text. |
| Models | Discovered from `initialize` (no prompt) with the other catalogs, every 6h; aliases until then. Offered only where `claude` is installed and set up (`~/.claude` exists), so throwaway homes in tests never start it. |
| Cost | Each turn's `total_cost_usd` goes on its last reply. serpis's prompt cache bookkeeping is off for these chats (what Claude Code sends isn't serpis's to know). |

*Done*: `serpis_ai::claude_code` (finding the CLI, models, discovery), `serpis_agent::claude_code`
(the engine), `Chat` running it, the four settings, cards. Tests: unit tests of the tool mapping,
diffs and replay; `crates/serpis-chat/tests/claude_code.rs` drives real chats through
`serpis-fake-claude` (serpis-testkit), a stand-in CLI: turns, one process, an edit's permission
prompt and diff, refusal by a hook and by the setting, interrupt, effort change, an API error,
`/compact`, resume, and a session missing on this machine. Checked against the real CLI
(2.1.311, Haiku 5.5) with `examples/claude_code_smoke.rs`: Write with a permission prompt, Read,
Edit with its diff, cost, resume remembering the conversation.

Next:
- *Done*: permission prompts answered by the person (proto `Ask`/`Decision`/`Answer`, the
  node, the desktop's card and bar with `chat-allow`, `chat-allow-always`, `chat-deny`;
  `tests/ui/claude_code.lua` with real keys and clicks; the phone's card above its composer,
  `testAllowingWhatClaudeCodeAsks` against the test machine's stand-in claude). Found on the
  way: machines' model lists (the node's `Models`, the desktop picker) left `claude-code/*` out;
  both use `serpis_ai::api::chat_api` now. Plan approval has its own words (`Decision::Plan
  {mode}`, sent back as a `setMode`: approve, accepting edits, or any mode; keep planning with
  what should change), on the desktop and the phone.
- *Done*: AskUserQuestion answered by the person (proto `Decision::Answers`, sent back as
  `updatedInput.answers`): its card lists the questions, `chat-answer-questions` asks them in
  the minibuffer (options, your own words, picks until Done), the phone has a form, and
  `tool-ask` handlers can answer `{answers = ...}`. With prompts not asked it is still told to
  ask in plain text.
- The phone's own lines for Claude Code's tools (todo list, subagents); it shows mapped tools
  already.
- *Done*: background work and subagents. The engine reads `claude` between runs too: its
  `system task_*` lines become `AgentEvent::Task` (a tool entry's `task`: status, usage), a
  subagent's lines (`parent_tool_use_id`) become `AgentEvent::Nested` (a tool entry's `steps`),
  and a turn it starts itself (work it left running finished) becomes `AgentEvent::Adopted`, a
  run the chat shows, records and can stop. `chat-stop-task` (`StopTask`, the CLI's
  `stop_task`). Its process tree ends with it (SIGTERM, then SIGKILL by pid and start time:
  commands it runs may sit in groups of their own). Found by probing the CLI 2.1.311 on Haiku.
  Kept for resume as custom entries (`claude-code-tool`, `claude-code-note`).
- *Done*: permission modes per chat (`ChatState::permission_mode`, `SetPermissionMode`, the CLI's
  `set_permission_mode`, its `system status` lines followed): the composer's TUI-like indicator,
  `S-TAB`, `chat-select-permission-mode`, the phone's menu; kept for resume
  (`claude-code-mode`).
- *Done*: mid-turn steering: a message sent while it works goes as `priority: next` with a uuid,
  and is the chat's (history, transcript) when its `command_lifecycle` says started; one too
  late for the turn is its next turn. Thinking: `--thinking-display summarized` by default, and
  its `system thinking_tokens` estimate in `ChatState::thinking_tokens` (a Thinking… line).
- Moving a chat carries serpis's session but not Claude Code's (`~/.claude/projects/...`); it is
  caught up with the conversation as text instead. Carry the session file along.
- *Done*: `claude-code-usage` (and `serpis.agent.claude_code_usage`): the CLI's `get_usage`
  control request (its `/usage`, experimental there: plan windows, extra usage, advice), asked
  of the chat's machine (proto `ClaudeCodeUsage`) or this one; the stand-in answers made-up
  numbers. Next there: the phone (a usage sheet from the model menu).
- *Done*: usage limits from `rate_limit_event` (and a refused reply's
  `api_error_params.rate_limit_info`): `ChatState::limit`, a warning in the composer, and at a
  limit a bar (phone: a card) with its reset; the chat goes on by itself after it
  (`continues_at`, a timer in the chat, kept as a `claude-code-limit` entry so a restarted node
  re-arms it), `SetContinueAfterLimit` to wait instead.
- The account (`initialize`'s `account`) in the model picker and header.
- Claude Code's `system/compact_boundary` and its context use in the header.

## Chats and terminals on other machines, and the iOS app

Goal: start chats and terminals on your laptop or on dedicated boxes, pick the machine from a
list, move a chat (with its folder) between machines and resume it anywhere, and use a
chat-only iOS app on the same chats, the way t3code does. Agreed 2026-10-22.

| Topic | Decision |
|---|---|
| Where a chat runs | All of it runs on its machine: agent loop, LLM calls, tools, session file. Laptop sleeps, phone attaches: the chat keeps going. |
| Engine | `serpis-chat`: a chat without a window (no egui; its dependency tree has no GUI crate), run only by `serpis-node`, this machine's included (2026-10-24: step 1 first ran local chats in the window, which kept other devices from showing or driving them). The window is a client of nodes and nothing else. |
| Config | One config for both: the node loads the same init.lua and extensions headless; chat hooks, Lua agent tools and settings take effect there, window parts (keys, commands, UI) do nothing (`serpis.where`). A window lends `serpis_lua`/`serpis_api` and its own Lua tools to the chats it shows; settings it changes go to its machine's node. |
| What clients see | `serpis-proto`: the transcript as `Entry`s, sent as splices ("from entry N on, it is now these") plus a `ChatState`. The desktop chat already renders only from splices, local chats included, so one path is tested by every UI test. |
| Daemon on boxes | `serpis-node`, headless and egui-free, per user, owner-only Unix socket and nothing else, reached by `ssh <host> serpis-node agent` (stdio bridge), installed as a per-user service. |
| Security | serpis authenticates nothing itself. sshd decides who gets in: rayfish mesh SSH (keyless, `ssh_allow`) or your SSH keys. API keys stay on each box (its own auth.json); none cross the wire. Optional strict mode per host, after pi-env: pinned host key, no forwarding. |
| rayfish | The network and identity layer: mesh names, NAT traversal, keyless SSH, `ray status` for the machine list. No code dependency, so plain SSH works too (rayfish is pre-1.0 and unaudited). |
| Terminals | A terminal host of serpis's own (`serpis-node terminals`, 2026-10-26; manymux was dropped: its separate protocol and connection, its shell environment and its upgrades restarting every terminal cost more than it gave). Its terminals go over the node connection clients already have. |
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
   **Pinned for later: release builds of the node.** Building a node on demand only works when
   serpis runs from its source with Rust installed; a clean install elsewhere has nothing to
   build from, and falls back to a serpis-node installed on the machine. The fix is a release
   pipeline publishing serpis-node for each system (Linux x86-64/arm64, macOS), which serpis
   downloads when a machine first needs one, checked against checksums built into serpis
   itself. Needs a decision on where releases are hosted.
3. Moving chats between machines (next, in this order):
   a. *Done*: node upgrades. A machine still running an older node is restarted on the new one
      when none of its chats is mid-turn, and its chats come back (`tests/upgrade.rs`).
   b. *Done* (`tests/ui/remote_move.lua`): moving a chat with its history (here to a machine, back, or between two machines) into a
      folder picked there; it continues with its session, and its id keeps provider caches
      warm. The machine it left keeps the session, marked as moved.
   c. *Done* (`serpis_node::workspace`, `tests/ui/remote_move.lua`): carrying the folder along.
      A repository goes as a bundle of its branch plus a snapshot commit of its uncommitted work
      (through a temporary index and ref: nothing of it changes), and is checked out there with
      the work uncommitted again; any other folder as a tar.gz. In 4 MB chunks over the nodes'
      own connections (Pack, Fetch, Put, Unpack, Drop; packing off the node's thread), into a
      folder that keeps its identity: a registry on each machine
      (`~/.serpis/carried.json`) says where each repository (by first commit) and branch lives
      there and the state serpis left it in, so it returns to the same folder (or the one it
      came from), beside a clone it becomes a worktree, else `remote-carry-directory`
      (`~/.serpis/worktrees/{project}/{branch}`). Changes made there meanwhile are stashed or kept
      on a branch, never lost. Follow-ups: sending only what a machine lacks (today the bundle
      holds the branch's whole history), and environment variables from an allowlist (needs
      per-chat environments in the agent's tools).
   d. *Done* (`tests/ui/remote_resume.lua`): sessions of every machine in one list, chat-resume's
      "every machine" scope: this machine's at once, the others' as they answer (listing never
      asks for a username), moved ones hidden, a picked one resumed where it is.
   e. *Done*: the node does all the work and the window is a thin client. Lua in the node
      (`serpis_node::lua`: hooks, tools, `serpis.chat` by chat id, timers, inert window API),
      protocol for it (`SetVars`, `Reload`, `ReloadModels`, `Append`, `Lend`/`ToolCall`/`ToolDone`,
      `RefreshContext`, `Rename`, `AgeCache`, client-chosen chat ids, `Detach { close }`,
      session-list `open`/`running`, chat state with session, custom entries and a clock-free
      cache), this machine's node started and upgraded by the window (`Target::Own`), the chat
      buffer reduced to a view of a node chat, and running chats marked in `chat-resume` on
      every machine (`crates/serpis-node/tests/lua.rs`, the UI suite).
4. *Done*: terminals that outlive the window, on any machine (`serpis_node::terminals`,
   `crates/serpis/src/terminals.rs`). First built on manymux; replaced (2026-10-26) by a terminal
   host of serpis's own: `serpis-node terminals`, a process beside the node (so upgrades don't
   touch shells) that owns the PTYs, starts shells with the node's environment, keeps what each
   printed last (1 MB, from a line's start) and its screen (avt), and paints a view that attaches
   with both. The node passes its clients' terminal calls and `TERMINAL` frames (raw bytes) to
   it, so terminals use the connection clients already have. Tests: `crates/serpis-node/tests/
   terminals.rs` (through `serpis-node agent`: started, typed into, the node's home, what runs in
   it, outliving the chat node, reattached with history and screen, ended),
   `tests/ui/terminal_node.lua` and its `_restore`.
5. *Done*: turns that survive their node stopping. The node notes which chats are mid-turn
   (`open.json`: path, running, times continued in a row); on start, one cut off continues from
   its session (`Chat::continue_turn`: a tool call without a result gets one saying it was cut
   off and isn't run again), says so in the chat and runs `turn-cut-off`; cut off more than
   `chat-keep-going-limit` times in a row it is parked. `chat-keep-going` and each chat's own
   choice (a `keep-going` session entry, the composer's pill) say whether it continues. A stop
   asked for (`serpis-node stop`) cuts nothing off. `remote-install-service` makes the node a
   systemd user unit or launchd agent (`serpis_node::service`) running
   `~/.serpis/bin/serpis-node-current`, which every start points at its build, so reboots and
   crashes restart it; a client finding no node starts it through the service. Tests:
   `crates/serpis-node/tests/keep_going.rs` (killed with `kill -9`: continued, parked, waiting),
   `tests/service.rs` (with a stand-in service manager), `tests/ui/keep_going.lua`. Tried with a
   real systemd (2026-10-26, Arch, systemd 262, over rayfish ssh, in a throwaway node directory):
   started through the unit, restarted 4 s after `kill -9`, and found the user's manager with no
   `XDG_RUNTIME_DIR` (rayfish's ssh doesn't go through PAM). A real reboot too (same day): the chat
   was noted mid-tool, the machine rebooted with nobody logged in (lingering on), systemd started
   the node 9 s after boot and the turn continued (`tests/reboot.rs`, opt-in, two runs around the
   reboot). Not tried yet: a real launchd. (The node's own Lua was done in 3e.)
6. The iOS app (in progress, branch `ios-app`): a polished chat app and nothing else (no Lua,
   extensions, terminals or cache detail). `crates/serpis-mobile` is its Rust core: ssh
   in-process (russh), this device's ed25519 key (kept by the app in the Keychain), host keys
   accepted by the person, and serpis's own protocol client over `serpis-node agent`, found as
   `~/.serpis/bin/serpis-node-current` (every node start points it at its build) or on PATH; it
   never installs one. Swift sees it through UniFFI (`Core`, `Listener`), requests and events
   as the protocol's JSON. `ios/` is the SwiftUI app (XcodeGen's `project.yml`;
   `ios/build-rust.sh` builds the core into an xcframework): every machine's chats in one list,
   a chat live with Markdown, tool lines, photos, the model; new chats by machine and folder;
   machines with the device key and host key checks. Tests: `crates/serpis-mobile/tests/core.rs`
   (a real ssh server in the test, a node, the mock model), `ios/ui-test.sh` (unit tests, and a
   UI test through the whole flow against `ios/test-machine.sh`, screens saved).
7. Adding machines, on the desktop (in progress, branch `add-machine`, agreed 2026-10-26: rayfish
   is one way among ssh config, Tailscale and plain ssh; machines are added and configured on the
   desktop only). `machine-add` (`crates/serpis/src/machines.rs`, and "Add a machine…" in the
   machine picker) suggests the ssh config's hosts (and its includes), Tailscale peers (not
   phones), rayfish peers, Bonjour ssh machines (dns-sd, avahi-browse) and the ones used before,
   or takes any ssh destination; logs in and handles what stands in the way: an unknown host key
   (fingerprint shown, trusted into serpis's own `~/.serpis/known_hosts`, never ssh's; logins
   check strictly), a refused key (`ssh-copy-id` in a terminal, the key on the clipboard, another
   account), a changed host key (refused; `machine-forget-host-key`). Kept in `remote-machines`;
   `machine-rename`, `machine-remove`. Tests: unit tests of the parsers in `machines.rs`,
   `tests/ui/machine_add.lua` (stand-in ssh, ssh-copy-id and tailscale).
   Pairing the phone (*done*, same branch): `phone-pair` (`crates/serpis/src/pairing.rs`)
   shows a QR code, a `serpis://pair#…` link: the machines shared (from `ssh -G`), their host
   keys trusted here (`ssh-keygen -F`), and a one-time pairing key every machine lets in for a
   few minutes only to run `serpis-node pair` (`crates/serpis-node/src/keys.rs`: `restrict`,
   forced command, `expiry-time`). The phone (`crates/serpis-mobile/src/pair.rs`, `Core::pair`;
   the app opens the link from the Camera, or scans it itself with VisionKit) pins those host
   keys, logs in to the first machine it reaches with the pairing key and offers its key; the
   node asks the desktop (`PairRequested`), which asks you, adds the phone's key on every machine
   shared (`AuthorizeKey`), answers (`PairAnswer`) and ends the pairing (`EndPairing`). Tests:
   `crates/serpis-node/tests/pairing.rs`, `crates/serpis-mobile/tests/pair.rs`,
   `tests/ui/phone_pair.lua` (the code also read back from a screenshot, by hand),
   `ios/SerpisUITests` (`testPairingWithYourComputer`, against `ios/test-machine.sh` and its
   stand-in desktop, `examples/pair_desktop.rs`). This desktop is shared too ("this machine",
   `phone-pair local`), so the phone drives the chats this window shows: its address is picked
   once (Tailscale names from `tailscale status --json`, its `.local` name and LAN address, or
   typed; kept in `phone-pair-local-address`), its host keys read from sshd's
   `/etc/ssh/ssh_host_*_key.pub`, the port from `sshd_config`, and the code says so when nothing
   answers ssh here (Remote Login off). Not done: rayfish machines aren't
   offered (the phone isn't on rayfish); ssh config the phone can't follow (ProxyJump, a
   ProxyCommand) isn't carried over. Not tried yet with a real phone camera.

### Later (left open on the way; pick up when they matter)

Terminals (step 4):
- Not tried over real ssh yet: only through the local stand-in box.
- A terminal reattached shows the last 1 MB it printed; older output is gone.
- Images drawn in a terminal (kitty graphics) don't survive a reattach: the repaint is text.
- `terminal-attach` lists this machine and the connected ones; it could list every machine the way
  `chat-resume`'s "every machine" scope does.
- What runs in a terminal is polled every second per machine; the terminal host could say when
  it changes instead.
- Editing a terminal's command line in normal state waits up to 400 ms for each echo
  (`ECHO_TIMEOUT`); over a slow link to another machine that may be too short.
- Typing in a terminal on another machine feels sluggish (nvim especially). Measured 2026-10-26
  against a laptop on the same Wi-Fi; mostly the network, not serpis:
  - serpis's own path (window → ssh → `serpis-node agent` → node → terminal host → PTY and back)
    costs about 1 ms per key without a network, 3 ms for a reply in two writes. To that machine,
    typing every 30 ms, a key comes back in 9–11 ms, the same as plain ssh.
  - At typing pace (a key every 250 ms) everything is slow, serpis or not: plain `ssh -T` 52 ms
    median (up to 200), a tty session the same, ping over the LAN 50 ms average (up to 175;
    4 ms at 30 ms intervals), even the router 16 ms. That is Wi-Fi power saving: radios doze
    between packets. The laptop had `iw dev wlan0 get power_save` on; `sudo iw dev wlan0 set
    power_save off` (NetworkManager: `wifi.powersave = 2`) would be the thing to try first. The
    Mac's side dozes too (the router numbers).
  - `ssh -T` (no tty) was suspected (no TCP_NODELAY, bulk IPQoS) and measured not to matter.
  - What serpis adds, in the window: the reply waits 5–11 ms after arriving before the buffer
    takes it, and the window draws continuously while idle (about two terminal draws every
    16 ms with nothing changing), so bytes wait for the next frame and its vsync. Drawing only
    when something changed would cut that and save power; check it doesn't slow local terminals.
  - Ideas beyond that: predictive local echo (mosh-style) for the shell prompt; a keepalive
    trickle while typing to keep radios awake (wasteful; measure first).
  - Measuring: `cargo run --release -p serpis-node --example terminal_latency -- <command that
    runs serpis-node agent there>` (GAP=250 for typing pace). Run it against a throwaway node
    there (its own HOME, SERPIS_NODE_DIR, sockets and SHELL=/bin/sh), never the real one.

Phone app:
- serpis-proto pulls in serpis-ai (and its HTTP client) for a few types; the phone carries it.
  Moving those types to a small crate would trim the app.
- Not tried on a real phone or against a real machine yet. `scripts/install-ios.sh` installs it
  (signed with your own team) but has only run `--build-only`.
- Pairing shares what the desktop's `ssh -G` says, which the phone can't always use:
  - A rayfish peer added with `machine-add` is saved as an ordinary machine (`box.net.ray`), so
    `phone-pair` offers it (it only skips peers still listed as rayfish's). Only rayfish on the
    desktop resolves that name, and its keyless mesh SSH likely ignores `authorized_keys`.
    Fix: leave out `.ray` names and say why in the picker. Sharing them for real means going
    through a machine the phone can reach, a jump host (needs ProxyJump in the phone's ssh).
  - Tailscale works if Tailscale is on on the phone (iOS apps go through its VPN). A short
    MagicDNS name (`box`) should be carried as the full name (`box.tailXXXX.ts.net`), with a
    note that the phone needs Tailscale. A machine behind Tailscale SSH doesn't check the
    phone's key in `authorized_keys`; Tailscale's rules decide, and the phone would have to try
    the "none" login method (it doesn't yet). Untested.
  - Plain ssh breaks where the config does something the phone can't: ProxyJump/ProxyCommand
    (`ssh -G` shows both: leave those out, with the reason), names only the desktop resolves
    (`/etc/hosts`, a VPN's DNS), LAN-only addresses once the phone leaves home.

Tests:
- The first desktop UI run after a rebuild sometimes fails a few checks that pass on the next
  run (a freshly built binary's first launch is slow on macOS).

Machines and nodes:
- Release builds of the node, downloaded when a machine needs one (pinned under step 2).
- Old node builds pile up in `~/.serpis/bin` on machines (one per serpis build): prune the ones
  no node runs from.
- This machine's node keeps the environment of the window that started it: API keys exported in
  a later shell don't reach it until it restarts.
- A config error can be reported twice, once by the window and once by its node.
- Carrying a folder sends the branch's whole history; send only what the machine lacks (3c).
- Environment variables from an allowlist when a chat moves (needs per-chat environments in the
  agent's tools) (3c).
- A machine's chats run that machine's own Lua config; copying this one's there isn't built.
- A node killed mid-tool leaves that tool's processes running (a `sleep`, a build): they are
  orphans nobody stops, while the continued turn carries on. Put tools in a process group the
  node's successor can find and end (or report) on start.
- Upgrades still wait for a machine's chats to be idle; with turns that continue, they could stop
  the node at a step boundary instead.
