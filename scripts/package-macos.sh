#!/usr/bin/env bash
# Builds dist/Velarouter.app and a versioned DMG.
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build --release --locked -p vr-desktop -p vr-terminal -p velarouter
ARCH="$(uname -m)"
version=$(cargo metadata --locked --no-deps --format-version 1 | python3 -c 'import sys,json;print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="vr-desktop"))')
APP="dist/Velarouter.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "${CARGO_TARGET_DIR:-target}/release/velarouter-desktop" "$APP/Contents/MacOS/velarouter-desktop"
for cli in velarouter velarouter-view; do
    install -m755 "${CARGO_TARGET_DIR:-target}/release/$cli" "$APP/Contents/MacOS/$cli"
    "$APP/Contents/MacOS/$cli" --version | grep -Fx "$cli $version"
    codesign --force --sign - "$APP/Contents/MacOS/$cli"
done
cp assets/brand/app-icons/macos/Velarouter.icns "$APP/Contents/Resources/Velarouter.icns"
cp packaging/macos/Info.plist "$APP/Contents/Info.plist"
chmod +x "$APP/Contents/MacOS/velarouter-desktop"
cp LICENSE docs/clients/{DESKTOP-APP,TERMINAL}.md "$APP/Contents/Resources/"
/usr/libexec/PlistBuddy -c "Set :CFBundleVersion $version" "$APP/Contents/Info.plist"
/usr/libexec/PlistBuddy -c "Set :CFBundleShortVersionString $version" "$APP/Contents/Info.plist"
codesign --force --sign - "$APP"
codesign --verify --strict "$APP"

DMG="dist/Velarouter-${version}-${ARCH}.dmg"
rm -f "$DMG"
hdiutil create -quiet -volname "Velarouter" -srcfolder "$APP" -ov -format UDZO "$DMG"
hdiutil verify "$DMG"
echo "built $APP"
echo "built $DMG"
