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
4. **The overlay:**
   - A top-centre pill with the aura ring (layer-shell, Rust/iced).
   - Speak or type; the request goes to the shared AI helper.
   - A global key: hold to talk, tap to toggle (KDE: toggle only).
   - A panel applet and tray icon.
5. **`maprisma-actions`:** typed tools at three levels (run, confirm with Enter, never), Undo, and a log in the Manager.
6. **Spoken answers:** `Speak` using an installed Kokoro, read-only. The Narrator stays on its own.
7. **Omarchy:** the adapter `omarchy-dictation-maprisma` for Omarchy's dictation keys; an optional wake word (off by default).
