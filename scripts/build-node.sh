#!/usr/bin/env bash
# Build serpis-node for a box: release, with std linked statically, so the binary is one file
# that runs without a Rust toolchain. Copy it anywhere on the box's PATH for non-interactive
# ssh (e.g. ~/.local/bin, and check `ssh box 'command -v serpis-node'`).
#
#   scripts/build-node.sh                      # for this machine
#   scripts/build-node.sh x86_64-unknown-linux-gnu.2.28   # another target (here: Linux with
#       glibc 2.28 or newer, 2018 on): needs `cargo zigbuild` (cargo install cargo-zigbuild;
#       zig is already a serpis build dependency) and `rustup target add` for it
#   SERPIS_BUILD_PACKAGES="serpis-node serpis-testkit" ...   # also the mock provider (tests)
#   scripts/build-node.sh --store [target]   # and keep it where serpis installs nodes from
#       (~/.serpis/nodes/<version>/<target>/serpis-node, or $SERPIS_NODE_STORE/<target>/...);
#       serpis does this itself the first time a machine of that system needs one
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
store=""
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
if [[ "${1:-}" == "--store" ]]; then store="${SERPIS_NODE_STORE:-$HOME/.serpis/nodes/$version}"; shift; fi
# The workspace links std dynamically for the desktop's plugins (.cargo/config.toml); the node
# loads none. RUSTFLAGS replaces the config's flags (a --config array would add to them), and a
# target directory of its own keeps the two kinds of build from rebuilding each other.
export RUSTFLAGS="" CARGO_TARGET_DIR="$root/target/node"
packages=()
for p in ${SERPIS_BUILD_PACKAGES:-serpis-node}; do packages+=(-p "$p"); done
if [[ $# -gt 0 ]]; then
  cargo zigbuild --release -q --manifest-path "$root/Cargo.toml" "${packages[@]}" --target "$1"
  # A glibc version suffix (.2.28) is zigbuild's, not part of the target's directory.
  out="$CARGO_TARGET_DIR/${1%.2.*}/release/serpis-node"
else
  cargo build --release -q --manifest-path "$root/Cargo.toml" "${packages[@]}"
  out="$CARGO_TARGET_DIR/release/serpis-node"
fi
if [[ -n "$store" ]]; then
  triple="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
  triple="${triple%.2.*}"
  mkdir -p "$store/$triple"
  # Replaced, not overwritten in place: a node running from the old file keeps its copy.
  cp "$out" "$store/$triple/.serpis-node.new" && mv -f "$store/$triple/.serpis-node.new" "$store/$triple/serpis-node"
  out="$store/$triple/serpis-node"
fi
echo "$out"
