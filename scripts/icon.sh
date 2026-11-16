#!/bin/sh
# Renders assets/icon/slutty.svg into assets/icon/slutty.icns.
# Needs resvg (brew install resvg); iconutil ships with macOS.
set -eu

cd "$(dirname "$0")/../assets/icon"
set_dir=$(mktemp -d)/slutty.iconset
mkdir -p "$set_dir"
for size in 16 32 128 256 512; do
    resvg -w "$size" -h "$size" slutty.svg "$set_dir/icon_${size}x${size}.png"
    resvg -w $((size * 2)) -h $((size * 2)) slutty.svg "$set_dir/icon_${size}x${size}@2x.png"
done
iconutil --convert icns --output slutty.icns "$set_dir"
rm -r "$(dirname "$set_dir")"
echo "wrote assets/icon/slutty.icns"
