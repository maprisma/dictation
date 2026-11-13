# Maprisma Dictation: proposal (2026-11-13)

**Decided (Uwe, 2026-11-13):**
- the aura ring as the animation;
- the overlay at the top centre;
- one voice service; the Narrator is **not** merged into it (see Narrator);
- typed tools with Enter to confirm.

Goal: **one shared voice service for all Maprisma apps**, plus a **voice/typing overlay** in which you ask an
AI to do things on the desktop ("open Firefox", "how is the weather in Berlin", "switch to dark theme").
Its settings live in a new **Maprisma Settings** category. The design draws on:
- a survey of the existing code (below);
- web research;
- Antigravity's design advice (`research/antigravity-design.md`).

## What exists today

| Where | What | Use |
|---|---|---|
| Maprisma `packages/maprisma_core/lib/src/dictation.dart` (1,518 lines) | Engines: Omarchy, local Whisper (whisper-cli), local LLM runtimes, online (OpenAI/Groq); model downloads; recording with pw-record, arecord or ffmpeg; install actions | the most complete code; its engine logic moves into the service |
| omarchy-ai-agents-map `dictation.py` | Thin wrapper over `omarchy-dictation` and `superwhisper`; a mic toggle in Ask AI | replaced by the service |
| Omarchy 4.5 | `omarchy-dictation start/stop/toggle`, which runs the adapter `omarchy-dictation-<backend>`. The backend is Superwhisper (default) or Voxtype. Keys: Right Alt/F9 push-to-talk, Super+Ctrl+X toggle | we add an **adapter `omarchy-dictation-maprisma`** so Omarchy's keys can use our service |
| Superwhisper (Linux since 2026-11-05) | Commercial app; pastes text into the active app; no API for recording or transcribing (its CLI only works with history, macOS/Windows) | **cannot be our engine**. It stays selectable as an external engine that types the text, as in Omarchy |
| Voxtype | MIT, whisper.cpp/Parakeet, Vulkan; CLI and signals; no D-Bus | optional external engine (types the text) |
| Narrator | Speech only (Kokoro/Piper), a resident worker | stays on its own; the service may use the installed Kokoro only for spoken answers (see Narrator) |
| This PC | RX 7900 XTX (Vulkan), 7950X3D; pw-record and wtype present; **no STT engine or model installed** | first step: whisper.cpp with Vulkan plus one model |

## Architecture

```
 Ask AI pages (Launcher, Theme, Manager, Playground, Maprisma)   Omarchy keys
        │ mic button                                                │            │ omarchy-dictation-maprisma
        ▼                                                           ▼            ▼
   D-Bus  com.maprisma.Voice1  ── maprisma-voice daemon (Rust, user systemd service) ──┐
        │  STT: own recording (PipeWire) + VAD + engine          TTS: Kokoro worker    │
        │  engines: whisper.cpp server (Vulkan, resident model) │ Parakeet (sherpa-onnx, CPU) │
        │           online (OpenAI/Groq) │ external: Voxtype/Superwhisper (they type)   │
        ▼                                                                              │
   maprisma-dictation overlay (Rust/iced, layer-shell: works on COSMIC, KDE, Hyprland) ◄┘
        │ question ─► shared AI helper (~/.config/maprisma-ai) + maprisma-actions (typed tools)
```

- **Daemon `maprisma-voice`** (Rust, zbus, like the Notifier):
  - It is the only process that opens the microphone, and only while it listens. That way the desktop's
    microphone indicator is correct, and two apps can never grab the microphone at once: the last request
    wins and the earlier one gets its text.
  - Methods: `StartSession(app, mode, options) → session`, `Finish`, `Cancel`, `Speak(text)`, `StopSpeaking`.
  - Signals: `Partial(text)`, `Final(text)`, `Level(rms)`, `State(s)`, `Closed(reason)`.
  - The engine model stays loaded (whisper-server), so a dictation takes well under a second on the 7900 XTX.
- **Clients:**
  - Flutter apps use a small Dart D-Bus package that every Ask AI page uses for its mic button. Partial text
    is shown greyed in the field and becomes final on release.
  - The Rust apps use zbus, and the Shell uses a small QML bridge.
  - If the service is off or missing, the mic buttons disappear.
- **Languages:** Auto, German or English, set once in the Manager instead of hard-coded per app as today.

## The overlay (the "Maprisma Dictation" app)

- **Opening it:** one global key; Super+Space is suggested.
  - Hold the key to talk; it ends when you let go.
  - Tap the key to toggle: you talk, and the request is sent after 0.9 s of silence.
  - Typing works at once; the microphone stops when you type.
  - Esc works in stages: first it stops speech or cancels a pending confirmation, the second Esc closes.
  - KDE: toggle only, because KDE reports no key release.
- **Look:**
  - An island at the top centre: a 380×52 pill that grows to 480×360 as the answer arrives.
  - Corners, background and blur come from the active theme; the font is the system font.
  - Live text shows greyed, and the answer appears below it.
  - Actions appear as chips ("▸ Opened Firefox", with Undo). Confirmations use an amber card with the
    command and **[Enter] Run / [Esc] Cancel**. A spoken "yes" is never accepted as confirmation.
- **Voice animation** (choose one, see the questions):
  1. **Aura ring** around the pill (Antigravity's pick). The same ring appears around the mic buttons in the
     apps, so it is one recognisable look everywhere.
  2. **Orb:** three layered blobs (in the style of ChatGPT/Gemini), centred and larger.
  3. **Bars:** seven level bars (like Wispr Flow).

  In every option the animation follows the microphone level while listening. It shows a shimmer while
  transcribing, a slow swirl while thinking, a double pulse while acting, pulses with the voice while
  speaking, flashes green when done and shakes red on an error.

  Colours are taken from the theme's accent colour (OKLCH, two neighbouring hues) and adjusted for light and
  dark themes. Reduced motion is respected.
- **History:** follow-up questions within 45 s keep the context. The last 25 sessions are kept locally, and a
  private mode keeps none.

## Actions and safety (`maprisma-actions`)

The AI helper is the shared one from Manager → AI. It gets **typed tools** instead of a free shell:

| Level | Examples | Behaviour |
|---|---|---|
| 0 runs directly | open an app (through Launcher), switch workspace/window, theme light/dark (through Theme), do not disturb (through Notifier), volume, media, weather/web lookup | runs, with an Undo chip |
| 1 needs [Enter] | close windows, write or move files, any shell command | confirmation card |
| 2 never | delete outside the trash, packages, autostart, secrets (~/.ssh, keyrings, .env) | blocked |

Once a request has read outside text (web, screen text), every later action needs at least level 1. Every
action is logged, and the log is visible in the Manager.

## Narrator: stays on its own (decided 2026-11-13)

On checking, the Narrator is **not** merged, because its speech is built into how it reads:
- four voice providers: Kokoro, Piper, Qwen3-TTS and online;
- NLLB translation and audio routing;
- highlighting that follows each spoken passage;
- a runtime (`kindle-reader`, `ocrvox.service`) shared with the Omarchy plugin.

Moving that would mean rebuilding it and risking a working app. The Narrator reads and does not dictate, so it gains nothing from the service.

What remains: the overlay may read short answers aloud. For that, `Speak` uses an installed Kokoro (its model files, read-only) if one is present. The Narrator stays unchanged and independent of the Manager.

## Manager: new "Voice" category

- **Dictation:** on/off; engine (installed and ready, or Install… for whisper.cpp with Vulkan, Parakeet,
  Voxtype or online); model (download or remove); language; microphone with a level meter; key and mode
  (hold/toggle); the animation.
- **Assistant:** on/off; key; speak answers; history and private mode; action levels (which level-1 actions
  are allowed without asking); the action log.
- **Speech:** read answers aloud (Kokoro, if installed): voice and speed.
- **Omarchy:** "Use Maprisma Dictation for Omarchy's dictation keys". This installs the adapter and sets the
  backend through `omarchy-dictation-use`, never by editing Omarchy's files.

## Steps

1. **Service and engine:**
   - Daemon `maprisma-voice` with STT through whisper.cpp (Vulkan) and VAD, plus the D-Bus API.
   - Moving Maprisma's engine logic (models, online, external engines) into it.
   - A CLI `maprisma-voice dictate` for testing.
2. **Manager "Voice" category:** settings, install, model, microphone test.
3. **Mic buttons on every Ask AI page:** Launcher, Theme, Manager, Playground, Maprisma, plus Notifier/Shell
   where a text field exists.
4. **The overlay:** layer-shell, animation, typing and talking, the shared AI helper, plain answers first.
5. **`maprisma-actions`:** the typed tools, confirmation and log.
6. **Speaking answers:** the overlay reads its answers aloud with an installed Kokoro (optional). The Narrator stays as it is.
7. **Omarchy adapter;** optionally later a wake word (openWakeWord, off by default).

Each step on its own branch, with tests, installed and pushed only when you say so.
