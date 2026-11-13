# maprisma-voice D-Bus API

| | |
|---|---|
| Bus | session |
| Name | `com.maprisma.Voice1` (started on the first call through D-Bus activation; `install.sh` installs the activation file) |
| Object | `/com/maprisma/Voice1` |
| Interface | `com.maprisma.Voice1` |
| Errors | `com.maprisma.Voice1.Error.{Disabled, NotReady, InvalidArgs, UnknownSession, NotSupported, EngineFailed, RecorderFailed, Failed}`; the message is meant to be shown to people, in the service's language. |

Only the daemon opens the microphone, and only while a session is `listening`. One session listens at a time.

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

## Signals (all carry the `session_id` first)

| Signal | Arguments | When |
|---|---|---|
| `State` | `s state` | `listening`, then `transcribing`, then `done`; or `cancelled` or `error` |
| `Level` | `d rms` (0…1) | about 20 times a second while listening, for the animation |
| `Partial` | `s text` | about every 1.2 s of audio while listening (local Whisper only), the whole text so far |
| `Final` | `s text`, `a{sv} details` | once at the end. `details`: `typed` (b: an external engine typed the text itself, so `text` is empty), `engine`, `model`, `language`, `audio_ms`, `elapsed_ms` |
| `Error` | `s code`, `s message` | before `Closed("error")` |
| `Speaking` | `s speak_id`, `s state`, `s message` | `speaking`, then `done`, `stopped` or `error` (with a message); the first argument is the speak id, not a session |
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
