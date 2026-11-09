#!/bin/bash
# Installs the latest R-Browser on macOS.
#   curl -fsSL https://rahul428-2.github.io/r-browser/install.sh | bash
# Files fetched with curl aren't quarantined, so macOS opens the app without the
# "damaged" / "can't be verified" prompt that browser downloads of un-notarized apps get.
set -euo pipefail

URL="https://github.com/rahul428-2/r-browser/releases/latest/download/R-Browser.app.tar.gz"

[ "$(uname)" = Darwin ] || { echo "This installer is for macOS. Windows: https://rahul428-2.github.io/r-browser/"; exit 1; }
major=$(sw_vers -productVersion | cut -d. -f1)
[ "$major" -ge 14 ] || { echo "R-Browser needs macOS 14 or later (this Mac has $(sw_vers -productVersion))."; exit 1; }

# /Applications is writable for admin users; everyone else gets ~/Applications.
dest=/Applications
[ -w "$dest" ] || { dest="$HOME/Applications"; mkdir -p "$dest"; }

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

echo "Downloading R-Browser..."
curl -fL --progress-bar "$URL" -o "$tmp/r.tar.gz"
tar -xzf "$tmp/r.tar.gz" -C "$tmp"

app="$tmp/R-Browser.app"
arch=$(uname -m)
if ! lipo -archs "$app/Contents/MacOS/r-browser" | grep -qw "$arch"; then
  echo "The current release doesn't run on this Mac's processor ($arch) yet."
  exit 1
fi

osascript -e 'quit app "R-Browser"' 2>/dev/null || true
rm -rf "$dest/R-Browser.app"
mv "$app" "$dest/"
xattr -dr com.apple.quarantine "$dest/R-Browser.app" 2>/dev/null || true

echo "Installed to $dest/R-Browser.app"
open "$dest/R-Browser.app"
