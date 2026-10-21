#!/usr/bin/env bash
# Build Serpis.app and install it to ~/Applications, with a `serpis` command in ~/.local/bin.
# Run it again to update. Overrides: SERPIS_APP_DIR, SERPIS_BIN_DIR.
#
# The release binary links serpis' core (libserpis.dylib, a dylib so Rust plugins can load into
# it) from target/ and Rust's standard library from the toolchain, by path. The bundle carries
# both in Contents/Frameworks and points the binary at them, so it keeps working after
# `cargo clean` or a toolchain update.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
app_dir="${SERPIS_APP_DIR:-$HOME/Applications}"
bin_dir="${SERPIS_BIN_DIR:-$HOME/.local/bin}"

echo "Building (release)…"
cargo build --release -q --manifest-path "$root/Cargo.toml" -p serpis-bin
exe_src="$root/target/release/serpis"
core_src="$root/target/release/deps/libserpis.dylib"
core_id="$(otool -D "$core_src" | tail -1)"
std_name="$(otool -L "$exe_src" | awk '/@rpath\/libstd-/ {sub("@rpath/", "", $1); print $1}')"
std_src="$(rustc --print sysroot)/lib/rustlib/$(rustc -vV | sed -n 's/^host: //p')/lib/$std_name"
version="$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)"

stage="$(mktemp -d)"
trap 'command rm -rf "$stage"' EXIT
app="$stage/Serpis.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Frameworks" "$app/Contents/Resources"
exe="$app/Contents/MacOS/serpis"
cp "$exe_src" "$exe"
cp "$core_src" "$std_src" "$app/Contents/Frameworks/"
chmod u+w "$exe" "$app/Contents/Frameworks/"*

rpaths() { otool -l "$1" | awk '/cmd LC_RPATH/ {getline; getline; print $2}'; }
# The binary finds its libraries in the bundle, and only there.
install_name_tool -change "$core_id" @rpath/libserpis.dylib "$exe"
for r in $(rpaths "$exe"); do install_name_tool -delete_rpath "$r" "$exe"; done
install_name_tool -add_rpath @executable_path/../Frameworks "$exe"
core="$app/Contents/Frameworks/libserpis.dylib"
install_name_tool -id @rpath/libserpis.dylib "$core"
for r in $(rpaths "$core"); do install_name_tool -delete_rpath "$r" "$core"; done
install_name_tool -add_rpath @loader_path "$core"
install_name_tool -id "@rpath/$std_name" "$app/Contents/Frameworks/$std_name"

# Icon (assets/serpis.png; scripts/make-icon.py draws it).
iconset="$stage/serpis.iconset"
mkdir -p "$iconset"
for s in 16 32 128 256 512; do
  sips -z $s $s "$root/assets/serpis.png" --out "$iconset/icon_${s}x${s}.png" >/dev/null
  sips -z $((s * 2)) $((s * 2)) "$root/assets/serpis.png" --out "$iconset/icon_${s}x${s}@2x.png" >/dev/null
done
iconutil -c icns "$iconset" -o "$app/Contents/Resources/serpis.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Serpis</string>
  <key>CFBundleDisplayName</key><string>Serpis</string>
  <key>CFBundleIdentifier</key><string>dev.serpis.Serpis</string>
  <key>CFBundleExecutable</key><string>serpis</string>
  <key>CFBundleIconFile</key><string>serpis</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

# Ad-hoc signature (this Mac only): libraries first, then the app.
codesign --force --sign - "$app/Contents/Frameworks/"*.dylib
codesign --force --sign - "$app"

mkdir -p "$app_dir" "$bin_dir"
command rm -rf "$app_dir/Serpis.app"
mv "$app" "$app_dir/Serpis.app"

# `serpis [dir]`: start serpis from a terminal (in dir, or here), detached, with this shell's
# environment.
cat > "$bin_dir/serpis" <<SHIM
#!/bin/sh
# Installed by $root/scripts/install-macos.sh.
if [ -n "\$1" ]; then cd "\$1" || exit 1; shift; fi
nohup "$app_dir/Serpis.app/Contents/MacOS/serpis" "\$@" >/dev/null 2>&1 &
SHIM
chmod +x "$bin_dir/serpis"

echo "Installed $app_dir/Serpis.app (v$version)"
echo "Command: $bin_dir/serpis [dir]"
case ":$PATH:" in *":$bin_dir:"*) ;; *) echo "Note: $bin_dir is not on your PATH." ;; esac
