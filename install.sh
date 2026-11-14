#!/bin/sh
# Build and install neje for the current user (no root needed).
# Run from the source tree it builds with cargo; run from an unpacked release archive
# (neje and neje-gui next to this script) it installs the prebuilt binaries.
#   binaries  -> ~/.local/bin
#   launcher  -> ~/.local/share/applications/neje.desktop
#   icons     -> ~/.local/share/icons/hicolor/*/apps/neje.png
# Options:
#   --udev       also install the udev rule that gives access to the serial port
#                (asks for the sudo password)
#   --uninstall  remove everything installed by this script (the udev rule too, if present)
set -eu
cd "$(dirname "$0")"

BIN="${XDG_BIN_HOME:-$HOME/.local/bin}"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}"
APPS="$DATA/applications"
ICONS="$DATA/icons/hicolor"
SIZES="16 32 48 64 128 256 512"
RULE=70-neje.rules

refresh() {
    command -v update-desktop-database >/dev/null && update-desktop-database -q "$APPS" || true
    command -v gtk-update-icon-cache >/dev/null && gtk-update-icon-cache -q -t "$ICONS" || true
    command -v kbuildsycoca6 >/dev/null && kbuildsycoca6 --noincremental >/dev/null 2>&1 || true
}

reload_udev() {
    sudo udevadm control --reload
    sudo udevadm trigger --subsystem-match=tty
}

if [ "${1:-}" = "--uninstall" ]; then
    rm -f "$BIN/neje" "$BIN/neje-gui" "$APPS/neje.desktop"
    for s in $SIZES; do rm -f "$ICONS/${s}x${s}/apps/neje.png"; done
    refresh
    if [ -f "/etc/udev/rules.d/$RULE" ]; then
        echo "Removing /etc/udev/rules.d/$RULE (needs sudo)."
        sudo rm -f "/etc/udev/rules.d/$RULE"
        reload_udev
    fi
    echo "neje removed."
    exit 0
fi

if [ -f Cargo.toml ]; then
    cargo build --release
    SRC=target/release
else
    SRC=.
fi

install -Dm755 "$SRC/neje" "$BIN/neje"
install -Dm755 "$SRC/neje-gui" "$BIN/neje-gui"
for s in $SIZES; do
    install -Dm644 "assets/icon-$s.png" "$ICONS/${s}x${s}/apps/neje.png"
done
mkdir -p "$APPS"
sed "s|^Exec=neje-gui|Exec=$BIN/neje-gui|" packaging/linux/neje.desktop > "$APPS/neje.desktop"
chmod 644 "$APPS/neje.desktop"
refresh

if [ "${1:-}" = "--udev" ]; then
    sudo install -Dm644 "packaging/linux/$RULE" "/etc/udev/rules.d/$RULE"
    reload_udev
    echo "udev rule installed: unplug and plug the engraver again."
fi

echo "Installed: $BIN/neje, $BIN/neje-gui and the \"NEJE Engraver\" launcher."
case ":$PATH:" in *":$BIN:"*) ;; *) echo "Note: $BIN is not in your PATH." ;; esac
