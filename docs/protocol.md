# maprisma-voice D-Bus API

| | |
|---|---|
| Bus | session |
| Name | `com.maprisma.Voice1` (started on the first call through D-Bus activation; `install.sh` installs the activation file) |
| Object | `/com/maprisma/Voice1` |
| Interface | `com.maprisma.Voice1` |
| Errors | `com.maprisma.Voice1.Error.{Disabled, NotReady, InvalidArgs, UnknownSession, NotSupported, EngineFailed, RecorderFailed, Failed}`; the message is meant to be shown to people, in the service's language. |

Only the daemon opens the microphone: while a session is `listening`, and, when the wake phrase is on, while it waits for it ([The wake phrase](#the-wake-phrase)). One session listens at a time.

## Methods

### `StartSession(s app_id, s mode, a{sv} options) → s session_id`
- `app_id` names the app asking, for example `com.maprisma.Launcher`. It must not be empty.
- `mode` is `dictate` or `assistant`. Both behave the same for now; the overlay uses `assistant`.
- `options`, all of them strings:
  - `end`:
    - `release` (the default): the client calls `Finish` when the key is released or the button is pressed again.
    - `silence`: the daemon finishes on its own about 0.9 s after speech ends. It cancels with `no_speech` if nothing was said in the first 4 s.
    - Both modes stop at 60 s with `max_length`. This also holds by the wall clock (60 s + 5 s), even when the recorder delivers audio slowly.
    - A recorder that delivers nothing for 5 s has stalled: listening ends (`recorder_stalled`), with what was heard so far, or `RecorderFailed` when nothing came.
  - `language`: `auto` or a Whisper code (`de`, `en`, …). It overrides the setting for this session.
- The wake listener steps back first: it closes the microphone before the session opens it (at most 20 s, else `EngineFailed`), and waits until 1 s after the session closed.
- A running session is **preempted**: it gets `Final` with what it heard so far, then `Closed("preempted")`. The new session only starts once the old one has let go of the microphone (or its external engine has stopped). If that takes longer than 20 s, or the external stop fails, `StartSession` fails with `EngineFailed`; two recordings never overlap.
- When the daemon stops (SIGTERM, SIGINT, the bus going away), it first cancels every session and waits for them (at most 35 s), so an external recorder never goes on after it. They close with `shutdown`.
- If the calling client leaves the bus, its session is cancelled (`client_gone`).
- Errors: `Disabled`, `NotReady` (the message says why), `InvalidArgs`, `RecorderFailed`.

### `Finish(s session_id)`
Stop listening and transcribe.

### `Cancel(s session_id)`
Stop without text (`Closed("cancelled")`). If an external engine's cancel fails, the daemon stops it the plain way (Voxtype may then type what it heard). The session then ends with `Error` and `Closed("error")`, never with a clean `cancelled`.

### `Status() → a{sv}`
- `state`: `idle`, `listening` or `transcribing`.
- Details of the current session: `session_id`, `active_app`, `active_mode`.
- The chosen setup: `enabled`, `ready`, `why_not` and `message` (a code and its text), `engine`, `engine_setting`, `engines` (those available), `model`, `models`, `language`, `microphone`, `recorder`.
- The local engine: `whisper_server`, `server_running`, `server_port`.
- Also: `typed_by_engine`, `online_provider`, `sessions`, `version`.
- Spoken answers: `speech_ready`, `speech_why_not`, `speech_message`, `speaking`.
- The wake phrase: `wake_enabled`, `wake_active` (the microphone is open, waiting for the phrase), `wake_phrase`, `wake_why_not` and `wake_message` (a code and its text: why it isn't listening now).
- `wake_why_not` codes: `""` (listening), `off`, `disabled` (dictation is off), `busy` (a session runs, or ended less than 1 s ago), `speaking`, `cooldown` (heard less than 3 s ago), `no_whisper_server`, `no_model`, `no_recorder`, `recorder_failed`, `failed`.
- `why_not` codes: `disabled`, `no_engine`, `no_whisper_server`, `no_model`, `no_online_key`, `no_voxtype`, `no_superwhisper`, `no_omarchy`, `no_recorder`.

### `Speak(s text, a{sv} options) → s speak_id`
- Reads `text` aloud, up to 8000 characters. It is split into sentences and Markdown is dropped.
- The voice is **Kokoro** from the Maprisma Narrator's runtime (`~/.local/share/kindle-reader`, or the `kokoro_runtime` setting). It is used read-only: the Narrator's resident worker, with no bytecode written and the audio only in our private runtime folder. Sound goes out through `pw-play`, `paplay` or `aplay`.
- Only the German "Martin" voice is installed by default. It reads English too, with an accent.
- `options`: `language` (`auto`/`de` → German, `en` → English).
- A new `Speak`, `StopSpeaking` or `StartSession` stops the current speech at once.
- Kokoro gets 60 s per sentence (the first one loads the model); a worker that takes longer is killed and the speech ends with `error`. The service never waits on Kokoro while holding its state, so `StopSpeaking`, `Status` and `StartSession` answer at once even while a sentence is being made. After `done`, `stopped` or `error`, `speaking` is false.
- It is refused while a session is listening.
- Errors: `NotReady`, with `speech_why_not` being `no_kokoro`, `no_kokoro_model` or `no_player`.

### `StopSpeaking()`
Stops the voice at once.

### `WakeTest(u seconds) → a{sv}`
For "Try the phrase" buttons: listens once (1–10 s) for one stretch of speech, with the wake recogniser, and says what it heard. Nothing is started on a match and no `Wake` is sent. It works with `wake_enabled` off too (the recogniser is unloaded again afterwards).
- The answer: `heard` (s, the recogniser's text), `matched` (b), `score` (d, 0…1, 1 = exactly the phrase), `needed` (d, the score a match needs at this sensitivity), `rest` (s, what came after the phrase), `phrase` (s), `model` (s), `speech` (b: false when only quiet came within the time).
- It claims the microphone like a session (the wake listener pauses); it is refused while a session runs, and a session that starts meanwhile ends it with an error.
- Errors: `InvalidArgs` (seconds), `NotReady` (with why: no whisper-server, model or recorder), `Failed` (busy).

## Signals (the session signals carry the `session_id` first)

| Signal | Arguments | When |
|---|---|---|
| `State` | `s state` | `listening`, then `transcribing`, then `done`; or `cancelled` or `error` |
| `Level` | `d rms` (0…1) | about 20 times a second while listening, for the animation |
| `Partial` | `s text` | about every 1.2 s of audio while listening (local Whisper only), the whole text so far |
| `Final` | `s text`, `a{sv} details` | once at the end. `details`: `typed` (b: an external engine typed the text itself, so `text` is empty), `engine`, `model`, `language`, `audio_ms`, `elapsed_ms` |
| `Error` | `s code`, `s message` | before `Closed("error")` |
| `Speaking` | `s speak_id`, `s state`, `s message` | `speaking`, then `done`, `stopped` or `error` (with a message); the first argument is the speak id, not a session |
| `Wake` | `s phrase`, `s rest` | the wake phrase was heard (no session id); `rest` is what was said right after it in the same breath, or empty. The daemon then starts the overlay itself |
| `WakeListening` | `b active` | the wake listener opened (true) or closed (false) the microphone; for bar icons |
| `Closed` | `s reason` | always last: `finished`, `silence`, `max_length`, `no_speech`, `cancelled`, `preempted`, `client_gone`, `recorder_ended`, `recorder_stalled`, `shutdown` or `error` |

## Settings
`${XDG_CONFIG_HOME:-~/.config}/maprisma-voice/settings.json`. They are read again for every session and every `Status`. Writes are atomic and keep unknown keys; the folder is 0700 and the file 0600.

| Key | Default | Meaning |
|---|---|---|
| `enabled` | `true` | off: every `StartSession` fails with `Disabled` |
| `engine` | `auto` | `auto`, `whisper`, `online`, `voxtype`, `superwhisper` or `omarchy`. `auto` means local Whisper if a server and a model exist, else online with a key, else an external engine. |
| `model` | `""` | a file name or catalog id; empty prefers large-v3-turbo, then small, then base |
| `language` | `auto` | `auto` or a Whisper code |
| `microphone` | `default` | a PipeWire node name |
| `whisper_server` | `""` | the path to whisper-server; empty searches for it |
| `preload` | `false` | load the model when the daemon starts |
| `unload_minutes` | `10` | stop whisper-server, freeing its GPU memory, after this many idle minutes; `0` never stops it (the Kokoro worker too) |
| `kokoro_runtime` | `""` | the Narrator's runtime folder for spoken answers; empty means `~/.local/share/kindle-reader` |
| `wake_enabled` | `false` | listen for the wake phrase while no session runs; the microphone then stays open ([below](#the-wake-phrase)) |
| `wake_phrase` | `Hey Linux` | 2–40 characters: letters, digits, spaces and `,.!?'-`, at least two letters or digits, no spaces at the ends |
| `wake_language` | `auto` | the language the wake recogniser listens for: `auto` or a Whisper code. Set it to the phrase's language when it is misheard |
| `wake_model` | `""` | the wake recogniser's model (id or file); empty: the smallest installed one (tiny, then base, then small) |
| `wake_sensitivity` | `normal` | `strict`, `normal` or `loose`: how far what was heard may be from the phrase (normalised edit distance 0.15, 0.25, 0.35; a match needs a score of 0.85, 0.75, 0.65) |
| `ui_language` | `system` | the language of the messages: `en`, `de`, `fr`, `es`, `it`, `pt`, `nl` or `pl` |

The CLI writes them with `maprisma-voice set KEY VALUE` and `unset KEY`, which checks each value.

## Engines
- **whisper:** whisper.cpp `whisper-server`, started once on 127.0.0.1 on a free port with the model loaded.
  - Requests go to `POST /inference` from memory, so no audio file is written.
  - The server is started again if it died or the model changed.
  - It is looked up in the `whisper_server` setting, then `~/.local/share/maprisma-voice/whisper.cpp/bin/whisper-server`, then PATH.
  - Models are looked for in `~/.local/share/maprisma-voice/models`, `~/.local/share/maprisma/whisper/models`, `~/.local/share/voxtype/models` and `~/.cache/whisper`.
- **online:** OpenAI `whisper-1` or Groq `whisper-large-v3-turbo`.
  - The key comes from `OPENAI_API_KEY` or `GROQ_API_KEY`, or from `~/.codex/.env`.
  - It is passed to curl on stdin, never on the command line.
- **external:** `voxtype record start|stop`, `superwhisper start|stop` or `omarchy-dictation start|stop`. These type the text into the focused window themselves, so `Final` has `typed=true` and empty text.

## The wake phrase
With `wake_enabled` on, the daemon listens for the phrase (for example "Hey Linux") and opens Maprisma Dictation when it hears it.

- **When it listens:** dictation is enabled, no session runs (or ended less than 1 s ago), no answer is read aloud, and the phrase wasn't heard in the last 3 s. Any app's `StartSession` (also the Omarchy adapter's) makes it close the microphone first. A change of the wake settings or the microphone restarts it within half a second.
- **How:** it records with the usual recorder, cuts stretches of speech of 0.4–3 s with the energy detector (0.6 s of quiet ends one; longer talk is checked on its first 3 s only), and turns each one into text **locally**: a small Whisper model on a second whisper-server of its own, CPU only (`-ng -t 4`), so the main GPU server is not touched. Online and external engines are never used for it.
- **Matching:** both texts are normalised (lowercase, accents folded, punctuation dropped) and compared word by word with the edit distance, also on a light sound folding (`igh`, `ey`, `ei`, `ay` → `ai`: "High Linux" and "Hei Linux" match "Hey Linux"). The phrase must be at the start (one word before it is allowed). What follows it is the request: "Hey Linux, open Firefox" → `rest` = `open Firefox`.
- **On a match:** `Wake(phrase, rest)`, then `maprisma-dictation --wake` (with `--ask-rest REST` when something followed the phrase in a stretch under 3 s) from `~/.local/bin` or PATH, as its own process (no shell). The overlay needs the desktop's environment (`WAYLAND_DISPLAY`) in the daemon's: the systemd user manager has it on COSMIC, KDE and Hyprland when the session imports it.
- **Memory:** the wake whisper-server stays loaded while the wake phrase is on (about 200 MB of RAM with ggml-base) and is stopped when it is turned off; `unload_minutes` doesn't apply to it.
- **Privacy:** while it is on, **the microphone stays open** (the bar icon shows a mic outline with a ring). Audio stays in memory; each stretch is dropped right after it was compared. Nothing is logged but "wake phrase heard".
- **CLI:** `maprisma-voice wake test [--seconds N] [--json]` runs `WakeTest` (N ≤ 10, default 10): it prints what was heard, the score and whether it matches (`--json`: the answer as one object). Exit codes: 0 it matches, 1 it doesn't or failed, 3 no speech, 4 the service is missing. `maprisma-voice set wake_phrase "Hallo Rechner"` and the other keys are checked like every setting.

## For apps: `maprisma-voice dictate --events`
Apps that would rather not speak D-Bus themselves (Flutter, QML) run the CLI. Every Maprisma mic button uses this.

```
maprisma-voice dictate --events [--release] [--app-id com.maprisma.Launcher] [--language de]
```

- **stdout:** one JSON object per line, flushed straight away:
  - `{"event":"started","session":"…"}`
  - `{"event":"state","state":"listening|transcribing|done|cancelled|error"}`
  - `{"event":"level","rms":0.12}` (about 20 a second)
  - `{"event":"partial","text":"…"}`
  - `{"event":"final","text":"…","typed":false,"engine":"whisper"}`
  - `{"event":"error","code":"…","message":"…"}`
  - `{"event":"closed","reason":"finished|silence|no_speech|cancelled|preempted|…"}`, which comes last.
- **stdin:** the line `finish` (or an empty line) stops listening and transcribes; `cancel` cancels. **Keep stdin open while listening:** when it closes, the session is cancelled, so a crashed app never leaves the microphone on.
- **Ending:** without `--release` the session ends on silence. With `--release` it ends on `finish`, for a second click or a released key.
- **Exit codes:** 0 done, 1 failed or cancelled, 3 no speech, 4 the service is missing (also an `error` event with `code: "start"`).
- **Clients should bound every wait** (the Flutter client in `clients/flutter` does): `status --json` is killed and reaped when it takes longer than 8 s or prints more than 64 KiB; a `cancel` that brings no `closed` within 5 s counts as cancelled, and the program's stdin is closed and it is ended (SIGTERM, then SIGKILL); a start that is cancelled before the program is running is thrown away as soon as it comes back.
