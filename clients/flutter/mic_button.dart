// Maprisma Dictation: the mic button of a prompt field, with the aura ring.
//
// Canonical copy: maprisma/dictation clients/flutter/mic_button.dart. Copy it unchanged into each
// app next to voice_input.dart; changes go there first. Flutter only, no packages.
//
// - Shows only while the voice service is installed, switched on and ready (checked when it
//   appears, when the window comes back to the front and when the field gets focus).
// - Click: start listening; second click: stop and insert. Esc while it runs cancels.
// - While it runs, the Perimeter Aura Ring (research/antigravity-design.md B) hugs the button:
//   a 4 px ring, its colours from the theme accent in OKLCH, its width and glow from the level.
// - Texts come from the host's tr(): "Dictate", "Stop and insert", "Listening…",
//   "Dictation is not available: {0}".

import 'dart:async';
import 'dart:math' as math;

import 'package:flutter/material.dart';
import 'package:flutter/scheduler.dart';
import 'package:flutter/services.dart';

import 'voice_input.dart';

/// Puts dictated [text] where the cursor is in [controller], replacing the selection, with a space
/// before and after it where the neighbouring text needs one. The cursor ends after it.
void insertDictatedText(TextEditingController controller, String text) {
  final words = text.trim();
  if (words.isEmpty) return;
  final value = controller.value;
  final all = value.text;
  final sel = value.selection;
  final start = sel.isValid ? math.min(sel.start, sel.end).clamp(0, all.length) : all.length;
  final end = sel.isValid ? math.max(sel.start, sel.end).clamp(0, all.length) : all.length;
  final before = all.substring(0, start);
  final after = all.substring(end);
  var piece = words;
  if (before.isNotEmpty && !RegExp(r'\s$').hasMatch(before)) piece = ' $piece';
  if (after.isNotEmpty && !RegExp(r'^[\s.,;:!?)\]}»”]').hasMatch(after)) piece = '$piece ';
  final caret = before.length + piece.length - (piece.endsWith(' ') && after.isNotEmpty ? 1 : 0);
  controller.value = TextEditingValue(
    text: before + piece + after,
    selection: TextSelection.collapsed(offset: caret),
  );
}

/// What the button is doing.
enum MicPhase { idle, starting, listening, transcribing, done, error }

/// The mic button. Give it the field's [controller] (the final text is inserted there) or an
/// [onFinal]; [onPartial] gets the text to show greyed in the field ("Listening…", then what was
/// heard so far; empty: show nothing).
class MicButton extends StatefulWidget {
  const MicButton({
    super.key,
    required this.appId,
    required this.tr,
    this.controller,
    this.focusNode,
    this.onPartial,
    this.onFinal,
    this.language,
    this.size = 40,
    this.iconSize = 20,
    this.enabled = true,
  });

  /// The app's id, e.g. com.maprisma.Launcher.
  final String appId;

  /// The host's translation function.
  final String Function(String english) tr;

  final TextEditingController? controller;

  /// The field: the status is checked again when it gets focus, and it gets focus back after
  /// the text is inserted.
  final FocusNode? focusNode;
  final ValueChanged<String>? onPartial;
  final ValueChanged<String>? onFinal;

  /// A Whisper language code, or null for the service's setting.
  final String? language;
  final double size;
  final double iconSize;

  /// False: shown but not clickable (e.g. while the app is asking).
  final bool enabled;

  @override
  State<MicButton> createState() => _MicButtonState();
}

class _MicButtonState extends State<MicButton> with SingleTickerProviderStateMixin, WidgetsBindingObserver {
  VoiceStatus? _status = VoiceInput.cached;
  VoiceSession? _session;
  StreamSubscription<VoiceEvent>? _sub;
  MicPhase _phase = MicPhase.idle;
  String _error = '';

  /// Bumped by every start and by a cancel before the session exists: a [VoiceInput.start]
  /// that comes back for an older number is disposed at once, never listened to.
  int _startGen = 0;

  /// A [VoiceInput.start] is still on its way (no second one meanwhile).
  bool _startPending = false;
  Timer? _clear;
  late final Ticker _ticker = createTicker(_tick);
  final _ring = _RingState();
  Duration _last = Duration.zero;

  bool get _running => _phase == MicPhase.starting || _phase == MicPhase.listening || _phase == MicPhase.transcribing;

  @override
  void initState() {
    super.initState();
    WidgetsBinding.instance.addObserver(this);
    widget.focusNode?.addListener(_focusChanged);
    _check();
  }

  @override
  void didUpdateWidget(MicButton old) {
    super.didUpdateWidget(old);
    if (old.focusNode != widget.focusNode) {
      old.focusNode?.removeListener(_focusChanged);
      widget.focusNode?.addListener(_focusChanged);
    }
  }

  @override
  void dispose() {
    WidgetsBinding.instance.removeObserver(this);
    widget.focusNode?.removeListener(_focusChanged);
    HardwareKeyboard.instance.removeHandler(_key);
    _clear?.cancel();
    _sub?.cancel();
    _session?.dispose();
    _ticker.dispose();
    super.dispose();
  }

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    if (state == AppLifecycleState.resumed) _check();
  }

  void _focusChanged() {
    if (widget.focusNode?.hasFocus ?? false) _check();
  }

  Future<void> _check({bool fresh = false}) async {
    final s = await VoiceInput.available(fresh: fresh);
    if (mounted) setState(() => _status = s);
  }

  void _partial(String text) => widget.onPartial?.call(text);

  Future<void> _click() async {
    switch (_phase) {
      case MicPhase.listening:
        _session?.finish();
        _setPhase(MicPhase.transcribing);
      case MicPhase.starting:
        _cancel();
      case MicPhase.transcribing:
        break;
      case MicPhase.idle || MicPhase.done || MicPhase.error:
        if (!_startPending) await _start();
    }
  }

  Future<void> _start() async {
    _clear?.cancel();
    _error = '';
    final gen = ++_startGen;
    _startPending = true;
    _setPhase(MicPhase.starting);
    HardwareKeyboard.instance.addHandler(_key);
    VoiceSession? session;
    Object? failure;
    try {
      session = await VoiceInput.start(appId: widget.appId, language: widget.language);
    } on Object catch (e) {
      failure = e;
    } finally {
      _startPending = false;
    }
    // Cancelled (Esc, a click) or superseded while it was starting, or the widget is gone.
    if (!mounted || gen != _startGen) {
      session?.dispose();
      return;
    }
    if (session == null) {
      _event(VoiceError('start', '$failure'));
      _event(const VoiceClosed('error'));
      return;
    }
    _session = session;
    _sub = session.events.listen(_event);
  }

  /// Esc, or a click while starting: cancels the session, or the start still on its way.
  void _cancel() {
    final session = _session;
    if (session != null) {
      session.cancel();
      return;
    }
    _startGen++; // the pending start is disposed when it comes back
    HardwareKeyboard.instance.removeHandler(_key);
    _partial('');
    _setPhase(MicPhase.idle);
  }

  void _event(VoiceEvent e) {
    if (!mounted) return;
    switch (e) {
      case VoiceStarted():
        break;
      case VoiceStateEvent(:final state):
        if (state == 'listening') {
          _setPhase(MicPhase.listening);
          _partial(widget.tr('Listening…'));
        } else if (state == 'transcribing') {
          _setPhase(MicPhase.transcribing);
        }
      case VoiceLevel(:final rms):
        _ring.target = rms;
      case VoicePartial(:final text):
        _partial(text.trim().isEmpty ? widget.tr('Listening…') : text);
      case VoiceFinal(:final text, :final typed):
        _partial('');
        if (!typed && text.trim().isNotEmpty) {
          if (widget.onFinal != null) {
            widget.onFinal!(text.trim());
          } else if (widget.controller != null) {
            insertDictatedText(widget.controller!, text);
          }
          widget.focusNode?.requestFocus();
        }
        _setPhase(MicPhase.done);
      case VoiceError(:final message):
        _error = message;
        _setPhase(MicPhase.error);
      case VoiceClosed(:final reason):
        HardwareKeyboard.instance.removeHandler(_key);
        _sub?.cancel();
        _sub = null;
        _session?.dispose();
        _session = null;
        if (_phase == MicPhase.error) {
          final text = widget.tr('Dictation is not available: {0}').replaceAll('{0}', _error);
          _partial(text);
          _clear = Timer(const Duration(seconds: 5), () => _partial(''));
          VoiceInput.forget();
        } else {
          _partial('');
          if (_phase != MicPhase.done) _setPhase(reason == 'finished' || reason == 'silence' ? MicPhase.done : MicPhase.idle);
        }
    }
  }

  void _setPhase(MicPhase phase) {
    if (!mounted) return;
    setState(() => _phase = phase);
    _ring.phase(phase);
    if (!_ticker.isActive) {
      _last = Duration.zero;
      _ticker.start();
    }
  }

  /// Esc while it runs cancels (before the window's own Esc handling).
  bool _key(KeyEvent event) {
    if (!_running || event.logicalKey != LogicalKeyboardKey.escape) return false;
    if (event is KeyDownEvent) _cancel();
    return true;
  }

  void _tick(Duration now) {
    final dt = _last == Duration.zero ? 0.016 : (now - _last).inMicroseconds / 1e6;
    _last = now;
    final keep = _ring.step(dt, running: _running);
    if (!keep) {
      _ticker.stop();
      if (_phase == MicPhase.done || _phase == MicPhase.error) {
        setState(() => _phase = MicPhase.idle);
        _ring.phase(MicPhase.idle);
      }
    }
    setState(() {});
  }

  @override
  Widget build(BuildContext context) {
    final status = _status;
    // Hidden without a usable service; a running session keeps its button (to stop it).
    if ((status == null || !status.usable) && !_running) return const SizedBox.shrink();
    final scheme = Theme.of(context).colorScheme;
    final dark = Theme.of(context).brightness == Brightness.dark;
    final listening = _phase == MicPhase.listening || _phase == MicPhase.starting;
    final tip = switch (_phase) {
      MicPhase.listening || MicPhase.starting => widget.tr('Stop and insert'),
      MicPhase.transcribing => widget.tr('Listening…'),
      MicPhase.error => widget.tr('Dictation is not available: {0}').replaceAll('{0}', _error),
      _ => widget.tr('Dictate'),
    };
    final reduce = MediaQuery.maybeDisableAnimationsOf(context) ?? false;
    return Semantics(
      button: true,
      toggled: listening,
      child: CustomPaint(
        foregroundPainter: AuraRingPainter(
          ring: _ring.snapshot(reduce),
          palette: AuraPalette.fromAccent(scheme.primary, dark: dark, error: scheme.error),
        ),
        child: SizedBox.square(
          dimension: widget.size,
          child: IconButton(
            key: const ValueKey('voice-mic'),
            tooltip: tip,
            iconSize: widget.iconSize,
            padding: EdgeInsets.zero,
            constraints: BoxConstraints.tight(Size.square(widget.size)),
            onPressed: widget.enabled || _running ? _click : null,
            icon: Icon(
              listening ? Icons.mic : Icons.mic_none,
              color: listening ? scheme.primary : null,
            ),
          ),
        ),
      ),
    );
  }
}

/// The ring's moving parts: level envelope (attack 30 ms, release 130 ms), ripple, flash, shake.
final class _RingState {
  double target = 0, level = 0;

  /// 0…1 how much the ring shows (fades in 160 ms, out at the end).
  double show = 0;
  double spin = 0;
  double ripple = 0; // transcribing, 600 ms cycles
  double flash = 0; // done, 280 ms
  double fail = 0; // error, 600 ms
  MicPhase _phase = MicPhase.idle;

  void phase(MicPhase p) {
    if (p == MicPhase.done) flash = 1;
    if (p == MicPhase.error) fail = 1;
    if (p != MicPhase.listening) target = 0;
    _phase = p;
  }

  /// Advances by [dt] seconds; false when nothing moves any more.
  bool step(double dt, {required bool running}) {
    final tau = target > level ? 0.030 : 0.130;
    level += (target - level) * (1 - math.exp(-dt / tau));
    final want = running ? 1.0 : 0.0;
    show += (want - show) * (1 - math.exp(-dt / (running ? 0.05 : 0.12)));
    spin = (spin + dt / 1.4) % 1.0;
    if (_phase == MicPhase.transcribing) ripple = (ripple + dt / 0.6) % 1.0;
    flash = math.max(0, flash - dt / 0.28);
    fail = math.max(0, fail - dt / 0.6);
    return running || show > 0.01 || flash > 0 || fail > 0 || level > 0.01;
  }

  AuraRing snapshot(bool reduceMotion) => AuraRing(
    level: reduceMotion ? 0.3 * show : level,
    show: show,
    spin: reduceMotion ? 0 : spin,
    ripple: _phase == MicPhase.transcribing && !reduceMotion ? ripple : -1,
    flash: flash,
    fail: fail,
  );
}

/// One frame of the ring.
@immutable
final class AuraRing {
  const AuraRing({this.level = 0, this.show = 0, this.spin = 0, this.ripple = -1, this.flash = 0, this.fail = 0});
  final double level, show, spin, ripple, flash, fail;

  @override
  bool operator ==(Object other) =>
      other is AuraRing &&
      other.level == level &&
      other.show == show &&
      other.spin == spin &&
      other.ripple == ripple &&
      other.flash == flash &&
      other.fail == fail;

  @override
  int get hashCode => Object.hash(level, show, spin, ripple, flash, fail);
}

/// The ring's colours from the accent (OKLCH): primary hue, +28° and a −20° peak (L + 0.08);
/// dark themes L 0.74 / C 0.10–0.18, light themes L 0.50 / C 0.12–0.20.
@immutable
final class AuraPalette {
  const AuraPalette(this.primary, this.secondary, this.peak, this.done, this.error);
  final Color primary, secondary, peak, done, error;

  factory AuraPalette.fromAccent(Color accent, {required bool dark, Color? error}) {
    final (_, c0, h) = oklchOf(accent);
    final l = dark ? 0.74 : 0.50;
    final c = dark ? c0.clamp(0.10, 0.18) : c0.clamp(0.12, 0.20);
    return AuraPalette(
      oklchColor(l, c, h),
      oklchColor(l, c, h + 28),
      oklchColor(l + 0.08, c, h - 20),
      oklchColor(l, c, 155), // emerald
      error ?? oklchColor(l, c, 25),
    );
  }

  @override
  bool operator ==(Object other) =>
      other is AuraPalette && other.primary == primary && other.secondary == secondary && other.peak == peak && other.error == error;

  @override
  int get hashCode => Object.hash(primary, secondary, peak, error);
}

double _toLinear(double c) => c <= 0.04045 ? c / 12.92 : math.pow((c + 0.055) / 1.055, 2.4).toDouble();
double _toGamma(double c) => c <= 0.0031308 ? 12.92 * c : 1.055 * math.pow(c, 1 / 2.4) - 0.055;

/// sRGB colour → OKLCH (L 0…1, C, H in degrees).
(double, double, double) oklchOf(Color color) {
  final r = _toLinear(color.r), g = _toLinear(color.g), b = _toLinear(color.b);
  final l = math.pow(0.4122214708 * r + 0.5363325363 * g + 0.0514459929 * b, 1 / 3).toDouble();
  final m = math.pow(0.2119034982 * r + 0.6806995451 * g + 0.1073969566 * b, 1 / 3).toDouble();
  final s = math.pow(0.0883024619 * r + 0.2817188376 * g + 0.6299787005 * b, 1 / 3).toDouble();
  final ll = 0.2104542553 * l + 0.7936177850 * m - 0.0040720468 * s;
  final a = 1.9779984951 * l - 2.4285922050 * m + 0.4505937099 * s;
  final bb = 0.0259040371 * l + 0.7827717662 * m - 0.8086757660 * s;
  final h = (math.atan2(bb, a) * 180 / math.pi) % 360;
  return (ll, math.sqrt(a * a + bb * bb), h);
}

(double, double, double) _rgbOf(double l, double c, double h) {
  final hr = h * math.pi / 180;
  final a = c * math.cos(hr), b = c * math.sin(hr);
  final l_ = l + 0.3963377774 * a + 0.2158037573 * b;
  final m_ = l - 0.1055613458 * a - 0.0638541728 * b;
  final s_ = l - 0.0894841775 * a - 1.2914855480 * b;
  final ll = l_ * l_ * l_, mm = m_ * m_ * m_, ss = s_ * s_ * s_;
  return (
    4.0767416621 * ll - 3.3077115913 * mm + 0.2309699292 * ss,
    -1.2684380046 * ll + 2.6097574011 * mm - 0.3413193965 * ss,
    -0.0041960863 * ll - 0.7034186147 * mm + 1.7076147010 * ss,
  );
}

/// OKLCH → an sRGB colour; the chroma is lowered until it fits the gamut.
Color oklchColor(double l, double c, double h) {
  l = l.clamp(0.0, 1.0);
  var chroma = c;
  for (var i = 0; i < 24; i++) {
    final (r, g, b) = _rgbOf(l, chroma, h);
    const e = 1e-4;
    if (r >= -e && r <= 1 + e && g >= -e && g <= 1 + e && b >= -e && b <= 1 + e) break;
    chroma *= 0.9;
  }
  final (r, g, b) = _rgbOf(l, chroma, h);
  return Color.from(
    alpha: 1,
    red: _toGamma(r.clamp(0.0, 1.0)).clamp(0.0, 1.0),
    green: _toGamma(g.clamp(0.0, 1.0)).clamp(0.0, 1.0),
    blue: _toGamma(b.clamp(0.0, 1.0)).clamp(0.0, 1.0),
  );
}

/// Paints the aura ring inside the button's square: a 4 px band at its edge, a gradient stroke of
/// 1.5–4 px and a soft glow (15–65 % opacity) driven by the level.
class AuraRingPainter extends CustomPainter {
  AuraRingPainter({required this.ring, required this.palette});
  final AuraRing ring;
  final AuraPalette palette;

  static const band = 4.0;

  @override
  void paint(Canvas canvas, Size size) {
    final visible = math.max(ring.show, math.max(ring.flash, ring.fail));
    if (visible <= 0.01) return;
    final shake = ring.fail > 0 ? math.sin((1 - ring.fail) * math.pi * 2 * 2 * (0.6 / 0.32)) * 4 * ring.fail : 0.0;
    final center = size.center(Offset(shake, 0));
    final outer = size.shortestSide / 2;
    final width = 1.5 + 2.5 * ring.level;
    final radius = outer - band / 2;
    final rect = Rect.fromCircle(center: center, radius: radius);

    // Glow
    final glow = Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = band
      ..maskFilter = const MaskFilter.blur(BlurStyle.normal, 2.5)
      ..color = palette.primary.withValues(alpha: (0.15 + 0.5 * ring.level) * ring.show);
    canvas.drawCircle(center, radius, glow);

    // Gradient stroke, slowly turning
    final turn = ring.spin * 2 * math.pi;
    final stroke = Paint()
      ..style = PaintingStyle.stroke
      ..strokeWidth = width
      ..shader = SweepGradient(
        colors: [palette.primary, palette.secondary, palette.peak, palette.primary],
        transform: GradientRotation(turn),
      ).createShader(rect);
    if (ring.show > 0.01) {
      canvas.saveLayer(rect.inflate(band), Paint()..color = Colors.white.withValues(alpha: ring.show.clamp(0.0, 1.0)));
      canvas.drawCircle(center, radius, stroke);
      canvas.restore();
    }

    // Transcribing: a ripple moving inwards
    if (ring.ripple >= 0) {
      final p = Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = 1.5
        ..color = palette.secondary.withValues(alpha: (1 - ring.ripple) * 0.6 * ring.show);
      canvas.drawCircle(center, radius * (1 - 0.35 * ring.ripple), p);
    }
    if (ring.flash > 0) {
      canvas.drawCircle(center, radius, Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = band
        ..color = palette.done.withValues(alpha: ring.flash));
    }
    if (ring.fail > 0) {
      canvas.drawCircle(center, radius, Paint()
        ..style = PaintingStyle.stroke
        ..strokeWidth = band
        ..color = palette.error.withValues(alpha: ring.fail));
    }
  }

  @override
  bool shouldRepaint(AuraRingPainter old) => old.ring != ring || old.palette != palette;
}
