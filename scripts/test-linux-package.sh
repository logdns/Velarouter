#!/usr/bin/env bash
# Validate release artifacts without launching an app or touching the real home.
set -euo pipefail
if [[ $# != 2 ]]; then echo 'Usage: test-linux-package.sh <.deb> <.tar.gz>' >&2; exit 2; fi
package=$1
tarball=$2
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

[[ $(dpkg-deb -f "$package" Package) == velarouter ]]
arch=$(dpkg-deb -f "$package" Architecture)
version=$(dpkg-deb -f "$package" Version)
case $arch in
    amd64) tar_arch=x86_64 ;;
    arm64) tar_arch=aarch64 ;;
    *) echo "Unexpected Debian architecture: $arch" >&2; exit 1 ;;
esac
[[ ${package##*/} == "Velarouter-$version-linux-$arch.deb" ]]
[[ ${tarball##*/} == "Velarouter-$version-linux-$tar_arch.tar.gz" ]]
dpkg-deb -x "$package" "$work/system"
tar -xzf "$tarball" -C "$work"
portable="$work/Velarouter-$version-linux-$tar_arch"
system="$work/system/usr"

export HOME="$work/Portable home %"
export XDG_DATA_HOME="$HOME/share"
bash "$portable/install.sh"
cmp "$system/bin/Velarouter" "$HOME/.local/bin/Velarouter"
for cli in velarouter velarouter-view; do
    cmp "$portable/$cli" "$system/bin/$cli"
    cmp "$system/bin/$cli" "$HOME/.local/bin/$cli"
    "$system/bin/$cli" --version | grep -Fx "$cli $version"
done
for size in 48 128 256; do
    icon="icons/hicolor/${size}x${size}/apps/Velarouter.png"
    cmp "$portable/Velarouter-$size.png" "$system/share/$icon"
    cmp "$system/share/$icon" "$XDG_DATA_HOME/$icon"
done
for desktop in "$system/share/applications/Velarouter.desktop" "$XDG_DATA_HOME/applications/Velarouter.desktop"; do
    grep -Fx 'Icon=Velarouter' "$desktop"
    grep -Fx 'StartupWMClass=Velarouter' "$desktop"
    if command -v desktop-file-validate >/dev/null; then desktop-file-validate "$desktop"; fi
done
grep -Fx 'Exec=/usr/bin/Velarouter' "$system/share/applications/Velarouter.desktop"
grep -Fx "Exec=\"${HOME//%/%%}/.local/bin/Velarouter\"" "$XDG_DATA_HOME/applications/Velarouter.desktop"
