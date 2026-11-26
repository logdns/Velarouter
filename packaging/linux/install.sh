#!/usr/bin/env bash
# Install the unpacked app for the current desktop user; do not use sudo.
set -euo pipefail
cd "$(dirname "$0")"
data=${XDG_DATA_HOME:-$HOME/.local/share}
bin="$HOME/.local/bin/Velarouter"
install -Dm755 Velarouter "$bin"
for cli in velarouter velarouter-view; do
    install -Dm755 "$cli" "$HOME/.local/bin/$cli"
done
for size in 48 128 256; do
    install -Dm644 "Velarouter-$size.png" "$data/icons/hicolor/${size}x${size}/apps/Velarouter.png"
done
mkdir -p "$data/applications"

# Desktop Entry Exec is parsed by the spec, not by a shell. Escape the absolute
# path for a double-quoted argument and double literal percent field codes.
escaped=$bin
escaped=${escaped//\\/\\\\}
escaped=${escaped//\"/\\\"}
escaped=${escaped//\`/\\\`}
escaped=${escaped//\$/\\\$}
escaped=${escaped//%/%%}
desktop=$(<Velarouter.desktop)
desktop=${desktop/Exec=Velarouter/Exec=\"$escaped\"}
printf '%s\n' "$desktop" > "$data/applications/Velarouter.desktop"

if command -v update-desktop-database >/dev/null; then
    update-desktop-database "$data/applications" >/dev/null 2>&1 || true
fi
if command -v gtk-update-icon-cache >/dev/null; then
    gtk-update-icon-cache -f -t "$data/icons/hicolor" >/dev/null 2>&1 || true
fi
printf '已安装到 %s。请从应用菜单启动 Velarouter，再固定到 Dock。\n' "$bin"
