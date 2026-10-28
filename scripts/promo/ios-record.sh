#!/usr/bin/env bash
# Records the phone promo's footage: the iOS app in the simulator, driven by its PromoRecording
# UI test, against the test machine (ios/test-machine.sh) set up as a demo: the scripted model
# (demo_llm.py), the demo project in its home, and Claude Code's stand-in in its demo mode.
# Output: $1/screen.mov and $1/marks.txt ("<ms into the recording> <scene>"). Usually run by
# build.sh --ios.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
out="$1"
command rm -rf "$out"; mkdir -p "$out"
for v in $(compgen -e | grep -E '_(API_KEY|TOKEN)$' || true); do unset "$v"; done
device="${SERPIS_PROMO_DEVICE:-iPhone 18 Pro}"

tmp="$(mktemp -d /tmp/sprom.XXXX)"
python3 "$here/demo_llm.py" "$tmp/llm.port" &
llm=$!
for _ in $(seq 50); do [[ -s "$tmp/llm.port" ]] && break; sleep 0.1; done

# The machine: its chats on the demo model, and Claude Code's stand-in naming itself and
# editing as a real one would.
cat > "$tmp/models.json" <<JSON
{"providers":{"demo":{"baseUrl":"http://127.0.0.1:$(cat "$tmp/llm.port")/v1","api":"openai-completions","apiKey":"x","models":[{"id":"anthropic/claude-sonnet-5.5","name":"Claude Sonnet 5.5","reasoning":true,"contextWindow":1000000,"cost":{"input":3,"output":15}}]}}}
JSON
machine="$tmp/machine"; mkdir -p "$machine"
FAKE_CLAUDE_DEMO=1 SERPIS_TEST_MODELS="$tmp/models.json" SERPIS_TEST_MODEL="demo/anthropic/claude-sonnet-5.5" \
  "$root/ios/test-machine.sh" "$machine" > "$machine/env" 2> "$machine/log" &
pid=$!
rec=
cleanup() { [[ -n "$rec" ]] && kill -INT $rec 2>/dev/null; kill $pid $llm 2>/dev/null; wait $pid 2>/dev/null; true; }
trap cleanup EXIT
for _ in $(seq 3000); do grep -q SERPIS_TEST_HOME "$machine/env" 2>/dev/null && break; sleep 0.1; done
grep -q SERPIS_TEST_HOME "$machine/env" || { echo "the test machine didn't start:"; cat "$machine/log"; exit 1; }
"$here/project.sh" "$machine/home/ratelimit"
printf '[core]\n\tpager = cat\n' > "$machine/home/.gitconfig"

# The simulator as in a product shot: dark, 9:41, full bars.
xcrun simctl boot "$device" 2>/dev/null || true
xcrun simctl bootstatus "$device" -b >/dev/null
xcrun simctl ui "$device" appearance dark
xcrun simctl status_bar "$device" override --time 9:41 --dataNetwork wifi --wifiMode active --wifiBars 3 \
  --cellularMode active --cellularBars 4 --batteryState charged --batteryLevel 100

args=("TEST_RUNNER_SERPIS_PROMO_MARKS=$out/marks.abs")
while IFS='=' read -r k v; do args+=("TEST_RUNNER_$k=$v"); done < "$machine/env"
cd "$root/ios"
xcodegen -q
dest="platform=iOS Simulator,name=$device"
xcodebuild -project Serpis.xcodeproj -scheme Serpis -destination "$dest" -derivedDataPath build/dd build-for-testing -quiet

# Recording, then the run; the recording's start is when simctl says it started.
xcrun simctl io "$device" recordVideo --codec=h264 --force "$out/screen.mov" 2> "$tmp/rec.log" &
rec=$!
for _ in $(seq 100); do grep -q "Recording started" "$tmp/rec.log" 2>/dev/null && break; sleep 0.05; done
started=$(python3 -c 'import time; print(time.time())')
env "${args[@]}" xcodebuild -project Serpis.xcodeproj -scheme Serpis -destination "$dest" -derivedDataPath build/dd \
  -collect-test-diagnostics never test-without-building -only-testing:SerpisUITests/PromoRecording/testPromo 2>&1 | tee "$out/test.log" \
  | grep -E "error:|Test Case .*(passed|failed)|XCTAssert|Skipped" || true
kill -INT $rec; wait $rec 2>/dev/null || true; rec=
xcrun simctl status_bar "$device" clear
[[ -s "$out/marks.abs" ]] || { echo "no marks: the run failed"; exit 1; }
awk -v s="$started" '{ printf "%d %s\n", ($1 - s) * 1000, $2 }' "$out/marks.abs" > "$out/marks.txt"
cat "$out/marks.txt"
