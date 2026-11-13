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
     - a panel applet (COSMIC) and tray icon (KDE, Hyprland) that toggle it: built on branch `bar-icon`, 2026-11-13 ([overlay.md](overlay.md#the-bar-icon)); the Manager's catalog entry declares it (maprisma-manager branch `dictation-bar-icon`);
     - the key binding from the Manager's Voice page (step 2);
     - check keyboard focus and typing live on KDE and COSMIC;
     - history of the last 25 sessions with a private mode;
     - compositor blur behind the pill; starting pi's local model servers for `pi:` helpers (the Launcher does that).
5. **`maprisma-actions`.** Built on branch `step-5-actions`, 2026-11-13 (not merged or installed yet); see [overlay.md](overlay.md#actions).
   - The helper proposes actions in one `maprisma-actions` JSON block of a fixed schema (11 kinds); the overlay parses it strictly and runs them itself, never through a shell.
   - Levels: 0 runs with an Undo chip (apps, URLs, do not disturb, light/dark, volume, media, workspaces, weather), 1 waits for Enter on an amber card (close window, `run_command`), 2 is blocked (sudo, packages, deleting, autostart, secrets — also inside `run_command`'s argv). Outside text (a weather result, pasted text) makes later actions need Enter.
   - Weather card from Open-Meteo; the log in `~/.local/state/maprisma-dictation/actions.jsonl`; settings `actions`, `confirm_all`, `weather_location`.
   - Checked live on Hyprland (4K): "open Firefox", "how is the weather in Berlin" / "wie ist das Wetter in Hamburg?", the `run_command` card (cancelled), and `uname -r` confirmed through the real helper in a test harness.
   - Follow-ups: the action log in Maprisma Manager; KDE's workspace/window commands on a live Plasma session; COSMIC has no workspace/window interface for other apps; `write_file`/`move_file` left out.
6. **Spoken answers.** The service part is done, 2026-11-13: `Speak`/`StopSpeaking`, the `Speaking` signal and `maprisma-voice speak`. Checked live: Kokoro speaks two German sentences in 1.5 s, and Whisper transcribes them back word for word. The overlay's part is on branch `step-5-actions`: the `speak_answers` setting (off by default), `StopSpeaking` on Esc, closing or a new question/voice session, and a gently pulsing ring while it speaks; checked end to end on a private bus with a fake `pw-play`. The Narrator stays on its own.
7. **Omarchy.** The adapter is done, 2026-11-13: `omarchy-dictation-maprisma` and `omarchy-install-dictation-maprisma` ([omarchy.md](omarchy.md)); not yet tried on a real Omarchy.
   - **The wake phrase** (instead of openWakeWord): built on branch `wake-phrase`, 2026-11-13 (not merged or installed yet). `wake_enabled` (off by default), `wake_phrase` ("Hey Linux"), `wake_language`, `wake_model`, `wake_sensitivity`; the daemon's wake listener (`src/wake.rs`): energy-cut stretches of speech → a small local Whisper on its own CPU-only whisper-server → fuzzy match → `Wake(phrase, rest)` and `maprisma-dictation --wake [--ask-rest REST]`. Sessions always win the microphone; `WakeTest` / `maprisma-voice wake test` for the Manager's "Try the phrase"; a bar-icon state.
   - Checked: end to end on a private bus with the real daemon, a fake recorder streaming Kokoro's "Hey Linux" and ggml-base on the CPU (`tests/wake_bus.rs`): the phrase is recognised about 1 s after it ends (0.6 s of quiet, then about 0.4 s for ggml-base on 4 CPU threads; the first time also loads the model, under 1 s); "Hallo Welt, wie geht es dir?" does not match; "Hey Linux, öffne Firefox" (German voice, `wake_language en`) is heard as "High Linux, open in Firefox D." and matches with that rest.
   - Follow-ups: the Manager's Voice page (switch, phrase, language, sensitivity, "Try the phrase"); check with a real microphone and room noise; maybe `-ac` (a shorter audio context) for faster checks.
