#!/usr/bin/env bash
# Install the desktop entry and themed icons so the desktop can find them.
#
# Why this exists: GNOME on Wayland does not take a window's icon from the
# window itself. It matches the window's `app_id` to the basename of an
# installed .desktop file and uses that file's `Icon=` key, which is looked up
# in the icon theme. So the icon compiled into the binary is invisible there
# until these files exist.
#
# Everything goes under $XDG_DATA_HOME (default ~/.local/share) -- per-user,
# no root, and `--uninstall` removes exactly what was written.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
APPS="$DATA/applications"
ICONS="$DATA/icons/hicolor"
NAME="bombadil"
SIZES=(32 64 128 256 512)

if [[ "${1:-}" == "--uninstall" ]]; then
    rm -fv "$APPS/$NAME.desktop"
    for size in "${SIZES[@]}"; do
        rm -fv "$ICONS/${size}x${size}/apps/$NAME.png"
    done
    command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -qtf "$ICONS" || true
    echo "removed. log out and back in if the panel still shows it."
    exit 0
fi

# The binary the entry launches. Prefer a release build; fall back to debug so
# this works during development, and say which was chosen rather than leaving
# a .desktop file silently pointing at a binary that is not there.
BIN=""
for candidate in "$ROOT/target/release/bombadil-gui" "$ROOT/target/debug/bombadil-gui"; do
    [[ -x "$candidate" ]] && { BIN="$candidate"; break; }
done
if [[ -z "$BIN" ]]; then
    echo "no bombadil-gui binary found. run 'just build-release' first." >&2
    exit 1
fi

mkdir -p "$APPS"
sed "s|@EXEC@|$BIN|" "$ROOT/assets/$NAME.desktop" > "$APPS/$NAME.desktop"
chmod +x "$APPS/$NAME.desktop"
echo "wrote $APPS/$NAME.desktop  (Exec=$BIN)"

for size in "${SIZES[@]}"; do
    dir="$ICONS/${size}x${size}/apps"
    mkdir -p "$dir"
    cp "$ROOT/assets/icon-${size}.png" "$dir/$NAME.png"
    echo "wrote $dir/$NAME.png"
done

# Refreshes the theme cache. Absent on some systems, and the lookup still works
# without it, so a missing binary is not a failure.
if command -v gtk-update-icon-cache >/dev/null; then
    gtk-update-icon-cache -qtf "$ICONS" 2>/dev/null || true
fi
if command -v update-desktop-database >/dev/null; then
    update-desktop-database -q "$APPS" 2>/dev/null || true
fi

echo
echo "done. restart Bombadil to pick it up."
echo "on GNOME/Wayland the shell may need a moment, or a log out, to notice a"
echo "brand new entry -- it is cached per session."
