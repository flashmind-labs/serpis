#!/usr/bin/env bash
# Run a serpis-node over real ssh: scripts/ssh-check.sh [host] (default localhost).
#
# The far side runs in a throwaway home under /tmp (never the account's own sessions, settings
# or keys), and host keys go to a known-hosts file of the test's own. On localhost a whole chat
# runs against the mock provider; on another host set SERPIS_SSH_TEST_NODE to serpis-node's
# path there (scripts/build-node.sh), and the test stops once the node answered.
# Needs ssh that works without a prompt: a key in authorized_keys, or rayfish's mesh SSH.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
SERPIS_SSH_TEST_HOST="${1:-localhost}" cargo test -q --manifest-path "$root/Cargo.toml" -p serpis-node --test ssh -- --ignored --nocapture
