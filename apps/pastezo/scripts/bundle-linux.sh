#!/bin/sh
# Builds Pastezo for Linux (x86_64 or aarch64, the machine's own) into a tarball.
# Run on Linux: apps/pastezo/scripts/bundle-linux.sh
# Output: target/release/bundle/Pastezo_<version>_linux_<arch>.tar.gz, laid out like /usr:
#   bin/Pastezo, bin/pastezo-agent
#   share/pastezo/icons/                    alternative app icons (Settings → App Icon)
#   share/applications/app.pastezo.desktop  the launcher entry
#   share/icons/hicolor/512x512/apps/app.pastezo.png
#   share/doc/pastezo/MiSans-License.pdf    the font licenses travel with the fonts
#   share/doc/pastezo/JetBrainsMono-OFL.txt
#   install.sh                              copies it all into ~/.local
set -eu
root=$(cd "$(dirname "$0")/../../.." && pwd)
app_dir=$(cd "$(dirname "$0")/.." && pwd)
version=$(sed -n 's/^version = "\(.*\)"/\1/p' "$root/Cargo.toml" | head -1)
arch=$(uname -m)

cargo build --release -p pastezo -p pastezo-agent --manifest-path "$root/Cargo.toml"

out="$root/target/release/bundle"
name="Pastezo_${version}_linux_$arch"
dir="$out/$name"
rm -rf "$dir"
mkdir -p "$dir/bin" "$dir/share/pastezo/icons" "$dir/share/applications" \
  "$dir/share/icons/hicolor/512x512/apps" "$dir/share/doc/pastezo"
cp "$root/target/release/Pastezo" "$root/target/release/pastezo-agent" "$dir/bin/"
strip "$dir/bin/Pastezo" "$dir/bin/pastezo-agent" 2>/dev/null || true
cp "$app_dir"/icons/variants/*.png "$dir/share/pastezo/icons/"
cp "$app_dir/icons/icon.png" "$dir/share/icons/hicolor/512x512/apps/app.pastezo.png"
cp "$app_dir/fonts/MiSans-License.pdf" "$app_dir/fonts/JetBrainsMono-OFL.txt" "$dir/share/doc/pastezo/"

cat > "$dir/share/applications/app.pastezo.desktop" <<DESKTOP
[Desktop Entry]
Type=Application
Name=Pastezo
Comment=Clipboard history
Exec=Pastezo
Icon=app.pastezo
Categories=Utility;
StartupWMClass=Pastezo
DESKTOP

cat > "$dir/install.sh" <<'INSTALL'
#!/bin/sh
# Installs Pastezo for this user into ~/.local (no root needed).
set -eu
here=$(cd "$(dirname "$0")" && pwd)
prefix="${XDG_DATA_HOME:-$HOME/.local/share}/.."
prefix=$(cd "$prefix" && pwd)
mkdir -p "$prefix/bin" "$prefix/share"
cp "$here/bin/Pastezo" "$here/bin/pastezo-agent" "$prefix/bin/"
cp -R "$here/share/." "$prefix/share/"
sed -i "s|^Exec=.*|Exec=$prefix/bin/Pastezo|" "$prefix/share/applications/app.pastezo.desktop"
command -v update-desktop-database >/dev/null && update-desktop-database "$prefix/share/applications" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q "$prefix/share/icons/hicolor" || true
echo "Pastezo installed into $prefix"
INSTALL
chmod +x "$dir/install.sh"

tar -C "$out" -czf "$out/$name.tar.gz" "$name"
echo "$out/$name.tar.gz"
