// The mic button with a fake maprisma-voice (canonical copy: maprisma/dictation
// clients/flutter/test): hidden when the service is not ready, inserts the final text at the
// cursor, Esc cancels, and the insert rule itself.

import 'package:flutter/material.dart';
import 'package:flutter/services.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:APP/mic_button.dart';
import 'package:APP/voice_input.dart';

import 'voice_fake.dart';

void main() {
  group('insertDictatedText', () {
    TextEditingController at(String text, int start, [int? end]) =>
        TextEditingController(text: text)..selection = TextSelection(baseOffset: start, extentOffset: end ?? start);

    test('spaces where needed, selection replaced, caret after', () {
      var c = at('Find', 4);
      insertDictatedText(c, ' photo editors ');
      expect(c.text, 'Find photo editors');
      expect(c.selection.baseOffset, c.text.length);

      c = at('Find  apps', 5);
      insertDictatedText(c, 'nice');
      expect(c.text, 'Find nice apps');

      c = at('Find X now', 5, 6);
      insertDictatedText(c, 'video');
      expect(c.text, 'Find video now');
      expect(c.selection.baseOffset, 10);

      c = at('Hello.', 5);
      insertDictatedText(c, 'world');
      expect(c.text, 'Hello world.');

      c = TextEditingController(text: 'end'); // no selection: at the end
      insertDictatedText(c, 'here');
      expect(c.text, 'end here');

      c = at('', 0);
      insertDictatedText(c, '  ');
      expect(c.text, '');
    });
  });

  test('aura palette: dark and light lightness, hues next to the accent', () {
    final (l, c, h) = oklchOf(const Color(0xFF3584E4));
    expect(l, closeTo(0.6, 0.05));
    expect(c, greaterThan(0.1));
    final dark = AuraPalette.fromAccent(const Color(0xFF3584E4), dark: true);
    final light = AuraPalette.fromAccent(const Color(0xFF3584E4), dark: false);
    expect(oklchOf(dark.primary).$1, closeTo(0.74, 0.01));
    expect(oklchOf(light.primary).$1, closeTo(0.50, 0.01));
    expect(oklchOf(dark.primary).$3, closeTo(h, 2));
    expect(oklchOf(dark.secondary).$3, closeTo((h + 28) % 360, 3));
    // grey accent: the chroma is raised to the minimum, so the ring still has colour
    expect(oklchOf(AuraPalette.fromAccent(const Color(0xFF808080), dark: true).primary).$2, greaterThan(0.05));
  });

  group('MicButton', () {
    late FakeVoice fake;
    late TextEditingController controller;
    late List<String> partials;
    setUp(() async {
      fake = await FakeVoice.create();
      controller = TextEditingController(text: 'Find')..selection = const TextSelection.collapsed(offset: 4);
      partials = [];
    });
    tearDown(() => fake.dispose());

    Future<void> pump(WidgetTester tester) async {
      await tester.pumpWidget(MaterialApp(
        home: Scaffold(
          body: Row(children: [
            Expanded(child: TextField(controller: controller)),
            MicButton(appId: 'com.maprisma.Test', tr: (s) => s, controller: controller, onPartial: partials.add),
          ]),
        ),
      ));
      await settle(tester);
    }

    testWidgets('hidden when the service is not ready', (tester) async {
      fake.status(ready: false);
      await pump(tester);
      expect(find.byKey(const ValueKey('voice-mic')), findsNothing);
    });

    testWidgets('hidden when it is switched off', (tester) async {
      fake.status(enabled: false);
      await pump(tester);
      expect(find.byKey(const ValueKey('voice-mic')), findsNothing);
    });

    testWidgets('click, click: the final text at the cursor', (tester) async {
      await pump(tester);
      expect(find.byKey(const ValueKey('voice-mic')), findsOneWidget);
      expect(find.byTooltip('Dictate'), findsOneWidget);
      await tester.tap(find.byKey(const ValueKey('voice-mic')));
      await settle(tester);
      expect(find.byTooltip('Stop and insert'), findsOneWidget);
      expect(partials, containsAllInOrder(['Listening…', 'hello']));
      await tester.tap(find.byKey(const ValueKey('voice-mic')));
      await settle(tester);
      expect(controller.text, 'Find hello world');
      expect(partials.last, '');
      expect(fake.stdinLines, ['finish']);
      expect(fake.args.last, 'dictate --events --release --app-id com.maprisma.Test');
      await settle(tester, 100);
      expect(find.byTooltip('Dictate'), findsOneWidget);
    });

    testWidgets('Esc cancels and inserts nothing', (tester) async {
      await pump(tester);
      await tester.tap(find.byKey(const ValueKey('voice-mic')));
      await settle(tester);
      await tester.sendKeyEvent(LogicalKeyboardKey.escape);
      await settle(tester);
      expect(controller.text, 'Find');
      expect(fake.stdinLines, ['cancel']);
      expect(find.byTooltip('Dictate'), findsOneWidget);
    });

    testWidgets('Esc while it is still starting: the session that comes back is dropped', (tester) async {
      final old = VoiceInput.startDelay;
      VoiceInput.startDelay = const Duration(milliseconds: 300);
      addTearDown(() => VoiceInput.startDelay = old);
      await pump(tester);
      await tester.tap(find.byKey(const ValueKey('voice-mic')));
      await tester.pump();
      expect(find.byTooltip('Stop and insert'), findsOneWidget); // starting
      await tester.sendKeyEvent(LogicalKeyboardKey.escape);
      await tester.pump();
      expect(find.byTooltip('Dictate'), findsOneWidget);
      // a click while the cancelled start is still on its way starts nothing more
      await tester.tap(find.byKey(const ValueKey('voice-mic')));
      await settle(tester);
      expect(find.byTooltip('Dictate'), findsOneWidget);
      expect(partials, isNot(contains('Listening…')));
      expect(fake.args.where((a) => a.startsWith('dictate')).length, 1);
      // the session it got was disposed: stdin closed, which cancels
      expect(fake.stdinLines, ['EOF']);
      // and a new click after that works normally
      await tester.tap(find.byKey(const ValueKey('voice-mic')));
      await settle(tester);
      expect(find.byTooltip('Stop and insert'), findsOneWidget);
      await tester.sendKeyEvent(LogicalKeyboardKey.escape);
      await settle(tester);
      expect(fake.stdinLines, ['EOF', 'cancel']);
    });

    testWidgets('typed by an external engine: nothing inserted', (tester) async {
      fake.result('', typed: true);
      await pump(tester);
      await tester.tap(find.byKey(const ValueKey('voice-mic')));
      await settle(tester);
      await tester.tap(find.byKey(const ValueKey('voice-mic')));
      await settle(tester);
      expect(controller.text, 'Find');
    });

    testWidgets('a failed start shows the reason', (tester) async {
      await pump(tester);
      fake.failStart();
      await tester.tap(find.byKey(const ValueKey('voice-mic')));
      await settle(tester);
      expect(partials.last, 'Dictation is not available: No model');
      await tester.pump(const Duration(seconds: 6)); // the reason goes again
      expect(partials.last, '');
    });
  });
}
