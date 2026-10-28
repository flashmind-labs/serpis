#!/usr/bin/env bash
# The demo project both promo recordings work on: a small Rust crate with a flaky test.
#   project.sh <dir>     makes it there, a git repo with one commit
set -euo pipefail
proj="${1:?a directory}"; mkdir -p "$proj/src" "$proj/tests"
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
