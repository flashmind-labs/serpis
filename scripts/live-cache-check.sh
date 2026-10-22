#!/usr/bin/env bash
# Prompt caching end to end against real providers through OpenRouter (crates/serpis-ai/tests/
# live_cache.rs). It spends real money (a few cents at most per run) with the key in OPENROUTER_API_KEY;
# no key file is read. SERPIS_LIVE_MODELS=a/b,c/d picks other models; SERPIS_LIVE_ROUTING='{...}'
# sets OpenRouter's provider routing (e.g. {"order":["coreweave"],"allow_fallbacks":false}).
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
if [[ -z "${OPENROUTER_API_KEY:-}" ]]; then
  echo "OPENROUTER_API_KEY is not set: export it to run the live cache check." >&2
  exit 2
fi
SERPIS_LIVE_TEST=1 cargo test --manifest-path "$root/Cargo.toml" -p serpis-ai --test live_cache -- --ignored --nocapture
