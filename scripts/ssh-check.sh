#!/usr/bin/env bash
# Run a serpis-node over real ssh: scripts/ssh-check.sh [host] (default localhost).
#
# The far side runs in a throwaway home under /tmp (never the account's own sessions, settings
# or keys), and host keys go to a known-hosts file of the test's own. A whole chat runs there
# against the mock provider, which runs next to the node. The node is installed there by serpis
# itself, from SERPIS_SSH_TEST_NODE_BUILDS (default ~/.serpis/nodes, which
# `scripts/build-node.sh --store <target>` fills); on another host set SERPIS_SSH_TEST_MOCK to
# serpis-mock-provider's path there (SERPIS_BUILD_PACKAGES="serpis-node serpis-testkit").
# Needs ssh that works without a prompt: a key in authorized_keys, or rayfish's mesh SSH.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
host="${1:-localhost}"
case "$host" in localhost|127.0.0.1|::1) ;; *) export SERPIS_SSH_TEST_NODE_BUILDS="${SERPIS_SSH_TEST_NODE_BUILDS:-$HOME/.serpis/nodes}" ;; esac
SERPIS_SSH_TEST_HOST="$host" cargo test -q --manifest-path "$root/Cargo.toml" -p serpis-node --test ssh -- --ignored --nocapture
