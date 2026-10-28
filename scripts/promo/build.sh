#!/usr/bin/env bash
# Builds the promo video: records serpis (record.sh), then composes it (compose.py).
# Everything lands in target/promo: the take, a Python venv for the compositor, and the video.
#   scripts/promo/build.sh            record and compose
#   scripts/promo/build.sh --compose  compose the last take again (after editing compose.py)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="$root/target/promo"
mkdir -p "$out"
if [[ ! -x "$out/venv/bin/python" ]]; then
  python3 -m venv "$out/venv"
  "$out/venv/bin/pip" -q install pillow numpy
fi
if [[ "${1:-}" != "--compose" ]]; then
  cargo build -q --manifest-path "$root/Cargo.toml" -p serpis-bin -p serpis-node
  "$here/record.sh" "$out/take"
fi
"$out/venv/bin/python" "$here/compose.py" "$out/take" "$out/serpis-promo.mp4"
