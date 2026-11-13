// A fake `maprisma-voice` on a temporary PATH for the voice tests (canonical copy:
// maprisma/dictation clients/flutter/test). It answers `status --json` from status.json and plays
// a dictation: started, listening, a level, a partial; then reads stdin: `finish` → final text
// (final.txt) and closed finished; `cancel` or a closed stdin → cancelled. It records its
// arguments (args.log) and every stdin line (stdin.log). Nothing real is touched: HOME is a
// temporary folder too.

import 'dart:io';

import 'package:flutter_test/flutter_test.dart';

import 'package:APP/voice_input.dart';

final class FakeVoice {
  FakeVoice._(this.dir);
  final Directory dir;

  static Future<FakeVoice> create({bool ready = true, bool enabled = true, String finalText = 'hello world', bool typed = false}) async {
    final dir = await Directory.systemTemp.createTemp('fake-voice-');
    final bin = Directory('${dir.path}/bin')..createSync();
    Directory('${dir.path}/home').createSync();
    final fake = FakeVoice._(dir);
    fake.status(ready: ready, enabled: enabled);
    fake.result(finalText, typed: typed);
    final d = dir.path;
    final script = File('${bin.path}/maprisma-voice');
    script.writeAsStringSync('''#!/bin/sh
echo "\$@" >> "$d/args.log"
case "\$1" in
  status)
    if [ -f "$d/hang-status" ]; then echo \$\$ > "$d/status.pid"; exec sleep 30; fi
    cat "$d/status.json"; exit 0 ;;
  dictate) ;;
  *) exit 2 ;;
esac
if [ -f "$d/fail" ]; then
  echo '{"event":"error","code":"start","message":"No model"}'
  echo "Could not start: No model" >&2
  exit 1
fi
echo '{"event":"started","session":"s1"}'
echo 'not json'
echo '{"event":"state","state":"listening"}'
echo '{"event":"level","rms":0.4}'
echo '{"event":"partial","text":"hello"}'
if [ -f "$d/deaf" ]; then echo \$\$ > "$d/dictate.pid"; exec sleep 30; fi
while IFS= read -r line; do
  echo "\$line" >> "$d/stdin.log"
  case "\$line" in
    cancel)
      echo '{"event":"state","state":"cancelled"}'
      echo '{"event":"closed","reason":"cancelled"}'
      exit 1 ;;
    *)
      echo '{"event":"state","state":"transcribing"}'
      printf '{"event":"final","text":"%s","typed":%s,"engine":"whisper"}\\n' "\$(cat "$d/final.txt")" "\$(cat "$d/typed")"
      echo '{"event":"state","state":"done"}'
      echo '{"event":"closed","reason":"finished"}'
      exit 0 ;;
  esac
done
echo "EOF" >> "$d/stdin.log"
echo '{"event":"state","state":"cancelled"}'
echo '{"event":"closed","reason":"cancelled"}'
exit 1
''');
    Process.runSync('chmod', ['755', script.path]);
    VoiceInput.environment = {'HOME': '${dir.path}/home', 'PATH': '${bin.path}:/usr/bin:/bin'};
    VoiceInput.forget();
    return fake;
  }

  void status({bool ready = true, bool enabled = true}) {
    File('${dir.path}/status.json').writeAsStringSync(
      '{"enabled":$enabled,"ready":$ready,"why_not":"${ready ? '' : 'no_model'}","message":"${ready ? 'Ready' : 'No model'}","engine":"whisper","typed_by_engine":false}\n',
    );
    VoiceInput.forget();
  }

  /// What the next dictation hears (sync: usable inside testWidgets).
  void result(String text, {bool typed = false}) {
    File('${dir.path}/final.txt').writeAsStringSync(text);
    File('${dir.path}/typed').writeAsStringSync(typed ? 'true' : 'false');
  }

  /// `status --json` never answers (it writes its pid to status.pid first).
  void hangStatus() {
    File('${dir.path}/hang-status').writeAsStringSync('1');
    VoiceInput.forget();
  }

  /// The next dictation never reads stdin and never ends by itself (pid in dictate.pid).
  void deaf() => File('${dir.path}/deaf').writeAsStringSync('1');

  /// The pid a hanging fake wrote, and whether that process still runs (not a zombie).
  int? pid(String name) {
    final f = File('${dir.path}/$name');
    return f.existsSync() ? int.tryParse(f.readAsStringSync().trim()) : null;
  }

  static bool running(int pid) {
    final f = File('/proc/$pid/stat');
    if (!f.existsSync()) return false;
    try {
      final stat = f.readAsStringSync();
      return !stat.substring(stat.lastIndexOf(')') + 1).trimLeft().startsWith('Z');
    } on FileSystemException {
      return false;
    }
  }

  /// The next dictation fails to start.
  void failStart() => File('${dir.path}/fail').writeAsStringSync('1');

  List<String> get args {
    final f = File('${dir.path}/args.log');
    return f.existsSync() ? f.readAsLinesSync() : const [];
  }

  List<String> get stdinLines {
    final f = File('${dir.path}/stdin.log');
    return f.existsSync() ? f.readAsLinesSync() : const [];
  }

  Future<void> dispose() async {
    VoiceInput.environment = null;
    VoiceInput.forget();
    await dir.delete(recursive: true);
  }
}

/// Lets the fake program run: real time (its I/O) and frames (the widget's microtasks) in turn.
Future<void> settle(WidgetTester tester, [int ms = 600]) async {
  for (var i = 0; i < ms ~/ 20; i++) {
    await tester.runAsync(() => Future<void>.delayed(const Duration(milliseconds: 20)));
    await tester.pump(const Duration(milliseconds: 20));
  }
}
