#!/usr/bin/env bash
# Native build: run on the target Linux architecture, not the developer's Mac.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ $(uname -s) == Linux ]] || { echo '需要 Linux 构建环境' >&2; exit 2; }
case ${VR_LINUX_ARCH:-$(uname -m)} in
    x86_64) arch=x86_64; deb_arch=amd64 ;;
    aarch64|arm64) arch=aarch64; deb_arch=arm64 ;;
    *) echo '仅支持 x86_64 或 ARM64' >&2; exit 2 ;;
esac
target="$arch-unknown-linux-gnu"
cargo build --release --locked -p vr-desktop --target "$target"
bin="${CARGO_TARGET_DIR:-target}/$target/release/velarouter-desktop"
version=$(cargo metadata --no-deps --format-version 1 | python3 -c 'import sys,json;print(next(p["version"] for p in json.load(sys.stdin)["packages"] if p["name"]=="vr-desktop"))')
name="Velarouter-$version-linux-$arch"
mkdir -p dist
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
mkdir "$stage/$name"
install -m755 "$bin" "$stage/$name/Velarouter"
install -m755 packaging/linux/install.sh "$stage/$name/install.sh"
install -m644 packaging/linux/Velarouter.desktop "$stage/$name/Velarouter.desktop"
install -m644 LICENSE "$stage/$name/LICENSE"
install -m644 docs/clients/DESKTOP-APP.md "$stage/$name/README.txt"
for size in 48 128 256; do
    install -m644 "assets/brand/png/appicon/velarouter-$size.png" "$stage/$name/Velarouter-$size.png"
done
if [[ -n ${VR_SYSROOT:-} ]]; then
    # Use the target loader, so the manifest never lists build-host libraries.
    install -m755 "$bin" "$VR_SYSROOT/tmp/vr-library-check"
    chroot "$VR_SYSROOT" /lib64/ld-linux-x86-64.so.2 --list /tmp/vr-library-check > "$stage/$name/libraries.txt"
    rm "$VR_SYSROOT/tmp/vr-library-check"
else
    ldd "$bin" > "$stage/$name/libraries.txt"
fi
if grep -q 'not found' "$stage/$name/libraries.txt"; then echo '缺少运行库' >&2; exit 1; fi
tar -czf "dist/$name.tar.gz" -C "$stage" "$name"
# A system package registers the application ID and icon with the actual
# desktop shell; a file manager will still use its generic icon for a raw ELF.
deb="$stage/deb"
deb_name="Velarouter-$version-linux-$deb_arch.deb"
install -Dm755 "$stage/$name/Velarouter" "$deb/usr/bin/Velarouter"
install -Dm644 LICENSE "$deb/usr/share/doc/velarouter/copyright"
install -Dm644 docs/clients/DESKTOP-APP.md "$deb/usr/share/doc/velarouter/README.md"
for size in 48 128 256; do
    install -Dm644 "$stage/$name/Velarouter-$size.png" "$deb/usr/share/icons/hicolor/${size}x${size}/apps/Velarouter.png"
done
install -Dm644 "$stage/$name/Velarouter.desktop" "$deb/usr/share/applications/Velarouter.desktop"
# Do not resolve a stale copy on the user's PATH when launching the system package.
sed -i 's@^Exec=Velarouter$@Exec=/usr/bin/Velarouter@' "$deb/usr/share/applications/Velarouter.desktop"
mkdir -p "$deb/DEBIAN"
cat > "$deb/DEBIAN/control" <<EOF
Package: velarouter
Version: $version
Architecture: $deb_arch
Maintainer: Velarouter contributors <logdns@users.noreply.github.com>
Section: net
Priority: optional
Depends: libc6 (>= 2.36), libegl1, libgbm1, libgcc-s1, libxcb1, libpipewire-0.3-0t64 | libpipewire-0.3-0, libxkbcommon0, libxkbcommon-x11-0
Recommends: xdg-desktop-portal
Homepage: https://github.com/logdns/Velarouter
Description: Encrypted remote desktop and file transfer
 Velarouter connects computers over a local network or the internet.
EOF
dpkg-deb --root-owner-group --build "$deb" "dist/$deb_name"
(cd dist && for file in "$name.tar.gz" "$deb_name"; do sha256sum "$file" > "$file.sha256"; done)
