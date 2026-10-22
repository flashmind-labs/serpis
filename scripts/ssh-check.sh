#!/usr/bin/env bash
# Run a serpis-node over real ssh: scripts/ssh-check.sh [host] (default localhost).
#
# The far side runs in a throwaway home under /tmp (never the account's own sessions, settings
# or keys), and host keys go to a known-hosts file of the test's own. A whole chat runs there
# against the mock provider, which runs next to the node: on another host set
# SERPIS_SSH_TEST_NODE and SERPIS_SSH_TEST_MOCK to serpis-node and serpis-mock-provider there
# (SERPIS_BUILD_PACKAGES="serpis-node serpis-testkit" scripts/build-node.sh <target>).
# Needs ssh that works without a prompt: a key in authorized_keys, or rayfish's mesh SSH.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
SERPIS_SSH_TEST_HOST="${1:-localhost}" cargo test -q --manifest-path "$root/Cargo.toml" -p serpis-node --test ssh -- --ignored --nocapture
