#!/usr/bin/env bash
# Build Muster.app (macOS) so muster launches from the Dock / Spotlight like any app.
# Usage: scripts/macos-app.sh [install-dir]   (default: ~/Applications)
set -euo pipefail
cd "$(dirname "$0")/.."
DEST="${1:-$HOME/Applications}"
cargo build --release -p muster -p muster-gui
APP="$DEST/Muster.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp target/release/muster-gui target/release/muster "$APP/Contents/MacOS/"
cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Muster</string>
  <key>CFBundleDisplayName</key><string>Muster</string>
  <key>CFBundleIdentifier</key><string>nz.muster.app</string>
  <key>CFBundleVersion</key><string>0.1.0</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleExecutable</key><string>muster-gui</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
# Also put the CLI on PATH if ~/.local/bin exists.
if [ -d "$HOME/.local/bin" ]; then ln -sf "$APP/Contents/MacOS/muster" "$HOME/.local/bin/muster"; fi
echo "Installed $APP — open it from Spotlight, or: open -a Muster --args ~/code/your-repo"
