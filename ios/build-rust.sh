#!/usr/bin/env bash
# Build the app's Rust core (crates/serpis-mobile) for the iPhone and the simulator, and the
# Swift that calls it. Xcode runs it before every build; it runs by hand too.
#
# Out, under the directory given:
#   SerpisCore.xcframework   the static library for each target, with the C header and module map
#   SerpisCore.swift         the bindings, compiled into the app
set -euo pipefail

# Xcode runs it in a build environment of its own (compilers, SDK paths, flags for the app),
# which ring's C half picks up and builds against the wrong SDK with: it starts again clean.
if [ -n "${XCODE_PRODUCT_BUILD_VERSION:-}" ] && [ -z "${SERPIS_CLEAN_ENV:-}" ]; then
    exec env -i SERPIS_CLEAN_ENV=1 HOME="$HOME" USER="${USER:-}" TMPDIR="${TMPDIR:-/tmp}" \
        PATH="$HOME/.cargo/bin:/usr/bin:/bin:/usr/sbin:/sbin:/opt/homebrew/bin" "$0" "$@"
fi

out="${1:?where to put the framework and bindings}"
root="$(cd "$(dirname "$0")/.." && pwd)"

# Xcode runs this with a PATH of its own that has no cargo on it, and with the SDK of whatever
# it builds set, which ring's C half would compile against instead of the target's.
export PATH="$HOME/.cargo/bin:$PATH"
unset SDKROOT IPHONEOS_DEPLOYMENT_TARGET MACOSX_DEPLOYMENT_TARGET
export IPHONEOS_DEPLOYMENT_TARGET=17.0
# The workspace links std dynamically for the desktop's plugins (.cargo/config.toml); the app
# links it statically. RUSTFLAGS replaces the config's flags, and a target directory of its own
# keeps the two kinds of build from rebuilding each other.
export RUSTFLAGS="" CARGO_TARGET_DIR="$root/target/ios"

targets=(aarch64-apple-ios aarch64-apple-ios-sim)
for target in "${targets[@]}"; do
    cargo rustc --quiet --release --lib -p serpis-mobile --manifest-path "$root/Cargo.toml" --target "$target" --crate-type staticlib
done

# The bindings come from the compiled library (a host build, which keeps the metadata release
# builds strip), so what the app calls and what it links can't disagree.
cargo rustc --quiet --lib -p serpis-mobile --manifest-path "$root/Cargo.toml" --crate-type cdylib
generated="$out/generated"
rm -rf "$generated"
cargo run --quiet -p serpis-mobile --manifest-path "$root/Cargo.toml" --bin uniffi-bindgen -- \
    generate --library "$CARGO_TARGET_DIR/debug/libserpis_mobile.dylib" \
    --language swift --out-dir "$generated"

# Copied only when different, the framework remade only when something in it is newer: a fresh
# timestamp is Xcode recompiling and relinking the app for nothing.
same_or_copy() { cmp -s "$1" "$2" || cp "$1" "$2"; }
headers="$out/headers"
mkdir -p "$headers"
same_or_copy "$generated/SerpisCoreFFI.h" "$headers/SerpisCoreFFI.h"
same_or_copy "$generated/SerpisCoreFFI.modulemap" "$headers/module.modulemap"
same_or_copy "$generated/SerpisCore.swift" "$out/SerpisCore.swift"

framework="$out/SerpisCore.xcframework"
args=()
stale=false
[ -f "$framework/Info.plist" ] || stale=true
for target in "${targets[@]}"; do
    library="$CARGO_TARGET_DIR/$target/release/libserpis_mobile.a"
    args+=(-library "$library" -headers "$headers")
    [ "$library" -nt "$framework/Info.plist" ] && stale=true
done
for header in "$headers"/*; do
    [ "$header" -nt "$framework/Info.plist" ] && stale=true
done
if $stale; then
    rm -rf "$framework"
    xcodebuild -quiet -create-xcframework "${args[@]}" -output "$framework"
fi
