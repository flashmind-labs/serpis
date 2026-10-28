#!/usr/bin/env bash
# A machine for the app to reach in the simulator, made of throwaway parts on this one: a home
# with serpis-node in it (as the desktop leaves a machine), the mock model as its default, and
# an ssh server on 127.0.0.1 letting in a device key made for the test (and whatever the node
# adds to the home's authorized_keys: a phone paired), and a desktop pairing a second device
# key (examples/pair_desktop.rs). Nothing of yours is read or touched.
#
#   ios/test-machine.sh <dir>     prints the environment the UI tests read, and stays up
set -euo pipefail
dir="${1:?a directory for the machine}"
root="$(cd "$(dirname "$0")/.." && pwd)"
cargo build -q --manifest-path "$root/Cargo.toml" -p serpis-node -p serpis-testkit
cargo build -q --manifest-path "$root/Cargo.toml" -p serpis-node --example pair_desktop
mkdir -p "$dir/home/project/src" "$dir/home/notes" "$dir/home/.serpis/bin" "$dir/home/.ssh" "$dir/agent" "$dir/node"
printf '# project\n' > "$dir/home/project/README.md"
ln -sf "$root/target/debug/serpis-node" "$dir/home/.serpis/bin/serpis-node-current"
[ -f "$dir/device" ] || ssh-keygen -q -t ed25519 -N "" -C "serpis ui test" -f "$dir/device"
# A second device, let in only by pairing.
[ -f "$dir/phone2" ] || ssh-keygen -q -t ed25519 -N "" -C "serpis ui test 2" -f "$dir/phone2"
cp "$dir/device.pub" "$dir/home/.ssh/authorized_keys"

"$root/target/debug/serpis-mock-provider" "$dir/mock.port" "$dir/mock.log" &
mock=$!
sshd=; pair=
trap 'kill $mock ${sshd:-} ${pair:-} 2>/dev/null; HOME="$dir/home" SERPIS_NODE_DIR="$dir/node" SERPIS_NODE_SOCKET="$dir/node/node.sock" "$root/target/debug/serpis-node" stop >/dev/null 2>&1' EXIT
trap 'exit 130' INT TERM
for _ in $(seq 50); do [ -s "$dir/mock.port" ] && break; sleep 0.1; done
cat > "$dir/agent/models.json" <<JSON
{"providers":{"mock":{"name":"Mock","baseUrl":"http://127.0.0.1:$(cat "$dir/mock.port")/api/v1","api":"openai-completions","apiKey":"x","compat":{"openRouter":true},"models":[{"id":"m","name":"Mock model","contextWindow":200000}]}}}
JSON
# SERPIS_TEST_MODELS / SERPIS_TEST_MODEL: other models instead (the promo's), in place before
# anything starts the node.
[ -n "${SERPIS_TEST_MODELS:-}" ] && cp "$SERPIS_TEST_MODELS" "$dir/agent/models.json"
printf '{"session-directory":"%s/sessions","agent-model":"%s"}' "$dir" "${SERPIS_TEST_MODEL:-mock/m}" > "$dir/node/settings.json"

# What the server runs commands with: the machine's environment.
export HOME="$dir/home" SERPIS_NODE_DIR="$dir/node" SERPIS_NODE_SOCKET="$dir/node/node.sock" SERPIS_AGENT_DIR="$dir/agent" \
  SERPIS_CONFIG_DIR="$dir/config" SERPIS_TERMINAL_SOCKET="$dir/t.sock"
# Claude Code there is the stand-in (serpis-fake-claude), never a real claude.
export SERPIS_CLAUDE_CODE_PATH="$root/target/debug/serpis-fake-claude" FAKE_CLAUDE_LOG="$dir/claude.log" FAKE_CLAUDE_DIR="$dir/home"
"$root/target/debug/serpis-test-sshd" "$dir/home/.ssh/authorized_keys" "$dir/sshd.port" &
sshd=$!
for _ in $(seq 50); do [ -s "$dir/sshd.port" ] && break; sleep 0.1; done
read -r port fingerprint < "$dir/sshd.port" || true
"$root/target/debug/examples/pair_desktop" "$dir" 127.0.0.1 "$port" me "$dir/sshd.port.hostkey" &
pair=$!
for _ in $(seq 100); do [ -s "$dir/pair.link" ] && break; sleep 0.1; done
echo "SERPIS_TEST_PAIR_LINK=$(cat "$dir/pair.link")"
echo "SERPIS_TEST_PAIR_KEY=$dir/phone2"
echo "SERPIS_TEST_PAIR_DONE=$dir/pair.done"
echo "SERPIS_TEST_ADDRESS=me@127.0.0.1:$port"
echo "SERPIS_TEST_FINGERPRINT=$fingerprint"
echo "SERPIS_TEST_KEY=$dir/device"
echo "SERPIS_TEST_HOME=$dir/home"
wait $sshd
