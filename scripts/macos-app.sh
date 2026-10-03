#!/bin/sh
# Builds Reqlite.app and Reqlite.dmg in target/macos/, for use on this Mac.
#
# Usage: scripts/macos-app.sh
#
# The app is signed ad-hoc. That is enough on the Mac that built it. To share
# it, the app needs a Developer ID signature and Apple notarization.
set -eu

cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -n 1)
out=target/macos
app="$out/Reqlite.app"

cargo build --release -p reqlite-gui

rm -rf "$out"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/reqlite-gui "$app/Contents/MacOS/reqlite-gui"
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Reqlite</string>
  <key>CFBundleDisplayName</key><string>Reqlite</string>
  <key>CFBundleIdentifier</key><string>io.github.officialaritro.reqlite</string>
  <key>CFBundleExecutable</key><string>reqlite-gui</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
codesign --force --sign - "$app"

# The disk image holds the app and a link to /Applications to drag it onto.
stage="$out/dmg"
mkdir -p "$stage"
cp -R "$app" "$stage/"
ln -s /Applications "$stage/Applications"
hdiutil create -quiet -volname Reqlite -srcfolder "$stage" -ov -format UDZO "$out/Reqlite.dmg"
rm -rf "$stage"

echo "built $app and $out/Reqlite.dmg ($version)"
