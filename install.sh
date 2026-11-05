#!/usr/bin/env bash
# Build Newseum in release mode and install it for the current user.
set -euo pipefail

APP_ID=io.github.carterprince.Newseum
PREFIX="${PREFIX:-$HOME/.local}"
cd "$(dirname "$(readlink -f "$0")")"

echo "==> Building (release)"
cargo build --release --locked

echo "==> Installing to $PREFIX"
install -Dm755 target/release/newseum "$PREFIX/bin/newseum"
install -Dm644 "data/$APP_ID.desktop" "$PREFIX/share/applications/$APP_ID.desktop"
install -Dm644 "data/$APP_ID.svg" "$PREFIX/share/icons/hicolor/scalable/apps/$APP_ID.svg"

# Point Exec at the absolute path so the launcher works even if ~/.local/bin isn't on PATH.
sed -i "s|^Exec=.*|Exec=$PREFIX/bin/newseum|" "$PREFIX/share/applications/$APP_ID.desktop"

command -v update-desktop-database >/dev/null && update-desktop-database -q "$PREFIX/share/applications" || true
command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -qtf "$PREFIX/share/icons/hicolor" 2>/dev/null || true

echo "==> Installed: $PREFIX/bin/newseum"
echo "    Feeds: \${XDG_CONFIG_HOME:-~/.config}/newseum/feeds.csv"
