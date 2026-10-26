#!/usr/bin/env bash
# The app's tests in the simulator: unit tests, and the UI tests against a machine made for them
# (test-machine.sh), with screens saved to <shots> when given.
#
#   ios/ui-test.sh [shots-dir]
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
device="${SERPIS_TEST_DEVICE:-iPhone 18 Pro}"
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
/opt/homebrew/bin/xcodegen -q
env "${args[@]}" xcodebuild -project Serpis.xcodeproj -scheme Serpis -destination "platform=iOS Simulator,name=$device" \
  -derivedDataPath build/dd -collect-test-diagnostics never test 2>&1 | grep -E "error:|Test Case .*(passed|failed)|XCTAssert|\*\* TEST|Skipped" || true
