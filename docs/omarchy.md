# Omarchy's dictation keys through Maprisma Dictation

Omarchy 4.5 lets any program be its dictation backend: an executable `omarchy-dictation-<name>`
on PATH taking `start`, `stop` and `toggle`, which returns at once (`stop` must succeed when idle),
plus an installer that calls `omarchy-dictation-use <name>`. This is from Omarchy's manual, "Text
Extraction & Dictation". Maprisma Dictation provides both, installed by `./install.sh` into
`~/.local/bin`:

- `omarchy-dictation-maprisma start|stop|toggle` runs `maprisma-voice omarchy …`.
  - `start` launches a detached `maprisma-voice omarchy-run` that holds one voice session
    (`end = release`).
  - `stop` signals it (SIGUSR1): it transcribes and **types the text into the focused field**
    with `wtype`, as Omarchy's own backends do. Without `wtype`, the text goes to the clipboard
    (`wl-copy`).
  - `stop` with nothing running, or without Maprisma Dictation installed, succeeds.
  - The runner is found by a pid file in the private runtime folder, checked against its own
    command line.
- `omarchy-install-dictation-maprisma` checks that the voice service is ready (engine and model),
  then runs `omarchy-dictation-use maprisma`. This makes Omarchy's keys use it:
  - holding Right Alt or F9 to talk;
  - Super+Ctrl+X to toggle.

  Going back is `omarchy default dictation` and picking Superwhisper or Voxtype.

Omarchy's own files are not changed.

The speech recognition is the voice service's, so it is local Whisper on your GPU, with the
language and model from Manager › Voice. Superwhisper's default is the cloud.

Not done: a menu entry under Omarchy's Setup › Defaults › Dictation (an Omarchy menu extension).
Until then, run `omarchy-install-dictation-maprisma` once.

Tested 2026-11-13 on CachyOS against the real daemon on a private bus, with a stand-in recorder
and `wtype`: idle stop, double start, toggle, and the text typed as one argument. **Not yet tried
on a real Omarchy.**
