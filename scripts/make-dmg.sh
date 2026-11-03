#!/usr/bin/env bash
# Package a built Serpis.app as a disk image, laid out by scripts/dmg-settings.py. dmgbuild writes
# the window's layout itself, so it needs no Finder and works on a machine nobody is logged in to.
# It is installed for this run only.
#
#   scripts/make-dmg.sh path/to/Serpis.app out/Serpis.dmg
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
app="$1"
out="$2"
tools="$(mktemp -d)"
trap 'command rm -rf "$tools"' EXIT
python3 -m venv "$tools"
"$tools/bin/pip" install -q --disable-pip-version-check dmgbuild==1.6.7 ds_store==1.3.3 mac_alias==2.2.3
mkdir -p "$(dirname "$out")"
# hdiutil, underneath, can fail with "Resource busy" while macOS is still looking at a new folder.
for attempt in 1 2 3 4 5; do
  "$tools/bin/dmgbuild" -s "$root/scripts/dmg-settings.py" -D "app=$app" Serpis "$out" >/dev/null && break
  [[ $attempt -lt 5 ]] || exit 1
  sleep $((attempt * 5))
done
hdiutil verify -quiet "$out"
echo "$out"
