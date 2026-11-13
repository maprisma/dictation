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
# The bar icon that toggles the overlay:
#   ~/.local/bin/maprisma-dictation-applet                         the COSMIC panel applet
#   ~/.local/bin/maprisma-dictation-tray                           the tray icon (KDE Plasma, Hyprland)
#   ~/.local/share/applications/io.github.ubruckhaus.CosmicMaprismaDictation.desktop   makes it an applet
#   ~/.config/autostart/io.github.ubruckhaus.CosmicMaprismaDictation.Tray.desktop     the tray on KDE and Hyprland
#   ~/.local/share/icons/hicolor/scalable/apps/io.github.ubruckhaus.CosmicMaprismaDictation{,-open,-listening,-speaking}-symbolic.svg
# The engine (whisper.cpp's whisper-server) and its models are found in
# ~/.local/share/maprisma-voice/{whisper.cpp/bin,models}, or on PATH; see README.md.
#   ./install.sh --uninstall    removes these files again (settings and models stay; a running
#                               tray or applet keeps running until logout or a panel restart).
set -euo pipefail
cd "$(dirname "$0")"
bin="$HOME/.local/bin"
data="${XDG_DATA_HOME:-$HOME/.local/share}"
units="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
unit="$units/maprisma-voice.service"
activation="$data/dbus-1/services/com.maprisma.Voice1.service"
desktop="$data/applications/com.maprisma.Dictation.desktop"
icons="$data/icons/hicolor/scalable/apps"
bar_id=io.github.ubruckhaus.CosmicMaprismaDictation
applet_desktop="$data/applications/$bar_id.desktop"
tray_autostart="${XDG_CONFIG_HOME:-$HOME/.config}/autostart/$bar_id.Tray.desktop"
bar_icons=("$bar_id-symbolic" "$bar_id-open-symbolic" "$bar_id-listening-symbolic" "$bar_id-speaking-symbolic")

if [[ ${1:-} == --uninstall ]]; then
    systemctl --user stop maprisma-voice.service 2>/dev/null || true
    rm -f "$bin/maprisma-voice" "$unit" "$activation" "$bin/maprisma-dictation" "$desktop" \
        "$icons/com.maprisma.Dictation.svg" "$icons/com.maprisma.Dictation-symbolic.svg" \
        "$bin/maprisma-dictation-applet" "$bin/maprisma-dictation-tray" "$applet_desktop" "$tray_autostart"
    for icon in "${bar_icons[@]}"; do rm -f "$icons/$icon.svg"; done
    gtk-update-icon-cache -q -t "$data/icons/hicolor" 2>/dev/null || true
    systemctl --user daemon-reload 2>/dev/null || true
    echo "Removed the Maprisma voice service, the dictation overlay and its bar icon (settings and models stay)."
    exit 0
fi

cargo build --release --workspace
# A running program keeps its binary mapped: install next to it and rename over it.
for program in maprisma-voice maprisma-dictation maprisma-dictation-applet maprisma-dictation-tray; do
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

# The bar icon: the COSMIC applet's entry and the tray's autostart (KDE; Hyprland starts XDG
# autostart through hyprland-session.target, like the other Maprisma trays).
for icon in "${bar_icons[@]}"; do
    install -Dm644 "data/icons/$icon.svg" "$icons/$icon.svg"
done
# Desktop Exec has two escaping layers: quoted arguments, then string values.
desktop_arg() {
    local v=$1
    v=${v//\\/\\\\}
    v=${v//\"/\\\"}
    v=${v//\`/\\\`}
    v=${v//\$/\\\$}
    v=${v//\%/%%}
    v="\"$v\""
    v=${v//\\/\\\\}
    printf '%s' "$v"
}
mkdir -p "$(dirname "$applet_desktop")" "$(dirname "$tray_autostart")"
# The entry with Exec set to the installed program (all other lines kept).
install_entry() {
    local source=$1 target=$2 exec_arg line
    exec_arg=$(desktop_arg "$bin/$3")
    while IFS= read -r line; do
        if [[ $line == Exec=* ]]; then printf 'Exec=%s\n' "$exec_arg"; else printf '%s\n' "$line"; fi
    done < "data/$source" > "$target.new"
    mv -f "$target.new" "$target"
}
install_entry "$bar_id.desktop" "$applet_desktop" maprisma-dictation-applet
install_entry "$bar_id.Tray.desktop" "$tray_autostart" maprisma-dictation-tray
gtk-update-icon-cache -q -t "$data/icons/hicolor" 2>/dev/null || true

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
echo "Bar icon: COSMIC: add “Maprisma Dictation” in Settings › Desktop › Panel › Applets; KDE/Hyprland: the tray starts at the next login (or run $bin/maprisma-dictation-tray)."
