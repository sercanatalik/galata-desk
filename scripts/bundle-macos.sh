#!/bin/sh
# Build galata-desk as a macOS app bundle: target/bundle/Galata Desk.app
#
# The bundle carries the app icon (assets/AppIcon.icns), so Finder, the Dock
# and Launchpad show the Galata Tower. It is signed ad hoc, which is enough to
# run it on the machine that built it; distributing it needs a real identity.
set -eu

cd "$(dirname "$0")/.."

version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app="target/bundle/Galata Desk.app"

cargo build --release -p galata-desk

rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/galata-desk "$app/Contents/MacOS/galata-desk"
cp assets/AppIcon.icns "$app/Contents/Resources/AppIcon.icns"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Galata Desk</string>
  <key>CFBundleDisplayName</key><string>Galata Desk</string>
  <key>CFBundleIdentifier</key><string>dev.galata.desk</string>
  <key>CFBundleExecutable</key><string>galata-desk</string>
  <key>CFBundleIconFile</key><string>AppIcon</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.finance</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST

codesign --force --sign - "$app" >/dev/null 2>&1 || echo "note: the bundle could not be signed; it may need approving on first launch" >&2

echo "$app"
