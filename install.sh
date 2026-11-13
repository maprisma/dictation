#!/usr/bin/env bash
# Build and install the Maprisma voice service for the current user (no root needed):
#   ~/.local/bin/maprisma-voice                                    the daemon and its CLI
#   ~/.config/systemd/user/maprisma-voice.service                  the user service
#   ~/.local/share/dbus-1/services/com.maprisma.Voice1.service     starts it on the first call
# The engine (whisper.cpp's whisper-server) and its models are found in
# ~/.local/share/maprisma-voice/{whisper.cpp/bin,models}, or on PATH; see README.md.
#   ./install.sh --uninstall    removes the three files again (settings and models stay).
set -euo pipefail
cd "$(dirname "$0")"
bin="$HOME/.local/bin"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
units="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
unit="$units/maprisma-voice.service"
activation="$data/dbus-1/services/com.maprisma.Voice1.service"

if [[ ${1:-} == --uninstall ]]; then
    systemctl --user stop maprisma-voice.service 2>/dev/null || true
    rm -f "$bin/maprisma-voice" "$unit" "$activation"
    systemctl --user daemon-reload 2>/dev/null || true
    echo "Removed the Maprisma voice service (settings and models stay)."
    exit 0
fi

cargo build --release
# A running daemon keeps its binary mapped: install next to it and rename over it.
install -Dm755 target/release/maprisma-voice "$bin/.maprisma-voice.new"
mv -f "$bin/.maprisma-voice.new" "$bin/maprisma-voice"

mkdir -p "$units" "$(dirname "$activation")"
cat > "$unit" <<UNIT
[Unit]
Description=Maprisma voice service (speech to text for all Maprisma apps)
Documentation=https://github.com/maprisma/dictation

[Service]
Type=dbus
BusName=com.maprisma.Voice1
ExecStart=$bin/maprisma-voice daemon
Restart=on-failure
RestartSec=2
UNIT
cat > "$activation" <<SERVICE
[D-BUS Service]
Name=com.maprisma.Voice1
Exec=$bin/maprisma-voice daemon
SystemdService=maprisma-voice.service
SERVICE

systemctl --user daemon-reload 2>/dev/null || true
# A running daemon gets the new binary; otherwise the first call starts it.
if systemctl --user is-active --quiet maprisma-voice.service 2>/dev/null; then
    systemctl --user restart maprisma-voice.service
fi
echo "Installed. Try: maprisma-voice status   ·   maprisma-voice dictate"
