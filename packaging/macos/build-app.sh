#!/usr/bin/env bash
# Builds Sound-Pro-<version>-macos-<arch>.dmg containing Sound Pro.app from a
# release binary. The app is ad-hoc signed (no Apple Developer ID), so
# Gatekeeper needs a right-click > Open on first launch.
# Usage: build-app.sh <sound-pro binary> <version> <output dir>
set -euo pipefail

if [ "$#" -ne 3 ]; then
    echo "usage: $0 <sound-pro binary> <version> <output dir>" >&2
    exit 2
fi
binary=$1
version=$2
out=$3
here=$(cd "$(dirname "$0")" && pwd)
arch=$(uname -m)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

app=$work/stage/Sound\ Pro.app
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
install -m 755 "$binary" "$app/Contents/MacOS/sound-pro"
sed "s/@VERSION@/$version/g" "$here/Info.plist" > "$app/Contents/Info.plist"

# Build an .icns from the 256px PNG shipped for Linux.
iconset=$work/sound-pro.iconset
mkdir -p "$iconset"
src=$here/../sound-pro.png
for size in 16 32 64 128 256; do
    sips -z "$size" "$size" "$src" --out "$iconset/icon_${size}x${size}.png" >/dev/null
done
cp "$iconset/icon_32x32.png" "$iconset/icon_16x16@2x.png"
cp "$iconset/icon_64x64.png" "$iconset/icon_32x32@2x.png"
cp "$iconset/icon_256x256.png" "$iconset/icon_128x128@2x.png"
rm "$iconset/icon_64x64.png"
iconutil -c icns "$iconset" -o "$app/Contents/Resources/sound-pro.icns"

codesign --force --deep --sign - "$app"

ln -s /Applications "$work/stage/Applications"
mkdir -p "$out"
target=$out/Sound-Pro-$version-macos-$arch.dmg
hdiutil create -volname "Sound Pro" -srcfolder "$work/stage" -ov -format UDZO "$target" >/dev/null
echo "$target"
