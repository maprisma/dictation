# Plan

The design is in [proposal.md](proposal.md). Each step gets its own branch and tests, and is installed and pushed when it is done.

1. **Service and engine.** Done, 2026-11-13.
   - `maprisma-voice`: daemon, D-Bus API ([protocol.md](protocol.md)), voice detection with room-noise calibration, three engine types, CLI and `install.sh`.
   - whisper.cpp v1.9.5 with Vulkan; idle unloading of the GPU model.
   - Checked live: German and English test sentences give the right text, end to end over D-Bus, with live partial text and finishing on silence.
   - whisper.cpp v1.9.5 also ships `parakeet-cli`/libparakeet, a possible faster engine later (untested).
2. **Maprisma Manager, "Voice" category:** on/off, engine (install), model (download/remove), language, microphone with a level meter, key and mode, assistant options.
3. **Mic buttons:** on the Ask AI pages of Launcher, Theme, Manager, Playground and Maprisma, and wherever the Shell or the Notifier has a text field.
   - They use one small client per toolkit (Dart, Rust, QML).
   - Partial text is shown greyed and replaced by the final text.
4. **The overlay.** Built on branch `step-4-overlay`, 2026-11-13 (not merged or installed yet); see [overlay.md](overlay.md).
   - `maprisma-dictation`: a top-centre pill with the aura ring (layer-shell, libcosmic/iced), a workspace member next to `maprisma-voice`.
   - Speak or type; the question goes to the shared AI helper (one-shot `ask` from Maprisma's catalog); answers only, with follow-ups for 45 s.
   - `--toggle`, `--hold`/`--release` (hold to talk on Hyprland; KDE and COSMIC toggle only), `--demo`.
   - Checked on Hyprland off-screen: the demo, and the real voice service end to end (private bus, fake recorder, fake whisper-server, fake helper).
   - Follow-ups:
     - a panel applet (COSMIC) and tray icon (KDE, Hyprland) that toggle it, and a Maprisma Manager catalog entry;
     - the key binding from the Manager's Voice page (step 2);
     - check keyboard focus and typing live on KDE and COSMIC;
     - history of the last 25 sessions with a private mode, Undo chips and confirmation cards (step 5), spoken answers (step 6);
     - compositor blur behind the pill; starting pi's local model servers for `pi:` helpers (the Launcher does that).
5. **`maprisma-actions`:** typed tools at three levels (run, confirm with Enter, never), Undo, and a log in the Manager.
6. **Spoken answers:** `Speak` using an installed Kokoro, read-only. The Narrator stays on its own.
7. **Omarchy:** the adapter `omarchy-dictation-maprisma` for Omarchy's dictation keys; an optional wake word (off by default).
