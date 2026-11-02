#!/usr/bin/env bash
# Package a built Serpis.app as a disk image: the app beside a link to /Applications, to drag it onto.
#
#   scripts/make-dmg.sh path/to/Serpis.app out/Serpis.dmg
set -euo pipefail
app="$1"
out="$2"
stage="$(mktemp -d)"
trap 'command rm -rf "$stage"' EXIT
# ditto keeps the app's links and signatures.
ditto "$app" "$stage/Serpis.app"
ln -s /Applications "$stage/Applications"
mkdir -p "$(dirname "$out")"
# hdiutil can fail with "Resource busy" while macOS is still looking at the new folder.
for attempt in 1 2 3 4 5; do
  hdiutil create -quiet -ov -volname Serpis -srcfolder "$stage" -format UDZO "$out" && break
  [[ $attempt -lt 5 ]] || exit 1
  sleep $((attempt * 5))
done
hdiutil verify -quiet "$out"
echo "$out"
