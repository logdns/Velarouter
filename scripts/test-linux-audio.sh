#!/usr/bin/env bash
# Run as an unprivileged user with PulseAudio tools installed. Uses its own
# server/socket and a null sink, not the logged-in user's microphone/speakers.
set -euo pipefail
cd "$(dirname "$0")/.."
work=$(mktemp -d)
server=''
trap 'if [[ -n $server ]]; then kill "$server" 2>/dev/null || true; wait "$server" 2>/dev/null || true; fi; rm -rf "$work"' EXIT
export PULSE_SERVER="unix:$work/pulse.sock"
export PULSE_RUNTIME_PATH="$work/runtime"
export XDG_CONFIG_HOME="$work/config"
mkdir -m 700 "$PULSE_RUNTIME_PATH" "$XDG_CONFIG_HOME"
pulseaudio -n --daemonize=no --exit-idle-time=-1 --use-pid-file=no \
    -L "module-native-protocol-unix socket=$work/pulse.sock auth-anonymous=1" \
    -L 'module-null-sink sink_name=velarouter_test rate=48000 channels=2' >"$work/pulse.log" 2>&1 &
server=$!
for _ in {1..20}; do
    if pactl set-default-sink velarouter_test >/dev/null 2>&1; then
        if [[ $# == 1 ]]; then
            "$1" records_and_plays_synthetic_system_audio --ignored
        else
            cargo test --locked -p vr-audio records_and_plays_synthetic_system_audio -- --ignored
        fi
        exit
    fi
    sleep .2
done
printf 'Could not start isolated PulseAudio server\n' >&2
tail -20 "$work/pulse.log" >&2
exit 1
