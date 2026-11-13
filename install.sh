#!/usr/bin/env bash
# Build and install the Maprisma voice service and the dictation overlay for the current user
# (no root needed):
#   ~/.local/bin/maprisma-voice                                    the daemon and its CLI
#   ~/.config/systemd/user/maprisma-voice.service                  the user service
#   ~/.local/share/dbus-1/services/com.maprisma.Voice1.service     starts it on the first call
#   ~/.local/bin/maprisma-dictation                                the overlay (bind a key to
#                                                                  `maprisma-dictation --toggle`, docs/overlay.md)
#   ~/.local/share/applications/com.maprisma.Dictation.desktop     its launcher entry (hidden)
#   ~/.local/share/icons/hicolor/scalable/apps/com.maprisma.Dictation{,-symbolic}.svg
# The engine (whisper.cpp's whisper-server) and its models are found in
# ~/.local/share/maprisma-voice/{whisper.cpp/bin,models}, or on PATH; see README.md.
#   ./install.sh --uninstall    removes these files again (settings and models stay).
set -euo pipefail
cd "$(dirname "$0")"
bin="$HOME/.local/bin"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
units="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
unit="$units/maprisma-voice.service"
activation="$data/dbus-1/services/com.maprisma.Voice1.service"
desktop="$data/applications/com.maprisma.Dictation.desktop"
icons="$data/icons/hicolor/scalable/apps"

if [[ ${1:-} == --uninstall ]]; then
    systemctl --user stop maprisma-voice.service 2>/dev/null || true
    rm -f "$bin/maprisma-voice" "$unit" "$activation" "$bin/maprisma-dictation" "$desktop" \
        "$icons/com.maprisma.Dictation.svg" "$icons/com.maprisma.Dictation-symbolic.svg"
    systemctl --user daemon-reload 2>/dev/null || true
    echo "Removed the Maprisma voice service and the dictation overlay (settings and models stay)."
    exit 0
fi

cargo build --release --workspace
# A running program keeps its binary mapped: install next to it and rename over it.
for program in maprisma-voice maprisma-dictation; do
    install -Dm755 "target/release/$program" "$bin/.$program.new"
    mv -f "$bin/.$program.new" "$bin/$program"
done

# The overlay's launcher entry (hidden: it is opened by a key) and icons.
install -Dm644 data/icons/com.maprisma.Dictation.svg "$icons/com.maprisma.Dictation.svg"
install -Dm644 data/icons/com.maprisma.Dictation-symbolic.svg "$icons/com.maprisma.Dictation-symbolic.svg"
# Exec with the installed path (quoted for desktop entries: \ " ` $ escaped).
exec_path=$(printf '%s' "$bin/maprisma-dictation" | sed -e 's/[\\"`$]/\\&/g')
mkdir -p "$(dirname "$desktop")"
sed -e "s|^Exec=maprisma-dictation |Exec=\"$exec_path\" |" data/com.maprisma.Dictation.desktop > "$desktop.new"
mv -f "$desktop.new" "$desktop"

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
echo "Installed. Try: maprisma-voice status   ·   maprisma-voice dictate   ·   maprisma-dictation --demo"
echo "Bind a key to: $bin/maprisma-dictation --toggle   (see docs/overlay.md)"
