#!/usr/bin/env bash
# Run the scripted UI tests in tests/ui against a real serpis window.
# Uses throwaway config/agent dirs so your own config is untouched. SERPIS_TEST_BIN runs another
# build of serpis (a release one, to measure speed).
set -euo pipefail
# --narrow: serpis lays itself out at 760x560 in every test, however big its window is, so
# crowded layouts show (each test also fails on controls that overlap: lib.lua's T.finish).
if [[ "${1:-}" == "--narrow" ]]; then export SERPIS_TEST_SIZE=760x560; shift; fi
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo build -q --manifest-path "$root/Cargo.toml" -p serpis-bin -p serpis-testkit -p serpis-node
# Short: each test's node listens on a socket in its home, and socket paths are limited to about
# 100 bytes.
tmp="$(mktemp -d /tmp/sui.XXXXXX)"
# A throwaway HOME: tests never see your pi/serpis sessions, settings or API keys
# (~/.pi/agent is read as a fallback otherwise), and never call a real provider.
# Each test gets its own HOME (below): the window toolkit keeps the window's size there, and one
# test's leftover size must not change another's layout. A short, fixed prompt: terminal tests
# must not depend on the hostname or the checkout's folder name, which can push a command line
# past the terminal's width and wrap it.
new_home() { mkdir -p "$1"; echo "PROMPT='%# '" > "$1/.zshrc"; }
# Rust's own tools stay where they are (serpis builds its node with them when a machine needs one).
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
for v in $(compgen -e | grep -E '_(API_KEY|TOKEN)$' || true); do unset "$v"; done
# A local model (tests/ui/mock_llm.py) for tests that run agents end to end.
python3 "$root/tests/ui/mock_llm.py" "$tmp/mock.port" "$tmp/mock.log" &
mock=$!
# A simulated caching provider (crates/serpis-testkit) for the cache tests.
"$root/target/debug/serpis-mock-provider" "$tmp/cache-mock.port" "$tmp/cache-mock.log" &
cache=$!
# Terminals run in a terminal host per test home (never the real one, where your terminals
# are), which outlives the window like the chat node: all stopped at the end (a test
# and its _restore share one, and the second checks what the first left running).
stop_terminals() { for h in "$tmp"/home-*; do SERPIS_TERMINAL_SOCKET="$h/.t.sock" "$root/target/debug/serpis-node" stop-terminals >/dev/null 2>&1 || true; done; }
# A test's stand-in machine (a node in a /tmp/stbox.* home, named in <test>.out.box) is stopped
# and removed too, if the test that ends it didn't (it failed, or ran on its own).
stop_boxes() {
  for f in "$tmp"/*.box; do
    [[ -f "$f" ]] || continue
    local box; box=$(cat "$f")
    [[ "$box" == /tmp/stbox.* && -d "$box" ]] || continue
    for what in stop stop-terminals; do
      HOME="$box/home" SERPIS_NODE_DIR="$box/node" SERPIS_NODE_SOCKET="$box/node/node.sock" SERPIS_TERMINAL_SOCKET="$box/t.sock" \
        "$root/target/debug/serpis-node" "$what" >/dev/null 2>&1 || true
    done
    command rm -rf "$box"
  done
}
trap 'kill $mock $cache 2>/dev/null; stop_terminals; stop_boxes' EXIT
for _ in $(seq 50); do [[ -s "$tmp/mock.port" && -s "$tmp/cache-mock.port" ]] && break; sleep 0.1; done
export SERPIS_TEST_MOCK_URL="http://127.0.0.1:$(cat "$tmp/mock.port")/v1" SERPIS_TEST_MOCK_LOG="$tmp/mock.log"
export SERPIS_TEST_CACHE_URL="http://127.0.0.1:$(cat "$tmp/cache-mock.port")" SERPIS_TEST_CACHE_LOG="$tmp/cache-mock.log"
fail=0
for t in "${@:-vim emacs terminal terminal_esc workspaces fonts layout mouse sidebar sessions sessions_restore composer pickers chrome vim_chat jump extend terminal_images images terminal_mouse vim_wrap scroll frecency accent jump_quotes resume_scope header_model cache cache_restore compact worktree remote remote_login remote_setup config_reload remote_move remote_resume shared_chat terminal_node terminal_node_restore terminal_speed keep_going machine_add phone_pair claude_code codex_status bash_highlight log_view diff_view diff_remote diff_resume diff_resume_restore notify skills multi_click}"; do
  for name in $t; do
    out="$tmp/$name.out"
    # Each test starts from fresh state; sessions_restore checks what sessions left behind.
    state="${name%_restore}"
    [[ -d "$tmp/home-$state" ]] || new_home "$tmp/home-$state"
    # Never the real claude CLI (its login, sessions): no Claude Code, except the stand-in for
    # the test of it.
    claude=""
    [[ "$state" == claude_code ]] && claude="$root/target/debug/serpis-fake-claude"
    SERPIS_TEST_DIR="$root/tests/ui" SERPIS_TEST_OUT="$out" SERPIS_TEST_SCRIPT="$root/tests/ui/$name.lua" \
      SERPIS_CONFIG_DIR="$tmp/cfg-$state" SERPIS_AGENT_DIR="$tmp/agent-$state" HOME="$tmp/home-$state" SERPIS_TERMINAL_SOCKET="$tmp/home-$state/.t.sock" \
      SERPIS_CLAUDE_CODE_PATH="$claude" FAKE_CLAUDE_LOG="$tmp/$name.claude.log" FAKE_CLAUDE_DIR="$tmp/home-$state" \
      timeout "${SERPIS_TEST_TIMEOUT:-60}" "${SERPIS_TEST_BIN:-$root/target/debug/serpis}" >"$tmp/$name.log" 2>&1 || true
    # Chats run in this machine's node, which outlives the window: stopped with the test.
    HOME="$tmp/home-$state" "$root/target/debug/serpis-node" stop >/dev/null 2>&1 || true
    if [[ ! -s "$out" ]]; then echo "FAIL $name (no results; see $tmp/$name.log)"; fail=1; continue; fi
    sed "s/^/[$name] /" "$out"
    grep -q '^FAIL' "$out" && fail=1
  done
done
exit $fail
