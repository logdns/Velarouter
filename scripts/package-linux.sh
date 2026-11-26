#!/usr/bin/env bash
# Native build: run on the target Linux architecture, not the developer's Mac.
set -euo pipefail
cd "$(dirname "$0")/.."
[[ $(uname -s) == Linux ]] || { echo '需要 Linux 构建环境' >&2; exit 2; }
case ${VR_LINUX_ARCH:-$(uname -m)} in x86_64) arch=x86_64;; aarch64|arm64) arch=aarch64;; *) echo '仅支持 x86_64 或 ARM64' >&2; exit 2;; esac
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
(cd dist && sha256sum "$name.tar.gz" > "$name.tar.gz.sha256")
