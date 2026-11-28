//! Real-time streaming audio cleanup for 16 kHz mono s16 PCM chunks:
//! - USB startup-pop guard: drops initial wake-up DC transients / pops (such as the Razer Kiyo's
//!   ~350 ms `+0.48` DC transient when PipeWire opens the mic) before VAD or Whisper ever sees them.
//!   Latches permanently closed once clean audio arrives past 150 ms so chunks never oscillate.
//! - Seeded DC-blocker + 2nd-order Butterworth high-pass filter (~85 Hz in `standard`, ~120 Hz in
//!   `strong`).
//! - Pure-Rust radix-2 FFT (256-sample sqrt-Hann, 50 % overlap-add) spectral subtraction / Wiener
//!   noise reduction in `strong` mode with rolling minimum-statistics noise estimation so
//!   speech-first push-to-talk is never absorbed into the noise floor.
//! - Smooth adaptive noise gate / soft expander evaluated in 5 ms sub-blocks with fast attack
//!   (~5 ms) and smooth release (~150 ms) + 150 ms overhang so leading consonants and syllable
//!   tails stay intact.

use crate::audio::{self, RATE};
use std::collections::VecDeque;
use std::f64::consts::{SQRT_2, TAU};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoiseFilterMode {
    Off,
    #[default]
    Standard,
    Strong,
}

impl NoiseFilterMode {
    pub fn parse(s: &str) -> Self {
        match s {
            "off" => NoiseFilterMode::Off,
            "strong" => NoiseFilterMode::Strong,
            _ => NoiseFilterMode::Standard,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            NoiseFilterMode::Off => "off",
            NoiseFilterMode::Standard => "standard",
            NoiseFilterMode::Strong => "strong",
        }
    }
}

/// Output of one filtered chunk: cleaned PCM plus the post-HPF, pre-gate RMS for VAD and level meter.
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessedChunk {
    pub samples: Vec<i16>,
    pub vad_rms: f64,
}

/// First-order DC-blocking filter: `y[n] = x[n] - x[n-1] + R * y[n-1]`.
/// Seeded with the first post-guard sample so there is no step transient on sample 0.
#[derive(Debug, Clone)]
struct DcBlocker {
    r: f64,
    seeded: bool,
    prev_x: f64,
    prev_y: f64,
}

impl DcBlocker {
    fn new(r: f64) -> Self {
        DcBlocker { r, seeded: false, prev_x: 0.0, prev_y: 0.0 }
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        if !self.seeded {
            self.seeded = true;
            self.prev_x = x;
            self.prev_y = 0.0;
            return 0.0;
        }
        let y = x - self.prev_x + self.r * self.prev_y;
        self.prev_x = x;
        self.prev_y = y;
        y
    }
}

/// Second-order Butterworth high-pass filter (Direct Form II Transposed).
#[derive(Debug, Clone)]
struct BiquadHpf {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
    z1: f64,
    z2: f64,
}

impl BiquadHpf {
    fn butterworth(cutoff_hz: f64, sample_rate: f64) -> Self {
        let w0 = TAU * cutoff_hz / sample_rate;
        let cos_w0 = w0.cos();
        let sin_w0 = w0.sin();
        // Q = 1 / sqrt(2) => alpha = sin(w0) / (2 * Q) = sin(w0) / sqrt(2)
        let alpha = sin_w0 / SQRT_2;
        let a0 = 1.0 + alpha;
        let b0 = ((1.0 + cos_w0) * 0.5) / a0;
        let b1 = -(1.0 + cos_w0) / a0;
        let b2 = ((1.0 + cos_w0) * 0.5) / a0;
        let a1 = (-2.0 * cos_w0) / a0;
        let a2 = (1.0 - alpha) / a0;
        BiquadHpf { b0, b1, b2, a1, a2, z1: 0.0, z2: 0.0 }
    }

    #[inline]
    fn process(&mut self, x: f64) -> f64 {
        let y = self.b0 * x + self.z1;
        self.z1 = self.b1 * x - self.a1 * y + self.z2;
        self.z2 = self.b2 * x - self.a2 * y;
        y
    }
}

/// 5 ms sub-block length at 16 kHz (80 samples) so leading consonants open the gate immediately.
const GATE_SUB_BLOCK: usize = (RATE as usize * 5) / 1000;

/// Smooth adaptive soft noise gate / expander evaluated in 5 ms sub-blocks with ~5 ms attack,
/// ~150 ms release, and 150 ms overhang.
#[derive(Debug, Clone)]
struct NoiseGate {
    strong: bool,
    floor: Option<f64>,
    hold_ms: u32,
    gain: f64,
    alpha_attack: f64,
    alpha_release: f64,
}

impl NoiseGate {
    fn new(strong: bool) -> Self {
        let rate = RATE as f64;
        NoiseGate {
            strong,
            floor: None,
            hold_ms: 0,
            gain: 1.0,
            alpha_attack: 1.0 - (-1.0 / (0.005 * rate)).exp(),
            alpha_release: 1.0 - (-1.0 / (0.150 * rate)).exp(),
        }
    }

    fn process(&mut self, samples: &mut [f64]) {
        if samples.is_empty() {
            return;
        }
        let min_gain = if self.strong { 0.10 } else { 0.20 };
        for block in samples.chunks_mut(GATE_SUB_BLOCK) {
            let rms = (block.iter().map(|&s| s * s).sum::<f64>() / block.len() as f64).sqrt();
            let ms = (audio::ms_of(block.len()) as u32).max(1);
            let floor = match self.floor {
                None => rms.min(0.03),
                Some(f) if rms < f => f * 0.96 + rms * 0.04,
                Some(f) if rms < (f * 2.2).max(0.004) => f * 0.995 + rms.min(0.03) * 0.005,
                Some(f) => f,
            };
            self.floor = Some(floor);

            // Clamp threshold so quiet speech is preserved even in a somewhat noisy room.
            let gate_thresh = (floor * 2.2).max(0.004).min(0.035);
            let target_gain = if rms >= gate_thresh {
                self.hold_ms = 150;
                1.0
            } else if self.hold_ms > 0 {
                self.hold_ms = self.hold_ms.saturating_sub(ms);
                1.0
            } else {
                let ratio = (rms / gate_thresh.max(1e-6)).clamp(0.0, 1.0);
                (min_gain + (1.0 - min_gain) * ratio * ratio).min(1.0)
            };

            for s in block.iter_mut() {
                let alpha = if target_gain > self.gain { self.alpha_attack } else { self.alpha_release };
                self.gain += alpha * (target_gain - self.gain);
                *s *= self.gain;
            }
        }
    }
}

const FFT_N: usize = 256;
const FFT_HOP: usize = FFT_N / 2;
const FFT_BINS: usize = FFT_N / 2 + 1;
/// Rolling minimum-statistics window (~1 s at 8 ms hop).
const MIN_STATS_FRAMES: usize = 125;

/// In-place radix-2 Cooley-Tukey FFT (`FFT_N` = 256).
fn fft_radix2(re: &mut [f64; FFT_N], im: &mut [f64; FFT_N], inverse: bool) {
    // Bit-reversal permutation.
    let mut j = 0usize;
    for i in 1..FFT_N {
        let mut bit = FFT_N >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j ^= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    // Iterative Danielson-Lanczos butterflies.
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2usize;
    while len <= FFT_N {
        let half = len >> 1;
        let angle = sign * TAU / len as f64;
        let (w_sin, w_cos) = angle.sin_cos();
        let mut start = 0;
        while start < FFT_N {
            let mut wr = 1.0;
            let mut wi = 0.0;
            for k in 0..half {
                let u = start + k;
                let v = u + half;
                let tr = wr * re[v] - wi * im[v];
                let ti = wr * im[v] + wi * re[v];
                re[v] = re[u] - tr;
                im[v] = im[u] - ti;
                re[u] += tr;
                im[u] += ti;
                let next_wr = wr * w_cos - wi * w_sin;
                wi = wr * w_sin + wi * w_cos;
                wr = next_wr;
            }
            start += len;
        }
        len <<= 1;
    }
    if inverse {
        let inv_n = 1.0 / FFT_N as f64;
        for i in 0..FFT_N {
            re[i] *= inv_n;
            im[i] *= inv_n;
        }
    }
}

/// 256-sample sqrt-Hann 50 % overlap-add spectral subtraction / Wiener noise reducer.
#[derive(Debug, Clone)]
struct SpectralDenoiser {
    window: [f64; FFT_N],
    in_buf: Vec<f64>,
    ola: [f64; FFT_N],
    primed: bool,
    samples_in: usize,
    samples_out: usize,
    noise_psd: [f64; FFT_BINS],
    prev_gain: [f64; FFT_BINS],
    noise_frames: u32,
    frames_seen: u32,
    rms_history: VecDeque<f64>,
}

impl SpectralDenoiser {
    fn new() -> Self {
        let mut window = [0.0; FFT_N];
        for (k, w) in window.iter_mut().enumerate() {
            // Periodic Hann window; sqrt(hann) on analysis & synthesis sums to exact 1.0 at 50 % hop.
            let hann = 0.5 * (1.0 - (TAU * k as f64 / FFT_N as f64).cos());
            *w = hann.sqrt();
        }
        SpectralDenoiser {
            window,
            in_buf: vec![0.0; FFT_HOP],
            ola: [0.0; FFT_N],
            primed: false,
            samples_in: 0,
            samples_out: 0,
            noise_psd: [0.0; FFT_BINS],
            prev_gain: [1.0; FFT_BINS],
            noise_frames: 0,
            frames_seen: 0,
            rms_history: VecDeque::with_capacity(MIN_STATS_FRAMES),
        }
    }

    fn process(&mut self, samples: &[f64]) -> Vec<f64> {
        self.samples_in += samples.len();
        self.in_buf.extend_from_slice(samples);
        let mut out = Vec::with_capacity(samples.len());
        let mut pos = 0;
        while pos + FFT_N <= self.in_buf.len() {
            let mut frame = [0.0; FFT_N];
            frame.copy_from_slice(&self.in_buf[pos..pos + FFT_N]);
            self.process_frame(&frame, &mut out, false);
            pos += FFT_HOP;
        }
        if pos > 0 {
            self.in_buf.drain(..pos);
        }
        self.samples_out += out.len();
        out
    }

    /// Emits the remaining buffered overlap-add tail (up to ~16 ms) at the end of a recording.
    fn flush(&mut self) -> Vec<f64> {
        let remaining = self.samples_in.saturating_sub(self.samples_out);
        if remaining == 0 {
            return Vec::new();
        }
        let pad = FFT_N + FFT_HOP;
        self.in_buf.extend(std::iter::repeat_n(0.0, pad));
        let mut out = Vec::with_capacity(remaining + FFT_HOP);
        let mut pos = 0;
        while pos + FFT_N <= self.in_buf.len() && out.len() < remaining {
            let mut frame = [0.0; FFT_N];
            frame.copy_from_slice(&self.in_buf[pos..pos + FFT_N]);
            self.process_frame(&frame, &mut out, true);
            pos += FFT_HOP;
        }
        if pos > 0 {
            self.in_buf.drain(..pos);
        }
        out.truncate(remaining);
        self.samples_out += out.len();
        out
    }

    fn process_frame(&mut self, frame: &[f64; FFT_N], out: &mut Vec<f64>, flushing: bool) {
        let mut re = [0.0; FFT_N];
        let mut im = [0.0; FFT_N];
        let mut energy = 0.0;
        for i in 0..FFT_N {
            let s = frame[i];
            energy += s * s;
            re[i] = s * self.window[i];
        }
        let frame_rms = (energy / FFT_N as f64).sqrt();
        fft_radix2(&mut re, &mut im, false);

        // Skip the half-zero priming frame (and zero-padded flush frames) for noise estimation.
        if self.primed {
            let is_noise = if flushing {
                false
            } else {
                self.frames_seen = self.frames_seen.saturating_add(1);
                if self.rms_history.len() == MIN_STATS_FRAMES {
                    self.rms_history.pop_front();
                }
                self.rms_history.push_back(frame_rms);
                let min_floor = self.rms_history.iter().copied().fold(f64::INFINITY, f64::min).min(0.025);
                // Speech-first protection: only seed initial frames if genuinely quiet (< 0.012);
                // otherwise wait for stationary minimum-statistics confirmation (< 0.025).
                let initial_quiet = self.frames_seen <= 4 && frame_rms < 0.012;
                let stationary_noise = self.rms_history.len() >= 15
                    && frame_rms < 0.025
                    && frame_rms <= (min_floor * 1.6).max(0.002);
                let tracked_quiet = self.noise_frames > 0
                    && frame_rms < 0.025
                    && frame_rms <= (min_floor * 1.8).max(0.002);
                initial_quiet || stationary_noise || tracked_quiet
            };

            let over_sub = 1.8;
            let spectral_floor = 0.10;

            for k in 0..FFT_BINS {
                let psd = re[k] * re[k] + im[k] * im[k];
                if is_noise {
                    if self.noise_frames == 0 {
                        self.noise_psd[k] = psd;
                    } else {
                        let alpha = if self.noise_frames < 6 { 0.55 } else { 0.90 };
                        self.noise_psd[k] = alpha * self.noise_psd[k] + (1.0 - alpha) * psd;
                    }
                } else if self.noise_frames > 0 && !flushing && psd < self.noise_psd[k] {
                    self.noise_psd[k] = 0.92 * self.noise_psd[k] + 0.08 * psd;
                }

                let target_gain = if self.noise_frames > 0 || is_noise {
                    let clean = (psd - over_sub * self.noise_psd[k]).max(0.0);
                    (clean / psd.max(1e-12)).sqrt().clamp(spectral_floor, 1.0)
                } else {
                    1.0
                };
                // Smooth gain across frames (faster attack than release) to suppress musical noise.
                let smooth = if target_gain > self.prev_gain[k] { 0.20 } else { 0.35 };
                let g = smooth * self.prev_gain[k] + (1.0 - smooth) * target_gain;
                self.prev_gain[k] = g;

                re[k] *= g;
                im[k] *= g;
                if k > 0 && k < FFT_N / 2 {
                    re[FFT_N - k] *= g;
                    im[FFT_N - k] *= g;
                }
            }
            if is_noise {
                self.noise_frames = self.noise_frames.saturating_add(1);
            }
        }

        fft_radix2(&mut re, &mut im, true);
        for i in 0..FFT_N {
            self.ola[i] += re[i] * self.window[i];
        }
        if self.primed {
            out.extend_from_slice(&self.ola[..FFT_HOP]);
        } else {
            self.primed = true;
        }
        self.ola.copy_within(FFT_HOP..FFT_N, 0);
        self.ola[FFT_HOP..FFT_N].fill(0.0);
    }
}

/// Streaming real-time audio filter.
#[derive(Debug, Clone)]
pub struct AudioFilter {
    mode: NoiseFilterMode,
    elapsed_ms: u32,
    saw_dc_spike: bool,
    guard_done: bool,
    dc: DcBlocker,
    hpf: BiquadHpf,
    spectral: Option<SpectralDenoiser>,
    gate: NoiseGate,
}

impl AudioFilter {
    pub fn new(mode: NoiseFilterMode) -> Self {
        let cutoff = match mode {
            NoiseFilterMode::Strong => 120.0,
            _ => 85.0,
        };
        let spectral = (mode == NoiseFilterMode::Strong).then(SpectralDenoiser::new);
        AudioFilter {
            mode,
            elapsed_ms: 0,
            saw_dc_spike: false,
            guard_done: mode == NoiseFilterMode::Off,
            dc: DcBlocker::new(0.995),
            hpf: BiquadHpf::butterworth(cutoff, RATE as f64),
            spectral,
            gate: NoiseGate::new(mode == NoiseFilterMode::Strong),
        }
    }

    pub fn mode(&self) -> NoiseFilterMode {
        self.mode
    }

    /// Processes one chunk of 16 kHz mono s16 PCM. Returns `None` when a USB startup-pop chunk is
    /// dropped so neither VAD nor the audio buffer ever sees it.
    pub fn process_chunk(&mut self, chunk: &[i16]) -> Option<Vec<i16>> {
        self.process_chunk_with_level(chunk).map(|p| p.samples)
    }

    /// Like [AudioFilter::process_chunk], also returning the post-HPF, pre-gate RMS so VAD and
    /// level meters are not affected by soft noise-gate attenuation.
    pub fn process_chunk_with_level(&mut self, chunk: &[i16]) -> Option<ProcessedChunk> {
        if self.mode == NoiseFilterMode::Off {
            let vad_rms = audio::rms(chunk);
            return Some(ProcessedChunk { samples: chunk.to_vec(), vad_rms });
        }
        if chunk.is_empty() {
            return Some(ProcessedChunk { samples: Vec::new(), vad_rms: 0.0 });
        }

        let ms = (audio::ms_of(chunk.len()) as u32).max(1);
        let prev_elapsed = self.elapsed_ms;
        self.elapsed_ms = self.elapsed_ms.saturating_add(ms);

        let guard_ms = match self.mode {
            NoiseFilterMode::Standard | NoiseFilterMode::Strong => 500,
            NoiseFilterMode::Off => 0,
        };
        if !self.guard_done {
            if self.elapsed_ms <= guard_ms {
                let n = chunk.len() as f64;
                let (sum, sum_sq) = chunk.iter().fold((0.0, 0.0), |(s, sq), &sample| {
                    let x = sample as f64 / 32768.0;
                    (s + x, sq + x * x)
                });
                let mean_dc = sum / n;
                let rms_raw = (sum_sq / n).sqrt();
                // Flag DC spike strictly from actual DC offset (never from raw RMS alone).
                if mean_dc.abs() > 0.025 {
                    self.saw_dc_spike = true;
                }
                // After a real USB DC wake-up pop (e.g. Razer Kiyo V2 X), also drop the
                // decaying tail chunk (~350 ms, |dc| ≈ 0.013, rms ≈ 0.025) so it cannot
                // poison the VAD's initial calibration noise floor.
                if prev_elapsed < 150
                    || mean_dc.abs() > 0.015
                    || (self.saw_dc_spike && (mean_dc.abs() > 0.005 || rms_raw > 0.015))
                {
                    return None;
                }
            }
            // Past 150 ms with a clean chunk (or past guard_ms): latch guard closed permanently.
            self.guard_done = true;
        }

        let mut samples: Vec<f64> = chunk
            .iter()
            .map(|&s| {
                let x = s as f64 / 32768.0;
                let x = self.dc.process(x);
                self.hpf.process(x)
            })
            .collect();

        if let Some(spectral) = &mut self.spectral {
            samples = spectral.process(&samples);
            if samples.is_empty() {
                return None;
            }
        }

        let vad_rms = (samples.iter().map(|&s| s * s).sum::<f64>() / samples.len() as f64).sqrt();

        self.gate.process(&mut samples);

        let out = samples
            .into_iter()
            .map(|x| (x * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
            .collect();
        Some(ProcessedChunk { samples: out, vad_rms })
    }

    /// Flushes any buffered tail samples (the final 8–16 ms of STFT overlap-add in `Strong` mode).
    pub fn flush(&mut self) -> Vec<i16> {
        let Some(spectral) = &mut self.spectral else {
            return Vec::new();
        };
        let mut tail = spectral.flush();
        if tail.is_empty() {
            return Vec::new();
        }
        self.gate.process(&mut tail);
        tail.into_iter()
            .map(|x| (x * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::audio::{CHUNK_SAMPLES, rms, synth};

    fn run_all(mode: NoiseFilterMode, pcm: &[i16]) -> Vec<i16> {
        let mut f = AudioFilter::new(mode);
        let mut out = Vec::new();
        for chunk in pcm.chunks(CHUNK_SAMPLES) {
            if let Some(clean) = f.process_chunk(chunk) {
                out.extend_from_slice(&clean);
            }
        }
        out.extend_from_slice(&f.flush());
        out
    }

    #[test]
    fn off_mode_is_bit_exact_pass_through() {
        let pcm = [synth::tone(150, 50.0, 12000.0), synth::noise(200, 1000)].concat();
        assert_eq!(run_all(NoiseFilterMode::Off, &pcm), pcm);
    }

    #[test]
    fn drops_razer_kiyo_startup_dc_pop() {
        // Synthetic Razer Kiyo V2 X wake-up transient: 350 ms of +0.48 DC with peak 1.0,
        // decaying into quiet room noise (rms ≈ 0.0025).
        let pop_samples = (RATE as usize * 350) / 1000;
        let mut pop = Vec::with_capacity(pop_samples);
        for i in 0..pop_samples {
            let decay = 1.0 - 0.7 * (i as f64 / pop_samples as f64);
            let sample = ((0.48 + 0.52 * (i as f64 * 0.03).sin().abs()) * decay * 32767.0).clamp(-32768.0, 32767.0) as i16;
            pop.push(sample);
        }
        let quiet = synth::noise(1000, 120); // rms ≈ 0.0021
        let stream = [pop, quiet].concat();

        for mode in [NoiseFilterMode::Standard, NoiseFilterMode::Strong] {
            let mut f = AudioFilter::new(mode);
            let mut vad = crate::vad::Vad::new(crate::vad::VadConfig::default(), true);
            let mut emitted_chunks = 0usize;
            for (idx, chunk) in stream.chunks(CHUNK_SAMPLES).enumerate() {
                let res = f.process_chunk_with_level(chunk);
                if idx < 7 {
                    // First 350 ms = 7 chunks of 50 ms must ALL be dropped!
                    assert!(res.is_none(), "{mode:?}: pop chunk {idx} leaked through");
                }
                if let Some(clean) = res {
                    emitted_chunks += 1;
                    assert!(clean.vad_rms < 0.01, "{mode:?}: unexpected spike after pop: {}", clean.vad_rms);
                    vad.feed(clean.vad_rms, audio::ms_of(clean.samples.len()) as u32);
                }
            }
            assert!(emitted_chunks > 10, "{mode:?}: quiet room after pop should still stream");
            assert!(!vad.heard_speech(), "{mode:?}: pop fooled VAD into hearing speech");
        }
    }

    #[test]
    fn drops_razer_kiyo_decay_tail_so_vad_threshold_stays_low() {
        // Real Razer Kiyo V2 X wake-up profile: 300 ms strong DC pop followed by a 50 ms
        // decaying tail chunk (dc ≈ -0.013, rms ≈ 0.025) and then quiet room noise (rms ≈ 0.0025).
        let pop = vec![24000i16; CHUNK_SAMPLES * 6]; // 300 ms strong positive DC spike
        // Decaying tail chunk: dc ≈ -0.013, ac ≈ 0.021 -> rms ≈ 0.025
        let tail: Vec<i16> = (0..CHUNK_SAMPLES)
            .map(|i| ((-0.013 + 0.030 * (i as f64 * 0.09).sin()) * 32768.0) as i16)
            .collect();
        let quiet = synth::noise(500, 140); // rms ≈ 0.0025
        let speech = synth::tone(500, 260.0, 900.0); // modest desk speech: rms ≈ 0.019
        let stream = [pop, tail, quiet, speech].concat();

        let mut f = AudioFilter::new(NoiseFilterMode::Standard);
        let mut vad = crate::vad::Vad::new(crate::vad::VadConfig::default(), true);
        for (idx, chunk) in stream.chunks(CHUNK_SAMPLES).enumerate() {
            let res = f.process_chunk_with_level(chunk);
            if idx <= 6 {
                assert!(res.is_none(), "pop or decay tail chunk {idx} leaked into VAD");
                continue;
            }
            if let Some(clean) = res {
                vad.feed(clean.vad_rms, audio::ms_of(clean.samples.len()) as u32);
            }
        }
        assert!(vad.threshold() < 0.015, "VAD threshold was poisoned by pop tail: {}", vad.threshold());
        assert!(vad.heard_speech(), "modest speech after pop tail was missed by VAD");
    }

    #[test]
    fn startup_guard_latches_closed_and_never_oscillates() {
        let mut f = AudioFilter::new(NoiseFilterMode::Standard);
        // 150 ms warm-up dropped.
        for chunk in synth::silence(150).chunks(CHUNK_SAMPLES) {
            assert!(f.process_chunk(chunk).is_none());
        }
        // Clean chunk at 200 ms latches the guard permanently closed.
        assert!(f.process_chunk(&synth::silence(50)).is_some());
        // Even if a weird DC-offset transient occurs at 250 ms, it must NOT be dropped (no oscillation).
        let later_dc = vec![4000i16; CHUNK_SAMPLES];
        assert!(f.process_chunk(&later_dc).is_some());
    }

    #[test]
    fn hpf_and_dc_blocker_attenuate_hum_while_preserving_speech() {
        // Warm up past startup guard with 500 ms silence.
        let warmup = synth::silence(500);
        // 50 Hz hum + DC offset vs 400 Hz speech tone (both at amplitude 10000).
        let hum_and_dc: Vec<i16> = synth::tone(1000, 50.0, 10000.0).into_iter().map(|s| s.saturating_add(4000)).collect();
        let speech = synth::tone(1000, 400.0, 10000.0);

        let mut f_hum = AudioFilter::new(NoiseFilterMode::Standard);
        for c in warmup.chunks(CHUNK_SAMPLES) {
            let _ = f_hum.process_chunk(c);
        }
        let mut hum_out = Vec::new();
        for c in hum_and_dc.chunks(CHUNK_SAMPLES) {
            if let Some(o) = f_hum.process_chunk(c) {
                hum_out.extend(o);
            }
        }

        let mut f_speech = AudioFilter::new(NoiseFilterMode::Standard);
        for c in warmup.chunks(CHUNK_SAMPLES) {
            let _ = f_speech.process_chunk(c);
        }
        let mut speech_out = Vec::new();
        for c in speech.chunks(CHUNK_SAMPLES) {
            if let Some(o) = f_speech.process_chunk(c) {
                speech_out.extend(o);
            }
        }

        // Measure steady-state second half.
        let hum_in_rms = rms(&hum_and_dc[hum_and_dc.len() / 2..]);
        let hum_out_rms = rms(&hum_out[hum_out.len() / 2..]);
        let hum_dc = hum_out[hum_out.len() / 2..].iter().map(|&s| s as f64 / 32768.0).sum::<f64>() / (hum_out.len() / 2) as f64;
        let speech_in_rms = rms(&speech[speech.len() / 2..]);
        let speech_out_rms = rms(&speech_out[speech_out.len() / 2..]);

        assert!(hum_dc.abs() < 0.005, "DC offset not removed: {hum_dc}");
        assert!(hum_out_rms < hum_in_rms * 0.45, "50 Hz hum not attenuated enough: in={hum_in_rms}, out={hum_out_rms}");
        assert!(speech_out_rms > speech_in_rms * 0.90, "400 Hz speech attenuated too much: in={speech_in_rms}, out={speech_out_rms}");
    }

    #[test]
    fn strong_mode_reduces_broadband_noise_and_improves_snr() {
        // 1200 ms broadband noise (startup guard + minimum-statistics learning + steady state),
        // followed by 800 ms (400 Hz speech + broadband noise).
        let noise_a = synth::noise(1200, 900);
        let noise_b = synth::noise(800, 900);
        let tone = synth::tone(800, 400.0, 6000.0);
        let noisy_speech: Vec<i16> = tone.iter().zip(noise_b.iter()).map(|(&t, &n)| t.saturating_add(n)).collect();

        let mut f = AudioFilter::new(NoiseFilterMode::Strong);
        let mut noise_out = Vec::new();
        for (i, c) in noise_a.chunks(CHUNK_SAMPLES).enumerate() {
            if let Some(o) = f.process_chunk(c) {
                if i >= 14 {
                    // After 700 ms (past startup guard + minimum-statistics noise learning).
                    noise_out.extend(o);
                }
            }
        }
        let mut speech_out = Vec::new();
        for c in noisy_speech.chunks(CHUNK_SAMPLES) {
            if let Some(o) = f.process_chunk(c) {
                speech_out.extend(o);
            }
        }

        let raw_noise_rms = rms(&noise_a[noise_a.len() / 2..]);
        let clean_noise_rms = rms(&noise_out);
        let raw_speech_rms = rms(&noisy_speech[noisy_speech.len() / 2..]);
        let clean_speech_rms = rms(&speech_out[speech_out.len() / 2..]);

        let raw_snr = raw_speech_rms / raw_noise_rms;
        let clean_snr = clean_speech_rms / clean_noise_rms.max(1e-6);

        assert!(clean_noise_rms < raw_noise_rms * 0.25, "broadband noise not reduced enough: raw={raw_noise_rms}, clean={clean_noise_rms}");
        assert!(clean_speech_rms > raw_speech_rms * 0.70, "speech crushed: raw={raw_speech_rms}, clean={clean_speech_rms}");
        assert!(clean_snr > raw_snr * 3.0, "SNR did not improve enough: raw={raw_snr}, clean={clean_snr}");
    }

    #[test]
    fn strong_mode_speech_first_is_not_absorbed_into_noise_floor() {
        // Push-to-talk scenario: strong voice starts immediately after the 150 ms startup guard.
        let stream = [synth::silence(150), synth::tone(1000, 350.0, 5000.0)].concat();
        let in_rms = rms(&synth::tone(1000, 350.0, 5000.0));
        let out = run_all(NoiseFilterMode::Strong, &stream);
        assert!(out.len() >= 15_000, "flush should preserve full speech length: {}", out.len());
        let out_rms = rms(&out[out.len() / 2..]);
        // Must not be attenuated by > 6 dB (0.5x amplitude); in fact should be ~unity (> 0.85x).
        assert!(out_rms > in_rms * 0.85, "speech-first voice was attenuated: in={in_rms}, out={out_rms}");
    }

    #[test]
    fn dc_blocker_seeds_without_initial_step_transient() {
        let mut dc = DcBlocker::new(0.995);
        // Constant DC input of 0.5 should produce 0.0 on the very first sample, not a 0.5 spike.
        assert_eq!(dc.process(0.5), 0.0);
        assert_eq!(dc.process(0.5), 0.0);
    }

    #[test]
    fn fft_round_trip_is_exact() {
        let mut re = [0.0; FFT_N];
        let mut im = [0.0; FFT_N];
        let orig: Vec<f64> = (0..FFT_N).map(|i| (i as f64 * 0.17).sin() + 0.3 * (i as f64 * 0.51).cos()).collect();
        re.copy_from_slice(&orig);
        fft_radix2(&mut re, &mut im, false);
        fft_radix2(&mut re, &mut im, true);
        for i in 0..FFT_N {
            assert!((re[i] - orig[i]).abs() < 1e-10, "re[{i}] mismatch: {} vs {}", re[i], orig[i]);
            assert!(im[i].abs() < 1e-10, "im[{i}] mismatch: {}", im[i]);
        }
    }
}
