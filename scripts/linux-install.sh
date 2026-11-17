#!/bin/sh
# Installs Pitwall for this user from the unpacked release tarball: the
# binaries into ~/.local/bin, and a menu entry with its icon.
set -e
here=$(cd "$(dirname "$0")" && pwd)
bin="${XDG_BIN_HOME:-$HOME/.local/bin}"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
mkdir -p "$bin" "$data/applications" "$data/icons/hicolor/256x256/apps"
install -m 755 "$here/pitwall" "$here/pitwall-gui" "$bin/"
install -m 644 "$here/pitwall.png" "$data/icons/hicolor/256x256/apps/pitwall.png"
sed "s#^Exec=pitwall-gui#Exec=$bin/pitwall-gui#" "$here/pitwall.desktop" > "$data/applications/pitwall.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications" || true
echo "Installed to $bin. Run pitwall-gui, or find Pitwall in your app menu."
case ":$PATH:" in *":$bin:"*) ;; *) echo "Add $bin to your PATH to run pitwall from a terminal." ;; esac
