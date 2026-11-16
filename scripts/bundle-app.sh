#!/bin/sh
# Builds slutty.app from a release build. With --install, copies it to
# ~/Applications.
set -eu

cd "$(dirname "$0")/.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app=target/release/slutty.app

cargo build --release --locked
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp target/release/slutty "$app/Contents/MacOS/slutty"
cp assets/icon/slutty.icns "$app/Contents/Resources/slutty.icns"
cat > "$app/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleExecutable</key><string>slutty</string>
    <key>CFBundleIconFile</key><string>slutty</string>
    <key>CFBundleIdentifier</key><string>io.github.ae2rs.slutty</string>
    <key>CFBundleName</key><string>slutty</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>13.0</string>
    <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
EOF
# Ad-hoc signature: Apple silicon refuses to run unsigned bundles.
codesign --force --sign - "$app"

if [ "${1:-}" = "--install" ]; then
    mkdir -p "$HOME/Applications"
    rm -rf "$HOME/Applications/slutty.app"
    cp -R "$app" "$HOME/Applications/"
    # Finder and the Dock cache icons by modification date.
    touch "$HOME/Applications/slutty.app"
    echo "installed $HOME/Applications/slutty.app"
else
    echo "built $app"
fi
