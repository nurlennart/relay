#!/bin/sh
# Builds Relay.app around a compiled binary.
#   packaging/macos/bundle.sh <relay binary> <version> <output directory>
# Requires macOS for sips and iconutil.
set -eu

binary=$1
version=$2
out=$3

root=$(cd "$(dirname "$0")/../.." && pwd)
app="$out/Relay.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

cp "$binary" "$app/Contents/MacOS/relay"
cp "$root/LICENSE" "$app/Contents/Resources/LICENSE"
sed "s/@VERSION@/$version/g" "$root/packaging/macos/Info.plist" > "$app/Contents/Info.plist"

iconset=$(mktemp -d)/Relay.iconset
mkdir -p "$iconset"
for size in 16 32 64 128 256; do
    sips -z $size $size "$root/assets/icon.png" --out "$iconset/icon_${size}x${size}.png" >/dev/null
done
for size in 16 32 64 128; do
    cp "$iconset/icon_$((size * 2))x$((size * 2)).png" "$iconset/icon_${size}x${size}@2x.png"
done
rm "$iconset/icon_64x64.png"
iconutil -c icns "$iconset" -o "$app/Contents/Resources/Relay.icns"

# An ad-hoc signature lets Apple silicon run the bundle; it is not notarized.
codesign --force --deep --sign - "$app"
