#!/bin/sh
# Installs Relay for the current user from an extracted release archive.
#   ./install.sh            installs into ~/.local
#   PREFIX=/usr/local sudo -E ./install.sh
set -eu

here=$(cd "$(dirname "$0")" && pwd)
prefix=${PREFIX:-$HOME/.local}

install -Dm755 "$here/relay" "$prefix/bin/relay"
install -Dm644 "$here/relay.desktop" "$prefix/share/applications/relay.desktop"
install -Dm644 "$here/relay.png" "$prefix/share/icons/hicolor/256x256/apps/relay.png"
echo "Installed Relay to $prefix/bin/relay"
