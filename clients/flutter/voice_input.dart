// Maprisma Dictation: the Flutter client of the shared voice service `maprisma-voice`.
//
// Canonical copy: maprisma/dictation clients/flutter/voice_input.dart. Copy it unchanged into each
// app; changes go there first. Plain Dart (dart:io, dart:async, dart:convert), no packages.
//
// It runs `maprisma-voice dictate --events` (docs/protocol.md, "For apps") with an argv list, never
// a shell, and turns its JSON lines into typed events. stdin stays open while a session runs: when
// this app goes away the pipe closes and the service cancels, so no microphone stays on.

import 'dart:async';
import 'dart:convert';
import 'dart:io';

/// What `maprisma-voice status --json` said.
final class VoiceStatus {
  const VoiceStatus({
    required this.installed,
    this.enabled = false,
    this.ready = false,
    this.whyNot = '',
    this.message = '',
    this.engine = '',
    this.typedByEngine = false,
  });

  /// No program, or it did not answer: no mic buttons.
  static const missing = VoiceStatus(installed: false);

  /// The program exists and answered.
  final bool installed;
  final bool enabled;
  final bool ready;

  /// A code (`disabled`, `no_model`, …) and its text, in the service's language.
  final String whyNot;
  final String message;
  final String engine;

  /// The chosen engine types the text itself (Voxtype, Superwhisper, Omarchy).
  final bool typedByEngine;

  /// Mic buttons show only for a service that is installed, switched on and ready.
  bool get usable => installed && enabled && ready;

  /// Reads the `status --json` object; anything unexpected counts as missing.
  static VoiceStatus parse(String json) {
    try {
      final m = jsonDecode(json.trim());
      if (m is! Map) return missing;
      return VoiceStatus(
        installed: true,
        enabled: m['enabled'] == true,
        ready: m['ready'] == true,
        whyNot: m['why_not'] is String ? m['why_not'] as String : '',
        message: m['message'] is String ? m['message'] as String : '',
        engine: m['engine'] is String ? m['engine'] as String : '',
        typedByEngine: m['typed_by_engine'] == true,
      );
    } on FormatException {
      return missing;
    }
  }
}

/// One line of `dictate --events`.
sealed class VoiceEvent {
  const VoiceEvent();
}

final class VoiceStarted extends VoiceEvent {
  const VoiceStarted(this.session);
  final String session;
}

/// `listening`, `transcribing`, `done`, `cancelled` or `error`.
final class VoiceStateEvent extends VoiceEvent {
  const VoiceStateEvent(this.state);
  final String state;
}

/// The microphone level, 0…1, about 20 times a second while listening.
final class VoiceLevel extends VoiceEvent {
  const VoiceLevel(this.rms);
  final double rms;
}

/// The whole text heard so far (local Whisper only).
final class VoicePartial extends VoiceEvent {
  const VoicePartial(this.text);
  final String text;
}

/// The result; [typed]: an external engine typed it itself and [text] is empty.
final class VoiceFinal extends VoiceEvent {
  const VoiceFinal(this.text, {this.typed = false, this.engine = ''});
  final String text;
  final bool typed;
  final String engine;
}

final class VoiceError extends VoiceEvent {
  const VoiceError(this.code, this.message);
  final String code;
  final String message;
}

/// Always last: `finished`, `silence`, `no_speech`, `cancelled`, `preempted`, `error`, … and
/// `exited` when the program ended without saying why.
final class VoiceClosed extends VoiceEvent {
  const VoiceClosed(this.reason);
  final String reason;
}

/// Lines longer than this are dropped (a partial text is a few hundred bytes).
const voiceMaxLine = 64 * 1024;

String _str(Object? v) => v is String ? v : '';

/// One JSON line to an event; null for anything unknown or broken.
VoiceEvent? parseVoiceEvent(String line) {
  if (line.length > voiceMaxLine) return null;
  Object? m;
  try {
    m = jsonDecode(line);
  } on FormatException {
    return null;
  }
  if (m is! Map) return null;
  switch (m['event']) {
    case 'started':
      return VoiceStarted(_str(m['session']));
    case 'state':
      final s = _str(m['state']);
      return s.isEmpty ? null : VoiceStateEvent(s);
    case 'level':
      final r = m['rms'];
      if (r is! num || r.isNaN) return null;
      return VoiceLevel(r.toDouble().clamp(0.0, 1.0));
    case 'partial':
      return VoicePartial(_str(m['text']));
    case 'final':
      return VoiceFinal(_str(m['text']), typed: m['typed'] == true, engine: _str(m['engine']));
    case 'error':
      return VoiceError(_str(m['code']), _str(m['message']));
    case 'closed':
      return VoiceClosed(_str(m['reason']).isEmpty ? 'exited' : _str(m['reason']));
  }
  return null;
}

/// Splits bytes into lines, dropping any line longer than [maxLine] (bounded memory).
final class VoiceLineSplitter extends StreamTransformerBase<List<int>, String> {
  const VoiceLineSplitter({this.maxLine = voiceMaxLine});
  final int maxLine;

  @override
  Stream<String> bind(Stream<List<int>> stream) async* {
    final buffer = <int>[];
    var skipping = false;
    await for (final chunk in stream) {
      for (final b in chunk) {
        if (b == 10) {
          if (!skipping) {
            var line = utf8.decode(buffer, allowMalformed: true);
            if (line.endsWith('\r')) line = line.substring(0, line.length - 1);
            yield line;
          }
          buffer.clear();
          skipping = false;
        } else if (!skipping) {
          buffer.add(b);
          if (buffer.length > maxLine) {
            buffer.clear();
            skipping = true;
          }
        }
      }
    }
    if (!skipping && buffer.isNotEmpty) yield utf8.decode(buffer, allowMalformed: true);
  }
}

/// Finding and asking the service.
abstract final class VoiceInput {
  /// Tests: the environment used to find the program (HOME, PATH) and given to it. Under
  /// `flutter test` (FLUTTER_TEST=true) the real service is never used unless this is set.
  static Map<String, String>? environment;

  /// How long an answer of [available] is reused.
  static Duration cacheFor = const Duration(seconds: 5);

  /// How long `status --json` may take (the first call may start the daemon).
  static Duration statusTimeout = const Duration(seconds: 8);

  /// No `started` (or error) within this time: the session is cancelled.
  static Duration startTimeout = const Duration(seconds: 15);

  /// After `finish`, no `closed` within this time: the program is ended.
  static Duration finishTimeout = const Duration(seconds: 90);

  /// After `cancel`, no `closed` within this time: the session counts as cancelled, stdin is
  /// closed and the program is ended (SIGTERM after [grace], then SIGKILL).
  static Duration cancelTimeout = const Duration(seconds: 5);

  /// After stdin closes, the program has this long to end before it is killed.
  static Duration grace = const Duration(seconds: 2);

  /// `status --json` output beyond this many bytes counts as broken.
  static const statusMaxBytes = 64 * 1024;

  /// Tests: [start] waits this long before it starts the program (a slow start).
  static Duration startDelay = Duration.zero;

  static VoiceStatus? _cached;
  static DateTime? _cachedAt;
  static Future<VoiceStatus>? _asking;

  static Map<String, String> get _env => environment ?? Platform.environment;

  /// `~/.local/bin/maprisma-voice`, else the first one on PATH; null when there is none.
  static String? executable() {
    if (environment == null && Platform.environment['FLUTTER_TEST'] == 'true') return null;
    final env = _env;
    final home = env['HOME'] ?? '';
    final candidates = <String>[
      if (home.isNotEmpty) '$home/.local/bin/maprisma-voice',
      for (final dir in (env['PATH'] ?? '').split(':'))
        if (dir.isNotEmpty) '$dir/maprisma-voice',
    ];
    for (final path in candidates) {
      try {
        final stat = File(path).statSync();
        if (stat.type == FileSystemEntityType.file && stat.mode & 0x49 != 0) return path; // any x bit
      } on FileSystemException {
        // next
      }
    }
    return null;
  }

  /// Forgets the cached status (tests, or after a setting changed).
  static void forget() {
    _cached = null;
    _cachedAt = null;
  }

  /// Whether mic buttons may show; asks `status --json` at most every [cacheFor] unless [fresh].
  static Future<VoiceStatus> available({bool fresh = false}) {
    final at = _cachedAt;
    if (!fresh && _cached != null && at != null && DateTime.now().difference(at) < cacheFor) {
      return Future.value(_cached);
    }
    return _asking ??= _ask().whenComplete(() => _asking = null);
  }

  /// The last answer, if any (no waiting).
  static VoiceStatus? get cached => _cached;

  static Future<VoiceStatus> _ask() async {
    var status = VoiceStatus.missing;
    final exe = executable();
    if (exe != null) {
      try {
        final out = await _statusJson(exe);
        if (out != null) status = VoiceStatus.parse(out);
      } on Object {
        status = VoiceStatus.missing; // no D-Bus, not runnable
      }
    }
    _cached = status;
    _cachedAt = DateTime.now();
    return status;
  }

  /// Runs `status --json`: its stdout (at most [statusMaxBytes]) when it exits 0 in time; on a
  /// timeout or too much output the program is killed (SIGTERM, SIGKILL after a second) and
  /// reaped, never left running.
  static Future<String?> _statusJson(String exe) async {
    final p = await Process.start(exe, const ['status', '--json'], environment: environment);
    final out = <int>[];
    var tooMuch = false;
    final done = Completer<void>();
    p.stdin.close().catchError((_) {});
    p.stderr.listen((_) {}, onError: (_) {});
    p.stdout.listen((b) {
      if (tooMuch) return;
      out.addAll(b);
      if (out.length > statusMaxBytes) {
        tooMuch = true;
        if (!done.isCompleted) done.complete();
      }
    }, onError: (_) {}, onDone: () {
      if (!done.isCompleted) done.complete();
    });
    int? code;
    try {
      code = await p.exitCode.timeout(statusTimeout);
    } on TimeoutException {
      code = null;
    }
    // After the exit, the rest of the output (something it started may hold the pipe: not forever).
    if (code != null) await done.future.timeout(const Duration(seconds: 1), onTimeout: () {});
    if (code == null || tooMuch) {
      p.kill();
      try {
        await p.exitCode.timeout(const Duration(seconds: 1));
      } on TimeoutException {
        p.kill(ProcessSignal.sigkill);
        await p.exitCode.timeout(const Duration(seconds: 2), onTimeout: () => -1);
      }
      return null;
    }
    return code == 0 ? utf8.decode(out, allowMalformed: true) : null;
  }

  /// Starts a dictation for [appId]. [release]: it ends on [VoiceSession.finish] (a second click);
  /// otherwise on silence. Throws [StateError] when the program is missing.
  static Future<VoiceSession> start({required String appId, String? language, bool release = true}) async {
    final exe = executable();
    if (exe == null) throw StateError('maprisma-voice is not installed');
    final args = <String>[
      'dictate',
      '--events',
      if (release) '--release',
      '--app-id',
      appId,
      if (language != null && language.isNotEmpty) ...['--language', language],
    ];
    if (startDelay > Duration.zero) await Future<void>.delayed(startDelay);
    final process = await Process.start(exe, args, environment: environment);
    return VoiceSession._(process);
  }
}

/// One running `dictate --events`. Listen to [events] (single listener; it ends after
/// [VoiceClosed]); call [finish] or [cancel]; always [dispose].
final class VoiceSession {
  VoiceSession._(this._process) {
    _process.stderr.listen((b) {
      _stderr.addAll(b);
      if (_stderr.length > 2048) _stderr.removeRange(0, _stderr.length - 2048);
    }, onError: (_) {});
    _process.stdout.transform(const VoiceLineSplitter()).listen(_line, onDone: _stdoutDone, onError: (_) {});
    _process.exitCode.then((code) {
      _exit = code;
      _exited.complete(code);
      if (_stdoutClosed) _end();
    });
    _watch = Timer(VoiceInput.startTimeout, () {
      if (!_started) _fail('timeout', 'The voice service did not answer.');
    });
  }

  final Process _process;
  final _controller = StreamController<VoiceEvent>();
  final _stderr = <int>[];
  final _exited = Completer<int>();
  Timer? _watch, _kill, _cancelWatch;
  bool _started = false, _errored = false, _closed = false, _stdinClosed = false, _stdoutClosed = false;
  int? _exit;

  /// The events, ending after [VoiceClosed].
  Stream<VoiceEvent> get events => _controller.stream;

  /// The session id once started.
  String session = '';

  /// [VoiceClosed] was seen (or made up).
  bool get closed => _closed;

  Future<int> get exitCode => _exited.future;

  void _emit(VoiceEvent e) {
    if (_closed) return;
    if (e is VoiceStarted || e is VoiceError) _started = true;
    if (e is VoiceError) _errored = true;
    if (e is VoiceStarted) session = e.session;
    if (e is VoiceClosed) {
      _closed = true;
      _watch?.cancel();
      _cancelWatch?.cancel();
      _controller.add(e);
      _controller.close();
      dispose(); // the program ends by itself now; killed if it does not
      return;
    }
    _controller.add(e);
  }

  void _line(String line) {
    final e = parseVoiceEvent(line);
    if (e != null) _emit(e);
  }

  void _stdoutDone() {
    _stdoutClosed = true;
    if (_exit != null) _end();
  }

  /// The program ended without a `closed` line.
  void _end() {
    _kill?.cancel();
    if (_closed) return;
    final why = utf8.decode(_stderr, allowMalformed: true).trim();
    if (_exit != 0 && !_errored) _emit(VoiceError('exited', why.isEmpty ? 'maprisma-voice ended ($_exit).' : why.split('\n').last));
    _emit(const VoiceClosed('exited'));
  }

  void _fail(String code, String message) {
    _emit(VoiceError(code, message));
    _emit(const VoiceClosed('error'));
    dispose();
  }

  void _write(String line) {
    if (_stdinClosed) return;
    try {
      _process.stdin.write(line);
      _process.stdin.flush().catchError((_) {});
    } on Object {
      // the program is gone; its end comes through exitCode
    }
  }

  /// Stop listening and transcribe (the second click).
  void finish() {
    _write('finish\n');
    _watch?.cancel();
    _watch = Timer(VoiceInput.finishTimeout, () {
      if (!_closed) _fail('timeout', 'The voice service did not answer.');
    });
  }

  /// Stop without text. If no `closed` comes within [VoiceInput.cancelTimeout], the session ends
  /// as `cancelled` anyway and the program is ended ([dispose]'s escalation).
  void cancel() {
    _write('cancel\n');
    if (_closed || _cancelWatch != null) return;
    _cancelWatch = Timer(VoiceInput.cancelTimeout, () {
      if (_closed) return;
      _emit(const VoiceClosed('cancelled')); // also disposes
    });
  }

  void _closeStdin() {
    if (_stdinClosed) return;
    _stdinClosed = true;
    _process.stdin.close().catchError((_) {});
  }

  /// Closes stdin (which cancels a running session) and ends the program after [VoiceInput.grace].
  void dispose() {
    _watch?.cancel();
    _closeStdin();
    if (_exit != null || _kill != null) return;
    _kill = Timer(VoiceInput.grace, () {
      if (_exit != null) return;
      _process.kill();
      _kill = Timer(const Duration(seconds: 1), () {
        if (_exit == null) _process.kill(ProcessSignal.sigkill);
      });
    });
  }
}
