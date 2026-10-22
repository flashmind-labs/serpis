#!/usr/bin/env bash
# Run the scripted UI tests in tests/ui against a real serpis window.
# Uses throwaway config/agent dirs so your own config is untouched.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo build -q --manifest-path "$root/Cargo.toml" -p serpis-bin -p serpis-testkit -p serpis-node
tmp="$(mktemp -d)"
# A throwaway HOME: tests never see your pi/serpis sessions, settings or API keys
# (~/.pi/agent is read as a fallback otherwise), and never call a real provider.
# Each test gets its own HOME (below): the window toolkit keeps the window's size there, and one
# test's leftover size must not change another's layout. A short, fixed prompt: terminal tests
# must not depend on the hostname or the checkout's folder name, which can push a command line
# past the terminal's width and wrap it.
new_home() { mkdir -p "$1"; echo "PROMPT='%# '" > "$1/.zshrc"; }
for v in $(compgen -e | grep -E '_(API_KEY|TOKEN)$' || true); do unset "$v"; done
# A local model (tests/ui/mock_llm.py) for tests that run agents end to end.
python3 "$root/tests/ui/mock_llm.py" "$tmp/mock.port" "$tmp/mock.log" &
mock=$!
# A simulated caching provider (crates/serpis-testkit) for the cache tests.
"$root/target/debug/serpis-mock-provider" "$tmp/cache-mock.port" "$tmp/cache-mock.log" &
cache=$!
trap 'kill $mock $cache 2>/dev/null' EXIT
for _ in $(seq 50); do [[ -s "$tmp/mock.port" && -s "$tmp/cache-mock.port" ]] && break; sleep 0.1; done
export SERPIS_TEST_MOCK_URL="http://127.0.0.1:$(cat "$tmp/mock.port")/v1" SERPIS_TEST_MOCK_LOG="$tmp/mock.log"
export SERPIS_TEST_CACHE_URL="http://127.0.0.1:$(cat "$tmp/cache-mock.port")" SERPIS_TEST_CACHE_LOG="$tmp/cache-mock.log"
fail=0
for t in "${@:-vim emacs terminal terminal_esc workspaces fonts layout mouse sidebar sessions sessions_restore composer pickers chrome vim_chat jump extend terminal_images images terminal_mouse vim_wrap scroll frecency accent jump_quotes resume_scope header_model cache cache_restore compact worktree remote}"; do
  for name in $t; do
    out="$tmp/$name.out"
    # Each test starts from fresh state; sessions_restore checks what sessions left behind.
    state="${name%_restore}"
    [[ -d "$tmp/home-$state" ]] || new_home "$tmp/home-$state"
    SERPIS_TEST_DIR="$root/tests/ui" SERPIS_TEST_OUT="$out" SERPIS_TEST_SCRIPT="$root/tests/ui/$name.lua" \
      SERPIS_CONFIG_DIR="$tmp/cfg-$state" SERPIS_AGENT_DIR="$tmp/agent-$state" HOME="$tmp/home-$state" \
      timeout 60 "$root/target/debug/serpis" >"$tmp/$name.log" 2>&1 || true
    if [[ ! -s "$out" ]]; then echo "FAIL $name (no results; see $tmp/$name.log)"; fail=1; continue; fi
    sed "s/^/[$name] /" "$out"
    grep -q '^FAIL' "$out" && fail=1
  done
done
exit $fail
