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

# The demo project.
proj="$home/code/ratelimit"; mkdir -p "$proj/src" "$proj/tests"
cat > "$proj/Cargo.toml" <<'EOF'
[package]
name = "ratelimit"
version = "0.3.1"
edition = "2024"
EOF
cat > "$proj/src/limiter.rs" <<'EOF'
use std::time::{Duration, Instant};

/// A token bucket: `capacity` tokens, refilled at `rate` per second.
pub struct Limiter {
    capacity: u32,
    tokens: f64,
    rate: f64,
    last: Instant,
}

impl Limiter {
    pub fn new(capacity: u32, rate: f64) -> Self {
        Self { capacity, tokens: capacity as f64, rate, last: Instant::now() }
    }

    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now - self.last;
        self.tokens = (self.tokens + elapsed.as_secs_f64() * self.rate).min(self.capacity as f64);
        self.last = now;
    }

    pub fn try_acquire(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}
EOF
cat > "$proj/tests/limiter.rs" <<'EOF'
#[test]
fn refills_after_100ms() {
    let mut rl = ratelimit::Limiter::new(5, 10.0);
    for _ in 0..5 { assert!(rl.try_acquire()); }
    std::thread::sleep(std::time::Duration::from_millis(100));
    assert!(rl.try_acquire());
    assert!(!rl.try_acquire());
}
EOF
echo "pub mod limiter;
pub use limiter::Limiter;" > "$proj/src/lib.rs"
(cd "$proj" && git init -q -b main && git add -A && git -c user.name=demo -c user.email=demo@example.com commit -qm "Token bucket limiter")

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
