#!/usr/bin/env bash
# Build the iPhone app and install it on an iPhone paired with this Mac (by cable, or over the
# network once paired). Run it again to update. It signs with your own Apple developer team: a
# free Apple ID works (Xcode > Settings > Accounts), its apps then run for 7 days before this has
# to run again. Nothing of anyone else's (team, keys, machines) is used.
#
#   scripts/install-ios.sh [--build-only] [iPhone name or UDID]
#
# Overrides: SERPIS_IOS_TEAM (team id; else your one Apple Development certificate's),
# SERPIS_IOS_BUNDLE_SUFFIX (app id suffix; else ".<team id>": app ids are unique across all
# teams), SERPIS_IOS_DEVICE (which iPhone). Signing is kept in ios/Local.xcconfig (not checked
# in): edit or remove it to change.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
ios="$root/ios"
build_only=
want="${SERPIS_IOS_DEVICE:-}"
for a in "$@"; do
  case "$a" in
    --build-only) build_only=1 ;;
    -h|--help) sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) want="$a" ;;
  esac
done
fail() { echo "error: $*" >&2; exit 1; }

# What it needs.
command -v xcodebuild >/dev/null || fail "Xcode is needed (from the App Store), with its iOS platform"
xcode-select -p | grep -q '\.app/' || fail "the command line tools are selected, not Xcode: sudo xcode-select -s /Applications/Xcode.app"
command -v xcodegen >/dev/null || fail "xcodegen is needed: brew install xcodegen"
command -v cargo >/dev/null || fail "Rust is needed: https://rustup.rs"
if command -v rustup >/dev/null; then
  rustup target add aarch64-apple-ios aarch64-apple-ios-sim >/dev/null 2>&1 || fail "rustup can't add the iOS targets: rustup target add aarch64-apple-ios aarch64-apple-ios-sim"
else
  echo "note: no rustup: the aarch64-apple-ios and aarch64-apple-ios-sim targets must be installed for this Rust"
fi

# Signing: your team.
local_cfg="$ios/Local.xcconfig"
team="${SERPIS_IOS_TEAM:-}"
if [ -z "$team" ] && [ -f "$local_cfg" ]; then
  team="$(sed -n 's/^ *DEVELOPMENT_TEAM *= *\([A-Z0-9]*\).*/\1/p' "$local_cfg" | head -1)"
fi
if [ -z "$team" ]; then
  # The team of each Apple Development certificate in your keychain (its subject's OU).
  certs="$(mktemp -d)"
  trap 'command rm -rf "$certs"' EXIT
  security find-certificate -a -c "Apple Development" -p 2>/dev/null | awk -v d="$certs" '/BEGIN CERT/ {n++} {print > (d "/" n ".pem")}'
  teams="$(for f in "$certs"/*.pem; do [ -f "$f" ] && openssl x509 -in "$f" -noout -subject -nameopt sep_multiline 2>/dev/null | sed -n 's/^ *OU=//p'; done | sort -u)"
  case "$(printf '%s' "$teams" | grep -c .)" in
    0) fail "no Apple Development certificate: in Xcode > Settings > Accounts, add your Apple ID (free is fine), then Manage Certificates > + > Apple Development. Or set SERPIS_IOS_TEAM." ;;
    1) team="$teams" ;;
    *) fail "certificates of several teams ($(echo $teams)): choose one with SERPIS_IOS_TEAM=<team id>" ;;
  esac
fi
if [ ! -f "$local_cfg" ] || [ -n "${SERPIS_IOS_TEAM:-}${SERPIS_IOS_BUNDLE_SUFFIX:-}" ]; then
  cat > "$local_cfg" <<CFG
// This Mac's signing for the iPhone app (scripts/install-ios.sh wrote it; not checked in).
DEVELOPMENT_TEAM = $team
SERPIS_BUNDLE_SUFFIX = ${SERPIS_IOS_BUNDLE_SUFFIX:-.$team}
CODE_SIGN_STYLE = Automatic
CFG
fi
echo "Signing with team $team (ios/Local.xcconfig)"

# The iPhone: the one named, else the only one paired.
udid=
if [ -z "$build_only" ]; then
  devices="$(mktemp)"
  xcrun devicectl list devices --json-output "$devices" >/dev/null 2>&1 || fail "can't list devices (xcrun devicectl): is Xcode up to date?"
  found="$(xcrun python3 - "$devices" "$want" <<'PY'
import json, sys
devices = json.load(open(sys.argv[1]))["result"]["devices"]
want = sys.argv[2]
phones = [d for d in devices if d["hardwareProperties"].get("platform") == "iOS" and d["hardwareProperties"].get("reality") == "physical"]
if want:
    phones = [d for d in phones if want in (d["deviceProperties"]["name"], d["hardwareProperties"].get("udid"), d.get("identifier"))]
for d in phones:
    print("\t".join([d["hardwareProperties"].get("udid", ""), d["deviceProperties"]["name"], d["connectionProperties"].get("pairingState", ""), str(d["deviceProperties"].get("developerModeStatus", ""))]))
PY
)"
  command rm -f "$devices"
  case "$(printf '%s' "$found" | grep -c .)" in
    0) fail "${want:+no iPhone called $want: }connect your iPhone to this Mac by cable, unlock it and tap Trust (and once, pair it in Xcode > Window > Devices and Simulators)" ;;
    1) IFS=$'\t' read -r udid name pairing devmode <<<"$found" ;;
    *) fail "several iPhones: say which, by name or UDID: $(printf '%s' "$found" | cut -f2 | paste -sd, -)" ;;
  esac
  [ "$pairing" = paired ] || fail "$name isn't paired with this Mac yet: connect it by cable, unlock it and tap Trust"
  [ "$devmode" = enabled ] || echo "note: $name may need Developer Mode on (Settings > Privacy & Security > Developer Mode), then a restart"
  echo "Installing on $name"
fi

# Build (the Rust core too: Xcode runs ios/build-rust.sh first), signed for the device.
cd "$ios"
xcodegen -q
dd="$ios/build/install"
destination="${udid:+id=$udid}"
xcodebuild -project Serpis.xcodeproj -scheme Serpis -configuration Release -destination "${destination:-generic/platform=iOS}" \
  -derivedDataPath "$dd" -allowProvisioningUpdates -quiet build \
  || fail "the build failed (see above). Signing problems: open ios/Serpis.xcodeproj in Xcode once, choose your team under Signing & Capabilities, and let it make a profile"
app="$dd/Build/Products/Release-iphoneos/Serpis.app"
[ -d "$app" ] || fail "no app at $app"
if [ -n "$build_only" ]; then
  echo "Built $app"
  exit 0
fi

xcrun devicectl device install app --device "$udid" "$app" >/dev/null || fail "installing on $name failed (unlocked? Developer Mode on?)"
bundle="$(/usr/libexec/PlistBuddy -c 'Print CFBundleIdentifier' "$app/Info.plist")"
echo "Installed $bundle on $name"
if ! xcrun devicectl device process launch --device "$udid" "$bundle" >/dev/null 2>&1; then
  echo "To open it the first time, trust your developer profile on the phone: Settings > General > VPN & Device Management > your Apple ID > Trust."
fi
