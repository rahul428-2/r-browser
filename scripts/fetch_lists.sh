#!/bin/sh
# Refresh the bundled filter lists and the Tranco top-10k list. Run from the repo root.
set -eu
d=src-tauri/resources/lists
mkdir -p "$d"
get() { curl -fsSL --retry 3 -o "$d/$1.tmp" "$2" && mv "$d/$1.tmp" "$d/$1"; }
get easylist.txt    https://easylist.to/easylist/easylist.txt
get easyprivacy.txt https://easylist.to/easylist/easyprivacy.txt
get ublock.txt      https://ublockorigin.github.io/uAssets/filters/filters.txt
get peterlowe.txt   "https://pgl.yoyo.org/adservers/serverlist.php?hostformat=adblockplus&showintro=0&mimetype=plaintext"
get nocoin.txt      https://raw.githubusercontent.com/hoshsadiq/adblock-nocoin-list/master/nocoin.txt
t=$(mktemp -d)
curl -fsSL --retry 3 -o "$t/top.zip" https://tranco-list.eu/top-1m.csv.zip
unzip -p "$t/top.zip" | head -n 10000 | cut -d, -f2 | tr -d '\r' > src-tauri/resources/top10k.txt
rm -rf "$t"
wc -l "$d"/*.txt src-tauri/resources/top10k.txt
