#!/usr/bin/env bash
# Run the scripted UI tests in tests/ui against a real serpis window.
# Uses throwaway config/agent dirs so your own config is untouched.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo build -q --manifest-path "$root/Cargo.toml" -p serpis-bin
tmp="$(mktemp -d)"
fail=0
for t in "${@:-vim emacs terminal workspaces fonts layout}"; do
  for name in $t; do
    out="$tmp/$name.out"
    SERPIS_TEST_DIR="$root/tests/ui" SERPIS_TEST_OUT="$out" SERPIS_TEST_SCRIPT="$root/tests/ui/$name.lua" \
      SERPIS_CONFIG_DIR="$tmp/cfg" SERPIS_AGENT_DIR="$tmp/agent" \
      timeout 60 "$root/target/debug/serpis" >"$tmp/$name.log" 2>&1 || true
    if [[ ! -s "$out" ]]; then echo "FAIL $name (no results; see $tmp/$name.log)"; fail=1; continue; fi
    sed "s/^/[$name] /" "$out"
    grep -q '^FAIL' "$out" && fail=1
  done
done
exit $fail
