#!/usr/bin/env bash
# For Ubuntu 24.04 or Debian bookworm, run as root.
set -euo pipefail
apt-get update
apt-get install -y --no-install-recommends build-essential cmake pkg-config python3 clang libclang-dev libxcb1-dev libxrandr-dev libdbus-1-dev libpipewire-0.3-dev libwayland-dev libegl-dev libxkbcommon-dev libxkbcommon-x11-dev libxdo-dev libssl-dev libfontconfig1-dev libgbm-dev libudev-dev libxcb-shape0-dev libxcb-xfixes0-dev
