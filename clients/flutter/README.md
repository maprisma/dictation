# Flutter client of maprisma-voice

**Copy unchanged into each app; changes go here first.**

| File | Into the app | What |
|---|---|---|
| `voice_input.dart` | `flutter_app/lib/` | finds `maprisma-voice` (`~/.local/bin`, then PATH), `VoiceInput.available()` (`status --json`, cached 5 s), `VoiceInput.start()` → `VoiceSession` (runs `dictate --events --release --app-id …`, typed events, `finish()`, `cancel()`, `dispose()`) |
| `mic_button.dart` | `flutter_app/lib/` | `MicButton` (click starts, second click stops and inserts, Esc cancels, the 4 px aura ring), `insertDictatedText()` |
| `test/voice_fake.dart`, `test/voice_input_test.dart`, `test/mic_button_test.dart` | `flutter_app/test/` | the same tests in every app; only `package:APP/` becomes the app's package name |

Plain Dart and Flutter, no packages. The protocol is in [../../docs/protocol.md](../../docs/protocol.md), section
"For apps: `maprisma-voice dictate --events`"; the ring follows [../../docs/research/antigravity-design.md](../../docs/research/antigravity-design.md) B.

```sh
C=/mnt/Data/Projects/maprisma-dictation/clients/flutter
cp $C/voice_input.dart $C/mic_button.dart flutter_app/lib/
for f in voice_fake voice_input_test mic_button_test; do
  sed 's#package:APP/#package:<app_package>/#' $C/test/$f.dart > flutter_app/test/$f.dart
done
```

## Using it

```dart
MicButton(
  appId: 'com.maprisma.Launcher',   // the app's real application id
  tr: tr,                           // the app's translation function
  controller: request,              // the final text goes in at the cursor (replacing the selection)
  focusNode: _field,                // status re-checked on focus; focus comes back after inserting
  onPartial: (t) => setState(() => _heard = t), // show greyed (e.g. helperText); '' = nothing
)
```

- The button is **hidden** unless the service is installed, switched on and ready. It checks again when it
  appears, when the window comes back to the front and when the field gets focus.
- Esc (or a click) while it is still starting cancels that start too: the session that comes back
  is disposed at once, and no second start runs meanwhile.
- Every wait is bounded: `status --json` is killed and reaped after `VoiceInput.statusTimeout`
  (8 s) or 64 KiB of output; after `cancel()` without `closed` for `VoiceInput.cancelTimeout`
  (5 s) the session ends as `cancelled` and the program is ended (stdin closed, SIGTERM after
  `grace`, then SIGKILL).
- An external engine that types the text itself (`typed: true`) inserts nothing here.
- The four texts go into each app's translations (all 8 languages): `Dictate`, `Stop and insert`,
  `Listening…`, `Dictation is not available: {0}`.
- Tests: `FakeVoice.create()` puts a fake `maprisma-voice` on a temporary PATH with a temporary HOME
  (`VoiceInput.environment`); widget tests let it run with `tester.runAsync`.
