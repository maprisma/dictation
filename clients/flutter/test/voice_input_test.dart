// The voice client: event parsing, line bounds and the session against a fake maprisma-voice
// (canonical copy: maprisma/dictation clients/flutter/test).

import 'dart:async';
import 'dart:convert';
import 'dart:io';

import 'package:flutter_test/flutter_test.dart';
import 'package:APP/voice_input.dart';

import 'voice_fake.dart';

void main() {
  group('parseVoiceEvent', () {
    test('every event kind', () {
      expect((parseVoiceEvent('{"event":"started","session":"a1"}') as VoiceStarted).session, 'a1');
      expect((parseVoiceEvent('{"event":"state","state":"listening"}') as VoiceStateEvent).state, 'listening');
      expect((parseVoiceEvent('{"event":"level","rms":0.25}') as VoiceLevel).rms, 0.25);
      expect((parseVoiceEvent('{"event":"level","rms":7}') as VoiceLevel).rms, 1.0);
      expect((parseVoiceEvent('{"event":"partial","text":"hal"}') as VoicePartial).text, 'hal');
      final f = parseVoiceEvent('{"event":"final","text":"Hallo","typed":false,"engine":"whisper"}') as VoiceFinal;
      expect((f.text, f.typed, f.engine), ('Hallo', false, 'whisper'));
      expect((parseVoiceEvent('{"event":"final","text":"","typed":true}') as VoiceFinal).typed, isTrue);
      final e = parseVoiceEvent('{"event":"error","code":"start","message":"Off"}') as VoiceError;
      expect((e.code, e.message), ('start', 'Off'));
      expect((parseVoiceEvent('{"event":"closed","reason":"no_speech"}') as VoiceClosed).reason, 'no_speech');
    });

    test('junk is ignored', () {
      for (final line in ['', 'x', '[]', '{"event":"what"}', '{"event":"level","rms":"a"}', '{"event":"state"}', '{"event":1}']) {
        expect(parseVoiceEvent(line), isNull, reason: line);
      }
      expect(parseVoiceEvent('{"event":"partial","text":"${'a' * (voiceMaxLine + 1)}"}'), isNull);
    });

    test('status', () {
      final s = VoiceStatus.parse('{"enabled":true,"ready":false,"why_not":"no_model","message":"No model"}');
      expect((s.installed, s.enabled, s.ready, s.usable, s.whyNot), (true, true, false, false, 'no_model'));
      expect(VoiceStatus.parse('{"enabled":true,"ready":true}').usable, isTrue);
      expect(VoiceStatus.parse('nope').installed, isFalse);
    });
  });

  test('lines: split, CR stripped, overlong dropped, a split UTF-8 character kept', () async {
    final bytes = utf8.encode('a\r\n${'x' * 50}\nü\nend');
    final chunks = [bytes.sublist(0, 4), bytes.sublist(4, 55), bytes.sublist(55)];
    final lines = await Stream<List<int>>.fromIterable(chunks).transform(const VoiceLineSplitter(maxLine: 20)).toList();
    expect(lines, ['a', 'ü', 'end']);
  });

  group('with a fake service', () {
    late FakeVoice fake;
    setUp(() async => fake = await FakeVoice.create());
    tearDown(() => fake.dispose());

    test('finds it: ~/.local/bin first, then PATH; none → missing', () async {
      expect(VoiceInput.executable(), '${fake.dir.path}/bin/maprisma-voice');
      final local = Directory('${fake.dir.path}/home/.local/bin')..createSync(recursive: true);
      File('${fake.dir.path}/bin/maprisma-voice').copySync('${local.path}/maprisma-voice');
      Process.runSync('chmod', ['755', '${local.path}/maprisma-voice']);
      expect(VoiceInput.executable(), '${local.path}/maprisma-voice');
      VoiceInput.environment = {'HOME': '/nonexistent', 'PATH': '/nonexistent'};
      VoiceInput.forget();
      expect(VoiceInput.executable(), isNull);
      expect((await VoiceInput.available()).installed, isFalse);
    });

    test('available: ready, cached, fresh', () async {
      expect((await VoiceInput.available()).usable, isTrue);
      fake.status(ready: false); // forgets the cache
      expect((await VoiceInput.available()).usable, isFalse);
      fake.status(ready: true, enabled: false);
      final s = await VoiceInput.available();
      expect((s.installed, s.enabled, s.usable), (true, false, false));
    });

    Future<List<VoiceEvent>> until(VoiceSession s, bool Function(VoiceEvent) when, void Function() then) async {
      final seen = <VoiceEvent>[];
      final done = Completer<void>();
      var acted = false;
      s.events.listen((e) {
        seen.add(e);
        if (!acted && when(e)) {
          acted = true;
          then();
        }
      }, onDone: done.complete);
      await done.future.timeout(const Duration(seconds: 10));
      return seen;
    }

    test('finish: listening → partial → final → closed; argv and stdin', () async {
      final s = await VoiceInput.start(appId: 'com.maprisma.Test', language: 'de');
      final seen = await until(s, (e) => e is VoicePartial, s.finish);
      expect(seen.whereType<VoiceStarted>().single.session, 's1');
      expect(s.session, 's1');
      expect(seen.whereType<VoiceLevel>().single.rms, 0.4);
      expect(seen.whereType<VoicePartial>().single.text, 'hello');
      expect(seen.whereType<VoiceFinal>().single.text, 'hello world');
      expect([for (final e in seen.whereType<VoiceStateEvent>()) e.state], ['listening', 'transcribing', 'done']);
      expect((seen.last as VoiceClosed).reason, 'finished');
      expect(s.closed, isTrue);
      expect(await s.exitCode, 0);
      expect(fake.args.last, 'dictate --events --release --app-id com.maprisma.Test --language de');
      expect(fake.stdinLines, ['finish']);
    });

    test('cancel', () async {
      final s = await VoiceInput.start(appId: 'com.maprisma.Test', release: false);
      final seen = await until(s, (e) => e is VoiceStateEvent && e.state == 'listening', s.cancel);
      expect((seen.last as VoiceClosed).reason, 'cancelled');
      expect(seen.whereType<VoiceFinal>(), isEmpty);
      expect(fake.args.last, 'dictate --events --app-id com.maprisma.Test');
      expect(fake.stdinLines, ['cancel']);
    });

    test('dispose closes stdin, which cancels', () async {
      final s = await VoiceInput.start(appId: 'com.maprisma.Test');
      final seen = await until(s, (e) => e is VoicePartial, s.dispose);
      expect((seen.last as VoiceClosed).reason, 'cancelled');
      expect(fake.stdinLines, ['EOF']);
      expect(await s.exitCode.timeout(const Duration(seconds: 5)), 1);
    });

    test('a failed start: one error with the service message, then closed', () async {
      fake.failStart();
      final s = await VoiceInput.start(appId: 'com.maprisma.Test');
      final seen = await until(s, (_) => false, () {});
      expect(seen.whereType<VoiceError>().single.message, 'No model');
      expect((seen.last as VoiceClosed).reason, 'exited');
    });

    test('a hanging program is ended after the start timeout', () async {
      File('${fake.dir.path}/bin/maprisma-voice').writeAsStringSync('#!/bin/sh\nexec sleep 30\n');
      final old = (VoiceInput.startTimeout, VoiceInput.grace);
      VoiceInput.startTimeout = const Duration(milliseconds: 300);
      VoiceInput.grace = const Duration(milliseconds: 100);
      addTearDown(() {
        VoiceInput.startTimeout = old.$1;
        VoiceInput.grace = old.$2;
      });
      final s = await VoiceInput.start(appId: 'com.maprisma.Test');
      final seen = await until(s, (_) => false, () {});
      expect(seen.whereType<VoiceError>().single.code, 'timeout');
      expect((seen.last as VoiceClosed).reason, 'error');
      expect(await s.exitCode.timeout(const Duration(seconds: 5)), isNot(0));
    });

    test('a status check that hangs: missing, and the program is killed and reaped', () async {
      fake.hangStatus();
      final old = VoiceInput.statusTimeout;
      VoiceInput.statusTimeout = const Duration(milliseconds: 300);
      addTearDown(() => VoiceInput.statusTimeout = old);
      final started = DateTime.now();
      expect((await VoiceInput.available()).installed, isFalse);
      expect(DateTime.now().difference(started), lessThan(const Duration(seconds: 3)));
      final pid = fake.pid('status.pid');
      expect(pid, isNotNull);
      expect(FakeVoice.running(pid!), isFalse, reason: 'the status program still runs');
    });

    test('too much status output counts as missing', () async {
      File('${fake.dir.path}/status.json').writeAsStringSync('{"enabled":true,"ready":true,"x":"${'a' * (VoiceInput.statusMaxBytes + 10)}"}');
      VoiceInput.forget();
      expect((await VoiceInput.available()).installed, isFalse);
    });

    test('cancel has a deadline: a program that ignores it is ended', () async {
      fake.deaf();
      final old = (VoiceInput.cancelTimeout, VoiceInput.grace);
      VoiceInput.cancelTimeout = const Duration(milliseconds: 300);
      VoiceInput.grace = const Duration(milliseconds: 100);
      addTearDown(() {
        VoiceInput.cancelTimeout = old.$1;
        VoiceInput.grace = old.$2;
      });
      final s = await VoiceInput.start(appId: 'com.maprisma.Test');
      final started = DateTime.now();
      final seen = await until(s, (e) => e is VoicePartial, s.cancel);
      expect((seen.last as VoiceClosed).reason, 'cancelled');
      expect(DateTime.now().difference(started), lessThan(const Duration(seconds: 3)));
      expect(await s.exitCode.timeout(const Duration(seconds: 5)), isNot(0));
      expect(FakeVoice.running(fake.pid('dictate.pid')!), isFalse);
    });
  });

  test('under flutter test the real service is never used without an environment', () async {
    VoiceInput.environment = null;
    VoiceInput.forget();
    expect(VoiceInput.executable(), isNull);
    expect((await VoiceInput.available()).installed, isFalse);
  });

  test('missing program: start throws', () async {
    VoiceInput.environment = {'HOME': '/nonexistent', 'PATH': '/nonexistent'};
    addTearDown(() => VoiceInput.environment = null);
    expect(() => VoiceInput.start(appId: 'x'), throwsStateError);
  });
}
