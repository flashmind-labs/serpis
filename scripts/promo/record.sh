#!/usr/bin/env bash
# Records the promo's footage: serpis in a throwaway home, on a demo project, against the
# scripted model (demo_llm.py). Output: $1/frames/*.png and $1/marks.txt. Usually run by
# build.sh.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="$1"
command rm -rf "$out"; mkdir -p "$out/frames"
tmp="$(mktemp -d /tmp/sdemo.XXXX)"
home="$tmp/home"; mkdir -p "$home" "$tmp/agent"; echo "PROMPT='%F{blue}%1~%f %# '" > "$home/.zshrc"
export RUSTUP_HOME="${RUSTUP_HOME:-$HOME/.rustup}" CARGO_HOME="${CARGO_HOME:-$HOME/.cargo}"
for v in $(compgen -e | grep -E '_(API_KEY|TOKEN)$' || true); do unset "$v"; done

# git as a user has it: colors (its default on a terminal), and no pager to get stuck in.
printf '[core]\n\tpager = cat\n[color]\n\tui = auto\n' > "$home/.gitconfig"

# The demo project.
proj="$home/code/ratelimit"
"$here/project.sh" "$proj"

python3 "$here/demo_llm.py" "$tmp/llm.port" &
llm=$!
trap 'kill $llm 2>/dev/null; HOME="$home" "$root/target/debug/serpis-node" stop >/dev/null 2>&1; HOME="$home" SERPIS_TERMINAL_SOCKET="$home/.t.sock" "$root/target/debug/serpis-node" stop-terminals >/dev/null 2>&1; true' EXIT
for _ in $(seq 50); do [[ -s "$tmp/llm.port" ]] && break; sleep 0.1; done

# A tiling window manager would size the window itself: under AeroSpace it floats once it
# shows, so it can take the recording's size.
command -v aerospace >/dev/null && ( for _ in $(seq 60); do
    id=$(aerospace list-windows --all --format '%{window-id} %{app-name}' 2>/dev/null | awk '$2 ~ /^serpis$/ {print $1; exit}')
    if [[ -n "$id" ]]; then aerospace layout floating --window-id "$id" 2>/dev/null; exit; fi
    sleep 0.2
  done ) &

cd "$proj"
DEMO_OUT="$out" DEMO_LLM_URL="http://127.0.0.1:$(cat "$tmp/llm.port")/v1" \
  SERPIS_TEST_DIR="$root/tests/ui" SERPIS_TEST_SCRIPT="$here/record.lua" \
  SERPIS_CONFIG_DIR="$tmp/cfg" SERPIS_AGENT_DIR="$tmp/agent" HOME="$home" SERPIS_TERMINAL_SOCKET="$home/.t.sock" \
  SERPIS_CLAUDE_CODE_PATH="" timeout 120 "$root/target/debug/serpis" >"$out/serpis.log" 2>&1 || true
ls "$out/frames" | wc -l
