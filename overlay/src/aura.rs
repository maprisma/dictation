//! The aura ring's maths (docs/research/antigravity-design.md B, option 1): colours from the
//! theme's accent in OKLCH, the level envelope, and the timings of each state. Drawing is in
//! ui.rs; everything here is pure and tested.

use std::f32::consts::PI;

/// sRGB, 0…1 per channel.
pub type Rgb = [f32; 3];

fn to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn from_linear(c: f32) -> f32 {
    if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 }
}

/// sRGB → OKLCH (L 0…1, C, H in degrees 0…360).
pub fn oklch([r, g, b]: Rgb) -> [f32; 3] {
    let (r, g, b) = (to_linear(r), to_linear(g), to_linear(b));
    let l = (0.412_221_46 * r + 0.536_332_55 * g + 0.051_445_995 * b).cbrt();
    let m = (0.211_903_5 * r + 0.680_699_5 * g + 0.107_396_96 * b).cbrt();
    let s = (0.088_302_46 * r + 0.281_718_85 * g + 0.629_978_7 * b).cbrt();
    let ll = 0.210_454_26 * l + 0.793_617_8 * m - 0.004_072_047 * s;
    let a = 1.977_998_5 * l - 2.428_592_2 * m + 0.450_593_7 * s;
    let bb = 0.025_904_037 * l + 0.782_771_77 * m - 0.808_675_77 * s;
    let c = (a * a + bb * bb).sqrt();
    let h = bb.atan2(a).to_degrees().rem_euclid(360.0);
    [ll, c, h]
}

/// OKLCH → linear sRGB (may be outside 0…1).
fn oklch_linear([l, c, h]: [f32; 3]) -> [f32; 3] {
    let (a, b) = (c * h.to_radians().cos(), c * h.to_radians().sin());
    let l_ = l + 0.396_337_78 * a + 0.215_803_76 * b;
    let m_ = l - 0.105_561_346 * a - 0.063_854_17 * b;
    let s_ = l - 0.089_484_18 * a - 1.291_485_5 * b;
    let (l3, m3, s3) = (l_ * l_ * l_, m_ * m_ * m_, s_ * s_ * s_);
    [
        4.076_741_7 * l3 - 3.307_711_6 * m3 + 0.230_969_94 * s3,
        -1.268_438 * l3 + 2.609_757_4 * m3 - 0.341_319_38 * s3,
        -0.004_196_086_3 * l3 - 0.703_418_6 * m3 + 1.707_614_7 * s3,
    ]
}

/// OKLCH → sRGB; chroma is lowered until the colour fits (no clipping of hue).
pub fn srgb([l, c, h]: [f32; 3]) -> Rgb {
    let fits = |c: f32| oklch_linear([l, c, h]).iter().all(|v| (-0.0005..=1.0005).contains(v));
    let c = if fits(c) {
        c
    } else {
        let (mut lo, mut hi) = (0.0, c);
        for _ in 0..24 {
            let mid = (lo + hi) / 2.0;
            if fits(mid) { lo = mid } else { hi = mid }
        }
        lo
    };
    oklch_linear([l, c, h]).map(|v| from_linear(v.clamp(0.0, 1.0)))
}

/// WCAG relative luminance.
pub fn luminance([r, g, b]: Rgb) -> f32 {
    0.2126 * to_linear(r) + 0.7152 * to_linear(g) + 0.0722 * to_linear(b)
}

pub fn contrast(a: Rgb, b: Rgb) -> f32 {
    let (x, y) = (luminance(a), luminance(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// What the ring shows in colour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// The theme's accent hue.
    Accent,
    /// Amber (H 75): a confirmation waits (step 5).
    Confirm,
    /// Green (H 150): done.
    Done,
    /// Red (H 25): an error.
    Error,
}

/// The ring's three stops: primary H, secondary H+28°, energy peak H−20° (L ± 0.08).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AuraColors {
    pub primary: Rgb,
    pub secondary: Rgb,
    pub peak: Rgb,
}

/// The ring's colours from the theme's accent: dark themes L 0.74, C 0.10–0.18; light themes
/// L 0.50, C 0.12–0.20 (keeps ≥ 4.5:1 against white).
pub fn aura_colors(accent: Rgb, dark: bool, tone: Tone) -> AuraColors {
    let [_, c, h] = oklch(accent);
    let (l, c) = if dark { (0.74, c.clamp(0.10, 0.18)) } else { (0.50, c.clamp(0.12, 0.20)) };
    let h = match tone {
        Tone::Accent => h,
        Tone::Confirm => 75.0,
        Tone::Done => 150.0,
        Tone::Error => 25.0,
    };
    let peak_l = if dark { l + 0.08 } else { l - 0.08 };
    AuraColors {
        primary: srgb([l, c, h]),
        secondary: srgb([l, c, (h + 28.0).rem_euclid(360.0)]),
        peak: srgb([peak_l, c, (h - 20.0).rem_euclid(360.0)]),
    }
}

/// The microphone's RMS (0…1, linear) as a level 0…1 for the ring: −50 dBFS and below is 0,
/// −12 dBFS and above is 1 (speech sits in between).
pub fn level_from_rms(rms: f64) -> f32 {
    if !rms.is_finite() || rms <= 0.0 {
        return 0.0;
    }
    let db = 20.0 * rms.log10();
    ((db + 50.0) / 38.0).clamp(0.0, 1.0) as f32
}

/// Attack 30 ms, release 130 ms.
pub const ATTACK: f32 = 0.030;
pub const RELEASE: f32 = 0.130;

/// A one-pole follower with separate rise (attack) and fall (release) times.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Envelope {
    pub value: f32,
}

impl Envelope {
    pub fn step(&mut self, target: f32, dt: f32) -> f32 {
        let tau = if target > self.value { ATTACK } else { RELEASE };
        let k = 1.0 - (-dt.max(0.0) / tau).exp();
        self.value += (target.clamp(0.0, 1.0) - self.value) * k;
        self.value
    }
}

/// Stroke width 1.5–3.5 px by level.
pub fn stroke_width(level: f32) -> f32 {
    1.5 + 2.0 * level.clamp(0.0, 1.0)
}

/// Bloom opacity 15–65 % by level.
pub fn bloom(level: f32) -> f32 {
    0.15 + 0.50 * level.clamp(0.0, 1.0)
}

/// cubic-bezier(x1, y1, x2, y2) at progress t (0…1).
pub fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let bez = |a: f32, b: f32, s: f32| 3.0 * a * s * (1.0 - s).powi(2) + 3.0 * b * s * s * (1.0 - s) + s.powi(3);
    // Solve x(s) = t by bisection (monotone for 0 ≤ x1, x2 ≤ 1).
    let (mut lo, mut hi) = (0.0f32, 1.0f32);
    for _ in 0..30 {
        let mid = (lo + hi) / 2.0;
        if bez(x1, x2, mid) < t { lo = mid } else { hi = mid }
    }
    bez(y1, y2, (lo + hi) / 2.0)
}

/// The pill's size change: 220 ms, cubic-bezier(0.2, 0, 0, 1).
pub const MORPH: f32 = 0.220;
pub fn morph(t: f32) -> f32 {
    cubic_bezier(0.2, 0.0, 0.0, 1.0, t / MORPH)
}

/// Done: a 280 ms green flash settling to no bloom (ease-out); the flash's strength 1 → 0.
pub const DONE_FLASH: f32 = 0.280;
pub fn done_flash(t: f32) -> f32 {
    let p = (t / DONE_FLASH).clamp(0.0, 1.0);
    1.0 - (1.0 - (1.0 - p).powi(3)).min(1.0)
}

/// Error: a 320 ms damped horizontal shake of ±4 px, 2 cycles.
pub const SHAKE: f32 = 0.320;
pub fn shake(t: f32) -> f32 {
    if !(0.0..SHAKE).contains(&t) {
        return 0.0;
    }
    let p = t / SHAKE;
    4.0 * (1.0 - p) * (2.0 * 2.0 * PI * p).sin()
}

/// Thinking: a 1.4 s linear sweep; the gradient's angle in radians.
pub const SWEEP: f32 = 1.4;
pub fn sweep_angle(t: f32) -> f32 {
    (t / SWEEP).fract() * 2.0 * PI
}

/// Transcribing: a 600 ms inward ripple; how far in (0…1) the ripple is.
pub const RIPPLE: f32 = 0.600;
pub fn ripple(t: f32) -> f32 {
    let p = (t / RIPPLE).fract();
    // ease-in-out
    if p < 0.5 { 2.0 * p * p } else { 1.0 - (-2.0 * p + 2.0).powi(2) / 2.0 }
}

/// Idle: a slow 0.2 Hz breath (0…1).
pub fn breath(t: f32) -> f32 {
    0.5 - 0.5 * (2.0 * PI * 0.2 * t).cos()
}

/// Speaking: a gentle pulse, 0…1, about one swell per 1.1 s (a speaking pace without a level).
pub fn speak_pulse(t: f32) -> f32 {
    let slow = 0.5 - 0.5 * (2.0 * PI * t / 1.1).cos();
    let fine = 0.5 - 0.5 * (2.0 * PI * t / 0.37).cos();
    (0.8 * slow + 0.2 * fine).clamp(0.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaking_pulse_is_gentle() {
        let v: Vec<f32> = (0..200).map(|i| speak_pulse(i as f32 * 0.01)).collect();
        assert!(v.iter().all(|x| (0.0..=1.0).contains(x)));
        assert!(v[0] < 0.01 && v.iter().cloned().fold(0.0, f32::max) > 0.8);
        // no jumps between 60 Hz frames
        assert!((1..200).all(|i| (speak_pulse(i as f32 / 60.0) - speak_pulse((i - 1) as f32 / 60.0)).abs() < 0.08));
    }

    fn hex(v: u32) -> Rgb {
        [((v >> 16) & 255) as f32 / 255.0, ((v >> 8) & 255) as f32 / 255.0, (v & 255) as f32 / 255.0]
    }

    #[test]
    fn oklch_round_trip() {
        for c in [0xe78a4e, 0x3daee9, 0x63b3ed, 0x000000, 0xffffff, 0x8a2be2, 0x10a050] {
            let rgb = hex(c);
            let back = srgb(oklch(rgb));
            for i in 0..3 {
                assert!((back[i] - rgb[i]).abs() < 0.003, "{c:06x}: {back:?} vs {rgb:?}");
            }
        }
        // Known value: pure sRGB red is about L 0.628, C 0.258, H 29.2.
        let [l, c, h] = oklch([1.0, 0.0, 0.0]);
        assert!((l - 0.628).abs() < 0.002 && (c - 0.258).abs() < 0.002 && (h - 29.2).abs() < 0.3);
    }

    #[test]
    fn dark_theme_colours() {
        let accent = hex(0xe78a4e);
        let colors = aura_colors(accent, true, Tone::Accent);
        let [l, c, h] = oklch(colors.primary);
        assert!((l - 0.74).abs() < 0.01, "{l}");
        assert!((0.099..=0.181).contains(&c), "{c}");
        assert!((h - oklch(accent)[2]).abs() < 1.5);
        let [_, _, h2] = oklch(colors.secondary);
        assert!(((h2 - h).rem_euclid(360.0) - 28.0).abs() < 1.5, "{h2} {h}");
        let [lp, _, hp] = oklch(colors.peak);
        assert!((lp - 0.82).abs() < 0.01);
        assert!(((h - hp).rem_euclid(360.0) - 20.0).abs() < 1.5);
        // A glow that reads on dark surfaces.
        for surface in [0x1b1b1b, 0x231a13, 0x2e3440] {
            assert!(contrast(colors.primary, hex(surface)) >= 4.5, "{surface:06x}");
        }
        // A grey accent still gets colour (C at least 0.10).
        assert!(oklch(aura_colors(hex(0x808080), true, Tone::Accent).primary)[1] >= 0.095);
    }

    #[test]
    fn light_theme_colours_keep_contrast_on_white() {
        for accent in [0x3daee9, 0xe78a4e, 0xf9e2af, 0x10a050, 0x8a2be2, 0xffff00] {
            let colors = aura_colors(hex(accent), false, Tone::Accent);
            let [l, c, _] = oklch(colors.primary);
            assert!((l - 0.50).abs() < 0.01 && (0.0..=0.201).contains(&c));
            assert!(contrast(colors.primary, [1.0; 3]) >= 4.5, "{accent:06x}: {}", contrast(colors.primary, [1.0; 3]));
            assert!(contrast(colors.peak, [1.0; 3]) >= 4.5);
        }
    }

    #[test]
    fn tones_override_the_hue() {
        let accent = hex(0x3daee9);
        for (tone, hue) in [(Tone::Confirm, 75.0), (Tone::Done, 150.0), (Tone::Error, 25.0)] {
            for dark in [true, false] {
                let h = oklch(aura_colors(accent, dark, tone).primary)[2];
                assert!((h - hue).abs() < 2.0, "{tone:?} {dark}: {h}");
            }
        }
    }

    #[test]
    fn envelope_attack_and_release() {
        let mut e = Envelope::default();
        // Attack: about 63 % after 30 ms, nearly there after 100 ms.
        for _ in 0..30 {
            e.step(1.0, 0.001);
        }
        assert!((e.value - 0.632).abs() < 0.01, "{}", e.value);
        for _ in 0..70 {
            e.step(1.0, 0.001);
        }
        assert!(e.value > 0.96);
        // Release is slower: 63 % of the way down after 130 ms, not after 30 ms.
        let mut d = Envelope { value: 1.0 };
        for _ in 0..30 {
            d.step(0.0, 0.001);
        }
        assert!(d.value > 0.75, "{}", d.value);
        for _ in 0..100 {
            d.step(0.0, 0.001);
        }
        assert!((d.value - 0.368).abs() < 0.01, "{}", d.value);
        // Frame-rate independent: one 16 ms step ≈ sixteen 1 ms steps.
        let (mut a, mut b) = (Envelope::default(), Envelope::default());
        a.step(1.0, 0.016);
        for _ in 0..16 {
            b.step(1.0, 0.001);
        }
        assert!((a.value - b.value).abs() < 1e-4);
        // Out-of-range targets and negative time are tamed.
        let mut c = Envelope::default();
        c.step(5.0, 1.0);
        assert!(c.value <= 1.0);
        c.step(0.0, -1.0);
        assert!(c.value <= 1.0);
    }

    #[test]
    fn level_drives_width_and_bloom() {
        assert_eq!(level_from_rms(0.0), 0.0);
        assert_eq!(level_from_rms(f64::NAN), 0.0);
        assert_eq!(level_from_rms(1.0), 1.0);
        assert!(level_from_rms(0.003) < 0.05); // ≈ −50 dB room noise
        let speech = level_from_rms(0.05); // ≈ −26 dB
        assert!((0.4..0.8).contains(&speech), "{speech}");
        assert_eq!((stroke_width(0.0), stroke_width(1.0), stroke_width(2.0)), (1.5, 3.5, 3.5));
        assert_eq!((bloom(0.0), bloom(1.0)), (0.15, 0.65));
    }

    #[test]
    fn timings() {
        assert!(morph(0.0).abs() < 1e-3 && (morph(MORPH) - 1.0).abs() < 1e-3 && morph(1.0) == 1.0);
        // ease-out-ish: more than half way at 30 % of the time.
        assert!(morph(0.3 * MORPH) > 0.5);
        assert!((done_flash(0.0) - 1.0).abs() < 1e-6 && done_flash(DONE_FLASH) == 0.0);
        assert_eq!(shake(-0.1), 0.0);
        assert_eq!(shake(SHAKE), 0.0);
        let peak = (0..320).map(|ms| shake(ms as f32 / 1000.0).abs()).fold(0.0f32, f32::max);
        assert!((3.0..=4.0).contains(&peak), "{peak}");
        assert!((sweep_angle(0.7) - PI).abs() < 1e-4 && sweep_angle(1.4) < 1e-4);
        assert!(ripple(0.0) == 0.0 && (ripple(0.3) - 0.5).abs() < 1e-4);
        assert!(breath(0.0) == 0.0 && (breath(2.5) - 1.0).abs() < 1e-4);
    }
}
