#!/usr/bin/env bash
# Build serpis and install it for this user, with an application-menu entry and a `serpis`
# command. Run it again to update.
#
# Overrides: SERPIS_APP_DIR, SERPIS_BIN_DIR, SERPIS_DESKTOP_DIR.
# SERPIS_NODE_BINARY puts that serpis-node beside the app instead of building one.
#
# The workspace links serpis, its core library and Rust's standard library dynamically so Rust
# plugins share the same copies. The install carries both libraries beside the executable, where
# its $ORIGIN runtime search path finds them after `cargo clean` or a toolchain update.
set -euo pipefail

fail() {
  echo "install-linux.sh: $*" >&2
  exit 1
}

[[ "$(uname -s)" == "Linux" ]] || fail "this installer only runs on Linux"

root="$(cd "$(dirname "$0")/.." && pwd)"
data_home="${XDG_DATA_HOME:-$HOME/.local/share}"
app_dir="${SERPIS_APP_DIR:-$data_home/serpis}"
bin_dir="${SERPIS_BIN_DIR:-$HOME/.local/bin}"
desktop_dir="${SERPIS_DESKTOP_DIR:-$data_home/applications}"

# The app directory is replaced as a unit below. Refuse broad paths even when an override is bad.
case "$app_dir" in
  "" | / | "$HOME" | "$data_home") fail "SERPIS_APP_DIR is too broad: $app_dir" ;;
  *"'"*) fail "SERPIS_APP_DIR cannot contain a single quote: $app_dir" ;;
  *$'\n'*) fail "SERPIS_APP_DIR cannot contain a newline" ;;
esac

command -v cargo >/dev/null || fail "Rust is needed: https://rustup.rs"
command -v rustc >/dev/null || fail "rustc is needed: https://rustup.rs"

echo "Building (release)..."
cargo build --release -q --manifest-path "$root/Cargo.toml" -p serpis-bin
node_src="${SERPIS_NODE_BINARY:-$("$root/scripts/build-node.sh")}"
exe_src="$root/target/release/serpis"
core_src="$root/target/release/libserpis.so"
std_dir="$(rustc --print target-libdir)"
std_src="$(find "$std_dir" -maxdepth 1 -type f -name 'libstd-*.so' -print -quit)"

[[ -x "$exe_src" ]] || fail "the build did not produce $exe_src"
[[ -f "$core_src" ]] || fail "the build did not produce $core_src"
[[ -x "$node_src" ]] || fail "no executable serpis-node at $node_src"
[[ -n "$std_src" && -f "$std_src" ]] || fail "could not find Rust's shared standard library in $std_dir"

stage="$(mktemp -d)"
trap 'command rm -rf "$stage"' EXIT
app="$stage/serpis"
mkdir -p "$app"
cp "$exe_src" "$app/serpis"
cp "$node_src" "$app/serpis-node"
cp "$core_src" "$std_src" "$app/"
cp "$root/assets/serpis.png" "$app/serpis.png"
chmod +x "$app/serpis" "$app/serpis-node"

mkdir -p "$(dirname "$app_dir")" "$bin_dir" "$desktop_dir"
command rm -rf "$app_dir"
mv "$app" "$app_dir"

# `serpis [dir]`: start the GUI in dir (or here), detached from this terminal.
# Single-quote its installed path so shell metacharacters in an overridden app directory stay
# literal in the generated launcher.
shell_exe="'$app_dir/serpis'"
cat >"$bin_dir/serpis" <<SHIM
#!/bin/sh
# Installed by $root/scripts/install-linux.sh.
if [ -n "\${1:-}" ]; then cd "\$1" || exit 1; shift; fi
nohup $shell_exe "\$@" >/dev/null 2>&1 &
SHIM
chmod +x "$bin_dir/serpis"

# Exec values use desktop-entry quoting, which requires these four characters to be escaped
# inside double quotes.
desktop_exec="$app_dir/serpis"
desktop_exec="${desktop_exec//\\/\\\\}"
desktop_exec="${desktop_exec//\"/\\\"}"
desktop_exec="${desktop_exec//\`/\\\`}"
desktop_exec="${desktop_exec//\$/\\\$}"
cat >"$desktop_dir/serpis.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Serpis
Comment=Agent harness in an Emacs-shaped GUI
Exec="$desktop_exec"
Icon=$app_dir/serpis.png
Terminal=false
Categories=Development;IDE;
StartupNotify=true
StartupWMClass=serpis
DESKTOP
chmod 644 "$desktop_dir/serpis.desktop"

if command -v update-desktop-database >/dev/null; then
  update-desktop-database "$desktop_dir" >/dev/null 2>&1 || true
fi

version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"
echo "Installed Serpis $version in $app_dir"
echo "Application entry: $desktop_dir/serpis.desktop"
echo "Command: $bin_dir/serpis [dir]"
case ":$PATH:" in *":$bin_dir:"*) ;; *) echo "Note: $bin_dir is not on your PATH." ;; esac
