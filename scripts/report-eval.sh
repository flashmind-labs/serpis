#!/usr/bin/env bash
# The report eval (evals/report/README.md): serpis's agent does real tasks on fresh clones of this
# repository, once per variant of the report skill, and each report it ends with is scored as
# serpis reads it. It spends real money on OpenRouter, with the key serpis has (OPENROUTER_API_KEY
# wins). Arguments go to crates/serpis/examples/report_eval.rs, e.g.:
#   scripts/report-eval.sh --n 10 --variants current,proposed
#   scripts/report-eval.sh --score target/report-eval/<run>     (score a run again, no calls)
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
exec cargo run -q --manifest-path "$root/Cargo.toml" -p serpis --example report_eval -- "$@"
