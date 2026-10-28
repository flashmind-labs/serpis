#!/usr/bin/env bash
# Builds a promo video: records it, then composes it. The desktop one records serpis
# (record.sh, compose.py); --ios records the phone app in the simulator (ios-record.sh,
# compose_ios.py). Everything lands in target/promo: the takes, a Python venv for the
# compositors, and the videos.
#   scripts/promo/build.sh [--ios]            record and compose
#   scripts/promo/build.sh [--ios] --compose  compose the last take again (after editing a compositor)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="$root/target/promo"
ios=; compose_only=
for a in "$@"; do
  case "$a" in
    --ios) ios=1 ;;
    --compose) compose_only=1 ;;
    *) echo "unknown option $a"; exit 1 ;;
  esac
done
mkdir -p "$out"
if [[ ! -x "$out/venv/bin/python" ]]; then
  python3 -m venv "$out/venv"
  "$out/venv/bin/pip" -q install pillow numpy
fi
if [[ -n "$ios" ]]; then
  [[ -n "$compose_only" ]] || "$here/ios-record.sh" "$out/ios-take"
  "$out/venv/bin/python" "$here/compose_ios.py" "$out/ios-take" "$out/serpis-ios-promo.mp4"
else
  if [[ -z "$compose_only" ]]; then
    cargo build -q --manifest-path "$root/Cargo.toml" -p serpis-bin -p serpis-node
    "$here/record.sh" "$out/take"
  fi
  "$out/venv/bin/python" "$here/compose.py" "$out/take" "$out/serpis-promo.mp4"
fi
