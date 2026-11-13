# Maprisma Dictation

One voice service for every Maprisma app. It runs on COSMIC, KDE Plasma and Hyprland/Omarchy.

- **`maprisma-voice`** (this step) is a small user service on the session D-Bus (`com.maprisma.Voice1`).
  - It records the microphone while an app asks, notices when you stop talking and turns the speech into text.
  - It runs on your own PC by default: whisper.cpp with Vulkan, keeping the model loaded in memory. Online engines and external dictation apps can be used instead.
- **Coming next:**
  - the Voice settings in Maprisma Manager;
  - a mic button on every Ask AI page;
  - the dictation overlay: speak or type, and the AI helper does it.
  - See [docs/plan.md](docs/plan.md) and [docs/proposal.md](docs/proposal.md).

## Install

```sh
./install.sh            # ~/.local/bin/maprisma-voice + systemd user unit + D-Bus activation
./install.sh --uninstall
```

The service starts itself on the first call. It needs `pw-record` (PipeWire), or else `parecord` or `arecord`.

### The local engine
`maprisma-voice` uses whisper.cpp's `whisper-server`. It looks for it in
`~/.local/share/maprisma-voice/whisper.cpp/bin/whisper-server`, then in the `whisper_server` setting, then on PATH.

Build it with Vulkan (works on AMD and Intel GPUs) like this:

```sh
git clone --depth 1 -b v1.9.5 https://github.com/ggml-org/whisper.cpp
cmake -B build -S whisper.cpp -DGGML_VULKAN=ON -DCMAKE_BUILD_TYPE=Release -DCMAKE_INSTALL_PREFIX=$PWD/install
cmake --build build -j && cmake --install build
ln -s $PWD/install ~/.local/share/maprisma-voice/whisper.cpp
```

To build it without root, get the Vulkan and SPIR-V headers from their GitHub repos and pass `-DCMAKE_PREFIX_PATH`.

Models:

```sh
maprisma-voice model download turbo    # large-v3-turbo q5_0, 574 MB: best accuracy, multilingual
maprisma-voice model download base     # 148 MB: fast; set a fixed language
```

Downloads are checked against their size and SHA-256.

## Use

```sh
maprisma-voice dictate              # speak; it stops when you stop talking (partial text on stderr)
maprisma-voice dictate --seconds 5  # a fixed length
maprisma-voice transcribe file.wav  # a 16-bit PCM WAV, without the daemon
maprisma-voice status               # engine, model and why it isn't ready
maprisma-voice set language de      # settings: enabled, engine, model, language, microphone, …
```

Apps use the D-Bus API: [docs/protocol.md](docs/protocol.md).

Measured on an RX 7900 XTX with large-v3-turbo, a 5.5 s sentence takes about 0.25–0.4 s, in German and English. The model takes about 850 MB of GPU memory and is unloaded after 10 idle minutes (`unload_minutes`).

## Privacy
- Audio stays in memory. A file is written only when an engine needs one: online engines get a 0600 file in a private runtime folder, deleted straight away.
- The microphone is open only while a session is listening. If an app quits during a session, its session is cancelled.

## Development

```sh
cargo test                    # headless: fake recorder, fake whisper-server, private bus
tools/gen-translations        # after editing translations/*.json (en, de, fr, es, it, pt, nl, pl)
```

MIT licence.
