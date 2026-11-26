#!/usr/bin/env bash
# Use a private X11 server; never capture the caller's graphical session.
set -euo pipefail
cd "$(dirname "$0")/.."
if [[ $(uname -s) != Linux ]]; then
    printf 'This acceptance test requires Linux, xvfb, xauth and xsetroot.\n' >&2
    exit 1
fi
for tool in xvfb-run Xvfb xauth xsetroot; do
    if ! command -v "$tool" >/dev/null; then
        printf 'Missing %s (Debian/Ubuntu: install xvfb xauth x11-xserver-utils).\n' "$tool" >&2
        exit 1
    fi
done

xvfb-run -a -s '-screen 0 800x450x24 -noreset -nolisten tcp' \
    env -u WAYLAND_DISPLAY -u WAYLAND_SOCKET XDG_SESSION_TYPE=x11 VR_WALL_XVFB_TEST=1 \
    bash -ec 'xsetroot -solid "#174060"
              exec cargo test --locked -p vr-desktop --bin velarouter-desktop "$@" -- wall_xvfb_four_previews --ignored' bash "$@"
