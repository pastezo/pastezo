#!/bin/sh
# Builds Pastezo.app and a DMG for one Mac architecture.
#   scripts/bundle-macos.sh aarch64   (Apple Silicon, macOS 11+)
#   scripts/bundle-macos.sh x86_64    (Intel, macOS 10.15+)
# Output: target/<triple>/release/bundle/{Pastezo.app, Pastezo_<version>_<arch>.dmg}
set -eu
arch=${1:-$(uname -m | sed 's/arm64/aarch64/')}
triple="$arch-apple-darwin"
case $arch in
  aarch64) min_macos=11.0 ;;
  x86_64) min_macos=10.15 ;;
  *) echo "unknown arch $arch" >&2; exit 1 ;;
esac

root=$(cd "$(dirname "$0")/../../.." && pwd)
app_dir=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)

MACOSX_DEPLOYMENT_TARGET=$min_macos cargo build --release --target "$triple" -p pastezo -p pastezo-agent --manifest-path "$root/Cargo.toml"

out="$root/target/$triple/release/bundle"
app="$out/Pastezo.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Helpers" "$app/Contents/Resources"
cp "$root/target/$triple/release/Pastezo" "$app/Contents/MacOS/"
# the agent outside Contents/MacOS: there macOS would take it for the app itself
# (the same bundle id) once it has a global shortcut — a Dock icon of its own, and
# opening Pastezo would activate the agent instead of starting the window
cp "$root/target/$triple/release/pastezo-agent" "$app/Contents/Helpers/"
# symbols are not needed at runtime (the Rust toolchain's own strip may be unavailable)
strip -x "$app/Contents/MacOS/Pastezo" "$app/Contents/Helpers/pastezo-agent" 2>/dev/null || true
cp "$app_dir/icons/icon.icns" "$app/Contents/Resources/icon.icns"
# the alternative app icons (Settings → App Icon), read at runtime
mkdir -p "$app/Contents/Resources/icons"
cp "$app_dir"/icons/variants/*.png "$app/Contents/Resources/icons/"
# the font licenses travel with the fonts (they are embedded in the binary)
cp "$app_dir/fonts/MiSans-License.pdf" "$app_dir/fonts/JetBrainsMono-OFL.txt" "$app/Contents/Resources/"

cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Pastezo</string>
    <key>CFBundleDisplayName</key><string>Pastezo</string>
    <key>CFBundleIdentifier</key><string>app.pastezo</string>
    <key>CFBundleExecutable</key><string>Pastezo</string>
    <key>CFBundleIconFile</key><string>icon</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$version</string>
    <key>CFBundleVersion</key><string>$version</string>
    <key>LSMinimumSystemVersion</key><string>$min_macos</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.utilities</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSHumanReadableCopyright</key><string>Fonts: MiSans © Beijing Xiaomi Mobile Software Co., Ltd.; JetBrains Mono © The JetBrains Mono Project Authors (OFL 1.1)</string>
</dict>
</plist>
PLIST

codesign --force --deep --sign - "$app" >/dev/null
dmg="$out/Pastezo_${version}_$arch.dmg"
rm -f "$dmg"
hdiutil create -quiet -volname Pastezo -srcfolder "$app" -ov -format UDZO "$dmg"
echo "$app"
echo "$dmg"
