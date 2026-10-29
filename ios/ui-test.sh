#!/usr/bin/env bash
# The app's tests in the simulator: unit tests, and the UI tests against a machine made for them
# (test-machine.sh), with screens saved to <shots> when given.
#
#   ios/ui-test.sh [shots-dir]
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
# The simulator: SERPIS_TEST_DEVICE, else an iPhone one that is running, else a Pro, else any.
sims="$(xcrun simctl list devices available | grep -E '^ +iPhone')"
pick() { sed -n 's/^ *\(iPhone[^(]*\) (.*/\1/p' | sed 's/ *$//' | head -1; }
device="${SERPIS_TEST_DEVICE:-$( { grep Booted <<<"$sims"; grep ' Pro (' <<<"$sims"; echo "$sims"; } | pick)}"
[ -n "$device" ] || { echo "no iPhone simulator: add one in Xcode (Window > Devices and Simulators)"; exit 1; }
command -v xcodegen >/dev/null || { echo "xcodegen is needed: brew install xcodegen"; exit 1; }
machine="$(mktemp -d /tmp/sim.XXXXXX)"
"$here/test-machine.sh" "$machine" > "$machine/env" 2> "$machine/log" &
pid=$!
cleanup() { kill $pid 2>/dev/null; wait $pid 2>/dev/null; rm -rf "$machine"; }
trap cleanup EXIT
trap 'exit 130' INT TERM
for _ in $(seq 3000); do grep -q SERPIS_TEST_HOME "$machine/env" 2>/dev/null && break; sleep 0.1; done
grep -q SERPIS_TEST_HOME "$machine/env" || { echo "the test machine didn't start:"; cat "$machine/log"; exit 1; }
args=()
while IFS='=' read -r k v; do args+=("TEST_RUNNER_$k=$v"); done < "$machine/env"
if [ -n "${1:-}" ]; then mkdir -p "$1"; args+=("TEST_RUNNER_SERPIS_SHOTS=$(cd "$1" && pwd)"); fi
cd "$here"
xcodegen -q
# Into a file, then read: through a pipe, the reader would wait for every process xcodebuild
# started to let go of it, and some (the simulator's) outlive it.
env "${args[@]}" xcodebuild -project Serpis.xcodeproj -scheme Serpis -destination "platform=iOS Simulator,name=$device" \
  -derivedDataPath build/dd -collect-test-diagnostics never test > "$machine/xcodebuild.log" 2>&1 || true
grep -E "error:|Test Case .*(passed|failed)|XCTAssert|\*\* TEST|Skipped" "$machine/xcodebuild.log" || true
