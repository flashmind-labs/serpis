#!/usr/bin/env bash
# Run the scripted UI tests in tests/ui against a real serpis window.
# Uses throwaway config/agent dirs so your own config is untouched.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo build -q --manifest-path "$root/Cargo.toml" -p serpis-bin
tmp="$(mktemp -d)"
# A throwaway HOME: tests never see your pi/serpis sessions, settings or API keys
# (~/.pi/agent is read as a fallback otherwise), and never call a real provider.
mkdir -p "$tmp/home"
touch "$tmp/home/.zshrc"
for v in $(compgen -e | grep -E '_(API_KEY|TOKEN)$' || true); do unset "$v"; done
fail=0
for t in "${@:-vim emacs terminal terminal_esc workspaces fonts layout mouse sidebar sessions sessions_restore composer pickers chrome}"; do
  for name in $t; do
    out="$tmp/$name.out"
    # Each test starts from fresh state; sessions_restore checks what sessions left behind.
    state="${name%_restore}"
    SERPIS_TEST_DIR="$root/tests/ui" SERPIS_TEST_OUT="$out" SERPIS_TEST_SCRIPT="$root/tests/ui/$name.lua" \
      SERPIS_CONFIG_DIR="$tmp/cfg-$state" SERPIS_AGENT_DIR="$tmp/agent-$state" HOME="$tmp/home" \
      timeout 60 "$root/target/debug/serpis" >"$tmp/$name.log" 2>&1 || true
    if [[ ! -s "$out" ]]; then echo "FAIL $name (no results; see $tmp/$name.log)"; fail=1; continue; fi
    sed "s/^/[$name] /" "$out"
    grep -q '^FAIL' "$out" && fail=1
  done
done
exit $fail
